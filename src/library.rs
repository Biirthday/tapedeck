//! Music library: folder scan, tags, cover art and synced lyrics.
use std::cmp::Ordering;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{self, AtomicUsize};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use image::imageops::FilterType;
use lofty::config::ParseOptions;
use lofty::picture::PictureType;
use lofty::prelude::*;
use lofty::probe::Probe;

// Only formats the bundled decoders (symphonia) can play. Opus is not among
// them, so .opus files are left out rather than listed and failing.
const AUDIO_EXT: &[&str] = &["flac", "mp3", "m4a", "ogg", "wav"];
const COVER_NAMES: &[&str] = &[
    "folder.jpg", "cover.jpg", "front.jpg", "folder.png", "cover.png",
    "Folder.jpg", "Cover.jpg", "Front.jpg", "Folder.png", "Cover.png",
];

pub struct Track {
    pub title: String,
    pub path: PathBuf,
    pub duration: Option<Duration>,
    /// Codec and resolution as stored, e.g. "FLAC 24/96" or "MP3 320K/44.1".
    pub format: Option<String>,
    /// Stored beyond CD quality: more than 16 bits or faster than 48 kHz.
    pub hires: bool,
}

pub struct Album {
    /// Path relative to the library root; stable id used for saved state.
    pub rel: String,
    pub dir: PathBuf,
    pub album: String,
    pub artist: String,
    pub year: String,
    pub genre: String,
    pub tracks: Vec<Track>,
    /// Track tags are read lazily, when the tape is first loaded.
    pub tracks_read: bool,
    /// Release details from the first track's tags (label, catalog number,
    /// dates...), as name and value; read with the track tags.
    pub details: Vec<(&'static str, String)>,
    /// Size of the audio files on disk, in bytes; read with the track tags.
    pub bytes: u64,
    /// Set for mixtapes; 0 is the blank tape that starts a new one.
    pub mix: Option<u64>,
}

/// The library under `root`, sorted by artist and year, and the lyrics
/// folders found there (see `lyric_mirrors`).
pub fn scan(root: &Path) -> (Vec<Album>, Vec<PathBuf>) {
    let mirrors = lyric_mirrors(root);
    let mut groups = Vec::new();
    walk(root, &mirrors, &mut groups);
    let mut albums: Vec<Album> = groups
        .into_iter()
        .map(|(dir, files)| {
            let rel = dir.strip_prefix(root).unwrap_or(&dir).to_string_lossy().into_owned();
            let (album, artist, year, genre) = album_tags(&dir.join(&files[0]), &rel);
            let tracks = files.iter().map(|f| track_from_path(dir.join(f))).collect();
            Album { rel, dir, album, artist, year, genre, tracks, tracks_read: false, details: Vec::new(), bytes: 0, mix: None }
        })
        .collect();
    albums.sort_by(|a, b| {
        icase_cmp(&a.artist, &b.artist).then_with(|| a.year.cmp(&b.year))
    });
    (albums, mirrors)
}

/// Folders at the top of the library that hold `.lrc` files and no audio:
/// lyrics kept apart from the music, mirroring its paths. Whatever they are
/// called, they are searched for lyrics and left out of the tapes.
pub fn lyric_mirrors(root: &Path) -> Vec<PathBuf> {
    // (has audio, has lyrics), stopping at the first audio file.
    fn look(dir: &Path, lyrics: &mut bool) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else { return false };
        let mut subdirs = Vec::new();
        for e in entries.flatten() {
            let path = e.path();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                subdirs.push(path);
            } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if AUDIO_EXT.iter().any(|a| ext.eq_ignore_ascii_case(a)) {
                    return true;
                }
                *lyrics |= ext.eq_ignore_ascii_case("lrc");
            }
        }
        subdirs.iter().any(|d| look(d, lyrics))
    }
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut mirrors: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()) && !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .filter(|d| {
            let mut lyrics = false;
            !look(d, &mut lyrics) && lyrics
        })
        .collect();
    mirrors.sort();
    mirrors
}

pub fn icase_cmp(a: &str, b: &str) -> Ordering {
    a.chars().flat_map(char::to_lowercase).cmp(b.chars().flat_map(char::to_lowercase))
}

