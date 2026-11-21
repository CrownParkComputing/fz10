#!/usr/bin/env python3
"""Builds the 3DO game case from scratch — no imported model, no third party.

    blender -b -noaudio --python docs/make-case-model.py

Writes app/assets/dvd-case/case.glb.

A standard Amaray-style keep case: 190 mm tall, 135 mm wide, 14 mm thick shut,
hinged along the spine, with a disc seated on the hub in the base half.

Geometry is written out vertex by vertex. There are no booleans and no
modifiers: a cutter that overlaps its target deletes the whole case, and that
failure is silent in a headless build.

Axes here are Blender's. The glTF exporter turns Z-up into Y-up, so what the
renderer sees is height = +X, width = +Z, front-cover normal = +Y, which is
the convention docs/case-view-notes.md records and fz1_3d.rs relies on:

    blender  X -> glTF  X      height, 190 mm
    blender  Y -> glTF -Z      width, 135 mm
    blender  Z -> glTF  Y      thickness, 14 mm

Node names are the contract with the renderer. It dresses parts by node name
and hinges the two it is told to:

    CaseShell  the base half, the spine and the inner walls
    CaseLid    the front cover, origin ON THE HINGE so a rotation about X
               opens it
    Wrap       the printed sleeve's back cover and spine (static)
    WrapLid    the printed sleeve's front cover, origin on the same hinge
    CaseDisc   the disc's printed label face
    DiscRing   the unprinted outer rim
    DiscHub    the clear centre hub
"""

import math
import sys
from pathlib import Path

import bpy

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "app" / "assets" / "dvd-case" / "case.glb"

# --- the case, in metres --------------------------------------------------
H = 0.190           # height
W = 0.135           # width of one cover
T = 0.014           # thickness shut
SHELL = 0.0015      # how thick the polypropylene is
HX = H / 2          # +/- half height, along X
HINGE_Y = W / 2     # the spine edge: the hinge axis runs along X here
FREE_Y = -W / 2     # the opening edge


def span(y):
    """0 at the spine, 1 at the opening edge, whichever way the case faces."""
    return (HINGE_Y - y) / W
HZ = T / 2          # +/- half thickness

# The sleeve sits a hair proud of the shell or it z-fights with it.
PROUD = 0.0004

# Wrap UV: 284 mm across U as back cover | 14 mm spine | front cover.
# Walking from the front cover's free edge round to the back cover's.
U_SPINE_BACK = 0.4825
U_SPINE_FRONT = 0.5175

DISC_R = 0.060          # 120 mm disc
DISC_LABEL_R = 0.0585   # printed area stops just inside the rim
DISC_HUB_R = 0.0115     # clear hub ring
DISC_HOLE_R = 0.0075    # 15 mm centre hole
DISC_Z = -HZ + SHELL + 0.0012   # seated on the hub in the base half
SEG = 96                # segments round the disc

MATERIALS = {
    "CaseShell": (0.045, 0.045, 0.050, 1.0),
    "CaseLid": (0.045, 0.045, 0.050, 1.0),
    "Wrap": (1.0, 1.0, 1.0, 1.0),
    "WrapLid": (1.0, 1.0, 1.0, 1.0),
    "CaseDisc": (1.0, 1.0, 1.0, 1.0),
    "DiscRing": (0.80, 0.82, 0.86, 1.0),
    "DiscHub": (0.10, 0.10, 0.12, 1.0),
}


def clear_scene():
    bpy.ops.wm.read_factory_settings(use_empty=True)


def material(name):
    mat = bpy.data.materials.new(name)
    mat.use_nodes = True
    bsdf = mat.node_tree.nodes["Principled BSDF"]
    bsdf.inputs["Base Color"].default_value = MATERIALS[name]
    # Flat-ish plastic: the renderer has no specular path worth feeding.
    bsdf.inputs["Roughness"].default_value = 0.45
    bsdf.inputs["Metallic"].default_value = 0.0
    return mat


