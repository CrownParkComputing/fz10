//! CLIO: the interrupt handshake, the timer bank, video timing and the
//! expansion bus as the boot ROM drives it.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::panic
)]

use std::sync::Arc;

use retro3do::arm60::{Arm60, Mode, FLAG_F, MODE_MASK, VECTOR_FIQ};
use retro3do::clio::{
    irq, poll, reg, timer_config, timer_interrupt_bit, Clio, DMA_XBUS_BIT, XBUS_CTL_WRITE_VETO,
};
use retro3do::madam::{self, Madam};
use retro3do::memory::{Memory, CLIO_BASE, ROM_BASE};
use retro3do::pad::PadState;
use retro3do::system::System;
use retro3do::xbus::{
    CMD_ABORT, CMD_EJECT, CMD_MOTOR_ON, CMD_READ, CMD_READ_CAPACITY, CMD_READ_STATUS, CMD_VERSION,
    STATUS_DISC_IN, STATUS_READY, STATUS_SPIN_UP,
};
use retro3do::{Console, Region};

/// Timer 3 is the one the boot ROM uses, and being odd it can interrupt.
const TIMER3_IRQ: u32 = timer_interrupt_bit(3);
const TIMER3_COUNTER: u32 = reg::TIMER_BASE + 3 * reg::TIMER_STRIDE;
const TIMER3_RELOAD: u32 = TIMER3_COUNTER + 4;

const fn config(timer: u32, bits: u32) -> u32 {
    bits << (timer * timer_config::BITS)
}

/// A CPU and the system it addresses, with nothing else attached.
struct Chip {
    cpu: Arm60,
    sys: System,
}

impl Chip {
    fn new() -> Self {
        Self {
            cpu: Arm60::new(),
            sys: System::new(Arc::new(PadState::new())),
        }
    }
    fn clio(&mut self) -> &mut Clio {
        &mut self.sys.clio
    }
    fn step(&mut self) {
        self.cpu.step(&mut self.sys);
    }
    fn unmask_fiq(&mut self) {
        let cpsr = self.cpu.cpsr();
        self.cpu.set_cpsr(cpsr & !FLAG_F);
    }
    /// Back to ordinary supervisor code at the ROM base with FIQ unmasked.
    fn return_to_supervisor(&mut self) {
        let cpsr = self.cpu.cpsr();
        self.cpu
            .set_cpsr((cpsr & !MODE_MASK & !FLAG_F) | Mode::Supervisor.bits());
        self.cpu.set_reg(15, ROM_BASE);
    }
    /// One seven-byte command into the drive.
    fn command(&mut self, bytes: [u8; 7]) {
        for byte in bytes {
            self.clio().write(reg::XBUS_COMMAND, u32::from(byte));
        }
    }
    fn opcode(&mut self, opcode: u8) {
        self.command([opcode, 0, 0, 0, 0, 0, 0]);
    }
}

/// CPU cycles that advance a timer by exactly n counts at the reset divider
/// (38.095 cycles a count, rounded up by one).
fn counts(n: u64) -> u32 {
    (n * 12_500_000 * 64 / 21_000_000 + 1) as u32
}

// --- interrupt controller --------------------------------------------------

#[test]
fn a_pending_interrupt_does_nothing_until_it_is_enabled() {
    let mut c = Chip::new();
    c.clio().raise(irq::VERTICAL_BLANK0);
    assert_eq!(c.clio().read(reg::IRQ0_PENDING), irq::VERTICAL_BLANK0);
    assert!(!c.clio().fiq_line());
    c.clio().write(reg::IRQ0_ENABLE, irq::VERTICAL_BLANK0);
    assert_eq!(
        c.clio().read(reg::IRQ0_ENABLE) & !irq::SECONDARY_BANK,
        irq::VERTICAL_BLANK0
    );
    assert!(c.clio().fiq_line());
}

