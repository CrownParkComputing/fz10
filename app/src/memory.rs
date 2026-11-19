//! The machine's storage: DRAM, VRAM, the boot ROM and the battery-backed NVRAM,
//! as the CPU and the chips address them.
//!
//! This module holds memory only. Chip registers (CLIO, MADAM, SPORT) are
//! dispatched one level up in [`crate::system::System`], which owns a `Memory`
//! next to the chips; keeping the two apart is what lets the cel engine and the
//! display walk VRAM while the CPU is borrowed elsewhere.
//!
//! The 3DO is big-endian as the CPU sees it. Every word and halfword access
//! here assembles bytes most-significant first, whatever the host order.
#![forbid(unsafe_code)]

/// DRAM: two megabytes in one run (DRAM1 at 0, DRAM2 at 1 MB).
pub const DRAM_BASE: u32 = 0x0000_0000;
pub const DRAM_SIZE: u32 = 2 * 1024 * 1024;
/// VRAM: one megabyte directly above DRAM.
pub const VRAM_BASE: u32 = 0x0020_0000;
pub const VRAM_SIZE: u32 = 1024 * 1024;
/// The boot ROM, which is also where the reset vector lands after the overlay.
pub const ROM_BASE: u32 = 0x0300_0000;
pub const ROM_SIZE: u32 = 1024 * 1024;

/// NVRAM: 32 KiB stored one byte per 32-bit word (the data lines sit on the low
/// byte), so it occupies a 128 KiB window, mirrored once 0x20000 higher.
pub const NVRAM_BASE: u32 = 0x0314_0000;
pub const NVRAM_SIZE: u32 = 32 * 1024;
pub const NVRAM_STRIDE: u32 = 4;
pub const NVRAM_MIRROR: u32 = 0x0002_0000;
pub const NVRAM_WINDOW: u32 = NVRAM_SIZE * NVRAM_STRIDE;
pub const NVRAM_SPAN: u32 = NVRAM_MIRROR + NVRAM_WINDOW;

/// SPORT decodes a 64 KiB window, not the megabyte the region is listed as.
pub const SPORT_BASE: u32 = 0x0320_0000;
pub const SPORT_SIZE: u32 = 0x0001_0000;
pub const MADAM_BASE: u32 = 0x0330_0000;
pub const MADAM_SIZE: u32 = 0x0010_0000;
pub const CLIO_BASE: u32 = 0x0340_0000;
pub const CLIO_SIZE: u32 = 0x0010_0000;

/// A formatted, empty NVRAM image, as the boot ROM itself writes it on a
/// console with no disc. A zeroed NVRAM has no filesystem, and titles booted
/// straight from disc then report it full rather than formatting it.
const FORMATTED_NVRAM: [u8; 172] = [
    0x01, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x6E, 0x76, 0x72, 0x61, 0x6D, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x80, 0x00, //
    0xFF, 0xFF, 0xFF, 0xFE, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x84, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x85, 0x5A, 0x02, 0xB6, 0x00, 0x00, 0x00, 0x98, 0x00, 0x00, 0x00, 0x98, //
    0x00, 0x00, 0x00, 0x14, 0x00, 0x00, 0x00, 0x14, 0x7A, 0xA5, 0x65, 0xBD, //
    0x00, 0x00, 0x00, 0x84, 0x00, 0x00, 0x00, 0x84, 0x00, 0x00, 0x7F, 0x68, //
    0x00, 0x00, 0x00, 0x14,
];

/// Whether an address belongs to a chip rather than to memory. Byte and
/// halfword accesses to these go through the containing word, because the
/// registers are word-wide.
pub fn is_device(address: u32) -> bool {
    in_window(address, SPORT_BASE, SPORT_SIZE)
        || in_window(address, MADAM_BASE, MADAM_SIZE)
        || in_window(address, CLIO_BASE, CLIO_SIZE)
}

/// `base <= address < base + size`, without overflowing.
pub fn in_window(address: u32, base: u32, size: u32) -> bool {
    address.wrapping_sub(base) < size && address >= base
}

// ---------------------------------------------------------------------------
// Big-endian helpers over byte slices. Out-of-range reads return zero and
// out-of-range writes are dropped: an address computed from guest data must
// never be able to panic the host.
// ---------------------------------------------------------------------------

/// Read a big-endian word at `offset`, or zero if any byte is out of range.
pub fn load_be32(bytes: &[u8], offset: usize) -> u32 {
    match offset.checked_add(4).and_then(|end| bytes.get(offset..end)) {
        Some(&[a, b, c, d]) => u32::from_be_bytes([a, b, c, d]),
        _ => 0,
    }
}

/// Read a big-endian halfword at `offset`, or zero if out of range.
pub fn load_be16(bytes: &[u8], offset: usize) -> u16 {
    match offset.checked_add(2).and_then(|end| bytes.get(offset..end)) {
        Some(&[a, b]) => u16::from_be_bytes([a, b]),
        _ => 0,
    }
}

