//! Full-screen area picker for macOS, drawn with egui. Runs as a short-lived
//! child process (`scr8 --select [x,y,w,h]`) that prints the chosen region to
//! stdout, so the main app never has to juggle full-screen window state.
//! Windows uses the GDI picker in `overlay_win`; both follow `look`.

use eframe::egui::{
    self, Color32, ColorImage, CursorIcon, FontId, Key, Pos2, Rect, Sense, Stroke, StrokeKind,
    TextureHandle, TextureOptions, Vec2, ViewportBuilder, ViewportId, WindowLevel,
};

use crate::capture;
use crate::config::Region;
use crate::look;

const ACCENT: Color32 = Color32::from_rgb(look::ACCENT.0, look::ACCENT.1, look::ACCENT.2);
const HANDLE: f32 = look::HANDLE;

pub fn run(initial: Option<Region>) {
    let options = eframe::NativeOptions {
        viewport: overlay_builder().with_title("scr8 — select area"),
        ..Default::default()
    };
    crate::render::run(
        "scr8-select",
        options,
        Box::new(move |_cc| {
            crate::render::mark_started();
            Ok(Box::new(Overlay::new(initial)))
        }),
    );
    // Window closed without a choice.
    std::process::exit(1);
}

// Windows are placed over their monitor after creation instead of using
// full screen: `ViewportBuilder::with_monitor` makes glutin fail to create
// the GL context on Windows, borderless full screen leaves a stale surface
// there, and macOS refuses full screen for borderless windows.
fn overlay_builder() -> ViewportBuilder {
    ViewportBuilder::default()
        .with_decorations(false)
        .with_window_level(WindowLevel::AlwaysOnTop)
        .with_taskbar(false)
        .with_active(true)
}

struct Monitor {
    /// Native coordinates (physical px on Windows, points on macOS).
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    /// Physical pixel rect as reported by winit, used for window placement.
    phys: Rect,
    placed: bool,
    /// Placement tries so far; gives up eventually rather than loop forever.
    attempts: u32,
    /// Frames drawn since the window was placed; the first ones may still go
    /// to a surface of the old size and show up stretched and blurry.
    settled_frames: u8,
    tex: Option<TextureHandle>,
    /// Where the Save/Esc buttons were last drawn, in window points.
    bar: Option<Rect>,
}

#[derive(Clone, Copy)]
enum Drag {
    New(Pos2),
    Move(Vec2),
    /// Which edges follow the pointer: (left, top, right, bottom).
    Resize(bool, bool, bool, bool),
}

struct Overlay {
    monitors: Vec<Monitor>,
    /// Selection in native global coordinates.
    sel: Option<Rect>,
    drag: Option<Drag>,
    /// Where the primary button went down (window points), until released.
    press: Option<Pos2>,
    /// The Save/Esc panel as last laid out.
    panel: Option<Rect>,
    ready: bool,
}

impl Overlay {
    fn new(initial: Option<Region>) -> Self {
        Self {
            monitors: Vec::new(),
            sel: initial.map(|r| {
                Rect::from_min_size(
                    Pos2::new(r.x as f32, r.y as f32),
                    Vec2::new(r.w as f32, r.h as f32),
                )
            }),
            drag: None,
            press: None,
            panel: None,
            ready: false,
        }
    }

    /// Moves the part of the selection a drag holds to native point `n`.
    fn apply_drag(&mut self, d: Drag, n: Pos2) {
        match d {
            Drag::New(start) => self.sel = Some(Rect::from_two_pos(start, n)),
            Drag::Move(off) => {
                if let Some(s) = self.sel.as_mut() {
                    *s = Rect::from_min_size(n + off, s.size());
                }
            }
            Drag::Resize(l, t, r, b) => {
                if let Some(s) = self.sel.as_mut() {
                    if l {
                        s.min.x = n.x;
                    }
                    if t {
                        s.min.y = n.y;
                    }
                    if r {
                        s.max.x = n.x;
                    }
                    if b {
                        s.max.y = n.y;
                    }
                    *s = Rect::from_two_pos(s.min, s.max);
                }
            }
        }
    }

