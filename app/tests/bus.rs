//! Memory map and byte order, plus the console-level checks that ride on them.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::panic
)]

use retro3do::audio_ring::StereoSample;
use retro3do::memory::{Memory, RomError, NVRAM_BASE, ROM_BASE, ROM_SIZE, VRAM_BASE};
use retro3do::pad::{Joypad, PadButton};
use retro3do::video::framebuffer_offset;
use retro3do::{Console, Region};

#[test]
fn words_are_stored_big_endian() {
    let mut bus = Memory::new();
    bus.write32(0x100, 0x1122_3344);
    assert_eq!(bus.read8(0x100), 0x11);
    assert_eq!(bus.read8(0x101), 0x22);
    assert_eq!(bus.read8(0x102), 0x33);
    assert_eq!(bus.read8(0x103), 0x44);
    assert_eq!(bus.read32(0x100), 0x1122_3344);
}

#[test]
fn halfwords_are_stored_big_endian() {
    let mut bus = Memory::new();
    bus.write16(0x200, 0xABCD);
    assert_eq!(bus.read8(0x200), 0xAB);
    assert_eq!(bus.read8(0x201), 0xCD);
    assert_eq!(bus.read16(0x200), 0xABCD);
}

#[test]
fn word_accesses_ignore_the_low_address_bits() {
    let mut bus = Memory::new();
    bus.write32(0x300, 0xCAFE_BABE);
    for address in 0x301..=0x303 {
        assert_eq!(bus.read32(address), 0xCAFE_BABE);
    }
}

#[test]
fn dram_and_vram_are_separate() {
    let mut bus = Memory::new();
    bus.write32(0x400, 1);
    bus.write32(VRAM_BASE + 0x400, 2);
    assert_eq!(bus.read32(0x400), 1);
    assert_eq!(bus.read32(VRAM_BASE + 0x400), 2);
}

#[test]
fn rom_ignores_writes() {
    let mut bus = Memory::new();
    assert!(bus.load_rom(&[0xDE, 0xAD, 0xBE, 0xEF, 0, 0, 0, 0]).is_ok());
    bus.write32(ROM_BASE, 0);
    assert_eq!(bus.read32(ROM_BASE), 0xDEAD_BEEF);
}

#[test]
fn an_oversized_bios_is_refused() {
    let mut bus = Memory::new();
    assert_eq!(
        bus.load_rom(&vec![0; ROM_SIZE as usize + 4]),
        Err(RomError::TooLarge)
    );
    assert!(!bus.bios_loaded());
}

#[test]
fn a_reset_clears_ram_but_keeps_the_bios() {
    let mut bus = Memory::new();
    assert!(bus.load_rom(&[0x12, 0x34, 0x56, 0x78]).is_ok());
    bus.write32(0x500, 0xFFFF_FFFF);
    bus.reset();
    assert_eq!(bus.read32(0x500), 0);
    assert_eq!(bus.read32(ROM_BASE), 0x1234_5678);
    assert!(bus.bios_loaded());
}

#[test]
fn a_console_without_a_bios_reports_why() {
    let mut console = Console::new();
    assert!(!console.bios_loaded());
    assert!(!console.load_bios("/definitely/not/a/real/path.rom"));
    assert!(!console.last_error().is_empty());
}

#[test]
fn the_builtin_demo_is_an_original_executable_rom() {
    let mut console = Console::new();
    assert!(console.load_builtin_demo());
    assert!(console.bios_loaded());
    assert!(console.demo_loaded());
    assert_eq!(console.system_mut().read32(ROM_BASE), 0xea00_0006);

    // It really executes through the ARM and the bus: within a few fields it
    // installs a display list, starts the DSP and draws something not black.
    for _ in 0..30 {
        console.run_frame();
    }
    assert!(console.cpu().total_instructions() > 1000);
    assert_ne!(console.madam().vdl_address(), 0);
    assert!(console.clio().dsp().running());
    assert!(console.vdlp().entries_walked() > 0);

    let frame = console.framebuffer();
    let has_colour = (0..frame.height).step_by(16).any(|y| {
        (0..frame.width)
            .step_by(16)
            .any(|x| frame.pixels[y * frame.width + x] & 0x00ff_ffff != 0)
    });
    assert!(has_colour);

    // The DSP program produces a real signal, sampled through the app's ring.
    console.audio().reset();
    console.run_frame();
    let mut samples = [StereoSample::default(); 1024];
    let count = console.audio().pull(&mut samples) as usize;
    let real = &samples[..count];
    assert!(real.iter().any(|s| s.left != 0 || s.right != 0));
    assert!(real.windows(2).any(|w| w[0].left != w[1].left));
    assert!(real.iter().all(|s| s.left == s.right));
    assert!(
        real.iter()
            .map(|s| i32::from(s.left).abs())
            .max()
            .unwrap_or(0)
            <= 4096
    );

    // PBUS input is consumed by the ROM itself: holding Right moves its marker
    // in VRAM, which proves the host pad reached emulated software.
    let marker = VRAM_BASE + framebuffer_offset(152, 204, 320);
    let before = console.system_mut().read32(marker);
    console.set_joypad(Joypad::default().with(PadButton::Right, true));
    for _ in 0..3 {
        console.run_frame();
    }
    let after = console.system_mut().read32(marker);
    assert_ne!(before, after);
}

