//! Direct2D / DirectWrite / WIC plumbing, the theme palette and a tiny
//! painter used by both the switcher and the settings window.

use windows::core::{Interface, Result, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Registry::HKEY_CURRENT_USER;
use windows::Win32::UI::WindowsAndMessaging::HICON;

use crate::config::{Config, Theme};
use crate::util::{self, utf16, wide, UiCell};

pub struct Gfx {
    pub d2d: ID2D1Factory,
    pub dw: IDWriteFactory,
    pub wic: IWICImagingFactory,
    family: Vec<u16>,
}

pub static GFX: UiCell<Gfx> = UiCell::new();
pub fn gfx() -> &'static Gfx {
    GFX.get()
}

impl Gfx {
    pub fn init() -> Result<()> {
        unsafe {
            let d2d: ID2D1Factory =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, Some(&D2D1_FACTORY_OPTIONS::default()))?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let wic: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            // Prefer Windows 11's Segoe UI Variable, fall back to Segoe UI.
            let mut family = wide("Segoe UI");
            let mut coll: Option<IDWriteFontCollection> = None;
            if dw.GetSystemFontCollection(&mut coll, false).is_ok() {
                if let Some(c) = coll {
                    let name = wide("Segoe UI Variable Text");
                    let mut idx = 0u32;
                    let mut exists = BOOL(0);
                    if c.FindFamilyName(PCWSTR(name.as_ptr()), &mut idx, &mut exists).is_ok()
                        && exists.as_bool()
                    {
                        family = name;
                    }
                }
            }
            GFX.set(Gfx { d2d, dw, wic, family });
            Ok(())
        }
    }

    pub fn text_format(&self, size: f32, weight: DWRITE_FONT_WEIGHT, align: DWRITE_TEXT_ALIGNMENT) -> Result<IDWriteTextFormat> {
        unsafe {
            let locale = wide("en-us");
            let f = self.dw.CreateTextFormat(
                PCWSTR(self.family.as_ptr()),
                None,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                PCWSTR(locale.as_ptr()),
            )?;
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            f.SetTextAlignment(align)?;
            let sign = self.dw.CreateEllipsisTrimmingSign(&f)?;
            let trimming = DWRITE_TRIMMING {
                granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                delimiter: 0,
                delimiterCount: 0,
            };
            f.SetTrimming(&trimming, &sign)?;
            Ok(f)
        }
    }

    pub fn measure(&self, text: &str, fmt: &IDWriteTextFormat) -> f32 {
        unsafe {
            let t = utf16(text);
            match self.dw.CreateTextLayout(&t, fmt, 10_000.0, 1000.0) {
                Ok(layout) => {
                    let mut m = DWRITE_TEXT_METRICS::default();
                    if layout.GetMetrics(&mut m).is_ok() {
                        m.widthIncludingTrailingWhitespace
                    } else {
                        0.0
                    }
                }
                Err(_) => 0.0,
            }
        }
    }

    pub fn create_target(&self, hwnd: HWND, w: u32, h: u32) -> Result<ID2D1HwndRenderTarget> {
        unsafe {
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let hprops = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd,
                pixelSize: D2D_SIZE_U { width: w.max(1), height: h.max(1) },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            let rt = self.d2d.CreateHwndRenderTarget(&props, &hprops)?;
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            Ok(rt)
        }
    }

    pub fn icon_bitmap(&self, rt: &ID2D1RenderTarget, icon: HICON) -> Option<ID2D1Bitmap> {
        unsafe {
            let src = self.wic.CreateBitmapFromHICON(icon).ok()?;
            let conv = self.wic.CreateFormatConverter().ok()?;
            conv.Initialize(
                &src,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeMedianCut,
            )
            .ok()?;
            rt.CreateBitmapFromWicBitmap(&conv, None).ok()
        }
    }
}