    fn init(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        self.ready = true;
        let Some(window) = frame.winit_window() else {
            return;
        };
        for (i, m) in window.available_monitors().enumerate() {
            let (pos, size) = (m.position(), m.size());
            let scale = if cfg!(target_os = "macos") {
                m.scale_factor()
            } else {
                1.0
            };
            let region = Region {
                x: (pos.x as f64 / scale).round() as i32,
                y: (pos.y as f64 / scale).round() as i32,
                w: (size.width as f64 / scale).round() as u32,
                h: (size.height as f64 / scale).round() as u32,
            };
            // Our own windows aren't mapped yet, so this is a clean snapshot.
            let tex = capture::capture(region).ok().map(|f| {
                let pixels = f
                    .bgrx
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|p| Color32::from_rgb(p[2], p[1], p[0]))
                    .collect();
                let img = ColorImage::new([f.width as usize, f.height as usize], pixels);
                ctx.load_texture(format!("mon{i}"), img, TextureOptions::LINEAR)
            });
            self.monitors.push(Monitor {
                x: region.x as f32,
                y: region.y as f32,
                w: region.w as f32,
                h: region.h as f32,
                phys: Rect::from_min_size(
                    Pos2::new(pos.x as f32, pos.y as f32),
                    Vec2::new(size.width as f32, size.height as f32),
                ),
                placed: false,
                attempts: 0,
                settled_frames: 0,
                tex,
                bar: None,
            });
        }
    }

    fn finish(&self) {
        if let Some(r) = self.sel {
            let r = Region {
                x: r.min.x.round() as i32,
                y: r.min.y.round() as i32,
                w: r.width().round().max(1.0) as u32,
                h: r.height().round().max(1.0) as u32,
            };
            println!("{}", r.to_arg());
            std::process::exit(0);
        }
    }

    /// Draws one monitor's view and handles its input.
    fn monitor_ui(&mut self, ui: &mut egui::Ui, idx: usize) {
        let screen = ui.max_rect();
        let Some(m) = self.monitors.get_mut(idx) else {
            return;
        };
        // Until the window covers its monitor and has settled at that size,
        // the frozen screen would show up scaled wrong or blurry (for a
        // second or so with software rendering), so show plain black.
        if !m.placed || m.settled_frames < 2 {
            if m.placed {
                m.settled_frames += 1;
            }
            ui.painter().rect_filled(screen, 0.0, Color32::BLACK);
            ui.ctx().request_repaint();
            if ui.input(|i| i.key_pressed(Key::Escape)) {
                std::process::exit(1);
            }
            return;
        }
        let m = &self.monitors[idx];
        // Map through the window's real position so an off-by-one placement
        // (e.g. an invisible border) never shifts the chosen area.
        // Native units are physical px on Windows and points on macOS.
        let k = if cfg!(target_os = "macos") {
            1.0
        } else {
            ui.ctx().pixels_per_point()
        };
        let origin = ui
            .input(|i| i.viewport().inner_rect)
            .map_or(Pos2::new(m.x / k, m.y / k), |r| r.min);
        let to_local = |p: Pos2| (p.to_vec2() / k - origin.to_vec2()).to_pos2();
        let to_native = |p: Pos2| ((origin + p.to_vec2()).to_vec2() * k).to_pos2();
        let image_rect = Rect::from_min_max(
            to_local(Pos2::new(m.x, m.y)),
            to_local(Pos2::new(m.x + m.w, m.y + m.h)),
        );

        let painter = ui.painter().clone();
        painter.rect_filled(screen, 0.0, Color32::BLACK);
        if let Some(tex) = &m.tex {
            painter.image(
                tex.id(),
                image_rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }

        // Clicks only: the Save button on top takes its own clicks, and a
        // double-click inside the selection saves it.
        let resp = ui.interact(screen, ui.id().with(("screen", idx)), Sense::click());
        let local_sel =
            |sel: Option<Rect>| sel.map(|s| Rect::from_min_max(to_local(s.min), to_local(s.max)));

        // Selection dragging reads raw pointer events instead of egui's
        // per-frame drag state, so a press, moves and release that all land
        // in one slow frame (software rendering) still count.
        let bar = self.monitors[idx].bar;
        for ev in ui.input(|i| i.events.clone()) {
            match ev {
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    ..
                } => {
                    if pressed {
                        self.press = (!bar.is_some_and(|b| b.contains(pos))).then_some(pos);
                    } else {
                        if let Some(d) = self.drag.take() {
                            self.apply_drag(d, to_native(pos));
                            if self
                                .sel
                                .is_some_and(|s| s.width() < 3.0 || s.height() < 3.0)
                            {
                                self.sel = None;
                            }
                        }
                        self.press = None;
                    }
                }
                egui::Event::PointerMoved(pos) => {
                    if let Some(start) = self.press
                        && self.drag.is_none()
                        && start.distance(pos) > 4.0
                    {
                        // Decide what the drag does from where the button went down.
                        let sel_local = local_sel(self.sel);
                        self.drag = Some(match (sel_local.map(|s| edge_hit(s, start)), self.sel) {
                            (Some(Some(edges)), _) => {
                                Drag::Resize(edges.0, edges.1, edges.2, edges.3)
                            }
                            (Some(None), Some(s)) if sel_local.unwrap().contains(start) => {
                                Drag::Move(s.min - to_native(start))
                            }
                            _ => Drag::New(to_native(start)),
                        });
                    }
                    if let Some(d) = self.drag {
                        self.apply_drag(d, to_native(pos));
                    }
                }
                _ => {}
            }
        }

        let pointer = ui.ctx().pointer_hover_pos();
        let local_sel = local_sel(self.sel);
        let hover_kind = pointer.and_then(|p| local_sel.map(|s| edge_hit(s, p)));
        if resp.double_clicked()
            && local_sel
                .is_some_and(|s| s.contains(resp.interact_pointer_pos().unwrap_or_default()))
        {
            self.finish();
        }

        let cursor = match (self.drag, hover_kind) {
            (Some(Drag::Move(_)), _) => CursorIcon::Grabbing,
            (Some(Drag::Resize(l, t, r, b)), _) | (None, Some(Some((l, t, r, b)))) => {
                resize_cursor(l, t, r, b)
            }
            (None, Some(None)) if local_sel.zip(pointer).is_some_and(|(s, p)| s.contains(p)) => {
                CursorIcon::Grab
            }
            _ => CursorIcon::Crosshair,
        };
        ui.ctx().set_cursor_icon(cursor);

        // Dim everything outside the selection.
        let dim = Color32::from_black_alpha(look::DIM_ALPHA);
        let sel = self
            .sel
            .map(|s| Rect::from_min_max(to_local(s.min), to_local(s.max)));
        match sel.map(|s| s.intersect(screen)).filter(|s| s.is_positive()) {
            Some(s) => {
                for r in [
                    Rect::from_min_max(screen.min, Pos2::new(screen.max.x, s.min.y)),
                    Rect::from_min_max(Pos2::new(screen.min.x, s.max.y), screen.max),
                    Rect::from_min_max(
                        Pos2::new(screen.min.x, s.min.y),
                        Pos2::new(s.min.x, s.max.y),
                    ),
                    Rect::from_min_max(
                        Pos2::new(s.max.x, s.min.y),
                        Pos2::new(screen.max.x, s.max.y),
                    ),
                ] {
                    painter.rect_filled(r, 0.0, dim);
                }
            }
            None => {
                painter.rect_filled(screen, 0.0, dim);
            }
        }

        if let Some(s) = sel {
            painter.rect_stroke(
                s,
                0.0,
                Stroke::new(look::BORDER, ACCENT),
                StrokeKind::Outside,
            );
            for c in [
                s.left_top(),
                s.right_top(),
                s.left_bottom(),
                s.right_bottom(),
            ] {
                painter.rect_filled(
                    Rect::from_center_size(c, Vec2::splat(look::HANDLE)),
                    1.0,
                    ACCENT,
                );
            }
            let native = self.sel.unwrap();
            let label = format!("{} × {}", native.width().round(), native.height().round());
            let galley = badge_galley(&painter, &label);
            let size = galley.size() + 2.0 * Vec2::from(look::BADGE_PAD);
            let above = s.min.y - look::BADGE_GAP - size.y >= screen.min.y;
            let at = if above {
                Pos2::new(s.min.x, s.min.y - look::BADGE_GAP - size.y)
            } else {
                s.min + Vec2::splat(look::BADGE_GAP)
            };
            draw_badge(&painter, Rect::from_min_size(at, size), galley);

            self.monitors[idx].bar = None;
            if self.drag.is_none() && screen.intersects(s) {
                match self.buttons(ui, screen, s) {
                    Some(true) => self.finish(),
                    Some(false) => std::process::exit(1),
                    None => {}
                }
                self.monitors[idx].bar = self.panel;
            }
        } else {
            let galley = badge_galley(&painter, look::HINT);
            let size = galley.size() + 2.0 * Vec2::from(look::BADGE_PAD);
            let at = Pos2::new(
                screen.center().x - size.x / 2.0,
                screen.min.y + look::HINT_TOP,
            );
            draw_badge(&painter, Rect::from_min_size(at, size), galley);
        }

        let (enter, esc) = ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
        if esc {
            std::process::exit(1);
        }
        if enter {
            self.finish();
        }
    }
}

