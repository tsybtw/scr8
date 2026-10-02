//! Self-test: `SCR8_SELFTEST=<dir>` runs a scripted check (used by CI on
//! real Windows and macOS machines) and writes screenshots and a report
//! there. Only compiled with `--features selftest`, so releases don't have it.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

use global_hotkey::{GlobalHotKeyEvent, HotKeyState};

use super::{Service, Wake, Waker};
use crate::ipc::ToSettings;
use crate::{capture, picker};

pub(super) enum Step {
    Shoot,
    OpenSettings,
    /// Unload the settings window, as closing it does with "keep loaded" off.
    CloseSettings,
    Snap(&'static str),
    EditArea,
    ClosePicker,
    Finish,
}

pub(super) fn start(wake: Waker) {
    let Some(dir) = std::env::var_os("SCR8_SELFTEST").map(PathBuf::from) else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    std::thread::spawn(move || {
        let step = |s, wait_ms| {
            wake(Wake::SelfTest(s));
            std::thread::sleep(Duration::from_millis(wait_ms));
        };
        std::thread::sleep(Duration::from_secs(2));
        step(Step::Shoot, 1500);
        step(Step::OpenSettings, 4000);
        step(Step::Snap("settings.png"), 500);
        step(Step::EditArea, 3000);
        step(Step::Snap("picker.png"), 500);
        step(Step::ClosePicker, 1500);
        // Unload the window and open a fresh one.
        step(Step::CloseSettings, 2000);
        step(Step::OpenSettings, 4000);
        step(Step::Snap("settings-reopened.png"), 500);
        step(Step::Finish, 0);
    });
}

impl Service {
    pub(super) fn self_test_step(&mut self, step: Step) {
        let Some(dir) = std::env::var_os("SCR8_SELFTEST").map(PathBuf::from) else {
            return;
        };
        match step {
            Step::Shoot => {
                // As if the first bind's hotkey was pressed.
                if let Some(bind) = self.cfg.binds.first()
                    && let Some(&id) = self.shot_ids.get(&bind.id)
                {
                    for state in [HotKeyState::Pressed, HotKeyState::Released] {
                        let _ = self.key_tx.send(GlobalHotKeyEvent { id, state });
                    }
                }
            }
            Step::OpenSettings => self.open_settings(),
            Step::CloseSettings => {
                if let Some(link) = &self.link {
                    link.send(&ToSettings::Exit);
                }
            }
            Step::Snap(name) => {
                let path = dir.join(name);
                let result = capture::capture(capture::primary_region())
                    .and_then(|f| crate::encode::encode_png(&f, crate::config::PngLevel::Fast))
                    .and_then(|png| std::fs::write(&path, png).map_err(|e| e.to_string()));
                if let Err(e) = result {
                    let _ = std::fs::write(dir.join(format!("{name}.error.txt")), e);
                }
            }
            Step::EditArea => {
                if let Some(id) = self.cfg.binds.first().map(|b| b.id) {
                    self.edit_area(id);
                }
            }
            Step::ClosePicker => picker::cancel(),
            Step::Finish => {
                let report = serde_json::json!({
                    "saved": self.engine.saved.load(Ordering::Relaxed),
                    "last_error": self.engine.last_error.lock().unwrap().clone(),
                    "bind_errors": self.bind_errors,
                    "hotkeys_available": self.manager.is_some(),
                    "settings_connected": self.link.is_some(),
                    "settings_running": self.settings_alive(),
                    "service_pid": std::process::id(),
                    "tray": self.tray.is_some(),
                    "permission": capture::ensure_permission(),
                    "capture": capture::backend_status(),
                });
                let _ = std::fs::write(dir.join("report.json"), report.to_string());
                self.quit = true;
            }
        }
    }
}
