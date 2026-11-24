# Task progress (2026-11-20)

## 1. Disc texture bug — DONE, verified

Root cause: `platter_art()` in `app/src/bin/retro3do-gui.rs` dressed the tray disc
with `game.cover` (the box2d cover) first; `media/3do/discs/` was never consulted.

Fix (app/src/bin/retro3do-gui.rs, app/src/bin/retro3do-gui/fz1_3d.rs):
- `scan_games` now scans the media `discs` set and resolves it first
  (`resolve_disc_scan`), falling back to `cartridges` art.
- `resolve_disc_scan` matches the title exactly (or disc-1 of a set) so one
  game's scan never stands in for another's.
- `platter_art` no longer falls back to the cover: disc art, else the plain
  drawn platter (`title:` sentinel → `case_art::disc` with no art).
- `disc_label` now returns the resolved disc art; `media_for` deleted.
- `request_platter` renders the scan through `case_art::disc` (same cut as the
  disc beside the case).

Verified in the real window: `RETRO3DO_SCRIPT="4:load:Doom;..."` — the disc on the
FZ-1 tray shows the Doom disc scan (DOOM label art, black hub), not the upright
3DO cover. Evidence: /tmp/shots2/{f5.png,cmp.png}.

## 2. Case-view framing — DONE, verified
Root cause: the 3D camera framed *every* non-carton model, so the case view
framed console+case together and the case came out tiny.
- fz1_3d.rs `render_into`: each view now frames its own model — the machine
  view the console, the case view the case alone.
- The case is now ~4x bigger on screen (was ~90x135px in a 750px window,
  now ~300x480px in a 1009px window). Orbit room kept: case_zoom stays 1.0
  and the wheel still zooms (0.5..2.5).
- Caption band behind the title/hint so they read over the case.
- Shelf strip: `shelf_h` 0.34 -> 0.22 of the height, clamped 120..210, and at
  most 20 spines on show at once (`SHELF_MAX_SPINES`).

## 3. In-game chrome — DONE, verified
- Removed the hover bar (pause/reset/power-off) from the picture.
- The hamburger menu now carries what the bar had: "Back to the game",
  "Reset", "Power off" (the menu already pauses on open).
- Verified in the real window with Doom booting: only the hamburger shows.

## 4. Box lid hinge — DONE
The lid hinges at the rear top edge and opens 160°; the flaps fold out flat.
See section 5 for the carton's proportions and the unboxing it drives.

Tree left dirty (standing instruction: nothing committed without approval).

## 5. Unbox choreography + carton proportions — DONE, verified
- `docs/make-fz1-model.py`: carton 360 x 290 x 130 mm (wide, shallow), face
  textures resized to the new aspects, foam bed + an insert either side of the
  console standing on its rear panel, dust flaps only as wide as the box is deep.
- `fz1_3d.rs` `carton_pose`: new `tilt`. The machine rises straight up on its
  rear panel (drawer face to the sky), clears the rim, then swings down flat
  and settles; `lift()` pitches it about PIVOT_Y above its feet.
- Drawer stays shut while boxed: `tray_t`/`open` forced 0 in `render_into`,
  and `tray_goal` forced 0 in `animate` so it cannot snap open at the end.
- Verified: /tmp/unbox/sheet.png (offscreen, tray=1 at u=0.70/0.78 shows no
  drawer) and the real window /tmp/win/{a..d}.png.

## 6. Eject, drawer travel, shelf, header, settings page — DONE, verified
- fz1_3d.rs: `TRAY_TRAVEL = 0.150` m (was 0.13) — the platter's rear edge now
  passes the front face, so a CD drops on flat. Camera pull-back 0.15→0.24·open.
- gui: `toggle_tray()`/`eject()` — OPEN/CLOSE during a load cancels it; while
  running it stops+saves the machine and empties the drive. "⏏ Eject" added to
  the frontend row.
- Shelf strip 0.44→0.528 of the height (clamp 360..672) = spines 20% taller.
- In-GUI "RETRO 3DO" title gone (header 44→30 px, game title kept); the
  "pad connected" lines gone from the status panel.
- New Settings page (cog in the header, "⚙ Settings" in the frontend row):
  pad select + rebind (sdl_pad runtime bindings), CPU overclock 100–150 %,
  smooth/crisp picture filter, games/bios/artwork folders. All persisted to
  launcher.conf (smooth, cpu_scale, pad_primary, pad_bind).
