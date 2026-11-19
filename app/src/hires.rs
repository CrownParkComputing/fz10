//! Internal-resolution rendering: hi-res mirrors of the framebuffers MADAM
//! draws into.
//!
//! The 3DO draws everything as cels: textured quads whose corners MADAM steps
//! in 16.16 fixed point and then truncates to whole pixels. That truncation is
//! where the jagged polygon edges come from, and it is the only thing a higher
//! internal resolution changes. With a scale `k` above one, every cel is
//! rasterized a second time with its corners multiplied by `k` (before the
//! truncation), into a [`HiResSurface`]: a `k`x mirror of the framebuffer it
//! targets. Texel colours are decoded once and stay native; only where each
//! texel lands is resolved more finely.
//!
//! VRAM stays the ground truth. The cel engine still writes every native
//! pixel exactly as it does at 1x, so the machine (and anything that reads the
//! framebuffer back) cannot tell the difference; the mirror is output only.
//!
//! Anything else that writes a framebuffer (the CPU, SPORT clears and copies,
//! DMA) is picked up through [`crate::memory::VramWatch`]: before a cel list
//! draws into a surface and before the display reads a line of one, the pages
//! that changed are compared pixel by pixel against a native-resolution
//! shadow of what the mirror was built from, and only pixels that differ are
//! re-mirrored (as a `k`x`k` block). A single pixel the CPU writes over a
//! hi-res polygon therefore costs one block, not the page.
//!
//! The display falls back to the native line, repeated, for any line that no
//! surface covers: screens drawn only by the CPU still show correctly.
//!
//! Cost: the hi-res pass of a large cel list runs on a helper thread while
//! the walk carries on natively (see `madam::hires_pass`), so on a machine
//! with a spare core the native timing barely moves; scale one does none of
//! this at all.
#![forbid(unsafe_code)]

use crate::memory::{load_be16, Memory, VramWatch, VRAM_SIZE};

/// The largest internal scale. Software rasterization at 8x would be 64 times
/// the native fill, which is not real time; a front end that wants more scales
/// the 4x picture on the GPU.
pub const MAX_INTERNAL_SCALE: u32 = 4;

/// Surfaces kept at once. Titles double- or triple-buffer, and may also draw
/// into an off-screen buffer; the least recently used one is dropped first.
const MAX_SURFACES: usize = 4;

/// Framebuffers taller than this are not mirrored (no 3DO display mode is).
const MAX_HEIGHT: u32 = 1024;

/// A clamped, valid internal scale.
pub fn clamp_scale(scale: u32) -> u32 {
    scale.clamp(1, MAX_INTERNAL_SCALE)
}

/// One framebuffer, mirrored at `scale` times its resolution, in raw 16-bit
/// pixel form (bit 15 keeps its CLUT-bypass meaning).
#[derive(Clone, Debug)]
pub struct HiResSurface {
    /// VRAM offset of the buffer.
    base: u32,
    /// Bytes from one row pair to the next (MADAM's write stride).
    stride: u32,
    /// Native pixels per row (`stride / 4`) and rows.
    width: u32,
    height: u32,
    scale: u32,
    /// `(width * scale) * (height * scale)`, row-major (not interleaved).
    pixels: Vec<u16>,
    /// The native pixel each block was last made from, `width * height`.
    shadow: Vec<u16>,
    /// The [`VramWatch`] generation last reconciled, per page of the region.
    page_cursors: Vec<u32>,
    first_page: u32,
    last_used: u64,
    /// Where the cel engine's current destination sits inside this buffer, in
    /// native pixels. A game can point the cel engine at a window into the
    /// frame — Road Rash draws its race view into one — and the drawing then
    /// belongs in the displayed buffer's own surface, offset by this.
    origin_x: u32,
    origin_y: u32,
}

