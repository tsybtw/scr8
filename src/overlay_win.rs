//! Area picker for Windows, drawn with plain GDI.
//!
//! The frozen screen and a dimmed copy are prepared once per monitor; while
//! the mouse moves only the strip around the selection is repainted, by
//! copying from those two pictures through a back buffer. That keeps it
//! instant with or without a GPU, and cheap over Remote Desktop. It runs on
//! whatever thread calls [`pick`], inside the main app (no extra process).

use std::cell::RefCell;
use std::ptr::{null, null_mut};
use std::sync::Once;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetThreadDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, SetFocus, VK_ESCAPE, VK_RETURN,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::capture;
use crate::config::Region;
use crate::look::{self, Rgb};

/// Pointer travel (in points) before a press becomes a drag.
const DRAG_START: f32 = 4.0;

/// Shows the picker on every monitor and blocks until the user saves
/// (returns the area in physical pixels) or cancels.
pub fn pick(initial: Option<Region>) -> Option<Region> {
    unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    register_fonts();
    register_class();

    // Freeze every monitor before any of our windows exists.
    let monitors = monitors();
    let mut mons = Vec::new();
    for r in monitors {
        if let Some(m) = Mon::new(r) {
            mons.push(m);
        }
    }
    if mons.is_empty() {
        return None;
    }
    let sel = initial.map(|r| Rc::new(r.x, r.y, r.x + r.w as i32, r.y + r.h as i32));
    STATE.with_borrow_mut(|s| {
        *s = Some(Picker {
            mons,
            sel,
            drag: None,
            press: None,
            press_button: None,
            hover: None,
            result: None,
        })
    });

    // Create and show the windows outside any borrow: creation sends
    // messages straight to `wndproc`.
    let count = STATE.with_borrow(|s| s.as_ref().map_or(0, |p| p.mons.len()));
    for i in 0..count {
        let area = STATE.with_borrow(|s| s.as_ref().unwrap().mons[i].area);
        let hwnd = create_window(area);
        STATE.with_borrow_mut(|s| {
            let p = s.as_mut().unwrap();
            p.mons[i].hwnd = hwnd;
            if !hwnd.is_null() {
                p.mons[i].set_scale(unsafe { GetDpiForWindow(hwnd) } as f32 / 96.0);
            }
        });
    }
    let hwnds: Vec<HWND> =
        STATE.with_borrow(|s| s.as_ref().unwrap().mons.iter().map(|m| m.hwnd).collect());
    #[cfg(feature = "selftest")]
    {
        *OPEN.lock().unwrap() = hwnds.iter().map(|&h| h as isize).collect();
    }
    for &h in &hwnds {
        unsafe {
            ShowWindow(h, SW_SHOW);
            UpdateWindow(h);
        }
    }
    // Keyboard focus to the window under the pointer.
    let mut cursor = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut cursor) };
    let focus = STATE.with_borrow(|s| {
        let p = s.as_ref().unwrap();
        p.mons
            .iter()
            .find(|m| m.area.contains(cursor.x, cursor.y))
            .unwrap_or(&p.mons[0])
            .hwnd
    });
    force_foreground(focus);

    let mut msg: MSG = unsafe { std::mem::zeroed() };
    while unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) } > 0 {
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // Tear down outside the borrow: destroying sends messages too.
    #[cfg(feature = "selftest")]
    OPEN.lock().unwrap().clear();
    let picker = STATE.with_borrow_mut(|s| s.take());
    let picker = picker?;
    for m in &picker.mons {
        if !m.hwnd.is_null() {
            unsafe { DestroyWindow(m.hwnd) };
        }
    }
    picker.result.flatten()
}

// ---------------------------------------------------------------------------
// Geometry

/// Integer rectangle, right/bottom exclusive.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Rc {
    l: i32,
    t: i32,
    r: i32,
    b: i32,
}

