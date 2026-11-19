//! The hi-res half of a cel walk as an ordered stream of jobs.
//!
//! At an internal scale above one every cel is rasterized twice: natively into
//! VRAM, which the machine depends on, and on the finer grid into a
//! [`HiResSurface`], which only the display reads. The two passes share nothing
//! but their order, so the walk records the hi-res work as [`Job`]s and a
//! large walk hands them to a helper thread that applies them while the walk
//! carries on natively. Small walks apply them in place at the end; either
//! way the jobs are applied in exactly the order they were recorded, so the
//! surface comes out identical (a test holds the two paths together).
//!
//! The helper never sees VRAM. A cel whose blend ignores the frame (most
//! textured 3D) is sent as geometry and colour, and the helper scans and
//! blends it. A cel that blends with the frame needs the pixel beneath as it
//! is at that moment, so the walk scans and blends it itself and sends the
//! finished runs.
#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::Arc;

use super::decoder::Texel as Decoded;
use super::pixel::PixelProcessor;
use super::raster::{scan_quad_grid, Clip, Rect, Texel};
use crate::hires::HiResSurface;
use crate::texpack::{source_fnv, PackTexture as FilteredPatch, TexturePack};

/// A cel's decoded source image, upsampled to the hi-res grid with
/// edge-aware bilinear filtering. Each cell packs a drawable flag in bit 31
/// and the 15-bit colour in the low bits; a cell in a transparent texel's
/// block stays 0 so sampling it writes nothing, matching the native walk.
/// Building it once per cel turns texture filtering into an indexed load per
/// output pixel: cheap enough for the helper thread to keep up at 4x.
///
/// Layout matches [`crate::texpack::PackTexture`] — the same type, so a
/// texture-pack replacement can stand in for a computed filter directly.
///
/// Drawable flag in a patch cell.
const VALID: u32 = 1 << 31;

/// The most source texels worth filtering: bigger cels (large letterboxed
/// images) fall back to flat texel fills rather than build a giant patch.
const MAX_PATCH_TEXELS: u64 = 1 << 18;

/// A cel's decoded source image in grid order, ready for the applier to
/// filter. Shared by [`CelSetup`]: it rides inside the [`Job::Cel`] so it can
/// cross to the helper thread.
#[derive(Clone, Debug)]
pub(super) struct RawPatch {
    pub width: u32,
    pub height: u32,
    /// `width * height` texels in source order; empty slots stay transparent.
    pub texels: Vec<Decoded>,
}

/// One channel of a filtered cell: taps' channel value and weight.
fn mix_channel(taps: [u32; 4], wx: u32, wy: u32) -> u32 {
    let [a, b, c, d] = taps;
    let top = a * (256 - wx) + b * wx;
    let bottom = c * (256 - wx) + d * wx;
    (top * (256 - wy) + bottom * wy + (1 << 15)) >> 16
}

/// How far two colours may sit apart, per 5-bit channel, and still be read as
/// the two halves of a dither pattern rather than a drawn edge.
const DITHER_SPREAD: i32 = 7;

