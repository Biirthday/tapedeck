//! Drawing helpers: fonts, rounded shapes, reels and the cassette face texture.
use std::f32::consts::{FRAC_PI_2, PI, TAU};
use std::process::Command;

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

pub struct Fonts {
    regular: Option<Font>,
    bold: Option<Font>,
    cjk: Option<Font>,
}

fn system_font(pattern: &str) -> Option<Font> {
    let out = Command::new("fc-match").args(["-f", "%{file}", pattern]).output().ok()?;
    let path = String::from_utf8(out.stdout).ok()?;
    let bytes = std::fs::read(path.trim()).ok()?;
    load_ttf_font_from_bytes(&bytes).ok()
}

impl Fonts {
    pub fn load() -> Self {
        Self {
            regular: system_font("JetBrains Mono:style=Regular").or_else(|| system_font("monospace")),
            bold: system_font("JetBrains Mono:style=Bold").or_else(|| system_font("monospace:bold")),
            // Many titles are Japanese or Korean.
            cjk: system_font("Noto Sans CJK JP:style=Bold"),
        }
    }

    fn pick(&self, text: &str, bold: bool) -> Option<&Font> {
        // Latin and general punctuation stay in the mono font.
        let wide = |c: char| c as u32 > 0x24F && !(0x2000..=0x206F).contains(&(c as u32));
        if text.chars().any(wide) && self.cjk.is_some() {
            return self.cjk.as_ref();
        }
        if bold { self.bold.as_ref() } else { self.regular.as_ref() }
    }

    pub fn width(&self, text: &str, size: f32, bold: bool) -> f32 {
        measure_text(text, self.pick(text, bold), size as u16, 1.0).width
    }

