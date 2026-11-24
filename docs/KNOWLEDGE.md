# The knowledge behind this machine

This page is what the launcher's **Wiki** section shows. The words here and
the words in the app are the same words: the app bundles this file at build
time, so the two cannot drift apart.

## What this is

A 3DO Interactive Multiplayer, written from scratch for this project.

The picture you see on a real FZ-1 comes from five cooperating chips: a CPU,
a graphics chip, a maths unit, a signal processor and an I/O controller. This
project models each of them, plus the buses and chips around them, from the
hardware's own published documentation — SDK header files, patent
applications and developer guides — and then watches the real thing boot to
settle what the documents leave open.

No code from another emulator (FreeDO, 4DO, Opera, MAME) is in this tree,
and none was consulted while the current source was written.

The honest word for how this was made is **documentation-driven**, not
strictly clean-room: this crate was ported from Crown Park Computing's own
earlier C++ core, and that core's development was historically exposed to
FreeDO-descended Opera code. Every routine that exposure touched was later
rebuilt against the public specifications listed below, and the earlier
exposure stays in the record instead of being edited out. The full lineage —
including the unfavourable parts — is in [PROVENANCE.md](PROVENANCE.md). This
makes no legal claim; it says where the code came from.

## The five cores, and the public spec each one follows

- **ARM60** (`app/src/arm60.rs`) — the CPU: a 12.5 MHz ARMv3 core of the
  ARM6 family, with no Thumb, no long multiply and no coprocessor
  interface. Public spec: **ARM DDI 0100** (ARM Architecture Reference
  Manual) plus ARM's published ARM6 instruction-timing material.
- **MADAM** (`app/src/madam/`) — the graphics chip: the cel engine that
  draws the game's sprites, the pixel processor that decodes them, and the
  matrix unit that does the 3D maths. Public specs: the 3DO SDK's
  `hardware.h`, the **3DO Graphics Programming Guide**, and patents
  **WO 94/10644** (Spryte Rendering System) and **WO 94/10643**; the matrix
  unit adds the SDK's `operamath.h` and **WO 94/10641** (Audio-Video
  Computer Architecture).
- **DSP** (`app/src/dsp.rs`) — the DSPP: the coprocessor the games use for
  audio and for most of the per-frame maths, with its field decoder,
  operand-group packets, ALU and barrel shifter. Public spec: **WO 94/16383**
  (Digital Signal Processor Architecture) and its published instruction
  diagrams.
- **CLIO** (`app/src/clio.rs`) — the I/O and DMA chip: the register file,
  timers, DMA channels and the CD-ROM interface. Public specs: the 3DO SDK
  headers, the 3DO system patents, the boot ROM's own code paths, and the
  community 3DOessence register map used as a cross-check.
- **XBUS** (`app/src/xbus.rs`) — the expansion bus that the console's
  peripheral port runs on, with **PBUS** (`pbus.rs`, the player bus the
  CD-ROM and modem use) and **SPORT** (`sport.rs`, the serial port). Public
  specs: **WO 94/16382** (Expansion Bus), **WO 94/10636** (Player Bus
  Apparatus and Method), the SDK's CD definitions, and the CR-560B CD
  device documentation (BSD-3-Clause, as published by MAME — device
  documentation, not emulator source).

Around them: the **VDLP** video display-list processor
(`app/src/vdlp.rs`), which follows the 3DO Graphics Programming Guide's
"Creating a Custom VDL" and patents **US 5,502,462** and **US 5,742,778**;
the **disc and CHD** layers, which follow published CD image layouts and the
CHD container format as described by libchdr (BSD-3-Clause, read as a format
description — the reader here is new Rust and links no C); and the
**video output** (`video.rs`, `scaler.rs`, `hires.rs`), which is this
project's own work: the picture once the documented hardware has produced it.

The complete per-module table, with notes, is [SOURCES.md](SOURCES.md).

## Where the behaviour came from

Where the documents disagree or say nothing, behaviour was settled by
watching the machine: the boot ROM's own code paths, and Crown Park
Computing's own demonstration ROM — a hardware test, not a BIOS. Observed
behaviour is fact and is not anyone's copyrighted expression, and every
load-bearing observation is pinned by a regression test.

A small number of behaviour-only compatibility facts were originally
settled with reference help rather than from documentation. They are listed
plainly in [PROVENANCE.md](PROVENANCE.md) under "Behaviour-only fallback
facts", each isolated, each covered by a test.

## How to check the claim

The statement "no other emulator's source is present here" is checkable, and
[AUDIT.md](AUDIT.md) is the record of it:

1. grep for FreeDO and Opera author names and licence notices — no matches;
2. grep for `opera_*` identifiers — no matches;
3. inventory of C and C++ files — exactly two, both ours;
4. no `build.rs`, no `cc`, no `bindgen` — no C is compiled or linked, and CHD
   decompression uses the pure-Rust `miniz_oxide`, `lzma-rs` and `ruzstd`;
5. size and coverage — 51 Rust files and 398 `#[test]` functions.

Re-run the whole thing with `bash docs/audit.sh`. If a check starts failing,
that is information, not a bug to be silenced.

## Read more

- [PROVENANCE.md](PROVENANCE.md) — what was read as reference, what was
  written fresh, and the fallback facts.
- [SOURCES.md](SOURCES.md) — the public authority for every module.
- [CLEANROOM.md](CLEANROOM.md) — the method, in full, and how to repeat it
  for a new module.
- [AUDIT.md](AUDIT.md) — the audit, and how to re-run it.
- [task-progress.md](task-progress.md) — the build log.

On the web: the project wiki at
<https://github.com/CrownParkComputing/fz10/wiki>, and the development notes
in the repository at <https://github.com/CrownParkComputing/fz10>.
