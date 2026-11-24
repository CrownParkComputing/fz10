//! Texture packs: hand-authored replacements for a cel's source image,
//! picked by the decoded content, not by address — the same sprite is the
//! same texture wherever it sits in memory.
//!
//! A pack is a directory of `<fnv>.png` files, where `fnv` is the 16-hex-digit
//! FNV-1a of the cel's decoded source: `(w, h)` as little-endian u32s, then
//! every texel as a little-endian u16 colour plus one transparency byte. Any
//! tool that can produce that pixel stream can name a replacement for it;
//! `tools/build_texpack.py` does it for sprite-art directories.
//!
//! The replacement is a PNG whose dimensions are an integer multiple of the
//! source's. It stands in for the filtered patch in the hi-res pass, sampled
//! at its own cells-per-texel density so detail survives whatever internal
//! scale is running.
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// One ready-to-sample replacement image in patch-cell layout.
#[derive(Clone, Debug)]
pub struct PackTexture {
    /// Cells per source texel horizontally/vertically (the image's own
    /// density, e.g. an 8x sprite over a 32x70 source has `k = 8`).
    pub k: u32,
    /// `source width * k`.
    pub width: u32,
    /// Row-major cells: bit 31 set for drawable, low 16 bits the 15-bit
    /// colour (matching [`crate::madam`]'s patch layout).
    pub cells: Vec<u32>,
}

/// A directory of `<fnv>.png` replacements, decoded lazily and cached.
#[derive(Debug, Default)]
pub struct TexturePack {
    files: HashMap<u64, PathBuf>,
    /// Decoded files, and the textures built from them for the source size
    /// that asked. Decoding is the expensive half, so it is cached on its
    /// own and can be done ahead of play by [`TexturePack::warm`].
    images: Mutex<HashMap<u64, Option<Arc<Decoded>>>>,
    cache: Mutex<HashMap<u64, Option<Arc<PackTexture>>>>,
    /// Replacements actually served / requested — the pack's only observable
    /// proof that its fingerprints match the console's.
    hits: AtomicUsize,
    misses: AtomicUsize,
}

/// The FNV-1a fingerprint of a decoded source image: dimensions then, per
/// texel, the 15-bit colour LE (position bit dropped) then one transparency
/// byte.
pub fn source_fnv(width: u32, height: u32, texel: impl Fn(u32, u32) -> (u16, bool)) -> u64 {
    let mut hash = 14695981039346656037u64;
    let mut byte = |b: u8| {
        hash = (hash ^ u64::from(b)).wrapping_mul(1099511628211);
    };
    for value in [width, height] {
        for shift in [0, 8, 16, 24] {
            byte((value >> shift) as u8);
        }
    }
    for y in 0..height {
        for x in 0..width {
            let (color, transparent) = texel(x, y);
            // The position bit is presentation state, not content: the same
            // sprite must fingerprint identically wherever it lands.
            // A transparent texel's colour is presentation too — the dump
            // writes it as zero — so the fingerprint must be zero as well,
            // or a pack built from that dump never matches the console that
            // wrote it.
            let color = if transparent { 0 } else { color & 0x7fff };
            byte((color & 0xff) as u8);
            byte((color >> 8) as u8);
            byte(transparent as u8);
        }
    }
    hash
}

/// Write the decoded source image as an RGBA PNG — for building packs:
/// dump the console's own fingerprints, then pair each dump with art.
pub fn write_source_png(
    path: &Path,
    width: u32,
    height: u32,
    texel: impl Fn(u32, u32) -> (u16, bool),
) -> bool {
    let Ok(file) = std::fs::File::create(path) else { return false };
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let Ok(mut writer) = encoder.write_header() else {
        return false;
    };
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let (color, transparent) = texel(x, y);
            if transparent {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                // 5-bit channels widened the usual way.
                rgba.extend_from_slice(&[
                    ((color >> 10) as u8) << 3 | ((color >> 10) as u8) >> 2,
                    ((color >> 5) as u8 & 0x1f) << 3 | ((color >> 5) as u8 & 0x1f) >> 2,
                    ((color as u8) & 0x1f) << 3 | ((color as u8) & 0x1f) >> 2,
                    0xff,
                ]);
            }
        }
    }
    writer.write_image_data(&rgba).is_ok()
}

/// A replacement PNG decoded to patch cells, before it is matched to a
/// source: the cells are the image itself, so the same decode fits whatever
/// source size turns out to ask for it.
#[derive(Debug)]
struct Decoded {
    width: u32,
    height: u32,
    cells: Vec<u32>,
}

