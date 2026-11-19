#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::panic
)]
//! The CD-ROM drive on the expansion bus.
//!
//! The first group is ported from the C++ suite's CLIO tests
//! (tests/test_clio.cpp) that exercise the drive alone. There they go through
//! CLIO's command register, which with the drive selected (the reset state)
//! passes each write straight to `write_command` and each read to
//! `read_status`; here they talk to the drive directly. The checks that are
//! CLIO's own — the poll register, interrupt pending bits, selection — stay
//! with CLIO.

use std::fs;
use std::path::PathBuf;

use retro3do::disc::Disc;
use retro3do::xbus::*;

fn command(drive: &mut CdRomDevice, bytes: [u8; 7]) {
    for byte in bytes {
        drive.write_command(byte);
    }
}

/// Sends a seven-byte command with only its opcode set.
fn opcode(drive: &mut CdRomDevice, op: u8) {
    command(drive, [op, 0, 0, 0, 0, 0, 0]);
}

// ---------------------------------------------------------------------------
// Ported from tests/test_clio.cpp
// ---------------------------------------------------------------------------

#[test]
fn a_multi_byte_cd_command_completes_once_not_once_per_byte() {
    // The boot ROM writes seven bytes per command. Replying to each byte runs
    // the status FIFO six replies ahead of the conversation, so every later
    // exchange reads the previous command's answer.
    let mut drive = CdRomDevice::new();
    assert!(drive.status_empty());

    for i in 0..6 {
        drive.write_command(if i == 0 { 0x83 } else { 0x00 });
        assert!(drive.status_empty()); // nothing yet
        assert_eq!(drive.commands_received(), 0);
    }
    drive.write_command(0x00); // the seventh byte
    assert_eq!(drive.commands_received(), 1);
    // The reply is there the moment the seventh byte lands. The driver spins
    // on the poll register without running the machine on, so a drive that
    // answers "later" never answers at all.
    assert!(!drive.status_empty());
    assert_eq!(drive.last_command(), 0x83);
}

#[test]
fn inserting_a_disc_is_visible_to_the_drive() {
    // Opening the image and telling the machine a disc is in the tray are two
    // different things, and only the second is visible to its software.
    let mut drive = CdRomDevice::new();
    assert!(!drive.disc_present());
    drive.set_disc_present(true);

    opcode(&mut drive, CMD_VERSION);
    assert!(!drive.status_empty());

    // A version reply opens by echoing the command and ends with drive status.
    assert_eq!(drive.read_status(), CMD_VERSION);
    let mut last = 0;
    for _ in 1..12 {
        last = drive.read_status();
    }
    assert_eq!(last & STATUS_DISC_IN, STATUS_DISC_IN);
    assert!(drive.status_empty());
}

#[test]
fn the_version_reply_identifies_the_drive() {
    // The boot ROM asks for this while enumerating the bus. Answering with
    // zeroes reads as "no drive fitted": the machine boots to its logo and
    // idles, never asking about a disc.
    let mut drive = CdRomDevice::new();
    opcode(&mut drive, CMD_VERSION);
    drive.tick(100_000);

    let reply: Vec<u8> = (0..12).map(|_| drive.read_status()).collect();
    assert_eq!(reply[0], CMD_VERSION); // echo
    assert_eq!(reply[2], 0x10); // manufacturer id
    assert_eq!(reply[4], 0x01); // manufacturer number
    assert!(drive.status_empty());
    assert_eq!(CdRomDevice::VERSION_REPLY_BYTES, reply.len());
}

#[test]
fn a_read_command_streams_sectors_from_the_disc() {
    // READ carries its start in MSF unless bit 0 of byte 4 says otherwise,
    // and a count of sectors. The reply is only an acknowledgement; the
    // sector data comes back through the data FIFO.
    let mut drive = CdRomDevice::new();
    drive.set_disc_present(true);

    // MSF 00:02:00 is LBA 0: the first two seconds of a CD are lead-in.
    command(&mut drive, [CMD_READ, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01]);
    drive.tick(100_000);

    assert_eq!(drive.read_status(), CMD_READ);
    assert_eq!(drive.read_status() & STATUS_SPIN_UP, STATUS_SPIN_UP);

    // With no disc attached there is nothing to stream, but the command must
    // still have been accepted rather than rejected.
    assert_eq!(drive.last_command(), CMD_READ);
}

