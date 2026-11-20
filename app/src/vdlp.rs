//! The VDLP, the Video Display List Processor.
//!
//! The 3DO has no fixed framebuffer register. The picture is described by a
//! linked list in memory: each VDL entry names the framebuffer to show, how many
//! scanlines it holds for, optional colour-table and display-control words, and
//! the next entry. Walking that list a line at a time is how a field is drawn,
//! and how titles change palette or buffer partway down the screen.
//!
//! # Specification basis
//!
//! The 3DO Graphics Programming Guide ("Creating a Custom VDL") and US patent
//! 5,502,462 give the same per-line order, which [`Vdlp::process_scanline`]
//! follows:
//!
//! 1. in horizontal blank, fetch an entry if the current one has run out;
//! 2. display the line with the resulting state;
//! 3. tick the bitmap pointer on to the next line.
//!
//! The list starts running at [`Vdlp::LIST_START_LINE`], well before the first
//! visible line, and the framebuffer pointer advances on every line it runs for,
//! blanking included. Each visible line is captured into a logical field at the
//! moment the beam reaches it, so a later CPU, SPORT or cel write cannot change
//! a line already shown. The field boundary only expands that logical field to
//! the display generator's 2x output ([`Vdlp::finish_field_2x`]).
//!
//! One counter boundary is a measured compatibility rule rather than a
//! documented one: a raw persist count of zero on the first entry of a field
//! lets the following entry replace it before any line is shown. Game traces
//! establish it; see the project's PROVENANCE notes.
//!
//! Malformed lists terminate: the walk fetches at most two entries per line,
//! stops at a self-reference or a null link, and gives up after
//! [`Vdlp::MAX_ENTRIES`] entries in one field.
#![forbid(unsafe_code)]

use crate::hires::{clamp_scale, HiResStore};
use crate::memory::{Memory, VRAM_SIZE};
use crate::video::{expand_rgb555, framebuffer_offset, PIXEL_LITERAL};

/// What every pixel shows when nothing has been drawn.
pub const OPAQUE_BLACK: u32 = 0xff00_0000;

/// Bits of an entry's first word, the control word.
pub mod control {
    /// Lines the entry holds for before the next entry may be fetched.
    pub const PERSIST_MASK: u32 = 0x0000_01ff;
    /// How many optional control words follow the four-word header.
    pub const WORD_COUNT_SHIFT: u32 = 9;
    pub const WORD_COUNT_MASK: u32 = 0x3f;
    pub const PREV_OVERRIDE: u32 = 1 << 15;
    /// Take the header's current-buffer word as the new framebuffer address.
    pub const CURR_OVERRIDE: u32 = 1 << 16;
    pub const PREV_TICK: u32 = 1 << 17;
    /// The next-entry word is relative to the end of the header.
    pub const NEXT_RELATIVE: u32 = 1 << 18;
    pub const VERTICAL_MODE: u32 = 1 << 19;
    /// Video DMA: without it the entry's lines are black.
    pub const ENABLE_DMA: u32 = 1 << 21;
    /// Index into [`super::LINE_MODULO`].
    pub const MODULO_SHIFT: u32 = 23;
    pub const MODULO_MASK: u32 = 0x7;
}

/// The top three bits of an optional control word say what kind it is.
pub mod word {
    pub const SELECTOR_SHIFT: u32 = 29;
    /// Selectors 0..=3: a colour-table write (the low two bits pick channels).
    pub const COLOUR: u32 = 0;
    /// Selectors 4 and 5: for the audio/video output stage, not the display.
    pub const AV_CONTROL: u32 = 4;
    pub const DISPLAY: u32 = 6;
    pub const BACKGROUND: u32 = 7;
}

/// Fields of a colour-table word.
pub mod colour {
    pub const BLUE_SHIFT: u32 = 0;
    pub const GREEN_SHIFT: u32 = 8;
    pub const RED_SHIFT: u32 = 16;
    pub const ADDR_SHIFT: u32 = 24;
    pub const ADDR_MASK: u32 = 0x1f;
    pub const ENABLE_SHIFT: u32 = 29;
    pub const ENABLE_MASK: u32 = 0x3;
}

/// Fields of a display-control word.
pub mod display {
    pub const DISABLE_VERTICAL_ONCE: u32 = 1 << 0;
    pub const COLOURS_ONLY: u32 = 1 << 1;
    pub const HORIZONTAL_INTERP: u32 = 1 << 2;
    pub const VERTICAL_INTERP: u32 = 1 << 3;
    pub const CLUT_BYPASS: u32 = 1 << 25;
}

/// Pixels between framebuffer lines, selected by the control word's modulo.
pub const LINE_MODULO: [u32; 8] = [320, 384, 512, 640, 1024, 320, 320, 320];

/// Slots in each channel of the output colour table.
pub const CLUT_ENTRIES: usize = 32;

/// Bytes in an entry's fixed header: control, current buffer, previous buffer,
/// next entry. Optional control words follow it.
const HEADER_BYTES: u32 = 16;

/// The VRAM address lines the VDLP has. Higher framebuffer address bits wrap.
const VRAM_ADDRESS_MASK: u32 = VRAM_SIZE - 1;