impl Overlay {
    /// Moves the current viewport over monitor `idx` until it sits there.
    fn place(&mut self, ctx: &egui::Context, idx: usize) {
        let Some(m) = self.monitors.get_mut(idx) else {
            return;
        };
        if m.placed {
            return;
        }
        let vp = ctx.input(|i| i.viewport().clone());
        let ppp = ctx.pixels_per_point();
        let want = Rect::from_min_size((m.phys.min.to_vec2() / ppp).to_pos2(), m.phys.size() / ppp);
        m.attempts += 1;
        m.placed = m.attempts > 120
            || vp.inner_rect.is_some_and(|r| {
                (r.min - want.min).length() < 0.5 && (r.size() - want.size()).length() < 0.5
            });
        if !m.placed {
            // Above the menu bar and Dock, or macOS keeps the window below them.
            #[cfg(target_os = "macos")]
            raise_windows();
            let border = match (vp.outer_rect, vp.inner_rect) {
                (Some(o), Some(i)) => i.min - o.min,
                _ => Vec2::ZERO,
            };
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(want.min - border));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(want.size()));
        }
        if m.placed {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        ctx.request_repaint();
    }
}

impl eframe::App for Overlay {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if !self.ready {
            self.init(&ctx, frame);
        }
        self.place(&ctx, 0);
        self.monitor_ui(ui, 0);
        for i in 1..self.monitors.len() {
            let id = ViewportId::from_hash_of(("scr8-monitor", i));
            ctx.show_viewport_immediate(
                id,
                overlay_builder().with_title("scr8 — select area"),
                |ui, _| {
                    self.place(ui.ctx(), i);
                    self.monitor_ui(ui, i)
                },
            );
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }
}