Verified in the real window and offscreen:
- `fz1_shot` `open-top`/`open-side` (new views): the platter now clears the
  console's front face completely — a CD drops onto it flat from straight
  above. /tmp/model/open-{top,side}.png
- Eject during a load: `2:click;4:load:Doom;...;9.6:tray` -> the launcher comes
  back with "load cancelled - the disc came out" and nothing boots.
  /tmp/vA/c_cancel.png
- Eject while running: `...;26:esc;28:tray` -> the machine stops, the drawer
  runs out empty and the shelf is back. /tmp/vB/h_halted.png
- Shelf spines a fifth taller, no "RETRO 3DO" title, no "pad connected" line,
  cog in the header opens the settings page with the pad list, the rebind
  table, the overclock slider and the folder fields. /tmp/vA/d_settings.png
- Smooth rendering: toggling it drops the picture's edge energy 5.39 -> 4.77
  (same region, same frame), so the bilinear filter really is in force.
- Settings round-trip: cpu_scale = 137 and a hand-written pad_bind survive a
  load-and-save cycle of launcher.conf unchanged.
- The whole settings page rendered and read in the real window (833x706):
  rebind rows, stock mapping, the overclock slider, the smooth checkbox and
  the three folder fields. /tmp/vC/s3.png
- `read these folders` re-reads the library: "11 games, 10 BIOS files" on the
  status line. /tmp/vC/s4.png
- `sdl_pad` tests: the stock mapping survives being written to the settings
  file and read back, and names this build does not know are dropped.
  24 tests pass.

