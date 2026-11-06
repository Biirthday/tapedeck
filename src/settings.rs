//! User settings changed in the settings panel and saved with the state.
//! Each one is an index into a short list of choices, so old state files
//! and new choices stay compatible.
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// How far `[`, `]` and media seek keys jump: SEEK_STEPS.
    pub seek_step: usize,
    /// Publish the player over MPRIS (status bars, media keys).
    pub mpris: bool,
    /// Tint highlights with the playing cover's color.
    pub accents: bool,
    /// Square panels, boxes and chips instead of rounded ones.
    pub square: bool,
    /// Off, low, normal, high.
    pub reflections: usize,
    pub glass: usize,
    pub glow: usize,
    /// Slow, normal, fast tape animations.
    pub motion: usize,
    /// Relaxed, normal, snappy karaoke fill.
    pub lyric_pace: usize,
    /// Slow, normal, fast autorepeat of held keys.
    pub key_repeat: usize,
    /// Frame rate while a tape plays and nothing moves: 30, 60, full.
    pub playing_fps: usize,
    /// Frame rate when nothing plays or moves: 12, 30, full.
    pub idle_fps: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            seek_step: 1,
            mpris: true,
            accents: true,
            square: false,
            reflections: 2,
            glass: 2,
            glow: 2,
            motion: 1,
            lyric_pace: 1,
            key_repeat: 1,
            playing_fps: 0,
            idle_fps: 0,
        }
    }
}

pub const SEEK_STEPS: [&str; 4] = ["5 S", "10 S", "15 S", "30 S"];
pub const LEVELS: [&str; 4] = ["OFF", "LOW", "NORMAL", "HIGH"];
pub const PACES: [&str; 3] = ["SLOW", "NORMAL", "FAST"];
pub const CORNERS: [&str; 2] = ["ROUNDED", "SQUARE"];
pub const LYRIC_PACES: [&str; 3] = ["RELAXED", "NORMAL", "SNAPPY"];
pub const PLAYING_FPS: [&str; 3] = ["30", "60", "FULL"];
pub const IDLE_FPS: [&str; 3] = ["12", "30", "FULL"];

fn pick<T: Copy>(choices: &[T], k: usize) -> T {
    choices[k.min(choices.len() - 1)]
}

impl Settings {
    pub fn seek_step(&self) -> f32 {
        pick(&[5.0, 10.0, 15.0, 30.0], self.seek_step)
    }

    /// Brightness of the floor reflection right under a tape.
    pub fn reflection(&self) -> f32 {
        pick(&[0.0, 0.15, 0.28, 0.4], self.reflections)
    }

    /// Brightness of the reflections on the tapes' clear plastic.
    pub fn sheen(&self) -> f32 {
        pick(&[0.0, 0.12, 0.22, 0.32], self.glass)
    }

    /// Strength of the cover-colored light in the stage's corner.
    pub fn glow(&self) -> f32 {
        pick(&[0.0, 0.1, 0.2, 0.3], self.glow)
    }

    /// Multiplier for the duration of tape animations.
    pub fn motion(&self) -> f32 {
        pick(&[1.4, 1.0, 0.7], self.motion)
    }

    /// Share of the gap to the next lyric line the fill may take, and the
    /// singing pace in seconds per character.
    pub fn lyric_pace(&self) -> (f32, f32) {
        pick(&[(0.95, 0.11), (0.9, 0.1), (0.8, 0.09)], self.lyric_pace)
    }

    /// Wait before a held key repeats, then the interval, in seconds.
    pub fn key_repeat(&self) -> (f64, f64) {
        pick(&[(0.5, 0.1), (0.35, 0.07), (0.25, 0.045)], self.key_repeat)
    }

    /// Seconds between frames while a tape plays (0: as fast as the display).
    pub fn playing_frame(&self) -> f32 {
        pick(&[1.0 / 30.0, 1.0 / 60.0, 0.0], self.playing_fps)
    }

    /// Seconds between frames when idle (0: as fast as the display).
    pub fn idle_frame(&self) -> f32 {
        pick(&[1.0 / 12.0, 1.0 / 30.0, 0.0], self.idle_fps)
    }
}