// ---------------------------------------------------------------------------
// Decoded list structures
// ---------------------------------------------------------------------------

/// An entry's control word, decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryControl {
    pub raw: u32,
    pub persist_lines: u32,
    pub word_count: u32,
    pub prev_override: bool,
    pub curr_override: bool,
    pub prev_tick: bool,
    pub next_relative: bool,
    pub vertical_mode: bool,
    pub enable_dma: bool,
    pub modulo_index: u32,
}

impl EntryControl {
    pub const fn decode(raw: u32) -> Self {
        Self {
            raw,
            persist_lines: raw & control::PERSIST_MASK,
            word_count: (raw >> control::WORD_COUNT_SHIFT) & control::WORD_COUNT_MASK,
            prev_override: raw & control::PREV_OVERRIDE != 0,
            curr_override: raw & control::CURR_OVERRIDE != 0,
            prev_tick: raw & control::PREV_TICK != 0,
            next_relative: raw & control::NEXT_RELATIVE != 0,
            vertical_mode: raw & control::VERTICAL_MODE != 0,
            enable_dma: raw & control::ENABLE_DMA != 0,
            modulo_index: (raw >> control::MODULO_SHIFT) & control::MODULO_MASK,
        }
    }

    /// Pixels per framebuffer line under this entry.
    pub fn line_modulo(&self) -> u32 {
        LINE_MODULO
            .get(self.modulo_index as usize)
            .copied()
            .unwrap_or(320)
    }
}

/// One VDL entry's four-word header, read from memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VdlEntry {
    pub address: u32,
    pub control: EntryControl,
    pub current_buffer: u32,
    pub previous_buffer: u32,
    pub next_word: u32,
}

impl VdlEntry {
    /// Read the entry at `address`. A control word of zero is the end of the
    /// list, not an entry.
    pub fn read(mem: &Memory, address: u32) -> Option<Self> {
        let at = |index: u32| mem.read32(address.wrapping_add(index.wrapping_mul(4)));
        let raw = at(0);
        if raw == 0 {
            return None;
        }
        Some(Self {
            address,
            control: EntryControl::decode(raw),
            current_buffer: at(1),
            previous_buffer: at(2),
            next_word: at(3),
        })
    }

    /// Where the optional control words start.
    pub fn words_address(&self) -> u32 {
        self.address.wrapping_add(HEADER_BYTES)
    }

    /// The optional control words, decoded in list order.
    pub fn control_words<'m>(&self, mem: &'m Memory) -> impl Iterator<Item = ControlWord> + 'm {
        let base = self.words_address();
        (0..self.control.word_count)
            .map(move |i| ControlWord::decode(mem.read32(base.wrapping_add(i.wrapping_mul(4)))))
    }

    /// The next entry, or `None` when the link ends the list (null, or the
    /// entry pointing at itself).
    pub fn next_entry(&self) -> Option<u32> {
        let next = if self.control.next_relative {
            self.next_word.wrapping_add(self.words_address())
        } else {
            self.next_word
        };
        (next != 0 && next != self.address).then_some(next)
    }
}

/// Which channels a colour-table word writes. An enable field of zero means
/// all three, which is not what the numbering suggests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channels {
    All,
    Blue,
    Green,
    Red,
}

/// A write into the output colour table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColourWrite {
    pub slot: u32,
    pub channels: Channels,
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

/// A display-control word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayControl {
    pub disable_vertical_once: bool,
    /// Later display words in the same entry are ignored.
    pub colours_only: bool,
    pub horizontal_interp: bool,
    pub vertical_interp: bool,
    /// Per-pixel escape: with it on, a pixel with bit 15 set is a literal
    /// colour; the rest still go through the table.
    pub clut_bypass: bool,
}

/// An optional control word after an entry's header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlWord {
    Colour(ColourWrite),
    /// For the audio/video output stage; the display ignores it.
    AvControl(u32),
    Display(DisplayControl),
    /// The colour a pixel of zero shows (low 24 bits).
    Background(u32),
}

impl ControlWord {
    pub const fn decode(raw: u32) -> Self {
        match raw >> word::SELECTOR_SHIFT {
            0..=3 => {
                let channels = match (raw >> colour::ENABLE_SHIFT) & colour::ENABLE_MASK {
                    0 => Channels::All,
                    1 => Channels::Blue,
                    2 => Channels::Green,
                    _ => Channels::Red,
                };
                ControlWord::Colour(ColourWrite {
                    slot: (raw >> colour::ADDR_SHIFT) & colour::ADDR_MASK,
                    channels,
                    red: (raw >> colour::RED_SHIFT) as u8,
                    green: (raw >> colour::GREEN_SHIFT) as u8,
                    blue: (raw >> colour::BLUE_SHIFT) as u8,
                })
            }
            4 | 5 => ControlWord::AvControl(raw),
            6 => ControlWord::Display(DisplayControl {
                disable_vertical_once: raw & display::DISABLE_VERTICAL_ONCE != 0,
                colours_only: raw & display::COLOURS_ONLY != 0,
                horizontal_interp: raw & display::HORIZONTAL_INTERP != 0,
                vertical_interp: raw & display::VERTICAL_INTERP != 0,
                clut_bypass: raw & display::CLUT_BYPASS != 0,
            }),
            _ => ControlWord::Background(raw),
        }
    }
}