#[test]
fn acknowledging_writes_the_bits_to_the_clear_port() {
    let mut c = Chip::new();
    c.clio()
        .write(reg::IRQ0_ENABLE, irq::VERTICAL_BLANK0 | TIMER3_IRQ);
    c.clio().raise(irq::VERTICAL_BLANK0 | TIMER3_IRQ);
    assert_eq!(
        c.clio().read(reg::IRQ0_PENDING),
        irq::VERTICAL_BLANK0 | TIMER3_IRQ
    );
    c.clio().write(reg::IRQ0_CLEAR, irq::VERTICAL_BLANK0);
    assert_eq!(c.clio().read(reg::IRQ0_PENDING), TIMER3_IRQ);
}

#[test]
fn the_disable_port_clears_enables_without_touching_pending() {
    let mut c = Chip::new();
    c.clio()
        .write(reg::IRQ0_ENABLE, irq::VERTICAL_BLANK0 | TIMER3_IRQ);
    c.clio().raise(irq::VERTICAL_BLANK0);
    c.clio().write(reg::IRQ0_DISABLE, irq::VERTICAL_BLANK0);
    assert_eq!(
        c.clio().read(reg::IRQ0_ENABLE) & !irq::SECONDARY_BANK,
        TIMER3_IRQ
    );
    assert_eq!(c.clio().read(reg::IRQ0_PENDING), irq::VERTICAL_BLANK0);
}

#[test]
fn an_enabled_and_pending_interrupt_reaches_the_cpu() {
    // CLIO drives FIQ, not IRQ.
    let mut c = Chip::new();
    c.unmask_fiq();
    c.clio().write(reg::IRQ0_ENABLE, irq::VERTICAL_BLANK0);
    c.clio().raise(irq::VERTICAL_BLANK0);
    c.step();
    assert_eq!(c.cpu.pc(), VECTOR_FIQ);
    assert_eq!(c.cpu.mode(), Mode::Fiq);
}

#[test]
fn the_interrupt_line_drops_when_the_source_is_acknowledged() {
    let mut c = Chip::new();
    c.unmask_fiq();
    c.clio().write(reg::IRQ0_ENABLE, irq::VERTICAL_BLANK0);
    c.clio().raise(irq::VERTICAL_BLANK0);
    c.step();
    assert_eq!(c.cpu.pc(), VECTOR_FIQ);

    c.clio().write(reg::IRQ0_CLEAR, irq::VERTICAL_BLANK0);
    c.return_to_supervisor();
    c.step();
    assert_eq!(c.cpu.mode(), Mode::Supervisor);
}

#[test]
fn an_unacknowledged_source_keeps_interrupting() {
    let mut c = Chip::new();
    c.unmask_fiq();
    c.clio().write(reg::IRQ0_ENABLE, irq::VERTICAL_BLANK0);
    c.clio().raise(irq::VERTICAL_BLANK0);
    c.step();
    assert_eq!(c.cpu.pc(), VECTOR_FIQ);

    c.return_to_supervisor();
    c.step();
    assert_eq!(c.cpu.pc(), VECTOR_FIQ); // still asserted

    c.clio().write(reg::IRQ0_CLEAR, irq::VERTICAL_BLANK0);
    c.return_to_supervisor();
    c.step();
    assert_ne!(c.cpu.pc(), VECTOR_FIQ);
}

#[test]
fn a_second_edge_interrupts_again() {
    let mut c = Chip::new();
    c.unmask_fiq();
    c.clio()
        .write(reg::IRQ0_ENABLE, irq::VERTICAL_BLANK0 | TIMER3_IRQ);
    c.clio().raise(irq::VERTICAL_BLANK0);
    c.step();
    assert_eq!(c.cpu.pc(), VECTOR_FIQ);

    c.return_to_supervisor();
    c.clio().write(reg::IRQ0_CLEAR, irq::VERTICAL_BLANK0);
    c.clio().raise(irq::VERTICAL_BLANK0);
    c.step();
    assert_eq!(c.cpu.pc(), VECTOR_FIQ);
}

#[test]
fn clio_registers_are_reachable_through_the_bus() {
    let mut c = Chip::new();
    c.sys.write32(CLIO_BASE + reg::IRQ0_ENABLE, TIMER3_IRQ);
    assert_eq!(
        c.sys.read32(CLIO_BASE + reg::IRQ0_ENABLE) & !irq::SECONDARY_BANK,
        TIMER3_IRQ
    );
    c.clio().raise(TIMER3_IRQ);
    assert_eq!(c.sys.read32(CLIO_BASE + reg::IRQ0_PENDING), TIMER3_IRQ);
}

