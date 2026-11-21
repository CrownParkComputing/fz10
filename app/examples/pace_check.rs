//! Measures what the user feels: when frames leave the core and whether the
//! audio ring ever starves.
//!
//! `pace_check BIOS DISC SCALE WARMUP_SECONDS MEASURE_SECONDS [PRESS]`
//!
//! One thread timestamps every mailbox publish the instant it lands (a spin on
//! the published counter, ~µs accurate); another drains the audio ring the way
//! a 44.1 kHz device would. Reports publish-interval statistics, late/deadline
//! counters, effective scale, and ring occupancy percentiles.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use retro3do::audio_ring::{StereoSample, SAMPLE_RATE};
use retro3do::pad::PadButton;
use retro3do::Retro3do;

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |i: usize| args.get(i).map(String::as_str).unwrap_or("");
    let usage =
        "usage: pace_check BIOS DISC SCALE WARMUP_SECONDS MEASURE_SECONDS [PRESS]";
    let number = |i: usize| arg(i).parse::<u64>().map_err(|_| usage.to_owned());
    let (scale, warmup, measure) = (number(3)?, number(4)?, number(5)?);
    let press = !arg(6).is_empty() && arg(6) != "0";

    let mut emu = Retro3do::new();
    if !arg(1).is_empty() && arg(1) != "-" {
        emu.load_bios(arg(1))?;
    }
    if !arg(2).is_empty() && arg(2) != "-" {
        emu.load_disc(arg(2))?;
    }
    emu.set_internal_scale(scale as u32);
    if std::env::var_os("RETRO3DO_FIXED_SCALE").is_some() {
        emu.set_adaptive_scale(false);
    }
    emu.start();

    let stop = Arc::new(AtomicBool::new(false));
    let mut publish_times: Vec<u64> = Vec::new();
    let mut occupancy: Vec<u32> = Vec::new();

    std::thread::scope(|scope| {
        // Publisher-side view: timestamp each publish the moment the counter moves.
        let stop2 = Arc::clone(&stop);
        let emu_ref = &emu;
        let publish_handle = scope.spawn(move || {
            let mut times = Vec::new();
            let mut seen = 0u64;
            let t0 = Instant::now();
            while !stop2.load(Ordering::Relaxed) {
                let n = emu_ref.mailbox().published();
                if n != seen {
                    seen = n;
                    times.push(t0.elapsed().as_nanos() as u64);
                }
            }
            times
        });

        // Device-side audio drain: pull 44.1 kHz stereo on a timer, like hardware.
        let stop2 = Arc::clone(&stop);
        let audio_handle = scope.spawn(move || {
            let ring = emu_ref.audio();
            let mut seen = Vec::new();
            let mut buf = vec![StereoSample::SILENCE; 2048];
            let mut next = Instant::now();
            let quantum = 512usize; // ~11.6 ms per pull, ALSA-like
            while !stop2.load(Ordering::Relaxed) {
                next += Duration::from_secs_f64(quantum as f64 / SAMPLE_RATE as f64);
                let now = Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                }
                seen.push(ring.available());
                let _ = ring.pull(&mut buf[..quantum]);
            }
            seen
        });

        // Warmup + measure, tapping A/Play so intros and menus advance into FMV.
        let t0 = Instant::now();
        let mut tick = 0u64;
        while t0.elapsed() < Duration::from_secs(warmup + measure) {
            if press {
                emu.pads().press(0, PadButton::A, (tick / 4).is_multiple_of(2));
                emu.pads().press(0, PadButton::Play, (tick / 10) % 2 == 1);
            }
            tick += 1;
            std::thread::sleep(Duration::from_millis(2));
        }
        stop.store(true, Ordering::Relaxed);
        publish_times = publish_handle.join().unwrap_or_default();
        occupancy = audio_handle.join().unwrap_or_default();
    });

    let stats = emu.stats();
    let underruns = emu.audio().underruns();
    let refused = emu.audio().refused();

    let warm_ns = warmup * 1_000_000_000;
    let mut iv: Vec<f64> = publish_times
        .windows(2)
        .filter(|w| w[0] >= warm_ns)
        .map(|w| (w[1] - w[0]) as f64 / 1e6)
        .collect();
    iv.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = iv.len() as f64;
    let mean = iv.iter().sum::<f64>() / n.max(1.0);
    let sd = (iv.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n.max(1.0)).sqrt();
    let out = iv.iter().filter(|&&v| !(13.0..=20.0).contains(&v)).count();

    let mut occ: Vec<f64> = occupancy.iter().map(|&v| v as f64).collect();
    occ.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    println!("== publish intervals (mailbox, ms; NTSC field = 16.667) ==");
    println!(
        "n={} mean={mean:.3} sd={sd:.3} p1={:.3} p50={:.3} p99={:.3} min={:.3} max={:.3} out-of-[13,20]ms={out}",
        iv.len(),
        pct(&iv, 0.01),
        pct(&iv, 0.50),
        pct(&iv, 0.99),
        iv.first().copied().unwrap_or(0.0),
        iv.last().copied().unwrap_or(0.0),
    );
    println!("== core stats (whole run) ==");
    println!(
        "emulated_fps={:.2} late_fields={} deadline_resets={} frame_ms={:.2} effective_scale={}",
        stats.emulated_fps,
        stats.frames_behind,
        stats.deadline_resets,
        stats.frame_ms,
        emu.effective_internal_scale()
    );
    println!("== audio ring (pairs, cap 32768) ==");
    println!(
        "occupancy p5={:.0} p50={:.0} p95={:.0} max={:.0}  underruns={underruns} refused={refused}",
        pct(&occ, 0.05),
        pct(&occ, 0.50),
        pct(&occ, 0.95),
        occ.last().copied().unwrap_or(0.0)
    );
    Ok(())
}