/// Undo the dithering a 3DO artist used to fake colours the hardware does not
/// have: where neighbouring texels differ only slightly they are averaged
/// into the colour they stand for, and where they differ sharply — an edge,
/// a highlight, a line — nothing moves. Transparent texels take no part and
/// stay transparent.
///
/// This is the one improvement that needs no artwork and suits every title:
/// the console's own picture, read as what it was drawn to mean. The result
/// feeds the same filter and cache as a texture pack would.
pub(super) fn dedithered(texels: &[Decoded], width: u32, height: u32) -> Vec<Decoded> {
    let (w, h) = (width as usize, height as usize);
    let mut out = texels.to_vec();
    if w == 0 || h == 0 || texels.len() < w * h {
        return out;
    }
    let channels = |colour: u16| {
        [
            (i32::from(colour) >> 10) & 0x1f,
            (i32::from(colour) >> 5) & 0x1f,
            i32::from(colour) & 0x1f,
        ]
    };
    for y in 0..h {
        for x in 0..w {
            let Some(centre) = texels.get(y * w + x).copied() else {
                continue;
            };
            if centre.transparent {
                continue;
            }
            let own = channels(centre.color);
            // Weighted by nearness in colour: a dither partner counts fully,
            // an edge not at all, so flat areas smooth and edges hold.
            let (mut sums, mut weight) = ([0i32; 3], 0i32);
            for dy in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                for dx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                    let Some(near) = texels.get(dy * w + dx).copied() else {
                        continue;
                    };
                    if near.transparent {
                        continue;
                    }
                    let theirs = channels(near.color);
                    let apart = theirs
                        .iter()
                        .zip(own.iter())
                        .map(|(a, b)| (a - b).abs())
                        .max()
                        .unwrap_or(0);
                    if apart > DITHER_SPREAD {
                        continue;
                    }
                    // The centre counts twice: its own colour leads.
                    let count = if dy == y && dx == x { 2 } else { 1 };
                    for (sum, theirs) in sums.iter_mut().zip(theirs.iter()) {
                        *sum += theirs * count;
                    }
                    weight += count;
                }
            }
            if weight == 0 {
                continue;
            }
            let channel = |c: usize| {
                let sum = sums.get(c).copied().unwrap_or(0);
                ((sum + weight / 2) / weight).clamp(0, 0x1f) as u16
            };
            let colour = (channel(0) << 10) | (channel(1) << 5) | channel(2);
            if let Some(slot) = out.get_mut(y * w + x) {
                // The position bit the console keeps in bit 15 rides along.
                slot.color = (centre.color & 0x8000) | colour;
            }
        }
    }
    out
}

/// Bilinear-upsample the decoded source image `scale` times. `texels` is
/// `width * height` entries in source order; transparent texels act as holes:
/// a filtered cell touching one keeps the colour of the texel it sits in, so
/// silhouettes stay sharp instead of haloing into the transparent colour.
pub(super) fn filtered_patch(
    texels: &[Decoded],
    width: u32,
    height: u32,
    scale: u32,
) -> Option<FilteredPatch> {
    if scale <= 1
        || width == 0
        || height == 0
        || u64::from(width) * u64::from(height) > MAX_PATCH_TEXELS
    {
        return None;
    }
    let (w, h, k) = (width as usize, height as usize, scale as usize);
    if texels.len() < w * h {
        return None;
    }
    let (fw, fh) = (w * k, h * k);
    let mut cells = vec![0u32; fw * fh];
    // For filtered cell i the bilinear point in texel space is
    // s = (2i + 1 - k) / (2k): tap at floor(s), weight = fraction(s).
    // Negative s (first cell, k > 1) clamps to texel 0 with no fraction.
    let axis = |i: usize| -> (usize, u32) {
        let num = (i as i64) * 2 + 1 - k as i64;
        let den = 2 * k as i64;
        if num < 0 {
            return (0, 0);
        }
        (
            num.div_euclid(den) as usize,
            (num.rem_euclid(den) * 256 / den) as u32,
        )
    };
    for fy in 0..fh {
        let (ty, wy) = axis(fy);
        let texel_row = fy / k;
        for fx in 0..fw {
            let (tx, wx) = axis(fx);
            let texel_col = fx / k;
            let centre = (texel_row * w + texel_col).min(texels.len().saturating_sub(1));
            let centre = texels.get(centre).copied().unwrap_or_default();
            if centre.transparent {
                continue;
            }
            let tap = |x: usize, y: usize| -> Option<Decoded> {
                let i = y.min(h.saturating_sub(1)) * w + x.min(w.saturating_sub(1));
                texels.get(i).copied()
            };
            let taps = [
                tap(tx, ty),
                tap(tx + 1, ty),
                tap(tx, ty + 1),
                tap(tx + 1, ty + 1),
            ];
            let colour = if taps.iter().all(|t| t.is_some_and(|d| !d.transparent)) {
                let chan = |shift: u32| {
                    mix_channel(
                        taps.map(|t| (u32::from(t.map_or(0, |d| d.color)) >> shift) & 0x1f),
                        wx,
                        wy,
                    )
                };
                (chan(10) << 10) | (chan(5) << 5) | chan(0)
            } else {
                u32::from(centre.color)
            };
            let cell = (fy * fw + fx).min(cells.len().saturating_sub(1));
            let value = VALID | (colour & 0x7fff) | u32::from(centre.color & 0x8000);
            if let Some(slot) = cells.get_mut(cell) {
                *slot = value;
            }
        }
    }
    Some(FilteredPatch {
        k: scale,
        width: fw as u32,
        cells,
    })
}

