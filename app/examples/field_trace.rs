//! Per-field fingerprints: `field cycles pc frame-hash samples audio-hash dma`.
//!
//! `field_trace BIOS FIELDS [DISC] [PAD_FROM] [SCALE]`: boot, optionally press
//! A/Play from field PAD_FROM on, and print one line per field. Two runs that
//! print the same lines produced the same pictures and sound. SCALE is the
//! internal cel resolution (default one); the frame hash then covers the
//! hi-res output.
use retro3do::audio_ring::StereoSample;
use retro3do::pad::{Joypad, PadButton};
use retro3do::Console;

fn fnv(bytes: impl Iterator<Item = u8>) -> u64 {
    bytes.fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |i: usize| args.get(i).map(String::as_str).unwrap_or("");
    let usage = "usage: field_trace BIOS FIELDS [DISC] [PAD_FROM] [SCALE]";
    let mut console = Console::new();
    if !console.load_bios(arg(1)) {
        return Err(format!("{usage}: {}", console.last_error()));
    }
    if !arg(3).is_empty() && !console.load_disc(arg(3)) {
        return Err(console.last_error().to_owned());
    }
    let fields: u32 = arg(2).parse().map_err(|_| usage.to_owned())?;
    let pad_from: i64 = arg(4).parse().unwrap_or(-1);
    let scale: u32 = arg(5).parse().unwrap_or(1);
    console.set_internal_scale(scale);
    console.reset();

    let mut samples = vec![StereoSample::default(); 4096];
    for field in 0..fields {
        if pad_from >= 0 {
            let f = i64::from(field);
            console.set_joypad(
                Joypad::default()
                    .with(PadButton::A, f >= pad_from && (f / 10) % 2 == 0)
                    .with(PadButton::Play, f >= pad_from && (f / 25) % 2 == 1),
            );
        }
        let cycles = console.run_frame();
        let frame = console.framebuffer();
        let frame_hash = fnv(frame.pixels.iter().flat_map(|p| p.to_le_bytes()));
        let pulled = console.audio().pull(&mut samples);
        let audio_hash = fnv(samples
            .iter()
            .take(pulled as usize)
            .flat_map(|s| [s.left.to_le_bytes(), s.right.to_le_bytes()].concat()));
        println!(
            "{field} {cycles} {:08x} {frame_hash:016x} {pulled} {audio_hash:016x} {}",
            console.cpu().pc(),
            console.expansion_dma_count()
        );
    }
    Ok(())
}
