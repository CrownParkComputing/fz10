//! MADAM: the 3DO's graphics and DMA engine.
//!
//! MADAM draws by walking a linked list of Cel Control Blocks. A CCB says
//! where a cel's pixels live, how they are encoded, and gives the 16.16
//! vectors that map the source rectangle onto the destination; because the
//! mapping is a general affine step with a per-row bend, a cel can be scaled,
//! rotated, sheared and given perspective. Every textured polygon in a 3DO
//! title is a cel.
//!
//! Specification basis: CCB, preamble and register fields from the SDK
//! `hardware.h` and the Graphics Programming Guide; projection, the pixel
//! decoder and processor and CEL bus state from WO 94/10644 (Spryte
//! rendering) and WO 94/10643; the matrix unit from `operamath.h` and
//! WO 94/10641. The visibility, scan-conversion and LR row-pair algorithms are
//! project implementations around those descriptions.
//!
//! Two decisions are structural rather than incidental:
//!
//! * Positions step incrementally: source coordinates advance by adding the
//!   vectors, never by multiplying per pixel.
//! * Cels are drawn strictly in list order. Later cels paint over earlier
//!   ones, so threading the list would race on the destination; the only safe
//!   split is destination row bands inside one cel.
//!
//! MADAM references no other chip. Everything that touches memory takes the
//! [`Memory`]; what the C++ core did through callbacks is state the machine
//! drains: [`Madam::cel_engine_pending`] / [`Madam::run_cel_engine`] at the
//! next instruction boundary, [`Madam::take_pbus_request`] right after a store
//! to the DMA-enable register, and [`Madam::take_interrupts`] for the CLIO
//! bank-0 bits the DSP DMA channels raise.
#![forbid(unsafe_code)]

mod ccb;
mod decoder;
mod fifo;
mod hires_pass;
mod matrix;
mod pixel;
mod raster;
mod source;

pub use ccb::*;
pub use fifo::{INPUT_FIFOS, MADAM_FIFO_BASE, MADAM_FIFO_END, MADAM_FIFO_OUTPUT, OUTPUT_FIFOS};
pub use matrix::{
    MADAM_MATRIX_CTL, MADAM_MATRIX_IN, MADAM_MATRIX_NUM_HI, MADAM_MATRIX_NUM_LO, MADAM_MATRIX_OUT,
    MADAM_MATRIX_VEC, MATRIX_COPY_ONLY, MATRIX_MULTIPLY_3X3, MATRIX_MULTIPLY_3X3_DIVIDE_Z,
    MATRIX_MULTIPLY_4X4,
};
pub use pixel::{
    CTL0_B0_MASK, CTL0_B0_ONE, CTL0_B0_PDC, CTL0_B0_PPMP, CTL0_B0_ZERO, CTL0_B15_MASK,
    CTL0_B15_ONE, CTL0_B15_PDC, CTL0_B15_ZERO, CTL0_SWAP_HV,
};
pub use raster::{cel_is_invisible, quad_is_wrong_way_round};

use decoder::{read_be16_bytes, CelDecoder, Texel as Decoded};
use fifo::DmaFifo;
use hires_pass::{run_worker, Applier, CelSetup, Colour, Job, RawPatch};
use matrix::MatrixEngine;
use pixel::PixelProcessor;
use raster::{
    scaled_footprint, scaled_footprint_at, scan_quad, Clip, Mapper, Rect, RowWalker,
    Texel,
};
use source::{BitReader, Packet};

use crate::hires::HiResStore;
use crate::memory::{in_window, Memory, VRAM_BASE, VRAM_SIZE};
use crate::video::framebuffer_offset;

// --- register map (offsets within MADAM's window) --------------------------

/// Chip identification, read-only.
pub const MADAM_REVISION: u32 = 0x0000;
/// Fitted memory, read-only. VRAM MB in bits 0-2, DRAM banks in 3-4 and 5-6.
pub const MADAM_MEM_CONFIG: u32 = 0x0004;
/// MCTL: DMA enables. Writing bit 15 starts a PBUS (controller) scan.
pub const MADAM_DMA_ENABLE: u32 = 0x0008;
/// SPRON / SPRPAU: bit 4 while the engine runs, bits 4 and 5 when suspended.
pub const MADAM_CEL_STATUS: u32 = 0x0028;

/// The CEL control ports. None carries an address; the port is the command.
pub const MADAM_CEL_START: u32 = 0x0100;
pub const MADAM_CEL_STOP: u32 = 0x0104;
/// SPRCNTU.
pub const MADAM_CEL_RESUME: u32 = 0x0108;
pub const MADAM_CEL_PAUSE: u32 = 0x010c;
/// CCBCTL0: B15/B0 position-bit sources and the H/V swap.
pub const MADAM_CCB_CTL0: u32 = 0x0110;

/// REGCTL0: read and write row strides, encoded, write eight bits up.
pub const MADAM_REG_CTL0: u32 = 0x0130;
/// REGCTL1: the inclusive clip rectangle (x in bits 0-9, y in 16-25).
pub const MADAM_REG_CTL1: u32 = 0x0134;
/// REGCTL2: the buffer cels read the frame FROM.
pub const MADAM_REG_CTL2: u32 = 0x0138;
/// REGCTL3: the buffer cels are drawn INTO.
pub const MADAM_REG_CTL3: u32 = 0x013c;

/// The general DMA channel array: address then length, eight bytes apart.
pub const MADAM_DMA_BASE: u32 = 0x0200;
pub const MADAM_DMA_STRIDE: u32 = 8;
pub const MADAM_DMA_CHANNELS: u32 = 32;

/// Expansion-bus DMA. Writing the length (bytes minus four) arms it.
pub const MADAM_XBUS_DMA_ADDRESS: u32 = 0x0540;
pub const MADAM_XBUS_DMA_LENGTH: u32 = 0x0544;

/// PBUS (controller) DMA.
pub const MADAM_PBUS_START: u32 = 0x8000;
pub const MADAM_PBUS_ADDRESS: u32 = 0x0570;
pub const MADAM_PBUS_LENGTH: u32 = 0x0574;
pub const MADAM_PBUS_POINTER: u32 = 0x0578;

/// The absolute bus address of the video display list.
pub const MADAM_VDL_ADDRESS: u32 = 0x0580;

/// The CEL walk: where the fetch cursor is, and where the list goes next.
pub const MADAM_CURRENT_CCB: u32 = 0x05a0;
pub const MADAM_NEXT_CCB: u32 = 0x05a4;

pub const MADAM_WINDOW_SIZE: u32 = 0x1_0000;

pub const MADAM_CEL_RUNNING: u32 = 0x10;
pub const MADAM_CEL_PAUSED: u32 = 0x20;

/// A stock consumer machine: VRAM 1 MB, DRAM 1 MB + 1 MB.
pub const MADAM_MEM_CONFIG_STOCK: u32 = 0x29;
/// The Green MADAM revision with the hardware matrix engine.
pub const MADAM_REVISION_GREEN: u32 = 0x0102_0000;

