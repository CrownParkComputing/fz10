//! Retro3DO: the 3DO Interactive Multiplayer emulation core, in safe Rust.
//!
//! The machine — ARM60, MADAM, CLIO, the DSP, SPORT, the VDLP, the expansion
//! bus with its CD-ROM drive, the PBUS pad chain — plus the pieces a front end
//! needs around it: an emulator thread with one pacing policy, a triple-buffered
//! frame mailbox and a lock-free audio ring. There is no window, no input
//! backend and no platform layer here.
//!
//! Applications bind through the C ABI in [`ffi`], which mirrors
//! `include/retro3do.h`. Rust callers can use [`Retro3do`] directly.
//!
//! Every module except `ffi` forbids `unsafe` code; `ffi` is the one thin place
//! where C pointers are turned into Rust references.
#![deny(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

pub mod arm60;
pub mod audio_ring;
pub mod bus;
pub mod chd;
pub mod clio;
pub mod console;
pub mod demo_rom;
pub mod disc;
pub mod dsp;
pub mod emulator;
pub mod frame_mailbox;
pub mod hires;
pub mod madam;
pub mod memory;
pub mod pad;
pub mod pbus;
pub mod scaler;
pub mod sport;
pub mod system;
pub mod texpack;
pub mod vdlp;
pub mod video;
pub mod xbus;

#[allow(unsafe_code)]
pub mod ffi;

pub use console::{Console, Region};
pub use emulator::{EmulatorStats, Retro3do};
pub use pad::{PadButton, MAX_PADS};
pub use scaler::VideoFilter;