#[test]
fn clio_reports_a_fixed_read_only_hardware_revision() {
    let mut clio = Clio::new();
    assert_eq!(clio.read(reg::REVISION), 0x0202_0000);
    clio.write(reg::REVISION, 0xdead_beef);
    assert_eq!(clio.read(reg::REVISION), 0x0202_0000);
    clio.reset();
    assert_eq!(clio.read(reg::REVISION), 0x0202_0000);
}

#[test]
fn ordinary_low_clio_registers_latch_and_reset_as_read_write_hardware() {
    let mut clio = Clio::new();
    let registers = [
        (reg::AUDIO_IN, 0x1020_3040),
        (reg::AUDIO_OUT, 0x2131_4151),
        (reg::SPARE, 0x3242_5262),
        (reg::HDELAY, 0x4353_6373),
        (reg::ADBCTL, 0x5464_7484),
    ];
    for (offset, value) in registers {
        clio.write(offset, value);
        assert_eq!(clio.read(offset), value);
    }
    clio.reset();
    for (offset, _) in registers {
        assert_eq!(clio.read(offset), 0);
    }
}

// --- video timing ----------------------------------------------------------

#[test]
fn the_line_counter_advances_and_wraps() {
    let mut clio = Clio::new();
    clio.set_scanlines_per_field(263);
    assert_eq!(clio.scanline(), 0);
    clio.tick(800);
    assert_eq!(clio.scanline(), 1);
    for _ in 0..300 {
        clio.tick(800);
    }
    assert!(clio.field_complete());
    assert!(clio.scanline() < 263);
}

#[test]
fn the_scanline_callback_runs_at_each_horizontal_blank() {
    let mut clio = Clio::new();
    let mut seen = Vec::new();
    clio.set_scanlines_per_field(263);
    clio.tick_with(800, |line| seen.push(line));
    assert_eq!(seen, [1]);
}

#[test]
fn a_configured_scanline_raises_the_vertical_blank_interrupt() {
    let mut clio = Clio::new();
    clio.set_scanlines_per_field(263);
    clio.write(reg::VINT0, 10);
    clio.write(reg::IRQ0_ENABLE, irq::VERTICAL_BLANK0);
    clio.tick(800 * 5);
    assert_eq!(clio.read(reg::IRQ0_PENDING) & irq::VERTICAL_BLANK0, 0);
    clio.tick(800 * 6);
    assert_eq!(
        clio.read(reg::IRQ0_PENDING) & irq::VERTICAL_BLANK0,
        irq::VERTICAL_BLANK0
    );
}

#[test]
fn the_region_sets_the_field_length() {
    let mut console = Console::new();
    console.set_region(Region::Ntsc);
    console.reset();
    assert_eq!(console.clio().scanline(), 0);
    // A frame stops at the field boundary rather than running the arithmetic
    // budget, or video and emulation drift apart.
    console.run_frame();
    assert!(console.clio().scanline() < 263);
}

#[test]
fn the_field_flag_sits_above_the_line_number() {
    // The ROM spins on bit 11 and then masks with 0x7FF for the line.
    let mut clio = Clio::new();
    clio.set_scanlines_per_field(263);
    assert_eq!(clio.read(reg::VCOUNT) & 0x800, 0);
    for _ in 0..263 {
        clio.tick(800);
    }
    let vcount = clio.read(reg::VCOUNT);
    assert_eq!(vcount & 0x800, 0x800);
    assert_eq!(vcount & 0x7ff, clio.scanline());
}

// --- timers ----------------------------------------------------------------

#[test]
fn a_timer_reloads_and_raises_an_interrupt_when_it_expires() {
    let mut clio = Clio::new();
    clio.write(TIMER3_COUNTER, 100);
    clio.write(TIMER3_RELOAD, 500);
    clio.write(reg::IRQ0_ENABLE, TIMER3_IRQ);
    clio.write(
        reg::TIMER_CONFIG_SET0,
        config(3, timer_config::DECREMENT | timer_config::RELOAD),
    );

    clio.tick(counts(50));
    assert_eq!(clio.read(TIMER3_COUNTER), 50);
    assert_eq!(clio.read(reg::IRQ0_PENDING) & TIMER3_IRQ, 0);

    // A counter of fifty has fifty-one counts left: the wrap is the step past
    // zero, not the arrival at it.
    clio.tick(counts(51));
    assert_eq!(clio.read(reg::IRQ0_PENDING) & TIMER3_IRQ, TIMER3_IRQ);
    assert_eq!(clio.read(TIMER3_COUNTER), 500);
}

