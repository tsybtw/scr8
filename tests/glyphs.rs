//! Every non-ASCII character in UI strings must exist in egui's built-in
//! fonts, otherwise it renders as an empty square.

use eframe::egui;

const UI_FILES: &[&str] = &[
    "src/app.rs",
    "src/overlay.rs",
    "src/hotkey.rs",
    "src/capture/macos.rs",
];

#[test]
fn ui_text_has_no_missing_glyphs() {
    let ctx = egui::Context::default();
    let _ = ctx.run_ui(Default::default(), |_| {});
    let font = egui::FontId::proportional(14.0);
    // `Fonts::has_glyph` reports false negatives for glyphs that live in the
    // same face as the replacement square, so compare rendered glyphs instead.
    let uv = |c: char| {
        let g =
            ctx.fonts_mut(|f| f.layout_no_wrap(c.to_string(), font.clone(), egui::Color32::WHITE));
        g.rows[0]
            .row
            .glyphs
            .first()
            .map(|g| (g.uv_rect.min, g.uv_rect.max))
    };
    let missing = uv('\u{E000}'); // private-use code point, never in a font

    let mut bad = Vec::new();
    for file in UI_FILES {
        let src = std::fs::read_to_string(file).unwrap();
        for (n, line) in src.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for c in line.chars().filter(|c| !c.is_ascii()) {
                if uv(c) == missing {
                    bad.push(format!("{file}:{} '{c}' U+{:04X}", n + 1, c as u32));
                }
            }
        }
    }
    assert!(
        bad.is_empty(),
        "glyphs missing from egui fonts:\n{}",
        bad.join("\n")
    );
}