/// Makes this process's windows (only the overlays live here) cover whole
/// screens on macOS, the way winit's "simple fullscreen" does: hide the menu
/// bar and Dock while we're active, so window frames aren't pushed below
/// them, and float above everything, including other apps' full-screen
/// Spaces.
#[cfg(target_os = "macos")]
fn raise_windows() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{
        NSApplication, NSApplicationPresentationOptions, NSWindowCollectionBehavior,
    };

    // NSScreenSaverWindowLevel: above the menu bar (24) and the Dock (20).
    const LEVEL: isize = 1000;
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    // Hiding the menu bar requires hiding the Dock too.
    app.setPresentationOptions(
        NSApplicationPresentationOptions::HideDock | NSApplicationPresentationOptions::HideMenuBar,
    );
    for window in app.windows().iter() {
        window.setLevel(LEVEL);
        window.setHidesOnDeactivate(false);
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
    }
}

/// Returns `Some(edges)` when `p` is on the selection border, `None` otherwise.
fn edge_hit(s: Rect, p: Pos2) -> Option<(bool, bool, bool, bool)> {
    let grab = s.expand(HANDLE);
    if !grab.contains(p) {
        return None;
    }
    let l = (p.x - s.min.x).abs() <= HANDLE;
    let r = (p.x - s.max.x).abs() <= HANDLE;
    let t = (p.y - s.min.y).abs() <= HANDLE;
    let b = (p.y - s.max.y).abs() <= HANDLE;
    (l || r || t || b).then_some((l, t, r && !l, b && !t))
}

fn resize_cursor(l: bool, t: bool, r: bool, b: bool) -> CursorIcon {
    match (l || r, t || b) {
        (true, true) if (l && t) || (r && b) => CursorIcon::ResizeNwSe,
        (true, true) => CursorIcon::ResizeNeSw,
        (true, false) => CursorIcon::ResizeHorizontal,
        _ => CursorIcon::ResizeVertical,
    }
}

