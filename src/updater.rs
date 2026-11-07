//! Built-in updater backed by GitHub Releases.
//!
//! * Check:   GET api.github.com/repos/larosa023/Switch/releases/latest
//! * Verify:  the release must ship `Switch.exe` plus `Switch.exe.sha256`;
//!            the download is rejected unless the SHA-256 matches.
//! * Install: write `Switch.exe.new`, rename the running exe to
//!            `Switch.exe.old` (Windows allows renaming a running image),
//!            move the new one into place, relaunch with `--updated`, exit.
//!            The new process deletes the `.old` file on startup.
//!
//! Networking uses WinHTTP, so it honours the system proxy and adds no TLS
//! library to the binary.

use std::sync::Mutex;

use sha2::{Digest, Sha256};
use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::util::{self, wide};

pub const REPO: &str = "larosa023/Switch";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const WM_UPDATE_STATE: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 40;
/// lParam of WM_UPDATE_STATE when an install finished and we should restart.
pub const READY_TO_RESTART: isize = 2;

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Downloading(String),
    Error(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub exe_url: String,
    pub sha_url: Option<String>,
    pub page: String,
}

pub static STATE: Mutex<State> = Mutex::new(State::Idle);
static NOTIFY_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

pub fn state() -> State {
    STATE.lock().map(|s| s.clone()).unwrap_or(State::Idle)
}

fn set_state(s: State, lparam: isize) {
    if let Ok(mut g) = STATE.lock() {
        *g = s;
    }
    let h = NOTIFY_HWND.load(std::sync::atomic::Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(util::hwnd(h), WM_UPDATE_STATE, WPARAM(0), LPARAM(lparam));
        }
    }
}

pub fn init(notify: HWND) {
    NOTIFY_HWND.store(notify.0 as isize, std::sync::atomic::Ordering::Relaxed);
}

pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.trim().trim_start_matches(['v', 'V']).split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((it.next()??, it.next().flatten().unwrap_or(0), it.next().flatten().unwrap_or(0)))
}

pub fn is_newer(remote: &str, local: &str) -> bool {
    match (parse_version(remote), parse_version(local)) {
        (Some(r), Some(l)) => r > l,
        _ => false,
    }
}

// ---- HTTP (WinHTTP) ---------------------------------------------------------

struct Handle(*mut std::ffi::c_void);
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn split_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("https://")?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    Some((host.to_string(), path.to_string()))
}

pub fn http_get(url: &str, accept: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
    let (host, path) = split_url(url).ok_or("only https URLs are supported")?;
    unsafe {
        let agent = wide(&format!("Switch-Updater/{VERSION}"));
        let session = Handle(WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ));
        if session.0.is_null() {
            return Err("could not start WinHTTP".into());
        }
        let _ = WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 30_000);
        let whost = wide(&host);
        let conn = Handle(WinHttpConnect(session.0, PCWSTR(whost.as_ptr()), INTERNET_DEFAULT_HTTPS_PORT, 0));
        if conn.0.is_null() {
            return Err(format!("could not connect to {host}"));
        }
        let verb = wide("GET");
        let wpath = wide(&path);
        let req = Handle(WinHttpOpenRequest(
            conn.0,
            PCWSTR(verb.as_ptr()),
            PCWSTR(wpath.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if req.0.is_null() {
            return Err("could not open request".into());
        }
        let headers: Vec<u16> = format!("Accept: {accept}\r\n").encode_utf16().collect();
        WinHttpSendRequest(req.0, Some(&headers), None, 0, 0, 0).map_err(|e| format!("request failed: {e}"))?;
        WinHttpReceiveResponse(req.0, std::ptr::null_mut()).map_err(|e| format!("no response: {e}"))?;

        let mut status: u32 = 0;
        let mut len = 4u32;
        let mut index = 0u32;
        WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut _),
            &mut len,
            &mut index,
        )
        .map_err(|e| format!("bad response: {e}"))?;
        if status != 200 {
            return Err(match status {
                404 => "no published release found".into(),
                403 | 429 => "GitHub rate limit reached, try again later".into(),
                s => format!("server returned HTTP {s}"),
            });
        }

        let mut body = Vec::new();
        loop {
            let mut avail = 0u32;
            WinHttpQueryDataAvailable(req.0, &mut avail).map_err(|e| format!("read failed: {e}"))?;
            if avail == 0 {
                break;
            }
            let start = body.len();
            body.resize(start + avail as usize, 0);
            let mut read = 0u32;
            WinHttpReadData(req.0, body[start..].as_mut_ptr() as *mut _, avail, &mut read)
                .map_err(|e| format!("read failed: {e}"))?;
            body.truncate(start + read as usize);
            if body.len() > max_bytes {
                return Err("download is larger than expected".into());
            }
        }
        Ok(body)
    }
}

