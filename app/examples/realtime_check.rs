//! Does the paced emulator thread keep real time at an internal scale?
//!
//! `realtime_check BIOS DISC SCALE WARMUP_SECONDS MEASURE_SECONDS`
//!
//! Starts the threaded machine (the same pacing every front end gets), holds
//! A and taps Play to get into play, waits WARMUP_SECONDS, then reports the
//! emulated fields per second, how many fields finished after their deadline
//! and how often the pacer gave up and reset its deadline, over the next
//! MEASURE_SECONDS. Frames are taken from the mailbox as a front end would.
use std::time::{Duration, Instant};

use retro3do::pad::PadButton;
use retro3do::Retro3do;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |i: usize| args.get(i).map(String::as_str).unwrap_or("");
    let usage = "usage: realtime_check BIOS DISC SCALE WARMUP_SECONDS MEASURE_SECONDS";
    let number = |i: usize| arg(i).parse::<u64>().map_err(|_| usage.to_owned());
    let (scale, warmup, measure) = (number(3)?, number(4)?, number(5)?);

    let mut emu = Retro3do::new();
    emu.load_bios(arg(1))?;
    emu.load_disc(arg(2))?;
    emu.set_internal_scale(scale as u32);
    if std::env::var_os("RETRO3DO_FIXED_SCALE").is_some() {
        emu.set_adaptive_scale(false);
    }
    emu.start();

    let pump = |until: Instant| {
        let mut tick = 0u64;
        while Instant::now() < until {
            emu.pads()
                .press(0, PadButton::A, (tick / 10).is_multiple_of(2));
            emu.pads().press(0, PadButton::Play, (tick / 25) % 2 == 1);
            emu.acquire_frame(|_, _, _| ());
            std::thread::sleep(Duration::from_millis(16));
            tick += 1;
        }
    };
    pump(Instant::now() + Duration::from_secs(warmup));
    let before = emu.stats();
    let started = Instant::now();
    pump(started + Duration::from_secs(measure));
    let after = emu.stats();
    let seconds = started.elapsed().as_secs_f64();
    emu.stop();

    println!(
        "effective scale at the end: {}x (asked {scale}x)",
        emu.effective_internal_scale()
    );
    println!(
        "scale {scale}: {:.2} fields/s, {} late fields, {} deadline resets, last frame {:.2} ms (smoothed)",
        (after.frames_emulated - before.frames_emulated) as f64 / seconds,
        after.frames_behind - before.frames_behind,
        after.deadline_resets - before.deadline_resets,
        after.frame_ms
    );
    Ok(())
}
