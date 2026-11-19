//! The C ABI, exercised the way an application sees it: only the `retro3do_*`
//! entry points, through raw pointers. Calling C functions is `unsafe` by
//! nature, so this test file (not the library) uses it.
#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::panic
)]

use std::ffi::CStr;
use std::ptr;
use std::time::{Duration, Instant};

use retro3do::ffi::*;

/// A machine with the built-in demonstration ROM in it.
struct Machine(*mut retro3do_t);

impl Machine {
    fn new() -> Self {
        let emu = retro3do_create();
        assert!(!emu.is_null());
        unsafe { retro3do_load_demo(emu) };
        Self(emu)
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        unsafe { retro3do_destroy(self.0) };
    }
}

fn stats(emu: *const retro3do_t) -> retro3do_stats {
    let mut out = retro3do_stats::default();
    unsafe { retro3do_get_stats(emu, &mut out) };
    out
}

/// Wait for the emulator thread to reach a condition, sleeping rather than
/// spinning, and give up rather than hang the run.
fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    condition()
}

fn empty_frame() -> retro3do_frame {
    retro3do_frame {
        pixels: ptr::null(),
        width: 0,
        height: 0,
    }
}

const PAD_UP: i32 = 0;
const PAD_LEFT: i32 = 2;
const PAD_RIGHT: i32 = 3;
const PAD_A: i32 = 4;
const PAD_B: i32 = 5;

#[test]
fn the_built_library_reports_the_abi_version_its_header_declares() {
    assert_eq!(retro3do_abi_version(), RETRO3DO_ABI_VERSION);
    assert_eq!(RETRO3DO_ABI_VERSION, 1);
}

#[test]
fn every_entry_point_tolerates_a_null_machine() {
    let null = ptr::null_mut();
    unsafe {
        retro3do_destroy(null);
        retro3do_stop(null);
        retro3do_eject_disc(null);
        retro3do_set_paused(null, 1);
        retro3do_request_reset(null);
        retro3do_reset(null);
        assert!(!retro3do_last_error(null).is_null());
        assert_eq!(retro3do_bios_loaded(null), 0);
        assert_eq!(retro3do_running(null), 0);
        assert_eq!(retro3do_run_frame(null), 0);
        assert_eq!(retro3do_audio_available(null), 0);
        assert_eq!(retro3do_pad_get_buttons(null, 0), 0);
    }
    assert_eq!(stats(ptr::null()).frames_emulated, 0);
}

#[test]
fn a_new_machine_has_no_media_and_no_error_to_report() {
    let machine = Machine::new();
    unsafe {
        assert_eq!(retro3do_disc_loaded(machine.0), 0);
        assert_eq!(retro3do_demo_loaded(machine.0), 1);
        let mut info = retro3do_disc_info {
            path: ptr::null(),
            is_chd: 0,
            sector_count: 0,
            track_count: 0,
        };
        assert_eq!(retro3do_get_disc_info(machine.0, &mut info), 0);
        assert_eq!(
            CStr::from_ptr(retro3do_last_error(machine.0)).to_bytes(),
            b""
        );
    }
}

#[test]
fn a_load_that_fails_says_why_in_words() {
    let machine = Machine::new();
    unsafe {
        assert_eq!(
            retro3do_load_disc(machine.0, c"/nonexistent/not-a-disc.iso".as_ptr()),
            0
        );
        let error = retro3do_last_error(machine.0);
        assert!(!error.is_null());
        assert!(!CStr::from_ptr(error).to_bytes().is_empty());
    }
}

#[test]
fn running_a_frame_by_hand_produces_a_frame_to_draw() {
    let machine = Machine::new();
    let mut frame = empty_frame();
    unsafe {
        assert_eq!(retro3do_current_frame(machine.0, &mut frame), 0);
        assert!(retro3do_run_frame(machine.0) > 0);
        assert_eq!(retro3do_acquire_frame(machine.0, &mut frame), 1);
        assert!(!frame.pixels.is_null());
        assert_eq!((frame.width, frame.height), (640, 480));
        // Nothing new since: keep the texture rather than re-upload it.
        assert_eq!(retro3do_acquire_frame(machine.0, &mut frame), 0);
        // But the last frame is still there for whoever must draw now.
        assert_eq!(retro3do_current_frame(machine.0, &mut frame), 1);
        assert!(!frame.pixels.is_null());
    }
}

