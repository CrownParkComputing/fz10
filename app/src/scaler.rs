//! Post-process scaling of the emulated output signal.
//!
//! The 3DO produces a finished framebuffer — there is no 3D rasteriser whose
//! internal resolution could simply be raised like a polygon console's. The
//! host-side equivalent of other emulators' "upscaling" is filtering the
//! machine's output signal: edge-adaptive integer scalers keep sprite and cel
//! edges crisp at large sizes, and the Lanczos resampler gives FMV and other
//! photographic content a smooth result. Everything here works on the XRGB8888
//! signal the machine publishes, so it benefits every frontend equally and is
//! selectable per consumer.

#![forbid(unsafe_code)]

/// Presentation filter applied to the emulated output signal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VideoFilter {
    /// Exact pixels, integer repeat. The baseline; fastest.
    #[default]
    Nearest,
    /// Four-tap linear resample. Soft, cheap, good on photographic content.
    Bilinear,
    /// Edge-directed 2x: copies a neighbour into a corner only when the three
    /// pixels around it agree, so diagonal edges come out smooth.
    Eagle,
    /// The classic edge-adaptive 2x scaler.
    Scale2x,
    /// The classic edge-adaptive 3x scaler.
    Scale3x,
    /// Scale2x applied twice; the sharpest big-integer option.
    Scale4x,
    /// Separable Lanczos-3 resample. The smooth, detailed choice — best for FMV
    /// — and the most expensive, so large factors cost real CPU.
    Lanczos,
}

impl VideoFilter {
    /// All filters, in menu order.
    pub const ALL: [Self; 7] = [
        Self::Nearest,
        Self::Bilinear,
        Self::Eagle,
        Self::Scale2x,
        Self::Scale3x,
        Self::Scale4x,
        Self::Lanczos,
    ];

    /// Short label for menus.
    pub fn label(self) -> &'static str {
        match self {
            Self::Nearest => "nearest",
            Self::Bilinear => "bilinear",
            Self::Eagle => "eagle 2x",
            Self::Scale2x => "scale2x",
            Self::Scale3x => "scale3x",
            Self::Scale4x => "scale4x",
            Self::Lanczos => "lanczos",
        }
    }

    /// The factor an edge-adaptive scaler produces in its first pass; the
    /// resamplers scale directly to the requested factor.
    fn natural_scale(self) -> u32 {
        match self {
            Self::Eagle | Self::Scale2x => 2,
            Self::Scale3x => 3,
            Self::Scale4x => 4,
            _ => 1,
        }
    }
}

/// Largest integer factor `apply` accepts; bigger requests are clamped.
pub const MAX_SCALE: u32 = 16;

#[inline]
fn px(src: &[u32], w: usize, x: i64, y: i64, h: i64) -> u32 {
    let xi = x.clamp(0, w as i64 - 1) as usize;
    let yi = y.clamp(0, h - 1) as usize;
    src.get(yi * w + xi).copied().unwrap_or(0xff00_0000)
}

#[inline]
fn put(dst: &mut [u32], w: usize, x: usize, y: usize, p: u32) {
    if let Some(slot) = dst.get_mut(y * w + x) {
        *slot = p;
    }
}

/// Integer-factor point resample from any source size to any factor.
fn nearest(src: &[u32], w: usize, h: usize, dst: &mut [u32], factor: usize) {
    let dw = w * factor;
    for dy in 0..h * factor {
        let sy = dy / factor;
        for dx in 0..dw {
            let sx = dx / factor;
            put(dst, dw, dx, dy, px(src, w, sx as i64, sy as i64, h as i64));
        }
    }
}

