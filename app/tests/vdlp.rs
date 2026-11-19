//! VDLP tests: pixel conversion and display-list walking, ported from the C++
//! reference's tests/test_vdlp.cpp. The C++ `Bus` becomes plain `Memory`.
//!
//! Not ported here, because they need the whole console (CPU, CLIO's scanline
//! timing and MADAM's VDL address register):
//! `a_frame_from_the_console_is_black_before_anything_configures_video`,
//! `a_pattern_written_through_the_bus_comes_back_out_of_the_frame`,
//! `madam_and_the_display_agree_about_where_a_pixel_goes`.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::panic
)]

use retro3do::memory::{Memory, VRAM_BASE, VRAM_SIZE};
use retro3do::vdlp::{colour, control, display, word, Vdlp};
use retro3do::video::{expand_rgb555, framebuffer_offset};

/// Write one RGB555 pixel into VRAM, big-endian, as the hardware stores it.
fn put_pixel(mem: &mut Memory, vram_offset: u32, pixel: u16) {
    mem.write16(VRAM_BASE + vram_offset, pixel);
}

/// Write a pixel into a framebuffer at (x, y), through the interleaved layout.
fn put_fb(mem: &mut Memory, fb_vram_offset: u32, x: u32, y: u32, width: u32, pixel: u16) {
    mem.write16(
        VRAM_BASE + fb_vram_offset + framebuffer_offset(x, y, width),
        pixel,
    );
}

const fn rgb555(r: u32, g: u32, b: u32) -> u16 {
    ((r << 10) | (g << 5) | b) as u16
}

// ---------------------------------------------------------------------------
// Colour
// ---------------------------------------------------------------------------

#[test]
fn black_and_white_reach_the_ends_of_the_range() {
    assert_eq!(expand_rgb555(rgb555(0, 0, 0)), 0xff000000);
    // Full scale in must be full scale out: catches a naive 5-to-8-bit shift.
    assert_eq!(expand_rgb555(rgb555(31, 31, 31)), 0xffffffff);
}

#[test]
fn the_channels_do_not_get_swapped() {
    assert_eq!(expand_rgb555(rgb555(31, 0, 0)), 0xffff0000);
    assert_eq!(expand_rgb555(rgb555(0, 31, 0)), 0xff00ff00);
    assert_eq!(expand_rgb555(rgb555(0, 0, 31)), 0xff0000ff);
}

#[test]
fn mid_grey_lands_near_the_middle() {
    let grey = expand_rgb555(rgb555(16, 16, 16));
    let channel = grey & 0xff;
    assert!(channel > 0x7f);
    assert!(channel < 0x90);
    // All three channels must agree, or greys will have a colour cast.
    assert_eq!((grey >> 16) & 0xff, channel);
    assert_eq!((grey >> 8) & 0xff, channel);
}

// ---------------------------------------------------------------------------
// Reading VRAM
// ---------------------------------------------------------------------------

#[test]
fn a_linear_framebuffer_reads_out_in_the_right_order() {
    let mut mem = Memory::new();
    let vdlp = Vdlp::new();

    put_pixel(&mut mem, 0, rgb555(31, 0, 0));
    put_pixel(&mut mem, 2, rgb555(0, 31, 0));
    put_pixel(&mut mem, 4, rgb555(0, 0, 31));
    put_pixel(&mut mem, 6, rgb555(31, 31, 31));

    let mut out = [0u32; 4];
    vdlp.render_linear(&mut out, 4, 1, 0, &mem);

    assert_eq!(out[0], 0xffff0000);
    assert_eq!(out[1], 0xff00ff00);
    assert_eq!(out[2], 0xff0000ff);
    assert_eq!(out[3], 0xffffffff);
}

#[test]
fn reading_past_the_end_of_vram_gives_black_not_a_crash() {
    let mem = Memory::new();
    let vdlp = Vdlp::new();

    let mut out = [0u32; 8];
    // Start two pixels short of the end of VRAM and ask for eight.
    vdlp.render_linear(&mut out, 8, 1, VRAM_SIZE - 4, &mem);
    assert_eq!(out[7], 0xff000000);
}

