//! Settings window: a custom-drawn Direct2D window with a sidebar of pages,
//! so it matches the switcher's look (no dated common controls).
//!
//! It's immediate-mode: every paint rebuilds the page from the current
//! config and records hit regions; clicks map back to config changes.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_U;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::*;
use windows::Win32::System::ProcessStatus::*;
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::*;
use crate::gfx::{self, gfx, Painter, Palette, C, R};
use crate::hook;
use crate::updater::{self, State};
use crate::util::{self, UiCell};

// ---- model ---------------------------------------------------------------------

const PAGES: [&str; 8] = ["General", "Shortcuts", "Appearance", "Layout", "Windows", "Hidden apps", "Updates", "About"];
const PAGE_DESC: [&str; 8] = [
    "Startup, tray and system behavior",
    "How you open and drive the switcher",
    "Theme, color, shape and motion",
    "How windows are arranged and labeled",
    "Which windows show up, and in what order",
    "Apps that never appear in Switch",
    "Stay on the latest version",
    "Version, resources and troubleshooting",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum K {
    Enabled,
    Autostart,
    TrayClick,
    TrimMemory,
    RunAsAdmin,
    AltTab,
    Release,
    Backtick,
    Delay,
    Theme,
    AccentHex,
    Radius,
    TextSize,
    Highlight,
    Anim,
    Dim,
    DimAmount,
    Layout,
    PreviewPx,
    ListPreview,
    Position,
    Label,
    Icons,
    SearchBar,
    Hints,
    Monitors,
    OpenOn,
    Grouping,
    Sort,
    Initial,
    Wrap,
    Minimized,
    Desktops,
    HoverSelect,
    ActionButtons,
    AddHidden,
    AutoUpdate,
    Notify,
    AutoInstall,
    UpdateNow,
    OpenRepo,
    OpenReleases,
    OpenFolder,
    OpenLog,
    Reset,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Nav(usize),
    Toggle(K),
    Seg(K, usize),
    Slider(K),
    Accent(usize),
    Hotkey,
    HotkeyClear,
    Button(K),
    Field(K),
    Unhide(usize),
    HideRunning(usize),
}

enum Ctl {
    None,
    Toggle(K, bool),
    Seg(K, &'static [&'static str], usize),
    Slider(K, u32, u32, u32, &'static str),
    Swatches,
    Hotkey,
    Button(K, String, bool),
    Field(K, &'static str),
    Info(String),
}

struct Row {
    label: String,
    desc: String,
    ctl: Ctl,
}

fn row(label: &str, desc: &str, ctl: Ctl) -> Row {
    Row { label: label.into(), desc: desc.into(), ctl }
}

enum Block {
    Section(&'static str),
    Card(Vec<Row>),
    Preview,
    Cheats,
    Hidden,
    Running,
    Notes,
    AboutHead,
    Gap(f32),
}

struct Fmts {
    h1: IDWriteTextFormat,
    brand: IDWriteTextFormat,
    label: IDWriteTextFormat,
    desc: IDWriteTextFormat,
    section: IDWriteTextFormat,
    ctl: IDWriteTextFormat,
    ctl_c: IDWriteTextFormat,
    ctl_r: IDWriteTextFormat,
    small: IDWriteTextFormat,
    wrap: IDWriteTextFormat,
    nav: IDWriteTextFormat,
}

struct Settings {
    hwnd: HWND,
    rt: Option<ID2D1HwndRenderTarget>,
    rtb: Option<ID2D1RenderTarget>,
    brush: Option<ID2D1SolidColorBrush>,
    brush2: Option<ID2D1SolidColorBrush>,
    fmts: Option<Fmts>,
    scale: f32,
    page: usize,
    scroll: f32,
    content_h: f32,
    hits: Vec<(R, Hit)>,
    hover: Option<Hit>,
    recording: bool,
    tracking: bool,
    dragging: Option<(K, R, u32, u32)>,
    field: Option<(K, String)>,
    confirm_reset: bool,
    running: Vec<(String, String)>,
}

static ST: UiCell<Settings> = UiCell::new();

const W: f32 = 900.0;
const H: f32 = 660.0;
const SIDEBAR: f32 = 212.0;
const TIMER_REFRESH: usize = 1;

pub fn show() {
    unsafe {
        if let Some(s) = ST.try_get() {
            if !s.hwnd.is_invalid() && IsWindow(s.hwnd).as_bool() {
                let _ = ShowWindow(s.hwnd, SW_RESTORE);
                let _ = SetForegroundWindow(s.hwnd);
                return;
            }
        }
        if let Err(e) = create() {
            util::log(&format!("settings window failed: {e}"));
        }
    }
}

/// Repaint if open (called when config or update state changes elsewhere).
pub fn refresh() {
    if let Some(s) = ST.try_get() {
        unsafe {
            if !s.hwnd.is_invalid() && IsWindow(s.hwnd).as_bool() {
                let _ = InvalidateRect(s.hwnd, None, false);
            }
        }
    }
}

unsafe fn create() -> windows::core::Result<()> {
    let inst = GetModuleHandleW(None)?;
    let icon = LoadImageW(inst, PCWSTR(1 as *const u16), IMAGE_ICON, 0, 0, LR_DEFAULTSIZE | LR_SHARED)
        .map(|h| HICON(h.0))
        .unwrap_or_default();
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: inst.into(),
        hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW)?,
        hIcon: icon,
        hIconSm: icon,
        lpszClassName: w!("Switch.Settings"),
        ..Default::default()
    };
    RegisterClassExW(&wc);

    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
    let (mut dx, mut dy) = (96u32, 96u32);
    let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
    let scale = dx as f32 / 96.0;
    let work = crate::winlist::monitor_work_area(mon);
    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
    let cw = (W * scale).min((work.right - work.left) as f32 * 0.95);
    let ch = (H * scale).min((work.bottom - work.top) as f32 * 0.9);
    let mut rc = RECT { left: 0, top: 0, right: cw as i32, bottom: ch as i32 };
    let _ = AdjustWindowRectExForDpi(&mut rc, style, false, WINDOW_EX_STYLE(0), dx);
    let (ww, wh) = (rc.right - rc.left, rc.bottom - rc.top);
    let x = work.left + ((work.right - work.left) - ww) / 2;
    let y = work.top + ((work.bottom - work.top) - wh) / 2;

    let page = ST.try_get().map(|s| s.page).unwrap_or(0);
    ST.set(Settings {
        hwnd: HWND::default(),
        rt: None,
        rtb: None,
        brush: None,
        brush2: None,
        fmts: None,
        scale,
        page,
        scroll: 0.0,
        content_h: 0.0,
        hits: Vec::new(),
        hover: None,
        recording: false,
        tracking: false,
        dragging: None,
        field: None,
        confirm_reset: false,
        running: Vec::new(),
    });
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("Switch.Settings"),
        w!("Switch Settings"),
        style,
        x,
        y,
        ww,
        wh,
        HWND::default(),
        HMENU::default(),
        inst,
        None,
    )?;
    ST.get().hwnd = hwnd;
    apply_titlebar(hwnd);
    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);
    Ok(())
}

unsafe fn apply_titlebar(hwnd: HWND) {
    let pal = gfx::palette(cfg());
    let dark: BOOL = pal.dark.into();
    let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, 4);
    let c = pal.surface.mix(pal.bg, if pal.dark { 0.5 } else { 0.0 });
    let cref = COLORREF(((c.2 * 255.0) as u32) << 16 | ((c.1 * 255.0) as u32) << 8 | (c.0 * 255.0) as u32);
    let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, &cref as *const _ as *const _, 4);
}

// ---- hotkey helpers -------------------------------------------------------------

fn hotkey_label(h: &Hotkey) -> String {
    let mut parts: Vec<String> = Vec::new();
    if h.ctrl {
        parts.push("Ctrl".into());
    }
    if h.win {
        parts.push("Win".into());
    }
    if h.alt {
        parts.push("Alt".into());
    }
    if h.shift {
        parts.push("Shift".into());
    }
    parts.push(key_name(h.vk));
    parts.join(" + ")
}

fn key_name(vk: u32) -> String {
    match vk {
        0x30..=0x39 | 0x41..=0x5A => char::from(vk as u8).to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        0x20 => "Space".into(),
        0x09 => "Tab".into(),
        0x0D => "Enter".into(),
        0xC0 => "`".into(),
        _ => unsafe {
            let mut scan = MapVirtualKeyW(vk, MAPVK_VK_TO_VSC) as i32;
            if matches!(vk, 0x21..=0x2E) {
                scan |= 0x100; // extended (nav cluster)
            }
            let mut buf = [0u16; 32];
            let n = GetKeyNameTextW(scan << 16, &mut buf);
            if n > 0 {
                String::from_utf16_lossy(&buf[..n as usize])
            } else {
                format!("Key {vk}")
            }
        },
    }
}

fn is_mod(vk: u32) -> bool {
    matches!(vk, 0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0..=0xA5)
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { (GetKeyState(vk.0 as i32) as u16 & 0x8000) != 0 }
}

