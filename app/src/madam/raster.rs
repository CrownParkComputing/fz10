//! Where a source texel lands, and which destination pixels it covers.
//!
//! The engine walks SOURCE pixels and steps the destination position by the
//! CCB's vectors (WO 94/10644): HDX/HDY per column, VDX/VDY per row, and
//! HDDX/HDDY bending the column step from one row to the next. Positions are
//! advanced incrementally, never multiplied per pixel, and wrap as 32-bit
//! registers do.
//!
//! Each texel is then filled by one of the hardware's three mappers. They are
//! not variations on one another: a 1:1 texel is one pixel, a scaled texel is
//! the rectangle out to where the next one lands, and a rotated or perspective
//! texel is a four-sided figure scan-converted with the CEL edge rules.
#![forbid(unsafe_code)]

use super::ccb::{Ccb, CCB_ACCW, CCB_ACW};

/// Which texel mapper a cel uses, decided once per cel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Mapper {
    /// Unscaled and unrotated on whole pixels: one source pixel, one write.
    #[default]
    Line,
    /// Axis-aligned but scaled: a rectangle per texel.
    Scale,
    /// Rotated, sheared or perspective-stepped: a quad per texel.
    Arbitrary,
}

impl Mapper {
    pub(super) fn for_cel(ccb: &Ccb) -> Self {
        let one_to_one = ccb.hdy == 0
            && ccb.vdx == 0
            && (ccb.hdx == 0x10000 || ccb.hdx == -0x10000)
            && (ccb.vdy == 0x10000 || ccb.vdy == -0x10000)
            && (ccb.x | ccb.y) & 0xffff == 0;
        if !ccb.is_axis_aligned() {
            Self::Arbitrary
        } else if one_to_one {
            Self::Line
        } else {
            Self::Scale
        }
    }
}

/// The row-to-row state of the walk: where each source row starts and the
/// column step it uses.
#[derive(Clone, Copy, Debug)]
pub(super) struct RowWalker {
    x: i32,
    y: i32,
    step_x: i32,
    step_y: i32,
    vdx: i32,
    vdy: i32,
    hddx: i32,
    hddy: i32,
}

impl RowWalker {
    pub(super) const fn new(ccb: &Ccb) -> Self {
        Self {
            x: ccb.x,
            y: ccb.y,
            step_x: ccb.hdx,
            step_y: ccb.hdy,
            vdx: ccb.vdx,
            vdy: ccb.vdy,
            hddx: ccb.hddx,
            hddy: ccb.hddy,
        }
    }

    /// A column cursor at the start of the current row.
    pub(super) fn columns(&self) -> Texel {
        let next_step_x = self.step_x.wrapping_add(self.hddx);
        let next_step_y = self.step_y.wrapping_add(self.hddy);
        Texel {
            x: self.x,
            y: self.y,
            step_x: self.step_x,
            step_y: self.step_y,
            down_x: self.x.wrapping_add(self.vdx),
            down_y: self.y.wrapping_add(self.vdy),
            next_step_x,
            next_step_y,
        }
    }

    pub(super) fn next_row(&mut self) {
        self.x = self.x.wrapping_add(self.vdx);
        self.y = self.y.wrapping_add(self.vdy);
        self.step_x = self.step_x.wrapping_add(self.hddx);
        self.step_y = self.step_y.wrapping_add(self.hddy);
    }
}

/// One texel's placement, 16.16: its own corner, its column step, and the
/// matching corner and step on the next row. A scaled texel needs only the
/// first two; a rotated one is the figure all four describe.
#[derive(Clone, Copy, Debug)]
pub(super) struct Texel {
    pub x: i32,
    pub y: i32,
    pub step_x: i32,
    pub step_y: i32,
    pub down_x: i32,
    pub down_y: i32,
    pub next_step_x: i32,
    pub next_step_y: i32,
}

impl Texel {
    pub(super) fn advance(&mut self) {
        self.advance_by(1);
    }

    /// Move along the row by `count` source columns. Both the texel's own edge
    /// and the next row's edge move, including across a transparent run.
    pub(super) fn advance_by(&mut self, count: u32) {
        let count = count as i32;
        self.x = self.x.wrapping_add(self.step_x.wrapping_mul(count));
        self.y = self.y.wrapping_add(self.step_y.wrapping_mul(count));
        self.down_x = self
            .down_x
            .wrapping_add(self.next_step_x.wrapping_mul(count));
        self.down_y = self
            .down_y
            .wrapping_add(self.next_step_y.wrapping_mul(count));
    }

