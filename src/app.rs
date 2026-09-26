use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use eframe::egui::{self, Color32, RichText, ViewportCommand};
use global_hotkey::GlobalHotKeyManager;
use global_hotkey::hotkey::HotKey;

use crate::config::{Config, PngLevel, Region};
use crate::engine::{Engine, Target};
use crate::hotkey::Hotkey;
use crate::single;
use crate::tray::{Tray, TrayCmd};
use crate::{autostart, bench, capture};

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Binds,
    Advanced,
}

/// Steps left for a bind that was just created.
#[derive(PartialEq, Clone, Copy)]
enum Wizard {
    Folder,
    Hotkey,
}

pub struct App {
    cfg: Config,
    engine: Engine,
    manager: Option<GlobalHotKeyManager>,
    registered: Vec<HotKey>,
    bind_errors: HashMap<u64, String>,
    tab: Tab,
    recording: Option<u64>,
    selecting: Option<(u64, Receiver<Option<Region>>)>,
    wizard: Option<(u64, Wizard)>,
    tray: Option<Tray>,
    tray_rx: Receiver<TrayCmd>,
    /// Engine error count already shown; a higher count means a new failure.
    errors_seen: u64,
    last_toast: Option<Instant>,
    permission_ok: bool,
    notice: Option<String>,
    bench_source: Option<u64>,
    bench_rx: Option<Receiver<Result<bench::Report, String>>>,
    bench_report: Option<Result<bench::Report, String>>,
    /// Started via autostart: the window was created off-screen and must be
    /// hidden on the first frame, then centered when first shown.
    start_hidden: bool,
    needs_centering: bool,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        start_hidden: bool,
        instance: single::Primary,
    ) -> Self {
        let cfg = Config::load();
        let c = cc.egui_ctx.clone();
        let (engine, key_tx) = Engine::start(cfg.png_level, move || c.request_repaint());
        global_hotkey::GlobalHotKeyEvent::set_event_handler(Some(move |e| {
            let _ = key_tx.send(e);
        }));
        let manager = GlobalHotKeyManager::new().ok();

        let (tray_tx, tray_rx): (Sender<TrayCmd>, _) = unbounded();
        let tray = Tray::create(&cc.egui_ctx, tray_tx.clone());
        // Launching scr8 again just brings this copy's window up.
        let c = cc.egui_ctx.clone();
        instance.listen(move || {
            let _ = tray_tx.send(TrayCmd::Show);
            c.request_repaint();
        });

        if cfg.autostart {
            let _ = autostart::apply(true);
        }

        let mut style = (*cc.egui_ctx.global_style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 4.0);
        cc.egui_ctx.set_global_style(style);

        let mut app = Self {
            cfg,
            engine,
            manager,
            registered: Vec::new(),
            bind_errors: HashMap::new(),
            tab: Tab::Binds,
            recording: None,
            selecting: None,
            wizard: None,
            tray,
            tray_rx,
            errors_seen: 0,
            last_toast: None,
            permission_ok: capture::ensure_permission(),
            notice: None,
            bench_source: None,
            bench_rx: None,
            bench_report: None,
            start_hidden,
            needs_centering: start_hidden,
        };
        if app.manager.is_none() {
            app.notice = Some("Global hotkeys are unavailable on this system".into());
        }
        app.sync_hotkeys();
        app
    }

    /// Turns new engine failures into a red tray icon and, at most every
    /// few seconds, a system notification.
    fn check_errors(&mut self) {
        let n = self.engine.errors.load(Ordering::Relaxed);
        if n == self.errors_seen {
            return;
        }
        self.errors_seen = n;
        let Some(msg) = self.engine.last_error.lock().unwrap().clone() else {
            return;
        };
        let Some(tray) = self.tray.as_mut() else {
            return;
        };
        tray.set_error(Some(&msg));
        if self
            .last_toast
            .is_none_or(|t| t.elapsed() > Duration::from_secs(10))
        {
            self.last_toast = Some(Instant::now());
            tray.notify("Screenshot not saved", &msg);
        }
    }

    fn clear_errors(&mut self) {
        *self.engine.last_error.lock().unwrap() = None;
        self.notice = None;
        if let Some(tray) = self.tray.as_mut() {
            tray.set_error(None);
        }
    }

    fn save(&mut self) {
        if let Err(e) = self.cfg.save() {
            self.notice = Some(format!("Can't save settings: {e}"));
        }
    }

    /// Re-registers every complete bind and publishes targets to the engine.
    fn sync_hotkeys(&mut self) {
        let Some(manager) = &self.manager else {
            return;
        };
        let _ = manager.unregister_all(&self.registered);
        self.registered.clear();
        self.bind_errors.clear();
        let mut targets = HashMap::new();
        // While recording, keys must reach our window instead of the OS hook.
        if self.recording.is_none() {
            for b in &self.cfg.binds {
                let (Some(hk), Some(region), Some(folder)) = (&b.hotkey, b.region, &b.folder)
                else {
                    continue;
                };
                let Some(g) = hk.to_global() else {
                    continue;
                };
                if targets.contains_key(&g.id()) {
                    self.bind_errors
                        .insert(b.id, "Same hotkey as another bind".into());
                    continue;
                }
                match manager.register(g) {
                    Ok(()) => {
                        self.registered.push(g);
                        targets.insert(
                            g.id(),
                            Target {
                                name: b.name.clone(),
                                region,
                                folder: folder.clone(),
                            },
                        );
                    }
                    Err(global_hotkey::Error::AlreadyRegistered(_)) => {
                        self.bind_errors
                            .insert(b.id, "This hotkey is taken by another app".into());
                    }
                    Err(e) => {
                        self.bind_errors
                            .insert(b.id, format!("Can't register hotkey: {e}"));
                    }
                }
            }
        }
        *self.engine.targets.write().unwrap() = targets;
    }

    fn show_window(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        if std::mem::take(&mut self.needs_centering)
            && let Some(w) = frame.winit_window()
            && let Some(m) = w.primary_monitor()
        {
            let (mp, ms, ws) = (m.position(), m.size(), w.outer_size());
            let ppp = w.scale_factor() as f32;
            let x = mp.x as f32 + (ms.width as f32 - ws.width as f32) / 2.0;
            let y = mp.y as f32 + (ms.height as f32 - ws.height as f32) / 2.0;
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(egui::pos2(x / ppp, y / ppp)));
        }
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
    }

    /// Hides the settings window and runs the picker as a child process.
    fn start_select(&mut self, ctx: &egui::Context, id: u64) {
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        self.recording = None;
        let initial = self.bind(id).and_then(|b| b.region);
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        let (tx, rx) = unbounded();
        let c = ctx.clone();
        std::thread::spawn(move || {
            // Let the window fade out so it isn't in the frozen screenshot.
            std::thread::sleep(Duration::from_millis(250));
            let mut cmd = Command::new(exe);
            cmd.arg("--select");
            if let Some(r) = initial {
                cmd.arg(r.to_arg());
            }
            let region = cmd
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| Region::parse(String::from_utf8_lossy(&o.stdout).trim()));
            let _ = tx.send(region);
            c.request_repaint();
        });
        self.selecting = Some((id, rx));
    }

    fn pick_folder(&mut self, id: u64) {
        let current = self.bind(id).and_then(|b| b.folder.clone());
        let mut dialog = rfd::FileDialog::new().set_title("Where to save screenshots");
        // New binds start on the Desktop; existing ones reopen their folder.
        if let Some(dir) = current.or_else(dirs::desktop_dir) {
            dialog = dialog.set_directory(dir);
        }
        if let Some(folder) = dialog.pick_folder() {
            if let Some(b) = self.bind_mut(id) {
                b.folder = Some(folder);
            }
            self.save();
            self.sync_hotkeys();
        }
    }

    fn start_recording(&mut self, id: u64) {
        self.recording = Some(id);
        self.sync_hotkeys();
    }

    fn stop_recording(&mut self) {
        self.recording = None;
        self.sync_hotkeys();
    }

    fn bind(&self, id: u64) -> Option<&crate::config::Bind> {
        self.cfg.binds.iter().find(|b| b.id == id)
    }

    fn bind_mut(&mut self, id: u64) -> Option<&mut crate::config::Bind> {
        self.cfg.binds.iter_mut().find(|b| b.id == id)
    }

    fn handle_recording(&mut self, ctx: &egui::Context) {
        let Some(id) = self.recording else {
            return;
        };
        let presses: Vec<(egui::Key, egui::Modifiers)> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key {
                        key,
                        physical_key,
                        pressed: true,
                        repeat: false,
                        modifiers,
                        ..
                    } => Some((physical_key.unwrap_or(*key), *modifiers)),
                    _ => None,
                })
                .collect()
        });
        for (key, mods) in presses {
            if key == egui::Key::Escape && !mods.any() {
                self.stop_recording();
                return;
            }
            if let Some(hk) = Hotkey::from_press(key, mods) {
                let dup = self
                    .cfg
                    .binds
                    .iter()
                    .find(|b| b.id != id && b.hotkey.as_ref() == Some(&hk))
                    .map(|b| b.name.clone());
                if let Some(other) = dup {
                    self.notice = Some(format!("{} is already used by \"{other}\"", hk.label()));
                    continue;
                }
                if let Some(b) = self.bind_mut(id) {
                    b.hotkey = Some(hk);
                }
                self.notice = None;
                self.save();
                self.stop_recording();
                return;
            }
        }
    }

    fn poll_background(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        while let Ok(cmd) = self.tray_rx.try_recv() {
            match cmd {
                TrayCmd::Show => self.show_window(ctx, frame),
                TrayCmd::Quit => {
                    let _ = self.cfg.save();
                    self.tray = None;
                    std::process::exit(0);
                }
            }
        }

        if let Some((id, rx)) = &self.selecting
            && let Ok(result) = rx.try_recv()
        {
            let id = *id;
            self.selecting = None;
            self.show_window(ctx, frame);
            match result {
                Some(region) => {
                    if let Some(b) = self.bind_mut(id) {
                        b.region = Some(region);
                    }
                    self.save();
                    self.sync_hotkeys();
                    if self.wizard.is_some_and(|(w, _)| w == id) {
                        self.wizard = Some((id, Wizard::Folder));
                    }
                }
                None => {
                    // Cancelled while creating: drop the empty bind.
                    if self.wizard.is_some_and(|(w, _)| w == id) {
                        self.wizard = None;
                        if self.bind(id).is_some_and(|b| b.region.is_none()) {
                            self.cfg.binds.retain(|b| b.id != id);
                            self.save();
                        }
                    }
                }
            }
        }

        if let Some(rx) = &self.bench_rx
            && let Ok(r) = rx.try_recv()
        {
            self.bench_report = Some(r);
            self.bench_rx = None;
        }

        if ctx.input(|i| i.viewport().close_requested()) {
            // Hotkeys are paused while recording; don't leave them off.
            if self.recording.is_some() {
                self.stop_recording();
            }
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        }
    }

    fn binds_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("➕ New bind").clicked() {
                let id = self.cfg.new_bind().id;
                self.save();
                self.wizard = Some((id, Wizard::Folder));
                self.start_select(ui.ctx(), id);
            }
            ui.label(
                RichText::new("Area, folder, hotkey. Press the hotkey anywhere to save a PNG.")
                    .weak(),
            );
        });
        ui.add_space(4.0);

        if self.cfg.binds.is_empty() {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("No binds yet").size(18.0));
                ui.label(RichText::new("Create one to start taking screenshots").weak());
            });
            return;
        }

        let mut delete = None;
        let mut action: Option<(u64, &'static str)> = None;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                let ids: Vec<u64> = self.cfg.binds.iter().map(|b| b.id).collect();
                for id in ids {
                    let recording = self.recording == Some(id);
                    let error = self.bind_errors.get(&id).cloned();
                    let Some(b) = self.cfg.binds.iter_mut().find(|b| b.id == id) else {
                        continue;
                    };
                    egui::Frame::group(ui.style())
                        .inner_margin(10.0)
                        .corner_radius(6.0)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                let name = ui.add(
                                    egui::TextEdit::singleline(&mut b.name)
                                        .desired_width(180.0)
                                        .font(egui::TextStyle::Heading),
                                );
                                if name.lost_focus() {
                                    action = Some((id, "rename"));
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.button("🗑").on_hover_text("Delete bind").clicked()
                                        {
                                            delete = Some(id);
                                        }
                                        let ready = b.hotkey.is_some()
                                            && b.region.is_some()
                                            && b.folder.is_some();
                                        if error.is_none() && ready {
                                            let green = Color32::from_rgb(60, 180, 90);
                                            ui.label(RichText::new("active").color(green));
                                            // Painted: the default font has no bullet glyph.
                                            let (dot, _) = ui.allocate_exact_size(
                                                egui::vec2(8.0, 8.0),
                                                egui::Sense::hover(),
                                            );
                                            ui.painter().circle_filled(dot.center(), 4.0, green);
                                        }
                                    },
                                );
                            });
                            egui::Grid::new(("grid", id))
                                .num_columns(3)
                                .spacing([12.0, 6.0])
                                .show(ui, |ui| {
                                    ui.label("Hotkey");
                                    let text = if recording {
                                        RichText::new("Press a combination…  (Esc to cancel)")
                                            .color(Color32::from_rgb(255, 170, 40))
                                    } else {
                                        match &b.hotkey {
                                            Some(h) => RichText::new(h.label()).strong(),
                                            None => RichText::new("not set").weak(),
                                        }
                                    };
                                    ui.label(text);
                                    let label = if recording {
                                        "Cancel"
                                    } else if b.hotkey.is_some() {
                                        "Change"
                                    } else {
                                        "Set hotkey"
                                    };
                                    if ui.button(label).clicked() {
                                        action = Some((
                                            id,
                                            if recording { "stop_rec" } else { "record" },
                                        ));
                                    }
                                    ui.end_row();

                                    ui.label("Area");
                                    match b.region {
                                        Some(r) => ui.label(format!(
                                            "{} × {}  at  {}, {}",
                                            r.w, r.h, r.x, r.y
                                        )),
                                        None => ui.label(RichText::new("not set").weak()),
                                    };
                                    let label = if b.region.is_some() {
                                        "Edit area"
                                    } else {
                                        "Select area"
                                    };
                                    if ui.button(label).clicked() {
                                        action = Some((id, "select"));
                                    }
                                    ui.end_row();

                                    ui.label("Folder");
                                    match &b.folder {
                                        Some(f) => ui
                                            .add(egui::Label::new(short_path(f)).truncate())
                                            .on_hover_text(f.display().to_string()),
                                        None => ui.label(RichText::new("not set").weak()),
                                    };
                                    ui.horizontal(|ui| {
                                        if ui.button("Choose…").clicked() {
                                            action = Some((id, "folder"));
                                        }
                                        if b.folder.is_some() && ui.button("Open").clicked() {
                                            action = Some((id, "open"));
                                        }
                                    });
                                    ui.end_row();
                                });
                            if let Some(e) = error {
                                ui.colored_label(Color32::from_rgb(230, 80, 70), e);
                            }
                        });
                    ui.add_space(6.0);
                }
            });

        if let Some(id) = delete {
            self.cfg.binds.retain(|b| b.id != id);
            if self.recording == Some(id) {
                self.recording = None;
            }
            self.save();
            self.sync_hotkeys();
        }
        if let Some((id, what)) = action {
            match what {
                "rename" => {
                    self.save();
                    self.sync_hotkeys();
                }
                "record" => self.start_recording(id),
                "stop_rec" => self.stop_recording(),
                "select" => self.start_select(ui.ctx(), id),
                "folder" => self.pick_folder(id),
                "open" => {
                    if let Some(f) = self.bind(id).and_then(|b| b.folder.clone()) {
                        open_folder(&f);
                    }
                }
                _ => {}
            }
        }
    }

    fn advanced_tab(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame) {
        ui.heading("PNG compression");
        ui.label(RichText::new("Screenshots are always lossless PNG. Compression only trades save speed for file size.").weak());
        ui.add_space(4.0);
        let mut level = self.cfg.png_level;
        for l in PngLevel::ALL {
            ui.radio_value(&mut level, l, format!("{}  —  {}", l.label(), l.hint()));
        }
        if level != self.cfg.png_level {
            self.cfg.png_level = level;
            self.engine.set_level(level);
            self.save();
        }

        ui.add_space(12.0);
        ui.heading("Speed test");
        ui.label(RichText::new("Captures an area and encodes it with every level to show real timings and sizes on this computer.").weak());
        ui.add_space(4.0);

        let primary = frame
            .winit_window()
            .and_then(|w| w.primary_monitor())
            .map(|m| {
                let (p, s) = (m.position(), m.size());
                let k = if cfg!(target_os = "macos") {
                    m.scale_factor()
                } else {
                    1.0
                };
                Region {
                    x: (p.x as f64 / k) as i32,
                    y: (p.y as f64 / k) as i32,
                    w: (s.width as f64 / k) as u32,
                    h: (s.height as f64 / k) as u32,
                }
            });
        let sources: Vec<(Option<u64>, String, Option<Region>)> =
            std::iter::once((None, "Full primary screen".to_string(), primary))
                .chain(
                    self.cfg
                        .binds
                        .iter()
                        .filter(|b| b.region.is_some())
                        .map(|b| (Some(b.id), b.name.clone(), b.region)),
                )
                .collect();
        if !sources.iter().any(|s| s.0 == self.bench_source) {
            self.bench_source = None;
        }
        let running = self.bench_rx.is_some();
        ui.horizontal(|ui| {
            let current = sources
                .iter()
                .find(|s| s.0 == self.bench_source)
                .map(|s| s.1.clone())
                .unwrap_or_default();
            egui::ComboBox::from_id_salt("bench-src")
                .selected_text(current)
                .width(200.0)
                .show_ui(ui, |ui| {
                    for (id, name, _) in &sources {
                        ui.selectable_value(&mut self.bench_source, *id, name);
                    }
                });
            let region = sources
                .iter()
                .find(|s| s.0 == self.bench_source)
                .and_then(|s| s.2);
            if ui
                .add_enabled(
                    !running && region.is_some(),
                    egui::Button::new("▶ Run test"),
                )
                .clicked()
                && let Some(r) = region
            {
                let (tx, rx) = unbounded();
                let c = ui.ctx().clone();
                std::thread::spawn(move || {
                    let _ = tx.send(bench::run(r));
                    c.request_repaint();
                });
                self.bench_rx = Some(rx);
                self.bench_report = None;
            }
            if running {
                ui.spinner();
            }
        });

        match &self.bench_report {
            Some(Ok(rep)) => {
                ui.add_space(6.0);
                ui.label(format!(
                    "Area {} × {} px  ·  capture {:.1} ms  ·  up to ~{:.0} shots/s",
                    rep.width,
                    rep.height,
                    rep.capture_ms,
                    1000.0 / rep.capture_ms.max(0.1)
                ));
                egui::Grid::new("bench")
                    .striped(true)
                    .num_columns(5)
                    .spacing([18.0, 4.0])
                    .show(ui, |ui| {
                        for h in ["Level", "Encode", "Write", "Total", "Size"] {
                            ui.label(RichText::new(h).strong());
                        }
                        ui.end_row();
                        for row in &rep.rows {
                            let name = if row.level == self.cfg.png_level {
                                RichText::new(format!("{} ✔", row.level.label())).strong()
                            } else {
                                RichText::new(row.level.label())
                            };
                            ui.label(name);
                            ui.label(format!("{:.1} ms", row.encode_ms));
                            ui.label(format!("{:.1} ms", row.write_ms));
                            ui.label(format!(
                                "{:.1} ms",
                                rep.capture_ms + row.encode_ms + row.write_ms
                            ));
                            ui.label(human_size(row.bytes));
                            ui.end_row();
                        }
                    });
                ui.label(RichText::new("Encoding runs in the background, so even slow levels never drop key presses.").weak());
            }
            Some(Err(e)) => {
                ui.colored_label(Color32::from_rgb(230, 80, 70), e);
            }
            None => {}
        }

        ui.add_space(12.0);
        ui.heading("System");
        let mut auto = self.cfg.autostart;
        if ui
            .checkbox(&mut auto, "Start with system (runs in the tray)")
            .changed()
        {
            match autostart::apply(auto) {
                Ok(()) => {
                    self.cfg.autostart = auto;
                    self.save();
                }
                Err(e) => self.notice = Some(format!("Autostart: {e}")),
            }
        }
        if let Some(dir) = dirs::config_dir() {
            ui.label(
                RichText::new(format!(
                    "Settings file: {}",
                    dir.join("scr8").join("config.json").display()
                ))
                .weak()
                .small(),
            );
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if std::mem::take(&mut self.start_hidden) {
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        }
        self.poll_background(ctx, frame);
        self.check_errors();
        if self.selecting.is_some() || self.bench_rx.is_some() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_recording(&ctx);

        // Continue the new-bind flow once the window is back.
        if self.selecting.is_none()
            && let Some((id, Wizard::Folder)) = self.wizard
        {
            self.wizard = Some((id, Wizard::Hotkey));
            if self.bind(id).is_some_and(|b| b.folder.is_none()) {
                self.pick_folder(id);
            }
        } else if self.selecting.is_none()
            && let Some((id, Wizard::Hotkey)) = self.wizard
        {
            self.wizard = None;
            if self.bind(id).is_some_and(|b| b.hotkey.is_none()) {
                self.start_recording(id);
            }
        }

        egui::Panel::top("tabs").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Binds, RichText::new("Binds").size(15.0));
                ui.selectable_value(
                    &mut self.tab,
                    Tab::Advanced,
                    RichText::new("Advanced").size(15.0),
                );
            });
            ui.add_space(4.0);
        });

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                let saved = self.engine.saved.load(Ordering::Relaxed);
                ui.label(RichText::new(format!("Saved this session: {saved}")).weak());
                let err = self.engine.last_error.lock().unwrap().clone();
                if let Some(e) = err.or_else(|| self.notice.clone()) {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("✖").on_hover_text("Dismiss").clicked() {
                            self.clear_errors();
                        }
                        let text = RichText::new(&e).color(Color32::from_rgb(230, 80, 70));
                        ui.add(egui::Label::new(text).truncate()).on_hover_text(e);
                    });
                }
            });
        });

        egui::CentralPanel::default().show(ui, |ui| {
            if !self.permission_ok {
                ui.colored_label(
                    Color32::from_rgb(255, 170, 40),
                    "Screen Recording permission is required. Enable scr8 in System Settings > Privacy & Security > Screen Recording, then restart scr8.",
                );
                if ui.button("Check again").clicked() {
                    self.permission_ok = capture::ensure_permission();
                }
                ui.separator();
            }
            match self.tab {
                Tab::Binds => self.binds_tab(ui),
                Tab::Advanced => {
                    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| self.advanced_tab(ui, frame));
                }
            }
        });

        // Keep the "saved" counter fresh while visible.
        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

fn short_path(p: &std::path::Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = p.strip_prefix(&home)
    {
        return format!("~/{}", rest.display());
    }
    p.display().to_string()
}

fn human_size(bytes: usize) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.2} MB", b as f64 / (1 << 20) as f64),
        b => format!("{:.0} KB", b as f64 / 1024.0),
    }
}

fn open_folder(path: &PathBuf) {
    #[cfg(windows)]
    let _ = Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg(path).spawn();
}
