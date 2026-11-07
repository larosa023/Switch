//! The switcher overlay: a topmost, non-activating popup that shows live DWM
//! thumbnails of every window, with search, grouping and window actions.

use std::collections::HashMap;
use std::time::Instant;

use windows::core::{w, Result};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::{self, cfg, Layout, Monitors, PreviewSize};
use crate::gfx::{self, gfx, Painter, Palette, C, R};
use crate::hook::{self, *};
use crate::util::{self, UiCell};
use crate::winlist::{self, WinInfo};

const TIMER_SHOW: usize = 1;
const TIMER_ANIM: usize = 2;
const FADE_MS: f32 = 140.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Btn {
    Pin,
    Min,
    Close,
}
const BTNS: [Btn; 3] = [Btn::Pin, Btn::Min, Btn::Close];

#[derive(Clone, Default)]
struct Item {
    win: usize,
    row: usize,
    tile: R,
    title: R,
    icon: R,
    thumb_area: R,
    btns: [R; 3],
}

struct Fmts {
    title: IDWriteTextFormat,
    search: IDWriteTextFormat,
    meta: IDWriteTextFormat,
    empty: IDWriteTextFormat,
    letter: IDWriteTextFormat,
}

pub struct Switcher {
    hwnd: HWND,
    rt: Option<ID2D1HwndRenderTarget>,
    rtb: Option<ID2D1RenderTarget>,
    brush: Option<ID2D1SolidColorBrush>,
    fmts: Option<Fmts>,
    fmt_scale: f32,
    pal: Palette,

    all: Vec<WinInfo>,
    items: Vec<Item>,
    sel: usize,
    query: String,
    grouped: bool,

    session: bool,
    visible: bool,
    scale: f32,
    work: RECT,
    panel_w: f32,
    panel_h: f32,
    thumb_h: f32,
    search_r: R,

    thumbs: HashMap<isize, isize>,
    icons: HashMap<isize, Option<ID2D1Bitmap>>,

    hover: Option<usize>,
    hover_btn: Option<(usize, Btn)>,
    tracking: bool,

