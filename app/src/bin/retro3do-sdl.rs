//! retro3do-sdl: a desktop front end for the Rust 3DO core.
//!
//! An SDL3 window with a Vulkan renderer, streaming game audio and
//! keyboard/gamepad input. Everything here is the window and device layer the
//! core deliberately does not own; the machine, its thread and its pacing live
//! inside `retro3do`.

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::ffi::CString;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use retro3do::{PadButton, Region, Retro3do};
use sdl3::audio::{AudioFormat, AudioSpec};
use sdl3::event::Event;
use sdl3::gamepad::{Axis, Button};
use sdl3::keyboard::Keycode;
use sdl3::pixels::PixelFormat;
use sdl3::render::{self, FRect, ScaleMode, Texture};

const FRAME_RATE_NTSC: f64 = 60.0;
const AUDIO_BYTES_PER_PAIR: usize = 4;
/// How much queued stream audio the front end tries to keep, in stereo pairs.
/// Roughly 100 ms: enough to cover a scheduler hiccup, small enough that pause
/// and input sound immediate.
const AUDIO_TARGET_PAIRS: i32 = 4410;
const AUDIO_PULL_PAIRS: usize = 1024;

struct Options {
    bios: Option<String>,
    disc: Option<String>,
    demo: bool,
    pal: bool,
    nvram: Option<PathBuf>,
    scale: u32,
    no_sound: bool,
}

fn usage(program: &str) -> String {
    format!(
        "usage: {program} [--bios FILE] [--disc FILE] [--demo] [--pal]\n\
         \x20                 [--nvram FILE] [--scale N] [--no-sound]\n\
         \n\
         With no --bios and no --demo the built-in demonstration ROM runs.\n\
         \n\
         keys: arrows d-pad, Z/X/C = A/B/C, Return = P, Backspace = X,\n\
         \x20      A/S = left/right shift, P pause, R reset, F fullscreen, Esc quit"
    )
}

fn parse_options() -> Result<Options, String> {
    let mut opts = Options {
        bios: None,
        disc: None,
        demo: false,
        pal: false,
        nvram: None,
        scale: 2,
        no_sound: false,
    };
    let program = std::env::args()
        .next()
        .unwrap_or_else(|| "retro3do-sdl".to_string());
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bios" => opts.bios = Some(take(&mut args, "--bios")?),
            "--disc" => opts.disc = Some(take(&mut args, "--disc")?),
            "--demo" => opts.demo = true,
            "--pal" => opts.pal = true,
            "--nvram" => opts.nvram = Some(PathBuf::from(take(&mut args, "--nvram")?)),
            "--scale" => {
                let text = take(&mut args, "--scale")?;
                opts.scale = text
                    .parse::<u32>()
                    .ok()
                    .filter(|n| (1..=8).contains(n))
                    .ok_or_else(|| "--scale wants a number 1-8".to_string())?;
            }
            "--no-sound" => opts.no_sound = true,
            "-h" | "--help" => return Err(usage(&program)),
            other => {
                if opts.disc.is_none() && std::path::Path::new(other).exists() {
                    opts.disc = Some(other.to_string());
                } else {
                    return Err(format!("{}\nunknown argument: {other}", usage(&program)));
                }
            }
        }
    }
    Ok(opts)
}

fn take(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{flag} wants a value"))
}

/// The keyboard and gamepad state, collapsed into one pad bitmask each frame.
struct InputState {
    mask: u32,
}

impl InputState {
    fn new() -> Self {
        Self { mask: 0 }
    }

    fn set(&mut self, button: PadButton, down: bool) {
        let bit = 1u32 << (button as u32);
        if down {
            self.mask |= bit;
        } else {
            self.mask &= !bit;
        }
    }

    fn key(button: Keycode) -> Option<PadButton> {
        Some(match button {
            Keycode::Up => PadButton::Up,
            Keycode::Down => PadButton::Down,
            Keycode::Left => PadButton::Left,
            Keycode::Right => PadButton::Right,
            Keycode::Z => PadButton::A,
            Keycode::X => PadButton::B,
            Keycode::C => PadButton::C,
            Keycode::Return | Keycode::Space => PadButton::Play,
            Keycode::Backspace => PadButton::Stop,
            Keycode::A => PadButton::LeftShift,
            Keycode::S => PadButton::RightShift,
            _ => return None,
        })
    }

