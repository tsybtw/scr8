//! Screen capture on macOS 11+, Intel and Apple Silicon.
//!
//! macOS 15.2+ uses ScreenCaptureKit, Apple's current capture API; older
//! systems, and any ScreenCaptureKit failure, use CoreGraphics. Region
//! coordinates are global points (origin at the top-left of the main
//! display); output is in native pixels (2x on Retina).

use std::ffi::c_void;

use super::Frame;
use crate::config::Region;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

// So they can be passed to Objective-C methods.
unsafe impl objc2::Encode for CGPoint {
    const ENCODING: objc2::Encoding =
        objc2::Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
}
unsafe impl objc2::Encode for CGSize {
    const ENCODING: objc2::Encoding =
        objc2::Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
}
unsafe impl objc2::Encode for CGRect {
    const ENCODING: objc2::Encoding =
        objc2::Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]);
}

impl CGRect {
    fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self {
            origin: CGPoint { x, y },
            size: CGSize {
                width: w,
                height: h,
            },
        }
    }

    fn intersect(&self, o: &CGRect) -> Option<CGRect> {
        let x0 = self.origin.x.max(o.origin.x);
        let y0 = self.origin.y.max(o.origin.y);
        let x1 = (self.origin.x + self.size.width).min(o.origin.x + o.size.width);
        let y1 = (self.origin.y + self.size.height).min(o.origin.y + o.size.height);
        (x1 > x0 && y1 > y0).then(|| CGRect::new(x0, y0, x1 - x0, y1 - y0))
    }
}

type CGImageRef = *mut c_void;
type CGContextRef = *mut c_void;
type CGColorSpaceRef = *mut c_void;
type CGDisplayModeRef = *mut c_void;
type CFDataRef = *const c_void;

const K_CG_IMAGE_ALPHA_NONE_SKIP_FIRST: u32 = 6;
const K_CG_IMAGE_ALPHA_PREMULTIPLIED_FIRST: u32 = 2;
const K_CG_BITMAP_ALPHA_INFO_MASK: u32 = 0x1F;
const K_CG_BITMAP_BYTE_ORDER_MASK: u32 = 0x7000;
const K_CG_BITMAP_BYTE_ORDER_32_LITTLE: u32 = 2 << 12;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayBounds(display: u32) -> CGRect;
    #[cfg(feature = "selftest")]
    fn CGMainDisplayID() -> u32;
    fn CGDisplayCopyDisplayMode(display: u32) -> CGDisplayModeRef;
    fn CGDisplayModeGetPixelWidth(mode: CGDisplayModeRef) -> usize;
    fn CGDisplayModeRelease(mode: CGDisplayModeRef);
    fn CGDisplayCreateImageForRect(display: u32, rect: CGRect) -> CGImageRef;
    fn CGImageGetWidth(image: CGImageRef) -> usize;
    fn CGImageGetHeight(image: CGImageRef) -> usize;
    fn CGImageGetBitsPerPixel(image: CGImageRef) -> usize;
    fn CGImageGetBytesPerRow(image: CGImageRef) -> usize;
    fn CGImageGetBitmapInfo(image: CGImageRef) -> u32;
    fn CGImageGetDataProvider(image: CGImageRef) -> *mut c_void;
    fn CGImageGetColorSpace(image: CGImageRef) -> CGColorSpaceRef;
    fn CGImageRelease(image: CGImageRef);
    fn CGDataProviderCopyData(provider: *mut c_void) -> CFDataRef;
    fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
    fn CGColorSpaceRelease(space: CGColorSpaceRef);
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: CGColorSpaceRef,
        bitmap_info: u32,
    ) -> CGContextRef;
    fn CGContextDrawImage(ctx: CGContextRef, rect: CGRect, image: CGImageRef);
    fn CGContextRelease(ctx: CGContextRef);
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDataGetBytePtr(data: CFDataRef) -> *const u8;
    fn CFDataGetLength(data: CFDataRef) -> isize;
    fn CFRelease(cf: *const c_void);
    fn CFRetain(cf: *const c_void) -> *const c_void;
}