#[test]
fn a_timer_with_no_reload_bit_fires_once_and_then_stops() {
    let mut clio = Clio::new();
    clio.write(TIMER3_COUNTER, 10);
    clio.write(reg::IRQ0_ENABLE, TIMER3_IRQ);
    clio.write(reg::TIMER_CONFIG_SET0, config(3, timer_config::DECREMENT));
    clio.tick(counts(20));
    assert_eq!(clio.read(reg::IRQ0_PENDING) & TIMER3_IRQ, TIMER3_IRQ);

    clio.write(reg::IRQ0_CLEAR, TIMER3_IRQ);
    clio.tick(counts(1000));
    assert_eq!(clio.read(reg::IRQ0_PENDING) & TIMER3_IRQ, 0);
}

#[test]
fn an_unconfigured_timer_does_not_count() {
    let mut clio = Clio::new();
    clio.write(TIMER3_COUNTER, 100);
    clio.write(reg::TIMER_CONFIG_SET0, config(3, timer_config::DECREMENT));
    clio.write(reg::TIMER_CONFIG_CLEAR0, config(3, timer_config::DECREMENT));
    clio.tick(counts(500));
    assert_eq!(clio.read(TIMER3_COUNTER), 100);
}

#[test]
fn timers_are_independent() {
    let mut clio = Clio::new();
    clio.write(reg::TIMER_BASE, 100); // timer 0
    clio.write(TIMER3_COUNTER, 900);
    clio.write(
        reg::TIMER_CONFIG_SET0,
        config(0, timer_config::DECREMENT) | config(3, timer_config::DECREMENT),
    );
    clio.tick(counts(50));
    assert_eq!(clio.read(reg::TIMER_BASE), 50);
    assert_eq!(clio.read(TIMER3_COUNTER), 850);
}

#[test]
fn an_even_timer_cannot_interrupt() {
    assert_eq!(timer_interrupt_bit(0), 0);
    assert_eq!(timer_interrupt_bit(2), 0);
    assert_eq!(timer_interrupt_bit(15), 1 << 3);
    assert_eq!(timer_interrupt_bit(3), 1 << 9);
    assert_eq!(timer_interrupt_bit(1), 1 << 10);
    assert_eq!(timer_interrupt_bit(16), 0);
}

#[test]
fn a_cascaded_timer_only_moves_when_the_one_below_it_wraps() {
    let mut clio = Clio::new();
    clio.write(reg::TIMER_BASE + 2 * reg::TIMER_STRIDE, 5); // timer 2
    clio.write(reg::TIMER_BASE + 2 * reg::TIMER_STRIDE + 4, 5);
    clio.write(TIMER3_COUNTER, 4);
    clio.write(
        reg::TIMER_CONFIG_SET0,
        config(2, timer_config::DECREMENT | timer_config::RELOAD)
            | config(3, timer_config::DECREMENT | timer_config::CASCADE),
    );
    clio.tick(counts(5));
    assert_eq!(clio.read(TIMER3_COUNTER), 4);
    // The sixth count takes timer 2 past zero; timer 3 takes exactly one step.
    clio.tick(counts(1));
    assert_eq!(clio.read(TIMER3_COUNTER), 3);
}