// Short names for the same offsets and values, for callers that already say
// `madam::` (`madam::XBUS_DMA_LENGTH` rather than `madam::MADAM_XBUS_DMA_LENGTH`).
pub const REVISION: u32 = MADAM_REVISION;
pub const MEM_CONFIG: u32 = MADAM_MEM_CONFIG;
pub const DMA_ENABLE: u32 = MADAM_DMA_ENABLE;
pub const CEL_STATUS: u32 = MADAM_CEL_STATUS;
pub const CEL_START: u32 = MADAM_CEL_START;
pub const CEL_STOP: u32 = MADAM_CEL_STOP;
pub const CEL_RESUME: u32 = MADAM_CEL_RESUME;
pub const CEL_PAUSE: u32 = MADAM_CEL_PAUSE;
pub const CCB_CTL0: u32 = MADAM_CCB_CTL0;
pub const REG_CTL0: u32 = MADAM_REG_CTL0;
pub const REG_CTL1: u32 = MADAM_REG_CTL1;
pub const REG_CTL2: u32 = MADAM_REG_CTL2;
pub const REG_CTL3: u32 = MADAM_REG_CTL3;
pub const DMA_BASE: u32 = MADAM_DMA_BASE;
pub const XBUS_DMA_ADDRESS: u32 = MADAM_XBUS_DMA_ADDRESS;
pub const XBUS_DMA_LENGTH: u32 = MADAM_XBUS_DMA_LENGTH;
pub const PBUS_START: u32 = MADAM_PBUS_START;
pub const PBUS_ADDRESS: u32 = MADAM_PBUS_ADDRESS;
pub const PBUS_LENGTH: u32 = MADAM_PBUS_LENGTH;
pub const PBUS_POINTER: u32 = MADAM_PBUS_POINTER;
pub const VDL_ADDRESS: u32 = MADAM_VDL_ADDRESS;
pub const CURRENT_CCB: u32 = MADAM_CURRENT_CCB;
pub const NEXT_CCB: u32 = MADAM_NEXT_CCB;
pub const FIFO_BASE: u32 = MADAM_FIFO_BASE;
pub const FIFO_OUTPUT: u32 = MADAM_FIFO_OUTPUT;
pub const FIFO_END: u32 = MADAM_FIFO_END;
pub const MATRIX_IN: u32 = MADAM_MATRIX_IN;
pub const MATRIX_VEC: u32 = MADAM_MATRIX_VEC;
pub const MATRIX_OUT: u32 = MADAM_MATRIX_OUT;
pub const MATRIX_NUM_HI: u32 = MADAM_MATRIX_NUM_HI;
pub const MATRIX_NUM_LO: u32 = MADAM_MATRIX_NUM_LO;
pub const MATRIX_CTL: u32 = MADAM_MATRIX_CTL;
pub const WINDOW_SIZE: u32 = MADAM_WINDOW_SIZE;
pub const CEL_RUNNING: u32 = MADAM_CEL_RUNNING;
pub const CEL_PAUSED: u32 = MADAM_CEL_PAUSED;
pub const MEM_CONFIG_STOCK: u32 = MADAM_MEM_CONFIG_STOCK;
pub const REVISION_GREEN: u32 = MADAM_REVISION_GREEN;

/// A cel list longer than this is treated as garbage: a dropped frame rather
/// than a freeze.
pub const MAX_CELS: u32 = 4096;

/// Low registers whose writes are recorded for bring-up.
pub const TRACKED_REGISTERS: u32 = 2048;

/// What the engine did on the last list, plus DSP channel diagnostics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MadamStats {
    pub cels_walked: u32,
    pub cels_drawn: u32,
    pub pixels_written: u64,
    /// Non-zero if the walk hit its safety limit.
    pub list_truncated: u32,
    pub fifo_asked: [u64; INPUT_FIFOS],
    pub fifo_delivered: [u64; INPUT_FIFOS],
    pub fifo_reloads: [u64; INPUT_FIFOS],
    pub fifo_out_writes: [u64; OUTPUT_FIFOS],
    pub fifo_out_stored: [u64; OUTPUT_FIFOS],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum CelEngineState {
    #[default]
    Idle,
    InProcess,
    Suspended,
}

/// The stride is the distance between one PAIR of rows and the next, built
/// from three fields at three weights.
const fn decode_stride(value: u32) -> i32 {
    (((value & 0x01) << 7) + ((value & 0x0c) << 8) + ((value & 0x70) << 4)) as i32
}

/// Byte offset of a pixel in a row-pair-interleaved buffer.
fn pixel_offset(x: i32, y: i32, stride: i32) -> u32 {
    (y >> 1)
        .wrapping_mul(stride)
        .wrapping_add((y & 1) << 1)
        .wrapping_add(x.wrapping_shl(2)) as u32
}

/// Where cels are drawn.
#[derive(Clone, Copy, Debug)]
struct Destination {
    /// Software has programmed REGCTL0/2/3: pixels go through the processor.
    configured: bool,
    reg_ctl0: u32,
    reg_ctl1: u32,
    read_base: u32,
    write_base: u32,
    read_stride: i32,
    write_stride: i32,
    clip_width: u32,
    clip_height: u32,
    /// The plain-store target used before software configures the buffer.
    target_address: u32,
    target_stride_bytes: u32,
}

impl Default for Destination {
    fn default() -> Self {
        Self {
            configured: false,
            reg_ctl0: 0,
            reg_ctl1: 0,
            read_base: 0,
            write_base: 0,
            read_stride: 320 * 4,
            write_stride: 320 * 4,
            clip_width: 320,
            clip_height: 240,
            target_address: VRAM_BASE,
            target_stride_bytes: 320 * 2,
        }
    }
}

impl Destination {
    fn clip(&self) -> Clip {
        Clip {
            width: self.clip_width as i32,
            height: self.clip_height as i32,
        }
    }

    fn contains(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && (x as u32) < self.clip_width && (y as u32) < self.clip_height
    }
}

/// Per-cel engine setup, fixed when a cel starts.
#[derive(Clone, Debug, Default)]
struct CelState {
    decoder: CelDecoder,
    processor: PixelProcessor,
    mapper: Mapper,
    bpp: u32,
}

#[derive(Clone, Debug)]
pub struct Madam {
    mem_config: u32,
    dma_enable: u32,
    dma_address: [u32; MADAM_DMA_CHANNELS as usize],
    dma_length: [u32; MADAM_DMA_CHANNELS as usize],
    xbus_dma_address: u32,
    xbus_dma_length: u32,
    xbus_dma_pending: bool,
    vdl_address: u32,

    pbus_address: u32,
    pbus_length: u32,
    pbus_pointer: u32,
    pbus_request: bool,

    input_fifo: [DmaFifo; INPUT_FIFOS],
    output_fifo: [DmaFifo; OUTPUT_FIFOS],
    dma_channel_enable: u32,
    interrupts: u32,

    cel_state: CelEngineState,
    cel_pending: bool,
    cel_walk_started: bool,
    current_ccb: u32,
    next_ccb: u32,
    ccb_ctl0: u32,
    registers: CcbRegisters,
    cel: CelState,
    destination: Destination,

