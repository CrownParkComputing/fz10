//! Internal-resolution cel rendering: MADAM rasterizing every cel a second
//! time on a grid `k` times finer, into hi-res mirrors of the framebuffers,
//! and the display sampling them.
//!
//! What must hold: at a scale of one nothing changes at all; above one, VRAM
//! (everything the machine can observe) is still exactly native, while the
//! mirror resolves cel edges at 1/k of a pixel (a real re-rasterization, not a
//! nearest-neighbour blow-up of the native picture); and pixels anything else
//! writes into a mirrored framebuffer show up in the mirror.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::panic
)]

use retro3do::hires::HiResSurface;
use retro3do::madam::*;
use retro3do::memory::{Memory, VRAM_BASE};
use retro3do::vdlp::{control, Vdlp};
use retro3do::video::framebuffer_offset;
use retro3do::Console;

const fn fixed(whole: i32) -> i32 {
    whole << 16
}

const WHITE: u16 = 0x7fff;
const RED: u16 = 0x7c00;

const CCB_AT: u32 = 0x1000;
const SOURCE_AT: u32 = 0x2000;

/// "Source times one, nothing from the frame": the PIXC titles use for
/// opaque cels, in both halves.
const PIXC_OPAQUE: u32 = 0x1f00_1f00;

/// A CCB with absolute pointers, every load flag, the preamble in the CCB and
/// both windings permitted. Steps are 16.16; horizontal ones are stored 12.20.
fn write_ccb(
    mem: &mut Memory,
    (x, y): (i32, i32),
    (hdx, hdy): (i32, i32),
    (vdx, vdy): (i32, i32),
    size: (u32, u32),
) {
    let (width, height) = size;
    let flags = CCB_LAST
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
    let row_words = (width * 16).div_ceil(32).max(2);
    let pre0 = 6 | PRE0_LINEAR | ((height - 1) << 6);
    let pre1 = (width - 1) | ((row_words - 2) << 16) | 0x0000_1000;
    let words: [u32; 15] = [
        flags,
        0,
        SOURCE_AT,
        0,
        x as u32,
        y as u32,
        hdx.wrapping_shl(4) as u32,
        hdy.wrapping_shl(4) as u32,
        vdx as u32,
        vdy as u32,
        0,
        0,
        PIXC_OPAQUE,
        pre0,
        pre1,
    ];
    for (i, word) in words.iter().enumerate() {
        mem.write32(CCB_AT + i as u32 * 4, *word);
    }
    for row in 0..height {
        for column in 0..width {
            mem.write16(SOURCE_AT + row * row_words * 4 + column * 2, WHITE);
        }
    }
}

/// MADAM drawing through the pixel processor into a 320x240 field at the
/// start of VRAM, as a title programs it.
fn configured(scale: u32) -> (Memory, Madam) {
    let mut mem = Memory::new();
    let mut madam = Madam::new();
    madam.set_internal_scale(scale);
    // 0x50: 1280 bytes between row pairs, for both the read and write side.
    madam.write(MADAM_REG_CTL0, 0x0000_5050, &mut mem);
    madam.write(MADAM_REG_CTL1, 0x00ef_013f, &mut mem);
    madam.write(MADAM_REG_CTL2, VRAM_BASE, &mut mem);
    madam.write(MADAM_REG_CTL3, VRAM_BASE, &mut mem);
    (mem, madam)
}

fn native(mem: &Memory, x: u32, y: u32) -> u16 {
    mem.read16(VRAM_BASE + framebuffer_offset(x, y, 320))
}

fn only_surface(madam: &Madam) -> &HiResSurface {
    let surfaces = madam.hires().surfaces();
    assert_eq!(surfaces.len(), 1, "one framebuffer drawn, one surface");
    &surfaces[0]
}

/// Forty-five degrees, scaled by two: a diamond with corners (40,40),
/// (48,48), (40,56) and (32,48), whose true area is 128 pixels.
fn draw_diamond(scale: u32) -> (Memory, Madam) {
    let (mut mem, mut madam) = configured(scale);
    let step = fixed(2);
    write_ccb(
        &mut mem,
        (fixed(40), fixed(40)),
        (step, step),
        (-step, step),
        (4, 4),
    );
    madam.render_cel_list(CCB_AT, &mut mem);
    (mem, madam)
}

