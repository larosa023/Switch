//! Low-level keyboard hook that takes over Alt+Tab.
//!
//! The hook runs on its own thread so a busy UI thread can never delay the
//! user's keystrokes (Windows silently unhooks slow low-level hooks). It
//! decides synchronously whether to swallow a key and forwards everything
//! else to the switcher window with `PostMessageW`.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering::*};

use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::util::hwnd;

pub const WM_SW_OPEN: u32 = WM_APP + 10;
pub const WM_SW_NEXT: u32 = WM_APP + 11;
pub const WM_SW_PREV: u32 = WM_APP + 12;
pub const WM_SW_COMMIT: u32 = WM_APP + 13;
pub const WM_SW_KEY: u32 = WM_APP + 14;

pub const OPEN_REVERSE: usize = 1;
pub const OPEN_STICKY: usize = 2;
/// Only windows of the foreground app (Alt+`).
pub const OPEN_SAME_APP: usize = 4;

/// Scan code of the key above Tab (` on US layouts), layout independent.
const SC_BACKTICK: u32 = 0x29;

pub const KEY_SHIFT: isize = 1 << 16;
pub const KEY_CTRL: isize = 1 << 17;

/// Marks input we inject so the hook ignores it.
pub const MAGIC: usize = 0x5357_4954;

pub static UI_HWND: AtomicIsize = AtomicIsize::new(0);
pub static ALT_TAB: AtomicBool = AtomicBool::new(true);
pub static ALT_BACKTICK: AtomicBool = AtomicBool::new(true);
/// Releasing Alt switches; when false Alt+Tab sessions are always sticky.
pub static RELEASE_TO_SWITCH: AtomicBool = AtomicBool::new(true);
/// A context menu is open over the switcher; let keys reach it.
pub static PASSTHRU: AtomicBool = AtomicBool::new(false);
/// Packed custom hotkey (see `Hotkey::pack`), 0 = none.
pub static HOTKEY: AtomicU32 = AtomicU32::new(0);
/// A switcher session is open (keys are being routed to it).
pub static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Releasing Alt does not commit; Enter/Esc/click ends the session.
pub static STICKY: AtomicBool = AtomicBool::new(false);
pub static PAUSED: AtomicBool = AtomicBool::new(false);
/// The settings window is recording a shortcut; pass everything through.
pub static RECORDING: AtomicBool = AtomicBool::new(false);
/// Safety net: tick when a session was opened and whether the UI picked it up.
/// If the UI thread ever failed to respond, the hook stops swallowing keys.
static OPEN_TICK: AtomicU32 = AtomicU32::new(0);
pub static UI_ACK: AtomicBool = AtomicBool::new(false);

fn begin_session(sticky: bool) {
    UI_ACK.store(false, Relaxed);
    OPEN_TICK.store(unsafe { windows::Win32::System::SystemInformation::GetTickCount() }, Relaxed);
    STICKY.store(sticky, Relaxed);
    ACTIVE.store(true, Relaxed);
}

fn post(msg: u32, w: usize, l: isize) {
    let h = UI_HWND.load(Relaxed);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(hwnd(h), msg, WPARAM(w), LPARAM(l));
        }
    }
}

#[inline]
fn down(vk: VIRTUAL_KEY) -> bool {
    unsafe { (GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000) != 0 }
}

fn is_modifier(vk: u32) -> bool {
    matches!(
        VIRTUAL_KEY(vk as u16),
        VK_SHIFT | VK_LSHIFT | VK_RSHIFT | VK_CONTROL | VK_LCONTROL | VK_RCONTROL | VK_MENU
            | VK_LMENU | VK_RMENU | VK_LWIN | VK_RWIN | VK_CAPITAL
    )
}

fn is_alt(vk: u32) -> bool {
    matches!(VIRTUAL_KEY(vk as u16), VK_MENU | VK_LMENU | VK_RMENU)
}

fn key_input(vk: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                time: 0,
                dwExtraInfo: MAGIC,
            },
        },
    }
}