/// What the frame-blind texels of one cel share. Boxed in its job: a cel is
/// rare next to its texels, and every job is as big as the biggest kind.
#[derive(Clone, Debug)]
pub(super) struct CelSetup {
    pub processor: PixelProcessor,
    pub clip: Clip,
    pub scale: u32,
    pub flags: u32,
    /// The cel blends against the frame buffer. Its texels are drawn in
    /// order, each reading what the one before it wrote, so a patch — which
    /// is sampled per texel against the frame as it stood when the texel was
    /// walked — cannot stand in for them without banding.
    pub reads_frame: bool,
    /// The cel is drawn one source texel to one native pixel. There is
    /// nothing for the filter to interpolate, so its texels keep their flat
    /// blocks — unless a texture pack has art for the cel, which is exactly
    /// the case where hand-drawn detail is worth having.
    pub one_to_one: bool,
    /// The cel's decoded source when it is worth filtering; the applier
    /// builds the hi-res filtered patch from it so the emu thread does not
    /// pay the upsample.
    pub patch: Option<Arc<RawPatch>>,
}

/// A texel's colour inputs, without the transparency flag (transparent
/// texels never become jobs). `frame` is the frame-buffer pixel the blend
/// reads, sampled once at the texel's own native pixel — zero for a texel
/// whose pixel-processor half ignores the frame, which is most of them.
#[derive(Clone, Copy, Debug)]
pub(super) struct Colour {
    pub color: u16,
    pub amv: u16,
    pub raw_vh: u16,
    pub frame: u16,
}

impl Colour {
    pub(super) fn of(value: Decoded, frame: u16) -> Self {
        Self {
            color: value.color,
            amv: value.amv,
            raw_vh: value.raw_vh,
            frame,
        }
    }
}

/// One step of hi-res work, in walk order. Kept small: a distant cel sends a
/// quarter of a million of these a field from one core to another.
#[derive(Clone, Debug)]
pub(super) enum Job {
    /// The cel the following frame-blind texels belong to.
    Cel(Box<CelSetup>),
    /// The column steps for the following texels (they change from row to
    /// row when a cel bends): `[step_x, step_y, next_step_x, next_step_y]`.
    Steps([i32; 4]),
    /// A rotated or perspective texel whose colour ignores the frame: its own
    /// corner and the matching corner on the next row, with the last steps.
    /// `src` is the texel's (column, row) in the cel's source image, which a
    /// cel never has more than a few thousand of, so it rides as u16 and a
    /// job stays inside its size budget.
    Quad {
        at: [i32; 4],
        src: [u16; 2],
        colour: Colour,
    },
    /// A scaled texel's clipped hi-res rectangle; its colour ignores the frame.
    Rect {
        area: Rect,
        src: [u16; 2],
        colour: Colour,
    },
}

/// Applies jobs to a surface, remembering the current cel.
#[derive(Clone, Debug, Default)]
pub(super) struct Applier {
    cel: CelSetup,
    steps: [i32; 4],
    /// The current cel's filtered source, built from its raw patch or taken
    /// from a texture pack on the applying thread (identical either way).
    filtered: Option<Arc<FilteredPatch>>,
    /// Filtered patches by raw-texel hash: a cel's source rarely changes
    /// frame to frame, so this avoids rebuilding big upsamples every field.
    patch_cache: Vec<(u64, Arc<FilteredPatch>, bool)>,
    /// Hand-authored cel replacements, looked up by content fingerprint.
    pub(super) pack: Option<Arc<TexturePack>>,
    /// Whether [`Applier::filtered`] is pack artwork rather than a computed
    /// upsample of the console's own texels.
    filtered_is_pack: bool,
    /// Read the console's own art as what it was drawn to mean: undo the
    /// dithering, and interpolate where the scale gives room. Off leaves
    /// every texel the flat block the console drew.
    pub(super) enhance: bool,
    /// Pack cells per output pixel when the artwork is denser than the
    /// picture — a 4x texture on a native-size field. Those cells are box
    /// filtered together, or the picture would be one arbitrary cell per
    /// pixel and crawl with aliasing.
    shrink: i64,
}

