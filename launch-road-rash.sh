#!/usr/bin/env bash
# AI Deck launcher: boot Road Rash in the egui front end with the
# legally-present local BIOS and library. `load:` matches a title fragment,
# so the disc does not depend on library ordering.
export RETRO3DO_SCRIPT="${RETRO3DO_SCRIPT:-3:load:road rash}"
exec /home/jon/retro3do-rs/app/target/release/retro3do-gui \
  --games-dir /home/jon/3do-library/games \
  --bios-dir /home/jon/3do-library/bios \
  "$@"
