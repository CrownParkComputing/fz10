//! MADAM tests: CCB decoding, the fixed-point mapping, the pixel formats, the
//! CEL engine's register protocol and the DMA channel registers.
//!
//! Ported from the C++ core's tests/test_madam.cpp, one `#[test]` per `TEST`
//! with the same name and every CHECK kept. Where the C++ test drove MADAM
//! through the `Bus` or a whole `Console`, these drive `Madam` and `Memory`
//! directly: a bus register store becomes `madam.write(offset, value, mem)`,
//! and the instruction-boundary hand-off becomes `run_cel_engine` when
//! `cel_engine_pending` says so. The mapping tests matter most: an error of
//! one bit in a 16.16 fraction is invisible on a still image and obvious once
//! anything moves, so position, scaling and the row-to-row bend are each
//! asserted separately.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::panic
)]

use retro3do::madam::*;
use retro3do::memory::{Memory, VRAM_BASE};
use retro3do::video::framebuffer_offset;

const fn fixed(whole: i32) -> i32 {
    whole << 16
}

const fn rgb555(r: u16, g: u16, b: u16) -> u16 {
    (r << 10) | (g << 5) | b
}

// Somewhere in DRAM to build test structures.
const CCB_AT: u32 = 0x1000;
const SOURCE_AT: u32 = 0x2000;
const PLUT_AT: u32 = 0x3000;

/// A CCB with absolute pointers, every load flag, CCBPRE (preamble in the
/// CCB) and both winding permissions. Steps are given in 16.16; the
/// horizontal ones are stored 12.20 as the hardware reads them.
#[allow(clippy::too_many_arguments)]
fn write_ccb_bent(
    mem: &mut Memory,
    at: u32,
    flags: u32,
    source: u32,
    plut: u32,
    (x, y): (i32, i32),
    (hdx, hdy): (i32, i32),
    (vdx, vdy): (i32, i32),
    pre0: u32,
    pre1: u32,
    (hddx, hddy): (i32, i32),
) {
    let all = CCB_LAST
        | CCB_NP_ABS
        | CCB_SP_ABS
        | CCB_PP_ABS
        | CCB_LD_SIZE
        | CCB_LD_PRS
        | CCB_LD_PPMP
        | CCB_LD_PLUT
        | CCB_CCB_PRE
        | CCB_YOXY
        | CCB_ACW
        | CCB_ACCW;
    let words: [u32; 15] = [
        flags | all,
        0,
        source,
        plut,
        x as u32,
        y as u32,
        hdx.wrapping_shl(4) as u32,
        hdy.wrapping_shl(4) as u32,
        vdx as u32,
        vdy as u32,
        hddx.wrapping_shl(4) as u32,
        hddy.wrapping_shl(4) as u32,
        0,
        pre0,
        pre1,
    ];
    for (i, word) in words.iter().enumerate() {
        mem.write32(at + i as u32 * 4, *word);
    }
}

#[allow(clippy::too_many_arguments)]
fn write_ccb(
    mem: &mut Memory,
    at: u32,
    flags: u32,
    source: u32,
    plut: u32,
    position: (i32, i32),
    h: (i32, i32),
    v: (i32, i32),
    pre0: u32,
    pre1: u32,
) {
    write_ccb_bent(
        mem,
        at,
        flags,
        source,
        plut,
        position,
        h,
        v,
        pre0,
        pre1,
        (0, 0),
    );
}

/// PRE0: the format, LINEAR for 16-bit direct colour, and height - 1.
fn pre0_for(format_code: u32, height: u32) -> u32 {
    let linear = if format_code == 6 { PRE0_LINEAR } else { 0 };
    format_code | linear | ((height - 1) << 6)
}

/// The smallest PRE1 row stride is two words.
fn row_words(width: u32, bpp: u32) -> u32 {
    ((width * bpp).div_ceil(32)).max(2)
}

fn row_bytes(width: u32, bpp: u32) -> u32 {
    row_words(width, bpp) * 4
}

/// PRE1: width - 1, the row stride, and TLLSB mode 1 (blue bit zero passes
/// through after the decoder saved it as the H bit).
fn pre1_for(width: u32, bpp: u32) -> u32 {
    let offset = row_words(width, bpp) - 2;
    let field = if bpp < 8 { offset << 24 } else { offset << 16 };
    (width - 1) | field | 0x0000_1000
}

const FORMAT_DIRECT16: u32 = 6;
const FORMAT_INDEXED8: u32 = 5;
const FORMAT_INDEXED4: u32 = 3;

fn read_target(mem: &Memory, x: u32, y: u32) -> u16 {
    read_target_in(mem, x, y, 320)
}

fn read_target_in(mem: &Memory, x: u32, y: u32, stride_pixels: u32) -> u16 {
    mem.read16(VRAM_BASE + framebuffer_offset(x, y, stride_pixels))
}

fn rig(clip: (u32, u32)) -> (Memory, Madam) {
    let mut madam = Madam::new();
    madam.set_clip(clip.0, clip.1);
    madam.set_target(VRAM_BASE, clip.0 * 2);
    (Memory::new(), madam)
}

/// MADAM as the machine leaves it after a console reset: NTSC clip and the
/// default target.
fn console() -> (Memory, Madam) {
    rig((320, 240))
}

/// What the machine does at the CPU instruction boundary.
fn run_pending_cel_engine(madam: &mut Madam, mem: &mut Memory) {
    if madam.cel_engine_pending() {
        madam.run_cel_engine(mem);
    }
}

const ONE: (i32, i32) = (fixed(1), 0);
const DOWN: (i32, i32) = (0, fixed(1));

