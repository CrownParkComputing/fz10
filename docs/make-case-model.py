#!/usr/bin/env python3
"""Converts the staged DVD game-case model into the glTF the GUI loads.

    app/assets/dvd-case/source/case.fbx  ->  app/assets/dvd-case/case.glb

The source carries one mesh per part with several materials on each; the
launcher needs one named node per surface it dresses, so the meshes are
split by material and the pieces renamed to what the renderer looks for:

    Wrap        the printed sleeve: back cover | spine | front cover across
                its U, the 190 mm height down its V (a 284x190 wrap)
    CaseShell   the black polypropylene case itself
    CaseDisc    the disc's label face
    DiscRing    the unprinted rim
    DiscHub     the clear hub

The clear outer sleeve and its fingerprint overlay are dropped: without a
real specular path they only grey the artwork out.

    blender -b -noaudio --python docs/make-case-model.py
"""

import sys
from pathlib import Path

import bpy

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "app" / "assets" / "dvd-case" / "source" / "case.fbx"
OUT = ROOT / "app" / "assets" / "dvd-case" / "case.glb"

# Which material each wanted piece comes in on, and the name it leaves with.
PIECES = {
    "Art.001": ("Wrap", (1.0, 1.0, 1.0, 1.0)),
    "Plastic": ("CaseShell", (0.045, 0.045, 0.05, 1.0)),
    "CD Art": ("CaseDisc", (1.0, 1.0, 1.0, 1.0)),
    "CD Colored": ("DiscRing", (0.80, 0.82, 0.86, 1.0)),
    "CD Black": ("DiscHub", (0.10, 0.10, 0.12, 1.0)),
}
DROP = ("Camera", "Light", "Circle", "Plastic", "Plastic - Transparent")


def main():
    bpy.ops.wm.read_factory_settings(use_empty=True)
    bpy.ops.import_scene.fbx(filepath=str(SRC))

    for name in DROP:
        obj = bpy.data.objects.get(name)
        if obj is not None:
            bpy.data.objects.remove(obj, do_unlink=True)

    # Split every remaining mesh into one object per material.
    for obj in list(bpy.data.objects):
        if obj.type != "MESH" or len(obj.data.materials) < 2:
            continue
        bpy.ops.object.select_all(action="DESELECT")
        obj.select_set(True)
        bpy.context.view_layer.objects.active = obj
        bpy.ops.mesh.separate(type="MATERIAL")

    # Name the pieces by the material they came in on, and give each one a
    # flat base colour: the launcher paints the artwork on itself.
    for obj in list(bpy.data.objects):
        if obj.type != "MESH":
            bpy.data.objects.remove(obj, do_unlink=True)
            continue
        mats = [m.name for m in obj.data.materials if m]
        piece = next((PIECES[m] for m in mats if m in PIECES), None)
        if piece is None:
            bpy.data.objects.remove(obj, do_unlink=True)
            continue
        obj.name = piece[0]
        obj.data.name = piece[0]
        mat = bpy.data.materials.new(piece[0])
        mat.use_nodes = True
        bsdf = mat.node_tree.nodes.get("Principled BSDF")
        if bsdf is not None:
            bsdf.inputs["Base Color"].default_value = piece[1]
            bsdf.inputs["Roughness"].default_value = 0.35
        obj.data.materials.clear()
        obj.data.materials.append(mat)
        obj.modifiers.clear()

    names = sorted(o.name for o in bpy.data.objects)
    if names != sorted(n for n, _ in PIECES.values()):
        sys.exit(f"unexpected pieces after the split: {names}")

    bpy.ops.export_scene.gltf(
        filepath=str(OUT),
        export_format="GLB",
        export_apply=True,
        export_yup=True,
        export_materials="EXPORT",
        export_cameras=False,
        export_lights=False,
    )
    print(f"wrote {OUT}")


main()
