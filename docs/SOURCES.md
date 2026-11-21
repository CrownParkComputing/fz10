# Source material

The public authority each part of the hardware model follows. Every row is
taken from the module's own documentation comment, so the code and this table
cannot drift apart silently: if a module's authority changes, its `//!` block
is the thing to change, and this file follows it.

None of these sources is another emulator's source code. Where a third-party
project is named, it is named as a *format or device description*, and the row
says so.

## Per module

| Module | Authority | Notes |
|---|---|---|
| `app/src/arm60.rs` | ARM DDI 0100 (ARM Architecture Reference Manual); ARM's published ARM6 instruction-timing material | ARM6-family core, ARMv3, 12.5 MHz. No Thumb, no long multiply, no coprocessor interface. |
| `app/src/madam/mod.rs` | 3DO SDK `hardware.h`; 3DO Graphics Programming Guide; WO 94/10644 (Spryte Rendering System); WO 94/10643 | CCB, preamble and register fields from the SDK and Guide; projection, pixel decoder/processor and CEL bus state from the patents. Visibility, scan conversion and LR row-pair traversal are project implementations around those descriptions. |
| `app/src/madam/matrix.rs` | 3DO SDK `operamath.h`; WO 94/10641 (Audio-Video Computer Architecture) | The SDK defines the operations; the patent describes the unit. |
| `app/src/dsp.rs` | WO 94/16383 (Digital Signal Processor Architecture) | Field decoder, operand-group packets, write-back selection, branch conditions, ALU and barrel shifter follow the published instruction diagrams. |
| `app/src/vdlp.rs` | 3DO Graphics Programming Guide, "Creating a Custom VDL"; US 5,502,462; US 5,742,778 | The Guide and patent give the same per-line order, which `Vdlp::process_scanline` follows. |
| `app/src/clio.rs` | 3DO SDK headers; the 3DO system patents; the boot ROM's own code paths; the community 3DOessence register map | Register offsets and bit assignments are cross-checked across all four. See `docs/HARDWARE.md`. |
| `app/src/sport.rs` | 3DO system architecture material; BIOS SPORT behaviour | The address is the command; no command register exists. |
| `app/src/pbus.rs` | WO 94/10636 (Player Bus Apparatus and Method) | Daisy-chained device reports written to memory by DMA; `0xFF` padding terminates. |
| `app/src/xbus.rs` | WO 94/16382 (Expansion Bus); 3DO SDK CD definitions; CR-560B device documentation (BSD-3-Clause, as published by MAME); boot-ROM observations | The patent documents the protocol in full. The CR-560B material is *device documentation*, not emulator source; no MAME source is compiled here. |
| `app/src/disc.rs` | Published CD image layouts: cooked 2048-byte sectors, raw 2352-byte mode 1 and mode 2, cue sheets | Not 3DO-specific. Knows nothing about the 3DO. |
| `app/src/chd/` | The CHD container format, as described by libchdr (BSD-3-Clause) | Read as a *format description*. The reader here is new Rust code and links no C. Decompression uses the pure-Rust crates in `THIRD_PARTY.md`. |
| `app/src/memory.rs` | 3DO memory map from the SDK headers and system patents | DRAM, VRAM, boot ROM, battery-backed NVRAM. |

## Primary documents, in full

Patents, all published by The 3DO Company and freely retrievable:

- WO 94/10636 — Player Bus Apparatus and Method
- WO 94/10641 — Audio-Video Computer Architecture
- WO 94/10643 — Improved Method and Apparatus for Processing Image Data
- WO 94/10644 — Spryte Rendering System
- WO 94/16382 — Expansion Bus
- WO 94/16383 — Digital Signal Processor Architecture
- US 5,502,462 — video display-list timing
- US 5,742,778 — video display-list timing

First-party developer documentation:

- 3DO SDK 2.5 headers: `hardware.h`, `operamath.h`, `cdrom.h`, controller
  definitions
- 3DO Graphics Programming Guide
- ARM DDI 0100 and the ARM6 timing material shipped with the 3DO developer
  documentation

Other published references:

- The community 3DOessence register map, used as a cross-check on CLIO
- CR-560B CD device documentation, BSD-3-Clause as published by MAME, used for
  device command and reply shapes only

## Observation as a source

Where public documentation is ambiguous, behaviour was settled by running the
boot ROM and the project's own test ROMs and recording what the machine does.
Observed behaviour is fact and is not anyone's copyrighted expression. Where
such a decision is load-bearing it is marked in the module and covered by a
regression test.

`cpp-reference/docs/PROVENANCE.md` records a small number of behaviour-only
compatibility facts that were originally settled with reference help rather
than from public documentation. Those are listed there, not hidden here.
