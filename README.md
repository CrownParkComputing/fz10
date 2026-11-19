# opera-rs

The 3DO Interactive Multiplayer emulation core, in safe Rust, with a C ABI
(`retro3do.h`). SDL3 frontend + launcher (`retro3do-gui`, `retro3do-sdl`).

- `app/` — the crate: ARM60 CPU, CLIO/Madam hardware, disc/CHD, audio ring
- `cpp-reference/` — the C++ codebase this port is based on
