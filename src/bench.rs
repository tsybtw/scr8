use std::time::Instant;

use crate::capture;
use crate::config::{PngLevel, Region};
use crate::engine::encode_png;

pub struct Row {
    pub level: PngLevel,
    pub encode_ms: f64,
    pub write_ms: f64,
    pub bytes: usize,
}

pub struct Report {
    pub width: u32,
    pub height: u32,
    pub capture_ms: f64,
    pub rows: Vec<Row>,
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

pub fn run(region: Region) -> Result<Report, String> {
    // Warm-up, then median of several runs.
    let mut frame = capture::capture(region)?;
    let mut times = Vec::new();
    for _ in 0..7 {
        let t = Instant::now();
        frame = capture::capture(region)?;
        times.push(ms(t));
    }
    let capture_ms = median(times);

    let tmp = std::env::temp_dir().join(format!("scr8-bench-{}.png", std::process::id()));
    let mut rows = Vec::new();
    for level in PngLevel::ALL {
        let mut enc = Vec::new();
        let mut wr = Vec::new();
        let mut bytes = 0;
        for _ in 0..3 {
            let t = Instant::now();
            let png = encode_png(&frame, level)?;
            enc.push(ms(t));
            bytes = png.len();
            let t = Instant::now();
            std::fs::write(&tmp, &png).map_err(|e| e.to_string())?;
            wr.push(ms(t));
        }
        rows.push(Row {
            level,
            encode_ms: median(enc),
            write_ms: median(wr),
            bytes,
        });
    }
    let _ = std::fs::remove_file(&tmp);
    Ok(Report {
        width: frame.width,
        height: frame.height,
        capture_ms,
        rows,
    })
}
