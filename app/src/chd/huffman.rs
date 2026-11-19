#![forbid(unsafe_code)]
//! The canonical Huffman decoder CHD uses for its v5 hunk map and for the
//! `huff` codec.
//!
//! A tree arrives as a list of code lengths, in one of two encodings (a small
//! run-length form for the map, a Huffman-coded form for data). Codes are then
//! assigned canonically, longest lengths first, and decoding goes through a
//! table indexed by the next `max_bits` bits of input.

use super::bits::BitReader;

/// The tree is malformed or the stream ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HuffmanError;

pub(crate) struct Huffman {
    max_bits: u32,
    /// Indexed by the next `max_bits` input bits: `(symbol << 5) | length`,
    /// with a length of zero marking bit patterns no code covers.
    lookup: Vec<u32>,
}

impl Huffman {
    /// Builds a decoder from a code length per symbol.
    fn from_lengths(lengths: &[u8], max_bits: u32) -> Result<Huffman, HuffmanError> {
        // Count the codes of each length.
        let mut starts = [0u32; 33];
        for &length in lengths {
            if u32::from(length) > max_bits {
                return Err(HuffmanError);
            }
            let slot = starts.get_mut(usize::from(length)).ok_or(HuffmanError)?;
            *slot = slot.saturating_add(1);
        }

        // Each length's first code, from the longest down. Every level must
        // pair up exactly, except a lone code of length one.
        let mut next = 0u32;
        for length in (1..=32usize).rev() {
            let slot = starts.get_mut(length).ok_or(HuffmanError)?;
            let total = next.checked_add(*slot).ok_or(HuffmanError)?;
            if length != 1 && total % 2 != 0 {
                return Err(HuffmanError);
            }
            *slot = next;
            next = total >> 1;
        }

        let size = 1usize << max_bits;
        let mut lookup = vec![0u32; size];
        for (symbol, &length) in lengths.iter().enumerate() {
            if length == 0 {
                continue;
            }
            let slot = starts.get_mut(usize::from(length)).ok_or(HuffmanError)?;
            let code = *slot;
            *slot = code.saturating_add(1);

            let shift = max_bits - u32::from(length);
            let first = u64::from(code) << shift;
            let last = ((u64::from(code) + 1) << shift).saturating_sub(1);
            if last >= size as u64 || last < first {
                return Err(HuffmanError);
            }
            let value = (u32::try_from(symbol).map_err(|_| HuffmanError)? << 5) | u32::from(length);
            let range = lookup
                .get_mut(first as usize..=last as usize)
                .ok_or(HuffmanError)?;
            range.fill(value);
        }
        Ok(Huffman { max_bits, lookup })
    }

    /// Reads a tree in the run-length form the v5 map uses.
    pub(crate) fn import_rle(
        bits: &mut BitReader,
        num_codes: usize,
        max_bits: u32,
    ) -> Result<Huffman, HuffmanError> {
        let width = if max_bits >= 16 {
            5
        } else if max_bits >= 8 {
            4
        } else {
            3
        };
        let mut lengths: Vec<u8> = Vec::with_capacity(num_codes);
        while lengths.len() < num_codes {
            let value = bits.read(width) as u8;
            if value != 1 {
                lengths.push(value);
                continue;
            }
            // One is an escape: a second one is a literal one, anything else
            // is a length followed by a repeat count.
            let value = bits.read(width) as u8;
            if value == 1 {
                lengths.push(1);
            } else {
                let repeat = bits.read(width) as usize + 3;
                if lengths.len() + repeat > num_codes {
                    return Err(HuffmanError);
                }
                lengths.resize(lengths.len() + repeat, value);
            }
        }
        let decoder = Huffman::from_lengths(&lengths, max_bits)?;
        if bits.overflowed() {
            return Err(HuffmanError);
        }
        Ok(decoder)
    }

    /// Reads a tree whose lengths are themselves Huffman-coded, as the `huff`
    /// codec stores it.
    pub(crate) fn import_huffman(
        bits: &mut BitReader,
        num_codes: usize,
        max_bits: u32,
    ) -> Result<Huffman, HuffmanError> {
        // First a small tree of 24 symbols that codes the lengths.
        let mut small = [0u8; 24];
        if let Some(first) = small.first_mut() {
            *first = bits.read(3) as u8;
        }
        let start = bits.read(3) as usize + 1;
        let mut count = 0u32;
        for (index, slot) in small.iter_mut().enumerate().skip(1) {
            if index < start || count == 7 {
                *slot = 0;
            } else {
                count = bits.read(3);
                *slot = if count == 7 { 0 } else { count as u8 };
            }
        }
        let small = Huffman::from_lengths(&small, 6)?;

        // The longest run a repeat can express.
        let mut run_bits = 0u32;
        let mut temp = num_codes.saturating_sub(9);
        while temp != 0 {
            temp >>= 1;
            run_bits += 1;
        }

        let mut lengths: Vec<u8> = Vec::with_capacity(num_codes);
        let mut last = 0u8;
        while lengths.len() < num_codes {
            let value = small.decode(bits)?;
            if value != 0 {
                last = u8::try_from(value - 1).map_err(|_| HuffmanError)?;
                lengths.push(last);
            } else {
                let mut repeat = bits.read(3) as usize + 2;
                if repeat == 7 + 2 {
                    repeat += bits.read(run_bits) as usize;
                }
                let repeat = repeat.min(num_codes - lengths.len());
                lengths.resize(lengths.len() + repeat, last);
            }
        }
        let decoder = Huffman::from_lengths(&lengths, max_bits)?;
        if bits.overflowed() {
            return Err(HuffmanError);
        }
        Ok(decoder)
    }

    /// Decodes one symbol.
    pub(crate) fn decode(&self, bits: &mut BitReader) -> Result<u32, HuffmanError> {
        let window = bits.peek(self.max_bits);
        let entry = self.lookup.get(window as usize).copied().unwrap_or(0);
        let length = entry & 0x1f;
        if length == 0 {
            return Err(HuffmanError);
        }
        bits.skip(length);
        Ok(entry >> 5)
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used)]
mod tests {
    use super::{BitReader, Huffman};

    #[test]
    fn canonical_codes_go_longest_first() {
        // Lengths 1, 2, 2: the two-bit codes take 00 and 01, the one-bit code 1.
        let tree = Huffman::from_lengths(&[1, 2, 2], 2).unwrap();
        // 1 | 00 | 01 | 1 | 00
        let mut bits = BitReader::new(&[0b1000_1100]);
        assert_eq!(tree.decode(&mut bits), Ok(0));
        assert_eq!(tree.decode(&mut bits), Ok(1));
        assert_eq!(tree.decode(&mut bits), Ok(2));
        assert_eq!(tree.decode(&mut bits), Ok(0));
        assert_eq!(tree.decode(&mut bits), Ok(1));
    }

    #[test]
    fn an_oversubscribed_tree_is_refused() {
        assert!(Huffman::from_lengths(&[1, 1, 1], 4).is_err());
        assert!(Huffman::from_lengths(&[2, 2, 2], 4).is_err());
        assert!(Huffman::from_lengths(&[5], 4).is_err());
    }
}
