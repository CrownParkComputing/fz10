//! gamecheck - boot every disc in a library, headless, and say what happened.
//! A port of cpp-reference/tools/gamecheck so the same table can be compared
//! core against core.
//!
//!   gamecheck <bios> <library-dir> [--frames N] [--only SUBSTR] [--shots DIR] [--press]
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use retro3do::pad::{Joypad, PadButton};
use retro3do::Console;

const PRESS_PERIOD: u64 = 180;
const PRESS_HOLD: u64 = 8;

struct Outcome {
    name: String,
    commands: u64,
    sectors: u64,
    cels: u64,
    screens: usize,
    pixels: usize,
    seconds: f64,
    loaded: bool,
}

fn hash_frame(pixels: &[u32]) -> u64 {
    let mut hash = 1469598103934665603u64;
    for &p in pixels {
        hash ^= u64::from(p);
        hash = hash.wrapping_mul(1099511628211);
    }
    hash
}

fn run(bios: &Path, disc: &Path, frames: u64, shot_dir: Option<&Path>, press: bool) -> Outcome {
    let mut result = Outcome {
        name: disc
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        commands: 0,
        sectors: 0,
        cels: 0,
        screens: 0,
        pixels: 0,
        seconds: 0.0,
        loaded: false,
    };

    let mut console = Console::new();
    if !console.load_bios(&bios.to_string_lossy()) {
        return result;
    }
    if !console.load_disc(&disc.to_string_lossy()) {
        return result;
    }
    result.loaded = true;
    console.reset();

    let mut screens: HashSet<u64> = HashSet::new();
    let mut best_frame: Vec<u32> = Vec::new();
    let mut best_w = 0usize;
    let mut best_h = 0usize;
    let mut last_frame: Vec<u32> = Vec::new();
    let mut last_w = 0usize;
    let mut last_h = 0usize;

    let started = Instant::now();
    for frame in 0..frames {
        if press {
            let pad = Joypad::default().with(PadButton::Play, frame % PRESS_PERIOD < PRESS_HOLD);
            console.set_joypad(pad);
        }
        console.run_frame();
        let image = console.framebuffer();
        screens.insert(hash_frame(image.pixels));

        // Pixels differing from the frame's commonest colour.
        let mut histogram: HashMap<u32, usize> = HashMap::new();
        for &p in image.pixels {
            *histogram.entry(p & 0x00ff_ffff).or_default() += 1;
        }
        let commonest = histogram.values().copied().max().unwrap_or(0);
        let lit = image.pixels.len() - commonest;
        if frame == frames - 1 {
            last_w = image.width;
            last_h = image.height;
            last_frame = image.pixels.to_vec();
        }
        if lit > result.pixels {
            result.pixels = lit;
            best_w = image.width;
            best_h = image.height;
            best_frame = image.pixels.to_vec();
        }
    }
    result.seconds = started.elapsed().as_secs_f64();
    result.commands = console.clio().cdrom().commands_received();
    result.sectors = console.expansion_dma_count();
    result.cels = console.madam().total_cels_drawn();
    result.screens = screens.len();

    if let Some(dir) = shot_dir {
        let _ = fs::create_dir_all(dir);
        let write = |out: PathBuf, pixels: &[u32], w: usize, h: usize| {
            if pixels.is_empty() {
                return;
            }
            let mut body = format!("P6\n{w} {h}\n255\n").into_bytes();
            for &p in pixels {
                body.extend_from_slice(&[
                    ((p >> 16) & 0xff) as u8,
                    ((p >> 8) & 0xff) as u8,
                    (p & 0xff) as u8,
                ]);
            }
            let _ = fs::write(out, body);
        };
        write(dir.join(format!("{}.ppm", result.name)), &best_frame, best_w, best_h);
        write(
            dir.join(format!("{} (final).ppm", result.name)),
            &last_frame,
            last_w,
            last_h,
        );
    }
    result
}

fn main() -> std::result::Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut positional: Vec<String> = Vec::new();
    let mut frames: u64 = 20000;
    let mut only: Option<String> = None;
    let mut shots: Option<PathBuf> = None;
    let mut press = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--frames" => {
                i += 1;
                frames = args
                    .get(i)
                    .and_then(|v| v.parse().ok())
                    .ok_or("--frames needs a number")?;
            }
            "--only" => {
                i += 1;
                only = args.get(i).cloned();
            }
            "--shots" => {
                i += 1;
                shots = args.get(i).map(PathBuf::from);
            }
            "--press" => press = true,
            other => positional.push(other.to_string()),
        }
        i += 1;
    }
    let (Some(bios), Some(lib)) = (positional.first(), positional.get(1)) else {
        return Err(
            "usage: gamecheck <bios> <library-dir> [--frames N] [--only S] [--shots D] [--press]"
                .to_string(),
        );
    };
    let bios = PathBuf::from(bios);

    let mut discs: Vec<PathBuf> = fs::read_dir(lib)
        .map_err(|e| format!("{lib}: {e}"))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .map(|e| e.eq_ignore_ascii_case("chd"))
                .unwrap_or(false)
        })
        .collect();
    discs.sort();
    if let Some(needle) = &only {
        discs.retain(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().contains(needle.as_str()))
                .unwrap_or(false)
        });
    }

    println!(
        "{:<52} {:>8} {:>8} {:>9} {:>7} {:>9} {:>7}",
        "title", "commands", "sectors", "cels", "screens", "pixels", "time"
    );
    for disc in discs {
        let r = run(&bios, &disc, frames, shots.as_deref(), press);
        println!(
            "{:<52} {:>8} {:>8} {:>9} {:>7} {:>9} {:>6.1}s{}",
            r.name,
            r.commands,
            r.sectors,
            r.cels,
            r.screens,
            r.pixels,
            r.seconds,
            if r.loaded { "" } else { "  (did not load)" }
        );
    }
    Ok(())
}
