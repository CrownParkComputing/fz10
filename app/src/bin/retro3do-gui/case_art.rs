//! The artwork one game's case wears: the printed sleeve and the disc that
//! lies beside it, composited from whatever the media folder has.
//!
//! A real DVD sleeve is one sheet — back cover, spine, front cover across
//! its width — and that is exactly how the case model's `Wrap` is mapped,
//! so the three pictures are laid into one canvas here. Anything the folder
//! is missing is drawn instead, so every game in the library gets a case:
//! a graphite cover with the title on it, a spine with the title down it,
//! and a plain silver platter for a disc.
//!
//! All of it runs on the artwork worker thread, so nothing here may touch
//! egui or the GPU: the canvases go back as plain images.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use egui::ColorImage;
use image::{imageops, Rgba, RgbaImage};

use super::fz1_3d::{WRAP_TEX_H, WRAP_TEX_W};

/// The platter canvas, which has to match the model's disc texture.
pub const DISC_TEX: u32 = 1024;

/// Where the sleeve's three panels sit across the wrap, in millimetres of
/// the real 284 mm sheet: two 135 mm covers either side of a 14 mm spine.
const COVER_MM: f32 = 135.0;
const SPINE_MM: f32 = 14.0;

/// The pictures a game's case is to be built from.
#[derive(Clone, Default)]
pub struct CaseArt {
    pub title: String,
    pub front: Option<PathBuf>,
    pub back: Option<PathBuf>,
    pub spine: Option<PathBuf>,
    pub disc: Option<PathBuf>,
    /// How many discs the title ships on, for the generated panels.
    pub discs: usize,
}

// --- small drawing primitives ---------------------------------------------

const PAPER: Rgba<u8> = Rgba([26, 27, 31, 255]);
const INK: Rgba<u8> = Rgba([228, 229, 233, 255]);
const RULE: Rgba<u8> = Rgba([188, 36, 44, 255]);
const SILVER: Rgba<u8> = Rgba([126, 130, 140, 255]);

fn fill(img: &mut RgbaImage, x0: i64, y0: i64, w: i64, h: i64, c: Rgba<u8>) {
    for y in y0.max(0)..(y0 + h).min(img.height() as i64) {
        for x in x0.max(0)..(x0 + w).min(img.width() as i64) {
            img.put_pixel(x as u32, y as u32, c);
        }
    }
}

/// A 5x7 stencil per character: the launcher has no font to hand on the
/// worker thread, and a title set in a few hundred bytes of bitmap is
/// enough for a cover nobody shipped art for.
const GLYPH_W: i64 = 5;
const GLYPH_H: i64 = 7;

#[rustfmt::skip]
const FONT: [(char, [u8; 7]); 45] = [
    ('A', [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001]),
    ('B', [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110]),
    ('C', [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110]),
    ('D', [0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110]),
    ('E', [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111]),
    ('F', [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000]),
    ('G', [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01110]),
    ('H', [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001]),
    ('I', [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b11111]),
    ('J', [0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100]),
    ('K', [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001]),
    ('L', [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111]),
    ('M', [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001]),
    ('N', [0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001]),
    ('O', [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110]),
    ('P', [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000]),
    ('Q', [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10011, 0b01111]),
    ('R', [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001]),
    ('S', [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110]),
    ('T', [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100]),
    ('U', [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110]),
    ('V', [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100]),
    ('W', [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001]),
    ('X', [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001]),
    ('Y', [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100]),
    ('Z', [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111]),
    ('0', [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110]),
    ('1', [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110]),
    ('2', [0b01110, 0b10001, 0b00001, 0b00110, 0b01000, 0b10000, 0b11111]),
    ('3', [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110]),
    ('4', [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010]),
    ('5', [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110]),
    ('6', [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110]),
    ('7', [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000]),
    ('8', [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110]),
    ('9', [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100]),
    ('-', [0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000]),
    ('.', [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100]),
    (',', [0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b00100, 0b01000]),
    (':', [0b00000, 0b01100, 0b01100, 0b00000, 0b01100, 0b01100, 0b00000]),
    ('!', [0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00000, 0b00100]),
    ('?', [0b01110, 0b10001, 0b00001, 0b00110, 0b00100, 0b00000, 0b00100]),
    ('\'', [0b00100, 0b00100, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000]),
    ('&', [0b01100, 0b10010, 0b10100, 0b01000, 0b10101, 0b10010, 0b01101]),
    ('/', [0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000]),
];

