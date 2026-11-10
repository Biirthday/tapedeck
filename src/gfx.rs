//! Drawing helpers: fonts, rounded shapes, reels and the cassette face texture.
use std::collections::HashSet;
use std::f32::consts::{FRAC_PI_2, PI, TAU};
use std::process::Command;
use std::rc::Rc;

use macroquad::miniquad::{Backend, MipmapFilterMode};
use macroquad::prelude::*;

pub const INK: Color = Color::new(0.957, 0.957, 0.941, 1.0);
pub const LINE: Color = Color::new(1.0, 1.0, 1.0, 0.82);
pub const DIM: Color = Color::new(1.0, 1.0, 1.0, 0.58);

pub fn filter_tape_texture(texture: &Texture2D) {
    let id = texture.raw_miniquad_id();
    let gl = unsafe { get_internal_gl() };
    // Macroquad's resolved render targets have no mip levels allocated on Metal.
    if gl.quad_context.info().backend == Backend::OpenGl {
        gl.quad_context.texture_generate_mipmaps(id);
        gl.quad_context.texture_set_filter(id, FilterMode::Linear, MipmapFilterMode::Linear);
    }
}

/// FxHash: a fast, non-cryptographic hash for the font caches, which are
/// looked up for every character drawn every frame.
#[derive(Default)]
struct Fx(u64);

impl Fx {
    fn add(&mut self, x: u64) {
        self.0 = (self.0.rotate_left(5) ^ x).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

impl std::hash::Hasher for Fx {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            self.add(u64::from_le_bytes(chunk.try_into().unwrap()));
        }
        for &b in chunks.remainder() {
            self.add(b as u64);
        }
    }
    fn write_u64(&mut self, x: u64) {
        self.add(x);
    }
    fn write_u32(&mut self, x: u32) {
        self.add(x as u64);
    }
    fn write_u16(&mut self, x: u16) {
        self.add(x as u64);
    }
    fn write_u8(&mut self, x: u8) {
        self.add(x as u64);
    }
    fn write_usize(&mut self, x: usize) {
        self.add(x as u64);
    }
}

type FxBuild = std::hash::BuildHasherDefault<Fx>;

/// A parsed font and the characters it has glyphs for.
struct Face {
    font: fontdue::Font,
    path: String,
    chars: HashSet<char, FxBuild>,
}

impl Face {
    /// Reads and parses a font file. Big fonts (the CJK one has ~45k glyphs)
    /// take a while, so this runs off the main thread where it can.
    fn parse(path: &str) -> Option<Face> {
        let bytes = std::fs::read(path).ok()?;
        let font = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).ok()?;
        let chars = font.chars().keys().copied().collect();
        Some(Face { font, path: path.to_string(), chars })
    }

    /// Whitespace and control characters draw as blanks in any font.
    fn has(&self, c: char) -> bool {
        c.is_whitespace() || c.is_control() || self.chars.contains(&c)
    }
}

/// The file of the system font fontconfig picks for `pattern`.
fn system_font_path(pattern: &str) -> Option<String> {
    let out = Command::new("fc-match").args(["-f", "%{file}", pattern]).output().ok()?;
    let path = String::from_utf8(out.stdout).ok()?;
    Some(path.trim().to_string()).filter(|p| !p.is_empty())
}

/// The first of `patterns` that resolves to a font that parses.
fn system_font(patterns: &[&str]) -> Option<Face> {
    patterns.iter().find_map(|p| Face::parse(&system_font_path(p)?))
}

type Loading = std::thread::JoinHandle<Option<Face>>;

/// The interface fonts being looked up and parsed: regular, bold and CJK.
static PRELOAD: std::sync::Mutex<Option<[Loading; 3]>> = std::sync::Mutex::new(None);

/// Starts finding and parsing the interface fonts, each on its own thread,
/// so the work overlaps the window opening and the library scan. Later
/// calls do nothing; `Fonts::load` picks up the results.
pub fn preload_fonts() {
    let mut preload = PRELOAD.lock().unwrap_or_else(|e| e.into_inner());
    if preload.is_some() {
        return;
    }
    let spawn = |patterns: &'static [&'static str]| std::thread::spawn(move || system_font(patterns));
    *preload = Some([
        spawn(&["JetBrains Mono:style=Regular", "monospace"]),
        spawn(&["JetBrains Mono:style=Bold", "monospace:bold"]),
        // Many titles are Japanese or Korean.
        spawn(&["Noto Sans CJK JP:style=Bold"]),
    ]);
}

/// Where a rasterized glyph sits in the atlas, and how to place it.
#[derive(Clone, Copy)]
struct Glyph {
    rect: Rect,
    offset: Vec2,
    advance: f32,
}

/// One texture holding every rasterized glyph, of every font and size.
///
/// Glyphs never move once placed, and a new one uploads only its own
/// pixels. (Macroquad's own font atlas uploads the whole texture for each
/// new glyph and repacks everything when it grows, which stalled frames for
/// tens of milliseconds whenever a title showed new characters.) When the
/// atlas fills up it gets taller: the glyphs keep their places in a new
/// texture, and the old texture is kept, not deleted, so draws already
/// queued with it still find their pixels and its GL name is never reused
/// (miniquad's binding cache would take a reused name as already bound).
struct Atlas {
    texture: Texture2D,
    pixels: Vec<u8>,
    height: u16,
    /// Packing cursor: the free spot in the current row and that row's height.
    x: u16,
    y: u16,
    row: u16,
    retired: Vec<Texture2D>,
}

const ATLAS_W: u16 = 1024;
const ATLAS_MAX_H: u16 = 8192;
/// Clear pixels around each glyph, so linear filtering does not bleed.
const ATLAS_GAP: u16 = 2;

impl Atlas {
    fn new() -> Self {
        let height = 512;
        let pixels = vec![0; ATLAS_W as usize * height as usize * 4];
        Self { texture: Self::upload(&pixels, height), pixels, height, x: 0, y: 0, row: 0, retired: Vec::new() }
    }

    fn upload(pixels: &[u8], height: u16) -> Texture2D {
        let texture = Texture2D::from_rgba8(ATLAS_W, height, pixels);
        texture.set_filter(FilterMode::Linear);
        texture
    }

