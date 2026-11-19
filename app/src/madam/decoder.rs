//! The pixel decoder: one source value in, a 16-bit colour, its multiplier,
//! its transparency and its saved V/H position bits out (WO 94/10644).
//!
//! A coded pixel is NOT an index. Only its low five bits select a PLUT entry;
//! the bits above carry a per-channel multiplier the pixel processor uses for
//! shading. Reading all eight bits of an eight-bit pixel as an index walks a
//! 32-entry palette off its end into whatever follows it in memory.
#![forbid(unsafe_code)]

use super::ccb::{Ccb, CCB_BGND, CCB_LD_PLUT, CCB_PACKED};
use crate::memory::Memory;

/// Entries in the engine's own palette.
pub const PLUT_ENTRIES: usize = 32;

/// The multiplier of an uncoded pixel: one in each of the three channels.
const UNIT_AMV: u16 = 0x49;

/// One decoded source pixel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Texel {
    /// The colour, after the blue-LSB substitution.
    pub color: u16,
    /// Three bits of multiplier per channel (red in bits 6-8).
    pub amv: u16,
    /// The source position bits (bit 15 and bit 0) saved before substitution.
    pub raw_vh: u16,
    /// The palette index that produced the colour; only meaningful when the
    /// cel's coding reads the PLUT; zero for uncoded pixels.
    pub index: u8,
    pub transparent: bool,
}

/// How the source value turns into a colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Coding {
    /// 1, 2 or 4 bits: an index into one block of the PLUT, the block chosen
    /// by the low bits of the CCB flags so cels can share one loaded palette.
    PaletteBlock { block: u32, mask: u32 },
    /// 6 bits: five of index and the top bit as the colour's bit 15.
    Coded6,
    /// 8 bits: five of index and a three-bit multiplier shared by all channels.
    Coded8,
    /// 8 bits read as RGB 3-3-2, widened to five bits a channel.
    Linear8,
    /// 16 bits: five of index, three bits of multiplier per channel, bit 15.
    Coded16,
    /// 16 bits that are the colour.
    Linear16,
}

/// Where the blue LSB comes from once bit 0 has been saved as the H bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlueLsb {
    Zero,
    Bit0,
    Bit4,
    Bit5,
}

impl BlueLsb {
    const fn from_field(field: u32) -> Self {
        match field & 3 {
            1 => Self::Bit0,
            2 => Self::Bit4,
            3 => Self::Bit5,
            _ => Self::Zero,
        }
    }

    const fn select(self, color: u16) -> u16 {
        match self {
            Self::Zero => 0,
            Self::Bit0 => color & 1,
            Self::Bit4 => (color >> 4) & 1,
            Self::Bit5 => (color >> 5) & 1,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct CelDecoder {
    coding: Coding,
    transparent_zero: bool,
    blue_lsb: BlueLsb,
    /// The engine's palette, loaded by a cel that asks and kept for the rest.
    plut: [u16; PLUT_ENTRIES],
}

impl Default for CelDecoder {
    fn default() -> Self {
        Self {
            coding: Coding::Linear16,
            transparent_zero: true,
            blue_lsb: BlueLsb::Zero,
            plut: [0; PLUT_ENTRIES],
        }
    }
}

impl CelDecoder {
    /// Configure for one cel, loading the PLUT if its CCB says to.
    /// `ccb_ctl0` supplies the blue-LSB rule for packed cels.
    pub(super) fn begin(&mut self, ccb: &Ccb, ccb_ctl0: u32, mem: &Memory) {
        let bpp = ccb.format.bits_per_pixel();
        let linear = ccb.is_linear();
        self.coding = match bpp {
            1 => Coding::PaletteBlock {
                block: (ccb.flags & 0x0f) * 2,
                mask: 1,
            },
            2 => Coding::PaletteBlock {
                block: (ccb.flags & 0x0e) * 2,
                mask: 3,
            },
            4 => Coding::PaletteBlock {
                block: (ccb.flags & 0x08) * 2,
                mask: 15,
            },
            6 => Coding::Coded6,
            8 if linear => Coding::Linear8,
            8 => Coding::Coded8,
            _ if linear => Coding::Linear16,
            _ => Coding::Coded16,
        };
        self.transparent_zero = ccb.flags & CCB_BGND == 0;
        self.blue_lsb = BlueLsb::from_field(if ccb.flags & CCB_PACKED != 0 {
            ccb_ctl0 >> 20
        } else {
            ccb.pre1 >> 12
        });

        if ccb.flags & CCB_LD_PLUT != 0 {
            let entries = match bpp {
                1 => 2,
                2 => 4,
                4 => 16,
                _ => PLUT_ENTRIES,
            };
            let base = ccb.plut_address & !1;
            let mut address = base;
            for entry in self.plut.iter_mut().take(entries) {
                *entry = read_be16_bytes(mem, address);
                address = address.wrapping_add(2);
            }
        }
    }

    fn plut(&self, index: u32) -> u16 {
        self.plut.get((index & 0x1f) as usize).copied().unwrap_or(0)
    }

    pub(super) fn decode(&self, value: u32) -> Texel {
        let (color, amv, index) = match self.coding {
            Coding::PaletteBlock { block, mask } => {
                let index = block + (value & mask);
                (self.plut(index), UNIT_AMV, index as u8)
            }
            Coding::Coded6 => {
                let top = if value & 0x20 != 0 { 0x8000 } else { 0 };
                (
                    (self.plut(value) & 0x7fff) | top,
                    UNIT_AMV,
                    (value & 0x1f) as u8,
                )
            }
            Coding::Coded8 => {
                let shade = (((value >> 6) & 3) * 2 + ((value >> 5) & 1)) as u16;
                (
                    self.plut(value),
                    (shade << 6) | (shade << 3) | shade,
                    (value & 0x1f) as u8,
                )
            }
            Coding::Linear8 => {
                let blue = value & 3;
                let green = (value >> 2) & 7;
                let red = (value >> 5) & 7;
                let widen3 = |c: u32| ((c << 2) + (c >> 1)) & 0x1f;
                let widen2 = ((blue << 3) + (blue << 1) + (blue >> 1)) & 0x1f;
                (
                    (widen3(red) << 10 | widen3(green) << 5 | widen2) as u16,
                    UNIT_AMV,
                    0,
                )
            }
            Coding::Coded16 => {
                let color = (self.plut(value) & 0x7fff) | (value & 0x8000) as u16;
                let amv =
                    (((value >> 11) & 7) << 6) | (((value >> 8) & 7) << 3) | ((value >> 5) & 7);
                (color, amv as u16, (value & 0x1f) as u8)
            }
            Coding::Linear16 => (value as u16, UNIT_AMV, 0),
        };
        Texel {
            color: (color & !1) | self.blue_lsb.select(color),
            amv,
            raw_vh: color & 0x8001,
            index,
            transparent: color & 0x7fff == 0 && self.transparent_zero,
        }
    }
}

/// A big-endian halfword assembled from two byte reads, so an odd address
/// reads the bytes it names rather than being aligned down.
pub(super) fn read_be16_bytes(mem: &Memory, address: u32) -> u16 {
    u16::from(mem.read8(address)) << 8 | u16::from(mem.read8(address.wrapping_add(1)))
}