    /// The four corners, in order round the figure: this texel, the next one
    /// along, the next row's next one, and the next row's matching one.
    pub(super) fn corners(&self) -> [(i32, i32); 4] {
        [
            (self.x, self.y),
            (
                self.x.wrapping_add(self.step_x),
                self.y.wrapping_add(self.step_y),
            ),
            (
                self.down_x.wrapping_add(self.next_step_x),
                self.down_y.wrapping_add(self.next_step_y),
            ),
            (self.down_x, self.down_y),
        ]
    }
}

/// The clip rectangle, inclusive of zero and exclusive of its size.
#[derive(Clone, Copy, Debug)]
pub(super) struct Clip {
    pub width: i32,
    pub height: i32,
}

/// A half-open rectangle of destination pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Rect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

/// The pixels a scaled texel covers: from where it lands to where the texel
/// diagonally next to it lands, displaced by BOTH vectors (a 1x1 cel stretched
/// into a quad often has HDX of zero and all of its width in VDX). MARIA caps
/// the downward stretch at one row. Clipped before it is walked.
pub(super) fn scaled_footprint(texel: &Texel, vdx: i32, vdy: i32, maria: bool, clip: Clip) -> Rect {
    scaled_footprint_at(texel, vdx, vdy, maria, clip, 1)
}

/// [`scaled_footprint`] on a grid `scale` times finer: the same 16.16 edges,
/// truncated to 1/`scale` of a pixel instead of a whole one. `clip` is in
/// that finer grid. At a scale of one it is exactly the native footprint.
pub(super) fn scaled_footprint_at(
    texel: &Texel,
    vdx: i32,
    vdy: i32,
    maria: bool,
    clip: Clip,
    scale: u32,
) -> Rect {
    let span_x = texel.step_x.wrapping_add(vdx);
    let mut span_y = texel.step_y.wrapping_add(vdy);
    if maria && span_y > 0x10000 {
        span_y = 0x10000;
    }
    let at = |position: i32| to_grid(position, scale);
    let (x0, x1) = ordered(at(texel.x), at(texel.x.wrapping_add(span_x)));
    let (y0, y1) = ordered(at(texel.y), at(texel.y.wrapping_add(span_y)));
    Rect {
        x0: x0.max(0),
        y0: y0.max(0),
        x1: x1.min(clip.width),
        y1: y1.min(clip.height),
    }
}

/// A 16.16 position truncated to a grid `scale` cells per pixel. Widened
/// first, so the multiply cannot wrap; the result always fits again.
#[inline]
pub(super) fn to_grid(position: i32, scale: u32) -> i32 {
    ((i64::from(position) * i64::from(scale)) >> 16) as i32
}

/// A reversed span covers the pixels after its far end up to and including
/// its start, so mirrored texels tile exactly as forward ones do.
fn ordered(from: i32, to: i32) -> (i32, i32) {
    if to < from {
        (to.wrapping_add(1), from.wrapping_add(1))
    } else {
        (from, to)
    }
}

/// One edge crossing on a scanline.
#[derive(Clone, Copy, Debug, Default)]
struct Crossing {
    x: i32,
    /// The edge runs down the screen: the counter-clockwise side.
    downward: bool,
}

/// Scan-convert one arbitrary texel with the CEL engine's edge rules
/// (WO 94/10644; conventional active-edge conversion). Corners are 16.16 and
/// are truncated to whole pixels first.
///
/// Edges are top-inclusive and bottom-exclusive. A permitted span includes
/// BOTH of its edge intersections: the extra right-hand pixel is what closes
/// one-pixel seams between perspective-stretched texels. The winding of each
/// span's left edge is checked against the CCB's ACW/ACCW permissions. A folded
/// quad can cross a scanline four times, so sorted crossings pair up.
///
/// `span(y, x_from, x_to)` receives each clipped half-open run.
pub(super) fn scan_quad(
    corners: [(i32, i32); 4],
    flags: u32,
    clip: Clip,
    span: impl FnMut(i32, i32, i32),
) {
    scan_quad_at(corners, flags, clip, 1, span);
}