// ---------------------------------------------------------------------------
// The display list
// ---------------------------------------------------------------------------

#[test]
fn no_display_list_gives_a_black_field_not_noise() {
    let mut mem = Memory::new();
    let mut vdlp = Vdlp::new();

    // Something bright at VRAM offset zero must not appear without a list.
    for i in (0..64).step_by(2) {
        put_pixel(&mut mem, i, rgb555(31, 0, 31));
    }

    let mut out = [0u32; 16];
    vdlp.render_field(&mut out, 4, 4, &mem);
    for pixel in out {
        assert_eq!(pixel, 0xff000000);
    }
    assert_eq!(vdlp.entries_walked(), 0);
}

#[test]
fn a_single_entry_covers_the_whole_field() {
    let mut mem = Memory::new();
    let mut vdlp = Vdlp::new();

    let list = 0x1000; // the VDL lives in DRAM
    let framebuffer = VRAM_BASE; // and points at the start of VRAM

    mem.write32(list, control::ENABLE_DMA | control::CURR_OVERRIDE | 1);
    mem.write32(list + 4, framebuffer);
    mem.write32(list + 8, framebuffer);
    mem.write32(list + 12, 0);

    put_fb(&mut mem, 0, 0, 0, 2, rgb555(31, 0, 0));
    put_fb(&mut mem, 0, 1, 0, 2, rgb555(0, 31, 0));
    put_fb(&mut mem, 0, 0, 1, 2, rgb555(0, 0, 31));
    put_fb(&mut mem, 0, 1, 1, 2, rgb555(31, 31, 31));

    vdlp.set_list_address(list);
    let mut out = [0u32; 4];
    vdlp.render_field(&mut out, 2, 2, &mem);

    assert_eq!(vdlp.entries_walked(), 1);
    assert_eq!(out[0], 0xffff0000);
    assert_eq!(out[1], 0xff00ff00);
    assert_eq!(out[2], 0xff0000ff);
    assert_eq!(out[3], 0xffffffff);
}

#[test]
fn an_entry_with_video_dma_disabled_renders_black() {
    let mut mem = Memory::new();
    let mut vdlp = Vdlp::new();

    let list = 0x1000;
    mem.write32(list, control::CURR_OVERRIDE);
    mem.write32(list + 4, VRAM_BASE);
    mem.write32(list + 8, VRAM_BASE);
    mem.write32(list + 12, 0);
    put_fb(&mut mem, 0, 0, 0, 1, rgb555(31, 31, 31));

    vdlp.set_list_address(list);
    let mut out = [0u32; 1];
    vdlp.render_field(&mut out, 1, 1, &mem);
    assert_eq!(out[0], 0xff000000);
}

#[test]
fn framebuffer_addresses_wrap_on_the_fitted_vram_address_lines() {
    let mut mem = Memory::new();
    let mut vdlp = Vdlp::new();

    let list = 0x1000;
    mem.write32(list, control::ENABLE_DMA | control::CURR_OVERRIDE);
    mem.write32(list + 4, VRAM_BASE + VRAM_SIZE);
    mem.write32(list + 8, VRAM_BASE + VRAM_SIZE);
    mem.write32(list + 12, 0);
    put_fb(&mut mem, 0, 0, 0, 1, rgb555(31, 0, 31));

    vdlp.set_list_address(list);
    let mut out = [0u32; 1];
    vdlp.render_field(&mut out, 1, 1, &mem);
    assert_eq!(out[0], 0xffff00ff);
}

