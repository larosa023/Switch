//! Settings window: a small custom-drawn Direct2D window so it matches the
//! switcher's look (no dated common controls).

use windows::core::{w, Result};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_U;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::{self, cfg, Hotkey, Layout, Monitors, PreviewSize, Theme};
use crate::gfx::{self, gfx, Painter, Palette, C, R};
use crate::hook;
use crate::updater::{self, State};
use crate::util::{self, UiCell};

#[derive(Clone, Copy, PartialEq, Debug)]
enum Id {
    Autostart,
    AltTab,
    Animations,
    Group,
    Minimized,
    AutoUpdate,
    Theme,
    Layout,
    Size,
    Monitors,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Toggle(Id),
    Seg(Id, usize),
    Accent(usize),
    HotkeyRecord,
    HotkeyClear,
    Unhide(usize),
    UpdateBtn,
    OpenConfig,
    OpenRepo,
}

struct Fmts {
    title: IDWriteTextFormat,
    label: IDWriteTextFormat,
    desc: IDWriteTextFormat,
    section: IDWriteTextFormat,
    ctl: IDWriteTextFormat,
    ctl_c: IDWriteTextFormat,
    small: IDWriteTextFormat,
}

struct Settings {
    hwnd: HWND,
    rt: Option<ID2D1HwndRenderTarget>,
    rtb: Option<ID2D1RenderTarget>,
    brush: Option<ID2D1SolidColorBrush>,
    fmts: Option<Fmts>,
    scale: f32,
    scroll: f32,
    content_h: f32,
    hits: Vec<(R, Hit)>,
    hover: Option<Hit>,
    recording: bool,
    tracking: bool,
}

static ST: UiCell<Settings> = UiCell::new();

type Ctl = Box<dyn Fn(&mut Settings, &Painter, &Palette, R)>;

const W: f32 = 600.0;
const H: f32 = 760.0;

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