/// Most cached filtered patches, and their total cell budget (≈32MB of
/// u32 cells): enough for a scene's cels without hoarding memory.
const PATCH_CACHE_LEN: usize = 64;
const PATCH_CACHE_CELLS: usize = 8 << 20;

/// `RETRO3DO_PATCH_DUMP=dir` dumps each distinct source image once, named by
/// its fingerprint: the raw material for building a texture pack.
fn dump_dir() -> Option<&'static std::path::PathBuf> {
    static DIR: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        std::env::var_os("RETRO3DO_PATCH_DUMP").map(|d| {
            let path = std::path::PathBuf::from(d);
            let _ = std::fs::create_dir_all(&path);
            path
        })
    }).as_ref()
}

/// A cheap checksum of a raw patch's texels: every texel feeds in, so a
/// changed source can't accidentally hit a stale filtered patch.
fn patch_hash(raw: &RawPatch) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64 ^ u64::from(raw.width) << 32 | u64::from(raw.height);
    for t in &raw.texels {
        hash = (hash ^ u64::from(t.color) << 16
            | u64::from(t.amv)
            | u64::from(t.raw_vh) << 32
            | (t.transparent as u64) << 48)
            .wrapping_mul(0x100_0000_01b3);
    }
    hash
}

impl Default for CelSetup {
    fn default() -> Self {
        Self {
            processor: PixelProcessor::default(),
            clip: Clip {
                width: 0,
                height: 0,
            },
            scale: 1,
            flags: 0,
            reads_frame: false,
            one_to_one: false,
            patch: None,
        }
    }
}

fn fill(surface: &mut HiResSurface, y: i32, from: i32, to: i32, out: u16) {
    let (Ok(y), Ok(from), Ok(to)) = (u32::try_from(y), u32::try_from(from), u32::try_from(to))
    else {
        return;
    };
    surface.fill_row(y, from, to, out);
}

impl Applier {
    /// An applier wired to a texture pack: mod.rs can't touch private fields.
    pub(super) fn with_pack(pack: Option<Arc<TexturePack>>, enhance: bool) -> Self {
        Self {
            pack,
            enhance,
            ..Self::default()
        }
    }

    /// One patch cell, box filtered over the cells an output pixel covers
    /// when the artwork is denser than the picture. Transparent cells take no
    /// part; a pixel made only of them stays transparent.
    #[inline]
    fn sample_cell(&self, patch: &FilteredPatch, index: usize, row_width: i64) -> Option<u32> {
        let cell = patch.cells.get(index).copied()?;
        if self.shrink <= 1 {
            return Some(cell);
        }
        let (mut r, mut g, mut b, mut count) = (0u32, 0u32, 0u32, 0u32);
        let last = patch.cells.len() as i64 - 1;
        for dy in 0..self.shrink {
            let row = index as i64 + dy * row_width;
            for dx in 0..self.shrink {
                let at = (row + dx).clamp(0, last) as usize;
                let Some(near) = patch.cells.get(at).copied() else {
                    continue;
                };
                if near & VALID == 0 {
                    continue;
                }
                r += (near >> 10) & 0x1f;
                g += (near >> 5) & 0x1f;
                b += near & 0x1f;
                count += 1;
            }
        }
        if count == 0 {
            return Some(cell & !VALID);
        }
        Some(VALID | (r / count) << 10 | (g / count) << 5 | (b / count))
    }

    /// Blend `color` with this cel's pixel processor settings, reusing the
    /// previous blend when the patch cell hasn't changed — most of a frame
    /// is flat source, so this skips nearly all blend work.
    #[inline]
    fn blended(&self, cell: u32, last: &mut Option<(u32, u16)>, colour: Colour) -> u16 {
        if let Some((same, out)) = *last {
            if same == cell {
                return out;
            }
        }
        let out = self
            .cel
            .processor
            // The patch carries colour only; bit 15 belongs to the texel the
            // console decoded — it picks the pixel processor's half, so a
            // replacement that dropped it would blend by the wrong rule.
            .blend(
                (cell & 0x7fff) as u16 | (colour.color & 0x8000),
                colour.amv,
                colour.frame,
                colour.raw_vh,
            );
        *last = Some((cell, out));
        out
    }