fn walk(dir: &Path, skip: &[PathBuf], out: &mut Vec<(PathBuf, Vec<String>)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files = Vec::new();
    let mut subdirs = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(kind) = e.file_type() else { continue };
        if kind.is_dir() {
            if !name.starts_with('.') && !skip.contains(&e.path()) {
                subdirs.push(e.path());
            }
        } else if let Some(ext) = Path::new(&name).extension().and_then(|e| e.to_str()) {
            if AUDIO_EXT.iter().any(|a| ext.eq_ignore_ascii_case(a)) {
                files.push(name);
            }
        }
    }
    if !files.is_empty() {
        files.sort_by(|a, b| natural_cmp(a, b));
        out.push((dir.to_path_buf(), files));
    }
    for d in subdirs {
        walk(&d, skip, out);
    }
}

pub fn track_from_path(path: PathBuf) -> Track {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Track { title: title_from_file(&name), path, duration: None, format: None, hires: false }
}

fn leading_number(s: &str) -> Option<u32> {
    let end = s.bytes().take_while(u8::is_ascii_digit).count();
    (end > 0).then(|| s[..end].parse().ok()).flatten()
}

fn natural_cmp(a: &str, b: &str) -> Ordering {
    match (leading_number(a), leading_number(b)) {
        (Some(x), Some(y)) if x != y => x.cmp(&y),
        _ => icase_cmp(a, b),
    }
}

fn title_from_file(name: &str) -> String {
    let stem = Path::new(name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let trimmed = stem.trim_start_matches(|c: char| c.is_ascii_digit());
    let trimmed = if trimmed.len() < stem.len() {
        trimmed.trim_start_matches([' ', '.', '-', '_'])
    } else {
        &stem
    };
    // Taggers replace "?" with "_" in file names.
    match trimmed.strip_suffix('_') {
        Some(t) => format!("{t}?"),
        None => trimmed.to_string(),
    }
}

fn album_tags(file: &Path, rel: &str) -> (String, String, String, String) {
    let parts: Vec<&str> = rel.split('/').collect();
    let fallback_album = parts.last().copied().unwrap_or("Unknown").to_string();
    let fallback_artist = if parts.len() > 1 { parts[0].to_string() } else { "Unknown".into() };
    let opts = ParseOptions::new().read_properties(false).read_cover_art(false);
    let Ok(tagged) = Probe::open(file).and_then(|p| p.options(opts).read()) else {
        return (fallback_album, fallback_artist, String::new(), String::new());
    };
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
        return (fallback_album, fallback_artist, String::new(), String::new());
    };
    let album = tag.album().map(|s| s.into_owned()).unwrap_or(fallback_album);
    let artist = tag
        .get_string(ItemKey::AlbumArtist)
        .map(str::to_string)
        .or_else(|| tag.artist().map(|s| s.into_owned()))
        .unwrap_or(fallback_artist);
    let year = tag
        .get_string(ItemKey::OriginalReleaseDate)
        .or_else(|| tag.get_string(ItemKey::RecordingDate))
        .map(|s| s.chars().take(4).collect())
        .or_else(|| tag.date().map(|d| d.year.to_string()))
        .unwrap_or_default();
    let genre = tag.get_strings(ItemKey::Genre).collect::<Vec<_>>().join("; ");
    (album, artist, year, genre)
}

/// Reads per-track titles and durations; called when a tape is loaded.
pub fn read_track_tags(album: &mut Album) {
    if album.tracks_read {
        return;
    }
    album.tracks_read = true;
    let opts = ParseOptions::new().read_cover_art(false);
    let mut discs = 0;
    for (k, t) in album.tracks.iter_mut().enumerate() {
        album.bytes += std::fs::metadata(&t.path).map(|m| m.len()).unwrap_or(0);
        let Ok(tagged) = Probe::open(&t.path).and_then(|p| p.options(opts).read()) else { continue };
        let props = tagged.properties();
        t.duration = Some(props.duration());
        t.format = Some(format_label(tagged.file_type(), props));
        t.hires = props.bit_depth().is_some_and(|b| b > 16) || props.sample_rate().is_some_and(|r| r > 48_000);
        let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else { continue };
        if let Some(title) = tag.title() {
            t.title = title.into_owned();
        }
        discs = discs.max(tag.disk().unwrap_or(0)).max(tag.disk_total().unwrap_or(0));
        // A mixtape's first track speaks for its own album, not the tape.
        if k == 0 && album.mix.is_none() {
            album.details = album_details(tag, &album.year);
        }
    }
    if discs > 1 && album.mix.is_none() {
        album.details.push(("DISCS", discs.to_string()));
    }
}

