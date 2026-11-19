//! The DSP's DMA channels: thirteen carry samples into the DSPP and four carry
//! them out. Each has a current address and length plus a reload pair, so a
//! sound can run continuously without the CPU in the loop (WO 94/10641).
//!
//! An input channel has a hardware FIFO between the DMA engine and the DSPP.
//! The engine keeps it filled, so the position software reads runs AHEAD of
//! what the DSPP has consumed by up to eight words, and a buffer's completion
//! interrupt arrives while its tail is still queued. The audio folio decides
//! whether a buffer has finished by reading that position inline, so the
//! lookahead is not a detail: without it a DataStreamer title can arm an
//! interrupt that never fires.
#![forbid(unsafe_code)]

use super::decoder::read_be16_bytes;
use super::Madam;
use crate::memory::Memory;

/// Input channels 0..12 at `0x400 + channel * 16`.
pub const MADAM_FIFO_BASE: u32 = 0x0400;
/// Output channels 0..3 at `0x500 + channel * 16`.
pub const MADAM_FIFO_OUTPUT: u32 = 0x0500;
pub const MADAM_FIFO_END: u32 = 0x0540;

pub const INPUT_FIFOS: usize = 13;
pub const OUTPUT_FIFOS: usize = 4;
const STAGED_WORDS: usize = 8;

/// A reload that arrives at an empty buffer finishes again at once. The
/// hardware would raise interrupts for ever; one fill stops after this many
/// consecutive boundaries that moved no data.
const MAX_EMPTY_BOUNDARIES: u32 = 2;

/// Registers of one channel, by the low nibble of the offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FifoRegister {
    Address,
    Length,
    NextAddress,
    NextLength,
}

impl FifoRegister {
    const fn decode(offset: u32) -> Self {
        match offset & 0x0f {
            0x00 => Self::Address,
            0x04 => Self::Length,
            0x08 => Self::NextAddress,
            _ => Self::NextLength,
        }
    }
}

/// A written length: zero is an idle channel, anything else is four bytes
/// longer than software asked for.
fn written_length(value: u32) -> i32 {
    if value != 0 {
        value.wrapping_add(4) as i32
    } else {
        0
    }
}

/// One DSP DMA channel. `index` is how far through the current buffer the DMA
/// has read; an address of zero means the channel is idle (software clears it
/// to stop a channel).
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct DmaFifo {
    index: i32,
    address: u32,
    length: i32,
    next_address: u32,
    next_length: i32,
    staged: [u16; STAGED_WORDS],
    staged_count: usize,
    staged_pos: usize,
}

impl DmaFifo {
    fn remaining(&self) -> i32 {
        self.length.wrapping_sub(self.index)
    }

    fn position(&self) -> u32 {
        self.address.wrapping_add(self.index as u32)
    }

    fn read_register(&self, offset: u32) -> u32 {
        // The address and length read back as the CURRENT position, which is
        // how software finds out how far a sound has played.
        match FifoRegister::decode(offset) {
            FifoRegister::Address => self.position(),
            FifoRegister::Length => self.remaining() as u32,
            FifoRegister::NextAddress => self.next_address,
            FifoRegister::NextLength => self.next_length as u32,
        }
    }

    fn write_register(&mut self, offset: u32, value: u32, input: bool) {
        match FifoRegister::decode(offset) {
            FifoRegister::Address => {
                self.address = value;
                // A new address abandons the armed reload and whatever the old
                // buffer had staged.
                if input {
                    self.next_address = 0;
                    self.staged_count = 0;
                    self.staged_pos = 0;
                }
            }
            FifoRegister::Length => {
                self.length = written_length(value);
                if input {
                    self.next_length = 0;
                }
            }
            FifoRegister::NextAddress => self.next_address = value,
            FifoRegister::NextLength => self.next_length = written_length(value),
        }
    }