/// ScreenCaptureKit's display-agnostic screenshot of a rectangle
/// (macOS 15.2+). The framework is loaded at runtime, so the app still
/// starts on systems that don't have it.
mod sck {
    use std::ffi::{CStr, c_char, c_int, c_void};
    use std::sync::{Mutex, OnceLock, mpsc};
    use std::time::Duration;

    use block2::RcBlock;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::{msg_send, sel};

    use super::{CFRetain, CGImageRef, CGRect};

    unsafe extern "C" {
        fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    }
    const RTLD_LAZY: c_int = 1;

    /// What happened on the last attempt, for the self-test report.
    pub static STATUS: Mutex<String> = Mutex::new(String::new());

    fn note(s: impl Into<String>) {
        *STATUS.lock().unwrap() = s.into();
    }

    fn manager() -> Result<&'static AnyClass, &'static str> {
        static CLASS: OnceLock<Result<&'static AnyClass, &'static str>> = OnceLock::new();
        *CLASS.get_or_init(|| {
            let path = c"/System/Library/Frameworks/ScreenCaptureKit.framework/ScreenCaptureKit";
            if unsafe { dlopen(path.as_ptr(), RTLD_LAZY) }.is_null() {
                return Err("ScreenCaptureKit not available");
            }
            let class = AnyClass::get(c"SCScreenshotManager").ok_or("no SCScreenshotManager")?;
            // Only macOS 15.2+ has the rectangle screenshot.
            class
                .class_method(sel!(captureImageInRect:completionHandler:))
                .ok_or("no captureImageInRect (macOS < 15.2)")?;
            Ok(class)
        })
    }

    /// Text of an NSError, for diagnostics.
    unsafe fn describe(error: *mut AnyObject) -> String {
        if error.is_null() {
            return "no image, no error".into();
        }
        unsafe {
            let desc: *mut AnyObject = msg_send![error, localizedDescription];
            if desc.is_null() {
                return "error".into();
            }
            let text: *const c_char = msg_send![desc, UTF8String];
            if text.is_null() {
                "error".into()
            } else {
                CStr::from_ptr(text).to_string_lossy().into_owned()
            }
        }
    }

    /// A retained CGImage of `rect`, or `None` to fall back to CoreGraphics.
    pub fn capture(rect: CGRect) -> Option<CGImageRef> {
        let class = match manager() {
            Ok(c) => c,
            Err(why) => {
                note(why);
                return None;
            }
        };
        let (tx, rx) = mpsc::sync_channel::<Result<usize, String>>(1);
        let done = RcBlock::new(move |image: *mut c_void, error: *mut AnyObject| {
            let result = if image.is_null() {
                Err(unsafe { describe(error) })
            } else {
                // Only valid during the callback unless retained.
                Ok((unsafe { CFRetain(image) }) as usize)
            };
            let _ = tx.send(result);
        });
        unsafe {
            let _: () = msg_send![class, captureImageInRect: rect, completionHandler: &*done];
        }
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(image)) => {
                note("ScreenCaptureKit ok");
                Some(image as CGImageRef)
            }
            Ok(Err(e)) => {
                note(format!("ScreenCaptureKit error: {e}"));
                None
            }
            Err(_) => {
                note("ScreenCaptureKit timed out");
                None
            }
        }
    }
}

/// How the last capture went (for the self-test report).
#[cfg(feature = "selftest")]
pub fn backend_status() -> String {
    sck::STATUS.lock().unwrap().clone()
}

/// Asks for the Screen Recording permission once. Returns whether it's granted.
pub fn ensure_permission() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() || CGRequestScreenCaptureAccess() }
}

struct Display {
    id: u32,
    bounds: CGRect,
    scale: f64,
}

