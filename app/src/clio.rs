//! CLIO — the 3DO's I/O controller.
//!
//! CLIO owns the interrupt controller, the timer bank, the video line counter,
//! the register window onto the DSP and the expansion bus (XBUS) with the
//! built-in CD-ROM drive on it. It is what lets the boot ROM get anywhere: with
//! no interrupts and no running line counter the ROM waits forever for a
//! vertical blank.
//!
//! CLIO knows nothing about the other chips. Where a register write must reach
//! MADAM (DMA channel enables, the expansion-bus transfer) it says so in the
//! [`WriteEffects`] it returns and the machine acts on it; scanlines are handed
//! to a caller-supplied closure as they pass; the CPU samples
//! [`Clio::fiq_line`].
//!
//! Register offsets and bit assignments are cross-checked against the SDK
//! headers, system patents, the boot ROM's own code paths and the community
//! 3DOessence register map (see docs/HARDWARE.md and docs/PROVENANCE.md).
#![forbid(unsafe_code)]

use crate::dsp::Dsp;
use crate::xbus::CdRomDevice;

/// Register offsets from the base of the CLIO window.
pub mod reg {
    /// Fixed silicon revision; the OS asks before anything else.
    pub const REVISION: u32 = 0x0000;
    pub const CSYSBITS: u32 = 0x0004;
    /// Scanline that raises vertical blank 0 / 1.
    pub const VINT0: u32 = 0x0008;
    pub const VINT1: u32 = 0x000c;
    pub const AUDIO_IN: u32 = 0x0020;
    pub const AUDIO_OUT: u32 = 0x0024;
    /// Why the machine started; the ROM masks it with 0x43.
    pub const CSTATBITS: u32 = 0x0028;
    pub const WATCHDOG: u32 = 0x0030;
    /// The current scanline (low eleven bits) with the field flag in bit 11.
    pub const VCOUNT: u32 = 0x0034;
    pub const HCOUNT: u32 = 0x0038;
    /// A hardware noise source.
    pub const SEED: u32 = 0x003c;

    pub const IRQ0_PENDING: u32 = 0x0040;
    pub const IRQ0_CLEAR: u32 = 0x0044;
    pub const IRQ0_ENABLE: u32 = 0x0048;
    pub const IRQ0_DISABLE: u32 = 0x004c;
    pub const MODE: u32 = 0x0050;
    pub const BADBITS: u32 = 0x0054;
    pub const SPARE: u32 = 0x0058;
    pub const IRQ1_PENDING: u32 = 0x0060;
    pub const IRQ1_CLEAR: u32 = 0x0064;
    pub const IRQ1_ENABLE: u32 = 0x0068;
    pub const IRQ1_DISABLE: u32 = 0x006c;

    pub const HDELAY: u32 = 0x0080;
    /// Masked-write control: bits 0-3 change only where bits 4-7 enable them.
    pub const CONTROL: u32 = 0x0084;
    pub const ADBCTL: u32 = 0x0088;

    /// Sixteen timers, each a counter word then a reload word.
    pub const TIMER_BASE: u32 = 0x0100;
    pub const TIMER_STRIDE: u32 = 8;
    pub const TIMER_COUNT: u32 = 16;
    /// Four configuration bits per timer: set/clear ports for timers 0-7, 8-15.
    pub const TIMER_CONFIG_SET0: u32 = 0x0200;
    pub const TIMER_CONFIG_CLEAR0: u32 = 0x0204;
    pub const TIMER_CONFIG_SET8: u32 = 0x0208;
    pub const TIMER_CONFIG_CLEAR8: u32 = 0x020c;
    /// TIMERCTL: the divider off the 21 MHz timer source.
    pub const TIMER_DELAY: u32 = 0x0220;

    /// Stops and clears the DMA channels named in the mask.
    pub const FIFO_CLEAR: u32 = 0x0300;
    pub const DMA_REQUEST_SET: u32 = 0x0304;
    pub const DMA_REQUEST_CLEAR: u32 = 0x0308;

    /// Two independent expansion-bus control registers, not a set/clear pair.
    pub const XBUS_CTL: u32 = 0x0400;
    pub const XBUS_DIRECTION: u32 = 0x0404;
    pub const XBUS_TYPE0: u32 = 0x0408;
    pub const XBUS_XFER_COUNT: u32 = 0x040c;
    pub const DIPIR1: u32 = 0x0410;
    pub const DIPIR2: u32 = 0x0414;