    matrix: MatrixEngine,

    stats: MadamStats,
    engine_runs: u64,
    total_cels_drawn: u64,

    written_value: Vec<u32>,
    written_flag: Vec<bool>,

    /// Internal-resolution mirrors (see [`crate::hires`]). Output only: the
    /// machine never reads them.
    hires: HiResStore,
    /// The surface the list being walked draws into, when the scale is above 1.
    hires_target: Option<usize>,
    /// The list wrote a native pixel the target's shadow does not cover (a
    /// clip wider than the buffer), so the surface must re-check its pages.
    hires_unsynced: bool,
    /// The target's native width and height and its scale, fixed for a walk.
    hires_geometry: (u32, u32, u32),
    /// Per-cel hi-res setup, fixed while one cel draws.
    hires_cel: HiResCel,
    /// Native pixels blended for the current texel, tagged with its serial so
    /// The walk's hi-res work, in order (see `hires_pass`).
    hires_jobs: Vec<Job>,
    /// A walk that records this many jobs moves the hi-res work to a helper
    /// thread.
    hires_worker_threshold: usize,
    /// Emptied job batches kept from walk to walk: allocating fresh ones
    /// costs a page fault every few dozen jobs.
    hires_pool: Vec<Vec<Job>>,
    /// The column steps last sent, so they go once per row, not per texel.
    hires_steps: Option<[i32; 4]>,
    /// When the current cel's source is collected for texture filtering.
    hires_collecting: bool,
    /// The cel setup to send once its texels are collected and filtered.
    hires_setup: Option<CelSetup>,
    /// The current cel's jobs, held until its filtered patch is built so the
    /// [`Job::Cel`] carrying the patch precedes them in the stream.
    hires_pending: Vec<Job>,
    /// The cel's decoded source image in grid order (transparent texels keep
    /// their slots), for [`filtered_patch`]. Sized `patch_w * patch_h`.
    hires_patch: Vec<Decoded>,
    hires_patch_w: u32,
    hires_patch_h: u32,
    /// Texel grid position of the next decoded source pixel.
    hires_src_x: u32,
    hires_src_y: u32,
    /// Hand-authored cel replacements the hi-res pass prefers over filtering.
    texture_pack: Option<std::sync::Arc<crate::texpack::TexturePack>>,
    /// Read the console's own cel art as what it was drawn to mean — undo its
    /// dithering, interpolate where the scale allows — for every title, with
    /// no artwork of its own.
    texture_enhance: bool,
}

/// Jobs before a walk is worth a helper thread: spawning one costs tens of
/// microseconds, applying this many in place about as much.
const HIRES_WORKER_THRESHOLD: usize = 4096;

/// Jobs handed to the helper at a time.
const HIRES_BATCH: usize = 4096;

/// Job batches kept for reuse (and the most in flight to the helper).
const HIRES_POOL: usize = 128;

/// What every texel of one cel shares in the hi-res pass.
#[derive(Clone, Copy, Debug)]
struct HiResCel {
    scale: u32,
    clip: Clip,
    /// The cel's blend ignores the frame: its texels go to the helper as
    /// geometry, blended once each.
    ignores_frame: bool,
    /// The cel's two pixel-processor halves disagree about the frame, so each
    /// texel has to be asked which one it lands in.
    mixed_frame: bool,
}

impl Default for Madam {
    fn default() -> Self {
        Self::new()
    }
}

impl Madam {
    pub fn new() -> Self {
        Self {
            mem_config: MADAM_MEM_CONFIG_STOCK,
            dma_enable: 0,
            dma_address: [0; MADAM_DMA_CHANNELS as usize],
            dma_length: [0; MADAM_DMA_CHANNELS as usize],
            xbus_dma_address: 0,
            xbus_dma_length: 0,
            xbus_dma_pending: false,
            vdl_address: 0,
            pbus_address: 0,
            pbus_length: 0,
            pbus_pointer: 0,
            pbus_request: false,
            input_fifo: [DmaFifo::default(); INPUT_FIFOS],
            output_fifo: [DmaFifo::default(); OUTPUT_FIFOS],
            dma_channel_enable: 0,
            interrupts: 0,
            cel_state: CelEngineState::Idle,
            cel_pending: false,
            cel_walk_started: false,
            current_ccb: 0,
            next_ccb: 0,
            ccb_ctl0: 0,
            registers: CcbRegisters::default(),
            cel: CelState::default(),
            destination: Destination::default(),
            matrix: MatrixEngine::default(),
            stats: MadamStats::default(),
            engine_runs: 0,
            total_cels_drawn: 0,
            written_value: vec![0; TRACKED_REGISTERS as usize],
            written_flag: vec![false; TRACKED_REGISTERS as usize],
            hires: HiResStore::new(),
            hires_target: None,
            hires_unsynced: false,
            hires_geometry: (0, 0, 1),
            hires_cel: HiResCel {
                scale: 1,
                clip: Clip {
                    width: 0,
                    height: 0,
                },
                ignores_frame: false,
                mixed_frame: false,
            },
            hires_jobs: Vec::new(),
            hires_worker_threshold: HIRES_WORKER_THRESHOLD,
            hires_pool: Vec::new(),
            hires_steps: None,
            hires_collecting: false,
            hires_setup: None,
            hires_pending: Vec::new(),
            hires_patch: Vec::new(),
            hires_patch_w: 0,
            hires_patch_h: 0,
            hires_src_x: 0,
            hires_src_y: 0,
            texture_pack: None,
            texture_enhance: false,
        }
    }

    /// Back to power-on state: DMA, XBUS, PBUS, CEL engine, matrix, FIFOs,
    /// PLUT, inherited CCB registers, counters and pending requests. The
    /// bring-up record of which registers were written survives, and so does
    /// the internal scale (front-end configuration); its surfaces do not.
    pub fn reset(&mut self) {
        let written_value = std::mem::take(&mut self.written_value);
        let written_flag = std::mem::take(&mut self.written_flag);
        let scale = self.hires.scale();
        let texture_pack = self.texture_pack.take();
        let texture_enhance = self.texture_enhance;
        *self = Self {
            written_value,
            written_flag,
            ..Self::new()
        };
        self.hires.set_scale(scale);
        // The pack is front-end configuration like the scale: a reset is the
        // machine's, not the picture's.
        self.texture_enhance = texture_enhance;
        self.hires
            .set_pack_mode(texture_pack.is_some() || texture_enhance);
        self.texture_pack = texture_pack;
    }

    // --- internal resolution --------------------------------------------

    /// Rasterize cels at `scale` times native resolution as well (1 to
    /// [`crate::hires::MAX_INTERNAL_SCALE`]; 1 is off and costs nothing).
    /// Native VRAM output is unchanged at every scale.
    pub fn set_internal_scale(&mut self, scale: u32) {
        self.hires.set_scale(scale);
        self.hires_target = None;
    }

