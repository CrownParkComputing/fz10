//! The physical gamepad on Android, as a 3DO pad.
//!
//! Android delivers a connected controller as key events (buttons, d-pad) and
//! generic motion events (sticks, triggers). winit drops the ones it has no
//! key for, so `android/src/lib.rs` exports four JNI entry points that the
//! Kotlin-free shim in `android/java` calls, and they land here: this module
//! keeps the held buttons and the axes as plain atomics, and turns them into
//! the same button mask the keyboard and the desktop pads produce.

use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

use retro3do::PadButton;

/// Buttons held down, as `1 << PadButton`.
static HELD: AtomicU32 = AtomicU32::new(0);

/// Stick and trigger positions in thousandths of full deflection.
static AXIS_X: AtomicI32 = AtomicI32::new(0);
static AXIS_Y: AtomicI32 = AtomicI32::new(0);
static AXIS_HAT_X: AtomicI32 = AtomicI32::new(0);
static AXIS_HAT_Y: AtomicI32 = AtomicI32::new(0);
static AXIS_LTRIGGER: AtomicI32 = AtomicI32::new(0);
static AXIS_RTRIGGER: AtomicI32 = AtomicI32::new(0);

/// How far a stick must go before it counts as a d-pad press.
const EDGE: i32 = 500;

/// Android `KeyEvent` codes worth acting on, mapped to 3DO buttons. A
/// controller reports the Xbox-style four (A south, B east, X west, Y north)
/// plus shoulders, start/select and d-pad.
fn key_to_button(code: i32) -> Option<PadButton> {
    Some(match code {
        19 => PadButton::Up,        // KEYCODE_DPAD_UP
        20 => PadButton::Down,      // KEYCODE_DPAD_DOWN
        21 => PadButton::Left,      // KEYCODE_DPAD_LEFT
        22 => PadButton::Right,     // KEYCODE_DPAD_RIGHT
        96 => PadButton::A,         // KEYCODE_BUTTON_A
        97 => PadButton::B,         // KEYCODE_BUTTON_B
        98 => PadButton::C,         // KEYCODE_BUTTON_C
        99 => PadButton::C,         // KEYCODE_BUTTON_X
        100 => PadButton::C,        // KEYCODE_BUTTON_Y
        101 => PadButton::C,        // KEYCODE_BUTTON_Z
        102 => PadButton::LeftShift,  // KEYCODE_BUTTON_L1
        103 => PadButton::RightShift, // KEYCODE_BUTTON_R1
        104 => PadButton::LeftShift,  // KEYCODE_BUTTON_L2
        105 => PadButton::RightShift, // KEYCODE_BUTTON_R2
        108 => PadButton::Play,       // KEYCODE_BUTTON_START
        109 => PadButton::Stop,       // KEYCODE_BUTTON_SELECT
        188 => PadButton::A,          // KEYCODE_BUTTON_1
        189 => PadButton::B,          // KEYCODE_BUTTON_2
        190 => PadButton::C,          // KEYCODE_BUTTON_3
        191 => PadButton::Play,       // KEYCODE_BUTTON_4
        192 => PadButton::Stop,       // KEYCODE_BUTTON_5
        _ => return None,
    })
}

/// A key from the activity, down or up.
pub fn set_key(code: i32, down: bool) {
    let Some(button) = key_to_button(code) else {
        return;
    };
    let bit = 1u32 << (button as u32);
    if down {
        HELD.fetch_or(bit, Ordering::Relaxed);
    } else {
        HELD.fetch_and(!bit, Ordering::Relaxed);
    }
}

/// An axis from the activity, in `-1.0..=1.0` (triggers `0.0..=1.0`). The axis
/// numbers are Android's `MotionEvent.AXIS_*`: the left stick and the hat drive
/// the d-pad, as on the desktop where only the left stick does.
pub fn set_axis(axis: i32, value: f32) {
    let value = (value.clamp(-1.0, 1.0) * 1000.0) as i32;
    let slot = match axis {
        0 => &AXIS_X,         // AXIS_X, left stick across
        1 => &AXIS_Y,         // AXIS_Y, left stick down
        15 => &AXIS_HAT_X,    // AXIS_HAT_X, the d-pad
        16 => &AXIS_HAT_Y,    // AXIS_HAT_Y
        17 => &AXIS_LTRIGGER, // AXIS_LTRIGGER
        18 => &AXIS_RTRIGGER, // AXIS_RTRIGGER
        _ => return,
    };
    slot.store(value, Ordering::Relaxed);
}

/// The pad mask for console pad `index`: the physical controller is pad 0.
pub fn pad_mask(index: usize) -> u32 {
    if index != 0 {
        return 0;
    }
    let mut mask = HELD.load(Ordering::Relaxed);
    let press = |mask: &mut u32, axis: &AtomicI32, low: PadButton, high: PadButton| {
        let v = axis.load(Ordering::Relaxed);
        if v <= -EDGE {
            *mask |= 1 << (low as u32);
        } else if v >= EDGE {
            *mask |= 1 << (high as u32);
        }
    };
    press(&mut mask, &AXIS_X, PadButton::Left, PadButton::Right);
    press(&mut mask, &AXIS_Y, PadButton::Up, PadButton::Down);
    press(&mut mask, &AXIS_HAT_X, PadButton::Left, PadButton::Right);
    press(&mut mask, &AXIS_HAT_Y, PadButton::Up, PadButton::Down);
    if AXIS_LTRIGGER.load(Ordering::Relaxed) >= EDGE {
        mask |= 1 << (PadButton::LeftShift as u32);
    }
    if AXIS_RTRIGGER.load(Ordering::Relaxed) >= EDGE {
        mask |= 1 << (PadButton::RightShift as u32);
    }
    mask
}