    /// The four XBUS transaction windows, each 0x40 bytes wide.
    pub const XBUS_SELECT: u32 = 0x0500;
    pub const XBUS_POLL: u32 = 0x0540;
    pub const XBUS_COMMAND: u32 = 0x0580;
    pub const XBUS_DATA: u32 = 0x05c0;
    pub const XBUS_WINDOW: u32 = 0x0040;

    /// The DSP's windows: program memory at two then one word per CPU word,
    /// data memory likewise, then data read-back views.
    pub const DSP_BASE: u32 = 0x1000;
    pub const DSP_SEMAPHORE: u32 = 0x17d0;
    pub const DSP_RESET: u32 = 0x17e8;
    pub const DSP_NOISE: u32 = 0x17f0;
    pub const DSP_RUN: u32 = 0x17fc;
    pub const DSP_PROGRAM2: u32 = 0x1800;
    pub const DSP_PROGRAM1: u32 = 0x2000;
    pub const DSP_DATA2: u32 = 0x3000;
    pub const DSP_DATA1: u32 = 0x3400;
    pub const DSP_READ2: u32 = 0x3800;
    pub const DSP_READ1: u32 = 0x3c00;
    pub const DSP_END: u32 = 0x4000;

    pub const WINDOW_SIZE: u32 = 0x1_0000;
}

/// Interrupt sources in bank 0.
pub mod irq {
    pub const VERTICAL_BLANK0: u32 = 1 << 0;
    pub const VERTICAL_BLANK1: u32 = 1 << 1;
    /// EXINT: XBUS devices, the CD-ROM among them.
    pub const EXPANSION_BUS: u32 = 1 << 2;
    // Bits 3..=10: the odd timers, timer 15 at bit 3 descending.
    pub const AUDIO_TIMER: u32 = 1 << 11;
    // Bits 12..=28: audio DMA channels.
    pub const XBUS_DMA_COMPLETE: u32 = 1 << 29;
    /// Announces that bank 1 has something pending.
    pub const SECONDARY_BANK: u32 = 1 << 31;

    /// Bank 1: a controller scan finished.
    pub const BANK1_PBUS_COMPLETE: u32 = 1 << 0;
}

/// Reset causes reported through CSTATBITS. A cold power-on is 0x40; bit 0
/// would send the ROM down the soft-reset path, which never brings the CD
/// driver up.
pub mod reset_cause {
    pub const SOFTWARE: u32 = 1 << 0;
    pub const WATCHDOG: u32 = 1 << 1;
    pub const POWER_ON: u32 = 1 << 6;
}

/// The four configuration bits of a timer.
pub mod timer_config {
    pub const DECREMENT: u32 = 0x1;
    pub const RELOAD: u32 = 0x2;
    pub const CASCADE: u32 = 0x4;
    pub const BITS: u32 = 4;
}

/// XBUS poll register bits, presented to software active high (the bus
/// signals are active low; CLIO inverts them).
pub mod poll {
    pub const STATUS_IRQ_ENABLE: u32 = 0x01;
    pub const READ_IRQ_ENABLE: u32 = 0x02;
    pub const WRITE_IRQ_ENABLE: u32 = 0x04;
    pub const RESET: u32 = 0x08;
    pub const STATUS_READY: u32 = 0x10;
    pub const CHUNK_READY: u32 = 0x20;
    pub const MEDIA_ACCESS: u32 = 0x40;
    /// An address with nothing fitted reads both state bits set.
    pub const UNFITTED: u32 = 0x30;
    /// SELECTION value naming the bus itself rather than a device.
    pub const SELECT_NONE: u32 = 0x0f;
    /// The built-in drive sits at address zero.
    pub const CD_ROM_ADDRESS: u32 = 0x00;
    /// The low nibble is control; it comes up all set, or the driver takes the
    /// drive for an empty slot.
    const CONTROL_RESET: u32 = 0x0f;
    pub(super) const fn control_reset() -> u32 {
        CONTROL_RESET
    }
}

/// The expansion-bus control register's ready bit, and the bit that vetoes a
/// write to it.
pub const XBUS_READY: u32 = 0x0080;
pub const XBUS_CTL_WRITE_VETO: u32 = 0x0800;
/// The DMA request bit that starts an expansion-bus transfer.
pub const DMA_XBUS_BIT: u32 = 0x0010_0000;
pub const REVISION_VALUE: u32 = 0x0202_0000;
/// The second DIPIR register reads a fixed 0x4000; all-zero DIPIRs tell the ROM
/// there is no disc-change hardware and it never brings the CD driver up.
pub const DIPIR2_VALUE: u32 = 0x4000;
const LINE_MASK: u32 = 0x07ff;
const FIELD_FLAG: u32 = 0x0800;
const CONTROL_WRITE_ENABLE_SHIFT: u32 = 4;
const CONTROL_MASK: u32 = 0x000f;