// ---------------------------------------------------------------------------
// Output stage state
// ---------------------------------------------------------------------------

/// The output colour table. Not an indexed palette: each five-bit channel is
/// looked up separately to eight bits, so it is three per-channel ramps. Titles
/// reprogram it to fade, tint or flash without touching a framebuffer pixel.
#[derive(Clone, Debug, PartialEq, Eq)]
struct OutputClut {
    red: [u8; CLUT_ENTRIES],
    green: [u8; CLUT_ENTRIES],
    blue: [u8; CLUT_ENTRIES],
}

impl OutputClut {
    /// The table software finds at power-on: a straight five-to-eight-bit ramp.
    fn ramp() -> Self {
        let mut ramp = [0u8; CLUT_ENTRIES];
        for (i, slot) in (0u32..).zip(ramp.iter_mut()) {
            *slot = ((i * 255 + 15) / 31) as u8;
        }
        Self {
            red: ramp,
            green: ramp,
            blue: ramp,
        }
    }

    fn lookup(table: &[u8; CLUT_ENTRIES], five: u32) -> u8 {
        table.get((five & 0x1f) as usize).copied().unwrap_or(0)
    }

    fn set(table: &mut [u8; CLUT_ENTRIES], slot: u32, value: u8) {
        if let Some(entry) = table.get_mut((slot & 0x1f) as usize) {
            *entry = value;
        }
    }

    fn apply(&mut self, write: ColourWrite) {
        match write.channels {
            Channels::All => {
                Self::set(&mut self.red, write.slot, write.red);
                Self::set(&mut self.green, write.slot, write.green);
                Self::set(&mut self.blue, write.slot, write.blue);
            }
            Channels::Blue => Self::set(&mut self.blue, write.slot, write.blue),
            Channels::Green => Self::set(&mut self.green, write.slot, write.green),
            Channels::Red => Self::set(&mut self.red, write.slot, write.red),
        }
    }
}

/// Colour table and display control. Programmed by the list and kept from
/// field to field; only a reset puts it back.
#[derive(Clone, Debug)]
struct OutputStage {
    clut: OutputClut,
    background: u32,
    clut_bypass: bool,
    horizontal_interp: bool,
    vertical_interp: bool,
    disable_vertical_once: bool,
}

impl OutputStage {
    fn power_on() -> Self {
        Self {
            clut: OutputClut::ramp(),
            background: 0,
            clut_bypass: false,
            horizontal_interp: false,
            vertical_interp: false,
            disable_vertical_once: false,
        }
    }

    /// Apply an entry's control words. Once a display word has asked for
    /// "colours only", later display words in the same entry are skipped.
    fn apply_words(&mut self, words: impl Iterator<Item = ControlWord>) {
        let mut colours_only = false;
        for control_word in words {
            match control_word {
                ControlWord::Colour(write) => self.clut.apply(write),
                ControlWord::AvControl(_) => {}
                ControlWord::Display(_) if colours_only => {}
                ControlWord::Display(d) => {
                    self.clut_bypass = d.clut_bypass;
                    self.horizontal_interp = d.horizontal_interp;
                    self.vertical_interp = d.vertical_interp;
                    self.disable_vertical_once = d.disable_vertical_once;
                    colours_only = d.colours_only;
                }
                ControlWord::Background(raw) => self.background = raw,
            }
        }
    }

    /// One framebuffer pixel to one output colour.
    ///
    /// Zero is not a colour: it shows the background. With CLUT bypass on, bit
    /// 15 decides per pixel whether the other fifteen bits are already a
    /// colour; pixels without it still go through the table.
    fn shade(&self, pixel: u16) -> u32 {
        if pixel == 0 {
            return OPAQUE_BLACK | (self.background & 0x00ff_ffff);
        }
        if self.clut_bypass && pixel & PIXEL_LITERAL != 0 {
            return expand_rgb555(pixel);
        }
        let p = u32::from(pixel);
        let r = OutputClut::lookup(&self.clut.red, p >> 10);
        let g = OutputClut::lookup(&self.clut.green, p >> 5);
        let b = OutputClut::lookup(&self.clut.blue, p);
        OPAQUE_BLACK | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
    }

    /// The interpolation the display generator applies to the line now shown.
    fn line_mode(&self) -> LineMode {
        LineMode {
            horizontal: self.horizontal_interp,
            vertical: self.vertical_interp && !self.disable_vertical_once,
        }
    }
}

/// Interpolation for one logical line, latched when it was displayed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct LineMode {
    horizontal: bool,
    vertical: bool,
}

/// The list walk's registers for one field.
#[derive(Clone, Copy, Debug)]
struct ListWalk {
    /// The entry the next fetch reads.
    next_entry: u32,
    /// The framebuffer address of the line about to be shown, once an entry
    /// has supplied one.
    buffer: Option<u32>,
    modulo_index: u32,
    dma_enabled: bool,
    ended: bool,
    /// Lines left on the current entry. An entry is fetched when this is
    /// exactly zero. It is decremented after every line, so an exhausted list
    /// drives it negative and no further fetch is attempted.
    lines_left: i32,
    entries_walked: u32,
}

