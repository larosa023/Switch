//! The switcher overlay: a topmost, non-activating popup that shows live DWM
//! thumbnails of every window, with fuzzy search, grouping, three layouts
//! and window actions.

use std::collections::HashMap;
use std::time::Instant;

use windows::core::{w, Result};
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_U;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::ProcessStatus::K32EmptyWorkingSet;
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::{self, cfg, Grouping, Highlight, Initial, Label, Layout, Monitors, OpenOn, Position, SearchBar, Sort};
use crate::fuzzy;
use crate::gfx::{self, gfx, Painter, Palette, C, R};
use crate::hook::{self, *};
use crate::util::{self, utf16, wide, UiCell};
use crate::winlist::{self, WinInfo};

const TIMER_SHOW: usize = 1;
const TIMER_TRIM: usize = 3;
const WM_SW_ANIM: u32 = WM_APP + 30;
const OPEN_MS: f32 = 160.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Btn {
    Pin,
    Min,
    Close,
}
const BTNS: [Btn; 3] = [Btn::Pin, Btn::Min, Btn::Close];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Act {
    Close,
    CloseApp,
    Minimize,
    Maximize,
    Pin,
    MoveNext,
    MovePrev,
    OpenLocation,
    NewWindow,
    HideApp,
}

#[derive(Clone, Default)]
struct Item {
    win: usize,
    /// Windows represented (Collapse grouping), 1 otherwise.
    count: usize,
    /// UTF-16 ranges of the title matched by the search.
    ranges: Vec<(u32, u32)>,
    row: usize,
    visible: bool,
    tile: R,
    title: R,
    icon: R,
    thumb_area: R,
    btns: [R; 3],
}

struct Fmts {
    title: IDWriteTextFormat,
    sub: IDWriteTextFormat,
    search: IDWriteTextFormat,
    meta: IDWriteTextFormat,
    empty: IDWriteTextFormat,
    letter: IDWriteTextFormat,
    hint: IDWriteTextFormat,
    badge: IDWriteTextFormat,
}

pub struct Switcher {
    hwnd: HWND,
    dim: HWND,
    rt: Option<ID2D1HwndRenderTarget>,
    rtb: Option<ID2D1RenderTarget>,
    brush: Option<ID2D1SolidColorBrush>,
    brush2: Option<ID2D1SolidColorBrush>,
    fmts: Option<Fmts>,
    fmt_key: (f32, f32),
    pal: Palette,

    all: Vec<WinInfo>,
    items: Vec<Item>,
    sel: usize,
    query: String,
    grouping: Grouping,
    layout: Layout,
    same_app: Option<String>,
    search_visible: bool,

    session: bool,
    visible: bool,
    scale: f32,
    work: RECT,
    mon_rect: RECT,
    panel_w: f32,
    panel_h: f32,
    thumb_h: f32,
    list_first: usize,
    list_rows: usize,
    search_r: R,
    content_r: R,
    footer_r: R,
    preview_r: R,

    thumbs: HashMap<isize, isize>,
    icons: HashMap<isize, Option<ID2D1Bitmap>>,

    hover: Option<usize>,
    hover_btn: Option<(usize, Btn)>,
    tracking: bool,

    hl: R,
    fade: f32,
    zoom: f32,
    t_open: Instant,
    t_session: Instant,
    t_last: Instant,
    animating: bool,
    /// Foreground window to give focus back to after a context menu.
    restore_fg: Option<isize>,
}

static SW: UiCell<Switcher> = UiCell::new();
fn sw() -> &'static mut Switcher {
    SW.get()
}

pub fn create() -> Result<HWND> {
    unsafe {
        let inst = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DROPSHADOW,
            lpfnWndProc: Some(wndproc),
            hInstance: inst.into(),
            hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW)?,
            lpszClassName: w!("Switch.Overlay"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            w!("Switch.Overlay"),
            w!("Switch"),
            WS_POPUP,
            0,
            0,
            100,
            100,
            HWND::default(),
            HMENU::default(),
            inst,
            None,
        )?;

        // Full-monitor backdrop for "Dim the desktop".
        let wc_dim = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(dim_proc),
            hInstance: inst.into(),
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            lpszClassName: w!("Switch.Dim"),
            ..Default::default()
        };
        RegisterClassExW(&wc_dim);
        let dim = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            w!("Switch.Dim"),
            w!(""),
            WS_POPUP,
            0,
            0,
            1,
            1,
            HWND::default(),
            HMENU::default(),
            inst,
            None,
        )
        .unwrap_or_default();

        let now = Instant::now();
        SW.set(Switcher {
            hwnd,
            dim,
            rt: None,
            rtb: None,
            brush: None,
            brush2: None,
            fmts: None,
            fmt_key: (0.0, 0.0),
            pal: gfx::palette(cfg()),
            all: Vec::new(),
            items: Vec::new(),
            sel: 0,
            query: String::new(),
            grouping: Grouping::Off,
            layout: Layout::Grid,
            same_app: None,
            search_visible: true,
            session: false,
            visible: false,
            scale: 1.0,
            work: RECT::default(),
            mon_rect: RECT::default(),
            panel_w: 100.0,
            panel_h: 100.0,
            thumb_h: 150.0,
            list_first: 0,
            list_rows: 0,
            search_r: R::default(),
            content_r: R::default(),
            footer_r: R::default(),
            preview_r: R::default(),
            thumbs: HashMap::new(),
            icons: HashMap::new(),
            hover: None,
            hover_btn: None,
            tracking: false,
            hl: R::default(),
            fade: 1.0,
            zoom: 1.0,
            t_open: now,
            t_session: now,
            t_last: now,
            animating: false,
            restore_fg: None,
        });
        // Warm up Direct2D so the first Alt+Tab is instant.
        sw().ensure_target();
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            HMODULE::default(),
            Some(on_foreground),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        );
        Ok(hwnd)
    }
}

/// Open the switcher in sticky mode (tray menu, `--open`).
pub fn open_sticky() {
    if sw().session {
        return;
    }
    hook::open_from_ui();
    sw().open(OPEN_STICKY);
}

pub fn is_active() -> bool {
    SW.try_get().map(|s| s.session).unwrap_or(false)
}

unsafe extern "system" fn on_foreground(_: HWINEVENTHOOK, _: u32, h: HWND, _: i32, _: i32, _: u32, _: u32) {
    let Some(s) = SW.try_get() else { return };
    if !s.session || h.is_invalid() || h == s.hwnd {
        return;
    }
    // Ignore our own windows (context menus) and the focus shuffle right after opening.
    let mut pid = 0;
    GetWindowThreadProcessId(h, Some(&mut pid));
    if pid == std::process::id() || s.t_session.elapsed().as_millis() < 400 {
        return;
    }
    s.close();
}

fn colorref(c: C) -> COLORREF {
    COLORREF(((c.2 * 255.0) as u32) << 16 | ((c.1 * 255.0) as u32) << 8 | (c.0 * 255.0) as u32)
}

impl Switcher {
    // ---- session lifecycle ---------------------------------------------------

