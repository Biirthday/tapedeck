//! The deck on the desktop: an MPRIS player, so status bars show what is
//! playing and media keys (or `playerctl`) control it.
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

pub use souvlaki::{MediaControlEvent, SeekDirection};
use souvlaki::{MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig};

/// What the desktop was last told, so it only hears about changes.
#[derive(Clone, PartialEq, Default)]
pub struct NowPlaying {
    pub title: String,
    pub album: String,
    pub artist: String,
    pub cover: Option<PathBuf>,
    pub duration: Option<Duration>,
}

pub struct Desktop {
    controls: Option<MediaControls>,
    events: Receiver<MediaControlEvent>,
    shown: Option<NowPlaying>,
    state: Option<bool>,
}

impl Desktop {
    /// Registers on the session bus. Without one (or on failure) the player
    /// simply runs without desktop integration.
    pub fn new() -> Self {
        let (tx, events) = channel();
        let config = PlatformConfig { display_name: "Tapedeck", dbus_name: "tapedeck", hwnd: None };
        let controls = MediaControls::new(config).ok().and_then(|mut controls| {
            controls.attach(move |event| {
                let _ = tx.send(event);
            }).ok()?;
            Some(controls)
        });
        Self { controls, events, shown: None, state: None }
    }

    /// Requests from the desktop since the last call.
    pub fn events(&self) -> Vec<MediaControlEvent> {
        self.events.try_iter().collect()
    }

    /// Publishes the track, when it differs from what was last published.
    pub fn show(&mut self, now: Option<NowPlaying>) {
        if self.shown == now {
            return;
        }
        let Some(controls) = &mut self.controls else { return };
        match &now {
            Some(n) => {
                let url = n.cover.as_ref().map(|p| format!("file://{}", p.display()));
                let _ = controls.set_metadata(MediaMetadata {
                    title: Some(&n.title),
                    album: Some(&n.album),
                    artist: Some(&n.artist),
                    cover_url: url.as_deref(),
                    duration: n.duration,
                });
            }
            None => {
                let _ = controls.set_playback(MediaPlayback::Stopped);
                self.state = None;
            }
        }
        self.shown = now;
    }

    /// Publishes playing or paused with the position; `force` after a seek,
    /// so listeners that extrapolate the position catch up.
    pub fn playback(&mut self, playing: bool, position: Duration, force: bool) {
        if self.shown.is_none() || (!force && self.state == Some(playing)) {
            return;
        }
        let Some(controls) = &mut self.controls else { return };
        let progress = Some(MediaPosition(position));
        let _ = controls.set_playback(if playing { MediaPlayback::Playing { progress } } else { MediaPlayback::Paused { progress } });
        self.state = Some(playing);
    }

    pub fn volume(&mut self, volume: f32) {
        if let Some(controls) = &mut self.controls {
            let _ = controls.set_volume(volume as f64);
        }
    }
}