fn glyph(c: char) -> Option<[u8; 7]> {
    let c = c.to_ascii_uppercase();
    FONT.iter().find(|(g, _)| *g == c).map(|(_, bits)| *bits)
}

/// Width of `text` set at `scale`, in pixels, including the gaps.
fn text_width(text: &str, scale: i64) -> i64 {
    text.chars().count() as i64 * (GLYPH_W + 1) * scale
}

/// Set `text` with its top-left at (x, y). `down` turns it a quarter turn
/// clockwise, which is how a spine is read.
fn text(img: &mut RgbaImage, x: i64, y: i64, s: &str, scale: i64, c: Rgba<u8>, down: bool) {
    let mut pen = 0i64;
    for ch in s.chars() {
        if let Some(bits) = glyph(ch) {
            for (row, line) in bits.iter().enumerate() {
                for col in 0..GLYPH_W {
                    if line >> (GLYPH_W - 1 - col) & 1 == 0 {
                        continue;
                    }
                    let (gx, gy) = (pen + col, row as i64);
                    let (dx, dy) = if down { (-gy, gx) } else { (gx, gy) };
                    fill(img, x + dx * scale, y + dy * scale, scale, scale, c);
                }
            }
        }
        pen += GLYPH_W + 1;
    }
}

/// `s` broken into the lines that fit a box `w` wide at `scale`.
fn wrap_lines(s: &str, w: i64, scale: i64) -> Vec<String> {
    let per_line = (w / ((GLYPH_W + 1) * scale)).max(1) as usize;
    let mut lines: Vec<String> = Vec::new();
    for word in s.split_whitespace() {
        match lines.last_mut() {
            Some(line) if line.chars().count() + 1 + word.chars().count() <= per_line => {
                line.push(' ');
                line.push_str(word);
            }
            _ => lines.push(word.chars().take(per_line).collect()),
        }
    }
    lines
}

/// The largest scale `s` can be set at and still fit a `w` x `h` box: a
/// long title is printed smaller rather than run off its panel.
fn fit_scale(s: &str, w: i64, h: i64, max: i64) -> i64 {
    (1..=max.max(1))
        .rev()
        .find(|&scale| {
            let lines = wrap_lines(s, w, scale);
            lines.len() as i64 * (GLYPH_H + 3) * scale <= h
        })
        .unwrap_or(1)
}

/// `text` centred in a box, broken into lines that fit, each as wide as it
/// can be. Returns the height it used.
fn titled(img: &mut RgbaImage, x: i64, y: i64, w: i64, s: &str, scale: i64, c: Rgba<u8>) -> i64 {
    let lines = wrap_lines(s, w, scale);
    let step = (GLYPH_H + 3) * scale;
    for (i, line) in lines.iter().enumerate() {
        let lx = x + (w - text_width(line, scale)) / 2;
        text(img, lx, y + i as i64 * step, line, scale, c, false);
    }
    lines.len() as i64 * step
}

/// A picture scaled to cover `w` x `h` and centre-cropped, so box art fills
/// its panel rather than sitting in a letterbox.
fn cover_fit(path: &Path, w: u32, h: u32) -> Option<RgbaImage> {
    let src = image::open(path).ok()?.to_rgba8();
    if src.width() == 0 || src.height() == 0 {
        return None;
    }
    let scale = (w as f32 / src.width() as f32).max(h as f32 / src.height() as f32);
    let (sw, sh) = (
        ((src.width() as f32 * scale).ceil() as u32).max(w),
        ((src.height() as f32 * scale).ceil() as u32).max(h),
    );
    let scaled = imageops::resize(&src, sw, sh, imageops::FilterType::Lanczos3);
    Some(imageops::crop_imm(&scaled, (sw - w) / 2, (sh - h) / 2, w, h).to_image())
}