fn current_mods() -> Hotkey {
    Hotkey {
        ctrl: key_down(VK_CONTROL),
        alt: key_down(VK_MENU),
        shift: key_down(VK_SHIFT),
        win: key_down(VK_LWIN) || key_down(VK_RWIN),
        vk: 0,
    }
}

fn clipboard_text() -> Option<String> {
    unsafe {
        OpenClipboard(HWND::default()).ok()?;
        let mut out = None;
        if let Ok(h) = GetClipboardData(13 /* CF_UNICODETEXT */) {
            let p = GlobalLock(HGLOBAL(h.0)) as *const u16;
            if !p.is_null() {
                let mut n = 0;
                while *p.add(n) != 0 && n < 4096 {
                    n += 1;
                }
                out = Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, n)));
                let _ = GlobalUnlock(HGLOBAL(h.0));
            }
        }
        let _ = CloseClipboard();
        out
    }
}

fn memory_usage() -> (f64, f64) {
    unsafe {
        let mut c = PROCESS_MEMORY_COUNTERS_EX::default();
        let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c as *mut _ as *mut PROCESS_MEMORY_COUNTERS, size).as_bool() {
            (c.WorkingSetSize as f64 / 1048576.0, c.PrivateUsage as f64 / 1048576.0)
        } else {
            (0.0, 0.0)
        }
    }
}

// ---- pages ----------------------------------------------------------------------

fn page_blocks(page: usize, confirm_reset: bool) -> Vec<Block> {
    let c = cfg().clone();
    let paused = hook::PAUSED.load(std::sync::atomic::Ordering::Relaxed);
    match page {
        0 => vec![
            Block::Card(vec![
                row("Switch is on", "Turn off to bring back the standard Windows Alt+Tab", Ctl::Toggle(K::Enabled, !paused)),
                row("Start with Windows", "Run Switch in the tray when you sign in", Ctl::Toggle(K::Autostart, autostart_enabled())),
                row("Tray icon click", "What a left-click on the tray icon does", Ctl::Seg(K::TrayClick, &["Settings", "Open switcher"], c.tray_click.index())),
            ]),
            Block::Section("Performance"),
            Block::Card(vec![row(
                "Release memory when idle",
                "Hands unused memory back to Windows 10 seconds after the switcher closes",
                Ctl::Toggle(K::TrimMemory, c.trim_memory),
            )]),
            Block::Section("Administrator windows"),
            Block::Card(vec![row(
                if crate::is_elevated() { "Running as administrator" } else { "Run as administrator" },
                if crate::is_elevated() {
                    "Alt+Tab works over elevated apps too"
                } else {
                    "Windows blocks Alt+Tab capture while an elevated app is focused"
                },
                if crate::is_elevated() {
                    Ctl::Info("Active".into())
                } else {
                    Ctl::Button(K::RunAsAdmin, "Restart as admin".into(), false)
                },
            )]),
        ],
        1 => vec![
            Block::Card(vec![
                row("Replace Alt+Tab", "Alt+Tab, Shift+Alt+Tab to go back, Ctrl+Alt+Tab keeps it open", Ctl::Toggle(K::AltTab, c.alt_tab)),
                row("Release Alt to switch", "Off: the switcher stays open until Enter, Esc or a click", Ctl::Toggle(K::Release, c.release_to_switch)),
                row("Alt+` for the current app", "Cycle between windows of the app you're in", Ctl::Toggle(K::Backtick, c.alt_backtick)),
                row("Custom shortcut", "Shortcuts with Alt switch on release; others stay open", Ctl::Hotkey),
                row(
                    "Quick-switch delay",
                    "A tap shorter than this flips windows without showing the switcher",
                    Ctl::Slider(K::Delay, 0, 250, c.show_delay_ms, " ms"),
                ),
            ]),
            Block::Section("Inside the switcher"),
            Block::Cheats,
        ],
        2 => vec![
            Block::Preview,
            Block::Gap(14.0),
            Block::Card(vec![
                row("Theme", "", Ctl::Seg(K::Theme, &["System", "Light", "Dark", "Black"], c.theme.index())),
                row("Accent color", "", Ctl::Swatches),
                row("Custom accent", "Any hex color, e.g. #FF6A00", Ctl::Field(K::AccentHex, "#RRGGBB")),
            ]),
            Block::Section("Shape and text"),
            Block::Card(vec![
                row("Corner radius", "", Ctl::Slider(K::Radius, 0, 24, c.corner_radius, " px")),
                row("Text size", "", Ctl::Seg(K::TextSize, &["Small", "Default", "Large"], c.text_size.index())),
                row("Selection style", "", Ctl::Seg(K::Highlight, &["Outline", "Fill", "Both"], c.highlight.index())),
            ]),
            Block::Section("Motion and backdrop"),
            Block::Card(vec![
                row("Animations", "Fade-in, zoom and the gliding selection", Ctl::Seg(K::Anim, &["Off", "Fast", "Normal", "Relaxed"], c.anim_speed.index())),
                row("Dim the desktop", "Darken the screen behind the switcher", Ctl::Toggle(K::Dim, c.dim_background)),
                row("Dim strength", "", Ctl::Slider(K::DimAmount, 10, 80, c.dim_amount, "%")),
            ]),
        ],
        3 => vec![
            Block::Preview,
            Block::Gap(14.0),
            Block::Card(vec![
                row("Layout", "Ctrl+L cycles layouts in the switcher", Ctl::Seg(K::Layout, &["Grid", "Row", "List"], c.layout.index())),
                row("Preview size", "Grid and row layouts", Ctl::Slider(K::PreviewPx, 80, 320, c.preview_px, " px")),
                row("Large preview in list", "Show a live preview of the selected window next to the list", Ctl::Toggle(K::ListPreview, c.list_preview)),
                row("Position on screen", "", Ctl::Seg(K::Position, &["Center", "Top", "Bottom"], c.position.index())),
            ]),
            Block::Section("Details"),
            Block::Card(vec![
                row("Window labels", "", Ctl::Seg(K::Label, &["Title", "App name", "Both"], c.label.index())),
                row("App icons", "", Ctl::Toggle(K::Icons, c.show_icons)),
                row("Search bar", "", Ctl::Seg(K::SearchBar, &["Always", "When typing"], c.search_bar.index())),
                row("Keyboard hints", "A one-line cheat sheet along the bottom", Ctl::Toggle(K::Hints, c.show_hints)),
            ]),
        ],
        4 => vec![
            Block::Card(vec![
                row("Show windows from", "", Ctl::Seg(K::Monitors, &["All monitors", "Current monitor"], c.monitors.index())),
                row("Open on", "Which monitor the switcher appears on", Ctl::Seg(K::OpenOn, &["Cursor", "Active window", "Primary"], c.open_on.index())),
                row("Other virtual desktops", "Include windows from your other desktops", Ctl::Toggle(K::Desktops, c.other_desktops)),
                row("Minimized windows", "", Ctl::Toggle(K::Minimized, c.show_minimized)),
            ]),
            Block::Section("Order"),
            Block::Card(vec![
                row("Group by app", "Ctrl+G cycles grouping. Collapse shows one tile per app", Ctl::Seg(K::Grouping, &["Off", "Together", "Collapse"], c.grouping.index())),
                row("Sort by", "Search results are always ranked by match", Ctl::Seg(K::Sort, &["Recent", "App", "Title"], c.sort.index())),
                row("Start on", "Which window is selected when the switcher opens", Ctl::Seg(K::Initial, &["Previous", "Current"], c.initial.index())),
                row("Wrap around", "Moving past the last window jumps to the first", Ctl::Toggle(K::Wrap, c.wrap)),
            ]),
            Block::Section("Mouse"),
            Block::Card(vec![
                row("Select on hover", "Pointing at a window selects it", Ctl::Toggle(K::HoverSelect, c.hover_select)),
                row("Action buttons", "Pin, minimize and close on the hovered window", Ctl::Toggle(K::ActionButtons, c.action_buttons)),
            ]),
        ],
        5 => vec![
            Block::Hidden,
            Block::Gap(10.0),
            Block::Card(vec![row("Hide an app by name", "Type the program's file name and press Enter", Ctl::Field(K::AddHidden, "spotify.exe"))]),
            Block::Section("Running apps"),
            Block::Running,
        ],
        6 => vec![
            Block::Card(vec![row("", "status", Ctl::None)]),
            Block::Notes,
            Block::Section("Preferences"),
            Block::Card(vec![
                row("Check for updates automatically", "On startup and every 6 hours, from GitHub Releases", Ctl::Toggle(K::AutoUpdate, c.auto_update)),
                row("Notify me", "Show a notification when a new version is out", Ctl::Toggle(K::Notify, c.notify_updates)),
                row("Install updates automatically", "Download, verify and restart without asking", Ctl::Toggle(K::AutoInstall, c.auto_install)),
            ]),
        ],
        _ => {
            let (ws, private) = memory_usage();
            vec![
                Block::AboutHead,
                Block::Section("Resources"),
                Block::Card(vec![
                    row("Memory in use", "Working set, live", Ctl::Info(format!("{ws:.1} MB"))),
                    row("Private memory", "", Ctl::Info(format!("{private:.1} MB"))),
                    row("Process", "", Ctl::Info(if crate::is_elevated() { "Administrator".into() } else { "Standard user".into() })),
                ]),
                Block::Section("Troubleshooting"),
                Block::Card(vec![
                    row("Settings and log folder", "%APPDATA%\\Switch", Ctl::Button(K::OpenFolder, "Open folder".into(), false)),
                    row("Log file", "Errors and update activity", Ctl::Button(K::OpenLog, "Open log".into(), false)),
                    row(
                        "Reset all settings",
                        "Back to defaults. Hidden apps are cleared too",
                        Ctl::Button(K::Reset, if confirm_reset { "Click again to reset".into() } else { "Reset".into() }, false),
                    ),
                ]),
                Block::Section("Project"),
                Block::Card(vec![
                    row("Source code", "github.com/larosa023/Switch", Ctl::Button(K::OpenRepo, "Open GitHub".into(), false)),
                    row("Release history", "", Ctl::Button(K::OpenReleases, "View releases".into(), false)),
                ]),
            ]
        }
    }
}