/// Decode a replacement PNG to patch cells.
fn decode_replacement(data: &[u8]) -> Option<Decoded> {
    let mut decoder = png::Decoder::new(data);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let size = reader.output_buffer_size();
    if size == 0 {
        return None;
    }
    let mut buffer = vec![0u8; size];
    let info = reader.next_frame(&mut buffer).ok()?;
    let (iw, ih) = (info.width, info.height);
    if iw == 0 || ih == 0 {
        return None;
    }
    let (palette, alpha_bits) = match info.color_type {
        png::ColorType::Indexed => {
            let pal = reader.info().palette.clone()?;
            let trns = reader.info().trns.clone();
            (Some((pal, trns)), 0)
        }
        png::ColorType::Grayscale => (None, 1),
        png::ColorType::GrayscaleAlpha => (None, 2),
        png::ColorType::Rgb => (None, 3),
        png::ColorType::Rgba => (None, 4),
    };
    let mut cells = Vec::with_capacity(iw as usize * ih as usize);
    for index in 0..iw as usize * ih as usize {
        let at = index * alpha_bits;
        let (r, g, b, a) = match (&palette, alpha_bits) {
            (Some((pal, trns)), _) => {
                let p = buffer.get(at).copied().unwrap_or(0) as usize * 3;
                let a = trns
                    .as_ref()
                    .and_then(|t| t.get(buffer.get(at).copied().unwrap_or(0) as usize))
                    .copied()
                    .unwrap_or(0xff);
                (
                    pal.get(p).copied().unwrap_or(0),
                    pal.get(p + 1).copied().unwrap_or(0),
                    pal.get(p + 2).copied().unwrap_or(0),
                    a,
                )
            }
            (None, 1) => {
                let v = buffer.get(at).copied().unwrap_or(0);
                (v, v, v, 0xff)
            }
            (None, 2) => {
                let v = buffer.get(at).copied().unwrap_or(0);
                (v, v, v, buffer.get(at + 1).copied().unwrap_or(0))
            }
            (None, 3) => (
                buffer.get(at).copied().unwrap_or(0),
                buffer.get(at + 1).copied().unwrap_or(0),
                buffer.get(at + 2).copied().unwrap_or(0),
                0xff,
            ),
            _ => (
                buffer.get(at).copied().unwrap_or(0),
                buffer.get(at + 1).copied().unwrap_or(0),
                buffer.get(at + 2).copied().unwrap_or(0),
                buffer.get(at + 3).copied().unwrap_or(0),
            ),
        };
        if a < 128 {
            cells.push(0);
        } else {
            let color = u32::from(r >> 3) << 10 | u32::from(g >> 3) << 5 | u32::from(b >> 3);
            cells.push(1 << 31 | color);
        }
    }
    Some(Decoded {
        width: iw,
        height: ih,
        cells,
    })
}

impl Decoded {
    /// The texture this decode makes for a `w` x `h` source, if it is a
    /// uniform integer multiple of it — the rule the pack authors were given.
    fn fit(&self, w: u32, h: u32) -> Option<PackTexture> {
        if w == 0 || h == 0 || !self.width.is_multiple_of(w) || !self.height.is_multiple_of(h) {
            return None;
        }
        let k = self.width / w;
        (k == self.height / h).then(|| PackTexture {
            k,
            width: self.width,
            cells: self.cells.clone(),
        })
    }
}

impl TexturePack {
    /// Index every `<fnv>.png` in `dir` (filenames only; decode is lazy).
    pub fn load(dir: &Path) -> Self {
        let mut files = HashMap::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(stem) = name.to_str().and_then(|s| s.strip_suffix(".png")) else {
                    continue;
                };
                if stem.len() == 16 && stem.bytes().all(|b| b.is_ascii_hexdigit()) {
                    if let Ok(fnv) = u64::from_str_radix(stem, 16) {
                        files.insert(fnv, entry.path());
                    }
                }
            }
        }
        Self {
            files,
            images: Mutex::new(HashMap::new()),
            cache: Mutex::new(HashMap::new()),
            hits: AtomicUsize::new(0),
            misses: AtomicUsize::new(0),
        }
    }

    /// The replacement for a source of `w` x `h` hashed to `fnv`, if the
    /// pack has one that fits.
    pub fn get(&self, fnv: u64, w: u32, h: u32) -> Option<Arc<PackTexture>> {
        if !self.files.contains_key(&fnv) {
            self.misses.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        if let Ok(cache) = self.cache.lock() {
            if let Some(hit) = cache.get(&fnv) {
                if hit.is_some() {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                }
                return hit.clone();
            }
        }
        let built = self.image(fnv).and_then(|image| image.fit(w, h)).map(Arc::new);
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(fnv, built.clone());
        }
        if built.is_some() {
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        built
    }

    /// The decoded file behind a fingerprint, decoding it if this is its
    /// first use.
    fn image(&self, fnv: u64) -> Option<Arc<Decoded>> {
        if let Ok(images) = self.images.lock() {
            if let Some(hit) = images.get(&fnv) {
                return hit.clone();
            }
        }
        let decoded = self
            .files
            .get(&fnv)
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|data| decode_replacement(&data))
            .map(Arc::new);
        if let Ok(mut images) = self.images.lock() {
            images.insert(fnv, decoded.clone());
        }
        decoded
    }

    /// Decode every file in the pack on a thread of its own, so the first
    /// use of a texture during play is a lookup and not a PNG decode in the
    /// middle of a field.
    pub fn warm(self: &Arc<Self>) {
        let pack = Arc::clone(self);
        std::thread::spawn(move || {
            for &fnv in pack.files.keys() {
                pack.image(fnv);
            }
        });
    }

    /// `(indexed files, lookups served, lookups missed)` for diagnostics.
    pub fn stats(&self) -> (usize, usize, usize) {
        (
            self.files.len(),
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
        )
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}
