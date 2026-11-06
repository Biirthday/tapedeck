//! Tapedeck: a native cassette-wall player for a local music library.
mod audio;
mod desktop;
mod gfx;
mod library;
mod mixtape;
mod settings;

use std::f32::consts::{FRAC_PI_2, PI};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use macroquad::texture::RenderPass;
use macroquad::models::{Vertex, draw_mesh};
use macroquad::prelude::*;
use serde::{Deserialize, Serialize};

use audio::{Audio, Sfx};
use desktop::{Desktop, MediaControlEvent, NowPlaying, SeekDirection};
use gfx::{DIM, FACE_H, FACE_W, FaceStyle, Fonts, INK, LINE, cover_fit, ring, round_rect, round_rect_lines};
use library::{Album, BLUR_SIZE, Cover, LyricLine, Lyrics};
use mixtape::Mixtape;
use settings::Settings;

// Rail geometry in world units (1 unit = 100 px on the perspective plane).
const STEP: Vec3 = vec3(0.82, 0.44, -1.3);
const TAPE: Vec3 = vec3(2.9, 1.84, 0.42);
const YAW: f32 = -24.0 * PI / 180.0;
const PERSPECTIVE: f32 = 15.0;
/// Horizontal gap between the first tape and the vanishing point (fraction of stage width).
const RAIL_TO_ORIGIN: f32 = 0.16;
const VISIBLE: (f32, f32) = (-2.6, 17.0);
/// Rail positions over which the farthest tapes fade out.
const FAR_FADE: f32 = 10.0;
/// Width the view and sort switches take in the stage's top right corner.
const SWITCHES_W: f32 = 460.0;
/// Direction the key light comes from, on the tape's face plane, and how
/// bright the metallic rim it catches is.
const SHEEN_DIR: Vec2 = vec2(0.55, 0.835);
const SHEEN: f32 = 0.85;
/// How deep the printed front sits under the clear window.
const RECESS: f32 = 0.07;
/// Depth of the printed card behind a turned tape's clear back.
const JCARD_Z: f32 = -TAPE.z / 2.0 + RECESS;
/// Smoke tint of the tapes' clear plastic.
const GLASS_SMOKE: f32 = 0.12;
/// How far back towards the rail (in its 1..0 return) a tape must be before
/// the tape replacing it may leave.
const SWAP_CLEAR: f32 = 0.5;
/// Seconds for a chosen tape to leave the rail for the deck (or come back).
const LOAD_TIME: f32 = 1.1;
/// Seconds to turn a tape around (or back).
const FLIP_TIME: f32 = 0.5;
/// How far a tape rises out of the rail before it turns around.
const FLIP_RISE: f32 = 2.2;
const FACE_RENDERS_PER_FRAME: usize = 4;
const FRAME_BG: Color = Color::new(0.035, 0.035, 0.035, 1.0);
const CANVAS_SAMPLES: i32 = 4;

// Copies the canvas to the window as opaque pixels. Blending leaves partial
// alpha in the canvas; passing it on would make the window see-through.
const PRESENT_VERTEX: &str = r#"#version 100
attribute vec3 position;
attribute vec2 texcoord;
varying lowp vec2 uv;
uniform mat4 Model;
uniform mat4 Projection;
void main() {
    gl_Position = Projection * Model * vec4(position, 1.0);
    uv = texcoord;
}"#;

const PRESENT_FRAGMENT: &str = r#"#version 100
varying lowp vec2 uv;
uniform sampler2D Texture;
void main() {
    gl_FragColor = vec4(texture2D(Texture, uv).rgb, 1.0);
}"#;

/// Multisampled texture for a tape face or its printed tracklist.
fn face_target(scale: f32) -> RenderTarget {
    let rt = render_target_ex((FACE_W * scale) as u32, (FACE_H * scale) as u32, RenderTargetParams { sample_count: CANVAS_SAMPLES, depth: false });
    rt.texture.set_filter(FilterMode::Linear);
    rt
}
const REC: Color = Color::new(0.93, 0.27, 0.24, 1.0);
/// Genre chips under the side list's cover: row pitch, gap and padding.
const GENRE_ROW: f32 = 20.0;
const GENRE_GAP: f32 = 4.0;
const GENRE_PAD: f32 = 6.0;
/// Grid tracklist panel: header height and row pitch.
const GRID_CARD_HEAD: f32 = 104.0;
const GRID_CARD_ROW: f32 = 24.0;
/// Angle of the volume knob's pointer for a level, 0..1: from bottom left
/// round through the top to bottom right.
fn knob_angle(v: f32) -> f32 {
    (-135.0 + v * 270.0).to_radians() - FRAC_PI_2
}

/// Height of the cassette player in the side column.
const DECK_H: f32 = 326.0;
/// Brushed gunmetal of the player's body, its keys, and its LCD.
const BODY: Color = Color::new(0.215, 0.222, 0.238, 1.0);
const KEY: Color = Color::new(0.11, 0.115, 0.125, 1.0);
const LCD: Color = Color::new(0.04, 0.055, 0.05, 1.0);
/// Deck speaker and volume knob radii; the VU meters take the room between them.
const GRILLE_R: f32 = 26.0;
const KNOB_R: f32 = 21.0;

// Spacing scale: every margin and gap in the layout is one of these.
const SP_S: f32 = 8.0;
const SP_M: f32 = 16.0;
const SP_L: f32 = 24.0;

// Corner radii, nested so inner corners follow outer ones: the frame wraps
// the panels at SP_M, panels hold boxes, boxes and lists hold chips.
const R_FRAME: f32 = R_PANEL + SP_M;
const R_PANEL: f32 = 12.0;
const R_BOX: f32 = 6.0;
const R_CHIP: f32 = 4.0;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Default)]
enum Sort {
    #[default]
    Artist,
    Year,
    Played,
    Recent,
    Color,
}

impl Sort {
    const ALL: [Sort; 5] = [Sort::Artist, Sort::Year, Sort::Played, Sort::Recent, Sort::Color];

    fn label(self) -> &'static str {
        match self {
            Sort::Artist => "ARTIST",
            Sort::Year => "YEAR",
            Sort::Played => "PLAYED",
            Sort::Recent => "RECENT",
            Sort::Color => "COLOR",
        }
    }
}

/// How the stage shows the library: the 3D cassette rail or a grid of covers.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Default)]
enum View {
    #[default]
    Rail,
    Grid,
}

impl View {
    const ALL: [View; 2] = [View::Rail, View::Grid];

    fn label(self) -> &'static str {
        match self {
            View::Rail => "RAIL",
            View::Grid => "GRID",
        }
    }
}

/// Cover grid geometry for the current stage size.
struct Grid {
    /// Visible part of the grid; rows scroll through it.
    area: Rect,
    cols: usize,
    /// Cover side.
    cell: f32,
    gap: f32,
    /// Distance from one row to the next: cover, caption and gap.
    row_h: f32,
}

impl Grid {
    fn rows(&self, n: usize) -> usize {
        n.div_ceil(self.cols)
    }

    fn max_scroll(&self, n: usize) -> f32 {
        (self.rows(n) as f32 * self.row_h - self.gap - self.area.h).max(0.0)
    }

    /// Cover rectangle of album `i` at the given scroll.
    fn cover(&self, i: usize, scroll: f32) -> Rect {
        let (col, row) = (i % self.cols, i / self.cols);
        Rect::new(self.area.x + col as f32 * (self.cell + self.gap), self.area.y + row as f32 * self.row_h - scroll, self.cell, self.cell)
    }

    /// Albums with at least part of their cell in view.
    fn visible(&self, n: usize, scroll: f32) -> std::ops::Range<usize> {
        let first = (scroll / self.row_h).floor().max(0.0) as usize;
        let last = ((scroll + self.area.h) / self.row_h).ceil() as usize;
        (first * self.cols).min(n)..((last + 1) * self.cols).min(n)
    }
}

/// Hue order for the color sort: saturated colors first around the wheel,
/// then greys from light to dark, then tapes without a cover.
fn color_key(avg: Option<[u8; 3]>) -> (u8, f32) {
    let Some([r, g, b]) = avg else { return (2, 0.0) };
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let (v, d) = (max, max - min);
    let sat = if max > 0.0 { d / max } else { 0.0 };
    if sat < 0.2 || v < 0.15 {
        return (1, -v);
    }
    let hue = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (0, hue)
}

/// A bright, readable version of the cover's mean color, for highlights on
/// black. Grey covers keep the plain ink.
fn accent_color(avg: Option<[u8; 3]>) -> Color {
    let Some(avg) = avg else { return INK };
    let (sat, hue) = match color_key(Some(avg)) {
        (0, hue) => {
            let [r, g, b] = avg.map(|c| c as f32);
            let (max, min) = (r.max(g).max(b), r.min(g).min(b));
            ((max - min) / max.max(1.0), hue)
        }
        _ => return INK,
    };
    macroquad::color::hsl_to_rgb(hue / 6.0, (sat * 1.6).clamp(0.45, 0.85), 0.7)
}

/// Near-black plastic with a hint of the cover's mean color.
fn shell_color(avg: [u8; 3]) -> Color {
    let [r, g, b] = avg.map(|c| (c as f32 * 0.1 + 14.0) as u8);
    Color::from_rgba(r, g, b, 255)
}

/// Top edge of a cassette: dark plastic with the row of rectangular openings
/// real shells have, and a lit front lip. Shared by every tape.
fn edge_texture() -> Texture2D {
    const W: usize = 256;
    const H: usize = 32;
    let mut px = vec![0u8; W * H * 4];
    for y in 0..H {
        for x in 0..W {
            let u = x as f32 / W as f32;
            let v = y as f32 / H as f32;
            let slot = [0.14, 0.32, 0.5, 0.68, 0.86].iter().any(|c| (u - c).abs() < 0.045) && (0.28..0.72).contains(&v);
            let lip = slot && (0.66..0.72).contains(&v);
            let c: [u8; 3] = if y < 2 {
                [78, 78, 82]
            } else if lip {
                [44, 44, 48]
            } else if slot {
                [3, 3, 4]
            } else {
                let g = (30.0 - v * 10.0) as u8;
                [g, g, g + 2]
            };
            let i = (y * W + x) * 4;
            px[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    let tex = Texture2D::from_rgba8(W as u16, H as u16, &px);
    tex.set_filter(FilterMode::Linear);
    gfx::filter_tape_texture(&tex);
    tex
}

fn colors_path() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("tapedeck").join("colors.json"))
}
/// Fills the area between a rectangle corner and its rounding arc.
fn corner_mask(corner: Vec2, center: Vec2, rad: f32, start: f32, color: Color) {
    const STEPS: usize = 10;
    for i in 0..STEPS {
        let a0 = start + FRAC_PI_2 * i as f32 / STEPS as f32;
        let a1 = start + FRAC_PI_2 * (i + 1) as f32 / STEPS as f32;
        draw_triangle(corner, center + vec2(a0.cos(), a0.sin()) * rad, center + vec2(a1.cos(), a1.sin()) * rad, color);
    }
}

fn window_conf() -> Conf {
    let fullscreen = load_saved().fullscreen;
    Conf {
        fullscreen,
        window_title: "Tapedeck".into(),
        window_width: 1320,
        window_height: 800,
        window_resizable: true,
        high_dpi: false,
        ..Default::default()
    }
}

#[derive(Serialize, Deserialize, Default)]
struct Saved {
    scroll: f32,
    album: Option<String>,
    track: usize,
    time: f32,
    volume: Option<f32>,
    #[serde(default)]
    sort: Sort,
    #[serde(default)]
    fullscreen: bool,
    /// Play queue as (album path relative to the library, track index);
    /// tape ids only last for one run.
    #[serde(default)]
    queue: Vec<(String, usize)>,
    /// Tracks started per tape, for the "played" sort.
    #[serde(default)]
    plays: HashMap<String, u32>,
    /// When each tape was last started, in seconds since the Unix epoch, for
    /// the "recent" sort.
    #[serde(default)]
    played_at: HashMap<String, u64>,
    /// Mouse actions already used (LEARNED_*); their hints are no longer shown.
    #[serde(default)]
    learned: u8,
    #[serde(default)]
    view: View,
    /// Mechanical deck noises; on unless turned off.
    #[serde(default)]
    quiet: bool,
    /// First album in view in the grid; rows depend on the window, so the
    /// album is kept rather than a pixel offset.
    #[serde(default)]
    grid_top: usize,
    /// The side list showed the queue instead of the tracklist.
    #[serde(default)]
    show_queue: bool,
    #[serde(default)]
    settings: Settings,
}

const LEARNED_LISTEN: u8 = 1;
const LEARNED_QUEUE: u8 = 2;
const LEARNED_FLIP: u8 = 4;

fn state_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("tapedeck").join("state.json"))
}

fn load_saved() -> Saved {
    state_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Saved { scroll: -1.0, ..Default::default() })
}

/// Off-center perspective, so the vanishing point sits up and to the left
/// of the stage like the CSS `perspective-origin: 30% 30%`.
struct StageCam {
    matrix: Mat4,
    viewport: (i32, i32, i32, i32),
    /// The multisampled canvas the frame is drawn into.
    pass: Option<RenderPass>,
}

impl Camera for StageCam {
    fn matrix(&self) -> Mat4 {
        self.matrix
    }
    fn depth_enabled(&self) -> bool {
        true
    }
    fn render_pass(&self) -> Option<RenderPass> {
        self.pass.clone()
    }
    fn viewport(&self) -> Option<(i32, i32, i32, i32)> {
        Some(self.viewport)
    }
}

struct Layout {
    frame: Rect,
    side: Rect,
    stage: Rect,
    deck: Rect,
    grille: Vec2,
    knob: Vec2,
    slot: Rect,
    /// Piano keys: rewind, play, fast forward, eject.
    keys: [Rect; 4],
    display: Rect,
    progress: Rect,
    list: Rect,
    /// Index strip along the bottom of the stage.
    ruler: Rect,
}

impl Layout {
    fn new(recording: bool, has_queue: bool) -> Self {
        let (sw, sh) = (screen_width(), screen_height());
        // The interface fills the window; the frame line sits just inside the edge.
        let frame = Rect::new(SP_S, SP_S, sw - 2.0 * SP_S, sh - 2.0 * SP_S);
        let side = Rect::new(frame.x + SP_M, frame.y + SP_M, 300.0, frame.h - 2.0 * SP_M);
        let stage = Rect::new(side.right() + SP_M, frame.y + SP_M, frame.right() - side.right() - 2.0 * SP_M, frame.h - 2.0 * SP_M);
        // A cassette player: speaker and meters on top, the cassette door,
        // an LCD readout, and a row of piano keys along the bottom.
        let deck = Rect::new(side.x, side.bottom() - DECK_H, side.w, DECK_H);
        let slot = Rect::new(deck.x + SP_M, deck.y + 88.0, deck.w - 2.0 * SP_M, 100.0);
        let display = Rect::new(deck.x + SP_M, slot.bottom() + 12.0, deck.w - 2.0 * SP_M, 50.0);
        let key_w = (deck.w - 2.0 * SP_M - 3.0 * 4.0) / 4.0;
        let key_y = display.bottom() + 18.0;
        let keys = [0.0, 1.0, 2.0, 3.0].map(|k| Rect::new(deck.x + SP_M + k * (key_w + 4.0), key_y, key_w, 42.0));
        let progress = Rect::new(display.x + SP_S, display.y + 20.0, display.w - 2.0 * SP_S, 3.0);
        let list_top = side.y + SP_S + if recording { SP_L } else { 0.0 };
        let list_bottom = deck.y - SP_L - if has_queue { SP_L } else { 0.0 };
        let list = Rect::new(side.x + SP_S, list_top, side.w - 2.0 * SP_S, list_bottom - list_top);
        let ruler = Rect::new(stage.x + SP_L, stage.bottom() - 72.0, stage.w - 2.0 * SP_L, 24.0);
        Self {
            ruler,
            frame,
            side,
            stage,
            deck,
            grille: vec2(deck.x + SP_M + GRILLE_R, deck.y + 48.0),
            knob: vec2(deck.right() - SP_M - KNOB_R, deck.y + 48.0),
            slot,
            keys,
            display,
            progress,
            list,
        }
    }

    /// Space the cover grid scrolls in: below the switches, above the index strip.
    fn grid_area(&self) -> Rect {
        let s = self.stage;
        let top = s.y + 56.0;
        Rect::new(s.x + SP_L, top, s.w - 2.0 * SP_L, (self.ruler.y - 32.0 - top).max(0.0))
    }

    /// Clickable area around the deck's progress bar.
    fn seek_bar(&self) -> Rect {
        Rect::new(self.progress.x, self.progress.y - 5.0, self.progress.w, 13.0)
    }

    /// Where the rail has faded fully into black, just above the index strip.
    /// Tapes below it are hidden and cannot be picked.
    fn rail_floor(&self) -> f32 {
        self.ruler.y - 18.0
    }

    fn list_rows(&self) -> usize {
        ((self.list.h - 50.0) / 22.0).floor() as usize
    }

    fn plain_lyrics_panel(&self) -> Rect {
        let s = self.stage;
        // The first lyric row's baseline (17 below its top) lines up with the
        // side list's album title (16 below the list's top).
        let top = self.list.y - 1.0;
        let w = (s.w * 0.5).min(640.0);
        // In a narrow stage the top row would run into the view and sort
        // switches; drop below them instead.
        let beside = s.w - 2.0 * SP_L - SWITCHES_W - SP_L;
        if beside >= w.min(320.0) {
            Rect::new(s.x + SP_L, top, w.min(beside), (s.h - 168.0).clamp(0.0, 240.0))
        } else {
            Rect::new(s.x + SP_L, s.y + 56.0, w, (s.h - 168.0).clamp(0.0, 240.0))
        }
    }

    fn synced_lyrics_panel(&self) -> Option<Rect> {
        let mut panel = self.plain_lyrics_panel();
        panel.h = panel.h.min(166.0);
        (panel.h >= 104.0 && panel.w >= 120.0).then_some(panel)
    }

    fn camera(&self) -> StageCam {
        let s = self.stage;
        let fovy = 2.0 * ((s.h / 200.0) / PERSPECTIVE).atan();
        let proj = Mat4::perspective_rh_gl(fovy, s.w / s.h, 0.1, 200.0);
        let view = Mat4::look_at_rh(vec3(0.0, 0.0, PERSPECTIVE), Vec3::ZERO, Vec3::Y);
        let origin_x = self.rail_x() + RAIL_TO_ORIGIN;
        let shift = Mat4::from_translation(vec3(origin_x * 2.0 - 1.0, 0.4, 0.0));
        StageCam {
            pass: None,
            matrix: shift * proj * view,
            viewport: (s.x as i32, (screen_height() - s.bottom()) as i32, s.w as i32, s.h as i32),
        }
    }

    /// Horizontal position of the first tape, as a fraction of the stage width.
    /// Narrow stages keep it near the left edge; wide ones move the whole rail
    /// (and its vanishing point) towards the center so the row fills the space.
    fn rail_x(&self) -> f32 {
        let aspect = self.stage.w / self.stage.h;
        (0.14 + (aspect - 0.6) * 0.2).clamp(0.14, 0.34)
    }

    /// Where the first tape sits, relative to the perspective origin.
    fn rail_origin(&self) -> Vec3 {
        vec3(-RAIL_TO_ORIGIN * self.stage.w / 100.0, -0.40 * self.stage.h / 100.0, 0.0)
    }

    fn project(&self, m: &Mat4, p: Vec3) -> Vec2 {
        let c = *m * p.extend(1.0);
        let n = c.truncate() / c.w;
        vec2(self.stage.x + (n.x + 1.0) / 2.0 * self.stage.w, self.stage.y + (1.0 - n.y) / 2.0 * self.stage.h)
    }
}

fn lyric_rows(panel: Rect, focus: usize, count: usize) -> impl Iterator<Item = (usize, Rect)> {
    let indices = [focus.checked_sub(1), Some(focus), Some(focus + 1)];
    let rows = [
        Rect::new(panel.x, panel.y, panel.w, 24.0),
        Rect::new(panel.x, panel.y + 24.0, panel.w, panel.h - 70.0),
        Rect::new(panel.x, panel.bottom() - 46.0, panel.w, 24.0),
    ];
    indices.into_iter().zip(rows).filter_map(move |(i, row)| Some((i.filter(|&i| i < count)?, row)))
}

struct Tape {
    /// Stable identity; indices change when the rail is re-sorted.
    id: usize,
    /// Mean cover color, cached across runs for the color sort.
    average: Option<[u8; 3]>,
    face: Option<RenderTarget>,
    cover: Option<Texture2D>,
    blur: Option<Texture2D>,
    shell: Color,
    dirty: bool,
    matched: bool,
    lift: f32,
    /// 1 when the active search leaves this tape out.
    dim: f32,
    /// 1 when the tape has left the rail for the deck.
    out: f32,
    /// 1 when the tape is turned around, showing its J-card.
    flip: f32,
}

struct App {
    albums: Vec<Album>,
    tapes: Vec<Tape>,
    face_pool: Vec<RenderTarget>,
    fonts: Fonts,
    audio: Audio,
    covers: Receiver<Cover>,
    /// Cover decoders for tapes a rescan added.
    extra_covers: Vec<Receiver<Cover>>,
    /// A rescan of the music folder running in the background.
    rescan: Option<Receiver<Vec<Album>>>,
    covers_done: Arc<AtomicUsize>,
    cover_jobs: usize,
    colors_saved: bool,
    next_id: usize,
    sort: Sort,
    fullscreen: bool,
    plays: HashMap<String, u32>,
    played_at: HashMap<String, u64>,
    learned: u8,
    settings: Settings,
    /// The settings panel is open, which row is selected, and its scroll.
    show_settings: bool,
    settings_row: usize,
    settings_scroll: f32,
    /// The volume slider in the panel is being dragged.
    settings_drag: bool,
    /// Mechanical deck noises on key presses, loading and ejecting.
    sfx: bool,
    view: View,
    /// Grid scroll in pixels, eased towards `grid_target`.
    grid_scroll: f32,
    grid_target: f32,
    /// Album to bring to the top of the grid once its size is known.
    grid_restore: Option<usize>,
    /// Grid columns, row height and visible height from the last frame, so
    /// scrolling to an album works without a layout at hand.
    grid_metrics: (usize, f32, f32),
    mixes: Vec<Mixtape>,
    /// Mixtape that right-clicked tracks are recorded onto.
    rec: Option<u64>,
    /// Mixtape whose label is being typed.
    renaming: Option<u64>,
    /// First press of Delete on a mixtape, waiting for the second.
    erase_armed: Option<(u64, f64)>,
    toast: Option<(String, f64)>,
    /// Tracks to play next, by stable tape id and track index.
    queue: VecDeque<(usize, usize)>,
    /// The side list shows the queue instead of the loaded tape's tracks.
    show_queue: bool,
    queue_scroll: f32,
    /// Queue row being dragged: where it started, the press height, and
    /// whether it has moved far enough to count as a drag.
    queue_drag: Option<(usize, f32, bool)>,
    ruler_drag: bool,
    scroll: f32,
    target: f32,
    hover: Option<usize>,
    current: Option<usize>,
    track: usize,
    music_root: PathBuf,
    lyrics: Option<Lyrics>,
    lyrics_scroll: f32,
    lyric_preview: Option<f32>,
    list_scroll: f32,
    volume: f32,
    drag: Option<(f32, f32, bool)>,
    /// Mouse angle around the volume knob on the previous frame, while turning it.
    knob_drag: Option<f32>,
    /// Where the deck's progress bar is being dragged to, 0..1. The track
    /// only seeks there when the button is let go.
    seek_drag: Option<f32>,
    query: String,
    /// Matching tapes, with the track whose title matched (if the album itself did not).
    matches: Vec<(usize, Option<usize>)>,
    focus: usize,
    backdrop: (Option<usize>, Option<usize>, f64),
    slot_in: f32,
    /// Spin of the deck's two reels; the emptier one turns faster.
    reels: (f32, f32),
    /// Reel speed, 0..1: the motor spins up on play and runs down on pause.
    reel_speed: f32,
    /// Something on screen is still easing towards where it is going.
    settling: bool,
    /// Last time the mouse, wheel or keyboard was used, and where the mouse
    /// was on the previous frame; drives the frame rate.
    active_at: f64,
    frame_mouse: Vec2,
    /// Where the accent color is heading.
    accent_goal: Color,
    /// Fast wind after a seek or skip: signed reel speed boost, decaying to 0.
    reel_rush: f32,
    /// When the volume last changed; the deck display shows it for a moment.
    volume_at: f64,
    /// The arrow keys moved the focus and the mouse has not moved since:
    /// the tape at the focus position acts as hovered.
    key_focus: bool,
    /// MPRIS player for status bars and media keys.
    desktop: Desktop,
    /// Cover written out for the desktop: tape id and file. Two files take
    /// turns, so listeners that cache by path still notice a new cover.
    desktop_art: Option<(usize, PathBuf)>,
    /// Playback position on the previous frame, to notice seeks.
    last_position: f32,
    /// The keyboard shortcut sheet over the stage, toggled with `?`.
    show_help: bool,
    /// Track row picked with the arrow keys on a turned tape's card.
    card_cursor: Option<usize>,
    /// Key being held for autorepeat, and when it next fires.
    held: Option<(KeyCode, f64)>,
    last_mouse: Option<Vec2>,
    /// Album whose tracklist is open over the grid, and its scroll in rows.
    grid_card: Option<usize>,
    grid_card_scroll: f32,
    /// Highlight color taken from the playing tape's cover, eased between tapes.
    accent: Color,
    /// Last focused synced-lyric line and when it became focused, for the line change animation.
    lyric_change: (usize, f64),
    /// Tape packed on each deck reel, as radii in pixels.
    packs: (f32, f32),
    /// Tape turned around to show its tracklist, and the J-card painted for it.
    flipped: Option<usize>,
    jcard_page: usize,
    jcard: Option<(usize, usize, RenderTarget)>,
    /// The deck's still parts, painted for the deck rectangle they fit.
    deck_cache: std::cell::RefCell<Option<(Rect, RenderTarget)>>,
    level: f32,
    /// Multisampled canvas the whole frame is drawn into, then copied to the
    /// window: smooth edges without relying on the window's own MSAA.
    canvas: Option<RenderTarget>,
    present: Option<Material>,
    edge: Texture2D,
    /// VU needle positions, 0..1 across the dial (0.87 is 0 VU).
    vu: (f32, f32),
    pressed: [f64; 4],
    saved_at: f64,
    dirty_state: bool,
}

impl App {
    fn layout(&self) -> Layout {
        Layout::new(self.rec.is_some(), !self.queue.is_empty())
    }