// ---------------------------------------------------------------------------
// Format decoding
// ---------------------------------------------------------------------------

#[test]
fn the_pixel_format_is_decoded_from_pre0() {
    assert_eq!(cel_format_from_pre0(1), CelFormat::Indexed1);
    assert_eq!(cel_format_from_pre0(3), CelFormat::Indexed4);
    assert_eq!(cel_format_from_pre0(5), CelFormat::Indexed8);
    assert_eq!(cel_format_from_pre0(6), CelFormat::Direct16);
    assert_eq!(cel_format_from_pre0(0), CelFormat::Unknown);
    assert_eq!(cel_format_from_pre0(7), CelFormat::Unknown);
}

#[test]
fn a_ccb_reports_its_dimensions() {
    let (mut mem, mut madam) = rig((320, 240));
    write_ccb(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        PLUT_AT,
        (0, 0),
        ONE,
        DOWN,
        pre0_for(FORMAT_DIRECT16, 12),
        pre1_for(20, 16),
    );
    let ccb = madam.read_ccb(CCB_AT, &mem);
    assert_eq!(ccb.width, 20);
    assert_eq!(ccb.height, 12);
    assert_eq!(ccb.format, CelFormat::Direct16);
}

#[test]
fn a_relative_pointer_is_taken_from_the_word_after_it() {
    let (mut mem, mut madam) = rig((320, 240));
    // Absolute flags clear: the source offset is relative. CCBPRE set, so
    // the preamble does not eat the front of the source.
    mem.write32(CCB_AT, CCB_LAST | CCB_CCB_PRE);
    mem.write32(CCB_AT + 8, 0x40);
    let ccb = madam.read_ccb(CCB_AT, &mem);
    assert_eq!(ccb.source_address, CCB_AT + 12 + 0x40);
}

// ---------------------------------------------------------------------------
// Direct 16-bit cels
// ---------------------------------------------------------------------------

#[test]
fn a_direct_cel_lands_where_it_is_told() {
    let (mut mem, mut madam) = rig((320, 240));
    mem.write16(SOURCE_AT, rgb555(31, 0, 0));
    mem.write16(SOURCE_AT + 2, rgb555(0, 31, 0));
    mem.write16(SOURCE_AT + row_bytes(2, 16), rgb555(0, 0, 31));
    mem.write16(SOURCE_AT + row_bytes(2, 16) + 2, rgb555(31, 31, 31));
    write_ccb(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        0,
        (fixed(10), fixed(20)),
        ONE,
        DOWN,
        pre0_for(FORMAT_DIRECT16, 2),
        pre1_for(2, 16),
    );

    madam.render_cel_list(CCB_AT, &mut mem);

    assert_eq!(madam.stats().cels_drawn, 1);
    assert_eq!(read_target(&mem, 10, 20), rgb555(31, 0, 0));
    assert_eq!(read_target(&mem, 11, 20), rgb555(0, 31, 0));
    assert_eq!(read_target(&mem, 10, 21), rgb555(0, 0, 31));
    assert_eq!(read_target(&mem, 11, 21), rgb555(31, 31, 31));
}

#[test]
fn an_lrform_cel_reads_the_two_rows_from_each_source_word() {
    let (mut mem, mut madam) = rig((320, 240));
    // [row0 x0, row1 x0], [row0 x1, row1 x1].
    mem.write16(SOURCE_AT, rgb555(31, 0, 0));
    mem.write16(SOURCE_AT + 2, rgb555(0, 0, 31));
    mem.write16(SOURCE_AT + 4, rgb555(0, 31, 0));
    mem.write16(SOURCE_AT + 6, rgb555(31, 31, 31));
    // VCNT zero means one pair: two output rows.
    write_ccb(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        0,
        (fixed(10), fixed(20)),
        ONE,
        DOWN,
        pre0_for(FORMAT_DIRECT16, 1),
        pre1_for(2, 16) | PRE1_LRFORM,
    );
    madam.render_cel_list(CCB_AT, &mut mem);

    assert_eq!(read_target(&mem, 10, 20), rgb555(31, 0, 0));
    assert_eq!(read_target(&mem, 11, 20), rgb555(0, 31, 0));
    assert_eq!(read_target(&mem, 10, 21), rgb555(0, 0, 31));
    assert_eq!(read_target(&mem, 11, 21), rgb555(31, 31, 31));
}

#[test]
fn colour_zero_is_transparent() {
    let (mut mem, mut madam) = rig((320, 240));
    mem.write16(VRAM_BASE + framebuffer_offset(0, 0, 320), rgb555(0, 31, 0));
    mem.write16(VRAM_BASE + framebuffer_offset(1, 0, 320), rgb555(0, 31, 0));
    mem.write16(SOURCE_AT, 0);
    mem.write16(SOURCE_AT + 2, rgb555(31, 0, 0));
    write_ccb(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        0,
        (0, 0),
        ONE,
        DOWN,
        pre0_for(FORMAT_DIRECT16, 1),
        pre1_for(2, 16),
    );
    madam.render_cel_list(CCB_AT, &mut mem);

    assert_eq!(read_target(&mem, 0, 0), rgb555(0, 31, 0)); // survived
    assert_eq!(read_target(&mem, 1, 0), rgb555(31, 0, 0)); // overwritten
}