/// Four-tap linear resample at an integer factor.
fn bilinear(src: &[u32], w: usize, h: usize, dst: &mut [u32], factor: usize) {
    let dw = w * factor;
    let f = factor as f32;
    for dy in 0..h * factor {
        let sy = (dy as f32 + 0.5) / f - 0.5;
        let y0 = sy.floor() as i64;
        let fy = sy - y0 as f32;
        for dx in 0..dw {
            let sx = (dx as f32 + 0.5) / f - 0.5;
            let x0 = sx.floor() as i64;
            let fx = sx - x0 as f32;
            let mut acc = [0f32; 3];
            for (oy, wy) in [(0i64, 1.0 - fy), (1, fy)] {
                for (ox, wx) in [(0i64, 1.0 - fx), (1, fx)] {
                    let p = px(src, w, x0 + ox, y0 + oy, h as i64);
                    let wgt = wy * wx;
                    acc[0] += ((p >> 16) & 0xff) as f32 * wgt;
                    acc[1] += ((p >> 8) & 0xff) as f32 * wgt;
                    acc[2] += (p & 0xff) as f32 * wgt;
                }
            }
            let r = acc[0].clamp(0.0, 255.0) as u32;
            let g = acc[1].clamp(0.0, 255.0) as u32;
            let b = acc[2].clamp(0.0, 255.0) as u32;
            put(dst, dw, dx, dy, 0xff00_0000 | (r << 16) | (g << 8) | b);
        }
    }
}

/// Neighbour fetches for the edge scalers: A..I around E, edges clamped.
#[inline]
#[allow(clippy::too_many_arguments)]
fn hood(src: &[u32], w: usize, h: usize, x: usize, y: usize) -> [u32; 9] {
    let (x, y, h) = (x as i64, y as i64, h as i64);
    [
        px(src, w, x - 1, y - 1, h),
        px(src, w, x, y - 1, h),
        px(src, w, x + 1, y - 1, h),
        px(src, w, x - 1, y, h),
        px(src, w, x, y, h),
        px(src, w, x + 1, y, h),
        px(src, w, x - 1, y + 1, h),
        px(src, w, x, y + 1, h),
        px(src, w, x + 1, y + 1, h),
    ]
}

/// Eagle 2x: each output corner takes a neighbour when the three pixels
/// around that corner all match it.
fn eagle(src: &[u32], w: usize, h: usize, dst: &mut [u32]) {
    let dw = w * 2;
    for y in 0..h {
        for x in 0..w {
            let s = hood(src, w, h, x, y);
            let [s0, s1, s2, s3, s4, s5, s6, s7, s8] = s;
            let e0 = if s0 == s1 && s0 == s3 { s0 } else { s4 };
            let e1 = if s1 == s2 && s1 == s5 { s1 } else { s4 };
            let e2 = if s3 == s6 && s3 == s7 { s3 } else { s4 };
            let e3 = if s5 == s7 && s5 == s8 { s5 } else { s4 };
            let (dx, dy) = (x * 2, y * 2);
            put(dst, dw, dx, dy, e0);
            put(dst, dw, dx + 1, dy, e1);
            put(dst, dw, dx, dy + 1, e2);
            put(dst, dw, dx + 1, dy + 1, e3);
        }
    }
}

/// The AdvMAME 2x edge scaler: each output quarter takes the edge neighbour
/// when the two flanking pixels agree and do not continue the other way.
fn scale2x(src: &[u32], w: usize, h: usize, dst: &mut [u32]) {
    let dw = w * 2;
    for y in 0..h {
        for x in 0..w {
            let s = hood(src, w, h, x, y);
            let (b, d, e, f, hh) = (s[1], s[3], s[4], s[5], s[7]);
            let e0 = if d == b && b != f && d != hh { d } else { e };
            let e1 = if b == f && b != d && f != hh { f } else { e };
            let e2 = if d == hh && d != b && hh != f { d } else { e };
            let e3 = if hh == f && hh != d && f != b { f } else { e };
            let (dx, dy) = (x * 2, y * 2);
            put(dst, dw, dx, dy, e0);
            put(dst, dw, dx + 1, dy, e1);
            put(dst, dw, dx, dy + 1, e2);
            put(dst, dw, dx + 1, dy + 1, e3);
        }
    }
}