def make_object(name, verts, faces, uvs, origin=(0.0, 0.0, 0.0)):
    """One mesh, one node, vertices relative to `origin`.

    The origin matters: the renderer rotates a node about its own origin, so
    the lid's has to sit on the hinge.
    """
    ox, oy, oz = origin
    local = [(x - ox, y - oy, z - oz) for (x, y, z) in verts]
    mesh = bpy.data.meshes.new(name)
    mesh.from_pydata(local, [], faces)
    mesh.validate()
    layer = mesh.uv_layers.new(name="UVMap")
    for loop_index, uv in enumerate(uvs):
        layer.data[loop_index].uv = uv
    mesh.update()
    obj = bpy.data.objects.new(name, mesh)
    obj.location = origin
    obj.data.materials.append(material(name))
    bpy.context.collection.objects.link(obj)
    return obj


def quad(verts, faces, uvs, corners, corner_uvs):
    """Append one quad, winding as given."""
    base = len(verts)
    verts.extend(corners)
    faces.append((base, base + 1, base + 2, base + 3))
    uvs.extend(corner_uvs)


def box(verts, faces, uvs, x0, x1, y0, y1, z0, z1):
    """A closed box. UVs are flat and unused on the shell parts."""
    y0, y1 = min(y0, y1), max(y0, y1)
    c = [
        (x0, y0, z0), (x1, y0, z0), (x1, y1, z0), (x0, y1, z0),
        (x0, y0, z1), (x1, y0, z1), (x1, y1, z1), (x0, y1, z1),
    ]
    flat = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]
    for face in [
        (0, 3, 2, 1), (4, 5, 6, 7), (0, 1, 5, 4),
        (1, 2, 6, 5), (2, 3, 7, 6), (3, 0, 4, 7),
    ]:
        quad(verts, faces, uvs, [c[i] for i in face], flat)


def build_shell():
    """Base half: back plate, spine wall, and the three rim walls."""
    v, f, uv = [], [], []
    # Back plate, the full footprint.
    box(v, f, uv, -HX, HX, HINGE_Y, FREE_Y, -HZ, -HZ + SHELL)
    # Spine wall, full thickness, along the hinge edge.
    box(v, f, uv, -HX, HX, HINGE_Y, HINGE_Y + SHELL, -HZ, HZ)
    # Rim walls on the base half only: head, foot and the opening edge.
    box(v, f, uv, -HX, -HX + SHELL, HINGE_Y, FREE_Y, -HZ, 0.0)
    box(v, f, uv, HX - SHELL, HX, HINGE_Y, FREE_Y, -HZ, 0.0)
    box(v, f, uv, -HX, HX, FREE_Y - SHELL, FREE_Y, -HZ, 0.0)
    # The hub the disc clips onto.
    ring(v, f, uv, 0.0, DISC_HOLE_R - 0.0004, -HZ + SHELL, DISC_Z + 0.0006, solid=True)
    make_object("CaseShell", v, f, uv)


def build_lid():
    """Front cover, hinged. Origin on the hinge axis at mid-thickness."""
    v, f, uv = [], [], []
    box(v, f, uv, -HX, HX, HINGE_Y, FREE_Y, HZ - SHELL, HZ)
    box(v, f, uv, -HX, -HX + SHELL, HINGE_Y, FREE_Y, 0.0, HZ)
    box(v, f, uv, HX - SHELL, HX, HINGE_Y, FREE_Y, 0.0, HZ)
    box(v, f, uv, -HX, HX, FREE_Y - SHELL, FREE_Y, 0.0, HZ)
    make_object("CaseLid", v, f, uv, origin=(0.0, HINGE_Y, 0.0))


def wrap_v(x):
    """Sleeve V runs down the 190 mm height."""
    return (x + HX) / H


def build_wrap():
    """Back cover and spine of the printed sleeve. Static."""
    v, f, uv = [], [], []
    z = -HZ - PROUD
    # Back cover, outer face. u runs 0.4825 at the spine to 0.0 at the free
    # edge, so the sheet reads continuously round from the front.
    def u_back(y):
        return U_SPINE_BACK * (1.0 - span(y))
    # Wound for a -Z normal: the back cover is seen from outside the case.
    # Wound the other way the print reads mirrored.
    quad(v, f, uv,
         [(-HX, FREE_Y, z), (HX, FREE_Y, z), (HX, HINGE_Y, z), (-HX, HINGE_Y, z)],
         [(u_back(FREE_Y), wrap_v(-HX)), (u_back(FREE_Y), wrap_v(HX)),
          (u_back(HINGE_Y), wrap_v(HX)), (u_back(HINGE_Y), wrap_v(-HX))])
    # Spine, across the 14 mm thickness.
    y = HINGE_Y + PROUD
    quad(v, f, uv,
         [(-HX, y, -HZ), (HX, y, -HZ), (HX, y, HZ), (-HX, y, HZ)],
         [(U_SPINE_BACK, wrap_v(-HX)), (U_SPINE_BACK, wrap_v(HX)),
          (U_SPINE_FRONT, wrap_v(HX)), (U_SPINE_FRONT, wrap_v(-HX))])
    make_object("Wrap", v, f, uv)


