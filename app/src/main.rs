//! retro3do-check: a headless harness for the core. No window, no input
//! backend: it runs the built-in demonstration ROM (or a BIOS and disc you
//! name) for a number of fields and reports what the machine did.
//!
//!   retro3do-check                       run the demo for 120 fields, print stats
//!   retro3do-check --screenshot out.png  run ~120 fields, save the frame as PNG
//!   retro3do-check --self-test out.json  drive the pad and check the machine
//!   retro3do-check --bios B --disc D --frames N --screenshot out.png
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fs::File;
use std::io::BufWriter;
use std::process::ExitCode;

use retro3do::audio_ring::StereoSample;
use retro3do::memory::VRAM_BASE;
use retro3do::pad::{Joypad, PadButton};
use retro3do::video::framebuffer_offset;
use retro3do::Console;

#[derive(Default)]
struct Options {
    screenshot: Option<String>,
    self_test: Option<String>,
    bios: Option<String>,
    disc: Option<String>,
    frames: Option<u32>,
    /// Internal render scale, as the launcher asks for it: above one the
    /// fields come from the hi-res surfaces instead of the cel expansion.
    scale: Option<u32>,
    /// The launcher's built-in art enhancement, which also turns on the
    /// surface path.
    enhance: bool,
}

fn parse() -> Result<Options, String> {
    let mut options = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--screenshot" => options.screenshot = Some(value()?),
            "--self-test" => options.self_test = Some(value()?),
            "--bios" => options.bios = Some(value()?),
            "--disc" => options.disc = Some(value()?),
            "--frames" => {
                options.frames = Some(
                    value()?
                        .parse()
                        .map_err(|_| "--frames needs a number".to_owned())?,
                )
            }
            "--enhance" => options.enhance = true,
            "--scale" => {
                options.scale = Some(
                    value()?
                        .parse()
                        .map_err(|_| "--scale needs a number".to_owned())?,
                )
            }
            "--help" | "-h" => return Err(String::new()),
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(options)
}

fn boot(options: &Options) -> Result<Console, String> {
    let mut console = Console::new();
    let loaded = match &options.bios {
        Some(path) => console.load_bios(path),
        None => console.load_builtin_demo(),
    };
    if !loaded {
        return Err(console.last_error().to_owned());
    }
    if let Some(disc) = &options.disc {
        if !console.load_disc(disc) {
            return Err(console.last_error().to_owned());
        }
    }
    console.reset();
    if let Some(scale) = options.scale {
        console.set_internal_scale(scale);
    }
    if options.enhance {
        console.set_texture_enhance(true);
    }
    Ok(console)
}

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

fn coloured_fraction(console: &Console) -> f64 {
    let frame = console.framebuffer();
    let lit = frame
        .pixels
        .iter()
        .filter(|&&p| p & 0x00ff_ffff != 0)
        .count();
    lit as f64 / frame.pixels.len().max(1) as f64
}

struct Step {
    name: &'static str,
    passed: bool,
    detail: String,
}

/// Play the demo the way a person would: let it boot, hold Right on pad one,
/// and check from the machine's own state that each part of the loop worked.
fn self_test(options: &Options) -> Result<Vec<Step>, String> {
    let mut console = boot(options)?;
    for _ in 0..30 {
        console.run_frame();
    }
    let marker = VRAM_BASE + framebuffer_offset(152, 204, 320);
    let before = console.system_mut().read32(marker);
    console.set_joypad(Joypad::default().with(PadButton::Right, true));
    for _ in 0..30 {
        console.run_frame();
    }
    let after = console.system_mut().read32(marker);
    console.set_joypad(Joypad::default());

    let mut steps = vec![Step {
        name: "move",
        passed: before != after,
        detail: format!("held Right for 30 fields: the demo's marker at (152,204) went {before:08x} -> {after:08x}"),
    }];

    console.audio().reset();
    console.run_frame();
    let mut samples = vec![StereoSample::default(); 2048];
    let real = console.audio().pull(&mut samples) as usize;
    let heard = samples
        .iter()
        .take(real)
        .filter(|s| s.left != 0 || s.right != 0)
        .count();
    let instructions = console.cpu().total_instructions();
    steps.push(Step {
        name: "act",
        passed: instructions > 1000 && heard > 0,
        detail: format!(
            "ARM60 retired {instructions} instructions; the DSP produced {real} samples, {heard} of them non-silent"
        ),
    });

    let lit = coloured_fraction(&console);
    let entries = console.vdlp().entries_walked();
    let frame = console.framebuffer();
    steps.push(Step {
        name: "hud",
        passed: lit > 0.01 && entries > 0 && console.madam().vdl_address() != 0,
        detail: format!(
            "{}x{} frame, {:.1}% of pixels lit, display list at {:08x} walked {entries} entries",
            frame.width,
            frame.height,
            lit * 100.0,
            console.madam().vdl_address()
        ),
    });
    Ok(steps)
}

fn json_escape(text: &str) -> String {
    text.chars()
        .flat_map(|c| match c {
            '"' => vec!['\\', '"'],
            '\\' => vec!['\\', '\\'],
            c if c.is_control() => format!("\\u{:04x}", c as u32).chars().collect(),
            c => vec![c],
        })
        .collect()
}

fn run() -> Result<(), String> {
    let options = parse().map_err(|error| {
        format!(
            "{error}\nusage: retro3do-check [--screenshot out.png] [--self-test out.json] \
             [--bios file] [--disc file] [--frames n]"
        )
    })?;

    if let Some(path) = &options.self_test {
        let steps = self_test(&options)?;
        let body: Vec<String> = steps
            .iter()
            .map(|s| {
                format!(
                    "    {{\"step\": \"{}\", \"passed\": {}, \"detail\": \"{}\"}}",
                    s.name,
                    s.passed,
                    json_escape(&s.detail)
                )
            })
            .collect();
        let json = format!(
            "{{\n  \"version\": 1,\n  \"steps\": [\n{}\n  ]\n}}\n",
            body.join(",\n")
        );
        std::fs::write(path, json).map_err(|e| format!("{path}: {e}"))?;
        for step in &steps {
            println!(
                "{} {}: {}",
                if step.passed { "PASS" } else { "FAIL" },
                step.name,
                step.detail
            );
        }
    }

    if options.self_test.is_none() || options.screenshot.is_some() {
        let mut console = boot(&options)?;
        let frames = options.frames.unwrap_or(120);
        let mut cycles = 0u64;
        for _ in 0..frames {
            cycles += u64::from(console.run_frame());
        }
        let frame = console.framebuffer();
        println!(
            "{frames} fields, {cycles} CPU cycles, {} instructions, {}x{} frame, {:.1}% lit, {} audio pairs queued",
            console.cpu().total_instructions(),
            frame.width,
            frame.height,
            coloured_fraction(&console) * 100.0,
            console.audio().available()
        );
        if let Some(path) = &options.screenshot {
            save_png(&console, path)?;
            println!("saved {path}");
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
