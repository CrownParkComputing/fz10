//! The real boot logo, taken out of the owner's own BIOS dump: the core runs
//! the ROM headless for a few seconds of emulated time and the framebuffer it
//! produces — the logo card, then the "REAL / 3DO INTERACTIVE MULTIPLAYER"
//! splash the machine shows on power-up — is the artwork. The finished frame
//! is cached beside the settings so this only runs once per BIOS file.

use std::path::{Path, PathBuf};

use egui::ColorImage;
use retro3do::Console;

/// The window of emulated fields in which the boot splash is on screen. The
/// FZ-10 shows it around field 900; slower or faster ROMs are covered by
/// sampling the whole window and keeping the frame with the most pixels lit.
const SCAN_FIRST: u32 = 800;
const SCAN_LAST: u32 = 1200;
const SCAN_STEP: u32 = 50;

fn cache_path(bios: &Path) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let stem = bios
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("bios")
        .to_string();
    PathBuf::from(format!("{home}/.config/retro3do/bootlogo-{stem}.png"))
}

fn from_cache(bios: &Path) -> Option<ColorImage> {
    let img = image::open(cache_path(bios)).ok()?.to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    Some(ColorImage::from_rgba_unmultiplied([w, h], img.as_raw()))
}

fn to_cache(bios: &Path, img: &ColorImage) {
    let path = cache_path(bios);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = image::save_buffer(
        &path,
        img.as_raw(),
        img.size[0] as u32,
        img.size[1] as u32,
        image::ColorType::Rgba8,
    );
}

fn lit_fraction(console: &Console) -> usize {
    console
        .framebuffer()
        .pixels
        .iter()
        .filter(|&&p| p & 0x00ff_ffff != 0)
        .count()
}

fn embedded(bytes: &[u8]) -> ColorImage {
    let Ok(img) = image::load_from_memory(bytes) else {
        return ColorImage::from_rgba_unmultiplied([1, 1], &[0, 0, 0, 0]);
    };
    let img = img.to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    ColorImage::from_rgba_unmultiplied([w, h], img.as_raw())
}

/// The recreated boot card — the badge, the chrome R·E·A·L, the caption and
/// the Panasonic line, drawn from docs/boot-logo-reference.png and baked in
/// (docs/make-boot-logo.py). Always there, BIOS or not; the extracted frame
/// replaces it when the ROM hands one over.
pub fn render() -> ColorImage {
    embedded(include_bytes!("../../../../docs/3do-logo-hires.png"))
}

/// The 3DO badge alone — the mark the machine view's wallpaper is tiled with.
pub fn badge() -> ColorImage {
    embedded(include_bytes!("../../../assets/cpc-mark.png"))
}

/// Boot the BIOS in a headless console and keep the brightest frame in the
/// splash window. Slow, so it runs on a worker thread and its result is
/// cached; a `None` means the ROM would not boot, which is not fatal.
pub fn extract(bios: &Path) -> Option<ColorImage> {
    if let Some(img) = from_cache(bios) {
        return Some(img);
    }
    let mut console = Console::new();
    if !console.load_bios(&bios.to_string_lossy()) {
        return None;
    }
    console.reset();
    let mut best: Option<(usize, Vec<u32>, usize, usize)> = None;
    for field in 0..=SCAN_LAST {
        console.run_frame();
        if field < SCAN_FIRST || field % SCAN_STEP != 0 {
            continue;
        }
        let frame = console.framebuffer();
        let lit = lit_fraction(&console);
        let score = lit * 1000 / frame.pixels.len().max(1);
        if best.as_ref().is_none_or(|(s, ..)| score > *s) {
            best = Some((score, frame.pixels.to_vec(), frame.width, frame.height));
        }
    }
    let (_, pixels, w, h) = best?;
    let rgba: Vec<u8> = pixels
        .iter()
        .flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8, 0xff])
        .collect();
    let img = ColorImage::from_rgba_unmultiplied([w, h], &rgba);
    to_cache(bios, &img);
    Some(img)
}