#[test]
fn a_cel_is_clipped_rather_than_writing_out_of_bounds() {
    let (mut mem, mut madam) = rig((4, 4));
    for y in 0..4 {
        for x in 0..4 {
            mem.write16(SOURCE_AT + y * row_bytes(4, 16) + x * 2, rgb555(31, 31, 31));
        }
    }
    // Most of it falls off the right and bottom edges.
    write_ccb(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        0,
        (fixed(2), fixed(2)),
        ONE,
        DOWN,
        pre0_for(FORMAT_DIRECT16, 4),
        pre1_for(4, 16),
    );
    madam.render_cel_list(CCB_AT, &mut mem);

    assert_eq!(read_target_in(&mem, 2, 2, 4), rgb555(31, 31, 31));
    assert_eq!(read_target_in(&mem, 3, 3, 4), rgb555(31, 31, 31));
    assert_eq!(madam.stats().pixels_written, 4);
}

// ---------------------------------------------------------------------------
// The mapping
// ---------------------------------------------------------------------------

#[test]
fn a_doubled_horizontal_step_stretches_the_cel() {
    let (mut mem, mut madam) = rig((320, 240));
    mem.write16(SOURCE_AT, rgb555(31, 0, 0));
    mem.write16(SOURCE_AT + 2, rgb555(0, 0, 31));
    write_ccb(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        0,
        (0, 0),
        (fixed(2), 0),
        DOWN,
        pre0_for(FORMAT_DIRECT16, 1),
        pre1_for(2, 16),
    );
    madam.render_cel_list(CCB_AT, &mut mem);

    assert_eq!(read_target(&mem, 0, 0), rgb555(31, 0, 0));
    assert_eq!(read_target(&mem, 2, 0), rgb555(0, 0, 31));
}

#[test]
fn fractional_steps_accumulate_rather_than_rounding_each_time() {
    let (mut mem, mut madam) = rig((320, 240));
    mem.write16(SOURCE_AT, rgb555(31, 0, 0));
    mem.write16(SOURCE_AT + 2, rgb555(0, 0, 31));
    write_ccb(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        0,
        (0, 0),
        (fixed(1) / 2, 0),
        DOWN,
        pre0_for(FORMAT_DIRECT16, 1),
        pre1_for(2, 16),
    );
    madam.render_cel_list(CCB_AT, &mut mem);

    // The second pixel overwrote the first at the same destination.
    assert_eq!(read_target(&mem, 0, 0), rgb555(0, 0, 31));
    assert_eq!(read_target(&mem, 1, 0), 0);
}

#[test]
fn a_vertical_delta_can_place_rows_diagonally() {
    let (mut mem, mut madam) = rig((320, 240));
    mem.write16(SOURCE_AT, rgb555(31, 0, 0));
    mem.write16(SOURCE_AT + row_bytes(1, 16), rgb555(0, 0, 31));
    // Each row steps right as well as down: a shear.
    write_ccb(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        0,
        (0, 0),
        ONE,
        (fixed(1), fixed(1)),
        pre0_for(FORMAT_DIRECT16, 2),
        pre1_for(1, 16),
    );
    madam.render_cel_list(CCB_AT, &mut mem);

    assert_eq!(read_target(&mem, 0, 0), rgb555(31, 0, 0));
    assert_eq!(read_target(&mem, 1, 1), rgb555(0, 0, 31));
}

#[test]
fn the_horizontal_step_bends_as_rows_advance() {
    let (mut mem, mut madam) = rig((320, 240));
    for y in 0..2 {
        for x in 0..2 {
            mem.write16(SOURCE_AT + y * row_bytes(2, 16) + x * 2, rgb555(31, 31, 31));
        }
    }
    write_ccb_bent(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        0,
        (0, 0),
        ONE,
        DOWN,
        pre0_for(FORMAT_DIRECT16, 2),
        pre1_for(2, 16),
        (fixed(1), 0),
    );
    madam.render_cel_list(CCB_AT, &mut mem);

    // Row 0 steps by one; row 1 steps by two, so its second pixel is at x = 2.
    assert_eq!(read_target(&mem, 1, 0), rgb555(31, 31, 31));
    assert_eq!(read_target(&mem, 2, 1), rgb555(31, 31, 31));
}

// ---------------------------------------------------------------------------
// Indexed formats
// ---------------------------------------------------------------------------

#[test]
fn an_eight_bit_indexed_cel_goes_through_the_palette() {
    let (mut mem, mut madam) = rig((320, 240));
    mem.write16(PLUT_AT + 2, rgb555(31, 0, 0));
    mem.write16(PLUT_AT + 4, rgb555(0, 0, 31));
    mem.write8(SOURCE_AT, 1);
    mem.write8(SOURCE_AT + 1, 2);
    write_ccb(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        PLUT_AT,
        (0, 0),
        ONE,
        DOWN,
        pre0_for(FORMAT_INDEXED8, 1),
        pre1_for(2, 8),
    );
    madam.render_cel_list(CCB_AT, &mut mem);

    assert_eq!(read_target(&mem, 0, 0), rgb555(31, 0, 0));
    assert_eq!(read_target(&mem, 1, 0), rgb555(0, 0, 31));
}

#[test]
fn a_four_bit_indexed_cel_unpacks_two_pixels_per_byte() {
    let (mut mem, mut madam) = rig((320, 240));
    mem.write16(PLUT_AT + 2, rgb555(31, 0, 0));
    mem.write16(PLUT_AT + 4, rgb555(0, 0, 31));
    // High nibble first: index 1 then index 2.
    mem.write8(SOURCE_AT, 0x12);
    write_ccb(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        PLUT_AT,
        (0, 0),
        ONE,
        DOWN,
        pre0_for(FORMAT_INDEXED4, 1),
        pre1_for(2, 4),
    );
    madam.render_cel_list(CCB_AT, &mut mem);

    assert_eq!(read_target(&mem, 0, 0), rgb555(31, 0, 0));
    assert_eq!(read_target(&mem, 1, 0), rgb555(0, 0, 31));
}