    /// A scaled texel's footprint mapped through the filtered patch: each
    /// output pixel samples the patch cell its fraction of the footprint
    /// lands on, so a magnified source fades between texels instead of
    /// showing one block per texel.
    fn filtered_rect(
        &self,
        surface: &mut HiResSurface,
        area: Rect,
        sx: u32,
        sy: u32,
        colour: Colour,
    ) {
        let w = i64::from(area.x1 - area.x0);
        let h = i64::from(area.y1 - area.y0);
        if w <= 0 || h <= 0 {
            return;
        }
        let Some(patch) = self.filtered.as_ref() else {
            return;
        };
        let step_u = 0x10000i64 / w;
        let step_v = 0x10000i64 / h;
        let k = i64::from(patch.k);
        let fw = i64::from(patch.width);
        let max = patch.cells.len() as i64 - 1;
        let col0 = i64::from(sx) * k;

        for y in area.y0..area.y1 {
            let v = i64::from(y - area.y0) * step_v + step_v / 2;
            let row = (i64::from(sy) * k + ((v.clamp(0, 0x10000) * k) >> 16)) * fw + col0;
            let mut frac = step_u / 2;
            let mut last = None;
            // Advance in runs: every output pixel between patch cell
            // boundaries samples the same cell, so one lookup fills a run.
            let mut x = area.x0;
            while x < area.x1 {
                let fx = (frac.clamp(0, 0x10000) * k) >> 16;
                let index = (row + fx).clamp(0, max) as usize;
                // x where the sampled cell next changes: frac past the
                // next cell edge, whole steps of step_u.
                let run_to = if frac >= 0x10000 {
                    area.x1
                } else {
                    let boundary = (((fx + 1) << 16) + k - 1) / k;
                    let steps = ((boundary - frac) + step_u - 1) / step_u;
                    x.saturating_add(steps.max(1).min(i64::from(area.x1 - x)) as i32)
                };
                if let Some(cell) = self.sample_cell(patch, index, fw) {
                    if cell & VALID != 0 {
                        let out = self.blended(cell, &mut last, colour);
                        fill(surface, y, x, run_to, out);
                    } else {
                        last = None;
                    }
                }
                frac += step_u * i64::from(run_to - x);
                x = run_to;
            }
        }
    }