impl ListWalk {
    fn idle() -> Self {
        Self::starting_at(0)
    }

    fn starting_at(list_address: u32) -> Self {
        Self {
            next_entry: list_address,
            buffer: None,
            modulo_index: 0,
            dma_enabled: false,
            ended: list_address == 0,
            lines_left: 0,
            entries_walked: 0,
        }
    }

    /// Step a framebuffer address to the next line. Lines are interleaved in
    /// pairs within words: from an even line the step is two bytes, from an odd
    /// line it is the rest of the pair.
    fn advance(&self, address: u32) -> u32 {
        let modulo = LINE_MODULO
            .get((self.modulo_index & 7) as usize)
            .copied()
            .unwrap_or(320);
        let step = if address & 2 != 0 {
            (modulo << 2).wrapping_sub(2)
        } else {
            2
        };
        address.wrapping_add(step)
    }
}

/// The logical field the beam has drawn so far.
#[derive(Clone, Debug, Default)]
struct LogicalField {
    width: u32,
    height: u32,
    active: bool,
    pixels: Vec<u32>,
    modes: Vec<LineMode>,
}

impl LogicalField {
    fn row_mut(&mut self, line: u32) -> Option<&mut [u32]> {
        let width = self.width as usize;
        let start = (line as usize).checked_mul(width)?;
        self.pixels.get_mut(start..start.checked_add(width)?)
    }

    fn row(&self, line: u32) -> &[u32] {
        let width = self.width as usize;
        (line as usize)
            .checked_mul(width)
            .and_then(|start| self.pixels.get(start..start.checked_add(width)?))
            .unwrap_or(&[])
    }
}

/// The same field at the internal scale: `(width * k) * (height * k)` pixels,
/// `k` rows per logical line. Only kept while the scale is above one.
#[derive(Clone, Debug, Default)]
struct HiResField {
    pixels: Vec<u32>,
    /// Which logical lines this field has captured.
    captured: Vec<bool>,
}

// ---------------------------------------------------------------------------
// The processor
// ---------------------------------------------------------------------------

/// The Video Display List Processor.
#[derive(Clone, Debug)]
pub struct Vdlp {
    list_address: u32,
    first_visible_line: u32,
    total_lines: u32,
    output: OutputStage,
    walk: ListWalk,
    field: LogicalField,
    hires_scale: u32,
    hires: HiResField,
    /// A texture pack is loaded, so fields come from the surfaces whatever
    /// the internal scale.
    hires_pack_mode: bool,
}

impl Default for Vdlp {
    fn default() -> Self {
        Self::new()
    }
}

impl Vdlp {
    /// The line the display list starts running on (in the vertical blank).
    pub const LIST_START_LINE: u32 = 5;

    /// A display list that never terminates must not hang the machine.
    pub const MAX_ENTRIES: u32 = 1024;

    pub fn new() -> Self {
        Self {
            list_address: 0,
            first_visible_line: Self::LIST_START_LINE,
            total_lines: 0,
            output: OutputStage::power_on(),
            walk: ListWalk::idle(),
            field: LogicalField::default(),
            hires_scale: 1,
            hires: HiResField::default(),
            hires_pack_mode: false,
        }
    }

    /// Back to power-on: no list, the ramp colour table, no field in progress.
    /// The field shape is front-end configuration and is kept.
    pub fn reset(&mut self) {
        self.list_address = 0;
        self.output = OutputStage::power_on();
        self.walk = ListWalk::idle();
        self.field.width = 0;
        self.field.height = 0;
        self.field.active = false;
    }

    /// Where the visible part of a field sits: the first visible line, and the
    /// line the list stops running at (zero: `first_visible + height`).
    ///
    /// The default is no blanking at all, so the first line the list runs for
    /// is the first one shown; the console sets the real region shape.
    pub fn set_field_shape(&mut self, first_visible: u32, last_line: u32) {
        self.first_visible_line = first_visible;
        self.total_lines = last_line;
    }

    /// Where the display list starts. Latched by [`Vdlp::begin_field`].
    pub fn set_list_address(&mut self, address: u32) {
        self.list_address = address;
    }

    pub fn list_address(&self) -> u32 {
        self.list_address
    }

    /// Which framebuffer line reaches the top of the screen, given that the
    /// list runs (and the address advances) from `LIST_START_LINE` onwards.
    pub fn buffer_start_line(&self) -> u32 {
        self.first_visible_line
            .saturating_sub(Self::LIST_START_LINE)
    }

    pub fn clut_red(&self, slot: u32) -> u8 {
        OutputClut::lookup(&self.output.clut.red, slot)
    }
    pub fn clut_green(&self, slot: u32) -> u8 {
        OutputClut::lookup(&self.output.clut.green, slot)
    }
    pub fn clut_blue(&self, slot: u32) -> u8 {
        OutputClut::lookup(&self.output.clut.blue, slot)
    }

    /// The background colour register as the list last wrote it.
    pub fn background(&self) -> u32 {
        self.output.background
    }

