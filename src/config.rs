//! User settings, persisted as JSON in %APPDATA%\Switch\settings.json.
//!
//! Every field has a default, so older or hand-edited files keep loading;
//! unknown fields are ignored.

use serde::{Deserialize, Serialize};
use windows::Win32::System::Registry::HKEY_CURRENT_USER;

use crate::util::{self, UiCell};

macro_rules! choice {
    ($name:ident { $($v:ident),+ $(,)? }) => {
        #[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
        #[serde(rename_all = "lowercase")]
        pub enum $name { $($v),+ }
        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$v),+];
            pub fn index(self) -> usize { Self::ALL.iter().position(|v| *v == self).unwrap_or(0) }
            pub fn from_index(i: usize) -> Self { Self::ALL[i.min(Self::ALL.len() - 1)] }
        }
    };
}

choice!(Theme { System, Light, Dark, Black });
choice!(Layout { Grid, Row, List });
choice!(Monitors { All, Current });
choice!(OpenOn { Cursor, Active, Primary });
choice!(Grouping { Off, Cluster, Collapse });
choice!(Sort { Recent, App, Title });
choice!(Initial { Previous, Current });
choice!(Label { Title, App, Both });
choice!(SearchBar { Always, Typing });
choice!(Highlight { Outline, Fill, Both });
choice!(Position { Center, Top, Bottom });
choice!(TextSize { Small, Default, Large });
choice!(AnimSpeed { Off, Fast, Normal, Relaxed });
choice!(TrayClick { Settings, Switcher });

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
    // ---- Shortcuts
    /// Take over Alt+Tab / Shift+Alt+Tab / Ctrl+Alt+Tab.
    pub alt_tab: bool,
    /// Releasing Alt switches (classic). Off = the switcher stays open until Enter.
    pub release_to_switch: bool,
    /// Alt+` cycles windows of the current app.
    pub alt_backtick: bool,
    /// Optional extra shortcut that opens the switcher.
    pub hotkey: Option<Hotkey>,
    /// Delay before the switcher becomes visible, so a quick Alt+Tab tap
    /// flips windows without flashing the UI.
    pub show_delay_ms: u32,

    // ---- Appearance
    pub theme: Theme,
    /// "system" or "#RRGGBB".
    pub accent: String,
    pub corner_radius: u32,
    pub text_size: TextSize,
    pub highlight: Highlight,
    pub dim_background: bool,
    /// 0-80 (%).
    pub dim_amount: u32,
    pub anim_speed: AnimSpeed,

    // ---- Layout
    pub layout: Layout,
    /// Thumbnail height in grid/row layouts, at 100% scaling.
    pub preview_px: u32,
    /// List layout: show a large live preview of the selected window.
    pub list_preview: bool,
    pub position: Position,
    pub label: Label,
    pub show_icons: bool,
    pub search_bar: SearchBar,
    pub show_hints: bool,

    // ---- Windows
    pub monitors: Monitors,
    pub open_on: OpenOn,
    pub grouping: Grouping,
    pub sort: Sort,
    pub initial: Initial,
    pub wrap: bool,
    pub show_minimized: bool,
    pub other_desktops: bool,
    pub hover_select: bool,
    pub action_buttons: bool,
    /// Lower-case executable names, e.g. "spotify.exe".
    pub hidden_apps: Vec<String>,

    // ---- General
    pub tray_click: TrayClick,
    pub trim_memory: bool,

    // ---- Updates
    pub auto_update: bool,
    pub notify_updates: bool,
    pub auto_install: bool,

    // ---- v0.1 fields, read once for migration and never written back.
    #[serde(skip_serializing)]
    animations: Option<bool>,
    #[serde(skip_serializing)]
    group_by_app: Option<bool>,
    #[serde(skip_serializing)]
    preview_size: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            alt_tab: true,
            release_to_switch: true,
            alt_backtick: true,
            hotkey: None,
            show_delay_ms: 40,
            theme: Theme::System,
            accent: "system".into(),
            corner_radius: 12,
            text_size: TextSize::Default,
            highlight: Highlight::Both,
            dim_background: false,
            dim_amount: 30,
            anim_speed: AnimSpeed::Normal,
            layout: Layout::Grid,
            preview_px: 150,
            list_preview: true,
            position: Position::Center,
            label: Label::Title,
            show_icons: true,
            search_bar: SearchBar::Always,
            show_hints: true,
            monitors: Monitors::All,
            open_on: OpenOn::Cursor,
            grouping: Grouping::Off,
            sort: Sort::Recent,
            initial: Initial::Previous,
            wrap: true,
            show_minimized: true,
            other_desktops: false,
            hover_select: false,
            action_buttons: true,
            hidden_apps: Vec::new(),
            tray_click: TrayClick::Settings,
            trim_memory: true,
            auto_update: true,
            notify_updates: true,
            auto_install: false,
            animations: None,
            group_by_app: None,
            preview_size: None,
        }
    }
}

impl Config {
    fn migrate(&mut self) {
        if let Some(a) = self.animations.take() {
            if !a {
                self.anim_speed = AnimSpeed::Off;
            }
        }
        if let Some(g) = self.group_by_app.take() {
            if g {
                self.grouping = Grouping::Cluster;
            }
        }
        if let Some(s) = self.preview_size.take() {
            self.preview_px = match s.as_str() {
                "small" => 112,
                "large" => 200,
                _ => 150,
            };
        }
        self.preview_px = self.preview_px.clamp(80, 320);
        self.corner_radius = self.corner_radius.min(24);
        self.dim_amount = self.dim_amount.min(80);
        self.show_delay_ms = self.show_delay_ms.min(500);
    }

    /// Animation duration multiplier (0 = off).
    pub fn anim_factor(&self) -> f32 {
        match self.anim_speed {
            AnimSpeed::Off => 0.0,
            AnimSpeed::Fast => 0.6,
            AnimSpeed::Normal => 1.0,
            AnimSpeed::Relaxed => 1.6,
        }
    }

    pub fn text_scale(&self) -> f32 {
        match self.text_size {
            TextSize::Small => 0.9,
            TextSize::Default => 1.0,
            TextSize::Large => 1.15,
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
        Ok(s) => match serde_json::from_str::<Config>(&s) {
            Ok(mut c) => {
                c.migrate();
                (c, false)
            }
            Err(e) => {
                util::log(&format!("settings.json is invalid ({e}); using defaults"));
                let _ = std::fs::copy(path(), path().with_extension("json.bad"));
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

pub fn reset() {
    *cfg() = Config::default();
    save();
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