#[test]
fn the_internal_scale_is_clamped_and_multiplies_the_frame() {
    let machine = Machine::new();
    let mut frame = empty_frame();
    unsafe {
        assert_eq!(retro3do_get_internal_scale(machine.0), 1);
        retro3do_set_internal_scale(machine.0, 0);
        assert_eq!(retro3do_get_internal_scale(machine.0), 1);
        retro3do_set_internal_scale(machine.0, 8);
        assert_eq!(retro3do_get_internal_scale(machine.0), 4);
        assert_eq!(retro3do_get_internal_scale(ptr::null()), 1);
        retro3do_set_internal_scale(ptr::null_mut(), 2);

        for (scale, width, height) in [(2, 1280, 960), (3, 1920, 1440), (1, 640, 480)] {
            retro3do_set_internal_scale(machine.0, scale);
            assert!(retro3do_run_frame(machine.0) > 0);
            assert_eq!(retro3do_acquire_frame(machine.0, &mut frame), 1);
            assert_eq!(
                (frame.width, frame.height),
                (width, height),
                "scale {scale}"
            );
            assert!(!frame.pixels.is_null());
        }
    }
}

#[test]
fn the_internal_scale_can_change_while_the_thread_runs() {
    let machine = Machine::new();
    let mut frame = empty_frame();
    unsafe {
        retro3do_start(machine.0);
        for scale in [2u32, 4, 1, 3] {
            retro3do_set_internal_scale(machine.0, scale);
            let expected = (640 * scale as i32, 480 * scale as i32);
            // Every frame handed out is whole: its size matches its slot.
            assert!(wait_until(|| {
                retro3do_acquire_frame(machine.0, &mut frame) == 1
                    && (frame.width, frame.height) == expected
            }));
            let pixels =
                std::slice::from_raw_parts(frame.pixels, (frame.width * frame.height) as usize);
            assert!(pixels.iter().all(|&p| p >> 24 == 0xff));
        }
        retro3do_stop(machine.0);
    }
}

#[test]
fn a_pal_machine_produces_a_taller_picture() {
    let machine = Machine::new();
    let mut frame = empty_frame();
    unsafe {
        retro3do_set_region(machine.0, RETRO3DO_REGION_PAL);
        assert_eq!(retro3do_get_region(machine.0), RETRO3DO_REGION_PAL);
        retro3do_run_frame(machine.0);
        assert_eq!(retro3do_acquire_frame(machine.0, &mut frame), 1);
        assert_eq!((frame.width, frame.height), (640, 576));
    }
}

#[test]
fn the_emulator_thread_runs_frames_and_stops_cleanly() {
    let machine = Machine::new();
    unsafe {
        assert_eq!(retro3do_running(machine.0), 0);
        retro3do_start(machine.0);
        assert_eq!(retro3do_running(machine.0), 1);
        assert!(wait_until(|| stats(machine.0).frames_emulated > 0));
        retro3do_stop(machine.0);
        assert_eq!(retro3do_running(machine.0), 0);
    }
    // Stopping keeps the machine and what it counted.
    assert!(stats(machine.0).frames_emulated > 0);
}

#[test]
fn a_paused_machine_stops_emulating_and_resumes_where_it_was() {
    let machine = Machine::new();
    unsafe {
        retro3do_start(machine.0);
        assert!(wait_until(|| stats(machine.0).frames_emulated > 0));
        retro3do_set_paused(machine.0, 1);
        assert_eq!(retro3do_paused(machine.0), 1);
    }
    // Let a frame already in flight finish before taking the reading.
    std::thread::sleep(Duration::from_millis(60));
    let before = stats(machine.0);
    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(stats(machine.0).frames_emulated, before.frames_emulated);

    unsafe { retro3do_set_paused(machine.0, 0) };
    assert!(wait_until(
        || stats(machine.0).frames_emulated > before.frames_emulated
    ));
    unsafe { retro3do_stop(machine.0) };
}

#[test]
fn running_a_frame_by_hand_is_refused_while_the_thread_owns_the_machine() {
    let machine = Machine::new();
    unsafe {
        retro3do_start(machine.0);
        assert_eq!(retro3do_run_frame(machine.0), 0);
        retro3do_stop(machine.0);
        assert!(retro3do_run_frame(machine.0) > 0);
    }
}

#[test]
fn a_pad_reports_back_exactly_what_was_pressed() {
    let machine = Machine::new();
    unsafe {
        retro3do_pad_press(machine.0, 0, PAD_A, 1);
        retro3do_pad_press(machine.0, 0, PAD_LEFT, 1);
        assert_eq!(retro3do_pad_pressed(machine.0, 0, PAD_A), 1);
        assert_eq!(retro3do_pad_pressed(machine.0, 0, PAD_B), 0);
        assert_eq!(
            retro3do_pad_get_buttons(machine.0, 0),
            (1 << PAD_A) | (1 << PAD_LEFT)
        );

        retro3do_pad_press(machine.0, 0, PAD_A, 0);
        assert_eq!(retro3do_pad_pressed(machine.0, 0, PAD_A), 0);

        let diagonal = (1 << PAD_UP) | (1 << PAD_RIGHT);
        retro3do_pad_set_buttons(machine.0, 0, diagonal);
        assert_eq!(retro3do_pad_get_buttons(machine.0, 0), diagonal);
        retro3do_pad_set_buttons(machine.0, 0, 0);
        assert_eq!(retro3do_pad_get_buttons(machine.0, 0), 0);
    }
}