impl Rc {
    fn new(l: i32, t: i32, r: i32, b: i32) -> Self {
        Self { l, t, r, b }
    }
    fn sized(l: i32, t: i32, w: i32, h: i32) -> Self {
        Self::new(l, t, l + w, t + h)
    }
    fn w(&self) -> i32 {
        self.r - self.l
    }
    fn h(&self) -> i32 {
        self.b - self.t
    }
    fn is_empty(&self) -> bool {
        self.w() <= 0 || self.h() <= 0
    }
    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.l && x < self.r && y >= self.t && y < self.b
    }
    fn offset(&self, dx: i32, dy: i32) -> Self {
        Self::new(self.l + dx, self.t + dy, self.r + dx, self.b + dy)
    }
    fn expand(&self, d: i32) -> Self {
        Self::new(self.l - d, self.t - d, self.r + d, self.b + d)
    }
    fn intersect(&self, o: &Rc) -> Rc {
        Rc::new(
            self.l.max(o.l),
            self.t.max(o.t),
            self.r.min(o.r),
            self.b.min(o.b),
        )
    }
    fn from_points(a: (i32, i32), b: (i32, i32)) -> Self {
        Self::new(a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1))
    }
    fn to_win(self) -> RECT {
        RECT {
            left: self.l,
            top: self.t,
            right: self.r,
            bottom: self.b,
        }
    }
}

#[derive(Clone, Copy)]
enum Drag {
    New((i32, i32)),
    /// Offset from the pointer to the selection's top-left corner.
    Move((i32, i32)),
    /// Which edges follow the pointer: (left, top, right, bottom).
    Resize(bool, bool, bool, bool),
}

#[derive(Clone, Copy, PartialEq)]
enum Button {
    Save,
    Cancel,
}

/// Returns `Some(edges)` when `p` is on the selection border, `None` inside.
fn edge_hit(s: Rc, x: i32, y: i32, grab: i32) -> Option<(bool, bool, bool, bool)> {
    let near = |v: i32, e: i32| (v - e).abs() <= grab;
    let l = near(x, s.l);
    let r = near(x, s.r);
    let t = near(y, s.t);
    let b = near(y, s.b);
    (l || r || t || b).then_some((l, t, r && !l, b && !t))
}

fn cursor_for(l: bool, t: bool, r: bool, b: bool) -> *const u16 {
    match (l || r, t || b) {
        (true, true) if (l && t) || (r && b) => IDC_SIZENWSE,
        (true, true) => IDC_SIZENESW,
        (true, false) => IDC_SIZEWE,
        _ => IDC_SIZENS,
    }
}

// ---------------------------------------------------------------------------
// GDI helpers

/// A 32-bit top-down DIB selected into its own memory DC.
struct Dib {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    w: i32,
    h: i32,
}

impl Dib {
    fn new(w: i32, h: i32) -> Option<Self> {
        unsafe {
            let screen = GetDC(null_mut());
            let dc = CreateCompatibleDC(screen);
            ReleaseDC(null_mut(), screen);
            if dc.is_null() {
                return None;
            }
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = w;
            info.bmiHeader.biHeight = -h;
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            info.bmiHeader.biCompression = BI_RGB;
            let mut bits = null_mut();
            let bmp = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if bmp.is_null() || bits.is_null() {
                DeleteDC(dc);
                return None;
            }
            let old = SelectObject(dc, bmp);
            Some(Self {
                dc,
                bmp,
                old,
                bits: bits as *mut u8,
                w,
                h,
            })
        }
    }

    fn pixels(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.w * self.h * 4) as usize) }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bmp);
            DeleteDC(self.dc);
        }
    }
}

fn rgb(c: Rgb) -> u32 {
    c.0 as u32 | (c.1 as u32) << 8 | (c.2 as u32) << 16
}

fn fill(dc: HDC, r: Rc, c: Rgb) {
    if r.is_empty() {
        return;
    }
    unsafe {
        let brush = CreateSolidBrush(rgb(c));
        FillRect(dc, &r.to_win(), brush);
        DeleteObject(brush);
    }
}

