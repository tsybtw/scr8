//! Raw screen capture. Returns uncompressed pixels as fast as the OS allows;
//! encoding happens elsewhere so the hotkey thread is never blocked.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "macos")]
pub use macos::{capture, ensure_permission};
#[cfg(windows)]
pub use windows::{capture, ensure_permission};

/// 4 bytes per pixel in B, G, R, X order, rows tightly packed.
/// Areas not covered by any monitor are black.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub bgrx: Vec<u8>,
}