    fn open(&mut self, flags: usize) {
        if self.session {
            self.step(if flags & OPEN_REVERSE != 0 { -1 } else { 1 });
            return;
        }
        let c = cfg();
        self.session = true;
        self.t_session = Instant::now();
        hook::UI_ACK.store(true, std::sync::atomic::Ordering::Relaxed);
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_TRIM);
        }
        self.pal = gfx::palette(c);
        self.query.clear();
        self.grouping = c.grouping;
        self.layout = c.layout;
        self.search_visible = c.search_bar == SearchBar::Always;
        self.hover = None;
        self.hover_btn = None;
        self.list_first = 0;

        unsafe {
            let fg = GetForegroundWindow();
            let mon = match c.open_on {
                OpenOn::Cursor => {
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST)
                }
                OpenOn::Active => MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST),
                OpenOn::Primary => MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY),
            };
            self.work = winlist::monitor_work_area(mon);
            self.mon_rect = winlist::monitor_rect(mon);
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            self.scale = dx as f32 / 96.0;

            let mut all = winlist::enumerate(c.other_desktops);
            all.retain(|w| {
                !c.hidden_apps.iter().any(|h| h.eq_ignore_ascii_case(&w.exe))
                    && (c.show_minimized || !w.minimized)
                    && (c.monitors == Monitors::All || w.monitor == mon.0 as isize || w.minimized)
            });
            let fg_first = all.first().map(|w| util::hwnd(w.hwnd) == fg).unwrap_or(false);

            // Alt+` : only the foreground app's windows.
            self.same_app = None;
            if flags & OPEN_SAME_APP != 0 {
                let exe = all
                    .iter()
                    .find(|w| util::hwnd(w.hwnd) == fg)
                    .or(all.first())
                    .map(|w| w.exe.clone())
                    .unwrap_or_default();
                all.retain(|w| w.exe == exe);
                self.grouping = Grouping::Off;
                self.same_app = Some(exe);
            }

            self.all = all;
            let live: std::collections::HashSet<isize> = self.all.iter().map(|w| w.hwnd).collect();
            self.icons.retain(|k, _| live.contains(k));

            self.rebuild();
            self.layout_panel();
            let n = self.items.len();
            self.sel = if n == 0 {
                0
            } else if flags & OPEN_REVERSE != 0 {
                n - 1
            } else if fg_first && n > 1 && c.initial == Initial::Previous {
                1
            } else {
                0
            };
            self.place_items();

            let delay = c.show_delay_ms;
            if delay == 0 || flags & OPEN_STICKY != 0 {
                self.show();
            } else {
                SetTimer(self.hwnd, TIMER_SHOW, delay, None);
            }
        }
    }

    fn panel_origin(&self) -> (i32, i32) {
        let ww = (self.work.right - self.work.left) as f32;
        let wh = (self.work.bottom - self.work.top) as f32;
        let x = self.work.left + ((ww - self.panel_w) / 2.0) as i32;
        let y = match cfg().position {
            Position::Center => self.work.top + ((wh - self.panel_h) / 2.0) as i32,
            Position::Top => self.work.top + (wh * 0.1) as i32,
            Position::Bottom => self.work.bottom - self.panel_h as i32 - (wh * 0.08) as i32,
        };
        (x, y.max(self.work.top))
    }

    fn show(&mut self) {
        if !self.session || self.visible {
            return;
        }
        let c = cfg();
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_SHOW);
            if c.dim_background && !self.dim.is_invalid() {
                let m = self.mon_rect;
                let _ = SetLayeredWindowAttributes(self.dim, COLORREF(0), 0, LWA_ALPHA);
                let _ = SetWindowPos(
                    self.dim,
                    HWND_TOPMOST,
                    m.left,
                    m.top,
                    m.right - m.left,
                    m.bottom - m.top,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
            let (x, y) = self.panel_origin();
            let (w, h) = (self.panel_w.round() as i32, self.panel_h.round() as i32);
            let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE | SWP_NOREDRAW);
            self.resize_target(w as u32, h as u32);
            let radius = c.corner_radius;
            let pref = if radius == 0 {
                DWMWCP_DONOTROUND
            } else if radius < 8 {
                DWMWCP_ROUNDSMALL
            } else {
                DWMWCP_ROUND
            };
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &pref as *const _ as *const _, 4);
            let border = colorref(self.pal.border.mix(self.pal.bg, 0.4));
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_BORDER_COLOR, &border as *const _ as *const _, 4);

            self.visible = true;
            self.t_open = Instant::now();
            self.t_last = self.t_open;
            let animate = c.anim_factor() > 0.0;
            self.fade = if animate { 0.0 } else { 1.0 };
            self.zoom = if animate { 0.965 } else { 1.0 };
            if let Some(it) = self.items.get(self.sel) {
                self.hl = it.tile;
            }
            self.render();
            self.update_thumbs();
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            self.kick();
        }
    }

    fn close(&mut self) {
        if !self.session {
            return;
        }
        self.session = false;
        hook::end_session();
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_SHOW);
            self.animating = false;
            if self.visible {
                for (_, t) in self.thumbs.drain() {
                    let _ = DwmUnregisterThumbnail(t);
                }
                // Leave a blank frame behind so the next open never flashes stale content.
                self.items.clear();
                self.fade = 0.0;
                self.render();
                let _ = ShowWindow(self.hwnd, SW_HIDE);
                if !self.dim.is_invalid() {
                    let _ = ShowWindow(self.dim, SW_HIDE);
                }
                self.visible = false;
            }
            self.thumbs.clear();
            self.items.clear();
            self.all.clear();
            if let Some(prev) = self.restore_fg.take() {
                if GetForegroundWindow() == self.hwnd && !IsIconic(util::hwnd(prev)).as_bool() {
                    winlist::activate(prev);
                }
            }
            if cfg().trim_memory {
                SetTimer(self.hwnd, TIMER_TRIM, 10_000, None);
            }
        }
    }

    fn commit(&mut self) {
        if !self.session {
            return;
        }
        let target = self.items.get(self.sel).map(|it| self.all[it.win].hwnd);
        if target.is_some() {
            self.restore_fg = None;
        }
        self.close();
        if let Some(h) = target {
            winlist::activate(h);
        }
    }

    // ---- list building & layout ----------------------------------------------

    fn rebuild(&mut self) {
        let q = self.query.trim().to_string();
        let mut picked: Vec<(usize, i32, Vec<(u32, u32)>)> = Vec::new();
        for (i, w) in self.all.iter().enumerate() {
            if q.is_empty() {
                picked.push((i, 0, Vec::new()));
            } else if let Some(m) = fuzzy::match_window(&q, &w.title, &w.app_name, &w.exe) {
                picked.push((i, m.score, m.title_ranges));
            }
        }
        if q.is_empty() {
            match cfg().sort {
                Sort::Recent => {}
                Sort::App => picked.sort_by_key(|p| self.all[p.0].app_name.to_lowercase()),
                Sort::Title => picked.sort_by_key(|p| self.all[p.0].title.to_lowercase()),
            }
        } else {
            picked.sort_by(|a, b| b.1.cmp(&a.1)); // stable: MRU order within equal scores
        }
        let mut items: Vec<Item> = picked
            .into_iter()
            .map(|(win, _, ranges)| Item { win, count: 1, ranges, ..Default::default() })
            .collect();
        match self.grouping {
            Grouping::Off => {}
            Grouping::Cluster | Grouping::Collapse => {
                let mut groups: Vec<(String, Vec<Item>)> = Vec::new();
                for it in items {
                    let key = self.all[it.win].exe.clone();
                    match groups.iter_mut().find(|(k, _)| *k == key) {
                        Some((_, v)) => v.push(it),
                        None => groups.push((key, vec![it])),
                    }
                }
                items = if self.grouping == Grouping::Collapse {
                    groups
                        .into_iter()
                        .map(|(_, mut v)| {
                            let n = v.len();
                            let mut first = v.swap_remove(0);
                            first.count = n;
                            first
                        })
                        .collect()
                } else {
                    groups.into_iter().flat_map(|(_, v)| v).collect()
                };
            }
        }
        self.items = items;
    }

    fn m(&self) -> Metrics {
        let s = self.scale;
        let ts = cfg().text_scale();
        Metrics {
            pad: 16.0 * s,
            search_h: if self.search_visible { 40.0 * s * ts.max(1.0) } else { 0.0 },
            footer_h: if cfg().show_hints { 24.0 * s } else { 0.0 },
            gap: 8.0 * s,
            group_gap: 22.0 * s,
            tile_pad: 8.0 * s,
            title_h: 26.0 * s * ts,
            title_gap: 6.0 * s,
            row_h: 48.0 * s * ts,
            radius: cfg().corner_radius as f32 * s,
        }
    }

    fn tile_w(&self, it: &Item, h: f32) -> f32 {
        (h * self.all[it.win].aspect).clamp(h * 0.8, h * 1.9) + 2.0 * self.m().tile_pad
    }

    fn group_break(&self, i: usize) -> bool {
        self.grouping == Grouping::Cluster
            && i > 0
            && self.all[self.items[i].win].exe != self.all[self.items[i - 1].win].exe
    }

    /// Greedy row packing. Returns rows of item indices and the widest row.
    fn pack(&self, h: f32, max_w: f32) -> (Vec<Vec<usize>>, f32) {
        let m = self.m();
        let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
        let mut row_w = 0.0f32;
        let mut widest = 0.0f32;
        for (i, it) in self.items.iter().enumerate() {
            let tw = self.tile_w(it, h);
            let sep = if self.group_break(i) { m.group_gap } else { m.gap };
            let cur = rows.last().unwrap();
            if !cur.is_empty() && row_w + sep + tw > max_w {
                widest = widest.max(row_w);
                rows.push(vec![i]);
                row_w = tw;
            } else {
                row_w += if cur.is_empty() { tw } else { sep + tw };
                rows.last_mut().unwrap().push(i);
            }
        }
        (rows, widest.max(row_w))
    }

    fn tile_h(&self, h: f32) -> f32 {
        let m = self.m();
        m.tile_pad + m.title_h + m.title_gap + h + m.tile_pad
    }

    fn chrome_h(&self) -> f32 {
        let m = self.m();
        let search = if m.search_h > 0.0 { m.search_h + m.pad * 0.75 } else { 0.0 };
        let footer = if m.footer_h > 0.0 { m.footer_h + 2.0 * self.scale } else { 0.0 };
        m.pad + search + footer + m.pad
    }

    /// Size the panel for the full (unfiltered) list. The panel then keeps its
    /// size while typing so it never jumps around.
    fn layout_panel(&mut self) {
        let m = self.m();
        let s = self.scale;
        let work_w = (self.work.right - self.work.left) as f32;
        let work_h = (self.work.bottom - self.work.top) as f32;
        let max_h = work_h * 0.9;
        let chrome = self.chrome_h();

        if self.layout == Layout::List {
            let n = self.items.len().max(1);
            let max_rows = (((max_h - chrome) / m.row_h).floor() as usize).clamp(3, 14);
            self.list_rows = n.min(max_rows);
            let list_h = self.list_rows as f32 * m.row_h;
            let list_w = 440.0 * s * cfg().text_scale();
            let preview_w = if cfg().list_preview {
                (list_h * 1.6).clamp(260.0 * s, work_w * 0.5)
            } else {
                0.0
            };
            self.panel_w = (m.pad + list_w + if preview_w > 0.0 { m.pad + preview_w } else { 0.0 } + m.pad).min(work_w * 0.96);
            self.panel_h = chrome + list_h.max(110.0 * s);
            return;
        }

        let max_w = work_w * 0.94 - 2.0 * m.pad;
        let min_h = 64.0 * s;
        let mut h = cfg().preview_px as f32 * s;
        let (rows, widest) = loop {
            let (rows, widest) = self.pack(h, max_w);
            let n = rows.len().max(1) as f32;
            let total = chrome + n * self.tile_h(h) + (n - 1.0) * m.gap;
            let too_many_rows = self.layout == Layout::Row && rows.len() > 1;
            if (too_many_rows || total > max_h) && h > min_h {
                h = (h * 0.92).max(min_h);
                continue;
            }
            break (rows, widest);
        };
        self.thumb_h = h;
        let n = if self.items.is_empty() { 0 } else { rows.len() };
        let content_h = if n == 0 {
            110.0 * s
        } else {
            n as f32 * self.tile_h(h) + (n as f32 - 1.0) * m.gap
        };
        self.panel_w = (widest + 2.0 * m.pad).max((500.0 * s).min(work_w * 0.9));
        self.panel_h = chrome + content_h;
    }

    /// Position everything inside the (fixed) panel.
    fn place_items(&mut self) {
        let m = self.m();
        let s = self.scale;
        self.search_r = R::new(m.pad, m.pad, self.panel_w - 2.0 * m.pad, m.search_h);
        let top = m.pad + if m.search_h > 0.0 { m.search_h + m.pad * 0.75 } else { 0.0 };
        let bottom = self.panel_h - m.pad - if m.footer_h > 0.0 { m.footer_h + 2.0 * s } else { 0.0 };
        self.content_r = R::new(m.pad, top, self.panel_w - 2.0 * m.pad, bottom - top);
        self.footer_r = R::new(m.pad, bottom + 2.0 * s, self.panel_w - 2.0 * m.pad, m.footer_h);
        for it in &mut self.items {
            it.visible = false;
        }
        if self.layout == Layout::List {
            self.place_list();
        } else {
            self.place_grid();
        }
    }

    fn place_list(&mut self) {
        let m = self.m();
        let s = self.scale;
        let c = self.content_r;
        let list_w = if cfg().list_preview { (440.0 * s * cfg().text_scale()).min(c.w * 0.6) } else { c.w };
        self.preview_r = if cfg().list_preview {
            R::new(c.x + list_w + m.pad, c.y, c.w - list_w - m.pad, c.h)
        } else {
            R::default()
        };
        let rows = self.list_rows.max(1);
        // Keep the selection in view.
        if self.sel < self.list_first {
            self.list_first = self.sel;
        } else if self.sel >= self.list_first + rows {
            self.list_first = self.sel + 1 - rows;
        }
        self.list_first = self.list_first.min(self.items.len().saturating_sub(rows));
        let icon_s = 24.0 * s;
        let b = 24.0 * s;
        for (i, it) in self.items.iter_mut().enumerate() {
            it.row = i;
            if i < self.list_first || i >= self.list_first + rows {
                continue;
            }
            let y = c.y + (i - self.list_first) as f32 * m.row_h;
            it.visible = true;
            it.tile = R::new(c.x, y, list_w, m.row_h);
            it.icon = R::new(c.x + 12.0 * s, y + (m.row_h - icon_s) / 2.0, icon_s, icon_s);
            it.title = R::new(it.icon.right() + 12.0 * s, y, list_w - icon_s - 36.0 * s, m.row_h);
            it.thumb_area = R::default();
            for (bi, r) in it.btns.iter_mut().enumerate() {
                let from_right = (3 - bi) as f32;
                *r = R::new(c.x + list_w - 8.0 * s - from_right * (b + 2.0 * s), y + (m.row_h - b) / 2.0, b, b);
            }
        }
    }

    fn place_grid(&mut self) {
        let m = self.m();
        let s = self.scale;
        let max_w = self.content_r.w;
        let avail_h = self.content_r.h;
        let mut h = self.thumb_h;
        let mut rows = self.pack(h, max_w).0;
        // Grouping gaps can push a filtered list over; shrink a touch if needed.
        while h > 48.0 * s {
            let n = rows.len() as f32;
            if n * self.tile_h(h) + (n - 1.0) * m.gap <= avail_h + 0.5 {
                break;
            }
            h *= 0.94;
            rows = self.pack(h, max_w).0;
        }
        let tile_h = self.tile_h(h);
        let mut y = self.content_r.y;
        for (ri, row) in rows.iter().enumerate() {
            let widths: Vec<f32> = row.iter().map(|&i| self.tile_w(&self.items[i], h)).collect();
            let mut row_w = 0.0;
            for (k, &i) in row.iter().enumerate() {
                if k > 0 {
                    row_w += if self.group_break(i) { m.group_gap } else { m.gap };
                }
                row_w += widths[k];
            }
            let mut x = self.content_r.x + (max_w - row_w) / 2.0;
            for (k, &i) in row.iter().enumerate() {
                if k > 0 {
                    x += if self.group_break(i) { m.group_gap } else { m.gap };
                }
                let tw = widths[k];
                let icon_s = 18.0 * s;
                let it = &mut self.items[i];
                it.visible = true;
                it.row = ri;
                it.tile = R::new(x, y, tw, tile_h);
                it.title = R::new(x + m.tile_pad, y + m.tile_pad, tw - 2.0 * m.tile_pad, m.title_h);
                it.icon = R::new(it.title.x + 2.0 * s, it.title.y + (m.title_h - icon_s) / 2.0, icon_s, icon_s);
                it.thumb_area = R::new(x + m.tile_pad, it.title.bottom() + m.title_gap, tw - 2.0 * m.tile_pad, h);
                let b = 22.0 * s;
                for (bi, r) in it.btns.iter_mut().enumerate() {
                    let from_right = (3 - bi) as f32;
                    *r = R::new(it.title.right() - from_right * (b + 2.0 * s) + 2.0 * s, it.title.y + (m.title_h - b) / 2.0, b, b);
                }
                x += tw;
            }
            y += tile_h + m.gap;
        }
    }

    fn refilter(&mut self) {
        let was_visible = self.search_visible;
        if cfg().search_bar == SearchBar::Typing {
            self.search_visible = !self.query.is_empty();
        }
        self.rebuild();
        self.sel = 0;
        self.list_first = 0;
        self.hover = None;
        self.hover_btn = None;
        if was_visible != self.search_visible {
            self.relayout_full();
        } else {
            self.place_items();
        }
        if !self.visible {
            self.show();
        }
        self.update_thumbs();
        self.kick();
    }

    /// Re-layout after windows were removed or grouping/layout changed (may resize panel).
    fn relayout_full(&mut self) {
        let keep = self.items.get(self.sel).map(|it| self.all[it.win].hwnd);
        // Size the panel for the whole list, then apply the current search.
        let q = std::mem::take(&mut self.query);
        self.rebuild();
        self.layout_panel();
        self.query = q;
        self.rebuild();
        self.sel = keep
            .and_then(|h| self.items.iter().position(|it| self.all[it.win].hwnd == h))
            .unwrap_or(0)
            .min(self.items.len().saturating_sub(1));
        self.place_items();
        if self.visible {
            unsafe {
                let (x, y) = self.panel_origin();
                let (w, h) = (self.panel_w.round() as i32, self.panel_h.round() as i32);
                let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
                self.resize_target(w as u32, h as u32);
            }
            if let Some(it) = self.items.get(self.sel) {
                self.hl = it.tile;
            }
            self.update_thumbs();
            self.render();
        }
    }

    // ---- DWM thumbnails --------------------------------------------------------

    fn zoom_rect(&self, r: R) -> R {
        if (self.zoom - 1.0).abs() < 0.0005 {
            return r;
        }
        let (cx, cy) = (self.panel_w / 2.0, self.panel_h / 2.0);
        R::new(cx + (r.x - cx) * self.zoom, cy + (r.y - cy) * self.zoom, r.w * self.zoom, r.h * self.zoom)
    }

    fn update_thumbs(&mut self) {
        if !self.visible {
            return;
        }
        let shown: HashMap<isize, R> = if self.layout == Layout::List {
            match (cfg().list_preview, self.items.get(self.sel)) {
                (true, Some(it)) => [(self.all[it.win].hwnd, self.preview_r.inset(10.0 * self.scale))].into(),
                _ => HashMap::new(),
            }
        } else {
            self.items.iter().filter(|it| it.visible).map(|it| (self.all[it.win].hwnd, it.thumb_area)).collect()
        };
        let opacity = (self.fade.clamp(0.0, 1.0) * 255.0) as u8;
        unsafe {
            for w in &self.all {
                let area = shown.get(&w.hwnd);
                let thumb = match (self.thumbs.get(&w.hwnd), area) {
                    (Some(&t), _) => t,
                    (None, Some(_)) => match DwmRegisterThumbnail(self.hwnd, util::hwnd(w.hwnd)) {
                        Ok(t) => {
                            self.thumbs.insert(w.hwnd, t);
                            t
                        }
                        Err(_) => continue,
                    },
                    (None, None) => continue,
                };
                let mut props = DWM_THUMBNAIL_PROPERTIES {
                    dwFlags: DWM_TNP_VISIBLE | DWM_TNP_RECTDESTINATION | DWM_TNP_OPACITY | DWM_TNP_SOURCECLIENTAREAONLY,
                    fSourceClientAreaOnly: false.into(),
                    opacity,
                    ..Default::default()
                };
                match area {
                    Some(a) => {
                        let fit = match DwmQueryThumbnailSourceSize(thumb) {
                            Ok(sz) if sz.cx > 0 && sz.cy > 0 => fit_rect(*a, sz.cx as f32 / sz.cy as f32),
                            _ => *a,
                        };
                        let fit = self.zoom_rect(fit);
                        props.fVisible = true.into();
                        props.rcDestination = RECT {
                            left: fit.x.round() as i32,
                            top: fit.y.round() as i32,
                            right: fit.right().round() as i32,
                            bottom: fit.bottom().round() as i32,
                        };
                    }
                    None => props.fVisible = false.into(),
                }
                let _ = DwmUpdateThumbnailProperties(thumb, &props);
            }
        }
    }

    fn drop_window(&mut self, h: isize) {
        if let Some(t) = self.thumbs.remove(&h) {
            unsafe {
                let _ = DwmUnregisterThumbnail(t);
            }
        }
        if let Some(pos) = self.all.iter().position(|w| w.hwnd == h) {
            self.all.remove(pos);
        }
    }

    // ---- animation -------------------------------------------------------------

    fn kick(&mut self) {
        if !self.visible {
            return;
        }
        if self.layout == Layout::List {
            self.place_items();
            self.update_thumbs();
        }
        if cfg().anim_factor() <= 0.0 {
            if let Some(it) = self.items.get(self.sel) {
                self.hl = it.tile;
            }
            self.fade = 1.0;
            self.zoom = 1.0;
            self.apply_dim();
            self.render();
            return;
        }
        if !self.animating {
            self.animating = true;
            self.t_last = Instant::now();
            unsafe {
                let _ = PostMessageW(self.hwnd, WM_SW_ANIM, WPARAM(0), LPARAM(0));
            }
        }
    }

    /// One animation frame. Frames are paced by `DwmFlush` (the compositor's
    /// vsync), so motion is smooth at any refresh rate and stops completely
    /// once everything has settled.
    fn tick(&mut self) {
        if !self.animating || !self.visible {
            self.animating = false;
            return;
        }
        let f = cfg().anim_factor().max(0.01);
        let now = Instant::now();
        let dt = (now - self.t_last).as_secs_f32().min(0.05);
        self.t_last = now;
        let t = ((now - self.t_open).as_secs_f32() * 1000.0 / (OPEN_MS * f)).min(1.0);
        let ease = 1.0 - (1.0 - t).powi(3);
        let fade_changed = (ease - self.fade).abs() > 0.001;
        self.fade = ease;
        self.zoom = 0.965 + 0.035 * ease;
        let mut settled = t >= 1.0;
        if let Some(it) = self.items.get(self.sel) {
            let k = 1.0 - (-dt * 24.0 / f).exp();
            self.hl = self.hl.lerp(&it.tile, k);
            if self.hl.dist(&it.tile) < 0.6 {
                self.hl = it.tile;
            } else {
                settled = false;
            }
        }
        self.apply_dim();
        if fade_changed {
            self.update_thumbs();
        }
        self.render();
        if settled {
            self.animating = false;
        } else {
            unsafe {
                if DwmFlush().is_err() {
                    std::thread::sleep(std::time::Duration::from_millis(8));
                }
                let _ = PostMessageW(self.hwnd, WM_SW_ANIM, WPARAM(0), LPARAM(0));
            }
        }
    }

    fn apply_dim(&self) {
        if cfg().dim_background && !self.dim.is_invalid() {
            let a = (cfg().dim_amount as f32 / 100.0 * 255.0 * self.fade) as u8;
            unsafe {
                let _ = SetLayeredWindowAttributes(self.dim, COLORREF(0), a, LWA_ALPHA);
            }
        }
    }

    // ---- navigation & actions --------------------------------------------------

    fn step(&mut self, d: i32) {
        let n = self.items.len() as i32;
        if n == 0 {
            return;
        }
        let next = self.sel as i32 + d;
        self.sel = if cfg().wrap { next.rem_euclid(n) } else { next.clamp(0, n - 1) } as usize;
        self.kick();
    }

    fn vertical(&mut self, d: i32) {
        if self.layout == Layout::List {
            return self.step(d);
        }
        let Some(cur) = self.items.get(self.sel) else { return };
        let target_row = cur.row as i32 + d;
        let cx = cur.tile.x + cur.tile.w / 2.0;
        let best = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, it)| it.row as i32 == target_row)
            .min_by(|a, b| {
                let da = (a.1.tile.x + a.1.tile.w / 2.0 - cx).abs();
                let db = (b.1.tile.x + b.1.tile.w / 2.0 - cx).abs();
                da.partial_cmp(&db).unwrap()
            })
            .map(|(i, _)| i);
        if let Some(i) = best {
            self.sel = i;
            self.kick();
        }
    }

    fn act(&mut self, idx: usize, a: Act) {
        let Some(it) = self.items.get(idx) else { return };
        let wi = it.win;
        let h = self.all[wi].hwnd;
        match a {
            Act::Close => {
                // In Collapse mode the tile stands for the whole app.
                if it.count > 1 {
                    return self.act(idx, Act::CloseApp);
                }
                winlist::close(h);
                self.drop_window(h);
                self.relayout_full();
            }
            Act::CloseApp => {
                let exe = self.all[wi].exe.clone();
                let gone: Vec<isize> = self.all.iter().filter(|w| w.exe == exe).map(|w| w.hwnd).collect();
                for g in gone {
                    winlist::close(g);
                    self.drop_window(g);
                }
                self.relayout_full();
            }
            Act::Minimize => {
                winlist::minimize(h);
                if cfg().show_minimized {
                    self.all[wi].minimized = true;
                    self.render();
                } else {
                    self.drop_window(h);
                    self.relayout_full();
                }
            }
            Act::Maximize => {
                self.all[wi].maximized = winlist::toggle_maximize(h);
                self.all[wi].minimized = false;
                self.render();
            }
            Act::Pin => {
                self.all[wi].topmost = winlist::toggle_topmost(h);
                // Keep our overlay above the newly pinned window.
                unsafe {
                    let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                }
                self.render();
            }
            Act::MoveNext | Act::MovePrev => {
                winlist::move_to_monitor(h, if a == Act::MoveNext { 1 } else { -1 });
                unsafe {
                    let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                }
                self.update_thumbs();
                self.render();
            }
            Act::OpenLocation => {
                let w = self.all[wi].clone();
                self.close();
                winlist::open_location(&w);
            }
            Act::NewWindow => {
                let w = self.all[wi].clone();
                self.close();
                winlist::new_window(&w);
            }
            Act::HideApp => {
                let exe = self.all[wi].exe.clone();
                if exe.is_empty() {
                    return;
                }
                if !cfg().hidden_apps.contains(&exe) {
                    cfg().hidden_apps.push(exe.clone());
                    config::save();
                    crate::settings_ui::refresh();
                }
                let gone: Vec<isize> = self.all.iter().filter(|w| w.exe == exe).map(|w| w.hwnd).collect();
                for g in gone {
                    self.drop_window(g);
                }
                self.relayout_full();
            }
        }
    }

    fn btn_act(b: Btn) -> Act {
        match b {
            Btn::Pin => Act::Pin,
            Btn::Min => Act::Minimize,
            Btn::Close => Act::Close,
        }
    }

    fn context_menu(&mut self, idx: usize) {
        let Some(it) = self.items.get(idx) else { return };
        let w = self.all[it.win].clone();
        let count = it.count;
        self.sel = idx;
        self.kick();
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let add = |id: usize, text: &str, flags: MENU_ITEM_FLAGS| {
                let t = wide(text);
                let _ = AppendMenuW(menu, flags, id, windows::core::PCWSTR(t.as_ptr()));
            };
            let sep = || {
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, windows::core::PCWSTR::null());
            };
            let has_path = !w.exe_path.is_empty();
            let multi_mon = winlist::monitors().len() > 1;
            add(1, "Switch to\tEnter", MF_STRING);
            sep();
            add(2, "Minimize\tCtrl+M", MF_STRING);
            add(3, if w.maximized { "Restore\tCtrl+Up" } else { "Maximize\tCtrl+Up" }, MF_STRING);
            add(4, "Pin on top\tCtrl+P", if w.topmost { MF_CHECKED } else { MF_STRING });
            if multi_mon {
                add(5, "Move to next monitor\tCtrl+Right", MF_STRING);
                add(6, "Move to previous monitor\tCtrl+Left", MF_STRING);
            }
            sep();
            add(7, "New window\tCtrl+N", if has_path { MF_STRING } else { MF_GRAYED });
            add(8, "Open file location\tCtrl+O", if has_path { MF_STRING } else { MF_GRAYED });
            add(9, &format!("Hide {} from Switch\tCtrl+H", w.app_name), MF_STRING);
            sep();
            if count <= 1 {
                add(10, "Close window\tDel", MF_STRING);
            }
            add(11, &format!("Close all {} windows\tCtrl+Shift+W", w.app_name), MF_STRING);

            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            hook::PASSTHRU.store(true, std::sync::atomic::Ordering::Relaxed);
            // The menu only gets keyboard input and dismisses on outside clicks
            // if its owner is in the foreground.
            let prev = GetForegroundWindow();
            if prev != self.hwnd {
                if self.restore_fg.is_none() {
                    self.restore_fg = Some(prev.0 as isize);
                }
                hook::inject_alt_tap();
                let _ = SetForegroundWindow(self.hwnd);
            }
            let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON, pt.x, pt.y, 0, self.hwnd, None);
            hook::PASSTHRU.store(false, std::sync::atomic::Ordering::Relaxed);
            let _ = DestroyMenu(menu);
            if !self.session {
                return;
            }
            match cmd.0 {
                1 => self.commit(),
                2 => self.act(idx, Act::Minimize),
                3 => self.act(idx, Act::Maximize),
                4 => self.act(idx, Act::Pin),
                5 => self.act(idx, Act::MoveNext),
                6 => self.act(idx, Act::MovePrev),
                7 => self.act(idx, Act::NewWindow),
                8 => self.act(idx, Act::OpenLocation),
                9 => self.act(idx, Act::HideApp),
                10 => self.act(idx, Act::Close),
                11 => self.act(idx, Act::CloseApp),
                _ => {}
            }
        }
    }

    fn key(&mut self, vk: u32, flags: isize) {
        if !self.session {
            return;
        }
        let ctrl = flags & KEY_CTRL != 0;
        let shift = flags & KEY_SHIFT != 0;
        let vkk = VIRTUAL_KEY(vk as u16);
        if ctrl {
            let a = match vkk {
                VK_UP => Some(Act::Maximize),
                VK_DOWN => Some(Act::Minimize),
                VK_LEFT => Some(Act::MovePrev),
                VK_RIGHT => Some(Act::MoveNext),
                _ => match vk as u8 {
                    b'W' if shift => Some(Act::CloseApp),
                    b'W' => Some(Act::Close),
                    b'M' => Some(Act::Minimize),
                    b'P' => Some(Act::Pin),
                    b'H' => Some(Act::HideApp),
                    b'N' => Some(Act::NewWindow),
                    b'O' => Some(Act::OpenLocation),
                    _ => None,
                },
            };
            if let Some(a) = a {
                return self.act(self.sel, a);
            }
            match vk as u8 {
                b'G' if self.same_app.is_none() => {
                    self.grouping = Grouping::from_index((self.grouping.index() + 1) % Grouping::ALL.len());
                    return self.relayout_full();
                }
                b'L' => {
                    self.layout = Layout::from_index((self.layout.index() + 1) % Layout::ALL.len());
                    self.list_first = 0;
                    for (_, t) in self.thumbs.drain() {
                        unsafe {
                            let _ = DwmUnregisterThumbnail(t);
                        }
                    }
                    return self.relayout_full();
                }
                _ => {}
            }
        }
        match vkk {
            VK_ESCAPE => {
                if self.query.is_empty() {
                    self.close();
                } else {
                    self.query.clear();
                    self.refilter();
                }
            }
            VK_RETURN => self.commit(),
            VK_TAB => self.step(if shift { -1 } else { 1 }),
            VK_LEFT => self.step(-1),
            VK_RIGHT => self.step(1),
            VK_UP => self.vertical(-1),
            VK_DOWN => self.vertical(1),
            VK_PRIOR => self.step(-(self.list_rows.max(1) as i32)),
            VK_NEXT => self.step(self.list_rows.max(1) as i32),
            VK_HOME => {
                self.sel = 0;
                self.kick();
            }
            VK_END => {
                self.sel = self.items.len().saturating_sub(1);
                self.kick();
            }
            VK_DELETE => self.act(self.sel, Act::Close),
            VK_APPS => self.context_menu(self.sel),
            VK_BACK => {
                if ctrl {
                    if !self.query.is_empty() {
                        self.query.clear();
                        self.refilter();
                    }
                } else if self.query.pop().is_some() {
                    self.refilter();
                }
            }
            _ if ctrl => {}
            _ => {
                let mut state = [0u8; 256];
                if shift {
                    state[VK_SHIFT.0 as usize] = 0x80;
                }
                let mut buf = [0u16; 8];
                let scan = (flags & 0xFFFF) as u32;
                let n = unsafe { ToUnicode(vk, scan, Some(&state), &mut buf, 4) };
                if n > 0 {
                    let s = String::from_utf16_lossy(&buf[..n as usize]);
                    let s: String = s.chars().filter(|c| !c.is_control()).collect();
                    if s.is_empty() || (self.query.is_empty() && s.trim().is_empty()) {
                        return;
                    }
                    // Typing turns the session sticky: let go of Alt and keep typing.
                    hook::STICKY.store(true, std::sync::atomic::Ordering::Relaxed);
                    self.query.push_str(&s);
                    self.refilter();
                }
            }
        }
    }

    fn show_btns(&self, i: usize) -> bool {
        cfg().action_buttons && (Some(i) == self.hover || i == self.sel)
    }

    fn hit(&self, x: f32, y: f32) -> (Option<usize>, Option<(usize, Btn)>) {
        for (i, it) in self.items.iter().enumerate() {
            if it.visible && it.tile.contains(x, y) {
                if self.show_btns(i) {
                    for (bi, r) in it.btns.iter().enumerate() {
                        if r.contains(x, y) {
                            return (Some(i), Some((i, BTNS[bi])));
                        }
                    }
                }
                return (Some(i), None);
            }
        }
        (None, None)
    }

    // ---- rendering ---------------------------------------------------------------

    fn ensure_target(&mut self) {
        if self.rt.is_some() {
            return;
        }
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        if let Ok(rt) = gfx().create_target(self.hwnd, rc.right as u32, rc.bottom as u32) {
            let base = gfx::as_rt(&rt);
            self.brush = gfx::make_brush(&base).ok();
            self.brush2 = gfx::make_brush(&base).ok();
            self.rtb = Some(base);
            self.rt = Some(rt);
            self.icons.clear();
        }
    }

    fn resize_target(&mut self, w: u32, h: u32) {
        self.ensure_target();
        if let Some(rt) = &self.rt {
            unsafe {
                let _ = rt.Resize(&D2D_SIZE_U { width: w, height: h });
            }
        }
    }

    fn ensure_fmts(&mut self) {
        let key = (self.scale, cfg().text_scale());
        if self.fmts.is_some() && self.fmt_key == key {
            return;
        }
        let s = self.scale * key.1;
        let g = gfx();
        let mk = |size: f32, weight, align| g.text_format(size * s, weight, align).ok();
        if let (Some(title), Some(sub), Some(search), Some(meta), Some(empty), Some(letter), Some(hint), Some(badge)) = (
            mk(12.5, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING),
            mk(11.5, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING),
            mk(15.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING),
            mk(12.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_TRAILING),
            mk(14.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER),
            mk(10.5, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER),
            mk(11.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER),
            mk(10.5, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER),
        ) {
            self.fmts = Some(Fmts { title, sub, search, meta, empty, letter, hint, badge });
            self.fmt_key = key;
        }
    }

    fn icon_for(&mut self, idx: usize) -> Option<ID2D1Bitmap> {
        let w = &self.all[idx];
        if let Some(b) = self.icons.get(&w.hwnd) {
            return b.clone();
        }
        let rt = self.rtb.as_ref()?;
        let bmp = winlist::window_icon(w).and_then(|(icon, owned)| {
            let b = gfx().icon_bitmap(rt, icon);
            unsafe { winlist::free_icon(icon, owned) };
            b
        });
        self.icons.insert(w.hwnd, bmp.clone());
        bmp
    }

    fn render(&mut self) {
        self.ensure_target();
        self.ensure_fmts();
        let show_icons = cfg().show_icons;
        let icons: Vec<Option<ID2D1Bitmap>> = (0..self.items.len())
            .map(|i| if show_icons && self.items[i].visible { self.icon_for(self.items[i].win) } else { None })
            .collect();
        let (Some(rt), Some(rtb), Some(brush), Some(brush2), Some(f)) =
            (&self.rt, &self.rtb, &self.brush, &self.brush2, &self.fmts)
        else {
            return;
        };
        let pal = self.pal;
        let s = self.scale;
        unsafe {
            rt.BeginDraw();
            rt.Clear(Some(&pal.bg.d2d()));
            let z = self.zoom;
            let (cx, cy) = (self.panel_w / 2.0, self.panel_h / 2.0);
            rt.SetTransform(&Matrix3x2 { M11: z, M12: 0.0, M21: 0.0, M22: z, M31: cx * (1.0 - z), M32: cy * (1.0 - z) });
        }
        if self.session {
            let p = Painter { rt: rtb, brush, brush2, alpha: self.fade };
            if self.search_visible {
                self.draw_search(&p, f, &pal);
            }
            if self.items.is_empty() {
                let msg = if self.query.is_empty() {
                    "No windows to switch to".to_string()
                } else {
                    format!("No windows match \u{201C}{}\u{201D}", self.query)
                };
                p.text(&msg, &f.empty, self.content_r, pal.subtext);
            } else {
                self.draw_highlight(&p, &pal);
                if self.layout == Layout::List {
                    for (i, it) in self.items.iter().enumerate().filter(|(_, it)| it.visible) {
                        self.draw_row(&p, f, &pal, i, it, icons[i].as_ref());
                    }
                    self.draw_preview(&p, f, &pal, &icons);
                } else {
                    for (i, it) in self.items.iter().enumerate().filter(|(_, it)| it.visible) {
                        self.draw_tile(&p, f, &pal, i, it, icons[i].as_ref());
                    }
                }
            }
            if cfg().show_hints {
                self.draw_hints(&p, f, &pal);
            }
            let _ = s;
        }
        unsafe {
            rt.SetTransform(&Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: 0.0, M32: 0.0 });
            if let Err(e) = rt.EndDraw(None, None) {
                if e.code() == D2DERR_RECREATE_TARGET {
                    self.rt = None;
                    self.rtb = None;
                    self.brush = None;
                    self.brush2 = None;
                    self.icons.clear();
                }
            }
            let _ = ValidateRect(self.hwnd, None);
        }
    }

    fn draw_highlight(&self, p: &Painter, pal: &Palette) {
        let s = self.scale;
        let r = self.m().radius;
        let hl = self.hl;
        if hl.w <= 0.0 {
            return;
        }
        let (fill, stroke) = match cfg().highlight {
            Highlight::Outline => (false, true),
            Highlight::Fill => (true, false),
            Highlight::Both => (true, true),
        };
        if fill {
            let a = match (cfg().highlight, pal.dark) {
                (Highlight::Fill, true) => 0.32,
                (Highlight::Fill, false) => 0.22,
                (_, true) => 0.20,
                (_, false) => 0.14,
            };
            p.fill(hl, r, pal.accent.a(a));
        }
        if stroke {
            p.stroke(hl, r, pal.accent, 2.0 * s);
        }
    }

    fn draw_search(&self, p: &Painter, f: &Fmts, pal: &Palette) {
        let s = self.scale;
        let r = self.search_r;
        let rad = (self.m().radius * 0.85).min(r.h / 2.0);
        p.fill(r, rad, pal.surface);
        p.stroke(r, rad, if self.query.is_empty() { pal.border } else { pal.accent.a(0.6) }, 1.0 * s);
        p.glyph_search(r.x + 20.0 * s, r.y + r.h / 2.0, 8.0 * s, pal.subtext, 1.6 * s);
        let count = self.items.len();
        let noun = if self.grouping == Grouping::Collapse { "app" } else { "window" };
        let mut meta = format!("{} {}{}", count, noun, if count == 1 { "" } else { "s" });
        if self.grouping == Grouping::Cluster {
            meta.push_str(" \u{00B7} grouped");
        }
        let meta_w = gfx().measure(&meta, &f.meta) + 4.0;
        let text_r = R::new(r.x + 38.0 * s, r.y, r.w - 38.0 * s - meta_w - 16.0 * s, r.h);
        if self.query.is_empty() {
            let placeholder = match &self.same_app {
                Some(_) => {
                    let name = self.all.first().map(|w| w.app_name.as_str()).unwrap_or("this app");
                    format!("{name} windows \u{00B7} type to search")
                }
                None => "Type to search windows".into(),
            };
            p.text(&placeholder, &f.search, text_r, pal.faint);
        } else {
            p.text(&self.query, &f.search, text_r, pal.text);
            let w = gfx().measure(&self.query, &f.search).min(text_r.w);
            let cx = text_r.x + w + 2.0 * s;
            p.line(cx, r.y + 11.0 * s, cx, r.bottom() - 11.0 * s, pal.accent, 1.5 * s);
        }
        p.text(&meta, &f.meta, R::new(r.right() - meta_w - 14.0 * s, r.y, meta_w, r.h), pal.faint);
    }

    fn draw_hints(&self, p: &Painter, f: &Fmts, pal: &Palette) {
        let hints = if self.layout == Layout::List {
            "\u{2191}\u{2193} move   \u{21B5} switch   Esc close   Del close window   Ctrl+L layout   Right-click for more"
        } else {
            "\u{21B5} switch   Esc close   Del close window   Ctrl+H hide app   Ctrl+G group   Ctrl+L layout   Right-click for more"
        };
        p.text(hints, &f.hint, self.footer_r, pal.faint);
    }

    fn label(&self, it: &Item) -> (String, Vec<(u32, u32)>) {
        let w = &self.all[it.win];
        if it.count > 1 {
            return (w.app_name.clone(), Vec::new());
        }
        match cfg().label {
            Label::Title => (w.title.clone(), it.ranges.clone()),
            Label::App => (w.app_name.clone(), Vec::new()),
            Label::Both => {
                if w.title.contains(&w.app_name) {
                    return (w.title.clone(), it.ranges.clone());
                }
                let prefix = format!("{} \u{2014} ", w.app_name);
                let off = utf16(&prefix).len() as u32;
                (prefix + &w.title, it.ranges.iter().map(|(a, b)| (a + off, *b)).collect())
            }
        }
    }

    fn draw_icon(&self, p: &Painter, f: &Fmts, pal: &Palette, r: R, w: &WinInfo, icon: Option<&ID2D1Bitmap>) {
        match icon {
            Some(b) => p.bitmap(b, r, 1.0),
            None => {
                p.circle(r.x + r.w / 2.0, r.y + r.h / 2.0, r.w / 2.0, pal.accent);
                let letter: String = w.app_name.chars().next().unwrap_or('?').to_uppercase().collect();
                p.text(&letter, &f.letter, r, pal.on_accent);
            }
        }
    }

    fn draw_buttons(&self, p: &Painter, pal: &Palette, i: usize, it: &Item) {
        let s = self.scale;
        let w = &self.all[it.win];
        for (bi, r) in it.btns.iter().enumerate() {
            let b = BTNS[bi];
            let hot = self.hover_btn == Some((i, b));
            if hot {
                let bg = if b == Btn::Close { C::hex(0xE81123) } else { pal.surface_hover };
                p.fill(*r, 6.0 * s, bg);
            }
            let fg = if hot && b == Btn::Close { C::hex(0xFFFFFF) } else { pal.subtext };
            let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
            match b {
                Btn::Close => p.glyph_close(cx, cy, 4.0 * s, fg, 1.3 * s),
                Btn::Min => p.glyph_minimize(cx, cy, 4.5 * s, fg, 1.3 * s),
                Btn::Pin => {
                    let c = if w.topmost { pal.accent } else { fg };
                    p.glyph_pin(cx, cy - 0.5 * s, 4.2 * s, c, 1.2 * s, w.topmost)
                }
            }
        }
    }

    /// Small status marks (count badge, pinned, other desktop) right-aligned
    /// ending at `right`. Returns the x where they start.
    fn draw_marks(&self, p: &Painter, f: &Fmts, pal: &Palette, it: &Item, right: f32, cy: f32) -> f32 {
        let s = self.scale;
        let w = &self.all[it.win];
        let mut x = right;
        if it.count > 1 {
            let t = format!("{}", it.count);
            let bw = (gfx().measure(&t, &f.badge) + 12.0 * s).max(20.0 * s);
            let r = R::new(x - bw, cy - 9.0 * s, bw, 18.0 * s);
            p.fill(r, 9.0 * s, pal.accent.a(0.9));
            p.text(&t, &f.badge, r, pal.on_accent);
            x = r.x - 6.0 * s;
        }
        if w.topmost && !self.show_btns_for(it) {
            p.glyph_pin(x - 8.0 * s, cy - 0.5 * s, 4.2 * s, pal.accent, 1.2 * s, true);
            x -= 20.0 * s;
        }
        if w.other_desktop {
            p.glyph_desktop(x - 9.0 * s, cy, 6.0 * s, pal.faint, 1.2 * s);
            x -= 22.0 * s;
        }
        x
    }

    fn show_btns_for(&self, it: &Item) -> bool {
        self.items.iter().position(|o| std::ptr::eq(o, it)).map(|i| self.show_btns(i)).unwrap_or(false)
    }

    fn draw_tile(&self, p: &Painter, f: &Fmts, pal: &Palette, i: usize, it: &Item, icon: Option<&ID2D1Bitmap>) {
        let s = self.scale;
        let rad = self.m().radius;
        let w = &self.all[it.win];
        let selected = i == self.sel;
        let hovered = Some(i) == self.hover;
        if hovered && !selected {
            p.fill(it.tile, rad, pal.surface_hover.a(0.8));
        }
        // Preview well (visible around letterboxed / minimized thumbnails).
        p.fill(it.thumb_area, (rad * 0.5).max(2.0 * s), pal.thumb_bg);
        if let Some(b) = icon {
            let big = (it.thumb_area.h * 0.32).min(48.0 * s);
            let r = R::new(
                it.thumb_area.x + (it.thumb_area.w - big) / 2.0,
                it.thumb_area.y + (it.thumb_area.h - big) / 2.0,
                big,
                big,
            );
            p.bitmap(b, r, 0.5);
        }
        let mut text_right = it.title.right();
        if self.show_btns(i) {
            self.draw_buttons(p, pal, i, it);
            text_right = it.btns[0].x - 4.0 * s;
        }
        text_right = self.draw_marks(p, f, pal, it, text_right, it.title.y + it.title.h / 2.0);
        let tx = if cfg().show_icons {
            self.draw_icon(p, f, pal, it.icon, w, icon);
            it.icon.right() + 8.0 * s
        } else {
            it.title.x + 4.0 * s
        };
        let color = if selected { pal.text } else if w.minimized { pal.faint } else { pal.subtext };
        let (label, ranges) = self.label(it);
        p.text_hl(&label, &f.title, R::new(tx, it.title.y, (text_right - tx).max(0.0), it.title.h), color, &ranges, pal.accent);
    }

    fn draw_row(&self, p: &Painter, f: &Fmts, pal: &Palette, i: usize, it: &Item, icon: Option<&ID2D1Bitmap>) {
        let s = self.scale;
        let rad = self.m().radius;
        let w = &self.all[it.win];
        let selected = i == self.sel;
        if Some(i) == self.hover && !selected {
            p.fill(it.tile, rad, pal.surface_hover.a(0.8));
        }
        let mut right = it.tile.right() - 12.0 * s;
        if self.show_btns(i) {
            self.draw_buttons(p, pal, i, it);
            right = it.btns[0].x - 6.0 * s;
        }
        right = self.draw_marks(p, f, pal, it, right, it.tile.y + it.tile.h / 2.0);
        let tx = if cfg().show_icons {
            self.draw_icon(p, f, pal, it.icon, w, icon);
            it.title.x
        } else {
            it.tile.x + 14.0 * s
        };
        let tw = (right - tx).max(0.0);
        let mid = it.tile.y + it.tile.h / 2.0;
        let (label, ranges) = self.label(it);
        let color = if selected { pal.text } else if w.minimized { pal.subtext } else { pal.text.mix(pal.subtext, 0.35) };
        p.text_hl(&label, &f.title, R::new(tx, mid - 17.0 * s * cfg().text_scale(), tw, 20.0 * s * cfg().text_scale()), color, &ranges, pal.accent);
        let mut sub = if it.count > 1 { format!("{} windows", it.count) } else { w.app_name.clone() };
        if w.minimized {
            sub.push_str(" \u{00B7} minimized");
        }
        if w.other_desktop {
            sub.push_str(" \u{00B7} other desktop");
        }
        p.text(&sub, &f.sub, R::new(tx, mid + 1.0 * s, tw, 17.0 * s * cfg().text_scale()), pal.faint);
    }

    fn draw_preview(&self, p: &Painter, f: &Fmts, pal: &Palette, icons: &[Option<ID2D1Bitmap>]) {
        if !cfg().list_preview || self.preview_r.w <= 0.0 {
            return;
        }
        let s = self.scale;
        let r = self.preview_r;
        p.fill(r, self.m().radius, pal.thumb_bg);
        if let Some(Some(b)) = icons.get(self.sel) {
            let big = 56.0 * s;
            p.bitmap(b, R::new(r.x + (r.w - big) / 2.0, r.y + (r.h - big) / 2.0, big, big), 0.45);
        } else if self.items.get(self.sel).is_some() {
            p.text("No preview", &f.empty, r, pal.faint);
        }
    }
}

