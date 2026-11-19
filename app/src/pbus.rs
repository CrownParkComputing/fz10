//! PBUS — the player bus, where the controllers live.
//!
//! Software builds no request and reads no port: it points MADAM's PBUS channel
//! at a buffer and sets a transfer going, and the hardware writes every
//! attached device's report into memory and interrupts when done. The OS's
//! input task blocks on that interrupt every frame.
//!
//! Devices report as a byte stream, each opening with an identifying byte, and
//! the stream ends in 0xFF padding. Nothing plugged in is padding alone, which
//! is still a complete answer (WO 94/10636).
#![forbid(unsafe_code)]

use crate::pad::{Joypad, PadButton};

/// First byte of a standard control pad's report.
pub const JOYPAD_ID: u8 = 0x80;
const CAPACITY: usize = 256;
const PADDING_BYTES: usize = 8;

/// Where each button sits in the pad's two report bytes. The wire layout is
/// what it is, so it is a table rather than arithmetic.
const FIRST_BYTE: [(PadButton, u8); 5] = [
    (PadButton::A, 0),
    (PadButton::Left, 1),
    (PadButton::Right, 2),
    (PadButton::Up, 3),
    (PadButton::Down, 4),
];
const SECOND_BYTE: [(PadButton, u8); 6] = [
    (PadButton::LeftShift, 2),
    (PadButton::RightShift, 3),
    (PadButton::Stop, 4),
    (PadButton::Play, 5),
    (PadButton::C, 6),
    (PadButton::B, 7),
];

fn pack(pad: Joypad, layout: &[(PadButton, u8)]) -> u8 {
    layout
        .iter()
        .filter(|(button, _)| pad.held(*button))
        .fold(0, |byte, (_, shift)| byte | (1 << shift))
}

/// One transfer's report, built device by device.
#[derive(Clone, Debug, Default)]
pub struct Pbus {
    data: Vec<u8>,
}

impl Pbus {
    pub fn new() -> Self {
        Self {
            data: Vec::with_capacity(CAPACITY),
        }
    }

    /// Start a new report.
    pub fn begin(&mut self) {
        self.data.clear();
    }

    pub fn add_joypad(&mut self, pad: Joypad) {
        if self.data.len() + 2 > CAPACITY {
            return;
        }
        self.data.push(JOYPAD_ID | pack(pad, &FIRST_BYTE));
        self.data.push(pack(pad, &SECOND_BYTE));
    }

    /// Close the report with the padding that ends the chain.
    pub fn pad(&mut self) {
        let room = CAPACITY.saturating_sub(self.data.len()).min(PADDING_BYTES);
        self.data.extend(std::iter::repeat_n(0xff, room));
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }
    pub fn size(&self) -> usize {
        self.data.len()
    }
}
