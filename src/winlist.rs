//! Enumerates the windows that belong in an Alt-Tab list, in Z (≈ MRU) order.

use std::collections::HashMap;
use std::os::windows::process::CommandExt;

use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Threading::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::UI::Shell::{ExtractIconExW, IVirtualDesktopManager, VirtualDesktopManager};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::util::{self, from_wide, wide, UiCell};

#[derive(Clone, Debug)]
pub struct WinInfo {
    pub hwnd: isize,
    pub title: String,
    /// Lower-case file name, e.g. "chrome.exe".
    pub exe: String,
    pub exe_path: String,
    /// Friendly name from the exe's version info, e.g. "Google Chrome".
    pub app_name: String,
    pub monitor: isize,
    pub minimized: bool,
    pub topmost: bool,
    /// Width / height of the window's normal (restored) bounds.
    pub aspect: f32,
    pub maximized: bool,
    /// Lives on another virtual desktop.
    pub other_desktop: bool,
}

#[derive(Default)]
struct Caches {
    app_names: HashMap<String, String>,
}
static CACHES: UiCell<Caches> = UiCell::new();

fn caches() -> &'static mut Caches {
    if CACHES.try_get().is_none() {
        CACHES.set(Caches::default());
    }
    CACHES.get()
}

unsafe fn cloak_value(h: HWND) -> u32 {
    let mut cloaked: u32 = 0;
    let _ = DwmGetWindowAttribute(
        h,
        DWMWA_CLOAKED,
        &mut cloaked as *mut u32 as *mut _,
        std::mem::size_of::<u32>() as u32,
    );
    cloaked
}

static VDM: UiCell<Option<IVirtualDesktopManager>> = UiCell::new();

/// Is the window on another virtual desktop (rather than cloaked for some other reason)?
unsafe fn on_other_desktop(h: HWND) -> bool {
    if VDM.try_get().is_none() {
        VDM.set(CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL).ok());
    }
    match VDM.get() {
        Some(vdm) => matches!(vdm.IsWindowOnCurrentVirtualDesktop(h), Ok(b) if !b.as_bool()),
        None => false,
    }
}

/// Raymond Chen's Alt-Tab eligibility test plus the modern extras (cloaking,
/// tool windows, no-activate windows).
/// Returns Some(on_other_desktop) for windows that belong in the list.
unsafe fn eligible(h: HWND, allow_other_desktops: bool) -> Option<bool> {
    if !IsWindowVisible(h).as_bool() {
        return None;
    }
    let ex = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
    if ex & WS_EX_APPWINDOW.0 == 0 {
        // Walk up to the root owner, then back down the last-active-popup chain.
        let mut walk = GetAncestor(h, GA_ROOTOWNER);
        loop {
            let tryw = GetLastActivePopup(walk);
            if tryw == walk || IsWindowVisible(tryw).as_bool() {
                walk = tryw;
                break;
            }
            walk = tryw;
        }
        if walk != h {
            return None;
        }
        if ex & WS_EX_TOOLWINDOW.0 != 0 {
            return None;
        }
    }
    if ex & WS_EX_NOACTIVATE.0 != 0 {
        return None;
    }
    let mut other = false;
    let cloak = cloak_value(h);
    if cloak != 0 {
        // DWM_CLOAKED_SHELL: the shell hides windows on other virtual desktops this way.
        if !(allow_other_desktops && cloak == 2 && on_other_desktop(h)) {
            return None;
        }
        other = true;
    }
    let mut cls = [0u16; 64];
    let n = GetClassNameW(h, &mut cls);
    let cls = String::from_utf16_lossy(&cls[..n.max(0) as usize]);
    if matches!(cls.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
        return None;
    }
    (GetWindowTextLengthW(h) > 0).then_some(other)
}