const CPU_HZ: u64 = 12_500_000;
const TIMER_SOURCE_HZ: u64 = 21_000_000;
const TIMER_DELAY_RESET: u32 = 64;
const TIMER_FIXED_ONE: u64 = 1 << 16;
const FIELDS_PER_SECOND_NTSC: u32 = 60;
const TRACKED_REGISTERS: usize = 2048;

/// The interrupt bit of an odd-numbered timer; even timers cannot interrupt
/// because the high half of a chained pair is the one that signals.
pub const fn timer_interrupt_bit(timer: u32) -> u32 {
    if timer % 2 == 1 && timer < 16 {
        1 << (3 + (15 - timer) / 2)
    } else {
        0
    }
}

/// CPU cycles per scanline for a field of `lines` lines at 60 fields a second.
/// `Clio::tick` (reference build 0x32450) divides by 60 in PAL too.
fn cycles_per_scanline(lines: u32) -> u32 {
    match lines {
        0 => 1,
        lines => (CPU_HZ as u32 / FIELDS_PER_SECOND_NTSC.saturating_mul(lines)).max(1),
    }
}

/// What a register write asks of the rest of the machine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WriteEffects {
    /// The DMA channel enable mask changed: `(enabled, cleared)`. The channels
    /// are MADAM's; CLIO only owns the register.
    pub channels: Option<(u32, u32)>,
    /// The expansion-bus DMA request bit is set: run the transfer now.
    pub start_xbus_dma: bool,
}

/// Two-bank interrupt controller. Pending and enable each have separate set
/// and clear ports; bank 1 chains into bit 31 of bank 0.
#[derive(Clone, Debug, Default)]
struct Interrupts {
    pending0: u32,
    enabled0: u32,
    pending1: u32,
    enabled1: u32,
}

impl Interrupts {
    /// CLIO holds the CPU's FIQ line while any enabled source is pending.
    fn line(&self) -> bool {
        self.pending0 & self.enabled0 != 0
    }

    fn raise_secondary(&mut self, sources: u32) {
        self.pending1 |= sources;
        // Any pending bank-1 source announces the bank, masked or not.
        if self.pending1 != 0 {
            self.pending0 |= irq::SECONDARY_BANK;
        }
    }
}

/// The timer bank: sixteen decrementing counters with four configuration bits
/// each, clocked from 21 MHz divided by TIMERCTL.
#[derive(Clone, Debug)]
struct Timers {
    counter: [u32; 16],
    reload: [u32; 16],
    config: u64,
    delay: u32,
    accumulator: u64,
}

impl Default for Timers {
    fn default() -> Self {
        Self {
            counter: [0; 16],
            reload: [0; 16],
            config: 0,
            delay: TIMER_DELAY_RESET,
            accumulator: 0,
        }
    }
}

impl Timers {
    fn config_of(&self, timer: u32) -> u32 {
        ((self.config >> (timer * timer_config::BITS)) & 0xf) as u32
    }

    /// The timer register an offset names: `(timer, is_reload)`.
    fn decode(offset: u32) -> Option<(usize, bool)> {
        let relative = offset.checked_sub(reg::TIMER_BASE)?;
        if relative >= reg::TIMER_COUNT * reg::TIMER_STRIDE {
            return None;
        }
        Some((
            (relative / reg::TIMER_STRIDE) as usize,
            relative % reg::TIMER_STRIDE >= 4,
        ))
    }

    /// Advance by `cycles` CPU cycles; returns the interrupt bits raised.
    fn advance(&mut self, cycles: u32) -> u32 {
        // 38.095 CPU cycles a count at the stock divider: the fraction is kept.
        let divider = u64::from(self.delay.max(1));
        let per_tick = (CPU_HZ * divider * TIMER_FIXED_ONE / TIMER_SOURCE_HZ).max(1);
        self.accumulator = self
            .accumulator
            .saturating_add(u64::from(cycles) * TIMER_FIXED_ONE);
        let mut raised = 0;
        while self.accumulator >= per_tick {
            self.accumulator -= per_tick;
            if self.config != 0 {
                raised |= self.step();
            }
        }
        raised
    }

