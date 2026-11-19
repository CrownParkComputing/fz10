//! ARM60 instruction tests, ported from the C++ reference's tests/test_arm60.cpp.
//!
//! Each test assembles a short program as raw words, loads it where the reset
//! vector points, and single-steps it. Encodings are written out longhand so
//! that a failure points at a specific bit field rather than at an assembler.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::panic
)]
// Encodings are written field by field, zero fields included, as in the reference.
#![allow(clippy::identity_op, clippy::erasing_op)]

use retro3do::arm60::{
    Arm60, Mode, FLAG_C, FLAG_F, FLAG_I, FLAG_N, FLAG_V, FLAG_Z, MODE_MASK, VECTOR_IRQ, VECTOR_SWI,
};
use retro3do::memory::{
    Memory, WriteWatch, NVRAM_BASE, NVRAM_MIRROR, NVRAM_SIZE, NVRAM_WINDOW, ROM_BASE, ROM_SIZE,
    VRAM_BASE,
};

/// A machine with a program in ROM, ready to step from the reset vector.
struct Machine {
    bus: Memory,
    cpu: Arm60,
}

impl Machine {
    fn new(program: &[u32]) -> Self {
        let rom: Vec<u8> = program.iter().flat_map(|word| word.to_be_bytes()).collect();
        let mut bus = Memory::new();
        bus.load_rom(&rom).unwrap();
        let mut cpu = Arm60::new();
        cpu.reset();
        Machine { bus, cpu }
    }

    fn step(&mut self) {
        self.steps(1);
    }

    fn steps(&mut self, count: usize) {
        for _ in 0..count {
            self.cpu.step(&mut self.bus);
        }
    }
}

const ALWAYS: u32 = 0xE << 28;

// ---------------------------------------------------------------------------
// Data processing
// ---------------------------------------------------------------------------

#[test]
fn mov_immediate_loads_a_register() {
    // MOV r0, #0x42
    let mut m = Machine::new(&[ALWAYS | (0x3A << 20) | (0 << 12) | 0x42]);
    m.step();
    assert_eq!(m.cpu.reg(0), 0x42);
}

#[test]
fn mov_immediate_rotates_the_operand() {
    // MOV r1, #0xFF000000  -- 0xFF rotated right by 8
    let mut m = Machine::new(&[ALWAYS | (0x3A << 20) | (1 << 12) | (4 << 8) | 0xFF]);
    m.step();
    assert_eq!(m.cpu.reg(1), 0xFF00_0000);
}

#[test]
fn add_computes_and_sets_flags() {
    let mut m = Machine::new(&[
        ALWAYS | (0x3A << 20) | (0 << 12) | 0xFF, // MOV r0, #0xFF
        ALWAYS | (0x29 << 20) | (0 << 16) | (1 << 12) | 1, // ADDS r1, r0, #1
    ]);
    m.steps(2);
    assert_eq!(m.cpu.reg(1), 0x100);
    assert!(m.cpu.cpsr() & FLAG_Z == 0);
    assert!(m.cpu.cpsr() & FLAG_N == 0);
}

#[test]
fn subs_to_zero_sets_the_zero_flag() {
    let mut m = Machine::new(&[
        ALWAYS | (0x3A << 20) | (0 << 12) | 7,             // MOV r0, #7
        ALWAYS | (0x25 << 20) | (0 << 16) | (1 << 12) | 7, // SUBS r1, r0, #7
    ]);
    m.steps(2);
    assert_eq!(m.cpu.reg(1), 0);
    assert!(m.cpu.cpsr() & FLAG_Z != 0);
    // A subtraction that does not borrow sets carry, ARM's convention.
    assert!(m.cpu.cpsr() & FLAG_C != 0);
}

#[test]
fn subs_that_borrows_clears_carry_and_sets_negative() {
    let mut m = Machine::new(&[
        ALWAYS | (0x3A << 20) | (0 << 12) | 1,             // MOV r0, #1
        ALWAYS | (0x25 << 20) | (0 << 16) | (1 << 12) | 2, // SUBS r1, r0, #2
    ]);
    m.steps(2);
    assert_eq!(m.cpu.reg(1), 0xFFFF_FFFF);
    assert!(m.cpu.cpsr() & FLAG_N != 0);
    assert!(m.cpu.cpsr() & FLAG_C == 0);
}