    fn pad_button(button: Button) -> Option<PadButton> {
        Some(match button {
            Button::DPadUp => PadButton::Up,
            Button::DPadDown => PadButton::Down,
            Button::DPadLeft => PadButton::Left,
            Button::DPadRight => PadButton::Right,
            // The face buttons: south is the primary on every layout, so it
            // lands on the 3DO's primary A; east/west/north follow round.
            Button::South => PadButton::A,
            Button::East => PadButton::B,
            Button::West => PadButton::C,
            Button::North => PadButton::C,
            Button::Start => PadButton::Play,
            Button::Back => PadButton::Stop,
            Button::LeftShoulder => PadButton::LeftShift,
            Button::RightShoulder => PadButton::RightShift,
            _ => return None,
        })
    }

    /// Analogue stick and triggers read as digital directions.
    fn on_axis(&mut self, axis: Axis, value: i16) {
        match axis {
            Axis::LeftX => {
                const EDGE: i16 = 16000;
                self.set(PadButton::Left, value < -EDGE);
                self.set(PadButton::Right, value > EDGE);
            }
            Axis::LeftY => {
                const EDGE: i16 = 16000;
                self.set(PadButton::Up, value < -EDGE);
                self.set(PadButton::Down, value > EDGE);
            }
            Axis::TriggerLeft => self.set(PadButton::LeftShift, value > 16000),
            Axis::TriggerRight => self.set(PadButton::RightShift, value > 16000),
            _ => {}
        }
    }
}

/// Where the frame lands inside the window: centred, aspect kept, whole
/// pixels preferred so scanlines stay even.
fn frame_rect(window_w: u32, window_h: u32, frame_w: usize, frame_h: usize) -> Option<FRect> {
    let fw = u32::try_from(frame_w).ok()?;
    let fh = u32::try_from(frame_h).ok()?;
    if fw == 0 || fh == 0 || window_w == 0 || window_h == 0 {
        return None;
    }
    let scale = (window_w as f64 / f64::from(fw)).min(window_h as f64 / f64::from(fh));
    if scale <= 0.0 {
        return None;
    }
    let w = (f64::from(fw) * scale) as f32;
    let h = (f64::from(fh) * scale) as f32;
    Some(FRect::new(
        (window_w as f32 - w) / 2.0,
        (window_h as f32 - h) / 2.0,
        w,
        h,
    ))
}

