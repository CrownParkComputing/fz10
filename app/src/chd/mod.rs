#![forbid(unsafe_code)]
//! CHD ("Compressed Hunks of Data"): recognition, description and reading.
//!
//! CHD is the archive format most 3DO libraries are stored in. Two layers live
//! here:
//!
//! * [`probe_chd`] and [`ChdInfo`] look at the first bytes of a file and say
//!   whether it is a CHD, which version, how big it is once decompressed, which
//!   codecs it uses and whether it is a delta against a parent. That is enough
//!   to describe a file to a person, and to keep a CHD away from the raw-image
//!   path, where compressed bytes would be taken for disc contents.
//! * [`ChdFile`] actually reads one: the header (versions 1 to 5), the hunk map
//!   (including v5's Huffman-coded map with its CRC), the metadata chain, and
//!   every codec a CD CHD uses — zlib, LZMA, Zstandard, Huffman and FLAC, and
//!   the CD variants of the first three plus FLAC, which compress sector data
//!   and subcode separately and regenerate the sync pattern and ECC of mode 1
//!   sectors on the way out.
//!
//! Everything here treats the file as untrusted: sizes are validated before
//! anything is allocated, every read is bounds-checked, and a damaged file
//! produces a [`ChdError`] rather than a panic or a hang. No C is linked; the
//! general-purpose decompressors come from pure-Rust crates and the
//! CHD-specific pieces (map decoding, Huffman, FLAC frames, CD ECC) are
//! written here from the published format.

mod bits;
mod codec;
mod crc;
mod ecc;
mod error;
mod file;
mod flac;
mod huffman;

use std::io::{Read, Seek, SeekFrom};

pub use error::ChdError;
pub use file::{ChdFile, ChdHeader, HunkKind};

/// Builds a four-character metadata or codec tag.
pub const fn make_tag(tag: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*tag)
}

/// CD-ROM track metadata, version 2 (`CHT2`): the form chdman writes today.
pub const CDROM_TRACK_METADATA2_TAG: u32 = make_tag(b"CHT2");
/// CD-ROM track metadata, version 1 (`CHTR`).
pub const CDROM_TRACK_METADATA_TAG: u32 = make_tag(b"CHTR");
/// The old binary CD-ROM table of contents (`CHCD`).
pub const CDROM_OLD_METADATA_TAG: u32 = make_tag(b"CHCD");
/// GD-ROM track metadata (`CHGD`).
pub const GDROM_TRACK_METADATA_TAG: u32 = make_tag(b"CHGD");
/// The old GD-ROM table (`CHGT`).
pub const GDROM_OLD_METADATA_TAG: u32 = make_tag(b"CHGT");
/// Hard-disk geometry metadata (`GDDD`).
pub const HARD_DISK_METADATA_TAG: u32 = make_tag(b"GDDD");

/// Bytes of sector data in one CD frame.
pub const CD_MAX_SECTOR_DATA: u32 = 2352;
/// Bytes of subcode in one CD frame.
pub const CD_MAX_SUBCODE_DATA: u32 = 96;
/// One CD frame as a CHD stores it: sector data, then subcode.
pub const CD_FRAME_SIZE: u32 = CD_MAX_SECTOR_DATA + CD_MAX_SUBCODE_DATA;
/// Tracks are stored padded up to a multiple of this many frames.
pub const CD_TRACK_PADDING: u32 = 4;

/// Every CHD starts with this, whatever its version.
const MAGIC: &[u8; 8] = b"MComprHD";

/// The codecs a CHD may be compressed with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ChdCodec {
    #[default]
    None,
    Zlib,
    Zstd,
    Lzma,
    Huffman,
    Flac,
    CdZlib,
    CdLzma,
    CdFlac,
    CdZstd,
    AvHuff,
    Unknown,
}

impl ChdCodec {
    /// The codec a v5 four-character tag names. A zero tag is an unused slot.
    pub fn from_tag(tag: u32) -> ChdCodec {
        match &tag.to_be_bytes() {
            [0, 0, 0, 0] => ChdCodec::None,
            b"zlib" => ChdCodec::Zlib,
            b"zstd" => ChdCodec::Zstd,
            b"lzma" => ChdCodec::Lzma,
            b"huff" => ChdCodec::Huffman,
            b"flac" => ChdCodec::Flac,
            b"cdzl" => ChdCodec::CdZlib,
            b"cdlz" => ChdCodec::CdLzma,
            b"cdfl" => ChdCodec::CdFlac,
            b"cdzs" => ChdCodec::CdZstd,
            b"avhu" => ChdCodec::AvHuff,
            _ => ChdCodec::Unknown,
        }
    }

    /// A short name for the interface.
    pub fn name(self) -> &'static str {
        match self {
            ChdCodec::None => "none",
            ChdCodec::Zlib => "zlib",
            ChdCodec::Zstd => "zstd",
            ChdCodec::Lzma => "lzma",
            ChdCodec::Huffman => "huffman",
            ChdCodec::Flac => "flac",
            ChdCodec::CdZlib => "cd-zlib",
            ChdCodec::CdLzma => "cd-lzma",
            ChdCodec::CdFlac => "cd-flac",
            ChdCodec::CdZstd => "cd-zstd",
            ChdCodec::AvHuff => "av-huffman",
            ChdCodec::Unknown => "unrecognised",
        }
    }
}

