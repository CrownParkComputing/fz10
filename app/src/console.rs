//! The machine as a whole: the CPU, the system it addresses, and the frame
//! loop that drives them.
//!
//! The console never presents. It emulates a field into [`Console::framebuffer`]
//! and returns; whoever owns the window decides when to draw it. Threading,
//! pacing and the frame mailbox live in [`crate::emulator`].
#![forbid(unsafe_code)]

use std::fs::File;
use std::io::Read;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use crate::arm60::Arm60;
use crate::audio_ring::{AudioRing, StereoSample, SAMPLE_RATE};
use crate::clio::Clio;
use crate::demo_rom::builtin_demo_rom;
use crate::disc::Disc;
use crate::madam::Madam;
use crate::memory::{Memory, RomError, WriteWatch, VRAM_BASE};
use crate::pad::{Joypad, PadState};
use crate::sport::Sport;
use crate::system::System;
use crate::vdlp::Vdlp;

/// The ARM60 clock; every other rate is derived from it.
pub const CPU_HZ: u32 = 12_500_000;
/// The DSP runs one pass of its program per output sample: about every 283
/// CPU cycles.
/// 283 cycles, as `Console::tick_dsp` (reference build 0x144c0) compares it.
const SAMPLE_PERIOD: u32 = CPU_HZ / SAMPLE_RATE;
/// Bound on a frame's sample buffer, so a runaway sample clock cannot grow it.
const MAX_SAMPLES_PER_FRAME: usize = 4096;
/// Where the visible picture starts inside the field: the display list runs
/// from well before it.
/// 0x15 in `Console::set_region` (reference build 0x13ca0).
const FIRST_VISIBLE_LINE: u32 = 21;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Region {
    /// 320x240, 60 fields a second.
    #[default]
    Ntsc,
    /// 320x288, 50 fields a second.
    Pal,
}

impl Region {
    pub fn fields_per_second(self) -> u32 {
        match self {
            Region::Ntsc => 60,
            Region::Pal => 50,
        }
    }
    /// Lines in a field, including vertical blank.
    pub fn scanlines_per_field(self) -> u32 {
        match self {
            Region::Ntsc => 263,
            Region::Pal => 313,
        }
    }
    pub fn visible_height(self) -> u32 {
        match self {
            Region::Ntsc => 240,
            Region::Pal => 288,
        }
    }
    pub fn cycles_per_frame(self) -> u32 {
        CPU_HZ / self.fields_per_second()
    }
    pub fn cycles_per_scanline(self) -> u32 {
        (self.cycles_per_frame() / self.scanlines_per_field()).max(2)
    }
}

/// A finished frame: XRGB8888 in host order, tightly packed.
#[derive(Clone, Copy, Debug)]
pub struct Frame<'a> {
    pub pixels: &'a [u32],
    pub width: usize,
    pub height: usize,
}

pub struct Console {
    cpu: Arm60,
    sys: System,
    audio: Arc<AudioRing>,
    cpu_scale_percent: Arc<AtomicU32>,

    region: Region,
    frame_width: u32,
    frame_height: u32,
    /// Internal cel resolution (1 = native).
    internal_scale: u32,
    /// The output signal: CLIO always doubles the 320-wide picture both ways.
    framebuffer: Vec<u32>,

    sample_accumulator: u32,
    samples: Vec<StereoSample>,

    last_error: String,
    demo_loaded: bool,
    frame_count: u64,
}

impl Default for Console {
    fn default() -> Self {
        Self::new()
    }
}

impl Console {
    pub fn new() -> Self {
        Self::with_shared(
            Arc::new(PadState::new()),
            Arc::new(AudioRing::new()),
            Arc::new(AtomicU32::new(100)),
        )
    }

    /// A console whose pads, audio ring and CPU-scale setting are shared with
    /// another thread (the host's input, audio callback and settings).
    pub fn with_shared(
        pads: Arc<PadState>,
        audio: Arc<AudioRing>,
        cpu_scale_percent: Arc<AtomicU32>,
    ) -> Self {
        let mut console = Self {
            cpu: Arm60::new(),
            sys: System::new(pads),
            audio,
            cpu_scale_percent,
            region: Region::Ntsc,
            frame_width: 320,
            frame_height: 240,
            internal_scale: 1,
            framebuffer: Vec::new(),
            sample_accumulator: 0,
            samples: Vec::with_capacity(MAX_SAMPLES_PER_FRAME),
            last_error: String::new(),
            demo_loaded: false,
            frame_count: 0,
        };
        console.set_region(Region::Ntsc);
        console.reset();
        console
    }

    // --- media ------------------------------------------------------------

