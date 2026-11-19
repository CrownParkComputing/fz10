//! The Cel Control Block: the DMA-facing word layout and the decoded form the
//! renderer works from.
//!
//! Field names and bit positions follow the SDK `hardware.h` and the Graphics
//! Programming Guide. A CCB is not a fixed structure: six words are always
//! fetched, and everything after them is a compact stream feeding
//! load-controlled registers. A field whose load flag is clear is absent from
//! memory, and the register keeps whatever the previous cel left in it.
#![forbid(unsafe_code)]

use crate::memory::Memory;

// --- FLAGS word ----------------------------------------------------------
//
// SKIP is the TOP bit and LAST the one below it, not the other way round: a
// cel meant to be skipped would otherwise be drawn, and the list would run on
// past the cel that was meant to end it.

/// Do not project this cel (its load flags still take effect).
pub const CCB_SKIP: u32 = 1 << 31;
/// This is the final cel in the list.
pub const CCB_LAST: u32 = 1 << 30;
/// NEXTPTR is absolute rather than relative.
pub const CCB_NP_ABS: u32 = 1 << 29;
/// SOURCEPTR is absolute.
pub const CCB_SP_ABS: u32 = 1 << 28;
/// PLUTPTR is absolute.
pub const CCB_PP_ABS: u32 = 1 << 27;
/// HDX, HDY, VDX, VDY follow.
pub const CCB_LD_SIZE: u32 = 1 << 26;
/// HDDX, HDDY follow.
pub const CCB_LD_PRS: u32 = 1 << 25;
/// PIXC follows.
pub const CCB_LD_PPMP: u32 = 1 << 24;
/// Load the PLUT from PLUTPTR.
pub const CCB_LD_PLUT: u32 = 1 << 23;
/// The preamble is carried in the CCB rather than at the front of the source.
pub const CCB_CCB_PRE: u32 = 1 << 22;
/// Load X and Y from the CCB.
pub const CCB_YOXY: u32 = 1 << 21;
pub const CCB_ACSC: u32 = 1 << 20;
pub const CCB_ALSC: u32 = 1 << 19;
/// A clockwise-wound cel may be drawn.
pub const CCB_ACW: u32 = 1 << 18;
/// A counter-clockwise-wound cel may be drawn.
pub const CCB_ACCW: u32 = 1 << 17;
pub const CCB_TWD: u32 = 1 << 16;
pub const CCB_LCE: u32 = 1 << 15;
pub const CCB_ACE: u32 = 1 << 14;
/// Limit vertical stretch of one source pixel to one destination row.
pub const CCB_MARIA: u32 = 1 << 12;
pub const CCB_PXOR: u32 = 1 << 11;
/// The PIXC AV field carries the secondary-source controls.
pub const CCB_USE_AV: u32 = 1 << 10;
/// The source is run-length coded.
pub const CCB_PACKED: u32 = 1 << 9;
/// The written V/H bits come from the source pixel rather than the origin.
pub const CCB_PLUT_POS: u32 = 1 << 6;
/// Colour zero is drawn rather than transparent.
pub const CCB_BGND: u32 = 1 << 5;
/// Pure black may be written.
pub const CCB_NO_BLK: u32 = 1 << 4;
pub const CCB_POVER_MASK: u32 = 0x180;

/// A CCB pointer is 24 bits. The upper byte is dropped BEFORE a relative base
/// is added, or a stray high bit turns a short offset into a far address.
pub const CCB_ADDRESS_MASK: u32 = 0x00ff_ffff;

// --- preamble words ------------------------------------------------------

const PRE0_FORMAT_MASK: u32 = 0x0000_0007;
/// Pixels carry their colour directly instead of through the PLUT. Bit FOUR.
pub const PRE0_LINEAR: u32 = 0x0000_0010;
const PRE0_HEIGHT_SHIFT: u32 = 6;
const PRE0_HEIGHT_MASK: u32 = 0x0000_03ff;
const PRE0_SKIP_SHIFT: u32 = 24;
const PRE0_SKIP_MASK: u32 = 0x0f;
const PRE1_WIDTH_MASK: u32 = 0x0000_07ff;
/// The source is stored in the framebuffer's interleaved row-pair layout.
pub const PRE1_LRFORM: u32 = 0x0000_0800;
/// Suppress CCBCTL0's H/V swap for this cel.
pub const PRE1_NO_SWAP: u32 = 0x0000_4000;

