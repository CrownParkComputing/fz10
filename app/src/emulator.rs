//! The core as an application holds it: the machine, the thread it runs on and
//! the one pacing policy.
//!
//! Emulation runs on its own thread and nowhere else. It publishes finished
//! frames into a mailbox and pushes audio into a ring; the display takes the
//! newest frame whenever it looks, and neither side waits for the other.
//!
//! Pacing, stated once:
//! - pace to the field rate against a monotonic deadline;
//! - an overrun lets the deadline slip rather than sprinting to catch up (a
//!   burst starves audio and overruns again);
//! - more than four frames behind restarts the clock instead of accumulating
//!   debt that can never be paid;
//! - always sleep to the deadline, never spin: a handheld at 100% of a core
//!   gets hot, and a hot handheld is slower.
//!
//! Calls that only touch the pads, the audio ring, the mailbox, the stats, the
//! pause/reset flags or the CPU scale never wait for the machine. Everything
//! else takes the machine's lock, which the thread holds only while it is
//! emulating a field.
#![forbid(unsafe_code)]

use std::fs::File;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::audio_ring::AudioRing;
use crate::console::{Console, Region};
use crate::frame_mailbox::FrameMailbox;
use crate::pad::PadState;
use crate::scaler::{self, VideoFilter};

/// How the machine is keeping up.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EmulatorStats {
    pub frames_emulated: u64,
    /// Fields where the machine missed its deadline.
    pub frames_behind: u64,
    /// Times it fell so far behind that the clock was restarted.
    pub deadline_resets: u64,
    /// What the machine actually manages, against 60 or 50. Reported apart from
    /// the display's rate: a half-speed machine behind a smooth window is
    /// exactly the failure that must stay visible.
    pub emulated_fps: f64,
    /// Time to emulate one field, smoothed.
    pub frame_ms: f64,
}

/// What `retro3do_get_disc_info` reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscInfo {
    pub path: String,
    pub is_chd: bool,
    pub sector_count: u32,
    pub track_count: u32,
}

/// A float in an atomic word.
#[derive(Default)]
struct AtomicF64(AtomicU64);

impl AtomicF64 {
    fn store(&self, value: f64) {
        self.0.store(value.to_bits(), Ordering::Relaxed);
    }
    fn load(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }
}

struct Shared {
    console: Mutex<Console>,
    mailbox: FrameMailbox,
    pads: Arc<PadState>,
    audio: Arc<AudioRing>,
    cpu_scale_percent: Arc<AtomicU32>,

    should_stop: AtomicBool,
    paused: AtomicBool,
    reset_requested: AtomicBool,
    /// An NVRAM image taken at a frame boundary, waiting to be collected.
    nvram: Mutex<Option<Vec<u8>>>,

    frames_emulated: AtomicU64,
    frames_behind: AtomicU64,
    deadline_resets: AtomicU64,
    emulated_fps: AtomicF64,
    frame_ms: AtomicF64,
    /// Sampled after each field: live values for a stats overlay.
    cpu_pc: AtomicU32,
    expansion_dma: AtomicU64,
    /// Presentation filter + integer factor, packed: filter | scale << 8.
    video: AtomicU32,
    /// Internal cel resolution, applied by the emulator thread between fields.
    internal_scale: AtomicU32,
    /// The scale actually being drawn at: with [`Shared::adaptive_scale`] on,
    /// the thread lowers it below the asked-for one rather than let fields
    /// run late, and raises it again once they stop.
    effective_scale: AtomicU32,
    adaptive_scale: AtomicBool,
    /// Texture-pack directory the emulator thread should point the cel
    /// engine at, and a counter bumped whenever it changes.
    texture_dir: Mutex<Option<std::path::PathBuf>>,
    texture_gen: AtomicU64,
    /// Built-in art enhancement, applied by the emulator thread between
    /// fields; see [`Console::set_texture_enhance`].
    texture_enhance: AtomicBool,
}