    fn front(&self) -> Option<u16> {
        (self.staged_count > 0).then(|| self.staged.get(self.staged_pos).copied().unwrap_or(0))
    }

    fn pop(&mut self) -> Option<u16> {
        let value = self.front()?;
        self.staged_pos = (self.staged_pos + 1) % STAGED_WORDS;
        self.staged_count -= 1;
        Some(value)
    }

    fn push(&mut self, value: u16) {
        if let Some(slot) = self
            .staged
            .get_mut((self.staged_pos + self.staged_count) % STAGED_WORDS)
        {
            *slot = value;
        }
        self.staged_count += 1;
    }

    /// The loop test at a buffer boundary: reload if software armed one and
    /// left the channel enabled, otherwise finish.
    fn boundary(&mut self, enabled: bool) -> bool {
        self.index = 0;
        let reload = self.next_address != 0 && enabled;
        if reload {
            self.address = self.next_address;
            self.length = self.next_length;
        } else {
            self.address = 0;
        }
        reload
    }
}

/// Where in the FIFO register block an offset falls.
pub(super) fn fifo_slot(offset: u32) -> Option<(bool, usize)> {
    if !(MADAM_FIFO_BASE..MADAM_FIFO_END).contains(&offset) {
        return None;
    }
    let output = offset >= MADAM_FIFO_OUTPUT;
    let channel = ((offset >> 4) & 0x0f) as usize;
    Some((
        output,
        if output {
            channel % OUTPUT_FIFOS
        } else {
            channel % INPUT_FIFOS
        },
    ))
}

impl Madam {
    fn fifo_mut(&mut self, output: bool, channel: usize) -> Option<&mut DmaFifo> {
        if output {
            self.output_fifo.get_mut(channel)
        } else {
            self.input_fifo.get_mut(channel)
        }
    }

    pub(super) fn read_fifo_register(&self, offset: u32) -> Option<u32> {
        let (output, channel) = fifo_slot(offset)?;
        let fifo = if output {
            self.output_fifo.get(channel)
        } else {
            self.input_fifo.get(channel)
        };
        Some(fifo.map_or(0, |f| f.read_register(offset)))
    }

    /// Returns `false` if the offset is not a FIFO register.
    pub(super) fn write_fifo_register(&mut self, offset: u32, value: u32) -> bool {
        let Some((output, channel)) = fifo_slot(offset) else {
            return false;
        };
        if let Some(fifo) = self.fifo_mut(output, channel) {
            fifo.write_register(offset, value, !output);
        }
        true
    }

    fn raise_fifo_done(&mut self, channel: usize, output: bool) {
        let bit = if output { channel + 12 } else { channel + 16 };
        self.interrupts |= 1u32.checked_shl(bit as u32).unwrap_or(0);
    }

    /// The DMA half of an input channel: keep its FIFO filled from memory.
    /// The completion interrupt and the reload happen when the last word is
    /// FETCHED, not when the DSPP finally consumes it.
    fn fill_input_fifo(&mut self, channel: usize, mem: &Memory) {
        let enabled = self.dma_channel_enable & 1u32.checked_shl(channel as u32).unwrap_or(0) != 0;
        let mut empty_boundaries = 0;
        loop {
            let Some(fifo) = self.input_fifo.get_mut(channel) else {
                return;
            };
            if fifo.staged_count >= STAGED_WORDS || fifo.address == 0 {
                return;
            }
            if fifo.remaining() > 0 {
                let value = read_be16_bytes(mem, fifo.position());
                fifo.push(value);
                fifo.index = fifo.index.wrapping_add(2);
                empty_boundaries = 0;
                continue;
            }
            if empty_boundaries >= MAX_EMPTY_BOUNDARIES {
                return;
            }
            empty_boundaries += 1;
            let reloaded = fifo.boundary(enabled);
            self.raise_fifo_done(channel, false);
            if reloaded {
                if let Some(count) = self.stats.fifo_reloads.get_mut(channel) {
                    *count += 1;
                }
            }
        }
    }

