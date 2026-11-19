//! Build a texture pack from sprite-art directories.
//!
//! `texpack_build OUT_DIR ROOT [ROOT ...]`
//!
//! Walks each ROOT for `NAME-original.png` files (the native-resolution
//! reference render of a source sprite), fingerprints each with the same
//! `texpack::source_fnv` the console applies to decoded cel texels, then
//! finds the hand-authored replacement for it: any `NAME-*.png` in the
//! sibling `hd/` directory (where several variants exist, the last by name
//! wins — rename the one you want so it sorts last). The chosen file is
//! hard-linked into OUT_DIR as `<fnv>.png`, the name the pack looks up.
//!
//! Transparent source pixels (alpha < 128) fingerprint as colour 0 +
//! transparent, matching the decoder's transparent-zero texels.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use retro3do::texpack::source_fnv;

/// Decode a PNG to RGBA8; returns (width, height, pixels).
fn decode_png(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(std::fs::File::open(path).ok()?);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let size = reader.output_buffer_size();
    let mut buffer = vec![0u8; size];
    let info = reader.next_frame(&mut buffer).ok()?;
    // EXPAND leaves 4 bytes per pixel only for sources with alpha; pad the
    // rest so the loop below can treat everything uniformly.
    let per_pixel = size / info.width as usize / info.height.max(1) as usize;
    if !(1..=4).contains(&per_pixel) {
        return None;
    }
    let mut rgba = Vec::with_capacity(info.width as usize * info.height as usize * 4);
    for chunk in buffer.chunks_exact(per_pixel) {
        let (r, g, b, a) = match per_pixel {
            1 => (chunk[0], chunk[0], chunk[0], 0xff),
            2 => (chunk[0], chunk[0], chunk[0], *chunk.get(1)?),
            3 => (chunk[0], *chunk.get(1)?, *chunk.get(2)?, 0xff),
            _ => (chunk[0], *chunk.get(1)?, *chunk.get(2)?, *chunk.get(3)?),
        };
        rgba.extend_from_slice(&[r, g, b, a]);
    }
    Some((info.width, info.height, rgba))
}

/// The pack fingerprint of a decoded source image.
fn png_fnv(width: u32, height: u32, rgba: &[u8]) -> u64 {
    source_fnv(width, height, |x, y| {
        let i = (y as usize * width as usize + x as usize) * 4;
        let (r, g, b, a) = (
            rgba.get(i).copied().unwrap_or(0),
            rgba.get(i + 1).copied().unwrap_or(0),
            rgba.get(i + 2).copied().unwrap_or(0),
            rgba.get(i + 3).copied().unwrap_or(0),
        );
        if a < 128 {
            (0, true)
        } else {
            (
                (u16::from(r >> 3) << 10) | (u16::from(g >> 3) << 5) | u16::from(b >> 3),
                false,
            )
        }
    })
}

/// Every `*.png` under `root`, recursively.
fn walk_pngs(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_pngs(&path, out);
        } else if path.extension().is_some_and(|e| e == "png") {
            out.push(path);
        }
    }
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        return Err("usage: texpack_build OUT_DIR ROOT [ROOT ...]".to_owned());
    }
    let out_dir = PathBuf::from(&args[1]);
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;

    for root in &args[2..] {
        let mut originals = Vec::new();
        walk_pngs(Path::new(root), &mut originals);
        originals.retain(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.ends_with("-original"))
        });
        for original in originals {
            let Some((w, h, rgba)) = decode_png(&original) else {
                eprintln!("skip (undecodable): {}", original.display());
                continue;
            };
            let name = original
                .file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.strip_suffix("-original"))
                .unwrap_or_default()
                .to_owned();
            // Candidates live in the `hd/` dir that is a sibling of the dir
            // holding this original (or of its parent for flat layouts).
            let parent = original.parent().map(Path::to_path_buf).unwrap_or_default();
            let mut hd_dirs = vec![parent.join("hd")];
            if let Some(up) = parent.parent() {
                hd_dirs.push(up.join("hd"));
            }
            let mut candidates: BTreeMap<String, PathBuf> = BTreeMap::new();
            for hd in &hd_dirs {
                if let Ok(entries) = std::fs::read_dir(hd) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                            continue;
                        };
                        if stem.starts_with(&name) && stem != name && path.extension().is_some_and(|e| e == "png") {
                            candidates.insert(stem.to_owned(), path);
                        }
                    }
                }
            }
            let Some((chosen_name, chosen)) = candidates.into_iter().next_back() else {
                eprintln!("no hd variant: {name}");
                continue;
            };
            let fnv = png_fnv(w, h, &rgba);
            let dest = out_dir.join(format!("{fnv:016x}.png"));
            if dest.exists() {
                std::fs::remove_file(&dest).map_err(|e| e.to_string())?;
            }
            std::fs::hard_link(chosen, &dest).map_err(|e| e.to_string())?;
            println!("{name} ({w}x{h}) <- {chosen_name} => {fnv:016x}");
        }
    }
    Ok(())
}