    /// A rotated texel's footprint mapped through the filtered patch. The
    /// quad is treated as a parallelogram: `u`,`v` come from inverting its
    /// edge vectors at each pixel centre, which is exact for the common case
    /// and a very close approximation when HDDX/HDDY bend the far edge.
    fn filtered_quad(
        &self,
        surface: &mut HiResSurface,
        texel: &Texel,
        sx: u32,
        sy: u32,
        colour: Colour,
    ) {
        let e1x = i64::from(texel.step_x);
        let e1y = i64::from(texel.step_y);
        let e2x = i64::from(texel.down_x - texel.x);
        let e2y = i64::from(texel.down_y - texel.y);
        let det = e1x * e2y - e1y * e2x;
        if det == 0 {
            return;
        }
        let (ax, ay) = (i64::from(texel.x), i64::from(texel.y));
        let k = i64::from(self.cel.scale);
        let Some(patch) = self.filtered.as_ref() else {
            return;
        };
        let tk = i64::from(patch.k);
        // u,v per hi-res pixel step: d(u,v)/d(px) from the inverse matrix.
        // px16 = (2px+1)<<15/k is the pixel centre in native 16.16.
        let dudx = ((e2y as i128 * 0x10000 * 0x10000) / (i128::from(k) * i128::from(det))) as i64;
        let dvdx = -((e1x as i128 * 0x10000 * 0x10000) / (i128::from(k) * i128::from(det))) as i64;
        // Minified in both axes: a pixel covers a whole texel, so the patch
        // sample is nearest anyway — a flat fill is as good and far cheaper.
        if dudx.abs() >= 0x10000 && dvdx.abs() >= 0x10000 {
            let CelSetup {
                processor,
                flags,
                clip,
                scale,
                ..
            } = self.cel;
            let out = processor.blend(colour.color, colour.amv, colour.frame, colour.raw_vh);
            scan_quad_grid(texel.corners(), flags, clip, scale, |y, from, to| {
                fill(surface, y, from, to, out);
            });
            return;
        }
        let CelSetup {
            flags, clip, scale, ..
        } = self.cel;
        let Some(patch) = self.filtered.as_ref() else {
            return;
        };
        let fw = i64::from(patch.width);
        let max = patch.cells.len() as i64 - 1;

        scan_quad_grid(texel.corners(), flags, clip, scale, |y, from, to| {
            // (u,v) at the first pixel centre of this run.
            let dx = (i64::from(from) * 2 + 1) * 0x8000 / k - ax;
            let dy = (i64::from(y) * 2 + 1) * 0x8000 / k - ay;
            let mut u = ((dx * e2y - dy * e2x) as i128 * 0x10000 / i128::from(det)) as i64;
            let mut v = ((e1x * dy - e1y * dx) as i128 * 0x10000 / i128::from(det)) as i64;
            let mut last = None;
            for x in from..to {
                let fx = i64::from(sx) * tk + ((u.clamp(0, 0x10000) * tk) >> 16);
                let fy = i64::from(sy) * tk + ((v.clamp(0, 0x10000) * tk) >> 16);
                let index = (fy * fw + fx).clamp(0, max) as usize;
                if let Some(cell) = self.sample_cell(patch, index, fw) {
                    if cell & VALID != 0 {
                        let out = self.blended(cell, &mut last, colour);
                        if let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) {
                            surface.put(x, y, out);
                        }
                    } else {
                        last = None;
                    }
                }
                u += dudx;
                v += dvdx;
            }
        });
    }

    /// Remember a patch under the raw-content hash, evicting oldest first
    /// within the count and cell budgets.
    fn cache_patch(&mut self, hash: u64, patch: Arc<FilteredPatch>, from_pack: bool) {
        let mut cells = self
            .patch_cache
            .iter()
            .map(|(_, p, _)| p.cells.len())
            .sum::<usize>();
        while self.patch_cache.len() >= PATCH_CACHE_LEN
            || cells + patch.cells.len() > PATCH_CACHE_CELLS
        {
            let Some((_, evicted, _)) = self.patch_cache.first().cloned() else {
                break;
            };
            cells = cells.saturating_sub(evicted.cells.len());
            self.patch_cache.remove(0);
        }
        self.patch_cache.push((hash, patch, from_pack));
    }

    pub(super) fn apply(&mut self, surface: &mut HiResSurface, job: &Job, degrade: bool) {
        match job {
            Job::Cel(setup) => {
                self.cel = (**setup).clone();
                if degrade {
                    self.filtered = None;
                    return;
                }
                let raw = self.cel.patch.clone();
                let pack = self.pack.clone();
                let scale = self.cel.scale;
                let dump = dump_dir();
                let mut from_pack = false;
                self.filtered = raw.as_ref().and_then(|raw| {
                    let hash = patch_hash(raw);
                    if let Some((_, patch, cached_from_pack)) =
                        self.patch_cache.iter().find(|(h, _, _)| *h == hash)
                    {
                        from_pack = *cached_from_pack;
                        return Some(patch.clone());
                    }
                    let fnv = source_fnv(raw.width, raw.height, |x, y| {
                        let texel = raw
                            .texels
                            .get((y * raw.width + x) as usize)
                            .copied()
                            .unwrap_or_default();
                        (texel.color, texel.transparent)
                    });
                    // Dump mode records each source once under its
                    // fingerprint, for pairing with pack artwork.
                    if let Some(dir) = dump {
                        let file = dir.join(format!("{fnv:016x}.png"));
                        if !file.exists() {
                            crate::texpack::write_source_png(&file, raw.width, raw.height, |x, y| {
                                let texel = raw
                                    .texels
                                    .get((y * raw.width + x) as usize)
                                    .copied()
                                    .unwrap_or_default();
                                (texel.color, texel.transparent)
                            });
                        }
                    }
                    if self.cel.reads_frame {
                        return None;
                    }
                    // A pack replacement shadows the computed filter: the
                    // artwork IS the source, at whatever density the artist
                    // painted it.
                    if let Some(pack) = &pack {
                        if let Some(texture) = pack.get(fnv, raw.width, raw.height) {
                            self.cache_patch(hash, texture.clone(), true);
                            from_pack = true;
                            return Some(texture);
                        }
                    }
                    // No pack art: read the console's own instead. Its
                    // dithering is undone first — that is what 3DO art uses
                    // in place of the colours the hardware lacks — and the
                    // result is interpolated when there are pixels to spread
                    // it over, or taken as it is when there are not.
                    // One-to-one cels are left alone for now: their texels
                    // come out of the patch banded, which is a bug in the
                    // 1:1 rect path, not in the reading of the art.
                    if !self.enhance || self.cel.one_to_one || scale <= 1 {
                        return None;
                    }

                    let clean = if std::env::var_os("RETRO3DO_DD_OFF").is_some() {
                        raw.texels.clone()
                    } else {
                        dedithered(&raw.texels, raw.width, raw.height)
                    };
                    let built = Arc::new(filtered_patch(&clean, raw.width, raw.height, scale)?);
                    self.cache_patch(hash, built.clone(), false);
                    Some(built)
                });
                self.filtered_is_pack = from_pack;
                self.shrink = self
                    .filtered
                    .as_ref()
                    .map(|patch| i64::from(patch.k / self.cel.scale.max(1)).max(1))
                    .unwrap_or(1);
            }
            Job::Steps(steps) => self.steps = *steps,
            &Job::Quad {
                at: [x, y, down_x, down_y],
                src: [sx, sy],
                colour,
            } => {
                let [step_x, step_y, next_step_x, next_step_y] = self.steps;
                let texel = Texel {
                    x,
                    y,
                    step_x,
                    step_y,
                    down_x,
                    down_y,
                    next_step_x,
                    next_step_y,
                };
                if self.filtered.is_some() {
                    self.filtered_quad(surface, &texel, u32::from(sx), u32::from(sy), colour);
                    return;
                }
                let CelSetup {
                    processor,
                    clip,
                    scale,
                    flags,
                    ..
                } = self.cel;
                // Blended on first use: many far-off texels cover nothing.
                let mut out: Option<u16> = None;
                scan_quad_grid(texel.corners(), flags, clip, scale, |y, from, to| {
                    let out = *out.get_or_insert_with(|| {
                        processor.blend(colour.color, colour.amv, colour.frame, colour.raw_vh)
                    });
                    fill(surface, y, from, to, out);
                });
            }
            &Job::Rect {
                area,
                src: [sx, sy],
                colour,
            } => {
                if area.x0 >= area.x1 || area.y0 >= area.y1 {
                    return;
                }
                if self.filtered.is_some() {
                    self.filtered_rect(surface, area, u32::from(sx), u32::from(sy), colour);
                    return;
                }
                let out = self
                    .cel
                    .processor
                    .blend(colour.color, colour.amv, colour.frame, colour.raw_vh);
                for y in area.y0..area.y1 {
                    fill(surface, y, area.x0, area.x1, out);
                }
            }
        }
    }
}

