//! Desktop gamepads, read from SDL3, as 3DO pads.
//!
//! egui owns the window, so the GUI runs no SDL event loop of its own: the
//! gamepad subsystem is updated by hand and a pad plugged in after launch is
//! picked up by re-reading the pads that are attached. The Android build
//! takes its pads from the Java shim instead (see `android_pad.rs`); both
//! hand the core the same button mask, so the emulator does not care which
//! one is driving.

use sdl3::gamepad::{Axis, Button, Gamepad};
use sdl3::joystick::JoystickId;
use sdl3::GamepadSubsystem;

use retro3do::PadButton;

/// How far a stick must go before it counts as a d-pad press.
const EDGE: i16 = 16000;

/// Console pad 0 is the keyboard, so the gamepads take the pads behind it.
const MAX_GAMEPADS: usize = retro3do::MAX_PADS as usize - 1;

/// An SDL button and the 3DO button it drives. The face buttons follow the
/// 3DO's own shape: south is the primary A, east is B, and west and north
/// both land on C — the 3DO has three face buttons, not four.
const BUTTONS: [(Button, PadButton); 12] = [
    (Button::DPadUp, PadButton::Up),
    (Button::DPadDown, PadButton::Down),
    (Button::DPadLeft, PadButton::Left),
    (Button::DPadRight, PadButton::Right),
    (Button::South, PadButton::A),
    (Button::East, PadButton::B),
    (Button::West, PadButton::C),
    (Button::North, PadButton::C),
    (Button::Start, PadButton::Play),
    (Button::Back, PadButton::Stop),
    (Button::LeftShoulder, PadButton::LeftShift),
    (Button::RightShoulder, PadButton::RightShift),
];

/// One gamepad SDL has open: the instance id it came from, the name SDL
/// reports for it and the handle itself.
struct Pad {
    id: JoystickId,
    name: String,
    pad: Gamepad,
}

/// The gamepads SDL sees, as 3DO pads.
pub struct Gamepads {
    subsystem: GamepadSubsystem,
    pads: Vec<Pad>,
}

impl Gamepads {
    pub fn new(subsystem: GamepadSubsystem) -> Self {
        let mut gamepads = Self {
            subsystem,
            pads: Vec::new(),
        };
        gamepads.rescan();
        gamepads
    }

    /// Looks for gamepads that came or went since the last look. Returns true
    /// when the set of pads changed.
    pub fn refresh(&mut self) -> bool {
        // Without an SDL event loop nothing pumps the controller state, so
        // ask SDL for it here.
        self.subsystem.update();
        self.rescan()
    }

    /// Re-reads the attached gamepads and opens the ones that are new.
    fn rescan(&mut self) -> bool {
        let Ok(ids) = self.subsystem.gamepads() else {
            return false;
        };
        let ids: Vec<JoystickId> = ids
            .into_iter()
            .filter(|id| self.subsystem.is_gamepad(*id))
            .take(MAX_GAMEPADS)
            .collect();
        if ids.len() == self.pads.len()
            && ids
                .iter()
                .zip(self.pads.iter())
                .all(|(id, pad)| *id == pad.id)
        {
            return false;
        }
        // SDL's instance ids change when a pad is unplugged and plugged back
        // in, so the pads are opened afresh rather than patched.
        let mut pads = Vec::new();
        for id in ids {
            if let Ok(pad) = self.subsystem.open(id) {
                let name = pad
                    .name()
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| "gamepad".to_string());
                pads.push(Pad { id, name, pad });
            }
        }
        self.pads = pads;
        true
    }

    /// How many gamepads are attached.
    pub fn count(&self) -> usize {
        self.pads.len()
    }

    /// The names SDL reports, for the launcher's status line.
    pub fn names(&self) -> Vec<String> {
        self.pads.iter().map(|pad| pad.name.clone()).collect()
    }

    /// The 3DO buttons held down on gamepad `index`.
    pub fn mask(&self, index: usize) -> u32 {
        let Some(pad) = self.pads.get(index) else {
            return 0;
        };
        mask_of(|button| pad.pad.button(button), |axis| pad.pad.axis(axis))
    }
}

