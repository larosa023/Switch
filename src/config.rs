//! User settings, persisted as JSON in %APPDATA%\Switch\settings.json.

use serde::{Deserialize, Serialize};
use windows::Win32::System::Registry::HKEY_CURRENT_USER;

use crate::util::{self, UiCell};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    System,
    Light,
    Dark,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    Grid,
    Row,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum PreviewSize {
    Small,
    Medium,
    Large,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Monitors {
    /// Only windows on the monitor the switcher opens on (the one with the cursor).
    Current,
    All,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub vk: u32,
}

impl Hotkey {
    pub fn pack(&self) -> u32 {
        (self.vk & 0xFFFF)
            | (self.ctrl as u32) << 16
            | (self.alt as u32) << 17
            | (self.shift as u32) << 18
            | (self.win as u32) << 19
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    /// Take over Alt+Tab / Shift+Alt+Tab.
    pub alt_tab: bool,
    /// Optional extra shortcut that opens the switcher.
    pub hotkey: Option<Hotkey>,
    pub theme: Theme,
    /// "system" or a "#RRGGBB" color.
    pub accent: String,
    pub layout: Layout,
    pub preview_size: PreviewSize,
    pub animations: bool,
    pub monitors: Monitors,
    pub group_by_app: bool,
    pub show_minimized: bool,
    /// Lower-case executable names, e.g. "spotify.exe".
    pub hidden_apps: Vec<String>,
    pub auto_update: bool,
    /// Delay before the switcher becomes visible, so a quick Alt+Tab tap
    /// flips windows without flashing the UI.
    pub show_delay_ms: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            alt_tab: true,
            hotkey: None,
            theme: Theme::System,
            accent: "system".into(),
            layout: Layout::Grid,
            preview_size: PreviewSize::Medium,
            animations: true,
            monitors: Monitors::All,
            group_by_app: false,
            show_minimized: true,
            hidden_apps: Vec::new(),
            auto_update: true,
            show_delay_ms: 40,
        }
    }
}

pub static CONFIG: UiCell<Config> = UiCell::new();

pub fn cfg() -> &'static mut Config {
    CONFIG.get()
}

pub fn path() -> std::path::PathBuf {
    util::data_dir().join("settings.json")
}

/// Returns (config, first_run).
pub fn load() -> (Config, bool) {
    match std::fs::read_to_string(path()) {
        Ok(s) => match serde_json::from_str(&s) {
            Ok(c) => (c, false),
            Err(e) => {
                util::log(&format!("settings.json is invalid ({e}); using defaults"));
                (Config::default(), false)
            }
        },
        Err(_) => (Config::default(), true),
    }
}

pub fn save() {
    if let Ok(s) = serde_json::to_string_pretty(cfg()) {
        let tmp = path().with_extension("json.tmp");
        if std::fs::write(&tmp, s).is_ok() {
            let _ = std::fs::rename(&tmp, path());
        }
    }
}

// ---- Start with Windows (HKCU Run key) -------------------------------------

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_NAME: &str = "Switch";

fn run_command() -> String {
    let exe = std::env::current_exe().unwrap_or_default();
    format!("\"{}\" --background", exe.display())
}

pub fn autostart_enabled() -> bool {
    util::reg_get_string(HKEY_CURRENT_USER, RUN_KEY, RUN_NAME).is_some()
}

pub fn set_autostart(on: bool) {
    if on {
        util::reg_set_string(HKEY_CURRENT_USER, RUN_KEY, RUN_NAME, &run_command());
    } else {
        util::reg_delete_value(HKEY_CURRENT_USER, RUN_KEY, RUN_NAME);
    }
}

/// Keep the Run entry pointing at the current exe if the user moved it.
pub fn refresh_autostart_path() {
    if let Some(v) = util::reg_get_string(HKEY_CURRENT_USER, RUN_KEY, RUN_NAME) {
        if v != run_command() {
            set_autostart(true);
        }
    }
}
