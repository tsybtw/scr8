//! Picks how windows are drawn and recovers when that fails.
//!
//! OpenGL is tried first. Machines without a usable OpenGL driver (servers,
//! remote desktop, VMs, broken drivers) are relaunched on wgpu, which uses
//! DirectX 12 / Metal / Vulkan and falls back to a software adapter (WARP on
//! Windows), so scr8 runs even without a GPU. A process can create its window
//! only once, hence the relaunch instead of a retry.

use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::{egui_wgpu, wgpu};

const RENDERER_VAR: &str = "SCR8_RENDERER";
/// Set on a process started as the fallback of a failed one.
pub const RELAUNCHED_VAR: &str = "SCR8_RELAUNCHED";

/// Set once a window is up; failures after that are crashes, not driver trouble.
static STARTED: AtomicBool = AtomicBool::new(false);

pub fn mark_started() {
    STARTED.store(true, Ordering::Relaxed);
}

fn using_wgpu() -> bool {
    std::env::var(RENDERER_VAR).is_ok_and(|v| v == "wgpu")
}

pub fn configure(options: &mut eframe::NativeOptions) {
    if !using_wgpu() {
        options.renderer = eframe::Renderer::Glow;
        return;
    }
    options.renderer = eframe::Renderer::Wgpu;
    if let egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup {
        setup.native_adapter_selector = Some(Arc::new(pick_adapter));
    }
}

/// A real GPU if there is one, otherwise the software adapter.
/// `SCR8_SOFTWARE=1` forces the software adapter (for testing).
fn pick_adapter(
    adapters: &[wgpu::Adapter],
    surface: Option<&wgpu::Surface<'_>>,
) -> Result<wgpu::Adapter, String> {
    let usable = || {
        adapters
            .iter()
            .filter(|a| surface.is_none_or(|s| a.is_surface_supported(s)))
    };
    let is_cpu = |a: &&wgpu::Adapter| a.get_info().device_type == wgpu::DeviceType::Cpu;
    let software_only = std::env::var_os("SCR8_SOFTWARE").is_some();
    let pick = if software_only {
        usable().find(is_cpu)
    } else {
        usable()
            .find(|a| !is_cpu(a))
            .or_else(|| usable().find(is_cpu))
    };
    pick.cloned()
        .ok_or_else(|| "no graphics adapter can draw to this window".into())
}

/// Runs the window; on failure relaunches on wgpu or reports the error.
pub fn run(app_name: &str, mut options: eframe::NativeOptions, creator: eframe::AppCreator<'_>) {
    configure(&mut options);
    if let Err(e) = eframe::run_native(app_name, options, creator) {
        fail(&e.to_string());
    }
}

/// Also used by the panic hook: a panic before the first window is treated
/// like a failed window.
fn fail(details: &str) -> ! {
    if !using_wgpu() && !STARTED.load(Ordering::Relaxed) {
        relaunch_on_wgpu();
    }
    show_error(&format!(
        "scr8 can't open its window: no working graphics driver (OpenGL or DirectX 12) was found.\n\nDetails: {details}"
    ));
    std::process::exit(1);
}

/// Starts this program again on wgpu with the same arguments.
///
/// The area picker waits for it and passes on its exit code, since its
/// parent reads the result from stdout (which the new process inherits).
/// The main app exits right away so the new copy can take over the
/// single-instance slot.
fn relaunch_on_wgpu() -> ! {
    let picker = std::env::args().nth(1).as_deref() == Some("--select");
    let child = std::env::current_exe().ok().and_then(|exe| {
        Command::new(exe)
            .args(std::env::args_os().skip(1))
            .env(RENDERER_VAR, "wgpu")
            .env(RELAUNCHED_VAR, "1")
            .spawn()
            .ok()
    });
    let Some(mut child) = child else {
        show_error("scr8 can't open its window and couldn't restart itself.");
        std::process::exit(1);
    };
    if !picker {
        std::process::exit(0);
    }
    let code = child.wait().ok().and_then(|s| s.code()).unwrap_or(1);
    std::process::exit(code);
}

pub fn show_error(text: &str) {
    let _ = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("scr8")
        .set_description(text)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

/// Crashes show a short message instead of vanishing silently.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown error".into());
        let place = info
            .location()
            .map(|l| format!(" ({}:{})", l.file(), l.line()))
            .unwrap_or_default();
        let details = format!("{msg}{place}");
        let main_thread = std::thread::current().name() == Some("main");
        if main_thread && !STARTED.load(Ordering::Relaxed) {
            fail(&details);
        }
        // AppKit dialogs must run on the main thread.
        if cfg!(windows) || main_thread {
            show_error(&format!(
                "scr8 crashed and will close.\n\nDetails: {details}"
            ));
        }
    }));
}