// ---- drawing ----------------------------------------------------------------------

impl Settings {
    fn ensure(&mut self) {
        if self.rt.is_none() {
            let mut rc = RECT::default();
            unsafe {
                let _ = GetClientRect(self.hwnd, &mut rc);
            }
            if let Ok(rt) = gfx().create_target(self.hwnd, rc.right as u32, rc.bottom as u32) {
                let b = gfx::as_rt(&rt);
                self.brush = gfx::make_brush(&b).ok();
                self.brush2 = gfx::make_brush(&b).ok();
                self.rtb = Some(b);
                self.rt = Some(rt);
            }
        }
        if self.fmts.is_none() {
            let s = self.scale;
            let g = gfx();
            let mk = |sz: f32, wt, al| g.text_format(sz * s, wt, al).unwrap();
            self.fmts = Some(Fmts {
                h1: mk(24.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING),
                brand: mk(17.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING),
                label: mk(14.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING),
                desc: mk(12.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING),
                section: mk(13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING),
                ctl: mk(13.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING),
                ctl_c: mk(13.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER),
                ctl_r: mk(13.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_TRAILING),
                small: mk(12.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING),
                wrap: g.text_format_ex(12.5 * s, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING, true).unwrap(),
                nav: mk(13.5, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING),
            });
        }
    }

    fn render(&mut self) {
        self.ensure();
        let (Some(rt), Some(rtb), Some(brush), Some(brush2)) =
            (self.rt.clone(), self.rtb.clone(), self.brush.clone(), self.brush2.clone())
        else {
            return;
        };
        let pal = gfx::palette(cfg());
        let p = Painter { rt: &rtb, brush: &brush, brush2: &brush2, alpha: 1.0 };
        unsafe {
            rt.BeginDraw();
            rt.Clear(Some(&pal.bg.d2d()));
        }
        self.hits.clear();
        let f = self.fmts.take().unwrap();
        self.draw_content(&p, &pal, &f);
        self.draw_sidebar(&p, &pal, &f);
        self.fmts = Some(f);
        unsafe {
            if let Err(e) = rt.EndDraw(None, None) {
                if e.code() == D2DERR_RECREATE_TARGET {
                    self.rt = None;
                    self.rtb = None;
                    self.brush = None;
                    self.brush2 = None;
                }
            }
            let _ = ValidateRect(self.hwnd, None);
        }
    }

    fn client_h(&self) -> f32 {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        rc.bottom as f32
    }

    fn draw_sidebar(&mut self, p: &Painter, pal: &Palette, f: &Fmts) {
        let s = self.scale;
        let h = self.client_h();
        let side = R::new(0.0, 0.0, SIDEBAR * s, h);
        let side_bg = if pal.dark { pal.bg.mix(pal.surface, 0.45) } else { pal.surface_hover.mix(pal.bg, 0.55) };
        p.fill(side, 0.0, side_bg);
        p.line(side.right() - 0.5, 0.0, side.right() - 0.5, h, pal.border, 1.0);

        // Brand
        let logo = R::new(20.0 * s, 20.0 * s, 32.0 * s, 32.0 * s);
        draw_logo(p, pal, logo, s);
        p.text("Switch", &f.brand, R::new(logo.right() + 10.0 * s, logo.y - 2.0 * s, 140.0 * s, 22.0 * s), pal.text);
        p.text(
            &format!("Version {}", updater::VERSION),
            &f.desc,
            R::new(logo.right() + 10.0 * s, logo.y + 18.0 * s, 140.0 * s, 16.0 * s),
            pal.faint,
        );

        let mut y = 76.0 * s;
        for (i, name) in PAGES.iter().enumerate() {
            let r = R::new(10.0 * s, y, side.w - 20.0 * s, 36.0 * s);
            let hit = Hit::Nav(i);
            let active = i == self.page;
            if active {
                p.fill(r, 8.0 * s, pal.surface_hover.mix(pal.accent, if pal.dark { 0.10 } else { 0.06 }));
                p.fill(R::new(r.x, r.y + 9.0 * s, 3.0 * s, r.h - 18.0 * s), 1.5 * s, pal.accent);
            } else if self.hover == Some(hit) {
                p.fill(r, 8.0 * s, pal.surface_hover.a(0.7));
            }
            p.text(name, &f.nav, R::new(r.x + 16.0 * s, r.y, r.w - 40.0 * s, r.h), if active { pal.text } else { pal.subtext });
            if i == 6 && matches!(updater::state(), State::Available(_)) {
                p.circle(r.right() - 16.0 * s, r.y + r.h / 2.0, 4.0 * s, pal.accent);
            }
            self.hits.push((r, hit));
            y += 38.0 * s;
        }

        // Status at the bottom.
        let paused = hook::PAUSED.load(std::sync::atomic::Ordering::Relaxed);
        let sy = h - 40.0 * s;
        p.circle(26.0 * s, sy + 10.0 * s, 4.0 * s, if paused { C::hex(0xF59E0B) } else { C::hex(0x22C55E) });
        p.text(
            if paused { "Paused" } else { "Running" },
            &f.desc,
            R::new(38.0 * s, sy, side.w - 50.0 * s, 20.0 * s),
            pal.subtext,
        );
    }

    fn draw_content(&mut self, p: &Painter, pal: &Palette, f: &Fmts) {
        let s = self.scale;
        let x0 = SIDEBAR * s + 32.0 * s;
        let cw = (W * s).min(self.client_w()) - x0 - 32.0 * s;
        let mut y = 26.0 * s - self.scroll;
        p.text(PAGES[self.page], &f.h1, R::new(x0, y, cw, 34.0 * s), pal.text);
        y += 34.0 * s;
        p.text(PAGE_DESC[self.page], &f.desc, R::new(x0, y, cw, 18.0 * s), pal.subtext);
        y += 34.0 * s;

        for b in page_blocks(self.page, self.confirm_reset) {
            y = match b {
                Block::Section(t) => {
                    p.text(t, &f.section, R::new(x0 + 2.0 * s, y + 14.0 * s, cw, 20.0 * s), pal.subtext);
                    y + 42.0 * s
                }
                Block::Gap(g) => y + g * s,
                Block::Card(rows) => self.card(p, pal, f, x0, y, cw, rows) + 6.0 * s,
                Block::Preview => self.preview(p, pal, f, x0, y, cw) + 4.0 * s,
                Block::Cheats => self.cheats(p, pal, f, x0, y, cw),
                Block::Hidden => self.hidden(p, pal, f, x0, y, cw),
                Block::Running => self.running_apps(p, pal, f, x0, y, cw),
                Block::Notes => self.notes(p, pal, f, x0, y, cw),
                Block::AboutHead => self.about_head(p, pal, f, x0, y, cw),
            };
        }
        self.content_h = y + self.scroll + 30.0 * s;
    }

    fn client_w(&self) -> f32 {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        rc.right as f32
    }

    fn card_bg(&self, p: &Painter, pal: &Palette, r: R) {
        let s = self.scale;
        p.fill(r, 10.0 * s, pal.surface);
        p.stroke(r, 10.0 * s, pal.border, 1.0 * s);
    }

    fn row_h(&self, r: &Row) -> f32 {
        if r.desc.is_empty() {
            52.0 * self.scale
        } else {
            62.0 * self.scale
        }
    }