    /// Install a BIOS image from memory.
    pub fn load_bios_bytes(&mut self, image: &[u8], name: &str) -> bool {
        match self.sys.mem.load_rom(image) {
            Ok(()) => {
                // New code underneath the CPU: anything decoded is stale.
                self.demo_loaded = false;
                self.cpu.invalidate_decode_cache();
                self.last_error.clear();
                true
            }
            Err(RomError::Empty) => self.fail(format!("BIOS file is empty: {name}")),
            Err(RomError::TooLarge) => {
                self.fail("BIOS image is larger than the 3DO's ROM window".into())
            }
        }
    }

    pub fn load_bios(&mut self, path: &str) -> bool {
        match std::fs::read(path) {
            Ok(image) => self.load_bios_bytes(&image, path),
            Err(_) => self.fail(format!("Could not open BIOS file: {path}")),
        }
    }

    /// The C ABI's descriptor variant: the file is already open.
    pub fn load_bios_file(&mut self, mut file: File, display_name: &str) -> bool {
        let mut image = Vec::new();
        match file.read_to_end(&mut image) {
            Ok(_) => self.load_bios_bytes(&image, display_name),
            Err(_) => self.fail(format!(
                "BIOS file could not be read in full: {display_name}"
            )),
        }
    }

    /// The project-owned demonstration ROM. A hardware test, not a BIOS.
    pub fn load_builtin_demo(&mut self) -> bool {
        let rom = builtin_demo_rom();
        if self.sys.mem.load_rom(rom.data).is_err() {
            self.demo_loaded = false;
            return self.fail("The built-in demonstration ROM could not be loaded".into());
        }
        self.demo_loaded = true;
        self.cpu.invalidate_decode_cache();
        self.last_error.clear();
        true
    }

    pub fn bios_loaded(&self) -> bool {
        self.sys.mem.bios_loaded()
    }
    pub fn demo_loaded(&self) -> bool {
        self.demo_loaded
    }

    /// Insert a disc image. Replacing a disc is an eject and an insert, not a
    /// reset: software at a "please insert disc" prompt sees the same media
    /// change a real drive raises.
    pub fn load_disc(&mut self, path: &str) -> bool {
        self.insert(|disc| disc.open(path))
    }

    /// The descriptor variant. `display_name` supplies the filename (and so the
    /// container format) the descriptor does not carry.
    pub fn load_disc_file(&mut self, file: File, display_name: &str) -> bool {
        self.insert(|disc| disc.open_file(file, display_name))
    }

    fn insert(&mut self, open: impl FnOnce(&mut Disc) -> bool) -> bool {
        let drive = self.sys.clio.cdrom_mut();
        if drive.disc().is_some() {
            drive.set_disc_present(false);
        }
        let mut disc = Disc::new();
        if !open(&mut disc) {
            drive.attach_disc(None);
            let reason = disc.last_error().to_owned();
            return self.fail(reason);
        }
        // Opening the image and telling the drive are different things; only
        // the second is visible to software.
        drive.attach_disc(Some(disc));
        drive.set_disc_present(true);
        self.last_error.clear();
        true
    }

    pub fn eject_disc(&mut self) {
        let drive = self.sys.clio.cdrom_mut();
        drive.attach_disc(None);
        drive.set_disc_present(false);
    }

    pub fn disc_loaded(&self) -> bool {
        self.disc().is_some_and(Disc::is_open)
    }
    pub fn disc(&self) -> Option<&Disc> {
        self.sys.clio.cdrom().disc()
    }

    fn fail(&mut self, reason: String) -> bool {
        self.last_error = reason;
        false
    }

    pub fn last_error(&self) -> &str {
        &self.last_error
    }

    // --- configuration ----------------------------------------------------

    pub fn set_region(&mut self, region: Region) {
        self.region = region;
        self.frame_width = 320;
        self.frame_height = region.visible_height();
        self.sys
            .clio
            .set_scanlines_per_field(region.scanlines_per_field());
        self.apply_geometry();
        self.sys
            .vdlp
            .set_field_shape(FIRST_VISIBLE_LINE, region.scanlines_per_field() - 1);
        self.resize_output();
    }

    /// MADAM's clip and default target follow the region; its own reset puts
    /// back NTSC-sized defaults, so this is reapplied after every reset.
    fn apply_geometry(&mut self) {
        self.sys.madam.set_clip(self.frame_width, self.frame_height);
        self.sys.madam.set_target(VRAM_BASE, self.frame_width * 2);
    }

    pub fn region(&self) -> Region {
        self.region
    }

    /// Rasterize cels at `scale` times native resolution (1 to
    /// [`crate::hires::MAX_INTERNAL_SCALE`]; 1, the default, is the machine
    /// exactly as it was). The output frame becomes `(640 * k) x (2h * k)`.
    /// Emulation is unaffected: VRAM, timing and everything software can
    /// observe stay native. Safe between any two fields.
    pub fn set_internal_scale(&mut self, scale: u32) {
        let scale = crate::hires::clamp_scale(scale);
        if scale == self.internal_scale {
            return;
        }
        self.internal_scale = scale;
        self.sys.madam.set_internal_scale(scale);
        self.sys.vdlp.set_internal_scale(scale);
        self.resize_output();
    }

