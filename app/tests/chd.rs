#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::panic
)]
//! CHD recognition and reading.
//!
//! The first tests are ported from the C++ suite (tests/test_chd.cpp): a CHD
//! must be recognised and described, and one that cannot be read must be
//! REFUSED with a reason rather than misread as a raw image. The rest cover
//! the reader itself: an embedded chdman-made CD CHD (LZMA, FLAC and
//! self-referenced hunks, with sync and ECC to regenerate), hand-built v4 and
//! v5 files, damaged files, and — when the fixtures folder is present — CHDs
//! made with every codec compared against the images they were made from.

use std::fs::{self, File};
use std::io::{Cursor, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use retro3do::chd::{looks_like_chd, probe_chd, ChdCodec, ChdError, ChdFile, ChdInfo, HunkKind};
use retro3do::disc::Disc;

/// A private folder in the temp directory, removed when dropped.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(test: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("retro3do-chd-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Scratch { dir }
    }

    fn path(&self, name: &str) -> String {
        self.dir.join(name).to_string_lossy().into_owned()
    }

    fn write(&self, name: &str, bytes: &[u8]) -> String {
        let path = self.path(name);
        fs::write(&path, bytes).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn put_be32(v: &mut [u8], at: usize, value: u32) {
    v[at..at + 4].copy_from_slice(&value.to_be_bytes());
}

fn put_be64(v: &mut [u8], at: usize, value: u64) {
    v[at..at + 8].copy_from_slice(&value.to_be_bytes());
}

/// A v5 header, which is what any modern dump is.
fn make_chd_v5(logical: u64, tag0: u32, tag1: u32, with_parent: bool) -> Vec<u8> {
    let mut v = vec![0u8; 4096];
    v[..8].copy_from_slice(b"MComprHD");
    put_be32(&mut v, 8, 124); // header length
    put_be32(&mut v, 12, 5); // version
    put_be32(&mut v, 16, tag0);
    put_be32(&mut v, 20, tag1);
    put_be64(&mut v, 32, logical);
    put_be32(&mut v, 56, 19584); // hunk bytes
    put_be32(&mut v, 60, 2448); // unit bytes
    if with_parent {
        v[104] = 0xab; // a non-zero parent SHA-1
    }
    v
}

const TAG_CD_ZLIB: u32 = 0x6364_7a6c;
const TAG_CD_FLAC: u32 = 0x6364_666c;
const TAG_ZSTD: u32 = 0x7a73_7464;

fn probe_file(path: &str) -> ChdInfo {
    let mut file = File::open(path).unwrap();
    probe_chd(&mut file)
}

// ---------------------------------------------------------------------------
// Ported from tests/test_chd.cpp
// ---------------------------------------------------------------------------

#[test]
fn the_magic_identifies_a_chd() {
    let good = *b"MComprHD";
    let bad = *b"Notachd!";
    assert!(looks_like_chd(&good));
    assert!(!looks_like_chd(&bad));
    // Too short to tell must not read past the buffer.
    assert!(!looks_like_chd(&good[..4]));
    // The C++ null pointer: nothing at all.
    assert!(!looks_like_chd(&[]));
}

#[test]
fn a_v5_header_yields_its_size_and_codecs() {
    let scratch = Scratch::new("v5");
    let path = scratch.write(
        "v5.chd",
        &make_chd_v5(650 * 1024 * 1024, TAG_CD_ZLIB, TAG_CD_FLAC, false),
    );
    let info = probe_file(&path);

    assert!(info.is_chd);
    assert_eq!(info.version, 5);
    assert_eq!(info.logical_bytes, 650 * 1024 * 1024);
    assert_eq!(info.hunk_bytes, 19584);
    assert_eq!(info.codecs[0], ChdCodec::CdZlib);
    assert_eq!(info.codecs[1], ChdCodec::CdFlac);
    assert_eq!(info.codecs[2], ChdCodec::None);
    assert!(!info.has_parent);
}

#[test]
fn a_parent_chd_is_noticed() {
    // A delta against a parent cannot be read on its own however good the
    // decompressor is, so it is worth telling apart from a plain one.
    let scratch = Scratch::new("parent");
    let path = scratch.write(
        "parent.chd",
        &make_chd_v5(100 * 1024 * 1024, TAG_ZSTD, 0, true),
    );
    let info = probe_file(&path);

    assert!(info.is_chd);
    assert!(info.has_parent);
    assert!(info.describe().contains("parent"));
}

#[test]
fn probing_does_not_disturb_the_read_position() {
    // probe_chd is called before deciding what to do with a file, so it must
    // leave it exactly as it found it.
    let scratch = Scratch::new("seek");
    let path = scratch.write("seek.chd", &make_chd_v5(1024, TAG_ZSTD, 0, false));

    let mut file = File::open(&path).unwrap();
    file.seek(SeekFrom::Start(64)).unwrap();
    probe_chd(&mut file);
    assert_eq!(file.stream_position().unwrap(), 64);
}

#[test]
fn an_ordinary_image_is_not_mistaken_for_a_chd() {
    let scratch = Scratch::new("plain");
    let path = scratch.write("plain.iso", &[0x42u8; 4096]);
    let info = probe_file(&path);
    assert!(!info.is_chd);
    assert_eq!(info.describe(), "not a CHD");
}

#[test]
fn a_synthetic_chd_header_alone_is_not_a_readable_disc() {
    // A header with no hunks and no track metadata behind it is not a disc.
    // It must fail with a reason naming the file and must NOT report a
    // sector count: falling through to the raw-image path would take
    // compressed bytes for disc contents.
    let scratch = Scratch::new("refuse");
    let path = scratch.write(
        "refuse.chd",
        &make_chd_v5(650 * 1024 * 1024, TAG_CD_ZLIB, 0, false),
    );

    let mut disc = Disc::new();
    assert!(!disc.open(&path));
    assert!(!disc.is_open());
    assert_eq!(disc.sector_count(), 0);

    // And it must still be recognised as a CHD, not as something unknown.
    assert!(disc.is_chd());
    assert!(disc.chd().is_chd);
    assert!(!disc.last_error().is_empty());
}

#[test]
fn the_description_names_the_version_size_and_compression() {
    let scratch = Scratch::new("describe");
    let path = scratch.write(
        "describe.chd",
        &make_chd_v5(650 * 1024 * 1024, TAG_CD_ZLIB, TAG_CD_FLAC, false),
    );
    let text = probe_file(&path).describe();

    assert!(text.contains("CHD v5"));
    assert!(text.contains("650 MB"));
    assert!(text.contains("cd-zlib"));
    assert!(text.contains("cd-flac"));
}

#[test]
fn an_uncompressed_chd_says_so() {
    let scratch = Scratch::new("raw");
    let path = scratch.write("raw.chd", &make_chd_v5(1024 * 1024, 0, 0, false));
    assert!(probe_file(&path).describe().contains("uncompressed"));
}

#[test]
fn a_chd_of_an_unparsed_version_is_still_reported_as_a_chd() {
    // Better to say "a CHD I do not understand" than "an unrecognisable file".
    let mut v = make_chd_v5(1024, TAG_ZSTD, 0, false);
    put_be32(&mut v, 12, 99); // a version from the future
    let scratch = Scratch::new("future");
    let path = scratch.write("future.chd", &v);
    let info = probe_file(&path);

    assert!(info.is_chd);
    assert_eq!(info.version, 99);
    assert!(info.describe().contains("CHD v99"));
}

#[test]
fn a_truncated_file_that_starts_like_a_chd_does_not_read_past_its_end() {
    let mut v = vec![0u8; 20];
    v[..8].copy_from_slice(b"MComprHD");
    put_be32(&mut v, 8, 124);
    put_be32(&mut v, 12, 5);
    let scratch = Scratch::new("short");
    let path = scratch.write("short.chd", &v);
    let info = probe_file(&path);

    // Identified, but no fields invented from bytes that are not there.
    assert!(info.is_chd);
    assert_eq!(info.logical_bytes, 0);
}

// ---------------------------------------------------------------------------
// An embedded CD CHD made by chdman 0.288 (`createcd -hs 4896`, default
// codecs) from a 4-sector MODE1/2352 track with valid EDC/ECC and a 9-frame
// audio track: a sawtooth for five frames, then silence.
// ---------------------------------------------------------------------------

const TINY_CHD_HEX: &[&str] = &[
    "4d436f6d707248440000007c0000000563646c7a63647a6c6364666c000000000000000000009900000000000000078e",
    "000000000000007c0000132000000990f51c5b4fbca6e1dbc8576d8ea0dd4621b75a65a93f14f5e295ae8ec6190d917c",
    "f42c3cd9ba0084770000000000000000000000000000000000000000434854320100005800000000000000e454524143",
    "4b3a3120545950453a4d4f4445315f52415720535542545950453a4e4f4e45204652414d45533a34205052454741503a",
    "30205047545950453a4d4f4445312050475355423a4e4f4e4520504f53544741503a3000434854320100005400000000",
    "00000000545241434b3a3220545950453a415544494f20535542545950453a4e4f4e45204652414d45533a3920505245",
    "4741503a30205047545950453a4d4f4445312050475355423a4e4f4e4520504f53544741503a300003007f00006c7e8d",
    "ff108a5f9fcfb1889ad899997c6adf0a990288c7e7e13020696a637647eb0b9c0d1f922fc02234d6a25aacd8d2fb41c3",
    "a778fa95de627e35bcff1f61b261bec181f695e9dc7e799cc9ea8372d24c32891f2eead9c7e60d0ce3dea60e84bb6296",
    "9655f1851b5a80dde2720633fb461344034fe52f6b238f992600636018da000003008600006c7ea5ffdf8b67e0ab868c",
    "0017a9e30d096749ab5da22eb620aa72add16763962eacd98e44070fe79f7cd6eec2028343da91db9d2c9a4f456e0cd0",
    "f4abb62f51d2586285b67516bcc17542364a1654f7fb0761f858b1d862177c47aaa4adccd36b590b8c8d9e15ab055583",
    "6d9cad269c395acbd46f8f5e6505c23fc1b7fa2712c8a6236c636018da0000fff879a80004972600000015fc187c1980",
    "1fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    "ffffff806011636018da0000fff879a8000497260000004382fc02fdd25ffe010c092492492492492492492492492492",
    "4924924924924924924924924924924924924924924924924924924924924924924924924924924b42850a142850a142",
    "85080000000000000000000000000000003e800000000000000000000000000000072850a142850a142850a142850a14",
    "2850a142850a142850a142850a142850a142850a142850a142850a142850a142850a142850a142850a142850a142850a",
    "142850a142850a142850a142850a142850a142850a142850a142850a142850a142850a142850a142850a142850a14285",
    "0a142850a142850809249249249249249249249249249249249249249249249249249249249249249249249249249249",
    "249249249249249249249249249249204924924924924924924924924924924924924924924924924924924924924924",
    "924924924924924924924924924924924924924924924902492492492492492492492492492492492492492492492492",
    "492492492492492492492492492492492492492492492492492492492492481249249249249249249249249249249249",
    "249249249249249249249249249249249249249249249249249249249249249249249249249240924924924924924924",
    "924924924924924924924924924924924924924924924924924924924924924924924924924924924924924924920492",
    "492492492492492492492492492492492492492492492492492492492492492492492492492492492492492492492492",
    "492492492490271d636018da0000fff879a800049726000000138210064ccccccccccccccccccccccccccccccccccccc",
    "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
    "ccccccccccccc4cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
    "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccb34d34d34d34d34d34d34d34d34d34d",
    "300000000000000000000000000000000000000000000000000000000000000399a69a69a69a69a69a69a69a69a69a69",
    "a69a69a69a69a69a69a69a69a69a69a69a69a69a69a69a69a69a69a69a69a69a69a69a69a69a69a69a69a69a69a69a69",
    "a69a69a69a69a69a69a69a69a69a69a69a69a69a69a69a69a69a69a69a69899999999999999999999999999999999999",
    "999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999999",
    "9999999999999999900000000000000000000000000000000000007a1084210842108421084210842108421084210842",
    "108421084210842108421084210842108421084210842108421084210842108421084210842108421084210842108421",
    "08421084210842108421084210842108421084210842100ffffffffffffffffffffffffffffffffffffe1fffffffffff",
    "fffffffffffffffffffffffffc3ffffffffffffffffffffffffffffffffffff89946636018da0000fff87918000497be",
    "000000000000de4e636018da00000000001d000000000148eda20a03000011030030303103c29644012191f908a56861",
    "fc85d32d1556e502c5e1d4",
];

fn tiny_chd() -> Vec<u8> {
    let hex: String = TINY_CHD_HEX.concat();
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

/// The user data the tiny CHD's data sectors were made with.
fn tiny_payload(lba: u32) -> Vec<u8> {
    (0..2048u32)
        .map(|i| ((i / 64) * (lba + 1) + lba) as u8)
        .collect()
}

/// The audio track's samples as CHD stores them: big-endian.
fn tiny_audio_frame(frame: u32) -> Vec<u8> {
    let mut out = Vec::new();
    for n in frame * 588..(frame + 1) * 588 {
        let v: i16 = if frame < 5 {
            ((n * 3) % 4000) as i16 - 2000
        } else {
            0
        };
        out.extend_from_slice(&v.to_be_bytes());
        out.extend_from_slice(&(-v).to_be_bytes());
    }
    out
}

#[test]
fn the_embedded_cd_chd_reads_as_a_disc() {
    let scratch = Scratch::new("tiny");
    let path = scratch.write("tiny.chd", &tiny_chd());

    let mut disc = Disc::new();
    assert!(disc.open(&path), "{}", disc.last_error());
    assert!(disc.is_chd());
    assert_eq!(disc.sector_count(), 4);
    assert_eq!(disc.tracks().len(), 2);
    assert_eq!(
        (disc.tracks()[0].number, disc.tracks()[0].is_audio),
        (1, false)
    );
    assert_eq!(
        (disc.tracks()[1].start_lba, disc.tracks()[1].length_sectors),
        (4, 9)
    );
    assert!(disc.tracks()[1].is_audio);

    for lba in 0..4 {
        let mut sector = [0u8; 2048];
        assert!(disc.read_sector(lba, &mut sector));
        assert_eq!(sector.to_vec(), tiny_payload(lba), "sector {lba}");
    }
    let mut sector = [0u8; 2048];
    assert!(!disc.read_sector(4, &mut sector));

    // A raw read of a MODE1_RAW CHD starts at the payload, as the C++ does.
    let mut raw = [0u8; 2352];
    assert_eq!(disc.read_raw_sector(2, &mut raw), Some(2352));
    assert_eq!(raw[..2048].to_vec(), tiny_payload(2));
}

#[test]
fn the_embedded_cd_chd_regenerates_sync_and_ecc_and_decodes_flac() {
    let mut chd = ChdFile::open(Cursor::new(tiny_chd())).unwrap();
    assert_eq!(chd.header().hunk_bytes, 4896);
    assert_eq!(chd.header().unit_bytes, 2448);

    let kinds: Vec<HunkKind> = (0..chd.hunk_count())
        .map(|h| chd.hunk_kind(h).unwrap())
        .collect();
    assert!(kinds.contains(&HunkKind::Compressed(ChdCodec::CdLzma)));
    assert!(kinds.contains(&HunkKind::Compressed(ChdCodec::CdFlac)));
    assert!(kinds.iter().any(|k| matches!(k, HunkKind::SameAs(_))));

    // Every hunk reads, and each is checked against chdman's CRC on the way.
    let mut hunks = Vec::new();
    for hunk in 0..chd.hunk_count() {
        let mut buf = vec![0u8; 4896];
        chd.read_hunk(hunk, &mut buf).unwrap();
        hunks.extend_from_slice(&buf);
    }
    let frame = |n: usize| &hunks[n * 2448..n * 2448 + 2352];

    // Data frames: sync, an MSF header in BCD, mode 1, the payload. The
    // compressor dropped the sync and ECC; they had to be rebuilt to match.
    for lba in 0..4u32 {
        let f = frame(lba as usize);
        assert_eq!(
            &f[..12],
            &[0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0]
        );
        assert_eq!(&f[12..16], &[0x00, 0x02, lba as u8, 1]);
        assert_eq!(f[16..16 + 2048].to_vec(), tiny_payload(lba));
    }
    // The audio track starts at CHD frame 4 (four data frames, already a
    // multiple of four) and runs nine frames.
    for n in 0..9u32 {
        assert_eq!(
            frame(4 + n as usize).to_vec(),
            tiny_audio_frame(n),
            "audio frame {n}"
        );
    }
    assert!(chd.read_hunk(chd.hunk_count(), &mut [0u8; 4896]).is_err());
}

#[test]
fn every_single_byte_corruption_of_a_chd_fails_cleanly() {
    // Flip each byte of the file in turn. Opening may succeed or fail, and
    // reads may succeed or fail, but nothing may panic or hang.
    let original = tiny_chd();
    for at in 0..original.len() {
        for flip in [0x01u8, 0x80, 0xff] {
            let mut bytes = original.clone();
            bytes[at] ^= flip;
            if let Ok(mut chd) = ChdFile::open(Cursor::new(bytes)) {
                let mut buf = vec![0u8; chd.header().hunk_bytes as usize];
                for hunk in 0..chd.hunk_count().min(64) {
                    let _ = chd.read_hunk(hunk, &mut buf);
                }
                let _ = chd.metadata(retro3do::chd::CDROM_TRACK_METADATA2_TAG, 0, 512);
            }
        }
    }
}

#[test]
fn every_truncation_of_a_chd_fails_cleanly() {
    let original = tiny_chd();
    for length in 0..original.len() {
        if let Ok(mut chd) = ChdFile::open(Cursor::new(original[..length].to_vec())) {
            let mut buf = vec![0u8; chd.header().hunk_bytes as usize];
            let mut any_failed = false;
            for hunk in 0..chd.hunk_count() {
                any_failed |= chd.read_hunk(hunk, &mut buf).is_err();
            }
            // The last hunk's bytes are at the end of the file, so a shorter
            // file cannot read them all.
            assert!(any_failed, "a {length}-byte prefix read every hunk");
        }
    }
}

#[test]
fn a_delta_chd_is_refused_with_requires_parent() {
    let mut bytes = tiny_chd();
    bytes[104] = 0x5a; // a parent SHA-1
    assert_eq!(
        ChdFile::open(Cursor::new(bytes.clone())).err(),
        Some(ChdError::RequiresParent)
    );

    let scratch = Scratch::new("delta");
    let path = scratch.write("delta.chd", &bytes);
    let mut disc = Disc::new();
    assert!(!disc.open(&path));
    assert!(disc.chd().has_parent);
    assert!(
        disc.last_error().ends_with("delta.chd: requires parent"),
        "{}",
        disc.last_error()
    );
}

// ---------------------------------------------------------------------------
// Hand-built files for the map forms chdman no longer writes
// ---------------------------------------------------------------------------

/// Appends a metadata entry and links the previous one (or the header field
/// at `link`) to it.
fn add_metadata(file: &mut Vec<u8>, link: usize, tag: &[u8; 4], text: &str) -> usize {
    let at = file.len();
    put_be64(file, link, at as u64);
    let mut data = text.as_bytes().to_vec();
    data.push(0);
    file.extend_from_slice(tag);
    file.extend_from_slice(&(data.len() as u32 | 0x0100_0000).to_be_bytes());
    file.extend_from_slice(&0u64.to_be_bytes());
    file.extend_from_slice(&data);
    at + 8
}

/// Two-frame hunks of cooked data: each frame holds a 2048-byte sector
/// filled with its number, then zeros to the end of the 2448-byte frame.
fn cooked_hunk(first_frame: u32) -> Vec<u8> {
    let mut hunk = vec![0u8; 4896];
    for i in 0..2 {
        let start = i * 2448;
        hunk[start..start + 2048].fill(first_frame as u8 + i as u8 + 1);
    }
    hunk
}

#[test]
fn a_v4_chd_reads_compressed_uncompressed_mini_and_self_hunks() {
    let hunk_bytes = 4896u32;
    let hunks = 4u32;
    let mut file = vec![0u8; 108];
    file[..8].copy_from_slice(b"MComprHD");
    put_be32(&mut file, 8, 108);
    put_be32(&mut file, 12, 4);
    put_be32(&mut file, 20, 1); // zlib
    put_be32(&mut file, 24, hunks);
    put_be64(&mut file, 28, u64::from(hunks * hunk_bytes));
    put_be32(&mut file, 44, hunk_bytes);

    // The map, then its end cookie.
    let map_at = file.len();
    file.resize(map_at + 16 * hunks as usize, 0);
    file.extend_from_slice(b"EndOfListCookie\0");
    add_metadata(
        &mut file,
        36,
        b"CHTR",
        "TRACK:1 TYPE:MODE1 SUBTYPE:NONE FRAMES:8",
    );

    let compressed = miniz_oxide::deflate::compress_to_vec(&cooked_hunk(0), 6);
    let entry = |hunk: usize, offset: u64, length: u32, kind: u8, file: &mut Vec<u8>| {
        let at = map_at + hunk * 16;
        put_be64(file, at, offset);
        file[at + 12..at + 14].copy_from_slice(&(length as u16).to_be_bytes());
        file[at + 14] = (length >> 16) as u8;
        file[at + 15] = kind;
    };
    let offset = file.len() as u64;
    file.extend_from_slice(&compressed);
    entry(0, offset, compressed.len() as u32, 1, &mut file);
    let offset = file.len() as u64;
    file.extend_from_slice(&cooked_hunk(2));
    entry(1, offset, hunk_bytes, 2, &mut file);
    entry(2, 0x0505_0505_0505_0505, 0, 3, &mut file);
    entry(3, 0, 0, 4, &mut file);

    let scratch = Scratch::new("v4");
    let path = scratch.write("v4.chd", &file);
    let mut disc = Disc::new();
    assert!(disc.open(&path), "{}", disc.last_error());
    assert_eq!(disc.chd().version, 4);
    assert_eq!(disc.sector_count(), 8);
    let mut sector = [0u8; 2048];
    for lba in 0..4 {
        assert!(disc.read_sector(lba, &mut sector));
        assert!(sector.iter().all(|b| *b == lba as u8 + 1), "sector {lba}");
    }
    for lba in 4..6 {
        assert!(disc.read_sector(lba, &mut sector));
        assert!(sector.iter().all(|b| *b == 5), "mini sector {lba}");
    }
    // Hunk 3 is the same as hunk 0.
    assert!(disc.read_sector(6, &mut sector));
    assert!(sector.iter().all(|b| *b == 1));

    // Without its end cookie the map is refused.
    let mut broken = file.clone();
    broken[map_at + 16 * hunks as usize] ^= 1;
    let path = scratch.write("v4-broken.chd", &broken);
    assert!(!disc.open(&path));
    assert!(
        disc.last_error().ends_with(": invalid file"),
        "{}",
        disc.last_error()
    );
}

#[test]
fn an_uncompressed_v5_chd_maps_audio_padding_and_unwritten_hunks() {
    // Track 1 is three audio frames, padded to four inside the CHD; track 2
    // is two MODE1_RAW frames starting at CHD frame 4, in hunk 2. Hunk 1
    // was never written and reads as zeros.
    let hunk_bytes = 4896u32;
    let mut file = vec![0u8; 124];
    file[..8].copy_from_slice(b"MComprHD");
    put_be32(&mut file, 8, 124);
    put_be32(&mut file, 12, 5);
    put_be64(&mut file, 32, 3 * u64::from(hunk_bytes));
    put_be64(&mut file, 40, 124); // map offset
    put_be32(&mut file, 56, hunk_bytes);
    put_be32(&mut file, 60, 2448);
    file.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2]);
    let link = add_metadata(
        &mut file,
        48,
        b"CHT2",
        "TRACK:1 TYPE:AUDIO SUBTYPE:NONE FRAMES:3 PREGAP:0 PGTYPE:MODE1 PGSUB:NONE POSTGAP:0",
    );
    add_metadata(
        &mut file,
        link,
        b"CHT2",
        "TRACK:2 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:2 PREGAP:0 PGTYPE:MODE1 PGSUB:NONE POSTGAP:0",
    );
    file.resize(hunk_bytes as usize, 0);
    file.extend(std::iter::repeat_n(0x11u8, hunk_bytes as usize));
    let mut data = vec![0u8; hunk_bytes as usize];
    for frame in 0..2 {
        let start = frame * 2448;
        data[start..start + 16].fill(0xee); // sync and header stand-ins
        data[start + 16..start + 16 + 2048].fill(0x40 + frame as u8);
    }
    file.extend_from_slice(&data);
    file.push(0); // the map offset check wants the file to run past it

    let scratch = Scratch::new("v5raw");
    let path = scratch.write("v5raw.chd", &file);
    let mut disc = Disc::new();
    assert!(disc.open(&path), "{}", disc.last_error());
    assert!(disc.chd().describe().contains("uncompressed"));
    assert_eq!(disc.sector_count(), 2);
    let tracks = disc.tracks();
    assert_eq!(tracks.len(), 2);
    assert_eq!(
        (
            tracks[0].is_audio,
            tracks[0].start_lba,
            tracks[0].length_sectors
        ),
        (true, 0, 3)
    );
    assert_eq!(
        (
            tracks[1].is_audio,
            tracks[1].start_lba,
            tracks[1].length_sectors
        ),
        (false, 3, 2)
    );

    let mut sector = [0u8; 2048];
    for lba in 0..2 {
        assert!(disc.read_sector(lba, &mut sector));
        assert!(
            sector.iter().all(|b| *b == 0x40 + lba as u8),
            "sector {lba}"
        );
    }

    let mut chd = ChdFile::open(File::open(&path).unwrap()).unwrap();
    assert_eq!(chd.hunk_kind(1), Some(HunkKind::Zero));
    let mut hunk = vec![0xffu8; 4896];
    chd.read_hunk(1, &mut hunk).unwrap();
    assert!(hunk.iter().all(|b| *b == 0));
}