    fn card(&mut self, p: &Painter, pal: &Palette, f: &Fmts, x: f32, y: f32, w: f32, rows: Vec<Row>) -> f32 {
        let s = self.scale;
        let total: f32 = rows.iter().map(|r| self.row_h(r)).sum();
        self.card_bg(p, pal, R::new(x, y, w, total));
        let mut ry = y;
        for (i, r) in rows.into_iter().enumerate() {
            let rh = self.row_h(&r);
            if i > 0 {
                p.line(x + 18.0 * s, ry, x + w - 18.0 * s, ry, pal.border, 1.0 * s);
            }
            let rr = R::new(x, ry, w, rh);
            // Controls first so labels can use the remaining width.
            let ctl_w = self.control(p, pal, f, rr, &r.ctl);
            let lw = (w - ctl_w - 52.0 * s).max(120.0 * s);
            if self.page == 6 && i == 0 && matches!(r.ctl, Ctl::None) {
                self.update_status(p, pal, f, rr);
            } else if r.desc.is_empty() {
                p.text(&r.label, &f.label, R::new(x + 20.0 * s, ry, lw, rh), pal.text);
            } else {
                p.text(&r.label, &f.label, R::new(x + 20.0 * s, ry + 11.0 * s, lw, 22.0 * s), pal.text);
                p.text(&r.desc, &f.desc, R::new(x + 20.0 * s, ry + 33.0 * s, lw, 18.0 * s), pal.subtext);
            }
            ry += rh;
        }
        ry
    }

    /// Draws a row's control right-aligned; returns its width.
    fn control(&mut self, p: &Painter, pal: &Palette, f: &Fmts, row: R, ctl: &Ctl) -> f32 {
        let s = self.scale;
        let right = row.right() - 20.0 * s;
        let cy = row.y + row.h / 2.0;
        match ctl {
            Ctl::None => 0.0,
            Ctl::Info(t) => {
                let w = gfx().measure(t, &f.ctl_r) + 4.0;
                p.text(t, &f.ctl_r, R::new(right - w, row.y, w, row.h), pal.subtext);
                w
            }
            Ctl::Toggle(k, on) => {
                let (tw, th) = (42.0 * s, 22.0 * s);
                let r = R::new(right - tw, cy - th / 2.0, tw, th);
                let hit = Hit::Toggle(*k);
                let hot = self.hover == Some(hit);
                if *on {
                    p.fill(r, th / 2.0, if hot { pal.accent.mix(C::hex(0xFFFFFF), 0.12) } else { pal.accent });
                    p.circle(r.right() - th / 2.0, cy, th / 2.0 - 4.0 * s, pal.on_accent);
                } else {
                    p.fill(r, th / 2.0, if hot { pal.surface_hover } else { pal.surface });
                    p.stroke(r, th / 2.0, pal.subtext.a(0.8), 1.2 * s);
                    p.circle(r.x + th / 2.0, cy, th / 2.0 - 5.5 * s, pal.subtext);
                }
                self.hits.push((R::new(r.x - 8.0 * s, row.y, tw + 16.0 * s, row.h), hit));
                tw
            }
            Ctl::Seg(k, opts, cur) => {
                let widths: Vec<f32> = opts.iter().map(|o| gfx().measure(o, &f.ctl_c) + 26.0 * s).collect();
                let total: f32 = widths.iter().sum::<f32>() + 6.0 * s;
                let h = 32.0 * s;
                let r = R::new(right - total, cy - h / 2.0, total, h);
                p.fill(r, 8.0 * s, pal.thumb_bg);
                let mut x = r.x + 3.0 * s;
                for (i, o) in opts.iter().enumerate() {
                    let ir = R::new(x, r.y + 3.0 * s, widths[i], h - 6.0 * s);
                    let hit = Hit::Seg(*k, i);
                    if i == *cur {
                        p.fill(ir, 6.0 * s, pal.accent);
                        p.text(o, &f.ctl_c, ir, pal.on_accent);
                    } else {
                        if self.hover == Some(hit) {
                            p.fill(ir, 6.0 * s, pal.surface_hover);
                        }
                        p.text(o, &f.ctl_c, ir, pal.text);
                    }
                    self.hits.push((ir, hit));
                    x += widths[i];
                }
                total
            }
            Ctl::Slider(k, min, max, val, suffix) => {
                let tw = 200.0 * s;
                let vw = 64.0 * s;
                let track = R::new(right - tw, cy - 2.0 * s, tw, 4.0 * s);
                let t = (*val - min) as f32 / (*max - min).max(1) as f32;
                p.fill(track, 2.0 * s, pal.thumb_bg.mix(pal.subtext, 0.25));
                p.fill(R::new(track.x, track.y, track.w * t, track.h), 2.0 * s, pal.accent);
                let kx = track.x + track.w * t;
                let hot = self.hover == Some(Hit::Slider(*k)) || matches!(self.dragging, Some((dk, ..)) if dk == *k);
                p.circle(kx, cy, if hot { 9.0 } else { 8.0 } * s, pal.accent);
                p.circle(kx, cy, if hot { 4.0 } else { 3.5 } * s, pal.on_accent);
                let label = format!("{val}{suffix}");
                p.text(&label, &f.ctl_r, R::new(track.x - vw - 12.0 * s, row.y, vw, row.h), pal.subtext);
                let hr = R::new(track.x - 10.0 * s, row.y + 6.0 * s, tw + 20.0 * s, row.h - 12.0 * s);
                self.hits.push((hr, Hit::Slider(*k)));
                // Remember the track geometry for dragging.
                self.hits.push((track, Hit::Slider(*k)));
                tw + vw + 12.0 * s
            }
            Ctl::Swatches => {
                let d = 24.0 * s;
                let gap = 10.0 * s;
                let n = gfx::ACCENTS.len() + 1;
                let total = n as f32 * d + (n - 1) as f32 * gap;
                let mut x = right - total;
                let cur = cfg().accent.clone();
                for i in 0..n {
                    let (color, selected) = if i == 0 {
                        (C::hex(gfx::system_accent()), cur == "system")
                    } else {
                        let (_, hex) = gfx::ACCENTS[i - 1];
                        (C::hex(hex), gfx::parse_hex(&cur) == Some(hex))
                    };
                    let ccx = x + d / 2.0;
                    let hit = Hit::Accent(i);
                    if i == 0 {
                        p.circle(ccx, cy, d / 2.0, pal.thumb_bg.mix(pal.subtext, 0.2));
                        p.circle(ccx, cy, d / 2.0 - 5.0 * s, color);
                    } else {
                        p.circle(ccx, cy, d / 2.0, color);
                    }
                    if selected {
                        p.ring(ccx, cy, d / 2.0 + 3.5 * s, pal.text, 1.6 * s);
                    } else if self.hover == Some(hit) {
                        p.ring(ccx, cy, d / 2.0 + 3.0 * s, pal.subtext.a(0.6), 1.2 * s);
                    }
                    self.hits.push((R::new(x - 3.0 * s, cy - d / 2.0 - 3.0 * s, d + 6.0 * s, d + 6.0 * s), hit));
                    x += d + gap;
                }
                total
            }
            Ctl::Hotkey => {
                let w = 200.0 * s;
                let h = 34.0 * s;
                let r = R::new(right - w, cy - h / 2.0, w, h);
                let hk = cfg().hotkey;
                let hot = self.hover == Some(Hit::Hotkey);
                p.fill(r, 8.0 * s, if hot && !self.recording { pal.surface_hover } else { pal.thumb_bg });
                if self.recording {
                    p.stroke(r, 8.0 * s, pal.accent, 1.6 * s);
                    let live = current_mods();
                    let mut t = String::new();
                    if live.ctrl || live.alt || live.shift || live.win {
                        let mut l = hotkey_label(&Hotkey { vk: 0, ..live });
                        l.truncate(l.rfind(" + ").unwrap_or(l.len()));
                        t = l + " + \u{2026}";
                    }
                    if t.is_empty() {
                        t = "Press a shortcut\u{2026}".into();
                    }
                    p.text(&t, &f.ctl_c, r, pal.accent);
                } else {
                    let label = hk.map(|h| hotkey_label(&h)).unwrap_or_else(|| "Click to set".into());
                    let color = if hk.is_some() { pal.text } else { pal.faint };
                    let tr = if hk.is_some() { R::new(r.x, r.y, r.w - 24.0 * s, r.h) } else { r };
                    p.text(&label, &f.ctl_c, tr, color);
                    if hk.is_some() {
                        let cr = R::new(r.right() - 28.0 * s, r.y + 5.0 * s, 24.0 * s, r.h - 10.0 * s);
                        if self.hover == Some(Hit::HotkeyClear) {
                            p.fill(cr, 6.0 * s, pal.surface_hover);
                        }
                        p.glyph_close(cr.x + cr.w / 2.0, cr.y + cr.h / 2.0, 3.5 * s, pal.subtext, 1.2 * s);
                        self.hits.push((cr, Hit::HotkeyClear));
                    }
                }
                self.hits.push((r, Hit::Hotkey));
                w
            }
            Ctl::Button(k, label, primary) => {
                let bw = gfx().measure(label, &f.ctl_c) + 32.0 * s;
                let h = 34.0 * s;
                let r = R::new(right - bw, cy - h / 2.0, bw, h);
                let hit = Hit::Button(*k);
                let hot = self.hover == Some(hit);
                let danger = *k == K::Reset && self.confirm_reset;
                if *primary || danger {
                    let base = if danger { C::hex(0xE5484D) } else { pal.accent };
                    p.fill(r, 8.0 * s, if hot { base.mix(C::hex(0xFFFFFF), 0.12) } else { base });
                    p.text(label, &f.ctl_c, r, if danger { C::hex(0xFFFFFF) } else { pal.on_accent });
                } else {
                    p.fill(r, 8.0 * s, if hot { pal.surface_hover } else { pal.thumb_bg });
                    p.stroke(r, 8.0 * s, pal.border, 1.0 * s);
                    p.text(label, &f.ctl_c, r, pal.text);
                }
                self.hits.push((r, hit));
                bw
            }
            Ctl::Field(k, placeholder) => {
                let w = 200.0 * s;
                let h = 34.0 * s;
                let r = R::new(right - w, cy - h / 2.0, w, h);
                let hit = Hit::Field(*k);
                let focused = matches!(&self.field, Some((fk, _)) if fk == k);
                p.fill(r, 8.0 * s, if self.hover == Some(hit) && !focused { pal.surface_hover } else { pal.thumb_bg });
                p.stroke(r, 8.0 * s, if focused { pal.accent } else { pal.border }, if focused { 1.6 } else { 1.0 } * s);
                let tr = R::new(r.x + 12.0 * s, r.y, r.w - 24.0 * s, r.h);
                let current = if *k == K::AccentHex && cfg().accent.starts_with('#') { cfg().accent.clone() } else { String::new() };
                let text = match &self.field {
                    Some((fk, t)) if fk == k => t.clone(),
                    _ => current.clone(),
                };
                if text.is_empty() {
                    let ph = if current.is_empty() { placeholder.to_string() } else { current.clone() };
                    p.text(&ph, &f.ctl, tr, pal.faint);
                } else {
                    p.text(&text, &f.ctl, tr, pal.text);
                }
                if focused {
                    let cx = tr.x + gfx().measure(&text, &f.ctl).min(tr.w) + 1.0 * s;
                    p.line(cx, r.y + 9.0 * s, cx, r.bottom() - 9.0 * s, pal.accent, 1.4 * s);
                }
                if *k == K::AccentHex {
                    if let Some(hex) = gfx::parse_hex(&text) {
                        p.circle(r.right() - 17.0 * s, cy, 7.0 * s, C::hex(hex));
                    }
                }
                self.hits.push((r, hit));
                w
            }
        }
    }