impl HiResSurface {
    /// A surface for the buffer at VRAM offset `base`, filled by repeating
    /// each native pixel. `None` for a geometry that cannot be a framebuffer.
    fn new(base: u32, stride: u32, height: u32, scale: u32, mem: &Memory) -> Option<Self> {
        let width = stride / 4;
        if width == 0 || height == 0 || height > MAX_HEIGHT || base >= VRAM_SIZE {
            return None;
        }
        let scale = clamp_scale(scale);
        let hires =
            (width as usize * scale as usize).checked_mul(height as usize * scale as usize)?;
        let pairs = height.div_ceil(2);
        let end = base.checked_add(stride.checked_mul(pairs)?)?.min(VRAM_SIZE);
        let first_page = base / VramWatch::PAGE_BYTES;
        let last_page = end.saturating_sub(1) / VramWatch::PAGE_BYTES;
        let pages = (last_page - first_page + 1) as usize;
        let mut surface = Self {
            base,
            stride,
            width,
            height,
            scale,
            pixels: vec![0; hires],
            shadow: vec![0; width as usize * height as usize],
            page_cursors: vec![0; pages],
            first_page,
            last_used: 0,
            origin_x: 0,
            origin_y: 0,
        };
        // The first look mirrors everything: force every page to differ.
        let watch = mem.vram_watch();
        for (page, cursor) in (first_page..).zip(surface.page_cursors.iter_mut()) {
            *cursor = watch.generation(page).wrapping_sub(1);
        }
        surface.mirror_all(mem);
        Some(surface)
    }

    /// Holds a surface's slot while the real one is lent to the cel engine's
    /// helper thread.
    fn placeholder() -> Self {
        Self {
            base: 0,
            stride: 0,
            width: 0,
            height: 0,
            scale: 1,
            pixels: Vec::new(),
            shadow: Vec::new(),
            page_cursors: Vec::new(),
            first_page: 0,
            last_used: 0,
            origin_x: 0,
            origin_y: 0,
        }
    }

