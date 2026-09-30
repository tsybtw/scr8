use std::cell::RefCell;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::*;

use super::Frame;
use crate::config::Region;

/// A reusable GDI target. Creating a DIB section for a large area costs
/// milliseconds, so each capture thread keeps the last few alive.
struct Target {
    w: i32,
    h: i32,
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
}

impl Drop for Target {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

thread_local! {
    static TARGETS: RefCell<Vec<Target>> = const { RefCell::new(Vec::new()) };
}

const MAX_CACHED: usize = 4;

pub fn ensure_permission() -> bool {
    true
}

/// Desktop Duplication when available, otherwise GDI.
pub fn capture(r: Region) -> Result<Frame, String> {
    if r.w == 0 || r.h == 0 {
        return Err("empty region".into());
    }
    match super::dda::capture(r) {
        Some(frame) => Ok(frame),
        None => capture_gdi(r),
    }
}

/// GDI capture: works everywhere (Remote Desktop, no GPU), but slower.
pub fn capture_gdi(r: Region) -> Result<Frame, String> {
    let (w, h) = (r.w as i32, r.h as i32);
    if w <= 0 || h <= 0 {
        return Err("empty region".into());
    }
    TARGETS.with_borrow_mut(|targets| unsafe {
        let screen = GetDC(null_mut::<core::ffi::c_void>() as HWND);
        if screen.is_null() {
            return Err("GetDC failed".into());
        }
        let result = (|| {
            let idx = match targets.iter().position(|t| t.w == w && t.h == h) {
                Some(i) => i,
                None => {
                    if targets.len() >= MAX_CACHED {
                        targets.remove(0);
                    }
                    targets.push(new_target(screen, w, h)?);
                    targets.len() - 1
                }
            };
            let t = &targets[idx];
            let len = w as usize * h as usize * 4;
            // Pixels the screen DC can't supply (off-monitor) are left
            // untouched by BitBlt, so clear the reused buffer to black first.
            std::ptr::write_bytes(t.bits, 0, len);
            if BitBlt(t.dc, 0, 0, w, h, screen, r.x, r.y, SRCCOPY) == 0 {
                return Err("BitBlt failed".to_string());
            }
            GdiFlush();
            let bgrx = std::slice::from_raw_parts(t.bits, len).to_vec();
            Ok(Frame {
                width: w as u32,
                height: h as u32,
                bgrx,
            })
        })();
        ReleaseDC(null_mut::<core::ffi::c_void>() as HWND, screen);
        result
    })
}

unsafe fn new_target(screen: HDC, w: i32, h: i32) -> Result<Target, String> {
    unsafe {
        let dc = CreateCompatibleDC(screen);
        if dc.is_null() {
            return Err("CreateCompatibleDC failed".into());
        }
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = w;
        info.bmiHeader.biHeight = -h; // top-down rows
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;
        let mut bits: *mut core::ffi::c_void = null_mut();
        let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if bitmap.is_null() || bits.is_null() {
            DeleteDC(dc);
            return Err(format!("CreateDIBSection {w}x{h} failed"));
        }
        let old = SelectObject(dc, bitmap);
        Ok(Target {
            w,
            h,
            dc,
            bitmap,
            old,
            bits: bits as *mut u8,
        })
    }
}