    /// One count. The carry chain runs from timer 0 upwards and starts set, so a
    /// cascaded timer 0 still counts. Underflow is the wrap past zero.
    fn step(&mut self) -> u32 {
        let mut raised = 0;
        let mut carry = true;
        for timer in 0..reg::TIMER_COUNT {
            let config = self.config_of(timer);
            let counts = config & timer_config::DECREMENT != 0
                && (config & timer_config::CASCADE == 0 || carry);
            carry = false;
            if !counts {
                continue;
            }
            let index = timer as usize;
            let Some(counter) = self.counter.get_mut(index) else {
                continue;
            };
            let was = *counter;
            *counter = was.wrapping_sub(1);
            if was != 0 {
                continue;
            }
            if config & timer_config::RELOAD != 0 {
                *counter = self.reload.get(index).copied().unwrap_or(0);
            } else {
                // A spent timer stops rather than interrupting every tick.
                *counter = 0;
                self.config &=
                    !(u64::from(timer_config::DECREMENT) << (timer * timer_config::BITS));
            }
            carry = true;
            raised |= timer_interrupt_bit(timer);
        }
        raised
    }
}

/// The expansion bus as CLIO presents it.
#[derive(Clone, Debug)]
struct Xbus {
    /// SELECTION: the low nibble names a device; bit 7 asks for the control
    /// nibble alone.
    select: u32,
    select_modifier: u32,
    /// The bus's own poll register (selection 0x0F).
    bus_poll: u32,
    /// The drive's control nibble.
    device_poll: u32,
    dma_enable: u32,
    dma_requested: bool,
    media_changed: bool,
    control: u32,
    direction: u32,
    type0: u32,
    xfer_count: u32,
    dipir1: u32,
    dipir2: u32,
}

impl Default for Xbus {
    fn default() -> Self {
        Self {
            select: 0,
            select_modifier: 0,
            bus_poll: 0,
            device_poll: poll::control_reset(),
            dma_enable: 0,
            dma_requested: false,
            media_changed: false,
            control: XBUS_READY,
            direction: 0,
            type0: 0,
            xfer_count: 0,
            dipir1: 0,
            dipir2: DIPIR2_VALUE,
        }
    }
}

/// Plain read/write latches with no modelled side effect.
#[derive(Clone, Debug, Default)]
struct Latches {
    csys_bits: u32,
    cstat_bits: u32,
    vint0: u32,
    vint1: u32,
    audio_in: u32,
    audio_out: u32,
    watchdog: u32,
    seed: u32,
    mode: u32,
    spare: u32,
    hdelay: u32,
    adbctl: u32,
    control: u32,
}

/// Which low registers software wrote, with what, and how often it read them.
#[derive(Clone, Debug)]
struct RegisterLog {
    written: Vec<Option<u32>>,
    reads: Vec<u64>,
}

impl RegisterLog {
    fn new() -> Self {
        Self {
            written: vec![None; TRACKED_REGISTERS],
            reads: vec![0; TRACKED_REGISTERS],
        }
    }
    fn index(offset: u32) -> usize {
        (offset / 4) as usize
    }
}

pub struct Clio {
    interrupts: Interrupts,
    timers: Timers,
    xbus: Xbus,
    latches: Latches,
    log: RegisterLog,

    scanline: u32,
    cycles_into_line: u32,
    scanlines_per_field: u32,
    field_complete: bool,
    field_odd: bool,

    random_state: u32,
    /// The raw DSP window, kept for addresses that are not live DSP memory.
    dsp_window: Vec<u32>,
    dsp_writes: u64,

    dsp: Dsp,
    cdrom: CdRomDevice,
}

impl Default for Clio {
    fn default() -> Self {
        Self::new()
    }
}

impl Clio {
    pub fn new() -> Self {
        let mut clio = Self {
            interrupts: Interrupts::default(),
            timers: Timers::default(),
            xbus: Xbus::default(),
            latches: Latches::default(),
            log: RegisterLog::new(),
            scanline: 0,
            cycles_into_line: 0,
            scanlines_per_field: 263,
            field_complete: false,
            field_odd: false,
            random_state: 0xdead_beef,
            dsp_window: vec![0; ((reg::DSP_END - reg::DSP_BASE) / 4) as usize],
            dsp_writes: 0,
            dsp: Dsp::new(),
            cdrom: CdRomDevice::new(),
        };
        clio.reset();
        clio
    }

    /// Power-on state. A reset reports a cold start through CSTATBITS: zero is
    /// not a neutral answer to "why did the machine start", it hangs the ROM.
    /// The DSP is a separate device and keeps its own reset.
    pub fn reset(&mut self) {
        self.interrupts = Interrupts::default();
        self.timers = Timers::default();
        self.xbus = Xbus::default();
        self.latches = Latches {
            cstat_bits: reset_cause::POWER_ON,
            ..Latches::default()
        };
        self.scanline = 0;
        self.cycles_into_line = 0;
        self.field_complete = false;
        self.field_odd = false;
        self.dsp_window.fill(0);
        self.dsp_writes = 0;
        self.cdrom.reset();
    }

