//! Decoder fidelity check: decodes each file through rodio/symphonia (the
//! player's own path) and through ffmpeg at the file's native rate and
//! channel count, then compares the PCM sample by sample.
//!
//!     cargo run --release --example verify_decode -- FILE...
use std::fs::File;
use std::process::Command;

use rodio::{Decoder, Source};

fn ffmpeg_pcm(path: &str) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i", path, "-map", "0:a:0", "-f", "f32le", "-acodec", "pcm_f32le", "-"])
        .output()
        .expect("ffmpeg");
    out.stdout.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
}

fn peak(v: &[f32]) -> f32 {
    v.iter().fold(0.0, |m, s| m.max(s.abs()))
}

/// Frame offset that best lines `a` up with `b` (decoders may trim encoder
/// delay differently for lossy formats).
fn best_offset(a: &[f32], b: &[f32], channels: usize) -> isize {
    let (start, len, reach) = (20_000 * channels, 8192 * channels, 3000isize);
    if a.len() < start + len + 2 * reach as usize * channels || b.len() < start + len + 2 * reach as usize * channels {
        return 0;
    }
    let err = |o: isize| -> f64 {
        (0..len).map(|k| (a[start + k] - b[(start as isize + k as isize + o * channels as isize) as usize]).abs() as f64).sum()
    };
    let best = (-reach..=reach).min_by(|&x, &y| err(x).total_cmp(&err(y))).unwrap();
    // Silence fits every offset equally; only move when it clearly helps.
    if err(best) < err(0) * 0.5 { best } else { 0 }
}

fn main() {
    for path in std::env::args().skip(1) {
        let dec = match Decoder::try_from(File::open(&path).unwrap()) {
            Ok(d) => d,
            Err(e) => {
                println!("FAIL  {path}: {e}");
                continue;
            }
        };
        let (rate, channels) = (dec.sample_rate().get(), dec.channels().get() as usize);
        let ours: Vec<f32> = dec.map(|s| s as f32).collect();
        let theirs = ffmpeg_pcm(&path);
        let offset = best_offset(&ours, &theirs, channels);
        let shift = offset * channels as isize;
        let pairs = ours.iter().enumerate().filter_map(|(k, &a)| {
            let j = k as isize + shift;
            (j >= 0 && (j as usize) < theirs.len()).then(|| (a, theirs[j as usize]))
        });
        let (mut n, mut differing, mut max_diff) = (0usize, 0usize, 0f32);
        for (a, b) in pairs {
            n += 1;
            let d = (a - b).abs();
            if d > 0.0 {
                differing += 1;
            }
            max_diff = max_diff.max(d);
        }
        let name = path.rsplit('/').next().unwrap_or(&path);
        println!(
            "{name}\n  rodio: {rate} Hz, {channels} ch, {} frames, peak {:.4}\n  ffmpeg: {} frames, peak {:.4}\n  offset {offset} frames, compared {n} samples, differing {differing}, max diff {max_diff:.2e} ({:.1} dBFS)",
            ours.len() / channels.max(1),
            peak(&ours),
            theirs.len() / channels.max(1),
            peak(&theirs),
            if max_diff > 0.0 { 20.0 * max_diff.log10() } else { f32::NEG_INFINITY },
        );
    }
}