    /// VDL entries taken this field. Zero means the list was empty or did not
    /// look like a list: the symptom to look for when a title shows nothing.
    pub fn entries_walked(&self) -> u32 {
        self.walk.entries_walked
    }

    /// The logical field captured so far, `width * height` XRGB8888 pixels.
    pub fn logical_frame(&self) -> &[u32] {
        &self.field.pixels
    }

    /// The last line (exclusive) the list runs for this field.
    fn last_line(&self) -> u32 {
        if self.total_lines != 0 {
            self.total_lines
        } else {
            self.first_visible_line.wrapping_add(self.field.height)
        }
    }

    /// Latch the list root at the field boundary and clear the logical field
    /// to black. The colour table and display control carry over.
    pub fn begin_field(&mut self, width: u32, height: u32) {
        self.walk = ListWalk::starting_at(self.list_address);
        let pixels = (width as usize).checked_mul(height as usize);
        let hires_active = self.hires_active();
        let field = &mut self.field;
        field.width = width;
        field.height = height;
        field.active = width > 0 && height > 0 && pixels.is_some();
        field.pixels.clear();
        field.pixels.resize(pixels.unwrap_or(0), OPAQUE_BLACK);
        field.modes.clear();
        field.modes.resize(height as usize, LineMode::default());

        let hires = &mut self.hires;
        hires.captured.clear();
        if hires_active && field.active {
            let k = self.hires_scale as usize;
            let pixels = (width as usize * k).saturating_mul(height as usize * k);
            // Every line is rewritten as it is captured; reuse the buffer.
            hires.pixels.resize(pixels, OPAQUE_BLACK);
            hires.captured.resize(height as usize, false);
        } else {
            hires.pixels = Vec::new();
        }
    }

    // --- internal resolution ---------------------------------------------

    /// Also build each field at `scale` times its resolution (1 to
    /// [`crate::hires::MAX_INTERNAL_SCALE`]), sampling MADAM's hi-res
    /// surfaces where a line shows one. Takes effect at the next field.
    pub fn set_internal_scale(&mut self, scale: u32) {
        self.hires_scale = clamp_scale(scale);
    }

    /// Build fields from MADAM's surfaces even at native scale, because a
    /// texture pack puts its own art in them.
    pub fn set_pack_mode(&mut self, pack_mode: bool) {
        self.hires_pack_mode = pack_mode;
    }

    /// Whether fields come from the surfaces at all.
    fn hires_active(&self) -> bool {
        self.hires_scale > 1 || self.hires_pack_mode
    }

    pub fn internal_scale(&self) -> u32 {
        self.hires_scale
    }

    /// Fetch the next entry, if the list has one, and apply it.
    fn take_entry(&mut self, mem: &Memory) {
        let walk = &mut self.walk;
        if walk.ended || walk.entries_walked >= Self::MAX_ENTRIES {
            return;
        }
        let Some(entry) = VdlEntry::read(mem, walk.next_entry) else {
            walk.ended = true;
            return;
        };
        walk.entries_walked = walk.entries_walked.wrapping_add(1);

        let c = entry.control;
        if c.curr_override {
            walk.buffer = Some(entry.current_buffer);
        }
        walk.modulo_index = c.modulo_index;
        walk.dma_enabled = c.enable_dma;
        walk.lines_left = c.persist_lines as i32;

        self.output.apply_words(entry.control_words(mem));

        match entry.next_entry() {
            Some(next) => self.walk.next_entry = next,
            None => self.walk.ended = true,
        }
    }

    /// Run one scanline of the field: fetch in horizontal blank, capture the
    /// line if it is visible, then tick the bitmap pointer.
    pub fn process_scanline(&mut self, field_line: u32, mem: &Memory) {
        self.process_scanline_with(field_line, mem, None);
    }

    /// [`Vdlp::process_scanline`], with MADAM's hi-res surfaces to sample
    /// when the internal scale is above one.
    pub fn process_scanline_with(
        &mut self,
        field_line: u32,
        mem: &Memory,
        hires: Option<&mut HiResStore>,
    ) {
        if !self.field.active || field_line < Self::LIST_START_LINE {
            return;
        }
        if field_line >= self.last_line() {
            return;
        }

        if field_line == Self::LIST_START_LINE {
            self.take_entry(mem);
        }
        if self.walk.lines_left == 0 {
            self.take_entry(mem);
        }

        if let Some(output_line) = field_line.checked_sub(self.first_visible_line) {
            if output_line < self.field.height {
                self.capture_line(output_line, mem);
                if self.hires_active() {
                    self.capture_hires_line(output_line, mem, hires);
                }
            }
        }

        if let Some(buffer) = self.walk.buffer {
            self.walk.buffer = Some(self.walk.advance(buffer));
        }
        self.output.disable_vertical_once = false;
        self.walk.lines_left = self.walk.lines_left.wrapping_sub(1);
    }

