# Case-view notes (work in progress)

## Model

`docs/make-case-model.py` converts `app/assets/dvd-case/source/case.fbx` into
`app/assets/dvd-case/case.glb` (blender -b -noaudio --python ...). Named nodes:
`Wrap`, `CaseShell`, `CaseDisc`, `DiscRing`, `DiscHub`. Clear sleeve and
fingerprint overlay dropped.

## Wrap UV layout (derived from the source UVs)

The sleeve is one 284 x 190 mm wrap across U, V down the 190 mm height:

* `u = 0.7675 - 2.5 * y_blender` (y = the 135 mm axis) → 1 u unit = 284 mm
* `u 0.000 .. 0.4825` one cover, `0.4825 .. 0.5175` the spine (14 mm),
  `0.5175 .. 1.0` the other cover
* `v = 0.4985 + 5.34 * x_blender` (x = the 190 mm axis), v 0..1 over 190 mm
* the cover at `u > 0.52` is the face that pointed +Z in blender (so +Y in
  the exported glTF, since the export is Y-up)
* wrap aspect 284:190 = 1.495, which matches the sample `textures/gow2.jpg`
  (2566x1726 = 1.487) — the sample is a standard DVD sleeve scan

Disc label (`CaseDisc`) UV is a plain 0..1 square across the disc face
(bbox 0.021..0.979), so a square canvas works like the FZ-1 platter's.

## Placement

In the glTF the case lies flat: height axis = +X, width = +Z, front face
normal = +Y; the disc sits at z = +0.139. `place = rot_z(90°) * rot_x(90°)`
stands it up facing the camera (+Z) with the disc to its right.

## Renderer

`fz1_3d` is reused whole: `load_case()` + two more dynamic texture slots
(wrap, case disc) bound by part name, a `case` flag on `Fz1State` so the
case target orbits on `yaw/pitch/zoom` like target 0, and the case models
appended to `fz1_scene` so one `Fz1Renderer` covers both views.

## The shelf

`View::Shelf` is the entry into the case browser: the whole library stood
spine out on a board, browsed horizontally. It is plain egui painting — a
textured quad per case (`spine_quad`), no second 3D scene. The picked case
leans out of the row by widening the top edge of its quad, the row falls off
into the dark towards the screen edges, and `shelf_scroll` eases the row so
the picked spine sits in the middle.

* Spines: `media/spine/` scans where present (96x1024 strip via
  `case_art::spine`, letterboxed by `cover_fit`), a drawn title strip
  (`drawn_spine`) elsewhere — one of six papers picked by title hash so a
  shelf of them does not look like a comb.
* Widths vary: `shelf_thickness` hashes the title for a small jitter and
  fattens multi-disc sets, like a real shelf of uneven boxes.
* Walking: arrows, pad left/right, wheel, and drag all fold into a nudge
  accumulator; every 44 px of it steps one case. `case_step` wraps the ends.
* Taking one (enter, click on the picked spine, pad A/start) calls
  `take_from_shelf`: `View::Cases` opens with `yaw = -pi/2` so the case
  arrives spine-on exactly as it stood, then the view's usual sway turns it
  round to the front. Escape/B in the case view puts it back on the shelf.
* Only the spines on screen get their art asked for, so a big library does
  not decode hundreds of strips up front.

## Where it stands

Done: the model loads (`fz1_3d::load_case`), the renderer carries the two
extra artwork slots, `case_art` composites a sleeve and a disc label per
game (drawing stand-ins where the folder has no art), and `View::Cases` is
in the launcher with its own carousel, its header toggle and a launch that
goes through the drawer the way the shelf does.

The sheet goes onto the model exactly as a sleeve is printed — no flip, as
the renders proved; the spine is the face at yaw -pi/2 and reads top to
bottom. `cargo run --example case_shot -- OUT` renders the lot offscreen.

## What was checked

* `cargo test --release` is green: 22 targets, including three new tests in
  `case_art` (a game with no artwork still gets a full sleeve; the stand-in
  platter is round on a clear square; the spine fits its strip) and one on
  `case_step`, the carousel's walk round the ends of the shelf.
* `cargo run --example case_shot -- OUT` renders the case front, three
  quarter, both edges, back and top, for a title with no artwork and one
  dressed in the sample scans. That is how the sleeve's layout was pinned
  down: the sheet goes on as printed, and the spine is the face at yaw
  -pi/2.
* The launcher itself, on the real library (`--cases 1`), shows each game
  wearing its own `box2d` cover with its `cartridges` disc label beside it,
  the title above and the shelf position and hint below, the case swaying
  slowly between frames.
* Starting a game from the case view was seen working — the machine view
  came up and the BIOS booted. It was a stray pad press that found it: a
  stick resting off centre chattered across the threshold, every crossing
  read as a press, and the shelf walked itself four titles on and launched
  one. A press now only counts after the pad has been still for 200 ms, and
  the shelf held its place for the whole of a later sitting.

Driving the keyboard at the running window could not be done on this
desktop: the compositor kept handing focus to other windows, so synthetic
key events never reached the launcher. The arrow and enter handling is the
same `step`/`play` pair the pad feeds, and `case_step` is tested directly.

Not committed, as asked.