/// [`scan_quad`] on a grid `scale` times finer (see [`scaled_footprint_at`]):
/// the corners keep `scale` times the sub-pixel precision, so the figure's
/// edges are resolved at that resolution. `clip` and the spans are in the
/// finer grid.
pub(super) fn scan_quad_at(
    corners: [(i32, i32); 4],
    flags: u32,
    clip: Clip,
    scale: u32,
    mut span: impl FnMut(i32, i32, i32),
) {
    let points = corners.map(|(x, y)| (to_grid(x, scale), to_grid(y, scale)));
    let [(ax, _), (bx, _), (cx, _), (dx, _)] = points;
    if ax == bx && bx == cx && cx == dx {
        return;
    }

    let lowest = points.iter().map(|p| p.1).min().unwrap_or(0);
    let highest = points.iter().map(|p| p.1).max().unwrap_or(0);
    let first_y = lowest.max(0);
    let last_y = highest.min(clip.height);

    let permitted = |downward: bool| {
        if downward {
            flags & CCB_ACCW != 0
        } else {
            flags & CCB_ACW != 0
        }
    };

    for y in first_y..last_y {
        let mut crossings = [Crossing::default(); 4];
        let mut count = 0usize;
        for (edge, &(x0, y0)) in points.iter().enumerate() {
            let Some(&(x1, y1)) = points.get((edge + 1) & 3) else {
                continue;
            };
            if y0 == y1 || y < y0.min(y1) || y >= y0.max(y1) {
                continue;
            }
            let along = i64::from(x1.wrapping_sub(x0)) * i64::from(y.wrapping_sub(y0));
            let x = x0.wrapping_add((along / i64::from(y1.wrapping_sub(y0))) as i32);
            if let Some(slot) = crossings.get_mut(count) {
                *slot = Crossing {
                    x,
                    downward: y1 > y0,
                };
                count += 1;
            }
        }
        let crossings = crossings.get_mut(..count).unwrap_or(&mut []);
        crossings.sort_by_key(|c| c.x);

        for [left, right] in crossings.as_chunks::<2>().0 {
            if !permitted(left.downward) {
                continue;
            }
            let from = left.x.max(0);
            let to = if right.x < clip.width {
                right.x + 1
            } else {
                clip.width
            };
            if from < to {
                span(y, from, to);
            }
        }
    }
}

/// One non-horizontal edge of a grid quad, set up once per texel.
#[derive(Clone, Copy, Debug, Default)]
struct GridEdge {
    x0: i32,
    y0: i32,
    dx: i64,
    dy: i64,
    /// The rows it crosses: `top <= y < bottom`.
    top: i32,
    bottom: i32,
    downward: bool,
}

/// [`scan_quad_at`] for the hi-res grid, built for volume: a distant cel can
/// hand the hi-res pass a quarter of a million texels a field, most a pixel
/// or two across, so the per-texel setup is what costs. Edges are set up once
/// rather than per row, texels wholly off the clip are dropped from their
/// bounds, and the crossings are ordered with a stable insertion sort. The
/// spans are exactly [`scan_quad_at`]'s; a test holds the two together. The
/// native pass keeps using [`scan_quad`].
pub(super) fn scan_quad_grid(
    corners: [(i32, i32); 4],
    flags: u32,
    clip: Clip,
    scale: u32,
    mut span: impl FnMut(i32, i32, i32),
) {
    let points = corners.map(|(x, y)| (to_grid(x, scale), to_grid(y, scale)));
    let [(ax, ay), (bx, by), (cx, cy), (dx, dy)] = points;
    if ax == bx && bx == cx && cx == dx {
        return;
    }
    let first_y = ay.min(by).min(cy).min(dy).max(0);
    let last_y = ay.max(by).max(cy).max(dy).min(clip.height);
    if first_y >= last_y {
        return;
    }
    // Every crossing lies between its edge's ends, so a quad wholly left of
    // or right of the clip has no span that survives clipping.
    if ax.max(bx).max(cx).max(dx) < 0 || ax.min(bx).min(cx).min(dx) >= clip.width {
        return;
    }

    let mut edges = [GridEdge::default(); 4];
    let mut edge_count = 0usize;
    for (edge, &(x0, y0)) in points.iter().enumerate() {
        let Some(&(x1, y1)) = points.get((edge + 1) & 3) else {
            continue;
        };
        if y0 == y1 {
            continue;
        }
        if let Some(slot) = edges.get_mut(edge_count) {
            *slot = GridEdge {
                x0,
                y0,
                dx: i64::from(x1.wrapping_sub(x0)),
                dy: i64::from(y1.wrapping_sub(y0)),
                top: y0.min(y1),
                bottom: y0.max(y1),
                downward: y1 > y0,
            };
            edge_count += 1;
        }
    }
    let edges = edges.get(..edge_count).unwrap_or(&[]);
    let permitted = |downward: bool| flags & if downward { CCB_ACCW } else { CCB_ACW } != 0;

    for y in first_y..last_y {
        let mut crossings = [Crossing::default(); 4];
        let mut count = 0usize;
        for edge in edges {
            if y < edge.top || y >= edge.bottom {
                continue;
            }
            // Grid coordinates are small, so the product nearly always fits
            // 32 bits, where division is cheaper; both truncate toward zero.
            let along = edge.dx * i64::from(y.wrapping_sub(edge.y0));
            let offset = match (i32::try_from(along), i32::try_from(edge.dy)) {
                (Ok(along), Ok(dy)) => along.checked_div(dy).unwrap_or(0),
                _ => (along / edge.dy) as i32,
            };
            if let Some(slot) = crossings.get_mut(count) {
                *slot = Crossing {
                    x: edge.x0.wrapping_add(offset),
                    downward: edge.downward,
                };
                count += 1;
            }
        }
        let crossings = crossings.get_mut(..count).unwrap_or(&mut []);
        // Order by position, equal positions keeping edge order (the
        // reference's sort is stable). Two crossings is the usual case.
        if let [first, second] = crossings {
            if first.x > second.x {
                std::mem::swap(first, second);
            }
        } else {
            crossings.sort_by_key(|c| c.x);
        }
        for [left, right] in crossings.as_chunks::<2>().0 {
            if !permitted(left.downward) {
                continue;
            }
            let from = left.x.max(0);
            let to = if right.x < clip.width {
                right.x + 1
            } else {
                clip.width
            };
            if from < to {
                span(y, from, to);
            }
        }
    }
}

