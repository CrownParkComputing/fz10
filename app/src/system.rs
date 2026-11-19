//! Everything the CPU can address: memory plus the chips, and the wiring
//! between them.
//!
//! The chips hold no references to each other. A register write that must
//! reach another chip comes back as an effect (CLIO) or a pending flag (MADAM),
//! and this module carries it across. That is the one place the machine's
//! cross-chip behaviour lives — DMA channel enables, the expansion-bus and PBUS
//! transfers, the DSP's FIFOs and interrupt, and the cel engine's start.
#![forbid(unsafe_code)]

use std::sync::Arc;

use crate::bus::CpuBus;
use crate::clio::{irq, Clio, WriteEffects};
use crate::dsp::DspHost;
use crate::madam::{self, Madam};
use crate::memory::{
    in_window, is_device, Memory, CLIO_BASE, CLIO_SIZE, MADAM_BASE, MADAM_SIZE, SPORT_BASE,
    SPORT_SIZE,
};
use crate::pad::PadState;
use crate::pbus::Pbus;
use crate::sport::Sport;
use crate::vdlp::Vdlp;

/// The DSP's view of the machine for one pass of its program: MADAM's DMA
/// channels for its FIFOs, and a note of whether it asked for the audio
/// interrupt (raised in CLIO once the pass is over).
struct DspBridge<'a> {
    madam: &'a mut Madam,
    mem: &'a mut Memory,
    audio_interrupt: bool,
}

impl DspHost for DspBridge<'_> {
    fn input_next(&mut self, channel: u16) -> u16 {
        self.madam.fifo_input_next(channel, self.mem)
    }
    fn input_peek(&mut self, channel: u16) -> u16 {
        self.madam.fifo_input_peek(channel, self.mem)
    }
    fn input_status(&mut self, channel: u16) -> u16 {
        self.madam.fifo_input_status(channel)
    }
    fn output_status(&mut self, channel: u16) -> u16 {
        self.madam.fifo_output_status(channel)
    }
    fn output(&mut self, channel: u16, value: u16) {
        self.madam.fifo_output(channel, value, self.mem);
    }
    fn audio_interrupt(&mut self) {
        self.audio_interrupt = true;
    }
}

pub struct System {
    pub mem: Memory,
    pub clio: Clio,
    pub madam: Madam,
    pub sport: Sport,
    pub vdlp: Vdlp,
    pub pads: Arc<PadState>,
    pbus: Pbus,
    sport_accesses: u64,
    expansion_dma: u64,
    /// Code ranges written behind the CPU's back (DMA), waiting to be dropped
    /// from its decode cache.
    code_invalidations: Vec<(u32, u32)>,
}

impl System {
    pub fn new(pads: Arc<PadState>) -> Self {
        Self {
            mem: Memory::new(),
            clio: Clio::new(),
            madam: Madam::new(),
            sport: Sport::new(),
            vdlp: Vdlp::new(),
            pads,
            pbus: Pbus::new(),
            sport_accesses: 0,
            expansion_dma: 0,
            code_invalidations: Vec::new(),
        }
    }

    /// Reset memory and the chips. ROM, NVRAM and the disc survive.
    pub fn reset(&mut self) {
        self.mem.reset();
        self.clio.reset();
        self.vdlp.reset();
        self.madam.reset();
        self.sport.reset();
        self.sport_accesses = 0;
        self.expansion_dma = 0;
        self.code_invalidations.clear();
    }

    /// SPORT accesses, kept as a diagnostic: a silent decode failure there is
    /// hard to attribute, and the ROM's memory test drives it.
    pub fn sport_accesses(&self) -> u64 {
        self.sport_accesses
    }

    /// Expansion-bus transfers served: whether the disc is read at all.
    pub fn expansion_dma_count(&self) -> u64 {
        self.expansion_dma
    }

