#![forbid(unsafe_code)]
//! An MSB-first bit reader over an untrusted byte slice.
//!
//! Reading past the end yields zero bits rather than failing, which is what
//! the CHD map and Huffman streams expect; [`BitReader::overflowed`] says
//! afterwards whether that happened, and loops that could otherwise run on
//! zero bits for ever check it.

pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    /// Index of the next unread bit.
    position: u64,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        BitReader { data, position: 0 }
    }

    /// The next `count` bits (0..=32) without consuming them.
    pub(crate) fn peek(&self, count: u32) -> u32 {
        if count == 0 {
            return 0;
        }
        let count = count.min(32);
        let first = usize::try_from(self.position / 8).unwrap_or(usize::MAX);
        let shift = (self.position % 8) as u32;
        let mut window = 0u64;
        for index in 0..5usize {
            let byte = first
                .checked_add(index)
                .and_then(|at| self.data.get(at))
                .copied()
                .unwrap_or(0);
            window |= u64::from(byte) << (56 - 8 * index as u32);
        }
        // `shift + count` is at most 39, inside the 40 bits gathered.
        ((window << shift) >> (64 - count)) as u32
    }

    /// Consumes `count` bits.
    pub(crate) fn skip(&mut self, count: u32) {
        self.position = self.position.saturating_add(u64::from(count));
    }

    /// Reads `count` bits (0..=32) as an unsigned number.
    pub(crate) fn read(&mut self, count: u32) -> u32 {
        let value = self.peek(count);
        self.skip(count.min(32));
        value
    }

    /// Reads `count` bits (1..=32) as a two's-complement signed number.
    pub(crate) fn read_signed(&mut self, count: u32) -> i32 {
        let value = self.read(count);
        if count == 0 || count >= 32 {
            return value as i32;
        }
        let shift = 32 - count;
        ((value << shift) as i32) >> shift
    }

    /// Counts zero bits up to and including the next one bit, returning the
    /// number of zeros. Fails rather than spinning once the data runs out.
    pub(crate) fn read_unary(&mut self) -> Option<u32> {
        let mut zeros = 0u32;
        loop {
            if self.overflowed() {
                return None;
            }
            let window = self.peek(32);
            if window != 0 {
                let leading = window.leading_zeros();
                zeros = zeros.checked_add(leading)?;
                self.skip(leading + 1);
                return if self.overflowed() { None } else { Some(zeros) };
            }
            zeros = zeros.checked_add(32)?;
            self.skip(32);
        }
    }

    /// True once more bits have been consumed than the data holds.
    pub(crate) fn overflowed(&self) -> bool {
        self.position > (self.data.len() as u64).saturating_mul(8)
    }

    /// Skips to the next byte boundary.
    pub(crate) fn align_to_byte(&mut self) {
        let rem = self.position % 8;
        if rem != 0 {
            self.position = self.position.saturating_add(8 - rem);
        }
    }

    /// The byte index of the next unread bit, which must be byte-aligned to
    /// mean anything.
    pub(crate) fn byte_position(&self) -> usize {
        usize::try_from(self.position / 8).unwrap_or(usize::MAX)
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used)]
mod tests {
    use super::BitReader;

    #[test]
    fn reads_msb_first_and_zero_fills_past_the_end() {
        let mut bits = BitReader::new(&[0b1010_0000, 0xff]);
        assert_eq!(bits.read(3), 0b101);
        assert_eq!(bits.read(5), 0);
        assert_eq!(bits.read(4), 0xf);
        assert!(!bits.overflowed());
        assert_eq!(bits.read(8), 0xf0);
        assert!(bits.overflowed());
        assert_eq!(bits.read(32), 0);
    }

    #[test]
    fn signed_and_unary_reads() {
        let mut bits = BitReader::new(&[0b1110_0001, 0b0000_0000]);
        assert_eq!(bits.read_signed(3), -1);
        assert_eq!(bits.read_unary(), Some(4));
        let mut empty = BitReader::new(&[0, 0]);
        assert_eq!(empty.read_unary(), None);
    }
}