// ---------------------------------------------------------------------------
// The machine cannot tell
// ---------------------------------------------------------------------------

#[test]
fn scale_one_is_off_and_keeps_no_surfaces() {
    let (_, madam) = draw_diamond(1);
    assert_eq!(madam.internal_scale(), 1);
    assert!(madam.hires().surfaces().is_empty());
}

#[test]
fn vram_is_identical_at_every_scale() {
    let (reference, reference_madam) = draw_diamond(1);
    for scale in 2..=4 {
        let (mem, madam) = draw_diamond(scale);
        assert!(mem.vram() == reference.vram(), "VRAM differs at {scale}x");
        assert_eq!(
            madam.stats(),
            reference_madam.stats(),
            "engine statistics differ at {scale}x"
        );
    }
}

#[test]
fn the_scale_is_clamped_and_survives_a_reset() {
    let mut madam = Madam::new();
    madam.set_internal_scale(0);
    assert_eq!(madam.internal_scale(), 1);
    madam.set_internal_scale(9);
    assert_eq!(madam.internal_scale(), 4);
    madam.set_internal_scale(3);
    madam.reset();
    assert_eq!(madam.internal_scale(), 3);
}

// ---------------------------------------------------------------------------
// Edges re-resolve
// ---------------------------------------------------------------------------

/// Hi-res pixels painted, and how far outside the true figure the worst one's
/// centre lies, in native pixels.
fn diamond_coverage(surface: &HiResSurface) -> (u32, f64) {
    let k = f64::from(surface.scale());
    let mut painted = 0;
    let mut worst: f64 = 0.0;
    for y in 0..surface.hires_height() {
        for x in 0..surface.hires_width() {
            if surface.pixel(x, y) == Some(0) {
                continue;
            }
            painted += 1;
            let (cx, cy) = ((f64::from(x) + 0.5) / k, (f64::from(y) + 0.5) / k);
            worst = worst.max((cx - 40.0).abs() + (cy - 48.0).abs() - 8.0);
        }
    }
    (painted, worst)
}

#[test]
fn a_rotated_cel_covers_k_squared_the_pixels_and_hugs_its_true_edges() {
    let (mem, _) = draw_diamond(1);
    let native_painted = (0..240)
        .flat_map(|y| (0..320).map(move |x| (x, y)))
        .filter(|&(x, y)| native(&mem, x, y) != 0)
        .count();
    let native_error = (native_painted as f64 - 128.0).abs() / 128.0;

    for k in [2u32, 4] {
        let (_, madam) = draw_diamond(k);
        let surface = only_surface(&madam);
        let (painted, worst) = diamond_coverage(surface);
        let area = f64::from(painted) / f64::from(k * k);
        // The edge rule paints both intersections, one grid cell wide: at a
        // finer grid that extra sliver is thinner, so the area converges on
        // the true 128.
        let error = (area - 128.0).abs() / 128.0;
        assert!(
            error < native_error,
            "{k}x: area {area} is no closer to 128 than native {native_painted}"
        );
        assert!(error < 0.25, "{k}x: area {area} too far from 128");
        // Every painted cell sits on the figure: within about one grid cell
        // of its edge, measured in native pixels.
        assert!(
            worst <= 2.0 / f64::from(k),
            "{k}x: a pixel lands {worst} px outside the figure"
        );
    }
}

#[test]
fn hi_res_output_is_not_a_blow_up_of_the_native_picture() {
    let (mem, _) = draw_diamond(4);
    let (_, madam) = draw_diamond(4);
    let surface = only_surface(&madam);
    let k = surface.scale();
    let mut differs = 0;
    for y in 0..surface.hires_height() {
        for x in 0..surface.hires_width() {
            if surface.pixel(x, y) != Some(native(&mem, x / k, y / k)) {
                differs += 1;
            }
        }
    }
    // A diagonal edge staircases at 1/4 pixel instead of whole pixels.
    assert!(
        differs > 64,
        "only {differs} hi-res pixels differ from the native ones under them"
    );
}

