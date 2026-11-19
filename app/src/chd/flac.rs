#![forbid(unsafe_code)]
//! A FLAC frame decoder for CHD's audio hunks.
//!
//! CHD stores FLAC as bare frames: no `fLaC` marker and no STREAMINFO block,
//! just one frame after another, always two channels of 16-bit samples at
//! 44.1 kHz. The CD codec follows the last frame with zlib-compressed subcode,
//! so the decoder also has to say exactly where the frames ended.
//!
//! This handles everything a FLAC encoder emits for such audio: constant,
//! verbatim, fixed and LPC subframes, wasted bits, Rice and Rice2 residuals
//! with escapes, the three stereo decorrelation modes, and both frame CRCs.

use super::bits::BitReader;
use super::crc::{crc16_flac, crc8_flac};

/// The audio could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FlacError;

/// Byte order of the 16-bit samples written out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Endian {
    Little,
    Big,
}

/// Bits per sample when a frame defers to the (absent) STREAMINFO.
const DEFAULT_BITS: u32 = 16;
const CHANNELS: usize = 2;
/// The largest bit depth handled; a side channel needs one bit more.
const MAX_BITS: u32 = 24;

#[derive(Clone, Copy)]
enum Stereo {
    Independent,
    LeftSide,
    RightSide,
    MidSide,
}

struct FrameHeader {
    block_size: usize,
    stereo: Stereo,
    bits_per_sample: u32,
}

/// Decodes frames from `src` until `out` holds its fill of interleaved
/// stereo samples, and returns how many bytes of `src` those frames took.
/// `out.len()` must be a multiple of four.
pub(crate) fn decode(src: &[u8], out: &mut [u8], endian: Endian) -> Result<usize, FlacError> {
    let wanted = out.len() / 4;
    let mut produced = 0usize;
    let mut frame_start = 0usize;
    let mut channels: [Vec<i32>; CHANNELS] = [Vec::new(), Vec::new()];

    while produced < wanted {
        let frame = src.get(frame_start..).ok_or(FlacError)?;
        let (header, length) = decode_frame(frame, &mut channels)?;
        let [left, right] = &channels;
        let take = header.block_size.min(wanted - produced);
        let shift = 32 - header.bits_per_sample;
        for index in 0..take {
            let l = left.get(index).copied().ok_or(FlacError)?;
            let r = right.get(index).copied().ok_or(FlacError)?;
            let base = (produced + index) * 4;
            let slot = out.get_mut(base..base + 4).ok_or(FlacError)?;
            for (pair, sample) in slot.as_chunks_mut::<2>().0.iter_mut().zip([l, r]) {
                // Scale to 16 bits the way a 16-bit output conversion does.
                let value = (sample.wrapping_shl(shift) >> 16) as i16;
                let bytes = match endian {
                    Endian::Little => value.to_le_bytes(),
                    Endian::Big => value.to_be_bytes(),
                };
                pair.copy_from_slice(&bytes);
            }
        }
        produced += take;
        frame_start = frame_start.checked_add(length).ok_or(FlacError)?;
    }
    Ok(frame_start)
}

/// Decodes one frame into `channels` and returns its header and byte length.
fn decode_frame(
    frame: &[u8],
    channels: &mut [Vec<i32>; CHANNELS],
) -> Result<(FrameHeader, usize), FlacError> {
    let mut bits = BitReader::new(frame);
    let header = read_header(&mut bits, frame)?;

    for (index, samples) in channels.iter_mut().enumerate() {
        let side = matches!(
            (header.stereo, index),
            (Stereo::LeftSide, 1) | (Stereo::RightSide, 0) | (Stereo::MidSide, 1)
        );
        let depth = header.bits_per_sample + u32::from(side);
        samples.clear();
        samples.resize(header.block_size, 0);
        read_subframe(&mut bits, depth, samples)?;
    }

    // Zero padding to a byte boundary, then the CRC of everything before it.
    bits.align_to_byte();
    let body_end = bits.byte_position();
    let crc = bits.read(16) as u16;
    if bits.overflowed() || crc16_flac(frame.get(..body_end).ok_or(FlacError)?) != crc {
        return Err(FlacError);
    }

    let [first, second] = channels;
    for (a, b) in first.iter_mut().zip(second.iter_mut()) {
        let (x, y) = (i64::from(*a), i64::from(*b));
        let (left, right) = match header.stereo {
            Stereo::Independent => (x, y),
            Stereo::LeftSide => (x, x - y),
            Stereo::RightSide => (x + y, y),
            Stereo::MidSide => {
                let mid = (x << 1) | (y & 1);
                ((mid + y) >> 1, (mid - y) >> 1)
            }
        };
        *a = left as i32;
        *b = right as i32;
    }
    Ok((header, body_end + 2))
}

