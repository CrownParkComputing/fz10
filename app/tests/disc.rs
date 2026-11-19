#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::panic
)]
//! Disc image tests, ported from the C++ suite (tests/test_disc.cpp).
//!
//! These build synthetic images on disk and read them back. The sector-stride
//! cases matter most: reading a raw image as though it were cooked *almost*
//! works — the first sector is right and everything after it is off by 304
//! bytes — which presents as a corrupt disc rather than a misread one.

use std::fs::{self, File};
use std::path::PathBuf;

use retro3do::disc::{bytes_per_sector, user_offset_in_sector, Disc, SectorLayout};

/// A private folder in the temp directory, removed when dropped. File names
/// inside it are the C++ suite's, so cue sheets can name their images as the
/// originals do.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(test: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("retro3do-disc-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Scratch { dir }
    }

    /// The path of `retro3do-test-<name>` inside the folder.
    fn path(&self, name: &str) -> String {
        self.dir
            .join(format!("retro3do-test-{name}"))
            .to_string_lossy()
            .into_owned()
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

const SYNC: [u8; 12] = [
    0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00,
];

/// A cooked image: user data only, one byte pattern per sector.
fn make_cooked(sectors: u32) -> Vec<u8> {
    (0..sectors)
        .flat_map(|s| vec![(s + 1) as u8; 2048])
        .collect()
}

/// A raw image with the given mode byte, same payload pattern.
fn make_raw(sectors: u32, mode: u8) -> Vec<u8> {
    let user_offset = if mode == 2 { 24 } else { 16 };
    let mut image = vec![0u8; sectors as usize * 2352];
    for s in 0..sectors as usize {
        let sector = &mut image[s * 2352..(s + 1) * 2352];
        sector[..12].copy_from_slice(&SYNC);
        sector[15] = mode;
        sector[user_offset..user_offset + 2048].fill((s + 1) as u8);
    }
    image
}

// ---------------------------------------------------------------------------
// Layout arithmetic
// ---------------------------------------------------------------------------

#[test]
fn each_layout_knows_its_stride_and_payload_offset() {
    assert_eq!(bytes_per_sector(SectorLayout::Cooked2048), 2048);
    assert_eq!(bytes_per_sector(SectorLayout::Raw2352Mode1), 2352);
    assert_eq!(bytes_per_sector(SectorLayout::Raw2352Mode2), 2352);

    assert_eq!(user_offset_in_sector(SectorLayout::Cooked2048), 0);
    assert_eq!(user_offset_in_sector(SectorLayout::Raw2352Mode1), 16);
    // Mode 2 has an eight-byte subheader after the header. Missing it is what
    // makes a mode 2 disc read as almost-right garbage.
    assert_eq!(user_offset_in_sector(SectorLayout::Raw2352Mode2), 24);
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

#[test]
fn a_cooked_image_is_detected_and_read() {
    let scratch = Scratch::new("cooked");
    let path = scratch.write("cooked.iso", &make_cooked(4));

    let mut disc = Disc::new();
    assert!(disc.open(&path));
    assert_eq!(disc.layout(), SectorLayout::Cooked2048);
    assert_eq!(disc.sector_count(), 4);

    let mut sector = [0u8; 2048];
    assert!(disc.read_sector(2, &mut sector));
    assert_eq!(sector[0], 3);
    assert_eq!(sector[2047], 3);
}

#[test]
fn a_raw_mode1_image_is_detected_by_its_sync_pattern_not_its_name() {
    // Deliberately named .iso while holding raw sectors. Trusting the
    // extension here is exactly the bug this detection exists to prevent.
    let scratch = Scratch::new("raw-mode1");
    let path = scratch.write("raw-mode1.iso", &make_raw(4, 1));

    let mut disc = Disc::new();
    assert!(disc.open(&path));
    assert_eq!(disc.layout(), SectorLayout::Raw2352Mode1);
    assert_eq!(disc.sector_count(), 4);

    let mut sector = [0u8; 2048];
    assert!(disc.read_sector(3, &mut sector));
    assert_eq!(sector[0], 4);
}

#[test]
fn a_raw_mode2_image_skips_the_subheader() {
    let scratch = Scratch::new("raw-mode2");
    let path = scratch.write("raw-mode2.bin", &make_raw(4, 2));

    let mut disc = Disc::new();
    assert!(disc.open(&path));
    assert_eq!(disc.layout(), SectorLayout::Raw2352Mode2);

    let mut sector = [0u8; 2048];
    assert!(disc.read_sector(1, &mut sector));
    assert_eq!(sector[0], 2);
    assert_eq!(sector[2047], 2);
}

#[test]
fn every_sector_of_a_raw_image_reads_correctly_not_just_the_first() {
    // The off-by-304 failure only shows from the second sector on, so the
    // whole image is walked rather than spot-checked.
    let scratch = Scratch::new("raw-walk");
    let path = scratch.write("raw-walk.bin", &make_raw(16, 1));

    let mut disc = Disc::new();
    assert!(disc.open(&path));

    let mut sector = [0u8; 2048];
    for s in 0..16u32 {
        assert!(disc.read_sector(s, &mut sector));
        assert_eq!(sector[0], (s + 1) as u8);
        assert_eq!(sector[1024], (s + 1) as u8);
    }
}

#[test]
fn reading_past_the_end_fails_rather_than_returning_rubbish() {
    let scratch = Scratch::new("short");
    let path = scratch.write("short.iso", &make_cooked(2));

    let mut disc = Disc::new();
    assert!(disc.open(&path));
    let mut sector = [0u8; 2048];
    assert!(disc.read_sector(1, &mut sector));
    assert!(!disc.read_sector(2, &mut sector));
    assert!(!disc.read_sector(1000, &mut sector));
}

#[test]
fn a_missing_file_reports_why() {
    let mut disc = Disc::new();
    assert!(!disc.open("/definitely/not/here.iso"));
    assert!(!disc.last_error().is_empty());
    assert!(!disc.is_open());
}

#[test]
fn a_bare_image_presents_as_one_data_track() {
    let scratch = Scratch::new("single-track");
    let path = scratch.write("single-track.iso", &make_cooked(8));

    let mut disc = Disc::new();
    assert!(disc.open(&path));
    assert_eq!(disc.tracks().len(), 1);
    assert_eq!(disc.tracks()[0].number, 1);
    assert!(!disc.tracks()[0].is_audio);
    assert_eq!(disc.tracks()[0].length_sectors, 8);
}

// ---------------------------------------------------------------------------
// Cue sheets
// ---------------------------------------------------------------------------

#[test]
fn a_cue_sheet_gives_track_numbers_types_and_starts() {
    let scratch = Scratch::new("multi");
    scratch.write("multi.bin", &make_raw(1000, 1));
    let cue = scratch.write(
        "multi.cue",
        b"FILE \"retro3do-test-multi.bin\" BINARY\n\
          \x20 TRACK 01 MODE1/2352\n\
          \x20   INDEX 01 00:00:00\n\
          \x20 TRACK 02 AUDIO\n\
          \x20   INDEX 01 00:04:00\n\
          \x20 TRACK 03 AUDIO\n\
          \x20   INDEX 01 00:08:00\n",
    );

    let mut disc = Disc::new();
    assert!(disc.open(&cue));
    assert_eq!(disc.tracks().len(), 3);

    assert_eq!(disc.tracks()[0].number, 1);
    assert!(!disc.tracks()[0].is_audio);
    assert_eq!(disc.tracks()[0].start_lba, 0);

    // 4 seconds at 75 frames a second.
    assert_eq!(disc.tracks()[1].start_lba, 300);
    assert!(disc.tracks()[1].is_audio);

    assert_eq!(disc.tracks()[2].start_lba, 600);
}

#[test]
fn track_lengths_come_from_the_next_tracks_start() {
    let scratch = Scratch::new("len");
    scratch.write("len.bin", &make_raw(1000, 1));
    let cue = scratch.write(
        "len.cue",
        b"FILE \"retro3do-test-len.bin\" BINARY\n\
          \x20 TRACK 01 MODE1/2352\n\
          \x20   INDEX 01 00:00:00\n\
          \x20 TRACK 02 AUDIO\n\
          \x20   INDEX 01 00:04:00\n",
    );

    let mut disc = Disc::new();
    assert!(disc.open(&cue));
    assert_eq!(disc.tracks()[0].length_sectors, 300);
    // The last track runs to the end of the image.
    assert_eq!(disc.tracks()[1].length_sectors, 700);
}

#[test]
fn a_cue_naming_a_wave_file_is_refused_rather_than_playing_silence() {
    let scratch = Scratch::new("wav");
    scratch.write("wav.bin", &make_raw(10, 1));
    let cue = scratch.write(
        "wav.cue",
        b"FILE \"retro3do-test-wav.bin\" BINARY\n\
          \x20 TRACK 01 MODE1/2352\n\
          \x20   INDEX 01 00:00:00\n\
          FILE \"music.wav\" WAVE\n\
          \x20 TRACK 02 AUDIO\n\
          \x20   INDEX 01 00:00:00\n",
    );

    let mut disc = Disc::new();
    assert!(!disc.open(&cue));
    assert!(!disc.last_error().is_empty());
}

#[test]
fn a_cue_with_no_file_line_reports_why() {
    let scratch = Scratch::new("empty");
    let cue = scratch.write("empty.cue", b"TRACK 01 MODE1/2352\n  INDEX 01 00:00:00\n");
    let mut disc = Disc::new();
    assert!(!disc.open(&cue));
    assert!(!disc.last_error().is_empty());
}

#[test]
fn data_read_through_a_cue_still_lands_on_the_right_sector() {
    let scratch = Scratch::new("cuedata");
    scratch.write("cuedata.bin", &make_raw(64, 1));
    let cue = scratch.write(
        "cuedata.cue",
        b"FILE \"retro3do-test-cuedata.bin\" BINARY\n\
          \x20 TRACK 01 MODE1/2352\n\
          \x20   INDEX 01 00:00:00\n",
    );

    let mut disc = Disc::new();
    assert!(disc.open(&cue));
    let mut sector = [0u8; 2048];
    assert!(disc.read_sector(10, &mut sector));
    assert_eq!(sector[0], 11);
}

// ---------------------------------------------------------------------------
// Raw access
// ---------------------------------------------------------------------------

#[test]
fn raw_reads_hand_back_the_whole_sector() {
    let scratch = Scratch::new("rawread");
    let path = scratch.write("rawread.bin", &make_raw(4, 1));

    let mut disc = Disc::new();
    assert!(disc.open(&path));

    let mut sector = [0u8; 2352];
    let size = disc.read_raw_sector(1, &mut sector);
    assert!(size.is_some());
    assert_eq!(size, Some(2352));
    // The sync pattern is part of what comes back, unlike a cooked read.
    assert_eq!(sector[0], 0x00);
    assert_eq!(sector[1], 0xff);
}

// ---------------------------------------------------------------------------
// Opening an already-open file
// ---------------------------------------------------------------------------
//
// Android's scoped storage hands out content URIs, not paths: there is no
// filename to open, only a descriptor the system opened for us. The C ABI
// turns that descriptor into a `File` and hands it to `Disc::open_file`.
// These cover the same ground as the path tests, because the two routes
// must agree.

fn open_read(path: &str) -> File {
    File::open(path).unwrap()
}

#[test]
fn a_descriptor_reads_the_same_data_as_a_path() {
    let scratch = Scratch::new("fd-cooked");
    let path = scratch.write("fd-cooked.iso", &make_cooked(4));

    let mut by_path = Disc::new();
    assert!(by_path.open(&path));
    let mut expected = [0u8; 2048];
    assert!(by_path.read_sector(2, &mut expected));

    let mut by_fd = Disc::new();
    assert!(by_fd.open_file(open_read(&path), "fd-cooked.iso"));
    assert_eq!(by_fd.sector_count(), by_path.sector_count());

    let mut actual = [0u8; 2048];
    assert!(by_fd.read_sector(2, &mut actual));
    assert_eq!(actual[0], expected[0]);
    assert_eq!(actual[2047], expected[2047]);
}

#[test]
fn layout_detection_works_on_a_descriptor_too() {
    // The descriptor carries no filename, so detection cannot fall back on
    // the extension even in principle — which is the behaviour wanted anyway.
    let scratch = Scratch::new("fd-raw");
    let path = scratch.write("fd-raw.bin", &make_raw(8, 2));

    let mut disc = Disc::new();
    assert!(disc.open_file(open_read(&path), "fd-raw.bin"));
    assert_eq!(disc.layout(), SectorLayout::Raw2352Mode2);

    let mut sector = [0u8; 2048];
    assert!(disc.read_sector(5, &mut sector));
    assert_eq!(sector[0], 6);
}

#[test]
fn a_bad_descriptor_is_reported_not_crashed_on() {
    // A `File` cannot hold -1, so the C++ invalid descriptor becomes the
    // nearest thing a `File` can be: a handle that cannot be read from.
    let scratch = Scratch::new("fd-bad");
    let unreadable = File::create(scratch.path("nothing.iso")).unwrap();

    let mut disc = Disc::new();
    assert!(!disc.open_file(unreadable, "nothing.iso"));
    assert!(!disc.last_error().is_empty());
    assert!(!disc.is_open());
}

#[test]
fn a_cue_by_descriptor_is_refused_with_a_useful_message() {
    // A cue names a sibling image, and a descriptor gives no way to reach it.
    // Refusing clearly beats opening the cue as though it were an image and
    // then reporting a corrupt disc.
    let scratch = Scratch::new("fd-refuse");
    let path = scratch.write(
        "fd-refuse.cue",
        b"FILE \"x.bin\" BINARY\n  TRACK 01 MODE1/2352\n",
    );

    let mut disc = Disc::new();
    assert!(!disc.open_file(open_read(&path), "fd-refuse.cue"));
    assert!(disc.last_error().contains("cue"));
}

#[test]
fn closing_the_disc_releases_the_descriptor() {
    // The disc owns the file, so it must not be closed twice and must not
    // leak. Opening many in a row would exhaust the process limit if it
    // leaked.
    let scratch = Scratch::new("fd-leak");
    let path = scratch.write("fd-leak.iso", &make_cooked(2));

    for _ in 0..512 {
        let mut disc = Disc::new();
        assert!(disc.open_file(open_read(&path), "fd-leak.iso"));
        disc.close();
    }
    // Getting here without running out of descriptors is the assertion.
    let mut final_check = Disc::new();
    assert!(final_check.open_file(open_read(&path), "fd-leak.iso"));
}

// ---------------------------------------------------------------------------
// Beyond the C++ suite: behaviour the C++ has and the tests above leave
// implicit.
// ---------------------------------------------------------------------------

#[test]
fn the_errors_name_the_file_in_the_cpp_wording() {
    let scratch = Scratch::new("wording");
    let mut disc = Disc::new();

    assert!(!disc.open("/definitely/not/here.iso"));
    assert_eq!(
        disc.last_error(),
        "Could not open disc image: /definitely/not/here.iso"
    );

    let empty = scratch.write("empty.iso", &[]);
    assert!(!disc.open(&empty));
    assert_eq!(
        disc.last_error(),
        format!("Disc image is too small to contain a single sector: {empty}")
    );
    assert!(!disc.open_file(open_read(&empty), "empty.iso"));
    assert_eq!(
        disc.last_error(),
        "Disc image is too small to contain a sector: empty.iso"
    );

    let cue = scratch.write("nofile.cue", b"TRACK 01 MODE1/2352\n");
    assert!(!disc.open(&cue));
    assert_eq!(
        disc.last_error(),
        format!("Cue sheet names no image file: {cue}")
    );

    assert!(!disc.open("/definitely/not/here.cue"));
    assert_eq!(
        disc.last_error(),
        "Could not open cue sheet: /definitely/not/here.cue"
    );

    // A successful open clears the last error.
    let good = scratch.write("good.iso", &make_cooked(1));
    assert!(disc.open(&good));
    assert!(disc.last_error().is_empty());
}

#[test]
fn a_cue_opens_the_image_beside_it_and_reports_that_path() {
    let scratch = Scratch::new("cuepath");
    let bin = scratch.write("cuepath.bin", &make_raw(20, 2));
    let cue = scratch.write(
        "cuepath.cue",
        b"FILE \"retro3do-test-cuepath.bin\" BINARY\r\n  TRACK 01 MODE2/2352\r\n    INDEX 00 00:00:00\r\n    INDEX 01 00:00:05\r\n",
    );
    let mut disc = Disc::new();
    assert!(disc.open(&cue));
    assert_eq!(disc.path(), bin);
    assert_eq!(disc.layout(), SectorLayout::Raw2352Mode2);
    // INDEX 01 wins over the pregap's INDEX 00.
    assert_eq!(disc.tracks()[0].start_lba, 5);
    assert_eq!(disc.tracks()[0].length_sectors, 15);
    disc.close();
    assert!(!disc.is_open());
    assert!(disc.path().is_empty());
    assert!(disc.tracks().is_empty());
}