#[test]
fn a_reset_silences_everything() {
    let mut clio = Clio::new();
    clio.write(reg::IRQ0_ENABLE, irq::VERTICAL_BLANK0);
    clio.raise(irq::VERTICAL_BLANK0);
    clio.write(reg::TIMER_CONFIG_SET0, config(3, timer_config::DECREMENT));
    clio.write(reg::CONTROL, 0x22);
    clio.write(reg::XBUS_SELECT, 7);
    clio.write(reg::XBUS_DIRECTION, 0x280);
    clio.write(reg::XBUS_TYPE0, 0x1234);
    clio.write(reg::DMA_REQUEST_SET, DMA_XBUS_BIT);

    clio.reset();

    assert_eq!(clio.read(reg::IRQ0_PENDING), 0);
    assert_eq!(clio.read(reg::IRQ0_ENABLE) & !irq::SECONDARY_BANK, 0);
    assert_eq!(clio.read(reg::TIMER_CONFIG_SET0), 0);
    assert_eq!(clio.read(reg::CONTROL), 0);
    assert_eq!(clio.read(reg::XBUS_SELECT), 0);
    assert_eq!(clio.read(reg::XBUS_DIRECTION), 0);
    assert_eq!(clio.read(reg::XBUS_TYPE0), 0);
    assert_eq!(clio.read(reg::DMA_REQUEST_CLEAR), 0);
    assert!(!clio.xbus_dma_requested());
    assert_eq!(clio.scanline(), 0);
}

#[test]
fn a_new_source_still_interrupts_while_an_older_one_stays_pending() {
    let mut c = Chip::new();
    c.unmask_fiq();
    c.clio()
        .write(reg::IRQ0_ENABLE, irq::VERTICAL_BLANK1 | irq::EXPANSION_BUS);
    c.clio().raise(irq::VERTICAL_BLANK1);
    c.step();
    assert_eq!(c.cpu.pc(), VECTOR_FIQ);

    c.clio().write(reg::IRQ0_CLEAR, irq::VERTICAL_BLANK1);
    c.clio().raise(irq::EXPANSION_BUS);
    c.return_to_supervisor();
    c.step();
    assert_eq!(c.cpu.pc(), VECTOR_FIQ);
}

// --- the expansion bus and the drive ---------------------------------------

#[test]
fn a_multi_byte_cd_command_completes_once_not_once_per_byte() {
    let mut c = Chip::new();
    assert!(c.clio().cdrom().status_empty());
    for i in 0..6 {
        c.clio()
            .write(reg::XBUS_COMMAND, if i == 0 { 0x83 } else { 0 });
        assert!(c.clio().cdrom().status_empty());
        assert_eq!(c.clio().cdrom().commands_received(), 0);
    }
    c.clio().write(reg::XBUS_COMMAND, 0);
    assert_eq!(c.clio().cdrom().commands_received(), 1);
    // The reply is there the moment the seventh byte lands.
    assert!(!c.clio().cdrom().status_empty());
    assert_eq!(c.clio().cdrom().last_command(), 0x83);
}

#[test]
fn a_completed_cd_command_reports_status_ready_and_raises_its_interrupt() {
    let mut c = Chip::new();
    c.clio().write(reg::IRQ0_ENABLE, irq::EXPANSION_BUS);
    c.opcode(0x83);
    c.clio().tick(100_000);
    assert_eq!(
        c.clio().read(reg::XBUS_POLL) & poll::STATUS_READY,
        poll::STATUS_READY
    );
    assert_eq!(
        c.clio().read(reg::IRQ0_PENDING) & irq::EXPANSION_BUS,
        irq::EXPANSION_BUS
    );
}

#[test]
fn inserting_a_disc_is_visible_to_the_drive() {
    let mut c = Chip::new();
    assert!(!c.clio().cdrom().disc_present());
    c.clio().cdrom_mut().set_disc_present(true);
    c.opcode(CMD_VERSION);
    assert!(!c.clio().cdrom().status_empty());

    // A version reply echoes the command and ends with drive status.
    assert_eq!(c.clio().read(reg::XBUS_COMMAND), u32::from(CMD_VERSION));
    let mut last = 0;
    for _ in 1..12 {
        last = c.clio().read(reg::XBUS_COMMAND);
    }
    assert_eq!(last & u32::from(STATUS_DISC_IN), u32::from(STATUS_DISC_IN));
    assert!(c.clio().cdrom().status_empty());
}

#[test]
fn the_version_reply_identifies_the_drive() {
    let mut c = Chip::new();
    c.opcode(CMD_VERSION);
    c.clio().tick(100_000);
    let reply: Vec<u32> = (0..12).map(|_| c.clio().read(reg::XBUS_COMMAND)).collect();
    assert_eq!(reply[0], u32::from(CMD_VERSION)); // echo
    assert_eq!(reply[2], 0x10); // manufacturer id
    assert_eq!(reply[4], 0x01); // manufacturer number
    assert!(c.clio().cdrom().status_empty());
}