    // --- interrupts -------------------------------------------------------

    /// Raise sources in bank 0.
    pub fn raise(&mut self, sources: u32) {
        self.interrupts.pending0 |= sources;
    }

    /// Raise sources in bank 1, which announce themselves through bank 0.
    pub fn raise_secondary(&mut self, sources: u32) {
        self.interrupts.raise_secondary(sources);
    }

    /// The level CLIO drives onto the CPU's FIQ input.
    pub fn fiq_line(&self) -> bool {
        self.interrupts.line()
    }

    pub fn irq0_pending(&self) -> u32 {
        self.interrupts.pending0
    }
    pub fn irq0_enabled(&self) -> u32 {
        self.interrupts.enabled0
    }

    // --- video timing -----------------------------------------------------

    pub fn set_scanlines_per_field(&mut self, lines: u32) {
        self.scanlines_per_field = lines;
    }
    pub fn scanline(&self) -> u32 {
        self.scanline
    }
    /// Set when the line counter wraps; the frame loop stops there.
    pub fn field_complete(&self) -> bool {
        self.field_complete
    }
    pub fn clear_field_complete(&mut self) {
        self.field_complete = false;
    }

    /// Advance the drive by `cycles`. Returns whether an expansion-bus transfer
    /// is waiting for data the drive now has, so the machine can resume it.
    pub fn advance_drive(&mut self, cycles: u32) -> bool {
        self.cdrom.tick(cycles);
        self.xbus.dma_requested && self.cdrom.has_chunk()
    }

    /// Advance timers and the line counter by `cycles`, handing each scanline
    /// to `on_scanline` as it begins (horizontal blank, when the VDLP fetches).
    pub fn advance_clock(&mut self, cycles: u32, mut on_scanline: impl FnMut(u32)) {
        if self.cdrom.take_media_changed() {
            self.xbus.media_changed = true;
        }
        if self.cdrom.take_interrupt_request() {
            self.raise(irq::EXPANSION_BUS);
        }

        let raised = self.timers.advance(cycles);
        self.raise(raised);

        let per_line = cycles_per_scanline(self.scanlines_per_field);
        self.cycles_into_line = self.cycles_into_line.saturating_add(cycles);
        while self.cycles_into_line >= per_line {
            self.cycles_into_line -= per_line;
            self.scanline = self.scanline.wrapping_add(1);
            if self.scanline == self.latches.vint0 {
                self.raise(irq::VERTICAL_BLANK0);
            }
            if self.scanline == self.latches.vint1 {
                self.raise(irq::VERTICAL_BLANK1);
            }
            if self.scanline >= self.scanlines_per_field {
                self.scanline = 0;
                self.field_complete = true;
                // Interlaced fields alternate; the ROM waits on this flag.
                self.field_odd = !self.field_odd;
            }
            on_scanline(self.scanline);
        }
    }

    /// Everything one tick does, with nowhere to send scanlines or a DMA
    /// resume. For tests and harnesses that drive CLIO alone.
    pub fn tick(&mut self, cycles: u32) {
        self.tick_with(cycles, |_| {});
    }

    pub fn tick_with(&mut self, cycles: u32, on_scanline: impl FnMut(u32)) {
        self.advance_drive(cycles);
        self.advance_clock(cycles, on_scanline);
    }

    // --- expansion bus ----------------------------------------------------

    pub fn cdrom(&self) -> &CdRomDevice {
        &self.cdrom
    }
    pub fn cdrom_mut(&mut self) -> &mut CdRomDevice {
        &mut self.cdrom
    }
    pub fn xbus_dma_requested(&self) -> bool {
        self.xbus.dma_requested
    }
    pub fn clear_xbus_dma_request(&mut self) {
        self.xbus.dma_requested = false;
    }
    pub fn set_xbus_ready(&mut self, ready: bool) {
        if ready {
            self.xbus.control |= XBUS_READY;
        } else {
            self.xbus.control &= !XBUS_READY;
        }
    }

    /// The drive's poll register: software's control nibble plus live state.
    fn drive_poll(&self) -> u32 {
        let mut value = self.xbus.device_poll & 0x0f;
        if !self.cdrom.status_empty() {
            value |= poll::STATUS_READY;
        }
        if self.cdrom.has_chunk() {
            value |= poll::CHUNK_READY;
        }
        if self.xbus.media_changed {
            value |= poll::MEDIA_ACCESS;
        }
        value
    }