    /// Give the hi-res pass a directory of hand-authored cel replacements
    /// (`<fnv>.png` each, see [`crate::texpack`]); `None` clears it.
    /// Turn the built-in art enhancement on or off; see
    /// [`hires_pass::dedithered`]. Presentation only.
    pub fn set_texture_enhance(&mut self, enhance: bool) {
        self.texture_enhance = enhance;
        self.hires.set_pack_mode(enhance || self.texture_pack.is_some());
    }

    pub fn texture_enhance(&self) -> bool {
        self.texture_enhance
    }

    pub fn set_texture_dir(&mut self, dir: Option<&std::path::Path>) {
        self.hires
            .set_pack_mode(dir.is_some() || self.texture_enhance);
        self.texture_pack = dir.map(|d| {
            let pack = std::sync::Arc::new(crate::texpack::TexturePack::load(d));
            // Decoding ahead of play keeps a texture's first use off the
            // emulation thread's field budget.
            pack.warm();
            pack
        });
    }

    /// `(indexed files, replacements served, misses)` when a texture pack
    /// is installed — the only observable proof its fingerprints match.
    pub fn texture_pack_stats(&self) -> Option<(usize, usize, usize)> {
        self.texture_pack.as_ref().map(|p| p.stats())
    }

    pub fn internal_scale(&self) -> u32 {
        self.hires.scale()
    }

    pub fn hires(&self) -> &HiResStore {
        &self.hires
    }

    pub fn hires_mut(&mut self) -> &mut HiResStore {
        &mut self.hires
    }

    /// Find (or make) the surface the next list draws into, up to date.
    fn prepare_hires(&mut self, mem: &Memory) {
        self.hires_target = None;
        self.hires_unsynced = false;
        let d = &self.destination;
        if !self.hires.enabled() || !d.configured || !in_window(d.write_base, VRAM_BASE, VRAM_SIZE)
        {
            return;
        }
        let Ok(stride) = u32::try_from(d.write_stride) else {
            return;
        };
        let bound = self
            .hires
            .target(d.write_base - VRAM_BASE, stride, d.clip_height, mem);
        self.hires_target = bound.map(|(index, _, _)| index);
        if let Some(((_, origin_x, origin_y), surface)) =
            bound.zip(self.hires_target.and_then(|i| self.hires.surface(i)))
        {
            // The destination is what the cels draw into: a window's own size,
            // measured from where it starts in the buffer.
            self.hires_geometry = (
                surface.width().saturating_sub(origin_x),
                surface.height().saturating_sub(origin_y),
                surface.scale(),
            );
        }
    }

    /// Walk the list to its end, with the hi-res pass alongside when there is
    /// a target: in place for a small list, on a helper thread for a large one
    /// (see `hires_pass`). Either way the surface ends up the same.
    fn walk(&mut self, mem: &mut Memory) {
        let enhance = self.texture_enhance;
        let lent = self
            .hires_target
            .and_then(|i| self.hires.take_surface(i).map(|s| (i, s)));
        let Some((index, surface)) = lent else {
            while self.step_cel_walk(mem) {}
            return;
        };
        self.hires_jobs.clear();
        let threshold = self.hires_worker_threshold;
        let returned = std::thread::scope(|scope| {
            let mut surface = Some(surface);
            let mut worker = None;
            let backlog_counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (spent_tx, spent_rx) = std::sync::mpsc::sync_channel::<Vec<Job>>(HIRES_POOL);
            loop {
                let more = self.step_cel_walk(mem);
                if worker.is_none() && self.hires_jobs.len() >= threshold {
                    if let Some(lent) = surface.take() {
                        let (batch_tx, batch_rx) =
                            std::sync::mpsc::sync_channel::<Vec<Job>>(HIRES_POOL);
                        let spent = spent_tx.clone();
                        let backlog = backlog_counter.clone();
                        let pack = self.texture_pack.clone();
                        worker = Some((
                            batch_tx,
                            scope.spawn(move || run_worker(lent, batch_rx, spent, backlog, pack, enhance)),
                        ));
                    }
                }
                if let Some((batches, _)) = &worker {
                    // Whole batches only: a hand-off per cel would wake the
                    // helper thousands of times a field.
                    if self.hires_jobs.len() >= HIRES_BATCH
                        || (!more && !self.hires_jobs.is_empty())
                    {
                        let spare = self
                            .hires_pool
                            .pop()
                            .or_else(|| spent_rx.try_recv().ok())
                            .unwrap_or_else(|| Vec::with_capacity(HIRES_BATCH * 2));
                        // A helper that died takes the hi-res result with it;
                        // the native walk carries on regardless.
                        backlog_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let _ = batches.send(std::mem::replace(&mut self.hires_jobs, spare));
                    }
                }
                if !more {
                    break;
                }
            }
            match worker {
                Some((batches, helper)) => {
                    drop(batches);
                    let surface = helper.join().ok();
                    let room = HIRES_POOL.saturating_sub(self.hires_pool.len());
                    self.hires_pool.extend(spent_rx.try_iter().take(room));
                    surface
                }
                None => surface.map(|mut surface| {
                    let mut applier =
                        Applier::with_pack(self.texture_pack.clone(), self.texture_enhance);
                    for job in &self.hires_jobs {
                        applier.apply(&mut surface, job, false);
                    }
                    surface
                }),
            }
        });
        self.hires_jobs.clear();
        match returned {
            Some(surface) => self.hires.put_surface(index, surface),
            None => {
                self.hires.clear();
                self.hires_target = None;
            }
        }
    }

    /// How many recorded hi-res jobs move a walk's hi-res pass to a helper
    /// thread (0: always, `usize::MAX`: never). For tests and measurements;
    /// the result is the same either way.
    #[doc(hidden)]
    pub fn set_hires_worker_threshold(&mut self, jobs: usize) {
        self.hires_worker_threshold = jobs;
    }

    /// After a list. The surface was up to date when it began, and since then
    /// only the engine has written memory (the CPU is stopped while it owns
    /// the bus), drawing every pixel it wrote into the target into the mirror
    /// too. So the shadow is simply VRAM again and the dirtied pages need no
    /// re-check, which matters for titles that start the engine hundreds of
    /// times a field. A list that wrote past the buffer's edge (a clip wider
    /// than its stride) wrote pixels the mirror never saw; that surface is
    /// left for the next reconcile to sort out.
    fn finish_hires(&mut self, mem: &Memory) {
        if let Some(surface) = self
            .hires_target
            .take()
            .and_then(|i| self.hires.surface_mut(i))
        {
            if !self.hires_unsynced {
                surface.adopt_vram(mem);
            }
        }
    }

    // --- registers ------------------------------------------------------

