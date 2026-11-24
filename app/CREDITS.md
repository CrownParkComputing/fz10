# Credits and sources

This crate is a Rust rewrite of the Retro3DO C++ core (Crown Park Computing,
MIT). The C++ reference was read as the specification; no source from any
other emulator was used. The reference's own provenance record — shipped at
`cpp-reference/docs/PROVENANCE.md` until that tree was removed on 2026-11-24,
now folded into `docs/PROVENANCE.md` — applies to the hardware knowledge
carried over and must travel with this crate.


Repository-level provenance, the per-module source register, the
no-copied-source audit and the third-party/licence record are in
`docs/PROVENANCE.md`, `docs/SOURCES.md`, `docs/AUDIT.md` and
`docs/THIRD_PARTY.md`. This file is the per-file carry-over list only.

## Files carried over from the reference

| File here | Source | What it is |
|---|---|---|
| `src/demo_rom.bin` | `cpp-reference/core/demo_rom_data.h` (bytes of `cpp-reference/demo/retro3do-demo.s`) | The project's own hardware demonstration ROM, 1,794 bytes, SHA-256 `f019b2a8…0a4b`. Crown Park Computing, MIT. |
| `include/retro3do.h` | `cpp-reference/include/retro3do.h` | The public C interface, unchanged in shape; comments now say Rust. |
| `tests/c/abi_c_consumer.c` | `cpp-reference/tests/abi_c_consumer.c` | The smallest C application, used to check the ABI. |
| `src/memory.rs` `FORMATTED_NVRAM` | `cpp-reference/core/bus.cpp` `kFormattedNvram` | The formatted-empty NVRAM image the boot ROM writes. |

No retail BIOS, game, disc image or Portfolio material is included.

## Public specifications the hardware model follows

As recorded in the reference's PROVENANCE.md: 3DO SDK 2.5 headers and the
Graphics Programming Guide; WO 94/10636, 94/10641, 94/10643, 94/10644, 94/16382,
94/16383; US 5,502,462 and 5,742,778; ARM DDI 0100I. The CHD container format
was read from libchdr's source (BSD-3-Clause) as a format description; the
reader here is new code and links no C.

## Rust dependencies

miniz_oxide (MIT/Zlib/Apache-2.0), lzma-rs (MIT),
ruzstd (MIT), png (MIT/Apache-2.0, check tool only).
