use eframe::egui::{Key, Modifiers as EguiMods};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use serde::{Deserialize, Serialize};

/// A key combination as stored in the config. `key` is the label from [`KEYS`].
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// Cmd on macOS, Win on Windows.
    pub meta: bool,
    pub key: String,
}

impl Hotkey {
    /// Builds a hotkey from a key press seen by the settings window.
    /// Returns `None` for keys we can't register or for combos that would
    /// swallow normal typing (a letter without any modifier).
    pub fn from_press(key: Key, mods: EguiMods) -> Option<Self> {
        let (_, _, label) = KEYS.iter().find(|(k, _, _)| *k == key)?;
        let hk = Self {
            ctrl: if cfg!(target_os = "macos") {
                mods.ctrl
            } else {
                mods.ctrl || mods.command
            },
            alt: mods.alt,
            shift: mods.shift,
            meta: if cfg!(target_os = "macos") {
                mods.mac_cmd || mods.command
            } else {
                false
            },
            key: (*label).to_owned(),
        };
        let has_mod = hk.ctrl || hk.alt || hk.meta || hk.shift;
        let is_fkey = label.starts_with('F') && label.len() > 1;
        (has_mod || is_fkey).then_some(hk)
    }

    pub fn to_global(&self) -> Option<HotKey> {
        let code = KEYS.iter().find(|(_, _, l)| *l == self.key)?.1;
        let mut mods = Modifiers::empty();
        mods.set(Modifiers::CONTROL, self.ctrl);
        mods.set(Modifiers::ALT, self.alt);
        mods.set(Modifiers::SHIFT, self.shift);
        mods.set(Modifiers::SUPER, self.meta);
        Some(HotKey::new(Some(mods), code))
    }

    pub fn label(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if cfg!(target_os = "macos") {
            if self.ctrl {
                parts.push("Ctrl");
            }
            if self.alt {
                parts.push("Option");
            }
            if self.shift {
                parts.push("Shift");
            }
            if self.meta {
                parts.push("Cmd");
            }
        } else {
            if self.ctrl {
                parts.push("Ctrl");
            }
            if self.alt {
                parts.push("Alt");
            }
            if self.shift {
                parts.push("Shift");
            }
            if self.meta {
                parts.push("Win");
            }
        }
        parts.push(&self.key);
        parts.join(" + ")
    }
}

#[rustfmt::skip]
const KEYS: &[(Key, Code, &str)] = &[
    (Key::A, Code::KeyA, "A"), (Key::B, Code::KeyB, "B"), (Key::C, Code::KeyC, "C"),
    (Key::D, Code::KeyD, "D"), (Key::E, Code::KeyE, "E"), (Key::F, Code::KeyF, "F"),
    (Key::G, Code::KeyG, "G"), (Key::H, Code::KeyH, "H"), (Key::I, Code::KeyI, "I"),
    (Key::J, Code::KeyJ, "J"), (Key::K, Code::KeyK, "K"), (Key::L, Code::KeyL, "L"),
    (Key::M, Code::KeyM, "M"), (Key::N, Code::KeyN, "N"), (Key::O, Code::KeyO, "O"),
    (Key::P, Code::KeyP, "P"), (Key::Q, Code::KeyQ, "Q"), (Key::R, Code::KeyR, "R"),
    (Key::S, Code::KeyS, "S"), (Key::T, Code::KeyT, "T"), (Key::U, Code::KeyU, "U"),
    (Key::V, Code::KeyV, "V"), (Key::W, Code::KeyW, "W"), (Key::X, Code::KeyX, "X"),
    (Key::Y, Code::KeyY, "Y"), (Key::Z, Code::KeyZ, "Z"),
    (Key::Num0, Code::Digit0, "0"), (Key::Num1, Code::Digit1, "1"), (Key::Num2, Code::Digit2, "2"),
    (Key::Num3, Code::Digit3, "3"), (Key::Num4, Code::Digit4, "4"), (Key::Num5, Code::Digit5, "5"),
    (Key::Num6, Code::Digit6, "6"), (Key::Num7, Code::Digit7, "7"), (Key::Num8, Code::Digit8, "8"),
    (Key::Num9, Code::Digit9, "9"),
    (Key::F1, Code::F1, "F1"), (Key::F2, Code::F2, "F2"), (Key::F3, Code::F3, "F3"),
    (Key::F4, Code::F4, "F4"), (Key::F5, Code::F5, "F5"), (Key::F6, Code::F6, "F6"),
    (Key::F7, Code::F7, "F7"), (Key::F8, Code::F8, "F8"), (Key::F9, Code::F9, "F9"),
    (Key::F10, Code::F10, "F10"), (Key::F11, Code::F11, "F11"), (Key::F12, Code::F12, "F12"),
    (Key::F13, Code::F13, "F13"), (Key::F14, Code::F14, "F14"), (Key::F15, Code::F15, "F15"),
    (Key::F16, Code::F16, "F16"), (Key::F17, Code::F17, "F17"), (Key::F18, Code::F18, "F18"),
    (Key::F19, Code::F19, "F19"), (Key::F20, Code::F20, "F20"),
    (Key::Space, Code::Space, "Space"), (Key::Enter, Code::Enter, "Enter"),
    (Key::Tab, Code::Tab, "Tab"), (Key::Backspace, Code::Backspace, "Backspace"),
    (Key::Insert, Code::Insert, "Insert"), (Key::Delete, Code::Delete, "Delete"),
    (Key::Home, Code::Home, "Home"), (Key::End, Code::End, "End"),
    (Key::PageUp, Code::PageUp, "PageUp"), (Key::PageDown, Code::PageDown, "PageDown"),
    (Key::ArrowUp, Code::ArrowUp, "Up"), (Key::ArrowDown, Code::ArrowDown, "Down"),
    (Key::ArrowLeft, Code::ArrowLeft, "Left"), (Key::ArrowRight, Code::ArrowRight, "Right"),
    (Key::Minus, Code::Minus, "-"), (Key::Equals, Code::Equal, "="),
    (Key::Comma, Code::Comma, ","), (Key::Period, Code::Period, "."),
    (Key::Semicolon, Code::Semicolon, ";"), (Key::Quote, Code::Quote, "'"),
    (Key::Slash, Code::Slash, "/"), (Key::Backslash, Code::Backslash, "\\"),
    (Key::OpenBracket, Code::BracketLeft, "["), (Key::CloseBracket, Code::BracketRight, "]"),
    (Key::Backtick, Code::Backquote, "`"),
];