/// The 3DO buttons a gamepad's state maps to. The buttons and the axes come
/// in separately so the mapping can be tested without a gamepad attached.
fn mask_of(buttons: impl Fn(Button) -> bool, axes: impl Fn(Axis) -> i16) -> u32 {
    let mut mask = 0u32;
    for (button, three) in BUTTONS {
        if buttons(button) {
            mask |= three.mask();
        }
    }
    // The left stick drives the directions, as the d-pad does.
    let mut direction = |axis: Axis, low: PadButton, high: PadButton| {
        let value = axes(axis);
        if value < -EDGE {
            mask |= low.mask();
        } else if value > EDGE {
            mask |= high.mask();
        }
    };
    direction(Axis::LeftX, PadButton::Left, PadButton::Right);
    direction(Axis::LeftY, PadButton::Up, PadButton::Down);
    // The triggers are the shoulders.
    if axes(Axis::TriggerLeft) > EDGE {
        mask |= PadButton::LeftShift.mask();
    }
    if axes(Axis::TriggerRight) > EDGE {
        mask |= PadButton::RightShift.mask();
    }
    mask
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A gamepad with nothing pressed.
    fn idle() -> impl Fn(Button) -> bool {
        |_| false
    }

    /// A gamepad with one button held and every axis at rest.
    fn holding(button: Button) -> impl Fn(Button) -> bool {
        move |other| other == button
    }

    /// A gamepad with one axis moved and every button up.
    fn axis_at(axis: Axis, value: i16) -> impl Fn(Axis) -> i16 {
        move |other| if other == axis { value } else { 0 }
    }

    #[test]
    fn face_buttons_land_on_a_b_c() {
        assert_eq!(mask_of(holding(Button::South), axis_at(Axis::LeftX, 0)), PadButton::A.mask());
        assert_eq!(mask_of(holding(Button::East), axis_at(Axis::LeftX, 0)), PadButton::B.mask());
        assert_eq!(mask_of(holding(Button::West), axis_at(Axis::LeftX, 0)), PadButton::C.mask());
        assert_eq!(mask_of(holding(Button::North), axis_at(Axis::LeftX, 0)), PadButton::C.mask());
    }

    #[test]
    fn start_and_back_are_play_and_stop() {
        assert_eq!(
            mask_of(holding(Button::Start), axis_at(Axis::LeftX, 0)),
            PadButton::Play.mask()
        );
        assert_eq!(
            mask_of(holding(Button::Back), axis_at(Axis::LeftX, 0)),
            PadButton::Stop.mask()
        );
    }

    #[test]
    fn shoulders_are_the_shifts() {
        assert_eq!(
            mask_of(holding(Button::LeftShoulder), axis_at(Axis::LeftX, 0)),
            PadButton::LeftShift.mask()
        );
        assert_eq!(
            mask_of(holding(Button::RightShoulder), axis_at(Axis::LeftX, 0)),
            PadButton::RightShift.mask()
        );
    }

    #[test]
    fn dpad_is_the_directions() {
        assert_eq!(
            mask_of(holding(Button::DPadUp), axis_at(Axis::LeftX, 0)),
            PadButton::Up.mask()
        );
        assert_eq!(
            mask_of(holding(Button::DPadDown), axis_at(Axis::LeftX, 0)),
            PadButton::Down.mask()
        );
        assert_eq!(
            mask_of(holding(Button::DPadLeft), axis_at(Axis::LeftX, 0)),
            PadButton::Left.mask()
        );
        assert_eq!(
            mask_of(holding(Button::DPadRight), axis_at(Axis::LeftX, 0)),
            PadButton::Right.mask()
        );
    }

    #[test]
    fn the_stick_drives_the_directions() {
        assert_eq!(mask_of(idle(), axis_at(Axis::LeftX, -32000)), PadButton::Left.mask());
        assert_eq!(mask_of(idle(), axis_at(Axis::LeftX, 32000)), PadButton::Right.mask());
        assert_eq!(mask_of(idle(), axis_at(Axis::LeftY, -32000)), PadButton::Up.mask());
        assert_eq!(mask_of(idle(), axis_at(Axis::LeftY, 32000)), PadButton::Down.mask());
    }

    #[test]
    fn a_stick_inside_the_deadzone_is_nothing() {
        assert_eq!(mask_of(idle(), axis_at(Axis::LeftX, EDGE - 1)), 0);
        assert_eq!(mask_of(idle(), axis_at(Axis::LeftY, -(EDGE - 1))), 0);
    }

    #[test]
    fn the_triggers_are_the_shoulders() {
        assert_eq!(
            mask_of(idle(), axis_at(Axis::TriggerLeft, 32000)),
            PadButton::LeftShift.mask()
        );
        assert_eq!(
            mask_of(idle(), axis_at(Axis::TriggerRight, 32000)),
            PadButton::RightShift.mask()
        );
    }
}