// ---- Geometry ---------------------------------------------------------------

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct R {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl R {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }
    pub fn inset(&self, d: f32) -> R {
        R::new(self.x + d, self.y + d, (self.w - 2.0 * d).max(0.0), (self.h - 2.0 * d).max(0.0))
    }
    pub fn d2d(&self) -> D2D_RECT_F {
        D2D_RECT_F { left: self.x, top: self.y, right: self.x + self.w, bottom: self.y + self.h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn lerp(&self, o: &R, t: f32) -> R {
        R::new(
            self.x + (o.x - self.x) * t,
            self.y + (o.y - self.y) * t,
            self.w + (o.w - self.w) * t,
            self.h + (o.h - self.h) * t,
        )
    }
    pub fn dist(&self, o: &R) -> f32 {
        (self.x - o.x).abs() + (self.y - o.y).abs() + (self.w - o.w).abs() + (self.h - o.h).abs()
    }
}

// ---- Color & theme ----------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct C(pub f32, pub f32, pub f32, pub f32);

impl C {
    pub fn hex(rgb: u32) -> C {
        C(
            ((rgb >> 16) & 0xFF) as f32 / 255.0,
            ((rgb >> 8) & 0xFF) as f32 / 255.0,
            (rgb & 0xFF) as f32 / 255.0,
            1.0,
        )
    }
    pub fn a(self, a: f32) -> C {
        C(self.0, self.1, self.2, self.3 * a)
    }
    pub fn mix(self, o: C, t: f32) -> C {
        C(
            self.0 + (o.0 - self.0) * t,
            self.1 + (o.1 - self.1) * t,
            self.2 + (o.2 - self.2) * t,
            self.3 + (o.3 - self.3) * t,
        )
    }
    pub fn d2d(self) -> D2D1_COLOR_F {
        D2D1_COLOR_F { r: self.0, g: self.1, b: self.2, a: self.3 }
    }
    pub fn luminance(self) -> f32 {
        0.2126 * self.0 + 0.7152 * self.1 + 0.0722 * self.2
    }
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    pub bg: C,
    pub surface: C,
    pub surface_hover: C,
    pub border: C,
    pub text: C,
    pub subtext: C,
    pub faint: C,
    pub accent: C,
    pub on_accent: C,
    pub thumb_bg: C,
}

pub const ACCENTS: [(&str, u32); 6] = [
    ("Blue", 0x3B82F6),
    ("Indigo", 0x6366F1),
    ("Teal", 0x14B8A6),
    ("Green", 0x22C55E),
    ("Amber", 0xF59E0B),
    ("Rose", 0xF43F5E),
];

pub fn system_dark() -> bool {
    util::reg_get_dword(
        HKEY_CURRENT_USER,
        r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        "AppsUseLightTheme",
    )
    .map(|v| v == 0)
    .unwrap_or(true)
}

pub fn system_accent() -> u32 {
    // Stored as ABGR.
    util::reg_get_dword(HKEY_CURRENT_USER, r"Software\Microsoft\Windows\DWM", "AccentColor")
        .map(|abgr| ((abgr & 0xFF) << 16) | (abgr & 0xFF00) | ((abgr >> 16) & 0xFF))
        .unwrap_or(0x3B82F6)
}

pub fn parse_hex(s: &str) -> Option<u32> {
    u32::from_str_radix(s.trim().trim_start_matches('#'), 16).ok()
}

pub fn palette(c: &Config) -> Palette {
    let dark = match c.theme {
        Theme::System => system_dark(),
        Theme::Dark => true,
        Theme::Light => false,
    };
    let accent_rgb = if c.accent == "system" {
        system_accent()
    } else {
        parse_hex(&c.accent).unwrap_or(0x3B82F6)
    };
    let mut accent = C::hex(accent_rgb);
    // Keep the accent readable against the background.
    if dark && accent.luminance() < 0.25 {
        accent = accent.mix(C::hex(0xFFFFFF), 0.35);
    }
    if !dark && accent.luminance() > 0.7 {
        accent = accent.mix(C::hex(0x000000), 0.3);
    }
    let on_accent = if accent.luminance() > 0.6 { C::hex(0x111114) } else { C::hex(0xFFFFFF) };
    if dark {
        Palette {
            dark,
            bg: C::hex(0x1B1B1F),
            surface: C::hex(0x26262B),
            surface_hover: C::hex(0x2E2E34),
            border: C::hex(0xFFFFFF).a(0.08),
            text: C::hex(0xF4F4F6),
            subtext: C::hex(0xA3A3AD),
            faint: C::hex(0x6E6E78),
            accent,
            on_accent,
            thumb_bg: C::hex(0x131316),
        }
    } else {
        Palette {
            dark,
            bg: C::hex(0xF6F6F8),
            surface: C::hex(0xFFFFFF),
            surface_hover: C::hex(0xEAEAEE),
            border: C::hex(0x000000).a(0.08),
            text: C::hex(0x17171B),
            subtext: C::hex(0x5D5D68),
            faint: C::hex(0x9A9AA5),
            accent,
            on_accent,
            thumb_bg: C::hex(0xE4E4E9),
        }
    }
}

// ---- Painter ----------------------------------------------------------------

pub struct Painter<'a> {
    pub rt: &'a ID2D1RenderTarget,
    pub brush: &'a ID2D1SolidColorBrush,
    /// Global opacity multiplier (fade-in).
    pub alpha: f32,
}

impl<'a> Painter<'a> {
    fn set(&self, c: C) {
        unsafe { self.brush.SetColor(&c.a(self.alpha).d2d()) }
    }
    pub fn fill(&self, r: R, radius: f32, c: C) {
        self.set(c);
        unsafe {
            if radius <= 0.0 {
                self.rt.FillRectangle(&r.d2d(), self.brush);
            } else {
                let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
                self.rt.FillRoundedRectangle(&rr, self.brush);
            }
        }
    }
    pub fn stroke(&self, r: R, radius: f32, c: C, width: f32) {
        self.set(c);
        unsafe {
            let rr = D2D1_ROUNDED_RECT { rect: r.inset(width / 2.0).d2d(), radiusX: radius, radiusY: radius };
            self.rt.DrawRoundedRectangle(&rr, self.brush, width, None);
        }
    }
    pub fn circle(&self, cx: f32, cy: f32, rad: f32, c: C) {
        self.set(c);
        unsafe {
            let e = D2D1_ELLIPSE { point: D2D_POINT_2F { x: cx, y: cy }, radiusX: rad, radiusY: rad };
            self.rt.FillEllipse(&e, self.brush);
        }
    }
    pub fn ring(&self, cx: f32, cy: f32, rad: f32, c: C, width: f32) {
        self.set(c);
        unsafe {
            let e = D2D1_ELLIPSE { point: D2D_POINT_2F { x: cx, y: cy }, radiusX: rad, radiusY: rad };
            self.rt.DrawEllipse(&e, self.brush, width, None);
        }
    }
    pub fn line(&self, x0: f32, y0: f32, x1: f32, y1: f32, c: C, width: f32) {
        self.set(c);
        unsafe {
            self.rt.DrawLine(
                D2D_POINT_2F { x: x0, y: y0 },
                D2D_POINT_2F { x: x1, y: y1 },
                self.brush,
                width,
                None,
            );
        }
    }
    pub fn text(&self, s: &str, fmt: &IDWriteTextFormat, r: R, c: C) {
        if s.is_empty() || r.w <= 1.0 {
            return;
        }
        self.set(c);
        let t = utf16(s);
        unsafe {
            self.rt.DrawText(
                &t,
                fmt,
                &r.d2d(),
                self.brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }
    pub fn bitmap(&self, b: &ID2D1Bitmap, r: R, opacity: f32) {
        unsafe {
            self.rt.DrawBitmap(
                b,
                Some(&r.d2d()),
                opacity * self.alpha,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                None,
            );
        }
    }

    // Simple vector glyphs (font-independent, crisp at any DPI).
    pub fn glyph_close(&self, cx: f32, cy: f32, s: f32, c: C, w: f32) {
        self.line(cx - s, cy - s, cx + s, cy + s, c, w);
        self.line(cx - s, cy + s, cx + s, cy - s, c, w);
    }
    pub fn glyph_minimize(&self, cx: f32, cy: f32, s: f32, c: C, w: f32) {
        self.line(cx - s, cy, cx + s, cy, c, w);
    }
    pub fn glyph_pin(&self, cx: f32, cy: f32, s: f32, c: C, w: f32, filled: bool) {
        // A push-pin: head, needle.
        let head = R::new(cx - s * 0.75, cy - s, s * 1.5, s * 1.1);
        if filled {
            self.fill(head, s * 0.3, c);
        } else {
            self.stroke(head, s * 0.3, c, w);
        }
        self.line(cx - s * 1.05, cy + s * 0.1, cx + s * 1.05, cy + s * 0.1, c, w);
        self.line(cx, cy + s * 0.1, cx, cy + s * 1.1, c, w);
    }
    pub fn glyph_search(&self, cx: f32, cy: f32, s: f32, c: C, w: f32) {
        self.ring(cx - s * 0.2, cy - s * 0.2, s * 0.65, c, w);
        self.line(cx + s * 0.28, cy + s * 0.28, cx + s * 0.85, cy + s * 0.85, c, w);
    }
    pub fn glyph_check(&self, cx: f32, cy: f32, s: f32, c: C, w: f32) {
        self.line(cx - s, cy, cx - s * 0.3, cy + s * 0.7, c, w);
        self.line(cx - s * 0.3, cy + s * 0.7, cx + s, cy - s * 0.6, c, w);
    }
}

pub fn make_brush(rt: &ID2D1RenderTarget) -> Result<ID2D1SolidColorBrush> {
    unsafe { rt.CreateSolidColorBrush(&C::hex(0).d2d(), None) }
}

pub fn as_rt(t: &ID2D1HwndRenderTarget) -> ID2D1RenderTarget {
    t.cast().expect("render target")
}