/// The AdvMAME 3x edge scaler.
fn scale3x(src: &[u32], w: usize, h: usize, dst: &mut [u32]) {
    let dw = w * 3;
    for y in 0..h {
        for x in 0..w {
            let s = hood(src, w, h, x, y);
            let (a, b, c, d, e, f, g, hh, i) =
                (s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7], s[8]);
            let out = [
                if d == b && b != f && d != hh { d } else { e },
                if (d == b && b != f && d != hh && e != c)
                    || (b == f && b != d && f != hh && e != a)
                {
                    b
                } else {
                    e
                },
                if b == f && b != d && f != hh { f } else { e },
                if (d == b && b != f && d != hh && e != g)
                    || (d == hh && d != b && hh != f && e != a)
                {
                    d
                } else {
                    e
                },
                e,
                if (b == f && b != d && f != hh && e != i)
                    || (hh == f && hh != d && f != b && e != g)
                {
                    f
                } else {
                    e
                },
                if d == hh && d != b && hh != f { d } else { e },
                if (d == hh && d != b && hh != f && e != i)
                    || (hh == f && hh != d && f != b && e != c)
                {
                    hh
                } else {
                    e
                },
                if hh == f && hh != d && f != b { f } else { e },
            ];
            let (dx, dy) = (x * 3, y * 3);
            for (n, p) in out.iter().enumerate() {
                put(dst, dw, dx + n % 3, dy + n / 3, *p);
            }
        }
    }
}

/// Point resample to an exact output size — used to top an edge scaler's
/// natural factor up to whatever the caller asked for.
fn nearest_fit(src: &[u32], sw: usize, sh: usize, dst: &mut [u32], dw: usize, dh: usize) {
    for dy in 0..dh {
        let sy = dy * sh / dh;
        for dx in 0..dw {
            let sx = dx * sw / dw;
            put(
                dst,
                dw,
                dx,
                dy,
                px(src, sw, sx as i64, sy as i64, sh as i64),
            );
        }
    }
}

/// Separable Lanczos-3 resample at an integer factor. Two passes through an
/// intermediate so each output pixel costs a+b taps, not a*b.
fn lanczos(src: &[u32], w: usize, h: usize, dst: &mut [u32], factor: usize, tmp: &mut Vec<u32>) {
    const TAPS: i64 = 3;
    let dw = w * factor;
    let dh = h * factor;
    let f = factor as f32;

    let kernel = |x: f32| -> f32 {
        if x.abs() >= TAPS as f32 {
            0.0
        } else if x.abs() < 1e-6 {
            1.0
        } else {
            let ax = std::f32::consts::PI * x;
            (ax.sin() / ax) * ((ax / 3.0).sin() / (ax / 3.0))
        }
    };

    // Per-destination-x weight lists: (first src x, normalised weights).
    let mut weights: Vec<(i64, [f32; 6])> = Vec::with_capacity(dw);
    for dx in 0..dw {
        let cx = (dx as f32 + 0.5) / f - 0.5;
        let base = (cx + 0.5).floor() as i64;
        let mut ws = [0f32; 6];
        let mut sum = 0.0;
        for (t, wgt) in ws.iter_mut().enumerate() {
            let d = cx - (base + t as i64 - 2) as f32;
            *wgt = kernel(d);
            sum += *wgt;
        }
        if sum.abs() > 1e-6 {
            for weight in ws.iter_mut() {
                *weight /= sum;
            }
        }
        weights.push((base - 2, ws));
    }

    tmp.clear();
    tmp.resize(dw * h, 0);
    for y in 0..h {
        for dx in 0..dw {
            let Some(&(x0, ws)) = weights.get(dx) else {
                continue;
            };
            let mut acc = [0f32; 3];
            for (t, weight) in ws.iter().enumerate() {
                if weight.abs() < 1e-6 {
                    continue;
                }
                let p = px(src, w, x0 + t as i64, y as i64, h as i64);
                acc[0] += ((p >> 16) & 0xff) as f32 * weight;
                acc[1] += ((p >> 8) & 0xff) as f32 * weight;
                acc[2] += (p & 0xff) as f32 * weight;
            }
            let r = acc[0].clamp(0.0, 255.0) as u32;
            let g = acc[1].clamp(0.0, 255.0) as u32;
            let b = acc[2].clamp(0.0, 255.0) as u32;
            put(tmp, dw, dx, y, 0xff00_0000 | (r << 16) | (g << 8) | b);
        }
    }

    // Vertical: the same one-dimensional resample, transposed roles.
    let mut wv: Vec<(i64, [f32; 6])> = Vec::with_capacity(dh);
    for dy in 0..dh {
        let cy = (dy as f32 + 0.5) / f - 0.5;
        let base = (cy + 0.5).floor() as i64;
        let mut ws = [0f32; 6];
        let mut sum = 0.0;
        for (t, wgt) in ws.iter_mut().enumerate() {
            let d = cy - (base + t as i64 - 2) as f32;
            *wgt = kernel(d);
            sum += *wgt;
        }
        if sum.abs() > 1e-6 {
            for weight in ws.iter_mut() {
                *weight /= sum;
            }
        }
        wv.push((base - 2, ws));
    }
    for dy in 0..dh {
        let Some(&(y0, ws)) = wv.get(dy) else {
            continue;
        };
        for dx in 0..dw {
            let mut acc = [0f32; 3];
            for (t, weight) in ws.iter().enumerate() {
                if weight.abs() < 1e-6 {
                    continue;
                }
                let p = px(tmp, dw, dx as i64, y0 + t as i64, h as i64);
                acc[0] += ((p >> 16) & 0xff) as f32 * weight;
                acc[1] += ((p >> 8) & 0xff) as f32 * weight;
                acc[2] += (p & 0xff) as f32 * weight;
            }
            let r = acc[0].clamp(0.0, 255.0) as u32;
            let g = acc[1].clamp(0.0, 255.0) as u32;
            let b = acc[2].clamp(0.0, 255.0) as u32;
            put(dst, dw, dx, dy, 0xff00_0000 | (r << 16) | (g << 8) | b);
        }
    }
}