    fn new(library: Vec<Album>, saved: Saved, music_root: PathBuf) -> Self {
        let mixes = mixtape::load();
        let mut albums = vec![mixtape::blank()];
        albums.extend(mixes.iter().map(mixtape::album));
        albums.extend(library);
        let start = saved.scroll.clamp(-1.5, (albums.len() as f32 - 3.0).max(-1.5));
        // Decode covers nearest to the saved position first.
        let mut jobs: Vec<_> = albums
            .iter()
            .enumerate()
            .filter(|(_, a)| a.mix.is_none())
            .map(|(i, a)| (i, a.dir.clone(), a.tracks[0].path.clone()))
            .collect();
        jobs.sort_by(|a, b| (a.0 as f32 - start).abs().total_cmp(&(b.0 as f32 - start).abs()));
        let cover_jobs = jobs.len();
        let colors: HashMap<String, [u8; 3]> = colors_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let tapes = albums
            .iter()
            .enumerate()
            .map(|(id, a)| {
                let average = colors.get(&a.rel).copied();
                Tape {
                    id,
                    average,
                    shell: average.map(shell_color).unwrap_or_else(|| Self::plain_shell(a)),
                    face: None,
                    cover: None,
                    blur: None,
                    dirty: true,
                    matched: false,
                    lift: 0.0,
                    dim: 0.0,
                    out: 0.0,
                    flip: 0.0,
                }
            })
            .collect();
        let (covers, covers_done) = library::spawn_cover_workers(jobs);
        let mut app = Self {
            tapes,
            face_pool: Vec::new(),
            fonts: Fonts::load(),
            audio: Audio::new(),
            covers,
            extra_covers: Vec::new(),
            rescan: None,
            covers_done,
            cover_jobs,
            colors_saved: false,
            next_id: albums.len(),
            sort: saved.sort,
            fullscreen: saved.fullscreen,
            plays: saved.plays,
            played_at: saved.played_at,
            learned: saved.learned,
            settings: saved.settings,
            show_settings: false,
            settings_row: 0,
            settings_scroll: 0.0,
            settings_drag: false,
            sfx: !saved.quiet,
            view: saved.view,
            grid_scroll: 0.0,
            grid_target: 0.0,
            grid_restore: Some(saved.grid_top),
            grid_metrics: (1, 1.0, 1.0),
            mixes,
            rec: None,
            renaming: None,
            erase_armed: None,
            toast: None,
            queue: VecDeque::new(),
            show_queue: saved.show_queue,
            queue_scroll: 0.0,
            queue_drag: None,
            ruler_drag: false,
            scroll: start,
            target: start,
            hover: None,
            current: None,
            track: 0,
            music_root,
            lyrics: None,
            lyrics_scroll: 0.0,
            lyric_preview: None,
            list_scroll: 0.0,
            volume: saved.volume.unwrap_or(0.8),
            drag: None,
            knob_drag: None,
            seek_drag: None,
            query: String::new(),
            matches: Vec::new(),
            focus: 0,
            backdrop: (None, None, 0.0),
            slot_in: 0.0,
            reels: (0.0, 0.0),
            reel_speed: 0.0,
            settling: true,
            active_at: 0.0,
            frame_mouse: Vec2::ZERO,
            accent_goal: INK,
            reel_rush: 0.0,
            volume_at: f64::MIN,
            // The tape in front starts out selected, as it was left.
            key_focus: true,
            desktop: if saved.settings.mpris { Desktop::new() } else { Desktop::off() },
            desktop_art: None,
            last_position: 0.0,
            show_help: false,
            card_cursor: None,
            held: None,
            last_mouse: None,
            grid_card: None,
            grid_card_scroll: 0.0,
            accent: INK,
            lyric_change: (0, 0.0),
            packs: (16.0, 5.0),
            flipped: None,
            jcard_page: 0,
            jcard: None,
            deck_cache: Default::default(),
            level: 0.0,
            canvas: None,
            edge: edge_texture(),
            present: load_material(
                ShaderSource::Glsl { vertex: PRESENT_VERTEX, fragment: PRESENT_FRAGMENT },
                MaterialParams::default(),
            )
            .map_err(|e| eprintln!("[tapedeck] present shader: {e}"))
            .ok(),
            vu: (0.0, 0.0),
            pressed: [-1.0; 4],
            saved_at: 0.0,
            dirty_state: false,
            albums,
        };
        app.audio.set_volume(app.volume);
        app.apply_sort(false);
        for (rel, n) in &saved.queue {
            if let Some(i) = app.albums.iter().position(|a| &a.rel == rel) {
                library::read_track_tags(&mut app.albums[i]);
                app.queue.push_back((app.tapes[i].id, *n));
            }
        }
        if let Some(i) = saved.album.and_then(|rel| app.albums.iter().position(|a| a.rel == rel)) {
            app.load(i, saved.track, false, saved.time);
            // Restored as it was left: already in the deck.
            app.tapes[i].out = 1.0;
            app.slot_in = 1.0;
        }
        app
    }

    /// (Re)creates the canvas to match the window size.
    fn ensure_canvas(&mut self) {
        let (w, h) = (screen_width().max(1.0) as u32, screen_height().max(1.0) as u32);
        let fits = self.canvas.as_ref().is_some_and(|c| c.texture.width() as u32 == w && c.texture.height() as u32 == h);
        if !fits {
            let rt = render_target_ex(w, h, RenderTargetParams { sample_count: CANVAS_SAMPLES, depth: true });
            rt.texture.set_filter(FilterMode::Nearest);
            self.canvas = Some(rt);
        }
    }

    /// 2D camera in window coordinates that draws into the canvas.
    fn ui_camera(&self) -> Camera2D {
        let mut cam = Camera2D::from_display_rect(Rect::new(0.0, 0.0, screen_width(), screen_height()));
        cam.render_target = self.canvas.clone();
        cam
    }

    /// Copies the finished canvas to the window.
    fn present(&self) {
        set_default_camera();
        clear_background(BLACK);
        if let Some(c) = &self.canvas {
            if let Some(m) = &self.present {
                gl_use_material(m);
            }
            let params = DrawTextureParams { dest_size: Some(vec2(screen_width(), screen_height())), flip_y: true, ..Default::default() };
            draw_texture_ex(&c.texture, 0.0, 0.0, WHITE, params);
            gl_use_default_material();
        }
    }

    fn plain_shell(album: &Album) -> Color {
        match album.mix {
            Some(0) => Color::from_rgba(214, 211, 202, 255),
            Some(_) => Color::from_rgba(188, 186, 178, 255),
            None => Color::from_rgba(22, 22, 22, 255),
        }
    }

    fn index_of(&self, id: usize) -> Option<usize> {
        self.tapes.iter().position(|t| t.id == id)
    }

    fn mix_index(&self, mix: u64) -> Option<usize> {
        self.albums.iter().position(|a| a.mix == Some(mix))
    }

    /* ---------- Sorting ---------- */

    fn rail_cmp(&self, a: usize, b: usize) -> std::cmp::Ordering {
        let (x, y) = (&self.albums[a], &self.albums[b]);
        // Mixtapes stay pinned at the head of the rail, blank tape first.
        match (x.mix, y.mix) {
            (Some(m), Some(n)) => return m.cmp(&n),
            (Some(_), None) => return std::cmp::Ordering::Less,
            (None, Some(_)) => return std::cmp::Ordering::Greater,
            _ => {}
        }
        let by_artist = || library::icase_cmp(&x.artist, &y.artist).then_with(|| x.year.cmp(&y.year));
        let plays = |a: &Album| self.plays.get(&a.rel).copied().unwrap_or(0);
        match self.sort {
            Sort::Artist => by_artist(),
            Sort::Year => (x.year.is_empty(), &x.year).cmp(&(y.year.is_empty(), &y.year)).then_with(by_artist),
            Sort::Played => plays(y).cmp(&plays(x)).then_with(by_artist),
            // Most recent first; never-played tapes after, by artist.
            Sort::Recent => {
                let at = |a: &Album| self.played_at.get(&a.rel).copied();
                at(y).cmp(&at(x)).then_with(by_artist)
            }
            Sort::Color => {
                let (kx, ky) = (color_key(self.tapes[a].average), color_key(self.tapes[b].average));
                kx.0.cmp(&ky.0).then(kx.1.total_cmp(&ky.1)).then_with(by_artist)
            }
        }
    }