#[test]
fn overflow_is_signed_not_unsigned() {
    // 0x7FFFFFFF + 1 overflows as a signed value but does not carry. Getting
    // these two confused is the classic ARM flag bug, so they are asserted
    // together.
    let mut m = Machine::new(&[ALWAYS | (0x29 << 20) | (0 << 16) | (1 << 12) | 1]); // ADDS r1,r0,#1
    m.cpu.set_reg(0, 0x7FFF_FFFF);
    m.step();
    assert_eq!(m.cpu.reg(1), 0x8000_0000);
    assert!(m.cpu.cpsr() & FLAG_V != 0);
    assert!(m.cpu.cpsr() & FLAG_C == 0);
    assert!(m.cpu.cpsr() & FLAG_N != 0);
}

#[test]
fn unsigned_carry_without_signed_overflow() {
    // 0xFFFFFFFF + 1 carries but does not overflow - the mirror of the above.
    let mut m = Machine::new(&[ALWAYS | (0x29 << 20) | (0 << 16) | (1 << 12) | 1]); // ADDS r1,r0,#1
    m.cpu.set_reg(0, 0xFFFF_FFFF);
    m.step();
    assert_eq!(m.cpu.reg(1), 0);
    assert!(m.cpu.cpsr() & FLAG_C != 0);
    assert!(m.cpu.cpsr() & FLAG_V == 0);
    assert!(m.cpu.cpsr() & FLAG_Z != 0);
}

#[test]
fn cmp_does_not_write_its_destination() {
    let mut m = Machine::new(&[
        ALWAYS | (0x35 << 20) | (0 << 16) | (1 << 12), // CMP r0, #0
    ]);
    m.cpu.set_reg(1, 0xABCD);
    m.step();
    assert_eq!(m.cpu.reg(1), 0xABCD);
    assert!(m.cpu.cpsr() & FLAG_Z != 0);
}

// ---------------------------------------------------------------------------
// Barrel shifter
// ---------------------------------------------------------------------------

#[test]
fn logical_shift_left_by_register() {
    // MOV r2, r0, LSL r1
    let mut m = Machine::new(&[ALWAYS | (0x1A << 20) | (2 << 12) | (1 << 8) | (0 << 5) | (1 << 4)]);
    m.cpu.set_reg(0, 0x1);
    m.cpu.set_reg(1, 4);
    m.step();
    assert_eq!(m.cpu.reg(2), 0x10);
}

#[test]
fn arithmetic_shift_right_preserves_sign() {
    // MOV r2, r0, ASR #4
    let mut m = Machine::new(&[ALWAYS | (0x1A << 20) | (2 << 12) | (4 << 7) | (2 << 5)]);
    m.cpu.set_reg(0, 0xFFFF_FF00);
    m.step();
    assert_eq!(m.cpu.reg(2), 0xFFFF_FFF0);
}

#[test]
fn immediate_lsr_zero_means_shift_by_thirty_two() {
    // MOV r2, r0, LSR #0  -- encodes LSR #32, so the result is zero
    let mut m = Machine::new(&[ALWAYS | (0x1A << 20) | (2 << 12) | (0 << 7) | (1 << 5)]);
    m.cpu.set_reg(0, 0x8000_0000);
    m.step();
    assert_eq!(m.cpu.reg(2), 0);
}

// ---------------------------------------------------------------------------
// Branches
// ---------------------------------------------------------------------------

#[test]
fn branch_jumps_relative_to_the_pipeline() {
    // A branch is relative to PC, which reads eight ahead of the branch itself.
    // So an offset of zero already skips the next instruction - the pipeline
    // offset is the thing this test exists to pin down.
    let mut m = Machine::new(&[
        ALWAYS | (0xA << 24),                     // B  +0
        ALWAYS | (0x3A << 20) | (0 << 12) | 0xAA, // MOV r0,#0xAA (skipped)
        ALWAYS | (0x3A << 20) | (0 << 12) | 0xBB, // MOV r0,#0xBB
    ]);
    m.step();
    assert_eq!(m.cpu.pc(), ROM_BASE + 8);
    m.step();
    assert_eq!(m.cpu.reg(0), 0xBB);
}

#[test]
fn branch_with_link_records_the_return_address() {
    let mut m = Machine::new(&[ALWAYS | (0xB << 24) | 4]); // BL forward
    m.step();
    assert_eq!(m.cpu.reg(14), ROM_BASE + 4);
}