/// Output size `apply` will produce for a given input and request.
pub fn output_size(w: usize, h: usize, scale: u32) -> (usize, usize) {
    let s = scale.clamp(1, MAX_SCALE) as usize;
    (w * s, h * s)
}

/// Filter `src` (w×h XRGB8888) into `dst`, resized as needed, at `scale`
/// times the input dimensions. Returns the output size written. `tmp` is a
/// reusable scratch for the filters that need an intermediate.
pub fn apply(
    filter: VideoFilter,
    scale: u32,
    src: &[u32],
    w: usize,
    h: usize,
    dst: &mut Vec<u32>,
    tmp: &mut Vec<u32>,
) -> (usize, usize) {
    let scale = scale.clamp(1, MAX_SCALE) as usize;
    let (dw, dh) = (w * scale, h * scale);
    dst.clear();
    dst.resize(dw * dh, 0xff00_0000);
    if scale == 1 {
        dst.copy_from_slice(src.get(..dw * dh).unwrap_or(&[]));
        return (dw, dh);
    }
    match filter {
        VideoFilter::Nearest => nearest(src, w, h, dst, scale),
        VideoFilter::Bilinear => bilinear(src, w, h, dst, scale),
        VideoFilter::Lanczos => lanczos(src, w, h, dst, scale, tmp),
        edge => {
            let nat = edge.natural_scale() as usize;
            tmp.clear();
            tmp.resize(w * nat * h * nat, 0);
            match edge {
                VideoFilter::Eagle => eagle(src, w, h, tmp),
                VideoFilter::Scale2x => scale2x(src, w, h, tmp),
                VideoFilter::Scale3x => scale3x(src, w, h, tmp),
                VideoFilter::Scale4x => {
                    // Two passes of scale2x: src -> mid (2x) -> tmp (4x).
                    let mut mid = vec![0u32; w * 2 * h * 2];
                    scale2x(src, w, h, &mut mid);
                    scale2x(&mid, w * 2, h * 2, tmp);
                }
                _ => {}
            }
            if nat == scale {
                dst.clear();
                dst.extend_from_slice(tmp);
            } else {
                // Top the natural factor up to the request with nearest: the
                // edges the pass found stay clean at any size.
                let edged = std::mem::take(tmp);
                nearest_fit(&edged, w * nat, h * nat, dst, dw, dh);
                *tmp = edged;
            }
        }
    }
    (dw, dh)
}