pub fn process_path(pid: u32) -> Option<String> {
    unsafe {
        let hp = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            hp,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(hp);
        ok.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// UWP apps are hosted by ApplicationFrameHost; the real app owns a child
/// CoreWindow in a different process.
unsafe fn uwp_child_pid(h: HWND, host_pid: u32) -> Option<u32> {
    struct Ctx {
        host: u32,
        found: u32,
    }
    unsafe extern "system" fn cb(c: HWND, l: LPARAM) -> BOOL {
        let ctx = &mut *(l.0 as *mut Ctx);
        let mut pid = 0;
        GetWindowThreadProcessId(c, Some(&mut pid));
        if pid != ctx.host && pid != 0 {
            ctx.found = pid;
            return false.into();
        }
        true.into()
    }
    let mut ctx = Ctx { host: host_pid, found: 0 };
    let _ = EnumChildWindows(h, Some(cb), LPARAM(&mut ctx as *mut Ctx as isize));
    (ctx.found != 0).then_some(ctx.found)
}

fn file_description(path: &str) -> Option<String> {
    unsafe {
        let wp = wide(path);
        let size = GetFileVersionInfoSizeW(PCWSTR(wp.as_ptr()), None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(wp.as_ptr()), 0, size, data.as_mut_ptr() as *mut _).ok()?;
        let mut ptr: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut len: u32 = 0;
        let q = wide(r"\VarFileInfo\Translation");
        let mut langs: Vec<(u16, u16)> = Vec::new();
        if VerQueryValueW(data.as_ptr() as *const _, PCWSTR(q.as_ptr()), &mut ptr, &mut len)
            .as_bool()
            && len >= 4
        {
            let p = ptr as *const u16;
            for i in 0..(len / 4) as usize {
                langs.push((*p.add(i * 2), *p.add(i * 2 + 1)));
            }
        }
        langs.push((0x0409, 0x04B0));
        langs.push((0x0409, 0x04E4));
        for (lang, cp) in langs {
            let q = wide(&format!(r"\StringFileInfo\{lang:04x}{cp:04x}\FileDescription"));
            if VerQueryValueW(data.as_ptr() as *const _, PCWSTR(q.as_ptr()), &mut ptr, &mut len)
                .as_bool()
                && len > 1
            {
                let s = from_wide(std::slice::from_raw_parts(ptr as *const u16, len as usize));
                let s = s.trim().to_string();
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }
        None
    }
}

fn app_name_for(path: &str, exe: &str) -> String {
    let c = caches();
    if let Some(n) = c.app_names.get(path) {
        return n.clone();
    }
    let name = file_description(path).unwrap_or_else(|| {
        let stem = exe.trim_end_matches(".exe");
        let mut ch = stem.chars();
        match ch.next() {
            Some(f) => f.to_uppercase().collect::<String>() + ch.as_str(),
            None => exe.to_string(),
        }
    });
    c.app_names.insert(path.to_string(), name.clone());
    name
}

unsafe fn aspect_of(h: HWND, minimized: bool) -> f32 {
    let mut r = RECT::default();
    if minimized {
        let mut wp = WINDOWPLACEMENT {
            length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
            ..Default::default()
        };
        if GetWindowPlacement(h, &mut wp).is_ok() {
            r = wp.rcNormalPosition;
        }
    } else if DwmGetWindowAttribute(
        h,
        DWMWA_EXTENDED_FRAME_BOUNDS,
        &mut r as *mut RECT as *mut _,
        std::mem::size_of::<RECT>() as u32,
    )
    .is_err()
    {
        let _ = GetWindowRect(h, &mut r);
    }
    let w = (r.right - r.left) as f32;
    let hh = (r.bottom - r.top) as f32;
    if w > 10.0 && hh > 10.0 {
        (w / hh).clamp(0.3, 4.0)
    } else {
        16.0 / 10.0
    }
}

pub fn enumerate(allow_other_desktops: bool) -> Vec<WinInfo> {
    struct Ctx {
        handles: Vec<(HWND, bool)>,
        other: bool,
    }
    let mut ctx = Ctx { handles: Vec::with_capacity(64), other: allow_other_desktops };
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
        let c = &mut *(l.0 as *mut Ctx);
        if let Some(o) = eligible(h, c.other) {
            c.handles.push((h, o));
        }
        true.into()
    }
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut ctx as *mut _ as isize));
    }
    let handles = ctx.handles;
    let own_pid = std::process::id();
    let mut out = Vec::with_capacity(handles.len());
    for (h, other_desktop) in handles {
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut pid));
            if pid == own_pid {
                continue;
            }
            let mut path = process_path(pid).unwrap_or_default();
            if path.to_lowercase().ends_with("applicationframehost.exe") {
                if let Some(child) = uwp_child_pid(h, pid) {
                    if let Some(p) = process_path(child) {
                        path = p;
                    }
                }
            }
            let exe = path.rsplit(['\\', '/']).next().unwrap_or("").to_lowercase();
            let len = GetWindowTextLengthW(h);
            let mut buf = vec![0u16; len as usize + 1];
            GetWindowTextW(h, &mut buf);
            let title = from_wide(&buf);
            let minimized = IsIconic(h).as_bool();
            let ex = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
            let app_name = if path.is_empty() {
                title.clone()
            } else {
                app_name_for(&path, &exe)
            };
            out.push(WinInfo {
                hwnd: h.0 as isize,
                title,
                exe,
                exe_path: path,
                app_name,
                monitor: MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST).0 as isize,
                minimized,
                topmost: ex & WS_EX_TOPMOST.0 != 0,
                aspect: aspect_of(h, minimized),
                maximized: IsZoomed(h).as_bool(),
                other_desktop,
            });
        }
    }
    out
}