    pub fn read(&self, offset: u32) -> u32 {
        let offset = offset & (MADAM_WINDOW_SIZE - 1);
        if let Some((channel, is_length)) = dma_slot(offset) {
            let table = if is_length {
                &self.dma_length
            } else {
                &self.dma_address
            };
            return table.get(channel).copied().unwrap_or(0);
        }
        if let Some(value) = self.matrix.read(offset) {
            return value;
        }
        if let Some(value) = self.read_fifo_register(offset) {
            return value;
        }
        match offset {
            MADAM_REVISION => MADAM_REVISION_GREEN,
            MADAM_MEM_CONFIG => self.mem_config,
            MADAM_DMA_ENABLE => self.dma_enable,
            MADAM_CEL_STATUS => match self.cel_state {
                CelEngineState::Idle => 0,
                CelEngineState::InProcess => MADAM_CEL_RUNNING,
                CelEngineState::Suspended => MADAM_CEL_RUNNING | MADAM_CEL_PAUSED,
            },
            MADAM_XBUS_DMA_ADDRESS => self.xbus_dma_address,
            MADAM_XBUS_DMA_LENGTH => self.xbus_dma_length,
            MADAM_VDL_ADDRESS => self.vdl_address,
            MADAM_PBUS_ADDRESS => self.pbus_address,
            MADAM_PBUS_LENGTH => self.pbus_length,
            MADAM_PBUS_POINTER => self.pbus_pointer,
            MADAM_CCB_CTL0 => self.ccb_ctl0,
            MADAM_REG_CTL0 => self.destination.reg_ctl0,
            MADAM_REG_CTL1 => self.destination.reg_ctl1,
            MADAM_REG_CTL2 => self.destination.read_base,
            MADAM_REG_CTL3 => self.destination.write_base,
            MADAM_CURRENT_CCB => self.current_ccb,
            MADAM_NEXT_CCB => self.next_ccb,
            _ => 0,
        }
    }

    /// A store to a MADAM register. `mem` is unused by the registers
    /// themselves but keeps the signature uniform with the other chips.
    pub fn write(&mut self, offset: u32, value: u32, _mem: &mut Memory) {
        let offset = offset & (MADAM_WINDOW_SIZE - 1);
        self.note_write(offset, value);

        if self.matrix.write(offset, value) {
            return;
        }
        if let Some((channel, is_length)) = dma_slot(offset) {
            let table = if is_length {
                &mut self.dma_length
            } else {
                &mut self.dma_address
            };
            if let Some(slot) = table.get_mut(channel) {
                *slot = value;
            }
            return;
        }
        if self.write_fifo_register(offset, value) {
            return;
        }

        match offset {
            // Read-only: chip identification, fitted memory (the boot ROM
            // writes zero here; honouring it makes the machine find no DRAM),
            // and the engine status.
            MADAM_REVISION | MADAM_MEM_CONFIG | MADAM_CEL_STATUS => {}
            MADAM_DMA_ENABLE => {
                self.dma_enable = value;
                // The scan runs inside the store that asks for it: the OS
                // starts it and blocks on the interrupt it raises.
                if value & MADAM_PBUS_START != 0 {
                    self.pbus_request = true;
                }
            }
            MADAM_XBUS_DMA_ADDRESS => self.xbus_dma_address = value,
            MADAM_XBUS_DMA_LENGTH => {
                self.xbus_dma_length = value;
                self.xbus_dma_pending = true;
            }
            MADAM_VDL_ADDRESS => self.vdl_address = value,
            MADAM_PBUS_ADDRESS => self.pbus_address = value,
            MADAM_PBUS_LENGTH => self.pbus_length = value,
            MADAM_PBUS_POINTER => self.pbus_pointer = value,

            // Starting arms the engine; it runs once the CPU's current
            // instruction is finished, so software may finish writing the CCB
            // the engine is about to read.
            MADAM_CEL_START => {
                if self.cel_state == CelEngineState::Idle {
                    self.cel_state = CelEngineState::InProcess;
                    self.cel_walk_started = false;
                    self.cel_pending = true;
                }
            }
            MADAM_CEL_STOP => {
                self.cel_state = CelEngineState::Idle;
                self.cel_walk_started = false;
                self.cel_pending = false;
                self.next_ccb = 0;
            }
            MADAM_CEL_RESUME => {
                if self.cel_state == CelEngineState::Suspended {
                    self.cel_state = CelEngineState::InProcess;
                    self.cel_pending = true;
                }
            }
            MADAM_CEL_PAUSE => {
                if self.cel_state == CelEngineState::InProcess {
                    self.cel_state = CelEngineState::Suspended;
                    self.cel_pending = false;
                }
            }

            MADAM_CCB_CTL0 => self.ccb_ctl0 = value,
            MADAM_REG_CTL0 => {
                let d = &mut self.destination;
                d.reg_ctl0 = value;
                d.read_stride = decode_stride(value);
                d.write_stride = decode_stride(value >> 8);
                d.configured = true;
            }
            MADAM_REG_CTL1 => {
                let d = &mut self.destination;
                d.reg_ctl1 = value;
                // Inclusive: the last pixel drawn, not the first dropped.
                d.clip_width = (value & 0x3ff) + 1;
                d.clip_height = ((value >> 16) & 0x3ff) + 1;
            }
            MADAM_REG_CTL2 => {
                self.destination.read_base = value;
                self.destination.configured = true;
            }
            MADAM_REG_CTL3 => {
                self.destination.write_base = value;
                self.destination.configured = true;
            }
            MADAM_CURRENT_CCB => self.current_ccb = value,
            MADAM_NEXT_CCB => self.next_ccb = value,
            _ => {}
        }
    }

    fn note_write(&mut self, offset: u32, value: u32) {
        let index = (offset / 4) as usize;
        if let Some(slot) = self.written_value.get_mut(index) {
            *slot = value;
        }
        if let Some(flag) = self.written_flag.get_mut(index) {
            *flag = true;
        }
    }

    /// Whether software ever wrote this low register (bring-up diagnostic).
    pub fn register_written(&self, offset: u32) -> bool {
        self.written_flag
            .get((offset / 4) as usize)
            .copied()
            .unwrap_or(false)
    }

    /// The last value written to this low register.
    pub fn register_last_write(&self, offset: u32) -> u32 {
        self.written_value
            .get((offset / 4) as usize)
            .copied()
            .unwrap_or(0)
    }

    // --- destination ----------------------------------------------------

    /// The rectangle cels are clipped to.
    pub fn set_clip(&mut self, width: u32, height: u32) {
        self.destination.clip_width = width;
        self.destination.clip_height = height;
    }

    pub fn clip_width(&self) -> u32 {
        self.destination.clip_width
    }

    pub fn clip_height(&self) -> u32 {
        self.destination.clip_height
    }

    /// Where cels are stored before software programs the framebuffer
    /// registers. The plain-store path lays pixels out with the clip width as
    /// the row width; `stride_bytes` is recorded only.
    pub fn set_target(&mut self, address: u32, stride_bytes: u32) {
        self.destination.target_address = address;
        self.destination.target_stride_bytes = stride_bytes;
    }

    // --- PBUS -----------------------------------------------------------

    /// A controller scan was requested by the last DMA-enable store. The
    /// machine serves it (and calls [`Madam::finish_pbus`]) before the next
    /// instruction.
    pub fn take_pbus_request(&mut self) -> bool {
        std::mem::take(&mut self.pbus_request)
    }