/// A cel larger than this in either direction is treated as corrupt. The size
/// fields cannot express more, so this is defence rather than the real bound.
pub const MAX_CEL_DIMENSION: u32 = 2048;

/// How a cel's source pixels are encoded (PRE0 bits 0-2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CelFormat {
    #[default]
    Unknown,
    /// 1 bit per pixel, through the PLUT.
    Indexed1,
    Indexed2,
    Indexed4,
    Indexed6,
    Indexed8,
    /// 16-bit pixels.
    Direct16,
}

impl CelFormat {
    pub const fn from_pre0(pre0: u32) -> Self {
        match pre0 & PRE0_FORMAT_MASK {
            1 => Self::Indexed1,
            2 => Self::Indexed2,
            3 => Self::Indexed4,
            4 => Self::Indexed6,
            5 => Self::Indexed8,
            6 => Self::Direct16,
            _ => Self::Unknown,
        }
    }

    /// How many bits one source pixel occupies; zero for an unknown format.
    pub const fn bits_per_pixel(self) -> u32 {
        match self {
            Self::Indexed1 => 1,
            Self::Indexed2 => 2,
            Self::Indexed4 => 4,
            Self::Indexed6 => 6,
            Self::Indexed8 => 8,
            Self::Direct16 => 16,
            Self::Unknown => 0,
        }
    }
}

/// Decode the pixel format from a CCB's PRE0 word.
pub const fn cel_format_from_pre0(pre0: u32) -> CelFormat {
    CelFormat::from_pre0(pre0)
}

/// One CCB, with its pointers resolved and the inherited registers filled in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ccb {
    pub flags: u32,
    pub next_address: u32,
    pub source_address: u32,
    pub plut_address: u32,

    /// Destination origin, 16.16.
    pub x: i32,
    pub y: i32,
    /// Per-column step (16.16; stored in memory as 12.20).
    pub hdx: i32,
    pub hdy: i32,
    /// Per-row step (16.16).
    pub vdx: i32,
    pub vdy: i32,
    /// Per-row change of the column step (16.16; stored as 12.20).
    pub hddx: i32,
    pub hddy: i32,

    pub pixc: u32,
    pub pre0: u32,
    pub pre1: u32,

    pub format: CelFormat,
    pub width: u32,
    pub height: u32,

    /// The hardware CURRENTCCB DMA cursor after loading this CCB: the six
    /// fixed words plus only the optional words its flags selected.
    pub fetch_end_address: u32,
}

impl Ccb {
    pub const fn has(&self, flag: u32) -> bool {
        self.flags & flag != 0
    }

    pub const fn is_packed(&self) -> bool {
        self.has(CCB_PACKED)
    }

    /// A cel the engine can project at all: a known format and a size inside
    /// the dimension guard.
    pub fn is_drawable(&self) -> bool {
        self.format != CelFormat::Unknown
            && (1..=MAX_CEL_DIMENSION).contains(&self.width)
            && (1..=MAX_CEL_DIMENSION).contains(&self.height)
    }

    /// No perspective stepping and square-on to one axis: the hardware's
    /// test for the scaling (rather than arbitrary) texel mapper.
    pub const fn is_axis_aligned(&self) -> bool {
        self.hddx == 0
            && self.hddy == 0
            && ((self.hdx == 0 && self.vdy == 0) || (self.hdy == 0 && self.vdx == 0))
    }

    /// Source pixels skipped at the front of every unpacked row (PRE0 SKIPX).
    pub const fn skip_x(&self) -> u32 {
        (self.pre0 >> PRE0_SKIP_SHIFT) & PRE0_SKIP_MASK
    }

    /// PRE0 VCNT plus one: rows, or row pairs for an LR-form cel.
    pub const fn vertical_count(&self) -> u32 {
        ((self.pre0 >> PRE0_HEIGHT_SHIFT) & PRE0_HEIGHT_MASK) + 1
    }

    pub const fn is_linear(&self) -> bool {
        self.pre0 & PRE0_LINEAR != 0
    }

    pub const fn is_lr_form(&self) -> bool {
        self.pre1 & PRE1_LRFORM != 0
    }
}