impl Shared {
    fn console(&self) -> MutexGuard<'_, Console> {
        self.console.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Copy the finished field into the mailbox and note live diagnostics.
    fn publish(&self, console: &Console) {
        let frame = console.framebuffer();
        self.mailbox
            .publish_frame(frame.pixels, frame.width, frame.height);
        self.cpu_pc.store(console.cpu().pc(), Ordering::Relaxed);
        self.expansion_dma
            .store(console.expansion_dma_count(), Ordering::Relaxed);
    }

    /// Snapshot the NVRAM if the machine wrote to it. Almost always false.
    fn snapshot_nvram(&self, console: &mut Console) {
        if console.memory().nvram_dirty() {
            let image = console.nvram().to_vec();
            *self.nvram.lock().unwrap_or_else(PoisonError::into_inner) = Some(image);
            console.memory_mut().clear_nvram_dirty();
        }
    }

    fn run(&self) {
        let mut deadline = Instant::now();
        let mut smoothed_ms = 0.0f64;
        let mut window_start = Instant::now();
        let mut window_frames = 0u64;

        // Adaptive scale: a window of fields, how many of them missed their
        // deadline, and how many clean windows have passed since the last
        // drop (a scale costs more than it looks on one field, so going back
        // up waits for a settled picture).
        let mut asked_scale = self.internal_scale.load(Ordering::Acquire);
        let mut scale = asked_scale;
        let mut window_late = 0u32;
        let mut window_fields = 0u32;
        let mut clean_windows = 0u32;
        // How long to stay down before trying the higher scale again: each
        // failed attempt doubles the wait, so a machine that cannot hold the
        // asked-for scale stops probing for it every few seconds.
        let mut retry_windows = ADAPT_CLEAN_WINDOWS;

        let mut applied_texture_gen = 0u64;
        while !self.should_stop.load(Ordering::Acquire) {
            if self.paused.load(Ordering::Acquire) {
                // Idle, not spin.
                std::thread::sleep(Duration::from_millis(8));
                deadline = Instant::now();
                continue;
            }

            let started;
            let period = {
                let mut console = self.console();
                if self.reset_requested.swap(false, Ordering::AcqRel) {
                    console.reset();
                    deadline = Instant::now();
                }
                // The asked-for scale wins immediately when it changes; the
                // adaptive step only ever moves away from it afterwards.
                let asked = self.internal_scale.load(Ordering::Acquire);
                if asked != asked_scale {
                    asked_scale = asked;
                    scale = asked;
                    window_late = 0;
                    window_fields = 0;
                    clean_windows = 0;
                    retry_windows = ADAPT_CLEAN_WINDOWS;
                }
                console.set_internal_scale(scale);
                let enhance = self.texture_enhance.load(Ordering::Acquire);
                if enhance != console.texture_enhance() {
                    console.set_texture_enhance(enhance);
                }
                self.effective_scale.store(scale, Ordering::Release);
                let texture_gen = self.texture_gen.load(Ordering::Acquire);
                if texture_gen != applied_texture_gen {
                    applied_texture_gen = texture_gen;
                    let dir = self
                        .texture_dir
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone();
                    console.set_texture_dir(dir.as_deref());
                }
                // The frame-time figure covers the field only, not a reset
                // before it (EmulatorThread::run @0xa8fb0 reads the clock here).
                started = Instant::now();
                console.run_frame();
                self.publish(&console);
                self.snapshot_nvram(&mut console);
                field_period(console.region())
            };

            let finished = Instant::now();
            let this_ms = finished.duration_since(started).as_secs_f64() * 1000.0;
            smoothed_ms = if smoothed_ms == 0.0 {
                this_ms
            } else {
                smoothed_ms * 0.9 + this_ms * 0.1
            };
            self.frame_ms.store(smoothed_ms);
            self.frames_emulated.fetch_add(1, Ordering::Relaxed);
            window_frames += 1;
            let window = finished.duration_since(window_start);
            if window >= Duration::from_millis(500) {
                self.emulated_fps
                    .store(window_frames as f64 / window.as_secs_f64());
                window_start = finished;
                window_frames = 0;
            }

            // --- pacing: the only place a frame's timing is decided ---------
            deadline += period;
            let now = Instant::now();
            let late = now >= deadline;
            if !late {
                std::thread::sleep(deadline - now);
            } else {
                self.frames_behind.fetch_add(1, Ordering::Relaxed);
                if now.duration_since(deadline) > period * 4 {
                    deadline = now;
                    self.deadline_resets.fetch_add(1, Ordering::Relaxed);
                }
            }

            // --- adaptive scale: judder costs more than sharpness ----------
            window_fields += 1;
            window_late += u32::from(late);
            if window_fields >= ADAPT_WINDOW_FIELDS {
                if self.adaptive_scale.load(Ordering::Relaxed) {
                    if window_late * 100 >= window_fields * ADAPT_DROP_PERCENT && scale > 1 {
                        scale -= 1;
                        clean_windows = 0;
                        retry_windows = (retry_windows * 2).min(ADAPT_MAX_RETRY_WINDOWS);
                    } else if window_late == 0 && scale < asked_scale {
                        clean_windows += 1;
                        if clean_windows >= retry_windows {
                            scale += 1;
                            clean_windows = 0;
                        }
                    } else if window_late > 0 {
                        clean_windows = 0;
                    }
                }
                window_fields = 0;
                window_late = 0;
            }
        }
    }
}

