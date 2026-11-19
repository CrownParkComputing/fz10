//! Audio ring and control pad: contracts between two threads.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::panic
)]

use retro3do::audio_ring::{AudioRing, StereoSample};
use retro3do::pad::{Joypad, PadButton, PadState, MAX_PADS};
use retro3do::pbus::Pbus;
use retro3do::Console;

fn sample_of(value: i32) -> StereoSample {
    StereoSample::new(value as i16, (-value) as i16)
}

// --- audio ring -----------------------------------------------------------

#[test]
fn samples_come_back_in_the_order_they_went_in() {
    let ring = AudioRing::new();
    let input: Vec<_> = (1..=8).map(sample_of).collect();
    assert_eq!(ring.push(&input), 8);
    assert_eq!(ring.available(), 8);
    let mut out = [StereoSample::default(); 8];
    assert_eq!(ring.pull(&mut out), 8);
    for (i, s) in out.iter().enumerate() {
        assert_eq!(s.left, (i + 1) as i16);
        assert_eq!(s.right, -((i + 1) as i16));
    }
    assert!(ring.is_empty());
}

#[test]
fn an_underrun_gives_silence_not_a_repeated_sample() {
    let ring = AudioRing::new();
    ring.push(&[sample_of(100), sample_of(200)]);
    let mut out = [sample_of(999); 6];
    assert_eq!(ring.pull(&mut out), 2);
    assert_eq!(out[0].left, 100);
    assert_eq!(out[1].left, 200);
    for s in &out[2..] {
        assert_eq!(*s, StereoSample::SILENCE);
    }
    assert_eq!(ring.underruns(), 1);
}

#[test]
fn a_full_ring_refuses_rather_than_blocking() {
    let ring = AudioRing::new();
    let huge = vec![sample_of(7); AudioRing::CAPACITY as usize + 100];
    assert_eq!(ring.push(&huge), AudioRing::CAPACITY);
    assert_eq!(ring.refused(), 100);
}

#[test]
fn the_ring_keeps_working_across_the_wrap() {
    let ring = AudioRing::new();
    let mut out = [StereoSample::default(); 1];
    for i in 0..AudioRing::CAPACITY * 3 {
        let value = (i & 0x7fff) as i32;
        assert_eq!(ring.push(&[sample_of(value)]), 1);
        assert_eq!(ring.pull(&mut out), 1);
        assert_eq!(out[0].left, value as i16);
    }
    assert_eq!(ring.underruns(), 0);
}

#[test]
fn a_producer_and_consumer_on_two_threads_lose_nothing() {
    const TOTAL: u32 = 200_000;
    let ring = AudioRing::new();
    let mut received = Vec::with_capacity(TOTAL as usize);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut sent = 0u32;
            while sent < TOTAL {
                let want = (TOTAL - sent).min(64);
                let chunk: Vec<_> = (0..want)
                    .map(|i| sample_of(((sent + i) & 0x7fff) as i32))
                    .collect();
                let wrote = ring.push(&chunk);
                sent += wrote;
                if wrote < want {
                    std::thread::yield_now();
                }
            }
        });
        let mut chunk = [StereoSample::default(); 64];
        while received.len() < TOTAL as usize {
            let got = ring.pull(&mut chunk) as usize;
            for s in chunk.iter().take(got) {
                if received.len() < TOTAL as usize {
                    received.push(*s);
                }
            }
            if got == 0 {
                std::thread::yield_now();
            }
        }
    });
    assert_eq!(received.len(), TOTAL as usize);
    assert!(received
        .iter()
        .enumerate()
        .all(|(i, s)| s.left == (i as u32 & 0x7fff) as i16));
}

#[test]
fn interleaved_pulls_fill_left_then_right_and_pad_with_silence() {
    let ring = AudioRing::new();
    ring.push(&[sample_of(5), sample_of(6)]);
    let mut out = [0x7fff_i16; 8];
    assert_eq!(ring.pull_interleaved(&mut out), 2);
    assert_eq!(out, [5, -5, 6, -6, 0, 0, 0, 0]);
}

#[test]
fn a_frame_of_emulation_produces_a_frame_of_audio() {
    let mut console = Console::new();
    console.reset();
    console.run_frame();
    let available = console.audio().available();
    assert!((700..=750).contains(&available), "{available}");
}

#[test]
fn an_arm_boost_does_not_speed_up_audio_or_video_time() {
    let run = |percent: u32| {
        let mut console = Console::new();
        console.reset();
        console.set_cpu_scale_percent(percent);
        console.system_mut().write32(0x8000, 0xeaff_fffe); // branch to itself
        console.cpu_mut().set_reg(15, 0x8000);
        let before = console.cpu().total_cycles();
        console.run_frame();
        (
            console.cpu().total_cycles() - before,
            console.audio().available(),
        )
    };
    let (native_cycles, native_samples) = run(100);
    let (boosted_cycles, boosted_samples) = run(150);
    assert!(boosted_cycles * 100 >= native_cycles * 145);
    assert!(boosted_cycles * 100 <= native_cycles * 155);
    assert_eq!(boosted_samples, native_samples);
}

// --- control pad ----------------------------------------------------------