/// Release details worth showing beside the album's title, artist and year.
fn album_details(tag: &lofty::tag::Tag, year: &str) -> Vec<(&'static str, String)> {
    let get = |key: ItemKey| tag.get_string(key).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let mut out = Vec::new();
    // Dates only when they say more than the year already shown.
    let released = get(ItemKey::ReleaseDate).or_else(|| get(ItemKey::RecordingDate)).filter(|d| d != year);
    if let Some(original) = get(ItemKey::OriginalReleaseDate).filter(|d| d.len() > 4 && Some(d) != released.as_ref()) {
        out.push(("ORIGINAL", original));
    }
    if let Some(released) = released {
        out.push(("RELEASED", released));
    }
    let keys = [
        ("LABEL", ItemKey::Label),
        ("LABEL", ItemKey::Publisher),
        ("CATALOG", ItemKey::CatalogNumber),
        ("COUNTRY", ItemKey::ReleaseCountry),
        ("TYPE", ItemKey::MusicBrainzReleaseType),
        ("MEDIA", ItemKey::OriginalMediaType),
        ("COMPOSER", ItemKey::Composer),
        ("PRODUCER", ItemKey::Producer),
        ("ENGINEER", ItemKey::Engineer),
        ("BARCODE", ItemKey::Barcode),
        ("COPYRIGHT", ItemKey::CopyrightMessage),
    ];
    for (name, key) in keys {
        if let Some(value) = get(key).filter(|_| !out.iter().any(|(n, _)| *n == name)) {
            out.push((name, value));
        }
    }
    out
}

fn format_label(kind: lofty::file::FileType, props: &lofty::properties::FileProperties) -> String {
    use lofty::file::FileType;
    let bits = props.bit_depth();
    let codec = match kind {
        FileType::Flac => "FLAC",
        FileType::Mpeg => "MP3",
        FileType::Mp4 if bits.is_some() => "ALAC",
        FileType::Mp4 => "AAC",
        FileType::Vorbis => "VORBIS",
        FileType::Wav => "WAV",
        FileType::Aiff => "AIFF",
        _ => "AUDIO",
    };
    let khz = props.sample_rate().map(|r| if r % 1000 == 0 { (r / 1000).to_string() } else { format!("{:.1}", r as f32 / 1000.0) });
    let khz = khz.unwrap_or_else(|| "?".into());
    match (bits, props.audio_bitrate()) {
        (Some(b), _) => format!("{codec} {b}/{khz}"),
        (None, Some(kbps)) => format!("{codec} {kbps}K/{khz}"),
        (None, None) => format!("{codec} {khz}K"),
    }
}

pub struct LyricLine {
    pub at: f32,
    pub words: String,
}

pub enum Lyrics {
    Synced(Vec<LyricLine>),
    Plain(Vec<String>),
}

/// Lyrics for a track: a `.lrc` next to it, or at the same path inside one
/// of the library's lyrics folders.
pub fn read_lyrics(track: &Track, root: &Path, mirrors: &[PathBuf]) -> Option<Lyrics> {
    let sidecar = track.path.with_extension("lrc");
    let rel = sidecar.strip_prefix(root).ok().map(Path::to_path_buf);
    let mirrored = mirrors.iter().filter_map(|m| Some(m.join(rel.as_ref()?)));
    std::iter::once(sidecar.clone()).chain(mirrored).find_map(|path| {
        let text = std::fs::read_to_string(path).ok()?;
        parse_lyrics(&text)
    })
}

fn parse_lyrics(text: &str) -> Option<Lyrics> {
    let mut out = Vec::new();
    let mut plain = Vec::new();
    for line in text.trim_start_matches('\u{feff}').trim().lines() {
        let line = line.trim();
        let mut rest = line;
        let mut stamps = Vec::new();
        while let Some(body) = rest.strip_prefix('[') {
            let Some(end) = body.find(']') else { break };
            let stamp = &body[..end];
            if let Some((m, s)) = stamp.split_once(':')
                && let (Ok(m), Ok(s)) = (m.parse::<u32>(), s.parse::<f32>())
                && s.is_finite() && s >= 0.0
            {
                stamps.push(m as f32 * 60.0 + s);
            }
            rest = &body[end + 1..];
        }
        if stamps.is_empty() {
            let metadata = line.strip_prefix('[').and_then(|s| s.strip_suffix(']'))
                .and_then(|s| s.split_once(':'))
                .is_some_and(|(key, _)| !key.is_empty() && key.chars().all(|c| c.is_ascii_alphabetic()));
            if !metadata {
                plain.push(line.to_string());
            }
        } else {
            let words = rest.trim().to_string();
            out.extend(stamps.into_iter().map(|at| LyricLine { at, words: words.clone() }));
        }
    }
    if out.iter().any(|line| !line.words.is_empty()) {
        out.sort_by(|a, b| a.at.total_cmp(&b.at));
        Some(Lyrics::Synced(out))
    } else if plain.iter().any(|line| !line.is_empty()) {
        Some(Lyrics::Plain(plain))
    } else {
        None
    }
}

