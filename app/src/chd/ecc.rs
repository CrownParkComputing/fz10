#![forbid(unsafe_code)]
//! Rebuilding the sync pattern and ECC of a CD-ROM sector.
//!
//! The CD codecs drop the twelve sync bytes and the 276 bytes of P and Q
//! parity from every sector whose ECC checks out, since both can be
//! recomputed from the rest; a per-frame flag says which ones. The parity is
//! the CD-ROM Reed-Solomon product code over GF(2^8) with polynomial 0x11d:
//! P covers 86 columns of 24 bytes, Q 52 diagonals of 43, both over the sector
//! from its header onward, and Q also over P.

use std::sync::LazyLock;

const SYNC: [u8; 12] = [
    0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00,
];

/// Where the mode byte sits.
const MODE_OFFSET: usize = 15;
/// Where the parity calculation starts: the header, just after the sync.
const HEADER_OFFSET: usize = 12;
const P_OFFSET: usize = 0x81c;
const Q_OFFSET: usize = 0x8c8;

struct Code {
    /// Parity bytes per half (P: 86, Q: 52).
    majors: usize,
    /// Source bytes per parity byte (P: 24, Q: 43).
    minors: usize,
    major_step: usize,
    minor_step: usize,
    dest: usize,
}

const P: Code = Code {
    majors: 86,
    minors: 24,
    major_step: 2,
    minor_step: 86,
    dest: P_OFFSET,
};
const Q: Code = Code {
    majors: 52,
    minors: 43,
    major_step: 86,
    minor_step: 88,
    dest: Q_OFFSET,
};

/// Multiplication by alpha in GF(2^8) with polynomial 0x11d.
fn double(value: u8) -> u8 {
    let wide = u16::from(value) << 1;
    (if value & 0x80 != 0 {
        wide ^ 0x11d
    } else {
        wide
    }) as u8
}

/// The partner table for the final division: `HALVE[x ^ double(x)] == x`.
static HALVE: LazyLock<[u8; 256]> = LazyLock::new(|| {
    let mut table = [0u8; 256];
    for value in 0..=255u8 {
        if let Some(slot) = table.get_mut(usize::from(value ^ double(value))) {
            *slot = value;
        }
    }
    table
});

fn forward(value: u8) -> u8 {
    double(value)
}

fn backward(value: u8) -> u8 {
    HALVE.get(usize::from(value)).copied().unwrap_or(0)
}

/// A byte of the parity source. In mode 2 the four header bytes are left out
/// of the calculation, as though they were zero.
fn source(sector: &[u8], offset: usize, mode2: bool) -> u8 {
    if mode2 && offset < 4 {
        return 0;
    }
    sector.get(HEADER_OFFSET + offset).copied().unwrap_or(0)
}

fn generate(sector: &mut [u8], code: &Code) {
    let mode2 = sector.get(MODE_OFFSET) == Some(&2);
    let size = code.majors * code.minors;
    for major in 0..code.majors {
        let mut index = (major >> 1) * code.major_step + (major & 1);
        let mut a = 0u8;
        let mut b = 0u8;
        for _ in 0..code.minors {
            let byte = source(sector, index, mode2);
            a ^= byte;
            b ^= byte;
            a = forward(a);
            index += code.minor_step;
            if index >= size {
                index -= size;
            }
        }
        a = backward(forward(a) ^ b);
        if let Some(slot) = sector.get_mut(code.dest + major) {
            *slot = a;
        }
        if let Some(slot) = sector.get_mut(code.dest + code.majors + major) {
            *slot = a ^ b;
        }
    }
}

/// Writes the sync pattern and recomputes P then Q parity in place. `sector`
/// is the 2352 bytes of one raw sector.
pub(crate) fn regenerate(sector: &mut [u8]) {
    if let Some(sync) = sector.get_mut(..SYNC.len()) {
        sync.copy_from_slice(&SYNC);
    }
    generate(sector, &P);
    generate(sector, &Q);
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used)]
mod tests {
    #[test]
    fn regenerating_a_sector_is_stable() {
        let mut sector = vec![0u8; 2352];
        sector[12..15].copy_from_slice(&[0x00, 0x02, 0x00]);
        sector[15] = 1;
        for (i, byte) in sector[16..16 + 2048].iter_mut().enumerate() {
            *byte = (i * 7 + 3) as u8;
        }
        super::regenerate(&mut sector);
        let once = sector.clone();
        // Wipe sync and parity, rebuild, and get the same bytes back.
        sector[..12].fill(0);
        sector[0x81c..0x930].fill(0);
        super::regenerate(&mut sector);
        assert_eq!(sector, once);
        assert_ne!(&once[0x81c..0x930], &[0u8; 0x930 - 0x81c][..]);
    }
}