    hl: R,
    fade: f32,
    t_open: Instant,
    t_session: Instant,
    t_last: Instant,
    animating: bool,
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
        let pref = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const _ as *const _,
            std::mem::size_of_val(&pref) as u32,
        );
        let now = Instant::now();
        SW.set(Switcher {
            hwnd,
            rt: None,
            rtb: None,
            brush: None,
            fmts: None,
            fmt_scale: 0.0,
            pal: gfx::palette(cfg()),
            all: Vec::new(),
            items: Vec::new(),
            sel: 0,
            query: String::new(),
            grouped: false,
            session: false,
            visible: false,
            scale: 1.0,
            work: RECT::default(),
            panel_w: 100.0,
            panel_h: 100.0,
            thumb_h: 150.0,
            search_r: R::default(),
            thumbs: HashMap::new(),
            icons: HashMap::new(),
            hover: None,
            hover_btn: None,
            tracking: false,
            hl: R::default(),
            fade: 1.0,
            t_open: now,
            t_session: now,
            t_last: now,
            animating: false,
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

/// Open the switcher in sticky mode (tray menu "Open Switch").
pub fn open_sticky() {
    if sw().session {
        return;
    }
    hook::open_from_ui();
    sw().open(OPEN_STICKY);
}

unsafe extern "system" fn on_foreground(
    _: HWINEVENTHOOK,
    _event: u32,
    h: HWND,
    _: i32,
    _: i32,
    _: u32,
    _: u32,
) {
    if let Some(s) = SW.try_get() {
        // Ignore the focus shuffle right after opening (e.g. the tray menu closing).
        if s.session && h != s.hwnd && !h.is_invalid() && s.t_session.elapsed().as_millis() > 400 {
            s.close();
        }
    }
}

impl Switcher {
    // ---- session lifecycle ---------------------------------------------------

    fn open(&mut self, flags: usize) {
        if self.session {
            self.step(if flags & OPEN_REVERSE != 0 { -1 } else { 1 });
            return;
        }
        self.session = true;
        self.t_session = Instant::now();
        hook::UI_ACK.store(true, std::sync::atomic::Ordering::Relaxed);
        self.pal = gfx::palette(cfg());
        self.query.clear();
        self.grouped = cfg().group_by_app;
        self.hover = None;
        self.hover_btn = None;

        unsafe {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            self.work = winlist::monitor_work_area(mon);
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            self.scale = dx as f32 / 96.0;

            let fg = GetForegroundWindow();
            let c = cfg();
            let mut all = winlist::enumerate();
            all.retain(|w| {
                !c.hidden_apps.iter().any(|h| h.eq_ignore_ascii_case(&w.exe))
                    && (c.show_minimized || !w.minimized)
                    && (c.monitors == Monitors::All || w.monitor == mon.0 as isize)
            });
            let fg_first = all.first().map(|w| util::hwnd(w.hwnd) == fg).unwrap_or(false);
            self.all = all;
            // Drop cached icons for windows that no longer exist.
            let live: std::collections::HashSet<isize> = self.all.iter().map(|w| w.hwnd).collect();
            self.icons.retain(|k, _| live.contains(k));

            self.rebuild();
            self.layout_panel();
            let n = self.items.len();
            self.sel = if n == 0 {
                0
            } else if flags & OPEN_REVERSE != 0 {
                n - 1
            } else if fg_first && n > 1 {
                1
            } else {
                0
            };

            let delay = cfg().show_delay_ms;
            if delay == 0 || flags & OPEN_STICKY != 0 {
                self.show();
            } else {
                SetTimer(self.hwnd, TIMER_SHOW, delay, None);
            }
        }
    }

    fn show(&mut self) {
        if !self.session || self.visible {
            return;
        }
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_SHOW);
            let x = self.work.left + ((self.work.right - self.work.left) as f32 - self.panel_w) as i32 / 2;
            let y = self.work.top + ((self.work.bottom - self.work.top) as f32 - self.panel_h) as i32 / 2;
            let (w, h) = (self.panel_w.round() as i32, self.panel_h.round() as i32);
            let _ = SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                x,
                y,
                w,
                h,
                SWP_NOACTIVATE | SWP_NOREDRAW,
            );
            self.resize_target(w as u32, h as u32);
            let border = self.pal.border.mix(self.pal.bg, 0.0);
            let colorref = COLORREF(
                ((border.2 * 255.0) as u32) << 16 | ((border.1 * 255.0) as u32) << 8 | (border.0 * 255.0) as u32,
            );
            let _ = DwmSetWindowAttribute(
                self.hwnd,
                DWMWA_BORDER_COLOR,
                &colorref as *const _ as *const _,
                4,
            );
            self.visible = true;
            self.t_open = Instant::now();
            self.t_last = self.t_open;
            self.fade = if cfg().animations { 0.0 } else { 1.0 };
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
            let _ = KillTimer(self.hwnd, TIMER_ANIM);
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
                self.visible = false;
            }
            self.thumbs.clear();
            self.items.clear();
            self.all.clear();
        }
    }

    fn commit(&mut self) {
        if !self.session {
            return;
        }
        let target = self.items.get(self.sel).map(|it| self.all[it.win].hwnd);
        self.close();
        if let Some(h) = target {
            winlist::activate(h);
        }
    }

    // ---- list building & layout ----------------------------------------------

    fn rebuild(&mut self) {
        let q = self.query.to_lowercase();
        let tokens: Vec<&str> = q.split_whitespace().collect();
        let mut picked: Vec<(usize, u8)> = Vec::new();
        for (i, w) in self.all.iter().enumerate() {
            if tokens.is_empty() {
                picked.push((i, 0));
                continue;
            }
            let hay = format!("{} {} {}", w.title, w.app_name, w.exe).to_lowercase();
            if tokens.iter().all(|t| hay.contains(t)) {
                let app = w.app_name.to_lowercase();
                let score = if app.starts_with(&q) || w.exe.starts_with(&q) {
                    0
                } else if hay.split(|c: char| !c.is_alphanumeric()).any(|word| word.starts_with(tokens[0])) {
                    1
                } else {
                    2
                };
                picked.push((i, score));
            }
        }
        picked.sort_by_key(|&(_, s)| s); // stable: keeps MRU order within a score
        let mut order: Vec<usize> = picked.into_iter().map(|(i, _)| i).collect();
        if self.grouped {
            let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
            for i in order {
                let key = self.all[i].exe.clone();
                match groups.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, v)) => v.push(i),
                    None => groups.push((key, vec![i])),
                }
            }
            order = groups.into_iter().flat_map(|(_, v)| v).collect();
        }
        self.items = order.into_iter().map(|win| Item { win, ..Default::default() }).collect();
    }

    fn base_thumb_h(&self) -> f32 {
        let h = match cfg().preview_size {
            PreviewSize::Small => 112.0,
            PreviewSize::Medium => 150.0,
            PreviewSize::Large => 200.0,
        };
        h * self.scale
    }

    fn metrics(&self) -> (f32, f32, f32, f32, f32, f32, f32) {
        let s = self.scale;
        // pad, search_h, gap, group_gap, tile_pad, title_h, title_gap
        (16.0 * s, 40.0 * s, 8.0 * s, 22.0 * s, 8.0 * s, 26.0 * s, 6.0 * s)
    }

    fn tile_w(&self, it: &Item, h: f32) -> f32 {
        let (_, _, _, _, tp, _, _) = self.metrics();
        (h * self.all[it.win].aspect).clamp(h * 0.8, h * 1.9) + 2.0 * tp
    }

    /// Greedy row packing. Returns rows of item indices and the widest row.
    fn pack(&self, h: f32, max_w: f32) -> (Vec<Vec<usize>>, f32) {
        let (_, _, gap, ggap, _, _, _) = self.metrics();
        let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
        let mut row_w = 0.0f32;
        let mut widest = 0.0f32;
        for (i, it) in self.items.iter().enumerate() {
            let tw = self.tile_w(it, h);
            let sep = if i > 0 && self.group_break(i) { ggap } else { gap };
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
        widest = widest.max(row_w);
        (rows, widest)
    }

    fn group_break(&self, i: usize) -> bool {
        self.grouped && i > 0 && self.all[self.items[i].win].exe != self.all[self.items[i - 1].win].exe
    }

    fn tile_h(&self, h: f32) -> f32 {
        let (_, _, _, _, tp, th, tg) = self.metrics();
        tp + th + tg + h + tp
    }

    /// Size the panel for the full (unfiltered) list. The panel then keeps its
    /// size for the rest of the session so typing never makes it jump around.
    fn layout_panel(&mut self) {
        let (pad, search_h, gap, _, _, _, _) = self.metrics();
        let s = self.scale;
        let work_w = (self.work.right - self.work.left) as f32;
        let work_h = (self.work.bottom - self.work.top) as f32;
        let max_w = work_w * 0.94 - 2.0 * pad;
        let max_h = work_h * 0.9;
        let min_h = 64.0 * s;
        let mut h = self.base_thumb_h();
        let (rows, widest) = loop {
            let (rows, widest) = self.pack(h, max_w);
            let n = rows.len().max(1) as f32;
            let total = pad + search_h + pad * 0.75 + n * self.tile_h(h) + (n - 1.0) * gap + pad;
            let too_many_rows = cfg().layout == Layout::Row && rows.len() > 1;
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
            n as f32 * self.tile_h(h) + (n as f32 - 1.0) * gap
        };
        self.panel_w = (widest + 2.0 * pad).max((480.0 * s).min(work_w * 0.9));
        self.panel_h = pad + search_h + pad * 0.75 + content_h + pad;
        self.place_items();
    }

    /// Position tiles inside the (fixed) panel.
    fn place_items(&mut self) {
        let (pad, search_h, gap, ggap, tp, th, tg) = self.metrics();
        let s = self.scale;
        self.search_r = R::new(pad, pad, self.panel_w - 2.0 * pad, search_h);
        let max_w = self.panel_w - 2.0 * pad;
        let avail_h = self.panel_h - (pad + search_h + pad * 0.75) - pad;
        let mut h = self.thumb_h;
        let mut rows = self.pack(h, max_w).0;
        // Grouping gaps can push a filtered list over; shrink a touch if needed.
        while h > 48.0 * s {
            let n = rows.len() as f32;
            if n * self.tile_h(h) + (n - 1.0) * gap <= avail_h + 0.5 {
                break;
            }
            h *= 0.94;
            rows = self.pack(h, max_w).0;
        }
        let tile_h = self.tile_h(h);
        let mut y = pad + search_h + pad * 0.75;
        for (ri, row) in rows.iter().enumerate() {
            let widths: Vec<f32> = row.iter().map(|&i| self.tile_w(&self.items[i], h)).collect();
            let mut row_w = 0.0;
            for (k, &i) in row.iter().enumerate() {
                if k > 0 {
                    row_w += if self.group_break(i) { ggap } else { gap };
                }
                row_w += widths[k];
            }
            let mut x = pad + (max_w - row_w) / 2.0;
            for (k, &i) in row.iter().enumerate() {
                if k > 0 {
                    x += if self.group_break(i) { ggap } else { gap };
                }
                let tw = widths[k];
                let tile = R::new(x, y, tw, tile_h);
                let title = R::new(x + tp, y + tp, tw - 2.0 * tp, th);
                let icon_s = 18.0 * s;
                let it = &mut self.items[i];
                it.row = ri;
                it.tile = tile;
                it.title = title;
                it.icon = R::new(title.x + 2.0 * s, title.y + (th - icon_s) / 2.0, icon_s, icon_s);
                it.thumb_area = R::new(x + tp, y + tp + th + tg, tw - 2.0 * tp, h);
                let b = 22.0 * s;
                for (bi, r) in it.btns.iter_mut().enumerate() {
                    let from_right = (3 - bi) as f32;
                    *r = R::new(title.right() - from_right * (b + 2.0 * s) + 2.0 * s, title.y + (th - b) / 2.0, b, b);
                }
                x += tw;
            }
            y += tile_h + gap;
        }
    }

    fn refilter(&mut self) {
        self.rebuild();
        self.place_items();
        self.sel = 0;
        self.hover = None;
        self.hover_btn = None;
        if !self.visible {
            self.show();
        }
        self.update_thumbs();
        self.kick();
    }

    /// Re-layout after windows were removed or grouping changed (may resize panel).
    fn relayout_full(&mut self) {
        let keep = self.items.get(self.sel).map(|it| self.all[it.win].hwnd);
        self.rebuild();
        self.layout_panel();
        self.sel = keep
            .and_then(|h| self.items.iter().position(|it| self.all[it.win].hwnd == h))
            .unwrap_or(0)
            .min(self.items.len().saturating_sub(1));
        if self.visible {
            unsafe {
                let x = self.work.left + ((self.work.right - self.work.left) as f32 - self.panel_w) as i32 / 2;
                let y = self.work.top + ((self.work.bottom - self.work.top) as f32 - self.panel_h) as i32 / 2;
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

    fn update_thumbs(&mut self) {
        if !self.visible {
            return;
        }
        let shown: HashMap<isize, R> =
            self.items.iter().map(|it| (self.all[it.win].hwnd, it.thumb_area)).collect();
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

    // ---- navigation & actions --------------------------------------------------

    fn kick(&mut self) {
        if !self.visible {
            return;
        }
        if !cfg().animations {
            if let Some(it) = self.items.get(self.sel) {
                self.hl = it.tile;
            }
            self.fade = 1.0;
            self.render();
            return;
        }
        if !self.animating {
            self.animating = true;
            self.t_last = Instant::now();
            unsafe {
                SetTimer(self.hwnd, TIMER_ANIM, 8, None);
            }
        }
        self.render();
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let dt = (now - self.t_last).as_secs_f32().min(0.05);
        self.t_last = now;
        let t = ((now - self.t_open).as_secs_f32() * 1000.0 / FADE_MS).min(1.0);
        let new_fade = 1.0 - (1.0 - t).powi(3);
        let fade_changed = (new_fade - self.fade).abs() > 0.001;
        self.fade = new_fade;
        let mut settled = self.fade >= 1.0;
        if let Some(it) = self.items.get(self.sel) {
            let k = 1.0 - (-dt * 24.0).exp();
            self.hl = self.hl.lerp(&it.tile, k);
            if self.hl.dist(&it.tile) < 0.6 {
                self.hl = it.tile;
            } else {
                settled = false;
            }
        }
        if fade_changed {
            self.update_thumbs();
        }
        self.render();
        if settled {
            self.animating = false;
            unsafe {
                let _ = KillTimer(self.hwnd, TIMER_ANIM);
            }
        }
    }

    fn step(&mut self, d: i32) {
        let n = self.items.len() as i32;
        if n == 0 {
            return;
        }
        self.sel = ((self.sel as i32 + d).rem_euclid(n)) as usize;
        self.kick();
    }

    fn vertical(&mut self, d: i32) {
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

    fn act(&mut self, idx: usize, b: Btn) {
        let Some(it) = self.items.get(idx) else { return };
        let h = self.all[it.win].hwnd;
        match b {
            Btn::Close => {
                winlist::close(h);
                self.drop_window(h);
                self.relayout_full();
            }
            Btn::Min => {
                winlist::minimize(h);
                if cfg().show_minimized {
                    self.all[it.win].minimized = true;
                    self.render();
                } else {
                    self.drop_window(h);
                    self.relayout_full();
                }
            }
            Btn::Pin => {
                let top = winlist::toggle_topmost(h);
                self.all[it.win].topmost = top;
                // Keep our overlay above the newly pinned window.
                unsafe {
                    let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                }
                self.render();
            }
        }
    }

    fn hide_app(&mut self) {
        let Some(it) = self.items.get(self.sel) else { return };
        let exe = self.all[it.win].exe.clone();
        if exe.is_empty() {
            return;
        }
        if !cfg().hidden_apps.contains(&exe) {
            cfg().hidden_apps.push(exe.clone());
            config::save();
            crate::settings_ui::refresh();
        }
        let gone: Vec<isize> = self.all.iter().filter(|w| w.exe == exe).map(|w| w.hwnd).collect();
        for h in gone {
            self.drop_window(h);
        }
        self.relayout_full();
    }

    fn key(&mut self, vk: u32, flags: isize) {
        if !self.session {
            return;
        }
        let ctrl = flags & KEY_CTRL != 0;
        let shift = flags & KEY_SHIFT != 0;
        let vkk = VIRTUAL_KEY(vk as u16);
        if ctrl {
            let handled = match vk as u8 {
                b'W' => {
                    self.act(self.sel, Btn::Close);
                    true
                }
                b'M' => {
                    self.act(self.sel, Btn::Min);
                    true
                }
                b'P' => {
                    self.act(self.sel, Btn::Pin);
                    true
                }
                b'H' => {
                    self.hide_app();
                    true
                }
                b'G' => {
                    self.grouped = !self.grouped;
                    self.relayout_full();
                    true
                }
                _ => false,
            };
            if handled {
                return;
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
            VK_HOME => {
                self.sel = 0;
                self.kick();
            }
            VK_END => {
                self.sel = self.items.len().saturating_sub(1);
                self.kick();
            }
            VK_DELETE => self.act(self.sel, Btn::Close),
            VK_BACK => {
                if self.query.pop().is_some() {
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

    fn hit(&self, x: f32, y: f32) -> (Option<usize>, Option<(usize, Btn)>) {
        for (i, it) in self.items.iter().enumerate() {
            if it.tile.contains(x, y) {
                let show_btns = Some(i) == self.hover || i == self.sel;
                if show_btns {
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
            self.rtb = Some(base);
            self.rt = Some(rt);
            self.icons.clear();
        }
    }

    fn resize_target(&mut self, w: u32, h: u32) {
        self.ensure_target();
        if let Some(rt) = &self.rt {
            unsafe {
                let _ = rt.Resize(&windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_U { width: w, height: h });
            }
        }
    }

    fn ensure_fmts(&mut self) {
        if self.fmts.is_some() && (self.fmt_scale - self.scale).abs() < 0.01 {
            return;
        }
        let s = self.scale;
        let g = gfx();
        let mk = |size: f32, weight, align| g.text_format(size * s, weight, align).ok();
        if let (Some(title), Some(search), Some(meta), Some(empty), Some(letter)) = (
            mk(12.5, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING),
            mk(15.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING),
            mk(12.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_TRAILING),
            mk(14.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER),
            mk(10.5, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER),
        ) {
            self.fmts = Some(Fmts { title, search, meta, empty, letter });
            self.fmt_scale = s;
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
        // Resolve icons first (needs &mut self).
        let icons: Vec<Option<ID2D1Bitmap>> =
            (0..self.items.len()).map(|i| self.icon_for(self.items[i].win)).collect();
        let (Some(rt), Some(rtb), Some(brush), Some(f)) = (&self.rt, &self.rtb, &self.brush, &self.fmts) else {
            return;
        };
        let p = Painter { rt: rtb, brush, alpha: 1.0 };
        let pal = self.pal;
        let s = self.scale;
        unsafe {
            rt.BeginDraw();
            rt.Clear(Some(&pal.bg.d2d()));
        }
        if self.session {
            let p = Painter { alpha: self.fade, ..p };
            self.draw_search(&p, f, &pal);
            if self.items.is_empty() {
                let msg = if self.query.is_empty() {
                    "No windows to switch to".to_string()
                } else {
                    format!("No windows match \u{201C}{}\u{201D}", self.query)
                };
                let y = self.search_r.bottom() + 12.0 * s;
                p.text(&msg, &f.empty, R::new(0.0, y, self.panel_w, self.panel_h - y - 12.0 * s), pal.subtext);
            } else {
                // Selection highlight (animated), drawn under the tiles.
                p.fill(self.hl, 12.0 * s, pal.accent.a(if pal.dark { 0.20 } else { 0.14 }));
                p.stroke(self.hl, 12.0 * s, pal.accent, 2.0 * s);
                for (i, it) in self.items.iter().enumerate() {
                    self.draw_tile(&p, f, &pal, i, it, icons[i].as_ref());
                }
            }
        }
        unsafe {
            let mut t1 = 0u64;
            let mut t2 = 0u64;
            if let Err(e) = rt.EndDraw(Some(&mut t1), Some(&mut t2)) {
                if e.code() == D2DERR_RECREATE_TARGET {
                    self.rt = None;
                    self.rtb = None;
                    self.brush = None;
                    self.icons.clear();
                }
            }
            let _ = ValidateRect(self.hwnd, None);
        }
    }

    fn draw_search(&self, p: &Painter, f: &Fmts, pal: &Palette) {
        let s = self.scale;
        let r = self.search_r;
        p.fill(r, 10.0 * s, pal.surface);
        p.stroke(r, 10.0 * s, if self.query.is_empty() { pal.border } else { pal.accent.a(0.6) }, 1.0 * s);
        p.glyph_search(r.x + 20.0 * s, r.y + r.h / 2.0, 8.0 * s, pal.subtext, 1.6 * s);
        let count = self.items.len();
        let meta = if self.grouped {
            format!("{} window{} \u{00B7} grouped", count, if count == 1 { "" } else { "s" })
        } else {
            format!("{} window{}", count, if count == 1 { "" } else { "s" })
        };
        let meta_w = gfx().measure(&meta, &f.meta) + 4.0;
        let text_r = R::new(r.x + 38.0 * s, r.y, r.w - 38.0 * s - meta_w - 16.0 * s, r.h);
        if self.query.is_empty() {
            p.text("Type to search windows", &f.search, text_r, pal.faint);
        } else {
            p.text(&self.query, &f.search, text_r, pal.text);
            // Caret
            let w = gfx().measure(&self.query, &f.search).min(text_r.w);
            let cx = text_r.x + w + 2.0 * s;
            p.line(cx, r.y + 11.0 * s, cx, r.bottom() - 11.0 * s, pal.accent, 1.5 * s);
        }
        p.text(&meta, &f.meta, R::new(r.right() - meta_w - 14.0 * s, r.y, meta_w, r.h), pal.faint);
    }

    fn draw_tile(&self, p: &Painter, f: &Fmts, pal: &Palette, i: usize, it: &Item, icon: Option<&ID2D1Bitmap>) {
        let s = self.scale;
        let w = &self.all[it.win];
        let selected = i == self.sel;
        let hovered = Some(i) == self.hover;
        if hovered && !selected {
            p.fill(it.tile, 12.0 * s, pal.surface_hover.a(0.8));
        }
        // Preview well (visible around letterboxed / minimized thumbnails).
        p.fill(it.thumb_area, 6.0 * s, pal.thumb_bg);
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
        // Title row.
        match icon {
            Some(b) => p.bitmap(b, it.icon, 1.0),
            None => {
                let c = it.icon;
                p.circle(c.x + c.w / 2.0, c.y + c.h / 2.0, c.w / 2.0, pal.accent);
                let letter: String = w.app_name.chars().next().unwrap_or('?').to_uppercase().collect();
                p.text(&letter, &f.letter, c, pal.on_accent);
            }
        }
        let show_btns = selected || hovered;
        let mut text_right = it.title.right();
        if show_btns {
            text_right = it.btns[0].x - 4.0 * s;
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
        } else if w.topmost {
            let r = it.btns[2];
            p.glyph_pin(r.x + r.w / 2.0, r.y + r.h / 2.0 - 0.5 * s, 4.2 * s, pal.accent, 1.2 * s, true);
            text_right = r.x - 2.0 * s;
        }
        let tx = it.icon.right() + 8.0 * s;
        let color = if selected { pal.text } else if w.minimized { pal.faint } else { pal.subtext };
        p.text(&w.title, &f.title, R::new(tx, it.title.y, (text_right - tx).max(0.0), it.title.h), color);
    }
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

fn mouse_xy(l: LPARAM) -> (f32, f32) {
    let x = (l.0 & 0xFFFF) as i16 as f32;
    let y = ((l.0 >> 16) & 0xFFFF) as i16 as f32;
    (x, y)
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
        WM_TIMER => match w.0 {
            TIMER_SHOW => s.show(),
            TIMER_ANIM => s.tick(),
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
            let (x, y) = mouse_xy(l);
            let old = (s.hover, s.hover_btn);
            // The hovered tile reveals its buttons, so hit-test again with the new hover.
            s.hover = s.hit(x, y).0;
            s.hover_btn = s.hit(x, y).1;
            if old != (s.hover, s.hover_btn) {
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
            let (x, y) = mouse_xy(l);
            match s.hit(x, y) {
                (_, Some((i, b))) => s.act(i, b),
                (Some(i), None) => {
                    s.sel = i;
                    s.commit();
                }
                _ => {}
            }
        }
        WM_MBUTTONUP => {
            let (x, y) = mouse_xy(l);
            if let (Some(i), _) = s.hit(x, y) {
                s.act(i, Btn::Close);
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