/// A picture scaled to fit inside `w` x `h` and padded to fill it, so the
/// whole cover lands on its panel instead of being cropped at the edges.
fn fit_in(path: &Path, w: u32, h: u32) -> Option<RgbaImage> {
    let src = image::open(path).ok()?.to_rgba8();
    if src.width() == 0 || src.height() == 0 {
        return None;
    }
    let scale = (w as f32 / src.width() as f32).min(h as f32 / src.height() as f32);
    let sw = ((src.width() as f32 * scale).round() as u32).clamp(1, w);
    let sh = ((src.height() as f32 * scale).round() as u32).clamp(1, h);
    let scaled = imageops::resize(&src, sw, sh, imageops::FilterType::Lanczos3);
    let mut img = RgbaImage::from_pixel(w, h, PAPER);
    imageops::overlay(&mut img, &scaled, ((w - sw) / 2) as i64, ((h - sh) / 2) as i64);
    Some(img)
}

fn blit(dst: &mut RgbaImage, src: &RgbaImage, x: i64, y: i64) {
    imageops::overlay(dst, src, x, y);
}


/// The 3DO mark, decoded once: the launcher's own artwork, so a title with
/// no scan still gets a case that is recognisably a 3DO release rather than
/// a grey rectangle.
fn mark() -> Option<&'static RgbaImage> {
    static MARK: OnceLock<Option<RgbaImage>> = OnceLock::new();
    MARK.get_or_init(|| {
        image::load_from_memory(include_bytes!("../../../assets/3do-mark.png"))
            .ok()
            .map(|img| img.to_rgba8())
    })
    .as_ref()
}

/// The mark laid in at `w` wide, keeping its aspect, tinted to `ink` so it
/// sits on the panel's own card rather than glaring off it. `x`, `y` are the
/// top-left of the box it is drawn into.
fn blit_mark(dst: &mut RgbaImage, x: i64, y: i64, w: u32, ink: Rgba<u8>) -> i64 {
    let Some(src) = mark() else {
        return 0;
    };
    if src.width() == 0 || w == 0 {
        return 0;
    }
    let h = ((w as f32) * src.height() as f32 / src.width() as f32).round() as u32;
    let scaled = imageops::resize(src, w.max(1), h.max(1), imageops::FilterType::Lanczos3);
    // The mark goes on as a stencil rather than a picture: its own
    // brightness is what decides how much ink lands, so it sits on the
    // sleeve's card whether the file carries an alpha channel or a solid
    // background behind the lettering.
    let mut tinted = scaled;
    for px in tinted.pixels_mut() {
        let luma = (px[0] as u32 * 299 + px[1] as u32 * 587 + px[2] as u32 * 114) / 1000;
        let a = (luma * px[3] as u32 / 255) as u8;
        *px = Rgba([ink[0], ink[1], ink[2], a]);
    }
    imageops::overlay(dst, &tinted, x, y);
    h as i64
}

// --- the drawn stand-ins ---------------------------------------------------

