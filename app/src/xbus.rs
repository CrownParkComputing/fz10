#![forbid(unsafe_code)]
//! XBUS — the Opera expansion bus, and the CD-ROM drive on it.
//!
//! Implemented from The 3DO Company's own patent, WO 94/16382 "Expansion
//! Bus", which documents the protocol in full. Device command shapes use the
//! SDK CD definitions, the BSD-licensed CR-560B device documentation and
//! boot-ROM observations; see docs/PROVENANCE.md.
//!
//! The model, in the patent's own terms
//! ------------------------------------
//! The bus is FIFOs, not registers. Each device holds:
//!
//! * a Command FIFO the system writes command bytes into;
//! * a Status Return FIFO the device answers through — every command returns
//!   at least one status byte when it completes;
//! * a Data Return FIFO carrying bulk data back to the system in "chunks";
//! * a Data Write FIFO for bulk data to the device (writable devices only).
//!
//! Interrupts say that a return FIFO has something in it; the Status Return
//! interrupt is what tells the system a command has finished.
//!
//! The two "valid" bits in the poll register are ACTIVE LOW: `StatValid-` is
//! high when the Status Return FIFO is empty, not when it has something. That
//! is the opposite of the obvious reading and the easiest thing here to
//! implement backwards.

use std::collections::VecDeque;
use std::sync::OnceLock;

use crate::disc::{Disc, SECTOR_USER_BYTES};

// Poll register bits, from the patent's table.
/// Reads 0.
pub const POLL_RESERVED0: u32 = 1 << 0;
/// Interrupt Disable-: interrupts are disabled when LOW.
pub const POLL_INTERRUPT_OFF: u32 = 1 << 1;
/// The media may have been touched; writing 1 clears it.
pub const POLL_MEDIA_ACCESS: u32 = 1 << 2;
/// Reset-.
pub const POLL_RESET: u32 = 1 << 3;
/// StatValid-: HIGH when the status FIFO is empty.
pub const POLL_STATUS_EMPTY: u32 = 1 << 4;
/// ChunkValid-: HIGH when no complete chunk is waiting.
pub const POLL_NO_CHUNK: u32 = 1 << 5;

// The drive's command set. It is a Panasonic CR-560B on a pre-IDE MKE
// interface, which is what the FZ-1 and FZ-10 contain. Every command but
// reset and read-subchannel echoes its opcode in the first reply byte.
pub const CMD_SEEK: u8 = 0x01;
pub const CMD_MOTOR_ON: u8 = 0x02;
pub const CMD_MOTOR_OFF: u8 = 0x03;
pub const CMD_READ_STATUS: u8 = 0x05;
pub const CMD_EJECT: u8 = 0x06;
pub const CMD_INJECT: u8 = 0x07;
/// One byte long, not seven.
pub const CMD_ABORT: u8 = 0x08;
pub const CMD_SET_MODE: u8 = 0x09;
pub const CMD_PLAY_MSF: u8 = 0x0e;
pub const CMD_PLAY_TRACK: u8 = 0x0f;
pub const CMD_READ: u8 = 0x10;
pub const CMD_DATA_PATH_CHECK: u8 = 0x80;
pub const CMD_READ_ERROR: u8 = 0x82;
pub const CMD_VERSION: u8 = 0x83;
pub const CMD_READ_CAPACITY: u8 = 0x85;
pub const CMD_READ_DISC_INFO: u8 = 0x8b;
pub const CMD_READ_TOC: u8 = 0x8c;
pub const CMD_READ_SESSION_INFO: u8 = 0x8d;

// Drive status byte, from The 3DO Company's SDK header <cdrom.h>. The bus
// patent only defines ERROR; the other bits are the device's own.
pub const STATUS_READY: u8 = 0x01;
pub const STATUS_DOUBLE_SPEED: u8 = 0x02;
pub const STATUS_PLAYING: u8 = 0x04;
pub const STATUS_SUCCESS: u8 = 0x08;
pub const STATUS_ERROR: u8 = 0x10;
/// The motor is running.
pub const STATUS_SPIN_UP: u8 = 0x20;
/// Media present.
pub const STATUS_DISC_IN: u8 = 0x40;
/// Door closed.
pub const STATUS_DOOR: u8 = 0x80;