fn run(opts: &Options) -> Result<(), String> {
    let sdl = sdl3::init().map_err(|e| format!("SDL: {e}"))?;
    let video = sdl.video().map_err(|e| format!("SDL video: {e}"))?;
    let audio = sdl.audio().map_err(|e| format!("SDL audio: {e}"))?;
    let gamepad = sdl.gamepad().map_err(|e| format!("SDL gamepad: {e}"))?;
    let mut events = sdl.event_pump().map_err(|e| format!("SDL events: {e}"))?;

    let mut emu = Retro3do::new();
    if let Some(bios) = &opts.bios {
        emu.load_bios(bios)
            .map_err(|e| format!("bios {bios}: {e}"))?;
    }
    if let Some(disc) = &opts.disc {
        emu.load_disc(disc)
            .map_err(|e| format!("disc {disc}: {e}"))?;
    }
    if opts.demo || !emu.bios_loaded() {
        emu.load_demo().map_err(|e| format!("demo rom: {e}"))?;
    }
    if opts.pal {
        emu.set_region(Region::Pal);
    }
    if let Some(nvram) = &opts.nvram {
        if let Ok(image) = std::fs::read(nvram) {
            if emu.restore_nvram(&image) {
                eprintln!(
                    "restored {} bytes of nvram from {}",
                    image.len(),
                    nvram.display()
                );
            }
        }
    }
    emu.pads().set_connected(0, true);

    let window_title = opts
        .disc
        .as_deref()
        .and_then(|d| std::path::Path::new(d).file_stem())
        .and_then(|s| s.to_str())
        .map_or_else(
            || "retro3do".to_string(),
            |name| format!("retro3do — {name}"),
        );
    let window = video
        .window(&window_title, 640 * opts.scale, 480 * opts.scale)
        .position_centered()
        .resizable()
        .build()
        .map_err(|e| format!("window: {e}"))?;

    // Vulkan first: the person asked for it, and SDL3's Vulkan driver wants
    // Vulkan 1.3 features when the driver exposes them. If creation fails the
    // error says why rather than silently drawing another way.
    let vulkan_name = CString::new("vulkan").map_err(|_| "bad driver name".to_string())?;
    let mut canvas = match render::create_renderer(window, Some(&vulkan_name)) {
        Ok(canvas) => canvas,
        Err(e) => {
            let window = video
                .window(&window_title, 640 * opts.scale, 480 * opts.scale)
                .position_centered()
                .resizable()
                .build()
                .map_err(|e| format!("window (retry): {e}"))?;
            let canvas = render::create_renderer(window, None)
                .map_err(|e2| format!("renderer: vulkan: {e}; fallback: {e2}"))?;
            eprintln!("vulkan renderer unavailable ({e}); using SDL's default");
            canvas
        }
    };
    eprintln!("renderer: {}", canvas.renderer_name);

    let creator = canvas.texture_creator();
    let mut texture: Option<Texture> = None;
    let mut tex_size = (0usize, 0usize);
    let mut frame_bytes: Vec<u8> = Vec::new();
    let mut frame_size = (0usize, 0usize);

    // Game audio: one stream at the machine's rate, topped up from the ring as
    // it drains. Silence on a shortfall is the ring's job, done inside it.
    let mut stream = None;
    if !opts.no_sound {
        let spec = AudioSpec {
            freq: Some(retro3do::audio_ring::SAMPLE_RATE as i32),
            channels: Some(2),
            format: Some(AudioFormat::S16LE),
        };
        match audio
            .open_playback_device(&spec)
            .and_then(|dev| dev.open_device_stream(Some(&spec)))
        {
            Ok(owner) => {
                let _ = owner.resume();
                stream = Some(owner);
            }
            Err(e) => eprintln!("no audio device ({e}); running silent"),
        }
    }
    let mut audio_buf = vec![0i16; AUDIO_PULL_PAIRS * 2];

    // A gamepad that arrives after start is picked up by the added event.
    let mut pad: Option<sdl3::gamepad::Gamepad> = None;
    if let Ok(ids) = gamepad.gamepads() {
        for id in ids {
            if gamepad.is_gamepad(id) {
                match gamepad.open(id) {
                    Ok(g) => {
                        eprintln!(
                            "gamepad: {}",
                            g.name().unwrap_or_else(|| "unknown".to_string())
                        );
                        pad = Some(g);
                        break;
                    }
                    Err(e) => eprintln!("gamepad open: {e}"),
                }
            }
        }
    }

    emu.start();

    let mut input = InputState::new();
    let mut quit = false;
    let mut fullscreen = false;
    let mut last_stats = Instant::now();
    let mut last_nvram_poll = Instant::now();
    let mut audio_underruns_seen = 0u64;

    while !quit {
        for event in events.poll_iter() {
            match event {
                Event::Quit { .. } => quit = true,
                Event::KeyDown {
                    keycode: Some(key),
                    repeat: false,
                    ..
                } => match key {
                    Keycode::Escape => quit = true,
                    Keycode::P => emu.set_paused(!emu.paused()),
                    Keycode::R => emu.request_reset(),
                    Keycode::F => {
                        fullscreen = !fullscreen;
                        let _ = canvas.window_mut().set_fullscreen(fullscreen);
                    }
                    other => {
                        if let Some(button) = InputState::key(other) {
                            input.set(button, true);
                        }
                    }
                },
                Event::KeyUp {
                    keycode: Some(key), ..
                } => {
                    if let Some(button) = InputState::key(key) {
                        input.set(button, false);
                    }
                }
                Event::GamepadAdded { which, .. } => {
                    if pad.is_none() && gamepad.is_gamepad(which) {
                        match gamepad.open(which) {
                            Ok(g) => pad = Some(g),
                            Err(e) => eprintln!("gamepad open: {e}"),
                        }
                    }
                }
                Event::GamepadRemoved { which, .. } => {
                    if pad.as_ref().is_some_and(|g| g.id().ok() == Some(which)) {
                        pad = None;
                    }
                }
                Event::GamepadButtonDown { button, .. } => {
                    if let Some(b) = InputState::pad_button(button) {
                        input.set(b, true);
                    }
                }
                Event::GamepadButtonUp { button, .. } => {
                    if let Some(b) = InputState::pad_button(button) {
                        input.set(b, false);
                    }
                }
                Event::GamepadAxisMotion { axis, value, .. } => {
                    input.on_axis(axis, value);
                }
                _ => {}
            }
        }
        emu.pads().set_buttons(0, input.mask);

        // Video: upload only when a new field landed; the window always
        // re-draws so resizes and compositor asks are harmless.
        let uploaded = emu.acquire_frame(|pixels, w, h| {
            if (w, h) != tex_size {
                match creator.create_texture_streaming(PixelFormat::XRGB8888, w as u32, h as u32) {
                    Ok(mut t) => {
                        t.set_scale_mode(ScaleMode::Nearest);
                        texture = Some(t);
                        tex_size = (w, h);
                    }
                    Err(e) => {
                        eprintln!("texture {w}x{h}: {e}");
                        return;
                    }
                }
            }
            frame_size = (w, h);
            frame_bytes.resize(w.saturating_mul(h).saturating_mul(4), 0);
            for (px, dst) in pixels.iter().zip(frame_bytes.as_chunks_mut::<4>().0) {
                dst.copy_from_slice(&px.to_ne_bytes());
            }
            if let Some(t) = texture.as_mut() {
                let pitch = w.saturating_mul(4);
                let _ = t.update(None, &frame_bytes, pitch);
            }
        });
        if uploaded.is_none() {
            // No new field yet; keep the last texture. A resize without a new
            // frame still needs a draw below.
            if tex_size != (0, 0) {
                frame_size = tex_size;
            }
        }

        let (win_w, win_h) = canvas.window().size();
        canvas.set_draw_color(sdl3::pixels::Color::RGB(8, 8, 12));
        canvas.clear();
        if let (Some(t), Some(dst)) = (
            texture.as_ref(),
            frame_rect(win_w, win_h, frame_size.0, frame_size.1),
        ) {
            let _ = canvas.copy(t, None, dst);
        }
        canvas.present();

        // Audio: keep the stream's queue near the target. put_data_i16 is
        // atomic per call, so one call per field-time of audio is plenty.
        if let Some(owner) = &stream {
            let queued = owner.queued_bytes().unwrap_or(0);
            let target_bytes = AUDIO_TARGET_PAIRS.saturating_mul(AUDIO_BYTES_PER_PAIR as i32);
            if queued < target_bytes {
                let pairs_wanted =
                    ((target_bytes - queued) as usize / AUDIO_BYTES_PER_PAIR).min(AUDIO_PULL_PAIRS);
                if let Some(buf) = audio_buf.get_mut(..pairs_wanted * 2) {
                    let _ = emu.audio().pull_interleaved(buf);
                    let _ = owner.put_data_i16(buf);
                }
            }
        }

        // Saves: the machine keeps its NVRAM until asked, so ask once a second.
        if last_nvram_poll.elapsed() >= Duration::from_secs(1) {
            last_nvram_poll = Instant::now();
            if let (Some(path), Some(image)) = (&opts.nvram, emu.take_nvram()) {
                if let Err(e) = std::fs::write(path, &image) {
                    eprintln!("nvram {}: {e}", path.display());
                }
            }
        }

        if last_stats.elapsed() >= Duration::from_secs(1) {
            last_stats = Instant::now();
            let stats = emu.stats();
            let underruns = emu.audio().underruns();
            let extra = if underruns != audio_underruns_seen {
                audio_underruns_seen = underruns;
                format!("  (+{} audio underruns)", underruns)
            } else {
                String::new()
            };
            let _ = canvas.window_mut().set_title(&format!(
                "{window_title}   {:.1}/{} fps{}",
                stats.emulated_fps,
                if opts.pal { 50.0 } else { FRAME_RATE_NTSC },
                extra,
            ));
        }

        // The emulator thread paces the machine; the window only needs to not
        // spin. Vsync would do this for us but a tiny sleep works on every
        // driver, including ones presenting without a swap interval.
        std::thread::sleep(Duration::from_millis(1));
    }

    emu.stop();
    if let (Some(path), Some(image)) = (&opts.nvram, emu.take_nvram()) {
        let _ = std::fs::write(path, &image);
    }
    Ok(())
}

fn main() {
    let opts = match parse_options() {
        Ok(o) => o,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "{e}");
            std::process::exit(if e.starts_with("usage") { 0 } else { 2 });
        }
    };
    if let Err(e) = run(&opts) {
        let _ = writeln!(std::io::stderr(), "retro3do-sdl: {e}");
        std::process::exit(1);
    }
}