#[test]
fn a_fractional_position_is_kept_at_the_finer_grid() {
    // Three texels 1.5 pixels wide from x = 10.5, two rows tall from y = 4:
    // natively columns 10..15, at 2x exactly 21..30 (a nearest blow-up would
    // be 20..30).
    let (mut mem, mut madam) = configured(2);
    write_ccb(
        &mut mem,
        (fixed(10) + 0x8000, fixed(4)),
        (0x18000, 0),
        (0, fixed(2)),
        (3, 1),
    );
    madam.render_cel_list(CCB_AT, &mut mem);

    let painted_native: Vec<u32> = (0..320).filter(|&x| native(&mem, x, 4) != 0).collect();
    assert_eq!(painted_native, (10..15).collect::<Vec<_>>());

    let surface = only_surface(&madam);
    for y in 8..12 {
        let painted: Vec<u32> = (0..surface.hires_width())
            .filter(|&x| surface.pixel(x, y) != Some(0))
            .collect();
        assert_eq!(painted, (21..30).collect::<Vec<_>>(), "hi-res row {y}");
    }
    assert!(
        (0..surface.hires_width()).all(|x| surface.pixel(x, 12) == Some(0)),
        "nothing below the cel"
    );
}

#[test]
fn a_one_to_one_cel_fills_whole_blocks() {
    let (mut mem, mut madam) = configured(3);
    write_ccb(
        &mut mem,
        (fixed(5), fixed(6)),
        (fixed(1), 0),
        (0, fixed(1)),
        (2, 2),
    );
    madam.render_cel_list(CCB_AT, &mut mem);
    let surface = only_surface(&madam);
    for y in 0..30 {
        for x in 0..30 {
            let inside = (15..21).contains(&x) && (18..24).contains(&y);
            assert_eq!(surface.pixel(x, y) != Some(0), inside, "hi-res {x},{y}");
        }
    }
}

/// A list of many overlapping cels, rotated, scaled, one-to-one and
/// translucent, big enough to be worth a helper thread.
fn draw_busy_list(threshold: usize) -> (Memory, Madam) {
    let (mut mem, mut madam) = configured(3);
    madam.set_hires_worker_threshold(threshold);
    type Shape = ((i32, i32), (i32, i32), (i32, i32), u32);
    let shapes: [Shape; 4] = [
        (
            (fixed(160), fixed(20)),
            (0x1_8000, 0x8000),
            (-0x8000, 0x1_4000),
            PIXC_OPAQUE,
        ),
        (
            (fixed(30) + 0x4000, fixed(100)),
            (0x1_8000, 0),
            (0, 0x1_8000),
            PIXC_OPAQUE,
        ),
        (
            (fixed(200), fixed(150)),
            (fixed(1), 0),
            (0, fixed(1)),
            PIXC_OPAQUE,
        ),
        // Half source plus half frame: blended against VRAM by the walk.
        (
            (fixed(120), fixed(60)),
            (0x1_2000, -0x6000),
            (0x6000, 0x1_2000),
            0x1f81_1f81,
        ),
    ];
    for (round, &(position, h, v, pixc)) in shapes.iter().cycle().take(12).enumerate() {
        let nudge = (round as i32) << 14;
        write_ccb(
            &mut mem,
            (position.0 + nudge, position.1 + nudge),
            h,
            v,
            (64, 64),
        );
        mem.write32(CCB_AT + 48, pixc);
        // A colour per round, so overdraw order shows.
        for i in 0..64 * 64 {
            mem.write16(SOURCE_AT + i * 2, 0x0421 * (round as u16 % 31 + 1));
        }
        madam.render_cel_list(CCB_AT, &mut mem);
    }
    (mem, madam)
}

