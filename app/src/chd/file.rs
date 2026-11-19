#![forbid(unsafe_code)]
//! Opening a CHD and reading its hunks and metadata.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use super::bits::BitReader;
use super::codec::Decoder;
use super::crc::{crc16_ccitt, crc16_ccitt_update};
use super::error::ChdError;
use super::huffman::Huffman;
use super::{
    ChdCodec, CDROM_OLD_METADATA_TAG, CDROM_TRACK_METADATA2_TAG, CDROM_TRACK_METADATA_TAG,
    CD_FRAME_SIZE, GDROM_OLD_METADATA_TAG, GDROM_TRACK_METADATA_TAG, HARD_DISK_METADATA_TAG, MAGIC,
};

/// Header sizes of versions 1 to 5.
const HEADER_SIZES: [u32; 5] = [76, 80, 120, 108, 124];
const MAX_HEADER_SIZE: usize = 124;

const FLAG_HAS_PARENT: u32 = 0x0000_0001;
const FLAGS_UNDEFINED: u32 = 0xffff_fffc;

/// Hunks larger than this are refused outright.
const MAX_HUNK_BYTES: u32 = 128 * 1024 * 1024;
/// A sanity cap on the decompressed size.
const MAX_LOGICAL_BYTES: u64 = 1 << 40;
/// The most hunks a map may have before it is refused as too large to hold:
/// 16M hunks of a CD's 19,584 bytes is over 300 GB of disc.
const MAX_HUNKS: u64 = 1 << 24;
/// Guards against a metadata chain that loops back on itself.
const MAX_METADATA_ENTRIES: u32 = 65536;
/// Self-references are followed at most this far.
const MAX_SELF_HOPS: u32 = 1024;

/// v3/v4 map entries are 16 bytes; v1/v2 ones 8.
const MAP_ENTRY_BYTES: u64 = 16;
const OLD_MAP_ENTRY_BYTES: u64 = 8;
const END_OF_LIST_COOKIE: &[u8; 16] = b"EndOfListCookie\0";

/// The v5 map's compression types. Values 7 to 13 only appear in the coded
/// form of the map and stand for runs and implied references.
mod v5 {
    pub const NONE: u8 = 4;
    pub const SELF: u8 = 5;
    pub const PARENT: u8 = 6;
    pub const RLE_SMALL: u32 = 7;
    pub const RLE_LARGE: u32 = 8;
    pub const SELF_0: u8 = 9;
    pub const SELF_1: u8 = 10;
    pub const PARENT_SELF: u8 = 11;
    pub const PARENT_0: u8 = 12;
    pub const PARENT_1: u8 = 13;
}

/// The parsed header of a CHD.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChdHeader {
    pub version: u32,
    /// Length of the header in bytes.
    pub length: u32,
    pub flags: u32,
    /// Codec tags (v5) or the compression number in slot 0 (v1-v4).
    pub compression: [u32; 4],
    pub logical_bytes: u64,
    pub hunk_bytes: u32,
    pub unit_bytes: u32,
    pub hunk_count: u32,
    pub map_offset: u64,
    pub meta_offset: u64,
    pub sha1: [u8; 20],
    pub parent_sha1: [u8; 20],
    pub md5: [u8; 16],
    pub parent_md5: [u8; 16],
    obsolete_cylinders: u32,
    obsolete_heads: u32,
    obsolete_sectors: u32,
    obsolete_hunk_size: u32,
}

impl ChdHeader {
    /// The codec in each slot.
    pub fn codecs(&self) -> [ChdCodec; 4] {
        if self.version >= 5 {
            self.compression.map(ChdCodec::from_tag)
        } else {
            [
                legacy_codec(self.compression[0]),
                ChdCodec::None,
                ChdCodec::None,
                ChdCodec::None,
            ]
        }
    }

    fn is_compressed(&self) -> bool {
        self.compression[0] != 0
    }
}