#[test]
fn a_failed_condition_executes_nothing() {
    // MOVEQ r0, #1 with Z clear must not write r0.
    let mut m = Machine::new(&[(0x0 << 28) | (0x3A << 20) | (0 << 12) | 1]);
    m.step();
    assert_eq!(m.cpu.reg(0), 0);
}

// ---------------------------------------------------------------------------
// Loads and stores
// ---------------------------------------------------------------------------

#[test]
fn store_then_load_a_word_through_dram() {
    let mut m = Machine::new(&[
        ALWAYS | (0x58 << 20) | (1 << 16) | (0 << 12), // STR r0,[r1]
        ALWAYS | (0x59 << 20) | (1 << 16) | (2 << 12), // LDR r2,[r1]
    ]);
    m.cpu.set_reg(0, 0xDEAD_BEEF);
    m.cpu.set_reg(1, 0x1000);
    m.steps(2);
    assert_eq!(m.cpu.reg(2), 0xDEAD_BEEF);
    assert_eq!(m.bus.read32(0x1000), 0xDEAD_BEEF);
}

#[test]
fn byte_store_touches_only_one_byte() {
    let mut m = Machine::new(&[
        ALWAYS | (0x5C << 20) | (1 << 16) | (0 << 12), // STRB r0,[r1]
    ]);
    m.bus.write32(0x2000, 0x1122_3344);
    m.cpu.set_reg(0, 0xFF);
    m.cpu.set_reg(1, 0x2000);
    m.step();
    // Big-endian: the byte at address 0x2000 is the most significant one.
    assert_eq!(m.bus.read32(0x2000), 0xFF22_3344);
}

#[test]
fn post_indexed_load_writes_the_base_back() {
    // LDR r2, [r1], #4
    let mut m = Machine::new(&[ALWAYS | (0x49 << 20) | (1 << 16) | (2 << 12) | 4]);
    m.bus.write32(0x3000, 0x1234_5678);
    m.cpu.set_reg(1, 0x3000);
    m.step();
    assert_eq!(m.cpu.reg(2), 0x1234_5678);
    assert_eq!(m.cpu.reg(1), 0x3004);
}

#[test]
fn block_store_and_load_round_trip() {
    // STMIA r4!, {r0-r2} then LDMIA r5!, {r6-r8}
    let stm = ALWAYS | (0x8A << 20) | (4 << 16) | 0x0007;
    let ldm = ALWAYS | (0x8B << 20) | (5 << 16) | 0x01C0;
    let mut m = Machine::new(&[stm, ldm]);
    m.cpu.set_reg(0, 0x1111);
    m.cpu.set_reg(1, 0x2222);
    m.cpu.set_reg(2, 0x3333);
    m.cpu.set_reg(4, 0x4000);
    m.cpu.set_reg(5, 0x4000);
    m.steps(2);
    assert_eq!(m.cpu.reg(6), 0x1111);
    assert_eq!(m.cpu.reg(7), 0x2222);
    assert_eq!(m.cpu.reg(8), 0x3333);
    assert_eq!(m.cpu.reg(4), 0x400C);
    assert_eq!(m.cpu.reg(5), 0x400C);
}

// ---------------------------------------------------------------------------
// Multiply
// ---------------------------------------------------------------------------

#[test]
fn multiply_produces_the_low_word() {
    // MUL r3, r0, r1   (rd=3, rs=1, rm=0)
    let mut m = Machine::new(&[ALWAYS | (0x00 << 20) | (3 << 16) | (1 << 8) | (9 << 4)]);
    m.cpu.set_reg(0, 7);
    m.cpu.set_reg(1, 6);
    m.step();
    assert_eq!(m.cpu.reg(3), 42);
}

// ---------------------------------------------------------------------------
// Status registers and exceptions
// ---------------------------------------------------------------------------

#[test]
fn reset_enters_supervisor_mode_with_interrupts_masked() {
    let m = Machine::new(&[ALWAYS | (0x3A << 20)]);
    assert!(m.cpu.mode() == Mode::Supervisor);
    assert!(m.cpu.cpsr() & FLAG_I != 0);
    assert!(m.cpu.cpsr() & FLAG_F != 0);
    assert_eq!(m.cpu.pc(), ROM_BASE);
}

#[test]
fn mrs_reads_the_status_register() {
    // MRS r0, CPSR
    let mut m = Machine::new(&[ALWAYS | 0x010F_0000]);
    m.step();
    assert_eq!(m.cpu.reg(0), m.cpu.cpsr());
}