/// A cover for a game whose art is not in the folder: the 3DO mark over
/// graphite with the title under it, cut the way the FZ-1 retail sleeves
/// were — a red rule across the top, the strap and the black foot band. The
/// back carries the same furniture with a blurb block and a barcode, so a
/// case with no scans still reads as a boxed 3DO game from either side.
fn drawn_cover(w: u32, h: u32, title: &str, strap: &str, front: bool) -> RgbaImage {
    let title = clean_title(title);
    let mut img = RgbaImage::from_pixel(w, h, PAPER);
    let (w64, h64) = (w as i64, h as i64);
    // A little shading so the panel is not a flat block.
    for y in 0..h {
        let k = 1.0 - 0.35 * (y as f32 / h as f32);
        let shade = Rgba([
            (PAPER[0] as f32 * k) as u8 + 8,
            (PAPER[1] as f32 * k) as u8 + 8,
            (PAPER[2] as f32 * k) as u8 + 12,
            255,
        ]);
        fill(&mut img, 0, y as i64, w64, 1, shade);
    }
    let band = h64 / 9;
    let rule = (h64 / 220).max(2);
    // The red rule across the head of the sleeve.
    fill(&mut img, 0, band / 3, w64, rule, RULE);
    let (col, col_w) = (w64 / 10, w64 * 8 / 10);
    // The strap's own line, which the panels keep clear.
    let strap_scale = (w64 / 90).max(1);
    let strap_y = h64 - band - GLYPH_H * strap_scale - h64 / 40;
    if front {
        // The mark is the cover: centred, high on the card, with the title
        // under it in whatever size the title needs to fit the room left.
        let mark_w = (w64 * 34 / 100) as u32;
        let mark_h = blit_mark(&mut img, (w64 - mark_w as i64) / 2, h64 / 10, mark_w, INK);
        let ty = h64 / 10 + mark_h + h64 / 12;
        let room = strap_y - ty - h64 / 30;
        let scale = fit_scale(&title, col_w, room, (w64 / 46).max(2));
        fill(&mut img, col, ty - h64 / 24, col_w, rule, RULE);
        titled(&mut img, col, ty, col_w, &title, scale, INK);
    } else {
        // The back: the title at the head, a blurb block under it, the mark
        // low on the left and the barcode low on the right.
        let head = h64 / 4 - band;
        let scale = fit_scale(&title, col_w, head, (w64 / 70).max(2));
        titled(&mut img, col, band, col_w, &title, scale, INK);
        let muted = Rgba([96, 98, 106, 255]);
        let line_h = (h64 / 150).max(2);
        let mut y = h64 / 4;
        for i in 0..9 {
            let len = col_w - (i % 3) * w64 / 9;
            fill(&mut img, col, y, len, line_h, muted);
            y += line_h * 4;
        }
        // The barcode: white card, black bars, as every retail back has.
        let (bw, bh) = (w64 * 26 / 100, h64 / 14);
        let (bx, by) = (w64 - col - bw, strap_y - bh - h64 / 30);
        fill(&mut img, bx, by, bw, bh, Rgba([235, 235, 235, 255]));
        let mut x = bx + bw / 20;
        let mut seed = 0x9e37u32;
        while x < bx + bw - bw / 20 {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let bar = ((seed >> 16) % 3 + 1) as i64 * (bw / 60).max(1);
            fill(&mut img, x, by + bh / 10, bar, bh * 8 / 10, Rgba([18, 18, 20, 255]));
            x += bar + (bw / 50).max(2);
        }
        // The mark sits above the barcode's line, so neither it nor the
        // strap below has anything printed over it.
        let mark_w = (w64 * 16 / 100) as u32;
        let mark_h = ((mark_w as i64) * 3) / 2;
        blit_mark(&mut img, col, by - h64 / 40 - mark_h, mark_w, INK);
    }
    // The strap has a line of its own above the foot band, so no title and
    // no barcode can run into it.
    let sx = col + (col_w - text_width(strap, strap_scale)) / 2;
    text(
        &mut img,
        sx,
        strap_y,
        strap,
        strap_scale,
        Rgba([150, 152, 160, 255]),
        false,
    );
    fill(&mut img, 0, h64 - band, w64, band, Rgba([16, 17, 20, 255]));
    let mark_text = "3DO INTERACTIVE MULTIPLAYER";
    let ms = (w64 / 150).max(1);
    text(
        &mut img,
        (w64 - text_width(mark_text, ms)) / 2,
        h64 - band + (band - GLYPH_H * ms) / 2,
        mark_text,
        ms,
        Rgba([205, 207, 214, 255]),
        false,
    );
    img
}

/// The stock a generated spine is printed on. A shelf of drawn spines all
/// in one grey reads as a wall, so the title picks its own card from a few
/// deep ones — the same card every run, since it comes from the name.
const SPINE_PAPERS: [Rgba<u8>; 6] = [
    Rgba([20, 21, 24, 255]),
    Rgba([27, 20, 22, 255]),
    Rgba([17, 24, 29, 255]),
    Rgba([25, 23, 16, 255]),
    Rgba([23, 18, 29, 255]),
    Rgba([16, 25, 22, 255]),
];