/// Store a big-endian word at `offset`; dropped if out of range.
pub fn store_be32(bytes: &mut [u8], offset: usize, value: u32) {
    if let Some(slot) = offset
        .checked_add(4)
        .and_then(|end| bytes.get_mut(offset..end))
    {
        slot.copy_from_slice(&value.to_be_bytes());
    }
}

/// Store a big-endian halfword at `offset`; dropped if out of range.
pub fn store_be16(bytes: &mut [u8], offset: usize, value: u16) {
    if let Some(slot) = offset
        .checked_add(2)
        .and_then(|end| bytes.get_mut(offset..end))
    {
        slot.copy_from_slice(&value.to_be_bytes());
    }
}

/// Which 4 KiB pages of DRAM have been written since the CPU's decode cache
/// was last brought up to date. A page set rather than a low/high range, so two
/// writes at opposite ends of memory do not invalidate everything between them.
#[derive(Clone, Debug)]
pub struct WriteWatch {
    bits: [u64; WriteWatch::WORDS],
    dirty: bool,
}

impl WriteWatch {
    pub const PAGE_BYTES: u32 = 4096;
    pub const PAGES: u32 = DRAM_SIZE / Self::PAGE_BYTES;
    const WORDS: usize = (Self::PAGES as usize).div_ceil(64);

    pub fn new() -> Self {
        Self {
            bits: [0; Self::WORDS],
            dirty: false,
        }
    }

    /// Record a write of `length` bytes at `address`.
    pub fn note(&mut self, address: u32, length: u32) {
        if length == 0 {
            return;
        }
        let first = address / Self::PAGE_BYTES;
        let last = address.saturating_add(length - 1) / Self::PAGE_BYTES;
        for page in first..=last.min(Self::PAGES.saturating_sub(1)) {
            if let Some(word) = self.bits.get_mut((page >> 6) as usize) {
                *word |= 1u64 << (page & 63);
            }
        }
        self.dirty = true;
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn page_dirty(&self, page: u32) -> bool {
        self.bits
            .get((page >> 6) as usize)
            .is_some_and(|word| (word >> (page & 63)) & 1 != 0)
    }

    /// The dirty pages, in ascending order.
    pub fn dirty_pages(&self) -> impl Iterator<Item = u32> + '_ {
        (0..Self::PAGES).filter(|&page| self.page_dirty(page))
    }

    pub fn clear(&mut self) {
        self.bits = [0; Self::WORDS];
        self.dirty = false;
    }
}

impl Default for WriteWatch {
    fn default() -> Self {
        Self::new()
    }
}

/// A write counter per VRAM page. Nothing in the machine reads it: it is how a
/// hi-res mirror of a framebuffer (see [`crate::hires`]) learns which pages
/// software, SPORT or DMA changed since it last looked. Each reader keeps its
/// own copy of the counters it has seen, so any number of readers can share
/// one watch and nobody has to clear it.
#[derive(Clone, Debug)]
pub struct VramWatch {
    generations: Vec<u32>,
}

impl VramWatch {
    pub const PAGE_BYTES: u32 = 4096;
    pub const PAGES: u32 = VRAM_SIZE / Self::PAGE_BYTES;

    pub fn new() -> Self {
        Self {
            generations: vec![0; Self::PAGES as usize],
        }
    }

    /// Record a write of `length` bytes at VRAM offset `offset`.
    #[inline]
    pub fn note(&mut self, offset: u32, length: u32) {
        let first = offset / Self::PAGE_BYTES;
        let last = offset.saturating_add(length.saturating_sub(1)) / Self::PAGE_BYTES;
        for page in first..=last {
            if let Some(generation) = self.generations.get_mut(page as usize) {
                *generation = generation.wrapping_add(1);
            }
        }
    }

    /// Every page changed (a reset clears all of VRAM).
    pub fn note_all(&mut self) {
        for generation in &mut self.generations {
            *generation = generation.wrapping_add(1);
        }
    }

    /// The page's counter; a reader that saw a different value must look again.
    pub fn generation(&self, page: u32) -> u32 {
        self.generations.get(page as usize).copied().unwrap_or(0)
    }
}

impl Default for VramWatch {
    fn default() -> Self {
        Self::new()
    }
}

/// Why a BIOS image was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RomError {
    Empty,
    TooLarge,
}

/// The machine's memories.
#[derive(Clone)]
pub struct Memory {
    dram: Vec<u8>,
    vram: Vec<u8>,
    rom: Vec<u8>,
    nvram: Vec<u8>,
    nvram_dirty: bool,
    bios_loaded: bool,
    /// At reset the low two megabytes read ROM; the first write anywhere in that
    /// range takes the overlay away for good. A write, not a control register.
    rom_overlay: bool,
    write_watch: WriteWatch,
    vram_watch: VramWatch,
}