fn legacy_codec(number: u32) -> ChdCodec {
    match number {
        0 => ChdCodec::None,
        1 | 2 => ChdCodec::Zlib,
        3 => ChdCodec::AvHuff,
        _ => ChdCodec::Unknown,
    }
}

/// How one hunk is stored, for callers that want to know.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HunkKind {
    /// Compressed with this codec.
    Compressed(ChdCodec),
    /// Stored as is.
    Uncompressed,
    /// Eight bytes repeated across the hunk.
    Mini,
    /// The same data as the given hunk.
    SameAs(u64),
    /// Data from a parent CHD.
    Parent,
    /// Never written: all zeroes.
    Zero,
    /// A type this reader does not know.
    Invalid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// Compressed with the codec in this slot.
    Codec(u8),
    Uncompressed,
    Mini,
    SelfRef,
    ParentRef,
    Zero,
    Invalid,
}

#[derive(Clone, Copy, Debug)]
struct MapEntry {
    kind: Kind,
    /// File offset; hunk number for a self reference; the eight bytes of a
    /// mini hunk.
    offset: u64,
    length: u32,
    crc: u16,
    /// Whether `crc` is a CRC-16 of the decompressed hunk to check.
    verify: bool,
}

impl MapEntry {
    fn new(kind: Kind, offset: u64, length: u32) -> MapEntry {
        MapEntry {
            kind,
            offset,
            length,
            crc: 0,
            verify: false,
        }
    }
}

/// An open CHD.
pub struct ChdFile<R = File> {
    source: R,
    file_size: u64,
    header: ChdHeader,
    map: Vec<MapEntry>,
    decoders: [Option<Decoder>; 4],
    /// One hunk's worth of room for compressed bytes.
    compressed: Vec<u8>,
}

impl<R: Read + Seek> ChdFile<R> {
    /// Opens a CHD that needs no parent.
    pub fn open(mut source: R) -> Result<ChdFile<R>, ChdError> {
        let file_size = source
            .seek(SeekFrom::End(0))
            .map_err(|_| ChdError::InvalidFile)?;
        if file_size == 0 {
            return Err(ChdError::InvalidFile);
        }

        let header = read_header(&mut source, file_size)?;

        // A delta against a parent cannot be read on its own.
        let needs_parent = if header.version < 5 {
            header.flags & FLAG_HAS_PARENT != 0
        } else {
            header.parent_sha1.iter().any(|b| *b != 0)
        };
        if needs_parent {
            return Err(ChdError::RequiresParent);
        }

        let map = if header.version < 5 {
            read_legacy_map(&mut source, &header, file_size)?
        } else {
            read_v5_map(&mut source, &header, file_size)?
        };

        let mut compressed = Vec::new();
        let hunk = usize::try_from(header.hunk_bytes).map_err(|_| ChdError::OutOfMemory)?;
        compressed
            .try_reserve_exact(hunk)
            .map_err(|_| ChdError::OutOfMemory)?;
        compressed.resize(hunk, 0);

        let mut decoders: [Option<Decoder>; 4] = [None, None, None, None];
        let codecs = header.codecs();
        for (slot, codec) in decoders.iter_mut().zip(codecs) {
            match codec {
                ChdCodec::None => {}
                ChdCodec::Unknown => return Err(ChdError::UnsupportedFormat),
                codec => *slot = Some(Decoder::new(codec, header.hunk_bytes)?),
            }
        }

        Ok(ChdFile {
            source,
            file_size,
            header,
            map,
            decoders,
            compressed,
        })
    }

    pub fn header(&self) -> &ChdHeader {
        &self.header
    }

    /// Hunks in the file.
    pub fn hunk_count(&self) -> u32 {
        self.header.hunk_count
    }