    pub fn base(&self) -> u32 {
        self.base
    }
    pub fn stride(&self) -> u32 {
        self.stride
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn scale(&self) -> u32 {
        self.scale
    }

    /// Hi-res pixels per row.
    pub fn hires_width(&self) -> u32 {
        self.width * self.scale
    }
    pub fn hires_height(&self) -> u32 {
        self.height * self.scale
    }

    /// One hi-res row, or empty past the bottom.
    pub fn row(&self, y: u32) -> &[u16] {
        let width = self.hires_width() as usize;
        let start = y as usize * width;
        self.pixels.get(start..start + width).unwrap_or(&[])
    }

    /// One hi-res row to write, or `None` past the bottom.
    pub fn row_mut(&mut self, y: u32) -> Option<&mut [u16]> {
        let width = self.hires_width() as usize;
        let start = y as usize * width;
        self.pixels.get_mut(start..start + width)
    }

    /// The hi-res pixel at (x, y), if inside.
    pub fn pixel(&self, x: u32, y: u32) -> Option<u16> {
        if x >= self.hires_width() {
            return None;
        }
        self.pixels
            .get(y as usize * self.hires_width() as usize + x as usize)
            .copied()
    }

    /// Byte offset of native pixel (x, y) from the buffer base.
    fn offset_of(&self, x: u32, y: u32) -> u32 {
        (y / 2) * self.stride + (y & 1) * 2 + x * 4
    }

    /// The native pixel an offset into VRAM is, if it is one of ours.
    fn locate(&self, offset: u32) -> Option<(u32, u32)> {
        let relative = offset.checked_sub(self.base)?;
        let pair = relative / self.stride;
        let within = relative % self.stride;
        let (x, y) = (within / 4, pair * 2 + (within >> 1 & 1));
        (x < self.width && y < self.height && within & 1 == 0).then_some((x, y))
    }

    /// Where the current destination starts inside this buffer, in native
    /// pixels: every drawing call is relative to it.
    pub fn set_origin(&mut self, x: u32, y: u32) {
        self.origin_x = x;
        self.origin_y = y;
    }

    pub fn origin(&self) -> (u32, u32) {
        (self.origin_x, self.origin_y)
    }

    /// Store a hi-res pixel of the destination. Callers pass coordinates
    /// already clipped to the destination's own hi-res size.
    #[inline]
    pub fn put(&mut self, x: u32, y: u32, value: u16) {
        let (x, y) = (
            x + self.origin_x * self.scale,
            y + self.origin_y * self.scale,
        );
        if x >= self.hires_width() {
            return;
        }
        let index = y as usize * self.hires_width() as usize + x as usize;
        if let Some(pixel) = self.pixels.get_mut(index) {
            *pixel = value;
        }
    }

    /// Fill the destination's native pixel (x, y)'s block with `value`.
    pub fn fill_block(&mut self, x: u32, y: u32, value: u16) {
        self.fill_block_raw(x + self.origin_x, y + self.origin_y, value);
    }

    /// One run of a destination row: `from` and `to` are hi-res columns.
    pub fn fill_row(&mut self, y: u32, from: u32, to: u32, value: u16) {
        let k = self.scale;
        let (shift_x, shift_y) = (self.origin_x * k, self.origin_y * k);
        let width = self.hires_width();
        let (from, to) = (from + shift_x, (to + shift_x).min(width));
        if from >= to {
            return;
        }
        let y = y + shift_y;
        let start = y as usize * width as usize;
        if let Some(run) = self
            .pixels
            .get_mut(start + from as usize..start + to as usize)
        {
            run.fill(value);
        }
    }

    /// Fill a native pixel's block in the buffer's own coordinates.
    fn fill_block_raw(&mut self, x: u32, y: u32, value: u16) {
        let k = self.scale as usize;
        let width = self.hires_width() as usize;
        if x >= self.width || y >= self.height {
            return;
        }
        let mut start = (y as usize * k) * width + x as usize * k;
        for _ in 0..k {
            if let Some(run) = self.pixels.get_mut(start..start + k) {
                run.fill(value);
            }
            start += width;
        }
    }

    fn mirror_all(&mut self, mem: &Memory) {
        let watch = mem.vram_watch();
        for (page, cursor) in (self.first_page..).zip(self.page_cursors.iter_mut()) {
            *cursor = watch.generation(page);
        }
        let vram = mem.vram();
        for y in 0..self.height {
            for x in 0..self.width {
                let value = load_be16(vram, (self.base + self.offset_of(x, y)) as usize);
                if let Some(slot) = self
                    .shadow
                    .get_mut(y as usize * self.width as usize + x as usize)
                {
                    *slot = value;
                }
                self.fill_block_raw(x, y, value);
            }
        }
    }

    /// Bring the mirror up to date with every page that changed.
    pub fn reconcile(&mut self, mem: &Memory) {
        let pages = self.page_cursors.len() as u32;
        self.reconcile_pages(mem, self.first_page, self.first_page + pages);
    }

    /// Bring the mirror up to date for the bytes `[offset, offset + length)`.
    pub fn reconcile_bytes(&mut self, mem: &Memory, offset: u32, length: u32) {
        let first = offset / VramWatch::PAGE_BYTES;
        let last = offset.saturating_add(length.saturating_sub(1)) / VramWatch::PAGE_BYTES;
        self.reconcile_pages(mem, first, last + 1);
    }

    fn reconcile_pages(&mut self, mem: &Memory, first: u32, end: u32) {
        let watch = mem.vram_watch();
        let first = first.max(self.first_page);
        for page in first..end {
            let Some(cursor) = self.page_cursors.get_mut((page - self.first_page) as usize) else {
                break;
            };
            let generation = watch.generation(page);
            if *cursor == generation {
                continue;
            }
            *cursor = generation;
            self.remirror_page(mem, page);
        }
    }

    /// Compare one page's native pixels with the shadow and re-mirror the
    /// ones that changed. Walked a row pair at a time, so the only division is
    /// per pair, not per pixel.
    fn remirror_page(&mut self, mem: &Memory, page: u32) {
        let vram = mem.vram();
        let region_end = self.base + self.stride * self.height.div_ceil(2);
        let end = ((page + 1) * VramWatch::PAGE_BYTES).min(region_end);
        let mut relative = (page * VramWatch::PAGE_BYTES).max(self.base) - self.base;
        // Pixels are halfwords from the buffer's own alignment.
        relative += relative & 1;
        let end = end.saturating_sub(self.base);
        let width = self.width as usize;
        while relative < end {
            let pair = relative / self.stride;
            let pair_start = pair * self.stride;
            let pair_end = (pair_start + self.stride).min(end);
            let mut within = relative - pair_start;
            while pair_start + within < pair_end {
                let (x, y) = (within / 4, pair * 2 + (within >> 1 & 1));
                if x < self.width && y < self.height {
                    let value = load_be16(vram, (self.base + pair_start + within) as usize);
                    let index = y as usize * width + x as usize;
                    if let Some(slot) = self.shadow.get_mut(index) {
                        if *slot != value {
                            *slot = value;
                            self.fill_block_raw(x, y, value);
                        }
                    }
                }
                within += 2;
            }
            relative = pair_end;
        }
    }

    /// Accept VRAM as it is now as what the mirror shows, without re-mirroring
    /// anything: the shadow becomes VRAM and every page counts as seen. Only
    /// for a caller that knows the mirror already holds every write since the
    /// last reconcile: the cel engine, after a list that drew only into this
    /// surface's own pixels.
    pub fn adopt_vram(&mut self, mem: &Memory) {
        let watch = mem.vram_watch();
        for (page, cursor) in (self.first_page..).zip(self.page_cursors.iter_mut()) {
            *cursor = watch.generation(page);
        }
        let vram = mem.vram();
        let width = self.width as usize;
        for (y, row) in (0u32..).zip(self.shadow.chunks_mut(width.max(1))) {
            let line = self.base + (y / 2) * self.stride + (y & 1) * 2;
            for (x, slot) in (0u32..).zip(row.iter_mut()) {
                *slot = load_be16(vram, (line + x * 4) as usize);
            }
        }
    }
}

/// Where a display line comes from in a surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineSource {
    pub surface: usize,
    /// The native row, and the native column the line starts at.
    pub y: u32,
    pub x: u32,
}

