# Provenance

This file records where the implementation in this repository came from. It
separates two claims that must never be blurred together:

1. what source code is present and compiled here; and
2. what knowledge entered the project historically, and by what route.

The first claim is auditable and is audited — see [AUDIT.md](AUDIT.md). The
second is a matter of history and is stated plainly below, including the parts
that are unfavourable. This document must not be edited to make the project
look cleaner than it is.

## What is compiled here

The Rust crate in `app/` is Crown Park Computing's own code. It contains no
source from FreeDO, 4DO, Opera, opera-libretro, MAME, or any other emulator.
That is a checkable statement and [AUDIT.md](AUDIT.md) gives the commands.

The implementation was written from:

- the public specifications listed in [SOURCES.md](SOURCES.md), which is
  organised per module and names the authority each module follows;
- Crown Park Computing's own C++ core, used as a written specification; and
- the project's own test suite and demonstration ROM.

## The route this code took, in full

**This is not a clean-room project, and it must not be described as one.**

The chain is:

    FreeDO / 4DO / Opera  ->  Retro3DO C++ core  ->  this Rust crate
         (third party)         (Crown Park)          (Crown Park)

The first link is the one that matters. The C++ core's own provenance record
used to live at `cpp-reference/docs/PROVENANCE.md`; on 2026-11-24 the
`cpp-reference/` tree was removed from this repository and that record is
folded in here, unsoftened, in the next two sections.

The Rust crate in `app/` was written by porting Crown Park Computing's C++
core. It therefore inherits that core's knowledge history. No Opera or FreeDO
source was consulted while writing the Rust code, and none is present in it,
but the author of this crate has previously read Opera source, and this
document records that rather than leaving it to be discovered.

FreeDO's published notice purports to require the owners' approval for
commercial use of knowledge obtained by studying its source. This file records
the existence of that term. It offers no opinion on its scope or
enforceability; that is a question for legal advice, not for a source
repository.

## The C++ core's historical status, in its own words

From `cpp-reference/docs/PROVENANCE.md`, folded in when the tree was removed:

> Retro-3DO is **not a clean-room project**. During development,
> FreeDO-descended Opera code in `4DO-Android/app/cpp/native_core/` and later
> the sanctioned local Opera tree were read and used as a compatibility
> reference. Some routines were initially close ports. Replacing those
> routines later does not undo that prior exposure, so this repository must
> not claim that it was independently developed without knowledge of FreeDO
> or Opera.

## What the September 2026 retrospective changed

The same record's implementation audit, which is why "the C++ core" could be
used as a written specification for this crate at all. Every routine
previously listed as a direct algorithm port was revisited; each current
implementation was rebuilt around a public hardware description and a
project-owned structure, and the Rust port works from those rebuilt routines:

| Area | Current implementation | Primary authority |
|---|---|---|
| ARM60 | Decode cache and ARMv3 interpreter; value-range ARM6 Booth timing | ARM DDI 0100I; 3DO SDK ARM6 performance guide |
| MADAM matrix | Generic signed 16.16 row dot-products and explicit project/divide stage | 3DO SDK `operamath.h`; WO 94/10641 |
| CEL rasterisation | Conventional active-edge scan conversion with half-open edges and paired crossings | WO 94/10644; standard polygon scan conversion |
| CEL visibility | Bounding geometry plus local tangent cross-products at the four projected corners | WO 94/10644 |
| LR-form CEL | Explicit row-pair/word/half traversal | Official Graphics Programming Guide and SDK `hardware.h` |
| DSPP | Field decoder, operand-group packets, explicit write-back selection, readable branch conditions, table-shaped ALU and barrel shifter | WO 94/16383 |
| SPORT | Explicit protected/source bit fields: `(destination & mask) \| (source & ~mask)` | System architecture material and BIOS SPORT tests |
| VDLP | Scanline-latched state machine and project pixel/interpolation path | Official Graphics Programming Guide; US 5,502,462; US 5,742,778 |
| CLIO | Project interrupt/timer/DMA register model | SDK headers, system patents, ROM disassembly and 3DOessence register documentation |
| XBUS/PBUS | FIFO bus and daisy-chain packet models | WO 94/16382, WO 94/10636, SDK CD/controller definitions |

The narrower, accurate conclusion stands: the current algorithms in the old
direct-port list have been independently restructured and are backed by public
specifications and project tests, **but the project remains historically
reference-exposed.**

## Behaviour-only fallback facts still retained

These are not copied routines. They are exact compatibility choices first
resolved with reference help and retained because public documentation is
ambiguous or describes only the surrounding protocol:

- a raw VDL persist count of zero can be replaced before displaying a line;
- the fitted CD drive presents the loaded/ready/spinning `0xE1` state used by
  the stock boot path;
- several uninteresting CLIO reset/latch values and CD reply details were
  parity-checked against Opera after the public-source implementation existed.

Each is isolated and covered by a ROM path, title trace or focused regression.
If physical-hardware measurements become available, those measurements should
replace the fallback authority without changing the surrounding algorithm.

## Files carried across from the C++ core

These came from Crown Park Computing's own `cpp-reference/` tree — present in
this repository until 2026-11-24, now removed — and are Crown Park Computing's
work under the MIT licence. They are listed so that "carried across" is never
mistaken for "written fresh".

| File here | From | What it is |
|---|---|---|
| `app/src/demo_rom.bin` | `cpp-reference/core/demo_rom_data.h` | The project's own 1,794-byte hardware demonstration ROM, assembled from `cpp-reference/demo/retro3do-demo.s`. Not a BIOS; cannot boot a commercial disc. |
| `app/include/retro3do.h` | `cpp-reference/include/retro3do.h` | The public C interface, unchanged in shape. |
| `app/tests/c/abi_c_consumer.c` | `cpp-reference/tests/abi_c_consumer.c` | The smallest C consumer, used to check the ABI. |
| `app/src/memory.rs` `FORMATTED_NVRAM` | `cpp-reference/core/bus.cpp` `kFormattedNvram` | The formatted-empty NVRAM image the boot ROM writes. |
| `docs/HARDWARE.md` | `cpp-reference/docs/HARDWARE.md` | The per-module hardware notes; `core/*` paths there map to `app/src/*` per its header table. |
| `docs/OPERA_AUDIT.md` | `cpp-reference/docs/OPERA_AUDIT.md` | The parity audit history. |
| `docs/FMV_STREAMING_HANDOVER.md` | `cpp-reference/docs/FMV_STREAMING_HANDOVER.md` | The CD-FIFO pacing investigation behind FMV streaming. |

## On the word "Opera"

"Opera" is The 3DO Company's own codename for the hardware platform. It is why
the 3DO SDK ships `operamath.h`, why the disc filesystem is called the Opera
filesystem, and why `app/src/xbus.rs` describes XBUS as "the Opera expansion
bus". Where this repository uses the word in that sense it refers to the
hardware, not to the opera-libretro project. The opposite sense — the emulator
— appears only in this file and in `AUDIT.md`, where it is the subject of the
audit.

## What is not here

No retail 3DO BIOS, no game, no disc image, and no Portfolio OS material is
included in this repository or required to build it. Whoever supplies a BIOS
or a disc image is responsible for holding the rights to it.

Third-party code and art, with licences, is recorded in
[THIRD_PARTY.md](THIRD_PARTY.md).