#[test]
fn the_helper_thread_draws_exactly_what_the_walk_would() {
    let (inline_mem, inline) = draw_busy_list(usize::MAX);
    let (helper_mem, helper) = draw_busy_list(0);
    assert!(inline_mem.vram() == helper_mem.vram());
    let (a, b) = (only_surface(&inline), only_surface(&helper));
    let mut painted = 0;
    for y in 0..a.hires_height() {
        assert_eq!(a.row(y), b.row(y), "hi-res row {y}");
        painted += a.row(y).iter().filter(|&&p| p != 0).count();
    }
    assert!(
        painted > 20_000,
        "the list drew only {painted} hi-res pixels"
    );
}

// ---------------------------------------------------------------------------
// Writes from anything else reach the mirror
// ---------------------------------------------------------------------------

#[test]
fn a_new_surface_starts_from_what_vram_already_shows() {
    let (mut mem, mut madam) = configured(2);
    mem.write16(VRAM_BASE + framebuffer_offset(100, 101, 320), RED);
    write_ccb(
        &mut mem,
        (fixed(10), fixed(10)),
        (fixed(1), 0),
        (0, fixed(1)),
        (1, 1),
    );
    madam.render_cel_list(CCB_AT, &mut mem);
    let surface = only_surface(&madam);
    for (x, y) in [(200, 202), (201, 202), (200, 203), (201, 203)] {
        assert_eq!(surface.pixel(x, y), Some(RED));
    }
}

#[test]
fn a_cpu_write_replaces_only_its_own_block() {
    let (mut mem, mut madam) = configured(2);
    write_ccb(
        &mut mem,
        (fixed(10) + 0x8000, fixed(4)),
        (0x18000, 0),
        (0, fixed(2)),
        (3, 1),
    );
    madam.render_cel_list(CCB_AT, &mut mem);

    // The CPU writes one pixel in the same row pair (and so the same VRAM
    // page) as the cel's hi-res edge.
    mem.write16(VRAM_BASE + framebuffer_offset(5, 5, 320), RED);
    // The display reaching that line brings the mirror up to date.
    let line = madam
        .hires_mut()
        .line_source(framebuffer_offset(0, 5, 320), 320, &mem)
        .expect("the line is mirrored");
    assert_eq!((line.x, line.y), (0, 5));

    let surface = only_surface(&madam);
    for (x, y) in [(10, 10), (11, 10), (10, 11), (11, 11)] {
        assert_eq!(
            surface.pixel(x, y),
            Some(RED),
            "the CPU's pixel at hi-res {x},{y}"
        );
    }
    // The cel's hi-res edge next door is untouched: column 20 stays empty
    // although native column 10 under it is painted.
    assert_ne!(native(&mem, 10, 4), 0);
    assert_eq!(surface.pixel(20, 8), Some(0));
    assert_ne!(surface.pixel(21, 8), Some(0));
}

#[test]
fn a_cpu_write_before_the_next_list_is_drawn_over() {
    let (mut mem, mut madam) = configured(2);
    write_ccb(
        &mut mem,
        (fixed(10), fixed(10)),
        (fixed(1), 0),
        (0, fixed(1)),
        (1, 1),
    );
    madam.render_cel_list(CCB_AT, &mut mem);
    // A clear of the whole buffer, as SPORT does between frames.
    for y in 0..240 {
        for x in 0..320 {
            mem.write16(VRAM_BASE + framebuffer_offset(x, y, 320), 0x0001);
        }
    }
    write_ccb(
        &mut mem,
        (fixed(50), fixed(50)),
        (fixed(1), 0),
        (0, fixed(1)),
        (1, 1),
    );
    madam.render_cel_list(CCB_AT, &mut mem);
    let surface = only_surface(&madam);
    assert_eq!(
        surface.pixel(20, 20),
        Some(0x0001),
        "the clear reached the mirror"
    );
    assert_eq!(
        surface.pixel(100, 100),
        Some(native(&mem, 50, 50)),
        "the new cel is drawn"
    );
}

#[test]
fn an_unmirrored_buffer_has_no_line_source() {
    let (mut mem, mut madam) = configured(2);
    write_ccb(
        &mut mem,
        (fixed(10), fixed(10)),
        (fixed(1), 0),
        (0, fixed(1)),
        (1, 1),
    );
    madam.render_cel_list(CCB_AT, &mut mem);
    // Half a megabyte on: no cel ever drew there.
    assert!(madam.hires_mut().line_source(0x8_0000, 320, &mem).is_none());
}