#[test]
fn the_decoder_substitutes_the_blue_lsb_selected_by_pre1() {
    let (mut mem, mut madam) = rig((320, 240));
    // Mode 0 forces zero, mode 2 copies PDC bit 4, mode 3 copies PDC bit 5.
    let inputs = [rgb555(0, 0, 3), rgb555(0, 0, 16), rgb555(0, 1, 0)];
    let modes = [0x0000_0000u32, 0x0000_2000, 0x0000_3000];
    let expected = [rgb555(0, 0, 2), rgb555(0, 0, 17), rgb555(0, 1, 1)];

    for i in 0..3u32 {
        let source = SOURCE_AT + i * 8;
        mem.write16(source, inputs[i as usize]);
        write_ccb(
            &mut mem,
            CCB_AT,
            0,
            source,
            0,
            (fixed(i as i32), 0),
            ONE,
            DOWN,
            pre0_for(FORMAT_DIRECT16, 1),
            (pre1_for(1, 16) & !0x0000_3000) | modes[i as usize],
        );
        madam.render_cel_list(CCB_AT, &mut mem);
        assert_eq!(read_target(&mem, i, 0), expected[i as usize]);
    }
}

#[test]
fn pdc_mfonly_uses_each_pixel_channels_multiplier_and_the_pixc_divider() {
    let (mut mem, mut madam) = rig((320, 240));
    // The real read/write framebuffer path: 0x50 decodes to the 1280-byte
    // distance between row pairs of a 320-pixel field.
    madam.write(MADAM_REG_CTL0, 0x0000_5050, &mut mem);
    madam.write(MADAM_REG_CTL2, VRAM_BASE, &mut mem);
    madam.write(MADAM_REG_CTL3, VRAM_BASE, &mut mem);
    madam.write(MADAM_CCB_CTL0, CTL0_B15_PDC | CTL0_B0_PPMP, &mut mem);

    mem.write16(SOURCE_AT, rgb555(28, 12, 4));
    write_ccb(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        0,
        (0, 0),
        ONE,
        DOWN,
        pre0_for(FORMAT_DIRECT16, 1),
        pre1_for(1, 16),
    );
    // MS=3 (PDC_MFONLY), DV1=2: multipliers 8, 4 and 2 from the channels'
    // high bits, all divided by four.
    mem.write32(CCB_AT + 48, 0x0000_6200);
    madam.render_cel_list(CCB_AT, &mut mem);

    assert_eq!(read_target(&mem, 0, 0), rgb555(31, 12, 2));
}

// ---------------------------------------------------------------------------
// Walking the list
// ---------------------------------------------------------------------------

#[test]
fn the_list_stops_at_the_last_flag() {
    let (mut mem, mut madam) = rig((320, 240));
    write_ccb(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        0,
        (0, 0),
        ONE,
        DOWN,
        pre0_for(FORMAT_DIRECT16, 1),
        pre1_for(1, 16),
    );
    madam.render_cel_list(CCB_AT, &mut mem);
    assert_eq!(madam.stats().cels_walked, 1);
    assert_eq!(madam.stats().list_truncated, 0);
}

#[test]
fn a_second_cel_paints_over_the_first() {
    let (mut mem, mut madam) = rig((320, 240));
    let first = CCB_AT;
    let second = CCB_AT + 0x100;
    mem.write16(SOURCE_AT, rgb555(31, 0, 0));
    mem.write16(SOURCE_AT + 2, rgb555(0, 0, 31));

    write_ccb(
        &mut mem,
        first,
        0,
        SOURCE_AT,
        0,
        (0, 0),
        ONE,
        DOWN,
        pre0_for(FORMAT_DIRECT16, 1),
        pre1_for(1, 16),
    );
    let flags = mem.read32(first) & !CCB_LAST;
    mem.write32(first, flags);
    mem.write32(first + 4, second);
    write_ccb(
        &mut mem,
        second,
        0,
        SOURCE_AT + 2,
        0,
        (0, 0),
        ONE,
        DOWN,
        pre0_for(FORMAT_DIRECT16, 1),
        pre1_for(1, 16),
    );

    madam.render_cel_list(first, &mut mem);

    assert_eq!(madam.stats().cels_walked, 2);
    assert_eq!(read_target(&mem, 0, 0), rgb555(0, 0, 31));
}

#[test]
fn a_skipped_cel_can_load_the_palette_for_the_cel_after_it() {
    let (mut mem, mut madam) = rig((320, 240));
    let first = CCB_AT;
    let second = CCB_AT + 0x100;
    mem.write8(SOURCE_AT, 1);
    mem.write8(SOURCE_AT + 8, 1);
    mem.write16(PLUT_AT + 2, rgb555(31, 0, 0));

    write_ccb(
        &mut mem,
        first,
        CCB_SKIP,
        SOURCE_AT,
        PLUT_AT,
        (fixed(1), fixed(1)),
        ONE,
        DOWN,
        pre0_for(FORMAT_INDEXED8, 1),
        pre1_for(1, 8),
    );
    let flags = mem.read32(first) & !CCB_LAST;
    mem.write32(first, flags);
    mem.write32(first + 4, second);

    write_ccb(
        &mut mem,
        second,
        0,
        SOURCE_AT + 8,
        PLUT_AT + 0x100,
        (fixed(5), fixed(6)),
        ONE,
        DOWN,
        pre0_for(FORMAT_INDEXED8, 1),
        pre1_for(1, 8),
    );
    let flags = mem.read32(second) & !CCB_LD_PLUT;
    mem.write32(second, flags);

    madam.render_cel_list(first, &mut mem);
    assert_eq!(madam.stats().cels_walked, 2);
    assert_eq!(madam.stats().cels_drawn, 1);
    assert_eq!(read_target(&mem, 5, 6), rgb555(31, 0, 0));
}