// ---- Check / install ----------------------------------------------------------

fn fetch_latest() -> Result<Release, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body = http_get(&url, "application/vnd.github+json", 4 * 1024 * 1024)?;
    let v: serde_json::Value = serde_json::from_slice(&body).map_err(|_| "unexpected reply from GitHub")?;
    let tag = v["tag_name"].as_str().ok_or("release has no tag")?.to_string();
    let page = v["html_url"].as_str().unwrap_or("").to_string();
    let mut exe_url = None;
    let mut sha_url = None;
    if let Some(assets) = v["assets"].as_array() {
        for a in assets {
            let name = a["name"].as_str().unwrap_or("").to_lowercase();
            let dl = a["browser_download_url"].as_str().unwrap_or("").to_string();
            if name == "switch.exe" {
                exe_url = Some(dl);
            } else if name == "switch.exe.sha256" {
                sha_url = Some(dl);
            }
        }
    }
    Ok(Release {
        version: tag.trim_start_matches(['v', 'V']).to_string(),
        exe_url: exe_url.ok_or("release has no Switch.exe asset")?,
        sha_url,
        page,
    })
}

/// Check in the background. `quiet` checks don't overwrite the state with
/// errors (so an offline laptop doesn't nag).
pub fn check_async(quiet: bool) {
    if matches!(state(), State::Checking | State::Downloading(_)) {
        return;
    }
    if !quiet {
        set_state(State::Checking, 0);
    }
    std::thread::spawn(move || match fetch_latest() {
        Ok(rel) if is_newer(&rel.version, VERSION) => set_state(State::Available(rel), 1),
        Ok(_) => set_state(State::UpToDate, 0),
        Err(e) => {
            util::log(&format!("update check failed: {e}"));
            if quiet {
                if state() == State::Checking {
                    set_state(State::Idle, 0);
                }
            } else {
                set_state(State::Error(e), 0);
            }
        }
    });
}

pub fn install_async() {
    let State::Available(rel) = state() else { return };
    set_state(State::Downloading(rel.version.clone()), 0);
    std::thread::spawn(move || match install(&rel) {
        Ok(()) => set_state(State::Downloading(rel.version.clone()), READY_TO_RESTART),
        Err(e) => {
            util::log(&format!("update install failed: {e}"));
            set_state(State::Error(e), 0);
        }
    });
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn install(rel: &Release) -> Result<(), String> {
    let sha_url = rel
        .sha_url
        .as_ref()
        .ok_or("release is missing Switch.exe.sha256, refusing to install an unverified file")?;
    let expected = String::from_utf8_lossy(&http_get(sha_url, "application/octet-stream", 4096)?)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase();
    if expected.len() != 64 {
        return Err("checksum file is malformed".into());
    }
    let data = http_get(&rel.exe_url, "application/octet-stream", 64 * 1024 * 1024)?;
    if data.len() < 1024 || &data[..2] != b"MZ" {
        return Err("downloaded file is not a Windows executable".into());
    }
    let actual = hex(&Sha256::digest(&data));
    if actual != expected {
        return Err("checksum mismatch, the download was discarded".into());
    }

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let new = exe.with_file_name(format!("{}.new", exe.file_name().unwrap().to_string_lossy()));
    let old = exe.with_file_name(format!("{}.old", exe.file_name().unwrap().to_string_lossy()));
    std::fs::write(&new, &data).map_err(|e| {
        format!("can't write to {} ({e}). Move Switch to a folder you own, e.g. %LOCALAPPDATA%\\Programs\\Switch", exe.parent().unwrap().display())
    })?;
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).map_err(|e| format!("couldn't move the current version aside: {e}"))?;
    if let Err(e) = std::fs::rename(&new, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(format!("couldn't put the new version in place: {e}"));
    }
    Ok(())
}

/// Start the freshly installed exe. Called on the UI thread right before exit.
pub fn relaunch() {
    if let Ok(exe) = std::env::current_exe() {
        // current_exe() still resolves to the original path, which now holds the new build.
        let _ = std::process::Command::new(exe).arg("--updated").spawn();
    }
}

/// After an update: remove the previous version once its process has exited.
pub fn cleanup_old() {
    std::thread::spawn(|| {
        if let Ok(exe) = std::env::current_exe() {
            let old = exe.with_file_name(format!("{}.old", exe.file_name().unwrap().to_string_lossy()));
            for _ in 0..30 {
                if !old.exists() || std::fs::remove_file(&old).is_ok() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions() {
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0-beta", "0.1.0"));
        assert!(!is_newer("garbage", "0.1.0"));
    }
}