fn read_header(bits: &mut BitReader, frame: &[u8]) -> Result<FrameHeader, FlacError> {
    // Fourteen-bit sync code, a reserved zero, then the blocking strategy.
    if bits.read(14) != 0x3ffe || bits.read(1) != 0 {
        return Err(FlacError);
    }
    let _variable_blocks = bits.read(1);
    let size_code = bits.read(4);
    let rate_code = bits.read(4);
    let channel_code = bits.read(4);
    let depth_code = bits.read(3);
    if bits.read(1) != 0 {
        return Err(FlacError);
    }

    // The frame or sample number, in UTF-8's variable-length form.
    let lead = bits.read(8) as u8;
    let extra = match lead.leading_ones() {
        0 => 0,
        n @ 2..=7 => n - 1,
        _ => return Err(FlacError),
    };
    for _ in 0..extra {
        if bits.read(2) != 0b10 {
            return Err(FlacError);
        }
        bits.skip(6);
    }

    let block_size = match size_code {
        1 => 192,
        2..=5 => 576 << (size_code - 2),
        6 => bits.read(8) as usize + 1,
        7 => bits.read(16) as usize + 1,
        8..=15 => 256 << (size_code - 8),
        _ => return Err(FlacError),
    };
    match rate_code {
        12 => bits.skip(8),
        13 | 14 => bits.skip(16),
        15 => return Err(FlacError),
        _ => {}
    }
    let stereo = match channel_code {
        1 => Stereo::Independent,
        8 => Stereo::LeftSide,
        9 => Stereo::RightSide,
        10 => Stereo::MidSide,
        // CHD's audio is always two channels.
        _ => return Err(FlacError),
    };
    let bits_per_sample = match depth_code {
        0 => DEFAULT_BITS,
        1 => 8,
        2 => 12,
        4 => 16,
        5 => 20,
        6 => 24,
        _ => return Err(FlacError),
    };

    let header_end = bits.byte_position();
    let crc = bits.read(8) as u8;
    if bits.overflowed() || crc8_flac(frame.get(..header_end).ok_or(FlacError)?) != crc {
        return Err(FlacError);
    }
    Ok(FrameHeader {
        block_size,
        stereo,
        bits_per_sample,
    })
}

fn read_subframe(bits: &mut BitReader, depth: u32, samples: &mut [i32]) -> Result<(), FlacError> {
    if bits.read(1) != 0 {
        return Err(FlacError);
    }
    let kind = bits.read(6);
    let wasted = if bits.read(1) == 1 {
        bits.read_unary().ok_or(FlacError)? + 1
    } else {
        0
    };
    if wasted >= depth {
        return Err(FlacError);
    }
    let depth = depth - wasted;
    if depth > MAX_BITS + 1 {
        return Err(FlacError);
    }

    match kind {
        0 => {
            let value = bits.read_signed(depth);
            samples.fill(value);
        }
        1 => {
            for sample in samples.iter_mut() {
                *sample = bits.read_signed(depth);
            }
        }
        8..=12 => {
            let order = (kind - 8) as usize;
            read_warmup(bits, depth, order, samples)?;
            read_residual(bits, order, samples)?;
            restore_fixed(order, samples)?;
        }
        32..=63 => {
            let order = (kind - 31) as usize;
            read_warmup(bits, depth, order, samples)?;
            let precision = bits.read(4) + 1;
            if precision == 16 {
                return Err(FlacError);
            }
            let shift = bits.read_signed(5);
            if shift < 0 {
                return Err(FlacError);
            }
            let mut coefficients = [0i64; 32];
            for coefficient in coefficients.iter_mut().take(order) {
                *coefficient = i64::from(bits.read_signed(precision));
            }
            read_residual(bits, order, samples)?;
            restore_lpc(
                coefficients.get(..order).ok_or(FlacError)?,
                shift as u32,
                samples,
            )?;
        }
        _ => return Err(FlacError),
    }

    if wasted > 0 {
        for sample in samples.iter_mut() {
            *sample = sample.wrapping_shl(wasted);
        }
    }
    if bits.overflowed() {
        return Err(FlacError);
    }
    Ok(())
}