    /// The drive interrupts when a condition is both asserted and enabled.
    /// Checked when software touches the bus, as the hardware does.
    fn raise_xbus_interrupt_if_pending(&mut self) {
        let value = self.drive_poll();
        let status = value & poll::STATUS_READY != 0 && value & poll::STATUS_IRQ_ENABLE != 0;
        let data = value & poll::CHUNK_READY != 0 && value & poll::READ_IRQ_ENABLE != 0;
        if status || data {
            self.raise(irq::EXPANSION_BUS);
        }
    }

    fn drive_selected(&self) -> bool {
        self.xbus.select == poll::CD_ROM_ADDRESS
    }

    fn read_xbus(&mut self, window: u32) -> u32 {
        match window {
            reg::XBUS_SELECT => self.xbus.select,
            reg::XBUS_POLL => {
                let mut value = if self.xbus.select == poll::SELECT_NONE {
                    self.xbus.bus_poll
                } else if self.drive_selected() {
                    let value = self.drive_poll();
                    self.xbus.media_changed = false; // media access is read-clear
                    value
                } else {
                    poll::UNFITTED
                };
                if self.xbus.select_modifier & 0x80 != 0 {
                    value &= 0x0f;
                }
                value
            }
            reg::XBUS_COMMAND => {
                // RD_STAT
                if !self.drive_selected() {
                    return 0;
                }
                let byte = self.cdrom.read_status();
                if self.cdrom.take_interrupt_request() {
                    self.raise(irq::EXPANSION_BUS);
                }
                u32::from(byte)
            }
            reg::XBUS_DATA if self.drive_selected() => u32::from(self.cdrom.read_data()),
            _ => 0,
        }
    }

    fn write_xbus(&mut self, window: u32, value: u32) {
        match window {
            reg::XBUS_SELECT => {
                self.xbus.select = value & 0x0f;
                self.xbus.select_modifier = value & 0xf0;
            }
            reg::XBUS_POLL => {
                if self.xbus.select == poll::SELECT_NONE {
                    self.xbus.bus_poll = (self.xbus.bus_poll & 0xf0) | (value & 0x0f);
                } else if self.drive_selected() {
                    self.xbus.device_poll = (self.xbus.device_poll & 0xf0) | (value & 0x0f);
                    self.raise_xbus_interrupt_if_pending();
                }
            }
            reg::XBUS_COMMAND if self.drive_selected() => {
                self.cdrom.write_command((value & 0xff) as u8);
                self.raise_xbus_interrupt_if_pending();
            }
            _ => {}
        }
    }

    // --- DSP window -------------------------------------------------------

    pub fn dsp(&self) -> &Dsp {
        &self.dsp
    }
    pub fn dsp_mut(&mut self) -> &mut Dsp {
        &mut self.dsp
    }
    /// What the machine has written into the DSP window, for diagnostics.
    pub fn dsp_writes(&self) -> u64 {
        self.dsp_writes
    }
    pub fn dsp_word(&self, offset: u32) -> u32 {
        offset
            .checked_sub(reg::DSP_BASE)
            .and_then(|relative| self.dsp_window.get((relative / 4) as usize))
            .copied()
            .unwrap_or(0)
    }

    fn noise(&mut self) -> u32 {
        self.random_state = self.random_state.wrapping_add(0x9e37_79b9);
        let mut z = self.random_state;
        z = (z ^ (z >> 16)).wrapping_mul(0x85eb_ca6b);
        z = (z ^ (z >> 13)).wrapping_mul(0xc2b2_ae35);
        z ^ (z >> 16)
    }

    /// The DSP decodes its own views (program and data memory at one and two
    /// words per CPU word, the read-back views, semaphore, reset and run);
    /// CLIO keeps the noise source and a raw image of the rest of the window.
    fn read_dsp(&mut self, offset: u32) -> Option<u32> {
        if let Some(value) = self.dsp.cpu_window_read(offset) {
            return Some(value);
        }
        if offset == reg::DSP_NOISE {
            return Some(self.noise());
        }
        (reg::DSP_BASE..reg::DSP_END)
            .contains(&offset)
            .then(|| self.dsp_word(offset))
    }