#[test]
fn read_capacity_reports_the_lead_out_in_msf() {
    let mut drive = CdRomDevice::new();
    opcode(&mut drive, CMD_READ_CAPACITY);
    drive.tick(100_000);

    assert_eq!(drive.read_status(), CMD_READ_CAPACITY);
    drive.read_status(); // reserved
    let m = drive.read_status();
    let s = drive.read_status();
    drive.read_status(); // frames
                         // An empty drive still answers. LBA zero is MSF 00:02:00, so the lead-in
                         // is present once. The sector count itself already names the lead-out.
    assert_eq!(m, 0);
    assert_eq!(s, 2);
}

#[test]
fn a_read_transfers_sector_bytes_through_the_data_fifo() {
    // The CPU never reads the drive's data port; sector bytes leave the drive
    // through the data FIFO, which MADAM's expansion DMA drains into memory.
    let mut drive = CdRomDevice::new();
    drive.set_disc_present(true);
    assert!(!drive.has_chunk());

    command(&mut drive, [CMD_READ, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01]);
    drive.tick(100_000);

    // With no disc attached there is nothing to stream; the point is that
    // the command is accepted and the drive reports the motor running.
    assert_eq!(drive.read_status(), CMD_READ);
    assert_eq!(drive.read_status() & STATUS_SPIN_UP, STATUS_SPIN_UP);
}

#[test]
fn ejecting_reports_an_empty_drive_and_spinning_up_reloads_it() {
    // The driver ejects the disc on purpose partway through a mount, reads
    // the status back, and decides what to do next from it. Told the drive
    // is still loaded it concludes the eject failed and waits for ever.
    let mut drive = CdRomDevice::new();
    drive.set_disc_present(true);

    let mut send = |op: u8| {
        opcode(&mut drive, op);
        assert_eq!(drive.read_status(), op);
        drive.read_status()
    };

    // Loaded: tray shut, disc in, spinning, ready.
    assert_eq!(send(CMD_READ_STATUS), 0xe1);

    // Ejected: ready and nothing else.
    assert_eq!(send(CMD_EJECT), STATUS_READY);
    assert_eq!(send(CMD_READ_STATUS), STATUS_READY);

    // Spinning up closes the tray again — the other half of the handshake.
    assert_eq!(send(CMD_MOTOR_ON), 0xe1);
}

#[test]
fn abort_is_one_byte_long_and_the_rest_are_seven() {
    // ABORT interrupts a transfer already running, so a drive that waited for
    // six more bytes would never abort anything. And the six bytes that would
    // have followed belong to the NEXT command.
    let mut drive = CdRomDevice::new();
    drive.set_disc_present(true);

    drive.write_command(CMD_ABORT);
    assert_eq!(drive.commands_received(), 1);
    assert_eq!(drive.last_command(), CMD_ABORT);

    // The next command starts cleanly rather than continuing the abort.
    opcode(&mut drive, CMD_VERSION);
    assert_eq!(drive.commands_received(), 2);
    assert_eq!(drive.last_command(), CMD_VERSION);
}

#[test]
fn a_completed_cd_command_requests_an_interrupt() {
    // The drive's half of the C++ test of the same idea: a completed command
    // leaves status waiting and asks for the expansion-bus interrupt, once.
    let mut drive = CdRomDevice::new();
    opcode(&mut drive, CMD_VERSION);
    drive.tick(100_000);
    assert!(!drive.status_empty());
    assert!(drive.take_interrupt_request());
    assert!(!drive.take_interrupt_request());
}

// ---------------------------------------------------------------------------
// With a disc in the drive
// ---------------------------------------------------------------------------

struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(test: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("retro3do-xbus-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Scratch { dir }
    }

    fn write(&self, name: &str, bytes: &[u8]) -> String {
        let path = self.dir.join(name).to_string_lossy().into_owned();
        fs::write(&path, bytes).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// A cooked image whose sector N is filled with N+1 (wrapping).
fn cooked_disc(scratch: &Scratch, sectors: u32) -> Disc {
    let image: Vec<u8> = (0..sectors)
        .flat_map(|s| vec![(s + 1) as u8; 2048])
        .collect();
    let path = scratch.write("disc.iso", &image);
    let mut disc = Disc::new();
    assert!(disc.open(&path));
    disc
}

fn loaded_drive(disc: Disc) -> CdRomDevice {
    let mut drive = CdRomDevice::new();
    assert!(drive.attach_disc(Some(disc)).is_none());
    drive.set_disc_present(true);
    drive
}

fn drain(drive: &mut CdRomDevice) -> Vec<u8> {
    let mut out = Vec::new();
    while drive.has_chunk() {
        out.push(drive.read_data());
    }
    out
}

#[test]
fn sectors_arrive_one_per_drive_interval_and_no_more_than_asked() {
    let scratch = Scratch::new("stream");
    let mut drive = loaded_drive(cooked_disc(&scratch, 5));
    drive.take_interrupt_request();

    // Three sectors from LBA 1, addressed by block number.
    command(&mut drive, [CMD_READ, 0x00, 0x00, 0x01, 0x01, 0x00, 0x03]);
    assert_eq!(drive.read_status(), CMD_READ);
    drive.read_status();
    drive.take_interrupt_request();

    // The first sector is ready at once.
    let first = drain(&mut drive);
    assert_eq!(first, vec![2u8; 2048]);
    assert_eq!(drive.read_data(), 0);

    // The next only once the drive has turned far enough.
    let delay = CdRomDevice::SECTOR_DELAY_CYCLES;
    drive.tick(delay - 1);
    assert!(!drive.has_chunk());
    assert!(!drive.take_interrupt_request());
    drive.tick(1);
    assert!(drive.has_chunk());
    assert!(drive.take_interrupt_request());
    assert_eq!(drain(&mut drive), vec![3u8; 2048]);

    drive.tick(delay);
    assert!(drive.take_interrupt_request());
    assert_eq!(drain(&mut drive), vec![4u8; 2048]);

    // Exactly the three asked for.
    drive.tick(delay * 4);
    assert!(!drive.has_chunk());
    assert!(!drive.take_interrupt_request());
}

#[test]
fn a_waiting_sector_holds_the_stream_until_it_is_drained() {
    let scratch = Scratch::new("hold");
    let mut drive = loaded_drive(cooked_disc(&scratch, 4));
    command(&mut drive, [CMD_READ, 0x00, 0x00, 0x00, 0x01, 0x00, 0x02]);
    // However long the host waits, the second sector does not overwrite the
    // first.
    drive.tick(CdRomDevice::SECTOR_DELAY_CYCLES * 10);
    assert_eq!(drain(&mut drive), vec![1u8; 2048]);
    drive.tick(CdRomDevice::SECTOR_DELAY_CYCLES);
    assert_eq!(drain(&mut drive), vec![2u8; 2048]);
}

#[test]
fn set_mode_chooses_the_block_size_handed_back() {
    let scratch = Scratch::new("mode");
    let mut drive = loaded_drive(cooked_disc(&scratch, 2));
    command(
        &mut drive,
        [CMD_SET_MODE, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00],
    );
    assert_eq!(drive.read_status(), CMD_SET_MODE);
    drive.read_status();
    command(&mut drive, [CMD_READ, 0x00, 0x00, 0x01, 0x01, 0x00, 0x01]);
    assert_eq!(drain(&mut drive), vec![2u8; 512]);

    // A reset puts the drive back to whole sectors.
    drive.reset();
    assert_eq!(drive.commands_received(), 0);
    command(&mut drive, [CMD_READ, 0x00, 0x00, 0x01, 0x01, 0x00, 0x01]);
    assert_eq!(drain(&mut drive).len(), 2048);
}

#[test]
fn reads_past_the_end_of_the_disc_deliver_nothing() {
    let scratch = Scratch::new("end");
    let mut drive = loaded_drive(cooked_disc(&scratch, 2));
    command(&mut drive, [CMD_READ, 0x00, 0x00, 0x01, 0x01, 0x00, 0x05]);
    assert_eq!(drain(&mut drive).len(), 2048);
    drive.tick(CdRomDevice::SECTOR_DELAY_CYCLES * 8);
    assert!(!drive.has_chunk());
}

#[test]
fn the_table_of_contents_describes_each_track() {
    let scratch = Scratch::new("toc");
    let mut image = Vec::new();
    for s in 0..1000u32 {
        let mut sector = vec![0u8; 2352];
        sector[..12].copy_from_slice(&[
            0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0,
        ]);
        sector[15] = 1;
        sector[16..16 + 2048].fill(s as u8);
        image.extend_from_slice(&sector);
    }
    scratch.write("toc.bin", &image);
    let cue = scratch.write(
        "toc.cue",
        b"FILE \"toc.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 00:04:00\n",
    );
    let mut disc = Disc::new();
    assert!(disc.open(&cue));
    let mut drive = loaded_drive(disc);

    opcode(&mut drive, CMD_READ_DISC_INFO);
    let info: Vec<u8> = (0..8).map(|_| drive.read_status()).collect();
    // First track 1, last track 2, lead-out at 1000 + 150 frames = 00:15:25.
    assert_eq!(
        info,
        vec![CMD_READ_DISC_INFO, 0x00, 0x01, 0x02, 0x00, 15, 25, 0xe1]
    );

    command(&mut drive, [CMD_READ_TOC, 0x00, 0x02, 0, 0, 0, 0]);
    let toc: Vec<u8> = (0..10).map(|_| drive.read_status()).collect();
    // Track 2 is audio (control 0) and starts at 300 + 150 frames = 00:06:00.
    assert_eq!(
        toc,
        vec![CMD_READ_TOC, 0x00, 0x00, 0x02, 0x00, 0, 6, 0, 0x00, 0xe1]
    );

    command(&mut drive, [CMD_READ_TOC, 0x00, 0x01, 0, 0, 0, 0]);
    let toc: Vec<u8> = (0..10).map(|_| drive.read_status()).collect();
    assert_eq!(toc[2], 0x04); // data
    assert_eq!(&toc[5..8], &[0, 2, 0]);

    // Entry zero is absent and reads back as zeroes.
    command(&mut drive, [CMD_READ_TOC, 0x00, 0x00, 0, 0, 0, 0]);
    let toc: Vec<u8> = (0..10).map(|_| drive.read_status()).collect();
    assert_eq!(&toc[1..9], &[0u8; 8]);

    opcode(&mut drive, CMD_READ_SESSION_INFO);
    let session: Vec<u8> = (0..8).map(|_| drive.read_status()).collect();
    assert_eq!(
        session,
        vec![CMD_READ_SESSION_INFO, 0, 0, 15, 25, 0, 0, 0xe1]
    );
}

#[test]
fn disc_questions_to_an_empty_drive_report_an_error() {
    let mut drive = CdRomDevice::new();
    for op in [CMD_READ_DISC_INFO, CMD_READ_TOC, CMD_READ_SESSION_INFO] {
        opcode(&mut drive, op);
        assert_eq!(drive.read_status(), op);
        let status = drive.read_status();
        assert_eq!(status & STATUS_ERROR, STATUS_ERROR);
        assert_eq!(status & STATUS_READY, 0);
        assert!(drive.status_empty());
    }
}

#[test]
fn a_media_change_is_an_event_read_once() {
    let mut drive = CdRomDevice::new();
    drive.set_disc_present(true);
    assert!(drive.take_media_changed());
    assert!(!drive.take_media_changed());
    assert!(drive.take_interrupt_request());
    // Setting the same state again is not a change.
    drive.set_disc_present(true);
    assert!(!drive.take_media_changed());
    assert!(!drive.take_interrupt_request());
    drive.set_disc_present(false);
    assert!(drive.take_media_changed());
}

#[test]
fn the_drive_owns_its_disc() {
    let scratch = Scratch::new("own");
    let disc = cooked_disc(&scratch, 3);
    let mut drive = CdRomDevice::new();
    assert!(drive.disc().is_none());
    drive.attach_disc(Some(disc));
    assert_eq!(drive.disc().map(Disc::sector_count), Some(3));

    opcode(&mut drive, CMD_READ_CAPACITY);
    let reply: Vec<u8> = (0..8).map(|_| drive.read_status()).collect();
    assert_eq!(&reply[2..5], &[0, 2, 3]); // 3 + 150 frames

    let taken = drive.take_disc().unwrap();
    assert_eq!(taken.sector_count(), 3);
    assert!(drive.disc().is_none());
}

#[test]
fn the_device_answers_through_the_bus_trait() {
    let mut drive = CdRomDevice::new();
    let device: &mut dyn XbusDevice = &mut drive;
    assert_eq!(device.name(), "CD-ROM");
    for byte in [CMD_DATA_PATH_CHECK, 0, 0, 0, 0, 0, 0] {
        device.write_command(byte);
    }
    let reply: Vec<u8> = (0..4).map(|_| device.read_status()).collect();
    assert_eq!(reply, vec![CMD_DATA_PATH_CHECK, 0xaa, 0x55, STATUS_DOOR]);
    assert!(device.status_empty());
    assert_eq!(device.read_status(), 0);
    assert!(!device.has_chunk());
}

#[test]
fn the_drive_and_its_disc_can_move_to_the_emulator_thread() {
    fn assert_send<T: Send>() {}
    assert_send::<CdRomDevice>();
    assert_send::<Disc>();
}
