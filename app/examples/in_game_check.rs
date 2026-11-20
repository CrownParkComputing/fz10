//! The bar the player sees: does the machine hold the field rate in gameplay?
//!
//! `in_game_check BIOS DISC OUT_PNG [MEASURE_SECONDS]`
//!
//! Runs the threaded emulator - the paced path every front end uses - scripts
//! the pad into a race, and then reports what the machine actually managed
//! over the gameplay window: fields a second, fields that missed their
//! deadline, deadline resets and the smoothed per-field cost. The last frame
//! is written out so the screen it was in can be checked by eye.
use std::fs::File;
use std::io::BufWriter;
use std::time::{Duration, Instant};

use retro3do::pad::PadButton;
use retro3do::Retro3do;

/// Fields between the button taps, and how long each tap lasts.
const TAP_PERIOD: u64 = 90;
const TAP_LEN: u64 = 8;
/// The taps that get past the menus and into the race.
const PLAY_TAPS: [u64; 3] = [1200, 1500, 1800];
/// Nothing is pressed after this: the last tap would pause the race.
const LAST_INPUT: u64 = 1900;

fn save_png(path: &str, pixels: &[u32], width: usize, height: usize) -> Result<(), String> {
    let rgba: Vec<u8> = pixels
        .iter()
        .flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8, 0xff])
        .collect();
    let file = File::create(path).map_err(|e| format!("{path}: {e}"))?;
    let mut encoder = png::Encoder::new(
        BufWriter::new(file),
        u32::try_from(width).map_err(|e| e.to_string())?,
        u32::try_from(height).map_err(|e| e.to_string())?,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(&rgba).map_err(|e| e.to_string())
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |i: usize| args.get(i).map(String::as_str).unwrap_or("");
    let measure: u64 = arg(3).parse().unwrap_or(20);

    let mut emu = Retro3do::new();
    emu.load_bios(arg(1))?;
    emu.load_disc(arg(2))?;
    emu.set_internal_scale(1);
    emu.start();

    let mut fields: u64 = 0;
    let mut last: Option<(Vec<u32>, usize, usize)> = None;
    let mut frames_seen = 0u64;
    let started = Instant::now();
    let mut measured_at: Option<(Instant, retro3do::EmulatorStats)> = None;

    while started.elapsed() < Duration::from_secs(measure + 30) {
        // The script: mash A to get past the logos and the menus, tap Play
        // to start a race, then leave the pad alone.
        let in_window = fields < LAST_INPUT;
        let a = in_window && fields >= 60 && fields % TAP_PERIOD < TAP_LEN;
        let play = in_window && PLAY_TAPS.iter().any(|&t| fields >= t && fields < t + TAP_LEN);
        emu.pads().press(0, PadButton::A, a);
        emu.pads().press(0, PadButton::Play, play);

        if let Some(frame) = emu.acquire_frame(|pixels, w, h| (pixels.to_vec(), w, h)) {
            fields += 1;
            last = Some(frame);
            frames_seen += 1;
            if frames_seen == 1200 && measured_at.is_none() {
                // The race is running by now: start measuring from here.
                measured_at = Some((Instant::now(), emu.stats()));
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    let (from, before) = measured_at.ok_or("the machine never got that far")?;
    let after = emu.stats();
    let seconds = from.elapsed().as_secs_f64();
    emu.stop();

    let (pixels, w, h) = last.ok_or("no frame was published")?;
    save_png(arg(3), &pixels, w, h)?;

    println!(
        "in-game: {:.2} fields/s, {} late fields, {} deadline resets, {:.2} ms/field (smoothed), internal scale {}x",
        (after.frames_emulated - before.frames_emulated) as f64 / seconds,
        after.frames_behind - before.frames_behind,
        after.deadline_resets - before.deadline_resets,
        after.frame_ms,
        emu.effective_internal_scale(),
    );
    println!("last frame written to {}", arg(3));
    Ok(())
}
