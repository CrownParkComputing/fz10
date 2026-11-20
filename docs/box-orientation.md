# The FZ-1 retail carton: faces, artwork and orientation

The carton is `app/assets/fz1-box.glb`, built by `docs/make-fz1-model.py` and
rendered by `app/src/bin/retro3do-gui/fz1_3d.rs`. `app/examples/fz1_shot.rs`
renders it offscreen (closed, opening, lid and flaps) so the model can be
checked against the photographs without opening a window.

    python3 docs/make-fz1-model.py          # rewrites app/assets/fz1-box.glb
    cargo build --release --example fz1_shot
    ./target/release/examples/fz1_shot /tmp/shots

## Which face carries what

| face | artwork |
|---|---|
| front | big white `Panasonic`, the machine's picture, `R·E·A·L 3DO INTERACTIVE MULTIPLAYER` lockup with `FZ-1`, the 3DO badge top right, the two columns of small print, the yellow bonus-software corner bottom left |
| top (the lid's outer face) | `Panasonic` top left, the retailer's `WITH PLUG` sticker, the machine's picture top right, the lockup and `FZ-1` along the front edge, the 3DO badge at the left |
| sides, back | generic board-and-lockup printing |
| inside | raw kraft board, foam bed and end blocks |

The reference for both printed faces is `docs/fz1-box-front.png` and
`docs/fz1-box-angle.png`; the machine's picture comes from the console
photograph already keyed out of its paper (`fz1-reference-front.png`).

## Orientation

Both printed faces map the image on **as it is drawn** - the front face's
bottom edge is the image's bottom, and the lid's face runs from the image's
left at the box's left. There is no flip to compensate for: an earlier attempt
that turned the top artwork over to "fix" it made it read mirrored, which is
how the mapping was confirmed in the first place.

The carton opens from the top: the lid is hinged along the top of the back
wall and swings back, and the two dust flaps are hinged along the tops of the
side walls and fold out sideways (`BoxLid`, `FlapLeft`, `FlapRight` in the
GLB, driven by `fz1_3d::carton_pose`).

## Proportions and how the machine is packed

The carton is 360 x 290 x 130 mm - wide and shallow, not the near-cube an
earlier pass built. That follows from how the machine goes in: lengthways,
standing on its rear panel, back face down at the bottom of the box and the
CD drawer's face up toward the lid, with a foam insert either side filling
the rest of the width (`docs/fz1-box-open-contents.png`).

So each dimension is the machine plus its packing: the width is the
machine's 284 mm length plus a 36 mm insert either side, the height its
268 mm depth plus a 10 mm foam bed and a little headroom, and the depth its
88 mm height plus foam front and back.

## The unboxing

`fz1_3d::carton_pose` plays the whole thing off one 0..1 curve, and the
phases barely overlap so each reads on its own: the lid swings back, the
dust flaps fold out, the machine rises *straight up* still standing on its
rear panel with the drawer's face to the sky, clears the rim, swings down
onto its feet (`tilt`, -90 degrees to 0, pitched about `PIVOT_Y` above the
feet) and settles on the table as the carton drops away.

Two things that are easy to get wrong: the machine must not come out already
flat - it is packed on edge, so it leaves the box on edge - and its drawer
stays shut for the whole of the unboxing, whatever the launcher is asking
for. `render_into` forces the drawer shut while the carton is still on show
and `animate` holds `tray_goal` at 0, so it cannot snap open at the end
either.

`app/examples/fz1_shot.rs` renders `intro-00` through `intro-95` along that
curve, and `intro-rise-top` looks down on the machine mid-rise: the face
pointing at the sky there has to be the drawer's.