#[test]
fn the_display_generator_interpolates_to_twice_the_resolution() {
    let mut mem = Memory::new();
    let mut vdlp = Vdlp::new();

    let list = 0x1000;
    // One display-control word enables both interpolation axes.
    mem.write32(
        list,
        control::ENABLE_DMA | control::CURR_OVERRIDE | (1 << control::WORD_COUNT_SHIFT) | 1,
    );
    mem.write32(list + 4, VRAM_BASE);
    mem.write32(list + 8, VRAM_BASE);
    mem.write32(list + 12, 0);
    mem.write32(
        list + 16,
        (word::DISPLAY << word::SELECTOR_SHIFT)
            | display::HORIZONTAL_INTERP
            | display::VERTICAL_INTERP,
    );

    put_fb(&mut mem, 0, 0, 0, 2, rgb555(31, 0, 0));
    put_fb(&mut mem, 0, 1, 0, 2, rgb555(0, 31, 0));
    put_fb(&mut mem, 0, 0, 1, 2, rgb555(0, 0, 31));
    put_fb(&mut mem, 0, 1, 1, 2, rgb555(31, 31, 31));

    vdlp.set_list_address(list);
    let mut out = [0u32; 16];
    vdlp.render_field_2x(&mut out, 2, 2, &mem);

    assert_eq!(out[0], 0xffff0000); // the cornerweight keeps the parent colour
    assert_eq!(out[1], 0xff7f7f00); // horizontal average
    assert_eq!(out[4], 0xff7f007f); // vertical average
    assert_eq!(out[5], 0xff7f7f7f); // four-pixel average
    assert_eq!(out[3], 0xff00ff00); // right edge clamps rather than wrapping
    assert_eq!(out[12], 0xff0000ff); // bottom edge clamps likewise
}

#[test]
fn a_displayed_scanline_is_not_changed_by_a_later_vram_write() {
    let mut mem = Memory::new();
    let mut vdlp = Vdlp::new();

    let list = 0x1000;
    mem.write32(list, control::ENABLE_DMA | control::CURR_OVERRIDE | 2);
    mem.write32(list + 4, VRAM_BASE);
    mem.write32(list + 8, VRAM_BASE);
    mem.write32(list + 12, 0);
    put_fb(&mut mem, 0, 0, 0, 1, rgb555(31, 0, 0));
    put_fb(&mut mem, 0, 0, 1, 1, rgb555(0, 31, 0));

    vdlp.set_list_address(list);
    vdlp.set_field_shape(Vdlp::LIST_START_LINE, Vdlp::LIST_START_LINE + 2);
    vdlp.begin_field(1, 2);
    vdlp.process_scanline(Vdlp::LIST_START_LINE, &mem);

    // The beam has passed the first line. Repainting its source now can only
    // affect a later field, while the second line has not yet been sampled.
    put_fb(&mut mem, 0, 0, 0, 1, rgb555(0, 0, 31));
    vdlp.process_scanline(Vdlp::LIST_START_LINE + 1, &mem);

    let mut out = [0u32; 8];
    vdlp.finish_field_2x(&mut out, 1, 2);
    assert_eq!(out[0], 0xffff0000);
    assert_eq!(out[4], 0xff00ff00);
}

#[test]
fn the_list_is_followed_to_a_second_entry() {
    let mut mem = Memory::new();
    let mut vdlp = Vdlp::new();

    let first = 0x1000;
    let second = 0x1100;

    // A persist count of one covers exactly one line.
    mem.write32(first, control::ENABLE_DMA | control::CURR_OVERRIDE | 1);
    mem.write32(first + 4, VRAM_BASE);
    mem.write32(first + 12, second);

    mem.write32(second, control::ENABLE_DMA | control::CURR_OVERRIDE | 1);
    mem.write32(second + 4, VRAM_BASE + 0x100);
    mem.write32(second + 12, 0);

    put_fb(&mut mem, 0, 0, 0, 1, rgb555(31, 0, 0));
    put_fb(&mut mem, 0x100, 0, 0, 1, rgb555(0, 0, 31));

    vdlp.set_list_address(first);
    let mut out = [0u32; 2];
    vdlp.render_field(&mut out, 1, 2, &mem);

    assert_eq!(vdlp.entries_walked(), 2);
    assert_eq!(out[0], 0xffff0000);
    assert_eq!(out[1], 0xff0000ff);
}

#[test]
fn a_list_that_points_at_itself_terminates() {
    let mut mem = Memory::new();
    let mut vdlp = Vdlp::new();

    let list = 0x1000;
    mem.write32(list, control::CURR_OVERRIDE);
    mem.write32(list + 4, VRAM_BASE);
    mem.write32(list + 12, list); // next points back at itself

    vdlp.set_list_address(list);
    let mut out = [0u32; 4];
    vdlp.render_field(&mut out, 2, 2, &mem);
    assert_eq!(vdlp.entries_walked(), 1);
}

