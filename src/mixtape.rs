//! Mixtapes: user-made tapes of tracks picked from any album.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::library::{self, Album};

#[derive(Serialize, Deserialize, Clone)]
pub struct Mixtape {
    pub id: u64,
    pub name: String,
    pub tracks: Vec<PathBuf>,
}

fn path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("tapedeck").join("mixtapes.json"))
}

pub fn load() -> Vec<Mixtape> {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(mixes: &[Mixtape]) {
    let Some(path) = path() else { return };
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    // Write then rename, so a crash never leaves a half-written file.
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_string_pretty(mixes).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(tmp, path);
    }
}

pub fn rel(id: u64) -> String {
    format!("mix:{id}")
}

pub fn album(m: &Mixtape) -> Album {
    let n = m.tracks.len();
    Album {
        rel: rel(m.id),
        dir: PathBuf::new(),
        album: m.name.clone(),
        artist: "MIXTAPE".into(),
        year: format!("{n} TRACK{}", if n == 1 { "" } else { "S" }),
        genre: String::new(),
        tracks: m.tracks.iter().cloned().map(library::track_from_path).collect(),
        tracks_read: false,
        mix: Some(m.id),
    }
}

/// The blank tape at the head of the rail; clicking it starts a new mixtape.
pub fn blank() -> Album {
    Album {
        rel: rel(0),
        dir: PathBuf::new(),
        album: "NEW MIXTAPE".into(),
        artist: "BLANK TAPE".into(),
        year: String::new(),
        genre: String::new(),
        tracks: Vec::new(),
        tracks_read: true,
        mix: Some(0),
    }
}