/// Anti-aliased rounded rectangle, blended with `alpha` over what's there.
fn fill_round(dc: HDC, r: Rc, radius: f32, c: Rgb, alpha: u8) {
    if r.is_empty() {
        return;
    }
    let Some(mut d) = Dib::new(r.w(), r.h()) else {
        return;
    };
    let (w, h) = (r.w() as f32, r.h() as f32);
    let rad = radius.min(w / 2.0).min(h / 2.0);
    let stride = r.w() as usize * 4;
    let px = d.pixels();
    for y in 0..r.h() as usize {
        for x in 0..r.w() as usize {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let cov = if rad < 0.5 {
                1.0
            } else {
                let cx = fx.clamp(rad, w - rad);
                let cy = fy.clamp(rad, h - rad);
                let dist = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
                (rad + 0.5 - dist).clamp(0.0, 1.0)
            };
            let a = (alpha as f32 * cov).round() as u32;
            // Premultiplied BGRA.
            let i = y * stride + x * 4;
            px[i] = (c.2 as u32 * a / 255) as u8;
            px[i + 1] = (c.1 as u32 * a / 255) as u8;
            px[i + 2] = (c.0 as u32 * a / 255) as u8;
            px[i + 3] = a as u8;
        }
    }
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    unsafe {
        AlphaBlend(dc, r.l, r.t, r.w(), r.h(), d.dc, 0, 0, r.w(), r.h(), blend);
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn register_fonts() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        for data in [
            epaint_default_fonts::UBUNTU_LIGHT,
            epaint_default_fonts::NOTO_EMOJI_REGULAR,
        ] {
            // Windows writes the number of fonts added here, although the
            // binding declares the pointer const.
            let mut count = 0u32;
            let count_ptr = &mut count as *mut u32 as *const u32;
            unsafe {
                AddFontMemResourceEx(data.as_ptr() as _, data.len() as u32, null(), count_ptr);
            }
        }
    });
}

fn create_font(face: &str, px: i32) -> HFONT {
    let name: Vec<u16> = face.encode_utf16().chain(Some(0)).collect();
    unsafe {
        CreateFontW(
            -px,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_TT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32,
            DEFAULT_PITCH as u32,
            name.as_ptr(),
        )
    }
}

/// Text plus an icon glyph from the emoji font, as egui draws it.
struct Fonts {
    text: HFONT,
    icon: HFONT,
    ascent: i32,
    height: i32,
}

impl Fonts {
    fn new(dc: HDC, px: i32) -> Self {
        let text = create_font("Ubuntu Light", px);
        let icon = create_font("Noto Emoji", px);
        let mut tm: TEXTMETRICW = unsafe { std::mem::zeroed() };
        unsafe {
            SelectObject(dc, text);
            GetTextMetricsW(dc, &mut tm);
        }
        Self {
            text,
            icon,
            ascent: tm.tmAscent,
            height: tm.tmHeight,
        }
    }

    fn width(&self, dc: HDC, font: HFONT, s: &str) -> i32 {
        let w = wide(s);
        let mut size = windows_sys::Win32::Foundation::SIZE { cx: 0, cy: 0 };
        unsafe {
            SelectObject(dc, font);
            GetTextExtentPoint32W(dc, w.as_ptr(), w.len() as i32, &mut size);
        }
        size.cx
    }

    /// Width of `icon + " " + label` (icon may be empty).
    fn run_width(&self, dc: HDC, icon: &str, label: &str) -> i32 {
        let label = if icon.is_empty() {
            label.to_owned()
        } else {
            format!(" {label}")
        };
        let iw = if icon.is_empty() {
            0
        } else {
            self.width(dc, self.icon, icon)
        };
        iw + self.width(dc, self.text, &label)
    }

    fn draw_run(&self, dc: HDC, x: i32, top: i32, icon: &str, label: &str, c: Rgb) {
        let baseline = top + self.ascent;
        let label = if icon.is_empty() {
            label.to_owned()
        } else {
            format!(" {label}")
        };
        unsafe {
            SetBkMode(dc, TRANSPARENT as _);
            SetTextColor(dc, rgb(c));
            SetTextAlign(dc, TA_BASELINE);
            let mut x = x;
            if !icon.is_empty() {
                let w = wide(icon);
                SelectObject(dc, self.icon);
                TextOutW(dc, x, baseline, w.as_ptr(), w.len() as i32);
                x += self.width(dc, self.icon, icon);
            }
            let w = wide(&label);
            SelectObject(dc, self.text);
            TextOutW(dc, x, baseline, w.as_ptr(), w.len() as i32);
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.text);
            DeleteObject(self.icon);
        }
    }
}

// ---------------------------------------------------------------------------
// Monitors