fn color(c: look::Rgb) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}

fn badge_galley(painter: &egui::Painter, text: &str) -> std::sync::Arc<egui::Galley> {
    painter.layout_no_wrap(
        text.to_owned(),
        FontId::proportional(look::BADGE_FONT),
        Color32::WHITE,
    )
}

/// Size label / hint: white text on translucent black.
fn draw_badge(painter: &egui::Painter, rect: Rect, galley: std::sync::Arc<egui::Galley>) {
    painter.rect_filled(
        rect,
        look::BADGE_RADIUS,
        Color32::from_black_alpha(look::BADGE_ALPHA),
    );
    painter.galley(
        rect.min + Vec2::from(look::BADGE_PAD),
        galley,
        Color32::WHITE,
    );
}

impl Overlay {
    /// Draws the Save / Esc panel under the selection (same layout as the
    /// Windows picker). Returns `Some(true)` for Save, `Some(false)` for Esc.
    fn buttons(&mut self, ui: &mut egui::Ui, screen: Rect, s: Rect) -> Option<bool> {
        let painter = ui.painter().clone();
        let font = FontId::proportional(look::BUTTON_FONT);
        let pad = Vec2::from(look::BUTTON_PAD);
        let label = |icon: &str, text: &str| {
            painter.layout_no_wrap(format!("{icon} {text}"), font.clone(), Color32::WHITE)
        };
        let save = label(look::SAVE_ICON, look::SAVE_LABEL);
        let cancel = label(look::CANCEL_ICON, look::CANCEL_LABEL);
        let btn_h = save.size().y + 2.0 * pad.y;
        let save_w = save.size().x + 2.0 * pad.x;
        let cancel_w = cancel.size().x + 2.0 * pad.x;
        let panel_size = Vec2::new(
            2.0 * look::PANEL_PAD + save_w + look::BUTTON_GAP + cancel_w,
            2.0 * look::PANEL_PAD + btn_h,
        );
        let y = if s.max.y + look::PANEL_GAP + panel_size.y <= screen.max.y {
            s.max.y + look::PANEL_GAP
        } else {
            s.max.y - look::PANEL_GAP - panel_size.y
        };
        let right = s
            .max
            .x
            .min(screen.max.x - look::PANEL_GAP)
            .max(screen.min.x + panel_size.x + look::PANEL_GAP);
        let panel = Rect::from_min_size(Pos2::new(right - panel_size.x, y), panel_size);
        self.panel = Some(panel);

        // Soft drop shadow, then border and fill.
        painter.rect_filled(
            panel.translate(Vec2::new(3.0, 5.0)).expand(2.0),
            look::PANEL_RADIUS + 2.0,
            Color32::from_black_alpha(45),
        );
        painter.rect_filled(panel, look::PANEL_RADIUS, color(look::PANEL_STROKE));
        painter.rect_filled(
            panel.shrink(1.0),
            look::PANEL_RADIUS - 1.0,
            color(look::PANEL_FILL),
        );

        let save_rect = Rect::from_min_size(
            panel.min + Vec2::splat(look::PANEL_PAD),
            Vec2::new(save_w, btn_h),
        );
        let cancel_rect = Rect::from_min_size(
            Pos2::new(save_rect.max.x + look::BUTTON_GAP, save_rect.min.y),
            Vec2::new(cancel_w, btn_h),
        );
        let mut clicked = None;
        for (rect, galley, is_save) in [(save_rect, save, true), (cancel_rect, cancel, false)] {
            let resp = ui.interact(rect, ui.id().with(("button", is_save)), Sense::click());
            let (bg, fg) = if resp.hovered() {
                (look::BUTTON_FILL_HOVER, look::BUTTON_TEXT_HOVER)
            } else {
                (look::BUTTON_FILL, look::BUTTON_TEXT)
            };
            painter.rect_filled(rect, look::BUTTON_RADIUS, color(bg));
            let pos = Pos2::new(rect.min.x + pad.x, rect.center().y - galley.size().y / 2.0);
            painter.galley(pos, galley, color(fg));
            if resp.clicked() {
                clicked = Some(is_save);
            }
        }
        clicked
    }
}
