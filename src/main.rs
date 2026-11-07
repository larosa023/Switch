//! Switch: a fast, modern Alt-Tab replacement for Windows.
#![windows_subsystem = "windows"]
#![allow(clippy::too_many_arguments)]

mod config;
mod gfx;
mod hook;
mod settings_ui;
mod switcher;
mod updater;
mod util;
mod winlist;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::updater::State;
use crate::util::{wide, UiCell};

const WM_TRAY: u32 = WM_APP + 1;
const WM_SHOW_SETTINGS: u32 = WM_APP + 2;
const WM_OPEN_SWITCHER: u32 = WM_APP + 3;
const TIMER_UPDATE: usize = 10;

const ID_OPEN: usize = 100;
const ID_SETTINGS: usize = 101;
const ID_UPDATE: usize = 102;
const ID_PAUSE: usize = 103;
const ID_QUIT: usize = 104;

struct App {
    main: HWND,
    icon: HICON,
    taskbar_created: u32,
    notified_version: Option<String>,
    first_check_done: bool,
}
static APP: UiCell<App> = UiCell::new();

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let updated = args.iter().any(|a| a == "--updated");
    let background = args.iter().any(|a| a == "--background");
    // `Switch.exe --open` opens the switcher (handy for mouse buttons, Stream Deck, etc.).
    let open = args.iter().any(|a| a == "--open");

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        // Single instance. After an update the old process may still be exiting.
        let name = wide("Local\\Switch.SingleInstance.larosa023");
        let mut mutex = None;
        for _ in 0..if updated { 50 } else { 1 } {
            let m = CreateMutexW(None, true, PCWSTR(name.as_ptr()));
            if GetLastError() == ERROR_ALREADY_EXISTS {
                if let Ok(h) = m {
                    let _ = CloseHandle(h);
                }
                if updated {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    continue;
                }
                // Already running: bring up its settings window instead.
                if let Ok(other) = FindWindowW(w!("Switch.Main"), PCWSTR::null()) {
                    let m = if open { WM_OPEN_SWITCHER } else { WM_SHOW_SETTINGS };
                    let _ = PostMessageW(other, m, WPARAM(0), LPARAM(0));
                }
                return;
            }
            mutex = m.ok();
            break;
        }
        if mutex.is_none() {
            util::log("could not acquire single-instance mutex");
            return;
        }

        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let (cfg, first_run) = config::load();
        config::CONFIG.set(cfg);
        if first_run {
            config::save();
        }
        config::refresh_autostart_path();
        if updated {
            updater::cleanup_old();
        }

        if let Err(e) = gfx::Gfx::init() {
            util::log(&format!("graphics init failed: {e}"));
            return;
        }

        let main = match create_main_window() {
            Ok(h) => h,
            Err(e) => {
                util::log(&format!("main window failed: {e}"));
                return;
            }
        };
        let sw = match switcher::create() {
            Ok(h) => h,
            Err(e) => {
                util::log(&format!("switcher window failed: {e}"));
                return;
            }
        };
        hook::UI_HWND.store(sw.0 as isize, std::sync::atomic::Ordering::Relaxed);
        hook::apply_config(config::cfg());
        hook::start();
        updater::init(main);

        let inst = GetModuleHandleW(None).unwrap_or_default();
        let icon = LoadImageW(inst, PCWSTR(1 as *const u16), IMAGE_ICON, 0, 0, LR_DEFAULTSIZE | LR_SHARED)
            .map(|h| HICON(h.0))
            .unwrap_or_default();
        APP.set(App {
            main,
            icon,
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            notified_version: None,
            first_check_done: false,
        });
        tray_add();

        if updated {
            balloon("Switch updated", &format!("You're now on version {}.", updater::VERSION));
        } else if first_run {
            balloon("Switch is running", "Press Alt+Tab to try it. Click the tray icon for settings.");
        }
        if open {
            let _ = PostMessageW(main, WM_OPEN_SWITCHER, WPARAM(0), LPARAM(0));
        } else if !background && !first_run && !updated {
            // Launched by hand while not running: show settings so it's obvious it started.
            settings_ui::show();
        }

        // First update check shortly after start, then every 6 hours.
        SetTimer(main, TIMER_UPDATE, 20_000, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        tray_remove();
        if let Some(m) = mutex {
            let _ = ReleaseMutex(m);
            let _ = CloseHandle(m);
        }
    }
}

unsafe fn create_main_window() -> windows::core::Result<HWND> {
    let inst = GetModuleHandleW(None)?;
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(main_proc),
        hInstance: inst.into(),
        lpszClassName: w!("Switch.Main"),
        ..Default::default()
    };
    RegisterClassExW(&wc);
    // A hidden top-level window (not message-only) so it receives TaskbarCreated.
    CreateWindowExW(
        WS_EX_TOOLWINDOW,
        w!("Switch.Main"),
        w!("Switch"),
        WS_POPUP,
        0,
        0,
        0,
        0,
        HWND::default(),
        HMENU::default(),
        inst,
        None,
    )
}

fn copy_into(dst: &mut [u16], s: &str) {
    let v: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..v.len()].copy_from_slice(&v);
    dst[v.len()] = 0;
}

