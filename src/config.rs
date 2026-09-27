use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::hotkey::Hotkey;

/// Screen rectangle in the OS's native global coordinates:
/// physical pixels on Windows, points on macOS.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Region {
    pub fn parse(s: &str) -> Option<Self> {
        let mut it = s.split(',').map(|p| p.trim().parse::<i64>().ok());
        let (x, y, w, h) = (it.next()??, it.next()??, it.next()??, it.next()??);
        (w > 0 && h > 0).then_some(Self {
            x: x as i32,
            y: y as i32,
            w: w as u32,
            h: h as u32,
        })
    }

    pub fn to_arg(self) -> String {
        format!("{},{},{},{}", self.x, self.y, self.w, self.h)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Bind {
    pub id: u64,
    pub name: String,
    pub hotkey: Option<Hotkey>,
    pub region: Option<Region>,
    pub folder: Option<PathBuf>,
    /// A disabled bind keeps its settings but registers no hotkeys.
    #[serde(default = "enabled_default")]
    pub enabled: bool,
    /// Opens area editing for this bind from anywhere.
    #[serde(default)]
    pub edit_hotkey: Option<Hotkey>,
}

fn enabled_default() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PngLevel {
    None,
    #[default]
    Fast,
    Balanced,
    Best,
}

impl PngLevel {
    pub const ALL: [PngLevel; 4] = [Self::None, Self::Fast, Self::Balanced, Self::Best];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "No compression",
            Self::Fast => "Fast",
            Self::Balanced => "Balanced",
            Self::Best => "Best",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::None => "About as fast as Fast, but huge files",
            Self::Fast => "Fast save, medium files (recommended)",
            Self::Balanced => "Slower save, noticeably smaller files than Fast",
            Self::Best => "Much slower, files only slightly smaller than Balanced",
        }
    }

    pub fn to_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(v: u8) -> Self {
        Self::ALL.get(v as usize).copied().unwrap_or_default()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    pub binds: Vec<Bind>,
    pub png_level: PngLevel,
    pub autostart: bool,
    pub next_id: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            binds: Vec::new(),
            png_level: PngLevel::default(),
            autostart: true,
            next_id: 0,
        }
    }
}

/// Where settings live. `SCR8_DATA_DIR` points a development build at a
/// separate folder so it never touches the real settings.
pub fn data_dir() -> Option<PathBuf> {
    match std::env::var_os("SCR8_DATA_DIR") {
        Some(dir) => Some(dir.into()),
        None => Some(dirs::config_dir()?.join("scr8")),
    }
}

pub fn is_dev() -> bool {
    std::env::var_os("SCR8_DATA_DIR").is_some()
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        Some(data_dir()?.join("config.json"))
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::path().ok_or("no config directory")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        // Write-then-rename so a crash never leaves a truncated config.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }

    pub fn new_bind(&mut self) -> &mut Bind {
        self.next_id = self
            .next_id
            .max(self.binds.iter().map(|b| b.id).max().unwrap_or(0))
            + 1;
        let n = self.binds.len() + 1;
        self.binds.push(Bind {
            id: self.next_id,
            name: format!("Bind {n}"),
            hotkey: None,
            region: None,
            folder: None,
            enabled: true,
            edit_hotkey: None,
        });
        self.binds.last_mut().unwrap()
    }
}