    fn update_status(&mut self, p: &Painter, pal: &Palette, f: &Fmts, row: R) {
        let s = self.scale;
        let st = updater::state();
        let (msg, color, btn): (String, C, Option<&str>) = match &st {
            State::Idle => (format!("Version {}", updater::VERSION), pal.text, Some("Check now")),
            State::Checking => ("Checking for updates\u{2026}".into(), pal.subtext, None),
            State::UpToDate => (format!("You're up to date ({})", updater::VERSION), pal.text, Some("Check now")),
            State::Available(r) => (format!("Version {} is available", r.version), pal.accent, Some("Install & restart")),
            State::Downloading(v) => (format!("Downloading {v}\u{2026}"), pal.subtext, None),
            State::Error(e) => (format!("Update failed: {e}"), C::hex(0xE5484D), Some("Try again")),
        };
        let mut text_w = row.w - 40.0 * s;
        if let Some(b) = btn {
            let primary = matches!(st, State::Available(_));
            text_w -= self.control(p, pal, f, row, &Ctl::Button(K::UpdateNow, b.into(), primary)) + 16.0 * s;
        }
        let sub = updater::last_checked_label().map(|l| format!("Last checked {l}")).unwrap_or_else(|| "Not checked yet this session".into());
        let x = if matches!(st, State::UpToDate) {
            p.glyph_check(row.x + 28.0 * s, row.y + 22.0 * s, 5.0 * s, C::hex(0x22C55E), 1.8 * s);
            row.x + 42.0 * s
        } else {
            row.x + 20.0 * s
        };
        p.text(&msg, &f.label, R::new(x, row.y + 11.0 * s, text_w - (x - row.x - 20.0 * s), 22.0 * s), color);
        p.text(&sub, &f.desc, R::new(row.x + 20.0 * s, row.y + 33.0 * s, text_w, 18.0 * s), pal.subtext);
    }

    fn notes(&mut self, p: &Painter, pal: &Palette, f: &Fmts, x: f32, y: f32, w: f32) -> f32 {
        let s = self.scale;
        let State::Available(rel) = updater::state() else { return y };
        let lines = updater::notes_lines(&rel.notes, 14);
        if lines.is_empty() {
            return y;
        }
        let y = y + 8.0 * s;
        p.text(&format!("What's new in {}", rel.version), &f.section, R::new(x + 2.0 * s, y + 6.0 * s, w, 20.0 * s), pal.subtext);
        let text = lines.join("\n");
        let body_w = w - 40.0 * s;
        let h = text_height(&text, &f.wrap, body_w) + 32.0 * s;
        let card = R::new(x, y + 34.0 * s, w, h);
        self.card_bg(p, pal, card);
        p.text(&text, &f.wrap, R::new(x + 20.0 * s, card.y + 16.0 * s, body_w, h), pal.text);
        card.bottom() + 6.0 * s
    }

    fn cheats(&mut self, p: &Painter, pal: &Palette, f: &Fmts, x: f32, y: f32, w: f32) -> f32 {
        let s = self.scale;
        let tips = [
            ("Type", "Fuzzy search titles and app names (\u{201C}vsc\u{201D} finds Visual Studio Code)"),
            ("Enter, click", "Switch to the selected window"),
            ("Arrows, Tab, PgUp/PgDn", "Move the selection"),
            ("Esc", "Clear the search, then close"),
            ("Del, Ctrl+W, middle-click", "Close the window"),
            ("Ctrl+Shift+W", "Close every window of that app"),
            ("Ctrl+M / Ctrl+Up", "Minimize / maximize or restore"),
            ("Ctrl+Left / Ctrl+Right", "Move the window to another monitor"),
            ("Ctrl+P", "Pin the window on top"),
            ("Ctrl+N / Ctrl+O", "New window of the app / open its file location"),
            ("Ctrl+H", "Hide the app from Switch"),
            ("Ctrl+G / Ctrl+L", "Cycle grouping / layout"),
            ("Right-click, Menu key", "All actions for a window"),
        ];
        let row_h = 30.0 * s;
        let card = R::new(x, y, w, tips.len() as f32 * row_h + 20.0 * s);
        self.card_bg(p, pal, card);
        for (i, (k, v)) in tips.iter().enumerate() {
            let ry = y + 10.0 * s + i as f32 * row_h;
            let kw = gfx().measure(k, &f.small) + 16.0 * s;
            let kr = R::new(x + 18.0 * s, ry + 4.0 * s, kw, row_h - 8.0 * s);
            p.fill(kr, 5.0 * s, pal.thumb_bg);
            p.text(k, &f.small, R::new(kr.x + 8.0 * s, kr.y, kw, kr.h), pal.text);
            p.text(v, &f.small, R::new(x + 250.0 * s, ry, w - 270.0 * s, row_h), pal.subtext);
        }
        card.bottom()
    }

    fn hidden(&mut self, p: &Painter, pal: &Palette, f: &Fmts, x: f32, y: f32, w: f32) -> f32 {
        let s = self.scale;
        let apps = cfg().hidden_apps.clone();
        let pad = 20.0 * s;
        let chip_h = 30.0 * s;
        let mut chips: Vec<(R, usize)> = Vec::new();
        let (mut cx, mut cy) = (x + pad, y + 58.0 * s);
        for (i, a) in apps.iter().enumerate() {
            let cw = gfx().measure(a, &f.ctl) + 46.0 * s;
            if cx + cw > x + w - pad && cx > x + pad {
                cx = x + pad;
                cy += chip_h + 8.0 * s;
            }
            chips.push((R::new(cx, cy, cw, chip_h), i));
            cx += cw + 8.0 * s;
        }
        let bottom = if apps.is_empty() { y + 62.0 * s } else { cy + chip_h + pad };
        let card = R::new(x, y, w, bottom - y);
        self.card_bg(p, pal, card);
        p.text(
            &format!("{} hidden app{}", apps.len(), if apps.len() == 1 { "" } else { "s" }),
            &f.label,
            R::new(x + pad, y + 11.0 * s, w * 0.6, 22.0 * s),
            pal.text,
        );
        let desc = if apps.is_empty() {
            "Press Ctrl+H in the switcher, or use the list below."
        } else {
            "Click \u{00D7} to show an app again."
        };
        p.text(desc, &f.desc, R::new(x + pad, y + 33.0 * s, w - 2.0 * pad, 18.0 * s), pal.subtext);
        for (r, i) in chips {
            let hit = Hit::Unhide(i);
            p.fill(r, r.h / 2.0, if self.hover == Some(hit) { pal.surface_hover } else { pal.thumb_bg });
            p.text(&apps[i], &f.ctl, R::new(r.x + 14.0 * s, r.y, r.w - 38.0 * s, r.h), pal.text);
            p.glyph_close(r.right() - 17.0 * s, r.y + r.h / 2.0, 3.5 * s, pal.subtext, 1.2 * s);
            self.hits.push((r, hit));
        }
        card.bottom()
    }