pub fn spine_paper(title: &str) -> Rgba<u8> {
    let mut hash: u32 = 2_166_136_261;
    for byte in title.bytes() {
        hash = (hash ^ byte as u32).wrapping_mul(16_777_619);
    }
    SPINE_PAPERS
        .get(hash as usize % SPINE_PAPERS.len())
        .copied()
        .unwrap_or(Rgba([38, 39, 46, 255]))
}

/// A quiet colour for a spine that has no scan: the cover's own average,
/// taken down to shelf-card depth so the light title still reads on it.
fn cover_paper(path: &Path) -> Option<Rgba<u8>> {
    let src = image::open(path).ok()?.to_rgba8();
    let small = imageops::resize(&src, 8, 8, imageops::FilterType::Triangle);
    let (mut r, mut g, mut b) = (0u64, 0u64, 0u64);
    for px in small.pixels() {
        r += u64::from(px[0]);
        g += u64::from(px[1]);
        b += u64::from(px[2]);
    }
    let n = small.pixels().count().max(1) as u64;
    let (r, g, b) = (r / n, g / n, b / n);
    Some(Rgba([
        (r / 4).clamp(14, 90) as u8,
        (g / 4).clamp(14, 90) as u8,
        (b / 4).clamp(14, 90) as u8,
        255,
    ]))
}

/// The stock a generated spine is printed on: the cover's own colour where
/// there is a cover, a quiet card the title picks where there is not.
fn spine_stock(art: &CaseArt) -> Rgba<u8> {
    art.front
        .as_deref()
        .and_then(cover_paper)
        .unwrap_or_else(|| spine_paper(&art.title))
}

/// The title as a person reads it, not as the file is named: "(USA)",
/// "(En,Fr)", "(RE1)", "(Disc 2 of 3)" and their bracketed kin are the
/// scraper's business, not the game's name.
pub fn clean_title(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut depth = 0i32;
    for ch in title.chars() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            _ => out.push(ch),
        }
    }
    out.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(|c: char| "-–—·:,".contains(c))
        .trim()
        .to_string()
}

/// A spine with the title read top to bottom, as a shelf shows it.
fn drawn_spine(w: u32, h: u32, title: &str, paper: Rgba<u8>) -> RgbaImage {
    let title = clean_title(title);
    let title = title.as_str();
    let mut img = RgbaImage::from_pixel(w, h, paper);
    let (w64, h64) = (w as i64, h as i64);
    fill(&mut img, 0, 0, w64, (h64 / 90).max(2), RULE);
    fill(&mut img, 0, h64 - (h64 / 90).max(2), w64, (h64 / 90).max(2), RULE);
    // The mark at both ends, as a retail spine carries its publisher's: the
    // shelf then reads as a row of 3DO releases however the titles run.
    let mark_w = (w64 * 76 / 100).max(1) as u32;
    let pad = h64 / 44;
    let mark_h = blit_mark(&mut img, (w64 - mark_w as i64) / 2, pad, mark_w, INK);
    blit_mark(
        &mut img,
        (w64 - mark_w as i64) / 2,
        h64 - pad - mark_h,
        mark_w,
        INK,
    );
    // The title lives between the two marks.
    let head = pad * 2 + mark_h;
    let room = h64 - head * 2;
    let fits = (room / text_width(title, 1).max(1)).max(1);
    let scale = ((w64 - 4) / GLYPH_H).max(1).min(fits);
    let cut = (room / ((GLYPH_W + 1) * scale)).max(1) as usize;
    let short: String = title.chars().take(cut).collect();
    let span = text_width(&short, scale);
    // Turned a quarter: the pen runs down the strip, the rows across it.
    text(
        &mut img,
        w64 - (w64 - GLYPH_H * scale) / 2,
        (h64 - span) / 2,
        &short,
        scale,
        INK,
        true,
    );
    img
}

