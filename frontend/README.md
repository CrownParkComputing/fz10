# frontend/ — Retro-3DO launcher UI (reference)

The launcher/frontend from the original Retro-3DO project: ImGui game
library, covers, guide, file browser, platform/storage shell, branding
assets, and store `play/` listings.

**No emulator core here.** The frontend was never part of the emulation
core — `src/core/` holds only the launcher-facing support files
(covers, guide, settings, pad layout, console interface header), not the
ARM60/CLIO/Madam emulation. The shipping emulator is the clean-room Rust
core under `app/`; the in-app launcher is `app/src/bin/retro3do-gui/`.

This tree is a reference snapshot — it is not wired into the Rust build.
Same license as the root LICENSE: proprietary, all rights reserved.