/// A CD counts time from two seconds in, so an MSF address is always 150
/// frames ahead of the block number it names.
pub const MSF_BIAS_FRAMES: u32 = 150;

/// The bus supports sixteen devices, addressed 0..15, each taking its address
/// at power-up by counting strobes.
pub const XBUS_MAX_DEVICES: u32 = 16;

/// One device on the bus: it accepts command bytes and answers with status
/// bytes when a command completes.
pub trait XbusDevice {
    /// A command byte has been written into the Command FIFO.
    fn write_command(&mut self, byte: u8);

    /// Takes the next byte from the Status Return FIFO, or 0 when it is
    /// empty.
    fn read_status(&mut self) -> u8;

    fn status_empty(&self) -> bool;

    fn read_data(&mut self) -> u8 {
        0
    }

    fn has_chunk(&self) -> bool {
        false
    }

    /// True once if the device has just queued something the host should be
    /// interrupted about.
    fn take_interrupt_request(&mut self) -> bool {
        false
    }

    /// Lets time pass. A drive does not answer instantly, and that is not
    /// cosmetic: the driver checks that the reply FIFO is EMPTY once it has
    /// read the bytes it expected.
    fn tick(&mut self, _cycles: u32) {}

    fn reset(&mut self);

    fn name(&self) -> &'static str;
}

/// A CD address is minutes:seconds:frames, 75 frames a second, and the first
/// two seconds are the lead-in.
fn msf_to_lba(msf: u32) -> u32 {
    let minutes = (msf >> 16) & 0xff;
    let seconds = (msf >> 8) & 0xff;
    let frames = msf & 0xff;
    let total = (minutes * 60 + seconds) * 75 + frames;
    total.saturating_sub(MSF_BIAS_FRAMES)
}

fn lba_to_msf(lba: u32) -> u32 {
    let frames = lba.wrapping_add(MSF_BIAS_FRAMES);
    ((frames / (60 * 75)) << 16) | (((frames / 75) % 60) << 8) | (frames % 75)
}

/// Whether `CDTRACE` asks for the command conversation on standard error.
fn trace() -> bool {
    static TRACE: OnceLock<bool> = OnceLock::new();
    *TRACE.get_or_init(|| std::env::var_os("CDTRACE").is_some())
}

/// The 3DO's CD-ROM drive, built into the machine rather than plugged in.
/// That matters: the drive is always present even with no disc in it, so "no
/// disc" must still answer commands. A device that never replies is a broken
/// machine, not an empty drive.
///
/// The drive owns the disc it is reading.
pub struct CdRomDevice {
    status: VecDeque<u8>,
    /// Bytes of the command still being assembled.
    pending: Vec<u8>,
    interrupt_request: bool,
    disc_present: bool,
    media_changed: bool,
    motor_on: bool,
    tray_open: bool,
    last_error: u8,
    sector_delay: u32,
    disc: Option<Disc>,
    // A transfer set up by READ and drained through the data FIFO.
    transfer_lba: u32,
    transfer_sectors: u32,
    sector_size: u32,
    data: Vec<u8>,
    data_pos: usize,
    commands: u64,
    last_command: u8,
    last_command_bytes: Vec<u8>,
}

impl Default for CdRomDevice {
    fn default() -> Self {
        CdRomDevice::new()
    }
}

impl CdRomDevice {
    /// How many bytes make one command (ABORT excepted).
    pub const COMMAND_BYTES: usize = 7;

    /// How many bytes the version reply is. Measured from the boot ROM before
    /// it was confirmed against the device documentation: over-supply the
    /// FIFO and the driver stops at exactly the count it expects.
    pub const VERSION_REPLY_BYTES: usize = 12;

    /// A drive delivers sectors at its rotational rate — 150 a second at
    /// double speed — not instantly. Data-ready pulses per sector rather than
    /// sticking on.
    pub const SECTOR_DELAY_CYCLES: u32 = 12_500_000 / 150;