/// The winding a pair of step vectors describes, as the flag that permits it.
/// With +Y down, a positive cross product is the counter-clockwise side.
/// Wrapping, as the corner arithmetic on the hardware is.
fn texel_winding(hdx: i64, hdy: i64, vdx: i64, vdy: i64) -> u32 {
    if hdx.wrapping_mul(vdy).wrapping_sub(hdy.wrapping_mul(vdx)) > 0 {
        CCB_ACCW
    } else {
        CCB_ACW
    }
}

/// Whether a cel is thrown away before any of it is drawn (WO 94/10644):
/// neither winding permitted; its bounds wholly off one side of the clip
/// rectangle with its steps leading further off; an axis-aligned cel facing
/// the forbidden way; or a consistently wound quad facing the forbidden way.
pub fn cel_is_invisible(ccb: &Ccb, packed: bool, clip_width: u32, clip_height: u32) -> bool {
    if ccb.flags & (CCB_ACW | CCB_ACCW) == 0 {
        return true;
    }
    // Inclusive, as the register is.
    let clip_x = (clip_width as i32).wrapping_sub(1);
    let clip_y = (clip_height as i32).wrapping_sub(1);
    let wide = ccb.width as i32;
    let high = ccb.height as i32;

    // Wrapping on purpose: a far-off cel overflows these on the hardware too,
    // and the test only ever rejects.
    let step = |base: i32, delta: i32, count: i32| base.wrapping_add(delta.wrapping_mul(count));
    let all = |values: &[i32], test: &dyn Fn(i32) -> bool| values.iter().all(|&v| test(v));

    if packed {
        // A packed cel's rows are variable length: only the vertical edge of
        // its bounds is known ahead of time.
        let xs = [ccb.x >> 16, step(ccb.x, ccb.vdx, high) >> 16];
        if all(&xs, &|v| v < 0) && ccb.hdx <= 0 && ccb.hddx <= 0 {
            return true;
        }
        if all(&xs, &|v| v > clip_x) && ccb.hdx >= 0 && ccb.hddx >= 0 {
            return true;
        }
        let ys = [ccb.y >> 16, step(ccb.y, ccb.vdy, high) >> 16];
        if all(&ys, &|v| v < 0) && ccb.hdy <= 0 && ccb.hddy <= 0 {
            return true;
        }
        if all(&ys, &|v| v > clip_y) && ccb.hdy >= 0 && ccb.hddy >= 0 {
            return true;
        }
    } else {
        let corners = |origin: i32, h: i32, v: i32, hd: i32| {
            let bottom = step(origin, v, high);
            [
                origin >> 16,
                step(origin, h, wide) >> 16,
                bottom >> 16,
                step(bottom, step(h, hd, high), wide) >> 16,
            ]
        };
        let xs = corners(ccb.x, ccb.hdx, ccb.vdx, ccb.hddx);
        if all(&xs, &|v| v < 0) || all(&xs, &|v| v > clip_x) {
            return true;
        }
        let ys = corners(ccb.y, ccb.hdy, ccb.vdy, ccb.hddy);
        if all(&ys, &|v| v < 0) || all(&ys, &|v| v > clip_y) {
            return true;
        }
    }

    if ccb.hddx == 0 && ccb.hddy == 0 {
        // A quarter turn: the horizontal step is vertical and vice versa.
        if ccb.hdx == 0 && ccb.vdy == 0 {
            let clockwise = (ccb.hdy < 0 && ccb.vdx > 0) || (ccb.hdy > 0 && ccb.vdx < 0);
            return ccb.flags & if clockwise { CCB_ACW } else { CCB_ACCW } == 0;
        }
        // Upright, possibly mirrored on either axis.
        if ccb.hdy == 0 && ccb.vdx == 0 {
            let counter = (ccb.hdx < 0 && ccb.vdy > 0) || (ccb.hdx > 0 && ccb.vdy < 0);
            return ccb.flags & if counter { CCB_ACCW } else { CCB_ACW } == 0;
        }
    }

    quad_is_wrong_way_round(ccb, if packed { 2048 } else { wide })
}

