# FZ-10

The 3DO Interactive Multiplayer emulation core, in safe Rust, with a C ABI
(`retro3do.h`). One front end: `retro3do-gui` (egui launcher; SDL3 for audio
and gamepads).

- `app/` — the crate: ARM60 CPU, CLIO/Madam hardware, disc/CHD, audio ring,
  the GUI front end and the headless check tools.

## How this emulator was written

[docs/CLEANROOM.md](docs/CLEANROOM.md) — the documentation-driven method:
which public specification each hardware module follows, what was read as
reference versus written fresh, and how to reproduce the audit.

## Provenance, sources and licensing

Read these before redistributing or shipping anything built from this tree:

- [docs/PROVENANCE.md](docs/PROVENANCE.md) — where every part of the
  implementation came from, including what was *not* clean-room.
- [docs/SOURCES.md](docs/SOURCES.md) — the public specifications each hardware
  module follows, per module.
- [docs/AUDIT.md](docs/AUDIT.md) — the checks that show no third-party
  emulator source is present here, with the commands to re-run them.
- [docs/THIRD_PARTY.md](docs/THIRD_PARTY.md) — vendored code, dependencies and
  art, with their licences. No unresolved items.
- [LICENSE](LICENSE) — proprietary, all rights reserved; copying or use requires written authorization from Crown Park Computing.

No retail BIOS, game or disc image is included or required to build. The
bundled demonstration ROM is Crown Park Computing's own hardware self-test and
cannot boot a commercial disc.

## Verified 2026-11-24

- `app/`: clean `cargo build --release`; `cargo test` 407/407 pass
  (`cargo test --bin retro3do-gui`: 28/28); `cargo clippy --all-targets --
  -D warnings` clean; `bash docs/audit.sh` passes all four checks.
- Headless `hires_shot` with `~/3do-library/bios/panafz10.bin` and
  `~/3do-library/games/Road Rash (USA).chd`: 2,400 fields at 2x internal
  scale with texture enhancement, reaching the live race at 1280x960 in
  11.38 ms/field mean.
- Headless `in_game_check` with `~/3do-library/bios/panafz10.bin` and
  `~/3do-library/games/Road Rash (USA).chd`: 59.97 fields/s, 0 deadline resets;
  the captured last frame is the Road Rash race, not a BIOS or black screen
  (`~/ai-scratch/3do-rr3do-gameplay.png`).
- The raw `~/3do-work/rr3do.iso` image is not recognised by this BIOS/core: it
  stays on the BIOS "Insert CD" screen for 2400 fields. The CHD dump of the same
  game is the working media.
- Registered on the AI Deck as `retro3do-rs` (state `TEST`): `launch-road-rash.sh`
  execs `retro3do-gui` with the local library and scripts a `load:road rash`.
