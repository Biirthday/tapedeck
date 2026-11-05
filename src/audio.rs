//! Playback through rodio, with a level meter feeding the speaker grille.
use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use rodio::buffer::SamplesBuffer;
use rodio::source::SeekError;
use rodio::cpal::SampleFormat;
use rodio::{ChannelCount, Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Sample, SampleRate, Source};

/// Mean absolute amplitude per channel (left, right), published every few
/// thousand samples. Mono sources feed both sides.
struct Meter<S> {
    inner: S,
    levels: Arc<[AtomicU32; 2]>,
    channels: u16,
    acc: [f32; 2],
    n: u32,
}

impl<S: Source> Iterator for Meter<S> {
    type Item = Sample;

    fn next(&mut self) -> Option<Sample> {
        let s = self.inner.next()?;
        let side = if self.channels < 2 { 0 } else { (self.n % self.channels as u32).min(1) as usize };
        self.acc[side] += (s as f32).abs();
        if self.channels < 2 {
            self.acc[1] += (s as f32).abs();
        }
        self.n += 1;
        if self.n >= 4096 {
            let per_side = (self.n / self.channels.clamp(1, 2) as u32) as f32;
            for k in 0..2 {
                self.levels[k].store((self.acc[k] / per_side).to_bits(), Ordering::Relaxed);
            }
            self.acc = [0.0; 2];
            self.n = 0;
        }
        Some(s)
    }
}

impl<S: Source> Source for Meter<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }
    fn channels(&self) -> ChannelCount {
        self.inner.channels()
    }
    fn sample_rate(&self) -> SampleRate {
        self.inner.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        self.inner.try_seek(pos)
    }
}

/// Mechanical deck noises, synthesized rather than shipped as files.
#[derive(Clone, Copy)]
pub enum Sfx {
    /// A transport key going down and coming back up.
    Key,
    /// A cassette pushed into the deck and latched.
    Insert,
    /// The deck letting the cassette go.
    Eject,
}

/// One mechanical click: a short burst of noise for the contact and a
/// decaying low sine for the body of the plastic.
fn click(out: &mut [f32], rate: f32, at: f32, noise: f32, tone: f32, hz: f32, seed: &mut u32) {
    let start = (at * rate) as usize;
    for (k, s) in out.iter_mut().skip(start).enumerate() {
        let t = k as f32 / rate;
        // xorshift: deterministic noise, the same click every time.
        *seed ^= *seed << 13;
        *seed ^= *seed >> 17;
        *seed ^= *seed << 5;
        let white = (*seed as f32 / u32::MAX as f32) * 2.0 - 1.0;
        *s += white * noise * (-t / 0.0012).exp() + (t * hz * std::f32::consts::TAU).sin() * tone * (-t / 0.014).exp();
    }
}

fn synth(sfx: Sfx, rate: f32) -> Vec<f32> {
    let mut out = vec![0.0; (rate * 0.32) as usize];
    let mut seed = 0x9e37_79b9;
    match sfx {
        Sfx::Key => {
            click(&mut out, rate, 0.0, 0.55, 0.35, 150.0, &mut seed);
            click(&mut out, rate, 0.075, 0.22, 0.12, 190.0, &mut seed);
        }
        Sfx::Insert => {
            click(&mut out, rate, 0.0, 0.35, 0.55, 95.0, &mut seed);
            click(&mut out, rate, 0.11, 0.7, 0.4, 130.0, &mut seed);
            click(&mut out, rate, 0.17, 0.25, 0.15, 170.0, &mut seed);
        }
        Sfx::Eject => {
            click(&mut out, rate, 0.0, 0.6, 0.35, 120.0, &mut seed);
            click(&mut out, rate, 0.06, 0.3, 0.5, 80.0, &mut seed);
        }
    }
    out
}

/// What the output stream runs at, and whether it matches the track.
#[derive(Clone, Copy, PartialEq)]
pub struct Output {
    pub rate: u32,
    pub channels: u16,
    /// False when the device refused the track's format and rodio's own
    /// (linear) converter has to bridge the gap.
    pub native: bool,
    /// Rate the sound server's graph runs at for this stream, when it can be
    /// read (PipeWire). It differs from `rate` when the server resamples.
    pub server_rate: Option<u32>,
}

/// PipeWire's graph clock: the current rate and the rates it may switch to.
fn pipewire_clock() -> Option<(u32, Vec<u32>)> {
    let out = std::process::Command::new("pw-metadata").args(["-n", "settings"]).output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let value = |key: &str| {
        text.lines()
            .find(|l| l.contains(&format!("key:'{key}'")))
            .and_then(|l| l.split("value:'").nth(1))
            .and_then(|v| v.split('\'').next())
            .map(str::to_string)
    };
    let force: u32 = value("clock.force-rate").and_then(|v| v.parse().ok()).unwrap_or(0);
    let rate: u32 = value("clock.rate")?.parse().ok()?;
    let allowed = value("clock.allowed-rates")
        .map(|v| v.trim_matches(|c| c == '[' || c == ']').split_whitespace().filter_map(|r| r.trim_matches(',').parse().ok()).collect())
        .unwrap_or_else(|| vec![rate]);
    Some((if force > 0 { force } else { rate }, if force > 0 { vec![force] } else { allowed }))
}