/// Whether a perspective-stepped cel is consistently wound the forbidden way.
///
/// P(x, y) = origin + x*(H + y*DD) + y*V, so the local tangents are H + y*DD
/// and V + x*DD. Their cross product is tested at all four corners; if the
/// signs disagree the surface is folded and cannot be rejected as one face.
pub fn quad_is_wrong_way_round(ccb: &Ccb, wide: i32) -> bool {
    let allowed = ccb.flags & (CCB_ACW | CCB_ACCW);
    if allowed == CCB_ACW | CCB_ACCW {
        return false;
    }
    let w = i64::from(wide);
    let h = i64::from(ccb.height);
    let add = |base: i32, delta: i32, count: i64| {
        i64::from(base).wrapping_add(i64::from(delta).wrapping_mul(count))
    };

    let (hdx, hdy) = (i64::from(ccb.hdx), i64::from(ccb.hdy));
    let (vdx, vdy) = (i64::from(ccb.vdx), i64::from(ccb.vdy));
    let (far_hdx, far_hdy) = (add(ccb.hdx, ccb.hddx, h), add(ccb.hdy, ccb.hddy, h));
    let (far_vdx, far_vdy) = (add(ccb.vdx, ccb.hddx, w), add(ccb.vdy, ccb.hddy, w));

    let first = texel_winding(hdx, hdy, vdx, vdy);
    let others = [
        texel_winding(hdx, hdy, far_vdx, far_vdy),
        texel_winding(far_hdx, far_hdy, vdx, vdy),
        texel_winding(far_hdx, far_hdy, far_vdx, far_vdy),
    ];
    if others.iter().any(|&winding| winding != first) {
        return false;
    }
    first == allowed
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn spans(scan: impl Fn(&mut dyn FnMut(i32, i32, i32))) -> Vec<(i32, i32, i32)> {
        let mut out = Vec::new();
        scan(&mut |y, from, to| out.push((y, from, to)));
        out
    }

    #[test]
    fn the_grid_scan_matches_the_reference_scan() {
        let mut seed = 0x9e37_79b9u32;
        let mut next = |range: i32| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed % (range as u32 * 2)) as i32 - range
        };
        let clip = Clip {
            width: 640,
            height: 480,
        };
        for case in 0..20_000 {
            // Mostly small texels near the clip edges, some large and folded.
            let reach = if case % 4 == 0 { 400 << 16 } else { 3 << 16 };
            let origin = (next(330 << 16), next(250 << 16));
            let corners = [0, 1, 2, 3].map(|_| (origin.0 + next(reach), origin.1 + next(reach)));
            for flags in [CCB_ACW | CCB_ACCW, CCB_ACW, CCB_ACCW] {
                for scale in [1u32, 2, 3, 4] {
                    let clip = Clip {
                        width: clip.width * scale as i32 / 2,
                        height: clip.height * scale as i32 / 2,
                    };
                    let reference = spans(|f| scan_quad_at(corners, flags, clip, scale, &mut *f));
                    let fast = spans(|f| scan_quad_grid(corners, flags, clip, scale, &mut *f));
                    assert_eq!(
                        fast, reference,
                        "corners {corners:?}, flags {flags:#x}, scale {scale}"
                    );
                }
            }
        }
    }
}
