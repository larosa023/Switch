//! Installed-app index for "launch from search".
//!
//! Built on a background thread from the shell's Apps folder (the same list
//! the Start menu uses: desktop apps and Store apps alike). If that isn't
//! available, it falls back to scanning Start menu shortcut folders.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Mutex;

use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::Win32::System::Com::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::util::{self, wide};

#[derive(Clone, Debug)]
pub struct AppEntry {
    pub name: String,
    /// Shell parsing name inside shell:AppsFolder (AUMID or path), or a
    /// shortcut path in fallback mode.
    pub target: String,
    pub from_apps_folder: bool,
}

static INDEX: Mutex<Vec<AppEntry>> = Mutex::new(Vec::new());
static BUILDING: AtomicBool = AtomicBool::new(false);

const SKIP: [&str; 9] = ["uninstall", "readme", "read me", "help", "documentation", "release notes", "license", "website", "manual"];

fn keep(name: &str) -> bool {
    let l = name.to_lowercase();
    !name.is_empty() && !SKIP.iter().any(|s| l.contains(s))
}

pub fn refresh_async() {
    if BUILDING.swap(true, Relaxed) {
        return;
    }
    std::thread::spawn(|| {
        let mut list = unsafe { from_apps_folder() };
        if list.is_empty() {
            list = from_start_menu();
        }
        list.retain(|a| keep(&a.name));
        list.sort_by_key(|a| a.name.to_lowercase());
        list.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name));
        if let Ok(mut g) = INDEX.lock() {
            *g = list;
        }
        BUILDING.store(false, Relaxed);
    });
}

unsafe fn from_apps_folder() -> Vec<AppEntry> {
    let mut out = Vec::new();
    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    let Ok(folder) = SHGetKnownFolderItem::<_, IShellItem>(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, HANDLE::default()) else {
        return out;
    };
    let Ok(en) = folder.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) else {
        return out;
    };
    loop {
        let mut items: [Option<IShellItem>; 16] = Default::default();
        let mut fetched = 0u32;
        if en.Next(&mut items, Some(&mut fetched)).is_err() || fetched == 0 {
            break;
        }
        for it in items.iter().take(fetched as usize).flatten() {
            let name = display_name(it, SIGDN_NORMALDISPLAY);
            let target = display_name(it, SIGDN_PARENTRELATIVEPARSING);
            if !name.is_empty() && !target.is_empty() {
                out.push(AppEntry { name, target, from_apps_folder: true });
            }
        }
    }
    out
}

unsafe fn display_name(it: &IShellItem, kind: SIGDN) -> String {
    match it.GetDisplayName(kind) {
        Ok(p) => {
            let s = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.0 as *const _));
            s
        }
        Err(_) => String::new(),
    }
}

fn from_start_menu() -> Vec<AppEntry> {
    let mut roots = Vec::new();
    if let Some(p) = std::env::var_os("ProgramData") {
        roots.push(std::path::PathBuf::from(p).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    if let Some(p) = std::env::var_os("APPDATA") {
        roots.push(std::path::PathBuf::from(p).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    let mut out = Vec::new();
    fn walk(dir: &std::path::Path, depth: u32, out: &mut Vec<AppEntry>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if depth < 4 {
                    walk(&p, depth + 1, out);
                }
            } else if matches!(p.extension().and_then(|x| x.to_str()).map(|x| x.to_lowercase()).as_deref(), Some("lnk" | "url")) {
                let name = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                out.push(AppEntry { name, target: p.to_string_lossy().to_string(), from_apps_folder: false });
            }
        }
    }
    for r in roots {
        walk(&r, 0, &mut out);
    }
    out
}

pub fn count() -> usize {
    INDEX.lock().map(|g| g.len()).unwrap_or(0)
}

/// Best matches for `query`: (index, score, entry), best first.
pub fn search(query: &str, max: usize) -> Vec<(usize, i32, AppEntry)> {
    let Ok(g) = INDEX.lock() else { return Vec::new() };
    let mut hits: Vec<(usize, i32, AppEntry)> = g
        .iter()
        .enumerate()
        .filter_map(|(i, a)| crate::fuzzy::match_window(query, &a.name, "", "").map(|m| (i, m.score, a.clone())))
        .collect();
    hits.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.2.name.len().cmp(&b.2.name.len())));
    hits.truncate(max);
    hits
}

pub fn launch(a: &AppEntry) {
    if a.from_apps_folder {
        util::shell_open(&format!("shell:AppsFolder\\{}", a.target));
    } else {
        util::shell_open(&a.target);
    }
}

/// Icon as a 32bpp HBITMAP (caller deletes) or an HICON (caller destroys).
pub enum AppIcon {
    Bitmap(HBITMAP),
    Icon(HICON),
}

pub fn icon(a: &AppEntry, px: i32) -> Option<AppIcon> {
    unsafe {
        if a.from_apps_folder {
            let path = wide(&format!("shell:AppsFolder\\{}", a.target));
            let item: IShellItemImageFactory = SHCreateItemFromParsingName(PCWSTR(path.as_ptr()), None).ok()?;
            let bmp = item.GetImage(SIZE { cx: px, cy: px }, SIIGBF_ICONONLY).ok()?;
            return Some(AppIcon::Bitmap(bmp));
        }
        let path = wide(&a.target);
        let mut info = SHFILEINFOW::default();
        let r = SHGetFileInfoW(
            PCWSTR(path.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        );
        (r != 0 && !info.hIcon.is_invalid()).then_some(AppIcon::Icon(info.hIcon))
    }
}
