//! Reading cel source data: the big-endian bitstream unpacked rows are stored
//! in, and the packet coding of packed rows (SDK Graphics Programming Guide).
#![forbid(unsafe_code)]

use crate::memory::Memory;

/// A big-endian bitstream in memory, most significant bit first, which is
/// the order the cel packer writes it in.
#[derive(Clone, Copy, Debug)]
pub(super) struct BitReader {
    base: u32,
    bit: u32,
}

impl BitReader {
    pub(super) const fn new(base: u32) -> Self {
        Self { base, bit: 0 }
    }

    /// The next `bits` bits (at most 16) as an unsigned value.
    pub(super) fn read(&mut self, mem: &Memory, bits: u32) -> u32 {
        let bits = bits.min(16);
        if bits == 0 {
            return 0;
        }
        // A value of up to sixteen bits starting anywhere in a byte lies
        // inside the three bytes from that one.
        let at = self.address();
        let window = (0..3u32).fold(0u32, |acc, i| {
            acc << 8 | u32::from(mem.read8(at.wrapping_add(i)))
        });
        let shift = 24 - (self.bit & 7) - bits;
        self.bit = self.bit.wrapping_add(bits);
        (window >> shift) & ((1u32 << bits) - 1)
    }

    pub(super) fn skip(&mut self, bits: u32) {
        self.bit = self.bit.wrapping_add(bits);
    }

    /// Where the reader has reached, rounded down to a byte.
    pub(super) const fn address(&self) -> u32 {
        self.base.wrapping_add(self.bit >> 3)
    }
}

/// One packet of a packed row: a two-bit type and a six-bit count-less-one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Packet {
    /// End of the row.
    End,
    /// `count` pixels follow, one value each.
    Literal(u32),
    /// Skip `count` pixels.
    Transparent(u32),
    /// One value follows and covers `count` pixels.
    Repeat(u32),
}

impl Packet {
    /// Read the next packet. Running past the row's declared end ends the row,
    /// whatever the bits say, so a corrupt length cannot run the decoder
    /// through the whole of memory.
    pub(super) fn read(reader: &mut BitReader, mem: &Memory, row_end: u32) -> Self {
        let kind = reader.read(mem, 2);
        let past_end = reader.address() >= row_end;
        let count = reader.read(mem, 6) + 1;
        match kind {
            _ if past_end => Self::End,
            1 => Self::Literal(count),
            2 => Self::Transparent(count),
            3 => Self::Repeat(count),
            _ => Self::End,
        }
    }
}