    /// An enable edge on an input channel runs the loop test right then: the
    /// audio folio's stop sequence sleeps on the interrupt only an enable can
    /// produce once the voice has stopped consuming.
    pub fn on_input_channel_enabled(&mut self, channel: u16, mem: &Memory) {
        if usize::from(channel) < INPUT_FIFOS {
            self.fill_input_fifo(usize::from(channel), mem);
        }
    }

    /// The word the DSPP would read next, without consuming it.
    pub fn fifo_input_peek(&mut self, channel: u16, mem: &Memory) -> u16 {
        let channel = usize::from(channel);
        if channel >= INPUT_FIFOS {
            return 0;
        }
        self.fill_input_fifo(channel, mem);
        self.input_fifo
            .get(channel)
            .and_then(DmaFifo::front)
            .unwrap_or(0)
    }

    /// The DSPP consumes one word; zero from a dead channel.
    pub fn fifo_input_next(&mut self, channel: u16, mem: &Memory) -> u16 {
        let channel = usize::from(channel);
        if channel >= INPUT_FIFOS {
            return 0;
        }
        if let Some(count) = self.stats.fifo_asked.get_mut(channel) {
            *count += 1;
        }
        self.fill_input_fifo(channel, mem);
        let Some(value) = self.input_fifo.get_mut(channel).and_then(DmaFifo::pop) else {
            return 0;
        };
        if let Some(count) = self.stats.fifo_delivered.get_mut(channel) {
            *count += 1;
        }
        // Refill behind the read, so the DMA stays ahead of consumption.
        self.fill_input_fifo(channel, mem);
        value
    }

    /// Two while the channel is live (DMA running or words still staged),
    /// otherwise zero; instruments branch on exactly that value.
    pub fn fifo_input_status(&self, channel: u16) -> u16 {
        match self.input_fifo.get(usize::from(channel)) {
            Some(fifo) if fifo.address != 0 || fifo.staged_count > 0 => 2,
            _ => 0,
        }
    }

    /// One while the output channel has room to accept a word.
    pub fn fifo_output_status(&self, channel: u16) -> u16 {
        match self.output_fifo.get(usize::from(channel)) {
            Some(fifo) if fifo.remaining() > 0 => 1,
            _ => 0,
        }
    }

    /// The DSPP writes one word to an output channel.
    pub fn fifo_output(&mut self, channel: u16, value: u16, mem: &mut Memory) {
        let channel = usize::from(channel);
        if channel >= OUTPUT_FIFOS {
            return;
        }
        if let Some(count) = self.stats.fifo_out_writes.get_mut(channel) {
            *count += 1;
        }
        // Output channels sit at bits 16..19 of the shared enable mask, the
        // numbering CLIO's set/clear ports use.
        let enabled =
            self.dma_channel_enable & 1u32.checked_shl(channel as u32 + 16).unwrap_or(0) != 0;
        let Some(fifo) = self.output_fifo.get_mut(channel) else {
            return;
        };
        if fifo.address == 0 {
            return;
        }
        if let Some(count) = self.stats.fifo_out_stored.get_mut(channel) {
            *count += 1;
        }
        if fifo.remaining() > 0 {
            let address = fifo.position();
            fifo.index = fifo.index.wrapping_add(2);
            let [high, low] = value.to_be_bytes();
            mem.write8(address, high);
            mem.write8(address.wrapping_add(1), low);
            return;
        }
        fifo.boundary(enabled);
        self.raise_fifo_done(channel, true);
    }

    /// Reset a channel to idle, as CLIO's clear port does.
    pub fn clear_fifo(&mut self, channel: u32, output: bool) {
        if let Some(fifo) = usize::try_from(channel)
            .ok()
            .and_then(|c| self.fifo_mut(output, c))
        {
            *fifo = DmaFifo::default();
        }
    }
}