/// Best available icon for a window. The bool says whether the caller owns
/// the handle (and must `DestroyIcon` it).
pub fn window_icon(w: &WinInfo) -> Option<(HICON, bool)> {
    unsafe {
        let h = util::hwnd(w.hwnd);
        let mut res: usize = 0;
        for kind in [ICON_BIG, ICON_SMALL2] {
            let ok = SendMessageTimeoutW(
                h,
                WM_GETICON,
                WPARAM(kind as usize),
                LPARAM(0),
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                40,
                Some(&mut res),
            );
            if ok.0 != 0 && res != 0 {
                return Some((HICON(res as *mut _), false));
            }
        }
        let c = GetClassLongPtrW(h, GCLP_HICON);
        if c != 0 {
            return Some((HICON(c as *mut _), false));
        }
        if !w.exe_path.is_empty() {
            let p = wide(&w.exe_path);
            let mut large = HICON::default();
            if ExtractIconExW(PCWSTR(p.as_ptr()), 0, Some(&mut large), None, 1) > 0
                && !large.is_invalid()
            {
                return Some((large, true));
            }
        }
        None
    }
}

pub unsafe fn free_icon(icon: HICON, owned: bool) {
    if owned {
        let _ = DestroyIcon(icon);
    }
}

// ---- Window actions --------------------------------------------------------

pub fn activate(h: isize) {
    unsafe {
        let h = util::hwnd(h);
        if IsIconic(h).as_bool() {
            let _ = ShowWindowAsync(h, SW_RESTORE);
        }
        if !crate::hook::alt_held() {
            // Not mid Alt+Tab (custom shortcut, mouse click): unlock the
            // foreground lock the documented way, with an Alt tap.
            crate::hook::inject_alt_tap();
        }
        let _ = SetForegroundWindow(h);
        if GetForegroundWindow() != h {
            let fg = GetForegroundWindow();
            let fg_thread = GetWindowThreadProcessId(fg, None);
            let me = GetCurrentThreadId();
            let _ = AttachThreadInput(me, fg_thread, true);
            let _ = BringWindowToTop(h);
            let _ = SetForegroundWindow(h);
            let _ = AttachThreadInput(me, fg_thread, false);
        }
        if GetForegroundWindow() != h {
            SwitchToThisWindow(h, true);
        }
    }
}

pub fn close(h: isize) {
    unsafe {
        let _ = PostMessageW(util::hwnd(h), WM_SYSCOMMAND, WPARAM(SC_CLOSE as usize), LPARAM(0));
    }
}