/// Once this many batches sit between the walk and the helper, the helper
/// sheds texture filtering and flat-fills until it catches up: better a few
/// plainer frames than the emulation thread stalling on a full queue.
const WORKER_BACKLOG_DEGRADE: usize = 8;

/// The helper thread: apply every batch in order, hand the emptied batches
/// back for reuse, and return the surface when the walk closes the channel.
/// When the queue runs deep it filters nothing — the picture keeps its
/// hi-res geometry and briefly drops to flat texel interiors.
pub(super) fn run_worker(
    mut surface: HiResSurface,
    batches: Receiver<Vec<Job>>,
    spent: SyncSender<Vec<Job>>,
    backlog: Arc<AtomicUsize>,
    pack: Option<Arc<TexturePack>>,
    enhance: bool,
) -> HiResSurface {
    let mut applier = Applier {
        pack,
        enhance,
        ..Applier::default()
    };
    for mut batch in batches {
        let queued = backlog.load(Ordering::Relaxed).saturating_sub(1);
        backlog.store(queued, Ordering::Relaxed);
        let degrade = queued > WORKER_BACKLOG_DEGRADE;
        for job in &batch {
            applier.apply(&mut surface, job, degrade);
        }
        batch.clear();
        // Full means the walk has spares enough; dropping this one is fine.
        let _ = spent.try_send(batch);
    }
    surface
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_job_stays_small() {
        assert!(
            std::mem::size_of::<super::Job>() <= 32,
            "{}",
            std::mem::size_of::<super::Job>()
        );
    }
}
