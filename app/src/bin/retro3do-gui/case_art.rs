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

/// `text` centred in a box, broken into lines that fit, each as wide as it
/// can be. Returns the height it used.
fn titled(img: &mut RgbaImage, x: i64, y: i64, w: i64, s: &str, scale: i64, c: Rgba<u8>) -> i64 {
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

// --- the drawn stand-ins ---------------------------------------------------

/// A cover for a game whose art is not in the folder: the title on graphite
/// over the 3DO strap, which is at least recognisably a 3DO case.
fn drawn_cover(w: u32, h: u32, title: &str, strap: &str) -> RgbaImage {
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
    let scale = (w64 / 46).max(2);
    let used = titled(&mut img, w64 / 10, h64 / 3, w64 * 8 / 10, title, scale, INK);
    fill(&mut img, w64 / 10, h64 / 3 - scale * 6, w64 * 8 / 10, scale.max(2), RULE);
    // The strap sits just above the bottom band rather than under the
    // title, which a long title would otherwise run into.
    let small = (scale / 2).max(1);
    let _ = used;
    let y = h64 - h64 / 9 - GLYPH_H * small - small * 8;
    let sx = w64 / 10 + (w64 * 8 / 10 - text_width(strap, small)) / 2;
    text(&mut img, sx, y, strap, small, Rgba([150, 152, 160, 255]), false);
    fill(&mut img, 0, h64 - h64 / 9, w64, h64 / 9, Rgba([16, 17, 20, 255]));
    let mark = "3DO INTERACTIVE MULTIPLAYER";
    let ms = (w64 / 150).max(1);
    text(
        &mut img,
        (w64 - text_width(mark, ms)) / 2,
        h64 - h64 / 9 + (h64 / 9 - GLYPH_H * ms) / 2,
        mark,
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
    // As large as the strip is wide, but stepped down until the whole title
    // fits down it: on a shelf the spine is all there is to read.
    let room = h64 - h64 / 12;
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
        .unwrap_or_else(|| drawn_cover(cover_w, h, &art.title, &strap));
    let spine = art
        .spine
        .as_deref()
        .and_then(|p| fit_in(p, spine_w, h))
        .unwrap_or_else(|| drawn_spine(spine_w, h, &art.title, spine_stock(art)));
    let front = art
        .front
        .as_deref()
        .and_then(|p| fit_in(p, cover_w, h))
        .unwrap_or_else(|| drawn_cover(cover_w, h, &art.title, &strap));

    blit(&mut sheet, &back, 0, 0);
    blit(&mut sheet, &spine, cover_w as i64, 0);
    blit(&mut sheet, &front, (cover_w + spine_w) as i64, 0);
    // The model's UVs read the sheet exactly as a sleeve is printed, so it
    // goes on as it stands: back cover, spine, front cover across it.
    to_color(&sheet)
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
