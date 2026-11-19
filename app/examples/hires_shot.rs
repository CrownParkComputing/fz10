//! Boot a disc at an internal scale, save the picture, and time the fields.
//!
//! `hires_shot BIOS DISC FIELDS PAD_FROM SCALE OUT.png [X Y W H]`
//!
//! Runs FIELDS fields (pressing A/Play from PAD_FROM on, as `field_trace`
//! does, so every scale reaches the same frame), writes the whole output frame
//! to OUT.png and, given a crop rectangle in 640x480 output coordinates, also
//! writes that region to OUT.crop.png magnified to 4x of the 640x480 size
//! whatever the scale, so crops at different scales line up for comparison.
//! Prints the mean and worst time per field over the run's second half; with
//! HIRES_SHOT_EVERY_FIELD set, every field's time goes to stderr as well.
use std::time::Instant;

use retro3do::pad::{Joypad, PadButton};
use retro3do::Console;

fn save(path: &str, pixels: &[u32], width: usize, height: usize) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    let rgb: Vec<u8> = pixels
        .iter()
        .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8])
        .collect();
    writer.write_image_data(&rgb).map_err(|e| e.to_string())
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |i: usize| args.get(i).map(String::as_str).unwrap_or("");
    let number = |i: usize| {
        arg(i).parse::<i64>().map_err(|_| {
            "usage: hires_shot BIOS DISC FIELDS PAD_FROM SCALE OUT.png [X Y W H]".to_owned()
        })
    };
    let (fields, pad_from, scale) = (number(3)?, number(4)?, number(5)?);
    let out = arg(6);

    let mut console = Console::new();
    if !console.load_bios(arg(1)) || !console.load_disc(arg(2)) {
        return Err(console.last_error().to_owned());
    }
    console.set_internal_scale(scale as u32);
    console.set_texture_enhance(std::env::var_os("RETRO3DO_PLAIN").is_none());
    console.set_texture_dir(std::env::var_os("RETRO3DO_TEXDIR").map(std::path::PathBuf::from).as_deref());
    console.reset();

    let mut times = Vec::new();
    for field in 0..fields {
        if pad_from >= 0 {
            console.set_joypad(
                Joypad::default()
                    .with(PadButton::A, field >= pad_from && (field / 10) % 2 == 0)
                    .with(PadButton::Play, field >= pad_from && (field / 25) % 2 == 1),
            );
        }
        let started = Instant::now();
        console.run_frame();
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        if field >= fields / 2 {
            times.push((ms, field));
        }
        if std::env::var_os("HIRES_SHOT_EVERY_FIELD").is_some() {
            eprintln!("{field} {ms:.2}");
        }
    }
    let mean = times.iter().map(|t| t.0).sum::<f64>() / times.len().max(1) as f64;
    times.sort_by(|a, b| b.0.total_cmp(&a.0));
    let worst: Vec<String> = times
        .iter()
        .take(4)
        .map(|(ms, field)| format!("{ms:.1} ms @{field}"))
        .collect();
    let surfaces = console.madam().hires().surfaces().len();
    println!(
        "scale {scale}: {mean:.2} ms/field mean; worst {}; {surfaces} surfaces",
        worst.join(", ")
    );
    if let Some((files, hits, misses)) = console.madam().texture_pack_stats() {
        println!("texpack: {files} files, {hits} replacements served, {misses} misses");
    }

    let frame = console.framebuffer();
    save(out, frame.pixels, frame.width, frame.height)?;

    if let (Ok(x), Ok(y), Ok(w), Ok(h)) = (number(7), number(8), number(9), number(10)) {
        let k = frame.width / 640;
        let (w4, h4) = (w as usize * 4, h as usize * 4);
        let mut crop = vec![0u32; w4 * h4];
        for (i, pixel) in crop.iter_mut().enumerate() {
            let (cx, cy) = (i % w4, i / w4);
            let sx = x as usize * k + cx * k / 4;
            let sy = y as usize * k + cy * k / 4;
            *pixel = frame
                .pixels
                .get(sy * frame.width + sx)
                .copied()
                .unwrap_or(0);
        }
        save(&out.replace(".png", ".crop.png"), &crop, w4, h4)?;
    }
    Ok(())
}