    /// Code ranges DMA wrote since the last call.
    pub fn drain_code_invalidations(&mut self) -> std::vec::Drain<'_, (u32, u32)> {
        self.code_invalidations.drain(..)
    }

    // --- CPU-visible access, with device dispatch ---------------------------

    pub fn read32(&mut self, address: u32) -> u32 {
        let addr = address & !3;
        if in_window(addr, CLIO_BASE, CLIO_SIZE) {
            self.clio.read(addr - CLIO_BASE)
        } else if in_window(addr, SPORT_BASE, SPORT_SIZE) {
            self.sport_accesses += 1;
            self.sport.read(addr - SPORT_BASE, &mut self.mem)
        } else if in_window(addr, MADAM_BASE, MADAM_SIZE) {
            self.madam.read(addr - MADAM_BASE)
        } else {
            // Unmapped space reads zero rather than aborting: a recorded
            // compatibility boundary (docs/OPERA_AUDIT.md).
            self.mem.read32(addr)
        }
    }

    pub fn write32(&mut self, address: u32, value: u32) {
        let addr = address & !3;
        if in_window(addr, CLIO_BASE, CLIO_SIZE) {
            let effects = self.clio.write(addr - CLIO_BASE, value);
            self.apply_clio_effects(effects);
        } else if in_window(addr, SPORT_BASE, SPORT_SIZE) {
            self.sport_accesses += 1;
            self.sport.write(addr - SPORT_BASE, value, &mut self.mem);
        } else if in_window(addr, MADAM_BASE, MADAM_SIZE) {
            self.madam.write(addr - MADAM_BASE, value, &mut self.mem);
            self.after_madam();
        } else {
            self.mem.write32(addr, value);
        }
    }

    /// Device registers are word-wide; narrower accesses go through the
    /// containing word, as the hardware does.
    pub fn read16(&mut self, address: u32) -> u16 {
        let addr = address & !1;
        if is_device(addr) {
            let word = self.read32(addr);
            return if addr & 2 != 0 {
                word as u16
            } else {
                (word >> 16) as u16
            };
        }
        self.mem.read16(addr)
    }

    pub fn read8(&mut self, address: u32) -> u8 {
        if is_device(address) {
            let shift = (3 - (address & 3)) * 8;
            return (self.read32(address) >> shift) as u8;
        }
        self.mem.read8(address)
    }

    pub fn write16(&mut self, address: u32, value: u16) {
        let addr = address & !1;
        if is_device(addr) {
            let word = self.read32(addr);
            let merged = if addr & 2 != 0 {
                (word & 0xffff_0000) | u32::from(value)
            } else {
                (word & 0x0000_ffff) | (u32::from(value) << 16)
            };
            self.write32(addr, merged);
        } else {
            self.mem.write16(addr, value);
        }
    }

    pub fn write8(&mut self, address: u32, value: u8) {
        if is_device(address) {
            let shift = (3 - (address & 3)) * 8;
            let word = self.read32(address);
            self.write32(
                address,
                (word & !(0xff << shift)) | (u32::from(value) << shift),
            );
        } else {
            self.mem.write8(address, value);
        }
    }

    // --- cross-chip wiring --------------------------------------------------

    /// Carry anything MADAM asked for to where it belongs.
    fn after_madam(&mut self) {
        self.clio.raise(self.madam.take_interrupts());
        if self.madam.take_pbus_request() {
            self.service_pbus_dma();
        }
    }

    fn apply_clio_effects(&mut self, effects: WriteEffects) {
        if let Some((enabled, cleared)) = effects.channels {
            self.update_dma_channels(enabled, cleared);
        }
        if effects.start_xbus_dma {
            self.service_expansion_dma();
        }
    }

    /// Enabling a channel is CLIO's register but the channel is MADAM's. An
    /// enable edge on an input channel starts its DMA engine now.
    fn update_dma_channels(&mut self, enabled: u32, cleared: u32) {
        let was = self.madam.dma_channel_enable();
        self.madam.set_dma_channel_enable(enabled);
        let newly = enabled & !was;
        for channel in (0..13u16).filter(|channel| newly & (1 << channel) != 0) {
            self.madam.on_input_channel_enabled(channel, &self.mem);
        }
        for channel in (0..13u32).filter(|channel| cleared & (1 << channel) != 0) {
            self.madam.clear_fifo(channel, false);
        }
        for channel in (0..4u32).filter(|channel| cleared & (1 << (channel + 16)) != 0) {
            self.madam.clear_fifo(channel, true);
        }
        self.clio.raise(self.madam.take_interrupts());
    }

    /// Move sector bytes from the drive into memory.
    ///
    /// The CPU never reads the drive's data port: it programs MADAM's
    /// expansion-bus DMA and waits for the completion interrupt. The length
    /// register holds bytes-minus-four, so a transfer moves length + 4 bytes. A
    /// transfer that outruns the drive stays armed across the sector gap.
    pub fn service_expansion_dma(&mut self) {
        if !self.clio.xbus_dma_requested() {
            return;
        }
        let start = self.madam.xbus_dma_address();
        let length = self.madam.read(madam::XBUS_DMA_LENGTH) as i32;
        let mut address = start;
        let mut left = length;
        while left >= 0 {
            if !self.clio.cdrom().has_chunk() {
                if address != start {
                    self.code_invalidations
                        .push((start, address.wrapping_sub(start)));
                }
                self.madam
                    .write(madam::XBUS_DMA_ADDRESS, address, &mut self.mem);
                self.madam
                    .write(madam::XBUS_DMA_LENGTH, left as u32, &mut self.mem);
                return;
            }
            // Bytes arrive most significant first and are stored in ascending
            // address order; reading them straight through would reverse every
            // word into plausible-looking rubbish.
            for lane in 0..4 {
                let byte = self.clio.cdrom_mut().read_data();
                self.write8(address.wrapping_add(lane), byte);
            }
            address = address.wrapping_add(4);
            left -= 4;
        }

        self.clio.clear_xbus_dma_request();
        self.madam
            .write(madam::XBUS_DMA_LENGTH, 0xffff_fffc, &mut self.mem);
        self.madam.clear_xbus_dma();
        self.clio.set_xbus_ready(true);
        let moved = (length as u32).wrapping_add(4);
        self.code_invalidations
            .push((self.madam.xbus_dma_address(), moved));
        self.expansion_dma += 1;
        self.clio.raise(irq::XBUS_DMA_COMPLETE);
    }

    /// Write every attached pad's report into memory and say so.
    ///
    /// The length counts down and holds bytes-minus-four, so a negative value
    /// means there is no buffer. The first word of the buffer is stepped over:
    /// the transfer begins by advancing past it, and software lays its buffer
    /// out expecting that.
    pub fn service_pbus_dma(&mut self) {
        if (self.madam.pbus_length() as i32) < 0 {
            return;
        }
        self.pbus.begin();
        for slot in 0..self.pads.connected_count() {
            self.pbus.add_joypad(self.pads.joypad(slot));
        }
        self.pbus.pad();

        let mut address = self.madam.pbus_address().wrapping_add(4);
        let mut remaining = (self.madam.pbus_length() as i32).wrapping_sub(4);
        self.madam.advance_pbus_pointer();

        let report: Vec<u32> = self
            .pbus
            .data()
            .chunks(4)
            // A report that ends partway through a word is padded as absent.
            .map(|word| {
                word.iter()
                    .copied()
                    .chain(std::iter::repeat(0xff))
                    .take(4)
                    .fold(0u32, |acc, byte| (acc << 8) | u32::from(byte))
            })
            .collect();
        // Anything the devices did not fill reads as absent.
        let mut words = report.into_iter().chain(std::iter::repeat(0xffff_ffff));
        while remaining > 0 {
            let word = words.next().unwrap_or(0xffff_ffff);
            self.write32(address, word);
            remaining -= 4;
            address = address.wrapping_add(4);
            self.madam.advance_pbus_pointer();
        }

        self.madam.set_pbus_address(address);
        self.madam.set_pbus_length(0xffff_fffc);
        self.madam.finish_pbus();
        self.clio.raise_secondary(irq::BANK1_PBUS_COMPLETE);
    }

    /// One pass of the DSP program: one audio sample. Returns its DAC words as
    /// `(right << 16) | left`.
    pub fn run_dsp_sample(&mut self) -> u32 {
        let Self {
            clio, madam, mem, ..
        } = self;
        let mut bridge = DspBridge {
            madam,
            mem,
            audio_interrupt: false,
        };
        let sample = clio.dsp_mut().run(&mut bridge);
        let audio = if bridge.audio_interrupt {
            irq::AUDIO_TIMER
        } else {
            0
        };
        clio.raise(madam.take_interrupts() | audio);
        sample
    }

    /// Advance CLIO and the drive by `cycles`, feeding each scanline to the
    /// VDLP and resuming any expansion transfer the drive can now serve.
    pub fn tick(&mut self, cycles: u32) {
        if self.clio.advance_drive(cycles) {
            self.service_expansion_dma();
        }
        let Self {
            clio,
            vdlp,
            mem,
            madam,
            ..
        } = self;
        let hires = madam.hires_mut();
        if hires.enabled() {
            clio.advance_clock(cycles, |line| {
                vdlp.process_scanline_with(line, mem, Some(&mut *hires))
            });
        } else {
            clio.advance_clock(cycles, |line| vdlp.process_scanline(line, mem));
        }
    }
}

impl CpuBus for System {
    fn read32(&mut self, address: u32) -> u32 {
        System::read32(self, address)
    }
    fn read8(&mut self, address: u32) -> u8 {
        System::read8(self, address)
    }
    fn write32(&mut self, address: u32, value: u32) {
        System::write32(self, address, value);
    }
    fn write8(&mut self, address: u32, value: u8) {
        System::write8(self, address, value);
    }
    fn fetch32(&mut self, address: u32) -> u32 {
        self.mem.fetch32(address)
    }

    /// The cel engine starts between instructions, never inside one.
    fn instruction_boundary(&mut self) {
        if self.madam.cel_engine_pending() {
            self.madam.run_cel_engine(&mut self.mem);
            self.after_madam();
        }
    }

    fn fiq_line(&self) -> bool {
        self.clio.fiq_line()
    }

    fn take_code_invalidation(&mut self) -> Option<(u32, u32)> {
        self.code_invalidations.pop()
    }
}