    /// Places a `w` by `h` glyph of coverage values; None when it is full.
    fn insert(&mut self, w: u16, h: u16, coverage: &[u8]) -> Option<Rect> {
        if w == 0 || h == 0 {
            return Some(Rect::default());
        }
        if w + 2 * ATLAS_GAP > ATLAS_W {
            return None;
        }
        if self.x + w + 2 * ATLAS_GAP > ATLAS_W {
            self.x = 0;
            self.y += self.row + ATLAS_GAP;
            self.row = 0;
        }
        while self.y + h + 2 * ATLAS_GAP > self.height {
            if self.height >= ATLAS_MAX_H {
                return None;
            }
            self.height *= 2;
            self.pixels.resize(ATLAS_W as usize * self.height as usize * 4, 0);
            let grown = Self::upload(&self.pixels, self.height);
            self.retired.push(std::mem::replace(&mut self.texture, grown));
        }
        let (x, y) = (self.x + ATLAS_GAP, self.y + ATLAS_GAP);
        let bytes: Vec<u8> = coverage.iter().flat_map(|&a| [255, 255, 255, a]).collect();
        let row_bytes = w as usize * 4;
        for (j, row) in bytes.chunks_exact(row_bytes).enumerate() {
            let at = ((y as usize + j) * ATLAS_W as usize + x as usize) * 4;
            self.pixels[at..at + row_bytes].copy_from_slice(row);
        }
        self.texture.update_part(&Image { bytes, width: w, height: h }, x as i32, y as i32, w as i32, h as i32);
        self.x += w + ATLAS_GAP;
        self.row = self.row.max(h + ATLAS_GAP);
        Some(Rect::new(x as f32, y as f32, w as f32, h as f32))
    }

    /// Starts over empty, for when it cannot grow any more.
    fn clear(&mut self) {
        let fresh = Self::new();
        let old = std::mem::replace(self, fresh);
        self.retired = old.retired;
        self.retired.push(old.texture);
    }
}

pub struct Fonts {
    regular: Option<Rc<Face>>,
    bold: Option<Rc<Face>>,
    /// Only needed for titles in Japanese or Korean, so it may still be
    /// parsing in the background when the first frames draw.
    cjk: std::cell::OnceCell<Option<Rc<Face>>>,
    cjk_loading: std::cell::RefCell<Option<Loading>>,
    /// Fonts found on the system for characters the ones above lack.
    fallbacks: std::cell::RefCell<Vec<Rc<Face>>>,
    /// Characters already looked up on the system, found or not.
    searched: std::cell::RefCell<HashSet<char, FxBuild>>,
    /// Rasterized glyphs by font address, character and size.
    glyphs: std::cell::RefCell<std::collections::HashMap<(usize, char, u16), Glyph, FxBuild>>,
    atlas: std::cell::RefCell<Atlas>,
    /// Measured widths by a hash of text, size and weight: the same labels
    /// are measured every frame, and measuring walks every glyph.
    widths: std::cell::RefCell<std::collections::HashMap<u64, f32, FxBuild>>,
    /// Truncated strings from `fit`, cached by text, size, weight and max width.
    fitted: std::cell::RefCell<std::collections::HashMap<u64, String, FxBuild>>,
}

fn face_id(face: &Rc<Face>) -> usize {
    Rc::as_ptr(face) as usize
}

/// Text size in pixels for the font, as macroquad sizes it.
fn pixel_size(size: u16) -> (u16, f32) {
    let dpi = miniquad::window::dpi_scale();
    ((size as f32 * dpi).ceil() as u16, dpi)
}

impl Fonts {
    /// Takes the fonts `preload_fonts` started on (starting it if needed),
    /// waiting for the regular and bold ones.
    pub fn load() -> Self {
        preload_fonts();
        let loading = PRELOAD.lock().unwrap_or_else(|e| e.into_inner()).take();
        let [regular, bold, cjk] = match loading {
            Some([r, b, c]) => [Some(r), Some(b), Some(c)],
            None => [None, None, None],
        };
        let join = |h: Option<Loading>| h.and_then(|h| h.join().ok().flatten()).map(Rc::new);
        Self {
            regular: join(regular),
            bold: join(bold),
            cjk: Default::default(),
            cjk_loading: std::cell::RefCell::new(cjk),
            fallbacks: Default::default(),
            searched: Default::default(),
            glyphs: Default::default(),
            atlas: std::cell::RefCell::new(Atlas::new()),
            widths: Default::default(),
            fitted: Default::default(),
        }
    }

    /// The CJK font, waiting for it to finish parsing the first time.
    fn cjk(&self) -> Option<&Rc<Face>> {
        self.cjk
            .get_or_init(|| self.cjk_loading.borrow_mut().take().and_then(|h| h.join().ok().flatten()).map(Rc::new))
            .as_ref()
    }

    /// The glyph for `c` at pixel size `px`, rasterizing it the first time.
    fn glyph(&self, face: &Rc<Face>, c: char, px: u16) -> Glyph {
        let key = (face_id(face), c, px);
        if let Some(&g) = self.glyphs.borrow().get(&key) {
            return g;
        }
        let (m, coverage) = face.font.rasterize(c, px as f32);
        let mut atlas = self.atlas.borrow_mut();
        let rect = atlas.insert(m.width as u16, m.height as u16, &coverage).or_else(|| {
            // Full: start a new atlas; glyphs are rasterized again as drawn.
            atlas.clear();
            self.glyphs.borrow_mut().clear();
            atlas.insert(m.width as u16, m.height as u16, &coverage)
        });
        let g = Glyph {
            rect: rect.unwrap_or_default(),
            offset: vec2(m.xmin as f32, m.ymin as f32),
            advance: m.advance_width,
        };
        self.glyphs.borrow_mut().insert(key, g);
        g
    }

    fn pick(&self, text: &str, bold: bool) -> Option<&Rc<Face>> {
        // Latin and general punctuation stay in the mono font.
        let wide = |c: char| c as u32 > 0x24F && !(0x2000..=0x206F).contains(&(c as u32));
        if text.chars().any(wide) && let Some(cjk) = self.cjk() {
            return Some(cjk);
        }
        if bold { self.bold.as_ref() } else { self.regular.as_ref() }
    }

    /// A font with a glyph for `c`: the text's own font when it has one,
    /// then the others loaded, then whatever fontconfig finds on the system.
    fn face_for(&self, c: char, primary: Option<&Rc<Face>>, bold: bool) -> Option<Rc<Face>> {
        if primary.is_none_or(|f| f.has(c)) {
            return primary.cloned();
        }
        let mono = if bold { &self.bold } else { &self.regular };
        let loaded = mono.iter().chain(self.cjk()).cloned();
        let found = loaded.chain(self.fallbacks.borrow().iter().cloned()).find(|f| f.has(c));
        if found.is_some() || !self.searched.borrow_mut().insert(c) {
            return found.or_else(|| primary.cloned());
        }
        let weight = if bold { ":weight=bold" } else { "" };
        let path = system_font_path(&format!(":charset={:x}{weight}", c as u32));
        let mut fallbacks = self.fallbacks.borrow_mut();
        let face = match path {
            Some(path) if !fallbacks.iter().any(|f| f.path == path) => Face::parse(&path).map(Rc::new),
            _ => None,
        };
        match face.filter(|f| f.has(c)) {
            Some(face) => {
                fallbacks.push(face.clone());
                Some(face)
            }
            None => primary.cloned(),
        }
    }