/// The surfaces, the scale they are drawn at, and which was used when.
#[derive(Clone, Debug, Default)]
pub struct HiResStore {
    /// Set while a texture pack is loaded; see [`HiResStore::set_pack_mode`].
    pack_mode: bool,
    scale: u32,
    surfaces: Vec<HiResSurface>,
    clock: u64,
}

impl HiResStore {
    pub fn new() -> Self {
        Self {
            pack_mode: false,
            scale: 1,
            surfaces: Vec::new(),
            clock: 0,
        }
    }

    /// 1 (off) to [`MAX_INTERNAL_SCALE`]. Changing it drops every surface;
    /// the next ones are rebuilt from VRAM.
    pub fn set_scale(&mut self, scale: u32) {
        let scale = clamp_scale(scale);
        if scale != self.scale {
            self.scale = scale;
            self.surfaces.clear();
        }
    }

    pub fn scale(&self) -> u32 {
        self.scale.max(1)
    }

    /// The pass runs when it has something to add: a scale above native, or
    /// texture-pack artwork to put in place of the console's own texels,
    /// which is worth doing even at native size.
    pub fn enabled(&self) -> bool {
        self.scale > 1 || self.pack_mode
    }

    /// Whether a texture pack is loaded. Surfaces are kept at native size for
    /// it, so a pack still replaces art on a machine that cannot hold a
    /// higher internal scale.
    pub fn set_pack_mode(&mut self, pack_mode: bool) {
        if pack_mode != self.pack_mode {
            self.pack_mode = pack_mode;
            if !self.enabled() {
                self.surfaces.clear();
            }
        }
    }

    pub fn pack_mode(&self) -> bool {
        self.pack_mode
    }

    pub fn clear(&mut self) {
        self.surfaces.clear();
    }

    pub fn surfaces(&self) -> &[HiResSurface] {
        &self.surfaces
    }

    pub fn surface(&self, index: usize) -> Option<&HiResSurface> {
        self.surfaces.get(index)
    }

    pub fn surface_mut(&mut self, index: usize) -> Option<&mut HiResSurface> {
        self.surfaces.get_mut(index)
    }