#[test]
fn software_interrupt_enters_supervisor_mode_at_the_vector() {
    let mut m = Machine::new(&[ALWAYS | (0xF << 24) | 0x12_3456]);
    m.step();
    assert_eq!(m.cpu.pc(), VECTOR_SWI);
    assert!(m.cpu.mode() == Mode::Supervisor);
    assert_eq!(m.cpu.reg(14), ROM_BASE + 4);
}

#[test]
fn an_unmasked_irq_is_taken_at_an_instruction_boundary() {
    let mut m = Machine::new(&[
        ALWAYS | (0x3A << 20) | (0 << 12) | 1, // MOV r0,#1
        ALWAYS | (0x3A << 20) | (0 << 12) | 2, // MOV r0,#2
    ]);
    // Clear the I bit so the line can be seen.
    m.cpu.set_cpsr(m.cpu.cpsr() & !FLAG_I);
    m.cpu.set_irq(true);
    m.step();
    assert_eq!(m.cpu.reg(0), 1);
    assert_eq!(m.cpu.pc(), VECTOR_IRQ);
    assert!(m.cpu.mode() == Mode::Irq);
}

#[test]
fn a_masked_irq_is_ignored() {
    let mut m = Machine::new(&[ALWAYS | (0x3A << 20) | (0 << 12) | 1]);
    m.cpu.set_irq(true); // I is still set from reset
    m.step();
    assert_eq!(m.cpu.pc(), ROM_BASE + 4);
    assert!(m.cpu.mode() == Mode::Supervisor);
}

#[test]
fn banked_stack_pointers_survive_a_mode_change() {
    let mut m = Machine::new(&[ALWAYS | (0x3A << 20)]);
    m.cpu.set_reg(13, 0xAAAA_0000); // r13_svc
    m.cpu
        .set_cpsr((m.cpu.cpsr() & !MODE_MASK) | Mode::Irq as u32);
    m.cpu.set_reg(13, 0xBBBB_0000); // r13_irq
    assert_eq!(m.cpu.reg(13), 0xBBBB_0000);
    m.cpu
        .set_cpsr((m.cpu.cpsr() & !MODE_MASK) | Mode::Supervisor as u32);
    assert_eq!(m.cpu.reg(13), 0xAAAA_0000);
}

// ---------------------------------------------------------------------------
// The decode cache
// ---------------------------------------------------------------------------

#[test]
fn rewritten_code_is_not_executed_from_a_stale_decode() {
    // The point of the cache is that an instruction is decoded once. This test
    // exists to prove that overwriting code and invalidating still re-decodes,
    // because getting that wrong produces bugs that only appear after a game
    // has been running for a while.
    let mut bus = Memory::new();
    let mut cpu = Arm60::new();

    // Put MOV r0,#1 in DRAM, run it, then rewrite it to MOV r0,#2.
    let mov_one = ALWAYS | (0x3A << 20) | (0 << 12) | 1;
    let mov_two = ALWAYS | (0x3A << 20) | (0 << 12) | 2;

    bus.write32(0x8000, mov_one);
    cpu.set_reg(15, 0x8000);
    cpu.step(&mut bus);
    assert_eq!(cpu.reg(0), 1);

    bus.write32(0x8000, mov_two);
    cpu.invalidate_decode_range(0x8000, 4);
    cpu.set_reg(15, 0x8000);
    cpu.step(&mut bus);
    assert_eq!(cpu.reg(0), 2);
}

#[test]
fn the_bus_reports_writes_that_could_land_on_code() {
    let mut bus = Memory::new();
    bus.write32(0x9000, 0x1234_5678);
    assert!(bus.write_watch().is_dirty());
    assert!(bus
        .write_watch()
        .page_dirty(0x9000 / WriteWatch::PAGE_BYTES));

    bus.write_watch_mut().clear();
    assert!(!bus.write_watch().is_dirty());

    // A write to VRAM is not code and must not force an invalidation.
    bus.write32(VRAM_BASE + 0x100, 0);
    assert!(!bus.write_watch().is_dirty());
}

#[test]
fn distant_writes_do_not_dirty_everything_between_them() {
    // Recorded as a range, two writes at opposite ends of memory would make the
    // whole span dirty and invalidation would cost as much as the machine has
    // memory rather than as much as was written.
    let mut bus = Memory::new();
    bus.write32(0x1000, 1);
    bus.write32(0x7f000, 1);
    assert!(bus
        .write_watch()
        .page_dirty(0x1000 / WriteWatch::PAGE_BYTES));
    assert!(bus
        .write_watch()
        .page_dirty(0x7f000 / WriteWatch::PAGE_BYTES));
    // and nothing in between
    assert!(!bus
        .write_watch()
        .page_dirty(0x40000 / WriteWatch::PAGE_BYTES));
}

