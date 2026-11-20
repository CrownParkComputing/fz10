# Core perf: Road Rash gameplay profile (2026-11-20)

## Method

`perf` is unusable on this host (`kernel.perf_event_paranoid=2`, no root), so the
work was done with manual instrumentation plus callgrind:

- **Wall clock** - the emulator's own cost, no pacing noise: a headless loop of
  `Console::run_frame` over the same disc with the same pad script
  (`app/examples/probe.rs`, new). The C++ reference was driven by an equivalent
  harness (`/tmp/cpp-timing/timing.cpp`) against the same library.
- **Instruction counts** - `valgrind --tool=callgrind` on both cores, then
  `callgrind_annotate --auto=yes` for line-level attribution (`debug = 1` under
  `[profile.release]` for the line view).

Workload: `~/3do-library/bios/panafz10.bin` + `~/3do-library/games/Road Rash (USA).chd`,
native 640x480 (internal scale 1), 3000 fields, A tapped every 120 fields and Play
every 180. Both cores reach the same screen - the 3D street scene with the bike.

## Result

Emulator cost per field, no instrumentation, same machine and script:

| | mean | fields/s | gameplay window (fields 2400-3000) | worst field |
|---|---|---|---|---|
| Rust, clean HEAD | 6.09 ms | 163.9 | 9.43 - 10.71 ms | 26.3 ms |
| **Rust, after the fixes** | **5.18 ms** | **192.5** | **8.49 - 9.30 ms** | **18.4 ms** |
| C++ reference (`-O3`) | 4.35 ms | 229.8 | 7.27 - 7.55 ms | 14.8 ms |

The gap to the reference went from 1.40x to 1.19x. The field budget is 16.67 ms, so
the machine holds 60 fields/s in gameplay on this host before and after; what the fixes
buy is headroom, and that is what turns into dropped frames on slower hardware.

End to end, through the paced thread every front end uses, in an unpaused race
(`app/examples/in_game_check.rs`, new): 59.98 fields/s, 0 deadline resets, worst field
under the budget. The threaded figure is dominated by the frame publish and the pacer,
so the headless per-field cost above is the meaningful one.

## Breakdown (counters in place, so the totals run a few tenths higher)

Per field, gameplay window, milliseconds:

| section | Rust before | Rust after | C++ reference |
|---|---|---|---|
| CPU (`arm60`) | 5.33 | 5.23 | 4.56 |
| DSP | 3.02 | 2.4 - 3.2 | 2.13 |
| tick (CLIO/MADAM/VDLP scanline) | 0.32 | 0.29 | 0.30 |
| decode invalidation | 0.30 | 0.05 | 0.28 |
| finish_field (framebuffer expand) | 0.69 | 0.22 | ~0.18 |

## What was wrong, and what was fixed

1. **`vdlp::finish_field_2x` used the slow generic expander.** The port already
   contained the optimised `expand_line_fast` (the mode split outside the pixel loop,
   `copy_from_slice` for the unfiltered case) but never called it - `finish_field_2x`
   used the per-pixel `expand_line`, and `expand_line_fast` was referenced only by its
   own equivalence test. Callgrind: 1.41e9 of 9.20e9 instructions (15.3%) in the Rust
   port against 84e6 (1.45%) in the C++ reference. Wired the fast path in;
   `expand_line`/`expand_pixel` are now `#[cfg(test)]` and stay as the reference the fast
   path is tested against. 0.69 -> 0.22 ms/field.
2. **The write watch scanned every page.** `WriteWatch::dirty_pages` tested all 512
   pages on every slice (960 slices/field) instead of walking the dirty bitmap.
   Replaced with a word-at-a-time bitmap walk (`SetBits`), so the cost follows the
   pages actually written. 0.40 -> 0.05 ms/field.
3. **Block transfers built their register list with an iterator chain.** `LDM`/`STM`
   tested all sixteen registers through a mapped/filtered iterator (213e6 instructions,
   2.9% of the whole profile). The list is now walked by its bits. CPU 5.53 -> 5.23
   ms/field.

## Ruled out by measurement

- **Cycle accounting is right.** Both cores consume the same `cycles_per_frame` budget
  and the same DSPP steps per field (81,975) and the same ARM decode counts. The port
  is not throttling and not running the CPU at the wrong rate; it spends about a fifth
  more host instructions per ARM instruction.
- **The MADAM/CLIO scanline path is at parity** (0.29 against 0.30 ms/field).
- **The CHD path is better than the reference's**: a prefetch thread plus a hunk cache
  means the worst field is 18 ms where the reference blocks on libchdr reads for
  48-108 ms. The port's LZMA work is 5x the reference's only because the prefetch
  window is eight hunks wide.

## Still open

The residual gap is the port's decode representation, in both processors: the ARM60 and
the DSP decode into a fat enum with every field extracted up front (the DSP costs 33
host instructions per DSPP instruction against roughly ten for the reference, which
decodes the opcode alone and re-extracts the fields at execution). Closing that means
changing how instructions are decoded, which is the architecture change this task ruled
out. It is the next thing to look at if the gap still matters.

## Reproducing

    cargo build --release --example probe
    ./target/release/examples/probe BIOS DISC OUTDIR 3000 1      # per-field cost
    cargo build --release --example in_game_check
    ./target/release/examples/in_game_check BIOS DISC out.png 20 # the paced path
    valgrind --tool=callgrind --callgrind-out-file=out ./target/release/examples/probe ...
    callgrind_annotate --auto=yes out
