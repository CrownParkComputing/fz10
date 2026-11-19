#![forbid(unsafe_code)]
//! Hunk decompression, one decoder per codec slot.
//!
//! The general codecs decompress a hunk directly. The CD codecs treat a hunk
//! as a run of 2448-byte frames and store the 2352 bytes of sector data and the
//! 96 bytes of subcode as two separate streams, since they compress very
//! differently; a small header in front records which sectors had their sync
//! and ECC removed (to be regenerated here) and how long the sector stream is.

use std::io::Cursor;

use miniz_oxide::inflate::core::{decompress as inflate_raw, inflate_flags, DecompressorOxide};
use ruzstd::decoding::FrameDecoder;

use super::bits::BitReader;
use super::error::ChdError;
use super::flac::{self, Endian};
use super::huffman::Huffman;
use super::{ecc, ChdCodec, CD_FRAME_SIZE, CD_MAX_SECTOR_DATA, CD_MAX_SUBCODE_DATA};

const SECTOR: usize = CD_MAX_SECTOR_DATA as usize;
const SUBCODE: usize = CD_MAX_SUBCODE_DATA as usize;
const FRAME: usize = CD_FRAME_SIZE as usize;

/// The LZMA properties byte every CHD uses: lc=3, lp=0, pb=2.
const LZMA_PROPERTIES: u8 = 93;

/// The sector-data stream inside a CD hunk.
#[derive(Clone, Copy)]
enum SectorStream {
    Zlib,
    Lzma,
    Zstd,
}

/// Decodes the hunks of one codec slot.
pub(crate) struct Decoder {
    codec: ChdCodec,
    hunk_bytes: usize,
    inflater: Box<DecompressorOxide>,
    zstd: Option<Box<FrameDecoder>>,
    /// Holds the two separated streams of a CD hunk before they are
    /// interleaved back into frames.
    scratch: Vec<u8>,
}

impl Decoder {
    /// Sets a decoder up for `codec`, refusing hunk sizes it cannot work in.
    pub(crate) fn new(codec: ChdCodec, hunk_bytes: u32) -> Result<Decoder, ChdError> {
        let hunk = usize::try_from(hunk_bytes).map_err(|_| ChdError::InvalidData)?;
        let cd = matches!(
            codec,
            ChdCodec::CdZlib | ChdCodec::CdLzma | ChdCodec::CdFlac | ChdCodec::CdZstd
        );
        if cd && hunk % FRAME != 0 {
            return Err(ChdError::CodecError);
        }
        if codec == ChdCodec::Flac && hunk % 4 != 0 {
            return Err(ChdError::CodecError);
        }
        let mut scratch = Vec::new();
        if cd {
            scratch
                .try_reserve_exact(hunk)
                .map_err(|_| ChdError::OutOfMemory)?;
            scratch.resize(hunk, 0);
        }
        Ok(Decoder {
            codec,
            hunk_bytes: hunk,
            inflater: Box::default(),
            zstd: None,
            scratch,
        })
    }

    /// Decompresses one hunk from `src` into `dest`, which is exactly one
    /// hunk long.
    pub(crate) fn decompress(&mut self, src: &[u8], dest: &mut [u8]) -> Result<(), ChdError> {
        if dest.len() != self.hunk_bytes {
            return Err(ChdError::InvalidParameter);
        }
        match self.codec {
            ChdCodec::Zlib => inflate(&mut self.inflater, src, dest),
            ChdCodec::Lzma => lzma(src, dest),
            ChdCodec::Zstd => zstd(&mut self.zstd, src, dest),
            ChdCodec::Huffman => huffman(src, dest),
            ChdCodec::Flac => {
                let (&order, frames) = src.split_first().ok_or(ChdError::DecompressionError)?;
                let endian = match order {
                    b'L' => Endian::Little,
                    b'B' => Endian::Big,
                    _ => return Err(ChdError::DecompressionError),
                };
                flac::decode(frames, dest, endian).map_err(|_| ChdError::DecompressionError)?;
                Ok(())
            }
            ChdCodec::CdZlib => self.cd(SectorStream::Zlib, src, dest),
            ChdCodec::CdLzma => self.cd(SectorStream::Lzma, src, dest),
            ChdCodec::CdZstd => self.cd(SectorStream::Zstd, src, dest),
            ChdCodec::CdFlac => self.cd_flac(src, dest),
            // Laserdisc audio/video: never found in a CD image.
            ChdCodec::AvHuff => Err(ChdError::UnsupportedFormat),
            ChdCodec::None | ChdCodec::Unknown => Err(ChdError::CodecError),
        }
    }

