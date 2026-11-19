//! SPORT tests, ported from the C++ reference's tests/test_sport.cpp. Everything
//! asserted here was derived from the boot ROM's own memory test.
//!
//! The C++ rig attached SPORT to its `Bus`; here a small rig routes the SPORT
//! window to [`Sport`] and everything else to [`Memory`], as the system
//! dispatcher does.
//!
//! Not ported: `the_field_flag_sits_above_the_line_number` (it tests CLIO's
//! line register, not SPORT). `a_copy_into_memory_marks_it_as_possibly_code`
//! keeps its one check, but against this rig rather than a whole console.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::panic
)]

use retro3do::memory::{in_window, Memory, SPORT_BASE, SPORT_SIZE, VRAM_BASE};
use retro3do::sport::{self, Sport, ADDRESS_SHIFT, FILL_WINDOW, PAGE_BYTES, VALUE_REGISTER};

struct Rig {
    mem: Memory,
    sport: Sport,
}

impl Rig {
    fn new() -> Self {
        Self {
            mem: Memory::new(),
            sport: Sport::new(),
        }
    }

    fn read32(&mut self, address: u32) -> u32 {
        if in_window(address, SPORT_BASE, SPORT_SIZE) {
            self.sport.read(address - SPORT_BASE, &mut self.mem)
        } else {
            self.mem.read32(address)
        }
    }

    fn write32(&mut self, address: u32, value: u32) {
        if in_window(address, SPORT_BASE, SPORT_SIZE) {
            self.sport.write(address - SPORT_BASE, value, &mut self.mem);
        } else {
            self.mem.write32(address, value);
        }
    }

    /// The address the machine would use to name a page.
    fn copy_addr(vram_offset: u32) -> u32 {
        SPORT_BASE + (vram_offset >> ADDRESS_SHIFT)
    }
    fn fill_addr(vram_offset: u32) -> u32 {
        SPORT_BASE + FILL_WINDOW + (vram_offset >> ADDRESS_SHIFT)
    }
}

#[test]
fn a_page_is_two_kilobytes() {
    // Fixed by the ROM: after a copy it compares exactly 512 words.
    assert_eq!(PAGE_BYTES, 2048);
}

#[test]
fn a_read_then_a_write_copies_a_page() {
    let mut rig = Rig::new();

    for i in (0..PAGE_BYTES).step_by(4) {
        rig.write32(VRAM_BASE + 0x56800 + i, 0x1000 + i);
        rig.write32(VRAM_BASE + 0xB3000 + i, 0xFFEEFFEE);
    }

    rig.read32(Rig::copy_addr(0x56800)); // prime
    rig.write32(Rig::copy_addr(0xB3000), 0xffffffff); // commit

    for i in (0..PAGE_BYTES).step_by(4) {
        assert_eq!(rig.read32(VRAM_BASE + 0xB3000 + i), 0x1000 + i);
    }
    assert_eq!(rig.sport.copies(), 1);
}

#[test]
fn sport_addresses_are_relative_to_vram_not_absolute() {
    let mut rig = Rig::new();
    rig.write32(VRAM_BASE + 0x800, 0xCAFEBABE);

    rig.read32(Rig::copy_addr(0x800));
    assert_eq!(rig.sport.latched_source(), VRAM_BASE + 0x800);

    rig.write32(Rig::copy_addr(0x1000), 0xffffffff);
    assert_eq!(rig.read32(VRAM_BASE + 0x1000), 0xCAFEBABE);
    // And nothing landed at the bare offset.
    assert_eq!(rig.read32(0x1000), 0);
}

#[test]
fn a_mask_bit_protects_the_destination_rather_than_selecting_it() {
    let mut rig = Rig::new();
    for i in (0..PAGE_BYTES).step_by(4) {
        rig.write32(VRAM_BASE + i, 0xAAAAAAAA);
        rig.write32(VRAM_BASE + 0x0800 + i, 0x55555555);
    }

    rig.read32(Rig::copy_addr(0x0000));
    rig.write32(Rig::copy_addr(0x0800), 0x0000FFFF);

    // A one PROTECTS: the destination's low half survives, the high half
    // comes from the source.
    assert_eq!(rig.read32(VRAM_BASE + 0x0800), 0xAAAA5555);
}

#[test]
fn the_fill_value_register_is_separate_from_the_fill() {
    let mut rig = Rig::new();
    for i in (0..PAGE_BYTES).step_by(4) {
        rig.write32(VRAM_BASE + 0x24800 + i, 0xAABBAABB);
    }

    rig.write32(SPORT_BASE + VALUE_REGISTER, 0x66676869);
    assert_eq!(rig.sport.fill_value(), 0x66676869);
    // Setting the value alone must not touch memory.
    assert_eq!(rig.read32(VRAM_BASE + 0x24800), 0xAABBAABB);

    rig.write32(Rig::fill_addr(0x24800), 0xffffffff);
    for i in (0..PAGE_BYTES).step_by(4) {
        assert_eq!(rig.read32(VRAM_BASE + 0x24800 + i), 0x66676869);
    }
    assert_eq!(rig.sport.fills(), 1);
}