/// A plain 3DO platter for a game with no disc art: silver, with the title
/// around the top of the label and the hub left clear.
fn drawn_disc(title: &str) -> RgbaImage {
    let size = DISC_TEX;
    let mut img = RgbaImage::from_pixel(size, size, Rgba([0, 0, 0, 0]));
    let c = size as f32 / 2.0;
    for y in 0..size {
        for x in 0..size {
            let (dx, dy) = (x as f32 - c, y as f32 - c);
            let r = (dx * dx + dy * dy).sqrt() / c;
            if r > 1.0 {
                continue;
            }
            // Pressed data rings, faint, plus a touch of sheen up top.
            let ring = ((r * 190.0).sin() * 0.5 + 0.5) * 9.0;
            let sheen = (1.0 - (dy / c + 0.3).abs()).max(0.0) * 18.0;
            let k = |v: u8| (v as f32 + ring + sheen).min(255.0) as u8;
            let px = if r < 0.18 {
                Rgba([44, 45, 50, 255])
            } else {
                Rgba([k(SILVER[0]), k(SILVER[1]), k(SILVER[2]), 255])
            };
            img.put_pixel(x, y, px);
        }
    }
    let w = size as i64;
    titled(&mut img, w / 6, w / 8, w * 4 / 6, title, (w / 90).max(2), Rgba([235, 236, 240, 255]));
    let mark = "3DO";
    let ms = (w / 70).max(2);
    text(
        &mut img,
        (w - text_width(mark, ms)) / 2,
        w * 3 / 4,
        mark,
        ms,
        Rgba([235, 236, 240, 255]),
        false,
    );
    img
}

// --- the two canvases the renderer wants ----------------------------------

fn to_color(img: &RgbaImage) -> ColorImage {
    ColorImage::from_rgba_unmultiplied(
        [img.width() as usize, img.height() as usize],
        img.as_raw(),
    )
}

/// The printed sleeve: back cover, spine and front cover across one sheet,
/// laid out the way the model's UVs read it.
pub fn wrap(art: &CaseArt) -> ColorImage {
    let (w, h) = (WRAP_TEX_W, WRAP_TEX_H);
    let mut sheet = RgbaImage::from_pixel(w, h, PAPER);
    let per_mm = w as f32 / (COVER_MM * 2.0 + SPINE_MM);
    let cover_w = (COVER_MM * per_mm).round() as u32;
    let spine_w = w - cover_w * 2;
    let strap = if art.discs > 1 {
        format!("{} DISCS", art.discs)
    } else {
        "ONE DISC".to_string()
    };

    let back = art
        .back
        .as_deref()
        .and_then(|p| fit_in(p, cover_w, h))
        .unwrap_or_else(|| drawn_cover(cover_w, h, &art.title, &strap, false));
    let spine = art
        .spine
        .as_deref()
        .and_then(|p| fit_in(p, spine_w, h))
        .unwrap_or_else(|| drawn_spine(spine_w, h, &art.title, spine_stock(art)));
    let front = art
        .front
        .as_deref()
        .and_then(|p| fit_in(p, cover_w, h))
        .unwrap_or_else(|| drawn_cover(cover_w, h, &art.title, &strap, true));

    blit(&mut sheet, &back, 0, 0);
    blit(&mut sheet, &spine, cover_w as i64, 0);
    blit(&mut sheet, &front, (cover_w + spine_w) as i64, 0);
    // The model's UVs read the sheet exactly as a sleeve is printed, so it
    // goes on as it stands: back cover, spine, front cover across it.
    to_color(&sheet)
}

/// A whole spine for a title the media folder has no scan of, as the shelf
/// wants it: the generic 3DO card, the mark at both ends and the title down
/// the middle. The shelf asks for this once per title and keeps the texture.
pub fn generic_spine(w: u32, h: u32, title: &str) -> ColorImage {
    to_color(&drawn_spine(w, h, title, spine_paper(title)))
}