    /// The CD codecs built on zlib, LZMA and Zstandard.
    fn cd(&mut self, stream: SectorStream, src: &[u8], dest: &mut [u8]) -> Result<(), ChdError> {
        let frames = dest.len() / FRAME;
        let length_bytes = if dest.len() < 65536 { 2 } else { 3 };
        let flag_bytes = frames.div_ceil(8);
        let header_bytes = flag_bytes + length_bytes;

        let length_field = src
            .get(flag_bytes..header_bytes)
            .ok_or(ChdError::DecompressionError)?;
        let sector_length = length_field
            .iter()
            .fold(0usize, |acc, byte| (acc << 8) | usize::from(*byte));
        let sector_end = header_bytes
            .checked_add(sector_length)
            .ok_or(ChdError::DecompressionError)?;
        let sector_src = src
            .get(header_bytes..sector_end)
            .ok_or(ChdError::DecompressionError)?;
        let subcode_src = src.get(sector_end..).ok_or(ChdError::DecompressionError)?;
        let flags = src.get(..flag_bytes).ok_or(ChdError::DecompressionError)?;

        let (sectors, subcode) = self.scratch.split_at_mut(frames * SECTOR);
        match stream {
            SectorStream::Zlib => inflate(&mut self.inflater, sector_src, sectors)?,
            SectorStream::Lzma => lzma(sector_src, sectors)?,
            SectorStream::Zstd => zstd(&mut self.zstd, sector_src, sectors)?,
        }
        let subcode = subcode
            .get_mut(..frames * SUBCODE)
            .ok_or(ChdError::DecompressionError)?;
        match stream {
            SectorStream::Zstd => zstd(&mut self.zstd, subcode_src, subcode)?,
            SectorStream::Zlib | SectorStream::Lzma => {
                inflate(&mut self.inflater, subcode_src, subcode)?
            }
        }

        interleave(&self.scratch, frames, dest)?;
        for frame in 0..frames {
            let flagged = flags
                .get(frame / 8)
                .is_some_and(|byte| byte & (1 << (frame % 8)) != 0);
            if flagged {
                let start = frame * FRAME;
                let sector = dest
                    .get_mut(start..start + SECTOR)
                    .ok_or(ChdError::DecompressionError)?;
                ecc::regenerate(sector);
            }
        }
        Ok(())
    }

    /// CD FLAC: the sector data is CD audio as big-endian samples, then the
    /// subcode follows the last FLAC frame as a zlib stream.
    fn cd_flac(&mut self, src: &[u8], dest: &mut [u8]) -> Result<(), ChdError> {
        let frames = dest.len() / FRAME;
        let (sectors, subcode) = self.scratch.split_at_mut(frames * SECTOR);
        let used =
            flac::decode(src, sectors, Endian::Big).map_err(|_| ChdError::DecompressionError)?;
        let subcode_src = src.get(used..).ok_or(ChdError::DecompressionError)?;
        let subcode = subcode
            .get_mut(..frames * SUBCODE)
            .ok_or(ChdError::DecompressionError)?;
        inflate(&mut self.inflater, subcode_src, subcode)?;
        interleave(&self.scratch, frames, dest)
    }
}

/// Rebuilds frames from the scratch layout: all sector data, then all
/// subcode.
fn interleave(scratch: &[u8], frames: usize, dest: &mut [u8]) -> Result<(), ChdError> {
    let (sectors, subcode) = scratch.split_at(frames * SECTOR);
    for ((frame, sector), sub) in dest
        .as_chunks_mut::<FRAME>()
        .0
        .iter_mut()
        .zip(sectors.as_chunks::<SECTOR>().0)
        .zip(subcode.as_chunks::<SUBCODE>().0)
    {
        let (data, tail) = frame.split_at_mut(SECTOR);
        data.copy_from_slice(sector);
        tail.copy_from_slice(sub);
    }
    Ok(())
}

