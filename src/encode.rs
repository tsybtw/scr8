//! PNG encoding.
//!
//! The compressed levels split the image into horizontal stripes that are
//! filtered and deflated on all cores at once, then joined into one ordinary
//! zlib stream: each stripe ends on a byte boundary (sync flush) and only the
//! last one is final, so any PNG reader decodes the result. On a 2560x1440
//! screenshot this made "Fast" about 2x faster than the single-threaded
//! fdeflate path with smaller files, and "Best" about 6x faster.
//! "No compression" still goes through the `png` crate.

use std::thread;

use crate::capture::Frame;
use crate::config::PngLevel;

pub fn encode_png(frame: &Frame, level: PngLevel) -> Result<Vec<u8>, String> {
    match level {
        PngLevel::None => encode_stored(frame),
        PngLevel::Fast => Ok(encode_parallel(frame, 4)),
        PngLevel::Balanced => Ok(encode_parallel(frame, 6)),
        PngLevel::Best => Ok(encode_parallel(frame, 9)),
    }
}

fn encode_stored(frame: &Frame) -> Result<Vec<u8>, String> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let mut rgb = vec![0u8; w * h * 3];
    bgrx_to_rgb(&frame.bgrx, &mut rgb);
    let mut out = Vec::with_capacity(w * h + 1024);
    let mut enc = png::Encoder::new(&mut out, frame.width, frame.height);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::NoCompression);
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(&rgb).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

/// Converts 4-byte BGRX pixels to 3-byte RGB.
pub fn bgrx_to_rgb(src: &[u8], dst: &mut [u8]) {
    for (d, s) in dst
        .as_chunks_mut::<3>()
        .0
        .iter_mut()
        .zip(src.as_chunks::<4>().0)
    {
        *d = [s[2], s[1], s[0]];
    }
}

fn encode_parallel(frame: &Frame, level: u32) -> Vec<u8> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let threads = thread::available_parallelism().map_or(4, |n| n.get());
    // Stripes of at least 32 rows; tiny images end up as a single stripe.
    let stripes = threads.min(h.div_ceil(32)).max(1);
    let rows_per = h.div_ceil(stripes);

    let parts: Vec<(Vec<u8>, u32, usize)> = thread::scope(|s| {
        let handles: Vec<_> = (0..stripes)
            .map(|i| {
                let (y0, y1) = (i * rows_per, ((i + 1) * rows_per).min(h));
                let last = y1 == h;
                s.spawn(move || compress_stripe(frame, y0, y1, level, last))
            })
            .collect();
        handles.into_iter().map(|t| t.join().unwrap()).collect()
    });

    let mut adler = 1u32;
    let mut deflated = Vec::with_capacity(parts.iter().map(|p| p.0.len()).sum());
    for (data, part_adler, len) in &parts {
        adler = adler32_combine(adler, *part_adler, *len);
        deflated.extend_from_slice(data);
    }

    let mut idat = Vec::with_capacity(deflated.len() + 6);
    // zlib header: deflate, 32K window; level hint "default" or "best".
    idat.extend_from_slice(if level >= 9 {
        &[0x78, 0xDA]
    } else {
        &[0x78, 0x9C]
    });
    idat.extend_from_slice(&deflated);
    idat.extend_from_slice(&adler.to_be_bytes());

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    // 8-bit RGB, deflate, adaptive filtering, no interlace.
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);

    let mut out = Vec::with_capacity(idat.len() + 64);
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    write_chunk(&mut out, b"IHDR", &ihdr);
    write_chunk(&mut out, b"IDAT", &idat);
    write_chunk(&mut out, b"IEND", &[]);
    out
}

/// Filters rows `y0..y1` and deflates them as a raw stream piece.
/// Returns the compressed bytes, the Adler-32 of the filtered data and its
/// length (for combining the checksums).
fn compress_stripe(
    frame: &Frame,
    y0: usize,
    y1: usize,
    level: u32,
    last: bool,
) -> (Vec<u8>, u32, usize) {
    let w = frame.width as usize;
    let row = w * 3;
    let src_row = |y: usize| &frame.bgrx[y * w * 4..(y + 1) * w * 4];

    let mut prev = vec![0u8; row];
    if y0 > 0 {
        bgrx_to_rgb(src_row(y0 - 1), &mut prev);
    }
    let mut cur = vec![0u8; row];
    let mut filtered = Vec::with_capacity((y1 - y0) * (row + 1));
    let mut candidate = vec![0u8; row];
    let mut best = vec![0u8; row];
    for y in y0..y1 {
        bgrx_to_rgb(src_row(y), &mut cur);
        let kind = filter_row(&cur, &prev, &mut candidate, &mut best);
        filtered.push(kind);
        filtered.extend_from_slice(&best);
        std::mem::swap(&mut prev, &mut cur);
    }

    let adler = simd_adler32::adler32(&filtered.as_slice());
    let mut z = flate2::Compress::new(flate2::Compression::new(level), false);
    let mut out = Vec::with_capacity(filtered.len() / 2 + 1024);
    let flush = if last {
        flate2::FlushCompress::Finish
    } else {
        flate2::FlushCompress::Sync
    };
    loop {
        let consumed = z.total_in() as usize;
        if out.capacity() - out.len() < 1024 {
            out.reserve(out.capacity().max(4096));
        }
        let status = z
            .compress_vec(&filtered[consumed..], &mut out, flush)
            .expect("deflate");
        let done = z.total_in() as usize == filtered.len();
        match status {
            flate2::Status::StreamEnd => break,
            // A sync flush is complete once all input is in and there was
            // room left over for the flush marker.
            _ if !last && done && out.capacity() - out.len() > 0 => break,
            _ => {}
        }
    }
    (out, adler, filtered.len())
}