    /// How a hunk is stored, or `None` past the end.
    pub fn hunk_kind(&self, hunk: u32) -> Option<HunkKind> {
        let entry = self.map.get(usize::try_from(hunk).ok()?)?;
        Some(match entry.kind {
            Kind::Codec(slot) => {
                let codecs = self.header.codecs();
                HunkKind::Compressed(
                    codecs
                        .get(usize::from(slot))
                        .copied()
                        .unwrap_or(ChdCodec::None),
                )
            }
            Kind::Uncompressed => HunkKind::Uncompressed,
            Kind::Mini => HunkKind::Mini,
            Kind::SelfRef => HunkKind::SameAs(entry.offset),
            Kind::ParentRef => HunkKind::Parent,
            Kind::Zero => HunkKind::Zero,
            Kind::Invalid => HunkKind::Invalid,
        })
    }

    /// Reads one hunk into the first `hunk_bytes` of `dest`.
    pub fn read_hunk(&mut self, hunk: u32, dest: &mut [u8]) -> Result<(), ChdError> {
        let hunk_bytes =
            usize::try_from(self.header.hunk_bytes).map_err(|_| ChdError::InvalidParameter)?;
        let dest = dest
            .get_mut(..hunk_bytes)
            .ok_or(ChdError::InvalidParameter)?;

        let mut current = u64::from(hunk);
        for _ in 0..=MAX_SELF_HOPS {
            if current >= u64::from(self.header.hunk_count) {
                return Err(ChdError::HunkOutOfRange);
            }
            let entry = usize::try_from(current)
                .ok()
                .and_then(|index| self.map.get(index))
                .copied()
                .ok_or(ChdError::HunkOutOfRange)?;

            match entry.kind {
                Kind::SelfRef => {
                    current = entry.offset;
                    continue;
                }
                Kind::Codec(slot) => {
                    let length = usize::try_from(entry.length).map_err(|_| ChdError::ReadError)?;
                    if length > hunk_bytes {
                        return Err(ChdError::ReadError);
                    }
                    let compressed = self
                        .compressed
                        .get_mut(..length)
                        .ok_or(ChdError::ReadError)?;
                    read_at(&mut self.source, self.file_size, entry.offset, compressed)?;
                    let decoder = self
                        .decoders
                        .get_mut(usize::from(slot))
                        .and_then(Option::as_mut)
                        .ok_or(ChdError::CodecError)?;
                    decoder.decompress(compressed, dest)?;
                }
                Kind::Uncompressed => {
                    let length = usize::try_from(entry.length).map_err(|_| ChdError::ReadError)?;
                    let target = dest.get_mut(..length).ok_or(ChdError::ReadError)?;
                    read_at(&mut self.source, self.file_size, entry.offset, target)?;
                }
                Kind::Mini => {
                    let pattern = entry.offset.to_be_bytes();
                    for (byte, value) in dest.iter_mut().zip(pattern.iter().cycle()) {
                        *byte = *value;
                    }
                }
                Kind::Zero => dest.fill(0),
                Kind::ParentRef => return Err(ChdError::RequiresParent),
                Kind::Invalid => return Err(ChdError::DecompressionError),
            }

            if entry.verify && crc16_ccitt(dest) != entry.crc {
                return Err(ChdError::DecompressionError);
            }
            return Ok(());
        }
        Err(ChdError::InvalidData)
    }

    /// The `index`th metadata item tagged `tag`, cut to at most `limit` bytes.
    pub fn metadata(&mut self, tag: u32, index: u32, limit: usize) -> Result<Vec<u8>, ChdError> {
        let entry = find_metadata(&mut self.source, self.header.meta_offset, tag, index)?;
        let length = usize::try_from(entry.length)
            .unwrap_or(usize::MAX)
            .min(limit);
        let mut data = vec![0u8; length];
        read_at(
            &mut self.source,
            self.file_size,
            entry.offset.saturating_add(16),
            &mut data,
        )?;
        Ok(data)
    }
}

/// Reads exactly `out.len()` bytes at `offset`.
fn read_at<R: Read + Seek>(
    source: &mut R,
    file_size: u64,
    offset: u64,
    out: &mut [u8],
) -> Result<(), ChdError> {
    let end = offset
        .checked_add(out.len() as u64)
        .ok_or(ChdError::ReadError)?;
    if end > file_size {
        return Err(ChdError::ReadError);
    }
    source
        .seek(SeekFrom::Start(offset))
        .map_err(|_| ChdError::ReadError)?;
    source.read_exact(out).map_err(|_| ChdError::ReadError)
}