/// Raw deflate. Succeeds when exactly `dest` is filled.
fn inflate(state: &mut DecompressorOxide, src: &[u8], dest: &mut [u8]) -> Result<(), ChdError> {
    state.init();
    let flags = inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;
    let (_, _, written) = inflate_raw(state, src, dest, 0, flags);
    if written == dest.len() {
        Ok(())
    } else {
        Err(ChdError::DecompressionError)
    }
}

/// The dictionary size the encoder used, which the stream does not record:
/// the level-6 default reduced to fit the data, then rounded up the way the
/// encoder rounds it.
fn lzma_dictionary_size(bytes: usize) -> u32 {
    let reduce = u32::try_from(bytes).unwrap_or(u32::MAX);
    let mut size: u32 = 1 << 26;
    if size > reduce {
        size = size.min(reduce.max(1 << 12));
    }
    if size >= 1 << 21 {
        let mask = (1u32 << 20) - 1;
        return size.checked_add(mask).map_or(size, |v| v & !mask).max(size);
    }
    let mut step = 11 * 2;
    loop {
        let aligned = (2 + (step & 1)) << (step >> 1);
        if aligned >= size {
            return aligned;
        }
        step += 1;
    }
}

/// Raw LZMA with no header, lc=3 lp=0 pb=2.
fn lzma(src: &[u8], dest: &mut [u8]) -> Result<(), ChdError> {
    use lzma_rs::decompress::{Options, UnpackedSize};
    use std::io::Read;

    let dictionary = lzma_dictionary_size(dest.len());
    let mut header = [0u8; 5];
    header[0] = LZMA_PROPERTIES;
    header[1..].copy_from_slice(&dictionary.to_le_bytes());
    let mut input = (&header[..]).chain(src);
    let wanted = dest.len();
    let mut output = Cursor::new(dest);
    let options = Options {
        unpacked_size: UnpackedSize::UseProvided(Some(wanted as u64)),
        memlimit: Some(
            usize::try_from(dictionary)
                .unwrap_or(usize::MAX)
                .max(wanted),
        ),
        allow_incomplete: false,
    };
    lzma_rs::lzma_decompress_with_options(&mut input, &mut output, &options)
        .map_err(|_| ChdError::DecompressionError)?;
    if usize::try_from(output.position()).ok() == Some(wanted) {
        Ok(())
    } else {
        Err(ChdError::DecompressionError)
    }
}

/// A Zstandard frame. Succeeds when exactly `dest` is filled.
fn zstd(
    decoder: &mut Option<Box<FrameDecoder>>,
    src: &[u8],
    dest: &mut [u8],
) -> Result<(), ChdError> {
    let decoder = decoder.get_or_insert_with(|| Box::new(FrameDecoder::new()));
    match decoder.decode_all(src, dest) {
        Ok(written) if written == dest.len() => Ok(()),
        _ => Err(ChdError::DecompressionError),
    }
}

/// CHD's own Huffman codec: a tree, then one code per byte.
fn huffman(src: &[u8], dest: &mut [u8]) -> Result<(), ChdError> {
    let mut bits = BitReader::new(src);
    let tree =
        Huffman::import_huffman(&mut bits, 256, 16).map_err(|_| ChdError::DecompressionError)?;
    for byte in dest.iter_mut() {
        *byte = tree
            .decode(&mut bits)
            .map_err(|_| ChdError::DecompressionError)? as u8;
    }
    if bits.overflowed() {
        return Err(ChdError::DecompressionError);
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used)]
mod tests {
    use super::lzma_dictionary_size;

    #[test]
    fn dictionary_sizes_follow_the_encoders_rounding() {
        // A CD hunk's sector data, 8 frames of 2352 bytes.
        assert_eq!(lzma_dictionary_size(18816), 24576);
        assert_eq!(lzma_dictionary_size(19584), 24576);
        assert_eq!(lzma_dictionary_size(100), 4096);
        assert_eq!(lzma_dictionary_size(3 << 20), 3 << 20);
    }
}