    /// Sample one visible line from VRAM into the logical field.
    fn capture_line(&mut self, output_line: u32, mem: &Memory) {
        if let Some(mode) = self.field.modes.get_mut(output_line as usize) {
            *mode = self.output.line_mode();
        }
        let source = if self.walk.dma_enabled {
            self.walk.buffer
        } else {
            None
        };
        let width = self.field.width;
        let output = &self.output;
        let Some(row) = self.field.row_mut(output_line) else {
            return;
        };
        match source {
            None => row.fill(OPAQUE_BLACK),
            Some(address) => {
                // Only the fitted VRAM address lines exist; high bits wrap.
                let base = address & VRAM_ADDRESS_MASK;
                let vram = mem.vram();
                let byte = |at: u32| vram.get(at as usize).copied().unwrap_or(0);
                for (x, pixel) in (0u32..).zip(row.iter_mut()) {
                    let at = base.wrapping_add(framebuffer_offset(x, 0, width)) & VRAM_ADDRESS_MASK;
                    let next = at.wrapping_add(1) & VRAM_ADDRESS_MASK;
                    let value = u16::from_be_bytes([byte(at), byte(next)]);
                    *pixel = output.shade(value);
                }
            }
        }
    }

    /// The visible line at the internal scale: `k` rows sampled from the
    /// surface that holds the line, or the native line just captured,
    /// repeated, when none does.
    fn capture_hires_line(
        &mut self,
        output_line: u32,
        mem: &Memory,
        store: Option<&mut HiResStore>,
    ) {
        let k = self.hires_scale;
        let width = self.field.width;
        let row_width = (width * k) as usize;
        let start = (output_line * k) as usize * row_width;
        let Some(rows) = self
            .hires
            .pixels
            .get_mut(start..start + row_width * k as usize)
        else {
            return;
        };
        if let Some(flag) = self.hires.captured.get_mut(output_line as usize) {
            *flag = true;
        }
        let source = if self.walk.dma_enabled {
            self.walk.buffer
        } else {
            None
        };
        let found = source.zip(store).and_then(|(address, store)| {
            let line = store.line_source(address & VRAM_ADDRESS_MASK, width, mem)?;
            let surface = store.surface(line.surface)?;
            (surface.scale() == k).then_some((surface, line))
        });
        let output = &self.output;
        match found {
            Some((surface, line)) => {
                let skip = (line.x * k) as usize;
                // The surface is the framebuffer's own picture with the
                // field's cels drawn over it, so where the display list
                // drew nothing it still holds the previous field's pixels.
                // The logical row says what was actually drawn: black there
                // means nothing was, and the mirror must not show through —
                // that is what leaves a trail behind a moving element.
                let native = self.field.row(output_line);
                // Runs of one value are common; shade each value once.
                let mut last: Option<(u16, u32)> = None;
                for (sub, row) in (0u32..).zip(rows.chunks_mut(row_width)) {
                    let from = surface.row(line.y * k + sub).iter().skip(skip);
                    for (index, (pixel, &value)) in row.iter_mut().zip(from).enumerate() {
                        let native_x = line.x + (index as u32) / k;
                        if native.get(native_x as usize) == Some(&OPAQUE_BLACK) {
                            *pixel = OPAQUE_BLACK;
                            continue;
                        }
                        *pixel = match last {
                            Some((seen, shaded)) if seen == value => shaded,
                            _ => {
                                let shaded = output.shade(value);
                                last = Some((value, shaded));
                                shaded
                            }
                        };
                    }
                }
            }
            None => {
                let native = self.field.row(output_line);
                for row in rows.chunks_mut(row_width) {
                    for (cell, &pixel) in row.chunks_mut(k as usize).zip(native) {
                        cell.fill(pixel);
                    }
                }
            }
        }
    }

    /// The output frame's size at the current internal scale for a
    /// `width` x `height` logical field: twice the hi-res field both ways.
    pub fn output_size(&self, width: u32, height: u32) -> (u32, u32) {
        (width * 2 * self.hires_scale, height * 2 * self.hires_scale)
    }

    /// Expand the hi-res field to the 2x output, exactly as
    /// [`Vdlp::finish_field_2x`] expands the logical one: `out` should hold
    /// `(2 * width * k) * (2 * height * k)` pixels. Each hi-res row takes the
    /// interpolation latched for its logical line; a line the field never
    /// reached is black. Does nothing at a scale of one.
    pub fn finish_field_hires(&self, out: &mut [u32], width: u32, height: u32) {
        let field = &self.field;
        let k = self.hires_scale;
        if k == 0 || width == 0 || height == 0 || width != field.width || height != field.height {
            return;
        }
        let row_width = (width * k) as usize;
        let rows = height * k;
        let hires_row = |y: u32| {
            let start = y as usize * row_width;
            self.hires
                .pixels
                .get(start..start + row_width)
                .unwrap_or(&[])
        };
        let captured = |line: u32| {
            self.hires
                .captured
                .get(line as usize)
                .copied()
                .unwrap_or(false)
        };
        let output_width = row_width * 2;
        let mut pairs = out.chunks_mut(output_width * 2);
        for y in 0..rows {
            let Some(pair) = pairs.next() else {
                return;
            };
            let line = y / k;
            if !captured(line) {
                pair.fill(OPAQUE_BLACK);
                continue;
            }
            let below_y = if y + 1 < rows && captured((y + 1) / k) {
                y + 1
            } else {
                y
            };
            let mode = field.modes.get(line as usize).copied().unwrap_or_default();
            let (upper, lower) = pair.split_at_mut(output_width.min(pair.len()));
            expand_line_fast(hires_row(y), hires_row(below_y), mode, upper, lower);
        }
    }

