//! Hotkey → capture → save pipeline.
//!
//! One dedicated thread does nothing but grab raw pixels, so a burst of key
//! presses is never lost; PNG encoding and disk writes run on a worker pool.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;

use crossbeam_channel::{Receiver, Sender, unbounded};
use global_hotkey::{GlobalHotKeyEvent, HotKeyState};

use crate::capture::{self, Frame};
use crate::config::{PngLevel, Region};

#[derive(Clone)]
pub struct Target {
    pub name: String,
    pub region: Region,
    pub folder: PathBuf,
}

struct Job {
    frame: Frame,
    path: PathBuf,
    level: PngLevel,
}

#[derive(Clone)]
pub struct Engine {
    /// Hotkey id → what to capture. Swapped wholesale when binds change.
    pub targets: Arc<RwLock<HashMap<u32, Target>>>,
    pub level: Arc<AtomicU8>,
    pub saved: Arc<AtomicU64>,
    pub last_error: Arc<Mutex<Option<String>>>,
    /// Bumped on every failure so the UI can tell new errors from old ones.
    pub errors: Arc<AtomicU64>,
    /// Wakes the UI thread (even while its window is hidden).
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Engine {
    pub fn start(
        level: PngLevel,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> (Self, Sender<GlobalHotKeyEvent>) {
        let engine = Self {
            targets: Arc::default(),
            level: Arc::new(AtomicU8::new(level.to_u8())),
            saved: Arc::default(),
            last_error: Arc::default(),
            errors: Arc::default(),
            wake: Arc::new(wake),
        };
        let (job_tx, job_rx) = unbounded::<Job>();
        let workers = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 8);
        for i in 0..workers {
            let rx = job_rx.clone();
            let e = engine.clone();
            thread::Builder::new()
                .name(format!("scr8-encode-{i}"))
                .spawn(move || e.encode_loop(rx))
                .expect("spawn encoder");
        }

        let (key_tx, key_rx) = unbounded::<GlobalHotKeyEvent>();
        let e = engine.clone();
        thread::Builder::new()
            .name("scr8-capture".into())
            .spawn(move || e.capture_loop(key_rx, job_tx))
            .expect("spawn capture");
        (engine, key_tx)
    }

    pub fn set_level(&self, level: PngLevel) {
        self.level.store(level.to_u8(), Ordering::Relaxed);
    }

    fn capture_loop(&self, keys: Receiver<GlobalHotKeyEvent>, jobs: Sender<Job>) {
        let mut names = NameGen::default();
        // Windows registers hotkeys with MOD_NOREPEAT, so holding a combo
        // already yields a single event. macOS needs explicit tracking.
        #[cfg(target_os = "macos")]
        let mut held = std::collections::HashSet::new();
        for ev in keys {
            #[cfg(target_os = "macos")]
            {
                if ev.state == HotKeyState::Released {
                    held.remove(&ev.id);
                    continue;
                }
                if !held.insert(ev.id) {
                    continue;
                }
            }
            if ev.state != HotKeyState::Pressed {
                continue;
            }
            let Some(target) = self.targets.read().unwrap().get(&ev.id).cloned() else {
                continue;
            };
            match capture::capture(target.region) {
                Ok(frame) => {
                    let path = names.next(&target.folder, &target.name);
                    let level = PngLevel::from_u8(self.level.load(Ordering::Relaxed));
                    let _ = jobs.send(Job { frame, path, level });
                }
                Err(e) => self.report(e),
            }
        }
    }

    fn encode_loop(&self, jobs: Receiver<Job>) {
        for job in jobs {
            let result = encode_png(&job.frame, job.level).and_then(|png| {
                write_file(&job.path, &png).map_err(|e| {
                    let dir = job.path.parent().unwrap_or(&job.path);
                    format!("Can't save to {}: {e}", dir.display())
                })
            });
            match result {
                Ok(()) => {
                    self.saved.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => self.report(e),
            }
        }
    }

    fn report(&self, e: String) {
        *self.last_error.lock().unwrap() = Some(e);
        self.errors.fetch_add(1, Ordering::Relaxed);
        (self.wake)();
    }
}

fn write_file(path: &Path, data: &[u8]) -> std::io::Result<()> {
    match std::fs::write(path, data) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, data)
        }
        r => r,
    }
}

pub fn encode_png(frame: &Frame, level: PngLevel) -> Result<Vec<u8>, String> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let mut rgb = Vec::with_capacity(w * h * 3);
    for px in frame.bgrx.as_chunks::<4>().0 {
        rgb.extend_from_slice(&[px[2], px[1], px[0]]);
    }
    let mut out = Vec::with_capacity(w * h + 1024);
    let mut enc = png::Encoder::new(&mut out, frame.width, frame.height);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(match level {
        PngLevel::None => png::Compression::NoCompression,
        PngLevel::Fast => png::Compression::Fast,
        PngLevel::Balanced => png::Compression::Balanced,
        PngLevel::Best => png::Compression::High,
    });
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(&rgb).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

/// Unique, sortable file names even for many shots within one millisecond.
#[derive(Default)]
struct NameGen {
    last: String,
    seq: u32,
}

impl NameGen {
    fn next(&mut self, folder: &Path, bind: &str) -> PathBuf {
        let stamp = chrono::Local::now()
            .format("%Y-%m-%d_%H-%M-%S-%3f")
            .to_string();
        if stamp == self.last {
            self.seq += 1;
        } else {
            self.last = stamp.clone();
            self.seq = 0;
        }
        let prefix = sanitize(bind);
        let name = if self.seq == 0 {
            format!("{prefix}_{stamp}.png")
        } else {
            format!("{prefix}_{stamp}_{}.png", self.seq)
        };
        folder.join(name)
    }
}

fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let s = s.trim().trim_end_matches('.').to_owned();
    if s.is_empty() { "shot".into() } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_within_a_millisecond() {
        let mut g = NameGen::default();
        let dir = Path::new("x");
        let names: std::collections::HashSet<_> = (0..1000).map(|_| g.next(dir, "a:b")).collect();
        assert_eq!(names.len(), 1000);
        assert!(names.iter().all(|p| p.to_string_lossy().contains("a_b_")));
    }

    /// Needs a real screen (and Screen Recording permission on macOS):
    /// `cargo test --release -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_primary_area() {
        let r = Region {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let rep = crate::bench::run(r).expect("bench");
        println!(
            "capture {:.2} ms for {}x{}",
            rep.capture_ms, rep.width, rep.height
        );
        for row in rep.rows {
            println!(
                "{:?}: encode {:.1} ms, write {:.1} ms, {} KB",
                row.level,
                row.encode_ms,
                row.write_ms,
                row.bytes / 1024
            );
        }
    }
}