#[test]
fn the_frame_is_the_right_shape_for_the_region() {
    let mut console = Console::new();
    console.set_region(Region::Ntsc);
    let frame = console.framebuffer();
    assert_eq!((frame.width, frame.height), (640, 480));
    assert_eq!(frame.pixels.len(), 640 * 480);

    console.set_region(Region::Pal);
    let frame = console.framebuffer();
    assert_eq!((frame.width, frame.height), (640, 576));
}

#[test]
fn a_reset_preserves_the_selected_pal_geometry_in_madam() {
    let mut console = Console::new();
    console.set_region(Region::Pal);
    console.reset();
    assert_eq!(console.madam().clip_width(), 320);
    assert_eq!(console.madam().clip_height(), 288);
}

#[test]
fn nvram_survives_a_round_trip_through_a_saved_image() {
    let mut bus = Memory::new();
    // Formatting counts as a write, so the first ask hands back the image.
    assert!(bus.nvram_dirty());
    bus.clear_nvram_dirty();
    assert!(!bus.nvram_dirty());

    bus.write32(NVRAM_BASE + 300 * 4, 0xa5);
    assert!(bus.nvram_dirty());
    let saved = bus.nvram().to_vec();

    let mut fresh = Memory::new();
    assert_eq!(fresh.read32(NVRAM_BASE + 300 * 4) & 0xff, 0);
    assert!(fresh.restore_nvram(&saved));
    assert_eq!(fresh.read32(NVRAM_BASE + 300 * 4) & 0xff, 0xa5);
    assert!(!fresh.nvram_dirty());
}

#[test]
fn a_wrong_sized_nvram_image_is_refused_rather_than_padded() {
    let mut bus = Memory::new();
    assert!(!bus.restore_nvram(&[0xff; 16]));
    assert!(!bus.restore_nvram(&[]));
    assert_eq!(bus.read32(NVRAM_BASE) & 0xff, 0x01);
}

#[test]
fn the_nvram_comes_up_formatted_rather_than_blank() {
    let mut bus = Memory::new();
    assert_eq!(bus.read32(NVRAM_BASE) & 0xff, 0x01); // record type
    for i in 1..=5 {
        assert_eq!(bus.read32(NVRAM_BASE + i * 4) & 0xff, 0x5a); // sync
    }
    for (i, &letter) in b"nvram".iter().enumerate() {
        assert_eq!(
            bus.read32(NVRAM_BASE + (40 + i as u32) * 4) & 0xff,
            u32::from(letter)
        );
    }
    let blocks = (0..4).fold(0u32, |acc, i| {
        (acc << 8) | (bus.read32(NVRAM_BASE + (80 + i) * 4) & 0xff)
    });
    assert_eq!(blocks, 32768);

    // A reset does not wipe it, any more than switching a console off does.
    bus.write32(NVRAM_BASE + 200 * 4, 0x5a);
    bus.reset();
    assert_eq!(bus.read32(NVRAM_BASE + 200 * 4) & 0xff, 0x5a);
}

#[test]
fn nvram_is_one_byte_per_word_on_the_low_lane_and_mirrored() {
    let mut bus = Memory::new();
    bus.write8(NVRAM_BASE + 10 * 4 + 3, 0x77);
    assert_eq!(bus.nvram()[10], 0x77);
    // Other byte lanes are not storage.
    bus.write8(NVRAM_BASE + 10 * 4, 0x11);
    assert_eq!(bus.nvram()[10], 0x77);
    assert_eq!(bus.read8(NVRAM_BASE + 10 * 4), 0);
    // The mirror 0x20000 up is the same storage.
    assert_eq!(bus.read32(NVRAM_BASE + 0x20000 + 10 * 4), 0x77);
}

#[test]
fn device_registers_take_byte_and_halfword_accesses_through_the_word() {
    use retro3do::clio::reg;
    use retro3do::memory::CLIO_BASE;
    let mut console = Console::new();
    let sys = console.system_mut();
    sys.write32(CLIO_BASE + reg::AUDIO_IN, 0x1122_3344);
    assert_eq!(sys.read8(CLIO_BASE + reg::AUDIO_IN), 0x11);
    assert_eq!(sys.read8(CLIO_BASE + reg::AUDIO_IN + 3), 0x44);
    assert_eq!(sys.read16(CLIO_BASE + reg::AUDIO_IN + 2), 0x3344);
    sys.write8(CLIO_BASE + reg::AUDIO_IN + 1, 0xaa);
    assert_eq!(sys.read32(CLIO_BASE + reg::AUDIO_IN), 0x11aa_3344);
    sys.write16(CLIO_BASE + reg::AUDIO_IN, 0xbeef);
    assert_eq!(sys.read32(CLIO_BASE + reg::AUDIO_IN), 0xbeef_3344);
}