    fn running_apps(&mut self, p: &Painter, pal: &Palette, f: &Fmts, x: f32, y: f32, w: f32) -> f32 {
        let s = self.scale;
        if self.running.is_empty() {
            self.refresh_running();
        }
        let hidden = cfg().hidden_apps.clone();
        let list: Vec<(usize, &(String, String))> =
            self.running.iter().enumerate().filter(|(_, (exe, _))| !hidden.contains(exe)).collect();
        let row_h = 46.0 * s;
        if list.is_empty() {
            let card = R::new(x, y, w, 52.0 * s);
            self.card_bg(p, pal, card);
            p.text("No other apps with windows right now.", &f.desc, R::new(x + 20.0 * s, y, w, card.h), pal.subtext);
            return card.bottom();
        }
        let card = R::new(x, y, w, row_h * list.len() as f32);
        self.card_bg(p, pal, card);
        let entries: Vec<(usize, String, String)> = list.iter().map(|(i, (e, n))| (*i, e.clone(), n.clone())).collect();
        for (k, (i, exe, name)) in entries.into_iter().enumerate() {
            let ry = y + k as f32 * row_h;
            if k > 0 {
                p.line(x + 18.0 * s, ry, x + w - 18.0 * s, ry, pal.border, 1.0 * s);
            }
            p.text(&name, &f.label, R::new(x + 20.0 * s, ry, w * 0.45, row_h), pal.text);
            p.text(&exe, &f.small, R::new(x + w * 0.45, ry, w * 0.3, row_h), pal.faint);
            let bw = 70.0 * s;
            let br = R::new(x + w - 20.0 * s - bw, ry + (row_h - 30.0 * s) / 2.0, bw, 30.0 * s);
            let hit = Hit::HideRunning(i);
            p.fill(br, 7.0 * s, if self.hover == Some(hit) { pal.surface_hover } else { pal.thumb_bg });
            p.text("Hide", &f.ctl_c, br, pal.text);
            self.hits.push((br, hit));
        }
        card.bottom()
    }

    fn refresh_running(&mut self) {
        let mut seen: Vec<(String, String)> = Vec::new();
        for w in crate::winlist::enumerate(false) {
            if !w.exe.is_empty() && !seen.iter().any(|(e, _)| *e == w.exe) {
                seen.push((w.exe.clone(), w.app_name.clone()));
            }
        }
        seen.sort_by_key(|(_, n)| n.to_lowercase());
        self.running = seen;
    }

    fn about_head(&mut self, p: &Painter, pal: &Palette, f: &Fmts, x: f32, y: f32, w: f32) -> f32 {
        let s = self.scale;
        let card = R::new(x, y, w, 96.0 * s);
        self.card_bg(p, pal, card);
        let logo = R::new(x + 22.0 * s, y + 22.0 * s, 52.0 * s, 52.0 * s);
        draw_logo(p, pal, logo, s * 1.6);
        p.text(&format!("Switch {}", updater::VERSION), &f.brand, R::new(logo.right() + 16.0 * s, y + 22.0 * s, w, 26.0 * s), pal.text);
        p.text(
            "A fast, lightweight Alt+Tab replacement for Windows. Native Win32 + Direct2D, no runtime.",
            &f.desc,
            R::new(logo.right() + 16.0 * s, y + 50.0 * s, w - 120.0 * s, 18.0 * s),
            pal.subtext,
        );
        card.bottom() + 4.0 * s
    }

    /// A miniature, live-updating mock of the switcher using the current settings.
    fn preview(&mut self, p: &Painter, pal: &Palette, f: &Fmts, x: f32, y: f32, w: f32) -> f32 {
        let s = self.scale;
        let c = cfg().clone();
        let h = 190.0 * s;
        let stage = R::new(x, y, w, h);
        // "Desktop" behind the switcher.
        let desk = if pal.dark { C::hex(0x2B3A55) } else { C::hex(0xB9C8E3) };
        p.fill(stage, 10.0 * s, desk);
        p.fill(R::new(x + 24.0 * s, y + 18.0 * s, w * 0.42, h * 0.55), 6.0 * s, C::hex(0xFFFFFF).a(0.35));
        p.fill(R::new(x + w * 0.5, y + 40.0 * s, w * 0.42, h * 0.5), 6.0 * s, C::hex(0xFFFFFF).a(0.25));
        if c.dim_background {
            p.fill(stage, 10.0 * s, C::hex(0).a(c.dim_amount as f32 / 100.0));
        }
        let k = 0.62; // mock scale
        let rad = c.corner_radius as f32 * s * k;
        let pad = 10.0 * s;
        let search_h = if c.search_bar == SearchBar::Always { 22.0 * s } else { 0.0 };
        let hints_h = if c.show_hints { 14.0 * s } else { 0.0 };
        let pw = (w * 0.74).min(520.0 * s);
        let body_h = match c.layout {
            Layout::List => 4.0 * 24.0 * s,
            Layout::Row => (c.preview_px as f32 * s * k * 0.55).clamp(40.0 * s, 90.0 * s) + 22.0 * s,
            Layout::Grid => (c.preview_px as f32 * s * k * 0.55).clamp(40.0 * s, 90.0 * s) + 22.0 * s,
        };
        let ph = (pad * 2.0 + if search_h > 0.0 { search_h + 8.0 * s } else { 0.0 } + body_h + hints_h).min(h - 20.0 * s);
        let py = match c.position {
            Position::Center => y + (h - ph) / 2.0,
            Position::Top => y + 12.0 * s,
            Position::Bottom => y + h - ph - 12.0 * s,
        };
        let panel = R::new(x + (w - pw) / 2.0, py, pw, ph);
        p.fill(R::new(panel.x + 2.0 * s, panel.y + 4.0 * s, panel.w, panel.h), (rad + 2.0 * s).max(2.0), C::hex(0).a(0.18));
        p.fill(panel, rad.max(1.0), pal.bg);
        p.stroke(panel, rad.max(1.0), pal.border, 1.0);
        let mut cy = panel.y + pad;
        if search_h > 0.0 {
            let sr = R::new(panel.x + pad, cy, panel.w - 2.0 * pad, search_h);
            p.fill(sr, (rad * 0.85).min(sr.h / 2.0), pal.surface);
            p.glyph_search(sr.x + 11.0 * s, sr.y + sr.h / 2.0, 4.5 * s, pal.faint, 1.0 * s);
            p.fill(R::new(sr.x + 22.0 * s, sr.y + sr.h / 2.0 - 2.0 * s, 70.0 * s, 4.0 * s), 2.0 * s, pal.faint.a(0.6));
            cy += search_h + 8.0 * s;
        }
        let fills = [C::hex(0x3B82F6), C::hex(0x22C55E), C::hex(0xF59E0B), C::hex(0xA855F7)];
        let hl_style = c.highlight;
        let draw_hl = |r: R| {
            let (fill, stroke) = match hl_style {
                Highlight::Outline => (false, true),
                Highlight::Fill => (true, false),
                Highlight::Both => (true, true),
            };
            if fill {
                p.fill(r, rad, pal.accent.a(if hl_style == Highlight::Fill { 0.3 } else { 0.18 }));
            }
            if stroke {
                p.stroke(r, rad, pal.accent, 1.5 * s);
            }
        };
        match c.layout {
            Layout::List => {
                let list_w = if c.list_preview { (panel.w - 2.0 * pad) * 0.5 } else { panel.w - 2.0 * pad };
                for i in 0..4 {
                    let r = R::new(panel.x + pad, cy + i as f32 * 24.0 * s, list_w, 24.0 * s);
                    if i == 1 {
                        draw_hl(r);
                    }
                    if c.show_icons {
                        p.fill(R::new(r.x + 6.0 * s, r.y + 6.0 * s, 12.0 * s, 12.0 * s), 3.0 * s, fills[i]);
                    }
                    let tx = r.x + if c.show_icons { 24.0 } else { 8.0 } * s;
                    p.fill(R::new(tx, r.y + 7.0 * s, list_w * 0.5, 4.0 * s), 2.0 * s, pal.text.a(0.55));
                    p.fill(R::new(tx, r.y + 14.0 * s, list_w * 0.3, 3.0 * s), 1.5 * s, pal.faint.a(0.6));
                }
                if c.list_preview {
                    let pr = R::new(panel.x + pad + list_w + 8.0 * s, cy, panel.w - 2.0 * pad - list_w - 8.0 * s, 4.0 * 24.0 * s);
                    p.fill(pr, rad, pal.thumb_bg);
                    let inner = pr.inset(8.0 * s);
                    p.fill(inner, 3.0 * s, fills[1].a(0.8));
                    p.fill(R::new(inner.x, inner.y, inner.w, 8.0 * s), 3.0 * s, C::hex(0xFFFFFF).a(0.35));
                }
            }
            Layout::Grid | Layout::Row => {
                let n = 4;
                let gap = 6.0 * s;
                let tw = (panel.w - 2.0 * pad - gap * (n - 1) as f32) / n as f32;
                let th = body_h;
                for i in 0..n {
                    let r = R::new(panel.x + pad + i as f32 * (tw + gap), cy, tw, th);
                    if i == 1 {
                        draw_hl(r);
                    }
                    let inner = R::new(r.x + 5.0 * s, r.y + 18.0 * s, r.w - 10.0 * s, r.h - 23.0 * s);
                    p.fill(inner, (rad * 0.5).max(1.0), pal.thumb_bg);
                    let win = inner.inset(3.0 * s);
                    p.fill(win, 2.0 * s, fills[i].a(0.75));
                    p.fill(R::new(win.x, win.y, win.w, 6.0 * s), 2.0 * s, C::hex(0xFFFFFF).a(0.35));
                    if c.show_icons {
                        p.fill(R::new(r.x + 6.0 * s, r.y + 5.0 * s, 9.0 * s, 9.0 * s), 2.0 * s, fills[i]);
                    }
                    let tx = r.x + if c.show_icons { 19.0 } else { 7.0 } * s;
                    p.fill(R::new(tx, r.y + 8.0 * s, (r.w * 0.5).min(60.0 * s), 3.5 * s), 1.5 * s, pal.text.a(if i == 1 { 0.8 } else { 0.45 }));
                }
            }
        }
        if hints_h > 0.0 {
            let hy = panel.bottom() - pad - 6.0 * s;
            p.fill(R::new(panel.x + panel.w * 0.25, hy, panel.w * 0.5, 3.0 * s), 1.5 * s, pal.faint.a(0.5));
        }
        let _ = f;
        stage.bottom()
    }

