This build contains the original game's own files, for the private use of the person who owns the game, on their own computer. It must not be shared or published.

(In this crate that means the project-owned demonstration ROM carried over
from the Retro3DO C++ core; no retail BIOS, game or disc is included.)

# retro3do — the 3DO emulation core in safe Rust

The 3DO Interactive Multiplayer machine — ARM60, MADAM (cel engine and matrix),
CLIO (interrupts, timers, video counters), the DSPP audio DSP, SPORT, the VDLP,
the expansion bus with its CD-ROM drive, the PBUS pad chain, ISO/BIN/CUE and
CHD discs — plus what a front end needs around it: an emulator thread with one
pacing policy, a triple-buffered frame mailbox and a lock-free audio ring.

It is the core and nothing else: no window, no input backend, no platform
layer. It is an idiomatic Rust rewrite of the Retro3DO C++ core, structured
around the documented hardware and the reference test suite; it shares no
code with any other emulator. See `CREDITS.md` and the reference's
`docs/PROVENANCE.md`.

## Using it

From C (or Swift through a module map), link `libretro3do.a` / `libretro3do.so`
and include `include/retro3do.h` — the same interface the C++ core exposed, so
existing front ends bind unchanged:

```c
#include "retro3do.h"

retro3do_t *emu = retro3do_create();
retro3do_load_bios(emu, "panafz10.bin");
retro3do_load_disc(emu, "game.chd");
retro3do_start(emu);                       /* the core runs its own thread */

retro3do_frame frame;
if (retro3do_acquire_frame(emu, &frame)) { /* only when there is a new one */
    upload(frame.pixels, frame.width, frame.height);   /* XRGB8888 */
}
retro3do_audio_pull(emu, buffer, 512);     /* interleaved 16-bit stereo */
retro3do_pad_press(emu, 0, RETRO3DO_PAD_A, 1);

retro3do_destroy(emu);
```

From Rust, use `retro3do::Retro3do` (threaded, same semantics as the C ABI) or
`retro3do::Console` (one machine, driven by hand).

### Internal resolution

`retro3do_set_internal_scale(emu, k)` (Rust: `set_internal_scale`) makes MADAM
rasterize every cel a second time on a grid `k` times finer (k = 2, 3 or 4),
the 3DO counterpart of the internal-resolution option other console
emulators have. Polygon edges and sub-pixel cel positions resolve at 1/k of a
pixel; texture colours stay native. Frames become (640·k) × (480·k). The
emulation itself is untouched: VRAM, timing and everything software can see
stay native, and k = 1 is byte-for-byte the native picture (checked with
per-field traces of five titles). How it works is documented at the top of
`src/hires.rs`. A large cel list does its hi-res half on a helper thread, so
the cost lands on a spare core.

Measured on Need for Speed in a race (the heaviest scene tried; it draws a
new picture every third field), on a busy desktop: the paced emulator holds
60.0 fields/s at 1x, 59.8 at 2x and about 55 at 4x. The average field costs
4.4 ms at 1x, about 6.3 ms at 2x and 10 ms at 4x; the drawing fields are
the expensive ones (about 16, 27 and 33 ms at the 90th percentile).

The picture gives way before the motion does: the emulator thread watches
how many fields miss their deadline and steps the internal scale down when a
fifth of a second's worth do, raising it again after three clean seconds (and
waiting twice as long before each further attempt). Road Rash asked for 4x
holds 60.0 fields/s at an effective 2x where fixed 4x manages 35.5 with 103
deadline resets; asked for 2x it holds 60.0 with 39 late fields a run against
506 fixed. `set_adaptive_scale(false)` pins the asked-for scale, judder and
all, and `effective_internal_scale()` says what is actually being drawn.

Three examples help check it on real discs: `field_trace` prints per-field
fingerprints (compare two runs line by line), `hires_shot` saves a frame at
any scale, with a magnified crop, and reports the time per field, and
`realtime_check` runs the paced emulator thread and reports whether it keeps
real time.

## Safety

- Every module forbids `unsafe` except `src/ffi.rs`, the thin C surface, which
  only turns C pointers into references and slices (and adopts descriptors).
- The crate denies `unwrap`, `expect`, `panic!` and slice indexing; guest data
  (CCBs, display lists, DSP programs, disc images) is read with checked access
  and wrapping arithmetic, so a garbage program or corrupt image cannot panic
  the host. A panic that slipped through anyway stops at the C boundary and
  becomes a failed call.
- CHD is read in safe Rust (no libchdr, no C). Decompression uses pure-Rust
  crates (miniz_oxide, lzma-rs, ruzstd; FLAC is decoded by the crate itself).

## Building and testing

```sh
cargo build --release          # rlib, cdylib and staticlib
cargo test                     # the ported reference suite and the C consumer
cargo clippy --all-targets
```

`retro3do-check` is a headless harness (no window): it runs the built-in demo
ROM, or a BIOS and disc you name, and can save the frame or self-test the pad
path:

```sh
cargo run --release -- --screenshot shot.png
cargo run --release -- --self-test result.json
cargo run --release -- --bios bios.bin --disc game.chd --frames 600 --screenshot boot.png
```

## Layout

| Path | What |
|---|---|
| `include/retro3do.h` | The public C interface. |
| `src/ffi.rs` | Its implementation: translation, no decisions. |
| `src/emulator.rs` | The thread, pacing, NVRAM snapshots (`Retro3do`). |
| `src/console.rs` | The machine and its frame loop. |
| `src/system.rs` | Device dispatch and the wiring between chips. |
| `src/arm60.rs`, `madam*`, `clio.rs`, `dsp.rs`, `vdlp.rs`, `sport.rs`, `xbus.rs`, `pbus.rs` | The chips. |
| `src/disc.rs`, `src/chd*` | Disc images and the CHD reader. |
| `src/hires.rs` | Internal-resolution mirrors of the framebuffers (output only). |
| `src/frame_mailbox.rs`, `src/audio_ring.rs`, `src/pad.rs` | The thread-crossing pieces. |
| `tests/` | The reference suite, ported, plus extra cases. |
