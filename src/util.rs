//! Small helpers shared across the app: wide strings, a single-threaded global
//! cell for UI state, logging and registry access.

use std::cell::UnsafeCell;
use std::io::Write;
use std::path::PathBuf;

use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::System::Registry::*;

/// Null-terminated UTF-16 buffer.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// UTF-16 without terminator (for DirectWrite draw calls).
pub fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Global state that is only ever touched from the UI thread.
///
/// Win32 window procedures re-enter freely (e.g. `SetWindowPos` sends
/// `WM_SIZE` synchronously), which makes `RefCell` panic-prone. All UI state
/// lives on the main thread, so a plain cell is the pragmatic choice.
pub struct UiCell<T>(UnsafeCell<Option<T>>);
unsafe impl<T> Sync for UiCell<T> {}
impl<T> UiCell<T> {
    pub const fn new() -> Self {
        Self(UnsafeCell::new(None))
    }
    pub fn set(&self, v: T) {
        unsafe { *self.0.get() = Some(v) }
    }
    #[allow(clippy::mut_from_ref)]
    pub fn get(&self) -> &mut T {
        unsafe { (*self.0.get()).as_mut().expect("UiCell not initialised") }
    }
    #[allow(clippy::mut_from_ref)]
    pub fn try_get(&self) -> Option<&mut T> {
        unsafe { (*self.0.get()).as_mut() }
    }
}

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("Switch");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Append a line to %APPDATA%\Switch\switch.log (kept under ~256 KB).
pub fn log(msg: &str) {
    let path = data_dir().join("switch.log");
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > 256 * 1024 {
            let _ = std::fs::remove_file(&path);
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{secs}] {msg}");
    }
}

pub fn reg_get_dword(root: HKEY, path: &str, name: &str) -> Option<u32> {
    let p = wide(path);
    let n = wide(name);
    let mut val: u32 = 0;
    let mut size = 4u32;
    let r = unsafe {
        RegGetValueW(
            root,
            PCWSTR(p.as_ptr()),
            PCWSTR(n.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut val as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    (r == ERROR_SUCCESS).then_some(val)
}

pub fn reg_get_string(root: HKEY, path: &str, name: &str) -> Option<String> {
    let p = wide(path);
    let n = wide(name);
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(
            root,
            PCWSTR(p.as_ptr()),
            PCWSTR(n.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    (r == ERROR_SUCCESS).then(|| from_wide(&buf))
}

pub fn reg_set_string(root: HKEY, path: &str, name: &str, value: &str) -> bool {
    unsafe {
        let p = wide(path);
        let mut key = HKEY::default();
        if RegCreateKeyExW(
            root,
            PCWSTR(p.as_ptr()),
            0,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        ) != ERROR_SUCCESS
        {
            return false;
        }
        let n = wide(name);
        let v = wide(value);
        let bytes = std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len() * 2);
        let ok = RegSetValueExW(key, PCWSTR(n.as_ptr()), 0, REG_SZ, Some(bytes)) == ERROR_SUCCESS;
        let _ = RegCloseKey(key);
        ok
    }
}

pub fn reg_delete_value(root: HKEY, path: &str, name: &str) {
    unsafe {
        let p = wide(path);
        let mut key = HKEY::default();
        if RegOpenKeyExW(root, PCWSTR(p.as_ptr()), 0, KEY_SET_VALUE, &mut key) == ERROR_SUCCESS {
            let n = wide(name);
            let _ = RegDeleteValueW(key, PCWSTR(n.as_ptr()));
            let _ = RegCloseKey(key);
        }
    }
}

/// Open a file or URL with the shell.
pub fn shell_open(target: &str) {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let t = wide(target);
    let op = wide("open");
    unsafe {
        ShellExecuteW(
            HWND::default(),
            PCWSTR(op.as_ptr()),
            PCWSTR(t.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

#[inline]
pub fn hwnd(v: isize) -> HWND {
    HWND(v as *mut _)
}