fn tray_data() -> NOTIFYICONDATAW {
    let app = APP.get();
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: app.main,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP,
        uCallbackMessage: WM_TRAY,
        hIcon: app.icon,
        ..Default::default()
    };
    let paused = hook::PAUSED.load(std::sync::atomic::Ordering::Relaxed);
    let tip = match updater::state() {
        State::Available(r) => format!("Switch: update {} available", r.version),
        _ if paused => "Switch (paused)".to_string(),
        _ => "Switch".to_string(),
    };
    copy_into(&mut nid.szTip, &tip);
    nid
}

fn tray_add() {
    unsafe {
        let mut nid = tray_data();
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
        nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
    }
}

fn tray_update() {
    unsafe {
        let nid = tray_data();
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

fn tray_remove() {
    unsafe {
        let nid = tray_data();
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

fn balloon(title: &str, text: &str) {
    unsafe {
        let mut nid = tray_data();
        nid.uFlags |= NIF_INFO;
        nid.dwInfoFlags = NIIF_USER | NIIF_LARGE_ICON;
        nid.hBalloonIcon = APP.get().icon;
        copy_into(&mut nid.szInfoTitle, title);
        copy_into(&mut nid.szInfo, text);
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

unsafe fn tray_menu(h: HWND) {
    let Ok(menu) = CreatePopupMenu() else { return };
    let paused = hook::PAUSED.load(std::sync::atomic::Ordering::Relaxed);
    let add = |id: usize, text: &str, flags: MENU_ITEM_FLAGS| {
        let t = wide(text);
        let _ = AppendMenuW(menu, flags, id, PCWSTR(t.as_ptr()));
    };
    add(ID_OPEN, "Open switcher", MF_STRING);
    add(ID_SETTINGS, "Settings\u{2026}", MF_STRING);
    let _ = SetMenuDefaultItem(menu, ID_SETTINGS as u32, 0);
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    match updater::state() {
        State::Available(r) => add(ID_UPDATE, &format!("Install update {}", r.version), MF_STRING),
        State::Checking => add(ID_UPDATE, "Checking for updates\u{2026}", MF_GRAYED),
        State::Downloading(v) => add(ID_UPDATE, &format!("Downloading {v}\u{2026}"), MF_GRAYED),
        _ => add(ID_UPDATE, "Check for updates", MF_STRING),
    }
    add(ID_PAUSE, "Pause Switch", if paused { MF_CHECKED } else { MF_UNCHECKED });
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    add(ID_QUIT, "Quit", MF_STRING);

    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    // Required so the menu closes when clicking elsewhere.
    let _ = SetForegroundWindow(h);
    let cmd = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
        pt.x,
        pt.y,
        0,
        h,
        None,
    );
    let _ = DestroyMenu(menu);
    command(cmd.0 as usize);
}

fn command(id: usize) {
    match id {
        ID_OPEN => switcher::open_sticky(),
        ID_SETTINGS => settings_ui::show(),
        ID_UPDATE => match updater::state() {
            State::Available(_) => updater::install_async(),
            _ => {
                updater::check_async(false);
                settings_ui::show();
            }
        },
        ID_PAUSE => {
            let p = !hook::PAUSED.load(std::sync::atomic::Ordering::Relaxed);
            hook::PAUSED.store(p, std::sync::atomic::Ordering::Relaxed);
            tray_update();
        }
        ID_QUIT => unsafe { PostQuitMessage(0) },
        _ => {}
    }
}

fn on_update_state(lparam: isize) {
    tray_update();
    settings_ui::refresh();
    let app = APP.get();
    match updater::state() {
        State::Available(r) if lparam == 1 => {
            if app.notified_version.as_deref() != Some(&r.version) {
                app.notified_version = Some(r.version.clone());
                balloon(
                    &format!("Switch {} is available", r.version),
                    "Click here to install it, or use the tray menu.",
                );
            }
        }
        State::Downloading(_) if lparam == updater::READY_TO_RESTART => {
            updater::relaunch();
            unsafe { PostQuitMessage(0) };
        }
        State::Error(e) => {
            if lparam == 0 && app.first_check_done {
                util::log(&format!("updater: {e}"));
            }
        }
        _ => {}
    }
}

unsafe extern "system" fn main_proc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if let Some(app) = APP.try_get() {
        if msg == app.taskbar_created && msg != 0 {
            tray_add();
            return LRESULT(0);
        }
    }
    match msg {
        WM_TRAY => {
            // NOTIFYICON_VERSION_4: event in LOWORD(lParam).
            match (l.0 & 0xFFFF) as u32 {
                WM_LBUTTONUP => settings_ui::show(),
                WM_CONTEXTMENU | WM_RBUTTONUP => tray_menu(h),
                NIN_BALLOONUSERCLICK => match updater::state() {
                    State::Available(_) => updater::install_async(),
                    _ => settings_ui::show(),
                },
                _ => {}
            }
            LRESULT(0)
        }
        WM_OPEN_SWITCHER => {
            switcher::open_sticky();
            LRESULT(0)
        }
        WM_SHOW_SETTINGS => {
            settings_ui::show();
            LRESULT(0)
        }
        updater::WM_UPDATE_STATE => {
            on_update_state(l.0);
            LRESULT(0)
        }
        WM_TIMER if w.0 == TIMER_UPDATE => {
            if let Some(app) = APP.try_get() {
                if !app.first_check_done {
                    app.first_check_done = true;
                    let _ = KillTimer(h, TIMER_UPDATE);
                    SetTimer(h, TIMER_UPDATE, 6 * 60 * 60 * 1000, None);
                }
            }
            if config::cfg().auto_update {
                updater::check_async(true);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, w, l),
    }
}
