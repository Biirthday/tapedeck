//! The deck on the desktop: an MPRIS player, so status bars show what is
//! playing and media keys (or `playerctl`) control it, and the desktop's
//! folder chooser for picking the music folder.
use std::path::{Path, PathBuf};
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

    /// A player that stays off the bus, for when the setting is off.
    pub fn off() -> Self {
        Self { controls: None, events: channel().1, shown: None, state: None }
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

/// Opens the desktop's folder chooser (the XDG file chooser portal) at
/// `start`, and blocks until it closes. `None` when it is cancelled or there
/// is no portal; run it off the main thread.
pub fn pick_folder(start: &Path) -> Option<PathBuf> {
    use std::collections::HashMap;
    use std::os::unix::ffi::OsStrExt;
    use zbus::blocking::{Connection, Proxy};
    use zvariant::{OwnedValue, Value};

    const PORTAL: &str = "org.freedesktop.portal.Desktop";
    let conn = Connection::session().ok()?;
    // The portal answers on a request object whose path is derived from our
    // bus name and a token we choose; listen there before asking.
    let token = format!("tapedeck{}", std::process::id());
    let sender = conn.unique_name()?.as_str().trim_start_matches(':').replace('.', "_");
    let request = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
    let request = Proxy::new(&conn, PORTAL, request.as_str(), "org.freedesktop.portal.Request").ok()?;
    let mut responses = request.receive_signal("Response").ok()?;

    let chooser = Proxy::new(&conn, PORTAL, "/org/freedesktop/portal/desktop", "org.freedesktop.portal.FileChooser").ok()?;
    let mut folder = start.as_os_str().as_bytes().to_vec();
    folder.push(0);
    let mut options: HashMap<&str, Value> = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));
    options.insert("directory", Value::from(true));
    options.insert("modal", Value::from(true));
    options.insert("current_folder", Value::from(folder));
    chooser.call_method("OpenFile", &("", "Choose the music folder", options)).ok()?;

    let reply = responses.next()?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = reply.body().deserialize().ok()?;
    if code != 0 {
        return None;
    }
    let uris: Vec<String> = results.get("uris")?.try_clone().ok()?.try_into().ok()?;
    let path = uris.first()?.strip_prefix("file://")?;
    Some(PathBuf::from(percent_decode(path)))
}

/// Undoes the %XX escapes of a file URI's path.
fn percent_decode(s: &str) -> std::ffi::OsString {
    use std::os::unix::ffi::OsStringExt;
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut k = 0;
    while k < bytes.len() {
        let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
        match (bytes[k], bytes.get(k + 1).copied().and_then(hex), bytes.get(k + 2).copied().and_then(hex)) {
            (b'%', Some(hi), Some(lo)) => {
                out.push(hi * 16 + lo);
                k += 3;
            }
            (b, _, _) => {
                out.push(b);
                k += 1;
            }
        }
    }
    std::ffi::OsString::from_vec(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uri_escapes_are_decoded() {
        assert_eq!(percent_decode("/home/me/M%C3%BAsica%20boa"), std::ffi::OsString::from("/home/me/Música boa"));
        assert_eq!(percent_decode("/plain/path"), std::ffi::OsString::from("/plain/path"));
        assert_eq!(percent_decode("/odd%2"), std::ffi::OsString::from("/odd%2"));
    }
}
