//! Full-screen area picker. Runs as a short-lived child process
//! (`scr8 --select [x,y,w,h]`) that prints the chosen region to stdout, so the
//! main app never has to juggle full-screen window state.

use eframe::egui::{
    self, Align2, Color32, ColorImage, CursorIcon, FontId, Key, Pos2, Rect, Sense, Stroke,
    StrokeKind, TextureHandle, TextureOptions, Vec2, ViewportBuilder, ViewportId, WindowLevel,
};

use crate::capture;
use crate::config::Region;

const ACCENT: Color32 = Color32::from_rgb(64, 156, 255);
const HANDLE: f32 = 8.0;

pub fn run(initial: Option<Region>) {
    let options = eframe::NativeOptions {
        viewport: overlay_builder().with_title("scr8 — select area"),
        ..Default::default()
    };
    let result = eframe::run_native(
        "scr8-select",
        options,
        Box::new(move |_cc| Ok(Box::new(Overlay::new(initial)))),
    );
    if let Err(e) = result {
        eprintln!("overlay failed: {e}");
    }
    // Window closed without a choice.
    std::process::exit(1);
}

// `ViewportBuilder::with_monitor` makes glutin fail to create the GL context
// on Windows, and switching to borderless full screen later leaves a stale
// surface there, so windows are placed over their monitor after creation.
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
    tex: Option<TextureHandle>,
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
            ready: false,
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
                tex,
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
        let Some(m) = self.monitors.get(idx) else {
            return;
        };
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

        let resp = ui.interact(screen, ui.id().with(("screen", idx)), Sense::drag());
        let pointer = resp.interact_pointer_pos().or(ui.ctx().pointer_hover_pos());
        let local_sel = self
            .sel
            .map(|s| Rect::from_min_max(to_local(s.min), to_local(s.max)));

        // Hover feedback and drag start.
        let hover_kind = pointer.and_then(|p| local_sel.map(|s| edge_hit(s, p)));
        if resp.drag_started()
            && let Some(p) = resp.interact_pointer_pos()
        {
            self.drag = Some(match (hover_kind, self.sel) {
                (Some(Some(edges)), _) => Drag::Resize(edges.0, edges.1, edges.2, edges.3),
                (Some(None), Some(s)) if local_sel.unwrap().contains(p) => {
                    Drag::Move(s.min - to_native(p))
                }
                _ => Drag::New(to_native(p)),
            });
        }
        if resp.dragged()
            && let (Some(d), Some(p)) = (self.drag, resp.interact_pointer_pos())
        {
            let n = to_native(p);
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
        if resp.drag_stopped() {
            self.drag = None;
            if self
                .sel
                .is_some_and(|s| s.width() < 3.0 || s.height() < 3.0)
            {
                self.sel = None;
            }
        }
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
        let dim = Color32::from_black_alpha(140);
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
            painter.rect_stroke(s, 0.0, Stroke::new(1.5, ACCENT), StrokeKind::Outside);
            for c in [
                s.left_top(),
                s.right_top(),
                s.left_bottom(),
                s.right_bottom(),
            ] {
                painter.rect_filled(Rect::from_center_size(c, Vec2::splat(HANDLE)), 1.0, ACCENT);
            }
            let native = self.sel.unwrap();
            let label = format!("{} × {}", native.width().round(), native.height().round());
            let above = s.min.y - 24.0 > screen.min.y;
            let at = if above {
                s.left_top() - Vec2::new(0.0, 6.0)
            } else {
                s.left_top() + Vec2::new(6.0, 6.0)
            };
            let anchor = if above {
                Align2::LEFT_BOTTOM
            } else {
                Align2::LEFT_TOP
            };
            text_badge(&painter, at, anchor, &label);

            if self.drag.is_none() && screen.intersects(s) {
                let below = s.max.y + 44.0 < screen.max.y;
                let y = if below { s.max.y + 8.0 } else { s.max.y - 44.0 };
                let x = s.max.x.min(screen.max.x - 8.0).max(screen.min.x + 190.0);
                let bar = Rect::from_min_size(Pos2::new(x - 182.0, y), Vec2::new(182.0, 34.0));
                let mut done = false;
                let mut cancel = false;
                ui.scope_builder(egui::UiBuilder::new().max_rect(bar), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            done = ui.button("✔ Save  (Enter)").clicked();
                            cancel = ui.button("✖ Esc").clicked();
                        });
                    });
                });
                if done {
                    self.finish();
                }
                if cancel {
                    std::process::exit(1);
                }
            }
        } else {
            text_badge(
                &painter,
                screen.center_top() + Vec2::new(0.0, 40.0),
                Align2::CENTER_TOP,
                "Drag to select an area  ·  Enter to save  ·  Esc to cancel",
            );
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
        if cfg!(target_os = "macos") {
            // Full screen is the only way to cover the menu bar on macOS.
            m.placed = vp.fullscreen == Some(true);
            if !m.placed {
                ctx.send_viewport_cmd(egui::ViewportCommand::SetMonitor(idx));
            }
        } else {
            let ppp = ctx.pixels_per_point();
            let want =
                Rect::from_min_size((m.phys.min.to_vec2() / ppp).to_pos2(), m.phys.size() / ppp);
            m.placed = vp.inner_rect.is_some_and(|r| {
                (r.min - want.min).length() < 0.5 && (r.size() - want.size()).length() < 0.5
            });
            if !m.placed {
                let border = match (vp.outer_rect, vp.inner_rect) {
                    (Some(o), Some(i)) => i.min - o.min,
                    _ => Vec2::ZERO,
                };
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(want.min - border));
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(want.size()));
            }
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

fn text_badge(painter: &egui::Painter, at: Pos2, anchor: Align2, text: &str) {
    let galley =
        painter.layout_no_wrap(text.to_owned(), FontId::proportional(14.0), Color32::WHITE);
    let rect = anchor.anchor_size(at, galley.size() + Vec2::new(12.0, 6.0));
    painter.rect_filled(rect, 4.0, Color32::from_black_alpha(200));
    painter.galley(rect.min + Vec2::new(6.0, 3.0), galley, Color32::WHITE);
}