#[test]
fn an_entry_claiming_no_lines_does_not_spin() {
    let mut mem = Memory::new();
    let mut vdlp = Vdlp::new();

    let list = 0x1000;
    mem.write32(list, control::CURR_OVERRIDE);
    mem.write32(list + 4, VRAM_BASE);
    mem.write32(list + 12, list); // and loops

    vdlp.set_list_address(list);
    let mut out = [0u32; 16];
    vdlp.render_field(&mut out, 4, 4, &mem);
    assert_eq!(vdlp.entries_walked(), 1);
}

// ---------------------------------------------------------------------------
// The framebuffer layout
// ---------------------------------------------------------------------------

#[test]
fn the_framebuffer_is_interleaved_by_line_pairs() {
    let width = 320;

    // Same x, adjacent lines: two halves of one word, two bytes apart.
    assert_eq!(framebuffer_offset(0, 0, width), 0);
    assert_eq!(framebuffer_offset(0, 1, width), 2);

    // Adjacent x on one line: four bytes apart, not two.
    assert_eq!(framebuffer_offset(1, 0, width), 4);
    assert_eq!(framebuffer_offset(1, 1, width), 6);

    // A pair of lines occupies width words.
    assert_eq!(framebuffer_offset(0, 2, width), width * 4);
    assert_eq!(framebuffer_offset(0, 3, width), width * 4 + 2);
}

#[test]
fn reading_the_framebuffer_linearly_is_the_bug_that_looks_like_a_stride_error() {
    let mut mem = Memory::new();
    let mut vdlp = Vdlp::new();

    const WIDTH: u32 = 4;
    const HEIGHT: u32 = 2;

    // A single red pixel at (2, 1) - an odd line, so the low half of a word.
    put_pixel(&mut mem, framebuffer_offset(2, 1, WIDTH), rgb555(31, 0, 0));

    let list = 0x1000;
    mem.write32(
        list,
        control::ENABLE_DMA | control::CURR_OVERRIDE | (HEIGHT - 1),
    );
    mem.write32(list + 4, VRAM_BASE);
    mem.write32(list + 12, 0);
    vdlp.set_list_address(list);

    let mut out = [0u32; (WIDTH * HEIGHT) as usize];
    vdlp.render_field(&mut out, WIDTH, HEIGHT, &mem);

    assert_eq!(out[(WIDTH + 2) as usize], 0xffff0000);
    // And nowhere else.
    let lit = out.iter().filter(|&&p| p & 0xffffff != 0).count();
    assert_eq!(lit, 1);
}

#[test]
fn the_display_list_can_reprogram_the_output_colour_table() {
    let mut mem = Memory::new();
    let mut vdlp = Vdlp::new();

    put_fb(&mut mem, 0, 0, 0, 1, rgb555(31, 0, 0));

    let list = 0x1000;
    // One entry, one line, carrying two colour words behind its header.
    mem.write32(
        list,
        control::ENABLE_DMA | control::CURR_OVERRIDE | (2 << control::WORD_COUNT_SHIFT),
    );
    mem.write32(list + 4, VRAM_BASE);
    mem.write32(list + 8, VRAM_BASE);
    mem.write32(list + 12, 0);
    // Slot 31 of red becomes half brightness; the enable field of zero means
    // all three channels, so green and blue go dark with it.
    mem.write32(
        list + 16,
        (31 << colour::ADDR_SHIFT) | (0x80 << colour::RED_SHIFT),
    );
    // And the same slot again, red only this time.
    mem.write32(
        list + 20,
        (3 << colour::ENABLE_SHIFT) | (31 << colour::ADDR_SHIFT) | (0x40 << colour::RED_SHIFT),
    );

    vdlp.set_list_address(list);
    let mut out = [0u32; 1];
    vdlp.render_field(&mut out, 1, 1, &mem);

    // Red at full scale now reads slot 31, which the second word left at 0x40.
    assert_eq!((out[0] >> 16) & 0xff, 0x40);
}