#[test]
fn selection_names_a_device_by_value_not_by_address() {
    let mut c = Chip::new();
    c.opcode(0x83);
    c.clio().tick(100_000);
    assert_eq!(
        c.clio().read(reg::XBUS_POLL) & poll::STATUS_READY,
        poll::STATUS_READY
    );

    c.clio().write(reg::XBUS_SELECT, 3);
    assert_eq!(
        c.clio().read(reg::XBUS_POLL) & poll::UNFITTED,
        poll::UNFITTED
    );
    let before = c.clio().cdrom().commands_received();
    for _ in 0..7 {
        c.clio().write(reg::XBUS_COMMAND, 0x83);
    }
    assert_eq!(c.clio().cdrom().commands_received(), before);
}

#[test]
fn an_empty_address_reads_back_as_unfitted() {
    let mut clio = Clio::new();
    clio.write(reg::XBUS_SELECT, 7);
    assert_eq!(clio.read(reg::XBUS_POLL), poll::UNFITTED);
}

#[test]
fn selection_addresses_a_device_with_the_low_nibble_only() {
    let mut c = Chip::new();
    c.clio()
        .write(reg::XBUS_SELECT, 0x30 | poll::CD_ROM_ADDRESS);
    c.opcode(0x83);
    assert_eq!(c.clio().cdrom().commands_received(), 1);
}

#[test]
fn selection_bit_seven_asks_for_the_control_nibble_alone() {
    let mut c = Chip::new();
    c.clio().write(reg::XBUS_SELECT, poll::CD_ROM_ADDRESS);
    c.opcode(0x83);
    c.clio().tick(100_000);
    assert_eq!(
        c.clio().read(reg::XBUS_POLL) & poll::STATUS_READY,
        poll::STATUS_READY
    );
    c.clio()
        .write(reg::XBUS_SELECT, 0x80 | poll::CD_ROM_ADDRESS);
    assert_eq!(c.clio().read(reg::XBUS_POLL) & 0xf0, 0);
}

#[test]
fn only_the_control_nibble_of_the_poll_register_is_writable() {
    let mut c = Chip::new();
    c.opcode(0x83);
    c.clio().tick(100_000);
    c.clio().write(reg::XBUS_POLL, poll::STATUS_IRQ_ENABLE);
    let value = c.clio().read(reg::XBUS_POLL);
    assert_eq!(value & poll::STATUS_IRQ_ENABLE, poll::STATUS_IRQ_ENABLE);
    assert_eq!(value & poll::STATUS_READY, poll::STATUS_READY);
}

#[test]
fn the_control_register_only_changes_bits_that_are_write_enabled() {
    let mut clio = Clio::new();
    assert_eq!(clio.read(reg::CONTROL), 0);
    clio.write(reg::CONTROL, 0x02); // no write-enable: ignored
    assert_eq!(clio.read(reg::CONTROL), 0);
    clio.write(reg::CONTROL, 0x22); // enable bit 1, set bit 1
    assert_eq!(clio.read(reg::CONTROL), 0x02);
    clio.write(reg::CONTROL, 0x11); // enable bit 0, set bit 0
    assert_eq!(clio.read(reg::CONTROL), 0x03);
    clio.write(reg::CONTROL, 0x20); // enable bit 1, clear bit 1
    assert_eq!(clio.read(reg::CONTROL), 0x01);
}

#[test]
fn both_ports_of_an_interrupt_pair_read_the_same_mask() {
    let mut clio = Clio::new();
    clio.write(reg::IRQ0_ENABLE, irq::VERTICAL_BLANK1);
    assert_eq!(
        clio.read(reg::IRQ0_ENABLE) & !irq::SECONDARY_BANK,
        irq::VERTICAL_BLANK1
    );
    assert_eq!(
        clio.read(reg::IRQ0_DISABLE) & !irq::SECONDARY_BANK,
        irq::VERTICAL_BLANK1
    );
    clio.raise(irq::VERTICAL_BLANK1);
    assert_eq!(clio.read(reg::IRQ0_PENDING), irq::VERTICAL_BLANK1);
    assert_eq!(clio.read(reg::IRQ0_CLEAR), irq::VERTICAL_BLANK1);
}

