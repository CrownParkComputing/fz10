#![forbid(unsafe_code)]
//! The checksums CHD and FLAC use, table-driven.

use std::sync::LazyLock;

fn entry16(index: usize, poly: u16) -> u16 {
    let mut crc = (index as u16) << 8;
    for _ in 0..8 {
        crc = if crc & 0x8000 != 0 {
            (crc << 1) ^ poly
        } else {
            crc << 1
        };
    }
    crc
}

fn entry8(index: usize, poly: u8) -> u8 {
    let mut crc = index as u8;
    for _ in 0..8 {
        crc = if crc & 0x80 != 0 {
            (crc << 1) ^ poly
        } else {
            crc << 1
        };
    }
    crc
}

static CCITT: LazyLock<[u16; 256]> = LazyLock::new(|| std::array::from_fn(|i| entry16(i, 0x1021)));
static FLAC16: LazyLock<[u16; 256]> = LazyLock::new(|| std::array::from_fn(|i| entry16(i, 0x8005)));
static FLAC8: LazyLock<[u8; 256]> = LazyLock::new(|| std::array::from_fn(|i| entry8(i, 0x07)));

fn update16(table: &[u16; 256], mut crc: u16, data: &[u8]) -> u16 {
    for &byte in data {
        let index = usize::from((crc >> 8) as u8 ^ byte);
        crc = (crc << 8) ^ table.get(index).copied().unwrap_or(0);
    }
    crc
}

/// CRC-16/CCITT continuing from `crc`: what CHD uses for hunks and the v5 map.
pub(crate) fn crc16_ccitt_update(crc: u16, data: &[u8]) -> u16 {
    update16(&CCITT, crc, data)
}

/// CRC-16/CCITT of `data`, starting from 0xffff.
pub(crate) fn crc16_ccitt(data: &[u8]) -> u16 {
    crc16_ccitt_update(0xffff, data)
}

/// FLAC's frame footer CRC-16 (polynomial 0x8005, initial value zero).
pub(crate) fn crc16_flac(data: &[u8]) -> u16 {
    update16(&FLAC16, 0, data)
}

/// FLAC's frame header CRC-8 (polynomial 0x07, initial value zero).
pub(crate) fn crc8_flac(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &byte in data {
        crc = FLAC8.get(usize::from(crc ^ byte)).copied().unwrap_or(0);
    }
    crc
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used)]
mod tests {
    #[test]
    fn known_check_values() {
        // The standard "123456789" check values for each algorithm.
        assert_eq!(super::crc16_ccitt(b"123456789"), 0x29b1);
        assert_eq!(super::crc16_flac(b"123456789"), 0xfee8);
        assert_eq!(super::crc8_flac(b"123456789"), 0xf4);
    }
}