unsafe fn create() -> Result<()> {
    let inst = GetModuleHandleW(None)?;
    let icon = LoadImageW(inst, PCWSTR_RES(1), IMAGE_ICON, 0, 0, LR_DEFAULTSIZE | LR_SHARED)
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
    let ch = (H * scale).min((work.bottom - work.top) as f32 * 0.9);
    let mut rc = RECT { left: 0, top: 0, right: (W * scale) as i32, bottom: ch as i32 };
    let _ = AdjustWindowRectExForDpi(&mut rc, style, false, WINDOW_EX_STYLE(0), dx);
    let (ww, wh) = (rc.right - rc.left, rc.bottom - rc.top);
    let x = work.left + ((work.right - work.left) - ww) / 2;
    let y = work.top + ((work.bottom - work.top) - wh) / 2;

    ST.set(Settings {
        hwnd: HWND::default(),
        rt: None,
        rtb: None,
        brush: None,
        fmts: None,
        scale,
        scroll: 0.0,
        content_h: 0.0,
        hits: Vec::new(),
        hover: None,
        recording: false,
        tracking: false,
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

#[allow(non_snake_case)]
fn PCWSTR_RES(id: u16) -> windows::core::PCWSTR {
    windows::core::PCWSTR(id as usize as *const u16)
}

unsafe fn apply_titlebar(hwnd: HWND) {
    let pal = gfx::palette(cfg());
    let dark: BOOL = pal.dark.into();
    let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, 4);
    let c = pal.bg;
    let cref = COLORREF(((c.2 * 255.0) as u32) << 16 | ((c.1 * 255.0) as u32) << 8 | (c.0 * 255.0) as u32);
    let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, &cref as *const _ as *const _, 4);
}

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
                self.rtb = Some(b);
                self.rt = Some(rt);
            }
        }
        if self.fmts.is_none() {
            let s = self.scale;
            let g = gfx();
            let mk = |sz: f32, wt, al| g.text_format(sz * s, wt, al).unwrap();
            self.fmts = Some(Fmts {
                title: mk(22.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING),
                label: mk(14.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING),
                desc: mk(12.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING),
                section: mk(12.5, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING),
                ctl: mk(13.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING),
                ctl_c: mk(13.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER),
                small: mk(12.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING),
            });
        }
    }

    fn render(&mut self) {
        self.ensure();
        let (Some(rt), Some(rtb), Some(brush)) = (self.rt.clone(), self.rtb.clone(), self.brush.clone()) else {
            return;
        };
        let pal = gfx::palette(cfg());
        let p = Painter { rt: &rtb, brush: &brush, alpha: 1.0 };
        unsafe {
            rt.BeginDraw();
            rt.Clear(Some(&pal.bg.d2d()));
        }
        self.hits.clear();
        self.draw(&p, &pal);
        unsafe {
            if let Err(e) = rt.EndDraw(None, None) {
                if e.code() == D2DERR_RECREATE_TARGET {
                    self.rt = None;
                    self.rtb = None;
                    self.brush = None;
                }
            }
            let _ = ValidateRect(self.hwnd, None);
        }
    }

    fn draw(&mut self, p: &Painter, pal: &Palette) {
        let s = self.scale;
        let f = self.fmts.take().unwrap();
        let m = 28.0 * s;
        let cw = W * s - 2.0 * m;
        let mut y = 24.0 * s - self.scroll;

        // Header
        let icon_r = R::new(m, y + 2.0 * s, 30.0 * s, 30.0 * s);
        p.fill(icon_r, 8.0 * s, pal.accent);
        let i = icon_r.inset(7.0 * s);
        p.fill(R::new(i.x, i.y, i.w * 0.7, i.h * 0.62), 2.0 * s, C::hex(0xFFFFFF).a(0.6));
        p.fill(R::new(i.x + i.w * 0.3, i.y + i.h * 0.38, i.w * 0.7, i.h * 0.62), 2.0 * s, C::hex(0xFFFFFF));
        p.text("Switch", &f.title, R::new(m + 42.0 * s, y, 300.0 * s, 34.0 * s), pal.text);
        y += 52.0 * s;

        let c = cfg().clone();

        // ---- General
        y = self.section(p, pal, &f, "General", m, y);
        let rows: Vec<(&str, String, Ctl)> = vec![
            (
                "Start with Windows",
                "Run Switch in the tray when you sign in".into(),
                Box::new(move |me: &mut Self, p: &Painter, pal: &Palette, r: R| me.toggle(p, pal, r, Id::Autostart, config::autostart_enabled())),
            ),
            (
                "Replace Alt+Tab",
                "Hold Alt and tap Tab. Ctrl+Alt+Tab keeps it open.".into(),
                Box::new(move |me: &mut Self, p: &Painter, pal: &Palette, r: R| me.toggle(p, pal, r, Id::AltTab, c.alt_tab)),
            ),
            (
                "Custom shortcut",
                "Shortcuts with Alt switch on release; others stay open".into(),
                Box::new(move |me: &mut Self, p: &Painter, pal: &Palette, r: R| me.hotkey(p, pal, r, c.hotkey)),
            ),
        ];
        self.fmts = Some(f);
        y = self.card(p, pal, m, y, cw, rows);
        let f = self.fmts.take().unwrap();

        // ---- Appearance
        y = self.section(p, pal, &f, "Appearance", m, y + 8.0 * s);
        let theme_i = match c.theme {
            Theme::System => 0,
            Theme::Light => 1,
            Theme::Dark => 2,
        };
        let layout_i = match c.layout {
            Layout::Grid => 0,
            Layout::Row => 1,
        };
        let size_i = match c.preview_size {
            PreviewSize::Small => 0,
            PreviewSize::Medium => 1,
            PreviewSize::Large => 2,
        };
        let accent = c.accent.clone();
        let rows: Vec<(&str, String, Ctl)> = vec![
            (
                "Theme",
                String::new(),
                Box::new(move |me: &mut Self, p: &Painter, pal: &Palette, r: R| me.seg(p, pal, r, Id::Theme, &["System", "Light", "Dark"], theme_i)),
            ),
            (
                "Accent color",
                String::new(),
                Box::new(move |me: &mut Self, p: &Painter, pal: &Palette, r: R| me.accents(p, pal, r, &accent)),
            ),
            (
                "Layout",
                String::new(),
                Box::new(move |me: &mut Self, p: &Painter, pal: &Palette, r: R| me.seg(p, pal, r, Id::Layout, &["Grid", "Row"], layout_i)),
            ),
            (
                "Preview size",
                String::new(),
                Box::new(move |me: &mut Self, p: &Painter, pal: &Palette, r: R| me.seg(p, pal, r, Id::Size, &["Small", "Medium", "Large"], size_i)),
            ),
            (
                "Animations",
                "Fade in and glide the selection".into(),
                Box::new(move |me: &mut Self, p: &Painter, pal: &Palette, r: R| me.toggle(p, pal, r, Id::Animations, c.animations)),
            ),
        ];
        self.fmts = Some(f);
        y = self.card(p, pal, m, y, cw, rows);
        let f = self.fmts.take().unwrap();

        // ---- Windows
        y = self.section(p, pal, &f, "Windows", m, y + 8.0 * s);
        let mon_i = if c.monitors == Monitors::All { 0 } else { 1 };
        let rows: Vec<(&str, String, Ctl)> = vec![
            (
                "Show windows from",
                "Current = the monitor your cursor is on".into(),
                Box::new(move |me: &mut Self, p: &Painter, pal: &Palette, r: R| me.seg(p, pal, r, Id::Monitors, &["All monitors", "Current"], mon_i)),
            ),
            (
                "Group by app",
                "Ctrl+G toggles it in the switcher".into(),
                Box::new(move |me: &mut Self, p: &Painter, pal: &Palette, r: R| me.toggle(p, pal, r, Id::Group, c.group_by_app)),
            ),
            (
                "Show minimized windows",
                String::new(),
                Box::new(move |me: &mut Self, p: &Painter, pal: &Palette, r: R| me.toggle(p, pal, r, Id::Minimized, c.show_minimized)),
            ),
        ];
        self.fmts = Some(f);
        y = self.card(p, pal, m, y, cw, rows);
        let f = self.fmts.take().unwrap();
        y = self.hidden_card(p, pal, &f, m, y + 10.0 * s, cw, &c.hidden_apps);

        // ---- Updates
        y = self.section(p, pal, &f, "Updates", m, y + 8.0 * s);
        let rows: Vec<(&str, String, Ctl)> = vec![
            (
                "Check for updates automatically",
                "Looks for new releases on GitHub".into(),
                Box::new(move |me: &mut Self, p: &Painter, pal: &Palette, r: R| me.toggle(p, pal, r, Id::AutoUpdate, c.auto_update)),
            ),
            (
                "",
                String::new(),
                Box::new(move |me: &mut Self, p: &Painter, pal: &Palette, r: R| me.update_row(p, pal, r)),
            ),
        ];
        self.fmts = Some(f);
        y = self.card(p, pal, m, y, cw, rows);
        let f = self.fmts.take().unwrap();

        // ---- Shortcuts cheat sheet
        y = self.section(p, pal, &f, "In the switcher", m, y + 8.0 * s);
        let tips = [
            ("Type", "Search by title or app"),
            ("Enter / click", "Switch to window"),
            ("Arrows, Tab", "Move selection"),
            ("Esc", "Clear search, then close"),
            ("Del, Ctrl+W, middle-click", "Close window"),
            ("Ctrl+M", "Minimize window"),
            ("Ctrl+P", "Pin window on top"),
            ("Ctrl+H", "Hide this app from Switch"),
        ];
        let row_h = 26.0 * s;
        let card = R::new(m, y, cw, tips.len() as f32 * row_h + 20.0 * s);
        p.fill(card, 10.0 * s, pal.surface);
        p.stroke(card, 10.0 * s, pal.border, 1.0 * s);
        for (i, (k, v)) in tips.iter().enumerate() {
            let ry = y + 10.0 * s + i as f32 * row_h;
            p.text(k, &f.ctl, R::new(m + 18.0 * s, ry, 200.0 * s, row_h), pal.text);
            p.text(v, &f.ctl, R::new(m + 230.0 * s, ry, cw - 250.0 * s, row_h), pal.subtext);
        }
        y = card.bottom() + 18.0 * s;

        // ---- Footer
        let ver = format!("Switch {}", updater::VERSION);
        p.text(&ver, &f.small, R::new(m, y, 120.0 * s, 20.0 * s), pal.faint);
        let gh = R::new(m + 120.0 * s, y, 60.0 * s, 20.0 * s);
        let cfg_r = R::new(m + 190.0 * s, y, 110.0 * s, 20.0 * s);
        let link = |r: R, t: &str, h: Hit| {
            let hot = self.hover == Some(h);
            p.text(t, &f.small, r, if hot { pal.accent } else { pal.subtext });
        };
        link(gh, "GitHub", Hit::OpenRepo);
        link(cfg_r, "Settings file", Hit::OpenConfig);
        self.hits.push((gh, Hit::OpenRepo));
        self.hits.push((cfg_r, Hit::OpenConfig));
        y += 40.0 * s;

        self.content_h = y + self.scroll;
        self.fmts = Some(f);
    }

    fn section(&mut self, p: &Painter, pal: &Palette, f: &Fmts, t: &str, x: f32, y: f32) -> f32 {
        let s = self.scale;
        p.text(t, &f.section, R::new(x + 4.0 * s, y, 300.0 * s, 22.0 * s), pal.subtext);
        y + 28.0 * s
    }

    fn card(
        &mut self,
        p: &Painter,
        pal: &Palette,
        x: f32,
        y: f32,
        w: f32,
        rows: Vec<(&str, String, Ctl)>,
    ) -> f32 {
        let s = self.scale;
        let row_h = 58.0 * s;
        let card = R::new(x, y, w, row_h * rows.len() as f32);
        p.fill(card, 10.0 * s, pal.surface);
        p.stroke(card, 10.0 * s, pal.border, 1.0 * s);
        for (i, (label, desc, ctl)) in rows.into_iter().enumerate() {
            let ry = y + i as f32 * row_h;
            if i > 0 {
                p.line(x + 16.0 * s, ry, x + w - 16.0 * s, ry, pal.border, 1.0 * s);
            }
            let f = self.fmts.take().unwrap();
            if !label.is_empty() {
                if desc.is_empty() {
                    p.text(label, &f.label, R::new(x + 18.0 * s, ry, w * 0.5, row_h), pal.text);
                } else {
                    p.text(label, &f.label, R::new(x + 18.0 * s, ry + 9.0 * s, w * 0.6, 22.0 * s), pal.text);
                    p.text(&desc, &f.desc, R::new(x + 18.0 * s, ry + 29.0 * s, w * 0.62, 18.0 * s), pal.subtext);
                }
            }
            self.fmts = Some(f);
            ctl(self, p, pal, R::new(x, ry, w, row_h));
        }
        card.bottom()
    }

    fn toggle(&mut self, p: &Painter, pal: &Palette, row: R, id: Id, on: bool) {
        let s = self.scale;
        let (tw, th) = (40.0 * s, 22.0 * s);
        let r = R::new(row.right() - 18.0 * s - tw, row.y + (row.h - th) / 2.0, tw, th);
        let hot = self.hover == Some(Hit::Toggle(id));
        if on {
            p.fill(r, th / 2.0, if hot { pal.accent.mix(C::hex(0xFFFFFF), 0.12) } else { pal.accent });
            p.circle(r.right() - th / 2.0, r.y + th / 2.0, th / 2.0 - 4.0 * s, pal.on_accent);
        } else {
            p.fill(r, th / 2.0, if hot { pal.surface_hover } else { pal.surface });
            p.stroke(r, th / 2.0, pal.subtext.a(0.8), 1.2 * s);
            p.circle(r.x + th / 2.0, r.y + th / 2.0, th / 2.0 - 5.0 * s, pal.subtext);
        }
        self.hits.push((R::new(r.x - 6.0 * s, row.y, tw + 12.0 * s, row.h), Hit::Toggle(id)));
    }

    fn seg(&mut self, p: &Painter, pal: &Palette, row: R, id: Id, opts: &[&str], cur: usize) {
        let s = self.scale;
        let f = self.fmts.take().unwrap();
        let widths: Vec<f32> = opts.iter().map(|o| gfx().measure(o, &f.ctl_c) + 26.0 * s).collect();
        let total: f32 = widths.iter().sum::<f32>() + 6.0 * s;
        let h = 32.0 * s;
        let r = R::new(row.right() - 18.0 * s - total, row.y + (row.h - h) / 2.0, total, h);
        p.fill(r, 8.0 * s, pal.thumb_bg);
        let mut x = r.x + 3.0 * s;
        for (i, o) in opts.iter().enumerate() {
            let ir = R::new(x, r.y + 3.0 * s, widths[i], h - 6.0 * s);
            let hit = Hit::Seg(id, i);
            if i == cur {
                p.fill(ir, 6.0 * s, pal.surface_hover.mix(pal.accent, 0.0));
                p.fill(ir, 6.0 * s, pal.accent.a(0.9));
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
        self.fmts = Some(f);
    }

    fn accents(&mut self, p: &Painter, pal: &Palette, row: R, cur: &str) {
        let s = self.scale;
        let d = 24.0 * s;
        let gap = 10.0 * s;
        let n = gfx::ACCENTS.len() + 1;
        let total = n as f32 * d + (n - 1) as f32 * gap;
        let mut x = row.right() - 18.0 * s - total;
        let cy = row.y + row.h / 2.0;
        for i in 0..n {
            let (color, selected) = if i == 0 {
                (C::hex(gfx::system_accent()), cur == "system")
            } else {
                let (_, hex) = gfx::ACCENTS[i - 1];
                (C::hex(hex), gfx::parse_hex(cur) == Some(hex))
            };
            let cx = x + d / 2.0;
            let hit = Hit::Accent(i);
            if i == 0 {
                // "System" swatch: ring with the system accent inside.
                p.circle(cx, cy, d / 2.0, pal.thumb_bg);
                p.circle(cx, cy, d / 2.0 - 5.0 * s, color);
            } else {
                p.circle(cx, cy, d / 2.0, color);
            }
            if selected {
                p.ring(cx, cy, d / 2.0 + 3.5 * s, pal.text, 1.6 * s);
            } else if self.hover == Some(hit) {
                p.ring(cx, cy, d / 2.0 + 3.0 * s, pal.subtext.a(0.6), 1.2 * s);
            }
            self.hits.push((R::new(x - 3.0 * s, cy - d / 2.0 - 3.0 * s, d + 6.0 * s, d + 6.0 * s), hit));
            x += d + gap;
        }
    }

    fn hotkey(&mut self, p: &Painter, pal: &Palette, row: R, hk: Option<Hotkey>) {
        let s = self.scale;
        let f = self.fmts.take().unwrap();
        let w = 190.0 * s;
        let h = 32.0 * s;
        let r = R::new(row.right() - 18.0 * s - w, row.y + (row.h - h) / 2.0, w, h);
        let hot = self.hover == Some(Hit::HotkeyRecord);
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
            let label = hk.map(|h| hotkey_label(&h)).unwrap_or_else(|| "Not set".into());
            let color = if hk.is_some() { pal.text } else { pal.faint };
            let tr = if hk.is_some() { R::new(r.x, r.y, r.w - 24.0 * s, r.h) } else { r };
            p.text(&label, &f.ctl_c, tr, color);
            if hk.is_some() {
                let cr = R::new(r.right() - 28.0 * s, r.y + 4.0 * s, 24.0 * s, r.h - 8.0 * s);
                if self.hover == Some(Hit::HotkeyClear) {
                    p.fill(cr, 6.0 * s, pal.surface_hover);
                }
                p.glyph_close(cr.x + cr.w / 2.0, cr.y + cr.h / 2.0, 3.5 * s, pal.subtext, 1.2 * s);
                self.hits.push((cr, Hit::HotkeyClear));
            }
        }
        self.hits.push((r, Hit::HotkeyRecord));
        self.fmts = Some(f);
    }

    fn hidden_card(&mut self, p: &Painter, pal: &Palette, f: &Fmts, x: f32, y: f32, w: f32, apps: &[String]) -> f32 {
        let s = self.scale;
        let pad = 18.0 * s;
        // Measure chips first to size the card.
        let chip_h = 28.0 * s;
        let mut chips: Vec<(R, usize)> = Vec::new();
        let (mut cx, mut cy) = (x + pad, y + 54.0 * s);
        for (i, a) in apps.iter().enumerate() {
            let cw = gfx().measure(a, &f.ctl) + 44.0 * s;
            if cx + cw > x + w - pad && cx > x + pad {
                cx = x + pad;
                cy += chip_h + 8.0 * s;
            }
            chips.push((R::new(cx, cy, cw, chip_h), i));
            cx += cw + 8.0 * s;
        }
        let bottom = if apps.is_empty() { y + 58.0 * s } else { cy + chip_h + pad };
        let card = R::new(x, y, w, bottom - y);
        p.fill(card, 10.0 * s, pal.surface);
        p.stroke(card, 10.0 * s, pal.border, 1.0 * s);
        p.text("Hidden apps", &f.label, R::new(x + pad, y + 9.0 * s, w * 0.6, 22.0 * s), pal.text);
        let desc = if apps.is_empty() {
            "None. Press Ctrl+H in the switcher to hide the selected app."
        } else {
            "These apps never appear in Switch. Click \u{00D7} to show one again."
        };
        p.text(desc, &f.desc, R::new(x + pad, y + 29.0 * s, w - 2.0 * pad, 18.0 * s), pal.subtext);
        for (r, i) in chips {
            let hit = Hit::Unhide(i);
            p.fill(r, r.h / 2.0, if self.hover == Some(hit) { pal.surface_hover } else { pal.thumb_bg });
            p.text(&apps[i], &f.ctl, R::new(r.x + 12.0 * s, r.y, r.w - 36.0 * s, r.h), pal.text);
            p.glyph_close(r.right() - 16.0 * s, r.y + r.h / 2.0, 3.5 * s, pal.subtext, 1.2 * s);
            self.hits.push((r, hit));
        }
        card.bottom()
    }

    fn update_row(&mut self, p: &Painter, pal: &Palette, row: R) {
        let s = self.scale;
        let f = self.fmts.take().unwrap();
        let st = updater::state();
        let (msg, color, btn): (String, C, Option<&str>) = match &st {
            State::Idle => (format!("Version {}", updater::VERSION), pal.text, Some("Check now")),
            State::Checking => ("Checking for updates\u{2026}".into(), pal.subtext, None),
            State::UpToDate => (format!("You're up to date ({})", updater::VERSION), pal.text, Some("Check now")),
            State::Available(r) => (format!("Version {} is available", r.version), pal.accent, Some("Install & restart")),
            State::Downloading(v) => (format!("Downloading {v}\u{2026}"), pal.subtext, None),
            State::Error(e) => (format!("Update failed: {e}"), C::hex(0xE5484D), Some("Try again")),
        };
        let mut text_w = row.w - 36.0 * s;
        if let Some(b) = btn {
            let bw = gfx().measure(b, &f.ctl_c) + 32.0 * s;
            let h = 32.0 * s;
            let r = R::new(row.right() - 18.0 * s - bw, row.y + (row.h - h) / 2.0, bw, h);
            let hot = self.hover == Some(Hit::UpdateBtn);
            let primary = matches!(st, State::Available(_));
            if primary {
                p.fill(r, 8.0 * s, if hot { pal.accent.mix(C::hex(0xFFFFFF), 0.12) } else { pal.accent });
                p.text(b, &f.ctl_c, r, pal.on_accent);
            } else {
                p.fill(r, 8.0 * s, if hot { pal.surface_hover } else { pal.thumb_bg });
                p.text(b, &f.ctl_c, r, pal.text);
            }
            self.hits.push((r, Hit::UpdateBtn));
            text_w -= bw + 12.0 * s;
        }
        if matches!(st, State::UpToDate) {
            p.glyph_check(row.x + 26.0 * s, row.y + row.h / 2.0, 5.0 * s, C::hex(0x22C55E), 1.8 * s);
            p.text(&msg, &f.label, R::new(row.x + 40.0 * s, row.y, text_w - 22.0 * s, row.h), color);
        } else {
            p.text(&msg, &f.label, R::new(row.x + 18.0 * s, row.y, text_w, row.h), color);
        }
        self.fmts = Some(f);
    }

    fn click(&mut self, hit: Hit) {
        let c = cfg();
        match hit {
            Hit::Toggle(Id::Autostart) => config::set_autostart(!config::autostart_enabled()),
            Hit::Toggle(Id::AltTab) => c.alt_tab = !c.alt_tab,
            Hit::Toggle(Id::Animations) => c.animations = !c.animations,
            Hit::Toggle(Id::Group) => c.group_by_app = !c.group_by_app,
            Hit::Toggle(Id::Minimized) => c.show_minimized = !c.show_minimized,
            Hit::Toggle(Id::AutoUpdate) => {
                c.auto_update = !c.auto_update;
                if c.auto_update {
                    updater::check_async(true);
                }
            }
            Hit::Seg(Id::Theme, i) => {
                c.theme = [Theme::System, Theme::Light, Theme::Dark][i];
                unsafe { apply_titlebar(self.hwnd) };
            }
            Hit::Seg(Id::Layout, i) => c.layout = [Layout::Grid, Layout::Row][i],
            Hit::Seg(Id::Size, i) => c.preview_size = [PreviewSize::Small, PreviewSize::Medium, PreviewSize::Large][i],
            Hit::Seg(Id::Monitors, i) => c.monitors = [Monitors::All, Monitors::Current][i],
            Hit::Accent(i) => {
                c.accent = if i == 0 { "system".into() } else { format!("#{:06X}", gfx::ACCENTS[i - 1].1) };
            }
            Hit::HotkeyRecord => {
                self.recording = true;
                hook::RECORDING.store(true, std::sync::atomic::Ordering::Relaxed);
                unsafe {
                    let _ = SetFocus(self.hwnd);
                }
            }
            Hit::HotkeyClear => c.hotkey = None,
            Hit::Unhide(i) => {
                if i < c.hidden_apps.len() {
                    c.hidden_apps.remove(i);
                }
            }
            Hit::UpdateBtn => match updater::state() {
                State::Available(_) => updater::install_async(),
                _ => updater::check_async(false),
            },
            Hit::OpenConfig => {
                config::save();
                util::shell_open(&config::path().to_string_lossy());
            }
            Hit::OpenRepo => util::shell_open(&format!("https://github.com/{}", updater::REPO)),
            _ => {}
        }
        config::save();
        hook::apply_config(cfg());
        self.render();
    }

    fn stop_recording(&mut self) {
        self.recording = false;
        hook::RECORDING.store(false, std::sync::atomic::Ordering::Relaxed);
    }

    fn on_key(&mut self, vk: u32) -> bool {
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
        config::save();
        hook::apply_config(cfg());
        self.render();
        true
    }
}