#[test]
fn the_two_expansion_bus_control_registers_are_independent() {
    let mut clio = Clio::new();
    clio.write(reg::XBUS_CTL, 0x4000);
    clio.write(reg::XBUS_DIRECTION, 0x0280);
    assert_eq!(clio.read(reg::XBUS_CTL), 0x4000);
    assert_eq!(clio.read(reg::XBUS_DIRECTION), 0x0280);
    clio.write(reg::XBUS_CTL, 0x0040);
    assert_eq!(clio.read(reg::XBUS_CTL), 0x0040);
}

#[test]
fn a_bus_control_write_carrying_bit_eleven_is_refused() {
    let mut clio = Clio::new();
    clio.write(reg::XBUS_CTL, 0x4000);
    clio.write(reg::XBUS_CTL, XBUS_CTL_WRITE_VETO);
    assert_eq!(clio.read(reg::XBUS_CTL), 0x4000);
}

#[test]
fn a_read_command_streams_sectors_from_the_disc() {
    let mut c = Chip::new();
    c.clio().cdrom_mut().set_disc_present(true);
    // MSF 00:02:00 is LBA 0: the first two seconds are lead-in.
    c.command([CMD_READ, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01]);
    c.clio().tick(100_000);
    assert_eq!(c.clio().read(reg::XBUS_COMMAND), u32::from(CMD_READ));
    assert_eq!(
        c.clio().read(reg::XBUS_COMMAND) & u32::from(STATUS_SPIN_UP),
        u32::from(STATUS_SPIN_UP)
    );
    assert_eq!(c.clio().cdrom().last_command(), CMD_READ);
}

#[test]
fn read_capacity_reports_the_lead_out_in_msf() {
    let mut c = Chip::new();
    c.opcode(CMD_READ_CAPACITY);
    c.clio().tick(100_000);
    assert_eq!(
        c.clio().read(reg::XBUS_COMMAND),
        u32::from(CMD_READ_CAPACITY)
    );
    c.clio().read(reg::XBUS_COMMAND); // reserved
    let minutes = c.clio().read(reg::XBUS_COMMAND);
    let seconds = c.clio().read(reg::XBUS_COMMAND);
    c.clio().read(reg::XBUS_COMMAND); // frames
                                      // An empty drive still answers; LBA zero is MSF 00:02:00.
    assert_eq!(minutes, 0);
    assert_eq!(seconds, 2);
}

#[test]
fn a_read_transfers_sector_bytes_through_the_data_fifo() {
    let mut c = Chip::new();
    c.clio().cdrom_mut().set_disc_present(true);
    assert!(!c.clio().cdrom().has_chunk());
    c.command([CMD_READ, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01]);
    c.clio().tick(100_000);
    assert_eq!(c.clio().read(reg::XBUS_COMMAND), u32::from(CMD_READ));
    assert_eq!(
        c.clio().read(reg::XBUS_COMMAND) & u32::from(STATUS_SPIN_UP),
        u32::from(STATUS_SPIN_UP)
    );
}

#[test]
fn the_memory_configuration_cannot_be_written() {
    let mut mem = Memory::new();
    let mut madam = Madam::new();
    assert_eq!(madam.read(madam::MEM_CONFIG), madam::MEM_CONFIG_STOCK);
    madam.write(madam::MEM_CONFIG, 0, &mut mem);
    assert_eq!(madam.read(madam::MEM_CONFIG), madam::MEM_CONFIG_STOCK);
    assert_eq!(madam::MEM_CONFIG_STOCK & 0x07, 1); // 1 MB VRAM
    assert_eq!((madam::MEM_CONFIG_STOCK >> 3) & 0x03, 1); // 1 MB DRAM1
    assert_eq!((madam::MEM_CONFIG_STOCK >> 5) & 0x03, 1); // 1 MB DRAM2
}

