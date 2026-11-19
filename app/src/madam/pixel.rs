//! The pixel processor (PPMP), per WO 94/10644: it scales the primary source,
//! optionally combines it with a secondary source (a constant, the frame
//! buffer or the pixel itself), clips, and decides the two sub-position bits
//! written with the colour.
//!
//! Every pixel the engine writes to a configured framebuffer goes through
//! here. Writing the source straight out instead loses all of a cel's
//! brightness and blending.
#![forbid(unsafe_code)]

use super::ccb::{
    Ccb, CCB_NO_BLK, CCB_PLUT_POS, CCB_POVER_MASK, CCB_PXOR, CCB_USE_AV, PRE1_NO_SWAP,
};

// CCBCTL0: the fields the projector acts on.
pub const CTL0_B15_MASK: u32 = 0xc000_0000;
pub const CTL0_B15_ZERO: u32 = 0x0000_0000;
pub const CTL0_B15_ONE: u32 = 0x4000_0000;
pub const CTL0_B15_PDC: u32 = 0xc000_0000;
pub const CTL0_B0_MASK: u32 = 0x3000_0000;
pub const CTL0_B0_ZERO: u32 = 0x0000_0000;
pub const CTL0_B0_ONE: u32 = 0x1000_0000;
pub const CTL0_B0_PPMP: u32 = 0x2000_0000;
pub const CTL0_B0_PDC: u32 = 0x3000_0000;
pub const CTL0_SWAP_HV: u32 = 0x0800_0000;

/// One channel through the multiply/divide stage: multiplied by one of eight
/// factors and shifted down by one of four. A divide field of zero is a shift
/// of FOUR, not of none; reading it as none makes every cel sixteen times too
/// bright.
const fn scale(multiplier: u32, divider: u32, value: u32) -> i32 {
    let shift = (divider.wrapping_sub(1) & 3) + 1;
    (((value & 0x1f) * ((multiplier & 7) + 1)) >> shift) as i32
}

const fn clamp_channel(value: i32) -> i32 {
    if value < 0 {
        0
    } else if value > 31 {
        31
    } else {
        value
    }
}