Not driven from the harness: the rebind listener needs a real pad button held
down, which the script cannot fake (it reads SDL's live state). The row's
listening state and the saved binding were both checked instead.

New script verbs for the harness: `tray` (OPEN/CLOSE), `settings`,
`scroll:<px>` (the settings page), `dirs` (read the folder fields), `smooth`.

## 7. Chrome polish + startup spin — code done, verified in the real window

- Header panel gone (`egui::Panel::top("header")` deleted). The hamburger and
  the cog are one floating pair in the top-right corner, drawn from
  `chrome_icons()` in an `egui::Area` over everything (`icon_button()`:
  glyph on a dark pad, no frame).
- `chrome_shown()` hides all overlay chrome (both icons and the disc-swap
  strip) while `game_live()`, three seconds after the last pointer/touch/key
  event (`CHROME_IDLE`), and brings it back on the next one. Non-game states
  always show it. `request_repaint_after(CHROME_IDLE)` so the hide happens
  without a new field.
- Hamburger = `chrome_menu()`: the shelf from a running game, the way back
  into the game from the launcher over it. Cog = `open_settings()`.
- Startup spin: the show is now `INTRO_SECS = SPIN_SECS (6) + UNBOX_SECS (9)`.
  `intro` 0..SPIN_FRAC turns the sealed carton a full eased turn about `YAW0`
  (`smoothstep`), then `fz1_state.unbox` runs 0..1 as before. The shelf's fade
  keys off `unbox` now, not `intro`. Clicking (or the script's `click`) skips
  to `intro = 1`, `yaw = YAW0`, `spun = true`.
- New script verbs: `poke` (the touch the chrome wakes on — the harness cannot
  inject real pointer events) and `menu` (the hamburger's action).

Verified, /tmp/ch (1207x1424 window): spin1/2/3 differ by 18-20 mean grey
(the carton really turns), unbox/shelf show no header strip, the icon pad
carries 264 glyph pixels in the launcher, the frontend over a paused game and
in-game right after `poke`; 0 glyph pixels in-game 6 s after the last poke
(pure picture), 263 again after the next poke. No errors or panics in the log.

## 8. Preview video — DONE, verified

`docs/preview/road-rash-boot.mp4` — 57.8 s, h264 720x846, 15 MB, the real
window filmed with wf-recorder on the Hyprland session (no mock, no offscreen
path): launch -> the carton's presentation spin -> unboxing -> shelf -> the
Road Rash case -> the disc going in -> the drawer closing -> boot -> ~18 s of
Road Rash running.

Recipe (`/home/jon/preview-rec/record2.sh`, outside the repo):
`hyprctl -j clients` for the window's geometry, then
`timeout -s INT 58 wf-recorder -o DP-1 -g "$geom" -F scale=720:-2 -r 30
-c libx264 -p preset=ultrafast -p crf=24 -f raw.mkv`, with the app driven by
RETRO3DO_SCRIPT and left running until the recorder has finalised the file.
Two things the first take got wrong: at full 1212x1424 the box could not
encode 30 fps (402 frames in 58 s), and letting the app close first truncated
the matroska.

Stills (1212x1424, the app's own screenshot path): `docs/preview/shelf.png`,
`case.png`, `insert.png`, `gameplay.png`.

Verified: ffprobe reads 57.766 s / 1733 frames, and frames pulled at
2/5/9/13/16/19/24/27/34/45/55 s show, in order, the sealed box turning (rear,
front, side), the carton open with the machine rising, the machine on the
table with the shelf in, the case view, the Road Rash cover, the disc on the
tray, the drawer shut, and Road Rash playing. The stills also show the chrome:
the icon pad is there on the launcher frames and gone in `gameplay.png`.

## 9. Settings as a page of its own — code done

The settings window is gone. Settings is now a third `View` (`View::Settings`)
drawn by `view_settings()` straight into the central panel, so it has the whole
window and nothing is squeezed over the 3D scene:

- a top bar (`PANEL`, 74 px) with `‹ Back` and the page's name;
- the sections down the left (`SETTINGS_TABS`: Controller, Video, Paths,
  System) as `egui::Button::selectable` rows, one on screen at a time;
- the section itself in the rest of the window, its measure capped at 720 px so
  the text never runs the full width of a wide window, each group in a `CARD`
  frame (`settings_card()`) under a heading and a line of blurb
  (`settings_head()`).

Navigation: the cog (and the in-game hamburger's cog) calls `open_settings()`,
which remembers the view it came from in `settings_back`; `‹ Back` and Escape
call `close_settings()`, which returns there. `settings_open` is gone — being
on the page *is* `view == View::Settings`.

Paths gained `browse…` buttons: they reuse the folder picker with steps 11/12/13,
which the picker's "use this folder" now writes into the page's three fields.
System gained the BIOS choice (it only lived in the first-run walkthrough
before) beside the CPU overclock.

New script verbs: `settings-back`, `section:<name>`.

Verified in the real window (RETRO3DO_SCRIPT, /tmp/s9): `settings` opens the
page over the launcher — `‹ Back` + "Settings" top bar, the four sections
listed down the left, the active one lit. `section:Video/Paths/System` each
swap the right-hand column and only it (`ctrl.png`..`system.png`).
`settings-back` returns to the shelf (`back-shelf.png`), and opened over a
loading game it returns to the machine view it came from (`back-game.png`);
the last-visited tab persists between opens. `cargo test` 390 pass, clippy
clean.

## 7. Shelf hidden during boot; insert/eject as floating marks — DONE, verified
- `view_machine()`: the shelf's alpha is forced to 0 while a disc is on its way
  in (`load.is_some()`) or the machine is on with a disc in it
  (`emu.running() && !frontend`). It comes back only in the choosing state
  (menu open over a running game) or after an eject cancels the boot.
- `tray_icon()` (new, beside `icon_button`): the disc marks are drawn, not
  typed — an arrow over the tray's bar, up and out to eject, down and into it
  to insert, the same mark turned around.
- The floating corner (`chrome_icons`) now carries insert and eject beside the
  hamburger and the cog: insert when a case is open, eject when a disc is
  loaded or loading. The "▶ Insert disc" button in the case view and the
  "⏏ Eject" button in the frontend row are gone; the marks do both jobs.
- The marks follow the old auto-hide rule: hidden once the game has the
  window, back on the next touch.

Verified in the real window (RETRO3DO_SCRIPT, ~/.cache/r3tmp/shots-ui{,2}):
`library.png` shelf + menu/cog only; `case.png` the insert mark in the corner;
`boot.png`/`boot2.png` no shelf at all, the eject mark showing; `eject.png`
the shelf back after the eject cancels the load ("load cancelled - the disc
came out"); `live.png`/`live2.png` the game picture with no chrome. 24 GUI
tests pass.

The marks also wake on input like the rest of the chrome. New script verb
`key` pushes a key press into egui's event list (the pad reads the key
state, not the events, so the game does not see it) — xdotool/ydotool never
reach the window on this box. Verified in the real window
(~/.cache/r3tmp/shots-ui5): `hidden.png` the game with a bare window,
`woken.png` a second after the key the eject mark, the hamburger and the cog
are back, `slept.png` five seconds on they are gone again.

## 8. Finalisation + AI Deck registration — DONE, verified (2026-11-21)
- `app/cargo build --release`: clean (no source newer than the release binaries).
  `cargo test --bin retro3do-gui`: 26 passed, 0 failed.
- Headless verification: `in_game_check panafz10.bin "Road Rash (USA).chd"`
  → 59.97 fields/s, 0 deadline resets; the final frame is the in-game 3D
  street/bike/HUD (`~/ai-scratch/3do-rr3do-gameplay.png`). `probe` frames at
  1200/2400 confirm the race, not just one lucky frame.
- `~/3do-work/rr3do.iso` (raw ISO) does NOT boot here: 2400 fields of `probe`
  sit on the BIOS "Insert CD" screen. The CHD dump of the same disc boots.
- AI Deck `ready.json`: `retro3do-rs`, state `TEST`, launch
  `/home/jon/retro3do-rs/launch-road-rash.sh` (chmod +x), args `[]`, cwd repo.
  Wrapper execs `retro3do-gui --games-dir /home/jon/3do-library/games --bios-dir /home/jon/3do-library/bios` (the SDL front end is gone);
  launch checked with a 10 s timeout — renderer `vulkan`, no errors.

## 9. Retail boot milestone re-verified — DONE (2026-11-24)
Road Rash (USA).chd on panafz10.bin reaches input-driven gameplay end-to-end.
Re-ran the whole chain on the current tree (no source changes needed):
- `cargo build --release`: clean. `cargo test --bin retro3do-gui`: 28/28 pass.
- `probe panafz10.bin "Road Rash (USA).chd" ~/ai-scratch/3do-retail 2700`:
  frame_01200 = title screen ("ROAD RASH — Presented in DOLBY SURROUND"),
  frame_02400 = in-race 3D street with HUD (Player 1 / speed / Buzz).
  301.7 fields/s unthrottled, mean 3.31 ms/field.
- `in_game_check` (the threaded paced path front ends use, scripted A/Play taps
  through the menus): **59.97 fields/s, 0 late fields, 0 deadline resets**,
  final frame = live race (`~/ai-scratch/3do-retail/ingame.png`).
Evidence dir: `~/ai-scratch/3do-retail/` (probe frames + ingame.png + logs).
Known gap (unchanged): `~/3do-work/rr3do.iso` raw ISO still sits on the BIOS
"Insert CD" screen; the CHD dump of the same disc is the working path.
No commits made, per scope.

## 10. Consolidation epic — old paths removed, graphics wired, clean-room docs written (2026-11-24)

**Item 1 — old paths gone (clean-room only).** `cpp-reference/` (the
Opera-knowledge C++ core) is deleted from the tree; `app/src/bin/retro3do-sdl.rs`
is deleted, so the SDL front end is no longer a bin target — `app/src/bin/` now
holds only `retro3do-gui.rs`. `launch-road-rash.sh` execs `retro3do-gui` with
`--games-dir`/`--bios-dir`. The last two doc references to `retro3do-sdl`
(PLAN.md Milestone 1b, and the item-8 wrapper note above) were retargeted so no
stale name survives. `docs/PROVENANCE.md` records that the C++ tree was read as
reference but no longer ships; `docs/CLEANROOM.md` (new) tells the whole
documentation-driven method, and `README.md` links it.

**Item 2 — graphics options live.** The dead texpack path is now wired:
`apply_textures()` in `app/src/bin/retro3do-gui.rs` calls
`emu.set_texture_dir(dir.as_deref())` on every apply, where `texpack_dir()`
returns the configured `--texdir`/`RETRO3DO_TEXDIR` directory when set, else
probes `texpacks/<title>` beside the games folder (disc stem normalised, spaces
out — `Road Rash (USA)` -> `roadrash`). The settings pane exposes internal
render scale, smooth filter, the 100–150 % `cpu_scale` slider and the texture
pack directory field with a folder picker; the enhanced path (2x internal +
enhance) is on by default, so enhanced graphics are the out-of-the-box
experience rather than a hidden opt-in.

**Item 3 — documentation.** `docs/CLEANROOM.md` (132 lines) covers: which
public spec each hardware module follows (pulled from `docs/SOURCES.md`), the
knowledge lineage (FreeDO/Opera knowledge -> the C++ core as spec of record ->
this Rust crate, all read-only as reference), the audit method
(`docs/audit.sh` + `docs/AUDIT.md`), and what a reader needs to reproduce the
methodology. The repo now stands on its own without `cpp-reference/` present.

**Item 4 — video path.** The complete video path is the VDLP field walk plus
the CEL pixel decoder (`app/src/madam/decoder.rs`, the pixel processor of
WO 94/10644: PLUT selection, per-channel AMV multipliers, transparency, saved
V/H bits) plus the internal-scale/enhance/scaler stages. Evidence from the
milestone-9 run on this tree: `probe` frame_01200 = Road Rash title screen,
frame_02400 = in-race 3D street with HUD, at 59.97 fields/s with 0 late fields
and 0 deadline resets (`~/ai-scratch/3do-retail/`). On Cinepak specifically:
the 3DO's CEL engine has no Cinepak mode — Cinepak cels are decoded by the
ARM60 running the game's own decoder (the SDK shipped one), so there is no
hardware decoder to emulate and the FMV path is an ARM60-correctness question,
not a missing-codec question; `~/3do-work/cinepak.c`, `cinepak3do.py` and
`cpk.txt` are the prior research reference for that format. This session did
not re-run a dedicated FMV capture, so the item-4 claim rests on the milestone-9
gameplay evidence plus the format reading above, not on a fresh FMV frame.

Verification on the finished tree:
- `cargo build --release`: clean. `cargo test`: 398/398 pass
  (`cargo test --bin retro3do-gui`: 28/28; `cargo test --test texpack`: 3/3).
- `cargo clippy --all-targets -- -D warnings`: clean. One test-only
  `expect("case.glb")` in `fz1_3d.rs` was replaced with an assertion to get
  there.
- `bash docs/audit.sh`: all four checks pass; 51 Rust files, 398 `#[test]`
  functions.
- `launch-road-rash.sh` smoke in the real window: the shelf screenshot path
  works, `load:road rash` resolves the title and reaches the boot sequence,
  and the scripted close exits cleanly (`/tmp/r3smoke/{shelf,boot}.png`,
  `/tmp/retro3do-{launch,load}.log`).
- `hires_shot panafz10.bin "Road Rash (USA).chd" 2400 60 2
  /tmp/r3smoke/roadrash-2x.png`: 1280x960 live-race frame, 11.38 ms/field
  mean, 2 hi-res surfaces — the 2x/enhanced path exercised on the retail
  disc, not just compiled.
- `cargo fmt --check` remains noisy across the pre-existing tree (thousands
  of unrelated diffs), so it was not used as this item's gate.

No commits made, per scope.

**Item 5 — in-app knowledge surface (tester feedback 2026-11-24).**
`retro3do-gui/wiki.rs` bundles `docs/KNOWLEDGE.md` (player summary + per-core
public specs) and `docs/CLEANROOM.md` (method) via `include_str!`, so app text
and docs cannot drift. The settings page carries a `SettingsTab::Wiki` with
page selector + five clickable hyperlinks to `github.com/CrownParkComputing/fz10`
(`/wiki` and the four docs). Reuses the existing docs-dir pattern's Markdown
reader. Re-verified this session: `cargo test` 407/407 pass (was 398 — the wiki
tests added), release binary rebuilt fresh after all sources.

Final re-verification (2026-11-24, fresh session): `cargo test` in `app/`
ran 407 tests across all suites — 0 failed. Disk state confirmed: `docs/CLEANROOM.md`
+ `docs/KNOWLEDGE.md` present, README links CLEANROOM.md, `docs/PROVENANCE.md`
records the `cpp-reference/` removal (lines 37-38, 109-110), `cpp-reference/`
and `app/src/bin/retro3do-sdl.rs` absent from the tree, `wiki.rs` (in-app
knowledge surface) present with its link set. No commits made, per scope.
