# Launcher: flow, scene, and what suspends in-game

Working notes for the FZ-1 launcher (retro3do-gui). Uncommitted until the
visuals are signed off.

## Flow

1. **Open** — the FZ-1 retail carton sits sealed, the lid swings back, the dust
   flaps fold out, the machine rises out of its foam, the carton drops away and
   the machine settles (`carton_pose` in fz1_3d.rs, `UNBOX_SECS` = 9 s). A
   click skips it. Nothing boots: `--boot`/`--play` only point the shelf at a
   title. An empty machine opens its drawer when it is out of the box.
2. **Shelf** — its own strip under the machine view. Every game spine-out at
   the real spine proportion (146:2156): the media folder's `spine/` scan where
   there is one, otherwise a stock-coloured spine with the cleaned title
   (`case_art::clean_title`, no region tags) set down its length. Wider than the
   window → wheel scrolls it.
3. **Single click** a spine → the case opens in the upper view (case.glb, the
   sleeve composed by `case_art::wrap`, the disc beside it). Drag turns it over
   to read the back; wheel zooms; **Insert disc** or **Back** / Esc.
4. **Double click** (or Insert) → `start_load`: the running game (if any) stops
   and its disc is ejected, the drawer runs out, the old disc lifts away, the new
   disc — wearing the game's box cover, cut square, right way round — comes down
   onto the platter, the drawer shuts, the platter spins up for `SPINUP_SECS`,
   then the machine is switched on. One path for the first game and for a swap.
   A click skips straight to power-on.
5. **In game** — Esc or the ☰ corner sets the game aside (paused) and shows the
   launcher; Esc / "Back to the game" resumes.
6. **Close** — the window close is cancelled once: the game stops and saves, the
   drawer shuts, the box-away (the unboxing in reverse, `BOXAWAY_SECS` = 5 s)
   plays, then the window closes. A second close request exits at once.

No turntable: the machine and the case turn only when dragged.

## Scene

`fz1_3d.rs` draws three models: fz1.glb (Console), fz1-box.glb (Carton),
dvd-case/case.glb (Case). Baked textures in the files are uploaded once; the
game-dependent pictures are three slots (`ArtSlot::Platter`, `Wrap`,
`CaseDisc`) the GUI fills from worker threads. 4x MSAA. Target 0 is the machine
view, target 1 the case view.

Models are generated: `python3 docs/make-fz1-model.py` (console + carton),
`blender -b -noaudio --python docs/make-case-model.py` (case). The FZ-1's lamps
sit on the pods just under the bronze strips (red under POWER, green under
OPEN/CLOSE); the controller socket is centred under the fascia.

## In-game suspension

Once the picture has the window (`game_live`), `animate`/`advance_load` stop,
the wallpaper is not drawn, and after the half-second crossfade the renderer is
dropped and its egui textures freed (`suspend_scene`). Coming back rebuilds it
and re-asks the art.

## Checks

* `cargo run --release --example fz1_shot OUT` / `case_shot OUT` — offscreen
  renders of every side, the drawer open, the unboxing stages, and cases.
* `RETRO3DO_SCRIPT="1:shot:a;11:case:Doom;15:load:Road Rash;53:close"` with
  `RETRO3DO_SHOT_DIR` drives the real window and screenshots it. Actions:
  `shot:NAME case:GAME load:GAME esc click close`.
* `RETRO3DO_FRAME_LOG=1` prints present/field gap statistics.
