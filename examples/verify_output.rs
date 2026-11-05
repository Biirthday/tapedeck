//! Output check: plays each file silently through the player's own audio
//! module and asks the sound server what format the stream arrives in.
//!
//!     cargo run --release --example verify_output -- FILE...
#[path = "../src/audio.rs"]
#[allow(dead_code)]
mod audio;

use std::process::Command;
use std::time::Duration;

fn main() {
    let mut a = audio::Audio::new();
    a.set_volume(0.0);
    for path in std::env::args().skip(1) {
        a.load(std::path::Path::new(&path), Duration::ZERO, true);
        std::thread::sleep(Duration::from_millis(1500));
        let name = path.rsplit('/').next().unwrap_or(&path);
        println!("{name}");
        if let Some(e) = &a.error {
            println!("  error: {e}");
            continue;
        }
        if let Some(o) = a.output {
            println!("  player output: {} Hz, {} ch, native {}, server graph {:?} Hz", o.rate, o.channels, o.native, o.server_rate);
        }
        let streams = Command::new("pactl").args(["list", "sink-inputs", "short"]).output().unwrap();
        for line in String::from_utf8_lossy(&streams.stdout).lines() {
            println!("  server sees: {line}");
        }
    }
    a.stop();
}