fn mouse_xy(l: LPARAM) -> (f32, f32) {
    ((l.0 & 0xFFFF) as i16 as f32, ((l.0 >> 16) & 0xFFFF) as i16 as f32)
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let Some(s) = ST.try_get() else {
        return DefWindowProcW(h, msg, w, l);
    };
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
        WM_MOUSEMOVE => {
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
            let (x, y) = mouse_xy(l);
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
                let cursor = if s.hover.is_some() { IDC_HAND } else { IDC_ARROW };
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
        WM_LBUTTONUP => {
            let (x, y) = mouse_xy(l);
            let hit = s.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| *h);
            if s.recording && hit != Some(Hit::HotkeyRecord) {
                s.stop_recording();
            }
            if let Some(hit) = hit {
                if !(s.recording && hit == Hit::HotkeyRecord) {
                    s.click(hit);
                }
            } else {
                s.render();
            }
        }
        WM_MOUSEWHEEL => {
            let delta = ((w.0 >> 16) & 0xFFFF) as i16 as f32;
            let mut rc = RECT::default();
            let _ = GetClientRect(h, &mut rc);
            let max = (s.content_h - rc.bottom as f32).max(0.0);
            s.scroll = (s.scroll - delta / 120.0 * 48.0 * s.scale).clamp(0.0, max);
            s.render();
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            if s.on_key(w.0 as u32) {
                return LRESULT(0);
            }
        }
        WM_KEYUP | WM_SYSKEYUP => {
            if s.recording {
                s.render();
                return LRESULT(0);
            }
        }
        WM_SYSCHAR | WM_CHAR => {
            if s.recording {
                return LRESULT(0);
            }
        }
        WM_KILLFOCUS => {
            if s.recording {
                s.stop_recording();
                s.render();
            }
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
            s.rt = None;
            s.rtb = None;
            s.brush = None;
            s.fmts = None;
            s.hwnd = HWND::default();
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(h, msg, w, l)
}