/// Picks the PNG filter with the smallest sum of absolute values (the usual
/// heuristic), leaving the filtered row in `best`.
fn filter_row(cur: &[u8], prev: &[u8], candidate: &mut [u8], best: &mut [u8]) -> u8 {
    let score = |r: &[u8]| {
        r.iter()
            .map(|&b| (b as i8).unsigned_abs() as u32)
            .sum::<u32>()
    };
    let mut best_kind = 0u8;
    best.copy_from_slice(cur);
    let mut best_score = score(best);
    for kind in 1..=4u8 {
        // A flat row can't do better than zero.
        if best_score == 0 {
            break;
        }
        apply_filter(kind, cur, prev, candidate);
        let s = score(candidate);
        if s < best_score {
            best_score = s;
            best_kind = kind;
            best.copy_from_slice(candidate);
        }
    }
    best_kind
}

/// Writes `cur` filtered with PNG filter `kind` (1–4) into `out`.
/// The first pixel has no left neighbour and is handled apart, so the main
/// loops are branch-free and vectorize.
fn apply_filter(kind: u8, cur: &[u8], prev: &[u8], out: &mut [u8]) {
    const BPP: usize = 3;
    let n = cur.len().min(BPP);
    let (head, tail) = out.split_at_mut(n);
    let left = &cur[..cur.len() - n];
    let (up_head, up) = prev.split_at(n);
    let up_left = &prev[..prev.len() - n];
    let rest = &cur[n..];
    match kind {
        1 => {
            head.copy_from_slice(&cur[..n]);
            for ((o, &x), &a) in tail.iter_mut().zip(rest).zip(left) {
                *o = x.wrapping_sub(a);
            }
        }
        2 => {
            for ((o, &x), &b) in out.iter_mut().zip(cur).zip(prev) {
                *o = x.wrapping_sub(b);
            }
        }
        3 => {
            for ((o, &x), &b) in head.iter_mut().zip(&cur[..n]).zip(up_head) {
                *o = x.wrapping_sub(b / 2);
            }
            for (((o, &x), &a), &b) in tail.iter_mut().zip(rest).zip(left).zip(up) {
                *o = x.wrapping_sub(((a as u16 + b as u16) / 2) as u8);
            }
        }
        _ => {
            // Paeth with no left neighbour predicts the pixel above.
            for ((o, &x), &b) in head.iter_mut().zip(&cur[..n]).zip(up_head) {
                *o = x.wrapping_sub(b);
            }
            for ((((o, &x), &a), &b), &c) in
                tail.iter_mut().zip(rest).zip(left).zip(up).zip(up_left)
            {
                *o = x.wrapping_sub(paeth(a, b, c));
            }
        }
    }
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = (
        (p - a as i16).abs(),
        (p - b as i16).abs(),
        (p - c as i16).abs(),
    );
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Adler-32 of `A ++ B` from the checksums of both parts and `B`'s length.
fn adler32_combine(adler_a: u32, adler_b: u32, len_b: usize) -> u32 {
    const MOD: u64 = 65521;
    let rem = (len_b as u64) % MOD;
    let (a1, b1) = (adler_a as u64 & 0xFFFF, adler_a as u64 >> 16);
    let (a2, b2) = (adler_b as u64 & 0xFFFF, adler_b as u64 >> 16);
    let a = (a1 + a2 + MOD - 1) % MOD;
    let b = (b1 + b2 + rem * a1 % MOD + MOD - rem) % MOD;
    ((b << 16) | a) as u32
}

fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = crc32fast::Hasher::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(&crc.finalize().to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A screenshot-like test image: flat areas, gradients and noise.
    fn sample(w: u32, h: u32) -> Frame {
        let mut bgrx = Vec::with_capacity((w * h * 4) as usize);
        let mut seed = 12345u32;
        for y in 0..h {
            for x in 0..w {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                let noise = (seed >> 24) as u8;
                let px = if y < h / 3 {
                    [30, 30, 30, 0]
                } else if y < 2 * h / 3 {
                    [(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8, 0]
                } else {
                    [noise, noise.wrapping_mul(3), noise ^ 0x5A, 0]
                };
                bgrx.extend_from_slice(&px);
            }
        }
        Frame {
            width: w,
            height: h,
            bgrx,
        }
    }

    fn decode(png_bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let dec = png::Decoder::new(std::io::Cursor::new(png_bytes));
        let mut reader = dec.read_info().expect("valid png");
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).expect("decodes");
        assert_eq!(info.color_type, png::ColorType::Rgb);
        buf.truncate(info.buffer_size());
        (info.width, info.height, buf)
    }

    #[test]
    fn every_level_round_trips_exactly() {
        // Odd sizes, a one-row image and one that splits into many stripes.
        for (w, h) in [(1, 1), (7, 3), (333, 1), (640, 481), (1280, 1000)] {
            let frame = sample(w, h);
            let mut expected = vec![0u8; (w * h * 3) as usize];
            bgrx_to_rgb(&frame.bgrx, &mut expected);
            for level in PngLevel::ALL {
                let bytes = encode_png(&frame, level).unwrap();
                let (dw, dh, pixels) = decode(&bytes);
                assert_eq!((dw, dh), (w, h), "{level:?} {w}x{h}");
                assert!(pixels == expected, "{level:?} {w}x{h}: pixels differ");
            }
        }
    }

    #[test]
    fn adler_combine_matches_direct() {
        let data: Vec<u8> = (0..100_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let (a, b) = data.split_at(37_123);
        let combined = adler32_combine(
            simd_adler32::adler32(&a),
            simd_adler32::adler32(&b),
            b.len(),
        );
        assert_eq!(combined, simd_adler32::adler32(&data.as_slice()));
    }
}