#[test]
fn a_pixel_of_zero_shows_the_programmed_background() {
    let mut mem = Memory::new();
    let mut vdlp = Vdlp::new();

    put_fb(&mut mem, 0, 0, 0, 1, 0);

    let list = 0x1000;
    mem.write32(
        list,
        control::ENABLE_DMA | control::CURR_OVERRIDE | (1 << control::WORD_COUNT_SHIFT),
    );
    mem.write32(list + 4, VRAM_BASE);
    mem.write32(list + 8, VRAM_BASE);
    mem.write32(list + 12, 0);
    mem.write32(
        list + 16,
        (word::BACKGROUND << word::SELECTOR_SHIFT) | 0x00336699,
    );

    vdlp.set_list_address(list);
    let mut out = [0u32; 1];
    vdlp.render_field(&mut out, 1, 1, &mem);
    assert_eq!(out[0], 0xff336699);
}

// ---------------------------------------------------------------------------
// Additions for the Rust rewrite (not in the C++ suite). Expected values were
// cross-checked against the C++ reference build.
// ---------------------------------------------------------------------------

/// The console's drive sequence (NTSC shape 21..262, a 320x240 field) without
/// the console: the list runs from line 5, so the screen starts 16 lines into
/// the buffer.
#[test]
fn a_console_shaped_field_starts_the_picture_part_way_into_the_buffer() {
    let mut mem = Memory::new();
    let mut vdlp = Vdlp::new();
    vdlp.set_field_shape(21, 262);
    assert_eq!(vdlp.buffer_start_line(), 16);

    for y in 0..262 {
        put_fb(&mut mem, 0, 0, y, 320, rgb555(0, y % 32, 1));
    }
    let list = 0x2000;
    mem.write32(list, control::ENABLE_DMA | control::CURR_OVERRIDE | 261);
    mem.write32(list + 4, VRAM_BASE);
    mem.write32(list + 12, 0);
    vdlp.set_list_address(list);

    vdlp.begin_field(320, 240);
    for line in 0..300 {
        vdlp.process_scanline(line, &mem);
    }
    let mut out = vec![0u32; 640 * 480];
    vdlp.finish_field_2x(&mut out, 320, 240);

    assert_eq!(vdlp.entries_walked(), 1);
    // Output line 0 is buffer line 16; output line 100 is buffer line 116.
    assert_eq!(out[0], expand_rgb555(rgb555(0, 16, 1)));
    assert_eq!(out[200 * 640], expand_rgb555(rgb555(0, 116 % 32, 1)));
}

/// Guest-controlled garbage must never panic: a list of entries each linking
/// to the next, longer than the entry cap, and a short output slice.
#[test]
fn a_runaway_list_and_a_short_output_slice_are_survived() {
    let mut mem = Memory::new();
    let mut vdlp = Vdlp::new();
    for i in 0..1100u32 {
        let at = 0x1000 + i * 16;
        mem.write32(
            at,
            control::ENABLE_DMA | control::CURR_OVERRIDE | (63 << control::WORD_COUNT_SHIFT) | 1,
        );
        mem.write32(at + 4, 0xffff_fffe);
        mem.write32(at + 12, at + 16);
    }
    vdlp.set_list_address(0x1000);
    vdlp.set_field_shape(Vdlp::LIST_START_LINE, 2000);
    vdlp.begin_field(8, 8);
    for line in 0..2000 {
        vdlp.process_scanline(line, &mem);
    }
    assert_eq!(vdlp.entries_walked(), Vdlp::MAX_ENTRIES);
    let mut short = [0u32; 37];
    vdlp.finish_field_2x(&mut short, 8, 8);
    vdlp.render_linear(&mut short, 100, 100, u32::MAX, &mem);
    assert_eq!(short[0], 0xff000000);
}

#[test]
fn reset_restores_the_ramp_colour_table() {
    let vdlp = Vdlp::new();
    assert_eq!(vdlp.clut_red(0), 0);
    assert_eq!(vdlp.clut_green(16), 132);
    assert_eq!(vdlp.clut_blue(31), 255);
}