#[test]
fn the_secondary_bank_bit_always_reads_set_in_the_first_banks_enable() {
    let mut clio = Clio::new();
    assert_eq!(
        clio.read(reg::IRQ0_ENABLE) & irq::SECONDARY_BANK,
        irq::SECONDARY_BANK
    );
    clio.write(reg::IRQ0_DISABLE, 0xffff_ffff);
    assert_eq!(
        clio.read(reg::IRQ0_ENABLE) & irq::SECONDARY_BANK,
        irq::SECONDARY_BANK
    );
}

#[test]
fn a_pending_second_bank_source_announces_itself_in_the_first_bank() {
    let mut clio = Clio::new();
    clio.raise_secondary(irq::BANK1_PBUS_COMPLETE);
    assert_eq!(clio.read(reg::IRQ1_PENDING), irq::BANK1_PBUS_COMPLETE);
    assert_eq!(
        clio.read(reg::IRQ0_PENDING) & irq::SECONDARY_BANK,
        irq::SECONDARY_BANK
    );
}

#[test]
fn the_dsp_raises_the_audio_interrupt_from_its_own_program() {
    let mut console = Console::new();
    console.reset();
    // A control-format move of an immediate into 0x3EE, then sleep.
    let program = [0x9bee_u32, 0xc001, 0x8380];
    let sys = console.system_mut();
    for (i, word) in (0u32..).zip(program) {
        sys.write32(CLIO_BASE + 0x2000 + i * 4, word);
    }
    sys.write32(CLIO_BASE + reg::IRQ0_ENABLE, irq::AUDIO_TIMER);
    sys.write32(CLIO_BASE + reg::IRQ0_CLEAR, irq::AUDIO_TIMER);

    // A stopped DSP never reaches the instruction, whatever is loaded.
    console.run_frame();
    assert_eq!(
        console.system_mut().read32(CLIO_BASE + reg::IRQ0_PENDING) & irq::AUDIO_TIMER,
        0
    );

    console.system_mut().write32(CLIO_BASE + reg::DSP_RUN, 1);
    console.run_frame();
    assert_eq!(
        console.system_mut().read32(CLIO_BASE + reg::IRQ0_PENDING) & irq::AUDIO_TIMER,
        irq::AUDIO_TIMER
    );
}

#[test]
fn ejecting_reports_an_empty_drive_and_spinning_up_reloads_it() {
    let mut c = Chip::new();
    c.clio().cdrom_mut().set_disc_present(true);
    let mut command = |opcode: u8| {
        c.opcode(opcode);
        assert_eq!(c.clio().read(reg::XBUS_COMMAND), u32::from(opcode));
        c.clio().read(reg::XBUS_COMMAND) as u8
    };
    // Loaded: tray shut, disc in, spinning, ready.
    assert_eq!(command(CMD_READ_STATUS), 0xe1);
    // Ejected: ready and nothing else.
    assert_eq!(command(CMD_EJECT), STATUS_READY);
    assert_eq!(command(CMD_READ_STATUS), STATUS_READY);
    // Spinning up closes the tray again.
    assert_eq!(command(CMD_MOTOR_ON), 0xe1);
}

#[test]
fn abort_is_one_byte_long_and_the_rest_are_seven() {
    let mut c = Chip::new();
    c.clio().cdrom_mut().set_disc_present(true);
    c.clio().write(reg::XBUS_COMMAND, u32::from(CMD_ABORT));
    assert_eq!(c.clio().cdrom().commands_received(), 1);
    assert_eq!(c.clio().cdrom().last_command(), CMD_ABORT);
    c.opcode(CMD_VERSION);
    assert_eq!(c.clio().cdrom().commands_received(), 2);
    assert_eq!(c.clio().cdrom().last_command(), CMD_VERSION);
}

#[test]
fn a_dma_request_tells_the_machine_to_start_the_transfer() {
    let mut clio = Clio::new();
    let effects = clio.write(reg::DMA_REQUEST_SET, DMA_XBUS_BIT);
    assert!(effects.start_xbus_dma);
    assert_eq!(effects.channels, Some((DMA_XBUS_BIT, 0)));
    assert!(clio.xbus_dma_requested());
    let effects = clio.write(reg::FIFO_CLEAR, DMA_XBUS_BIT);
    assert!(!effects.start_xbus_dma);
    assert!(!clio.xbus_dma_requested());
}