    /// Route a store into the DSP window. Returns false when the offset is not
    /// in it.
    fn write_dsp(&mut self, offset: u32, value: u32) -> bool {
        if self.dsp.cpu_window_write(offset, value) {
            return true;
        }
        if !(reg::DSP_BASE..reg::DSP_END).contains(&offset) {
            return false;
        }
        if let Some(word) = self
            .dsp_window
            .get_mut(((offset - reg::DSP_BASE) / 4) as usize)
        {
            *word = value;
        }
        self.dsp_writes += 1;
        true
    }

    // --- register interface -----------------------------------------------

    pub fn register_written(&self, offset: u32) -> bool {
        self.register_last_write_opt(offset).is_some()
    }
    pub fn register_last_write(&self, offset: u32) -> u32 {
        self.register_last_write_opt(offset).unwrap_or(0)
    }
    fn register_last_write_opt(&self, offset: u32) -> Option<u32> {
        self.log
            .written
            .get(RegisterLog::index(offset))
            .copied()
            .flatten()
    }
    pub fn register_reads(&self, offset: u32) -> u64 {
        self.log
            .reads
            .get(RegisterLog::index(offset))
            .copied()
            .unwrap_or(0)
    }

    pub fn read(&mut self, offset: u32) -> u32 {
        let offset = offset & (reg::WINDOW_SIZE - 1);
        if let Some(count) = self.log.reads.get_mut(RegisterLog::index(offset)) {
            *count += 1;
        }

        if (reg::XBUS_SELECT..reg::XBUS_DATA + reg::XBUS_WINDOW).contains(&offset) {
            return self.read_xbus(offset & !(reg::XBUS_WINDOW - 1));
        }
        if let Some(value) = self.read_dsp(offset) {
            return value;
        }
        if let Some((timer, is_reload)) = Timers::decode(offset) {
            let bank = if is_reload {
                &self.timers.reload
            } else {
                &self.timers.counter
            };
            return bank.get(timer).copied().unwrap_or(0);
        }

        let latches = &self.latches;
        match offset {
            reg::REVISION => REVISION_VALUE,
            reg::CSYSBITS => latches.csys_bits,
            reg::VINT0 => latches.vint0,
            reg::VINT1 => latches.vint1,
            reg::AUDIO_IN => latches.audio_in,
            reg::AUDIO_OUT => latches.audio_out,
            reg::CSTATBITS => latches.cstat_bits,
            reg::WATCHDOG => latches.watchdog,
            reg::TIMER_DELAY => self.timers.delay,
            reg::VCOUNT => {
                (self.scanline & LINE_MASK) | if self.field_odd { FIELD_FLAG } else { 0 }
            }
            reg::HCOUNT => self.cycles_into_line,
            // A noise source, not a latch: a constant would make software that
            // picks between equally valid paths take the same branch for ever.
            reg::SEED => self.noise(),
            // Both ports of each pair read the same mask.
            reg::IRQ0_PENDING | reg::IRQ0_CLEAR => self.interrupts.pending0,
            // Bit 31 always reads set: the chip saying bank 1 exists.
            reg::IRQ0_ENABLE | reg::IRQ0_DISABLE => self.interrupts.enabled0 | irq::SECONDARY_BANK,
            reg::IRQ1_PENDING | reg::IRQ1_CLEAR => self.interrupts.pending1,
            reg::IRQ1_ENABLE | reg::IRQ1_DISABLE => self.interrupts.enabled1,
            reg::CONTROL => latches.control,
            reg::MODE => latches.mode,
            reg::SPARE => latches.spare,
            reg::HDELAY => latches.hdelay,
            reg::ADBCTL => latches.adbctl,
            reg::DMA_REQUEST_SET | reg::DMA_REQUEST_CLEAR => self.xbus.dma_enable,
            reg::XBUS_CTL => self.xbus.control,
            reg::XBUS_DIRECTION => self.xbus.direction,
            reg::XBUS_TYPE0 => self.xbus.type0,
            reg::XBUS_XFER_COUNT => self.xbus.xfer_count,
            reg::DIPIR1 => self.xbus.dipir1,
            reg::DIPIR2 => self.xbus.dipir2,
            reg::TIMER_CONFIG_SET0 | reg::TIMER_CONFIG_CLEAR0 => {
                (self.timers.config & 0xffff_ffff) as u32
            }
            reg::TIMER_CONFIG_SET8 | reg::TIMER_CONFIG_CLEAR8 => (self.timers.config >> 32) as u32,
            _ => 0,
        }
    }