    /// Re-orders the rail for the current sort, keeping indices that point
    /// into it valid. With `anchor`, the view follows the playing (or front) tape.
    fn apply_sort(&mut self, anchor: bool) {
        // The open tracklist is held by position, which the sort changes.
        self.grid_card = None;
        let n = self.albums.len();
        let front = self.current.unwrap_or(((self.target.round() + 1.0).max(0.0) as usize).min(n - 1));
        let anchor_id = self.tapes[front].id;
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| self.rail_cmp(a, b));
        let mut new_pos = vec![0; n];
        for (new, &old) in order.iter().enumerate() {
            new_pos[old] = new;
        }
        let mut albums: Vec<Option<Album>> = std::mem::take(&mut self.albums).into_iter().map(Some).collect();
        let mut tapes: Vec<Option<Tape>> = std::mem::take(&mut self.tapes).into_iter().map(Some).collect();
        self.albums = order.iter().map(|&o| albums[o].take().unwrap()).collect();
        self.tapes = order.iter().map(|&o| tapes[o].take().unwrap()).collect();
        self.current = self.current.map(|i| new_pos[i]);
        self.flipped = self.flipped.map(|i| new_pos[i]);
        if let Some((i, _, _)) = self.jcard.as_mut() {
            *i = new_pos[*i];
        }
        self.backdrop = (self.backdrop.0.map(|i| new_pos[i]), self.backdrop.1.map(|i| new_pos[i]), self.backdrop.2);
        self.hover = None;
        self.refresh_matches();
        if anchor {
            if let Some(i) = self.index_of(anchor_id) {
                self.scroll_to(i as f32 - 1.0);
                self.scroll = self.target;
            }
        }
    }

    fn set_sort(&mut self, sort: Sort) {
        if self.sort != sort {
            self.sort = sort;
            self.apply_sort(true);
            self.save();
        }
    }

    /* ---------- Mixtapes ---------- */

    fn new_mix(&mut self, first: Option<PathBuf>) -> u64 {
        let id = self.mixes.iter().map(|m| m.id).max().unwrap_or(0) + 1;
        let mix = Mixtape { id, name: format!("MIXTAPE {id:02}"), tracks: first.into_iter().collect() };
        self.albums.push(mixtape::album(&mix));
        self.tapes.push(Tape {
            id: self.next_id,
            average: None,
            face: None,
            cover: None,
            blur: None,
            shell: Self::plain_shell(self.albums.last().unwrap()),
            dirty: true,
            matched: false,
            lift: 0.0,
            dim: 0.0,
            out: 0.0,
            flip: 0.0,
        });
        self.next_id += 1;
        self.mixes.push(mix);
        mixtape::save(&self.mixes);
        self.apply_sort(false);
        id
    }

    /// Rebuilds a mixtape's tape after its tracks or name changed.
    fn sync_mix(&mut self, id: u64) {
        let Some(m) = self.mixes.iter().find(|m| m.id == id) else { return };
        let Some(i) = self.mix_index(id) else { return };
        self.albums[i] = mixtape::album(m);
        library::read_track_tags(&mut self.albums[i]);
        self.tapes[i].dirty = true;
        if self.flipped == Some(i) {
            let last_page = self.albums[i].tracks.len().saturating_sub(1) / gfx::JCARD_PAGE_SIZE;
            self.jcard_page = self.jcard_page.min(last_page);
        }
        if self.jcard.as_ref().is_some_and(|(j, _, _)| *j == i) {
            self.jcard = None;
        }
        if self.current == Some(i) {
            let len = self.albums[i].tracks.len();
            if len == 0 {
                self.return_tape();
            } else {
                self.track = self.track.min(len - 1);
            }
        }
        mixtape::save(&self.mixes);
        self.refresh_matches();
    }

    fn record(&mut self, path: PathBuf, title: &str) {
        let id = match self.rec.filter(|id| self.mixes.iter().any(|m| m.id == *id)) {
            Some(id) => {
                self.mixes.iter_mut().find(|m| m.id == id).unwrap().tracks.push(path);
                self.sync_mix(id);
                id
            }
            None => {
                let id = self.new_mix(Some(path));
                self.sync_mix(id);
                id
            }
        };
        self.rec = Some(id);
        let name = self.mixes.iter().find(|m| m.id == id).map(|m| m.name.clone()).unwrap_or_default();
        self.toast = Some((format!("+ {title}  >  {name}"), get_time()));
    }

    fn unrecord(&mut self, id: u64, n: usize) {
        let Some(m) = self.mixes.iter_mut().find(|m| m.id == id) else { return };
        if n < m.tracks.len() {
            m.tracks.remove(n);
            if self.current.is_some_and(|i| self.albums[i].mix == Some(id)) && n < self.track {
                self.track -= 1;
            }
            self.sync_mix(id);
        }
    }

    fn erase_mix(&mut self, id: u64) {
        let Some(i) = self.mix_index(id) else { return };
        if self.current == Some(i) {
            self.return_tape();
        }
        let shift = |x: Option<usize>| x.filter(|&x| x != i).map(|x| if x > i { x - 1 } else { x });
        self.current = shift(self.current);
        self.flipped = shift(self.flipped);
        self.jcard = self.jcard.take().and_then(|(j, page, rt)| shift(Some(j)).map(|j| (j, page, rt)));
        self.backdrop = (shift(self.backdrop.0), shift(self.backdrop.1), self.backdrop.2);
        self.hover = None;
        self.albums.remove(i);
        if let Some(rt) = self.tapes.remove(i).face {
            self.face_pool.push(rt);
        }
        self.mixes.retain(|m| m.id != id);
        if self.rec == Some(id) {
            self.rec = None;
        }
        if self.renaming == Some(id) {
            self.renaming = None;
        }
        mixtape::save(&self.mixes);
        self.refresh_matches();
        self.toast = Some(("MIXTAPE ERASED".into(), get_time()));
    }

    /// Typing goes to the label of the mixtape being named.
    fn rename_keys(&mut self) -> bool {
        let Some(id) = self.renaming else { return false };
        let mut changed = false;
        while let Some(c) = get_char_pressed() {
            if !c.is_control() {
                if let Some(m) = self.mixes.iter_mut().find(|m| m.id == id) {
                    if m.name.chars().count() < 40 {
                        m.name.push(c);
                        changed = true;
                    }
                }
            }
        }
        if is_key_pressed(KeyCode::Backspace) {
            if let Some(m) = self.mixes.iter_mut().find(|m| m.id == id) {
                m.name.pop();
                changed = true;
            }
        }
        if is_key_pressed(KeyCode::Enter) || is_key_pressed(KeyCode::Escape) {
            if let Some(m) = self.mixes.iter_mut().find(|m| m.id == id) {
                if m.name.trim().is_empty() {
                    m.name = format!("MIXTAPE {id:02}");
                }
            }
            self.renaming = None;
            changed = true;
        }
        if changed {
            self.sync_mix(id);
        }
        true
    }

    fn save(&mut self) {
        self.saved_at = get_time();
        self.dirty_state = false;
        let state = Saved {
            scroll: self.target,
            album: self.current.map(|i| self.albums[i].rel.clone()),
            track: self.track,
            time: self.audio.position().as_secs_f32(),
            volume: Some(self.volume),
            sort: self.sort,
            fullscreen: self.fullscreen,
            queue: self
                .queue
                .iter()
                .filter_map(|&(id, n)| Some((self.albums[self.index_of(id)?].rel.clone(), n)))
                .collect(),
            plays: self.plays.clone(),
            played_at: self.played_at.clone(),
            learned: self.learned,
            view: self.view,
            quiet: !self.sfx,
            grid_top: self.grid_top(),
            show_queue: self.show_queue,
            settings: self.settings,
        };
        if let Some(path) = state_path() {
            let _ = std::fs::create_dir_all(path.parent().unwrap());
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, serde_json::to_string(&state).unwrap_or_default()).is_ok() {
                let _ = std::fs::rename(tmp, path);
            }
        }
    }

    /// Scans the music folder again in the background; `tick` applies the result.
    fn start_rescan(&mut self) {
        if self.rescan.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let root = self.music_root.clone();
        std::thread::spawn(move || {
            let _ = tx.send(library::scan(&root));
        });
        self.rescan = Some(rx);
        self.toast = Some(("SCANNING THE MUSIC FOLDER".into(), get_time()));
    }

    /// Brings the rail in line with the music folder after a rescan: tapes
    /// for new albums join it, tapes whose album is gone leave it (except the
    /// one in the deck). Saved plays and positions are kept by album path.
    fn apply_rescan(&mut self, scanned: Vec<Album>) {
        let found: std::collections::HashSet<String> = scanned.iter().map(|a| a.rel.clone()).collect();
        let known: std::collections::HashSet<String> = self.albums.iter().filter(|a| a.mix.is_none()).map(|a| a.rel.clone()).collect();
        let mut removed = 0;
        let mut k = 0;
        while k < self.albums.len() {
            let gone = self.albums[k].mix.is_none() && !found.contains(&self.albums[k].rel) && self.current != Some(k);
            if gone {
                if let Some(face) = self.tapes[k].face.take() {
                    self.face_pool.push(face);
                }
                self.albums.remove(k);
                self.tapes.remove(k);
                if let Some(c) = self.current.as_mut().filter(|c| **c > k) {
                    *c -= 1;
                }
                removed += 1;
            } else {
                k += 1;
            }
        }
        let mut jobs = Vec::new();
        for album in scanned.into_iter().filter(|a| !known.contains(&a.rel)) {
            let id = self.next_id;
            self.next_id += 1;
            jobs.push((id, album.dir.clone(), album.tracks[0].path.clone()));
            self.tapes.push(Tape {
                id,
                average: None,
                shell: Self::plain_shell(&album),
                face: None,
                cover: None,
                blur: None,
                dirty: true,
                matched: false,
                lift: 0.0,
                dim: 0.0,
                out: 0.0,
                flip: 0.0,
            });
            self.albums.push(album);
        }
        let added = jobs.len();
        if !jobs.is_empty() {
            self.extra_covers.push(library::spawn_cover_workers(jobs).0);
        }
        // Positions shifted: drop everything that pointed into the rail by index.
        self.hover = None;
        self.flipped = None;
        self.grid_card = None;
        self.jcard = None;
        self.backdrop = (self.current, None, get_time());
        self.apply_sort(true);
        if !self.query.is_empty() {
            self.refresh_matches();
        }
        self.toast = Some((
            match (added, removed) {
                (0, 0) => "LIBRARY UP TO DATE".to_string(),
                (a, r) => format!("LIBRARY  +{a}  -{r}"),
            },
            get_time(),
        ));
    }

    /* ---------- Playback ---------- */

    fn load(&mut self, i: usize, track: usize, play: bool, time: f32) {
        if play {
            self.learned |= LEARNED_LISTEN;
        }
        // Empty tapes cannot play: the blank one starts a new mixtape, an
        // empty mixtape becomes the recording target.
        if self.albums[i].tracks.is_empty() {
            let id = match self.albums[i].mix {
                Some(0) => self.new_mix(None),
                Some(id) => id,
                None => return,
            };
            self.rec = Some(id);
            self.renaming = Some(id);
            if let Some(j) = self.mix_index(id) {
                self.scroll_to(j as f32 - 1.0);
            }
            self.toast = Some(("NAME THE TAPE, THEN RIGHT-CLICK TRACKS TO RECORD".into(), get_time()));
            return;
        }
        library::read_track_tags(&mut self.albums[i]);
        if self.current != Some(i) {
            self.slot_in = 0.0;
            self.list_scroll = 0.0;
        }
        if self.flipped == Some(i) {
            self.flipped = None;
        }
        let new_tape = self.current != Some(i);
        self.current = Some(i);
        self.set_backdrop(i);
        self.play_track(track, play, time);
        // After the track, which may have opened the output stream.
        if new_tape && play {
            self.sfx(Sfx::Insert);
        }
    }

    fn play_track(&mut self, n: usize, play: bool, time: f32) {
        let Some(i) = self.current else { return };
        let album = &self.albums[i];
        if album.tracks.is_empty() {
            return;
        }
        if play {
            *self.plays.entry(album.rel.clone()).or_insert(0) += 1;
            self.played_at.insert(album.rel.clone(), now_secs());
        }
        self.track = n % album.tracks.len();
        let track = &album.tracks[self.track];
        self.lyrics = library::read_lyrics(track, &self.music_root);
        self.lyrics_scroll = 0.0;
        self.lyric_preview = None;
        let path = track.path.clone();
        self.audio.load(&path, Duration::from_secs_f32(time.max(0.0)), play);
        // Keep the playing row in view.
        let rows = self.layout().list_rows() as f32;
        let n = self.track as f32;
        if n < self.list_scroll || n > self.list_scroll + rows - 1.0 {
            self.list_scroll = (n - (rows / 2.0).floor()).max(0.0);
        }
        self.save();
    }

    fn toggle(&mut self) {
        match self.current {
            Some(_) => self.audio.toggle(),
            None => {
                let i = self.hover.unwrap_or_else(|| (self.target.round() + 1.0).max(0.0) as usize).min(self.albums.len() - 1);
                if !self.albums[i].tracks.is_empty() {
                    self.load(i, 0, true, 0.0);
                }
            }
        }
        self.save();
    }

    fn next(&mut self) {
        if self.play_queued() {
            return;
        }
        let Some(i) = self.current else { return };
        let next_track = self.track + 1;
        if next_track < self.albums[i].tracks.len() {
            self.play_track(next_track, true, 0.0);
            return;
        }
        // At the end of a tape, follow the displayed rail order.
        let len = self.albums.len();
        let Some(n) = (1..len).map(|k| (i + k) % len).find(|&n| !self.albums[n].tracks.is_empty()) else { return };
        self.scroll_to(n as f32 - 1.0);
        self.load(n, 0, true, 0.0);
    }

    /* ---------- Queue ---------- */

    /// Plays the first queued track that still exists. False when the queue is empty.
    fn play_queued(&mut self) -> bool {
        while let Some((id, n)) = self.queue.pop_front() {
            if let Some(i) = self.index_of(id).filter(|&i| n < self.albums[i].tracks.len()) {
                self.load(i, n, true, 0.0);
                return true;
            }
        }
        false
    }

    fn enqueue(&mut self, i: usize, tracks: std::ops::Range<usize>) {
        library::read_track_tags(&mut self.albums[i]);
        let album = &self.albums[i];
        if album.tracks.is_empty() || tracks.is_empty() {
            return;
        }
        let what = if tracks.len() == album.tracks.len() { album.album.clone() } else { album.tracks[tracks.start].title.clone() };
        let id = self.tapes[i].id;
        // With Shift held the tracks go to the head of the queue, to play next.
        let next = is_key_down(KeyCode::LeftShift) || is_key_down(KeyCode::RightShift);
        if next {
            for n in tracks.rev() {
                self.queue.push_front((id, n));
            }
        } else {
            self.queue.extend(tracks.map(|n| (id, n)));
        }
        self.learned |= LEARNED_QUEUE;
        let label = if next { "+ NEXT" } else { "+ QUEUED" };
        self.toast = Some((format!("{label}  {}", what.to_uppercase()), get_time()));
        // An empty deck starts right away.
        if self.current.is_none() {
            self.play_queued();
        }
    }

    /// Next queued track, as "title (+more)".
    fn up_next(&self) -> Option<String> {
        let &(id, n) = self.queue.front()?;
        let i = self.index_of(id)?;
        let title = &self.albums[i].tracks.get(n)?.title;
        let more = self.queue.len() - 1;
        // Queuing what is already playing plays it again; say so.
        let again = if self.current == Some(i) && self.track == n { "REPEAT  " } else { "" };
        Some(if more > 0 { format!("{again}{title}  +{more}") } else { format!("{again}{title}") })
    }

    /// The "up next" badge follows the last visible row.
    fn queue_pill(&self, lay: &Layout) -> Rect {
        if self.queue.is_empty() {
            return Rect::default();
        }
        let visible_rows = if self.show_queue {
            self.queue.len().saturating_sub(self.queue_scroll.floor() as usize)
        } else if let Some(i) = self.current {
            self.albums[i].tracks.len().saturating_sub(self.list_scroll as usize)
        } else {
            lay.list_rows()
        };
        let y = (lay.list.y + 50.0 + visible_rows.min(lay.list_rows()) as f32 * 22.0)
            .min(lay.list.bottom()) + 4.0;
        Rect::new(lay.side.x + SP_S, y, lay.side.w - 2.0 * SP_S, 18.0)
    }

    fn prev(&mut self) {
        let Some(i) = self.current else { return };
        if self.audio.position().as_secs_f32() > 3.0 {
            self.audio.seek(Duration::ZERO);
        } else {
            let len = self.albums[i].tracks.len();
            self.play_track((self.track + len - 1) % len, true, 0.0);
        }
    }

    /// Takes the tape out of the deck and slides it back into its gap.
    fn return_tape(&mut self) {
        self.sfx(Sfx::Eject);
        self.audio.stop();
        self.current = None;
        self.lyrics = None;
        self.lyric_preview = None;
        self.slot_in = 0.0;
        self.save();
    }

    fn scroll_to(&mut self, v: f32) {
        let next = v.clamp(-1.5, (self.albums.len() as f32 - 3.0).max(-1.5));
        if (next - self.target).abs() > 1e-4 {
            self.target = next;
            self.dirty_state = true;
        }
        // The rail keeps its tape second from the front; the grid centers its row.
        if self.view == View::Grid {
            let i = (v + 1.0).round().clamp(0.0, self.albums.len().saturating_sub(1) as f32) as usize;
            let (cols, row_h, view_h) = self.grid_metrics;
            let row = (i / cols.max(1)) as f32;
            self.set_grid_scroll(row * row_h - (view_h - row_h) / 2.0);
        }
    }

    /// First album of the top row in view in the grid.
    fn grid_top(&self) -> usize {
        match self.grid_restore {
            Some(top) => top,
            None => {
                let (cols, row_h, _) = self.grid_metrics;
                (self.grid_target / row_h.max(1.0)).round() as usize * cols
            }
        }
    }

    fn set_grid_scroll(&mut self, v: f32) {
        let (cols, row_h, view_h) = self.grid_metrics;
        let rows = self.albums.len().div_ceil(cols.max(1)) as f32;
        self.grid_target = v.clamp(0.0, (rows * row_h - view_h).max(0.0));
    }

    fn grid(&self, lay: &Layout) -> Grid {
        let area = lay.grid_area();
        let gap = 18.0;
        let cols = ((area.w + gap) / (150.0 + gap)).floor().max(1.0) as usize;
        let cell = (area.w - gap * (cols - 1) as f32) / cols as f32;
        Grid { area, cols, cell, gap, row_h: cell + 40.0 + gap }
    }

    fn set_view(&mut self, view: View) {
        if view == self.view {
            return;
        }
        // Keep roughly the same albums in view across the switch.
        let first = match self.view {
            View::Rail => self.target + 1.0,
            View::Grid => {
                let (cols, row_h, _) = self.grid_metrics;
                ((self.grid_target / row_h).round() as usize * cols) as f32
            }
        };
        self.view = view;
        self.flipped = None;
        self.grid_card = None;
        match view {
            View::Grid => {
                self.scroll_to(first - 1.0);
                self.grid_scroll = self.grid_target;
            }
            View::Rail => self.scroll_to(first - 1.0),
        }
        self.dirty_state = true;
    }

    /// Visible stretch of the library, for the index strip, as album positions.
    fn view_span(&self) -> (f32, f32) {
        match self.view {
            View::Rail => (self.scroll + 0.5, self.scroll + 9.0),
            View::Grid => {
                let (cols, row_h, view_h) = self.grid_metrics;
                let first = (self.grid_scroll / row_h).floor().max(0.0) * cols as f32;
                let last = ((self.grid_scroll + view_h) / row_h).ceil() * cols as f32 - 1.0;
                (first, last)
            }
        }
    }

    fn set_backdrop(&mut self, i: usize) {
        if self.backdrop.0 != Some(i) {
            self.backdrop = (Some(i), self.backdrop.0, get_time());
        }
    }

    /* ---------- Per-frame update ---------- */

    fn receive_covers(&mut self) {
        const COVERS_PER_FRAME: usize = 6;
        let workers_finished = self.covers_done.load(Ordering::Acquire) >= self.cover_jobs;
        let mut received = 0;
        let mut loaded_dirs = Vec::new();
        let incoming: Vec<Cover> = self.covers.try_iter().chain(self.extra_covers.iter().flat_map(|rx| rx.try_iter())).take(COVERS_PER_FRAME).collect();
        for cover in incoming {
            received += 1;
            let Some(i) = self.index_of(cover.idx) else { continue };
            loaded_dirs.push(self.albums[i].dir.clone());
            let tape = &mut self.tapes[i];
            let tex = Texture2D::from_rgba8(cover.width, cover.height, &cover.rgba);
            tex.set_filter(FilterMode::Linear);
            let blur = Texture2D::from_rgba8(BLUR_SIZE, BLUR_SIZE, &cover.blur);
            blur.set_filter(FilterMode::Linear);
            tape.average = Some(cover.average);
            tape.shell = shell_color(cover.average);
            tape.cover = Some(tex);
            tape.blur = Some(blur);
            tape.dirty = true;
        }
        if !loaded_dirs.is_empty() {
            // Mixtape labels are collages of these covers; only mark dirty when a matching cover arrived.
            for (tape, album) in self.tapes.iter_mut().zip(&self.albums) {
                if album.mix.is_some_and(|m| m > 0)
                    && album.tracks.iter().any(|t| t.path.parent().is_some_and(|d| loaded_dirs.iter().any(|ld| ld == d)))
                {
                    tape.dirty = true;
                }
            }
        }
        // Once every cover is in, cache the colors and settle the color sort.
        if !self.colors_saved && workers_finished && received < COVERS_PER_FRAME {
            self.colors_saved = true;
            let colors: HashMap<&str, [u8; 3]> = self
                .albums
                .iter()
                .zip(&self.tapes)
                .filter_map(|(a, t)| Some((a.rel.as_str(), t.average?)))
                .collect();
            if let Some(path) = colors_path() {
                let _ = std::fs::create_dir_all(path.parent().unwrap());
                let _ = std::fs::write(path, serde_json::to_string(&colors).unwrap_or_default());
            }
            if self.sort == Sort::Color {
                self.apply_sort(true);
            }
        }
    }

    /// Covers of the albums a mixtape's tracks come from, for its collage.
    fn mix_covers(&self, i: usize) -> Vec<Texture2D> {
        let mut out: Vec<Texture2D> = Vec::new();
        let mut seen = Vec::new();
        for t in &self.albums[i].tracks {
            let Some(dir) = t.path.parent() else { continue };
            let Some(j) = self.albums.iter().position(|a| a.mix.is_none() && a.dir == dir) else { continue };
            if seen.contains(&j) {
                continue;
            }
            seen.push(j);
            if let Some(tex) = &self.tapes[j].cover {
                out.push(tex.clone());
                if out.len() == 4 {
                    break;
                }
            }
        }
        out
    }

    fn render_faces(&mut self) {
        let keep = (VISIBLE.0 - 12.0)..=(VISIBLE.1 + 12.0);
        for (i, tape) in self.tapes.iter_mut().enumerate() {
            if tape.face.is_some() && !keep.contains(&(i as f32 - self.scroll)) {
                self.face_pool.push(tape.face.take().unwrap());
                tape.dirty = true;
            }
        }
        let mut budget = FACE_RENDERS_PER_FRAME;
        for i in self.visible() {
            if budget == 0 {
                break;
            }
            if !self.tapes[i].dirty {
                continue;
            }
            let (covers, style) = match self.albums[i].mix {
                Some(0) => (Vec::new(), FaceStyle::Blank),
                Some(_) => (self.mix_covers(i), FaceStyle::Mix),
                None => (self.tapes[i].cover.iter().cloned().collect(), FaceStyle::Album),
            };
            let tape = &mut self.tapes[i];
            let target = tape.face.get_or_insert_with(|| self.face_pool.pop().unwrap_or_else(|| face_target(1.0)));
            let album = &self.albums[i];
            let stripe = accent_color(tape.average);
            gfx::render_face(target, &self.fonts, &covers, tape.shell, stripe, &album.album, &album.artist, &album.year, style);
            tape.dirty = false;
            budget -= 1;
        }

        // Paint the J-card of a tape that was just turned around.
        if let Some(i) = self.flipped {
            let page = self.jcard_page;
            if self.jcard.as_ref().is_none_or(|(j, cached_page, _)| *j != i || *cached_page != page) {
                library::read_track_tags(&mut self.albums[i]);
                let album = &self.albums[i];
                let tracks: Vec<(String, String)> = album
                    .tracks
                    .iter()
                    .map(|t| (t.title.clone(), t.duration.map(|d| fmt_time(d.as_secs_f32())).unwrap_or_default()))
                    .collect();
                let target = match self.jcard.take() {
                    Some((_, _, rt)) => rt,
                    None => face_target(gfx::JCARD_SCALE),
                };
                let plays = self.plays.get(&album.rel).copied().unwrap_or(0);
                let hires = album.tracks.iter().any(|t| t.hires);
                gfx::render_jcard(&target, &self.fonts, &album.album, &album.artist, &tracks, page, plays, hires);
                self.jcard = Some((i, page, target));
            }
        }
    }

    /// Visible tape indices, nearest first.
    fn visible(&self) -> std::ops::Range<usize> {
        let lo = (self.scroll + VISIBLE.0).ceil().max(0.0) as usize;
        let hi = (self.scroll + VISIBLE.1).floor();
        if hi < 0.0 || self.albums.is_empty() {
            return 0..0;
        }
        let end = (hi as usize + 1).min(self.albums.len());
        lo.min(end)..end
    }

    /// The tape's resting place on the rail; what is left when it is ejected.
    fn rail_matrix(&self, lay: &Layout, i: usize) -> Mat4 {
        let p = i as f32 - self.scroll;
        Mat4::from_translation(lay.rail_origin() + STEP * p) * Mat4::from_rotation_y(YAW)
    }

    fn tape_matrix(&self, lay: &Layout, i: usize) -> Mat4 {
        let t = &self.tapes[i];
        // Turning a tape around happens in two overlapping moves: it first
        // rises out of the rail, clear of the tapes in front, then turns and
        // comes forward. Turning back plays the same path in reverse.
        let ease = |x: f32| {
            let x = x.clamp(0.0, 1.0);
            x * x * (3.0 - 2.0 * x)
        };
        let up = ease(t.flip / 0.35);
        let flip = ease((t.flip - 0.28) / 0.72);
        let lift = t.lift * (1.0 - up);
        let rail = self.rail_matrix(lay, i).w_axis.truncate();
        let s = lay.stage;
        let origin_x = lay.rail_x() + RAIL_TO_ORIGIN;
        // World position of a point at a fraction of the stage (from its left
        // and top edges) and a depth towards the camera.
        let at = |fx: f32, fy: f32, depth: f32| {
            let ppu = 100.0 * PERSPECTIVE / (PERSPECTIVE - depth);
            vec3((fx - origin_x) * s.w / ppu, (0.30 - fy) * s.h / ppu, depth)
        };

        // Loading: the tape is pulled up out of the rail, comes forward
        // facing the viewer for a beat, then flies off the stage's lower left
        // edge into the deck. A tape coming back skips the presentation, which
        // belongs to the tape replacing it: it flies in, slowing down, and
        // drops straight into its place.
        let returning = self.current != Some(i);
        let (pull, present, fly) = if returning {
            (ease(t.out / 0.45), 0.0, ease((t.out - 0.45) / 0.55))
        } else {
            (ease(t.out / 0.25), ease((t.out - 0.15) / 0.4), ((t.out - 0.58) / 0.42).clamp(0.0, 1.0).powi(3))
        };
        let pulled = rail + vec3(0.0, 1.5, 0.5);
        let shown = at(0.36, 0.48, 3.6);
        let gone = at(-0.3, 0.92, 2.4);
        let travel = rail.lerp(pulled, pull).lerp(shown, present).lerp(gone, fly)
            + vec3(0.0, (fly * PI).sin() * 0.5, 0.0)
            - rail;
        let rise = vec3(0.0, lift * 0.46, 0.0) + travel;

        // Move the turned tape in front of the rail and keep the full label in view.
        let depth = 5.2;
        let pixels_per_unit = 100.0 * PERSPECTIVE / (PERSPECTIVE - depth);
        let inspect = at(0.46, 0.52, depth);
        let inspect_scale = (0.82 * s.w / (TAPE.x * pixels_per_unit))
            .min(0.66 * s.h / (TAPE.y * pixels_per_unit))
            .clamp(0.05, 1.12);
        let scale = (1.0 + (inspect_scale - 1.0) * flip) * (1.0 - 0.25 * fly);
        let raised = rail + vec3(0.0, FLIP_RISE * up, 0.4 * up);
        // A slight arc keeps it above the front tapes on the way down to the front.
        let arc = vec3(0.0, (flip * PI).sin() * 0.4, 0.0);
        // Facing the viewer while presented; leaning into the turn as it flies.
        let yaw = YAW * (1.0 - 0.6 * flip) * (1.0 - present) - 0.5 * fly;
        let pitch = lift * 0.07 - 0.18 * pull * (1.0 - present) + 0.2 * fly;
        let roll = 0.06 * present * (1.0 - fly) + 0.35 * fly;
        Mat4::from_translation(raised.lerp(inspect, flip) + arc + rise)
            * Mat4::from_rotation_y(yaw)
            * Mat4::from_rotation_x(pitch)
            * Mat4::from_rotation_z(roll)
            * Mat4::from_rotation_y(PI * flip)
            * Mat4::from_scale(Vec3::splat(scale))
    }

    /// The ejected tape is picked by its gap; the others by their body.
    fn pick_matrix(&self, lay: &Layout, i: usize) -> Mat4 {
        if self.tapes[i].out > 0.5 { self.rail_matrix(lay, i) } else { self.tape_matrix(lay, i) }
    }

    fn pick(&self, lay: &Layout, mouse: Vec2) -> Option<usize> {
        if self.view == View::Grid {
            let g = self.grid(lay);
            if !g.area.contains(mouse) {
                return None;
            }
            // A cell is its cover plus the caption under it.
            return g.visible(self.albums.len(), self.grid_scroll).find(|&i| {
                let c = g.cover(i, self.grid_scroll);
                Rect::new(c.x, c.y, c.w, c.h + 40.0).contains(mouse)
            });
        }
        if !lay.stage.contains(mouse) || mouse.y >= lay.rail_floor() {
            return None;
        }
        let cam = lay.camera().matrix;
        let h = TAPE / 2.0;
        let inside = |m: Mat4, z| {
            let corners = [vec3(-h.x, h.y, z), vec3(h.x, h.y, z), vec3(h.x, -h.y, z), vec3(-h.x, -h.y, z)];
            let q = corners.map(|c| lay.project(&cam, m.transform_point3(c)));
            let side = |a: Vec2, b: Vec2| (b - a).perp_dot(mouse - a);
            let s = [side(q[0], q[1]), side(q[1], q[2]), side(q[2], q[3]), side(q[3], q[0])];
            s.iter().all(|&v| v >= 0.0) || s.iter().all(|&v| v <= 0.0)
        };
        if let Some(i) = self.flipped
            && self.tapes[i].flip > 0.5
            && inside(self.tape_matrix(lay, i), -h.z - 0.002)
        {
            return Some(i);
        }
        self.visible().into_iter().find(|&i| {
            // A hovered tape rises out from under the cursor; keep its resting
            // place hot too, or it would drop, catch the cursor and rise again.
            let turning = self.tapes[i].flip > 0.5;
            !turning && (inside(self.pick_matrix(lay, i), h.z)
                || (self.hover == Some(i) && self.tapes[i].out < 0.5 && inside(self.rail_matrix(lay, i), h.z)))
        })
    }

    /// Which J-card row of a turned tape is under the mouse.
    fn jcard_row(&self, lay: &Layout, i: usize, mouse: Vec2) -> Option<(usize, Rect)> {
        let s = lay.stage;
        let ndc = vec2((mouse.x - s.x) / s.w * 2.0 - 1.0, 1.0 - (mouse.y - s.y) / s.h * 2.0);
        let inv = lay.camera().matrix.inverse();
        let local = self.tape_matrix(lay, i).inverse();
        let a = local.transform_point3(inv.project_point3(ndc.extend(-1.0)));
        let b = local.transform_point3(inv.project_point3(ndc.extend(1.0)));
        let h = TAPE / 2.0;
        let t = (JCARD_Z - a.z) / (b.z - a.z);
        let hit = a + (b - a) * t;
        // The card is seen from behind, so its x runs right to left.
        let px = vec2((h.x - hit.x) / TAPE.x * FACE_W, (h.y - hit.y) / TAPE.y * FACE_H);
        let range = gfx::jcard_range(self.albums[i].tracks.len(), self.jcard_page);
        let rows = gfx::jcard_rows(range.len());
        range.zip(rows).find(|(_, row)| row.contains(px))
    }

    fn input(&mut self, lay: &Layout) {
        let mouse: Vec2 = mouse_position().into();
        let queue_pill = self.queue_pill(lay);
        let (wx, wy) = mouse_wheel();
        let wheel = if wy != 0.0 { wy } else { wx };
        let wheel = wheel.signum() * wheel.abs().min(3.0);
        let buttons = [MouseButton::Left, MouseButton::Right, MouseButton::Middle];
        if mouse != self.frame_mouse || wheel != 0.0 || !get_keys_down().is_empty() || buttons.iter().any(|&b| is_mouse_button_down(b)) {
            self.active_at = get_time();
        }
        self.frame_mouse = mouse;
        // Ctrl+, opens the settings, which then take all input.
        let ctrl = is_key_down(KeyCode::LeftControl) || is_key_down(KeyCode::RightControl);
        if self.show_settings {
            self.input_settings();
            return;
        }
        if ctrl && is_key_pressed(KeyCode::Comma) {
            self.open_settings(true);
            while get_char_pressed().is_some() {}
            return;
        }

        let grid = self.grid(lay);
        // A resize changes columns and row height: keep the same album at
        // the top of the grid rather than the same pixel offset.
        let (cols, row_h, _) = self.grid_metrics;
        if (cols, row_h) != (grid.cols, grid.row_h) && self.grid_restore.is_none() {
            self.grid_restore = Some(self.grid_top());
        }
        self.grid_metrics = (grid.cols, grid.row_h, grid.area.h);
        if let Some(top) = self.grid_restore.take() {
            self.grid_target = (top / grid.cols) as f32 * grid.row_h;
            self.grid_scroll = self.grid_target;
        }
        // Keeps the grid in range when the window is resized.
        self.set_grid_scroll(self.grid_target);
        let gridded = self.view == View::Grid;
        let on_lyrics = !gridded && self.input_lyrics(lay, mouse, wheel);
        let card_open = self.grid_card.is_some();
        let on_card = gridded && self.input_grid_card(lay, mouse, wheel);
        let picked = if on_card { None } else { self.pick(lay, mouse) };
        let wheel = if on_card { 0.0 } else { wheel };

        let scrolled_card = self.flipped.filter(|&i| {
            wheel != 0.0 && !on_lyrics && self.albums[i].tracks.len() > gfx::JCARD_PAGE_SIZE
                && self.tapes[i].flip > 0.5 && picked == Some(i)
        });
        if let Some(i) = scrolled_card {
            let last_page = (self.albums[i].tracks.len() - 1) / gfx::JCARD_PAGE_SIZE;
            self.jcard_page = if wheel < 0.0 {
                (self.jcard_page + 1).min(last_page)
            } else {
                self.jcard_page.saturating_sub(1)
            };
        }

        // Stage: wheel, drag, click.
        if wheel != 0.0 && !on_lyrics && scrolled_card.is_none() && lay.stage.contains(mouse) {
            if gridded {
                self.set_grid_scroll(self.grid_target - wheel * 80.0);
                self.dirty_state = true;
            } else {
                self.scroll_to(self.target - wheel * 0.45);
                self.saved_at = get_time() - 4.0;
                // Scrolling moves the focus, as the arrows do: the tape in
                // front lifts until the mouse moves again.
                self.key_focus = true;
                self.last_mouse = Some(mouse);
            }
        }
        // Ruler and the switches sit on the stage but are not part of the rail.
        let on_controls = on_lyrics
            || lay.ruler.contains(mouse)
            || self.sort_pills(lay).iter().any(|(_, r)| r.contains(mouse))
            || self.view_pills(lay).iter().any(|(_, r)| r.contains(mouse))
            || self.sfx_pill(lay).contains(mouse);
        if is_mouse_button_pressed(MouseButton::Left) {
            if lay.ruler.contains(mouse) {
                self.ruler_drag = true;
            } else if self.sfx_pill(lay).contains(mouse) {
                self.sfx = !self.sfx;
                self.sfx(Sfx::Key);
                self.save();
            } else if let Some((view, _)) = self.view_pills(lay).into_iter().find(|(_, r)| r.contains(mouse)) {
                self.set_view(view);
            } else if let Some((sort, _)) = self.sort_pills(lay).into_iter().find(|(_, r)| r.contains(mouse)) {
                self.set_sort(sort);
            } else if !on_lyrics && lay.stage.contains(mouse) {
                self.drag = Some((mouse.x, self.target, false));
            }
        }
        if self.ruler_drag {
            let f = ((mouse.x - lay.ruler.x) / lay.ruler.w).clamp(0.0, 1.0);
            self.scroll_to(f * (self.albums.len() - 1) as f32 - 1.0);
            if is_mouse_button_released(MouseButton::Left) {
                self.ruler_drag = false;
                self.save();
            }
        }
        if let Some((x0, start, moved)) = self.drag.as_mut() {
            if (mouse.x - *x0).abs() > 5.0 {
                *moved = true;
            }
            if *moved && !gridded {
                let v = *start - (mouse.x - *x0) / 90.0;
                self.target = v.clamp(-1.5, (self.albums.len() as f32 - 3.0).max(-1.5));
                self.dirty_state = true;
            }
        }
        if is_mouse_button_released(MouseButton::Left) {
            if let Some((_, _, moved)) = self.drag.take() {
                if !moved {
                    if let Some(i) = picked {
                        if gridded && self.current == Some(i) {
                            // The grid has no gap to return the tape to; its cover pauses instead.
                            self.toggle();
                        } else if self.current == Some(i) && self.tapes[i].out > 0.5 {
                            self.return_tape();
                        } else if self.flipped == Some(i) && self.tapes[i].flip > 0.5 {
                            let track = self.jcard_row(lay, i, mouse).map(|(track, _)| track);
                            if let Some(track) = track.or_else(|| self.albums[i].tracks.is_empty().then_some(0)) {
                                self.flipped = None;
                                self.load(i, track, true, 0.0);
                            }
                        } else {
                            self.load(i, self.start_track(i), true, 0.0);
                        }
                    }
                } else {
                    self.save();
                }
            }
        }
        self.hover = if self.drag.is_some_and(|d| d.2) || self.ruler_drag || self.seek_drag.is_some() || on_controls { None } else { picked };
        // Arrow keys and the wheel move a focus that stands in for the mouse
        // until it moves again. The first frame only records where it is.
        match self.last_mouse {
            Some(last) if mouse.distance(last) > 2.0 => {
                self.key_focus = false;
                self.last_mouse = Some(mouse);
            }
            None => self.last_mouse = Some(mouse),
            _ => {}
        }
        if self.key_focus && self.query.is_empty() {
            let i = (self.target.round() + 1.0).clamp(0.0, self.albums.len().saturating_sub(1) as f32) as usize;
            self.hover = Some(i);
        }
        // Right click turns a tape around to read its tracklist.
        // In the grid a right click opens the album's tracklist instead.
        if is_mouse_button_pressed(MouseButton::Right) && gridded && !card_open {
            if let Some(i) = picked {
                library::read_track_tags(&mut self.albums[i]);
                self.grid_card = Some(i);
                self.grid_card_scroll = 0.0;
                self.learned |= LEARNED_FLIP;
            }
        }
        if is_mouse_button_pressed(MouseButton::Right) && !gridded {
            if let Some(i) = self.hover.filter(|&i| self.current != Some(i)) {
                self.flipped = if self.flipped == Some(i) { None } else { Some(i) };
                self.card_cursor = None;
                self.jcard_page = 0;
                self.learned |= LEARNED_FLIP;
            }
        }
        if self.query.is_empty() && is_key_pressed(KeyCode::Escape) {
            self.flipped = None;
        }
        let focused = if self.query.is_empty() { None } else { self.matches.get(self.focus).map(|m| m.0) };
        if let Some(i) = self.hover.or(focused) {
            self.set_backdrop(i);
        } else if let Some(i) = self.current {
            self.set_backdrop(i);
        }

        // Queue list, when open, takes the side list's place.
        if self.queue.is_empty() {
            self.show_queue = false;
        }
        if self.show_queue {
            let rows = lay.list_rows() as f32;
            if wheel != 0.0 && lay.list.contains(mouse) {
                self.queue_scroll = (self.queue_scroll - wheel).clamp(0.0, (self.queue.len() as f32 - rows).max(0.0)).round();
            }
            if let Some(k) = self.queue_row_at(lay, mouse) {
                if is_mouse_button_pressed(MouseButton::Left) {
                    self.queue_drag = Some((k, mouse.y, false));
                } else if is_mouse_button_pressed(MouseButton::Right) && self.queue_drag.is_none() {
                    self.queue.remove(k);
                }
            }
            if let Some((from, y0, moved)) = self.queue_drag.as_mut() {
                if (mouse.y - *y0).abs() > 4.0 {
                    *moved = true;
                }
                let (from, moved) = (*from, *moved);
                // Near the list's edges the list scrolls to reach rows out of view.
                if moved {
                    let top = lay.list.y + 50.0;
                    let bottom = top + rows * 22.0;
                    let speed = get_frame_time() * 12.0;
                    if mouse.y < top + 8.0 {
                        self.queue_scroll = (self.queue_scroll - speed).max(0.0);
                    } else if mouse.y > bottom - 8.0 {
                        self.queue_scroll = (self.queue_scroll + speed).min((self.queue.len() as f32 - rows).max(0.0));
                    }
                }
                if is_mouse_button_released(MouseButton::Left) {
                    self.queue_drag = None;
                    self.queue_scroll = self.queue_scroll.round();
                    if moved {
                        let to = self.queue_drop_index(lay, mouse);
                        if let Some(item) = self.queue.remove(from) {
                            self.queue.insert(to.min(self.queue.len()), item);
                        }
                    } else if from < self.queue.len() {
                        // A plain click jumps the queue: play it now, keep the rest in order.
                        let item = self.queue.remove(from).unwrap();
                        self.queue.push_front(item);
                        self.play_queued();
                    }
                }
            }
        } else if let Some(i) = self.current {
            let len = self.albums[i].tracks.len() as f32;
            let rows = lay.list_rows() as f32;
            if wheel != 0.0 && lay.list.contains(mouse) {
                self.list_scroll = (self.list_scroll - wheel).clamp(0.0, (len - rows).max(0.0)).round();
            }
            if is_mouse_button_pressed(MouseButton::Left) {
                if let Some(n) = self.track_at(lay, mouse) {
                    self.play_track(n, true, 0.0);
                }
                // Clicking a mixtape's title names it.
                if let Some(id) = self.albums[i].mix {
                    if Rect::new(lay.list.x, lay.list.y, lay.list.w, 24.0).contains(mouse) {
                        self.renaming = Some(id);
                        self.rec = Some(id);
                    }
                }
            }
            // Right click records a track onto the mixtape, or takes it off one.
            if is_mouse_button_pressed(MouseButton::Right) {
                if let Some(n) = self.track_at(lay, mouse) {
                    match self.albums[i].mix {
                        Some(id) => self.unrecord(id, n),
                        None => {
                            let t = &self.albums[i].tracks[n];
                            let (path, title) = (t.path.clone(), t.title.clone());
                            self.record(path, &title);
                        }
                    }
                }
            }
        }
        // Middle click queues: a track from the side list or a J-card, or a
        // whole tape from the rail.
        if !on_lyrics && is_mouse_button_pressed(MouseButton::Middle) {
            if let (Some(i), Some(n)) = (self.current, self.track_at(lay, mouse)) {
                self.enqueue(i, n..n + 1);
            } else if let Some(i) = picked {
                let row = (self.flipped == Some(i) && self.tapes[i].flip > 0.5).then(|| self.jcard_row(lay, i, mouse)).flatten();
                match row {
                    Some((n, _)) => self.enqueue(i, n..n + 1),
                    None => self.enqueue(i, 0..self.albums[i].tracks.len()),
                }
            }
        }
        if is_mouse_button_pressed(MouseButton::Left) && queue_pill.contains(mouse) {
            if mouse.x > queue_pill.right() - 24.0 {
                self.queue.clear();
                self.show_queue = false;
            } else {
                self.show_queue = !self.show_queue;
                self.queue_scroll = 0.0;
            }
        }
        if is_mouse_button_pressed(MouseButton::Left) && self.rec_pill(lay).contains(mouse) {
            self.rec = None;
            self.renaming = None;
        }
        // Delete twice on a mixtape erases it.
        if is_key_pressed(KeyCode::Delete) {
            if let Some(id) = self.hover.and_then(|i| self.albums[i].mix).filter(|&id| id > 0) {
                match self.erase_armed {
                    Some((armed, at)) if armed == id && get_time() - at < 2.5 => {
                        self.erase_armed = None;
                        self.erase_mix(id);
                    }
                    _ => {
                        self.erase_armed = Some((id, get_time()));
                        self.toast = Some(("PRESS DELETE AGAIN TO ERASE THIS MIXTAPE".into(), get_time()));
                    }
                }
            }
        }

        // Deck controls.
        let knob_area = Rect::new(lay.knob.x - 32.0, lay.knob.y - 32.0, 64.0, 64.0);
        // The knob turns with the mouse: the volume follows the change in the
        // pointer's angle around its center, clockwise to raise it. Its 270°
        // sweep maps to the full range.
        let offset = mouse - lay.knob;
        let angle = offset.y.atan2(offset.x);
        if is_mouse_button_pressed(MouseButton::Left) && knob_area.contains(mouse) {
            self.knob_drag = Some(angle);
        }
        if let Some(last) = self.knob_drag {
            // Too close to the center the angle jumps around; wait for the
            // pointer to move out before reading it.
            if offset.length() > 6.0 {
                let delta = (angle - last + PI).rem_euclid(2.0 * PI) - PI;
                self.set_volume(self.volume + delta / (1.5 * PI));
                self.knob_drag = Some(angle);
            }
            if is_mouse_button_released(MouseButton::Left) {
                self.knob_drag = None;
                self.save();
            }
        }
        if wheel != 0.0 && knob_area.contains(mouse) {
            self.set_volume(self.volume + wheel * 0.05);
        }
        if is_mouse_button_pressed(MouseButton::Left) {
            let bar = lay.seek_bar();
            if lay.keys[0].contains(mouse) {
                self.press(0);
            } else if lay.keys[1].contains(mouse) {
                self.press(1);
            } else if lay.keys[2].contains(mouse) {
                self.press(2);
            } else if lay.keys[3].contains(mouse) {
                self.press(3);
            } else if lay.slot.contains(mouse) {
                if let Some(i) = self.current {
                    self.scroll_to(i as f32 - 1.0);
                }
            } else if bar.contains(mouse) && self.duration().is_some() {
                self.seek_drag = Some(0.0);
            }
        }
        if self.seek_drag.is_some() {
            let f = ((mouse.x - lay.progress.x) / lay.progress.w).clamp(0.0, 1.0);
            self.seek_drag = Some(f);
            if is_mouse_button_released(MouseButton::Left) {
                if let Some(d) = self.duration() {
                    let now = self.audio.position().as_secs_f32() / d.as_secs_f32().max(0.001);
                    self.reel_rush = if f >= now { 1.0 } else { -1.0 };
                    self.audio.seek(d.mul_f32(f));
                }
                self.seek_drag = None;
            }
        }

        // Keyboard.
        if is_key_pressed(KeyCode::F5) {
            self.start_rescan();
        }
        if is_key_pressed(KeyCode::F11) {
            self.fullscreen = !self.fullscreen;
            set_fullscreen(self.fullscreen);
            self.save();
        }
        if self.rename_keys() {
            return;
        }
        if !self.search_keys() {
            if is_key_pressed(KeyCode::Space) {
                self.press(1);
            }
            if is_key_pressed(KeyCode::Period) {
                self.press(2);
            }
            if is_key_pressed(KeyCode::Comma) {
                self.press(0);
            }
            if is_key_pressed(KeyCode::Tab) {
                self.set_view(if self.view == View::Rail { View::Grid } else { View::Rail });
            }
            // Held arrows keep stepping, like a keyboard's autorepeat. Every
            // key is polled each frame so a held one keeps repeating.
            let (right, left) = (self.repeat(KeyCode::Right), self.repeat(KeyCode::Left));
            let (down, up) = (self.repeat(KeyCode::Down), self.repeat(KeyCode::Up));
            let side = right as i32 as f32 - left as i32 as f32;
            let vertical = down as i32 - up as i32;
            if self.view == View::Grid {
                // The grid moves through albums both ways: up and down by a row.
                let step = side + vertical as f32 * self.grid_metrics.0 as f32;
                if step != 0.0 {
                    self.key_focus = true;
                    self.scroll_to(self.target.round() + step);
                }
            } else {
                // On the rail, the sides change tape...
                if side != 0.0 {
                    self.key_focus = true;
                    self.flipped = None;
                    self.scroll_to(self.target.round() + side);
                }
                // ...and up and down change track: on a turned tape's card,
                // or else on the tape in the deck.
                if vertical != 0 {
                    self.step_track(vertical);
                }
            }
            // Volume and seeking by keyboard, repeating while held.
            let louder = self.repeat(KeyCode::Equal) | self.repeat(KeyCode::KpAdd);
            let quieter = self.repeat(KeyCode::Minus) | self.repeat(KeyCode::KpSubtract);
            if louder || quieter {
                self.set_volume(self.volume + if louder { 0.05 } else { -0.05 });
                self.dirty_state = true;
            }
            let (back, ahead) = (self.repeat(KeyCode::LeftBracket), self.repeat(KeyCode::RightBracket));
            if back || ahead {
                let step = self.settings.seek_step();
                self.seek_by(if ahead { step } else { -step });
            }
            // Home brings the playing tape into view.
            if is_key_pressed(KeyCode::Home)
                && let Some(i) = self.current
            {
                self.key_focus = false;
                self.flipped = None;
                self.scroll_to(i as f32 - 1.0);
            }
            if is_key_pressed(KeyCode::Escape) {
                self.show_help = false;
            }
            if is_key_pressed(KeyCode::Enter) {
                match (self.flipped, self.card_cursor) {
                    (Some(i), Some(n)) => self.load(i, n, true, 0.0),
                    _ => {
                        if let Some(i) = self.hover {
                            self.load(i, self.start_track(i), true, 0.0);
                        }
                    }
                }
            }
        }
        while let Some(c) = get_char_pressed() {
            if c == '?' && self.query.is_empty() {
                self.show_help = !self.show_help;
                continue;
            }
            // While the query is empty, space, comma and period stay deck shortcuts.
            let starts = c.is_alphanumeric() || !self.query.is_empty();
            if starts && !c.is_control() {
                self.query.push(c);
                self.search();
            }
        }
    }

    /// Answers media keys and other desktop requests, and keeps the desktop's
    /// view of the player (track, cover, playing or paused, position) current.
    fn sync_desktop(&mut self, dt: f32) {
        for event in self.desktop.events() {
            let playing = self.audio.is_playing();
            match event {
                MediaControlEvent::Play if !playing => self.toggle(),
                MediaControlEvent::Pause | MediaControlEvent::Stop if playing => self.toggle(),
                MediaControlEvent::Toggle => self.toggle(),
                MediaControlEvent::Next => self.press(2),
                MediaControlEvent::Previous => self.press(0),
                MediaControlEvent::Seek(dir) => {
                    let step = self.settings.seek_step();
                    self.seek_by(if dir == SeekDirection::Forward { step } else { -step })
                }
                MediaControlEvent::SeekBy(dir, by) => {
                    let by = by.as_secs_f32();
                    self.seek_by(if dir == SeekDirection::Forward { by } else { -by });
                }
                MediaControlEvent::SetPosition(to) => self.audio.seek(to.0),
                MediaControlEvent::SetVolume(v) => {
                    self.set_volume(v as f32);
                    self.dirty_state = true;
                }
                _ => {}
            }
        }

        let now = self.current.and_then(|i| {
            let (album, tape) = (&self.albums[i], &self.tapes[i]);
            let track = album.tracks.get(self.track)?;
            // Write the cover out once per tape, as soon as it has loaded.
            if album.mix.is_none()
                && self.desktop_art.as_ref().is_none_or(|(id, _)| *id != tape.id)
                && let Some(tex) = &tape.cover
                && let Some(dir) = dirs::cache_dir().map(|d| d.join("tapedeck"))
            {
                let turn = self.desktop_art.as_ref().is_some_and(|(_, p)| p.ends_with("now-playing-0.png"));
                let path = dir.join(format!("now-playing-{}.png", turn as u8));
                let _ = std::fs::create_dir_all(&dir);
                // Read back from the GPU bottom row first; turn it upright.
                let mut image = tex.get_texture_data();
                let row = image.width as usize * 4;
                let flipped: Vec<u8> = image.bytes.chunks(row).rev().flatten().copied().collect();
                image.bytes = flipped;
                image.export_png(&path.to_string_lossy());
                self.desktop_art = Some((tape.id, path));
            }
            let cover = self.desktop_art.as_ref().filter(|(id, _)| *id == tape.id).map(|(_, p)| p.clone());
            Some(NowPlaying {
                title: track.title.clone(),
                album: album.album.clone(),
                artist: album.artist.clone(),
                cover,
                duration: self.duration(),
            })
        });
        let loaded = self.current.is_some();
        self.desktop.show(now);
        // A jump in position (a seek, a new track) is published at once;
        // otherwise listeners extrapolate from the last report.
        let pos = self.audio.position().as_secs_f32();
        let jumped = (pos - self.last_position - dt).abs() > 1.0;
        self.last_position = pos;
        if loaded {
            self.desktop.playback(self.audio.is_playing(), Duration::from_secs_f32(pos), jumped);
        }
    }

    /// Seconds to wait between frames. Full speed while anything is being
    /// used or is moving; a running tape only needs its reels, meters and
    /// lyrics kept smooth; a still deck can idle.
    fn frame_budget(&self) -> f32 {
        let now = get_time();
        let recent = |at: f64, span: f64| now - at < span;
        let busy = recent(self.active_at, 1.5)
            || self.settling
            || recent(self.lyric_change.1, 0.5)
            || recent(self.volume_at, 1.5)
            || recent(self.backdrop.2, 1.3)
            || self.toast.as_ref().is_some_and(|(_, at)| recent(*at, 2.7))
            || self.seek_drag.is_some()
            || self.knob_drag.is_some()
            || self.rescan.is_some()
            || self.visible().any(|i| self.tapes[i].dirty)
            || (self.accent.r - self.accent_goal.r).abs() + (self.accent.g - self.accent_goal.g).abs() + (self.accent.b - self.accent_goal.b).abs() > 0.01;
        if busy {
            0.0
        } else if self.audio.is_playing() {
            self.settings.playing_frame()
        } else {
            self.settings.idle_frame()
        }
    }

    /// Seeks the playing track by `by` seconds, winding the reels that way.
    fn seek_by(&mut self, by: f32) {
        let Some(d) = self.duration() else { return };
        let to = (self.audio.position().as_secs_f32() + by).clamp(0.0, (d.as_secs_f32() - 0.5).max(0.0));
        self.audio.seek(Duration::from_secs_f32(to));
        self.reel_rush = by.signum();
    }

    /// Moves one track up (-1) or down (+1) without leaving the tape: the
    /// highlighted row of a turned tape's card, or the track in the deck.
    fn step_track(&mut self, by: i32) {
        if let Some(i) = self.flipped {
            let n = self.albums[i].tracks.len();
            if n == 0 {
                return;
            }
            let from = self.card_cursor.map_or(if by > 0 { -1 } else { n as i32 }, |c| c as i32);
            let row = (from + by).clamp(0, n as i32 - 1) as usize;
            self.card_cursor = Some(row);
            // Follow the cursor onto the card page that holds it.
            self.jcard_page = row / gfx::JCARD_PAGE_SIZE;
            return;
        }
        let Some(i) = self.current else { return };
        let n = self.albums[i].tracks.len() as i32;
        let track = self.track as i32 + by;
        if (0..n).contains(&track) {
            let key = if by > 0 { 2 } else { 0 };
            self.pressed[key] = get_time();
            self.sfx(Sfx::Key);
            self.reel_rush = by.signum() as f32;
            self.play_track(track as usize, true, 0.0);
        }
    }

    /// True when `key` was just pressed, and again at the key repeat interval
    /// once it has been held for the repeat delay (both from the settings).
    fn repeat(&mut self, key: KeyCode) -> bool {
        let now = get_time();
        let (delay, every) = self.settings.key_repeat();
        if is_key_pressed(key) {
            self.held = Some((key, now + delay));
            return true;
        }
        match self.held {
            Some((held, _)) if held == key && !is_key_down(key) => {
                self.held = None;
                false
            }
            Some((held, at)) if held == key && now >= at => {
                self.held = Some((key, now + every));
                true
            }
            _ => false,
        }
    }

    /// Search keys take over the arrows and Enter while a query is active.
    fn search_keys(&mut self) -> bool {
        if self.query.is_empty() {
            return false;
        }
        if is_key_pressed(KeyCode::Escape) {
            self.query.clear();
            self.search();
            return true;
        }
        // Every key is polled each frame so a held one keeps repeating.
        let erase = self.repeat(KeyCode::Backspace);
        let next = self.repeat(KeyCode::Right) | self.repeat(KeyCode::Down) | self.repeat(KeyCode::Tab);
        let back = self.repeat(KeyCode::Left) | self.repeat(KeyCode::Up);
        if erase {
            self.query.pop();
            self.search();
        } else if next {
            self.focus_match(1);
        } else if back {
            self.focus_match(-1);
        } else if is_key_pressed(KeyCode::Enter) {
            if let Some(i) = self.hover.or(self.matches.get(self.focus).map(|m| m.0)) {
                self.load(i, self.start_track(i), true, 0.0);
            }
        }
        true
    }

    /// Recomputes the tapes matching the query and brings the first one into view.
    fn search(&mut self) {
        self.refresh_matches();
        if let Some(&(i, _)) = self.matches.first() {
            self.scroll_to(i as f32 - 1.0);
        }
    }

    fn refresh_matches(&mut self) {
        self.matches.clear();
        self.focus = 0;
        for tape in &mut self.tapes {
            tape.matched = false;
        }
        let query = fold(&self.query);
        let tokens: Vec<_> = query.split_whitespace().map(|token| (token, compact_genre(token))).collect();
        if tokens.is_empty() {
            return;
        }
        for (i, a) in self.albums.iter().enumerate() {
            let base = fold(&format!("{} {} {}", a.artist, a.album, a.year));
            let genre = compact_genre(&fold(&a.genre));
            let missing: Vec<&str> = tokens
                .iter()
                .filter(|(token, genre_token)| {
                    !base.contains(*token) && (genre_token.is_empty() || !genre.contains(genre_token))
                })
                .map(|(token, _)| *token)
                .collect();
            if missing.is_empty() {
                self.tapes[i].matched = true;
                self.matches.push((i, None));
            } else if let Some(t) = a.tracks.iter().position(|t| {
                let title = fold(&t.title);
                missing.iter().all(|&tok| title.contains(tok))
            }) {
                self.tapes[i].matched = true;
                self.matches.push((i, Some(t)));
            }
        }
    }

    fn focus_match(&mut self, step: isize) {
        let n = self.matches.len() as isize;
        if n == 0 {
            return;
        }
        self.focus = (self.focus as isize + step).rem_euclid(n) as usize;
        self.scroll_to(self.matches[self.focus].0 as f32 - 1.0);
    }

    /// The track a tape should start on: the one the search found, if any.
    fn start_track(&self, i: usize) -> usize {
        self.matches.iter().find(|m| m.0 == i).and_then(|m| m.1).unwrap_or(0)
    }

    /// Sort switches in the top-right corner of the stage.
    fn sort_pills(&self, lay: &Layout) -> Vec<(Sort, Rect)> {
        let mut x = lay.stage.right() - SP_L;
        let mut out: Vec<(Sort, Rect)> = Sort::ALL
            .iter()
            .rev()
            .map(|&sort| {
                let w = self.fonts.width(sort.label(), 10.0, true) + 14.0;
                x -= w;
                let r = Rect::new(x, lay.stage.y + SP_M, w, 18.0);
                x -= 4.0;
                (sort, r)
            })
            .collect();
        out.reverse();
        out
    }

    /// Panel of the tracklist opened over the grid, and how many rows fit.
    fn grid_card_rect(&self, lay: &Layout, i: usize) -> (Rect, usize) {
        let s = lay.stage;
        let n = self.albums[i].tracks.len().max(1);
        let fit = ((s.h - 2.0 * 72.0 - GRID_CARD_HEAD - SP_M) / GRID_CARD_ROW).floor().max(1.0) as usize;
        let rows = n.min(fit);
        let w = (s.w - 2.0 * SP_L * 2.0).min(560.0);
        let h = GRID_CARD_HEAD + rows as f32 * GRID_CARD_ROW + SP_M;
        (Rect::new(s.center().x - w / 2.0, s.y + (s.h - h) / 2.0 - SP_M, w, h), rows)
    }

    /// The track row of the open grid tracklist under the mouse, with its rectangle.
    fn grid_card_row(&self, lay: &Layout, mouse: Vec2) -> Option<(usize, Rect)> {
        let i = self.grid_card?;
        let (card, rows) = self.grid_card_rect(lay, i);
        let first = self.grid_card_scroll as usize;
        (0..rows).map(|j| (first + j, Rect::new(card.x + SP_M, card.y + GRID_CARD_HEAD + j as f32 * GRID_CARD_ROW, card.w - 2.0 * SP_M, GRID_CARD_ROW - 2.0)))
            .filter(|(n, _)| *n < self.albums[i].tracks.len())
            .find(|(_, r)| r.contains(mouse))
    }

    /// Clicks, wheel and keys for the tracklist over the grid. True while it is
    /// open, so the grid underneath ignores the mouse.
    fn input_grid_card(&mut self, lay: &Layout, mouse: Vec2, wheel: f32) -> bool {
        let Some(i) = self.grid_card else { return false };
        let (card, rows) = self.grid_card_rect(lay, i);
        let row = self.grid_card_row(lay, mouse);
        if is_key_pressed(KeyCode::Escape) || is_mouse_button_pressed(MouseButton::Right) {
            self.grid_card = None;
        } else if is_mouse_button_pressed(MouseButton::Left) {
            if let Some((n, _)) = row {
                self.load(i, n, true, 0.0);
                self.grid_card = None;
            } else if !card.contains(mouse) && lay.stage.contains(mouse) {
                self.grid_card = None;
            }
        } else if is_mouse_button_pressed(MouseButton::Middle) {
            if let Some((n, _)) = row {
                self.enqueue(i, n..n + 1);
            }
        }
        if wheel != 0.0 && card.contains(mouse) {
            let last = self.albums[i].tracks.len().saturating_sub(rows) as f32;
            self.grid_card_scroll = (self.grid_card_scroll - wheel).clamp(0.0, last).round();
        }
        lay.stage.contains(mouse)
    }

    /// Switch for the deck noises, left of the tape counter.
    fn sfx_pill(&self, lay: &Layout) -> Rect {
        let s = lay.stage;
        let counter = self.fonts.width("000 / 000", 12.0, false);
        let w = self.fonts.width("SFX", 10.0, true) + 14.0;
        Rect::new(s.right() - SP_L - 36.0 - SP_S - counter - SP_M - w, s.bottom() - 33.0, w, 18.0)
    }

    /// View switches, left of the sort switches.
    fn view_pills(&self, lay: &Layout) -> Vec<(View, Rect)> {
        let sort_x = self.sort_pills(lay).first().map(|(_, r)| r.x).unwrap_or(lay.stage.right() - SP_L);
        let mut x = sort_x - self.fonts.width("SORT", 10.0, false) - SP_S - SP_L;
        let mut out: Vec<(View, Rect)> = View::ALL
            .iter()
            .rev()
            .map(|&view| {
                let w = self.fonts.width(view.label(), 10.0, true) + 14.0;
                x -= w;
                let r = Rect::new(x, lay.stage.y + SP_M, w, 18.0);
                x -= 4.0;
                (view, r)
            })
            .collect();
        out.reverse();
        out
    }

    /// The "recording onto" badge; empty when not recording.
    fn rec_pill(&self, lay: &Layout) -> Rect {
        let Some(name) = self.rec.and_then(|id| self.mixes.iter().find(|m| m.id == id)).map(|m| &m.name) else {
            return Rect::default();
        };
        let text = format!("REC  {}  x", name.to_uppercase());
        Rect::new(lay.side.x + SP_S, lay.side.y + SP_S, (self.fonts.width(&text, 10.0, true) + 26.0).min(lay.side.w - 2.0 * SP_S), 18.0)
    }

    fn press(&mut self, key: usize) {
        self.pressed[key] = get_time();
        self.sfx(Sfx::Key);
        match key {
            0 => {
                self.reel_rush = -1.0;
                self.prev()
            }
            1 => self.toggle(),
            2 => {
                self.reel_rush = 1.0;
                self.next()
            }
            _ => {
                if self.current.is_some() {
                    self.return_tape();
                }
            }
        }
    }

    fn sfx(&self, sfx: Sfx) {
        if self.sfx {
            self.audio.play_sfx(sfx);
        }
    }

    fn set_volume(&mut self, v: f32) {
        self.volume_at = get_time();
        self.volume = v.clamp(0.0, 1.0);
        self.desktop.volume(self.volume);
        self.audio.set_volume(self.volume);
    }

    /// How far through the current track, 0..1, or where it is being dragged to.
    fn progress(&self) -> f32 {
        if let Some(f) = self.seek_drag {
            return f;
        }
        match self.duration() {
            Some(d) if d > Duration::ZERO => (self.audio.position().as_secs_f32() / d.as_secs_f32()).clamp(0.0, 1.0),
            _ => 0.0,
        }
    }

    fn duration(&self) -> Option<Duration> {
        let i = self.current?;
        self.audio.duration.or(self.albums[i].tracks[self.track].duration)
    }

    /// Position a dragged queue row would land at.
    fn queue_drop_index(&self, lay: &Layout, mouse: Vec2) -> usize {
        let top = lay.list.y + 50.0;
        let rows = lay.list_rows().max(1) as f32;
        let slot = ((mouse.y - top) / 22.0).floor().clamp(0.0, rows - 1.0);
        ((slot + self.queue_scroll.floor()) as usize).min(self.queue.len().saturating_sub(1))
    }

    fn queue_row_at(&self, lay: &Layout, mouse: Vec2) -> Option<usize> {
        let top = lay.list.y + 50.0;
        if !self.show_queue || !lay.list.contains(mouse) || mouse.y < top {
            return None;
        }
        let row = ((mouse.y - top) / 22.0).floor() as usize;
        let k = row + self.queue_scroll as usize;
        (row < lay.list_rows() && k < self.queue.len()).then_some(k)
    }

    fn track_at(&self, lay: &Layout, mouse: Vec2) -> Option<usize> {
        if self.show_queue {
            return None;
        }
        let i = self.current?;
        let top = lay.list.y + 50.0;
        if !lay.list.contains(mouse) || mouse.y < top {
            return None;
        }
        let row = ((mouse.y - top) / 22.0).floor() as usize;
        let n = row + self.list_scroll as usize;
        (row < lay.list_rows() && n < self.albums[i].tracks.len()).then_some(n)
    }

    fn tick(&mut self, dt: f32) {
        self.grid_scroll += (self.grid_target - self.grid_scroll) * (1.0 - (-dt * 12.0).exp());
        if (self.grid_target - self.grid_scroll).abs() < 0.5 {
            self.grid_scroll = self.grid_target;
        }
        self.scroll += (self.target - self.scroll) * (1.0 - (-dt * 8.0).exp());
        if (self.target - self.scroll).abs() < 0.0005 {
            self.scroll = self.target;
        }
        let k = 1.0 - (-dt * 12.0).exp();
        let vis = self.visible();
        if self.flipped.is_some_and(|f| !vis.contains(&f)) {
            self.flipped = None;
        }
        let searching = !self.query.is_empty();
        let focused_idx = if searching { self.matches.get(self.focus).map(|m| m.0) } else { None };
        let mut moving = false;
        let motion = self.settings.motion();
        // A new tape waits until the one it replaces has come back onto the
        // stage and is past the spot where the new one will be shown.
        let current = self.current;
        let returning = self.tapes.iter().enumerate().any(|(j, t)| current != Some(j) && t.out > SWAP_CLEAR);
        for (i, tape) in self.tapes.iter_mut().enumerate() {
            let matched = searching && tape.matched;
            let goal = if Some(i) == self.hover || focused_idx == Some(i) {
                1.0
            } else if matched {
                0.72
            } else {
                0.0
            };
            let dim = if searching && !matched { 1.0 } else { 0.0 };
            if vis.contains(&i) || (tape.lift - goal).abs() > 0.0005 || (tape.dim - dim).abs() > 0.0005 {
                tape.lift += (goal - tape.lift) * k;
                tape.dim += (dim - tape.dim) * k;
            }
            let out = match current == Some(i) {
                true if returning => tape.out,
                true => 1.0,
                false => 0.0,
            };
            let flip = if self.flipped == Some(i) { 1.0 } else { 0.0 };
            moving |= (tape.lift - goal).abs() > 0.002 || (tape.dim - dim).abs() > 0.002 || tape.out != out || tape.flip != flip;
            // Linear in time, like the flip: tape_matrix shapes it into the
            // pull, the presentation and the flight to the deck.
            let step = dt / (LOAD_TIME * motion);
            tape.out = if out > tape.out { (tape.out + step).min(out) } else { (tape.out - step).max(out) };
            // Linear in time: tape_matrix shapes it into the rise and the turn.
            let step = dt / (FLIP_TIME * motion);
            tape.flip = if flip > tape.flip { (tape.flip + step).min(flip) } else { (tape.flip - step).max(flip) };
        }
        self.settling = moving
            || self.scroll != self.target
            || self.grid_scroll != self.grid_target
            || self.slot_in < 1.0
            || self.reel_rush != 0.0
            || (0.01..0.99).contains(&self.reel_speed);
        // The deck takes the tape once it has flown off the stage towards it.
        if self.current.is_some_and(|i| self.view == View::Grid || self.tapes[i].out > 0.85) {
            self.slot_in = (self.slot_in + dt * 2.2).min(1.0);
        }
        self.packs = gfx::pack_radii(gfx::DECK_WINDOW_H, self.progress());
        let motor = if self.audio.is_playing() { 1.0 } else { 0.0 };
        let rate = if motor > self.reel_speed { 3.0 } else { 5.0 };
        self.reel_speed += (motor - self.reel_speed) * (1.0 - (-dt * rate).exp());
        // A seek or skip winds the tape fast for a moment, forwards or back.
        self.reel_rush *= (-dt * 3.5).exp();
        if self.reel_rush.abs() < 0.01 {
            self.reel_rush = 0.0;
        }
        let speed = self.reel_speed + self.reel_rush * 9.0;
        if speed.abs() > 0.001 {
            // Constant tape speed: angular speed is inversely proportional to the pack.
            const TAPE_SPEED: f32 = 14.0;
            self.reels.0 += dt * TAPE_SPEED * speed / self.packs.0;
            self.reels.1 += dt * TAPE_SPEED * speed / self.packs.1;
        }
        let goal = match self.current {
            Some(i) if self.albums[i].mix.is_none() && self.settings.accents => accent_color(self.tapes[i].average),
            _ => INK,
        };
        let ease = 1.0 - (-dt * 4.0).exp();
        let mix = |a: f32, b: f32| a + (b - a) * ease;
        self.accent = Color::new(mix(self.accent.r, goal.r), mix(self.accent.g, goal.g), mix(self.accent.b, goal.b), 1.0);
        self.accent_goal = goal;
        if let Some(Lyrics::Synced(lines)) = &self.lyrics {
            let focus = self.lyric_focus(lines, self.audio.position().as_secs_f32() + 0.2);
            if focus != self.lyric_change.0 {
                self.lyric_change = (focus, get_time());
            }
        }
        let (left, right) = self.audio.levels();
        let level = ((left + right) * 2.0).min(1.0);
        self.level += (level - self.level) * (1.0 - (-dt * 20.0).exp());
        // VU ballistics: quick to rise, slower to fall, like a real needle.
        let needle = |amp: f32| {
            let db = 20.0 * (amp * 3.0).max(1e-4).log10();
            ((db + 20.0) / 23.0).clamp(0.0, 1.06)
        };
        for (v, goal) in [(&mut self.vu.0, needle(left)), (&mut self.vu.1, needle(right))] {
            let rate = if goal > *v { 14.0 } else { 3.5 };
            *v += (goal - *v) * (1.0 - (-dt * rate).exp());
        }
        if self.audio.finished() {
            self.next();
        }
        if let Some(scanned) = self.rescan.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.rescan = None;
            self.apply_rescan(scanned);
        }
        self.sync_desktop(dt);
        if get_time() - self.saved_at > 5.0 && (self.dirty_state || self.audio.is_playing()) {
            self.save();
        }
    }

    /* ---------- Drawing ---------- */

    fn draw_backdrop(&self) {
        let (sw, sh) = (screen_width(), screen_height());
        let fade = ((get_time() - self.backdrop.2) / 1.2).min(1.0) as f32;
        let dest = Rect::new(-sw * 0.1, -sh * 0.1, sw * 1.2, sh * 1.2);
        for (slot, alpha) in [(self.backdrop.1, 1.0 - fade), (self.backdrop.0, fade)] {
            if let Some(tex) = slot.and_then(|i| self.tapes[i].blur.as_ref()) {
                // A faint wash of the cover behind the panels.
                cover_fit(tex, dest, 0.5, Color::new(0.16, 0.16, 0.16, alpha));
            }
        }
    }

    fn draw_frame(&self, lay: &Layout) {
        round_rect_lines(lay.frame, R_FRAME, 1.5, LINE);
        round_rect(lay.stage, R_PANEL, BLACK);
    }

    fn draw_stage(&self, lay: &Layout) {
        let mut cam = lay.camera();
        cam.pass = self.canvas.as_ref().map(|c| c.render_pass.clone());
        set_camera(&cam);
        let rail = if self.view == View::Rail { self.visible() } else { 0..0 };
        // Draw calls are what the GPU driver spends its time on, and a batch
        // ends whenever the texture changes. So every tape is drawn in passes
        // that share a texture: plain plastic, the ridged edges, the printed
        // faces, and last the clear plastic and highlights, far to near.
        let mut tapes = Vec::new();
        for i in rail.rev() {
            let p = i as f32 - self.scroll;
            // The far end of the rail sinks into the dark over a long, eased
            // stretch, so the small labels there do not compete with the near ones.
            let fade = if p > VISIBLE.1 - FAR_FADE {
                let t = ((VISIBLE.1 - p) / FAR_FADE).clamp(0.0, 1.0);
                t * t * (3.0 - 2.0 * t)
            } else if p < -1.4 {
                (p - VISIBLE.0) / 1.2
            } else {
                1.0
            }
            .clamp(0.0, 1.0);
            tapes.push((i, fade));
        }
        for pass in [Pass::Solid, Pass::Edge, Pass::Face, Pass::Clear] {
            for &(i, fade) in &tapes {
                let t = &self.tapes[i];
                // Tapes resting on the rail reflect in the floor; one being turned
                // around or leaving for the deck has left it.
                if t.flip < 0.01 && t.out < 0.02 && self.settings.reflection() > 0.0 {
                    self.draw_tape(lay, i, fade, true, pass);
                }
                // A tape on its way to the deck comes out of the dark of the far
                // rail into full light, and is gone once it has left the stage.
                if t.out < 0.999 {
                    let lit = fade + (1.0 - fade) * (t.out * 4.0).min(1.0);
                    self.draw_tape(lay, i, lit, false, pass);
                }
            }
        }
        // Gaps are lines, a different draw mode: they get their own batch.
        for &(i, fade) in &tapes {
            let out = self.tapes[i].out;
            if out > 0.01 {
                self.draw_gap(lay, i, fade * out);
            }
        }
        set_camera(&self.ui_camera());

        // Fade the front of the rail into black so it never runs under the
        // index strip and the hint line.
        let s = lay.stage;
        let floor = lay.rail_floor();
        let fade_top = floor - 64.0;
        let shade = |x: f32, y: f32, a: u8| Vertex { position: vec3(x, y, 0.0), uv: Vec2::ZERO, color: [0, 0, 0, a], normal: Vec4::ZERO };
        let vertices = vec![shade(s.x, fade_top, 0), shade(s.right(), fade_top, 0), shade(s.right(), floor, 255), shade(s.x, floor, 255)];
        draw_mesh(&Mesh { vertices, indices: vec![0, 1, 2, 0, 2, 3], texture: None });
        draw_rectangle(s.x, floor, s.w, s.bottom() - floor, BLACK);
        self.draw_glow(lay);

        // Mask the 3D corners to the stage's rounded border.
        for (corner, start) in [
            (s.point(), PI),
            (vec2(s.right(), s.y), -FRAC_PI_2),
            (vec2(s.right(), s.bottom()), 0.0),
            (vec2(s.x, s.bottom()), FRAC_PI_2),
        ] {
            let center = corner + vec2(if corner.x > s.x { -R_PANEL } else { R_PANEL }, if corner.y > s.y { -R_PANEL } else { R_PANEL });
            corner_mask(corner, center, R_PANEL, start, FRAME_BG);
        }
        round_rect_lines(s, R_PANEL, 1.5, LINE);
        let c = s.center();
        draw_rectangle(c.x, s.y, 1.5, 10.0, LINE);
        draw_rectangle(c.x, s.bottom() - 10.0, 1.5, 10.0, LINE);
        draw_rectangle(s.x, c.y, 10.0, 1.5, LINE);
        draw_rectangle(s.right() - 10.0, c.y, 10.0, 1.5, LINE);

        let focused = if self.query.is_empty() { None } else { self.matches.get(self.focus).map(|m| m.0) };
        // The playing tape is already named in the side list; only name the one being browsed.
        if self.view == View::Grid {
            self.draw_grid(lay);
            if let Some(i) = self.grid_card {
                self.draw_grid_card(lay, i);
            }
        } else if let Some(i) = self.hover.or(focused).filter(|&i| self.current != Some(i)) {
            // Right-aligned under the switches; the lyrics hold the top left.
            let a = &self.albums[i];
            let max = s.w * 0.4;
            let right = s.right() - SP_L;
            let f = &self.fonts;
            let line = |text: &str, y: f32, size: f32, color: Color| {
                let text = f.fit(text, size, false, max);
                f.draw(&text, right - f.width(&text, size, false), s.y + y, size, color, false);
            };
            line(&a.album.to_uppercase(), 64.0, 14.0, INK);
            line(&a.artist.to_uppercase(), 81.0, 13.0, DIM);
            if let Some(t) = self.matches.iter().find(|m| m.0 == i).and_then(|m| m.1) {
                line(&format!("A{:02} {}", t + 1, a.tracks[t].title), 100.0, 12.0, INK);
            }
        }

        if self.flipped.is_none() && self.view == View::Rail {
            if self.lyrics.is_some() {
                self.draw_lyrics(lay);
            } else {
                self.draw_now_playing(lay);
            }
        }
        self.draw_sort_pills(lay);
        self.draw_ruler(lay);
        if let Some((text, at)) = &self.toast {
            let age = (get_time() - at) as f32;
            if age < 2.6 {
                let a = (2.6 - age).min(1.0);
                let w = self.fonts.width(text, 12.0, true) + 24.0;
                let r = Rect::new(s.center().x - w / 2.0, s.y + 52.0, w, 26.0);
                round_rect(r, R_BOX, Color::new(0.0, 0.0, 0.0, 0.85 * a));
                round_rect_lines(r, R_BOX, 1.5, Color { a, ..REC });
                self.fonts.draw(text, r.x + 12.0, r.y + 17.0, 12.0, Color { a, ..INK }, true);
            }
        }

        if self.show_help {
            self.draw_help(lay);
        }
        if self.query.is_empty() {
            let gridded = self.view == View::Grid;
            let hint: String = match self.hover {
                _ if self.grid_card.is_some() => "CLICK A TRACK TO PLAY IT.  MIDDLE-CLICK TO QUEUE IT.  RIGHT-CLICK TO CLOSE.".into(),
                Some(i) if gridded && self.current == Some(i) => "CLICK TO PAUSE OR PLAY.  RIGHT-CLICK FOR TRACKS.".into(),
                Some(i) if self.albums[i].mix == Some(0) => "CLICK TO START A NEW MIXTAPE.".into(),
                Some(i) if gridded && self.albums[i].mix.is_some() && self.albums[i].mix != Some(0) && self.current != Some(i) => {
                    "CLICK TO PLAY.  DELETE TWICE TO ERASE.".into()
                }
                Some(i) if self.albums[i].mix.is_some() && self.current != Some(i) => {
                    "CLICK TO PLAY.  DELETE TWICE TO ERASE.  RIGHT-CLICK TO FLIP.".into()
                }
                Some(i) if self.current == Some(i) => "CLICK THE GAP TO RETURN THE TAPE.".into(),
                Some(i) if self.flipped == Some(i) => "CLICK A TRACK TO PLAY IT.  RIGHT-CLICK TO TURN BACK.".into(),
                // The idle hint only lists what has not been tried yet.
                _ => [(LEARNED_LISTEN, "CLICK TO LISTEN."), (LEARNED_QUEUE, "MIDDLE-CLICK TO QUEUE."), (LEARNED_FLIP, "RIGHT-CLICK TO FLIP.")]
                    .iter()
                    .filter(|(bit, _)| self.learned & bit == 0)
                    .map(|&(bit, text)| if gridded && bit == LEARNED_FLIP { "RIGHT-CLICK FOR TRACKS." } else { text })
                    .collect::<Vec<_>>()
                    .join("  "),
            };
            // Leave room for the tape counter on the right.
            let hint = self.fonts.fit(&hint, 12.0, false, self.sfx_pill(lay).x - SP_M - s.x - SP_L);
            self.fonts.draw(&hint, s.x + SP_L, s.bottom() - 19.0, 12.0, INK, false);
        } else {
            // Search field, styled like the deck's display.
            let caret = if get_time().fract() < 0.5 { "_" } else { " " };
            let text = format!("{}{caret}", self.query);
            let status = match self.matches.len() {
                0 => "NO TAPES".to_string(),
                n => format!("{}/{} TAPES  < >  ESC", self.focus + 1, n),
            };
            let tw = self.fonts.width(&text, 14.0, false);
            let sw = self.fonts.width(&status, 11.0, false);
            let field = Rect::new(s.x + 16.0, s.bottom() - 44.0, (tw + sw + 100.0).max(260.0), 28.0);
            round_rect(field, R_BOX, Color::new(0.0, 0.0, 0.0, 0.85));
            round_rect_lines(field, R_BOX, 1.5, LINE);
            self.fonts.draw("SEARCH", field.x + 10.0, field.y + 18.0, 10.0, DIM, true);
            self.fonts.draw(&text, field.x + 62.0, field.y + 19.0, 14.0, INK, false);
            let color = if self.matches.is_empty() { Color::from_rgba(255, 120, 110, 255) } else { DIM };
            self.fonts.draw(&status, field.right() - 10.0 - sw, field.y + 18.0, 11.0, color, false);
        }

        let shown = match self.view {
            View::Rail => self.scroll.round() as i32 + 2,
            View::Grid => self.view_span().0 as i32 + 1,
        };
        let count = format!("{:03} / {:03}", shown.clamp(1, self.albums.len() as i32), self.albums.len());
        let cw = self.fonts.width(&count, 12.0, false);
        let icon = Rect::new(s.right() - SP_L - 36.0, s.bottom() - 36.0, 36.0, 24.0);
        self.fonts.draw(&count, icon.x - SP_S - cw, icon.y + 16.0, 12.0, DIM, false);
        round_rect_lines(icon, R_CHIP, 1.5, LINE);
        let body = Rect::new(icon.x + 5.0, icon.y + 4.0, 26.0, 16.0);
        round_rect_lines(body, 2.0, 1.2, INK);
        round_rect_lines(Rect::new(body.x + 5.0, body.y + 4.0, 16.0, 6.0), 3.0, 1.2, INK);

        let pill = self.sfx_pill(lay);
        if self.sfx {
            round_rect(pill, R_CHIP, INK);
        } else {
            round_rect_lines(pill, R_CHIP, 1.0, Color::new(1.0, 1.0, 1.0, 0.35));
        }
        self.fonts.draw("SFX", pill.x + 7.0, pill.y + 13.0, 10.0, if self.sfx { BLACK } else { DIM }, true);
    }

    /// The choices a settings field offers; empty for the volume slider and
    /// one label for an action.
    fn field_choices(&self, field: Field) -> Vec<&'static str> {
        use settings::*;
        match field {
            Field::Volume => vec![],
            Field::SeekStep => SEEK_STEPS.to_vec(),
            Field::Sfx | Field::Mpris | Field::Fullscreen | Field::Accents => OFF_ON.to_vec(),
            Field::View => View::ALL.iter().map(|v| v.label()).collect(),
            Field::Sort => Sort::ALL.iter().map(|s| s.label()).collect(),
            Field::Reflections | Field::Glass | Field::Glow => LEVELS.to_vec(),
            Field::Motion | Field::KeyRepeat => PACES.to_vec(),
            Field::LyricPace => LYRIC_PACES.to_vec(),
            Field::PlayingFps => PLAYING_FPS.to_vec(),
            Field::IdleFps => IDLE_FPS.to_vec(),
            Field::Rescan => vec!["RESCAN"],
            Field::Hints => vec!["SHOW AGAIN"],
        }
    }

    fn field_value(&self, field: Field) -> usize {
        let s = &self.settings;
        match field {
            Field::Volume | Field::Rescan | Field::Hints => 0,
            Field::SeekStep => s.seek_step,
            Field::Sfx => self.sfx as usize,
            Field::Mpris => s.mpris as usize,
            Field::View => View::ALL.iter().position(|&v| v == self.view).unwrap_or(0),
            Field::Sort => Sort::ALL.iter().position(|&v| v == self.sort).unwrap_or(0),
            Field::Fullscreen => self.fullscreen as usize,
            Field::Accents => s.accents as usize,
            Field::Reflections => s.reflections,
            Field::Glass => s.glass,
            Field::Glow => s.glow,
            Field::Motion => s.motion,
            Field::LyricPace => s.lyric_pace,
            Field::KeyRepeat => s.key_repeat,
            Field::PlayingFps => s.playing_fps,
            Field::IdleFps => s.idle_fps,
        }
    }

    /// Applies choice `v` of a settings field (or runs its action) and saves.
    fn set_field(&mut self, field: Field, v: usize) {
        match field {
            Field::Volume => {}
            Field::SeekStep => self.settings.seek_step = v,
            Field::Sfx => self.sfx = v == 1,
            Field::Mpris => {
                self.settings.mpris = v == 1;
                self.desktop = if v == 1 { Desktop::new() } else { Desktop::off() };
            }
            Field::View => self.set_view(View::ALL[v]),
            Field::Sort => self.set_sort(Sort::ALL[v]),
            Field::Fullscreen => {
                self.fullscreen = v == 1;
                set_fullscreen(self.fullscreen);
            }
            Field::Accents => self.settings.accents = v == 1,
            Field::Reflections => self.settings.reflections = v,
            Field::Glass => {
                self.settings.glass = v;
                // The J-card's back sheet is part of the tape; nothing to repaint.
            }
            Field::Glow => self.settings.glow = v,
            Field::Motion => self.settings.motion = v,
            Field::LyricPace => self.settings.lyric_pace = v,
            Field::KeyRepeat => self.settings.key_repeat = v,
            Field::PlayingFps => self.settings.playing_fps = v,
            Field::IdleFps => self.settings.idle_fps = v,
            Field::Rescan => self.start_rescan(),
            Field::Hints => {
                self.learned = 0;
                self.toast = Some(("CONTROL HINTS WILL SHOW AGAIN".into(), get_time()));
            }
        }
        self.save();
    }

    /// The settings panel's card, the height of its content, and for each
    /// field its row and the rectangles of its choices (or slider), at the
    /// current scroll.
    fn settings_layout(&self) -> (Rect, f32, Vec<(Field, Rect, Vec<Rect>)>) {
        let f = &self.fonts;
        let (sw, sh) = (screen_width(), screen_height());
        let w = 640.0_f32.min(sw - 2.0 * SP_L);
        let mut content = 64.0;
        let mut heading = "";
        for (h, _, _) in FIELDS {
            if h != heading {
                content += 34.0;
                heading = h;
            }
            content += SETTINGS_ROW;
        }
        content += SP_M;
        let h = content.min(sh - 2.0 * SP_L);
        let card = Rect::new((sw - w) / 2.0, (sh - h) / 2.0, w, h);
        let mut y = card.y + 64.0 - self.settings_scroll;
        let mut heading = "";
        let mut rows = Vec::new();
        for (h, field, _) in FIELDS {
            if h != heading {
                y += 34.0;
                heading = h;
            }
            let row = Rect::new(card.x + SP_M, y, card.w - 2.0 * SP_M, SETTINGS_ROW - 4.0);
            let right = row.right() - SP_S;
            let mut controls = Vec::new();
            if field == Field::Volume {
                controls.push(Rect::new(right - 200.0, row.center().y - 3.0, 200.0, 6.0));
            } else {
                let mut x = right;
                for label in self.field_choices(field).iter().rev() {
                    let w = f.width(label, 10.0, false) + 14.0;
                    x -= w;
                    controls.push(Rect::new(x, row.center().y - 9.0, w, 18.0));
                    x -= 4.0;
                }
                controls.reverse();
            }
            rows.push((field, row, controls));
            y += SETTINGS_ROW;
        }
        (card, content, rows)
    }

    fn open_settings(&mut self, open: bool) {
        self.show_settings = open;
        self.show_help = false;
        self.held = None;
    }

    /// Keys and mouse while the settings panel is open: it takes all input.
    fn input_settings(&mut self) {
        while get_char_pressed().is_some() {}
        let ctrl = is_key_down(KeyCode::LeftControl) || is_key_down(KeyCode::RightControl);
        if is_key_pressed(KeyCode::Escape) || (ctrl && is_key_pressed(KeyCode::Comma)) {
            self.open_settings(false);
            return;
        }
        let n = FIELDS.len();
        if self.repeat(KeyCode::Down) {
            self.settings_row = (self.settings_row + 1).min(n - 1);
        }
        if self.repeat(KeyCode::Up) {
            self.settings_row = self.settings_row.saturating_sub(1);
        }
        let field = FIELDS[self.settings_row].1;
        let count = self.field_choices(field).len();
        let (left, right) = (self.repeat(KeyCode::Left), self.repeat(KeyCode::Right));
        if field == Field::Volume {
            if left || right {
                self.set_volume(self.volume + if right { 0.05 } else { -0.05 });
                self.save();
            }
        } else if count > 1 && (left || right) {
            let v = self.field_value(field);
            let v = if right { (v + 1).min(count - 1) } else { v.saturating_sub(1) };
            if v != self.field_value(field) {
                self.set_field(field, v);
            }
        } else if count == 1 && is_key_pressed(KeyCode::Enter) {
            self.set_field(field, 0);
        }

        let (card, content, rows) = self.settings_layout();
        // Keep the selected row in view; the wheel scrolls too.
        let max = (content - card.h).max(0.0);
        let (_, wy) = mouse_wheel();
        if wy != 0.0 {
            self.settings_scroll -= wy.signum() * 40.0;
        }
        if let Some((_, row, _)) = rows.get(self.settings_row)
            && (left || right || is_key_down(KeyCode::Up) || is_key_down(KeyCode::Down))
        {
            if row.y < card.y + 64.0 {
                self.settings_scroll -= card.y + 64.0 - row.y;
            } else if row.bottom() > card.bottom() - SP_M {
                self.settings_scroll += row.bottom() - (card.bottom() - SP_M);
            }
        }
        self.settings_scroll = self.settings_scroll.clamp(0.0, max);

        let mouse: Vec2 = mouse_position().into();
        if is_mouse_button_pressed(MouseButton::Left) {
            if !card.contains(mouse) {
                self.open_settings(false);
                return;
            }
            for (k, (field, row, controls)) in rows.iter().enumerate() {
                if !row.contains(mouse) || mouse.y < card.y + 56.0 || mouse.y > card.bottom() {
                    continue;
                }
                self.settings_row = k;
                if *field == Field::Volume {
                    self.settings_drag = true;
                } else if let Some(v) = controls.iter().position(|r| r.contains(mouse)) {
                    self.set_field(*field, v);
                }
            }
        }
        if self.settings_drag {
            if let Some((_, _, controls)) = rows.iter().find(|(f, _, _)| *f == Field::Volume) {
                let bar = controls[0];
                self.set_volume((mouse.x - bar.x) / bar.w);
            }
            if is_mouse_button_released(MouseButton::Left) {
                self.settings_drag = false;
                self.save();
            }
        }
    }

    /// The settings panel over the whole window.
    fn draw_settings(&self) {
        let f = &self.fonts;
        let (card, _, rows) = self.settings_layout();
        draw_rectangle(0.0, 0.0, screen_width(), screen_height(), Color::new(0.0, 0.0, 0.0, 0.6));
        round_rect(card, R_PANEL, FRAME_BG);
        round_rect_lines(card, R_PANEL, 1.5, LINE);
        f.draw("SETTINGS", card.x + SP_L, card.y + 36.0, 16.0, INK, false);
        let keys = "↑ ↓ choose   ← → change   ESC close";
        f.draw(keys, card.right() - SP_L - f.width(keys, 10.0, false), card.y + 34.0, 10.0, DIM, false);
        draw_rectangle(card.x + SP_M, card.y + 50.0, card.w - 2.0 * SP_M, 1.0, Color::new(1.0, 1.0, 1.0, 0.15));

        // Rows scroll under the title.
        let body = Rect::new(card.x, card.y + 56.0, card.w, card.h - 60.0);
        let gl = unsafe { get_internal_gl() };
        gl.quad_gl.scissor(Some((body.x as i32, body.y as i32, body.w as i32, body.h as i32)));
        let mouse: Vec2 = mouse_position().into();
        let mut heading = "";
        for (k, ((h, field, label), (_, row, controls))) in FIELDS.iter().zip(&rows).enumerate() {
            if *h != heading {
                f.draw(h, row.x + SP_S, row.y - 12.0, 10.0, DIM, false);
                heading = h;
            }
            let selected = k == self.settings_row;
            if selected {
                round_rect(*row, R_CHIP, Color::new(1.0, 1.0, 1.0, 0.07));
            }
            let text = match field {
                Field::Rescan => format!("{label}  ·  {}", self.music_root.display()),
                _ => label.to_string(),
            };
            let room = controls.first().map_or(row.right(), |c| c.x) - row.x - 2.0 * SP_M;
            f.draw(&f.fit(&text, 12.0, false, room), row.x + SP_S, row.center().y + 4.0, 12.0, if selected { INK } else { DIM }, false);
            if *field == Field::Volume {
                let bar = controls[0];
                round_rect(bar, 3.0, Color::new(1.0, 1.0, 1.0, 0.15));
                round_rect(Rect::new(bar.x, bar.y, bar.w * self.volume, bar.h), 3.0, self.accent);
                draw_circle(bar.x + bar.w * self.volume, bar.center().y, 6.0, INK);
                let pct = format!("{}", (self.volume * 100.0).round() as u32);
                f.draw(&pct, bar.x - SP_S - f.width(&pct, 11.0, false), row.center().y + 4.0, 11.0, INK, false);
                continue;
            }
            let value = self.field_value(*field);
            let choices = self.field_choices(*field);
            let action = choices.len() == 1;
            for (v, (pill, label)) in controls.iter().zip(choices).enumerate() {
                let on = !action && v == value;
                let hot = pill.contains(mouse);
                if on {
                    round_rect(*pill, R_CHIP, INK);
                } else {
                    round_rect_lines(*pill, R_CHIP, 1.0, Color::new(1.0, 1.0, 1.0, if hot || action { 0.6 } else { 0.3 }));
                }
                f.draw(label, pill.x + 7.0, pill.y + 13.0, 10.0, if on { BLACK } else if action { INK } else { DIM }, false);
            }
        }
        gl.quad_gl.scissor(None);
    }

    /// Sheet of keyboard and mouse shortcuts over the stage. Regular weight
    /// only: new sizes of the bold face here garbled its glyph atlas.
    fn draw_help(&self, lay: &Layout) {
        const KEYS: [(&str, &str); 19] = [
            ("← →", "Previous / next tape"),
            ("↑ ↓", "Previous / next track (on a turned tape: pick a track)"),
            ("ENTER", "Play the tape or track in focus"),
            ("SPACE", "Play / pause"),
            (",  .", "Previous / next track"),
            ("[  ]", "Back / ahead 10 seconds"),
            ("-  =", "Volume down / up"),
            ("HOME", "Show the playing tape"),
            ("TAB", "Switch rail / grid"),
            ("TYPE", "Search; ← → step through matches, ESC clears"),
            ("F5", "Rescan the music folder"),
            ("CTRL+,", "Settings"),
            ("F11", "Fullscreen"),
            ("CLICK", "Play a tape or track"),
            ("MIDDLE-CLICK", "Queue (with SHIFT: play next)"),
            ("RIGHT-CLICK", "Turn a tape around / grid: its tracks"),
            ("WHEEL", "Move along the rail, scroll lists"),
            ("DELETE ×2", "Erase a mixtape"),
            ("?  /  ESC", "Show / hide this sheet"),
        ];
        let f = &self.fonts;
        let s = lay.stage;
        let row = 24.0;
        let w = 560.0_f32.min(s.w - 2.0 * SP_L);
        let h = 64.0 + KEYS.len() as f32 * row + SP_M;
        let card = Rect::new(s.center().x - w / 2.0, s.y + ((s.h - h) / 2.0).max(SP_M), w, h);
        draw_rectangle(s.x, s.y, s.w, s.h, Color::new(0.0, 0.0, 0.0, 0.6));
        round_rect(card, R_PANEL, FRAME_BG);
        round_rect_lines(card, R_PANEL, 1.5, LINE);
        f.draw("SHORTCUTS", card.x + SP_L, card.y + 36.0, 16.0, INK, false);
        for (k, (key, what)) in KEYS.iter().enumerate() {
            let y = card.y + 64.0 + k as f32 * row;
            let kw = f.width(key, 11.0, false) + 12.0;
            let chip = Rect::new(card.x + SP_L, y, kw, 18.0);
            round_rect_lines(chip, R_CHIP, 1.0, Color::new(1.0, 1.0, 1.0, 0.35));
            f.draw(key, chip.x + 6.0, chip.y + 13.0, 11.0, INK, false);
            f.draw(&f.fit(what, 12.0, false, card.right() - SP_L - card.x - 150.0), card.x + 150.0, y + 13.0, 12.0, DIM, false);
        }
    }

    /// The deck's light spilling into the stage from its lower left, in the
    /// playing cover's color: a radial falloff clipped to the stage, laid over
    /// the rail like a faint haze.
    fn draw_glow(&self, lay: &Layout) {
        let glow = self.settings.glow();
        if self.current.is_none() || glow == 0.0 {
            return;
        }
        let s = lay.stage;
        let c = vec2(s.x + s.w * 0.1, s.bottom() - s.h * 0.12);
        let radius = s.w.max(s.h) * 0.9;
        let lit = |a: f32| [(self.accent.r * 255.0) as u8, (self.accent.g * 255.0) as u8, (self.accent.b * 255.0) as u8, (a * 255.0) as u8];
        let v = |p: Vec2, color: [u8; 4]| Vertex { position: p.extend(0.0), uv: Vec2::ZERO, color, normal: Vec4::ZERO };
        // Rings of falling brightness approximate a smooth falloff.
        const STEPS: usize = 48;
        const RINGS: usize = 6;
        let mut vertices = vec![v(c, lit(glow))];
        let mut indices = Vec::new();
        for r in 1..=RINGS {
            let t = r as f32 / RINGS as f32;
            let alpha = glow * (1.0 - t).powi(2);
            for k in 0..STEPS {
                let a = k as f32 / STEPS as f32 * std::f32::consts::TAU;
                vertices.push(v(c + vec2(a.cos(), a.sin()) * radius * t, lit(alpha)));
            }
        }
        for k in 0..STEPS {
            let next = (k + 1) % STEPS;
            indices.extend([0, 1 + k as u16, 1 + next as u16]);
            for r in 1..RINGS {
                let (inner, outer) = (1 + (r - 1) * STEPS, 1 + r * STEPS);
                let (a, b, c2, d) = (inner + k, inner + next, outer + next, outer + k);
                indices.extend([a as u16, b as u16, c2 as u16, a as u16, c2 as u16, d as u16]);
            }
        }
        let gl = unsafe { get_internal_gl() };
        gl.quad_gl.scissor(Some((s.x as i32, s.y as i32, s.w as i32, s.h as i32)));
        draw_mesh(&Mesh { vertices, indices, texture: None });
        let gl = unsafe { get_internal_gl() };
        gl.quad_gl.scissor(None);
    }

    /// Draws a tape, or with `mirror` its reflection in the glossy floor its
    /// rail position stands on: flipped through that floor, dim, and fading
    /// out away from it.
    fn draw_tape(&self, lay: &Layout, i: usize, fade: f32, mirror: bool, pass: Pass) {
        let tape = &self.tapes[i];
        let h = TAPE / 2.0;
        let m = if mirror {
            let floor = self.rail_matrix(lay, i).w_axis.y - h.y;
            Mat4::from_translation(vec3(0.0, floor, 0.0))
                * Mat4::from_scale(vec3(1.0, -1.0, 1.0))
                * Mat4::from_translation(vec3(0.0, -floor, 0.0))
                * self.tape_matrix(lay, i)
        } else {
            self.tape_matrix(lay, i)
        };
        // Reflections are brightest where the tape meets the floor.
        let reflection = self.settings.reflection();
        let gloss = |p: Vec3| if mirror { reflection * (1.0 - (p.y + h.y) / (2.0 * h.y)).clamp(0.0, 1.0).powi(2) } else { 1.0 };
        // Fade by darkening into the black stage rather than with alpha, which
        // would leak into the window's own transparency. Tapes left out by the
        // search sink into the dark the same way.
        let a = 255;
        let shade = fade * (1.0 - 0.86 * tape.dim);
        let shade = shade + (1.0 - shade) * tape.flip;
        let tint = |c: u8, g: f32| (c as f32 * shade * g) as u8;
        let vert = |p: Vec3, uv: Vec2, rgb: [u8; 3]| {
            let g = gloss(p);
            Vertex { position: m.transform_point3(p), uv, color: [tint(rgb[0], g), tint(rgb[1], g), tint(rgb[2], g), a], normal: Vec4::ZERO }
        };
        // Shell walls: the face's rounded outline extruded through the tape's
        // depth, so the rim wraps the corners instead of leaving a notch.
        let outline = shell_outline();
        // A key light from the upper right: walls facing it, near that corner,
        // catch a metallic glint on their front edge.
        let glint = |p: Vec2, n: Vec2| {
            let facing = n.dot(SHEEN_DIR).max(0.0).powi(3);
            let corner = ((p.x / h.x + p.y / h.y) / 2.0).clamp(0.0, 1.0).powi(2);
            facing * corner
        };
        let lit = |p: Vec2, n: Vec2, front: bool| {
            // Top walls catch the light, bottom ones fall into shadow.
            let v = 16.0 + 34.0 * (n.y + 1.0) / 2.0;
            let v = if front { v + 190.0 * glint(p, n) } else { v * 0.4 };
            [v.min(255.0) as u8; 3]
        };
        let count = outline.len() as u16;
        let card = tape.flip > 0.3 && !mirror && self.jcard.as_ref().is_some_and(|(j, _, _)| *j == i);
        let white = [255, 255, 255];
        let face_z = h.z - RECESS;
        match pass {
            Pass::Solid => {
                let mut vertices = Vec::with_capacity(outline.len() * 2 + 1);
                let mut indices = Vec::with_capacity(outline.len() * 9);
                for &(p, n) in outline {
                    vertices.push(vert(p.extend(h.z), Vec2::ZERO, lit(p, n, true)));
                    vertices.push(vert(p.extend(-h.z), Vec2::ZERO, lit(p, n, false)));
                }
                for k in 0..count {
                    let (a0, b0) = (k * 2, k * 2 + 1);
                    let (a1, b1) = (((k + 1) % count) * 2, ((k + 1) % count) * 2 + 1);
                    indices.extend([a0, a1, b1, a0, b1, b0]);
                }
                // Back plate as a fan around the center; a turned tape shows its
                // card through a clear back instead.
                if !card {
                    let center = vertices.len() as u16;
                    vertices.push(vert(vec3(0.0, 0.0, -h.z), Vec2::ZERO, [14, 14, 14]));
                    for k in 0..count {
                        indices.extend([center, k * 2 + 1, ((k + 1) % count) * 2 + 1]);
                    }
                }
                draw_mesh(&Mesh { vertices, indices, texture: None });
                draw_recess(&m, h.z, face_z, 1.0, shade, &gloss, Recess::Walls);
                if card {
                    draw_recess(&m, -h.z, JCARD_Z, -1.0, shade, &|_| 1.0, Recess::Walls);
                }
            }
            Pass::Edge => {
                // Ridged top edge along the straight part of the shell.
                let (r, y, top) = (gfx::FACE_RADIUS / FACE_W * TAPE.x, h.y + 0.002, [255, 255, 255]);
                let edge = vec![
                    vert(vec3(-h.x + r, y, h.z), vec2(0.0, 0.0), top),
                    vert(vec3(h.x - r, y, h.z), vec2(1.0, 0.0), top),
                    vert(vec3(h.x - r, y, -h.z), vec2(1.0, 1.0), top),
                    vert(vec3(-h.x + r, y, -h.z), vec2(0.0, 1.0), top),
                ];
                draw_mesh(&Mesh { vertices: edge, indices: vec![0, 1, 2, 0, 2, 3], texture: Some(self.edge.clone()) });
            }
            Pass::Face => {
                // The printed front sits inside the shell, under a clear plastic
                // window that is flush with the rim.
                let front = vec![
                    vert(vec3(-h.x, h.y, face_z), vec2(0.0, 1.0), white),
                    vert(vec3(h.x, h.y, face_z), vec2(1.0, 1.0), white),
                    vert(vec3(h.x, -h.y, face_z), vec2(1.0, 0.0), white),
                    vert(vec3(-h.x, -h.y, face_z), vec2(0.0, 0.0), white),
                ];
                draw_mesh(&Mesh { vertices: front, indices: vec![0, 1, 2, 0, 2, 3], texture: tape.face.as_ref().map(|rt| rt.texture.clone()) });
                // J-card on the back, seen once the tape is turned around.
                if card && let Some((_, _, rt)) = &self.jcard {
                    let z = JCARD_Z;
                    let back = vec![
                        vert(vec3(h.x, h.y, z), vec2(0.0, 1.0), white),
                        vert(vec3(-h.x, h.y, z), vec2(1.0, 1.0), white),
                        vert(vec3(-h.x, -h.y, z), vec2(1.0, 0.0), white),
                        vert(vec3(h.x, -h.y, z), vec2(0.0, 0.0), white),
                    ];
                    draw_mesh(&Mesh { vertices: back, indices: vec![0, 1, 2, 0, 2, 3], texture: Some(rt.texture.clone()) });
                }
            }
            Pass::Clear => {
                // The clear front, then a bevel highlight: a thin bright band just
                // inside the rim, strongest at the top right corner and fading
                // along the edges.
                if !mirror {
                    draw_recess(&m, h.z, face_z, 1.0, shade, &gloss, Recess::Glass(self.settings.sheen()));
                    let z = h.z + 0.003;
                    let rim = |p: Vec3, a: f32| {
                        let v = (255.0 * shade) as u8;
                        Vertex { position: m.transform_point3(p), uv: Vec2::ZERO, color: [v, v, v, (a * 255.0) as u8], normal: Vec4::ZERO }
                    };
                    let mut vertices = Vec::with_capacity(outline.len() * 2);
                    for &(p, n) in outline {
                        let g = glint(p, n) * SHEEN;
                        vertices.push(rim(p.extend(z), g));
                        vertices.push(rim((p - n * 0.035).extend(z), g * 0.15));
                    }
                    let mut indices = Vec::with_capacity(outline.len() * 6);
                    for k in 0..count {
                        let next = (k + 1) % count;
                        let (a0, b0, a1, b1) = (k * 2, k * 2 + 1, next * 2, next * 2 + 1);
                        indices.extend([a0, a1, b1, a0, b1, b0]);
                    }
                    draw_mesh(&Mesh { vertices, indices, texture: None });
                }

                // Highlighted track row and the clear back over a turned tape's card.
                if card {
                    let z = JCARD_Z;
                    let mouse_row = (self.hover == Some(i)).then(|| self.jcard_row(lay, i, mouse_position().into())).flatten().map(|(_, r)| r);
                    let key_row = self.card_cursor.and_then(|c| {
                        let range = gfx::jcard_range(self.albums[i].tracks.len(), self.jcard_page);
                        range.contains(&c).then(|| gfx::jcard_rows(range.len())[c - range.start])
                    });
                    if self.flipped == Some(i)
                        && let Some(row) = mouse_row.or(key_row)
                    {
                        let corner = |x, y| Vertex {
                            position: m.transform_point3(vec3(h.x - x / FACE_W * TAPE.x, h.y - y / FACE_H * TAPE.y, z - 0.002)),
                            uv: Vec2::ZERO,
                            color: [136, 49, 34, 34],
                            normal: Vec4::ZERO,
                        };
                        let vertices = vec![corner(row.x, row.y), corner(row.right(), row.y), corner(row.right(), row.bottom()), corner(row.x, row.bottom())];
                        draw_mesh(&Mesh { vertices, indices: vec![0, 1, 2, 0, 2, 3], texture: None });
                    }
                    draw_recess(&m, -h.z, z, -1.0, shade, &|_| 1.0, Recess::Glass(self.settings.sheen()));
                }
            }
        }
    }

    fn input_lyrics(&mut self, lay: &Layout, mouse: Vec2, wheel: f32) -> bool {
        if self.flipped.is_none() {
            match &self.lyrics {
                Some(Lyrics::Synced(lines)) => {
                    if let Some(panel) = lay.synced_lyrics_panel().filter(|r| r.contains(mouse)) {
                        let pos = self.audio.position().as_secs_f32() + 0.2;
                        let focus = self.lyric_focus(lines, pos);
                        if wheel != 0.0 {
                            let start = self.lyric_preview.unwrap_or(focus as f32);
                            self.lyric_preview = Some((start - wheel).clamp(0.0, (lines.len() - 1) as f32));
                        } else if is_mouse_button_pressed(MouseButton::Left)
                            && let Some((i, _)) = lyric_rows(panel, focus, lines.len()).find(|(_, row)| row.contains(mouse))
                            && let Ok(time) = Duration::try_from_secs_f32(lines[i].at)
                        {
                            self.audio.seek(time);
                            self.lyric_preview = None;
                            self.save();
                        }
                        return true;
                    }
                }
                Some(Lyrics::Plain(lines)) => {
                    let panel = lay.plain_lyrics_panel();
                    if panel.h >= 60.0 && panel.w >= 120.0 && panel.contains(mouse) {
                        self.lyrics_scroll = (self.lyrics_scroll - wheel).clamp(0.0, (lines.len() - 1) as f32);
                        return true;
                    }
                }
                None => {}
            }
        }
        self.lyric_preview = None;
        false
    }

    fn lyric_focus(&self, lines: &[LyricLine], pos: f32) -> usize {
        self.lyric_preview.map(|line| line.round() as usize)
            .unwrap_or_else(|| lines.partition_point(|line| line.at <= pos).saturating_sub(1))
    }

    fn draw_lyrics(&self, lay: &Layout) {
        let lyrics = match &self.lyrics {
            Some(Lyrics::Synced(lines)) => lines,
            Some(Lyrics::Plain(lines)) => return self.draw_plain_lyrics(lay, lines),
            None => return,
        };
        let Some(panel) = lay.synced_lyrics_panel() else { return };
        let mouse = mouse_position().into();
        let hovered = panel.contains(mouse);
        let f = &self.fonts;
        let pos = self.audio.position().as_secs_f32() + 0.2;
        let focus = self.lyric_focus(lyrics, pos);
        let previewing = self.lyric_preview.is_some();
        let x = panel.x + 6.0;
        let max_w = panel.w - 12.0;
        // A new line slides up into place and fades in.
        let t = (((get_time() - self.lyric_change.1) / 0.35) as f32).clamp(0.0, 1.0);
        let settle = 1.0 - (1.0 - t).powi(3);
        let dy = (1.0 - settle) * 18.0;
        let fade = |c: Color| Color { a: c.a * (0.35 + 0.65 * settle), ..c };

        if hovered {
            round_rect(panel, R_BOX, Color::new(0.035, 0.035, 0.035, 0.97));
            let hint = if previewing {
                format!("{} · CLICK TO SEEK", fmt_time(lyrics[focus].at))
            } else {
                "SCROLL · CLICK TO SEEK".into()
            };
            f.draw(&f.fit(&hint, 12.0, false, max_w), x, panel.bottom() - 5.0, 12.0, DIM, false);
        }

        for (i, row) in lyric_rows(panel, focus, lyrics.len()) {
            let line = &lyrics[i];
            let text = if line.words.is_empty() { ". . ." } else { &line.words };
            let row_hovered = row.contains(mouse);
            if row_hovered {
                round_rect(row, R_CHIP, Color::new(1.0, 1.0, 1.0, 0.1));
            }
            if focus != i {
                let color = if row_hovered { INK } else { DIM };
                f.draw(&f.fit(text, 15.0, false, max_w), x, row.center().y + 5.0 + dy, 15.0, color, false);
                continue;
            }

            let size = (row.h - 8.0).min(32.0);
            let max_lines = if row.h >= 80.0 { 2 } else { 1 };
            let lines = f.wrap(text, size, true, max_w, max_lines);
            let widths: Vec<f32> = lines.iter().map(|l| f.width(l, size, true)).collect();
            let next_at = lyrics.get(i + 1).map(|l| l.at).unwrap_or(line.at + 4.0);
            // LRC stamps only mark where a line starts. The voice usually ends
            // before the next stamp (a breath, a held note, an instrumental gap),
            // so the fill finishes early: within 90% of the gap, and no slower
            // than a typical singing pace for the line's length.
            let gap = (next_at - line.at).max(0.1);
            let (share, per_char) = self.settings.lyric_pace();
            let sung = (0.5 + text.chars().count() as f32 * per_char).min(gap * share).max(0.3);
            let progress = ((pos - line.at) / sung).clamp(0.0, 1.0);
            let mut fill = progress * widths.iter().sum::<f32>();
            let line_h = size * 1.25;
            let first_base = row.center().y + size * 0.32 - (lines.len() - 1) as f32 * line_h / 2.0 + dy;
            for (k, (text, width)) in lines.iter().zip(widths).enumerate() {
                let base = first_base + k as f32 * line_h;
                let color = if previewing { INK } else { DIM };
                f.draw(text, x, base, size, fade(color), true);
                let lit = fill.clamp(0.0, width);
                if !previewing && lit > 0.0 {
                    let gl = unsafe { get_internal_gl() };
                    gl.quad_gl.scissor(Some((x as i32, (base - size) as i32, lit.ceil() as i32, (size * 1.4) as i32)));
                    f.draw(text, x, base, size, fade(self.accent), true);
                    let gl = unsafe { get_internal_gl() };
                    gl.quad_gl.scissor(None);
                }
                fill -= width;
            }
        }
    }

    /// Without lyrics, the lyric space names what is playing instead.
    fn draw_now_playing(&self, lay: &Layout) {
        let Some(i) = self.current else { return };
        let (a, f) = (&self.albums[i], &self.fonts);
        let Some(track) = a.tracks.get(self.track) else { return };
        let panel = lay.plain_lyrics_panel();
        if panel.w < 120.0 {
            return;
        }
        let x = panel.x + 6.0;
        f.draw(&format!("NOW PLAYING  ·  A{:02} / {:02}", self.track + 1, a.tracks.len()), x, panel.y + 17.0, 11.0, DIM, false);
        let lines = f.wrap(&track.title, 32.0, true, panel.w - 12.0, 2);
        for (k, line) in lines.iter().enumerate() {
            f.draw(line, x, panel.y + 59.0 + k as f32 * 40.0, 32.0, INK, true);
        }
        let by = [a.artist.as_str(), a.album.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("  ·  ");
        let y = panel.y + 59.0 + lines.len() as f32 * 40.0 - 8.0;
        f.draw(&f.fit(&by.to_uppercase(), 13.0, false, panel.w - 12.0), x, y, 13.0, self.accent, false);
    }

    fn draw_plain_lyrics(&self, lay: &Layout, lines: &[String]) {
        let panel = lay.plain_lyrics_panel();
        if panel.w < 120.0 || panel.h < 60.0 {
            return;
        }
        round_rect(panel, R_BOX, Color::new(0.035, 0.035, 0.035, 0.97));
        let x = panel.x + 12.0;
        let width = panel.w - 24.0;
        let f = &self.fonts;
        let heading = f.fit("LYRICS · SCROLL TO READ", 12.0, false, width);
        f.draw(&heading, x, panel.y + 20.0, 12.0, DIM, false);
        let mut y = panel.y + 43.0;
        for line in lines.iter().skip(self.lyrics_scroll.round() as usize) {
            for text in f.wrap(line, 15.0, false, width, usize::MAX) {
                if y > panel.bottom() - 10.0 {
                    return;
                }
                f.draw(&text, x, y, 15.0, INK, false);
                y += 19.0;
            }
        }
    }

    fn draw_sort_pills(&self, lay: &Layout) {
        let views = self.view_pills(lay);
        if let Some((_, first)) = views.first() {
            let w = self.fonts.width("VIEW", 10.0, false);
            self.fonts.draw("VIEW", first.x - w - 8.0, first.y + 13.0, 10.0, DIM, false);
        }
        for (view, r) in views {
            let on = view == self.view;
            if on {
                round_rect(r, R_CHIP, INK);
            } else {
                round_rect_lines(r, R_CHIP, 1.0, Color::new(1.0, 1.0, 1.0, 0.35));
            }
            self.fonts.draw(view.label(), r.x + 7.0, r.y + 13.0, 10.0, if on { BLACK } else { DIM }, true);
        }
        let pills = self.sort_pills(lay);
        if let Some((_, first)) = pills.first() {
            let w = self.fonts.width("SORT", 10.0, false);
            self.fonts.draw("SORT", first.x - w - 8.0, first.y + 13.0, 10.0, DIM, false);
        }
        for (sort, r) in pills {
            let on = sort == self.sort;
            if on {
                round_rect(r, R_CHIP, INK);
            } else {
                round_rect_lines(r, R_CHIP, 1.0, Color::new(1.0, 1.0, 1.0, 0.35));
            }
            self.fonts.draw(sort.label(), r.x + 7.0, r.y + 13.0, 10.0, if on { BLACK } else { DIM }, true);
        }
    }

    /// Index strip: one tick per tape, colored by its cover, with labels for
    /// the current sort, the visible stretch, the playing tape and search hits.
    fn draw_ruler(&self, lay: &Layout) {
        let r = lay.ruler;
        let n = self.albums.len();
        let x_of = |i: f32| r.x + (i + 0.5) / n as f32 * r.w;
        let base = r.bottom() - 4.0;
        let (a, b) = self.view_span();
        let (a, b) = (a.max(0.0), b.min(n as f32 - 1.0));
        let window = Rect::new(x_of(a), base - 15.0, (x_of(b) - x_of(a)).max(4.0), 15.0);
        draw_rectangle(window.x, window.y, window.w, window.h, Color::new(1.0, 1.0, 1.0, 0.07));
        draw_rectangle(window.x, window.y, window.w, 1.0, Color::new(1.0, 1.0, 1.0, 0.35));
        draw_rectangle(r.x, base, r.w, 1.0, Color::new(1.0, 1.0, 1.0, 0.3));

        let searching = !self.query.is_empty();
        let tick_w = (r.w / n as f32 * 0.7).clamp(1.0, 3.0);
        let mouse: Vec2 = mouse_position().into();
        let pointing = r.contains(mouse) || self.ruler_drag;
        let pointed = pointing.then(|| (((mouse.x - r.x) / r.w * n as f32).floor().max(0.0) as usize).min(n - 1));
        let mut last_label = f32::MIN;
        let mut last_artist_char: Option<char> = None;
        let mut last_key: &str = "";
        let mut char_buf = [0u8; 4];
        for i in 0..n {
            let (album, tape) = (&self.albums[i], &self.tapes[i]);
            let x = x_of(i as f32);
            let cover = match (album.mix, tape.average) {
                (Some(_), _) => Color::from_rgba(214, 211, 202, 255),
                (None, Some([r, g, b])) => Color::from_rgba(r, g, b, 255),
                (None, None) => Color::from_rgba(90, 90, 90, 255),
            };
            let (h, color) = if searching {
                if tape.matched { (12.0, INK) } else { (4.0, Color::from_rgba(60, 60, 60, 255)) }
            } else {
                (7.0, cover)
            };
            // Ticks swell around the pointer like a magnifier.
            let (h, color) = match pointed {
                Some(p) if p == i => (h * 2.0, INK),
                Some(_) => (h * (1.0 + 0.8 * (-((x - mouse.x) / 30.0).powi(2)).exp()), color),
                None => (h, color),
            };
            draw_rectangle(x - tick_w / 2.0, base - h, tick_w, h, color);

            let key: &str = match (album.mix, self.sort) {
                (Some(_), _) => {
                    last_artist_char = None;
                    "MIX"
                }
                (None, Sort::Artist) => {
                    let ch = album
                        .artist
                        .chars()
                        .next()
                        .filter(|c| c.is_alphanumeric())
                        .and_then(|c| c.to_uppercase().next())
                        .unwrap_or('#');
                    if last_artist_char == Some(ch) {
                        continue;
                    }
                    last_artist_char = Some(ch);
                    last_key = "";
                    let s = ch.encode_utf8(&mut char_buf);
                    let w = self.fonts.width(s, 10.0, false);
                    if x - last_label >= 6.0 {
                        self.fonts.draw(s, x - 1.0, r.y + 8.0, 10.0, DIM, false);
                        last_label = x + w;
                    }
                    continue;
                }
                (None, Sort::Year) => &album.year,
                (None, Sort::Played) => match self.plays.get(&album.rel).copied().unwrap_or(0) {
                    0 => "NEVER",
                    _ if i == 0 || self.albums[i - 1].mix.is_some() => "MOST",
                    _ => last_key,
                },
                (None, Sort::Recent) => recency(self.played_at.get(&album.rel).copied()),
                (None, Sort::Color) => "",
            };
            if key != last_key && !key.is_empty() {
                let w = self.fonts.width(key, 10.0, false);
                if x - last_label >= 6.0 {
                    self.fonts.draw(key, x - 1.0, r.y + 8.0, 10.0, DIM, false);
                    last_label = x + w;
                }
            }
            last_key = key;
        }
        if let Some(i) = self.current {
            let x = x_of(i as f32);
            draw_triangle(vec2(x, base + 2.0), vec2(x - 4.5, base + 8.0), vec2(x + 4.5, base + 8.0), self.accent);
        }

        // Tooltip naming the tape under the mouse, with its cover when loaded.
        if let Some(i) = pointed {
            let a = &self.albums[i];
            let text = format!("{}  -  {}", a.artist.to_uppercase(), a.album.to_uppercase());
            let text = self.fonts.fit(&text, 11.0, false, 420.0);
            let w = self.fonts.width(&text, 11.0, false) + 14.0;
            let x = (mouse.x - w / 2.0).clamp(r.x, r.right() - w);
            let tip = Rect::new(x, r.y - 26.0, w, 20.0);
            round_rect(tip, R_CHIP, Color::new(0.0, 0.0, 0.0, 0.9));
            round_rect_lines(tip, R_CHIP, 1.0, LINE);
            self.fonts.draw(&text, tip.x + 7.0, tip.y + 14.0, 11.0, INK, false);
            if let Some(tex) = &self.tapes[i].cover {
                const SIZE: f32 = 84.0;
                let x = (mouse.x - SIZE / 2.0).clamp(r.x, r.right() - SIZE);
                let thumb = Rect::new(x, tip.y - SIZE - 6.0, SIZE, SIZE);
                draw_rectangle(thumb.x - 2.0, thumb.y - 2.0, thumb.w + 4.0, thumb.h + 4.0, BLACK);
                cover_fit(tex, thumb, 0.5, WHITE);
                draw_rectangle_lines(thumb.x - 2.0, thumb.y - 2.0, thumb.w + 4.0, thumb.h + 4.0, 1.0, LINE);
            }
        }
    }

    /// The library as a grid of covers with captions, scrolled vertically.
    fn draw_grid(&self, lay: &Layout) {
        let g = self.grid(lay);
        let f = &self.fonts;
        let a = g.area;
        let searching = !self.query.is_empty();
        let focused = if searching { self.matches.get(self.focus).map(|m| m.0) } else { None };
        let clip = |on: bool| {
            let gl = unsafe { get_internal_gl() };
            gl.quad_gl.scissor(on.then(|| (a.x as i32 - 4, a.y as i32, a.w as i32 + 8, a.h as i32)));
        };
        clip(true);
        for i in g.visible(self.albums.len(), self.grid_scroll) {
            let (album, tape) = (&self.albums[i], &self.tapes[i]);
            let hovered = self.hover == Some(i);
            let playing = self.current == Some(i);
            let mut c = g.cover(i, self.grid_scroll);
            if hovered {
                c.y -= 3.0;
            }
            let shade = if searching && !tape.matched { 0.22 } else { 1.0 };
            let tint = Color::new(shade, shade, shade, 1.0);

            // Cover: the album art, a collage for mixtapes, or the tape's plastic until it loads.
            let mix = if album.mix.is_some() { self.mix_covers(i) } else { Vec::new() };
            if let Some(tex) = tape.cover.as_ref().filter(|_| album.mix.is_none()) {
                cover_fit(tex, c, 0.5, tint);
            } else if !mix.is_empty() {
                let half = Rect::new(c.x, c.y, c.w / 2.0, c.h / 2.0);
                for k in 0..4 {
                    let r = Rect::new(half.x + (k % 2) as f32 * half.w, half.y + (k / 2) as f32 * half.h, half.w, half.h);
                    cover_fit(&mix[k % mix.len()], r, 0.5, tint);
                }
            } else {
                let base = Color { r: tape.shell.r * shade, g: tape.shell.g * shade, b: tape.shell.b * shade, a: 1.0 };
                draw_rectangle(c.x, c.y, c.w, c.h, base);
                let label = if album.mix == Some(0) { "+ NEW MIXTAPE".to_string() } else { album.album.to_uppercase() };
                let lines = f.wrap(&label, 13.0, true, c.w - 24.0, 3);
                for (k, line) in lines.iter().enumerate() {
                    f.draw(line, c.x + 12.0, c.y + 26.0 + k as f32 * 17.0, 13.0, Color { a: shade, ..INK }, true);
                }
            }

            if playing {
                draw_rectangle_lines(c.x - 3.0, c.y - 3.0, c.w + 6.0, c.h + 6.0, 2.0, self.accent);
                let badge = if self.audio.is_playing() { "PLAYING" } else { "PAUSED" };
                let bw = f.width(badge, 10.0, true) + 14.0;
                draw_rectangle(c.x, c.y, bw, 18.0, self.accent);
                f.draw(badge, c.x + 7.0, c.y + 13.0, 10.0, BLACK, true);
            } else if hovered || focused == Some(i) {
                draw_rectangle_lines(c.x - 2.0, c.y - 2.0, c.w + 4.0, c.h + 4.0, 1.5, INK);
            } else {
                draw_rectangle_lines(c.x, c.y, c.w, c.h, 1.0, Color::new(1.0, 1.0, 1.0, 0.12 * shade));
            }
            if hovered && !playing {
                // Play button in the middle of the hovered cover.
                let m = c.center();
                draw_circle(m.x, m.y, 22.0, Color::new(0.0, 0.0, 0.0, 0.7));
                ring(m.x, m.y, 22.0, 1.5, INK);
                draw_triangle(m + vec2(-6.0, -9.0), m + vec2(-6.0, 9.0), m + vec2(10.0, 0.0), INK);
            }

            let text = if searching && !tape.matched { Color { a: 0.3, ..INK } } else { INK };
            let dim = if searching && !tape.matched { Color { a: 0.2, ..DIM } } else { DIM };
            let title = if album.mix == Some(0) { "BLANK TAPE".to_string() } else { album.album.to_uppercase() };
            f.draw(&f.fit(&title, 12.0, true, c.w), c.x, c.bottom() + 18.0, 12.0, if playing { self.accent } else { text }, true);
            let by = [album.artist.as_str(), album.year.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
            f.draw(&f.fit(&by.to_uppercase(), 10.0, false, c.w), c.x, c.bottom() + 33.0, 10.0, dim, false);
        }
        clip(false);

        // Soft edges where rows scroll out of view.
        let edge = |y0: f32, y1: f32, a0: u8, a1: u8| {
            let v = |x: f32, y: f32, a: u8| Vertex { position: vec3(x, y, 0.0), uv: Vec2::ZERO, color: [0, 0, 0, a], normal: Vec4::ZERO };
            let (l, r) = (a.x - 6.0, a.right() + 6.0);
            draw_mesh(&Mesh { vertices: vec![v(l, y0, a0), v(r, y0, a0), v(r, y1, a1), v(l, y1, a1)], indices: vec![0, 1, 2, 0, 2, 3], texture: None });
        };
        if self.grid_scroll > 1.0 {
            edge(a.y, a.y + 24.0, 255, 0);
        }
        if self.grid_scroll < g.max_scroll(self.albums.len()) - 1.0 {
            edge(a.bottom() - 24.0, a.bottom(), 0, 255);
        }
        // Scroll bar along the right edge.
        let total = g.rows(self.albums.len()) as f32 * g.row_h;
        if total > a.h {
            let bar_h = (a.h * a.h / total).max(24.0);
            let bar_y = a.y + (a.h - bar_h) * self.grid_scroll / g.max_scroll(self.albums.len()).max(1.0);
            draw_rectangle(a.right() + 12.0, bar_y, 2.0, bar_h, Color::new(1.0, 1.0, 1.0, 0.25));
        }
    }

    /// An album's tracklist over the grid: cover, title, then one row per track.
    fn draw_grid_card(&self, lay: &Layout, i: usize) {
        let (a, tape, f) = (&self.albums[i], &self.tapes[i], &self.fonts);
        let (card, rows) = self.grid_card_rect(lay, i);
        let s = lay.stage;
        draw_rectangle(s.x, s.y + 48.0, s.w, lay.ruler.y - s.y - 56.0, Color::new(0.0, 0.0, 0.0, 0.8));
        round_rect(card, R_PANEL, FRAME_BG);
        round_rect_lines(card, R_PANEL, 1.5, LINE);

        let art = Rect::new(card.x + SP_M, card.y + SP_M, 72.0, 72.0);
        match &tape.cover {
            Some(tex) if a.mix.is_none() => cover_fit(tex, art, 0.5, WHITE),
            _ => draw_rectangle(art.x, art.y, art.w, art.h, tape.shell),
        }
        draw_rectangle_lines(art.x, art.y, art.w, art.h, 1.0, Color::new(1.0, 1.0, 1.0, 0.2));
        let tx = art.right() + SP_M;
        let tw = card.right() - SP_M - tx;
        f.draw(&f.fit(&a.album.to_uppercase(), 16.0, true, tw), tx, art.y + 18.0, 16.0, INK, true);
        let by = [a.artist.as_str(), a.year.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
        f.draw(&f.fit(&by.to_uppercase(), 11.0, false, tw), tx, art.y + 36.0, 11.0, DIM, false);
        let total: f32 = a.tracks.iter().filter_map(|t| t.duration).map(|d| d.as_secs_f32()).sum();
        let mut summary = format!("{} TRACKS  ·  {} MIN", a.tracks.len(), (total / 60.0).round() as u32);
        match self.plays.get(&a.rel).copied().unwrap_or(0) {
            0 => {}
            1 => summary.push_str("  ·  1 PLAY"),
            n => summary.push_str(&format!("  ·  {n} PLAYS")),
        }
        f.draw(&summary, tx, art.bottom() - 4.0, 10.0, DIM, false);
        if a.tracks.iter().any(|t| t.hires) {
            let w = f.width("HI-RES", 9.0, true) + 12.0;
            let tag = Rect::new(tx + f.width(&summary, 10.0, false) + SP_S, art.bottom() - 15.0, w, 14.0);
            round_rect_lines(tag, R_CHIP, 1.0, INK);
            f.draw("HI-RES", tag.x + 6.0, tag.y + 10.5, 9.0, INK, true);
        }

        let hovered = self.grid_card_row(lay, mouse_position().into()).map(|(n, _)| n);
        let first = self.grid_card_scroll as usize;
        for (j, (n, t)) in a.tracks.iter().enumerate().skip(first).take(rows).enumerate() {
            let row = Rect::new(card.x + SP_M, card.y + GRID_CARD_HEAD + j as f32 * GRID_CARD_ROW, card.w - 2.0 * SP_M, GRID_CARD_ROW - 2.0);
            let playing = self.current == Some(i) && self.track == n;
            if hovered == Some(n) {
                round_rect(row, R_CHIP, Color::new(1.0, 1.0, 1.0, 0.08));
            }
            let color = if playing { self.accent } else if hovered == Some(n) { INK } else { Color { a: 0.85, ..INK } };
            let y = row.y + 15.0;
            f.draw(&format!("A{:02}", n + 1), row.x + SP_S, y, 12.0, if playing { self.accent } else { DIM }, false);
            let time = t.duration.map(|d| fmt_time(d.as_secs_f32())).unwrap_or_default();
            let time_w = f.width(&time, 11.0, false);
            f.draw(&time, row.right() - SP_S - time_w, y, 11.0, DIM, false);
            f.draw(&f.fit(&t.title, 12.0, false, row.w - 72.0 - time_w), row.x + 48.0, y, 12.0, color, false);
        }
        if a.tracks.len() > rows {
            let track_h = rows as f32 * GRID_CARD_ROW;
            let bar_h = track_h * rows as f32 / a.tracks.len() as f32;
            let bar_y = card.y + GRID_CARD_HEAD + track_h * first as f32 / a.tracks.len() as f32;
            draw_rectangle(card.right() - 6.0, bar_y, 2.0, bar_h, Color::new(1.0, 1.0, 1.0, 0.25));
        }
    }

    /// The empty place a tape leaves on the rail while it plays in the deck.
    fn draw_gap(&self, lay: &Layout, i: usize, strength: f32) {
        let m = self.rail_matrix(lay, i);
        let h = TAPE / 2.0;
        let outline = shell_outline();
        let hovered = self.hover == Some(i);
        // Only the front outline, faint and tinted with the playing cover, so
        // the slot reads as a space in the row rather than a box on top of it.
        let v = strength * if hovered { 0.75 } else { 0.28 };
        let color = Color::new(self.accent.r * v, self.accent.g * v, self.accent.b * v, 1.0);
        for k in 0..outline.len() {
            let (a, b) = (outline[k].0, outline[(k + 1) % outline.len()].0);
            draw_line_3d(m.transform_point3(a.extend(h.z)), m.transform_point3(b.extend(h.z)), color);
        }
    }

    fn draw_side(&self, lay: &Layout) {
        let s = lay.side;
        let f = &self.fonts;

        let queue = self.queue_pill(lay);
        if queue.w > 0.0 {
            round_rect_lines(queue, R_CHIP, 1.0, LINE);
            if self.show_queue {
                round_rect(queue, R_CHIP, Color::new(1.0, 1.0, 1.0, 0.1));
            }
            let label = format!("UP NEXT  {}", self.up_next().unwrap_or_default().to_uppercase());
            f.draw(&f.fit(&label, 10.0, true, queue.w - 30.0), queue.x + 8.0, queue.y + 13.0, 10.0, INK, true);
            f.draw("x", queue.right() - 12.0, queue.y + 13.0, 10.0, INK, true);
        }

        let rec = self.rec_pill(lay);
        if rec.w > 0.0 {
            let name = self.rec.and_then(|id| self.mixes.iter().find(|m| m.id == id)).map(|m| m.name.to_uppercase()).unwrap_or_default();
            round_rect_lines(rec, R_CHIP, 1.0, REC);
            let pulse = 0.55 + 0.45 * (get_time() * 4.0).sin().abs() as f32;
            draw_circle(rec.x + 9.0, rec.center().y, 3.5, Color { a: pulse, ..REC });
            f.draw(&f.fit(&format!("REC  {name}"), 10.0, true, rec.w - 34.0), rec.x + 18.0, rec.y + 13.0, 10.0, REC, true);
            f.draw("x", rec.right() - 12.0, rec.y + 13.0, 10.0, REC, true);
        }

        // What's playing.
        let l = lay.list;
        if self.show_queue {
            self.draw_queue(lay);
            self.draw_deck(lay);
            return;
        }
        match self.current {
            None => {
                let cy = l.center().y;
                let t = "WHAT'S PLAYING ?";
                f.draw(t, s.center().x - f.width(t, 16.0, true) / 2.0, cy, 16.0, INK, true);
                let h = "CLICK TO LOAD THE TAPE";
                f.draw(h, s.center().x - f.width(h, 10.0, false) / 2.0, cy + 20.0, 10.0, DIM, false);
            }
            Some(i) => {
                let a = &self.albums[i];
                let naming = a.mix.is_some() && a.mix == self.renaming;
                let title = if naming {
                    format!("{}{}", a.album.to_uppercase(), if get_time().fract() < 0.5 { "_" } else { " " })
                } else {
                    a.album.to_uppercase()
                };
                f.draw(&f.fit(&title, 17.0, true, l.w - 12.0), l.x + 2.0, l.y + 16.0, 17.0, if naming { REC } else { INK }, true);
                let by = [a.artist.as_str(), a.year.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                f.draw(&f.fit(&by.to_uppercase(), 11.0, false, l.w - 12.0), l.x + 2.0, l.y + 34.0, 11.0, DIM, false);
                let rows = lay.list_rows();
                let first = self.list_scroll as usize;
                let hovered = self.track_at(lay, mouse_position().into());
                for (k, t) in a.tracks.iter().enumerate().skip(first).take(rows) {
                    let y = l.y + 50.0 + (k - first) as f32 * 22.0;
                    let row = Rect::new(l.x, y, l.w, 20.0);
                    let color = if k == self.track {
                        // The part still to play is greyed, so the row fills as the track plays.
                        round_rect(row, R_CHIP, Color::from_rgba(150, 150, 146, 255));
                        let played = row.w * self.progress();
                        if played > 0.5 {
                            round_rect(Rect::new(row.x, row.y, played.max(8.0), row.h), R_CHIP, self.accent);
                        }
                        BLACK
                    } else if hovered == Some(k) {
                        round_rect(row, R_CHIP, Color::new(1.0, 1.0, 1.0, 0.06));
                        INK
                    } else {
                        DIM
                    };
                    f.draw(&format!("A{:02}", k + 1), row.x + 6.0, y + 14.0, 12.0, color, false);
                    // Hovered rows show what a right click does.
                    let (right, right_color) = match (hovered == Some(k), a.mix) {
                        (true, Some(_)) => ("- REMOVE".to_string(), REC),
                        (true, None) => ("+ REC".to_string(), REC),
                        _ => (t.duration.map(|d| fmt_time(d.as_secs_f32())).unwrap_or_default(), color),
                    };
                    let right_color = if k == self.track && hovered == Some(k) { Color::from_rgba(170, 30, 25, 255) } else { right_color };
                    let rw = f.width(&right, 11.0, false);
                    f.draw(&right, row.right() - 6.0 - rw, y + 14.0, 11.0, right_color, false);
                    f.draw(&f.fit(&t.title, 12.0, false, row.w - 60.0 - rw), row.x + 44.0, y + 14.0, 12.0, color, false);
                }
                if a.tracks.len() > rows {
                    let track_h = l.h - 50.0;
                    let bar_h = track_h * rows as f32 / a.tracks.len() as f32;
                    let bar_y = l.y + 50.0 + track_h * self.list_scroll / a.tracks.len() as f32;
                    draw_rectangle(l.right() - 1.0, bar_y, 2.0, bar_h, Color::new(1.0, 1.0, 1.0, 0.25));
                }
                self.draw_sleeve(l, i, a.tracks.len().min(rows));
            }
        }

        self.draw_deck(lay);
    }

    /// The play queue in the side list: order, title, artist; hover shows
    /// what a right click does.
    fn draw_queue(&self, lay: &Layout) {
        let f = &self.fonts;
        let l = lay.list;
        f.draw("QUEUE", l.x + 2.0, l.y + 16.0, 17.0, INK, true);
        let total: f32 = self
            .queue
            .iter()
            .filter_map(|&(id, n)| self.albums[self.index_of(id)?].tracks.get(n)?.duration)
            .map(|d| d.as_secs_f32())
            .sum();
        let n = self.queue.len();
        let mut summary = format!("{n} TRACK{}", if n == 1 { "" } else { "S" });
        if total >= 60.0 {
            summary.push_str(&format!("  ·  {} MIN", (total / 60.0).round()));
        }
        summary.push_str("  ·  CLICK PLAYS  ·  DRAG MOVES");
        f.draw(&f.fit(&summary, 11.0, false, l.w - 12.0), l.x + 2.0, l.y + 34.0, 11.0, DIM, false);

        let rows = lay.list_rows();
        let first = self.queue_scroll.floor() as usize;
        let mouse: Vec2 = mouse_position().into();
        // While dragging, rows are shown in the order they would end up in,
        // with an empty slot where the dragged one would land.
        let dragging = self.queue_drag.filter(|d| d.2).map(|d| (d.0, self.queue_drop_index(lay, mouse)));
        let mut order: Vec<usize> = (0..n).collect();
        if let Some((from, to)) = dragging {
            let item = order.remove(from);
            order.insert(to.min(order.len()), item);
        }
        let hovered = if dragging.is_some() { None } else { self.queue_row_at(lay, mouse) };
        for (k, &q) in order.iter().enumerate().skip(first).take(rows) {
            let (id, t) = self.queue[q];
            let y = l.y + 50.0 + (k - first) as f32 * 22.0;
            let row = Rect::new(l.x, y, l.w, 20.0);
            if dragging.is_some_and(|(from, _)| from == q) {
                round_rect_lines(row, R_CHIP, 1.0, Color::new(1.0, 1.0, 1.0, 0.35));
                continue;
            }
            let Some(i) = self.index_of(id) else { continue };
            let Some(track) = self.albums[i].tracks.get(t) else { continue };
            let color = if k == 0 {
                round_rect(row, R_CHIP, Color::new(1.0, 1.0, 1.0, 0.1));
                INK
            } else if hovered == Some(k) {
                round_rect(row, R_CHIP, Color::new(1.0, 1.0, 1.0, 0.06));
                INK
            } else {
                DIM
            };
            f.draw(&format!("{:02}", k + 1), row.x + 6.0, y + 14.0, 12.0, color, false);
            let (right, right_color) = if hovered == Some(k) {
                ("- REMOVE".to_string(), REC)
            } else {
                (self.albums[i].artist.to_uppercase(), Color { a: color.a * 0.8, ..color })
            };
            let right = f.fit(&right, 10.0, false, 96.0);
            let rw = f.width(&right, 10.0, false);
            f.draw(&right, row.right() - 6.0 - rw, y + 14.0, 10.0, right_color, false);
            f.draw(&f.fit(&track.title, 12.0, false, row.w - 50.0 - rw), row.x + 32.0, y + 14.0, 12.0, color, false);
        }
        // The dragged row floats under the cursor.
        if let Some((from, _)) = dragging {
            let (id, t) = self.queue[from];
            if let Some(track) = self.index_of(id).and_then(|i| self.albums[i].tracks.get(t)) {
                let y = (mouse.y - 10.0).clamp(l.y + 46.0, l.y + 50.0 + (rows as f32 - 1.0) * 22.0 + 4.0);
                let row = Rect::new(l.x - 4.0, y, l.w + 8.0, 20.0);
                draw_rectangle(row.x + 3.0, row.y + 4.0, row.w, row.h, Color::new(0.0, 0.0, 0.0, 0.5));
                round_rect(row, R_CHIP, INK);
                f.draw(&f.fit(&track.title, 12.0, false, row.w - 20.0), row.x + 10.0, y + 14.0, 12.0, BLACK, false);
            }
        }
        if n > rows {
            let track_h = l.h - 50.0;
            let bar_h = track_h * rows as f32 / n as f32;
            let bar_y = l.y + 50.0 + track_h * self.queue_scroll / n as f32;
            draw_rectangle(l.right() - 1.0, bar_y, 2.0, bar_h, Color::new(1.0, 1.0, 1.0, 0.25));
        }
    }

    /// Fills the space a short tracklist leaves with the cover and a summary
    /// line (tracks, running time, genre).
    fn draw_sleeve(&self, l: Rect, i: usize, rows: usize) {
        let a = &self.albums[i];
        let chips = self.genre_chips(&a.genre, l.w - 8.0);
        // Below the cover: the summary line, then up to two rows of genre chips.
        let below = 26.0 + chips.len() as f32 * GENRE_ROW;
        let free = l.h - 50.0 - rows as f32 * 22.0 - (below - 26.0);
        if free < 150.0 {
            return;
        }
        let covers = match a.mix {
            Some(_) => self.mix_covers(i),
            None => self.tapes[i].cover.iter().cloned().collect(),
        };
        let size = (free - 52.0).min(l.w - 8.0).min(270.0);
        let art = Rect::new(l.x + (l.w - size) / 2.0, l.bottom() - size - below, size, size);
        draw_rectangle(art.x + 4.0, art.y + 6.0, art.w, art.h, Color::new(0.0, 0.0, 0.0, 0.5));
        match covers.as_slice() {
            [] => round_rect(art, R_CHIP, Color::from_rgba(236, 231, 216, 255)),
            [one] => cover_fit(one, art, 0.5, WHITE),
            many => {
                let half = size / 2.0;
                for (k, tex) in many.iter().cycle().take(4).enumerate() {
                    let (cx, cy) = ((k % 2) as f32, (k / 2) as f32);
                    cover_fit(tex, Rect::new(art.x + cx * half, art.y + cy * half, half, half), 0.5, WHITE);
                }
            }
        }
        round_rect_lines(art, 2.0, 1.0, Color::new(1.0, 1.0, 1.0, 0.25));

        let total: f32 = a.tracks.iter().filter_map(|t| t.duration).map(|d| d.as_secs_f32()).sum();
        let minutes = (total / 60.0).round() as u32;
        let mut parts = vec![format!("{} TRACK{}", a.tracks.len(), if a.tracks.len() == 1 { "" } else { "S" })];
        if minutes > 0 {
            parts.push(format!("{minutes} MIN"));
        }
        let text = self.fonts.fit(&parts.join("  ·  "), 10.0, false, l.w - 8.0);
        let w = self.fonts.width(&text, 10.0, false);
        let summary_y = art.bottom() + 20.0;
        self.fonts.draw(&text, l.center().x - w / 2.0, summary_y, 10.0, DIM, false);

        for (row, line) in chips.iter().enumerate() {
            let total: f32 = line.iter().map(|(_, w)| w).sum::<f32>() + (line.len() - 1) as f32 * GENRE_GAP;
            let mut x = l.center().x - total / 2.0;
            let y = summary_y + 10.0 + row as f32 * GENRE_ROW;
            for (name, w) in line {
                let chip = Rect::new(x, y, *w, GENRE_ROW - 4.0);
                round_rect_lines(chip, R_CHIP, 1.0, Color::new(1.0, 1.0, 1.0, 0.3));
                self.fonts.draw(name, chip.x + GENRE_PAD, chip.y + 12.0, 9.0, DIM, true);
                x += w + GENRE_GAP;
            }
        }
    }

    /// Genre tags split into chips and laid out centered in at most two rows;
    /// what does not fit becomes a "+N" chip. Each chip comes with its width.
    fn genre_chips(&self, genre: &str, width: f32) -> Vec<Vec<(String, f32)>> {
        let names: Vec<String> = genre
            .split([';', ',', '/'])
            .map(|g| g.trim().to_uppercase())
            .filter(|g| !g.is_empty())
            .collect();
        let chip_w = |name: &str| self.fonts.width(name, 9.0, true) + 2.0 * GENRE_PAD;
        let mut rows: Vec<Vec<(String, f32)>> = Vec::new();
        for (k, name) in names.iter().enumerate() {
            let w = chip_w(name).min(width);
            let fits = rows.last().is_some_and(|row| row.iter().map(|(_, w)| w + GENRE_GAP).sum::<f32>() + w <= width);
            if fits {
                rows.last_mut().unwrap().push((name.clone(), w));
            } else if rows.len() < 2 {
                rows.push(vec![(name.clone(), w)]);
            } else {
                // Out of room: swap the last chip for a count of the rest.
                let row = rows.last_mut().unwrap();
                let left = names.len() - k + 1;
                row.pop();
                let more = format!("+{left}");
                let w = chip_w(&more);
                row.push((more, w));
                break;
            }
        }
        for row in &mut rows {
            for (name, w) in row.iter_mut() {
                *name = self.fonts.fit(name, 9.0, true, *w - 2.0 * GENRE_PAD);
            }
        }
        rows
    }

    /// The tape seen through the cassette door: shell, paper label with the
    /// side and title, the cover behind the window, and the reels winding.
    fn draw_deck_cassette(&self, mini: Rect, i: usize) {
        let (tape, album, f) = (&self.tapes[i], &self.albums[i], &self.fonts);
        round_rect(mini, R_BOX, tape.shell);
        round_rect_lines(mini, R_BOX, 1.0, Color::new(1.0, 1.0, 1.0, 0.25));
        let label = Rect::new(mini.x + 6.0, mini.y + 5.0, mini.w - 12.0, mini.h - 16.0);
        if let Some(tex) = &tape.cover {
            cover_fit(tex, label, 0.4, WHITE);
        } else {
            draw_rectangle(label.x, label.y, label.w, label.h, Color::from_rgba(236, 231, 216, 255));
        }
        // Paper strip with the side and the album, as on the tapes' labels.
        let strip = Rect::new(label.x, label.y, label.w, 15.0);
        draw_rectangle(strip.x, strip.y, strip.w, strip.h, Color::from_rgba(236, 231, 216, 255));
        let ink = Color::from_rgba(28, 34, 70, 255);
        draw_rectangle_lines(strip.x + 3.0, strip.y + 2.0, 11.0, 11.0, 1.0, ink);
        f.draw("A", strip.x + 5.5, strip.y + 11.0, 9.0, ink, true);
        f.draw(&f.fit(&album.album.to_uppercase(), 9.0, true, strip.w - 24.0), strip.x + 19.0, strip.y + 11.0, 9.0, ink, true);
        let win = Rect::new(mini.x + mini.w * 0.2, label.y + strip.h + (label.h - strip.h - gfx::DECK_WINDOW_H) / 2.0, mini.w * 0.6, gfx::DECK_WINDOW_H);
        gfx::winding_window(win, self.progress(), self.reels);
        for (x, y) in [(mini.x + 4.0, mini.y + 4.0), (mini.right() - 4.0, mini.y + 4.0), (mini.x + 4.0, mini.bottom() - 4.0), (mini.right() - 4.0, mini.bottom() - 4.0)] {
            draw_circle(x, y, 1.8, Color::from_rgba(150, 152, 156, 255));
        }
        // The head opening along the bottom edge.
        let foot = Rect::new(mini.x + mini.w * 0.25, mini.bottom() - 9.0, mini.w * 0.5, 9.0);
        draw_rectangle(foot.x, foot.y, foot.w, foot.h, Color::from_rgba(8, 8, 9, 255));
    }

    /// Analog VU meter: cream dial, scale with a red zone, black needle.
    /// The dial of a VU meter (with `value: None`), or its needle.
    fn draw_vu(&self, r: Rect, value: Option<f32>, channel: &str) {
        let f = &self.fonts;
        let pivot = vec2(r.center().x, r.bottom() + r.h * 0.18);
        let radius = r.h * 0.92;
        let span = 0.95_f32; // radians either side of vertical
        let angle_of = |v: f32| -FRAC_PI_2 - span + v * 2.0 * span;
        let ink = Color::from_rgba(40, 36, 30, 255);
        if let Some(value) = value {
            let a = angle_of(value);
            let tip = pivot + vec2(a.cos(), a.sin()) * radius * 0.97;
            // The pivot sits below the dial; start the needle at the dial's edge.
            let t = ((r.bottom() - 1.0 - pivot.y) / (tip.y - pivot.y)).clamp(0.0, 1.0);
            let base = pivot + (tip - pivot) * t;
            draw_line(base.x, base.y, tip.x, tip.y, 1.4, ink);
            return;
        }
        round_rect(r, R_BOX, Color::from_rgba(232, 224, 200, 255));
        round_rect_lines(r, R_BOX, 1.0, Color::new(1.0, 1.0, 1.0, 0.5));
        let red = Color::from_rgba(196, 52, 40, 255);
        // Scale arc: black up to 0 VU, red beyond.
        gfx::arc(pivot.x, pivot.y, radius * 0.86, angle_of(0.0), angle_of(0.87) - angle_of(0.0), 1.2, ink);
        gfx::arc(pivot.x, pivot.y, radius * 0.86, angle_of(0.87), angle_of(1.0) - angle_of(0.87), 2.4, red);
        for k in 0..=10 {
            let v = k as f32 / 10.0;
            let a = angle_of(v);
            let dir = vec2(a.cos(), a.sin());
            let (inner, outer) = (radius * 0.86, radius * if k % 5 == 0 { 0.98 } else { 0.93 });
            let p0 = pivot + dir * inner;
            let p1 = pivot + dir * outer;
            draw_line(p0.x, p0.y, p1.x, p1.y, 1.0, if v > 0.87 { red } else { ink });
        }
        f.draw("VU", r.center().x - f.width("VU", 9.0, true) / 2.0, r.bottom() - 6.0, 9.0, ink, true);
        f.draw(channel, r.x + 5.0, r.bottom() - 5.0, 8.0, ink, true);
    }

    /// Rectangles of the two VU meters, between the speaker and the knob.
    fn vu_rects(lay: &Layout) -> [Rect; 2] {
        let left_edge = lay.grille.x + GRILLE_R + SP_S;
        // The knob's end stops and pointer reach a few pixels past its radius.
        let right_edge = lay.knob.x - KNOB_R - 4.0 - SP_S;
        let w = ((right_edge - left_edge - SP_S) / 2.0).min(84.0);
        let h = 56.0;
        let top = lay.grille.y - h / 2.0;
        [0.0, 1.0].map(|k| Rect::new((left_edge + right_edge) / 2.0 - w - SP_S / 2.0 + k * (w + SP_S), top, w, h))
    }

    /// Everything on the deck that does not move: body, speaker plate, meter
    /// dials, knob scale, the frames of the door, LCD and keys, and the print.
    /// Drawn once into a texture; see `draw_deck`.
    fn draw_deck_body(&self, lay: &Layout) {
        let d = lay.deck;
        let f = &self.fonts;

        // Body: brushed gunmetal with a lit top edge and a shadowed bottom.
        round_rect(d, R_PANEL, BODY);
        let mut y = d.y + 3.0;
        let mut k = 0;
        while y < d.bottom() - 3.0 {
            let a = if k % 2 == 0 { 0.025 } else { 0.0 } + if k % 7 == 0 { 0.02 } else { 0.0 };
            draw_rectangle(d.x + 4.0, y, d.w - 8.0, 1.0, Color::new(1.0, 1.0, 1.0, a));
            y += 2.0;
            k += 1;
        }
        draw_rectangle(d.x + R_PANEL, d.y + 1.0, d.w - 2.0 * R_PANEL, 1.0, Color::new(1.0, 1.0, 1.0, 0.25));
        draw_rectangle(d.x + R_PANEL, d.bottom() - 2.0, d.w - 2.0 * R_PANEL, 1.0, Color::new(0.0, 0.0, 0.0, 0.5));
        round_rect_lines(d, R_PANEL, 1.5, LINE);

        // Speaker plate; its holes are drawn live.
        let g = lay.grille;
        let grille = Rect::new(g.x - GRILLE_R, g.y - GRILLE_R, 2.0 * GRILLE_R, 2.0 * GRILLE_R);
        round_rect(grille, R_BOX, Color::new(0.0, 0.0, 0.0, 0.35));
        round_rect_lines(grille, R_BOX, 1.0, Color::new(1.0, 1.0, 1.0, 0.18));

        for (r, name) in Self::vu_rects(lay).into_iter().zip(["L", "R"]) {
            self.draw_vu(r, None, name);
        }

        // Volume knob: an open ring from 0 (bottom left) to 100 (bottom right).
        let k = lay.knob;
        let rad = KNOB_R - 3.0;
        draw_circle(k.x, k.y, rad - 4.0, Color::new(0.0, 0.0, 0.0, 0.3));
        gfx::arc(k.x, k.y, rad, knob_angle(0.0), 1.5 * PI, 3.0, Color::new(1.0, 1.0, 1.0, 0.22));
        // Stops at both ends of the travel.
        for v in [0.0, 1.0] {
            let dir = vec2(knob_angle(v).cos(), knob_angle(v).sin());
            let (p0, p1) = (k + dir * (rad - 5.0), k + dir * (rad + 4.0));
            draw_line(p0.x, p0.y, p1.x, p1.y, 1.5, INK);
        }

        // Cassette door: a recessed compartment, and the print below it.
        let s = lay.slot;
        round_rect(Rect::new(s.x - 2.0, s.y - 2.0, s.w + 4.0, s.h + 4.0), R_BOX + 2.0, Color::new(0.0, 0.0, 0.0, 0.45));
        round_rect(s, R_BOX, Color::from_rgba(14, 15, 17, 255));
        let print = "AUTO STOP  ·  NORMAL BIAS  120µs EQ";
        f.draw(print, s.x + 12.0, s.bottom() - 6.0, 8.0, Color::new(1.0, 1.0, 1.0, 0.4), false);

        // The LCD set into the body.
        let dp = lay.display;
        round_rect(Rect::new(dp.x - 2.0, dp.y - 2.0, dp.w + 4.0, dp.h + 4.0), R_BOX + 2.0, Color::new(0.0, 0.0, 0.0, 0.45));
        round_rect(dp, R_BOX, LCD);
        draw_rectangle(dp.x + 4.0, dp.y + 2.0, dp.w - 8.0, 1.0, Color::new(1.0, 1.0, 1.0, 0.06));
        round_rect_lines(dp, R_BOX, 1.0, Color::new(1.0, 1.0, 1.0, 0.2));

        // Key slots, with the keys' names printed on the body above them.
        for (key, name) in lay.keys.iter().zip(["REW", "PLAY", "FF", "EJECT"]) {
            f.draw(name, key.center().x - f.width(name, 9.0, true) / 2.0, key.y - 6.0, 9.0, Color::new(1.0, 1.0, 1.0, 0.75), true);
            round_rect(Rect::new(key.x - 1.0, key.y - 1.0, key.w + 2.0, key.h + 2.0), R_CHIP + 1.0, Color::new(0.0, 0.0, 0.0, 0.6));
        }
    }

    fn draw_deck(&self, lay: &Layout) {
        let d = lay.deck;
        let f = &self.fonts;

        // The still parts come from a texture painted once per deck size:
        // drawn live they cost hundreds of draw calls a frame.
        let mut cache = self.deck_cache.borrow_mut();
        if cache.as_ref().is_none_or(|(r, _)| *r != d) {
            let rt = render_target_ex(d.w as u32, d.h as u32, RenderTargetParams { sample_count: CANVAS_SAMPLES, depth: false });
            rt.texture.set_filter(FilterMode::Nearest);
            let mut cam = Camera2D::from_display_rect(d);
            cam.render_target = Some(rt.clone());
            set_camera(&cam);
            clear_background(Color::new(0.0, 0.0, 0.0, 0.0));
            self.draw_deck_body(lay);
            set_camera(&self.ui_camera());
            *cache = Some((d, rt));
        }
        if let Some((_, rt)) = cache.as_ref() {
            let params = DrawTextureParams { dest_size: Some(d.size()), flip_y: true, ..Default::default() };
            draw_texture_ex(&rt.texture, d.x, d.y, WHITE, params);
        }
        drop(cache);

        // Speaker holes, swelling with the music.
        let g = lay.grille;
        let grille = Rect::new(g.x - GRILLE_R, g.y - GRILLE_R, 2.0 * GRILLE_R, 2.0 * GRILLE_R);
        let hole = 1.5 + self.level * 0.6;
        let step = 6.5;
        let n = ((grille.w - 8.0) / step).floor() as i32;
        let pad = (grille.w - (n - 1) as f32 * step) / 2.0;
        for row in 0..n {
            for col in 0..n {
                draw_circle(grille.x + pad + col as f32 * step, grille.y + pad + row as f32 * step, hole, Color::new(0.0, 0.0, 0.0, 0.85));
            }
        }

        for (r, value) in Self::vu_rects(lay).into_iter().zip([self.vu.0, self.vu.1]) {
            self.draw_vu(r, Some(value), "");
        }

        // The knob's level: the filled part of the ring, its pointer and number.
        let k = lay.knob;
        let rad = KNOB_R - 3.0;
        gfx::arc(k.x, k.y, rad, knob_angle(0.0), 1.5 * PI * self.volume, 3.0, INK);
        let dir = vec2(knob_angle(self.volume).cos(), knob_angle(self.volume).sin());
        let tip = k + dir * rad;
        draw_circle(tip.x, tip.y, 4.0, INK);
        draw_circle(tip.x, tip.y, 1.5, BLACK);
        let level = format!("{}", (self.volume * 100.0).round() as u32);
        f.draw(&level, k.x - f.width(&level, 11.0, true) / 2.0, k.y + 4.0, 11.0, INK, true);

        // The cassette bay behind the door's smoked glass.
        let s = lay.slot;
        let bay = Rect::new(s.x + 10.0, s.y + 8.0, s.w - 20.0, s.h - 26.0);
        let gl = unsafe { get_internal_gl() };
        gl.quad_gl.scissor(Some((bay.x as i32, bay.y as i32, bay.w as i32, bay.h as i32)));
        match self.current {
            None => {
                // An empty bay: the two drive spindles and a hint.
                for x in [bay.x + bay.w * 0.3, bay.right() - bay.w * 0.3] {
                    gfx::reel(x, bay.center().y, 7.0, 0.0, Color::from_rgba(14, 15, 17, 255));
                }
                let t = "CHOOSE A TAPE";
                f.draw(t, bay.center().x - f.width(t, 10.0, false) / 2.0, bay.bottom() - 6.0, 10.0, DIM, false);
            }
            Some(i) => {
                // The tape drops in from above as the deck takes it.
                let ease = 1.0 - (1.0 - self.slot_in).powi(3);
                let mini = Rect::new(bay.x + 4.0, bay.y + 3.0 - (1.0 - ease) * bay.h, bay.w - 8.0, bay.h - 6.0);
                self.draw_deck_cassette(mini, i);
            }
        }
        let gl = unsafe { get_internal_gl() };
        gl.quad_gl.scissor(None);
        // Glass: a little smoke and a diagonal glint, then the frame.
        draw_rectangle(bay.x, bay.y, bay.w, bay.h, Color::new(0.05, 0.06, 0.07, 0.18));
        let glint = Color::new(1.0, 1.0, 1.0, 0.07);
        draw_triangle(vec2(bay.x + bay.w * 0.18, bay.y), vec2(bay.x + bay.w * 0.3, bay.y), vec2(bay.x + bay.w * 0.12, bay.bottom()), glint);
        draw_triangle(vec2(bay.x + bay.w * 0.12, bay.bottom()), vec2(bay.x + bay.w * 0.3, bay.y), vec2(bay.x + bay.w * 0.24, bay.bottom()), glint);
        round_rect_lines(bay, 3.0, 1.0, Color::new(1.0, 1.0, 1.0, 0.16));
        round_rect_lines(s, R_BOX, 1.0, Color::new(1.0, 1.0, 1.0, 0.22));
        // Sticker on the door's corner while a hi-res track plays.
        if let Some(i) = self.current
            && self.albums[i].tracks.get(self.track).is_some_and(|t| t.hires)
        {
            let w = f.width("HI-RES", 9.0, true) + 12.0;
            let tag = Rect::new(s.right() - SP_S - w, s.y - 7.0, w, 14.0);
            round_rect(tag, R_CHIP, self.accent);
            f.draw("HI-RES", tag.x + 6.0, tag.y + 10.5, 9.0, BLACK, true);
        }

        // Piano key caps. Play stays latched down while the tape runs; the
        // others spring back.
        let now = get_time();
        let playing = self.audio.is_playing();
        for (k, key) in lay.keys.iter().enumerate() {
            let latched = k == 1 && playing;
            let tapped = now - self.pressed[k] < 0.12;
            let sink = if latched { 5.0 } else if tapped { 3.0 } else { 0.0 };
            let cap = Rect::new(key.x, key.y + sink, key.w, key.h - sink);
            let base = if k == 1 { Color { a: 1.0, ..self.accent } } else { KEY };
            let shade = if latched { 0.8 } else { 1.0 };
            round_rect(cap, R_CHIP, Color::new(base.r * shade, base.g * shade, base.b * shade, 1.0));
            // A lit front lip, and three grip dots near the bottom.
            draw_rectangle(cap.x + 3.0, cap.y + 1.0, cap.w - 6.0, 1.0, Color::new(1.0, 1.0, 1.0, if k == 1 { 0.45 } else { 0.18 }));
            let dark = if k == 1 { Color::new(0.0, 0.0, 0.0, 0.55) } else { Color::new(1.0, 1.0, 1.0, 0.35) };
            for dx in [-6.0, 0.0, 6.0] {
                draw_circle(cap.center().x + dx, cap.bottom() - 8.0, 1.5, dark);
            }
            // The key's symbol, small, above the dots.
            let c = vec2(cap.center().x, cap.y + 14.0);
            let pt = |x: f32, y: f32| c + vec2(x, y);
            match k {
                0 => {
                    for dx in [-5.0, 1.0] {
                        draw_triangle(pt(dx, 0.0), pt(dx + 5.0, -4.0), pt(dx + 5.0, 4.0), dark);
                    }
                }
                1 if playing => {
                    draw_rectangle(c.x - 4.0, c.y - 4.0, 2.5, 8.0, dark);
                    draw_rectangle(c.x + 1.5, c.y - 4.0, 2.5, 8.0, dark);
                }
                1 => draw_triangle(pt(-3.0, -4.5), pt(-3.0, 4.5), pt(4.5, 0.0), dark),
                2 => {
                    for dx in [-5.0, 1.0] {
                        draw_triangle(pt(dx, -4.0), pt(dx, 4.0), pt(dx + 5.0, 0.0), dark);
                    }
                }
                _ => {
                    draw_triangle(pt(-5.0, 1.0), pt(5.0, 1.0), pt(0.0, -5.0), dark);
                    draw_rectangle(c.x - 5.0, c.y + 3.0, 10.0, 2.0, dark);
                }
            }
        }

        // Display: track, time, progress, current lyric.
        let dp = lay.display;
        let dur = self.duration().map(|d| d.as_secs_f32()).filter(|&d| d > 0.0);
        let pos = match (self.seek_drag, dur) {
            (Some(f), Some(d)) => f * d,
            _ => self.audio.position().as_secs_f32(),
        };
        // The track number stays put; only the title moves.
        let (number, label) = match self.current {
            Some(i) => (format!("A{:02}", self.track + 1), self.albums[i].tracks[self.track].title.clone()),
            None => (String::new(), "--".into()),
        };
        let label_x = if number.is_empty() {
            dp.x + SP_S
        } else {
            f.draw(&number, dp.x + SP_S, dp.y + 13.0, 11.0, DIM, false);
            dp.x + SP_S + f.width(&number, 11.0, false) + SP_S
        };
        // Titles too long for the display scroll through it, like a hi-fi readout.
        let room = dp.right() - SP_S - label_x;
        let lw = f.width(&label, 11.0, false);
        if lw <= room {
            f.draw(&label, label_x, dp.y + 13.0, 11.0, INK, false);
        } else {
            const GAP: f32 = 40.0;
            const SPEED: f32 = 22.0;
            // Pause on the start of the title before each pass.
            let cycle = (lw + GAP) / SPEED + 1.5;
            let t = (get_time() as f32 % cycle - 1.5).max(0.0);
            let x = label_x - t * SPEED;
            let gl = unsafe { get_internal_gl() };
            gl.quad_gl.scissor(Some((label_x as i32, dp.y as i32 + 2, room as i32, 16)));
            f.draw(&label, x, dp.y + 13.0, 11.0, INK, false);
            f.draw(&label, x + lw + GAP, dp.y + 13.0, 11.0, INK, false);
            let gl = unsafe { get_internal_gl() };
            gl.quad_gl.scissor(None);
        }
        // Elapsed on the left, time left on the right, around the progress bar.
        let p = lay.progress;
        let mouse: Vec2 = mouse_position().into();
        let over = self.seek_drag.is_some() || (dur.is_some() && lay.seek_bar().contains(mouse));
        let bar_h = if over { 5.0 } else { p.h };
        let bar_y = p.center().y - bar_h / 2.0;
        draw_rectangle(p.x, bar_y, p.w, bar_h, Color::new(1.0, 1.0, 1.0, 0.18));
        let time_y = p.y + 13.0;
        f.draw(&fmt_time(pos), p.x, time_y, 10.0, INK, false);
        if let Some(dur) = dur {
            let frac = (pos / dur).clamp(0.0, 1.0);
            draw_rectangle(p.x, bar_y, p.w * frac, bar_h, self.accent);
            draw_circle(p.x + p.w * frac, p.center().y, if over { 4.5 } else { 3.0 }, self.accent);
            if over && self.seek_drag.is_none() {
                // Where a click would land.
                let at = ((mouse.x - p.x) / p.w).clamp(0.0, 1.0);
                draw_rectangle(p.x + p.w * at - 0.5, p.center().y - 5.0, 1.0, 10.0, DIM);
            }
            let left = format!("-{}", fmt_time(dur - pos));
            f.draw(&left, p.right() - f.width(&left, 10.0, false), time_y, 10.0, DIM, false);
        }
        // Bottom line: the file's real format and what happens to it on the
        // way out. Red when the player itself has to convert.
        let (signal, signal_color) = match (&self.audio.error, self.current) {
            (Some(e), _) => (e.clone(), REC),
            (None, Some(i)) => {
                let format = self.albums[i].tracks[self.track].format.clone().unwrap_or_default();
                match self.audio.output {
                    Some(o) if !o.native => (format!("{format}  LINEAR>{}K", o.rate / 1000), REC),
                    Some(o) => match o.server_rate {
                        Some(srv) if srv != o.rate => (format!("{format}  OUT {}  PW>{}K", khz(o.rate), srv / 1000), DIM),
                        _ => (format!("{format}  OUT {}", khz(o.rate)), DIM),
                    },
                    None => (format, DIM),
                }
            }
            (None, None) => (String::new(), DIM),
        };
        let (signal, signal_color) = if get_time() - self.volume_at < 1.4 {
            (format!("VOLUME  {:>3}%", (self.volume * 100.0).round() as u32), INK)
        } else {
            (signal, signal_color)
        };
        f.draw(&f.fit(&signal, 10.0, false, dp.w - 16.0), dp.x + 8.0, dp.y + 45.0, 10.0, signal_color, false);
    }
}

/// The window over a printed side of a tape: the walls down from the rim at
/// `surface` to the print at `inner`, or the clear sheet flush with the rim,
/// with a little smoke and two diagonal reflections.
/// `side` is +1 for the front and -1 for the back, which faces the other way.
fn draw_recess(m: &Mat4, surface: f32, inner: f32, side: f32, shade: f32, gloss: &dyn Fn(Vec3) -> f32, part: Recess) {
    let outline = shell_outline();
    let h = TAPE / 2.0;
    let count = outline.len() as u16;
    let band = |vertices: &[Vertex]| {
        let mut indices = Vec::with_capacity(vertices.len() * 3);
        for k in 0..count {
            let next = (k + 1) % count;
            let (a0, b0, a1, b1) = (k * 2, k * 2 + 1, next * 2, next * 2 + 1);
            indices.extend([a0, a1, b1, a0, b1, b0]);
        }
        indices
    };
    let point = |p: Vec3, v: f32, a: f32| {
        let c = (v * shade * gloss(p)).clamp(0.0, 255.0) as u8;
        Vertex { position: m.transform_point3(p), uv: Vec2::ZERO, color: [c, c, c, (a * 255.0) as u8], normal: Vec4::ZERO }
    };

    // Walls, seen at an angle between rim and print.
    if part == Recess::Walls {
        let mut vertices = Vec::with_capacity(outline.len() * 2);
        for &(p, n) in outline {
            let wall = p - n * 0.01;
            let v = 22.0 + 30.0 * (1.0 - (n.y + 1.0) / 2.0);
            vertices.push(point(wall.extend(surface), v, 1.0));
            vertices.push(point(wall.extend(inner), v * 0.6, 1.0));
        }
        let indices = band(&vertices);
        draw_mesh(&Mesh { vertices, indices, texture: None });
        return;
    }

    let sheet = |p: Vec3, rgb: f32, a: f32| {
        let v = (rgb * 255.0 * shade) as u8;
        Vertex { position: m.transform_point3(p), uv: Vec2::ZERO, color: [v, v, v, (a * shade * 255.0) as u8], normal: Vec4::ZERO }
    };
    let mut vertices = vec![sheet(vec3(0.0, 0.0, surface), 0.04, GLASS_SMOKE)];
    vertices.extend(outline.iter().map(|&(p, _)| sheet(p.extend(surface), 0.04, GLASS_SMOKE)));
    let mut indices = Vec::with_capacity(outline.len() * 3);
    for k in 0..count {
        indices.extend([0, 1 + k, 1 + (k + 1) % count]);
    }
    // Each band runs top to bottom, leaning right as seen, brightest in its middle.
    let z = surface + 0.001 * side;
    let Recess::Glass(sheen) = part else { return };
    for (left, width, a) in [(-0.95, 0.5, sheen), (-0.2, 0.14, sheen * 0.7)] {
        let base = vertices.len() as u16;
        let lean = 0.3;
        for (x, alpha) in [(left, 0.0), (left + width / 2.0, a), (left + width, 0.0)] {
            vertices.push(sheet(vec3((x + lean) * side, h.y - 0.02, z), 1.0, alpha));
            vertices.push(sheet(vec3(x * side, -h.y + 0.02, z), 1.0, alpha));
        }
        for k in 0..2 {
            let i = base + k * 2;
            indices.extend([i, i + 2, i + 3, i, i + 3, i + 1]);
        }
    }
    draw_mesh(&Mesh { vertices, indices, texture: None });
}

/// One line of the settings panel.
#[derive(Clone, Copy, PartialEq)]
enum Field {
    Volume,
    SeekStep,
    Sfx,
    Mpris,
    View,
    Sort,
    Fullscreen,
    Accents,
    Reflections,
    Glass,
    Glow,
    Motion,
    LyricPace,
    KeyRepeat,
    PlayingFps,
    IdleFps,
    Rescan,
    Hints,
}

/// The settings panel, grouped under headings: (heading, field, label).
const FIELDS: [(&str, Field, &str); 18] = [
    ("PLAYBACK", Field::Volume, "Volume"),
    ("PLAYBACK", Field::SeekStep, "Seek step ( [ ] and media keys)"),
    ("PLAYBACK", Field::Sfx, "Deck sound effects"),
    ("PLAYBACK", Field::Mpris, "Status bar and media keys (MPRIS)"),
    ("LOOK", Field::View, "View"),
    ("LOOK", Field::Sort, "Sort"),
    ("LOOK", Field::Fullscreen, "Fullscreen"),
    ("LOOK", Field::Accents, "Colors from the cover"),
    ("LOOK", Field::Reflections, "Floor reflections"),
    ("LOOK", Field::Glass, "Reflections on the plastic"),
    ("LOOK", Field::Glow, "Cover light on the stage"),
    ("MOTION", Field::Motion, "Tape animations"),
    ("MOTION", Field::LyricPace, "Lyrics fill"),
    ("MOTION", Field::KeyRepeat, "Held key repeat"),
    ("PERFORMANCE", Field::PlayingFps, "Frame rate while playing"),
    ("PERFORMANCE", Field::IdleFps, "Frame rate when idle"),
    ("LIBRARY", Field::Rescan, "Music folder"),
    ("LIBRARY", Field::Hints, "Control hints"),
];

const OFF_ON: [&str; 2] = ["OFF", "ON"];
/// Height of a line in the settings panel.
const SETTINGS_ROW: f32 = 30.0;

/// Which part of the tapes a stage pass draws; see `draw_stage`.
#[derive(Clone, Copy, PartialEq)]
enum Pass {
    Solid,
    Edge,
    Face,
    Clear,
}

/// The opaque walls of a window's recess, or the clear sheet over it.
#[derive(Clone, Copy, PartialEq)]
enum Recess {
    Walls,
    /// With the brightness of its reflections.
    Glass(f32),
}

/// Rounded-rectangle outline of the tape front, counter-clockwise, as
/// (point, outward normal) pairs.
fn shell_outline() -> &'static [(Vec2, Vec2)] {
    static OUTLINE: std::sync::OnceLock<Vec<(Vec2, Vec2)>> = std::sync::OnceLock::new();
    OUTLINE.get_or_init(|| {
        const STEPS: usize = 6;
        let (hx, hy, r) = (TAPE.x / 2.0, TAPE.y / 2.0, gfx::FACE_RADIUS / FACE_W * TAPE.x);
        let corners = [(hx - r, hy - r, 0.0), (-(hx - r), hy - r, FRAC_PI_2), (-(hx - r), -(hy - r), PI), (hx - r, -(hy - r), PI + FRAC_PI_2)];
        let mut out = Vec::with_capacity(4 * (STEPS + 1));
        for (cx, cy, start) in corners {
            for s in 0..=STEPS {
                let a = start + FRAC_PI_2 * s as f32 / STEPS as f32;
                let n = vec2(a.cos(), a.sin());
                out.push((vec2(cx, cy) + n * r, n));
            }
        }
        out
    })
}

/// Lowercase without accents, so "beyonce" finds "Beyoncé".
fn fold(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' | 'ø' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            '’' | '‘' | '`' => '\'',
            _ => c,
        })
        .collect()
}

/// Let genre searches match spelling variants such as K-pop and K Pop.
fn compact_genre(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '-' | '‐' | '‑' | '–' | '—' | '−'))
        .collect()
}

fn khz(rate: u32) -> String {
    if rate % 1000 == 0 { format!("{}K", rate / 1000) } else { format!("{:.1}K", rate as f32 / 1000.0) }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Ruler label for how long ago a tape was last played.
fn recency(played_at: Option<u64>) -> &'static str {
    const DAY: u64 = 86_400;
    match played_at.map(|t| now_secs().saturating_sub(t)) {
        None => "NEVER",
        Some(age) if age < DAY => "TODAY",
        Some(age) if age < 7 * DAY => "WEEK",
        Some(age) if age < 31 * DAY => "MONTH",
        Some(_) => "OLDER",
    }
}

fn fmt_time(s: f32) -> String {
    let s = s.max(0.0) as u32;
    format!("{:02}:{:02}", s / 60, s % 60)
}

fn music_dir() -> PathBuf {
    std::env::var_os("MUSIC_DIR")
        .map(PathBuf::from)
        .or_else(dirs::audio_dir)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join("Music"))
}

#[macroquad::main(window_conf)]
async fn main() {
    let root = music_dir();
    let albums = library::scan(&root);
    eprintln!("[tapedeck] {} tapes from {}", albums.len(), root.display());
    if albums.is_empty() {
        eprintln!("[tapedeck] no audio found; set MUSIC_DIR");
        return;
    }
    let saved = load_saved();
    let mut app = App::new(albums, saved, root);
    prevent_quit();

    // Debug aid: TAPEDECK_SCREENSHOT=out.png renders a few seconds, saves a frame and exits.
    let shot = std::env::var("TAPEDECK_SCREENSHOT").ok();
    if let Some(i) = std::env::var("TAPEDECK_FLIP").ok().and_then(|v| v.parse().ok()) {
        app.flipped = Some(i);
    }
    if std::env::var("TAPEDECK_VIEW").is_ok_and(|v| v == "grid") {
        app.grid_metrics = { let g = app.grid(&app.layout()); (g.cols, g.row_h, g.area.h) };
        app.set_view(View::Grid);
    }
    if let Ok(query) = std::env::var("TAPEDECK_SEARCH") {
        app.query = query;
        app.search();
    }
    let mut frames = 0u32;

    // Debug aid: TAPEDECK_PROFILE=1 prints the average time per frame spent
    // in each stage (CPU side, including the GL calls made there) every 240 frames.
    let profile = std::env::var("TAPEDECK_PROFILE").is_ok();
    let mut spent = [0.0f64; 8];
    loop {
        let frame_start = get_time();
        let mut clock = std::time::Instant::now();
        let mut lap = |k: usize| {
            if profile {
                spent[k] += clock.elapsed().as_secs_f64();
                clock = std::time::Instant::now();
            }
        };
        app.receive_covers();
        app.input(&app.layout());
        lap(0);
        app.tick(get_frame_time().min(0.1));
        lap(1);
        app.render_faces();
        lap(2);

        let lay = app.layout();
        app.ensure_canvas();
        set_camera(&app.ui_camera());
        clear_background(FRAME_BG);
        app.draw_backdrop();
        app.draw_frame(&lay);
        lap(3);
        app.draw_stage(&lay);
        lap(4);
        app.draw_side(&lay);
        if app.show_settings {
            app.draw_settings();
        }
        lap(5);
        app.present();
        lap(6);

        frames += 1;
        if profile && frames % 240 == 0 {
            let names = ["input", "tick", "faces", "frame", "stage", "side", "present", "swap"];
            let line: Vec<String> = names.iter().zip(spent).map(|(n, t)| format!("{n} {:.2}ms", t * 1000.0 / 240.0)).collect();
            eprintln!("[profile] {}  ({:.0} fps)", line.join("  "), get_fps());
            spent = [0.0; 8];
        }
        if let Some(path) = &shot {
            if frames == 240 {
                get_screen_data().export_png(path);
                break;
            }
        }
        if is_quit_requested() {
            app.save();
            break;
        }
        // Idle frames wait instead of redrawing an unchanged deck at the
        // display's full rate.
        let budget = app.frame_budget() as f64;
        let used = get_time() - frame_start;
        if budget > used {
            std::thread::sleep(std::time::Duration::from_secs_f64(budget - used));
        }
        let swap = std::time::Instant::now();
        next_frame().await;
        if profile {
            spent[7] += swap.elapsed().as_secs_f64();
        }
    }
}