fn be16(bytes: &[u8], at: usize) -> u16 {
    let mut word = [0u8; 2];
    if let Some(src) = bytes.get(at..at.saturating_add(2)) {
        word.copy_from_slice(src);
    }
    u16::from_be_bytes(word)
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    super::be32(bytes, at)
}

fn be48(bytes: &[u8], at: usize) -> u64 {
    (u64::from(be16(bytes, at)) << 32) | u64::from(be32(bytes, at.saturating_add(2)))
}

fn be64(bytes: &[u8], at: usize) -> u64 {
    super::be64(bytes, at)
}

fn copy<const N: usize>(bytes: &[u8], at: usize) -> [u8; N] {
    let mut out = [0u8; N];
    if let Some(src) = bytes.get(at..at.saturating_add(N)) {
        out.copy_from_slice(src);
    }
    out
}

fn read_header<R: Read + Seek>(source: &mut R, file_size: u64) -> Result<ChdHeader, ChdError> {
    let mut raw = [0u8; MAX_HEADER_SIZE];
    let start = raw.get_mut(..16).ok_or(ChdError::ReadError)?;
    read_at(source, file_size, 0, start)?;
    if raw.get(..8) != Some(&MAGIC[..]) {
        return Err(ChdError::InvalidData);
    }

    let mut header = ChdHeader {
        length: be32(&raw, 8),
        version: be32(&raw, 12),
        ..ChdHeader::default()
    };
    let expected = header
        .version
        .checked_sub(1)
        .and_then(|index| HEADER_SIZES.get(index as usize))
        .copied()
        .ok_or(ChdError::UnsupportedVersion)?;
    if header.length != expected {
        return Err(ChdError::InvalidData);
    }
    let full = raw
        .get_mut(..expected as usize)
        .ok_or(ChdError::InvalidData)?;
    read_at(source, file_size, 0, full)?;
    let raw = &raw;

    match header.version {
        1 | 2 => {
            header.flags = be32(raw, 16);
            header.compression[0] = be32(raw, 20);
            header.obsolete_hunk_size = be32(raw, 24);
            header.hunk_count = be32(raw, 28);
            header.obsolete_cylinders = be32(raw, 32);
            header.obsolete_heads = be32(raw, 36);
            header.obsolete_sectors = be32(raw, 40);
            header.md5 = copy(raw, 44);
            header.parent_md5 = copy(raw, 60);
            let sector_length = if header.version == 1 {
                512
            } else {
                be32(raw, 76)
            };
            let hunk_bytes = u64::from(sector_length) * u64::from(header.obsolete_hunk_size);
            header.logical_bytes = u64::from(header.obsolete_cylinders)
                .wrapping_mul(u64::from(header.obsolete_heads))
                .wrapping_mul(u64::from(header.obsolete_sectors))
                .wrapping_mul(u64::from(sector_length));
            header.hunk_bytes = match u32::try_from(hunk_bytes) {
                Ok(bytes) if bytes != 0 => bytes,
                _ => return Err(ChdError::InvalidData),
            };
            // These versions predate metadata; the geometry stands in for it.
            header.unit_bytes = header.hunk_bytes / header.obsolete_hunk_size.max(1);
        }
        3 | 4 => {
            header.flags = be32(raw, 16);
            header.compression[0] = be32(raw, 20);
            header.hunk_count = be32(raw, 24);
            header.logical_bytes = be64(raw, 28);
            header.meta_offset = be64(raw, 36);
            if header.version == 3 {
                header.md5 = copy(raw, 44);
                header.parent_md5 = copy(raw, 60);
                header.hunk_bytes = be32(raw, 76);
                header.sha1 = copy(raw, 80);
                header.parent_sha1 = copy(raw, 100);
            } else {
                header.hunk_bytes = be32(raw, 44);
                header.sha1 = copy(raw, 48);
                header.parent_sha1 = copy(raw, 68);
            }
            header.unit_bytes = guess_unit_bytes(source, &header)?;
        }
        _ => {
            for (slot, value) in header.compression.iter_mut().enumerate() {
                *value = be32(raw, 16 + slot * 4);
            }
            header.logical_bytes = be64(raw, 32);
            header.map_offset = be64(raw, 40);
            header.meta_offset = be64(raw, 48);
            header.hunk_bytes = be32(raw, 56);
            if header.hunk_bytes == 0 {
                return Err(ChdError::InvalidData);
            }
            let hunks = header.logical_bytes.div_ceil(u64::from(header.hunk_bytes));
            header.hunk_count = u32::try_from(hunks).map_err(|_| ChdError::InvalidData)?;
            header.unit_bytes = be32(raw, 60);
            if header.unit_bytes == 0 {
                return Err(ChdError::InvalidData);
            }
            header.sha1 = copy(raw, 84);
            header.parent_sha1 = copy(raw, 104);
        }
    }
    if header.unit_bytes == 0 {
        return Err(ChdError::InvalidData);
    }

    if header.version <= 4 {
        let zero_md5 = header.parent_md5.iter().all(|b| *b == 0);
        let zero_sha1 = header.parent_sha1.iter().all(|b| *b == 0);
        let obsolete = [
            header.obsolete_cylinders,
            header.obsolete_heads,
            header.obsolete_sectors,
            header.obsolete_hunk_size,
        ];
        if header.flags & FLAGS_UNDEFINED != 0
            || header.compression[0] > 3
            || header.hunk_bytes == 0
            || header.hunk_bytes >= 65536 * 256
            || header.hunk_count == 0
            || (header.flags & FLAG_HAS_PARENT != 0 && zero_md5 && zero_sha1)
            || (header.version < 3 && obsolete.contains(&0))
        {
            return Err(ChdError::InvalidData);
        }
    }

    let total = u64::from(header.hunk_bytes) * u64::from(header.hunk_count);
    if header.hunk_bytes >= MAX_HUNK_BYTES || total >= MAX_LOGICAL_BYTES {
        return Err(ChdError::InvalidData);
    }
    // Every hunk takes at least a bit of the map, so a small file cannot
    // honestly describe a vast number of them.
    if u64::from(header.hunk_count) > file_size.saturating_mul(8) {
        return Err(ChdError::InvalidData);
    }
    Ok(header)
}