impl Memory {
    pub fn new() -> Self {
        let mut memory = Self {
            dram: vec![0; DRAM_SIZE as usize],
            vram: vec![0; VRAM_SIZE as usize],
            rom: vec![0; ROM_SIZE as usize],
            nvram: vec![0; NVRAM_SIZE as usize],
            nvram_dirty: false,
            bios_loaded: false,
            rom_overlay: true,
            write_watch: WriteWatch::new(),
            vram_watch: VramWatch::new(),
        };
        memory.format_nvram();
        memory
    }

    /// Clear the volatile memories and restore the ROM overlay. ROM and NVRAM
    /// survive a reset, as they do in hardware.
    pub fn reset(&mut self) {
        self.dram.fill(0);
        self.vram.fill(0);
        self.vram_watch.note_all();
        self.write_watch.clear();
        self.rom_overlay = true;
    }

    /// Install a BIOS image. The rest of the ROM window reads zero.
    pub fn load_rom(&mut self, image: &[u8]) -> Result<(), RomError> {
        if image.is_empty() {
            self.bios_loaded = false;
            return Err(RomError::Empty);
        }
        if image.len() > self.rom.len() {
            self.bios_loaded = false;
            return Err(RomError::TooLarge);
        }
        self.rom.fill(0);
        if let Some(head) = self.rom.get_mut(..image.len()) {
            head.copy_from_slice(image);
        }
        self.bios_loaded = true;
        Ok(())
    }

    pub fn bios_loaded(&self) -> bool {
        self.bios_loaded
    }

    pub fn rom_overlay(&self) -> bool {
        self.rom_overlay
    }

    // --- NVRAM ------------------------------------------------------------

    /// Back to formatted-but-empty, where a console that has been switched on
    /// before starts from.
    pub fn format_nvram(&mut self) {
        self.nvram.fill(0);
        if let Some(head) = self.nvram.get_mut(..FORMATTED_NVRAM.len()) {
            head.copy_from_slice(&FORMATTED_NVRAM);
        }
        self.nvram_dirty = true;
    }

    /// The NVRAM as stored bytes: `NVRAM_SIZE` long, not the window size.
    pub fn nvram(&self) -> &[u8] {
        &self.nvram
    }

    /// Put a saved image back. Taken as-is; only the length is checked.
    pub fn restore_nvram(&mut self, image: &[u8]) -> bool {
        if image.len() != self.nvram.len() {
            return false;
        }
        self.nvram.copy_from_slice(image);
        self.nvram_dirty = false;
        true
    }

    pub fn nvram_dirty(&self) -> bool {
        self.nvram_dirty
    }

    pub fn clear_nvram_dirty(&mut self) {
        self.nvram_dirty = false;
    }

    fn nvram_index(address: u32) -> usize {
        ((address.wrapping_sub(NVRAM_BASE) % NVRAM_MIRROR) / NVRAM_STRIDE) as usize
    }

    fn nvram_byte_lane(address: u32) -> bool {
        (address.wrapping_sub(NVRAM_BASE) % NVRAM_MIRROR) % NVRAM_STRIDE == NVRAM_STRIDE - 1
    }

    // --- raw views for the chips -----------------------------------------

    pub fn dram(&self) -> &[u8] {
        &self.dram
    }
    pub fn dram_mut(&mut self) -> &mut [u8] {
        &mut self.dram
    }
    pub fn vram(&self) -> &[u8] {
        &self.vram
    }
    /// Raw VRAM for bulk writers. The whole of it counts as written.
    pub fn vram_mut(&mut self) -> &mut [u8] {
        self.vram_watch.note_all();
        &mut self.vram
    }
    pub fn rom(&self) -> &[u8] {
        &self.rom
    }

    pub fn write_watch(&self) -> &WriteWatch {
        &self.write_watch
    }
    pub fn write_watch_mut(&mut self) -> &mut WriteWatch {
        &mut self.write_watch
    }
    pub fn vram_watch(&self) -> &VramWatch {
        &self.vram_watch
    }

    // --- CPU-visible memory access ----------------------------------------
    //
    // These cover memory only. Device windows read zero and drop writes here;
    // `System` routes them to the chips before falling back to these.

    /// Word read; the low two address bits are ignored, as on the ARM.
    pub fn read32(&self, address: u32) -> u32 {
        let addr = address & !3;
        if addr < DRAM_SIZE {
            return if self.rom_overlay {
                load_be32(&self.rom, addr as usize)
            } else {
                load_be32(&self.dram, addr as usize)
            };
        }
        if in_window(addr, VRAM_BASE, VRAM_SIZE) {
            return load_be32(&self.vram, (addr - VRAM_BASE) as usize);
        }
        if in_window(addr, ROM_BASE, ROM_SIZE) {
            return load_be32(&self.rom, (addr - ROM_BASE) as usize);
        }
        if in_window(addr, NVRAM_BASE, NVRAM_SPAN) {
            return self
                .nvram
                .get(Self::nvram_index(addr))
                .copied()
                .map_or(0, u32::from);
        }
        0
    }