#[test]
fn nvram_stores_one_byte_per_word() {
    // The ARM60 has no bus address translator, so the NVRAM's data lines sit on
    // the low byte and each stored byte is addressed as a whole 32-bit word:
    // byte n lives at base + n*4, and 32 KiB of storage occupies 128 KiB of
    // address space.
    let mut bus = Memory::new();
    bus.write32(NVRAM_BASE, 0xAA);
    bus.write32(NVRAM_BASE + 4, 0xBB);

    assert_eq!(bus.read32(NVRAM_BASE), 0xAA);
    assert_eq!(bus.read32(NVRAM_BASE + 4), 0xBB);

    // The window is four times the storage, and the last word is still inside.
    assert_eq!(NVRAM_WINDOW, NVRAM_SIZE * 4);
    bus.write32(NVRAM_BASE + NVRAM_WINDOW - 4, 0xCC);
    assert_eq!(bus.read32(NVRAM_BASE + NVRAM_WINDOW - 4), 0xCC);
}

#[test]
fn low_memory_reads_rom_until_the_first_write_to_it() {
    // The machine comes up with ROM overlaid on the low memory: the ROM shadows
    // itself down there, writes to it, and carries on from the DRAM underneath.
    // It is the WRITE that removes the overlay, not any control register.
    let mut bus = Memory::new();
    let mut rom = vec![0u8; ROM_SIZE as usize];
    rom[..4].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    bus.load_rom(&rom).unwrap();

    assert!(bus.rom_overlay());
    assert_eq!(bus.read32(0), 0xDEAD_BEEF);

    // Writing anywhere in the low range reveals DRAM from then on.
    bus.write32(0x1000, 0x1234_5678);
    assert!(!bus.rom_overlay());
    assert_eq!(bus.read32(0x1000), 0x1234_5678);
    assert_eq!(bus.read32(0), 0); // DRAM, not the ROM's first word
}

#[test]
fn nvram_answers_at_its_mirror_too() {
    // A second copy sits 0x20000 higher.
    let mut bus = Memory::new();
    bus.write32(NVRAM_BASE + 8, 0x5A);
    assert_eq!(bus.read32(NVRAM_BASE + 8), 0x5A);
    assert_eq!(bus.read32(NVRAM_BASE + NVRAM_MIRROR + 8), 0x5A);
}

// ---------------------------------------------------------------------------
// Instruction timing
// ---------------------------------------------------------------------------

#[test]
fn a_multiply_costs_by_the_bits_of_its_multiplier_not_its_bytes() {
    // The ARM60 is an ARM6: a 2-bit Booth array that terminates on the
    // significant BIT count. Nearly every cycle table in circulation is the
    // ARM7's, which terminates on the significant BYTE count and so gives an
    // answer up to four times too cheap.
    //
    // Multiplier in Rs. Build it directly so the operand is exact.
    let cost_for = |rs_value: u32| -> u32 {
        let mut m = Machine::new(&[
            ALWAYS | 0x3A0_1001, // mov r1, #1      (multiplicand)
            ALWAYS | 0x000_0291, // mul r0, r1, r2  (Rs = r2)
        ]);
        m.step();
        m.cpu.set_reg(2, rs_value);
        let before = m.cpu.total_cycles();
        m.step();
        (m.cpu.total_cycles() - before) as u32
    };

    // ARM's published ranges: 0/1 take 1I; for n > 1, values from
    // 2^(2n-3) through 2^(2n-1)-1 take n I-cycles.
    assert_eq!(cost_for(0), 1 + 1);
    assert_eq!(cost_for(1), 1 + 1);
    assert_eq!(cost_for(2), 1 + 2);
    assert_eq!(cost_for(7), 1 + 2);
    assert_eq!(cost_for(8), 1 + 3);
    assert_eq!(cost_for(31), 1 + 3);
    assert_eq!(cost_for(32), 1 + 4);
    assert_eq!(cost_for(0xff), 1 + 5);
    assert_eq!(cost_for(0xffff), 1 + 9);
    // A full 32-bit multiplier saturates the array. The ARM7 table would have
    // charged four here; the ARM6 charges sixteen.
    assert_eq!(cost_for(0x8000_0000), 1 + 16);
    assert_eq!(cost_for(0xffff_ffff), 1 + 16);
}