#[test]
fn a_fill_respects_its_mask_too() {
    let mut rig = Rig::new();
    for i in (0..PAGE_BYTES).step_by(4) {
        rig.write32(VRAM_BASE + i, 0xFFFFFFFF);
    }
    rig.write32(SPORT_BASE + VALUE_REGISTER, 0x00000000);
    rig.write32(Rig::fill_addr(0x0000), 0x0000FF00);

    // Everything is filled with zero except the one byte the mask protects.
    assert_eq!(rig.read32(VRAM_BASE), 0x0000FF00);
}

#[test]
fn a_fill_does_not_disturb_the_next_page() {
    let mut rig = Rig::new();
    rig.write32(VRAM_BASE + PAGE_BYTES, 0x12345678);

    rig.write32(SPORT_BASE + VALUE_REGISTER, 0xDEADBEEF);
    rig.write32(Rig::fill_addr(0x0000), 0xffffffff);

    assert_eq!(rig.read32(VRAM_BASE), 0xDEADBEEF);
    assert_eq!(rig.read32(VRAM_BASE + PAGE_BYTES), 0x12345678);
}

#[test]
fn copying_a_page_onto_itself_is_harmless() {
    let mut rig = Rig::new();
    rig.write32(VRAM_BASE + 0x1000, 0x11223344);
    rig.read32(Rig::copy_addr(0x1000));
    rig.write32(Rig::copy_addr(0x1000), 0xffffffff);
    assert_eq!(rig.read32(VRAM_BASE + 0x1000), 0x11223344);
}

#[test]
fn a_copy_into_memory_marks_it_as_possibly_code() {
    // The C++ version also cleared the bus write-watch and drove the copy
    // through a whole console's bus. Its one check is the copy's result.
    let mut rig = Rig::new();
    for i in (0..PAGE_BYTES).step_by(4) {
        rig.write32(VRAM_BASE + i, 0xE1A00000);
    }
    rig.mem.write_watch_mut().clear();
    rig.read32(SPORT_BASE);
    rig.write32(SPORT_BASE + (0x0800 >> ADDRESS_SHIFT), 0xffffffff);
    assert!(rig.read32(VRAM_BASE + 0x800) == 0xE1A00000);
}

// ---------------------------------------------------------------------------
// Additions for the Rust rewrite (not in the C++ suite). Expected values were
// cross-checked against the C++ reference build.
// ---------------------------------------------------------------------------

#[test]
fn the_mask_rule_is_protect_on_one_take_on_zero() {
    assert_eq!(
        sport::apply_mask(0xAAAA_AAAA, 0x5555_5555, 0x0000_FFFF),
        0x5555_AAAA
    );
    assert_eq!(sport::apply_mask(0x1234_5678, 0x9ABC_DEF0, 0), 0x9ABC_DEF0);
    assert_eq!(
        sport::apply_mask(0x1234_5678, 0x9ABC_DEF0, 0xFFFF_FFFF),
        0x1234_5678
    );
}

/// The window is word-addressed: the byte-lane bits of an offset are ignored,
/// as the C++ bus's word accesses drop them before SPORT sees the offset, so a
/// page is always 2048-aligned.
#[test]
fn the_byte_lane_bits_of_a_sport_offset_are_ignored() {
    let mut rig = Rig::new();
    rig.write32(VRAM_BASE + 0x800, 0x600DCAFE);
    rig.sport.read((0x800 >> ADDRESS_SHIFT) | 3, &mut rig.mem);
    assert_eq!(rig.sport.latched_source(), VRAM_BASE + 0x800);
    rig.sport
        .write((0x1000 >> ADDRESS_SHIFT) | 1, 0xffffffff, &mut rig.mem);
    assert_eq!(rig.read32(VRAM_BASE + 0x1000), 0x600DCAFE);
    assert_eq!(rig.read32(VRAM_BASE + 0x1200), 0);
}

/// Reads latch only in the copy window proper; writes to the unclaimed
/// windows still copy. The top page index runs off the end of VRAM safely.
#[test]
fn unclaimed_windows_copy_on_write_but_never_latch() {
    let mut rig = Rig::new();
    rig.write32(VRAM_BASE + 0x800, 0x0BADF00D);
    rig.read32(Rig::copy_addr(0x800));
    rig.read32(SPORT_BASE + 0x6000 + (0x1000 >> ADDRESS_SHIFT));
    assert_eq!(rig.sport.latched_source(), VRAM_BASE + 0x800);

    rig.write32(SPORT_BASE + 0xE000 + (0x1800 >> ADDRESS_SHIFT), 0xffffffff);
    assert_eq!(rig.read32(VRAM_BASE + 0x1800), 0x0BADF00D);
    assert_eq!(rig.sport.copies(), 1);

    rig.write32(SPORT_BASE + sport::INDEX_MASK, 0xffffffff);
    rig.write32(SPORT_BASE + FILL_WINDOW + sport::INDEX_MASK, 0x0000ffff);
    assert_eq!(rig.sport.copies(), 2);
    assert_eq!(rig.sport.fills(), 1);
}