/// Reads consecutive big-endian words, the way the CCB DMA does.
struct WordStream {
    cursor: u32,
}

impl WordStream {
    fn next(&mut self, mem: &Memory) -> u32 {
        let value = mem.read32(self.cursor);
        self.cursor = self.cursor.wrapping_add(4);
        value
    }
}

// Word positions of the fixed head.
const FLAGS_WORD: u32 = 0;
const NEXT_WORD: u32 = 1;
const SOURCE_WORD: u32 = 2;
const PLUT_WORD: u32 = 3;
const X_WORD: u32 = 4;
const Y_WORD: u32 = 5;
const FIXED_WORDS: u32 = 6;

/// The engine's load-controlled registers. They outlive a cel: a compact CCB
/// that does not reload one inherits the preceding cel's value.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct CcbRegisters {
    x: i32,
    y: i32,
    hdx: i32,
    hdy: i32,
    vdx: i32,
    vdy: i32,
    hddx: i32,
    hddy: i32,
    pixc: u32,
    pre0: u32,
    pre1: u32,
}

impl CcbRegisters {
    /// Fetch one CCB, updating the registers its flags select.
    pub(super) fn load(&mut self, address: u32, mem: &Memory) -> Ccb {
        let word = |index: u32| mem.read32(address.wrapping_add(index.wrapping_mul(4)));
        let flags = word(FLAGS_WORD);

        // Relative pointers are measured from the word after the field.
        let pointer = |index: u32, absolute: u32| {
            let raw = word(index) & CCB_ADDRESS_MASK;
            if flags & absolute != 0 {
                raw
            } else {
                address.wrapping_add((index + 1) * 4).wrapping_add(raw)
            }
        };
        // A zero next pointer ends the list whatever the addressing mode.
        let next_address = if word(NEXT_WORD) & CCB_ADDRESS_MASK == 0 {
            0
        } else {
            pointer(NEXT_WORD, CCB_NP_ABS)
        };
        let mut source = WordStream {
            cursor: pointer(SOURCE_WORD, CCB_SP_ABS),
        };
        let plut_address = pointer(PLUT_WORD, CCB_PP_ABS);

        let mut stream = WordStream {
            cursor: address.wrapping_add(FIXED_WORDS * 4),
        };
        if flags & CCB_SKIP == 0 && flags & CCB_YOXY != 0 {
            self.x = word(X_WORD) as i32;
            self.y = word(Y_WORD) as i32;
        }
        // Horizontal vectors are stored 12.20 and become 16.16.
        if flags & CCB_LD_SIZE != 0 {
            self.hdx = (stream.next(mem) as i32) >> 4;
            self.hdy = (stream.next(mem) as i32) >> 4;
            self.vdx = stream.next(mem) as i32;
            self.vdy = stream.next(mem) as i32;
        }
        if flags & CCB_LD_PRS != 0 {
            self.hddx = (stream.next(mem) as i32) >> 4;
            self.hddy = (stream.next(mem) as i32) >> 4;
        }
        if flags & CCB_LD_PPMP != 0 {
            self.pixc = stream.next(mem);
        }

        // The preamble comes from the CCB under CCBPRE and otherwise from the
        // front of the source data, whose pixels then begin after it. A
        // packed cel never supplies PRE1: the register survives for the next
        // unpacked cel.
        let preamble = if flags & CCB_CCB_PRE != 0 {
            &mut stream
        } else {
            &mut source
        };
        self.pre0 = preamble.next(mem);
        if flags & CCB_PACKED == 0 {
            self.pre1 = preamble.next(mem);
        }

        let mut ccb = Ccb {
            flags,
            next_address,
            source_address: source.cursor,
            plut_address,
            x: self.x,
            y: self.y,
            hdx: self.hdx,
            hdy: self.hdy,
            vdx: self.vdx,
            vdy: self.vdy,
            hddx: self.hddx,
            hddy: self.hddy,
            pixc: self.pixc,
            pre0: self.pre0,
            pre1: self.pre1,
            format: CelFormat::from_pre0(self.pre0),
            width: (self.pre1 & PRE1_WIDTH_MASK) + 1,
            height: 0,
            fetch_end_address: stream.cursor,
        };
        ccb.height = ccb.vertical_count();
        ccb
    }
}
