//! Raw screen capture. Returns uncompressed pixels as fast as the OS allows;
//! encoding happens elsewhere so the hotkey thread is never blocked.

#[cfg(windows)]
mod dda;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "macos")]
pub use macos::{capture, ensure_permission};
/// The GDI path on its own, for comparing speeds in the benchmark.
#[cfg(all(windows, test))]
pub use windows::capture_gdi;
#[cfg(windows)]
pub use windows::{capture, ensure_permission};

/// Prepares the fastest capture method ahead of the first screenshot.
pub fn warm_up() {
    #[cfg(windows)]
    dda::warm_up();
}

/// 4 bytes per pixel in B, G, R, X order, rows tightly packed.
/// Areas not covered by any monitor are black.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub bgrx: Vec<u8>,
}