    pub fn internal_scale(&self) -> u32 {
        self.internal_scale
    }

    /// Point the hi-res pass at a directory of hand-authored cel
    /// replacements (`<fnv>.png`, see [`crate::texpack`]); `None` clears it.
    /// Presentation only — emulation is unaffected.
    /// Read the console's own cel art as what it was drawn to mean: undo the
    /// dithering 3DO artists used in place of colours the hardware lacks, and
    /// interpolate where the internal scale gives room. Works for every
    /// title, with no artwork of its own. Presentation only.
    pub fn set_texture_enhance(&mut self, enhance: bool) {
        self.sys.madam.set_texture_enhance(enhance);
        let pack_mode = self.sys.madam.hires().pack_mode();
        self.sys.vdlp.set_pack_mode(pack_mode);
        self.resize_output();
    }

    pub fn texture_enhance(&self) -> bool {
        self.sys.madam.texture_enhance()
    }

    pub fn set_texture_dir(&mut self, dir: Option<&std::path::Path>) {
        self.sys.madam.set_texture_dir(dir);
        // A pack is reason enough to run the hi-res pass: at native size it
        // puts the artwork in place of the console's texels, so the picture
        // is the pack's even on a machine that cannot hold a higher scale.
        let pack_mode = self.sys.madam.hires().pack_mode();
        self.sys.vdlp.set_pack_mode(pack_mode);
        self.resize_output();
    }

    fn resize_output(&mut self) {
        let (width, height) = self
            .sys
            .vdlp
            .output_size(self.frame_width, self.frame_height);
        self.framebuffer.clear();
        self.framebuffer
            .resize(width as usize * height as usize, 0xff00_0000);
    }

    /// More ARM work per field without moving any other clock. A slowdown
    /// aid, not fast-forward. Clamped to 100..=150.
    pub fn set_cpu_scale_percent(&self, percent: u32) {
        self.cpu_scale_percent
            .store(percent.clamp(100, 150), Ordering::Release);
    }
    pub fn cpu_scale_percent(&self) -> u32 {
        self.cpu_scale_percent.load(Ordering::Acquire)
    }

    pub fn reset(&mut self) {
        self.sys.reset();
        self.cpu.reset();
        self.apply_geometry();
        self.sys.pads.reset();
        self.audio.reset();
        self.sample_accumulator = 0;
        self.samples.clear();
        self.frame_count = 0;
        self.framebuffer.fill(0xff00_0000);
    }

    // --- running ----------------------------------------------------------

    /// Emulate one field. Returns the CPU cycles spent.
    pub fn run_frame(&mut self) -> u32 {
        let budget = self.region.cycles_per_frame();
        let scale = self.cpu_scale_percent();

        // The display-list root is latched at the field boundary; each line is
        // then fetched by the scanline callback during horizontal blank.
        let root = self.sys.madam.vdl_address();
        self.sys.vdlp.set_list_address(root);
        self.sys
            .vdlp
            .begin_field(self.frame_width, self.frame_height);

        // Slices shorter than a scanline: the CPU can only observe a line
        // number if CLIO stops on it, and the boot ROM waits for exact lines.
        let slice = self.region.cycles_per_scanline() / 2;
        let mut cpu_spent: u32 = 0;
        if scale == 100 {
            // Peripherals see the few cycles by which the last instruction of a
            // slice overshoots.
            while cpu_spent < budget {
                let ran = self.cpu.run(&mut self.sys, (budget - cpu_spent).min(slice));
                cpu_spent = cpu_spent.saturating_add(ran);
                if self.advance_hardware(ran) {
                    break;
                }
            }
        } else {
            // A second clock domain: video, timers and the DSP keep stock time;
            // only the ARM gets the scaled budget. Cumulative targets carry the
            // rounding forward instead of compounding it.
            let mut hardware_spent: u32 = 0;
            while hardware_spent < budget {
                let step = (budget - hardware_spent).min(slice);
                hardware_spent += step;
                let target = (u64::from(hardware_spent) * u64::from(scale) / 100) as u32;
                if cpu_spent < target {
                    let ran = self.cpu.run(&mut self.sys, target - cpu_spent);
                    cpu_spent = cpu_spent.saturating_add(ran);
                }
                if self.advance_hardware(step) {
                    break;
                }
            }
        }

        // This frame's DAC words. A stopped DSP contributes zeroes: silence
        // that is the truth rather than a stand-in.
        if !self.samples.is_empty() {
            self.audio.push(&self.samples);
            self.samples.clear();
        }

        // Lines are already latched; the field boundary only expands the
        // logical picture to the 2x output signal.
        if self.internal_scale > 1 || self.sys.madam.hires().pack_mode() {
            self.sys.vdlp.finish_field_hires(
                &mut self.framebuffer,
                self.frame_width,
                self.frame_height,
            );
        } else {
            self.sys.vdlp.finish_field_2x(
                &mut self.framebuffer,
                self.frame_width,
                self.frame_height,
            );
        }
        self.frame_count += 1;
        cpu_spent
    }

