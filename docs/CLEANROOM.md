# Written from documentation

How this emulator was built: from public hardware documentation and project
experiments, module by module, with an auditable record of where every part
came from.

**Read [PROVENANCE.md](PROVENANCE.md) first.** The honest word for this
project is *documentation-driven*, not clean-room: the Rust crate was ported
from Crown Park Computing's own C++ core, and that core's development was
historically reference-exposed to FreeDO-descended Opera code. Every routine
that exposure touched was later rebuilt against the public specifications
listed below — the September 2026 retrospective — but the exposure is part of
the record and does not get edited out. This file describes the *method*: how
the current source was produced and how anyone can check that claim. It makes
no legal claim.

## The method

Each hardware module in `app/src/` was written against a named public
authority — the same discipline the C++ core was rebuilt under:

1. **Name the authority.** Every module carries a `//!` documentation block
   naming the specification it follows. [SOURCES.md](SOURCES.md) is generated
   from those blocks, so the code and the source register cannot drift apart
   silently: if a module's authority changes, its `//!` block is the thing to
   change, and the table follows it.
2. **Implement the document, not another program.** Data structures and
   control flow are the project's own — conventional algorithms (active-edge
   scan conversion, row-pair traversal, field decoders) arranged around the
   published description, not around another emulator's shape.
3. **Settle ambiguity by observation.** Where the documents disagree or say
   nothing, the boot ROM and the project's own demonstration ROM
   (`app/src/demo_rom.bin`, a Crown Park Computing hardware test — not a BIOS)
   were run and the machine's behaviour recorded. Observed behaviour is fact,
   not anyone's expression. Load-bearing observations are marked in the
   module and pinned by a regression test.
4. **Disclose the exceptions.** A small number of behaviour-only
   compatibility facts were originally settled with reference help rather
   than from documentation. They are listed plainly in
   [PROVENANCE.md](PROVENANCE.md) under "Behaviour-only fallback facts", each
   isolated and covered by a test, each flagged to be replaced by a
   physical-hardware measurement if one becomes available.

## The public source set

The complete per-module table is [SOURCES.md](SOURCES.md). In short:

- **ARM60** (`arm60.rs`): ARM DDI 0100I and ARM's published ARM6
  instruction-timing material.
- **MADAM** (`madam/`): 3DO SDK `hardware.h`, the 3DO Graphics Programming
  Guide, and patents WO 94/10644 and WO 94/10643; the matrix unit adds SDK
  `operamath.h` and WO 94/10641.
- **DSPP** (`dsp.rs`): WO 94/16383, the published instruction diagrams.
- **VDLP** (`vdlp.rs`): the Guide's "Creating a Custom VDL" and patents
  US 5,502,462 / US 5,742,778.
- **CLIO** (`clio.rs`): SDK headers, the system patents, the boot ROM's own
  code paths, and the community 3DOessence register map — see
  [HARDWARE.md](HARDWARE.md).
- **XBUS / PBUS / SPORT** (`xbus.rs`, `pbus.rs`, `sport.rs`): patents
  WO 94/16382 and WO 94/10636, SDK CD/controller definitions, system
  architecture material, and the BSD-3-Clause CR-560B *device documentation*
  (a device description published by MAME, not emulator source).
- **Disc and CHD** (`disc.rs`, `chd/`): published CD image layouts and the
  CHD container format as described by libchdr (BSD-3-Clause) — read as a
  format description; the reader is new Rust linking no C.
- **Video output** (`video.rs`, `scaler.rs`, `hires.rs`): project work — the
  machine's picture once the documented hardware has produced it.

## What was read, what was written

Written fresh for this crate: everything in `app/src/`, the emulator thread
and frame mailbox (`emulator.rs`, `frame_mailbox.rs`), the egui front end,
the headless check tools (`probe`, `in_game_check`, `hires_shot`,
`gamecheck`), the test suite and the C ABI plumbing. No FreeDO, 4DO, Opera,
opera-libretro or MAME source was consulted or is present — the audit below
is how that claim is checked.

Carried across from Crown Park Computing's own C++ core (the `cpp-reference/`
tree, removed from this repository 2026-11-24): the demonstration ROM bytes,
the `retro3do.h` interface shape, the smallest C consumer used to test the
ABI, and the formatted-empty NVRAM image. Each is listed in
[PROVENANCE.md](PROVENANCE.md).

Read as *specifications*, with provenance disclosed: the C++ core itself
(this crate is a port of it, which is why its knowledge history is inherited
and disclosed), and the third-party format/device documents above. The full
lineage, including the unfavourable parts, is [PROVENANCE.md](PROVENANCE.md);
the investigation history survives in [OPERA_AUDIT.md](OPERA_AUDIT.md) and
[FMV_STREAMING_HANDOVER.md](FMV_STREAMING_HANDOVER.md).

## The audit

The claim "no other emulator's source is present here" is checkable, and
[AUDIT.md](AUDIT.md) is the record of it:

1. grep for FreeDO/Opera author names and licence notices → 0 matches;
2. grep for `opera_*` identifiers → 0 matches;
3. inventory of C/C++ files → exactly two, both ours (the public ABI header
   and its smallest consumer);
4. no `build.rs`, `cc` or `bindgen` — no C is compiled or linked; CHD
   decompression is the pure-Rust `miniz_oxide`, `lzma-rs` and `ruzstd`;
5. size and coverage — the shape of independent work, currently 51 Rust
   files and 398 `#[test]` functions.

Re-run the whole thing:

```sh
bash docs/audit.sh
```

If a check starts failing, that is information, not a bug to be silenced.

## Reproducing the methodology

To take a new hardware module through the same process:

1. Find the public authority (SDK header, patent, guide chapter) and name it
   in the module's `//!` block before writing code.
2. Implement the documented behaviour with the project's own structure;
   where the document is ambiguous, write down the question.
3. Answer open questions on hardware-equivalent observation: the boot ROM's
   own code paths, the demonstration ROM, or a focused test program. Record
   the observation where the decision lives.
4. Pin every settled detail with a test in `app/tests/` or a `#[test]` next
   to the code; run `cargo test` and `bash docs/audit.sh`.
5. Update [SOURCES.md](SOURCES.md)'s row (it mirrors the `//!` block) and,
   if any fallback fact was used, disclose it in
   [PROVENANCE.md](PROVENANCE.md).

The repository now stands on its own: `app/` is the only implementation
tree, and every fact the build needs is in these documents or the public
specifications they cite.