/// Fields per decision: a second's worth, long enough that a game which only
/// redraws every second or third field is judged on a whole cycle of them
/// rather than on one heavy field.
const ADAPT_WINDOW_FIELDS: u32 = 60;
/// Late fields in a window, as a percentage, that force a step down. A
/// handful of late fields a second is not yet visible judder; a fifth of them
/// is.
const ADAPT_DROP_PERCENT: u32 = 20;
/// Windows with nothing late before stepping back up the first time.
const ADAPT_CLEAN_WINDOWS: u32 = 3;
/// The longest that wait grows to, so a machine that will never hold the
/// asked-for scale probes for it about once a minute instead of constantly.
const ADAPT_MAX_RETRY_WINDOWS: u32 = 48;

/// 50 or 60 fields a second, in nanoseconds so the deadline does not drift:
/// 0xfe502b and 20,000,000 in `EmulatorThread::run` (reference build 0xa8fb0).
fn field_period(region: Region) -> Duration {
    match region {
        Region::Pal => Duration::from_nanos(20_000_000),
        Region::Ntsc => Duration::from_nanos(16_666_667),
    }
}

/// An emulated machine with its own thread.
pub struct Retro3do {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    /// Scratch for the scaled presentation frame (dst, intermediate).
    scaled: Mutex<(Vec<u32>, Vec<u32>)>,
}

impl Default for Retro3do {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Retro3do {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Retro3do {
    /// A powered-off machine with no media.
    pub fn new() -> Self {
        let pads = Arc::new(PadState::new());
        let audio = Arc::new(AudioRing::new());
        let cpu_scale_percent = Arc::new(AtomicU32::new(100));
        let console = Console::with_shared(
            Arc::clone(&pads),
            Arc::clone(&audio),
            Arc::clone(&cpu_scale_percent),
        );
        Self {
            shared: Arc::new(Shared {
                console: Mutex::new(console),
                mailbox: FrameMailbox::new(),
                pads,
                audio,
                cpu_scale_percent,
                should_stop: AtomicBool::new(false),
                paused: AtomicBool::new(false),
                reset_requested: AtomicBool::new(false),
                nvram: Mutex::new(None),
                frames_emulated: AtomicU64::new(0),
                frames_behind: AtomicU64::new(0),
                deadline_resets: AtomicU64::new(0),
                emulated_fps: AtomicF64::default(),
                frame_ms: AtomicF64::default(),
                cpu_pc: AtomicU32::new(0),
                expansion_dma: AtomicU64::new(0),
                video: AtomicU32::new(0),
                internal_scale: AtomicU32::new(1),
                effective_scale: AtomicU32::new(1),
                adaptive_scale: AtomicBool::new(true),
                texture_dir: Mutex::new(None),
                texture_gen: AtomicU64::new(1),
                texture_enhance: AtomicBool::new(false),
            }),
            thread: None,
            scaled: Mutex::new((Vec::new(), Vec::new())),
        }
    }