    /// Draws `text` with its baseline at `y`.
    pub fn draw(&self, text: &str, x: f32, y: f32, size: f32, color: Color, bold: bool) {
        draw_text_ex(text, x, y, TextParams { font: self.pick(text, bold), font_size: size as u16, color, ..Default::default() });
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
            return String::new();
        }
        buf.clear();
        buf.push_str(&text[..ends[lo - 1]]);
        buf.push('…');
        buf
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

/// Draws `tex` filling `dest`, cropped like CSS `background-size: cover`.
pub fn cover_fit(tex: &Texture2D, dest: Rect, focus_y: f32, tint: Color) {
    let (tw, th) = (tex.width(), tex.height());
    let scale = (dest.w / tw).max(dest.h / th);
    let (sw, sh) = (dest.w / scale, dest.h / scale);
    let source = Rect::new((tw - sw) / 2.0, (th - sh) * focus_y, sw, sh);
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
    let mut cam = Camera2D::from_display_rect(Rect::new(0.0, 0.0, FACE_W, FACE_H));
    cam.render_target = Some(target.clone());
    set_camera(&cam);
    clear_background(Color::new(0.0, 0.0, 0.0, 0.0));
    let paper = style == FaceStyle::Blank || (style == FaceStyle::Mix && covers.is_empty());
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
    let label = Rect::new(22.0, 18.0, FACE_W - 44.0, 214.0);
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
    let header = Rect::new(label.x, label.y, label.w, HEADER_H);
    let band = Rect::new(label.x, label.bottom() - BAND_H, label.w, BAND_H);
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

    if !paper {
        let ink = Color::from_rgba(28, 34, 70, 255);
        // Side box on the left of the strip, year on the right.
        let side = Rect::new(header.x + 12.0, header.y + 12.0, 30.0, 30.0);
        draw_rectangle_lines(side.x, side.y, side.w, side.h, 2.0, ink);
        fonts.draw("A", side.center().x - fonts.width("A", 20.0, true) / 2.0, side.y + 23.0, 20.0, ink, true);
        let yw = fonts.width(year, 13.0, false);
        fonts.draw(year, header.right() - 14.0 - yw, header.y + 17.0, 13.0, Color::from_rgba(60, 66, 100, 255), false);
        let tx = header.x + 52.0;
        let width = header.right() - 14.0 - yw - 12.0 - tx;
        let title = title.to_uppercase();
        let size = (18..=24).rev().find(|&size| fonts.width(&title, size as f32, true) <= width).unwrap_or(18) as f32;
        fonts.draw(&fonts.fit(&title, size, true, width), tx, header.y + 34.0, size, ink, true);
        let by = fonts.fit(&artist.to_uppercase(), 12.0, true, header.w - 66.0);
        fonts.draw(&by, tx, header.y + 54.0, 12.0, Color::from_rgba(60, 66, 100, 255), true);

        // Foot band: tape type and length, dark on the band's color.
        let dark = Color::new(0.0, 0.0, 0.0, 0.82);
        let base = band.y + band.h - 7.0;
        fonts.draw("C-60", band.x + 14.0, base, 17.0, dark, true);
        let x = band.x + 14.0 + fonts.width("C-60", 17.0, true) + 12.0;
        fonts.draw("60 MIN  ·  30 MIN PER SIDE", x, base - 1.0, 10.0, dark, true);
        let ty = "TYPE I  NORMAL";
        fonts.draw(ty, band.right() - 14.0 - fonts.width(ty, 10.0, true), base - 1.0, 10.0, dark, true);
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
    let title_size = (25..=32)
        .rev()
        .find(|&size| fonts.width(&title, size as f32, true) <= title_width)
        .unwrap_or(25) as f32;
    let text = fonts.fit(&title, title_size, true, title_width);
    fonts.draw(&text, tx, label.y + 37.0, title_size, ink, true);
    let by = fonts.fit(artist, 21.0, true, label.w - 32.0);
    fonts.draw(&by, tx, label.y + 62.0, 21.0, sub, true);

    // Side badge and year along the label's foot.
    let foot = label.bottom() - 14.0;
    draw_circle(tx + 10.0, foot - 5.0, 11.0, ink);
    let a_color = if paper { Color::from_rgba(236, 231, 216, 255) } else { BLACK };
    fonts.draw("A", tx + 10.0 - fonts.width("A", 15.0, true) / 2.0, foot, 15.0, a_color, true);
    let yw = fonts.width(year, 15.0, false);
    fonts.draw(year, label.right() - 16.0 - yw + 1.0, foot + 1.0, 15.0, shadow, false);
    fonts.draw(year, label.right() - 16.0 - yw, foot, 15.0, sub, false);
    }

    // Window: smoked plastic over two reels and the tape between them.
    let win = Rect::new(FACE_W / 2.0 - 132.0, 104.0, 264.0, 78.0);
    round_rect(Rect::new(win.x - 4.0, win.y - 4.0, win.w + 8.0, win.h + 8.0), 43.0, Color::from_rgba(4, 4, 5, 255));
    round_rect(win, 39.0, Color::from_rgba(16, 16, 19, 255));
    let (lc, rc) = (vec2(win.x + 50.0, win.center().y), vec2(win.right() - 50.0, win.center().y));
    let (left, right) = (19.0 + 17.0 * wound, 19.0 + 17.0 * (1.0 - wound));
    let mut gl = unsafe { get_internal_gl() };
    gl.flush();
    gl.quad_gl.scissor(Some((win.x as i32, win.y as i32, win.w as i32, win.h as i32)));
    window_reel(lc, left, 0.4 + wound);
    window_reel(rc, right, 1.3 - wound);
    draw_line(lc.x, lc.y + left, rc.x, rc.y + right, 2.0, Color::from_rgba(58, 40, 26, 255));
    let mut gl = unsafe { get_internal_gl() };
    gl.flush();
    gl.quad_gl.scissor(None);
    // Guide rollers at the window's lower corners.
    for x in [win.x + 22.0, win.right() - 22.0] {
        draw_circle(x, win.bottom() - 11.0, 4.5, Color::from_rgba(200, 200, 196, 255));
        draw_circle(x, win.bottom() - 11.0, 1.8, Color::from_rgba(60, 60, 64, 255));
    }
    // Printed minute scale between the reels, 0 to 30 each way from the middle.
    let scale = Color::new(1.0, 1.0, 1.0, 0.55);
    for k in -6..=6 {
        let x = win.center().x + k as f32 * 6.0;
        let tall = k % 2 == 0;
        draw_rectangle(x - 0.5, win.y + 8.0, 1.0, if tall { 7.0 } else { 4.0 }, scale);
    }
    for (k, label) in [(-6, "30"), (0, "0"), (6, "30")] {
        let x = win.center().x + k as f32 * 6.0 - fonts.width(label, 8.0, false) / 2.0;
        fonts.draw(label, x, win.y + 24.0, 8.0, scale, false);
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
        measure_text(text, fonts.pick(text, *bold), *font_size, font_scale);
    }
    for (text, x, y, font_size, color, bold) in &labels {
        draw_text_ex(text, *x, *y, TextParams {
            font: fonts.pick(text, *bold),
            font_size: *font_size,
            font_scale,
            color: *color,
            ..Default::default()
        });
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
    let mut gl = unsafe { get_internal_gl() };
    gl.flush();
    gl.quad_gl.scissor(Some((win.x as i32, win.y as i32, win.w as i32, win.h as i32)));
    let brown = Color::from_rgba(78, 54, 34, 255);
    draw_circle(lc.x, lc.y, left, brown);
    draw_circle(rc.x, rc.y, right, brown);
    // Tape running between the packs along the bottom.
    draw_line(lc.x, lc.y + left, rc.x, rc.y + right, 1.5, brown);
    reel(lc.x, lc.y, r_hub + 1.5, angles.0, hub);
    reel(rc.x, rc.y, r_hub + 1.5, angles.1, hub);
    let mut gl = unsafe { get_internal_gl() };
    gl.flush();
    gl.quad_gl.scissor(None);
    round_rect_lines(win, win.h / 2.0, 1.0, Color::new(1.0, 1.0, 1.0, 0.3));
}

const REEL_HUB: f32 = 5.0;
pub const DECK_WINDOW_H: f32 = 24.0;

/// Radii of the tape packed on the supply and take-up reels. Tape length
/// goes with pack area, so radii follow a square root.
pub fn pack_radii(window_h: f32, progress: f32) -> (f32, f32) {
    let full = (window_h * 0.62).min(16.0);
    let pack = |share: f32| (REEL_HUB * REEL_HUB + (full * full - REEL_HUB * REEL_HUB) * share.clamp(0.0, 1.0)).sqrt();
    (pack(1.0 - progress), pack(progress))
}