#[test]
fn a_pad_index_off_the_end_of_the_chain_is_ignored_not_written() {
    let machine = Machine::new();
    unsafe {
        retro3do_pad_press(machine.0, RETRO3DO_MAX_PADS, PAD_A, 1);
        retro3do_pad_set_buttons(machine.0, RETRO3DO_MAX_PADS + 100, 0xFFFF_FFFF);
        assert_eq!(retro3do_pad_pressed(machine.0, RETRO3DO_MAX_PADS, PAD_A), 0);
        assert_eq!(retro3do_pad_get_buttons(machine.0, 0), 0);
        // An out-of-range button is ignored too.
        retro3do_pad_press(machine.0, 0, RETRO3DO_PAD_COUNT, 1);
        retro3do_pad_press(machine.0, 0, -1, 1);
        assert_eq!(retro3do_pad_get_buttons(machine.0, 0), 0);
    }
}

#[test]
fn one_pad_is_attached_as_a_machine_ships() {
    let machine = Machine::new();
    unsafe {
        assert_eq!(retro3do_pad_connected(machine.0, 0), 1);
        assert_eq!(retro3do_pad_count(machine.0), 1);
        retro3do_pad_connect(machine.0, 1, 1);
        assert_eq!(retro3do_pad_count(machine.0), 2);
        retro3do_pad_connect(machine.0, 1, 0);
        assert_eq!(retro3do_pad_count(machine.0), 1);
    }
}

#[test]
fn pulling_from_a_silent_machine_gives_silence_and_admits_it() {
    let machine = Machine::new();
    let mut buffer = [0x7FFF_i16; 64];
    unsafe {
        assert_eq!(retro3do_audio_pull(machine.0, buffer.as_mut_ptr(), 32), 0);
        assert!(retro3do_audio_underruns(machine.0) > 0);
    }
    assert!(buffer.iter().all(|&s| s == 0));
}

#[test]
fn the_cpu_scale_is_clamped_to_a_machine_that_still_works() {
    let machine = Machine::new();
    unsafe {
        assert_eq!(retro3do_get_cpu_scale_percent(machine.0), 100);
        retro3do_set_cpu_scale_percent(machine.0, 130);
        assert_eq!(retro3do_get_cpu_scale_percent(machine.0), 130);
        retro3do_set_cpu_scale_percent(machine.0, 0);
        assert_eq!(retro3do_get_cpu_scale_percent(machine.0), 100);
        retro3do_set_cpu_scale_percent(machine.0, 100_000);
        assert_eq!(retro3do_get_cpu_scale_percent(machine.0), 150);
    }
}

#[test]
fn nvram_is_only_handed_out_when_the_machine_has_written_to_it() {
    let machine = Machine::new();
    unsafe {
        let size = retro3do_nvram_size(machine.0);
        assert!(size > 0);
        let mut save = vec![0u8; size as usize];
        let mut written = 0u32;
        // Nothing has saved a game, so there is nothing to persist.
        assert_eq!(
            retro3do_take_nvram(machine.0, save.as_mut_ptr(), size, &mut written),
            0
        );
        assert_eq!(retro3do_restore_nvram(machine.0, save.as_ptr(), size), 1);
    }
}

#[test]
fn the_thread_hands_out_a_consistent_nvram_image_once() {
    // The machine comes up with a freshly formatted NVRAM, which counts as a
    // write: the first frame boundary on the thread snapshots it, once.
    let machine = Machine::new();
    unsafe {
        let size = retro3do_nvram_size(machine.0);
        let mut save = vec![0u8; size as usize];
        let mut written = 0u32;
        retro3do_start(machine.0);
        assert!(wait_until(|| stats(machine.0).frames_emulated > 1));
        retro3do_stop(machine.0);
        assert_eq!(
            retro3do_take_nvram(machine.0, save.as_mut_ptr(), size, &mut written),
            1
        );
        assert_eq!(written, size);
        assert_eq!(&save[..6], &[0x01, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a]);
        assert_eq!(
            retro3do_take_nvram(machine.0, save.as_mut_ptr(), size, &mut written),
            0
        );
    }
}