pub struct Audio {
    // Fields drop in order: the player must go before the sink it feeds.
    player: Option<Player>,
    sink: Option<MixerDeviceSink>,
    levels: Arc<[AtomicU32; 2]>,
    loaded: bool,
    volume: f32,
    pub output: Option<Output>,
    pub duration: Option<Duration>,
    pub error: Option<String>,
}

impl Audio {
    /// The output stream opens with the first track, at that track's format.
    pub fn new() -> Self {
        Self { player: None, sink: None, levels: Default::default(), loaded: false, volume: 1.0, output: None, duration: None, error: None }
    }

    /// Opens the output at the track's own sample rate and channel count, in
    /// float, so no conversion happens in the player: the samples reach the
    /// sound server as decoded. The server either runs at that rate or does
    /// the conversion with its own (sinc) resampler. Only if the device
    /// refuses the format does the default config, and rodio's linear
    /// converter, come in; `Output::native` records that.
    fn open_output(&mut self, rate: SampleRate, channels: ChannelCount) -> Result<(), String> {
        if self.output.is_some_and(|o| o.native && o.rate == rate.get() && o.channels == channels.get()) && self.player.is_some() {
            return Ok(());
        }
        self.player = None;
        self.sink = None;
        let exact = DeviceSinkBuilder::from_default_device()
            .map(|b| b.with_sample_rate(rate).with_channels(channels).with_sample_format(SampleFormat::F32))
            .and_then(|b| b.open_stream());
        let (mut sink, native) = match exact {
            Ok(sink) => (sink, true),
            Err(_) => (DeviceSinkBuilder::open_default_sink().map_err(|e| format!("NO AUDIO OUTPUT: {e}"))?, false),
        };
        sink.log_on_drop(false);
        let cfg = sink.config();
        let native = native && cfg.sample_rate() == rate && cfg.channel_count() == channels;
        let out_rate = cfg.sample_rate().get();
        let server_rate = pipewire_clock().map(|(clock, allowed)| if allowed.contains(&out_rate) { out_rate } else { clock });
        self.output = Some(Output { rate: out_rate, channels: cfg.channel_count().get(), native, server_rate });
        let player = Player::connect_new(sink.mixer());
        player.set_volume(self.volume * self.volume);
        self.player = Some(player);
        self.sink = Some(sink);
        Ok(())
    }

    pub fn load(&mut self, path: &Path, start: Duration, play: bool) {
        if let Some(player) = &self.player {
            player.clear();
        }
        self.loaded = false;
        let decoder = File::open(path)
            .map_err(|e| e.to_string())
            .and_then(|f| Decoder::try_from(f).map_err(|e| e.to_string()));
        match decoder {
            Ok(dec) => {
                if let Err(e) = self.open_output(dec.sample_rate(), dec.channels()) {
                    self.error = Some(e);
                    return;
                }
                let Some(player) = &self.player else { return };
                self.duration = dec.total_duration();
                let channels = dec.channels().get();
                player.append(Meter { inner: dec, levels: self.levels.clone(), channels, acc: [0.0; 2], n: 0 });
                if start > Duration::ZERO {
                    let _ = player.try_seek(start);
                }
                if play {
                    player.play();
                }
                self.loaded = true;
                self.error = None;
            }
            Err(e) => self.error = Some(format!("CAN'T PLAY: {e}")),
        }
    }

    /// Empties the deck: the tape goes back on the rail.
    pub fn stop(&mut self) {
        if let Some(p) = &self.player {
            p.clear();
        }
        self.loaded = false;
        self.duration = None;
    }

    pub fn position(&self) -> Duration {
        self.player.as_ref().map(Player::get_pos).unwrap_or_default()
    }

    pub fn seek(&self, pos: Duration) {
        if let Some(p) = &self.player {
            let _ = p.try_seek(pos);
        }
    }

    pub fn is_playing(&self) -> bool {
        self.loaded && self.player.as_ref().is_some_and(|p| !p.is_paused() && !p.empty())
    }

    pub fn toggle(&self) {
        if let Some(p) = &self.player {
            if p.is_paused() { p.play() } else { p.pause() }
        }
    }

    pub fn set_volume(&mut self, v: f32) {
        self.volume = v;
        if let Some(p) = &self.player {
            // Perceptual curve so the knob feels linear.
            p.set_volume(v * v);
        }
    }

    /// True once the loaded track has played to its end.
    pub fn finished(&self) -> bool {
        self.loaded && self.player.as_ref().is_some_and(|p| p.empty())
    }

    /// Mixes a deck noise into the output, at the output's own rate and
    /// following the volume knob. Needs a stream, so it is silent before the
    /// first track opens one.
    pub fn play_sfx(&self, sfx: Sfx) {
        let (Some(sink), Some(out)) = (&self.sink, self.output) else { return };
        let Some(rate) = SampleRate::new(out.rate) else { return };
        let gain = 0.5 * self.volume * self.volume;
        let samples: Vec<f32> = synth(sfx, out.rate as f32).into_iter().map(|s| s * gain).collect();
        sink.mixer().add(SamplesBuffer::new(ChannelCount::new(1).unwrap(), rate, samples));
    }

    /// Left and right levels; silent while paused.
    pub fn levels(&self) -> (f32, f32) {
        if !self.is_playing() {
            return (0.0, 0.0);
        }
        let read = |k: usize| f32::from_bits(self.levels[k].load(Ordering::Relaxed));
        (read(0), read(1))
    }
}