#[test]
fn taking_an_exception_costs_what_a_taken_branch_costs() {
    // SWI, an undefined instruction and an absent coprocessor are the same
    // event as far as the pipeline is concerned: what was fetched is thrown
    // away and refetched from the vector. That is 2S+1N.
    let mut m = Machine::new(&[ALWAYS | 0x0F00_0000]); // swi #0
    let before = m.cpu.total_cycles();
    m.step();
    assert_eq!((m.cpu.total_cycles() - before) as u32, 2 * 1 + 4);
}

// ---------------------------------------------------------------------------
// Rust-side additions: the bus-driven interface that replaced the C++ Bus&.
// ---------------------------------------------------------------------------

/// Memory plus a FIQ line and a count of instruction boundaries.
struct LineBus {
    memory: Memory,
    fiq: bool,
    boundaries: u32,
}

impl retro3do::bus::CpuBus for LineBus {
    fn read32(&mut self, address: u32) -> u32 {
        self.memory.read32(address)
    }
    fn read8(&mut self, address: u32) -> u8 {
        self.memory.read8(address)
    }
    fn write32(&mut self, address: u32, value: u32) {
        self.memory.write32(address, value);
    }
    fn write8(&mut self, address: u32, value: u8) {
        self.memory.write8(address, value);
    }
    fn fetch32(&mut self, address: u32) -> u32 {
        self.memory.fetch32(address)
    }
    fn instruction_boundary(&mut self) {
        self.boundaries += 1;
    }
    fn fiq_line(&self) -> bool {
        self.fiq
    }
}

#[test]
fn the_bus_fiq_line_is_taken_before_a_held_irq() {
    let m = Machine::new(&[ALWAYS | (0x3A << 20) | 1]); // MOV r0,#1
    let mut bus = LineBus {
        memory: m.bus,
        fiq: true,
        boundaries: 0,
    };
    let mut cpu = m.cpu;
    cpu.set_cpsr(cpu.cpsr() & !(FLAG_I | FLAG_F));
    cpu.set_irq(true);
    cpu.step(&mut bus);
    assert_eq!(bus.boundaries, 1);
    assert!(cpu.mode() == Mode::Fiq);
    assert_eq!(cpu.pc(), retro3do::arm60::VECTOR_FIQ);
    // R14 is the next instruction plus the pipeline's four.
    assert_eq!(cpu.reg(14), ROM_BASE + 8);
    assert!(cpu.cpsr() & FLAG_I != 0 && cpu.cpsr() & FLAG_F != 0);
    assert_eq!(cpu.fiqs_taken(), 1);
    assert_eq!(cpu.irqs_taken(), 0);
}

#[test]
fn a_latched_irq_is_consumed_when_taken() {
    let mut m = Machine::new(&[ALWAYS | (0x3A << 20) | 1]); // MOV r0,#1
    m.cpu.set_cpsr(m.cpu.cpsr() & !FLAG_I);
    m.cpu.signal_irq();
    m.step();
    assert!(m.cpu.mode() == Mode::Irq);
    assert_eq!(m.cpu.spsr() & MODE_MASK, Mode::Supervisor as u32);

    // Back to supervisor mode with IRQs enabled: nothing is pending any more.
    m.cpu.set_cpsr(Mode::Supervisor as u32);
    m.cpu.set_reg(15, ROM_BASE);
    m.step();
    assert!(m.cpu.mode() == Mode::Supervisor);
    assert_eq!(m.cpu.pc(), ROM_BASE + 4);
    assert_eq!(m.cpu.irqs_taken(), 1);
}

#[test]
fn a_loop_is_decoded_once_until_invalidated() {
    let mut m = Machine::new(&[
        ALWAYS | 0x0280_0001, // ADD r0, r0, #1
        ALWAYS | 0x0AFF_FFFD, // B back to the ADD
    ]);
    let spent = m.cpu.run(&mut m.bus, 100);
    assert!(spent >= 100);
    assert_eq!(m.cpu.total_cycles(), u64::from(spent));
    assert!(m.cpu.reg(0) > 10);
    assert_eq!(m.cpu.decodes(), 2);

    m.cpu.invalidate_decode_range(ROM_BASE, 8);
    assert_eq!(m.cpu.invalidations(), 1);
    m.step();
    assert_eq!(m.cpu.decodes(), 3);
}
