#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod autostart;
mod bench;
mod capture;
mod config;
mod engine;
mod hotkey;
mod icon;
mod overlay;
mod single;
mod tray;

use eframe::egui;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--select") {
        overlay::run(args.get(1).and_then(|a| config::Region::parse(a)));
        return;
    }
    if args.first().map(String::as_str) == Some("--write-icon") {
        // Used by the macOS bundle script to build the .icns.
        let (Some(size), Some(path)) = (args.get(1).and_then(|s| s.parse().ok()), args.get(2))
        else {
            std::process::exit(2);
        };
        std::process::exit(i32::from(write_icon(size, path).is_err()));
    }
    let hidden = args.iter().any(|a| a == "--hidden");
    // A manual launch while running shows the existing window; a duplicate
    // autostart just quits.
    let instance = match single::acquire(!hidden, config::is_dev()) {
        single::Instance::Primary(p) => p,
        single::Instance::Secondary => return,
    };

    let icon = egui::IconData {
        rgba: icon::rgba(64, icon::Style::App),
        width: 64,
        height: 64,
    };
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("scr8")
        .with_app_id("scr8")
        .with_inner_size([640.0, 520.0])
        .with_min_inner_size([520.0, 380.0])
        .with_icon(icon);
    if hidden {
        // eframe always shows the window after the first frame, so park it
        // off-screen; the app hides it immediately.
        viewport = viewport.with_position([-30000.0, -30000.0]);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let _ = eframe::run_native(
        "scr8",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc, hidden, instance)))),
    );
}

fn write_icon(size: u32, path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, size, size);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header()?;
    w.write_image_data(&icon::rgba(size, icon::Style::App))?;
    Ok(w.finish()?)
}