fn monitors() -> Vec<Rc> {
    unsafe extern "system" fn each(mon: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> i32 {
        let out = unsafe { &mut *(data as *mut Vec<Rc>) };
        let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
        info.cbSize = size_of::<MONITORINFO>() as u32;
        if unsafe { GetMonitorInfoW(mon, &mut info) } != 0 {
            let r = info.rcMonitor;
            out.push(Rc::new(r.left, r.top, r.right, r.bottom));
        }
        1
    }
    let mut out: Vec<Rc> = Vec::new();
    unsafe {
        EnumDisplayMonitors(null_mut(), null(), Some(each), &mut out as *mut _ as LPARAM);
    }
    out
}

/// One monitor: its window and the pictures it's drawn from.
struct Mon {
    /// Physical pixels in virtual-desktop coordinates.
    area: Rc,
    hwnd: HWND,
    bright: Dib,
    dim: Dib,
    back: Dib,
    scale: f32,
    badge: Fonts,
    button: Fonts,
}

impl Mon {
    fn new(area: Rc) -> Option<Self> {
        let region = Region {
            x: area.l,
            y: area.t,
            w: area.w() as u32,
            h: area.h() as u32,
        };
        let frame = capture::capture(region).ok()?;
        let (w, h) = (frame.width as i32, frame.height as i32);
        let mut bright = Dib::new(w, h)?;
        let mut dim = Dib::new(w, h)?;
        let back = Dib::new(w, h)?;
        bright.pixels().copy_from_slice(&frame.bgrx);
        let keep = 255 - look::DIM_ALPHA as u32;
        for (d, s) in dim.pixels().iter_mut().zip(&frame.bgrx) {
            *d = (*s as u32 * keep / 255) as u8;
        }
        let badge = Fonts::new(back.dc, look::BADGE_FONT as i32);
        let button = Fonts::new(back.dc, look::BUTTON_FONT as i32);
        Some(Self {
            area,
            hwnd: null_mut(),
            bright,
            dim,
            back,
            scale: 1.0,
            badge,
            button,
        })
    }

    fn set_scale(&mut self, scale: f32) {
        self.scale = scale.max(0.5);
        self.badge = Fonts::new(self.back.dc, self.px(look::BADGE_FONT));
        self.button = Fonts::new(self.back.dc, self.px(look::BUTTON_FONT));
    }

    /// Points to device pixels on this monitor.
    fn px(&self, pt: f32) -> i32 {
        (pt * self.scale).round() as i32
    }

    fn size(&self) -> Rc {
        Rc::new(0, 0, self.area.w(), self.area.h())
    }
}

/// Where the decorations of the current selection go on one monitor.
struct Layout {
    sel: Option<Rc>,
    badge: Option<(Rc, String)>,
    panel: Option<Rc>,
    save: Rc,
    cancel: Rc,
    hint: Option<Rc>,
}

// ---------------------------------------------------------------------------
// Picker state

struct Picker {
    mons: Vec<Mon>,
    /// Selection in physical virtual-desktop pixels.
    sel: Option<Rc>,
    drag: Option<Drag>,
    /// Where the button went down, until released.
    press: Option<(i32, i32)>,
    press_button: Option<Button>,
    hover: Option<Button>,
    /// Set when done: `Some(None)` means cancelled.
    result: Option<Option<Region>>,
}

thread_local! {
    static STATE: RefCell<Option<Picker>> = const { RefCell::new(None) };
}

/// The open picker's windows (as integers, to share across threads), so
/// another thread can close it (used by the self-test).
#[cfg(feature = "selftest")]
static OPEN: std::sync::Mutex<Vec<isize>> = std::sync::Mutex::new(Vec::new());

/// Closes an open picker as if Esc was pressed.
#[cfg(feature = "selftest")]
pub fn cancel() {
    if let Some(&h) = OPEN.lock().unwrap().first() {
        unsafe { PostMessageW(h as HWND, WM_CLOSE, 0, 0) };
    }
}

impl Picker {
    fn layout(&self, i: usize) -> Layout {
        let m = &self.mons[i];
        let dc = m.back.dc;
        let screen = m.size();
        let sel = self
            .sel
            .map(|s| s.offset(-m.area.l, -m.area.t))
            .filter(|s| !s.intersect(&screen).is_empty());
        let mut out = Layout {
            sel,
            badge: None,
            panel: None,
            save: Rc::new(0, 0, 0, 0),
            cancel: Rc::new(0, 0, 0, 0),
            hint: None,
        };
        let (bpx, bpy) = (m.px(look::BADGE_PAD.0), m.px(look::BADGE_PAD.1));
        match (sel, self.sel) {
            (Some(s), Some(global)) => {
                let text = format!("{} × {}", global.w(), global.h());
                let bw = m.badge.width(dc, m.badge.text, &text) + 2 * bpx;
                let bh = m.badge.height + 2 * bpy;
                let gap = m.px(look::BADGE_GAP);
                let badge = if s.t - gap - bh >= 0 {
                    Rc::sized(s.l, s.t - gap - bh, bw, bh)
                } else {
                    Rc::sized(s.l + gap, s.t + gap, bw, bh)
                };
                out.badge = Some((badge, text));

                if self.drag.is_none() {
                    let (px_, py_) = (m.px(look::BUTTON_PAD.0), m.px(look::BUTTON_PAD.1));
                    let f = &m.button;
                    let save_w = f.run_width(dc, look::SAVE_ICON, look::SAVE_LABEL) + 2 * px_;
                    let cancel_w = f.run_width(dc, look::CANCEL_ICON, look::CANCEL_LABEL) + 2 * px_;
                    let btn_h = f.height + 2 * py_;
                    let pad = m.px(look::PANEL_PAD);
                    let bgap = m.px(look::BUTTON_GAP);
                    let pw = 2 * pad + save_w + bgap + cancel_w;
                    let ph = 2 * pad + btn_h;
                    let gap = m.px(look::PANEL_GAP);
                    let y = if s.b + gap + ph <= screen.b {
                        s.b + gap
                    } else {
                        s.b - gap - ph
                    };
                    let right = s.r.min(screen.r - gap).max(pw + gap);
                    let panel = Rc::sized(right - pw, y, pw, ph);
                    out.save = Rc::sized(panel.l + pad, panel.t + pad, save_w, btn_h);
                    out.cancel = Rc::sized(out.save.r + bgap, panel.t + pad, cancel_w, btn_h);
                    out.panel = Some(panel);
                }
            }
            _ if self.sel.is_none() => {
                let w = m.badge.width(dc, m.badge.text, look::HINT) + 2 * bpx;
                let h = m.badge.height + 2 * bpy;
                out.hint = Some(Rc::sized((screen.w() - w) / 2, m.px(look::HINT_TOP), w, h));
            }
            _ => {}
        }
        out
    }

    /// Everything the decorations cover on monitor `i`, for repainting.
    fn decor(&self, i: usize) -> Vec<Rc> {
        let m = &self.mons[i];
        let l = self.layout(i);
        let mut v = Vec::new();
        if let Some(s) = l.sel {
            let edge = m.px(look::HANDLE / 2.0).max(m.px(look::BORDER)) + 2;
            // The border strips only; the inside doesn't change on its own.
            v.push(Rc::new(s.l - edge, s.t - edge, s.r + edge, s.t + edge));
            v.push(Rc::new(s.l - edge, s.b - edge, s.r + edge, s.b + edge));
            v.push(Rc::new(s.l - edge, s.t - edge, s.l + edge, s.b + edge));
            v.push(Rc::new(s.r - edge, s.t - edge, s.r + edge, s.b + edge));
        }
        if let Some((b, _)) = &l.badge {
            v.push(b.expand(1));
        }
        if let Some(p) = l.panel {
            // Includes the drop shadow.
            v.push(Rc::new(p.l - 2, p.t - 2, p.r + m.px(8.0), p.b + m.px(10.0)));
        }
        if let Some(h) = l.hint {
            v.push(h.expand(1));
        }
        v
    }

    /// Runs `change`, then repaints what it moved on every monitor.
    fn update(&mut self, change: impl FnOnce(&mut Self)) {
        let old_sel = self.sel;
        let before: Vec<Vec<Rc>> = (0..self.mons.len()).map(|i| self.decor(i)).collect();
        change(self);
        let after: Vec<Vec<Rc>> = (0..self.mons.len()).map(|i| self.decor(i)).collect();
        for (i, m) in self.mons.iter().enumerate() {
            let mut dirty: Vec<Rc> = before[i].iter().chain(&after[i]).copied().collect();
            // The part of the screen that changes between bright and dim.
            if old_sel != self.sel {
                for s in [old_sel, self.sel].into_iter().flatten() {
                    dirty.push(s.offset(-m.area.l, -m.area.t));
                }
            }
            for r in dirty {
                let r = r.intersect(&m.size());
                if !r.is_empty() {
                    unsafe { InvalidateRect(m.hwnd, &r.to_win(), 0) };
                }
            }
        }
    }

    /// Composes `clip` in the back buffer and copies it to `target`.
    fn paint(&self, i: usize, clip: Rc, target: HDC) {
        let m = &self.mons[i];
        let clip = clip.intersect(&m.size());
        if clip.is_empty() {
            return;
        }
        let dc = m.back.dc;
        let copy = |src: &Dib, r: Rc| unsafe {
            BitBlt(dc, r.l, r.t, r.w(), r.h(), src.dc, r.l, r.t, SRCCOPY);
        };
        copy(&m.dim, clip);
        let l = self.layout(i);
        if let Some(s) = l.sel {
            copy(&m.bright, s.intersect(&clip));
            // Border outside the selection: a solid pixel, then the
            // fractional rest blended, like egui's 1.5-point stroke.
            let width = look::BORDER * m.scale;
            let solid = width.floor().max(1.0) as i32;
            let rest = ((width - solid as f32).clamp(0.0, 1.0) * 255.0) as u8;
            let ring = |d: i32, c: &dyn Fn(Rc)| {
                c(Rc::new(s.l - d, s.t - d, s.r + d, s.t));
                c(Rc::new(s.l - d, s.b, s.r + d, s.b + d));
                c(Rc::new(s.l - d, s.t, s.l, s.b));
                c(Rc::new(s.r, s.t, s.r + d, s.b));
            };
            ring(solid, &|r| fill(dc, r, look::ACCENT));
            if rest > 0 {
                let outer = |r: Rc| fill_round(dc, r, 0.0, look::ACCENT, rest);
                outer(Rc::new(
                    s.l - solid - 1,
                    s.t - solid - 1,
                    s.r + solid + 1,
                    s.t - solid,
                ));
                outer(Rc::new(
                    s.l - solid - 1,
                    s.b + solid,
                    s.r + solid + 1,
                    s.b + solid + 1,
                ));
                outer(Rc::new(
                    s.l - solid - 1,
                    s.t - solid,
                    s.l - solid,
                    s.b + solid,
                ));
                outer(Rc::new(
                    s.r + solid,
                    s.t - solid,
                    s.r + solid + 1,
                    s.b + solid,
                ));
            }
            let hs = m.px(look::HANDLE);
            for (x, y) in [(s.l, s.t), (s.r, s.t), (s.l, s.b), (s.r, s.b)] {
                fill_round(
                    dc,
                    Rc::sized(x - hs / 2, y - hs / 2, hs, hs),
                    m.scale,
                    look::ACCENT,
                    255,
                );
            }
        }
        if let Some((b, text)) = &l.badge {
            self.draw_badge(m, *b, text);
        }
        if let Some(h) = l.hint {
            self.draw_badge(m, h, look::HINT);
        }
        if let Some(p) = l.panel {
            let r = m.px(look::PANEL_RADIUS) as f32;
            // Soft drop shadow, then border and fill.
            fill_round(
                dc,
                p.offset(m.px(3.0), m.px(5.0)).expand(m.px(2.0)),
                r + 2.0,
                (0, 0, 0),
                45,
            );
            fill_round(dc, p, r, look::PANEL_STROKE, 255);
            fill_round(dc, p.expand(-1), r - 1.0, look::PANEL_FILL, 255);
            for (b, button, icon, label) in [
                (l.save, Button::Save, look::SAVE_ICON, look::SAVE_LABEL),
                (
                    l.cancel,
                    Button::Cancel,
                    look::CANCEL_ICON,
                    look::CANCEL_LABEL,
                ),
            ] {
                let hot = self.hover == Some(button);
                let (bg, fg) = if hot {
                    (look::BUTTON_FILL_HOVER, look::BUTTON_TEXT_HOVER)
                } else {
                    (look::BUTTON_FILL, look::BUTTON_TEXT)
                };
                fill_round(dc, b, m.px(look::BUTTON_RADIUS) as f32, bg, 255);
                let f = &m.button;
                let x = b.l + m.px(look::BUTTON_PAD.0);
                let y = b.t + (b.h() - f.height) / 2;
                f.draw_run(dc, x, y, icon, label, fg);
            }
        }
        unsafe {
            BitBlt(
                target,
                clip.l,
                clip.t,
                clip.w(),
                clip.h(),
                dc,
                clip.l,
                clip.t,
                SRCCOPY,
            );
        }
    }

    fn draw_badge(&self, m: &Mon, r: Rc, text: &str) {
        let dc = m.back.dc;
        fill_round(
            dc,
            r,
            m.px(look::BADGE_RADIUS) as f32,
            (0, 0, 0),
            look::BADGE_ALPHA,
        );
        m.badge.draw_run(
            dc,
            r.l + m.px(look::BADGE_PAD.0),
            r.t + m.px(look::BADGE_PAD.1),
            "",
            text,
            (255, 255, 255),
        );
    }

    fn button_at(&self, i: usize, x: i32, y: i32) -> Option<Button> {
        let l = self.layout(i);
        l.panel?;
        if l.save.contains(x, y) {
            Some(Button::Save)
        } else if l.cancel.contains(x, y) {
            Some(Button::Cancel)
        } else {
            None
        }
    }

    fn apply_drag(&mut self, d: Drag, x: i32, y: i32) {
        match d {
            Drag::New(start) => self.sel = Some(Rc::from_points(start, (x, y))),
            Drag::Move((dx, dy)) => {
                if let Some(s) = self.sel.as_mut() {
                    *s = Rc::sized(x + dx, y + dy, s.w(), s.h());
                }
            }
            Drag::Resize(l, t, r, b) => {
                if let Some(s) = self.sel.as_mut() {
                    if l {
                        s.l = x;
                    }
                    if t {
                        s.t = y;
                    }
                    if r {
                        s.r = x;
                    }
                    if b {
                        s.b = y;
                    }
                    *s = Rc::from_points((s.l, s.t), (s.r, s.b));
                }
            }
        }
    }

    fn finish(&mut self, save: bool) {
        if save && self.sel.is_none() {
            return;
        }
        self.result = Some(if save {
            self.sel.map(|s| Region {
                x: s.l,
                y: s.t,
                w: s.w().max(1) as u32,
                h: s.h().max(1) as u32,
            })
        } else {
            None
        });
        unsafe { PostQuitMessage(0) };
    }

    fn mon_of(&self, hwnd: HWND) -> Option<usize> {
        self.mons.iter().position(|m| m.hwnd == hwnd)
    }

    fn cursor(&self, i: usize, gx: i32, gy: i32) -> *const u16 {
        let m = &self.mons[i];
        let (lx, ly) = (gx - m.area.l, gy - m.area.t);
        if self.press_button.is_some() || self.button_at(i, lx, ly).is_some() {
            return IDC_ARROW;
        }
        match self.drag {
            Some(Drag::Move(_)) => return IDC_SIZEALL,
            Some(Drag::Resize(l, t, r, b)) => return cursor_for(l, t, r, b),
            Some(Drag::New(_)) => return IDC_CROSS,
            None => {}
        }
        if let Some(s) = self.sel {
            let grab = m.px(look::HANDLE);
            if s.expand(grab).contains(gx, gy) {
                if let Some((l, t, r, b)) = edge_hit(s, gx, gy, grab) {
                    return cursor_for(l, t, r, b);
                }
                if s.contains(gx, gy) {
                    return IDC_SIZEALL;
                }
            }
        }
        IDC_CROSS
    }
}

// ---------------------------------------------------------------------------
// Window

const CLASS: &str = "scr8-picker";

fn register_class() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let name: Vec<u16> = CLASS.encode_utf16().chain(Some(0)).collect();
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: GetModuleHandleW(null()),
            lpszClassName: name.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassExW(&class);
        // The class keeps a pointer to the name.
        std::mem::forget(name);
    });
}

