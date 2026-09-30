//! Shared look of the area picker, so the GDI version on Windows and the
//! egui version on macOS match. Sizes are in points (scaled by the display).

pub type Rgb = (u8, u8, u8);

pub const ACCENT: Rgb = (64, 156, 255);
/// Black over everything outside the selection.
pub const DIM_ALPHA: u8 = 140;
pub const BORDER: f32 = 1.5;
pub const HANDLE: f32 = 8.0;

/// Size label and hint: white text on translucent black.
pub const BADGE_FONT: f32 = 14.0;
pub const BADGE_ALPHA: u8 = 200;
pub const BADGE_PAD: (f32, f32) = (6.0, 3.0);
pub const BADGE_RADIUS: f32 = 4.0;
pub const BADGE_GAP: f32 = 6.0;
pub const HINT_TOP: f32 = 40.0;

/// Save / Esc buttons in a small panel under the selection.
pub const BUTTON_FONT: f32 = 13.0;
pub const PANEL_FILL: Rgb = (27, 27, 27);
pub const PANEL_STROKE: Rgb = (60, 60, 60);
pub const PANEL_RADIUS: f32 = 6.0;
pub const PANEL_PAD: f32 = 6.0;
pub const PANEL_GAP: f32 = 8.0;
pub const BUTTON_FILL: Rgb = (60, 60, 60);
pub const BUTTON_FILL_HOVER: Rgb = (70, 70, 70);
pub const BUTTON_TEXT: Rgb = (180, 180, 180);
pub const BUTTON_TEXT_HOVER: Rgb = (240, 240, 240);
pub const BUTTON_RADIUS: f32 = 3.0;
pub const BUTTON_PAD: (f32, f32) = (8.0, 4.0);
pub const BUTTON_GAP: f32 = 6.0;

pub const SAVE_ICON: &str = "✔";
pub const SAVE_LABEL: &str = "Save  (Enter)";
pub const CANCEL_ICON: &str = "✖";
pub const CANCEL_LABEL: &str = "Esc";
pub const HINT: &str = "Drag to select an area  ·  Enter to save  ·  Esc to cancel";
