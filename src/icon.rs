//! The app icon, drawn in code: corner brackets of a selection frame.
//! Dependency-free so `build.rs` can reuse it for the Windows .exe icon.

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// White brackets on a blue rounded square.
    App,
    /// Same on red: a screenshot failed.
    Error,
    /// Black brackets on transparent, for the macOS menu bar.
    Template,
}

pub fn rgba(size: u32, style: Style) -> Vec<u8> {
    let s = size as f32;
    let mut out = vec![0u8; (size * size * 4) as usize];
    let radius = s * 0.22;
    let (inset, arm, thick) = (s * 0.22, s * 0.2, (s * 0.09).max(2.0));
    let (glyph_px, bg_px) = match style {
        Style::App => ([255, 255, 255, 255], Some([38, 132, 255, 255])),
        Style::Error => ([255, 255, 255, 255], Some([225, 55, 50, 255])),
        Style::Template => ([0, 0, 0, 255], None),
    };
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let i = ((y * size + x) * 4) as usize;
            let px = if bracket(fx, fy, s, inset, arm, thick) {
                glyph_px
            } else {
                bg_px
                    .filter(|_| in_rounded(fx, fy, s, radius))
                    .unwrap_or_default()
            };
            out[i..i + 4].copy_from_slice(&px);
        }
    }
    out
}

fn in_rounded(x: f32, y: f32, s: f32, r: f32) -> bool {
    let cx = x.clamp(r, s - r);
    let cy = y.clamp(r, s - r);
    (x - cx).powi(2) + (y - cy).powi(2) <= r * r
}

fn bracket(x: f32, y: f32, s: f32, inset: f32, arm: f32, t: f32) -> bool {
    let (lo, hi) = (inset, s - inset);
    let near = |v: f32, edge: f32| (v - edge).abs() <= t / 2.0;
    let within = |v: f32, edge: f32, dir: f32| {
        let d = (v - edge) * dir;
        (-t / 2.0..=arm).contains(&d)
    };
    [(lo, 1.0), (hi, -1.0)].iter().any(|&(ex, dx)| {
        [(lo, 1.0), (hi, -1.0)].iter().any(|&(ey, dy)| {
            (near(x, ex) && within(y, ey, dy)) || (near(y, ey) && within(x, ex, dx))
        })
    })
}
