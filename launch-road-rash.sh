#!/usr/bin/env bash
# AI Deck launcher: boot Road Rash with the legally-present local BIOS/disc.
exec /home/jon/retro3do-rs/app/target/release/retro3do-sdl \
  --bios /home/jon/3do-library/bios/panafz10.bin \
  --disc "/home/jon/3do-library/games/Road Rash (USA).chd" \
  "$@"