#[test]
fn a_self_referencing_list_terminates() {
    let (mut mem, mut madam) = rig((320, 240));
    write_ccb(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        0,
        (0, 0),
        ONE,
        DOWN,
        pre0_for(FORMAT_DIRECT16, 1),
        pre1_for(1, 16),
    );
    let flags = mem.read32(CCB_AT) & !CCB_LAST;
    mem.write32(CCB_AT, flags);
    mem.write32(CCB_AT + 4, CCB_AT); // points at itself

    madam.render_cel_list(CCB_AT, &mut mem);
    assert_eq!(madam.stats().cels_walked, 1);
    assert_eq!(madam.stats().list_truncated, 0);
}

#[test]
fn the_largest_expressible_cel_is_still_bounded() {
    let (mut mem, mut madam) = rig((4, 4));
    // Rows as far apart as an all-ones stride field claims.
    let huge_row = (0x3ff + 2) * 4;
    for y in 0..4 {
        for x in 0..4 {
            mem.write16(SOURCE_AT + y * huge_row + x * 2, rgb555(31, 31, 31));
        }
    }
    write_ccb(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        0,
        (0, 0),
        ONE,
        DOWN,
        pre0_for(FORMAT_DIRECT16, 1),
        pre1_for(1, 16),
    );
    // Every bit of the size fields, but no skip.
    mem.write32(CCB_AT + 52, FORMAT_DIRECT16 | PRE0_LINEAR | 0x0000_ffc0);
    // Every bit except LRFORM.
    mem.write32(CCB_AT + 56, !PRE1_LRFORM);

    let ccb = madam.read_ccb(CCB_AT, &mem);
    assert!(ccb.width <= 2048);
    assert!(ccb.height <= 1024);

    madam.render_cel_list(CCB_AT, &mut mem);
    // It draws, but clipping means only the visible 4x4 is ever written.
    assert_eq!(madam.stats().pixels_written, 16);
}

#[test]
fn an_unknown_pixel_format_draws_nothing() {
    let (mut mem, mut madam) = rig((320, 240));
    write_ccb(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        0,
        (0, 0),
        ONE,
        DOWN,
        pre0_for(0, 4),
        pre1_for(4, 16),
    );
    madam.render_cel_list(CCB_AT, &mut mem);
    assert_eq!(madam.stats().cels_drawn, 0);
}

// ---------------------------------------------------------------------------
// Through the register interface (the C++ drove these through a Console)
// ---------------------------------------------------------------------------

#[test]
fn writing_the_start_register_runs_the_list() {
    let (mut mem, mut madam) = console();
    mem.write16(SOURCE_AT, rgb555(31, 0, 0));
    write_ccb(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        0,
        (fixed(5), fixed(5)),
        ONE,
        DOWN,
        pre0_for(FORMAT_DIRECT16, 1),
        pre1_for(1, 16),
    );

    // The list head goes in NEXTCCB; the start port only says "go".
    madam.write(MADAM_NEXT_CCB, CCB_AT, &mut mem);
    madam.write(MADAM_CEL_START, 0, &mut mem);

    // Starting does not run the engine: it runs at the instruction boundary.
    assert_eq!(madam.stats().cels_drawn, 0);
    run_pending_cel_engine(&mut madam, &mut mem);

    assert_eq!(madam.stats().cels_drawn, 1);
    assert_eq!(read_target(&mem, 5, 5), rgb555(31, 0, 0));
}

// ---------------------------------------------------------------------------
// Magnification
// ---------------------------------------------------------------------------

#[test]
fn a_magnified_cel_is_solid_rather_than_a_grid_of_dots() {
    let (mut mem, mut madam) = rig((320, 240));
    for y in 0..2 {
        for x in 0..2 {
            mem.write16(SOURCE_AT + y * row_bytes(2, 16) + x * 2, rgb555(31, 31, 31));
        }
    }
    // A 2x2 cel blown up four times each way: an 8x8 solid block.
    write_ccb(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        0,
        (0, 0),
        (fixed(4), 0),
        (0, fixed(4)),
        pre0_for(FORMAT_DIRECT16, 2),
        pre1_for(2, 16),
    );
    madam.render_cel_list(CCB_AT, &mut mem);

    for y in 0..8 {
        for x in 0..8 {
            assert_eq!(read_target(&mem, x, y), rgb555(31, 31, 31), "pixel {x},{y}");
        }
    }
}

#[test]
fn a_one_to_one_cel_still_writes_exactly_one_pixel_each() {
    let (mut mem, mut madam) = rig((320, 240));
    for y in 0..4 {
        for x in 0..4 {
            mem.write16(SOURCE_AT + y * row_bytes(4, 16) + x * 2, rgb555(31, 31, 31));
        }
    }
    write_ccb(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        0,
        (0, 0),
        ONE,
        DOWN,
        pre0_for(FORMAT_DIRECT16, 4),
        pre1_for(4, 16),
    );
    madam.render_cel_list(CCB_AT, &mut mem);
    assert_eq!(madam.stats().pixels_written, 16);
}