pub fn minimize(h: isize) {
    unsafe {
        let _ = ShowWindowAsync(util::hwnd(h), SW_MINIMIZE);
    }
}

/// Toggle always-on-top. Returns the new state.
pub fn toggle_topmost(h: isize) -> bool {
    unsafe {
        let hw = util::hwnd(h);
        let ex = GetWindowLongPtrW(hw, GWL_EXSTYLE) as u32;
        let now_top = ex & WS_EX_TOPMOST.0 == 0;
        let after = if now_top { HWND_TOPMOST } else { HWND_NOTOPMOST };
        let _ = SetWindowPos(hw, after, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        now_top
    }
}

pub fn monitor_work_area(mon: HMONITOR) -> RECT {
    unsafe {
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(mon, &mut mi);
        mi.rcWork
    }
}

pub fn toggle_maximize(h: isize) -> bool {
    unsafe {
        let hw = util::hwnd(h);
        let max = IsZoomed(hw).as_bool();
        let _ = ShowWindowAsync(hw, if max { SW_RESTORE } else { SW_MAXIMIZE });
        !max
    }
}

/// All monitors, left to right.
pub fn monitors() -> Vec<HMONITOR> {
    unsafe extern "system" fn cb(m: HMONITOR, _: HDC, _: *mut RECT, l: LPARAM) -> BOOL {
        (*(l.0 as *mut Vec<HMONITOR>)).push(m);
        true.into()
    }
    let mut v: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(HDC::default(), None, Some(cb), LPARAM(&mut v as *mut _ as isize));
    }
    v.sort_by_key(|m| {
        let r = monitor_rect(*m);
        (r.left, r.top)
    });
    v
}

pub fn monitor_rect(mon: HMONITOR) -> RECT {
    unsafe {
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        mi.rcMonitor
    }
}

/// Move a window to the next (dir = 1) or previous (dir = -1) monitor,
/// keeping its relative position and size.
pub fn move_to_monitor(h: isize, dir: i32) {
    unsafe {
        let hw = util::hwnd(h);
        let mons = monitors();
        if mons.len() < 2 {
            return;
        }
        let cur = MonitorFromWindow(hw, MONITOR_DEFAULTTONEAREST);
        let i = mons.iter().position(|m| *m == cur).unwrap_or(0) as i32;
        let target = mons[((i + dir).rem_euclid(mons.len() as i32)) as usize];
        let (from, to) = (monitor_work_area(cur), monitor_work_area(target));
        let was_max = IsZoomed(hw).as_bool();
        if was_max {
            let _ = ShowWindow(hw, SW_RESTORE);
        }
        let mut r = RECT::default();
        let _ = GetWindowRect(hw, &mut r);
        let (fw, fh) = ((from.right - from.left).max(1) as f32, (from.bottom - from.top).max(1) as f32);
        let (tw, th) = ((to.right - to.left) as f32, (to.bottom - to.top) as f32);
        let w = ((r.right - r.left) as f32).min(tw);
        let hh = ((r.bottom - r.top) as f32).min(th);
        let rx = (r.left - from.left) as f32 / fw;
        let ry = (r.top - from.top) as f32 / fh;
        let x = to.left as f32 + (rx * tw).clamp(0.0, tw - w);
        let y = to.top as f32 + (ry * th).clamp(0.0, th - hh);
        let _ = SetWindowPos(hw, HWND::default(), x as i32, y as i32, w as i32, hh as i32, SWP_NOZORDER | SWP_NOACTIVATE);
        if was_max {
            let _ = ShowWindow(hw, SW_MAXIMIZE);
        }
    }
}

pub fn open_location(w: &WinInfo) {
    if w.exe_path.is_empty() {
        return;
    }
    let _ = std::process::Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", w.exe_path))
        .spawn();
}

pub fn new_window(w: &WinInfo) {
    if !w.exe_path.is_empty() {
        util::shell_open(&w.exe_path);
    }
}