struct Metrics {
    pad: f32,
    search_h: f32,
    footer_h: f32,
    gap: f32,
    group_gap: f32,
    tile_pad: f32,
    title_h: f32,
    title_gap: f32,
    row_h: f32,
    radius: f32,
}

fn fit_rect(area: R, aspect: f32) -> R {
    let a_area = area.w / area.h.max(1.0);
    if aspect > a_area {
        let h = area.w / aspect;
        R::new(area.x, area.y + (area.h - h) / 2.0, area.w, h)
    } else {
        let w = area.h * aspect;
        R::new(area.x + (area.w - w) / 2.0, area.y, w, area.h)
    }
}

unsafe extern "system" fn dim_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    DefWindowProcW(h, m, w, l)
}

/// Map a client point back through the open-animation zoom.
fn mouse_xy(s: &Switcher, l: LPARAM) -> (f32, f32) {
    let x = (l.0 & 0xFFFF) as i16 as f32;
    let y = ((l.0 >> 16) & 0xFFFF) as i16 as f32;
    let (cx, cy) = (s.panel_w / 2.0, s.panel_h / 2.0);
    (cx + (x - cx) / s.zoom, cy + (y - cy) / s.zoom)
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let Some(s) = SW.try_get() else {
        return DefWindowProcW(h, msg, w, l);
    };
    match msg {
        WM_SW_OPEN => s.open(w.0),
        WM_SW_NEXT => {
            if s.session {
                s.step(1)
            }
        }
        WM_SW_PREV => {
            if s.session {
                s.step(-1)
            }
        }
        WM_SW_COMMIT => s.commit(),
        WM_SW_KEY => s.key(w.0 as u32, l.0),
        WM_SW_ANIM => s.tick(),
        WM_TIMER => match w.0 {
            TIMER_SHOW => s.show(),
            TIMER_TRIM => {
                let _ = KillTimer(h, TIMER_TRIM);
                if !s.session {
                    // Give idle memory back to Windows; it pages back in on demand.
                    let _ = K32EmptyWorkingSet(GetCurrentProcess());
                }
            }
            _ => {}
        },
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
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
            let (x, y) = mouse_xy(s, l);
            let old = (s.hover, s.hover_btn, s.sel);
            // The hovered tile reveals its buttons, so hit-test again with the new hover.
            s.hover = s.hit(x, y).0;
            s.hover_btn = s.hit(x, y).1;
            if cfg().hover_select {
                if let Some(i) = s.hover {
                    s.sel = i;
                }
            }
            if old != (s.hover, s.hover_btn, s.sel) {
                if old.2 != s.sel {
                    s.kick();
                }
                s.render();
            }
        }
        WM_MOUSELEAVE => {
            s.tracking = false;
            s.hover = None;
            s.hover_btn = None;
            s.render();
        }
        WM_LBUTTONUP => {
            let (x, y) = mouse_xy(s, l);
            match s.hit(x, y) {
                (_, Some((i, b))) => s.act(i, Switcher::btn_act(b)),
                (Some(i), None) => {
                    s.sel = i;
                    s.commit();
                }
                _ => {}
            }
        }
        WM_RBUTTONUP => {
            let (x, y) = mouse_xy(s, l);
            if let (Some(i), _) = s.hit(x, y) {
                s.context_menu(i);
            }
        }
        WM_MBUTTONUP => {
            let (x, y) = mouse_xy(s, l);
            if let (Some(i), _) = s.hit(x, y) {
                s.act(i, Act::Close);
            }
        }
        WM_MOUSEWHEEL => {
            let delta = ((w.0 >> 16) & 0xFFFF) as i16;
            s.step(if delta > 0 { -1 } else { 1 });
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(h, &mut ps);
            s.render();
            let _ = EndPaint(h, &ps);
        }
        WM_ERASEBKGND => return LRESULT(1),
        _ => return DefWindowProcW(h, msg, w, l),
    }
    LRESULT(0)
}