#[test]
fn a_rotated_cel_lands_as_a_solid_figure_not_a_stack_of_rectangles() {
    let (mut mem, mut madam) = rig((320, 240));
    for y in 0..4 {
        for x in 0..4 {
            mem.write16(SOURCE_AT + y * row_bytes(4, 16) + x * 2, rgb555(31, 31, 31));
        }
    }
    // Forty-five degrees, scaled by two.
    let step = fixed(2);
    write_ccb(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        0,
        (fixed(40), fixed(40)),
        (step, step),
        (-step, step),
        pre0_for(FORMAT_DIRECT16, 4),
        pre1_for(4, 16),
    );
    madam.render_cel_list(CCB_AT, &mut mem);

    // Both horizontal edge intersections are painted: three pixels on each of
    // four scanlines per texel.
    assert_eq!(madam.stats().pixels_written, 16 * 12);

    // Solid: every row it touches is one unbroken run.
    let mut rows_touched = 0;
    let mut widest = 0;
    for y in 0..240 {
        let painted: Vec<u32> = (0..320)
            .filter(|&x| read_target(&mem, x, y) == rgb555(31, 31, 31))
            .collect();
        let (Some(&first), Some(&last)) = (painted.first(), painted.last()) else {
            continue;
        };
        rows_touched += 1;
        assert_eq!(painted.len() as u32, last - first + 1, "hole in row {y}");
        widest = widest.max(painted.len());
    }
    // Inclusive spans also preserve the collapsed one-pixel tips.
    assert_eq!(rows_touched, 16);
    assert_eq!(widest, 17);
}

#[test]
fn a_transparent_run_advances_both_edges_of_a_rotated_packed_cel() {
    let (mut mem, mut madam) = rig((320, 240));
    // Packed cels take their blue-LSB substitution mode from CECONTROL.
    madam.write(MADAM_CCB_CTL0, 0x0010_0000, &mut mem);

    // One eight-byte packed row: three transparent pixels, one literal
    // palette index, end of row.
    mem.write8(SOURCE_AT, 0x00);
    mem.write8(SOURCE_AT + 1, 0x00);
    mem.write8(SOURCE_AT + 2, 0x82); // transparent, count 3
    mem.write8(SOURCE_AT + 3, 0x40); // literal, count 1
    mem.write8(SOURCE_AT + 4, 0x01); // PLUT entry 1
    mem.write8(SOURCE_AT + 5, 0x00); // end of row
    mem.write16(PLUT_AT + 2, rgb555(31, 31, 31));

    let step = fixed(2);
    write_ccb(
        &mut mem,
        CCB_AT,
        CCB_PACKED,
        SOURCE_AT,
        PLUT_AT,
        (fixed(20), fixed(20)),
        (step, step),
        (-step, step),
        pre0_for(FORMAT_INDEXED8, 1),
        pre1_for(4, 8),
    );
    madam.render_cel_list(CCB_AT, &mut mem);

    // Only the final texel is visible: one diamond, not a bridge back to the
    // row origin.
    assert_eq!(madam.stats().pixels_written, 12);
    assert_eq!(read_target(&mem, 20, 23), 0);
    assert_eq!(read_target(&mem, 26, 27), rgb555(31, 31, 31));
}

// ---------------------------------------------------------------------------
// DMA channel registers
// ---------------------------------------------------------------------------

#[test]
fn madam_reports_the_green_hardware_revision_and_keeps_it_read_only() {
    let (mut mem, mut madam) = rig((320, 240));
    assert_eq!(madam.read(MADAM_REVISION), 0x0102_0000);
    madam.write(MADAM_REVISION, 0, &mut mem);
    assert_eq!(madam.read(MADAM_REVISION), 0x0102_0000);
    madam.reset();
    assert_eq!(madam.read(MADAM_REVISION), 0x0102_0000);
}

#[test]
fn dma_channels_are_address_and_length_pairs_eight_bytes_apart() {
    let (mut mem, mut madam) = rig((320, 240));
    madam.write(0x218, 0x0002_0C00, &mut mem);
    madam.write(0x21C, 0x0000_0040, &mut mem);
    madam.write(0x238, 0x0003_0000, &mut mem);
    madam.write(0x23C, 0x0000_0010, &mut mem);

    assert_eq!(madam.dma_address(3), 0x0002_0C00);
    assert_eq!(madam.dma_length(3), 0x0000_0040);
    assert_eq!(madam.dma_address(7), 0x0003_0000);
    assert_eq!(madam.dma_length(7), 0x0000_0010);

    // And they read back through the register window.
    assert_eq!(madam.read(0x218), 0x0002_0C00);
    assert_eq!(madam.read(0x23C), 0x0000_0010);
}

#[test]
fn dma_channels_do_not_disturb_each_other() {
    let (mut mem, mut madam) = rig((320, 240));
    madam.write(0x200, 0x1111_1111, &mut mem); // channel 0 address
    madam.write(0x208, 0x2222_2222, &mut mem); // channel 1 address

    assert_eq!(madam.dma_address(0), 0x1111_1111);
    assert_eq!(madam.dma_address(1), 0x2222_2222);
    assert_eq!(madam.dma_length(0), 0);
}

#[test]
fn a_reset_clears_the_dma_channels() {
    let (mut mem, mut madam) = rig((320, 240));
    madam.write(0x218, 0xDEAD_BEEF, &mut mem);
    madam.reset();
    assert_eq!(madam.dma_address(3), 0);
}