fn displays() -> Vec<Display> {
    let mut ids = [0u32; 32];
    let mut count = 0u32;
    unsafe {
        if CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) != 0 {
            return Vec::new();
        }
    }
    ids[..count as usize]
        .iter()
        .map(|&id| unsafe {
            let bounds = CGDisplayBounds(id);
            let mode = CGDisplayCopyDisplayMode(id);
            let mut scale = 1.0;
            if !mode.is_null() {
                let px = CGDisplayModeGetPixelWidth(mode) as f64;
                if bounds.size.width > 0.0 && px > 0.0 {
                    scale = px / bounds.size.width;
                }
                CGDisplayModeRelease(mode);
            }
            Display { id, bounds, scale }
        })
        .collect()
}

#[cfg(feature = "selftest")]
pub fn primary_region() -> Region {
    let b = unsafe { CGDisplayBounds(CGMainDisplayID()) };
    Region {
        x: b.origin.x as i32,
        y: b.origin.y as i32,
        w: b.size.width.max(1.0) as u32,
        h: b.size.height.max(1.0) as u32,
    }
}

pub fn capture(r: Region) -> Result<Frame, String> {
    if r.w == 0 || r.h == 0 {
        return Err("empty region".into());
    }
    let want = CGRect::new(r.x as f64, r.y as f64, r.w as f64, r.h as f64);
    if let Some(img) = sck::capture(want) {
        let frame = unsafe { image_to_frame(img) };
        unsafe { CGImageRelease(img) };
        if let Some(frame) = frame {
            return Ok(frame);
        }
    }
    let hits: Vec<(Display, CGRect)> = displays()
        .into_iter()
        .filter_map(|d| want.intersect(&d.bounds).map(|i| (d, i)))
        .collect();
    let scale = hits.iter().map(|(d, _)| d.scale).fold(1.0, f64::max);
    let width = (r.w as f64 * scale).round() as usize;
    let height = (r.h as f64 * scale).round() as usize;

    // Common case: the region sits on a single display, so copy the
    // display's pixels straight out without any redraw.
    if let [(d, inter)] = hits.as_slice()
        && inter.size.width == want.size.width
        && inter.size.height == want.size.height
    {
        let local = CGRect::new(
            want.origin.x - d.bounds.origin.x,
            want.origin.y - d.bounds.origin.y,
            want.size.width,
            want.size.height,
        );
        unsafe {
            let img = CGDisplayCreateImageForRect(d.id, local);
            if img.is_null() {
                return Err(permission_error());
            }
            let fast = copy_bgrx(img);
            if let Some(frame) = fast {
                CGImageRelease(img);
                return Ok(frame);
            }
            CGImageRelease(img);
        }
    }

    // General case: compose every covered display into one black canvas.
    let mut buf = vec![0u8; width * height * 4];
    unsafe {
        let mut ctx: CGContextRef = std::ptr::null_mut();
        for (d, inter) in &hits {
            let local = CGRect::new(
                inter.origin.x - d.bounds.origin.x,
                inter.origin.y - d.bounds.origin.y,
                inter.size.width,
                inter.size.height,
            );
            let img = CGDisplayCreateImageForRect(d.id, local);
            if img.is_null() {
                continue;
            }
            if ctx.is_null() {
                // Use the display's own color space to avoid a color conversion.
                let mut space = CGImageGetColorSpace(img);
                let owned = space.is_null();
                if owned {
                    space = CGColorSpaceCreateDeviceRGB();
                }
                ctx = CGBitmapContextCreate(
                    buf.as_mut_ptr() as *mut c_void,
                    width,
                    height,
                    8,
                    width * 4,
                    space,
                    K_CG_IMAGE_ALPHA_NONE_SKIP_FIRST | K_CG_BITMAP_BYTE_ORDER_32_LITTLE,
                );
                if owned {
                    CGColorSpaceRelease(space);
                }
                if ctx.is_null() {
                    CGImageRelease(img);
                    return Err("CGBitmapContextCreate failed".into());
                }
            }
            // CoreGraphics contexts have a bottom-left origin.
            let dx = (inter.origin.x - want.origin.x) * scale;
            let dy_top = (inter.origin.y - want.origin.y) * scale;
            let dw = inter.size.width * scale;
            let dh = inter.size.height * scale;
            let rect = CGRect::new(dx, height as f64 - dy_top - dh, dw, dh);
            CGContextDrawImage(ctx, rect, img);
            CGImageRelease(img);
        }
        if !ctx.is_null() {
            CGContextRelease(ctx);
        } else if !hits.is_empty() {
            return Err(permission_error());
        }
    }
    Ok(Frame {
        width: width as u32,
        height: height as u32,
        bgrx: buf,
    })
}