    // ---- input ----------------------------------------------------------------------

    fn set_slider(&mut self, k: K, v: u32) {
        let c = cfg();
        match k {
            K::Delay => c.show_delay_ms = v,
            K::Radius => c.corner_radius = v,
            K::DimAmount => c.dim_amount = v,
            K::PreviewPx => c.preview_px = v,
            _ => {}
        }
    }

    fn slider_value_at(&self, x: f32) -> Option<u32> {
        let (_, track, min, max) = self.dragging?;
        let t = ((x - track.x) / track.w).clamp(0.0, 1.0);
        Some(min + ((max - min) as f32 * t).round() as u32)
    }

    fn begin_drag(&mut self, k: K, x: f32) {
        // The second hit recorded for a slider is its track.
        let track = self.hits.iter().filter(|(_, h)| *h == Hit::Slider(k)).map(|(r, _)| *r).nth(1);
        let (min, max) = match k {
            K::Delay => (0, 250),
            K::Radius => (0, 24),
            K::DimAmount => (10, 80),
            K::PreviewPx => (80, 320),
            _ => (0, 1),
        };
        if let Some(track) = track {
            self.dragging = Some((k, track, min, max));
            unsafe {
                SetCapture(self.hwnd);
            }
            if let Some(v) = self.slider_value_at(x) {
                self.set_slider(k, v);
            }
        }
    }

    fn commit_field(&mut self) {
        let Some((k, text)) = self.field.take() else { return };
        let t = text.trim().to_string();
        match k {
            K::AccentHex => {
                if let Some(hex) = gfx::parse_hex(&t) {
                    if t.trim_start_matches('#').len() == 6 {
                        cfg().accent = format!("#{hex:06X}");
                        unsafe { apply_titlebar(self.hwnd) };
                    }
                }
            }
            K::AddHidden => {
                let mut exe = t.to_lowercase();
                if !exe.is_empty() {
                    if !exe.ends_with(".exe") {
                        exe.push_str(".exe");
                    }
                    if !cfg().hidden_apps.contains(&exe) {
                        cfg().hidden_apps.push(exe);
                    }
                }
            }
            _ => {}
        }
        save_and_apply();
    }

    fn click(&mut self, hit: Hit) {
        if !matches!(hit, Hit::Button(K::Reset)) {
            self.confirm_reset = false;
        }
        if !matches!(hit, Hit::Field(_)) && self.field.is_some() {
            self.commit_field();
        }
        let c = cfg();
        match hit {
            Hit::Nav(i) => {
                self.page = i;
                self.scroll = 0.0;
                self.running.clear();
                unsafe {
                    if i == 7 {
                        SetTimer(self.hwnd, TIMER_REFRESH, 1000, None);
                    } else {
                        let _ = KillTimer(self.hwnd, TIMER_REFRESH);
                    }
                }
            }
            Hit::Toggle(k) => match k {
                K::Enabled => {
                    let p = !hook::PAUSED.load(std::sync::atomic::Ordering::Relaxed);
                    hook::PAUSED.store(p, std::sync::atomic::Ordering::Relaxed);
                    crate::tray_update();
                }
                K::Autostart => set_autostart(!autostart_enabled()),
                K::TrimMemory => c.trim_memory = !c.trim_memory,
                K::AltTab => c.alt_tab = !c.alt_tab,
                K::Release => c.release_to_switch = !c.release_to_switch,
                K::Backtick => c.alt_backtick = !c.alt_backtick,
                K::Dim => c.dim_background = !c.dim_background,
                K::ListPreview => c.list_preview = !c.list_preview,
                K::Icons => c.show_icons = !c.show_icons,
                K::Hints => c.show_hints = !c.show_hints,
                K::Wrap => c.wrap = !c.wrap,
                K::Minimized => c.show_minimized = !c.show_minimized,
                K::Desktops => c.other_desktops = !c.other_desktops,
                K::HoverSelect => c.hover_select = !c.hover_select,
                K::ActionButtons => c.action_buttons = !c.action_buttons,
                K::AutoUpdate => {
                    c.auto_update = !c.auto_update;
                    if c.auto_update {
                        updater::check_async(true);
                    }
                }
                K::Notify => c.notify_updates = !c.notify_updates,
                K::AutoInstall => c.auto_install = !c.auto_install,
                _ => {}
            },
            Hit::Seg(k, i) => match k {
                K::TrayClick => c.tray_click = TrayClick::from_index(i),
                K::Theme => {
                    c.theme = Theme::from_index(i);
                    unsafe { apply_titlebar(self.hwnd) };
                }
                K::TextSize => c.text_size = TextSize::from_index(i),
                K::Highlight => c.highlight = Highlight::from_index(i),
                K::Anim => c.anim_speed = AnimSpeed::from_index(i),
                K::Layout => c.layout = Layout::from_index(i),
                K::Position => c.position = Position::from_index(i),
                K::Label => c.label = Label::from_index(i),
                K::SearchBar => c.search_bar = SearchBar::from_index(i),
                K::Monitors => c.monitors = Monitors::from_index(i),
                K::OpenOn => c.open_on = OpenOn::from_index(i),
                K::Grouping => c.grouping = Grouping::from_index(i),
                K::Sort => c.sort = Sort::from_index(i),
                K::Initial => c.initial = Initial::from_index(i),
                _ => {}
            },
            Hit::Accent(i) => {
                c.accent = if i == 0 { "system".into() } else { format!("#{:06X}", gfx::ACCENTS[i - 1].1) };
                unsafe { apply_titlebar(self.hwnd) };
            }
            Hit::Hotkey => {
                self.recording = true;
                hook::RECORDING.store(true, std::sync::atomic::Ordering::Relaxed);
                unsafe {
                    let _ = SetFocus(self.hwnd);
                }
            }
            Hit::HotkeyClear => c.hotkey = None,
            Hit::Field(k) => {
                if !matches!(&self.field, Some((fk, _)) if *fk == k) {
                    self.commit_field();
                    // Start empty; the current value stays visible as the placeholder.
                    self.field = Some((k, String::new()));
                }
            }
            Hit::Unhide(i) => {
                if i < c.hidden_apps.len() {
                    c.hidden_apps.remove(i);
                }
            }
            Hit::HideRunning(i) => {
                if let Some((exe, _)) = self.running.get(i) {
                    if !c.hidden_apps.contains(exe) {
                        c.hidden_apps.push(exe.clone());
                    }
                }
            }
            Hit::Button(k) => match k {
                K::UpdateNow => match updater::state() {
                    State::Available(_) => updater::install_async(),
                    _ => updater::check_async(false),
                },
                K::RunAsAdmin => crate::restart_as_admin(),
                K::OpenRepo => util::shell_open(&format!("https://github.com/{}", updater::REPO)),
                K::OpenReleases => util::shell_open(&format!("https://github.com/{}/releases", updater::REPO)),
                K::OpenFolder => util::shell_open(&util::data_dir().to_string_lossy()),
                K::OpenLog => {
                    let log = util::data_dir().join("switch.log");
                    if !log.exists() {
                        util::log("log opened from settings");
                    }
                    util::shell_open(&log.to_string_lossy());
                }
                K::Reset => {
                    if self.confirm_reset {
                        self.confirm_reset = false;
                        reset();
                        unsafe { apply_titlebar(self.hwnd) };
                    } else {
                        self.confirm_reset = true;
                    }
                }
                _ => {}
            },
            Hit::Slider(_) => {}
        }
        save_and_apply();
        self.render();
    }

