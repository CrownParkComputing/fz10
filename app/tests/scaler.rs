//! Presentation scaling: the filters the host can apply to the output signal.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::panic
)]

use retro3do::scaler::{self, VideoFilter};
use retro3do::Retro3do;

fn px(r: u32, g: u32, b: u32) -> u32 {
    0xff00_0000 | (r << 16) | (g << 8) | b
}

fn run(
    filter: VideoFilter,
    scale: u32,
    src: &[u32],
    w: usize,
    h: usize,
) -> (Vec<u32>, usize, usize) {
    let mut dst = Vec::new();
    let mut tmp = Vec::new();
    let (dw, dh) = scaler::apply(filter, scale, src, w, h, &mut dst, &mut tmp);
    (dst, dw, dh)
}

// --- sizes -----------------------------------------------------------------

#[test]
fn every_filter_honours_the_requested_factor() {
    let src = vec![px(10, 20, 30); 8 * 6];
    for filter in VideoFilter::ALL {
        for scale in [1u32, 2, 3, 5] {
            let (dst, dw, dh) = run(filter, scale, &src, 8, 6);
            assert_eq!(
                (dw, dh),
                (8 * scale as usize, 6 * scale as usize),
                "{filter:?} {scale}x"
            );
            assert_eq!(dst.len(), dw * dh);
        }
    }
}

#[test]
fn scale_clamps_to_the_maximum() {
    assert_eq!(scaler::output_size(4, 3, 999), (4 * 16, 3 * 16));
    assert_eq!(scaler::output_size(4, 3, 0), (4, 3));
}

// --- nearest and resamplers ------------------------------------------------

#[test]
fn nearest_repeats_each_pixel() {
    let src = vec![px(255, 0, 0), px(0, 255, 0)];
    let (dst, dw, _) = run(VideoFilter::Nearest, 3, &src, 2, 1);
    assert_eq!(dw, 6);
    assert_eq!(&dst[..3], &[px(255, 0, 0); 3]);
    assert_eq!(&dst[3..6], &[px(0, 255, 0); 3]);
}

#[test]
fn bilinear_blends_the_middle() {
    let src = vec![px(0, 0, 0), px(200, 0, 0)];
    let (dst, ..) = run(VideoFilter::Bilinear, 2, &src, 2, 1);
    // Centres land between the two sources: quarters of the difference.
    assert_eq!(dst[0], px(0, 0, 0));
    assert_eq!(dst[1], px(50, 0, 0));
    assert_eq!(dst[2], px(150, 0, 0));
    assert_eq!(dst[3], px(200, 0, 0));
}

#[test]
fn lanczos_preserves_flat_colour() {
    let src = vec![px(120, 60, 30); 6 * 6];
    let (dst, dw, dh) = run(VideoFilter::Lanczos, 2, &src, 6, 6);
    // Away from the edges a flat field must stay flat.
    let mid = dst[dh / 2 * dw + dw / 2];
    let (r, g, b) = ((mid >> 16) & 0xff, (mid >> 8) & 0xff, mid & 0xff);
    assert!(
        (r as i32 - 120).abs() <= 1 && (g as i32 - 60).abs() <= 1 && (b as i32 - 30).abs() <= 1
    );
}

#[test]
fn lanczos_smooths_a_hard_step() {
    let mut src = vec![px(0, 0, 0); 8];
    for p in src.iter_mut().skip(4) {
        *p = px(255, 255, 255);
    }
    let (dst, ..) = run(VideoFilter::Lanczos, 2, &src, 8, 1);
    // The step's immediate neighbours should be blended, not binary.
    let mid_l = dst[7] & 0xff;
    let mid_r = dst[8] & 0xff;
    assert!(mid_l > 0 && mid_r < 255, "lanczos must soften the edge");
}

// --- edge scalers ----------------------------------------------------------

#[test]
fn scale2x_keeps_flat_areas_flat() {
    let src = vec![px(9, 9, 9); 4 * 4];
    let (dst, dw, dh) = run(VideoFilter::Scale2x, 2, &src, 4, 4);
    assert_eq!((dw, dh), (8, 8));
    assert!(dst.iter().all(|&p| p == px(9, 9, 9)));
}

#[test]
fn scale2x_continues_a_diagonal_edge() {
    // A diagonal: red above-left, blue below-right.
    //   R R B B
    //   R R B B
    //   B B R R
    //   B B R R
    let (r, b) = (px(200, 0, 0), px(0, 0, 200));
    let src = vec![r, r, b, b, r, r, b, b, b, b, r, r, b, b, r, r];
    let (dst, dw, _) = run(VideoFilter::Scale2x, 2, &src, 4, 4);
    // At the pixel just right of centre top, the upper-right quarter takes the
    // right-hand colour: B==F continuing the diagonal.
    let corner = dst[2 * dw + 5];
    assert_eq!(corner, b);
}

#[test]
fn scale3x_produces_nine_cells() {
    let src = vec![px(7, 7, 7); 3 * 3];
    let (dst, dw, dh) = run(VideoFilter::Scale3x, 3, &src, 3, 3);
    assert_eq!((dw, dh), (9, 9));
    assert!(dst.iter().all(|&p| p == px(7, 7, 7)));
}

#[test]
fn eagle_fills_agreeing_corners() {
    // A solid red block with one blue pixel at top-left inside a 1px border.
    let (r, b) = (px(200, 0, 0), px(0, 0, 200));
    let src = vec![b, r, r, r, r, r, r, r, r];
    let (dst, dw, _) = run(VideoFilter::Eagle, 2, &src, 3, 3);
    // Output (0,0) corner of pixel (0,0): S0==S1 fails so stays blue… centre
    // pixel (1,1) is surrounded by red, so all four of its quarters are red.
    for oy in 2..4 {
        for ox in 2..4 {
            assert_eq!(dst[oy * dw + ox], r);
        }
    }
}

#[test]
fn an_edge_scaler_tops_up_to_any_factor() {
    let mut src = vec![px(10, 10, 10); 4 * 4];
    src[0] = px(250, 0, 0);
    // 5x on a natural-2x filter: two passes' worth plus nearest top-up.
    let (dst, dw, dh) = run(VideoFilter::Scale2x, 5, &src, 4, 4);
    assert_eq!((dw, dh), (20, 20));
    assert_eq!(dst[0], px(250, 0, 0));
}

// --- through the machine ---------------------------------------------------

#[test]
fn video_setting_is_remembered_and_clamped() {
    let emu = Retro3do::new();
    assert_eq!(emu.video(), (VideoFilter::Nearest, 1));
    emu.set_video(VideoFilter::Lanczos, 99);
    assert_eq!(emu.video(), (VideoFilter::Lanczos, scaler::MAX_SCALE));
    emu.set_video(VideoFilter::Scale2x, 0);
    assert_eq!(emu.video(), (VideoFilter::Scale2x, 1));
}