    /// Lock the machine for direct work. Media, region and other state that a
    /// running frame is midway through expect the thread stopped or paused;
    /// while it runs, this waits for the frame in flight to finish.
    pub fn with_console<R>(&self, work: impl FnOnce(&mut Console) -> R) -> R {
        work(&mut self.shared.console())
    }

    // --- media --------------------------------------------------------------

    /// Loads return `Err` with a message fit for a user interface.
    fn load(&self, work: impl FnOnce(&mut Console) -> bool, fallback: &str) -> Result<(), String> {
        self.with_console(|console| {
            if work(console) {
                Ok(())
            } else if console.last_error().is_empty() {
                Err(fallback.to_owned())
            } else {
                Err(console.last_error().to_owned())
            }
        })
    }

    pub fn load_bios(&self, path: &str) -> Result<(), String> {
        self.load(|c| c.load_bios(path), "could not load that BIOS image")
    }
    pub fn load_bios_file(&self, file: File, display_name: &str) -> Result<(), String> {
        self.load(
            |c| c.load_bios_file(file, display_name),
            "could not load that BIOS image",
        )
    }
    pub fn load_demo(&self) -> Result<(), String> {
        self.load(
            Console::load_builtin_demo,
            "could not start the built-in demo",
        )
    }
    pub fn load_disc(&self, path: &str) -> Result<(), String> {
        self.load(|c| c.load_disc(path), "could not open that disc image")
    }
    pub fn load_disc_file(&self, file: File, display_name: &str) -> Result<(), String> {
        self.load(
            |c| c.load_disc_file(file, display_name),
            "could not open that disc image",
        )
    }
    pub fn eject_disc(&self) {
        self.with_console(Console::eject_disc);
    }
    pub fn bios_loaded(&self) -> bool {
        self.with_console(|c| c.bios_loaded())
    }
    pub fn demo_loaded(&self) -> bool {
        self.with_console(|c| c.demo_loaded())
    }
    pub fn disc_loaded(&self) -> bool {
        self.with_console(|c| c.disc_loaded())
    }
    /// Whether the drive's tray is out — software's EJECT, which a game uses
    /// to ask for the next disc. Front ends watch this to swap without reset.
    pub fn disc_tray_open(&self) -> bool {
        self.with_console(|c| c.disc_tray_open())
    }
    pub fn disc_info(&self) -> Option<DiscInfo> {
        self.with_console(|c| {
            c.disc().filter(|disc| disc.is_open()).map(|disc| DiscInfo {
                path: disc.path().to_owned(),
                is_chd: disc.is_chd(),
                sector_count: disc.sector_count(),
                track_count: u32::try_from(disc.tracks().len()).unwrap_or(u32::MAX),
            })
        })
    }

    // --- the machine --------------------------------------------------------

    pub fn set_region(&self, region: Region) {
        self.with_console(|c| c.set_region(region));
    }
    pub fn region(&self) -> Region {
        self.with_console(|c| c.region())
    }
    pub fn set_cpu_scale_percent(&self, percent: u32) {
        self.shared
            .cpu_scale_percent
            .store(percent.clamp(100, 150), Ordering::Release);
    }
    pub fn cpu_scale_percent(&self) -> u32 {
        self.shared.cpu_scale_percent.load(Ordering::Acquire)
    }

    /// Rasterize cels at `scale` times native resolution, 1 (native, the
    /// default) to [`crate::hires::MAX_INTERNAL_SCALE`]; out-of-range values
    /// clamp. Safe while running: the emulator thread picks it up at the next
    /// field boundary, after which frames are `(640 * k) x (2h * k)`. Only the
    /// picture changes; emulation stays exactly native.
    pub fn set_internal_scale(&self, scale: u32) {
        self.shared
            .internal_scale
            .store(crate::hires::clamp_scale(scale), Ordering::Release);
    }
    pub fn internal_scale(&self) -> u32 {
        self.shared.internal_scale.load(Ordering::Acquire)
    }

    /// The scale the machine is drawing at right now, which is the asked-for
    /// one unless the thread has stepped it down to hold the field rate.
    pub fn effective_internal_scale(&self) -> u32 {
        self.shared.effective_scale.load(Ordering::Acquire)
    }