/// The label on the disc beside the case.
pub fn disc(art: &CaseArt) -> ColorImage {
    let img = art
        .disc
        .as_deref()
        .and_then(|p| cover_fit(p, DISC_TEX, DISC_TEX))
        .unwrap_or_else(|| drawn_disc(&art.title));
    to_color(&img)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn art() -> CaseArt {
        CaseArt {
            title: "Alone in the Dark".to_string(),
            discs: 2,
            ..Default::default()
        }
    }

    #[test]
    fn a_game_with_no_artwork_still_gets_a_full_sleeve() {
        let sheet = wrap(&art());
        assert_eq!(sheet.size, [WRAP_TEX_W as usize, WRAP_TEX_H as usize]);
        // Drawn, not blank: the title and the strap put ink on the sheet.
        let lit = sheet
            .pixels
            .iter()
            .filter(|p| p.r() > 200 && p.g() > 200)
            .count();
        assert!(lit > 500, "nothing was drawn on the sleeve ({lit} px)");
    }

    #[test]
    fn the_stand_in_disc_is_a_round_platter_on_a_clear_square() {
        let platter = disc(&art());
        assert_eq!(platter.size, [DISC_TEX as usize, DISC_TEX as usize]);
        let at = |x: usize, y: usize| {
            platter
                .pixels
                .get(y * DISC_TEX as usize + x)
                .copied()
                .unwrap_or_default()
        };
        assert_eq!(at(2, 2).a(), 0, "the corner of the canvas is off the disc");
        assert!(at(DISC_TEX as usize / 2, 40).a() > 0, "the rim is missing");
    }

    #[test]
    fn the_spine_is_set_down_the_strip_and_fits_it() {
        let strip = drawn_spine(
            60,
            1140,
            "Shockwave Operation Jumpgate",
            Rgba([30, 30, 34, 255]),
        );
        assert_eq!((strip.width(), strip.height()), (60, 1140));
    }

    #[test]
    fn the_generic_spine_carries_the_3do_mark() {
        let strip = generic_spine(128, 1890, "Star Control II");
        assert_eq!(strip.size, [128, 1890]);
        let at = |x: usize, y: usize| {
            strip.pixels.get(y * 128 + x).copied().unwrap_or_default()
        };
        // The mark is laid in at both ends, so both have light ink on them.
        let lit = |band: std::ops::Range<usize>| {
            band.flat_map(|y| (0..128).map(move |x| at(x, y)))
                .filter(|p| p.r() > 180)
                .count()
        };
        assert!(lit(20..120) > 100, "no mark at the head of the spine");
        assert!(lit(1770..1870) > 100, "no mark at the foot of the spine");
    }

    #[test]
    fn the_drawn_back_is_not_the_drawn_front() {
        let front = drawn_cover(420, 640, "Killing Time", "ONE DISC", true);
        let back = drawn_cover(420, 640, "Killing Time", "ONE DISC", false);
        assert_ne!(front.as_raw(), back.as_raw());
        // The back's barcode is a white block low on the right.
        let barcode = |img: &RgbaImage| {
            // The barcode's own box, as the back lays it out.
            (498..543)
                .flat_map(|y| (269..378).map(move |x| (x, y)))
                .filter(|&(x, y)| {
                    let p = img.get_pixel(x, y);
                    p[0] > 220 && p[1] > 220 && p[2] > 220
                })
                .count()
        };
        assert!(barcode(&back) > 500, "the back has no barcode on it");
    }

    #[test]
    fn a_spine_title_drops_the_scrapers_tags() {
        assert_eq!(clean_title("Killing Time (USA) (RE1)"), "Killing Time");
        assert_eq!(
            clean_title("Road & Track Presents - The Need for Speed (USA, Europe)"),
            "Road & Track Presents - The Need for Speed"
        );
        assert_eq!(
            clean_title("Flashback - The Quest for Identity (USA) (En,Fr)"),
            "Flashback - The Quest for Identity"
        );
        assert_eq!(
            clean_title("Wing Commander III (USA, Europe) (Disc 2 of 4)"),
            "Wing Commander III"
        );
    }
}