    pub fn pbus_address(&self) -> u32 {
        self.pbus_address
    }
    pub fn pbus_length(&self) -> u32 {
        self.pbus_length
    }
    pub fn set_pbus_address(&mut self, value: u32) {
        self.pbus_address = value;
    }
    pub fn set_pbus_length(&mut self, value: u32) {
        self.pbus_length = value;
    }
    pub fn advance_pbus_pointer(&mut self) {
        self.pbus_pointer = self.pbus_pointer.wrapping_add(4);
    }
    /// The hardware clears the start bit when the scan is done.
    pub fn finish_pbus(&mut self) {
        self.dma_enable &= !MADAM_PBUS_START;
    }

    // --- DSP DMA channels (the rest is in `fifo`) -----------------------

    /// The channel enable mask, which software sets through CLIO.
    pub fn set_dma_channel_enable(&mut self, mask: u32) {
        self.dma_channel_enable = mask;
    }
    pub fn dma_channel_enable(&self) -> u32 {
        self.dma_channel_enable
    }

    /// CLIO bank-0 interrupt bits raised since the last call:
    /// `1 << (channel + 16)` when an input channel finishes a buffer and
    /// `1 << (channel + 12)` for an output channel.
    pub fn take_interrupts(&mut self) -> u32 {
        std::mem::take(&mut self.interrupts)
    }

    // --- XBUS, VDL and the general DMA channels -------------------------

    /// An expansion-bus DMA programmed and not yet served.
    pub fn xbus_dma_pending(&self) -> bool {
        self.xbus_dma_pending
    }
    pub fn xbus_dma_address(&self) -> u32 {
        self.xbus_dma_address
    }
    /// The length register holds bytes minus four.
    pub fn xbus_dma_bytes(&self) -> u32 {
        self.xbus_dma_length.wrapping_add(4)
    }
    pub fn clear_xbus_dma(&mut self) {
        self.xbus_dma_pending = false;
    }

    /// Where the display list lives; zero until software sets it.
    pub fn vdl_address(&self) -> u32 {
        self.vdl_address
    }

    pub fn dma_address(&self, channel: u32) -> u32 {
        self.dma_address.get(channel as usize).copied().unwrap_or(0)
    }
    pub fn dma_length(&self, channel: u32) -> u32 {
        self.dma_length.get(channel as usize).copied().unwrap_or(0)
    }

    // --- statistics -----------------------------------------------------

    /// The last list only.
    pub fn stats(&self) -> &MadamStats {
        &self.stats
    }
    pub fn engine_runs(&self) -> u64 {
        self.engine_runs
    }
    pub fn total_cels_drawn(&self) -> u64 {
        self.total_cels_drawn
    }

    // --- the CEL engine -------------------------------------------------

    /// START (or CONTINUE) has armed the engine and it has not run yet.
    pub fn cel_engine_pending(&self) -> bool {
        self.cel_pending
    }

    /// Run an armed engine: walk from NEXTCCB to the end of the list while
    /// the CEL owns the data bus. The machine calls this at the CPU
    /// instruction boundary after the START store.
    pub fn run_cel_engine(&mut self, mem: &mut Memory) {
        self.cel_pending = false;
        if self.cel_state != CelEngineState::InProcess {
            return;
        }
        if !self.cel_walk_started {
            self.begin_cel_walk(self.next_ccb);
        }
        self.prepare_hires(mem);
        self.walk(mem);
        self.finish_hires(mem);
        self.cel_state = CelEngineState::Idle;
        self.cel_walk_started = false;
    }

    /// Walk a complete list from `address` at once, outside the register
    /// protocol (for tests and tools).
    pub fn render_cel_list(&mut self, address: u32, mem: &mut Memory) {
        self.begin_cel_walk(address);
        self.prepare_hires(mem);
        self.walk(mem);
        self.finish_hires(mem);
        self.cel_walk_started = false;
    }

    /// Fetch one CCB as the engine would, updating the inherited registers.
    pub fn read_ccb(&mut self, address: u32, mem: &Memory) -> Ccb {
        self.registers.load(address, mem)
    }

    /// Whether `ccb` would be rejected before drawing, against the current clip.
    pub fn cel_is_invisible(&self, ccb: &Ccb, packed: bool) -> bool {
        cel_is_invisible(
            ccb,
            packed,
            self.destination.clip_width,
            self.destination.clip_height,
        )
    }

    fn begin_cel_walk(&mut self, address: u32) {
        self.stats = MadamStats::default();
        self.engine_runs += 1;
        self.cel_walk_started = true;
        self.next_ccb = address;
    }

    /// Consume one CCB; `false` when the list has ended.
    fn step_cel_walk(&mut self, mem: &mut Memory) -> bool {
        if self.next_ccb == 0 {
            return false;
        }
        if self.stats.cels_walked >= MAX_CELS {
            self.stats.list_truncated = 1;
            return false;
        }

        let address = self.next_ccb;
        let ccb = self.read_ccb(address, mem);
        self.current_ccb = ccb.fetch_end_address;
        self.stats.cels_walked += 1;
        self.next_ccb = ccb.next_address;

        // SKIP suppresses projection, not loading: a skipped CCB still sets
        // up the decoder, processor and PLUT for a compact follower.
        if ccb.is_drawable() {
            self.begin_cel(&ccb, mem);
        }
        if !ccb.has(CCB_SKIP) {
            let before = self.stats.cels_drawn;
            self.draw_cel(&ccb, mem);
            self.total_cels_drawn += u64::from(self.stats.cels_drawn - before);
        }

        if ccb.has(CCB_LAST) || self.next_ccb == 0 || self.next_ccb == address {
            return false;
        }
        if self.stats.cels_walked >= MAX_CELS {
            self.stats.list_truncated = 1;
            return false;
        }
        true
    }

    fn begin_cel(&mut self, ccb: &Ccb, mem: &Memory) {
        self.cel.decoder.begin(ccb, self.ccb_ctl0, mem);
        self.cel.processor.begin(ccb, self.ccb_ctl0);
        self.cel.mapper = Mapper::for_cel(ccb);
        self.cel.bpp = ccb.format.bits_per_pixel();
    }