#[test]
fn a_reset_clears_madams_operational_registers_and_inherited_cel_state() {
    let (mut mem, mut madam) = rig((320, 240));
    let writes = [
        (MADAM_DMA_ENABLE, 1),
        (MADAM_XBUS_DMA_ADDRESS, 0x1111_0000),
        (MADAM_XBUS_DMA_LENGTH, 0x200),
        (MADAM_VDL_ADDRESS, 0x2222_0000),
        (MADAM_PBUS_ADDRESS, 0x3333_0000),
        (MADAM_PBUS_LENGTH, 0x80),
        (MADAM_PBUS_POINTER, 0x20),
        (MADAM_CURRENT_CCB, 0x4444_0000),
        (MADAM_NEXT_CCB, 0x5555_0000),
        (MADAM_CCB_CTL0, 0x1234),
        (MADAM_REG_CTL0, 0x0101),
        (MADAM_REG_CTL1, 0x00ef_013f),
        (MADAM_REG_CTL2, 0x6666_0000),
        (MADAM_REG_CTL3, 0x7777_0000),
        (MADAM_FIFO_BASE, 0x8888_0000),
        (MADAM_FIFO_BASE + 4, 0x40),
        (MADAM_MATRIX_IN, 0x1111),
        (MADAM_MATRIX_VEC, 0x2222),
        (MADAM_MATRIX_NUM_HI, 0x3333),
    ];
    for (offset, value) in writes {
        madam.write(offset, value, &mut mem);
    }
    madam.reset();

    for (offset, _) in writes {
        assert_eq!(madam.read(offset), 0, "register {offset:#06x}");
    }
}

#[test]
fn the_start_port_ignores_the_value_written_to_it() {
    let (mut mem, mut madam) = console();
    mem.write16(SOURCE_AT, rgb555(31, 0, 0));
    write_ccb(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        0,
        (fixed(5), fixed(5)),
        ONE,
        DOWN,
        pre0_for(FORMAT_DIRECT16, 1),
        pre1_for(1, 16),
    );

    madam.write(MADAM_NEXT_CCB, CCB_AT, &mut mem);
    madam.write(MADAM_CEL_START, 0xdead_beef, &mut mem);
    run_pending_cel_engine(&mut madam, &mut mem);
    assert_eq!(madam.stats().cels_drawn, 1);
}

#[test]
fn the_cel_status_distinguishes_running_suspended_and_idle() {
    let (mut mem, mut madam) = console();
    write_ccb(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        0,
        (fixed(5), fixed(5)),
        ONE,
        DOWN,
        pre0_for(FORMAT_DIRECT16, 1),
        pre1_for(1, 16),
    );
    madam.write(MADAM_NEXT_CCB, CCB_AT, &mut mem);

    assert_eq!(madam.read(MADAM_CEL_STATUS), 0);
    madam.write(MADAM_CEL_START, 0, &mut mem);
    assert_eq!(madam.read(MADAM_CEL_STATUS), MADAM_CEL_RUNNING);

    madam.write(MADAM_CEL_PAUSE, 0, &mut mem);
    assert_eq!(
        madam.read(MADAM_CEL_STATUS),
        MADAM_CEL_RUNNING | MADAM_CEL_PAUSED
    );
    assert!(!madam.cel_engine_pending());

    madam.write(MADAM_CEL_RESUME, 0, &mut mem);
    assert_eq!(madam.read(MADAM_CEL_STATUS), MADAM_CEL_RUNNING);
    assert!(madam.cel_engine_pending());
    run_pending_cel_engine(&mut madam, &mut mem);
    assert_eq!(madam.read(MADAM_CEL_STATUS), 0);
}

#[test]
fn current_ccb_tracks_the_hardware_fetch_cursor() {
    let (mut mem, mut madam) = console();
    write_ccb(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        0,
        (fixed(5), fixed(5)),
        ONE,
        DOWN,
        pre0_for(FORMAT_DIRECT16, 1),
        pre1_for(1, 16),
    );
    madam.write(MADAM_NEXT_CCB, CCB_AT, &mut mem);
    madam.write(MADAM_CEL_START, 0, &mut mem);
    run_pending_cel_engine(&mut madam, &mut mem);

    // Six fixed words + four size + two perspective + PIXC + two preamble.
    assert_eq!(madam.read(MADAM_CURRENT_CCB), CCB_AT + 15 * 4);
}

#[test]
fn stop_leaves_the_engine_idle_and_discards_a_suspended_walk() {
    let (mut mem, mut madam) = console();
    madam.write(MADAM_NEXT_CCB, CCB_AT, &mut mem);
    madam.write(MADAM_CEL_START, 0, &mut mem);
    madam.write(MADAM_CEL_PAUSE, 0, &mut mem);
    madam.write(MADAM_CEL_STOP, 0, &mut mem);

    assert_eq!(madam.read(MADAM_CEL_STATUS), 0);
    assert_eq!(madam.read(MADAM_NEXT_CCB), 0);
    assert!(!madam.cel_engine_pending());
}

#[test]
fn stopping_the_engine_clears_where_it_would_go_next() {
    let (mut mem, mut madam) = console();
    mem.write16(SOURCE_AT, rgb555(31, 0, 0));
    write_ccb(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        0,
        (fixed(5), fixed(5)),
        ONE,
        DOWN,
        pre0_for(FORMAT_DIRECT16, 1),
        pre1_for(1, 16),
    );

    madam.write(MADAM_NEXT_CCB, CCB_AT, &mut mem);
    madam.write(MADAM_CEL_STOP, 0, &mut mem);
    madam.write(MADAM_CEL_START, 0, &mut mem);
    assert_eq!(madam.stats().cels_drawn, 0);
    // And the start that follows walks nothing.
    run_pending_cel_engine(&mut madam, &mut mem);
    assert_eq!(madam.stats().cels_drawn, 0);
}

#[test]
fn a_cel_without_ccbpre_takes_its_preamble_from_the_front_of_its_source() {
    let (mut mem, mut madam) = rig((320, 240));
    mem.write32(CCB_AT, CCB_LAST | CCB_SP_ABS);
    mem.write32(CCB_AT + 8, SOURCE_AT);
    // Different from the preamble in the source, so the wrong place cannot
    // accidentally agree.
    mem.write32(CCB_AT + 52, pre0_for(FORMAT_DIRECT16, 99));
    mem.write32(CCB_AT + 56, pre1_for(99, 16));
    mem.write32(SOURCE_AT, pre0_for(FORMAT_DIRECT16, 12));
    mem.write32(SOURCE_AT + 4, pre1_for(20, 16));

    let ccb = madam.read_ccb(CCB_AT, &mem);
    assert_eq!(ccb.width, 20);
    assert_eq!(ccb.height, 12);
    // The pixels start after the two words it consumed.
    assert_eq!(ccb.source_address, SOURCE_AT + 8);
}