/// Pixels of a whole image: copied directly when already BGRX, otherwise
/// drawn into a BGRX canvas.
unsafe fn image_to_frame(img: CGImageRef) -> Option<Frame> {
    unsafe {
        if let Some(frame) = copy_bgrx(img) {
            return Some(frame);
        }
        let (w, h) = (CGImageGetWidth(img), CGImageGetHeight(img));
        if w == 0 || h == 0 {
            return None;
        }
        let mut buf = vec![0u8; w * h * 4];
        let space = CGColorSpaceCreateDeviceRGB();
        let ctx = CGBitmapContextCreate(
            buf.as_mut_ptr() as *mut c_void,
            w,
            h,
            8,
            w * 4,
            space,
            K_CG_IMAGE_ALPHA_NONE_SKIP_FIRST | K_CG_BITMAP_BYTE_ORDER_32_LITTLE,
        );
        CGColorSpaceRelease(space);
        if ctx.is_null() {
            return None;
        }
        CGContextDrawImage(ctx, CGRect::new(0.0, 0.0, w as f64, h as f64), img);
        CGContextRelease(ctx);
        Some(Frame {
            width: w as u32,
            height: h as u32,
            bgrx: buf,
        })
    }
}

/// Copies a 32-bit little-endian BGRA/BGRX image without conversion.
unsafe fn copy_bgrx(img: CGImageRef) -> Option<Frame> {
    unsafe {
        let info = CGImageGetBitmapInfo(img);
        let alpha = info & K_CG_BITMAP_ALPHA_INFO_MASK;
        if CGImageGetBitsPerPixel(img) != 32
            || info & K_CG_BITMAP_BYTE_ORDER_MASK != K_CG_BITMAP_BYTE_ORDER_32_LITTLE
            || (alpha != K_CG_IMAGE_ALPHA_NONE_SKIP_FIRST
                && alpha != K_CG_IMAGE_ALPHA_PREMULTIPLIED_FIRST)
        {
            return None;
        }
        let (w, h) = (CGImageGetWidth(img), CGImageGetHeight(img));
        let stride = CGImageGetBytesPerRow(img);
        let data = CGDataProviderCopyData(CGImageGetDataProvider(img));
        if data.is_null() {
            return None;
        }
        let len = CFDataGetLength(data) as usize;
        let src = std::slice::from_raw_parts(CFDataGetBytePtr(data), len);
        let row = w * 4;
        let frame = if stride * (h - 1) + row <= len {
            let mut out = Vec::with_capacity(row * h);
            if stride == row {
                out.extend_from_slice(&src[..row * h]);
            } else {
                for y in 0..h {
                    out.extend_from_slice(&src[y * stride..y * stride + row]);
                }
            }
            Some(Frame {
                width: w as u32,
                height: h as u32,
                bgrx: out,
            })
        } else {
            None
        };
        CFRelease(data);
        frame
    }
}

fn permission_error() -> String {
    "Screen capture failed. Allow scr8 in System Settings > Privacy & Security > Screen Recording"
        .into()
}