/// Sign-extend from five bits, so a difference can come out negative.
const fn sign_extend5(value: i32) -> i32 {
    ((value << 3) as i8 as i32) >> 3
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rgb {
    r: i32,
    g: i32,
    b: i32,
}

impl Rgb {
    const fn split(pixel: u32) -> Self {
        Self {
            r: ((pixel >> 10) & 0x1f) as i32,
            g: ((pixel >> 5) & 0x1f) as i32,
            b: (pixel & 0x1f) as i32,
        }
    }

    fn map(self, f: impl Fn(i32) -> i32) -> Self {
        Self {
            r: f(self.r),
            g: f(self.g),
            b: f(self.b),
        }
    }

    fn zip(self, other: Self, f: impl Fn(i32, i32) -> i32) -> Self {
        Self {
            r: f(self.r, other.r),
            g: f(self.g, other.g),
            b: f(self.b, other.b),
        }
    }

    const fn pack(self) -> u32 {
        ((self.r & 0x1f) as u32) << 10 | ((self.g & 0x1f) as u32) << 5 | (self.b & 0x1f) as u32
    }
}

/// The fields of one 16-bit half of PIXC.
#[derive(Clone, Copy, Debug)]
struct PixcControl {
    /// DV2: final divide by two.
    final_halve: u32,
    av: u32,
    /// S2: secondary source.
    secondary: u32,
    /// DV1: primary divider.
    divider: u32,
    /// MXF: primary multiplier.
    multiplier: u32,
    /// MS: where the multiplier comes from.
    multiplier_select: u32,
    /// S1: primary source is the frame buffer rather than the pixel.
    primary_is_frame: bool,
}

impl PixcControl {
    const fn decode(control: u32) -> Self {
        Self {
            final_halve: control & 1,
            av: (control >> 1) & 0x1f,
            secondary: (control >> 6) & 3,
            divider: (control >> 8) & 3,
            multiplier: (control >> 10) & 7,
            multiplier_select: (control >> 13) & 3,
            primary_is_frame: (control >> 15) & 1 != 0,
        }
    }
}

/// Per-cel processor state, fixed when the cel starts.
#[derive(Clone, Copy, Debug)]
pub(super) struct PixelProcessor {
    flags: u32,
    pixc: u32,
    pre1: u32,
    ccb_ctl0: u32,
    pmode_or: u32,
    pmode_and: u32,
    origin_vh: u32,
}

impl Default for PixelProcessor {
    fn default() -> Self {
        Self {
            flags: 0,
            pixc: 0,
            pre1: 0,
            ccb_ctl0: 0,
            pmode_or: 0,
            pmode_and: 0xffff,
            origin_vh: 0,
        }
    }
}

impl PixelProcessor {
    pub(super) fn begin(&mut self, ccb: &Ccb, ccb_ctl0: u32) {
        let pmode = ccb.flags & CCB_POVER_MASK;
        *self = Self {
            flags: ccb.flags,
            pixc: ccb.pixc,
            pre1: ccb.pre1,
            ccb_ctl0,
            pmode_or: if pmode == 0x180 { 0x8000 } else { 0 },
            pmode_and: if pmode == 0x100 { 0x7fff } else { 0xffff },
            origin_vh: (ccb.x as u32 & 1) | ((ccb.y as u32 & 1) << 15),
        };
    }

    /// Whether each half of PIXC reads the frame. When neither does, a
    /// texel's output is the same wherever it lands. A cel whose halves disagree
    /// is mixed: the half a texel lands in decides, and only those cels need
    /// the per-texel question asked.
    pub(super) fn frame_reading_halves(&self) -> (bool, bool) {
        let reads = |half: u32| {
            let control = PixcControl::decode(half);
            control.primary_is_frame || control.secondary == 2
        };
        (reads(self.pixc & 0xffff), reads(self.pixc >> 16))
    }

    /// Whether the control word THIS source pixel selects reads the frame.
    /// A cel can mix the two halves: a sprite blends its shadow texels
    /// against the frame while the rest of it ignores the frame entirely.
    pub(super) fn texel_reads_frame(&self, source: u16) -> bool {
        let pixel = (u32::from(source) | self.pmode_or) & self.pmode_and;
        let control = PixcControl::decode(if pixel & 0x8000 != 0 {
            self.pixc >> 16
        } else {
            self.pixc & 0xffff
        });
        control.primary_is_frame || control.secondary == 2
    }

    /// Combine a decoded source pixel with the frame-buffer pixel beneath it,
    /// returning the word to store.
    pub(super) fn blend(&self, source: u16, amv: u16, frame: u16, raw_vh: u16) -> u16 {
        let pixel = (u32::from(source) | self.pmode_or) & self.pmode_and;
        let frame = u32::from(frame);
        let amv = u32::from(amv);

        // Two control words, chosen by the pixel's top bit: one cel, two blends.
        let control = PixcControl::decode(if pixel & 0x8000 != 0 {
            self.pixc >> 16
        } else {
            self.pixc & 0xffff
        });
        let (negate, extend, no_clip, dv3) = if self.flags & CCB_USE_AV != 0 {
            let av = control.av;
            (
                av & 1,
                (av >> 1) & 1 != 0,
                (av >> 2) & 1 != 0,
                (av >> 3) & 3,
            )
        } else {
            (0, false, false, 0)
        };

        let input = Rgb::split(if control.primary_is_frame {
            frame
        } else {
            pixel
        });
        let pixel_rgb = Rgb::split(pixel);

        let second = match control.secondary {
            0 => Rgb { r: 0, g: 0, b: 0 },
            1 => {
                let constant = (control.av >> dv3) as i32;
                Rgb {
                    r: constant,
                    g: constant,
                    b: constant,
                }
            }
            2 => Rgb::split(frame).map(|c| c >> dv3),
            _ => pixel_rgb.map(|c| c >> dv3),
        };

        let first = match control.multiplier_select {
            0 => input.map(|c| scale(control.multiplier, control.divider, c as u32)),
            // The multiplier comes from the pixel, three bits a channel.
            1 => Rgb {
                r: scale(amv >> 6, control.divider, input.r as u32),
                g: scale(amv >> 3, control.divider, input.g as u32),
                b: scale(amv, control.divider, input.b as u32),
            },
            // PDC multiply and divide from each channel's own bits.
            2 => input.zip(pixel_rgb, |c, p| {
                scale(p as u32 >> 2, p as u32 & 3, c as u32)
            }),
            // PDC_MFONLY: the channel's upper bits multiply, PIXC's divider
            // still divides.
            _ => input.zip(pixel_rgb, |c, p| {
                scale(p as u32 >> 2, control.divider, c as u32)
            }),
        };

        // PXOR: keep the first operand whole, or take it as the XOR operand.
        let (keep_mask, xor_mask) = if self.flags & CCB_PXOR != 0 {
            (0, 0x1f)
        } else {
            (0xff, 0)
        };
        let keep = first.map(|c| c & keep_mask);
        let first = first.map(|c| c & xor_mask);

        let mut other = if negate != 0 {
            second.map(|c| c ^ 0xff)
        } else {
            second.zip(first, |s, f| s ^ f)
        };
        if extend {
            other = other.map(sign_extend5);
        }

        let negate = negate as i32;
        let mut out = keep.zip(other, |k, o| (k + o + negate) >> control.final_halve);
        if !no_clip {
            out = out.map(clamp_channel);
        }

        let mut result = out.pack();
        // A cel that may not write pure black writes the darkest non-black.
        if self.flags & CCB_NO_BLK == 0 && result == 0 {
            result = 1 << 10;
        }

        (((result & 0x7ffe) | self.position_bits(u32::from(raw_vh), result)) & 0xffff) as u16
    }

    /// The two sub-position bits (15 = V, 0 = H) the projector writes.
    fn position_bits(&self, raw_vh: u32, result: u32) -> u32 {
        let mut vh = if self.flags & CCB_PLUT_POS != 0 {
            raw_vh
        } else {
            self.origin_vh
        };
        if self.ccb_ctl0 & CTL0_SWAP_HV != 0 && self.pre1 & PRE1_NO_SWAP == 0 {
            vh = (vh >> 15) | ((vh & 1) << 15);
        }
        match self.ccb_ctl0 & CTL0_B15_MASK {
            CTL0_B15_ZERO => vh &= !0x8000,
            CTL0_B15_ONE => vh |= 0x8000,
            _ => {}
        }
        match self.ccb_ctl0 & CTL0_B0_MASK {
            CTL0_B0_ZERO => vh &= !1,
            CTL0_B0_ONE => vh |= 1,
            CTL0_B0_PPMP => vh = (vh & !1) | (result & 1),
            _ => {}
        }
        vh
    }
}