    /// Let the emulator thread lower the internal scale when fields run late,
    /// and raise it back when they stop (on by default). Off pins the picture
    /// to the asked-for scale, judder and all.
    pub fn set_adaptive_scale(&self, adaptive: bool) {
        self.shared
            .adaptive_scale
            .store(adaptive, Ordering::Release);
    }

    pub fn adaptive_scale(&self) -> bool {
        self.shared.adaptive_scale.load(Ordering::Acquire)
    }

    /// Read the console's own cel art as what it was drawn to mean — undo
    /// its dithering, interpolate where the scale allows. Every title, no
    /// artwork needed. Applied at the next field boundary; presentation only.
    pub fn set_texture_enhance(&self, enhance: bool) {
        self.shared
            .texture_enhance
            .store(enhance, Ordering::Release);
    }

    pub fn texture_enhance(&self) -> bool {
        self.shared.texture_enhance.load(Ordering::Acquire)
    }

    /// Point the hi-res pass at a directory of hand-authored cel
    /// replacements (`<fnv>.png`, see [`crate::texpack`]); `None` clears it.
    /// Applied at the next field boundary; presentation only.
    pub fn set_texture_dir(&self, dir: Option<&std::path::Path>) {
        *self
            .shared
            .texture_dir
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = dir.map(std::path::Path::to_path_buf);
        self.shared.texture_gen.fetch_add(1, Ordering::Release);
    }

    /// Reset now if stopped; at the next frame boundary if running, because
    /// resetting from another thread midway through a frame is a data race.
    pub fn reset(&self) {
        if self.running() {
            self.request_reset();
        } else {
            self.with_console(Console::reset);
        }
    }

    pub fn request_reset(&self) {
        self.shared.reset_requested.store(true, Ordering::Release);
    }

    // --- running ------------------------------------------------------------

    /// Start the emulator thread; does nothing if it is running.
    pub fn start(&mut self) {
        if self.thread.is_some() {
            return;
        }
        self.shared.should_stop.store(false, Ordering::Release);
        let shared = Arc::clone(&self.shared);
        self.thread = Some(std::thread::spawn(move || shared.run()));
    }

    /// Stop and join. Machine state survives; a start resumes exactly here.
    pub fn stop(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        self.shared.should_stop.store(true, Ordering::Release);
        // Un-pause so a paused thread notices the stop.
        self.shared.paused.store(false, Ordering::Release);
        // A thread that panicked has nothing left to join; carry on either way.
        let _ = thread.join();
    }

    pub fn running(&self) -> bool {
        self.thread.is_some()
    }

    pub fn set_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::Release);
    }
    pub fn paused(&self) -> bool {
        self.shared.paused.load(Ordering::Acquire)
    }

    /// Emulate exactly one field on this thread and publish it. Refused (0)
    /// while the emulator thread owns the machine.
    pub fn run_frame(&self) -> u32 {
        if self.running() {
            return 0;
        }
        let mut console = self.shared.console();
        console.set_internal_scale(self.internal_scale());
        let dir = self
            .shared
            .texture_dir
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        console.set_texture_dir(dir.as_deref());
        let cycles = console.run_frame();
        self.shared.publish(&console);
        cycles
    }

    // --- video --------------------------------------------------------------

    pub fn mailbox(&self) -> &FrameMailbox {
        &self.shared.mailbox
    }

    /// The newest frame if it is new: `(pixels, width, height)` to `read`.
    /// Pixels are the machine's output run through the current video filter,
    /// so `width`/`height` reflect the filter's scale.
    pub fn acquire_frame<R>(&self, read: impl FnOnce(&[u32], usize, usize) -> R) -> Option<R> {
        let mailbox = &self.shared.mailbox;
        mailbox
            .acquire_with(|pixels| self.present_sized(pixels, read))
            .flatten()
    }

    /// The most recent frame whether or not it is new; `None` until the machine
    /// has published one (the mailbox's black seed is not a drawn frame).
    pub fn current_frame<R>(&self, read: impl FnOnce(&[u32], usize, usize) -> R) -> Option<R> {
        let mailbox = &self.shared.mailbox;
        if mailbox.published() == 0 {
            return None;
        }
        mailbox
            .current_with(|pixels| self.present_sized(pixels, read))
            .flatten()
    }