/// For v3 and v4, which do not record it: the sector size of a hard disk,
/// a CD frame, or else the whole hunk.
fn guess_unit_bytes<R: Read + Seek>(source: &mut R, header: &ChdHeader) -> Result<u32, ChdError> {
    if let Ok(entry) = find_metadata(source, header.meta_offset, HARD_DISK_METADATA_TAG, 0) {
        let length = usize::try_from(entry.length).unwrap_or(usize::MAX).min(512);
        let mut text = vec![0u8; length];
        let read = source
            .seek(SeekFrom::Start(entry.offset.saturating_add(16)))
            .and_then(|_| source.read_exact(&mut text));
        if read.is_ok() {
            if let Some(values) = scan_hard_disk(&text) {
                return Ok(values as u32);
            }
        }
    }
    let cd_tags = [
        CDROM_OLD_METADATA_TAG,
        CDROM_TRACK_METADATA_TAG,
        CDROM_TRACK_METADATA2_TAG,
        GDROM_OLD_METADATA_TAG,
        GDROM_TRACK_METADATA_TAG,
    ];
    if cd_tags
        .iter()
        .any(|&tag| find_metadata(source, header.meta_offset, tag, 0).is_ok())
    {
        return Ok(CD_FRAME_SIZE);
    }
    Ok(header.hunk_bytes)
}