/// The name of a codec, as the interface shows it.
pub fn name_of(codec: ChdCodec) -> &'static str {
    codec.name()
}

/// What a quick look at a file's header says about it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChdInfo {
    pub is_chd: bool,
    pub version: u32,
    /// Size of the image once decompressed.
    pub logical_bytes: u64,
    /// The granularity it is compressed in.
    pub hunk_bytes: u32,
    pub unit_bytes: u32,
    /// Up to four codecs may be used across a single file.
    pub codecs: [ChdCodec; 4],
    /// The file is a delta against a parent CHD and cannot be read alone.
    pub has_parent: bool,
}

impl ChdInfo {
    /// Sectors the disc would have, if the geometry can be worked out.
    pub fn sector_count(&self) -> u32 {
        // Truncating, as the size of any real disc fits comfortably.
        (self.logical_bytes / 2048) as u32
    }

    /// A short human-readable summary for the interface.
    pub fn describe(&self) -> String {
        if !self.is_chd {
            return "not a CHD".to_string();
        }
        let mut text = format!("CHD v{}", self.version);
        if self.logical_bytes > 0 {
            // Megabytes: the number is for a person deciding whether this is
            // the disc they meant, not for arithmetic.
            text.push_str(&format!(
                ", {} MB uncompressed",
                self.logical_bytes / (1024 * 1024)
            ));
        }
        let names: Vec<&str> = self
            .codecs
            .iter()
            .filter(|codec| **codec != ChdCodec::None)
            .map(|codec| codec.name())
            .collect();
        if names.is_empty() {
            text.push_str(", uncompressed");
        } else {
            text.push_str(", ");
            text.push_str(&names.join(" + "));
        }
        if self.has_parent {
            text.push_str(" (delta against a parent CHD)");
        }
        text
    }
}

/// True if `header` begins with CHD's magic. Cheap enough to call on anything.
pub fn looks_like_chd(header: &[u8]) -> bool {
    header.get(..MAGIC.len()) == Some(&MAGIC[..])
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    let mut word = [0u8; 4];
    if let Some(src) = bytes.get(at..at.saturating_add(4)) {
        word.copy_from_slice(src);
    }
    u32::from_be_bytes(word)
}

fn be64(bytes: &[u8], at: usize) -> u64 {
    (u64::from(be32(bytes, at)) << 32) | u64::from(be32(bytes, at.saturating_add(4)))
}

/// Reads as much of `buf` as the source holds, stopping quietly at its end.
fn read_up_to<R: Read>(source: &mut R, buf: &mut [u8]) -> usize {
    let mut filled = 0;
    while let Some(rest) = buf.get_mut(filled..) {
        if rest.is_empty() {
            break;
        }
        match source.read(rest) {
            Ok(0) => break,
            Ok(n) => filled = filled.saturating_add(n),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    filled
}

/// Looks at a file and decides whether it is a CHD. Restores the read
/// position, so it is safe to call before deciding what to do with the file.
pub fn probe_chd<R: Read + Seek>(file: &mut R) -> ChdInfo {
    let mut info = ChdInfo::default();
    let restore = file.stream_position().ok();

    // The largest header this understands is v5's 124 bytes.
    let mut header = [0u8; 128];
    let read = match file.seek(SeekFrom::Start(0)) {
        Ok(_) => read_up_to(file, &mut header),
        Err(_) => 0,
    };
    if let Some(position) = restore {
        let _ = file.seek(SeekFrom::Start(position));
    }

    let Some(header) = header.get(..read) else {
        return info;
    };
    if read < 16 || !looks_like_chd(header) {
        return info;
    }

    info.is_chd = true;
    let header_length = be32(header, 8);
    info.version = be32(header, 12);

    // Versions differ in where everything after the magic lives, so each is
    // read on its own terms.
    if info.version == 5 && read >= 124 && header_length >= 124 {
        for (slot, codec) in info.codecs.iter_mut().enumerate() {
            *codec = ChdCodec::from_tag(be32(header, 16 + slot * 4));
        }
        info.logical_bytes = be64(header, 32);
        info.hunk_bytes = be32(header, 56);
        info.unit_bytes = be32(header, 60);
        // A parent is named by a non-zero parent SHA-1.
        info.has_parent = header
            .get(104..124)
            .is_some_and(|sha| sha.iter().any(|b| *b != 0));
        return info;
    }

    if info.version == 4 && read >= 108 && header_length >= 108 {
        // v4 names a single compressor by number rather than by tag.
        info.codecs[0] = match be32(header, 20) {
            0 => ChdCodec::None,
            1 | 2 => ChdCodec::Zlib, // 2 is "zlib+"
            3 => ChdCodec::AvHuff,
            _ => ChdCodec::Unknown,
        };
        info.logical_bytes = be64(header, 28);
        info.hunk_bytes = be32(header, 76);
        info.has_parent = header
            .get(64..84)
            .is_some_and(|sha| sha.iter().any(|b| *b != 0));
        return info;
    }

    // A CHD of a version this does not describe. It is still definitely a CHD.
    info
}