    /// [`Self::present`] with the mailbox's dimensions, or `None` for a slot
    /// caught between a resize and the next publish (the frame size changes
    /// when the internal scale or region does), whose length disagrees.
    fn present_sized<R>(
        &self,
        pixels: &[u32],
        read: impl FnOnce(&[u32], usize, usize) -> R,
    ) -> Option<R> {
        let mailbox = &self.shared.mailbox;
        let (width, height) = (mailbox.width(), mailbox.height());
        (pixels.len() == width.saturating_mul(height))
            .then(|| self.present(pixels, width, height, read))
    }

    /// The presentation filter and integer scale factor currently applied to
    /// frames handed to `acquire_frame`/`current_frame` and the C ABI.
    pub fn video(&self) -> (VideoFilter, u32) {
        let packed = self.shared.video.load(Ordering::Relaxed);
        let index = packed & 0xff;
        let scale = (packed >> 8).clamp(1, crate::scaler::MAX_SCALE);
        let filter = VideoFilter::ALL
            .get(index as usize)
            .copied()
            .unwrap_or_default();
        (filter, scale)
    }

    /// Pick how the output signal is presented: `filter` reshapes each frame,
    /// `scale` is the integer factor applied to the native dimensions. Setting
    /// `Nearest` at `1` passes pixels through untouched — the cost is zero.
    pub fn set_video(&self, filter: VideoFilter, scale: u32) {
        let index = VideoFilter::ALL
            .iter()
            .position(|f| *f == filter)
            .unwrap_or(0) as u32;
        let scale = scale.clamp(1, crate::scaler::MAX_SCALE);
        self.shared
            .video
            .store(index | (scale << 8), Ordering::Relaxed);
    }

    /// Run a published frame through the video setting and hand it to `read`.
    /// The 1x nearest fast path passes the mailbox's pixels straight through.
    fn present<R>(
        &self,
        pixels: &[u32],
        w: usize,
        h: usize,
        read: impl FnOnce(&[u32], usize, usize) -> R,
    ) -> R {
        let (filter, scale) = self.video();
        if filter == VideoFilter::Nearest && scale == 1 {
            return read(pixels, w, h);
        }
        let mut scratch = self.scaled.lock().unwrap_or_else(PoisonError::into_inner);
        let (dst, tmp) = &mut *scratch;
        let (dw, dh) = scaler::apply(filter, scale, pixels, w, h, dst, tmp);
        read(dst.as_slice(), dw, dh)
    }

    // --- audio and input ----------------------------------------------------

    pub fn audio(&self) -> &AudioRing {
        &self.shared.audio
    }
    pub fn pads(&self) -> &PadState {
        &self.shared.pads
    }

    // --- saved games --------------------------------------------------------

    pub fn nvram_size(&self) -> usize {
        self.with_console(|c| c.nvram_size())
    }

    /// The NVRAM image if the machine wrote to it since the last call. Taken at
    /// a frame boundary by the emulator thread, so it is always consistent.
    pub fn take_nvram(&self) -> Option<Vec<u8>> {
        self.shared
            .nvram
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    pub fn restore_nvram(&self, image: &[u8]) -> bool {
        self.with_console(|c| c.restore_nvram(image))
    }

    // --- diagnostics --------------------------------------------------------

    pub fn stats(&self) -> EmulatorStats {
        let s = &self.shared;
        EmulatorStats {
            frames_emulated: s.frames_emulated.load(Ordering::Relaxed),
            frames_behind: s.frames_behind.load(Ordering::Relaxed),
            deadline_resets: s.deadline_resets.load(Ordering::Relaxed),
            emulated_fps: s.emulated_fps.load(),
            frame_ms: s.frame_ms.load(),
        }
    }

    /// Where the ARM was at the last frame boundary: a sample, not a
    /// synchronised value.
    pub fn cpu_pc(&self) -> u32 {
        self.shared.cpu_pc.load(Ordering::Relaxed)
    }
    pub fn expansion_dma_count(&self) -> u64 {
        self.shared.expansion_dma.load(Ordering::Relaxed)
    }
}