    /// Lend a surface out (to the thread drawing a cel list into it); its
    /// slot holds an empty stand-in until [`HiResStore::put_surface`].
    pub fn take_surface(&mut self, index: usize) -> Option<HiResSurface> {
        self.surfaces
            .get_mut(index)
            .map(|slot| std::mem::replace(slot, HiResSurface::placeholder()))
    }

    /// Return a lent surface to its slot.
    pub fn put_surface(&mut self, index: usize, surface: HiResSurface) {
        if let Some(slot) = self.surfaces.get_mut(index) {
            *slot = surface;
        }
    }

    fn touch(&mut self, index: usize) {
        self.clock += 1;
        let now = self.clock;
        if let Some(surface) = self.surfaces.get_mut(index) {
            surface.last_used = now;
        }
    }

    /// The surface cels with this destination draw into, created on first
    /// use and brought up to date with VRAM, and where the destination starts
    /// inside it. `base` is a VRAM offset, `stride` the bytes per row pair. A
    /// destination inside a buffer already tracked is a window into it, not a
    /// buffer of its own: its drawing has to land in the surface the display
    /// reads.
    pub fn target(
        &mut self,
        base: u32,
        stride: u32,
        height: u32,
        mem: &Memory,
    ) -> Option<(usize, u32, u32)> {
        if !self.enabled() {
            return None;
        }
        let found = self
            .surfaces
            .iter()
            .position(|s| s.base == base && s.stride == stride);
        let index = match found {
            Some(index) if self.surfaces.get(index).is_some_and(|s| s.height >= height) => {
                if let Some(surface) = self.surfaces.get_mut(index) {
                    surface.reconcile(mem);
                }
                index
            }
            Some(index) => {
                // Taller than before: rebuild from VRAM.
                let surface = HiResSurface::new(base, stride, height, self.scale, mem)?;
                let slot = self.surfaces.get_mut(index)?;
                *slot = surface;
                index
            }
            None => {
                // A window into a buffer already tracked draws into it.
                let window = self.surfaces.iter().enumerate().find_map(|(i, s)| {
                    let (x, y) = (s.stride == stride).then(|| s.locate(base))??;
                    (y + height <= s.height).then_some((i, x, y))
                });
                if let Some((index, x, y)) = window {
                    if let Some(surface) = self.surfaces.get_mut(index) {
                        surface.reconcile(mem);
                        surface.set_origin(x, y);
                    }
                    self.touch(index);
                    return Some((index, x, y));
                }
                let surface = HiResSurface::new(base, stride, height, self.scale, mem)?;
                if self.surfaces.len() >= MAX_SURFACES {
                    let oldest = (0..self.surfaces.len())
                        .min_by_key(|&i| self.surfaces.get(i).map_or(0, |s| s.last_used));
                    if let Some(oldest) = oldest {
                        self.surfaces.swap_remove(oldest);
                    }
                }
                self.surfaces.push(surface);
                self.surfaces.len() - 1
            }
        };
        if let Some(surface) = self.surfaces.get_mut(index) {
            surface.set_origin(0, 0);
        }
        self.touch(index);
        Some((index, 0, 0))
    }

    /// The surface holding the display line that starts at VRAM offset
    /// `offset` and runs `width` native pixels, brought up to date for that
    /// line. `None` sends the display to its native fallback.
    pub fn line_source(&mut self, offset: u32, width: u32, mem: &Memory) -> Option<LineSource> {
        if !self.enabled() {
            return None;
        }
        let (index, (x, y)) = self.surfaces.iter().enumerate().find_map(|(i, s)| {
            let (x, y) = s.locate(offset)?;
            (x + width <= s.width).then_some((i, (x, y)))
        })?;
        let surface = self.surfaces.get_mut(index)?;
        let pair_start = surface.base + (y / 2) * surface.stride;
        surface.reconcile_bytes(mem, pair_start, surface.stride);
        self.touch(index);
        Some(LineSource {
            surface: index,
            y,
            x,
        })
    }
}