pub struct Cover {
    pub idx: usize,
    pub width: u16,
    pub height: u16,
    pub rgba: Vec<u8>,
    /// Tiny version, scaled up for the blurred backdrop.
    pub blur: Vec<u8>,
    pub average: [u8; 3],
}

pub const BLUR_SIZE: u16 = 10;
const COVER_MAX_SIZE: u32 = 640;

/// Decodes covers on worker threads, nearest tapes first. `idx` is the
/// tape's stable id. The counter tells how many jobs are finished, with or
/// without a cover.
pub fn spawn_cover_workers(jobs: Vec<(usize, PathBuf, PathBuf)>) -> (Receiver<Cover>, Arc<AtomicUsize>) {
    let (tx, rx) = mpsc::sync_channel(24);
    let done = Arc::new(AtomicUsize::new(0));
    let workers = std::thread::available_parallelism().map(|n| n.get().clamp(2, 8)).unwrap_or(4);
    let queue = Arc::new(Mutex::new(VecDeque::from(jobs)));
    for _ in 0..workers {
        let queue = queue.clone();
        let tx = tx.clone();
        let done = done.clone();
        std::thread::spawn(move || loop {
            let Some((idx, dir, first)) = queue.lock().unwrap().pop_front() else { return };
            let cover = load_cover(idx, &dir, &first);
            if let Some(cover) = cover {
                if tx.send(cover).is_err() {
                    return;
                }
            }
            done.fetch_add(1, atomic::Ordering::Release);
        });
    }
    (rx, done)
}

fn cover_bytes(dir: &Path, first: &Path) -> Option<Vec<u8>> {
    for name in COVER_NAMES {
        if let Ok(bytes) = std::fs::read(dir.join(name)) {
            return Some(bytes);
        }
    }
    let opts = ParseOptions::new().read_properties(false);
    let tagged = Probe::open(first).and_then(|p| p.options(opts).read()).ok()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    let pics = tag.pictures();
    pics.iter()
        .find(|p| p.pic_type() == PictureType::CoverFront)
        .or_else(|| pics.first())
        .map(|p| p.data().to_vec())
}

fn load_cover(idx: usize, dir: &Path, first: &Path) -> Option<Cover> {
    let img = image::load_from_memory(&cover_bytes(dir, first)?).ok()?;
    let cover = if img.width().max(img.height()) > COVER_MAX_SIZE {
        img.resize(COVER_MAX_SIZE, COVER_MAX_SIZE, FilterType::Triangle).into_rgba8()
    } else {
        img.into_rgba8()
    };
    let blur = image::imageops::resize(&cover, BLUR_SIZE as u32, BLUR_SIZE as u32, FilterType::Triangle);
    let mut sum = [0u32; 3];
    for px in blur.pixels() {
        for c in 0..3 {
            sum[c] += px[c] as u32;
        }
    }
    let n = (BLUR_SIZE as u32) * (BLUR_SIZE as u32);
    Some(Cover {
        idx,
        width: cover.width() as u16,
        height: cover.height() as u16,
        rgba: cover.into_raw(),
        blur: blur.into_raw(),
        average: [(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lyrics_folders_are_found_by_content_not_name() {
        let root = std::env::temp_dir().join(format!("tapedeck-mirrors-{}", std::process::id()));
        let touch = |p: &str| {
            let p = root.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"").unwrap();
        };
        touch("Artist/Album/01 Song.flac");
        touch("Artist/Album/01 Song.lrc");
        touch("Letras/Artist/Album/01 Song.lrc");
        touch("Notes/readme.txt");
        touch(".hidden/Artist/x.lrc");
        assert_eq!(lyric_mirrors(&root), vec![root.join("Letras")]);
        let (albums, mirrors) = scan(&root);
        assert_eq!(albums.len(), 1);
        assert_eq!(mirrors, vec![root.join("Letras")]);
        std::fs::remove_dir_all(root).unwrap();
    }
}