    pub fn new() -> CdRomDevice {
        CdRomDevice {
            status: VecDeque::new(),
            pending: Vec::new(),
            interrupt_request: false,
            disc_present: false,
            media_changed: false,
            motor_on: false,
            tray_open: false,
            last_error: 0,
            sector_delay: 0,
            disc: None,
            transfer_lba: 0,
            transfer_sectors: 0,
            sector_size: SECTOR_USER_BYTES,
            data: Vec::new(),
            data_pos: 0,
            commands: 0,
            last_command: 0,
            last_command_bytes: Vec::new(),
        }
    }

    /// Puts a disc in the drive (or empties it with `None`), returning the one
    /// that was there. Whether the machine is told is
    /// [`CdRomDevice::set_disc_present`]'s job.
    pub fn attach_disc(&mut self, disc: Option<Disc>) -> Option<Disc> {
        std::mem::replace(&mut self.disc, disc)
    }

    /// Takes the disc out of the drive.
    pub fn take_disc(&mut self) -> Option<Disc> {
        self.disc.take()
    }

    pub fn disc(&self) -> Option<&Disc> {
        self.disc.as_ref()
    }

    pub fn disc_mut(&mut self) -> Option<&mut Disc> {
        self.disc.as_mut()
    }

    /// Whether a disc is in the drive, as the machine sees it.
    ///
    /// Inserting a disc is an EVENT, not just a state. The BIOS finishes its
    /// start-up with the expansion-bus interrupt armed and then idles,
    /// waiting to be told the media changed; setting the flag quietly leaves
    /// it idling next to a disc it has no reason to look at.
    pub fn set_disc_present(&mut self, present: bool) {
        if present == self.disc_present {
            return;
        }
        self.disc_present = present;
        self.media_changed = true;
        self.interrupt_request = true;
    }

    /// Media-access is read-clear.
    pub fn take_media_changed(&mut self) -> bool {
        std::mem::take(&mut self.media_changed)
    }

    /// Whether the tray is out. Software's EJECT is how a game asks for the
    /// next disc, so a front end can watch this to swap at the right moment.
    pub fn tray_open(&self) -> bool {
        self.tray_open
    }

    /// Shut the tray after media has been put in it. Inserting closes the
    /// drawer, the way a person does; the drive still waits to be spun up.
    pub fn close_tray(&mut self) {
        self.tray_open = false;
    }

    pub fn disc_present(&self) -> bool {
        self.disc_present
    }

    pub fn take_interrupt_request(&mut self) -> bool {
        std::mem::take(&mut self.interrupt_request)
    }

    pub fn commands_received(&self) -> u64 {
        self.commands
    }

    pub fn last_command(&self) -> u8 {
        self.last_command
    }

    pub fn status_empty(&self) -> bool {
        self.status.is_empty()
    }