#[test]
fn a_button_press_is_visible_and_release_clears_it() {
    let pads = PadState::new();
    assert!(!pads.pressed(0, PadButton::A));
    pads.press(0, PadButton::A, true);
    assert!(pads.pressed(0, PadButton::A));
    pads.press(0, PadButton::A, false);
    assert!(!pads.pressed(0, PadButton::A));
}

#[test]
fn buttons_from_different_sources_do_not_clobber_each_other() {
    let pads = PadState::new();
    pads.press(0, PadButton::Up, true);
    pads.press(0, PadButton::Left, true);
    assert!(pads.pressed(0, PadButton::Up));
    assert!(pads.pressed(0, PadButton::Left));
    pads.press(0, PadButton::Up, false);
    assert!(!pads.pressed(0, PadButton::Up));
    assert!(pads.pressed(0, PadButton::Left));
}

#[test]
fn pads_are_independent() {
    let pads = PadState::new();
    pads.set_connected(1, true);
    pads.press(0, PadButton::A, true);
    pads.press(1, PadButton::B, true);
    assert!(pads.pressed(0, PadButton::A));
    assert!(!pads.pressed(0, PadButton::B));
    assert!(pads.pressed(1, PadButton::B));
    assert!(!pads.pressed(1, PadButton::A));
}

#[test]
fn one_pad_is_connected_by_default_as_a_machine_ships() {
    let pads = PadState::new();
    assert!(pads.connected(0));
    assert!(!pads.connected(1));
    assert_eq!(pads.connected_count(), 1);
    pads.set_connected(1, true);
    assert_eq!(pads.connected_count(), 2);
}

#[test]
fn disconnecting_a_pad_releases_its_buttons() {
    let pads = PadState::new();
    pads.set_connected(1, true);
    pads.press(1, PadButton::Right, true);
    assert!(pads.pressed(1, PadButton::Right));
    pads.set_connected(1, false);
    assert!(!pads.pressed(1, PadButton::Right));
}

#[test]
fn out_of_range_pads_are_ignored_rather_than_corrupting_memory() {
    let pads = PadState::new();
    pads.press(MAX_PADS, PadButton::A, true);
    pads.press(999, PadButton::A, true);
    assert!(!pads.pressed(MAX_PADS, PadButton::A));
    assert_eq!(pads.buttons(999), 0);
}

// --- from the host's buttons to the machine -------------------------------

#[test]
fn a_press_reaches_the_machine_and_not_merely_the_pad_state() {
    let console = Console::new();
    console.pads().press(0, PadButton::A, true);
    console.pads().press(0, PadButton::Right, true);
    let seen = console.joypad();
    assert!(seen.held(PadButton::A));
    assert!(seen.held(PadButton::Right));
    assert!(!seen.held(PadButton::B));
    assert!(!seen.held(PadButton::Left));

    console.pads().press(0, PadButton::A, false);
    assert!(!console.joypad().held(PadButton::A));
    assert!(console.joypad().held(PadButton::Right));
}

#[test]
fn setting_pad_one_directly_lands_in_the_same_place_the_front_end_writes() {
    let console = Console::new();
    console.set_joypad(Joypad::default().with(PadButton::Play, true));
    assert!(console.pads().pressed(0, PadButton::Play));
    assert!(console.joypad().held(PadButton::Play));
}

#[test]
fn every_attached_pad_is_chained_onto_the_one_stream() {
    let mut bus = Pbus::new();
    bus.begin();
    bus.add_joypad(Joypad::default().with(PadButton::A, true));
    bus.add_joypad(Joypad::default().with(PadButton::B, true));
    let data = bus.data();
    assert!(data.len() >= 4);
    assert_eq!(data[0] & 0x80, 0x80);
    assert_eq!(data[0] & 0x01, 0x01);
    assert_eq!(data[2] & 0x80, 0x80);
    assert_eq!(data[2] & 0x01, 0x00);
    assert_eq!(data[3] & 0x80, 0x80);
}

#[test]
fn every_button_lands_on_its_wire_bit() {
    // The two report bytes, button by button, so a remapped control shows up
    // here rather than in a game.
    let cases = [
        (PadButton::A, 0, 0x01),
        (PadButton::Left, 0, 0x02),
        (PadButton::Right, 0, 0x04),
        (PadButton::Up, 0, 0x08),
        (PadButton::Down, 0, 0x10),
        (PadButton::LeftShift, 1, 0x04),
        (PadButton::RightShift, 1, 0x08),
        (PadButton::Stop, 1, 0x10),
        (PadButton::Play, 1, 0x20),
        (PadButton::C, 1, 0x40),
        (PadButton::B, 1, 0x80),
    ];
    for (button, byte, bit) in cases {
        let mut bus = Pbus::new();
        bus.add_joypad(Joypad::default().with(button, true));
        let report = bus.data();
        assert_eq!(report[byte] & bit, bit, "{button:?}");
        let other = 1 - byte;
        let expected_other = if other == 0 { 0x80 } else { 0 };
        assert_eq!(report[other], expected_other, "{button:?}");
    }
    let mut empty = Pbus::new();
    empty.pad();
    assert!(empty.data().iter().all(|&b| b == 0xff));
}