fn read_warmup(
    bits: &mut BitReader,
    depth: u32,
    order: usize,
    samples: &mut [i32],
) -> Result<(), FlacError> {
    if order > samples.len() {
        return Err(FlacError);
    }
    for sample in samples.iter_mut().take(order) {
        *sample = bits.read_signed(depth);
    }
    Ok(())
}

/// Reads the residual into `samples[order..]`.
fn read_residual(bits: &mut BitReader, order: usize, samples: &mut [i32]) -> Result<(), FlacError> {
    let (parameter_bits, escape) = match bits.read(2) {
        0 => (4, 15),
        1 => (5, 31),
        _ => return Err(FlacError),
    };
    let partition_order = bits.read(4);
    let partitions = 1usize << partition_order;
    let block_size = samples.len();
    if !block_size.is_multiple_of(partitions) {
        return Err(FlacError);
    }
    let per_partition = block_size / partitions;
    if per_partition < order {
        return Err(FlacError);
    }

    let mut at = order;
    for partition in 0..partitions {
        let count = if partition == 0 {
            per_partition - order
        } else {
            per_partition
        };
        let parameter = bits.read(parameter_bits);
        let slots = samples.get_mut(at..at + count).ok_or(FlacError)?;
        if parameter == escape {
            let width = bits.read(5);
            for slot in slots.iter_mut() {
                *slot = if width == 0 {
                    0
                } else {
                    bits.read_signed(width)
                };
            }
        } else {
            for slot in slots.iter_mut() {
                let quotient = u64::from(bits.read_unary().ok_or(FlacError)?);
                let remainder = u64::from(bits.read(parameter));
                let folded = (quotient << parameter) | remainder;
                // Undo the zigzag folding of signed values.
                let value = (folded >> 1) as i64 ^ -((folded & 1) as i64);
                *slot = value as i32;
            }
        }
        if bits.overflowed() {
            return Err(FlacError);
        }
        at += count;
    }
    Ok(())
}

fn restore_fixed(order: usize, samples: &mut [i32]) -> Result<(), FlacError> {
    const COEFFICIENTS: [&[i64]; 5] = [&[], &[1], &[2, -1], &[3, -3, 1], &[4, -6, 4, -1]];
    let coefficients = COEFFICIENTS.get(order).ok_or(FlacError)?;
    restore_lpc(coefficients, 0, samples)
}

/// Adds the prediction from the previous samples to each residual.
fn restore_lpc(coefficients: &[i64], shift: u32, samples: &mut [i32]) -> Result<(), FlacError> {
    let order = coefficients.len();
    for index in order..samples.len() {
        let mut sum = 0i64;
        for (tap, coefficient) in coefficients.iter().enumerate() {
            let past = samples.get(index - 1 - tap).copied().ok_or(FlacError)?;
            sum = sum.wrapping_add(coefficient.wrapping_mul(i64::from(past)));
        }
        let slot = samples.get_mut(index).ok_or(FlacError)?;
        *slot = (i64::from(*slot).wrapping_add(sum >> shift.min(63))) as i32;
    }
    Ok(())
}