/// The bytes-per-sector field of `CYLS:%d,HEADS:%d,SECS:%d,BPS:%d`.
fn scan_hard_disk(text: &[u8]) -> Option<i32> {
    let text = text.split(|b| *b == 0).next().unwrap_or(&[]);
    let text = std::str::from_utf8(text).ok()?;
    let mut fields = text.split(',');
    let mut value = |prefix: &str| -> Option<i32> {
        let field = fields.next()?.trim_start().strip_prefix(prefix)?;
        let digits: String = field
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '-')
            .collect();
        digits.parse().ok()
    };
    value("CYLS:")?;
    value("HEADS:")?;
    value("SECS:")?;
    value("BPS:")
}

struct MetadataEntry {
    offset: u64,
    length: u32,
}

/// Walks the metadata chain for the `index`th item tagged `tag`.
fn find_metadata<R: Read + Seek>(
    source: &mut R,
    first: u64,
    tag: u32,
    mut index: u32,
) -> Result<MetadataEntry, ChdError> {
    let mut offset = first;
    let mut visited = 0u32;
    while offset != 0 {
        visited += 1;
        if visited > MAX_METADATA_ENTRIES {
            return Err(ChdError::InvalidData);
        }
        let mut raw = [0u8; 16];
        let read = source
            .seek(SeekFrom::Start(offset))
            .and_then(|_| source.read_exact(&mut raw));
        if read.is_err() {
            break;
        }
        let entry_tag = be32(&raw, 0);
        // The high byte of the length holds flags.
        let length = be32(&raw, 4) & 0x00ff_ffff;
        let next = be64(&raw, 8);
        if entry_tag == tag {
            if index == 0 {
                return Ok(MetadataEntry { offset, length });
            }
            index -= 1;
        }
        offset = next;
    }
    Err(ChdError::MetadataNotFound)
}

fn reserve_map(count: u64) -> Result<Vec<MapEntry>, ChdError> {
    if count > MAX_HUNKS {
        return Err(ChdError::OutOfMemory);
    }
    let count = usize::try_from(count).map_err(|_| ChdError::OutOfMemory)?;
    let mut map = Vec::new();
    map.try_reserve_exact(count)
        .map_err(|_| ChdError::OutOfMemory)?;
    Ok(map)
}

/// The v1-v4 map: a table of fixed-size entries straight after the header,
/// ended by a cookie.
fn read_legacy_map<R: Read + Seek>(
    source: &mut R,
    header: &ChdHeader,
    file_size: u64,
) -> Result<Vec<MapEntry>, ChdError> {
    let entry_bytes = if header.version < 3 {
        OLD_MAP_ENTRY_BYTES
    } else {
        MAP_ENTRY_BYTES
    };
    let count = u64::from(header.hunk_count);
    let table_bytes = count.checked_mul(entry_bytes).ok_or(ChdError::ReadError)?;
    let start = u64::from(header.length);
    let table_end = start.checked_add(table_bytes).ok_or(ChdError::ReadError)?;
    // The entries must be in the file before anything is allocated for them.
    if table_end > file_size {
        return Err(ChdError::ReadError);
    }

    let mut map = reserve_map(count)?;
    let mut raw = vec![0u8; usize::try_from(table_bytes).map_err(|_| ChdError::OutOfMemory)?];
    read_at(source, file_size, start, &mut raw)?;

    let mut max_offset = 0u64;
    for chunk in raw.chunks_exact(entry_bytes as usize) {
        let (kind, offset, length) = if entry_bytes == MAP_ENTRY_BYTES {
            let offset = be64(chunk, 0);
            let length =
                u32::from(be16(chunk, 12)) | (u32::from(chunk.get(14).copied().unwrap_or(0)) << 16);
            let kind = match chunk.get(15).copied().unwrap_or(0) & 0x0f {
                1 => Kind::Codec(0),
                2 => Kind::Uncompressed,
                3 => Kind::Mini,
                4 => Kind::SelfRef,
                5 => Kind::ParentRef,
                _ => Kind::Invalid,
            };
            (kind, offset, length)
        } else {
            let packed = be64(chunk, 0);
            let length = (packed >> 44) as u32;
            let offset = packed & ((1 << 44) - 1);
            let kind = if length == header.hunk_bytes {
                Kind::Uncompressed
            } else {
                Kind::Codec(0)
            };
            (kind, offset, length)
        };
        if matches!(kind, Kind::Codec(_) | Kind::Uncompressed) {
            max_offset = max_offset.max(offset.saturating_add(u64::from(length)));
        }
        // An uncompressed hunk is read whole, whatever the entry says.
        let length = if kind == Kind::Uncompressed {
            header.hunk_bytes
        } else {
            length
        };
        map.push(MapEntry::new(kind, offset, length));
    }

    let mut cookie = [0u8; 16];
    let cookie = cookie
        .get_mut(..entry_bytes as usize)
        .ok_or(ChdError::InvalidFile)?;
    read_at(source, file_size, table_end, cookie).map_err(|_| ChdError::InvalidFile)?;
    if Some(&*cookie) != END_OF_LIST_COOKIE.get(..entry_bytes as usize) || max_offset > file_size {
        return Err(ChdError::InvalidFile);
    }
    Ok(map)
}