// ---------------------------------------------------------------------------
// The display
// ---------------------------------------------------------------------------

/// A one-entry display list showing the buffer at the start of VRAM.
fn show_vram_start(mem: &mut Memory) -> u32 {
    let list = 0x1000;
    mem.write32(list, control::ENABLE_DMA | control::CURR_OVERRIDE | 0x1ff);
    mem.write32(list + 4, VRAM_BASE);
    mem.write32(list + 8, VRAM_BASE);
    mem.write32(list + 12, 0);
    list
}

#[test]
fn an_unmirrored_field_is_the_native_one_repeated() {
    let mut mem = Memory::new();
    for y in 0..8 {
        for x in 0..8 {
            mem.write16(
                VRAM_BASE + framebuffer_offset(x, y, 8),
                ((x * 3 + y * 5) & 0x1f) as u16 * 0x421,
            );
        }
    }
    let list = show_vram_start(&mut mem);
    let mut native_vdlp = Vdlp::new();
    native_vdlp.set_list_address(list);
    let mut native_out = vec![0u32; 16 * 16];
    native_vdlp.render_field_2x(&mut native_out, 8, 8, &mem);

    for k in 2..=4u32 {
        let mut vdlp = Vdlp::new();
        vdlp.set_internal_scale(k);
        vdlp.set_list_address(list);
        let mut ignored = vec![0u32; 16 * 16];
        vdlp.render_field_2x(&mut ignored, 8, 8, &mem);
        let side = 16 * k;
        let mut out = vec![0u32; (side * side) as usize];
        vdlp.finish_field_hires(&mut out, 8, 8);
        for y in 0..side {
            for x in 0..side {
                let expected = native_out[((y / k) * 16 + x / k) as usize];
                assert_eq!(
                    out[(y * side + x) as usize],
                    expected,
                    "{k}x output pixel {x},{y}"
                );
            }
        }
    }
}

/// The 2x output of a console at `scale`, after `fields` of the demo.
fn demo_frame(scale: u32, fields: u32) -> (Vec<u32>, usize, usize) {
    let mut console = Console::new();
    assert!(console.load_builtin_demo());
    console.set_internal_scale(scale);
    console.reset();
    for _ in 0..fields {
        console.run_frame();
    }
    let frame = console.framebuffer();
    (frame.pixels.to_vec(), frame.width, frame.height)
}

#[test]
fn the_console_publishes_frames_k_times_larger() {
    for k in 1..=4u32 {
        let (pixels, width, height) = demo_frame(k, 4);
        assert_eq!((width, height), (640 * k as usize, 480 * k as usize));
        assert_eq!(pixels.len(), width * height);
    }
}

#[test]
fn switching_scale_mid_run_leaves_the_machine_where_it_was() {
    let (reference, _, _) = demo_frame(1, 90);
    let mut console = Console::new();
    assert!(console.load_builtin_demo());
    console.reset();
    for field in 0..90 {
        console.set_internal_scale(match field {
            0..30 => 2,
            30..60 => 4,
            _ => 1,
        });
        console.run_frame();
    }
    assert!(
        console.framebuffer().pixels == reference.as_slice(),
        "back at 1x the picture is the native one"
    );
}

#[test]
fn a_hi_res_demo_field_matches_the_native_one_on_average() {
    // Downsampled, the 2x picture should be the native one apart from edges.
    let (native_frame, width, height) = demo_frame(1, 120);
    let (hires, hires_width, _) = demo_frame(2, 120);
    let mut same = 0usize;
    for y in 0..height {
        for x in 0..width {
            if hires[(y * 2) * hires_width + x * 2] == native_frame[y * width + x] {
                same += 1;
            }
        }
    }
    assert!(
        same * 100 >= width * height * 90,
        "only {same} of {} pixels agree",
        width * height
    );
    assert!(
        native_frame.iter().any(|&p| p != 0xff00_0000),
        "the demo draws something"
    );
}
