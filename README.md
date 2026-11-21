# FZ-10

The 3DO Interactive Multiplayer emulation core, in safe Rust, with a C ABI
(`retro3do.h`). SDL3 frontend + launcher (`retro3do-gui`, `retro3do-sdl`).

- `app/` — the crate: ARM60 CPU, CLIO/Madam hardware, disc/CHD, audio ring
- `cpp-reference/` — Crown Park Computing's own C++ core, used here as the
  written specification this Rust implementation was worked from. Its
  provenance record travels with it; see [docs/PROVENANCE.md](docs/PROVENANCE.md).

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
- [LICENSE](LICENSE) — MIT, covering Crown Park Computing's own work only.

No retail BIOS, game or disc image is included or required to build. The
bundled demonstration ROM is Crown Park Computing's own hardware self-test and
cannot boot a commercial disc.

## Verified 2026-11-21

- `app/`: clean `cargo build --release`; `cargo test --bin retro3do-gui` 26/26 pass.
- Headless `in_game_check` with `~/3do-library/bios/panafz10.bin` and
  `~/3do-library/games/Road Rash (USA).chd`: 59.97 fields/s, 0 deadline resets;
  the captured last frame is the Road Rash race, not a BIOS or black screen
  (`~/ai-scratch/3do-rr3do-gameplay.png`).
- The raw `~/3do-work/rr3do.iso` image is not recognised by this BIOS/core: it
  stays on the BIOS "Insert CD" screen for 2400 fields. The CHD dump of the same
  game is the working media.
- Registered on the AI Deck as `retro3do-rs` (state `TEST`): `launch-road-rash.sh`
  execs `retro3do-sdl` with the local BIOS and CHD. Wrapper launch checked.