    pub fn read16(&self, address: u32) -> u16 {
        let addr = address & !1;
        if addr < DRAM_SIZE {
            return if self.rom_overlay {
                load_be16(&self.rom, addr as usize)
            } else {
                load_be16(&self.dram, addr as usize)
            };
        }
        if in_window(addr, VRAM_BASE, VRAM_SIZE) {
            return load_be16(&self.vram, (addr - VRAM_BASE) as usize);
        }
        if in_window(addr, ROM_BASE, ROM_SIZE) {
            return load_be16(&self.rom, (addr - ROM_BASE) as usize);
        }
        0
    }

    pub fn read8(&self, address: u32) -> u8 {
        if address < DRAM_SIZE {
            let source = if self.rom_overlay {
                &self.rom
            } else {
                &self.dram
            };
            return source.get(address as usize).copied().unwrap_or(0);
        }
        if in_window(address, VRAM_BASE, VRAM_SIZE) {
            return self
                .vram
                .get((address - VRAM_BASE) as usize)
                .copied()
                .unwrap_or(0);
        }
        if in_window(address, ROM_BASE, ROM_SIZE) {
            return self
                .rom
                .get((address - ROM_BASE) as usize)
                .copied()
                .unwrap_or(0);
        }
        if in_window(address, NVRAM_BASE, NVRAM_SPAN) && Self::nvram_byte_lane(address) {
            return self
                .nvram
                .get(Self::nvram_index(address))
                .copied()
                .unwrap_or(0);
        }
        0
    }

    /// Instruction fetch: ROM, low memory (through the overlay) and VRAM.
    pub fn fetch32(&self, address: u32) -> u32 {
        let addr = address & !3;
        if in_window(addr, ROM_BASE, ROM_SIZE) {
            return load_be32(&self.rom, (addr - ROM_BASE) as usize);
        }
        if addr < DRAM_SIZE {
            return if self.rom_overlay {
                load_be32(&self.rom, addr as usize)
            } else {
                load_be32(&self.dram, addr as usize)
            };
        }
        if in_window(addr, VRAM_BASE, VRAM_SIZE) {
            return load_be32(&self.vram, (addr - VRAM_BASE) as usize);
        }
        0
    }

    pub fn write32(&mut self, address: u32, value: u32) {
        let addr = address & !3;
        if addr < DRAM_SIZE {
            self.rom_overlay = false;
            store_be32(&mut self.dram, addr as usize, value);
            self.write_watch.note(addr, 4);
        } else if in_window(addr, VRAM_BASE, VRAM_SIZE) {
            store_be32(&mut self.vram, (addr - VRAM_BASE) as usize, value);
            self.vram_watch.note(addr - VRAM_BASE, 4);
        } else if in_window(addr, NVRAM_BASE, NVRAM_SPAN) {
            if let Some(byte) = self.nvram.get_mut(Self::nvram_index(addr)) {
                *byte = (value & 0xff) as u8;
                self.nvram_dirty = true;
            }
        }
        // ROM is read-only; device windows are not memory.
    }

    pub fn write16(&mut self, address: u32, value: u16) {
        let addr = address & !1;
        if addr < DRAM_SIZE {
            self.rom_overlay = false;
            store_be16(&mut self.dram, addr as usize, value);
            self.write_watch.note(addr, 2);
        } else if in_window(addr, VRAM_BASE, VRAM_SIZE) {
            store_be16(&mut self.vram, (addr - VRAM_BASE) as usize, value);
            self.vram_watch.note(addr - VRAM_BASE, 2);
        }
    }

    pub fn write8(&mut self, address: u32, value: u8) {
        if address < DRAM_SIZE {
            self.rom_overlay = false;
            if let Some(byte) = self.dram.get_mut(address as usize) {
                *byte = value;
            }
            self.write_watch.note(address, 1);
        } else if in_window(address, VRAM_BASE, VRAM_SIZE) {
            if let Some(byte) = self.vram.get_mut((address - VRAM_BASE) as usize) {
                *byte = value;
            }
            self.vram_watch.note(address - VRAM_BASE, 1);
        } else if in_window(address, NVRAM_BASE, NVRAM_SPAN) && Self::nvram_byte_lane(address) {
            if let Some(byte) = self.nvram.get_mut(Self::nvram_index(address)) {
                *byte = value;
                self.nvram_dirty = true;
            }
        }
    }
}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}