    fn draw_cel(&mut self, ccb: &Ccb, mem: &mut Memory) {
        if !ccb.is_drawable() || self.cel_is_invisible(ccb, ccb.is_packed()) {
            return;
        }
        // A cel drawn backwards along an axis is moved back half a pixel on
        // that axis, so truncation lands on the pixel the hardware means and a
        // mirrored cel sits where its unmirrored twin does. Only the
        // axis-aligned mappers need it.
        let mut placed = *ccb;
        if ccb.is_axis_aligned() {
            if ccb.hdx < 0 || ccb.vdx < 0 {
                placed.x = placed.x.wrapping_sub(0x8000);
            }
            if ccb.hdy < 0 || ccb.vdy < 0 {
                placed.y = placed.y.wrapping_sub(0x8000);
            }
        }
        if self.hires_target.is_some() {
            let d = &self.destination;
            let (width, height, k) = self.hires_geometry;
            let (low_reads, high_reads) = self.cel.processor.frame_reading_halves();
            let cel = HiResCel {
                scale: k,
                clip: Clip {
                    width: (d.clip_width.min(width) * k) as i32,
                    height: (d.clip_height.min(height) * k) as i32,
                },
                ignores_frame: !(low_reads || high_reads),
                mixed_frame: low_reads != high_reads,
            };
            self.hires_cel = cel;
            // Every cel is set up, frame-reading or not: one that blends some
            // texels against the frame still draws the rest frame-blind, and
            // those texels go through the filter and the texture pack.
            // Held back until the cel's texels are collected: the patch
            // inside travels with the Job::Cel sent at the cel's end.
            self.hires_setup = Some(CelSetup {
                processor: self.cel.processor,
                clip: cel.clip,
                scale: k,
                flags: ccb.flags,
                reads_frame: !cel.ignores_frame,
                one_to_one: self.cel.mapper == Mapper::Line,
                patch: None,
            });
            self.hires_steps = None;
            self.hires_pending.clear();
            self.hires_collecting = true;
            self.hires_src_x = 0;
            self.hires_src_y = 0;
            self.hires_patch_w = 0;
            self.hires_patch_h = 0;
            self.hires_patch.clear();
        }
        if ccb.is_packed() {
            self.draw_packed_cel(&placed, mem);
        } else if ccb.is_lr_form() && self.cel.bpp == 16 {
            self.draw_lr_cel(&placed, mem);
        } else {
            self.draw_unpacked_cel(&placed, mem);
        }
        if let Some(mut setup) = self.hires_setup.take() {
            if self.hires_collecting {
                // The raw image rides along; the applying thread upsamples it.
                if self.hires_patch_w > 0 && self.hires_patch_h > 0 {
                    setup.patch = Some(std::sync::Arc::new(RawPatch {
                        width: self.hires_patch_w,
                        height: self.hires_patch_h,
                        texels: std::mem::take(&mut self.hires_patch),
                    }));
                }
                self.hires_collecting = false;
            }
            self.hires_jobs.push(Job::Cel(Box::new(setup)));
            self.hires_jobs.append(&mut self.hires_pending);
        }
    }

    /// A packed cel's rows are packet streams, each opening with its length
    /// in words (excluding itself and one more): one byte below 8 bpp, two
    /// at 8 bpp and above.
    fn draw_packed_cel(&mut self, ccb: &Ccb, mem: &mut Memory) {
        let bpp = ccb.format.bits_per_pixel();
        if bpp == 0 || ccb.height == 0 || ccb.height > MAX_CEL_DIMENSION {
            return;
        }
        let length_bits = if bpp < 8 { 8 } else { 16 };
        let mut row_data = ccb.source_address;
        let mut rows = RowWalker::new(ccb);
        self.hires_begin_patch(ccb.width, ccb.height);

        for _ in 0..ccb.height {
            let mut reader = BitReader::new(row_data);
            let length = reader.read(mem, length_bits);
            let row_end = row_data.wrapping_add((length + 2) << 2);
            let mut texel = rows.columns();

            loop {
                match Packet::read(&mut reader, mem, row_end) {
                    Packet::End => break,
                    Packet::Transparent(count) => {
                        self.hires_skip(count);
                        texel.advance_by(count);
                    }
                    Packet::Repeat(count) => {
                        let value = self.cel.decoder.decode(reader.read(mem, bpp));
                        for _ in 0..count {
                            self.plot(ccb, &texel, value, mem);
                            texel.advance();
                        }
                    }
                    Packet::Literal(count) => {
                        for _ in 0..count {
                            let value = self.cel.decoder.decode(reader.read(mem, bpp));
                            self.plot(ccb, &texel, value, mem);
                            texel.advance();
                        }
                    }
                }
            }

            row_data = row_end;
            rows.next_row();
            self.hires_next_row();
        }
        self.stats.cels_drawn += 1;
    }

    /// An unpacked cel: rows a fixed number of WORDS apart, the stride taken
    /// from PRE1 (eight bits below 8 bpp, ten above), with SKIPX pixels
    /// dropped from the front of each row.
    fn draw_unpacked_cel(&mut self, ccb: &Ccb, mem: &mut Memory) {
        let bpp = ccb.format.bits_per_pixel();
        if bpp == 0 || ccb.height == 0 || ccb.height > MAX_CEL_DIMENSION {
            return;
        }
        let stride_words = if bpp < 8 {
            (ccb.pre1 >> 24) & 0xff
        } else {
            (ccb.pre1 >> 16) & 0x3ff
        };
        let row_bytes = (stride_words + 2) << 2;
        let skip = ccb.skip_x();
        if skip >= ccb.width {
            return;
        }
        let width = ccb.width - skip;
        let mut rows = RowWalker::new(ccb);
        self.hires_begin_patch(width, ccb.height);

        let mut row_data = ccb.source_address;
        for _ in 0..ccb.height {
            let mut reader = BitReader::new(row_data);
            reader.skip(bpp * skip);
            let mut texel = rows.columns();
            for _ in 0..width {
                let value = self.cel.decoder.decode(reader.read(mem, bpp));
                self.plot(ccb, &texel, value, mem);
                texel.advance();
            }
            row_data = row_data.wrapping_add(row_bytes);
            rows.next_row();
            self.hires_next_row();
        }
        self.stats.cels_drawn += 1;
    }

    /// An LR-form cel stores its source the way the framebuffer is stored:
    /// VCNT counts row PAIRS, and each 32-bit word is [row 2n pixel x, row
    /// 2n+1 pixel x]. A title can hand the engine a buffer it has just
    /// rendered or decoded with no rearranging.
    fn draw_lr_cel(&mut self, ccb: &Ccb, mem: &mut Memory) {
        let stride_words = (ccb.pre1 >> 16) & 0x3ff;
        let pair_stride = (stride_words + 2) * 4;
        let pairs = ccb.vertical_count();
        if pairs * 2 > MAX_CEL_DIMENSION {
            return;
        }
        let mut rows = RowWalker::new(ccb);
        let mut pair_base = ccb.source_address;
        self.hires_begin_patch(ccb.width, pairs * 2);
        for _ in 0..pairs {
            for half in 0..2u32 {
                let mut texel = rows.columns();
                let mut word = pair_base.wrapping_add(half * 2);
                for _ in 0..ccb.width {
                    let value = self.cel.decoder.decode(u32::from(mem.read16(word)));
                    self.plot(ccb, &texel, value, mem);
                    texel.advance();
                    word = word.wrapping_add(4);
                }
                rows.next_row();
                self.hires_next_row();
            }
            pair_base = pair_base.wrapping_add(pair_stride);
        }
        self.stats.cels_drawn += 1;
    }

    /// Size and clear the patch the current cel's decoded texels collect
    /// into, at the dimensions the draw loop about to run will fill.
    fn hires_begin_patch(&mut self, width: u32, height: u32) {
        if !self.hires_collecting {
            return;
        }
        self.hires_patch_w = width;
        self.hires_patch_h = height;
        self.hires_patch.clear();
        let cell = Decoded {
            transparent: true,
            ..Decoded::default()
        };
        self.hires_patch
            .resize(width as usize * height as usize, cell);
    }

    /// Source positions a packet skipped: the patch cells stay transparent.
    fn hires_skip(&mut self, count: u32) {
        self.hires_src_x = self.hires_src_x.saturating_add(count);
    }