    /// Advance everything but the CPU by `cycles`. Returns true at the field
    /// boundary.
    fn advance_hardware(&mut self, cycles: u32) -> bool {
        self.sys.tick(cycles);
        self.tick_dsp(cycles);
        self.apply_code_invalidations();
        if self.sys.clio.field_complete() {
            self.sys.clio.clear_field_complete();
            return true;
        }
        false
    }

    /// Run the DSP once per output sample. The low half of its DAC word is
    /// left, the high half right; a pass that writes nothing leaves the
    /// previous pair, as real DACs do.
    fn tick_dsp(&mut self, cycles: u32) {
        self.sample_accumulator = self.sample_accumulator.saturating_add(cycles);
        while self.sample_accumulator >= SAMPLE_PERIOD {
            self.sample_accumulator -= SAMPLE_PERIOD;
            let dac = self.sys.run_dsp_sample();
            if self.samples.len() < MAX_SAMPLES_PER_FRAME {
                self.samples.push(StereoSample::new(
                    dac as u16 as i16,
                    (dac >> 16) as u16 as i16,
                ));
            }
        }
    }

    /// Drop decoded instructions for memory written since the last slice.
    fn apply_code_invalidations(&mut self) {
        for (address, length) in self.sys.drain_code_invalidations() {
            self.cpu.invalidate_decode_range(address, length);
        }
        let watch = self.sys.mem.write_watch();
        if watch.is_dirty() {
            for page in watch.dirty_pages() {
                self.cpu
                    .invalidate_decode_range(page * WriteWatch::PAGE_BYTES, WriteWatch::PAGE_BYTES);
            }
            self.sys.mem.write_watch_mut().clear();
        }
    }

    pub fn framebuffer(&self) -> Frame<'_> {
        Frame {
            pixels: &self.framebuffer,
            width: (self.frame_width * 2 * self.internal_scale) as usize,
            height: (self.frame_height * 2 * self.internal_scale) as usize,
        }
    }

    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    // --- input ------------------------------------------------------------

    /// Set pad one directly. Writes through to the same place the front end
    /// does, so there is only ever one answer to "what is held".
    pub fn set_joypad(&self, pad: Joypad) {
        self.sys.pads.set_buttons(0, pad.bits());
    }
    pub fn joypad(&self) -> Joypad {
        self.sys.pads.joypad(0)
    }
    pub fn pads(&self) -> &PadState {
        &self.sys.pads
    }
    pub fn audio(&self) -> &AudioRing {
        &self.audio
    }

    // --- the parts --------------------------------------------------------

    pub fn cpu(&self) -> &Arm60 {
        &self.cpu
    }
    pub fn cpu_mut(&mut self) -> &mut Arm60 {
        &mut self.cpu
    }
    pub fn system(&self) -> &System {
        &self.sys
    }
    pub fn system_mut(&mut self) -> &mut System {
        &mut self.sys
    }
    /// The CPU and the system together, for stepping the CPU by hand.
    pub fn cpu_and_system(&mut self) -> (&mut Arm60, &mut System) {
        (&mut self.cpu, &mut self.sys)
    }
    pub fn memory(&self) -> &Memory {
        &self.sys.mem
    }
    pub fn memory_mut(&mut self) -> &mut Memory {
        &mut self.sys.mem
    }
    pub fn clio(&self) -> &Clio {
        &self.sys.clio
    }
    pub fn clio_mut(&mut self) -> &mut Clio {
        &mut self.sys.clio
    }
    pub fn madam(&self) -> &Madam {
        &self.sys.madam
    }
    pub fn madam_mut(&mut self) -> &mut Madam {
        &mut self.sys.madam
    }
    pub fn vdlp(&self) -> &Vdlp {
        &self.sys.vdlp
    }
    pub fn sport(&self) -> &Sport {
        &self.sys.sport
    }
    pub fn expansion_dma_count(&self) -> u64 {
        self.sys.expansion_dma_count()
    }

    // --- saved games --------------------------------------------------------

    pub fn nvram(&self) -> &[u8] {
        self.sys.mem.nvram()
    }
    pub fn nvram_size(&self) -> usize {
        self.sys.mem.nvram().len()
    }
    pub fn restore_nvram(&mut self, image: &[u8]) -> bool {
        self.sys.mem.restore_nvram(image)
    }
}