    /// `text` cut into runs that each draw with one font.
    fn runs<'a>(&self, text: &'a str, bold: bool) -> Vec<(Option<Rc<Face>>, &'a str)> {
        let primary = self.pick(text, bold);
        if primary.is_none_or(|f| text.chars().all(|c| f.has(c))) {
            return vec![(primary.cloned(), text)];
        }
        let mut runs: Vec<(Option<Rc<Face>>, &str)> = Vec::new();
        let mut start = 0;
        let mut current: Option<Option<Rc<Face>>> = None;
        for (i, c) in text.char_indices() {
            let face = self.face_for(c, primary, bold);
            match &current {
                Some(f) if same_face(f, &face) => {}
                Some(f) => {
                    runs.push((f.clone(), &text[start..i]));
                    start = i;
                    current = Some(face);
                }
                None => current = Some(face),
            }
        }
        if let Some(f) = current {
            runs.push((f, &text[start..]));
        }
        runs
    }

    /// Rasterizes every glyph of `text` ahead of drawing it.
    pub fn prepare_text(&self, text: &str, size: u16, bold: bool) {
        let (px, _) = pixel_size(size);
        for (face, run) in self.runs(text, bold) {
            if let Some(face) = &face {
                for c in run.chars() {
                    self.glyph(face, c, px);
                }
            }
        }
    }

    /// Width of a run in one font, as macroquad measures it: advances
    /// only, no kerning. Measuring does not rasterize.
    fn run_width(face: Option<&Rc<Face>>, run: &str, size: u16, scale: f32) -> f32 {
        let Some(face) = face else {
            return measure_text(run, None, size, scale).width;
        };
        let (px, dpi) = pixel_size(size);
        run.chars().map(|c| face.font.metrics(c, px as f32).advance_width * scale).sum::<f32>() / dpi
    }

    /// Draws `text` with its baseline at `y`, each run in its own font,
    /// at `scale` times `size`. Glyphs are placed as macroquad's
    /// `draw_text_ex` places them.
    pub fn draw_scaled(&self, text: &str, x: f32, y: f32, size: u16, scale: f32, color: Color, bold: bool) {
        let mut x = x;
        for (face, run) in self.runs(text, bold) {
            let Some(face) = &face else {
                // No system font at all: macroquad's built-in one.
                draw_text_ex(run, x, y, TextParams { font: None, font_size: size, font_scale: scale, color, ..Default::default() });
                x += measure_text(run, None, size, scale).width;
                continue;
            };
            let (px, dpi) = pixel_size(size);
            let mut pen = 0.0;
            let mut last = None;
            for c in run.chars() {
                let g = self.glyph(face, c, px);
                let kern = last.and_then(|l| face.font.horizontal_kern(l, c, px as f32)).unwrap_or(0.0);
                last = Some(c);
                if g.rect.w > 0.0 {
                    let (w, h) = (g.rect.w * scale, g.rect.h * scale);
                    let dx = (g.offset.x * scale + pen + kern) / dpi + x;
                    let dy = (-h - g.offset.y * scale) / dpi + y;
                    let atlas = self.atlas.borrow();
                    let params = DrawTextureParams { dest_size: Some(vec2(w / dpi, h / dpi)), source: Some(g.rect), ..Default::default() };
                    draw_texture_ex(&atlas.texture, dx, dy, color, params);
                }
                pen += g.advance * scale + kern;
            }
            x += Self::run_width(Some(face), run, size, scale);
        }
    }

    pub fn width(&self, text: &str, size: f32, bold: bool) -> f32 {
        use std::hash::{Hash, Hasher};
        let mut h = Fx::default();
        (text, size as u16, bold).hash(&mut h);
        let key = h.finish();
        if let Some(&w) = self.widths.borrow().get(&key) {
            return w;
        }
        let w = self.runs(text, bold).iter().map(|(face, run)| Self::run_width(face.as_ref(), run, size as u16, 1.0)).sum();
        let mut widths = self.widths.borrow_mut();
        // Scrolling titles and search results keep adding new strings.
        if widths.len() > 20_000 {
            widths.clear();
        }
        widths.insert(key, w);
        w
    }

    /// Draws `text` with its baseline at `y`.
    pub fn draw(&self, text: &str, x: f32, y: f32, size: f32, color: Color, bold: bool) {
        self.draw_scaled(text, x, y, size as u16, 1.0, color, bold);
    }

    /// Breaks `text` into at most `lines` lines no wider than `max`. Words
    /// stay whole when they fit; longer runs break at character boundaries.
    pub fn wrap(&self, text: &str, size: f32, bold: bool, max: f32, lines: usize) -> Vec<String> {
        let mut tokens: Vec<&str> = Vec::new();
        for word in text.split_inclusive(' ') {
            if self.width(word.trim_end(), size, bold) <= max {
                tokens.push(word);
            } else {
                for (i, c) in word.char_indices() {
                    tokens.push(&word[i..i + c.len_utf8()]);
                }
            }
        }
        let mut out = vec![String::new()];
        for token in tokens {
            let line = out.last_mut().unwrap();
            let prev_len = line.len();
            line.push_str(token);
            if prev_len > 0 && self.width(line.trim_end(), size, bold) > max {
                line.truncate(prev_len);
                out.push(token.trim_start().to_string());
            }
        }
        if out.len() > lines {
            let rest = out[lines - 1..].concat();
            out.truncate(lines - 1);
            out.push(rest);
        }
        let n = out.len();
        out.into_iter()
            .enumerate()
            .map(|(k, mut l)| {
                let trimmed_len = l.trim_end().len();
                l.truncate(trimmed_len);
                if k + 1 == n { self.fit(&l, size, bold, max) } else { l }
            })
            .collect()
    }

    /// Cuts `text` with an ellipsis so it fits in `max` pixels.
    pub fn fit(&self, text: &str, size: f32, bold: bool, max: f32) -> String {
        if self.width(text, size, bold) <= max {
            return text.to_string();
        }
        use std::hash::{Hash, Hasher};
        let mut h = Fx::default();
        (text, size as u16, bold, (max * 2.0) as i32).hash(&mut h);
        let key = h.finish();
        if let Some(s) = self.fitted.borrow().get(&key) {
            return s.clone();
        }
        let ends: Vec<usize> = text.char_indices().skip(1).map(|(i, _)| i).collect();
        let mut lo = 0usize;
        let mut hi = ends.len();
        let mut buf = String::with_capacity(text.len() + 3);
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            buf.clear();
            buf.push_str(&text[..ends[mid - 1]]);
            buf.push('…');
            if self.width(&buf, size, bold) <= max {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        if lo == 0 {
            buf.clear();
        } else {
            buf.clear();
            buf.push_str(&text[..ends[lo - 1]]);
            buf.push('…');
        }
        let mut fitted = self.fitted.borrow_mut();
        if fitted.len() > 8_000 {
            fitted.clear();
        }
        fitted.insert(key, buf.clone());
        buf
    }

    /// Picks the largest font size in `min..=max` where `text` fits in `max_w`
    /// without rasterizing intermediate sizes that will not be drawn.
    pub fn fit_size(&self, text: &str, min: u16, max: u16, bold: bool, max_w: f32) -> f32 {
        if text.is_empty() {
            return max as f32;
        }
        let w_min = self.width(text, min as f32, bold);
        if w_min >= max_w || w_min <= 0.0 {
            return min as f32;
        }
        let mut s = ((min as f32 * max_w / w_min).floor() as u16).clamp(min, max);
        while s > min && self.width(text, s as f32, bold) > max_w {
            s -= 1;
        }
        s as f32
    }
}