// ---------------------------------------------------------------------------
// External fixtures: CHDs made by chdman with every codec. Skipped unless the
// folder exists. It is built by scripts in the agents' workspace: bin/cue
// images (mixed, audiofirst, mode2, cooked), and for each, <name>-<codec>.chd
// for default, cdzl, cdlz, cdfl, cdzs, zlib, lzma, zstd, huff, flac and none.
// ---------------------------------------------------------------------------

fn fixtures() -> Option<PathBuf> {
    let dir = std::env::var_os("RETRO3DO_CHD_FIXTURES")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| Path::new(&h).join(".cache/r3do-agents/disc/fixtures"))
        })?;
    dir.join("mixed.cue").exists().then_some(dir)
}

/// The CHD frame stream chdman makes from a bin/cue: each track's sectors as
/// 2352-byte frames (audio byte-swapped to big-endian, cooked sectors
/// zero-padded) with 96 bytes of empty subcode, each track padded to a
/// multiple of four frames.
fn expected_frames(dir: &Path, name: &str) -> (Vec<u8>, Vec<(bool, u32)>) {
    let cue = fs::read_to_string(dir.join(format!("{name}.cue"))).unwrap();
    let bin = fs::read(dir.join(format!("{name}.bin"))).unwrap();
    let mut tracks: Vec<(String, u32)> = Vec::new();
    let mut kind = String::new();
    for line in cue.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("TRACK ") {
            kind = rest.split_whitespace().nth(1).unwrap().to_string();
        } else if let Some(rest) = line.strip_prefix("INDEX 01 ") {
            let p: Vec<u32> = rest.split(':').map(|f| f.parse().unwrap()).collect();
            tracks.push((kind.clone(), (p[0] * 60 + p[1]) * 75 + p[2]));
        }
    }
    let stride = |kind: &str| if kind == "MODE1/2048" { 2048 } else { 2352 };
    let mut frames = Vec::new();
    let mut layout = Vec::new();
    let mut offset = 0usize;
    for (index, (kind, _)) in tracks.iter().enumerate() {
        let count = match tracks.get(index + 1) {
            Some((_, next)) => next - tracks[index].1,
            None => ((bin.len() - offset) / stride(kind)) as u32,
        };
        layout.push((kind == "AUDIO", count));
        for _ in 0..count {
            let mut frame = vec![0u8; 2448];
            let s = stride(kind);
            frame[..s].copy_from_slice(&bin[offset..offset + s]);
            if kind == "AUDIO" {
                for pair in frame[..2352].as_chunks_mut::<2>().0 {
                    pair.swap(0, 1);
                }
            }
            frames.extend_from_slice(&frame);
            offset += s;
        }
        let padded = count.div_ceil(4) * 4;
        frames.resize(frames.len() + (padded - count) as usize * 2448, 0);
    }
    (frames, layout)
}