    /// Expand the captured logical field to the display generator's 2x
    /// output. `out` should hold `(2 * width) * (2 * height)` pixels; a shorter
    /// slice is filled as far as it goes. Nothing happens unless `width` and
    /// `height` match the field begun with [`Vdlp::begin_field`].
    ///
    /// With interpolation off for a line its 2x2 block is four copies (the
    /// hardware's "crispy pixels"). With it on, the upper-left subpixel keeps
    /// the parent colour and the other three average towards the right and
    /// lower neighbours, which clamp to the pixel itself at the edges.
    pub fn finish_field_2x(&self, out: &mut [u32], width: u32, height: u32) {
        let field = &self.field;
        if width == 0 || height == 0 || width != field.width || height != field.height {
            return;
        }
        let output_width = (width as usize).saturating_mul(2);
        let mut pairs = out.chunks_mut(output_width.saturating_mul(2));
        for y in 0..height {
            let Some(pair) = pairs.next() else {
                return;
            };
            let (upper, lower) = pair.split_at_mut(output_width.min(pair.len()));
            let below_y = if y + 1 < height { y + 1 } else { y };
            let mode = field.modes.get(y as usize).copied().unwrap_or_default();
            expand_line(field.row(y), field.row(below_y), mode, upper, lower);
        }
    }

    /// Run a whole field at once into `out` (`width * height` pixels): a
    /// convenience for tests and diagnostics, equivalent to the console's
    /// begin / per-line / capture sequence.
    pub fn render_field(&mut self, out: &mut [u32], width: u32, height: u32, mem: &Memory) {
        if width == 0 || height == 0 {
            return;
        }
        self.run_field(width, height, mem);
        for (dst, src) in out.iter_mut().zip(&self.field.pixels) {
            *dst = *src;
        }
    }

    /// Run a whole field and expand it to 2x into `out`
    /// (`(2 * width) * (2 * height)` pixels).
    pub fn render_field_2x(&mut self, out: &mut [u32], width: u32, height: u32, mem: &Memory) {
        if width == 0 || height == 0 {
            return;
        }
        self.run_field(width, height, mem);
        self.finish_field_2x(out, width, height);
    }

    fn run_field(&mut self, width: u32, height: u32, mem: &Memory) {
        self.begin_field(width, height);
        for line in Self::LIST_START_LINE..self.last_line() {
            self.process_scanline(line, mem);
        }
    }

    /// Diagnostic only: read VRAM from `vram_offset` as a plain linear RGB555
    /// image, ignoring the display list, the interleave and the colour table.
    /// Pixels past the end of VRAM are black.
    pub fn render_linear(
        &self,
        out: &mut [u32],
        width: u32,
        height: u32,
        vram_offset: u32,
        mem: &Memory,
    ) {
        let vram = mem.vram();
        let pixels = (width as usize).saturating_mul(height as usize);
        let mut offset = vram_offset;
        for pixel in out.iter_mut().take(pixels) {
            let in_range = offset.checked_add(1).is_some_and(|last| last < VRAM_SIZE);
            if !in_range {
                *pixel = OPAQUE_BLACK;
                continue;
            }
            let byte = |at: u32| vram.get(at as usize).copied().unwrap_or(0);
            *pixel = expand_rgb555(u16::from_be_bytes([
                byte(offset),
                byte(offset.wrapping_add(1)),
            ]));
            offset = offset.wrapping_add(2);
        }
    }
}

// ---------------------------------------------------------------------------
// The display generator's 2x expansion
// ---------------------------------------------------------------------------

/// Average two colours channel by channel; a carry out of blue cannot reach
/// green. The generator works on 24-bit colours after the table.
const fn average2(a: u32, b: u32) -> u32 {
    const RED_BLUE: u32 = 0x00ff_00ff;
    const GREEN: u32 = 0x0000_ff00;
    let rb = (((a & RED_BLUE) + (b & RED_BLUE)) >> 1) & RED_BLUE;
    let g = (((a & GREEN) + (b & GREEN)) >> 1) & GREEN;
    OPAQUE_BLACK | rb | g
}

/// Average four colours. Red and blue sit far enough apart to sum four
/// eight-bit channels in parallel without carrying into one another.
const fn average4(a: u32, b: u32, c: u32, d: u32) -> u32 {
    const RED_BLUE: u32 = 0x00ff_00ff;
    const GREEN: u32 = 0x0000_ff00;
    let rb = (((a & RED_BLUE) + (b & RED_BLUE) + (c & RED_BLUE) + (d & RED_BLUE)) >> 2) & RED_BLUE;
    let g = (((a & GREEN) + (b & GREEN) + (c & GREEN) + (d & GREEN)) >> 2) & GREEN;
    OPAQUE_BLACK | rb | g
}