    pub fn write(&mut self, offset: u32, value: u32) -> WriteEffects {
        let offset = offset & (reg::WINDOW_SIZE - 1);
        if let Some(slot) = self.log.written.get_mut(RegisterLog::index(offset)) {
            *slot = Some(value);
        }

        if (reg::XBUS_SELECT..reg::XBUS_DATA + reg::XBUS_WINDOW).contains(&offset) {
            self.write_xbus(offset & !(reg::XBUS_WINDOW - 1), value);
            return WriteEffects::default();
        }
        if self.write_dsp(offset, value) {
            return WriteEffects::default();
        }
        if let Some((timer, is_reload)) = Timers::decode(offset) {
            let bank = if is_reload {
                &mut self.timers.reload
            } else {
                &mut self.timers.counter
            };
            if let Some(slot) = bank.get_mut(timer) {
                *slot = value;
            }
            return WriteEffects::default();
        }

        let interrupts = &mut self.interrupts;
        let latches = &mut self.latches;
        match offset {
            // Fixed silicon identification.
            reg::REVISION => {}
            reg::CSYSBITS => latches.csys_bits = value,
            reg::VINT0 => latches.vint0 = value,
            reg::VINT1 => latches.vint1 = value,
            reg::AUDIO_IN => latches.audio_in = value,
            reg::AUDIO_OUT => latches.audio_out = value,
            reg::WATCHDOG => latches.watchdog = value,
            reg::SEED => latches.seed = value,
            reg::MODE => latches.mode = value,
            reg::SPARE => latches.spare = value,
            reg::HDELAY => latches.hdelay = value,
            reg::ADBCTL => latches.adbctl = value,
            // The ROM clears the cause once read, so it is reported once.
            reg::CSTATBITS => latches.cstat_bits = value,
            reg::TIMER_DELAY => self.timers.delay = value & 0x3ff,
            // Set/clear pairs rather than read-modify-write: acknowledging one
            // source cannot lose another that fires meanwhile.
            reg::IRQ0_PENDING => interrupts.pending0 |= value,
            reg::IRQ0_CLEAR => interrupts.pending0 &= !value,
            reg::IRQ0_ENABLE => interrupts.enabled0 |= value,
            reg::IRQ0_DISABLE => interrupts.enabled0 &= !value,
            reg::IRQ1_PENDING => interrupts.pending1 |= value,
            reg::IRQ1_CLEAR => interrupts.pending1 &= !value,
            reg::IRQ1_ENABLE => interrupts.enabled1 |= value,
            reg::IRQ1_DISABLE => interrupts.enabled1 &= !value,
            reg::FIFO_CLEAR => {
                self.xbus.dma_enable &= !value;
                if value & DMA_XBUS_BIT != 0 {
                    self.xbus.dma_requested = false;
                }
                return WriteEffects {
                    channels: Some((self.xbus.dma_enable, value)),
                    start_xbus_dma: false,
                };
            }
            reg::DMA_REQUEST_SET | reg::DMA_REQUEST_CLEAR => {
                if offset == reg::DMA_REQUEST_SET {
                    self.xbus.dma_enable |= value;
                } else {
                    self.xbus.dma_enable &= !value;
                    if value & DMA_XBUS_BIT != 0 {
                        self.xbus.dma_requested = false;
                    }
                }
                // The request bit alone starts the transfer; there is no second
                // enable to wait for.
                let start = self.xbus.dma_enable & DMA_XBUS_BIT != 0;
                if start {
                    self.xbus.dma_requested = true;
                }
                return WriteEffects {
                    channels: Some((self.xbus.dma_enable, 0)),
                    start_xbus_dma: start,
                };
            }
            // A write carrying bit 11 is refused, value and all.
            reg::XBUS_CTL if value & XBUS_CTL_WRITE_VETO == 0 => self.xbus.control = value,
            reg::XBUS_DIRECTION => self.xbus.direction = value,
            reg::XBUS_TYPE0 => self.xbus.type0 = value,
            reg::XBUS_XFER_COUNT => self.xbus.xfer_count = value,
            reg::CONTROL => {
                let enable = (value >> CONTROL_WRITE_ENABLE_SHIFT) & CONTROL_MASK;
                latches.control = (latches.control & !enable) | (value & enable & CONTROL_MASK);
            }
            reg::TIMER_CONFIG_SET0 => self.timers.config |= u64::from(value),
            reg::TIMER_CONFIG_CLEAR0 => self.timers.config &= !u64::from(value),
            reg::TIMER_CONFIG_SET8 => self.timers.config |= u64::from(value) << 32,
            reg::TIMER_CONFIG_CLEAR8 => self.timers.config &= !(u64::from(value) << 32),
            _ => {}
        }
        WriteEffects::default()
    }
}