fn same_face(a: &Option<Rc<Face>>, b: &Option<Rc<Face>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Rc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

pub fn round_rect(r: Rect, radius: f32, color: Color) {
    let rad = radius.min(r.w / 2.0).min(r.h / 2.0);
    draw_rectangle(r.x + rad, r.y, r.w - 2.0 * rad, r.h, color);
    draw_rectangle(r.x, r.y + rad, rad, r.h - 2.0 * rad, color);
    draw_rectangle(r.right() - rad, r.y + rad, rad, r.h - 2.0 * rad, color);
    for (cx, cy, start) in corners(r, rad) {
        sector(cx, cy, rad, start, color);
    }
}

fn corners(r: Rect, rad: f32) -> [(f32, f32, f32); 4] {
    [
        (r.x + rad, r.y + rad, PI),
        (r.right() - rad, r.y + rad, -FRAC_PI_2),
        (r.right() - rad, r.bottom() - rad, 0.0),
        (r.x + rad, r.bottom() - rad, FRAC_PI_2),
    ]
}

fn sector(cx: f32, cy: f32, rad: f32, start: f32, color: Color) {
    const STEPS: usize = 8;
    for i in 0..STEPS {
        let a0 = start + FRAC_PI_2 * i as f32 / STEPS as f32;
        let a1 = start + FRAC_PI_2 * (i + 1) as f32 / STEPS as f32;
        draw_triangle(
            vec2(cx, cy),
            vec2(cx + a0.cos() * rad, cy + a0.sin() * rad),
            vec2(cx + a1.cos() * rad, cy + a1.sin() * rad),
            color,
        );
    }
}

pub fn round_rect_lines(r: Rect, radius: f32, thickness: f32, color: Color) {
    let rad = radius.min(r.w / 2.0).min(r.h / 2.0);
    let t = thickness;
    draw_rectangle(r.x + rad, r.y, r.w - 2.0 * rad, t, color);
    draw_rectangle(r.x + rad, r.bottom() - t, r.w - 2.0 * rad, t, color);
    draw_rectangle(r.x, r.y + rad, t, r.h - 2.0 * rad, color);
    draw_rectangle(r.right() - t, r.y + rad, t, r.h - 2.0 * rad, color);
    for (cx, cy, start) in corners(r, rad) {
        arc(cx, cy, rad - t / 2.0, start, FRAC_PI_2, t, color);
    }
}

pub fn arc(cx: f32, cy: f32, rad: f32, start: f32, sweep: f32, thickness: f32, color: Color) {
    let steps = ((sweep.abs() * rad / 3.0).ceil() as usize).max(4);
    let (r0, r1) = (rad - thickness / 2.0, rad + thickness / 2.0);
    for i in 0..steps {
        let a0 = start + sweep * i as f32 / steps as f32;
        let a1 = start + sweep * (i + 1) as f32 / steps as f32;
        let (c0, s0, c1, s1) = (a0.cos(), a0.sin(), a1.cos(), a1.sin());
        let p = [
            vec2(cx + c0 * r0, cy + s0 * r0),
            vec2(cx + c0 * r1, cy + s0 * r1),
            vec2(cx + c1 * r1, cy + s1 * r1),
            vec2(cx + c1 * r0, cy + s1 * r0),
        ];
        draw_triangle(p[0], p[1], p[2], color);
        draw_triangle(p[0], p[2], p[3], color);
    }
}

pub fn ring(cx: f32, cy: f32, rad: f32, thickness: f32, color: Color) {
    arc(cx, cy, rad, 0.0, TAU, thickness, color);
}

/// A tape reel: rim, six teeth and a hub, rotated by `angle`.
pub fn reel(cx: f32, cy: f32, rad: f32, angle: f32, hub: Color) {
    draw_circle(cx, cy, rad, INK);
    draw_circle(cx, cy, rad * 0.78, hub);
    for k in 0..6 {
        let a = angle + k as f32 * TAU / 6.0;
        let (c, s) = (a.cos(), a.sin());
        let (w, r0, r1) = (rad * 0.17, rad * 0.32, rad * 0.8);
        let n = vec2(-s, c) * w;
        let p0 = vec2(cx + c * r0, cy + s * r0);
        let p1 = vec2(cx + c * r1, cy + s * r1);
        draw_triangle(p0 - n, p0 + n, p1 + n, INK);
        draw_triangle(p0 - n, p1 + n, p1 - n, INK);
    }
    draw_circle(cx, cy, rad * 0.3, hub);
}

/// The part of `tex` that fills an area of `size`, cropped like CSS
/// `background-size: cover`.
pub fn cover_source(tex: &Texture2D, size: Vec2, focus_y: f32) -> Rect {
    let (tw, th) = (tex.width(), tex.height());
    let scale = (size.x / tw).max(size.y / th);
    let (sw, sh) = (size.x / scale, size.y / scale);
    Rect::new((tw - sw) / 2.0, (th - sh) * focus_y, sw, sh)
}

/// Draws `tex` filling `dest`, cropped like CSS `background-size: cover`.
pub fn cover_fit(tex: &Texture2D, dest: Rect, focus_y: f32, tint: Color) {
    let source = cover_source(tex, dest.size(), focus_y);
    draw_texture_ex(tex, dest.x, dest.y, tint, DrawTextureParams { dest_size: Some(dest.size()), source: Some(source), ..Default::default() });
}

pub const FACE_W: f32 = 512.0;
pub const FACE_H: f32 = 324.0;
/// Corner radius of the cassette shell, in face-texture pixels.
pub const FACE_RADIUS: f32 = 18.0;

#[derive(Clone, Copy, PartialEq)]
pub enum FaceStyle {
    Album,
    /// Label is a collage of the covers its tracks come from.
    Mix,
    /// Unused tape: ruled paper waiting for a name.
    Blank,
}

/// Label paper, as used on blank labels and the title strip.
const PAPER: Color = Color::new(236.0 / 255.0, 231.0 / 255.0, 216.0 / 255.0, 1.0);
/// Height of the label's title strip and of its printed foot band.
const HEADER_H: f32 = 64.0;
const BAND_H: f32 = 26.0;

/// A silver cross-head screw.
fn screw(x: f32, y: f32, r: f32) {
    draw_circle(x, y, r + 1.5, Color::from_rgba(6, 6, 7, 255));
    draw_circle(x, y, r, Color::from_rgba(120, 122, 126, 255));
    draw_circle(x - r * 0.25, y - r * 0.25, r * 0.6, Color::from_rgba(190, 192, 196, 255));
    let k = r * 0.7;
    draw_line(x - k, y, x + k, y, 1.6, Color::from_rgba(40, 40, 44, 255));
    draw_line(x, y - k, x, y + k, 1.6, Color::from_rgba(40, 40, 44, 255));
}

/// Reel seen through the window: tape wound on a white hub with teeth.
fn window_reel(c: Vec2, pack: f32, angle: f32) {
    draw_circle(c.x, c.y, pack, Color::from_rgba(58, 40, 26, 255));
    ring(c.x, c.y, pack - 1.0, 1.0, Color::from_rgba(90, 64, 42, 255));
    draw_circle(c.x, c.y, 17.0, Color::from_rgba(226, 226, 222, 255));
    draw_circle(c.x, c.y, 11.0, Color::from_rgba(24, 24, 26, 255));
    for k in 0..6 {
        let a = angle + k as f32 * TAU / 6.0;
        let (dir, n) = (vec2(a.cos(), a.sin()), vec2(-a.sin(), a.cos()));
        let (p0, p1) = (c + dir * 6.0, c + dir * 11.5);
        draw_triangle(p0 - n * 1.6, p0 + n * 1.6, p1 + n * 1.6, Color::from_rgba(226, 226, 222, 255));
        draw_triangle(p0 - n * 1.6, p1 + n * 1.6, p1 - n * 1.6, Color::from_rgba(226, 226, 222, 255));
    }
    draw_circle(c.x, c.y, 4.0, Color::from_rgba(226, 226, 222, 255));
}

/// Paints a cassette front into `target`: dark shell, full-bleed label with
/// the album art and printed text, a smoked window showing the reels, and
/// the small parts real tapes have (screws, head openings, printed marks).
#[allow(clippy::too_many_arguments)]
pub fn render_face(
    target: &RenderTarget,
    fonts: &Fonts,
    covers: &[Texture2D],
    shell: Color,
    stripe: Color,
    title: &str,
    artist: &str,
    year: &str,
    style: FaceStyle,
) {
    let paper = style == FaceStyle::Blank || (style == FaceStyle::Mix && covers.is_empty());
    let label = Rect::new(22.0, 18.0, FACE_W - 44.0, 214.0);
    let header = Rect::new(label.x, label.y, label.w, HEADER_H);
    let band = Rect::new(label.x, label.bottom() - BAND_H, label.w, BAND_H);
    let win = Rect::new(FACE_W / 2.0 - 132.0, 104.0, 264.0, 78.0);
    let mut labels = Vec::with_capacity(11);
    let mut print = |text: &str, x: f32, y: f32, size: f32, color: Color, bold: bool| {
        labels.push((text.to_owned(), x, y, size as u16, color, bold));
    };

    if !paper {
        let ink = Color::from_rgba(28, 34, 70, 255);
        let side = Rect::new(header.x + 12.0, header.y + 12.0, 30.0, 30.0);
        print("A", side.center().x - fonts.width("A", 20.0, true) / 2.0, side.y + 23.0, 20.0, ink, true);
        let yw = fonts.width(year, 13.0, false);
        print(year, header.right() - 14.0 - yw, header.y + 17.0, 13.0, Color::from_rgba(60, 66, 100, 255), false);
        let tx = header.x + 52.0;
        let width = header.right() - 14.0 - yw - 12.0 - tx;
        let title = title.to_uppercase();
        let size = fonts.fit_size(&title, 18, 24, true, width);
        print(&fonts.fit(&title, size, true, width), tx, header.y + 34.0, size, ink, true);
        let by = fonts.fit(&artist.to_uppercase(), 12.0, true, header.w - 66.0);
        print(&by, tx, header.y + 54.0, 12.0, Color::from_rgba(60, 66, 100, 255), true);

        // Foot band: tape type and length, dark on the band's color.
        let dark = Color::new(0.0, 0.0, 0.0, 0.82);
        let base = band.y + band.h - 7.0;
        print("C-60", band.x + 14.0, base, 17.0, dark, true);
        let x = band.x + 14.0 + fonts.width("C-60", 17.0, true) + 12.0;
        print("60 MIN  ·  30 MIN PER SIDE", x, base - 1.0, 10.0, dark, true);
        let ty = "TYPE I  NORMAL";
        print(ty, band.right() - 14.0 - fonts.width(ty, 10.0, true), base - 1.0, 10.0, dark, true);
    } else {
        // Printed text: title and artist over the art.
        let (ink, sub, shadow) = if paper {
            (Color::from_rgba(28, 34, 70, 255), Color::from_rgba(60, 66, 100, 255), Color::new(0.0, 0.0, 0.0, 0.0))
        } else {
            (WHITE, Color::from_rgba(227, 227, 220, 255), Color::new(0.0, 0.0, 0.0, 0.75))
        };
        let tx = label.x + 16.0;
        let title = title.to_uppercase();
        let title_width = label.w - if style == FaceStyle::Blank { 80.0 } else { 32.0 };
        let title_size = fonts.fit_size(&title, 25, 32, true, title_width);
        let text = fonts.fit(&title, title_size, true, title_width);
        print(&text, tx, label.y + 37.0, title_size, ink, true);
        let by = fonts.fit(artist, 21.0, true, label.w - 32.0);
        print(&by, tx, label.y + 62.0, 21.0, sub, true);

        // Side badge and year along the label's foot.
        let foot = label.bottom() - 14.0;
        let a_color = if paper { Color::from_rgba(236, 231, 216, 255) } else { BLACK };
        print("A", tx + 10.0 - fonts.width("A", 15.0, true) / 2.0, foot, 15.0, a_color, true);
        let yw = fonts.width(year, 15.0, false);
        print(year, label.right() - 16.0 - yw + 1.0, foot + 1.0, 15.0, shadow, false);
        print(year, label.right() - 16.0 - yw, foot, 15.0, sub, false);
    }
    let scale = Color::new(1.0, 1.0, 1.0, 0.55);
    for (k, text) in [(-6, "30"), (0, "0"), (6, "30")] {
        let x = win.center().x + k as f32 * 6.0 - fonts.width(text, 8.0, false) / 2.0;
        print(text, x, win.y + 24.0, 8.0, scale, false);
    }
    for (text, _, _, font_size, _, bold) in &labels {
        fonts.prepare_text(text, *font_size, *bold);
    }

    let mut cam = Camera2D::from_display_rect(Rect::new(0.0, 0.0, FACE_W, FACE_H));
    cam.render_target = Some(target.clone());
    set_camera(&cam);
    clear_background(Color::new(0.0, 0.0, 0.0, 0.0));
    // Stable per-tape variation (how much tape is on each reel).
    let seed = title.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    let wound = (seed % 1000) as f32 / 1000.0;

    // Shell: moulded plastic, a touch lighter at the top.
    let full = Rect::new(0.0, 0.0, FACE_W, FACE_H);
    round_rect(full, FACE_RADIUS, shell);
    for i in 0..8 {
        let a = 0.035 * (8 - i) as f32 / 8.0;
        draw_rectangle(FACE_RADIUS, i as f32 * 4.0, FACE_W - 2.0 * FACE_RADIUS, 4.0, Color::new(1.0, 1.0, 1.0, a));
    }
    round_rect_lines(full, FACE_RADIUS, 2.0, Color::new(1.0, 1.0, 1.0, 0.22));
    round_rect_lines(Rect::new(2.0, 2.0, FACE_W - 4.0, FACE_H - 4.0), FACE_RADIUS - 2.0, 2.0, Color::new(0.0, 0.0, 0.0, 0.6));

    // Label, nearly edge to edge, with rounded corners like printed card.
    round_rect(Rect::new(label.x - 2.0, label.y - 2.0, label.w + 4.0, label.h + 4.0), 9.0, Color::from_rgba(4, 4, 5, 255));
    match (style, covers) {
        (FaceStyle::Blank, _) | (FaceStyle::Mix, []) => {
            round_rect(label, 8.0, Color::from_rgba(236, 231, 216, 255));
            let mut y = label.y + 92.0;
            while y < label.bottom() - 8.0 {
                draw_rectangle(label.x + 12.0, y, label.w - 24.0, 1.5, Color::from_rgba(150, 175, 205, 150));
                y += 24.0;
            }
            if style == FaceStyle::Blank {
                let c = vec2(label.right() - 40.0, label.y + 36.0);
                draw_rectangle(c.x - 18.0, c.y - 3.0, 36.0, 6.0, Color::from_rgba(190, 50, 45, 255));
                draw_rectangle(c.x - 3.0, c.y - 18.0, 6.0, 36.0, Color::from_rgba(190, 50, 45, 255));
            }
        }
        (_, [single]) => cover_fit(single, label, 0.35, WHITE),
        (_, many) => {
            // Collage: two or four tiles from the albums on the tape.
            let n = if many.len() >= 4 { 4 } else { 2 };
            let (cols, rows) = if n == 4 { (2.0, 2.0) } else { (2.0, 1.0) };
            let (w, h) = (label.w / cols, label.h / rows);
            for (k, tex) in many.iter().take(n).enumerate() {
                let (cx, cy) = ((k % 2) as f32, (k / 2) as f32);
                cover_fit(tex, Rect::new(label.x + cx * w, label.y + cy * h, w, h), 0.35, WHITE);
            }
        }
    }
    // Album labels follow a classic C-60 layout: a paper strip across the
    // top to write the title on, the art behind the window, and a printed
    // band at the foot with the tape's type and length.
    if !paper {
        draw_rectangle(header.x, header.y, header.w, header.h, PAPER);
        draw_rectangle(header.x, header.bottom(), header.w, 2.0, Color::from_rgba(4, 4, 5, 255));
        // Writing rule under the title.
        draw_rectangle(header.x + 52.0, header.y + 40.0, header.w - 68.0, 1.0, Color::from_rgba(150, 175, 205, 170));
        draw_rectangle(band.x, band.y - 2.0, band.w, 2.0, Color::from_rgba(4, 4, 5, 255));
        draw_rectangle(band.x, band.y, band.w, band.h, stripe);
        // Thin pinstripes, as printed bands on old labels have.
        for k in 0..3 {
            draw_rectangle(band.x, band.y + 4.0 + k as f32 * 3.0, band.w, 1.0, Color::new(0.0, 0.0, 0.0, 0.18));
        }
        let side = Rect::new(header.x + 12.0, header.y + 12.0, 30.0, 30.0);
        draw_rectangle_lines(side.x, side.y, side.w, side.h, 2.0, Color::from_rgba(28, 34, 70, 255));
    } else {
        let ink = if paper { Color::from_rgba(28, 34, 70, 255) } else { WHITE };
        draw_circle(label.x + 26.0, label.bottom() - 19.0, 11.0, ink);
    }
    // Rounded label corners: cut back to shell color.
    for (corner, start) in [
        (label.point(), PI),
        (vec2(label.right(), label.y), -FRAC_PI_2),
        (vec2(label.right(), label.bottom()), 0.0),
        (vec2(label.x, label.bottom()), FRAC_PI_2),
    ] {
        let center = corner + vec2(if corner.x > label.x { -8.0 } else { 8.0 }, if corner.y > label.y { -8.0 } else { 8.0 });
        for k in 0..6 {
            let a0 = start + FRAC_PI_2 * k as f32 / 6.0;
            let a1 = start + FRAC_PI_2 * (k + 1) as f32 / 6.0;
            draw_triangle(corner, center + vec2(a0.cos(), a0.sin()) * 8.0, center + vec2(a1.cos(), a1.sin()) * 8.0, Color::from_rgba(4, 4, 5, 255));
        }
    }

    // Window: smoked plastic over two reels and the tape between them.
    round_rect(Rect::new(win.x - 4.0, win.y - 4.0, win.w + 8.0, win.h + 8.0), 43.0, Color::from_rgba(4, 4, 5, 255));
    round_rect(win, 39.0, Color::from_rgba(16, 16, 19, 255));
    let (lc, rc) = (vec2(win.x + 50.0, win.center().y), vec2(win.right() - 50.0, win.center().y));
    let (left, right) = (19.0 + 17.0 * wound, 19.0 + 17.0 * (1.0 - wound));
    let gl = unsafe { get_internal_gl() };
    gl.quad_gl.scissor(Some((win.x as i32, win.y as i32, win.w as i32, win.h as i32)));
    window_reel(lc, left, 0.4 + wound);
    window_reel(rc, right, 1.3 - wound);
    draw_line(lc.x, lc.y + left, rc.x, rc.y + right, 2.0, Color::from_rgba(58, 40, 26, 255));
    let gl = unsafe { get_internal_gl() };
    gl.quad_gl.scissor(None);
    // Guide rollers at the window's lower corners.
    for x in [win.x + 22.0, win.right() - 22.0] {
        draw_circle(x, win.bottom() - 11.0, 4.5, Color::from_rgba(200, 200, 196, 255));
        draw_circle(x, win.bottom() - 11.0, 1.8, Color::from_rgba(60, 60, 64, 255));
    }
    // Printed minute scale between the reels, 0 to 30 each way from the middle.
    for k in -6..=6 {
        let x = win.center().x + k as f32 * 6.0;
        let tall = k % 2 == 0;
        draw_rectangle(x - 0.5, win.y + 8.0, 1.0, if tall { 7.0 } else { 4.0 }, scale);
    }
    // Glass: a soft reflection along the top and a rim.
    draw_rectangle(win.x + 30.0, win.y + 6.0, win.w - 60.0, 5.0, Color::new(1.0, 1.0, 1.0, 0.09));
    round_rect_lines(win, 39.0, 2.0, Color::new(1.0, 1.0, 1.0, 0.28));

    // Lower shell: head openings, guide holes and printed marks.
    let (top, bottom) = (FACE_H - 84.0, FACE_H - 4.0);
    let (l, r) = (96.0, FACE_W - 96.0);
    let well = Color::from_rgba(8, 8, 9, 255);
    draw_triangle(vec2(l + 24.0, top + 10.0), vec2(r - 24.0, top + 10.0), vec2(r, bottom), well);
    draw_triangle(vec2(l + 24.0, top + 10.0), vec2(r, bottom), vec2(l, bottom), well);
    draw_line(l + 24.0, top + 10.0, r - 24.0, top + 10.0, 1.5, Color::new(1.0, 1.0, 1.0, 0.16));
    for (k, w) in [(-2.0, 14.0), (-1.0, 10.0), (0.0, 30.0), (1.0, 10.0), (2.0, 14.0)] {
        let cx = FACE_W / 2.0 + k * 44.0;
        draw_rectangle(cx - w / 2.0, bottom - 22.0, w, 14.0, Color::from_rgba(0, 0, 0, 255));
        draw_rectangle(cx - w / 2.0, bottom - 22.0, w, 2.0, Color::from_rgba(48, 48, 52, 255));
    }
    for x in [FACE_W / 2.0 - 66.0, FACE_W / 2.0 + 66.0] {
        draw_circle(x, top + 34.0, 8.0, BLACK);
        ring(x, top + 34.0, 8.0, 1.5, Color::new(1.0, 1.0, 1.0, 0.18));
    }
    // Felt pressure pad behind the middle opening, on its leaf spring.
    let pad = Rect::new(FACE_W / 2.0 - 9.0, bottom - 19.0, 18.0, 8.0);
    draw_rectangle(pad.x - 4.0, pad.bottom(), pad.w + 8.0, 2.0, Color::from_rgba(150, 152, 156, 255));
    draw_rectangle(pad.x, pad.y, pad.w, pad.h, Color::from_rgba(196, 184, 150, 255));
    // Moulded grip ridges either side of the head area.
    let ridge = Color::new(1.0, 1.0, 1.0, 0.1);
    let shade = Color::new(0.0, 0.0, 0.0, 0.45);
    for side in [-1.0, 1.0] {
        for k in 0..7 {
            let x = FACE_W / 2.0 + side * (FACE_W / 2.0 - 36.0 - k as f32 * 8.0);
            draw_rectangle(x - 1.5, top + 22.0, 2.0, 40.0, shade);
            draw_rectangle(x + 0.5, top + 22.0, 1.0, 40.0, ridge);
        }
    }

    for (x, y) in [(13.0, 13.0), (FACE_W - 13.0, 13.0), (13.0, FACE_H - 13.0), (FACE_W - 13.0, FACE_H - 13.0), (FACE_W / 2.0, FACE_H - 50.0)] {
        screw(x, y, 5.5);
    }

    for pass_bold in [true, false] {
        for (text, x, y, font_size, color, bold) in &labels {
            if *bold == pass_bold {
                fonts.draw_scaled(text, *x, *y, *font_size, 1.0, *color, *bold);
            }
        }
    }

    // Gloss: a faint diagonal sheen across the whole front.
    let sheen = Color::new(1.0, 1.0, 1.0, 0.05);
    draw_triangle(vec2(60.0, 0.0), vec2(200.0, 0.0), vec2(20.0, FACE_H), sheen);
    draw_triangle(vec2(20.0, FACE_H), vec2(200.0, 0.0), vec2(-120.0, FACE_H), sheen);
    set_default_camera();
    filter_tape_texture(&target.texture);
}

pub const JCARD_SCALE: f32 = 2.0;
pub const JCARD_PAGE_SIZE: usize = 24;
const JCARD_TOP: f32 = 70.0;
const JCARD_BOTTOM: f32 = 26.0;

pub fn jcard_range(n: usize, page: usize) -> std::ops::Range<usize> {
    let start = (page * JCARD_PAGE_SIZE).min(n);
    start..(start + JCARD_PAGE_SIZE).min(n)
}

/// Track rows on the back of a tape, in face-texture pixels. Long albums
/// split into two columns.
pub fn jcard_rows(n: usize) -> Vec<Rect> {
    let columns = if n > 12 { 2 } else { 1 };
    let per_column = n.div_ceil(columns).max(1);
    let row_h = ((FACE_H - JCARD_TOP - JCARD_BOTTOM) / per_column as f32).min(24.0);
    let col_w = (FACE_W - 48.0) / columns as f32;
    (0..n)
        .map(|k| {
            let (col, row) = (k / per_column, k % per_column);
            Rect::new(24.0 + col as f32 * col_w, JCARD_TOP + row as f32 * row_h, col_w - 6.0, row_h)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jcard_pages_keep_every_track_in_a_readable_row() {
        for total in [0usize, 9, 12, 13, 24, 25, 35, 100] {
            let mut indices = Vec::new();
            for page in 0..total.div_ceil(JCARD_PAGE_SIZE) {
                let range = jcard_range(total, page);
                let rows = jcard_rows(range.len());
                assert_eq!(range.len(), rows.len());
                for (index, row) in range.zip(&rows) {
                    assert!(row.h >= 18.0 && row.bottom() <= FACE_H - JCARD_BOTTOM);
                    assert_eq!(rows.iter().filter(|r| r.contains(row.center())).count(), 1);
                    indices.push(index);
                }
            }
            assert_eq!(indices, (0..total).collect::<Vec<_>>());
        }
    }
}

/// Paints the J-card: a hand-written tracklist on ruled paper.
/// `plays` is how many times the tape was started; `hires` stamps the card.
pub fn render_jcard(target: &RenderTarget, fonts: &Fonts, album: &str, artist: &str, tracks: &[(String, String)], page: usize, plays: u32, hires: bool) {
    let mut cam = Camera2D::from_display_rect(Rect::new(0.0, 0.0, FACE_W, FACE_H));
    cam.render_target = Some(target.clone());
    set_camera(&cam);
    clear_background(Color::new(0.0, 0.0, 0.0, 0.0));

    // The back of the shell, as on the front: moulded plastic with a rim
    // and corner screws, holding the printed card.
    let full = Rect::new(0.0, 0.0, FACE_W, FACE_H);
    round_rect(full, FACE_RADIUS, Color::from_rgba(26, 26, 26, 255));
    for i in 0..8 {
        let a = 0.035 * (8 - i) as f32 / 8.0;
        draw_rectangle(FACE_RADIUS, i as f32 * 4.0, FACE_W - 2.0 * FACE_RADIUS, 4.0, Color::new(1.0, 1.0, 1.0, a));
    }
    round_rect_lines(full, FACE_RADIUS, 2.0, Color::new(1.0, 1.0, 1.0, 0.22));
    for (x, y) in [(12.0, 9.0), (FACE_W - 12.0, 9.0), (12.0, FACE_H - 9.0), (FACE_W - 12.0, FACE_H - 9.0)] {
        screw(x, y, 4.5);
    }

    let paper = Rect::new(22.0, 16.0, FACE_W - 44.0, FACE_H - 32.0);
    round_rect(Rect::new(paper.x - 2.0, paper.y - 2.0, paper.w + 4.0, paper.h + 4.0), 7.0, Color::from_rgba(6, 6, 7, 255));
    round_rect(paper, 6.0, Color::from_rgba(238, 232, 214, 255));
    paper_grain(paper, album);
    let pen = Color::from_rgba(24, 29, 51, 255);
    let red = Color::from_rgba(147, 42, 38, 255);
    let rule = Color::from_rgba(83, 106, 137, 62);
    let mut labels = Vec::new();
    let mut print = |text: &str, x, y, size: f32, color, bold| {
        labels.push((text.to_owned(), x, y, (size * JCARD_SCALE) as u16, color, bold));
    };

    print("SIDE A", paper.x + 14.0, paper.y + 23.0, 14.0, red, true);
    if plays > 0 {
        let count = format!("{plays} PLAY{}", if plays == 1 { "" } else { "S" });
        print(&count, paper.x + 14.0, paper.y + 41.0, 10.0, pen, false);
    }
    // A rubber stamp in the corner for hi-res tapes; the title makes room for it.
    let stamp_w = if hires { 74.0 } else { 0.0 };
    if hires {
        let stamp = Rect::new(paper.right() - 10.0 - stamp_w, paper.y + 10.0, stamp_w, 22.0);
        draw_rectangle_lines(stamp.x, stamp.y, stamp.w, stamp.h, 2.0, red);
        let w = fonts.width("HI-RES", 13.0, true);
        print("HI-RES", stamp.center().x - w / 2.0, stamp.y + 16.0, 13.0, red, true);
    }
    let title = fonts.fit(&album.to_uppercase(), 23.0, true, paper.w - 106.0 - stamp_w);
    print(&title, paper.x + 86.0, paper.y + 24.0, 23.0, pen, true);
    let by = fonts.fit(&artist.to_uppercase(), 14.0, true, paper.w - 106.0);
    print(&by, paper.x + 86.0, paper.y + 43.0, 14.0, pen, true);
    draw_rectangle(paper.x + 8.0, JCARD_TOP - 7.0, paper.w - 16.0, 1.5, pen);

    let range = jcard_range(tracks.len(), page);
    let rows = jcard_rows(range.len());
    for (index, row) in range.zip(rows) {
        let (name, length) = &tracks[index];
        let size = (row.h * 0.8).min(18.0).floor();
        let base = row.center().y + size * 0.32;
        let number = format!("{:02}", index + 1);
        let name_x = row.x + fonts.width(if tracks.len() < 100 { "00" } else { "000" }, size, true) + 11.0;
        let time_x = row.right() - fonts.width(length, size, false) - 4.0;
        draw_rectangle(row.x, row.bottom() - 1.0, row.w, 1.0, rule);
        draw_rectangle(name_x - 5.0, row.y, 1.0, row.h, Color::from_rgba(147, 42, 38, 85));
        print(&number, row.x + 2.0, base, size, red, true);
        let name = fonts.fit(name, size, true, time_x - name_x - 7.0);
        print(&name, name_x, base, size, pen, true);
        print(length, time_x, base, size, pen, false);
    }
    if tracks.len() > JCARD_PAGE_SIZE {
        let footer = format!("SCROLL · {}/{}", page + 1, tracks.len().div_ceil(JCARD_PAGE_SIZE));
        let x = paper.right() - 12.0 - fonts.width(&footer, 13.0, true);
        print(&footer, x, paper.bottom() - 4.0, 13.0, pen, true);
    }
    // Cache all glyphs before drawing, so atlas growth cannot move queued glyphs.
    let font_scale = 1.0 / JCARD_SCALE;
    for (text, _, _, font_size, _, bold) in &labels {
        fonts.prepare_text(text, *font_size, *bold);
    }
    for (text, x, y, font_size, color, bold) in &labels {
        fonts.draw_scaled(text, *x, *y, *font_size, font_scale, *color, *bold);
    }
    set_default_camera();
    filter_tape_texture(&target.texture);
}

/// Printed card stock: specks and short fibres in the paper, and edges a
/// shade darker where the card has aged. Stable per album.
fn paper_grain(paper: Rect, album: &str) {
    let mut seed = album.bytes().fold(0x2545_f491u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32)) | 1;
    let mut rand = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as f32 / u32::MAX as f32
    };
    for _ in 0..900 {
        let (x, y) = (paper.x + 3.0 + rand() * (paper.w - 6.0), paper.y + 3.0 + rand() * (paper.h - 6.0));
        let a = 0.05 + rand() * 0.1;
        draw_rectangle(x, y, 1.0, 1.0, Color::new(0.35, 0.28, 0.18, a));
    }
    for _ in 0..18 {
        let (x, y) = (paper.x + 6.0 + rand() * (paper.w - 70.0), paper.y + 6.0 + rand() * (paper.h - 12.0));
        draw_rectangle(x, y, 20.0 + rand() * 40.0, 1.0, Color::new(0.45, 0.38, 0.26, 0.05));
    }
    for k in 0..6 {
        let inset = k as f32 * 1.5;
        let r = Rect::new(paper.x + inset, paper.y + inset, paper.w - 2.0 * inset, paper.h - 2.0 * inset);
        round_rect_lines(r, 6.0, 1.5, Color::new(0.3, 0.22, 0.1, 0.07 * (6 - k) as f32 / 6.0));
    }
}

/// Mini cassette window whose tape packs shift from the left reel to the
/// right one as the track plays.
pub fn winding_window(win: Rect, progress: f32, angles: (f32, f32)) {
    let (left, right) = pack_radii(win.h, progress);
    let r_hub = REEL_HUB;
    let hub = Color::from_rgba(11, 11, 11, 255);
    round_rect(win, win.h / 2.0, hub);
    let (lc, rc) = (vec2(win.x + win.h * 0.62, win.center().y), vec2(win.right() - win.h * 0.62, win.center().y));
    let gl = unsafe { get_internal_gl() };
    gl.quad_gl.scissor(Some((win.x as i32, win.y as i32, win.w as i32, win.h as i32)));
    let brown = Color::from_rgba(78, 54, 34, 255);
    draw_circle(lc.x, lc.y, left, brown);
    draw_circle(rc.x, rc.y, right, brown);
    // Tape running between the packs along the bottom.
    draw_line(lc.x, lc.y + left, rc.x, rc.y + right, 1.5, brown);
    reel(lc.x, lc.y, r_hub + 1.5, angles.0, hub);
    reel(rc.x, rc.y, r_hub + 1.5, angles.1, hub);
    let gl = unsafe { get_internal_gl() };
    gl.quad_gl.scissor(None);
    round_rect_lines(win, win.h / 2.0, 1.0, Color::new(1.0, 1.0, 1.0, 0.3));
}

const REEL_HUB: f32 = 5.0;
pub const DECK_WINDOW_H: f32 = 26.0;

/// Radii of the tape packed on the supply and take-up reels. Tape length
/// goes with pack area, so radii follow a square root.
pub fn pack_radii(window_h: f32, progress: f32) -> (f32, f32) {
    let full = (window_h * 0.62).min(16.0);
    let pack = |share: f32| (REEL_HUB * REEL_HUB + (full * full - REEL_HUB * REEL_HUB) * share.clamp(0.0, 1.0)).sqrt();
    (pack(1.0 - progress), pack(progress))
}
