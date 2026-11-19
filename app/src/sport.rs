//! SPORT, the VRAM serial port: whole-page copies and fills without the CPU
//! moving the words.
//!
//! There is no command register. The *address* is the command. Within the
//! 64 KiB SPORT window, three bits of the offset pick one of three windows, and
//! the low eleven bits name a page:
//!
//! | offset & 0xE000 | read                     | write                                  |
//! |-----------------|--------------------------|----------------------------------------|
//! | `0x0000`        | latch the source page    | copy the latched page here, under mask |
//! | `0x2000`        | nothing                  | set the fill value (not paged)         |
//! | `0x4000`        | nothing                  | fill this page with the value, under mask |
//! | anything else   | nothing                  | copy, as for `0x0000`                  |
//!
//! The value written to a copy or fill address is the mask. All of this is
//! read off the boot ROM's own SPORT memory test, which primes with a load from
//! `0x03200000 | (offset >> 9)`, commits with a store of `0xFFFFFFFF`, and then
//! compares exactly 512 words. That comparison is what fixes the page at
//! 2048 bytes.
//!
//! The window is word-addressed like every device register: the low two
//! offset bits are ignored, as the ARM's word accesses ignore them. So although
//! the `>> 9` in the address is finer than the page, the reachable page indices
//! step a whole page at a time and the 512 pages tile VRAM exactly.
//!
//! Page addresses are **offsets into VRAM**, not absolute: the ROM builds them
//! from bare literals while it fills and verifies at `VRAM_BASE + literal`.
//!
//! SPORT holds no reference to the rest of the machine. The system dispatcher
//! passes the window-relative offset and the memory it should act on.
#![forbid(unsafe_code)]

use crate::memory::{Memory, VRAM_BASE};

/// Bytes moved by one copy or fill. Fixed by the ROM's 512-word verify.
pub const PAGE_BYTES: u32 = 2048;

/// The shift from a VRAM byte offset to a SPORT address. Not the page size:
/// the low two bits of the result are a word's byte lanes and are ignored.
pub const ADDRESS_SHIFT: u32 = 9;

/// Window offsets within the SPORT region.
pub const COPY_WINDOW: u32 = 0x0000;
pub const VALUE_REGISTER: u32 = 0x2000;
pub const FILL_WINDOW: u32 = 0x4000;
pub const WINDOW_SPAN: u32 = 0x2000;

/// The three offset bits that choose a window.
pub const WINDOW_MASK: u32 = 0xe000;

/// The page index wraps within the VRAM SPORT can reach.
pub const INDEX_MASK: u32 = 0x7ff;

/// Byte-lane bits of a SPORT offset. Word accesses never see them.
pub const BYTE_LANE_MASK: u32 = 0x3;

/// The "unmasked transfer" command: every bit comes from the source.
pub const NO_MASK: u32 = 0xffff_ffff;

/// Which window an offset falls in. Reads and writes treat the windows
/// differently: only the copy window proper latches on a read, while a write
/// anywhere the value and fill windows do not claim is a copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Window {
    /// `0x0000`: read latches the source page, write copies.
    Copy,
    /// `0x2000`: the fill-value register.
    FillValue,
    /// `0x4000`: write fills a page.
    Fill,
    /// `0x6000`..`0xE000`: undocumented by the ROM. A write copies; a read does
    /// nothing, since inventing a meaning for it would be guessing.
    Unclaimed,
}

impl Window {
    pub const fn of(offset: u32) -> Self {
        match offset & WINDOW_MASK {
            COPY_WINDOW => Window::Copy,
            VALUE_REGISTER => Window::FillValue,
            FILL_WINDOW => Window::Fill,
            _ => Window::Unclaimed,
        }
    }
}