/// The v5 map: four bytes per hunk when the file is uncompressed, otherwise
/// a Huffman-coded stream of compression types followed by the lengths,
/// offsets and CRCs they need, all checked by a CRC over the decoded map.
fn read_v5_map<R: Read + Seek>(
    source: &mut R,
    header: &ChdHeader,
    file_size: u64,
) -> Result<Vec<MapEntry>, ChdError> {
    let count = u64::from(header.hunk_count);

    if !header.is_compressed() {
        let bytes = count.checked_mul(4).ok_or(ChdError::InvalidFile)?;
        if header
            .map_offset
            .checked_add(bytes)
            .is_none_or(|end| end >= file_size)
        {
            return Err(ChdError::InvalidFile);
        }
        let mut map = reserve_map(count)?;
        let mut raw = vec![0u8; usize::try_from(bytes).map_err(|_| ChdError::OutOfMemory)?];
        read_at(source, file_size, header.map_offset, &mut raw)?;
        for chunk in raw.as_chunks::<4>().0 {
            let block = u64::from(be32(chunk, 0));
            map.push(if block == 0 {
                MapEntry::new(Kind::Zero, 0, 0)
            } else {
                MapEntry::new(
                    Kind::Uncompressed,
                    block * u64::from(header.hunk_bytes),
                    header.hunk_bytes,
                )
            });
        }
        return Ok(map);
    }

    let mut map_header = [0u8; 16];
    read_at(source, file_size, header.map_offset, &mut map_header)?;
    let map_bytes = be32(&map_header, 0);
    let first_offset = be48(&map_header, 4);
    let map_crc = be16(&map_header, 10);
    let [length_bits, self_bits, parent_bits] = copy::<3>(&map_header, 12).map(u32::from);
    if header
        .map_offset
        .checked_add(u64::from(map_bytes))
        .is_none_or(|end| end >= file_size)
    {
        return Err(ChdError::InvalidFile);
    }
    if length_bits > 32 || self_bits > 32 || parent_bits > 32 {
        return Err(ChdError::DecompressionError);
    }

    let mut coded = Vec::new();
    let coded_len = usize::try_from(map_bytes).map_err(|_| ChdError::OutOfMemory)?;
    coded
        .try_reserve_exact(coded_len)
        .map_err(|_| ChdError::OutOfMemory)?;
    coded.resize(coded_len, 0);
    read_at(
        source,
        file_size,
        header.map_offset.saturating_add(16),
        &mut coded,
    )?;
    let mut bits = BitReader::new(&coded);

    // First every hunk's compression type, run-length coded.
    let tree = Huffman::import_rle(&mut bits, 16, 8).map_err(|_| ChdError::DecompressionError)?;
    let mut types: Vec<u8> = Vec::new();
    types
        .try_reserve_exact(usize::try_from(count).map_err(|_| ChdError::OutOfMemory)?)
        .map_err(|_| ChdError::OutOfMemory)?;
    let mut repeat = 0u32;
    let mut last = 0u8;
    let decode = |bits: &mut BitReader| tree.decode(bits).map_err(|_| ChdError::DecompressionError);
    for _ in 0..count {
        if repeat > 0 {
            types.push(last);
            repeat -= 1;
            continue;
        }
        if bits.overflowed() {
            return Err(ChdError::DecompressionError);
        }
        let value = decode(&mut bits)?;
        if value == v5::RLE_SMALL {
            types.push(last);
            repeat = 2 + decode(&mut bits)?;
        } else if value == v5::RLE_LARGE {
            types.push(last);
            repeat = 2 + 16 + (decode(&mut bits)? << 4);
            repeat += decode(&mut bits)?;
        } else {
            last = u8::try_from(value).map_err(|_| ChdError::DecompressionError)?;
            types.push(last);
        }
    }

    // Then what each type needs, building the entries and the CRC of their
    // canonical twelve-byte form as it goes.
    let mut map = reserve_map(count)?;
    let mut crc = 0xffffu16;
    let mut cursor = first_offset;
    let mut last_self = 0u64;
    let mut last_parent = 0u64;
    let units_per_hunk = u64::from(header.hunk_bytes / header.unit_bytes);
    for (hunk, &coded_type) in (0u64..).zip(types.iter()) {
        let mut stored = coded_type;
        let mut offset = cursor;
        let mut length = 0u32;
        let mut hunk_crc = 0u16;
        match coded_type {
            0..=3 => {
                length = bits.read(length_bits);
                cursor = cursor.wrapping_add(u64::from(length));
                hunk_crc = bits.read(16) as u16;
            }
            v5::NONE => {
                length = header.hunk_bytes;
                cursor = cursor.wrapping_add(u64::from(length));
                hunk_crc = bits.read(16) as u16;
            }
            v5::SELF => {
                offset = u64::from(bits.read(self_bits));
                last_self = offset;
            }
            v5::PARENT => {
                offset = u64::from(bits.read(parent_bits));
                last_parent = offset;
            }
            v5::SELF_0 | v5::SELF_1 => {
                if coded_type == v5::SELF_1 {
                    last_self = last_self.wrapping_add(1);
                }
                stored = v5::SELF;
                offset = last_self;
            }
            v5::PARENT_SELF => {
                stored = v5::PARENT;
                offset =
                    hunk.wrapping_mul(u64::from(header.hunk_bytes)) / u64::from(header.unit_bytes);
                last_parent = offset;
            }
            v5::PARENT_0 | v5::PARENT_1 => {
                if coded_type == v5::PARENT_1 {
                    last_parent = last_parent.wrapping_add(units_per_hunk);
                }
                stored = v5::PARENT;
                offset = last_parent;
            }
            _ => {}
        }

        // The canonical form keeps 24 bits of length and 48 of offset.
        let length = length & 0x00ff_ffff;
        let offset = offset & 0xffff_ffff_ffff;
        let mut canonical = [0u8; 12];
        canonical[0] = stored;
        canonical[1..4].copy_from_slice(&length.to_be_bytes()[1..]);
        canonical[4..10].copy_from_slice(&offset.to_be_bytes()[2..]);
        canonical[10..12].copy_from_slice(&hunk_crc.to_be_bytes());
        crc = crc16_ccitt_update(crc, &canonical);

        let entry = match stored {
            0..=3 => MapEntry {
                kind: Kind::Codec(stored),
                offset,
                length,
                crc: hunk_crc,
                verify: true,
            },
            v5::NONE => MapEntry {
                kind: Kind::Uncompressed,
                offset,
                length,
                crc: hunk_crc,
                verify: true,
            },
            v5::SELF => MapEntry::new(Kind::SelfRef, offset, 0),
            v5::PARENT => MapEntry::new(Kind::ParentRef, offset, 0),
            _ => MapEntry::new(Kind::Invalid, offset, 0),
        };
        map.push(entry);
    }

    if crc != map_crc {
        return Err(ChdError::DecompressionError);
    }
    Ok(map)
}