    fn stop_recording(&mut self) {
        self.recording = false;
        hook::RECORDING.store(false, std::sync::atomic::Ordering::Relaxed);
    }

    fn on_key(&mut self, vk: u32) -> bool {
        if let Some((_, text)) = &mut self.field {
            match VIRTUAL_KEY(vk as u16) {
                VK_RETURN => {
                    self.commit_field();
                }
                VK_ESCAPE => {
                    self.field = None;
                }
                VK_BACK => {
                    text.pop();
                }
                _ if vk == b'V' as u32 && key_down(VK_CONTROL) => {
                    if let Some(t) = clipboard_text() {
                        text.push_str(t.lines().next().unwrap_or("").trim());
                    }
                }
                _ => return false, // let WM_CHAR deliver the character
            }
            self.render();
            return true;
        }
        if !self.recording {
            return false;
        }
        let mods = current_mods();
        let any_mod = mods.ctrl || mods.alt || mods.shift || mods.win;
        if is_mod(vk) {
            self.render();
            return true;
        }
        if !any_mod && vk == VK_ESCAPE.0 as u32 {
            self.stop_recording();
        } else if !any_mod && (vk == VK_BACK.0 as u32 || vk == VK_DELETE.0 as u32) {
            cfg().hotkey = None;
            self.stop_recording();
        } else {
            let is_fkey = (0x70..=0x87).contains(&vk);
            let alt_tab = mods.alt && vk == VK_TAB.0 as u32;
            if (any_mod || is_fkey) && !alt_tab {
                cfg().hotkey = Some(Hotkey { vk, ..mods });
                self.stop_recording();
            }
        }
        save_and_apply();
        self.render();
        true
    }

    fn on_char(&mut self, ch: u16) -> bool {
        let Some((k, text)) = &mut self.field else { return false };
        if let Some(c) = char::from_u32(ch as u32) {
            if !c.is_control() && text.chars().count() < 64 {
                if *k == K::AccentHex && !(c.is_ascii_hexdigit() || c == '#') {
                    return true;
                }
                text.push(c);
            }
        }
        self.render();
        true
    }
}

fn save_and_apply() {
    save();
    hook::apply_config(cfg());
}

fn text_height(text: &str, fmt: &IDWriteTextFormat, w: f32) -> f32 {
    unsafe {
        let t = util::utf16(text);
        match gfx().dw.CreateTextLayout(&t, fmt, w, 10_000.0) {
            Ok(l) => {
                let mut m = DWRITE_TEXT_METRICS::default();
                if l.GetMetrics(&mut m).is_ok() {
                    m.height
                } else {
                    100.0
                }
            }
            Err(_) => 100.0,
        }
    }
}

fn draw_logo(p: &Painter, pal: &Palette, r: R, s: f32) {
    p.fill(r, r.w * 0.26, pal.accent);
    let i = r.inset(r.w * 0.22);
    p.fill(R::new(i.x, i.y, i.w * 0.68, i.h * 0.6), 2.0 * s, pal.on_accent.a(0.55));
    p.fill(R::new(i.x + i.w * 0.32, i.y + i.h * 0.38, i.w * 0.68, i.h * 0.62), 2.0 * s, pal.on_accent);
}

fn mouse_xy(l: LPARAM) -> (f32, f32) {
    ((l.0 & 0xFFFF) as i16 as f32, ((l.0 >> 16) & 0xFFFF) as i16 as f32)
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let Some(s) = ST.try_get() else {
        return DefWindowProcW(h, msg, w, l);
    };
    if s.hwnd != h && !s.hwnd.is_invalid() {
        return DefWindowProcW(h, msg, w, l);
    }
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(h, &mut ps);
            if s.hwnd == h {
                s.render();
            }
            let _ = EndPaint(h, &ps);
            return LRESULT(0);
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_SIZE => {
            if let Some(rt) = &s.rt {
                let (cw, ch) = ((l.0 & 0xFFFF) as u32, ((l.0 >> 16) & 0xFFFF) as u32);
                let _ = rt.Resize(&D2D_SIZE_U { width: cw, height: ch });
            }
        }
        WM_TIMER if w.0 == TIMER_REFRESH => {
            if s.page == 7 {
                s.render();
            } else {
                let _ = KillTimer(h, TIMER_REFRESH);
            }
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            let (x, y) = mouse_xy(l);
            if let Some((k, ..)) = s.dragging {
                if let Some(v) = s.slider_value_at(x) {
                    s.set_slider(k, v);
                    s.render();
                }
                return LRESULT(0);
            }
            if !s.tracking {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: h,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut tme);
                s.tracking = true;
            }
            let hov = s.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| *h);
            if hov != s.hover {
                s.hover = hov;
                let cursor = if hov.is_some() { IDC_HAND } else { IDC_ARROW };
                SetCursor(LoadCursorW(HINSTANCE::default(), cursor).unwrap_or_default());
                s.render();
            }
        }
        WM_SETCURSOR => {
            if (l.0 & 0xFFFF) as u32 == HTCLIENT {
                let cursor = match s.hover {
                    Some(Hit::Field(_)) => IDC_IBEAM,
                    Some(_) => IDC_HAND,
                    None => IDC_ARROW,
                };
                SetCursor(LoadCursorW(HINSTANCE::default(), cursor).unwrap_or_default());
                return LRESULT(1);
            }
        }
        WM_MOUSELEAVE => {
            s.tracking = false;
            if s.hover.is_some() {
                s.hover = None;
                s.render();
            }
        }
        WM_LBUTTONDOWN => {
            let (x, y) = mouse_xy(l);
            if let Some(Hit::Slider(k)) = s.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| *h) {
                s.begin_drag(k, x);
                s.render();
            }
        }
        WM_LBUTTONUP => {
            if s.dragging.take().is_some() {
                let _ = ReleaseCapture();
                save_and_apply();
                s.render();
                return LRESULT(0);
            }
            let (x, y) = mouse_xy(l);
            let hit = s.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| *h);
            if s.recording && hit != Some(Hit::Hotkey) {
                s.stop_recording();
            }
            match hit {
                Some(hit) if !(s.recording && hit == Hit::Hotkey) => s.click(hit),
                Some(_) => {}
                None => {
                    s.confirm_reset = false;
                    if s.field.is_some() {
                        s.commit_field();
                    }
                    s.render();
                }
            }
        }
        WM_MOUSEWHEEL => {
            let delta = ((w.0 >> 16) & 0xFFFF) as i16 as f32;
            let max = (s.content_h - s.client_h()).max(0.0);
            s.scroll = (s.scroll - delta / 120.0 * 56.0 * s.scale).clamp(0.0, max);
            s.render();
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            if s.on_key(w.0 as u32) {
                return LRESULT(0);
            }
            // Ctrl+Tab / Ctrl+Shift+Tab move between pages.
            if w.0 as u16 == VK_TAB.0 && key_down(VK_CONTROL) {
                let n = PAGES.len();
                let next = if key_down(VK_SHIFT) { (s.page + n - 1) % n } else { (s.page + 1) % n };
                s.click(Hit::Nav(next));
                return LRESULT(0);
            }
        }
        WM_KEYUP | WM_SYSKEYUP => {
            if s.recording {
                s.render();
                return LRESULT(0);
            }
        }
        WM_CHAR => {
            if s.recording || s.on_char(w.0 as u16) {
                return LRESULT(0);
            }
        }
        WM_SYSCHAR => {
            if s.recording {
                return LRESULT(0);
            }
        }
        WM_KILLFOCUS => {
            if s.recording {
                s.stop_recording();
            }
            s.render();
        }
        WM_DPICHANGED => {
            s.scale = ((w.0 & 0xFFFF) as f32) / 96.0;
            s.fmts = None;
            let r = &*(l.0 as *const RECT);
            let _ = SetWindowPos(h, HWND::default(), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            s.render();
            return LRESULT(0);
        }
        WM_SETTINGCHANGE => {
            apply_titlebar(h);
            s.render();
        }
        WM_DESTROY => {
            s.stop_recording();
            if s.field.is_some() {
                s.commit_field();
            }
            let _ = KillTimer(h, TIMER_REFRESH);
            s.rt = None;
            s.rtb = None;
            s.brush = None;
            s.brush2 = None;
            s.fmts = None;
            s.hwnd = HWND::default();
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(h, msg, w, l)
}