/// The 2x2 block for one logical pixel: `[upper-left, upper-right]` and
/// `[lower-left, lower-right]`. `right` is `None` at the right edge, where the
/// horizontal neighbour clamps; `below` is the pixel itself on the last line.
fn expand_pixel(
    here: u32,
    right: Option<u32>,
    below: u32,
    diagonal: u32,
    mode: LineMode,
) -> ([u32; 2], [u32; 2]) {
    let right = if mode.horizontal { right } else { None };
    let upper_right = right.map_or(here, |r| average2(here, r));
    if !mode.vertical {
        return ([here, upper_right], [here, upper_right]);
    }
    let lower_left = average2(here, below);
    let lower_right = right.map_or(lower_left, |r| average4(here, r, below, diagonal));
    ([here, upper_right], [lower_left, lower_right])
}

/// [`expand_line`] with the mode decided once per line instead of per
/// pixel: the same output, for the hi-res field, where a 4x picture is sixteen
/// times the pixels. Rows must be the same length; output is written as far
/// as both output lines reach.
fn expand_line_fast(
    row: &[u32],
    below_row: &[u32],
    mode: LineMode,
    upper: &mut [u32],
    lower: &mut [u32],
) {
    let count = row
        .len()
        .min(below_row.len())
        .min(upper.len() / 2)
        .min(lower.len() / 2);
    let (Some(row), Some(below_row)) = (row.get(..count), below_row.get(..count)) else {
        return;
    };
    let (Some(upper), Some(lower)) = (upper.get_mut(..count * 2), lower.get_mut(..count * 2))
    else {
        return;
    };
    let (Some(&last), Some(&last_below)) = (row.last(), below_row.last()) else {
        return;
    };
    let pairs = row.windows(2).zip(below_row.windows(2));
    let cells = upper
        .as_chunks_mut::<2>()
        .0
        .iter_mut()
        .zip(lower.as_chunks_mut::<2>().0.iter_mut());
    match (mode.horizontal, mode.vertical) {
        (false, false) => {
            for (cell, &here) in upper.as_chunks_mut::<2>().0.iter_mut().zip(row) {
                *cell = [here, here];
            }
            lower.copy_from_slice(upper);
            return;
        }
        (true, false) => {
            for ((top, bottom), (here, _)) in cells.zip(pairs) {
                if let &[a, b] = here {
                    let right = average2(a, b);
                    *top = [a, right];
                    *bottom = [a, right];
                }
            }
        }
        (false, true) => {
            for ((top, bottom), (&here, &below)) in cells.zip(row.iter().zip(below_row)) {
                let lower_left = average2(here, below);
                *top = [here, here];
                *bottom = [lower_left, lower_left];
            }
            return;
        }
        (true, true) => {
            for ((top, bottom), (here, below)) in cells.zip(pairs) {
                if let (&[a, b], &[c, d]) = (here, below) {
                    *top = [a, average2(a, b)];
                    *bottom = [average2(a, c), average4(a, b, c, d)];
                }
            }
        }
    }
    // The right edge clamps: no horizontal neighbour.
    let lower_left = if mode.vertical {
        average2(last, last_below)
    } else {
        last
    };
    if let (Some(top), Some(bottom)) = (upper.last_chunk_mut::<2>(), lower.last_chunk_mut::<2>()) {
        *top = [last, last];
        *bottom = [lower_left, lower_left];
    }
}

/// Expand one logical line into its two output lines.
fn expand_line(
    row: &[u32],
    below_row: &[u32],
    mode: LineMode,
    upper: &mut [u32],
    lower: &mut [u32],
) {
    let mut upper_cells = upper.chunks_mut(2);
    let mut lower_cells = lower.chunks_mut(2);
    let mut rights = row.iter().skip(1);
    let mut diagonals = below_row.iter().skip(1);
    for (&here, &below) in row.iter().zip(below_row) {
        let right = rights.next().copied();
        let diagonal = diagonals.next().copied().unwrap_or(below);
        let (top, bottom) = expand_pixel(here, right, below, diagonal, mode);
        if let Some(cell) = upper_cells.next() {
            for (dst, src) in cell.iter_mut().zip(top) {
                *dst = src;
            }
        }
        if let Some(cell) = lower_cells.next() {
            for (dst, src) in cell.iter_mut().zip(bottom) {
                *dst = src;
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn the_fast_expansion_matches_the_reference_in_every_mode() {
        let mut seed = 0x1234_5678u32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            OPAQUE_BLACK | (seed & 0x00ff_ffff)
        };
        for width in [1usize, 2, 3, 17, 64] {
            let row: Vec<u32> = (0..width).map(|_| next()).collect();
            let below: Vec<u32> = (0..width).map(|_| next()).collect();
            for (horizontal, vertical) in
                [(false, false), (true, false), (false, true), (true, true)]
            {
                let mode = LineMode {
                    horizontal,
                    vertical,
                };
                let (mut up_a, mut low_a) = (vec![0; width * 2], vec![0; width * 2]);
                let (mut up_b, mut low_b) = (vec![0; width * 2], vec![0; width * 2]);
                expand_line(&row, &below, mode, &mut up_a, &mut low_a);
                expand_line_fast(&row, &below, mode, &mut up_b, &mut low_b);
                assert_eq!((up_a, low_a), (up_b, low_b), "width {width}, {mode:?}");
            }
        }
    }
}
