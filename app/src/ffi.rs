//! The C ABI, equivalent to `include/retro3do.h`.
//!
//! This is the only module in the crate that may use `unsafe`, and it uses it
//! for one thing: turning the pointers a C caller hands over into Rust
//! references and slices (and, on Unix, a descriptor into a `File`). Every
//! decision is made by the safe [`Retro3do`] it wraps; if a function here grows
//! a branch that is not "is this pointer usable, and what do I say if not",
//! the behaviour is in the wrong place.
//!
//! Every entry point tolerates a null machine. A panic inside the core is
//! caught at this boundary and reported as a failed call rather than
//! unwinding into C.
#![allow(non_camel_case_types)]
#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_int, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Mutex, PoisonError};

use crate::console::Region;
use crate::emulator::Retro3do;
use crate::pad::{PadButton, MAX_PADS};

pub const RETRO3DO_ABI_VERSION: c_int = 1;
pub const RETRO3DO_SAMPLE_RATE: u32 = 44_100;
pub const RETRO3DO_MAX_PADS: u32 = 8;
pub const RETRO3DO_REGION_NTSC: c_int = 0;
pub const RETRO3DO_REGION_PAL: c_int = 1;
pub const RETRO3DO_PAD_COUNT: c_int = 11;

// The ABI's numbering is the machine's numbering; stop the build if either
// drifts rather than quietly remapping the controls.
const _: () = assert!(PadButton::COUNT == RETRO3DO_PAD_COUNT as u32);
const _: () = assert!(MAX_PADS == RETRO3DO_MAX_PADS);
const _: () = assert!(crate::audio_ring::SAMPLE_RATE == RETRO3DO_SAMPLE_RATE);
const _: () = assert!(PadButton::RightShift as u32 == 10 && PadButton::Up as u32 == 0);

/// The opaque handle C sees as `retro3do_t`.
pub struct retro3do_machine {
    emu: Retro3do,
    error: CString,
    /// The disc's name as C sees it, kept until the disc changes.
    disc_path: Mutex<CString>,
    /// Scratch for the scaled frame a video filter produces (dst, temp). The
    /// C ABI hands out a pointer into `dst`, valid until the next frame call.
    scaled: Mutex<(Vec<u32>, Vec<u32>)>,
}