def build_wrap_lid():
    """Front cover of the sleeve. Same origin as the lid so it swings with it."""
    v, f, uv = [], [], []
    z = HZ + PROUD

    def u_front(y):
        return U_SPINE_FRONT + (1.0 - U_SPINE_FRONT) * span(y)

    # Wound for a +Z normal, the face the viewer sees when the case is shut.
    quad(v, f, uv,
         [(HX, HINGE_Y, z), (HX, FREE_Y, z), (-HX, FREE_Y, z), (-HX, HINGE_Y, z)],
         [(u_front(HINGE_Y), wrap_v(HX)), (u_front(FREE_Y), wrap_v(HX)),
          (u_front(FREE_Y), wrap_v(-HX)), (u_front(HINGE_Y), wrap_v(-HX))])
    make_object("WrapLid", v, f, uv, origin=(0.0, HINGE_Y, 0.0))


def ring(verts, faces, uvs, r_in, r_out, z0, z1, solid=False, square_uv=False):
    """An annulus on the z1 face, optionally walled so it reads as solid."""

    def uv_for(x, y):
        # Same square mapping the FZ-1's platter uses, written in Blender's
        # axes: the exporter turns (x, y, z) into glTF (x, z, -y), so the
        # platter's v = 0.5 + z/2r becomes 0.5 - y/2R here. Getting that sign
        # wrong mirrors the label rather than turning it.
        if square_uv:
            return (0.5 - (y / DISC_LABEL_R) * 0.5, 0.5 + (x / DISC_LABEL_R) * 0.5)
        return (0.0, 0.0)

    for i in range(SEG):
        j = (i + 1) % SEG
        # Wound so the normal points +Z, out of the open base toward the
        # viewer. Wound the other way the label is seen from behind, which
        # reads as a mirrored print and no UV change will fix it.
        face = [verts_at(r_in, i, z1), verts_at(r_out, i, z1),
                verts_at(r_out, j, z1), verts_at(r_in, j, z1)]
        quad(verts, faces, uvs, face, [uv_for(p[0], p[1]) for p in face])
        if solid:
            wall = [verts_at(r_out, i, z0), verts_at(r_out, j, z0),
                    verts_at(r_out, j, z1), verts_at(r_out, i, z1)]
            quad(verts, faces, uvs, wall, [(0.0, 0.0)] * 4)


def verts_at(r, i, z):
    a = 2.0 * math.pi * i / SEG
    return (r * math.cos(a), r * math.sin(a), z)


def build_disc():
    """The disc, seated in the base. Label UV is a plain 0..1 square."""
    v, f, uv = [], [], []
    ring(v, f, uv, DISC_HUB_R, DISC_LABEL_R, DISC_Z, DISC_Z + 0.0012, square_uv=True)
    make_object("CaseDisc", v, f, uv)

    v, f, uv = [], [], []
    ring(v, f, uv, DISC_LABEL_R, DISC_R, DISC_Z, DISC_Z + 0.0012)
    make_object("DiscRing", v, f, uv)

    v, f, uv = [], [], []
    ring(v, f, uv, DISC_HOLE_R, DISC_HUB_R, DISC_Z, DISC_Z + 0.0012)
    make_object("DiscHub", v, f, uv)


def main():
    clear_scene()
    build_shell()
    build_lid()
    build_wrap()
    build_wrap_lid()
    build_disc()
    OUT.parent.mkdir(parents=True, exist_ok=True)
    bpy.ops.export_scene.gltf(
        filepath=str(OUT),
        export_format="GLB",
        export_yup=True,
        export_apply=False,
        export_materials="EXPORT",
        export_cameras=False,
        export_lights=False,
    )
    names = sorted(o.name for o in bpy.data.objects)
    print(f"wrote {OUT} with nodes: {', '.join(names)}")


main()