fn create_window(area: Rc) -> HWND {
    let class: Vec<u16> = CLASS.encode_utf16().chain(Some(0)).collect();
    let title: Vec<u16> = "scr8 — select area".encode_utf16().chain(Some(0)).collect();
    unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            area.l,
            area.t,
            area.w(),
            area.h(),
            null_mut(),
            null_mut(),
            GetModuleHandleW(null()),
            null(),
        )
    }
}

/// Brings `hwnd` to the front with keyboard focus. Windows only allows that
/// to the app the user last interacted with, so first "press" Alt, which
/// lifts the lock (the same trick winit uses for new windows).
fn force_foreground(hwnd: HWND) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
        MAPVK_VK_TO_VSC, MapVirtualKeyW, SendInput, VK_LMENU, VK_MENU,
    };
    let scan = unsafe { MapVirtualKeyW(VK_MENU as u32, MAPVK_VK_TO_VSC) } as u16;
    let key = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_LMENU,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [
        key(KEYEVENTF_EXTENDEDKEY),
        key(KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP),
    ];
    unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        );
        SetForegroundWindow(hwnd);
        SetFocus(hwnd);
    }
}

fn point(lparam: LPARAM) -> (i32, i32) {
    (
        (lparam & 0xFFFF) as i16 as i32,
        ((lparam >> 16) & 0xFFFF) as i16 as i32,
    )
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let handled = STATE.with(|cell| {
        let Ok(mut guard) = cell.try_borrow_mut() else {
            return None;
        };
        let p = guard.as_mut()?;
        let i = p.mon_of(hwnd)?;
        let origin = (p.mons[i].area.l, p.mons[i].area.t);
        let global = |lp: LPARAM| {
            let (x, y) = point(lp);
            (x + origin.0, y + origin.1)
        };
        match msg {
            WM_PAINT => {
                let mut ps: PAINTSTRUCT = unsafe { std::mem::zeroed() };
                unsafe { BeginPaint(hwnd, &mut ps) };
                let r = ps.rcPaint;
                p.paint(i, Rc::new(r.left, r.top, r.right, r.bottom), ps.hdc);
                unsafe { EndPaint(hwnd, &ps) };
                Some(0)
            }
            WM_ERASEBKGND => Some(1),
            WM_SETCURSOR => {
                if (lparam & 0xFFFF) as u32 != HTCLIENT {
                    return None;
                }
                let mut c = POINT { x: 0, y: 0 };
                unsafe { GetCursorPos(&mut c) };
                let id = p.cursor(i, c.x, c.y);
                unsafe { SetCursor(LoadCursorW(null_mut(), id)) };
                Some(1)
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                let (gx, gy) = global(lparam);
                let (lx, ly) = point(lparam);
                if msg == WM_LBUTTONDBLCLK && p.sel.is_some_and(|s| s.contains(gx, gy)) {
                    p.finish(true);
                    return Some(0);
                }
                unsafe { SetCapture(hwnd) };
                match p.button_at(i, lx, ly) {
                    Some(b) => p.press_button = Some(b),
                    None => p.press = Some((gx, gy)),
                }
                Some(0)
            }
            WM_MOUSEMOVE => {
                let (gx, gy) = global(lparam);
                let (lx, ly) = point(lparam);
                let hover = if p.drag.is_none() {
                    p.button_at(i, lx, ly)
                } else {
                    None
                };
                let threshold = p.mons[i].px(DRAG_START);
                let start = p.press.filter(|&(sx, sy)| {
                    p.drag.is_none()
                        && ((gx - sx).pow(2) + (gy - sy).pow(2)) as f32
                            > (threshold * threshold) as f32
                });
                if hover != p.hover || start.is_some() || p.drag.is_some() {
                    p.update(|p| {
                        p.hover = hover;
                        if let Some((sx, sy)) = start {
                            // Decide what the drag does from where the button went down.
                            let grab = p.mons[i].px(look::HANDLE);
                            p.drag = Some(match p.sel {
                                Some(s) if s.expand(grab).contains(sx, sy) => {
                                    match edge_hit(s, sx, sy, grab) {
                                        Some((l, t, r, b)) => Drag::Resize(l, t, r, b),
                                        None if s.contains(sx, sy) => {
                                            Drag::Move((s.l - sx, s.t - sy))
                                        }
                                        None => Drag::New((sx, sy)),
                                    }
                                }
                                _ => Drag::New((sx, sy)),
                            });
                        }
                        if let Some(d) = p.drag {
                            p.apply_drag(d, gx, gy);
                        }
                    });
                }
                Some(0)
            }
            WM_LBUTTONUP => {
                let (gx, gy) = global(lparam);
                let (lx, ly) = point(lparam);
                // Releasing capture sends a message to this window; our
                // handler just falls through to the default one then.
                unsafe { ReleaseCapture() };
                if let Some(b) = p.press_button.take() {
                    if p.button_at(i, lx, ly) == Some(b) {
                        p.finish(b == Button::Save);
                    }
                    return Some(0);
                }
                p.update(|p| {
                    if let Some(d) = p.drag.take() {
                        p.apply_drag(d, gx, gy);
                        if p.sel.is_some_and(|s| s.w() < 3 || s.h() < 3) {
                            p.sel = None;
                        }
                    }
                    p.press = None;
                });
                Some(0)
            }
            WM_KEYDOWN => {
                match wparam as u16 {
                    VK_RETURN => p.finish(true),
                    VK_ESCAPE => p.finish(false),
                    _ => {}
                }
                Some(0)
            }
            WM_CLOSE => {
                p.finish(false);
                Some(0)
            }
            _ => None,
        }
    });
    match handled {
        Some(r) => r,
        None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