pub type retro3do_t = retro3do_machine;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct retro3do_disc_info {
    pub path: *const c_char,
    pub is_chd: c_int,
    pub sector_count: u32,
    pub track_count: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct retro3do_frame {
    pub pixels: *const u32,
    pub width: c_int,
    pub height: c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct retro3do_stats {
    pub frames_emulated: u64,
    pub frames_behind: u64,
    pub deadline_resets: u64,
    pub emulated_fps: f64,
    pub frame_ms: f64,
    pub audio_underruns: u64,
    pub expansion_dma: u64,
    pub cpu_pc: u32,
}

static NO_EMULATOR: &CStr = c"no emulator";

/// Run `body`, turning a panic into `fallback`.
fn guarded<R>(fallback: R, body: impl FnOnce() -> R) -> R {
    catch_unwind(AssertUnwindSafe(body)).unwrap_or(fallback)
}

fn to_cstring(text: &str) -> CString {
    CString::new(text.replace('\0', " ")).unwrap_or_default()
}

fn fail(machine: &mut retro3do_machine, message: &str) -> c_int {
    machine.error = to_cstring(message);
    0
}

fn report(machine: &mut retro3do_machine, result: Result<(), String>) -> c_int {
    match result {
        Ok(()) => 1,
        Err(message) => fail(machine, &message),
    }
}

fn flag(value: bool) -> c_int {
    c_int::from(value)
}

fn button(index: c_int) -> Option<PadButton> {
    u32::try_from(index).ok().and_then(PadButton::from_index)
}

/// A C string argument as text, if it is one.
unsafe fn text<'a>(pointer: *const c_char) -> Option<std::borrow::Cow<'a, str>> {
    if pointer.is_null() {
        return None;
    }
    // SAFETY: the caller passes a NUL-terminated string that outlives the call.
    Some(unsafe { CStr::from_ptr(pointer) }.to_string_lossy())
}

/// Adopt a descriptor the caller hands over. The core owns it from here.
#[cfg(unix)]
unsafe fn adopt(fd: c_int) -> Option<std::fs::File> {
    use std::os::fd::FromRawFd;
    if fd < 0 {
        return None;
    }
    // SAFETY: the ABI transfers ownership of an open descriptor to the core.
    Some(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(not(unix))]
unsafe fn adopt(_fd: c_int) -> Option<std::fs::File> {
    None
}

// SAFETY (for every function below): a non-null `emu` must be a handle from
// `retro3do_create` that has not been destroyed, and other pointers must be
// valid for the documented lengths, as retro3do.h states.

#[no_mangle]
pub extern "C" fn retro3do_abi_version() -> c_int {
    RETRO3DO_ABI_VERSION
}

// --- lifecycle ---------------------------------------------------------------

#[no_mangle]
pub extern "C" fn retro3do_create() -> *mut retro3do_t {
    guarded(std::ptr::null_mut(), || {
        Box::into_raw(Box::new(retro3do_machine {
            emu: Retro3do::new(),
            error: CString::default(),
            disc_path: Mutex::new(CString::default()),
            scaled: Mutex::new((Vec::new(), Vec::new())),
        }))
    })
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_destroy(emu: *mut retro3do_t) {
    if emu.is_null() {
        return;
    }
    // SAFETY: created by Box::into_raw in retro3do_create, destroyed once.
    let machine = unsafe { Box::from_raw(emu) };
    // Dropping stops and joins the emulator thread first.
    guarded((), move || drop(machine));
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_last_error(emu: *const retro3do_t) -> *const c_char {
    // SAFETY: see above.
    match unsafe { emu.as_ref() } {
        Some(machine) => machine.error.as_ptr(),
        None => NO_EMULATOR.as_ptr(),
    }
}

// --- media -------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn retro3do_load_bios(emu: *mut retro3do_t, path: *const c_char) -> c_int {
    // SAFETY: see above.
    let (Some(machine), Some(path)) = (unsafe { emu.as_mut() }, unsafe { text(path) }) else {
        return 0;
    };
    let result = guarded(Err("could not load that BIOS image".into()), || {
        machine.emu.load_bios(&path)
    });
    report(machine, result)
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_load_bios_fd(
    emu: *mut retro3do_t,
    fd: c_int,
    display_name: *const c_char,
) -> c_int {
    // SAFETY: see above.
    let Some(machine) = (unsafe { emu.as_mut() }) else {
        return 0;
    };
    let name = unsafe { text(display_name) }
        .unwrap_or_default()
        .into_owned();
    let Some(file) = (unsafe { adopt(fd) }) else {
        return fail(machine, &format!("Could not open {name}"));
    };
    let result = guarded(Err("could not load that BIOS image".into()), || {
        machine.emu.load_bios_file(file, &name)
    });
    report(machine, result)
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_load_demo(emu: *mut retro3do_t) -> c_int {
    // SAFETY: see above.
    let Some(machine) = (unsafe { emu.as_mut() }) else {
        return 0;
    };
    let result = guarded(Err("could not start the built-in demo".into()), || {
        machine.emu.load_demo()
    });
    report(machine, result)
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_load_disc(emu: *mut retro3do_t, path: *const c_char) -> c_int {
    // SAFETY: see above.
    let (Some(machine), Some(path)) = (unsafe { emu.as_mut() }, unsafe { text(path) }) else {
        return 0;
    };
    let result = guarded(Err("could not open that disc image".into()), || {
        machine.emu.load_disc(&path)
    });
    report(machine, result)
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_load_disc_fd(
    emu: *mut retro3do_t,
    fd: c_int,
    display_name: *const c_char,
) -> c_int {
    // SAFETY: see above.
    let Some(machine) = (unsafe { emu.as_mut() }) else {
        return 0;
    };
    let name = unsafe { text(display_name) }
        .unwrap_or_default()
        .into_owned();
    let Some(file) = (unsafe { adopt(fd) }) else {
        return fail(machine, &format!("Could not open {name}"));
    };
    let result = guarded(Err("could not open that disc image".into()), || {
        machine.emu.load_disc_file(file, &name)
    });
    report(machine, result)
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_eject_disc(emu: *mut retro3do_t) {
    // SAFETY: see above.
    if let Some(machine) = unsafe { emu.as_mut() } {
        guarded((), || machine.emu.eject_disc());
    }
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_bios_loaded(emu: *const retro3do_t) -> c_int {
    // SAFETY: see above.
    unsafe { emu.as_ref() }.map_or(0, |m| guarded(0, || flag(m.emu.bios_loaded())))
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_demo_loaded(emu: *const retro3do_t) -> c_int {
    // SAFETY: see above.
    unsafe { emu.as_ref() }.map_or(0, |m| guarded(0, || flag(m.emu.demo_loaded())))
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_disc_loaded(emu: *const retro3do_t) -> c_int {
    // SAFETY: see above.
    unsafe { emu.as_ref() }.map_or(0, |m| guarded(0, || flag(m.emu.disc_loaded())))
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_get_disc_info(
    emu: *const retro3do_t,
    out: *mut retro3do_disc_info,
) -> c_int {
    // SAFETY: see above.
    let (Some(machine), Some(out)) = (unsafe { emu.as_ref() }, unsafe { out.as_mut() }) else {
        return 0;
    };
    let Some(info) = guarded(None, || machine.emu.disc_info()) else {
        return 0;
    };
    let mut path = machine
        .disc_path
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    // Replaced only when the disc's name changes, so an earlier pointer stays
    // good for as long as that disc is in.
    if path.to_bytes() != info.path.as_bytes() {
        *path = to_cstring(&info.path);
    }
    *out = retro3do_disc_info {
        path: path.as_ptr(),
        is_chd: flag(info.is_chd),
        sector_count: info.sector_count,
        track_count: info.track_count,
    };
    1
}

// --- the machine -------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn retro3do_set_region(emu: *mut retro3do_t, region: c_int) {
    // SAFETY: see above.
    if let Some(machine) = unsafe { emu.as_mut() } {
        let region = if region == RETRO3DO_REGION_PAL {
            Region::Pal
        } else {
            Region::Ntsc
        };
        guarded((), || machine.emu.set_region(region));
    }
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_get_region(emu: *const retro3do_t) -> c_int {
    // SAFETY: see above.
    match unsafe { emu.as_ref() }.map(|m| guarded(Region::Ntsc, || m.emu.region())) {
        Some(Region::Pal) => RETRO3DO_REGION_PAL,
        _ => RETRO3DO_REGION_NTSC,
    }
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_set_cpu_scale_percent(emu: *mut retro3do_t, percent: u32) {
    // SAFETY: see above.
    if let Some(machine) = unsafe { emu.as_ref() } {
        machine.emu.set_cpu_scale_percent(percent);
    }
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_get_cpu_scale_percent(emu: *const retro3do_t) -> u32 {
    // SAFETY: see above.
    unsafe { emu.as_ref() }.map_or(100, |m| m.emu.cpu_scale_percent())
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_set_internal_scale(emu: *mut retro3do_t, scale: u32) {
    // SAFETY: see above.
    if let Some(machine) = unsafe { emu.as_ref() } {
        machine.emu.set_internal_scale(scale);
    }
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_get_internal_scale(emu: *const retro3do_t) -> u32 {
    // SAFETY: see above.
    unsafe { emu.as_ref() }.map_or(1, |m| m.emu.internal_scale())
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_reset(emu: *mut retro3do_t) {
    // SAFETY: see above.
    if let Some(machine) = unsafe { emu.as_ref() } {
        guarded((), || machine.emu.reset());
    }
}

// --- running -----------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn retro3do_start(emu: *mut retro3do_t) {
    // SAFETY: see above.
    if let Some(machine) = unsafe { emu.as_mut() } {
        guarded((), || machine.emu.start());
    }
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_stop(emu: *mut retro3do_t) {
    // SAFETY: see above.
    if let Some(machine) = unsafe { emu.as_mut() } {
        guarded((), || machine.emu.stop());
    }
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_running(emu: *const retro3do_t) -> c_int {
    // SAFETY: see above.
    unsafe { emu.as_ref() }.map_or(0, |m| flag(m.emu.running()))
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_set_paused(emu: *mut retro3do_t, paused: c_int) {
    // SAFETY: see above.
    if let Some(machine) = unsafe { emu.as_ref() } {
        machine.emu.set_paused(paused != 0);
    }
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_paused(emu: *const retro3do_t) -> c_int {
    // SAFETY: see above.
    unsafe { emu.as_ref() }.map_or(0, |m| flag(m.emu.paused()))
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_request_reset(emu: *mut retro3do_t) {
    // SAFETY: see above.
    if let Some(machine) = unsafe { emu.as_ref() } {
        machine.emu.request_reset();
    }
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_run_frame(emu: *mut retro3do_t) -> u32 {
    // SAFETY: see above.
    unsafe { emu.as_ref() }.map_or(0, |m| guarded(0, || m.emu.run_frame()))
}

// --- video -------------------------------------------------------------------

/// `None` for a slot caught mid-resize, whose length disagrees with the size
/// (the frame size changes with the internal scale): no frame this call.
fn frame_of(
    machine: &retro3do_machine,
    (pixels, len): (*const u32, usize),
) -> Option<retro3do_frame> {
    let mailbox = machine.emu.mailbox();
    let (w, h) = (mailbox.width(), mailbox.height());
    if len != w.saturating_mul(h) {
        return None;
    }
    let (filter, scale) = machine.emu.video();
    if filter == crate::scaler::VideoFilter::Nearest && scale == 1 {
        return Some(retro3do_frame {
            pixels,
            width: c_int::try_from(w).unwrap_or(0),
            height: c_int::try_from(h).unwrap_or(0),
        });
    }
    // SAFETY: `pixels` points into the mailbox's live (or just retired) frame
    // slot, which is `len` == w*h u32s. The scaled result lives in the machine's scratch: the pointer
    // we hand out stays valid until the next frame call on this handle, the
    // same contract the mailbox gives.
    let mut scratch = machine
        .scaled
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let (dst, tmp) = &mut *scratch;
    let src = unsafe { std::slice::from_raw_parts(pixels, len) };
    let (dw, dh) = crate::scaler::apply(filter, scale, src, w, h, dst, tmp);
    Some(retro3do_frame {
        pixels: dst.as_ptr(),
        width: c_int::try_from(dw).unwrap_or(0),
        height: c_int::try_from(dh).unwrap_or(0),
    })
}

/// Select the presentation filter applied to acquired frames: one of
/// `retro3do_video_filter`, times `scale` (integer, 1..=16).
#[no_mangle]
pub unsafe extern "C" fn retro3do_set_video(emu: *mut retro3do_t, filter: c_int, scale: u32) {
    if let Some(machine) = unsafe { emu.as_ref() } {
        let f = crate::scaler::VideoFilter::ALL
            .get(filter.max(0) as usize)
            .copied()
            .unwrap_or_default();
        machine.emu.set_video(f, scale);
    }
}

/// The current filter index and scale factor.
#[no_mangle]
pub unsafe extern "C" fn retro3do_get_video(
    emu: *const retro3do_t,
    filter: *mut c_int,
    scale: *mut u32,
) {
    if let Some(machine) = unsafe { emu.as_ref() } {
        let (f, s) = machine.emu.video();
        let index = crate::scaler::VideoFilter::ALL
            .iter()
            .position(|x| *x == f)
            .unwrap_or(0) as c_int;
        if let Some(p) = unsafe { filter.as_mut() } {
            *p = index;
        }
        if let Some(p) = unsafe { scale.as_mut() } {
            *p = s;
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_acquire_frame(
    emu: *mut retro3do_t,
    out: *mut retro3do_frame,
) -> c_int {
    // SAFETY: see above.
    let (Some(machine), Some(out)) = (unsafe { emu.as_ref() }, unsafe { out.as_mut() }) else {
        return 0;
    };
    match guarded(None, || machine.emu.mailbox().acquire_raw_sized())
        .and_then(|f| frame_of(machine, f))
    {
        Some(frame) => {
            *out = frame;
            1
        }
        None => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_current_frame(
    emu: *mut retro3do_t,
    out: *mut retro3do_frame,
) -> c_int {
    // SAFETY: see above.
    let (Some(machine), Some(out)) = (unsafe { emu.as_ref() }, unsafe { out.as_mut() }) else {
        return 0;
    };
    // "What is in the mailbox" is not "has the machine drawn anything": the
    // slots are seeded black, so report on published frames.
    let mailbox = machine.emu.mailbox();
    if mailbox.published() == 0 {
        return 0;
    }
    match guarded(None, || mailbox.current_raw_sized()).and_then(|f| frame_of(machine, f)) {
        Some(frame) => {
            *out = frame;
            1
        }
        None => 0,
    }
}

// --- audio -------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn retro3do_audio_available(emu: *const retro3do_t) -> u32 {
    // SAFETY: see above.
    unsafe { emu.as_ref() }.map_or(0, |m| m.emu.audio().available())
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_audio_pull(
    emu: *mut retro3do_t,
    interleaved: *mut i16,
    frames: u32,
) -> u32 {
    // SAFETY: see above.
    let Some(machine) = (unsafe { emu.as_ref() }) else {
        return 0;
    };
    if interleaved.is_null() || frames == 0 {
        return 0;
    }
    let Some(len) = (frames as usize).checked_mul(2) else {
        return 0;
    };
    // SAFETY: the caller provides room for `frames` stereo pairs.
    let out = unsafe { std::slice::from_raw_parts_mut(interleaved, len) };
    machine.emu.audio().pull_interleaved(out)
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_audio_underruns(emu: *const retro3do_t) -> u64 {
    // SAFETY: see above.
    unsafe { emu.as_ref() }.map_or(0, |m| m.emu.audio().underruns())
}

// --- input -------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn retro3do_pad_connect(emu: *mut retro3do_t, pad: u32, connected: c_int) {
    // SAFETY: see above.
    if let Some(machine) = unsafe { emu.as_ref() } {
        machine.emu.pads().set_connected(pad, connected != 0);
    }
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_pad_connected(emu: *const retro3do_t, pad: u32) -> c_int {
    // SAFETY: see above.
    unsafe { emu.as_ref() }.map_or(0, |m| flag(m.emu.pads().connected(pad)))
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_pad_count(emu: *const retro3do_t) -> u32 {
    // SAFETY: see above.
    unsafe { emu.as_ref() }.map_or(0, |m| m.emu.pads().connected_count())
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_pad_press(
    emu: *mut retro3do_t,
    pad: u32,
    which: c_int,
    down: c_int,
) {
    // SAFETY: see above.
    if let (Some(machine), Some(which)) = (unsafe { emu.as_ref() }, button(which)) {
        machine.emu.pads().press(pad, which, down != 0);
    }
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_pad_pressed(
    emu: *const retro3do_t,
    pad: u32,
    which: c_int,
) -> c_int {
    // SAFETY: see above.
    match (unsafe { emu.as_ref() }, button(which)) {
        (Some(machine), Some(which)) => flag(machine.emu.pads().pressed(pad, which)),
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_pad_set_buttons(emu: *mut retro3do_t, pad: u32, buttons: u32) {
    // SAFETY: see above.
    if let Some(machine) = unsafe { emu.as_ref() } {
        machine.emu.pads().set_buttons(pad, buttons);
    }
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_pad_get_buttons(emu: *const retro3do_t, pad: u32) -> u32 {
    // SAFETY: see above.
    unsafe { emu.as_ref() }.map_or(0, |m| m.emu.pads().buttons(pad))
}

// --- saved games -------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn retro3do_nvram_size(emu: *const retro3do_t) -> u32 {
    // SAFETY: see above.
    unsafe { emu.as_ref() }.map_or(0, |m| {
        u32::try_from(guarded(0, || m.emu.nvram_size())).unwrap_or(0)
    })
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_take_nvram(
    emu: *mut retro3do_t,
    out: *mut u8,
    capacity: u32,
    out_size: *mut u32,
) -> c_int {
    // SAFETY: see above.
    let Some(machine) = (unsafe { emu.as_mut() }) else {
        return 0;
    };
    if out.is_null() {
        return 0;
    }
    let Some(image) = machine.emu.take_nvram() else {
        return 0;
    };
    if image.len() > capacity as usize {
        return fail(machine, "NVRAM buffer is too small");
    }
    // SAFETY: the caller provides `capacity` writable bytes at `out`.
    let destination = unsafe { std::slice::from_raw_parts_mut(out, image.len()) };
    destination.copy_from_slice(&image);
    // SAFETY: a non-null `out_size` points at a writable u32.
    if let Some(size) = unsafe { out_size.as_mut() } {
        *size = u32::try_from(image.len()).unwrap_or(u32::MAX);
    }
    1
}

#[no_mangle]
pub unsafe extern "C" fn retro3do_restore_nvram(
    emu: *mut retro3do_t,
    data: *const u8,
    size: u32,
) -> c_int {
    // SAFETY: see above.
    let Some(machine) = (unsafe { emu.as_mut() }) else {
        return 0;
    };
    if data.is_null() {
        return 0;
    }
    // SAFETY: the caller provides `size` readable bytes at `data`.
    let image = unsafe { std::slice::from_raw_parts(data, size as usize) };
    if guarded(false, || machine.emu.restore_nvram(image)) {
        1
    } else {
        fail(machine, "that save data is not a valid NVRAM image")
    }
}

// --- diagnostics -------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn retro3do_get_stats(emu: *const retro3do_t, out: *mut retro3do_stats) {
    // SAFETY: see above.
    let Some(out) = (unsafe { out.as_mut() }) else {
        return;
    };
    *out = retro3do_stats::default();
    // SAFETY: see above.
    let Some(machine) = (unsafe { emu.as_ref() }) else {
        return;
    };
    let stats = machine.emu.stats();
    *out = retro3do_stats {
        frames_emulated: stats.frames_emulated,
        frames_behind: stats.frames_behind,
        deadline_resets: stats.deadline_resets,
        emulated_fps: stats.emulated_fps,
        frame_ms: stats.frame_ms,
        audio_underruns: machine.emu.audio().underruns(),
        expansion_dma: machine.emu.expansion_dma_count(),
        cpu_pc: machine.emu.cpu_pc(),
    };
}
