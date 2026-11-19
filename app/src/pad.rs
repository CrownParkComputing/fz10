//! The 3DO control pad, as the host reports it.
//!
//! Pads daisy-chain: the machine sees one serial stream with every connected
//! pad in it, which is why a 3DO needs no multitap. Up to eight.
//!
//! Written by whatever reads the host's controllers and read by the emulator
//! thread, so each pad's buttons live in one atomic word: the machine always
//! sees a coherent set, never half an update (which is what makes a diagonal
//! register as a stutter).
#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicU32, Ordering};

pub const MAX_PADS: u32 = 8;

/// The buttons, in the order the C ABI numbers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum PadButton {
    Up = 0,
    Down,
    Left,
    Right,
    A,
    B,
    C,
    /// The "P" button.
    Play,
    /// The "X" button.
    Stop,
    LeftShift,
    RightShift,
}

impl PadButton {
    pub const COUNT: u32 = 11;
    pub const ALL: [PadButton; 11] = [
        PadButton::Up,
        PadButton::Down,
        PadButton::Left,
        PadButton::Right,
        PadButton::A,
        PadButton::B,
        PadButton::C,
        PadButton::Play,
        PadButton::Stop,
        PadButton::LeftShift,
        PadButton::RightShift,
    ];

    pub fn from_index(index: u32) -> Option<Self> {
        Self::ALL.get(index as usize).copied()
    }

    pub const fn mask(self) -> u32 {
        1 << self as u32
    }
}

/// A pad as a set of held buttons.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Joypad(u32);

impl Joypad {
    pub fn from_bits(bits: u32) -> Self {
        Self(bits & ((1 << PadButton::COUNT) - 1))
    }
    pub fn bits(self) -> u32 {
        self.0
    }
    pub fn held(self, button: PadButton) -> bool {
        self.0 & button.mask() != 0
    }
    pub fn with(mut self, button: PadButton, down: bool) -> Self {
        if down {
            self.0 |= button.mask();
        } else {
            self.0 &= !button.mask();
        }
        self
    }
}

#[derive(Debug)]
pub struct PadState {
    buttons: [AtomicU32; MAX_PADS as usize],
    /// One bit per pad. One pad, as a machine ships.
    connected: AtomicU32,
}

impl Default for PadState {
    fn default() -> Self {
        Self::new()
    }
}

impl PadState {
    pub fn new() -> Self {
        Self {
            buttons: Default::default(),
            connected: AtomicU32::new(1),
        }
    }

    fn slot(&self, pad: u32) -> Option<&AtomicU32> {
        self.buttons.get(pad as usize)
    }

    pub fn reset(&self) {
        for pad in &self.buttons {
            pad.store(0, Ordering::Relaxed);
        }
        self.connected.store(1, Ordering::Relaxed);
    }

    /// Disconnecting releases the pad's buttons, or a pad unplugged mid-press
    /// leaves the machine holding a direction forever.
    pub fn set_connected(&self, pad: u32, connected: bool) {
        let Some(slot) = self.slot(pad) else { return };
        if connected {
            self.connected.fetch_or(1 << pad, Ordering::Relaxed);
        } else {
            self.connected.fetch_and(!(1 << pad), Ordering::Relaxed);
            slot.store(0, Ordering::Relaxed);
        }
    }

    pub fn connected(&self, pad: u32) -> bool {
        pad < MAX_PADS && self.connected.load(Ordering::Relaxed) & (1 << pad) != 0
    }

    pub fn connected_count(&self) -> u32 {
        self.connected.load(Ordering::Relaxed).count_ones()
    }

    /// Read-modify-write, not a store: keyboard and gamepad may both be mapped
    /// to one pad, and a store would lose whichever arrived first.
    pub fn press(&self, pad: u32, button: PadButton, down: bool) {
        let Some(slot) = self.slot(pad) else { return };
        if down {
            slot.fetch_or(button.mask(), Ordering::AcqRel);
        } else {
            slot.fetch_and(!button.mask(), Ordering::AcqRel);
        }
    }

    pub fn pressed(&self, pad: u32, button: PadButton) -> bool {
        self.buttons(pad) & button.mask() != 0
    }

    /// The whole pad as a bitmask indexed by [`PadButton`].
    pub fn buttons(&self, pad: u32) -> u32 {
        self.slot(pad)
            .map_or(0, |slot| slot.load(Ordering::Acquire))
    }

    /// Set every button of a pad in one store, so the machine never sees a pad
    /// halfway through an update.
    pub fn set_buttons(&self, pad: u32, buttons: u32) {
        if let Some(slot) = self.slot(pad) {
            slot.store(Joypad::from_bits(buttons).bits(), Ordering::Release);
        }
    }

    pub fn joypad(&self, pad: u32) -> Joypad {
        Joypad::from_bits(self.buttons(pad))
    }
}