/// Send an unassigned key so that releasing Alt/Win afterwards doesn't open
/// the focused app's menu bar or the Start menu.
pub fn inject_dummy() {
    let inputs = [key_input(0xE8, false), key_input(0xE8, true)];
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

/// A tap of Alt (with a dummy key in between) satisfies the foreground lock
/// so `SetForegroundWindow` is allowed, without toggling any menu bar.
pub fn inject_alt_tap() {
    let inputs = [
        key_input(VK_MENU.0, false),
        key_input(0xE8, false),
        key_input(0xE8, true),
        key_input(VK_MENU.0, true),
    ];
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

pub fn alt_held() -> bool {
    down(VK_MENU)
}

unsafe extern "system" fn kb_proc(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(HHOOK::default(), code, w, l);
    }
    let k = &*(l.0 as *const KBDLLHOOKSTRUCT);
    if k.dwExtraInfo == MAGIC || PAUSED.load(Relaxed) || RECORDING.load(Relaxed) || PASSTHRU.load(Relaxed) {
        return CallNextHookEx(HHOOK::default(), code, w, l);
    }
    let msg = w.0 as u32;
    let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
    let vk = k.vkCode;
    let alt = (k.flags.0 & LLKHF_ALTDOWN.0) != 0 || down(VK_MENU);
    let shift = down(VK_SHIFT);
    let ctrl = down(VK_CONTROL);
    let win = down(VK_LWIN) || down(VK_RWIN);
    let mut active = ACTIVE.load(Relaxed);
    if active && !UI_ACK.load(Relaxed) {
        let now = windows::Win32::System::SystemInformation::GetTickCount();
        if now.wrapping_sub(OPEN_TICK.load(Relaxed)) > 1500 {
            end_session();
            active = false;
        }
    }

    // Alt+Tab, Shift+Alt+Tab, Ctrl+Alt+Tab (sticky).
    if vk == VK_TAB.0 as u32 && alt && !win && ALT_TAB.load(Relaxed) {
        if is_down {
            if !active {
                let sticky = ctrl || !RELEASE_TO_SWITCH.load(Relaxed);
                begin_session(sticky);
                let mut f = 0;
                if shift {
                    f |= OPEN_REVERSE;
                }
                if sticky {
                    f |= OPEN_STICKY;
                }
                inject_dummy();
                post(WM_SW_OPEN, f, 0);
            } else {
                post(if shift { WM_SW_PREV } else { WM_SW_NEXT }, 0, 0);
            }
        }
        return LRESULT(1);
    }

    // Alt+` : windows of the current app.
    if k.scanCode == SC_BACKTICK && alt && !win && !ctrl && ALT_BACKTICK.load(Relaxed) {
        if is_down {
            if !active {
                let sticky = !RELEASE_TO_SWITCH.load(Relaxed);
                begin_session(sticky);
                let mut f = OPEN_SAME_APP;
                if shift {
                    f |= OPEN_REVERSE;
                }
                if sticky {
                    f |= OPEN_STICKY;
                }
                inject_dummy();
                post(WM_SW_OPEN, f, 0);
            } else {
                post(if shift { WM_SW_PREV } else { WM_SW_NEXT }, 0, 0);
            }
        }
        return LRESULT(1);
    }

    // Custom shortcut.
    let hk = HOTKEY.load(Relaxed);
    if hk != 0 && vk == (hk & 0xFFFF) {
        let (h_ctrl, h_alt, h_shift, h_win) =
            (hk & 1 << 16 != 0, hk & 1 << 17 != 0, hk & 1 << 18 != 0, hk & 1 << 19 != 0);
        if h_ctrl == ctrl && h_alt == alt && h_shift == shift && h_win == win {
            if is_down {
                if !active {
                    // Shortcuts with Alt behave like Alt+Tab (release to switch);
                    // others stay open until Enter, Esc or a click.
                    let sticky = !h_alt;
                    begin_session(sticky);
                    if h_alt || h_win {
                        inject_dummy();
                    }
                    post(WM_SW_OPEN, if sticky { OPEN_STICKY } else { 0 }, 0);
                } else {
                    post(WM_SW_NEXT, 0, 0);
                }
            }
            return LRESULT(1);
        }
    }

    if active {
        if is_modifier(vk) {
            if !is_down && is_alt(vk) && !STICKY.load(Relaxed) {
                post(WM_SW_COMMIT, 0, 0);
            }
            return CallNextHookEx(HHOOK::default(), code, w, l);
        }
        if is_down {
            let mut flags = (k.scanCode as isize) & 0xFFFF;
            if shift {
                flags |= KEY_SHIFT;
            }
            if ctrl {
                flags |= KEY_CTRL;
            }
            post(WM_SW_KEY, vk as usize, flags);
        }
        return LRESULT(1);
    }

    CallNextHookEx(HHOOK::default(), code, w, l)
}

pub fn start() {
    std::thread::Builder::new()
        .name("switch-hook".into())
        .spawn(|| unsafe {
            // Keep this thread responsive above everything else.
            use windows::Win32::System::Threading::*;
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
            let inst = GetModuleHandleW(None).unwrap_or_default();
            let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(kb_proc), HINSTANCE(inst.0), 0);
            if hook.is_err() {
                crate::util::log("failed to install keyboard hook");
                return;
            }
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        })
        .expect("spawn hook thread");
}

pub fn apply_config(c: &crate::config::Config) {
    ALT_TAB.store(c.alt_tab, Relaxed);
    ALT_BACKTICK.store(c.alt_backtick, Relaxed);
    RELEASE_TO_SWITCH.store(c.release_to_switch, Relaxed);
    HOTKEY.store(c.hotkey.map(|h| h.pack()).unwrap_or(0), Relaxed);
}

pub fn open_from_ui() {
    begin_session(true);
    UI_ACK.store(true, Relaxed);
}

pub fn end_session() {
    ACTIVE.store(false, Relaxed);
    STICKY.store(false, Relaxed);
}