#[test]
fn every_codec_reproduces_the_image_it_was_made_from() {
    let Some(dir) = fixtures() else {
        eprintln!("skipped: no CHD fixtures folder");
        return;
    };
    let mut checked = 0;
    for name in ["mixed", "audiofirst", "mode2", "cooked"] {
        let (frames, layout) = expected_frames(&dir, name);
        for codec in [
            "default", "cdzl", "cdlz", "cdfl", "cdzs", "zlib", "lzma", "zstd", "huff", "flac",
            "none", "hs2", "mix4",
        ] {
            let path = dir.join(format!("{name}-{codec}.chd"));
            if !path.exists() {
                continue;
            }
            let mut chd = ChdFile::open(File::open(&path).unwrap()).unwrap();
            let hunk_bytes = chd.header().hunk_bytes as usize;
            let mut hunk = vec![0u8; hunk_bytes];
            for index in 0..chd.hunk_count() {
                chd.read_hunk(index, &mut hunk)
                    .unwrap_or_else(|e| panic!("{path:?} hunk {index}: {e}"));
                let start = index as usize * hunk_bytes;
                let end = (start + hunk_bytes).min(frames.len());
                assert!(
                    hunk[..end - start] == frames[start..end],
                    "{path:?} hunk {index} differs"
                );
            }

            // And through the disc layer: the data track's user data.
            let mut disc = Disc::new();
            assert!(
                disc.open(path.to_str().unwrap()),
                "{path:?}: {}",
                disc.last_error()
            );
            let tracks: Vec<(bool, u32)> = disc
                .tracks()
                .iter()
                .map(|t| (t.is_audio, t.length_sectors))
                .collect();
            assert_eq!(tracks, layout, "{path:?}");
            let data_index = layout.iter().position(|(audio, _)| !audio).unwrap();
            let first_frame: u32 = layout[..data_index]
                .iter()
                .map(|(_, n)| n.div_ceil(4) * 4)
                .sum();
            let payload = match name {
                "mode2" => 24,
                "cooked" => 0,
                _ => 16,
            };
            let mut sector = [0u8; 2048];
            for lba in 0..disc.sector_count() {
                assert!(disc.read_sector(lba, &mut sector), "{path:?} lba {lba}");
                let at = (first_frame + lba) as usize * 2448 + payload;
                assert!(
                    sector[..] == frames[at..at + 2048],
                    "{path:?} lba {lba} differs"
                );
            }
            checked += 1;
        }
    }
    assert!(checked >= 40, "only {checked} fixture CHDs were found");
}

#[test]
fn chds_chdman_left_unfinished_are_refused() {
    // Files from interrupted chdman runs: a header with no map behind it.
    let Some(dir) = fixtures() else {
        eprintln!("skipped: no CHD fixtures folder");
        return;
    };
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.starts_with("broken-") {
            continue;
        }
        let mut disc = Disc::new();
        assert!(!disc.open(path.to_str().unwrap()), "{name}");
        assert!(disc.is_chd());
        assert_eq!(
            disc.last_error(),
            format!("{}: invalid file", path.to_str().unwrap())
        );
    }
}
