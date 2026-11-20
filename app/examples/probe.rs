//! Headless probe: boot a disc, script some pad presses, dump frames.
//!
//! `probe BIOS DISC OUT_DIR [FRAMES] [SCALE]`
//!
//! Prints per-field cost statistics and writes a PNG every 300 fields so the
//! caller can see which screen the machine is actually on.
use std::fs::File;
use std::io::BufWriter;
use std::time::Instant;

use retro3do::console::Console;
use retro3do::pad::PadButton;

fn save_png(console: &Console, path: &str) -> Result<(), String> {
    let frame = console.framebuffer();
    let rgba: Vec<u8> = frame
        .pixels
        .iter()
        .flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8, 0xff])
        .collect();
    let file = File::create(path).map_err(|e| format!("{path}: {e}"))?;
    let width = u32::try_from(frame.width).map_err(|e| e.to_string())?;
    let height = u32::try_from(frame.height).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(&rgba).map_err(|e| e.to_string())
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |i: usize| args.get(i).map(String::as_str).unwrap_or("");
    let frames: u32 = arg(4).parse().unwrap_or(3000);
    let scale: u32 = arg(5).parse().unwrap_or(1);

    let mut console = Console::new();
    if !console.load_bios(arg(1)) {
        return Err(format!("bios: {}", console.last_error()));
    }
    if !console.load_disc(arg(2)) {
        return Err(format!("disc: {}", console.last_error()));
    }
    console.reset();
    console.set_internal_scale(scale);

    let mut costs: Vec<f64> = Vec::with_capacity(frames as usize);
    let mut prev_decodes = 0u64;
    let started = Instant::now();
    for field in 0..frames {
        // Script: mash A early to get past logos and menus.
        let a = field % 120 < 12 && field >= 60;
        let play = field % 180 < 8 && field >= 120;
        console.pads().press(0, PadButton::A, a);
        console.pads().press(0, PadButton::Play, play);

        let t0 = Instant::now();
        console.run_frame();
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        costs.push(ms);

        if field % 300 == 299 {
            let name = format!("{}/frame_{:05}.png", arg(3), field + 1);
            save_png(&console, &name)?;
            let window = &costs[costs.len() - 300..];
            let sum: f64 = window.iter().sum();
            let decodes = console.cpu().decodes();
            println!(
                "field {:5}  window mean {:6.2} ms  max {:6.2} ms   arm decodes/field {:8.1}",
                field + 1,
                sum / window.len() as f64,
                window.iter().cloned().fold(0.0f64, f64::max),
                (decodes - prev_decodes) as f64 / 300.0,
            );
            prev_decodes = decodes;
        }
    }
    let total = started.elapsed().as_secs_f64();
    let sum: f64 = costs.iter().sum();
    println!(
        "{frames} fields in {total:.2}s: mean {:.2} ms/field, {:.1} fields/s",
        sum / costs.len() as f64,
        costs.len() as f64 / total
    );
    Ok(())
}