    /// A source row finished: the next texel is the next row's first.
    fn hires_next_row(&mut self) {
        self.hires_src_x = 0;
        self.hires_src_y = self.hires_src_y.saturating_add(1);
    }

    /// Where a hi-res job goes: buffered behind its cel's upcoming patch
    /// job, or straight into the stream when this cel is not collected.
    fn emit_job(&mut self, job: Job) {
        if self.hires_collecting {
            self.hires_pending.push(job);
        } else {
            self.hires_jobs.push(job);
        }
    }

    /// One decoded texel onto the destination, through the cel's mapper.
    fn plot(&mut self, ccb: &Ccb, texel: &Texel, value: Decoded, mem: &mut Memory) {
        let src = [self.hires_src_x as u16, self.hires_src_y as u16];
        if self.hires_collecting {
            let index =
                self.hires_src_y as usize * self.hires_patch_w as usize + self.hires_src_x as usize;
            if let Some(slot) = self.hires_patch.get_mut(index) {
                *slot = value;
            }
        }
        self.hires_src_x = self.hires_src_x.saturating_add(1);
        if value.transparent {
            return;
        }
        match self.cel.mapper {
            Mapper::Line => {
                if self.hires_target.is_some() {
                    self.plot_hires(ccb, texel, value, mem, src);
                }
                self.put_pixel(texel.x >> 16, texel.y >> 16, value, mem);
            }
            Mapper::Scale => {
                if self.hires_target.is_some() {
                    self.plot_hires(ccb, texel, value, mem, src);
                }
                let clip = self.destination.clip();
                let area = scaled_footprint(texel, ccb.vdx, ccb.vdy, ccb.has(CCB_MARIA), clip);
                for y in area.y0..area.y1 {
                    for x in area.x0..area.x1 {
                        self.put_pixel(x, y, value, mem);
                    }
                }
            }
            Mapper::Arbitrary => {
                if self.hires_target.is_some() {
                    self.plot_hires(ccb, texel, value, mem, src);
                }
                let clip = self.destination.clip();
                scan_quad(texel.corners(), ccb.flags, clip, |y, from, to| {
                    for x in from..to {
                        self.put_pixel(x, y, value, mem);
                    }
                });
            }
        }
    }

    fn put_pixel(&mut self, x: i32, y: i32, value: Decoded, mem: &mut Memory) {
        let d = &self.destination;
        if d.configured {
            self.process_pixel(x, y, value, mem);
            return;
        }
        // Before software programs the framebuffer registers (only ever a
        // test) pixels are stored plainly, in the display's own layout.
        if value.color == 0 || !d.contains(x, y) {
            return;
        }
        let address =
            d.target_address
                .wrapping_add(framebuffer_offset(x as u32, y as u32, d.clip_width));
        mem.write16(address, value.color);
        self.stats.pixels_written += 1;
    }

    /// Through the pixel processor: read the frame pixel beneath, blend,
    /// store.
    fn process_pixel(&mut self, x: i32, y: i32, value: Decoded, mem: &mut Memory) {
        let d = &self.destination;
        if !d.contains(x, y) {
            return;
        }
        let read_at = d.read_base.wrapping_add(pixel_offset(x, y, d.read_stride));
        let write_at = d
            .write_base
            .wrapping_add(pixel_offset(x, y, d.write_stride));
        let frame = read_be16_bytes(mem, read_at);
        let out = self
            .cel
            .processor
            .blend(value.color, value.amv, frame, value.raw_vh);
        let [high, low] = out.to_be_bytes();
        mem.write8(write_at, high);
        mem.write8(write_at.wrapping_add(1), low);
        self.stats.pixels_written += 1;

        if self.hires_target.is_some() {
            // Contained, so non-negative.
            let (x, y) = (x as u32, y as u32);
            let (width, height, _) = self.hires_geometry;
            if x >= width || y >= height {
                self.hires_unsynced = true;
            }
        }
    }

    /// Rasterize one texel into the hi-res surface: the same 16.16 corners
    /// on a grid `k` times finer, so its edges land on 1/`k` of a pixel.
    ///
    /// A texel whose pixel-processor half reads the frame carries the frame
    /// pixel it blends against, read from VRAM at the texel's own native
    /// pixel BEFORE the native write of the same texel, so a cel that reads
    /// the buffer it draws into is not blended twice. The helper then blends
    /// the filtered — or texture-pack — colour against that, which is what
    /// lets a pack reach a sprite the game draws translucently.
    fn plot_hires(
        &mut self,
        ccb: &Ccb,
        texel: &Texel,
        value: Decoded,
        mem: &Memory,
        src: [u16; 2],
    ) {
        let HiResCel {
            scale: k,
            clip,
            ignores_frame,
            mixed_frame,
        } = self.hires_cel;
        let frame = if ignores_frame
            || (mixed_frame && !self.cel.processor.texel_reads_frame(value.color))
        {
            0
        } else {
            let d = &self.destination;
            read_be16_bytes(
                mem,
                d.read_base
                    .wrapping_add(pixel_offset(texel.x >> 16, texel.y >> 16, d.read_stride)),
            )
        };
        let colour = Colour::of(value, frame);
        let job = match self.cel.mapper {
            // One to one: the texel owns exactly its pixel's block, which is
            // a rectangle like any other — and a rectangle the applier can
            // fill from pack artwork instead of one flat colour. Its own
            // corners are the block, with no stepping to follow.
            Mapper::Line => {
                let ki = k as i32;
                let x0 = (texel.x >> 16) * ki;
                let y0 = (texel.y >> 16) * ki;
                Job::Rect {
                    area: Rect {
                        x0: x0.max(0),
                        y0: y0.max(0),
                        x1: (x0 + ki).min(clip.width),
                        y1: (y0 + ki).min(clip.height),
                    },
                    src,
                    colour,
                }
            }
            Mapper::Scale => Job::Rect {
                area: scaled_footprint_at(texel, ccb.vdx, ccb.vdy, ccb.has(CCB_MARIA), clip, k),
                src,
                colour,
            },
            Mapper::Arbitrary => {
                let steps = [
                    texel.step_x,
                    texel.step_y,
                    texel.next_step_x,
                    texel.next_step_y,
                ];
                if self.hires_steps != Some(steps) {
                    self.hires_steps = Some(steps);
                    self.emit_job(Job::Steps(steps));
                }
                Job::Quad {
                    at: [texel.x, texel.y, texel.down_x, texel.down_y],
                    src,
                    colour,
                }
            }
        };
        self.emit_job(job);
    }
}

/// Which general DMA channel an offset names, and whether it is the length.
fn dma_slot(offset: u32) -> Option<(usize, bool)> {
    let relative = offset.checked_sub(MADAM_DMA_BASE)?;
    if relative >= MADAM_DMA_CHANNELS * MADAM_DMA_STRIDE {
        return None;
    }
    Some((
        (relative / MADAM_DMA_STRIDE) as usize,
        relative % MADAM_DMA_STRIDE >= 4,
    ))
}