#[test]
fn a_packed_cel_keeps_the_width_the_last_unpacked_cel_left_behind() {
    let (mut mem, mut madam) = rig((320, 240));
    mem.write32(CCB_AT, CCB_LAST | CCB_SP_ABS | CCB_CCB_PRE);
    mem.write32(CCB_AT + 8, SOURCE_AT);
    mem.write32(CCB_AT + 24, pre0_for(FORMAT_DIRECT16, 4));
    mem.write32(CCB_AT + 28, pre1_for(37, 16));
    assert_eq!(madam.read_ccb(CCB_AT, &mem).width, 37);

    // A packed cel with a different PRE1 in the CCB, which it must ignore.
    mem.write32(CCB_AT, CCB_LAST | CCB_SP_ABS | CCB_CCB_PRE | CCB_PACKED);
    mem.write32(CCB_AT + 28, pre1_for(5, 16));
    assert_eq!(madam.read_ccb(CCB_AT, &mem).width, 37);
}

#[test]
fn a_compact_ccb_inherits_the_load_controlled_cel_registers() {
    let (mut mem, mut madam) = rig((320, 240));
    write_ccb_bent(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        PLUT_AT,
        (fixed(13), fixed(17)),
        (fixed(2), fixed(3)),
        (fixed(4), fixed(5)),
        pre0_for(FORMAT_DIRECT16, 6),
        pre1_for(7, 16),
        (fixed(8), fixed(9)),
    );
    mem.write32(CCB_AT + 48, 0x1234_5678);

    let loaded = madam.read_ccb(CCB_AT, &mem);
    assert_eq!(loaded.x, fixed(13));
    assert_eq!(loaded.hdx, fixed(2));
    assert_eq!(loaded.pixc, 0x1234_5678);

    // All four load flags clear: the tail starts right after the X/Y slots,
    // which YOXY-clear ignores. PRE0/PRE1 at +24/+28 describe this cel; every
    // transform and PIXC register is inherited.
    let compact = 0x1100;
    mem.write32(
        compact,
        CCB_LAST | CCB_NP_ABS | CCB_SP_ABS | CCB_PP_ABS | CCB_CCB_PRE | CCB_ACW | CCB_ACCW,
    );
    mem.write32(compact + 4, 0);
    mem.write32(compact + 8, SOURCE_AT);
    mem.write32(compact + 12, PLUT_AT);
    mem.write32(compact + 16, 0x7fff_ffff);
    mem.write32(compact + 20, 0x7fff_ffff);
    mem.write32(compact + 24, pre0_for(FORMAT_DIRECT16, 10));
    mem.write32(compact + 28, pre1_for(11, 16));

    let inherited = madam.read_ccb(compact, &mem);
    assert_eq!(inherited.x, fixed(13));
    assert_eq!(inherited.y, fixed(17));
    assert_eq!(inherited.hdx, fixed(2));
    assert_eq!(inherited.hdy, fixed(3));
    assert_eq!(inherited.vdx, fixed(4));
    assert_eq!(inherited.vdy, fixed(5));
    assert_eq!(inherited.hddx, fixed(8));
    assert_eq!(inherited.hddy, fixed(9));
    assert_eq!(inherited.pixc, 0x1234_5678);
    assert_eq!(inherited.width, 11);
    assert_eq!(inherited.height, 10);
}

#[test]
fn the_engine_reads_the_ccb_as_it_stands_when_the_instruction_ends() {
    let (mut mem, mut madam) = console();
    mem.write16(SOURCE_AT, rgb555(31, 0, 0));
    write_ccb(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        0,
        (fixed(5), fixed(5)),
        ONE,
        DOWN,
        pre0_for(FORMAT_DIRECT16, 1),
        pre1_for(1, 16),
    );

    madam.write(MADAM_NEXT_CCB, CCB_AT, &mut mem);
    madam.write(MADAM_CEL_START, 0, &mut mem);

    // Still inside the instruction that asked: moving the cel now must move
    // where it lands.
    mem.write32(CCB_AT + 16, fixed(9) as u32);
    mem.write32(CCB_AT + 20, fixed(9) as u32);

    run_pending_cel_engine(&mut madam, &mut mem);

    assert_eq!(madam.stats().cels_drawn, 1);
    assert_eq!(read_target(&mem, 9, 9), rgb555(31, 0, 0));
    assert_eq!(read_target(&mem, 5, 5), 0);
}

#[test]
fn stopping_the_engine_also_cancels_a_start_that_has_not_happened_yet() {
    let (mut mem, mut madam) = console();
    write_ccb(
        &mut mem,
        CCB_AT,
        0,
        SOURCE_AT,
        0,
        (fixed(5), fixed(5)),
        ONE,
        DOWN,
        pre0_for(FORMAT_DIRECT16, 1),
        pre1_for(1, 16),
    );
    madam.write(MADAM_NEXT_CCB, CCB_AT, &mut mem);
    madam.write(MADAM_CEL_START, 0, &mut mem);
    madam.write(MADAM_CEL_STOP, 0, &mut mem);

    run_pending_cel_engine(&mut madam, &mut mem);
    assert_eq!(madam.stats().cels_drawn, 0);
}