    pub fn name(&self) -> &'static str {
        "CD-ROM"
    }

    /// Commands are several bytes and arrive one at a time. The boot ROM
    /// settles the length: it writes 0x83 and exactly six more bytes before it
    /// polls for a reply, and every later command is the same width.
    /// Answering per byte instead makes one command look like seven and runs
    /// the reply FIFO six bytes ahead of the conversation.
    ///
    /// ABORT is the exception and is one byte long: it interrupts a transfer
    /// already running, and a drive that waited for six more bytes would never
    /// abort anything — and would swallow the start of the next command.
    pub fn write_command(&mut self, byte: u8) {
        self.pending.push(byte);
        if self.pending.len() < Self::COMMAND_BYTES && self.pending.first() != Some(&CMD_ABORT) {
            return;
        }

        self.last_command = self.pending.first().copied().unwrap_or(0);
        self.last_command_bytes = std::mem::take(&mut self.pending);
        self.commands = self.commands.wrapping_add(1);
        if trace() {
            let bytes: Vec<String> = self
                .last_command_bytes
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect();
            eprintln!("[cd] command {}", bytes.join(" "));
        }

        // The reply is readable the moment the last command byte lands. The
        // driver writes seven bytes and then spins on the poll register
        // without ticking the machine on, so any delay here is a hang.
        self.status = self.build_reply(self.last_command).into();
        self.interrupt_request = true;
    }

    /// Takes the next byte of the reply, or 0 when there is none.
    pub fn read_status(&mut self) -> u8 {
        let Some(byte) = self.status.pop_front() else {
            return 0;
        };
        if trace() {
            eprintln!("  R {byte:02X}");
        }
        byte
    }

    /// The next byte of sector data.
    pub fn read_data(&mut self) -> u8 {
        let Some(&byte) = self.data.get(self.data_pos) else {
            return 0;
        };
        self.data_pos += 1;

        // Leave the FIFO empty until tick() supplies the next sector at the
        // drive's rate. Refilling here would let a stream run at CPU speed;
        // DMA must wait for data instead of reading zeroes across the gap.
        if self.data_pos >= self.data.len() {
            self.data.clear();
            self.data_pos = 0;
            self.sector_delay = Self::SECTOR_DELAY_CYCLES;
        }
        byte
    }

    /// Whether sector data is waiting in the data FIFO.
    pub fn has_chunk(&self) -> bool {
        self.data_pos < self.data.len()
    }

    /// Fetches the next sector of a transfer when the drive would have
    /// reached it.
    pub fn tick(&mut self, cycles: u32) {
        if self.transfer_sectors > 0 && self.data_pos >= self.data.len() {
            if self.sector_delay > cycles {
                self.sector_delay -= cycles;
            } else {
                self.sector_delay = Self::SECTOR_DELAY_CYCLES;
                if self.fill_next_sector_now() {
                    self.interrupt_request = true;
                }
            }
        }
    }

    pub fn reset(&mut self) {
        self.status.clear();
        self.pending.clear();
        self.media_changed = false;
        self.interrupt_request = false;
        self.motor_on = false;
        self.tray_open = false;
        self.last_error = 0;
        self.transfer_lba = 0;
        self.transfer_sectors = 0;
        self.sector_size = SECTOR_USER_BYTES;
        self.data.clear();
        self.data_pos = 0;
        self.sector_delay = 0;
        self.commands = 0;
        self.last_command = 0;
        self.last_command_bytes.clear();
    }

    /// The drive status byte: only what is actually true.
    ///
    /// A drive with a disc in it reports itself ready, tray shut, disc present
    /// and already spinning — 0xE1 — without being told to spin up; the boot
    /// branches on that, skipping the data-path check and spin-up. There is
    /// deliberately no "success" bit: the driver that boots discs defines
    /// none.
    ///
    /// EJECT is the exception to all of it. While the tray is open the drive
    /// reports READY and nothing else. The driver ejects on purpose partway
    /// through a mount, reads the status back, and told the drive is still
    /// loaded concludes the eject failed and waits for ever.
    pub fn drive_status(&self) -> u8 {
        if self.tray_open {
            return STATUS_READY;
        }
        let mut status = STATUS_DOOR;
        if self.disc_present {
            status |= STATUS_DISC_IN | STATUS_SPIN_UP | STATUS_READY;
        } else if self.motor_on {
            status |= STATUS_SPIN_UP | STATUS_READY;
        }
        status
    }

    fn command_byte(&self, index: usize) -> u8 {
        self.last_command_bytes.get(index).copied().unwrap_or(0)
    }

    fn disc_sectors(&self) -> u32 {
        self.disc.as_ref().map_or(0, Disc::sector_count)
    }

    fn last_track(&self) -> u32 {
        match &self.disc {
            Some(disc) if !disc.tracks().is_empty() => disc.tracks().len() as u32,
            _ => 1,
        }
    }

    /// The reply to a command that needs a disc and has none: the opcode,
    /// then the status with READY cleared and ERROR set.
    fn no_disc_reply(&self, command: u8) -> Vec<u8> {
        vec![
            command,
            (self.drive_status() & !STATUS_READY) | STATUS_ERROR,
        ]
    }

    fn has_disc(&self) -> bool {
        self.disc.is_some() && self.disc_present
    }

    /// Builds the reply to one command.
    ///
    /// Every command answers with its own opcode first — that is how the
    /// driver matches a reply to its request — and ends with the drive
    /// status. Reply lengths differ per command and the driver knows them, so
    /// the wrong number of bytes is not a soft failure: it reads zeroes off
    /// the end of an empty FIFO, takes them for the answer, and gives up.
    fn build_reply(&mut self, command: u8) -> Vec<u8> {
        match command {
            // Drive identification, asked for while the boot ROM enumerates
            // the bus. Answering with zeroes reads as "no drive fitted".
            CMD_VERSION => vec![
                CMD_VERSION,
                0x00, // manufacturer id
                0x10,
                0x00, // manufacturer number
                0x01,
                0x00,
                0x00,
                0x00, // revision
                0x00,
                0x00, // flags
                0x00,
                self.drive_status(),
            ],

            // A loopback the driver uses to prove the bus works.
            CMD_DATA_PATH_CHECK => vec![CMD_DATA_PATH_CHECK, 0xaa, 0x55, self.drive_status()],

            CMD_READ_STATUS => vec![CMD_READ_STATUS, self.drive_status()],

            // The opcode, the last error eight times, and the drive status.
            // The final byte is the STATUS: the driver compares it against
            // the status the failed command returned.
            CMD_READ_ERROR => {
                let mut reply = vec![CMD_READ_ERROR];
                reply.extend([self.last_error; 8]);
                reply.push(self.drive_status());
                reply
            }

            CMD_SET_MODE => {
                // Sub-command 0 carries the sector size the host wants back.
                if self.last_command_bytes.len() >= 5 && self.command_byte(1) == 0x00 {
                    let size =
                        (u32::from(self.command_byte(3)) << 8) | u32::from(self.command_byte(4));
                    if size != 0 {
                        self.sector_size = size;
                    }
                }
                vec![CMD_SET_MODE, self.drive_status()]
            }

            CMD_READ_CAPACITY => {
                // The lead-out is the first LBA after the final sector;
                // converting it to MSF adds the 150-frame lead-in exactly
                // once. Another 150 here reports two fictitious seconds after
                // the lead-out, and DIPIR rejects discs whose authored size
                // reaches that boundary.
                let lead_out = lba_to_msf(self.disc_sectors());
                vec![
                    CMD_READ_CAPACITY,
                    0x00,
                    (lead_out >> 16) as u8,
                    (lead_out >> 8) as u8,
                    lead_out as u8,
                    0x00,
                    0x00,
                    self.drive_status() | STATUS_READY,
                ]
            }

            CMD_READ_DISC_INFO => {
                // First track, last track and where the lead-out starts: how
                // the machine learns a disc's shape before reading any of it.
                if !self.has_disc() {
                    return self.no_disc_reply(CMD_READ_DISC_INFO);
                }
                self.motor_on = true;
                let lead_out = lba_to_msf(self.disc_sectors());
                vec![
                    CMD_READ_DISC_INFO,
                    0x00, // disc type: CD-ROM
                    0x01, // first track
                    self.last_track() as u8,
                    (lead_out >> 16) as u8,
                    (lead_out >> 8) as u8,
                    lead_out as u8,
                    self.drive_status(),
                ]
            }

            CMD_READ_TOC => {
                // Byte 2 names the track. The table is indexed by track
                // NUMBER: entry zero is not the lead-out, it is absent, and
                // reads back as zeroes.
                let track = self.command_byte(2);
                if !self.has_disc() {
                    return self.no_disc_reply(CMD_READ_TOC);
                }
                self.motor_on = true;

                let entry = track.checked_sub(1).and_then(|index| {
                    self.disc
                        .as_ref()?
                        .tracks()
                        .get(usize::from(index))
                        .copied()
                });
                // The control nibble says what kind of track it is: data is
                // 0x04, audio zero. The 0x10 "address" nibble belongs to a
                // subchannel Q reply, not this one.
                let (control, number, msf) = match entry {
                    Some(entry) => (
                        if entry.is_audio { 0x00 } else { 0x04 },
                        track,
                        lba_to_msf(entry.start_lba),
                    ),
                    None => (0, 0, 0),
                };
                vec![
                    CMD_READ_TOC,
                    0x00,
                    control,
                    number,
                    0x00,
                    (msf >> 16) as u8,
                    (msf >> 8) as u8,
                    msf as u8,
                    0x00,
                    self.drive_status() | STATUS_READY,
                ]
            }

            CMD_READ_SESSION_INFO => {
                // Where the session ends, in MSF: eight bytes, which the
                // driver insists on.
                if !self.has_disc() {
                    return self.no_disc_reply(CMD_READ_SESSION_INFO);
                }
                let session = lba_to_msf(self.disc_sectors());
                vec![
                    CMD_READ_SESSION_INFO,
                    0x00,
                    (session >> 16) as u8,
                    (session >> 8) as u8,
                    session as u8,
                    0x00,
                    0x00,
                    self.drive_status() | STATUS_READY,
                ]
            }

            CMD_READ => {
                // Bit 0 of byte 4 clear means the start is MSF rather than a
                // logical block number.
                let mut start = 0;
                let mut count = 0;
                if self.last_command_bytes.len() >= 7 {
                    start = (u32::from(self.command_byte(1)) << 16)
                        | (u32::from(self.command_byte(2)) << 8)
                        | u32::from(self.command_byte(3));
                    if self.command_byte(4) & 0x01 == 0 {
                        start = msf_to_lba(start);
                    }
                    count =
                        (u32::from(self.command_byte(5)) << 8) | u32::from(self.command_byte(6));
                }
                self.start_transfer(start, count);
                self.motor_on = true;
                vec![CMD_READ, self.drive_status()]
            }

            CMD_MOTOR_ON => {
                // Spinning up closes the tray too: the other half of the
                // eject handshake.
                self.motor_on = true;
                self.tray_open = false;
                self.media_changed = false;
                vec![CMD_MOTOR_ON, self.drive_status()]
            }

            CMD_EJECT => {
                self.tray_open = true;
                self.motor_on = false;
                self.media_changed = false;
                self.last_error = 0;
                vec![CMD_EJECT, self.drive_status()]
            }

            // Reports the tray's state without changing it; closing it is the
            // spin-up's job.
            CMD_INJECT => vec![CMD_INJECT, self.drive_status()],

            CMD_MOTOR_OFF => {
                self.motor_on = false;
                vec![CMD_MOTOR_OFF, self.drive_status()]
            }

            // Anything else: echo, then status. A drive that answers nothing
            // looks broken rather than idle.
            other => vec![other, self.drive_status()],
        }
    }

    fn start_transfer(&mut self, lba: u32, sectors: u32) {
        self.transfer_lba = lba;
        self.transfer_sectors = sectors;
        self.data.clear();
        self.data_pos = 0;
        self.sector_delay = Self::SECTOR_DELAY_CYCLES;
        self.fill_next_sector_now();
    }

    /// Pulls one sector into the data FIFO, delivering exactly the blocks the
    /// command asked for and no more. Streaming on past the request would
    /// hold data-ready asserted for ever, telling the host the previous
    /// transfer never finished.
    fn fill_next_sector_now(&mut self) -> bool {
        if self.transfer_sectors == 0 {
            return false;
        }
        let Some(disc) = self.disc.as_mut() else {
            return false;
        };
        if self.transfer_lba >= disc.sector_count() {
            self.transfer_sectors = 0;
            return false;
        }
        let mut sector = [0u8; SECTOR_USER_BYTES as usize];
        if !disc.read_sector(self.transfer_lba, &mut sector) {
            self.transfer_sectors = 0;
            return false;
        }
        let block = if self.sector_size != 0 && self.sector_size <= SECTOR_USER_BYTES {
            self.sector_size as usize
        } else {
            SECTOR_USER_BYTES as usize
        };
        self.data.clear();
        self.data
            .extend_from_slice(sector.get(..block).unwrap_or(&sector));
        self.data_pos = 0;
        self.transfer_lba = self.transfer_lba.wrapping_add(1);
        self.transfer_sectors -= 1;
        true
    }
}

impl XbusDevice for CdRomDevice {
    fn write_command(&mut self, byte: u8) {
        CdRomDevice::write_command(self, byte);
    }

    fn read_status(&mut self) -> u8 {
        CdRomDevice::read_status(self)
    }

    fn status_empty(&self) -> bool {
        CdRomDevice::status_empty(self)
    }

    fn read_data(&mut self) -> u8 {
        CdRomDevice::read_data(self)
    }

    fn has_chunk(&self) -> bool {
        CdRomDevice::has_chunk(self)
    }

    fn take_interrupt_request(&mut self) -> bool {
        CdRomDevice::take_interrupt_request(self)
    }

    fn tick(&mut self, cycles: u32) {
        CdRomDevice::tick(self, cycles);
    }

    fn reset(&mut self) {
        CdRomDevice::reset(self);
    }

    fn name(&self) -> &'static str {
        CdRomDevice::name(self)
    }
}
