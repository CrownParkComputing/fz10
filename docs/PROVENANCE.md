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
- Crown Park Computing's own C++ core in `cpp-reference/`, used as a written
  specification; and
- the project's own test suite and demonstration ROM.

## The route this code took, in full

**This is not a clean-room project, and it must not be described as one.**

The chain is:

    FreeDO / 4DO / Opera  ->  Retro3DO C++ core  ->  this Rust crate
         (third party)         (Crown Park)          (Crown Park)

The first link is the one that matters and it is disclosed in
`cpp-reference/docs/PROVENANCE.md`, which is part of this repository. In
summary, and without softening it: during the C++ core's development,
FreeDO-descended Opera code was read and used as a compatibility reference, and
some routines in that core were initially close ports. Those routines were
later rebuilt against public hardware documentation, but replacing a routine
does not undo the earlier exposure.

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

## Files carried across from the C++ core

These came from Crown Park Computing's own `cpp-reference/` tree and are
Crown Park Computing's work under the MIT licence. They are listed so that
"carried across" is never mistaken for "written fresh".

| File here | From | What it is |
|---|---|---|
| `app/src/demo_rom.bin` | `cpp-reference/core/demo_rom_data.h` | The project's own 1,794-byte hardware demonstration ROM, assembled from `cpp-reference/demo/retro3do-demo.s`. Not a BIOS; cannot boot a commercial disc. |
| `app/include/retro3do.h` | `cpp-reference/include/retro3do.h` | The public C interface, unchanged in shape. |
| `app/tests/c/abi_c_consumer.c` | `cpp-reference/tests/abi_c_consumer.c` | The smallest C consumer, used to check the ABI. |
| `app/src/memory.rs` `FORMATTED_NVRAM` | `cpp-reference/core/bus.cpp` `kFormattedNvram` | The formatted-empty NVRAM image the boot ROM writes. |

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
[THIRD_PARTY.md](THIRD_PARTY.md). That file currently contains unresolved
items and they are marked as such.