/// The absolute address of the page an offset names. The byte-lane bits are
/// dropped (word addressing) and the index wraps at `INDEX_MASK`, so the
/// highest page ends exactly at the end of VRAM.
pub const fn page_address(offset: u32) -> u32 {
    VRAM_BASE.wrapping_add((offset & INDEX_MASK & !BYTE_LANE_MASK) << ADDRESS_SHIFT)
}

/// Combine a destination word with incoming data under a SPORT mask.
///
/// A mask bit of **one protects** the destination bit; a **zero takes** the
/// source bit. The two fields are disjoint by construction, so the expression
/// is exactly `(destination & mask) | (source & !mask)`. The all-ones mask is
/// the port's separate unmasked command and never reaches this function.
///
/// Getting the polarity backwards writes the complement of the bits a title
/// asked for: the image's structure survives (the bits that should move still
/// move) while everything that should have been left alone is clobbered.
pub const fn apply_mask(destination: u32, source: u32, mask: u32) -> u32 {
    let protected_bits = destination & mask;
    let taken_bits = source & !mask;
    protected_bits | taken_bits
}

/// The VRAM serial port.
#[derive(Clone, Debug, Default)]
pub struct Sport {
    /// Absolute address of the page the next copy reads from.
    source_address: u32,
    fill_value: u32,
    copies: u64,
    fills: u64,
}

impl Sport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// A read from the SPORT window. In the copy window it latches the source
    /// page; nowhere does it touch memory or return data. `mem` is taken so the
    /// dispatcher drives reads and writes the same way.
    pub fn read(&mut self, offset: u32, _mem: &mut Memory) -> u32 {
        if Window::of(offset) == Window::Copy {
            self.source_address = page_address(offset);
        }
        0
    }

    /// A write to the SPORT window: set the fill value, copy a page or fill a
    /// page. For copy and fill, `value` is the mask.
    pub fn write(&mut self, offset: u32, value: u32, mem: &mut Memory) {
        match Window::of(offset) {
            Window::FillValue => self.fill_value = value,
            Window::Fill => self.fill_page(page_address(offset), value, mem),
            Window::Copy | Window::Unclaimed => self.copy_page(page_address(offset), value, mem),
        }
    }

    /// Copies performed since reset (including no-op self-copies).
    pub fn copies(&self) -> u64 {
        self.copies
    }

    /// Fills performed since reset.
    pub fn fills(&self) -> u64 {
        self.fills
    }

    /// Absolute address of the latched source page.
    pub fn latched_source(&self) -> u32 {
        self.source_address
    }

    pub fn fill_value(&self) -> u32 {
        self.fill_value
    }

    /// Copy the latched page into `destination`, one word at a time in
    /// ascending order through memory's ordinary word accessors.
    ///
    /// Going through `Memory::write32` keeps the DRAM write-watch informed,
    /// and any part of an address outside memory reads zero and drops writes,
    /// as for every unmapped address.
    fn copy_page(&mut self, destination: u32, mask: u32, mem: &mut Memory) {
        self.copies = self.copies.wrapping_add(1);
        let source = self.source_address;
        if destination == source {
            return;
        }
        for byte in (0..PAGE_BYTES).step_by(4) {
            let incoming = mem.read32(source.wrapping_add(byte));
            Self::store_masked(mem, destination.wrapping_add(byte), incoming, mask);
        }
    }

    /// Fill `destination` with the fill value, under `mask`.
    fn fill_page(&mut self, destination: u32, mask: u32, mem: &mut Memory) {
        self.fills = self.fills.wrapping_add(1);
        let incoming = self.fill_value;
        for byte in (0..PAGE_BYTES).step_by(4) {
            Self::store_masked(mem, destination.wrapping_add(byte), incoming, mask);
        }
    }

    fn store_masked(mem: &mut Memory, address: u32, incoming: u32, mask: u32) {
        let word = if mask == NO_MASK {
            incoming
        } else {
            apply_mask(mem.read32(address), incoming, mask)
        };
        mem.write32(address, word);
    }
}
