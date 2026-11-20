#!/usr/bin/env python3
"""Builds the two models the GUI's 3D view draws.

    app/assets/fz1.glb      the Panasonic FZ-1 itself
    app/assets/fz1-box.glb  the retail carton it came in

Both are generated here rather than modelled by hand: the console is a
measured replica (286 x 272 x 88 mm) whose front face is laid out from the
photographs in docs/ — fz1-reference-front.png (head on, the proportions)
and fz1-reference-front-top.png (the authoritative face layout). The
markings are baked into small textures from the artwork in docs/ rather
than carried as text geometry, which keeps the file small and the lettering
crisp from any distance.

No modelling package is involved: the glTF is written out directly, so the
only dependency is Pillow for the baked artwork.

    python3 docs/make-fz1-model.py
"""

import json
import math
import struct
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

ROOT = Path(__file__).resolve().parent.parent
DOCS = ROOT / "docs"
ASSETS = ROOT / "app" / "assets"

SANS = "/usr/share/fonts/liberation/LiberationSans-Bold.ttf"
SANS_R = "/usr/share/fonts/liberation/LiberationSans-Regular.ttf"
SANS_C = "/usr/share/fonts/TTF/DejaVuSansCondensed-Bold.ttf"
SERIF = "/usr/share/fonts/TTF/DejaVuSerif-Bold.ttf"
SERIF_I = "/usr/share/fonts/TTF/DejaVuSerif-BoldItalic.ttf"


# --- baked artwork ---------------------------------------------------------


def art(name, white_is_paper=False):
    """One of the reference pieces in docs/, with its paper keyed out."""
    img = Image.open(DOCS / name).convert("RGBA")
    if not white_is_paper:
        return img
    px = img.load()
    w, h = img.size
    for y in range(h):
        for x in range(w):
            r, g, b, a = px[x, y]
            m = min(r, g, b)
            if m > 232:
                px[x, y] = (r, g, b, 0)
            elif m > 150:
                px[x, y] = (r, g, b, int(255 * (232 - m) / 82))
    return img


def ink(img, rgb):
    """Recolour the black ink of a piece of line art, leaving its colours.

    The 3DO marks carry coloured squares that have to survive; only what is
    close to black is taken over by `rgb`, so silver lettering keeps the red,
    green and blue pips between the letters.
    """
    out = img.copy()
    px = out.load()
    w, h = out.size
    for y in range(h):
        for x in range(w):
            r, g, b, a = px[x, y]
            if a == 0:
                continue
            if max(r, g, b) < 90:
                px[x, y] = (rgb[0], rgb[1], rgb[2], a)
    return out


def tracked(draw, xy, text, font, fill, tracking=0.0, centre=False):
    """Letter-spaced text — the FZ-1's panel lettering is widely tracked."""
    widths = [draw.textlength(c, font=font) for c in text]
    total = sum(widths) + tracking * max(len(text) - 1, 0)
    x = xy[0] - total / 2 if centre else xy[0]
    for c, w in zip(text, widths):
        draw.text((x, xy[1]), c, font=font, fill=fill)
        x += w + tracking
    return total


def fit(img, box_w, box_h):
    """The art scaled to sit inside a box, keeping its shape."""
    s = min(box_w / img.width, box_h / img.height)
    return img.resize((max(int(img.width * s), 1), max(int(img.height * s), 1)),
                      Image.LANCZOS)


def cd_logo(size):
    """The "compact disc DIGITAL AUDIO" mark, drawn rather than traced."""
    w, h = size
    img = Image.new("RGBA", (w * 4, h * 4), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    W, H = img.size
    silver = (214, 214, 220, 255)
    # The two arcs and the disc between them.
    d.arc([W * 0.12, H * 0.18, W * 0.60, H * 0.66], 150, 330, fill=silver,
          width=max(int(H * 0.05), 2))
    d.arc([W * 0.40, H * 0.18, W * 0.88, H * 0.66], 150, 330, fill=silver,
          width=max(int(H * 0.05), 2))
    f_disc = ImageFont.truetype(SERIF_I, int(H * 0.42))
    tw = d.textlength("disc", font=f_disc)
    d.text((W / 2 - tw / 2, H * 0.30), "disc", font=f_disc, fill=silver)
    f_small = ImageFont.truetype(SANS, int(H * 0.15))
    tracked(d, (W * 0.5, H * 0.03), "COMPACT", f_small, silver,
            tracking=W * 0.012, centre=True)
    tracked(d, (W * 0.5, H * 0.78), "DIGITAL AUDIO", f_small, silver,
            tracking=W * 0.010, centre=True)
    return img.resize((w, h), Image.LANCZOS)


def mono(img, rgb):
    """Every mark in one colour — moulded or printed in a single ink, the
    console's own lettering has none of the lockup's coloured pips."""
    out = img.copy()
    px = out.load()
    for y in range(out.height):
        for x in range(out.width):
            r, g, b, a = px[x, y]
            if a and min(r, g, b) < 235:
                px[x, y] = (rgb[0], rgb[1], rgb[2], a)
            else:
                px[x, y] = (0, 0, 0, 0)
    return out


def real_block(width, light=True, colour=None):
    """R·E·A·L over its "3DO INTERACTIVE MULTIPLAYER" rule, from the lockup."""
    src = art("real-logo-lockup.png")
    block = src.crop((0, 185, 960, 400))
    if colour:
        block = mono(block, colour)
    elif light:
        block = ink(block, (206, 206, 212))
    scale = width / block.width
    return block.resize((width, max(int(block.height * scale), 1)), Image.LANCZOS)


def badge_art(height):
    """The 3DO badge: diamond, pill, sphere and the 3DO block under them."""
    img = art("3do-badge.png", white_is_paper=True)
    img = img.crop(img.getbbox())
    scale = height / img.height
    return img.resize((max(int(img.width * scale), 1), height), Image.LANCZOS)


def tex_fascia(w=2048, h=384):
    """The recessed strip under the drawer.

    Laid out from the head-on photograph: READY and the 3DO badge left of
    centre, the R·E·A·L lockup and FZ-1 in the middle, ACCESS and the compact
    disc mark right of centre, the controller socket low and right of centre
    with its label beside it. The strip spans the 176 mm between the pods.
    """
    img = Image.new("RGBA", (w, h), (8, 8, 9, 255))
    d = ImageDraw.Draw(img)
    # A touch of sheen down the strip, as the moulding catches the light.
    for y in range(h):
        f = 1.0 - abs(y - h * 0.18) / h
        v = int(5 + 7 * max(f, 0.0))
        d.line([(0, y), (w, y)], fill=(v, v, v + 1, 255))
    grey = (138, 138, 142, 255)

    f_label = ImageFont.truetype(SANS, int(h * 0.125))
    tracked(d, (int(w * 0.041), int(h * 0.05)), "READY", f_label, grey,
            tracking=w * 0.0016)
    tracked(d, (int(w * 0.862), int(h * 0.05)), "ACCESS", f_label, grey,
            tracking=w * 0.0016)

    badge = badge_art(int(h * 0.47))
    img.alpha_composite(badge, (int(w * 0.062) - badge.width // 2, int(h * 0.22)))

    block = real_block(int(w * 0.30), colour=grey[:3])
    img.alpha_composite(block, (int(w * 0.50) - block.width // 2, int(h * 0.10)))

    f_model = ImageFont.truetype(SANS_R, int(h * 0.15))
    tracked(d, (int(w * 0.505), int(h * 0.64)), "FZ-1", f_model, grey,
            tracking=w * 0.0022, centre=True)

    cd = mono(cd_logo((int(w * 0.082), int(h * 0.44))), grey[:3])
    img.alpha_composite(cd, (int(w * 0.878) - cd.width // 2, int(h * 0.24)))

    return img


def tex_door(w=1024, h=184):
    """The drawer's face: near-black with the gold Panasonic wordmark."""
    img = Image.new("RGBA", (w, h), (8, 8, 9, 255))
    d = ImageDraw.Draw(img)
    for y in range(h):
        v = int(12 - 5 * (y / h))
        d.line([(0, y), (w, y)], fill=(v, v, v + 1, 255))
    # The seam running round the drawer front.
    d.rectangle([2, 2, w - 3, h - 3], outline=(22, 22, 24, 255), width=3)
    # Pale gold: the shading lifts it toward the print's champagne.
    gold = (122, 98, 62, 255)
    f = ImageFont.truetype(SANS, int(h * 0.46))
    tw = d.textlength("Panasonic", font=f)
    d.text((w / 2 - tw / 2, h * 0.27), "Panasonic", font=f, fill=gold)
    return img


def tex_lid(w=1024, h=842):
    """The top shell: moulded texture with the markings raised out of it.

    The real lid carries no print at all — the name and the R·E·A·L lockup
    are embossed, so they are baked as a light-above/dark-below relief on a
    finely speckled surface rather than as flat ink.
    """
    import random

    rng = random.Random(7)
    img = Image.new("RGBA", (w, h), (13, 13, 14, 255))
    px = img.load()
    for y in range(h):
        for x in range(w):
            n = rng.randint(-3, 3)
            px[x, y] = (13 + n, 13 + n, 14 + n, 255)
    img = img.filter(ImageFilter.SMOOTH)

    def emboss(piece, at):
        shadow = mono(piece, (3, 3, 4))
        light = mono(piece, (27, 27, 29))
        img.alpha_composite(shadow, (at[0], at[1] + max(h // 320, 2)))
        img.alpha_composite(light, at)

    word = Image.new("RGBA", (w, h // 4), (0, 0, 0, 0))
    wd = ImageDraw.Draw(word)
    f = ImageFont.truetype(SANS, int(h * 0.115))
    tw = wd.textlength("Panasonic", font=f)
    wd.text((w / 2 - tw / 2, 0), "Panasonic", font=f, fill=(0, 0, 0, 255))
    word = word.crop(word.getbbox())
    emboss(word, (w // 2 - word.width // 2, int(h * 0.42)))

    block = real_block(int(w * 0.34), colour=(0, 0, 0))
    emboss(block, (w // 2 - block.width // 2, int(h * 0.60)))
    return img


def tex_button(label, w=512, h=96):
    """A bronze strip with its label pressed into it."""
    img = Image.new("RGBA", (w, h), (70, 58, 50, 255))
    d = ImageDraw.Draw(img)
    for y in range(h):
        f = y / h
        v = (int(86 - 36 * f), int(71 - 30 * f), int(61 - 26 * f))
        d.line([(0, y), (w, y)], fill=v + (255,))
    f = ImageFont.truetype(SERIF, int(h * 0.40))
    tracked(d, (w * 0.5, h * 0.30), label, f, (26, 20, 16, 255),
            tracking=w * 0.004, centre=True)
    return img


def tex_port(w=256, h=128):
    """What sits at the bottom of the controller socket: a 9-pin D shell."""
    img = Image.new("RGBA", (w, h), (10, 10, 11, 255))
    d = ImageDraw.Draw(img)
    d.polygon([(w * 0.10, h * 0.30), (w * 0.90, h * 0.30), (w * 0.84, h * 0.78),
               (w * 0.16, h * 0.78)], fill=(26, 26, 28, 255),
              outline=(72, 72, 76, 255))
    for i in range(5):
        x = w * (0.22 + i * 0.14)
        d.ellipse([x - w * 0.022, h * 0.38, x + w * 0.022, h * 0.50],
                  fill=(128, 128, 118, 255))
    for i in range(4):
        x = w * (0.29 + i * 0.14)
        d.ellipse([x - w * 0.022, h * 0.58, x + w * 0.022, h * 0.70],
                  fill=(128, 128, 118, 255))
    return img


# --- the carton's artwork --------------------------------------------------


def console_cutout(height):
    """The console lifted off the white of its photograph, for the box art."""
    img = art("fz1-reference-front.png", white_is_paper=True)
    bbox = img.getbbox()
    if bbox:
        img = img.crop(bbox)
    scale = height / img.height
    return img.resize((max(int(img.width * scale), 1), height), Image.LANCZOS)


def box_ground(w, h):
    """The carton's printed ground: near-black, warmed toward the bottom."""
    img = Image.new("RGBA", (w, h), (12, 12, 16, 255))
    d = ImageDraw.Draw(img)
    for y in range(h):
        f = y / h
        v = (int(14 + 20 * f * f), int(14 + 16 * f * f), int(20 + 26 * f * f))
        d.line([(0, y), (w, y)], fill=v + (255,))
    return img


def gold_rule(d, y, x0, x1, thick):
    d.rectangle([x0, y, x1, y + thick], fill=(186, 146, 62, 255))




def console_picture(height):
    """The machine's picture, keyed off its paper."""
    return console_cutout(height)


def with_plug_sticker(w, h):
    """The retailer's own sticker: 'WITH PLUG' in red on a white card."""
    img = Image.new("RGBA", (w, h), (238, 238, 236, 255))
    d = ImageDraw.Draw(img)
    f = ImageFont.truetype(SANS, int(h * 0.34))
    d.text((w * 0.06, h * 0.12), "WITH PLUG", font=f, fill=(176, 28, 32, 255))
    f2 = ImageFont.truetype(SANS_R, int(h * 0.17))
    d.text((w * 0.06, h * 0.55), "3DO SYSTEM", font=f2, fill=(40, 40, 44, 255))
    d.rectangle([0, 0, w - 1, h - 1], outline=(120, 120, 124, 255))
    return img


def bonus_triangle(size):
    """The yellow corner that promises the extra software."""
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.polygon([(0, size), (size, size), (0, 0)], fill=(232, 186, 32, 255))
    f = ImageFont.truetype(SANS, int(size * 0.115))
    d.text((size * 0.07, size * 0.80), "BONUS", font=f, fill=(30, 26, 10, 255))
    d.text((size * 0.07, size * 0.90), "SOFTWARE", font=f, fill=(30, 26, 10, 255))
    return img


def barcode(d, x0, y0, x1, y1, seed=0):
    """A printed barcode panel: a white card, black bars and digits under."""
    d.rectangle([x0, y0, x1, y1], fill=(238, 238, 236, 255))
    n = 32
    step = (x1 - x0 - 18) / n
    for i in range(n):
        if (i * 7 + seed * 3) % 3 == 0:
            continue
        bx = x0 + 9 + i * step
        d.rectangle([bx, y0 + 10, bx + step * 0.55, y1 - 26], fill=(24, 24, 26, 255))
    f = ImageFont.truetype(SANS_R, 15)
    d.text(((x0 + x1) / 2, y1 - 24), "4 3 5 0 0 0 1 2 3 4 5 6", font=f,
           fill=(24, 24, 26, 255), anchor="ma")

def tex_box_front(w=1024, h=826):
    """The carton's front face, laid out from docs/fz1-box-front.png.

    Brand across the top, the machine's photograph on its cream panel in the
    middle, the lockup and the model name along the bottom, the badge top
    right and the bonus corner bottom left - the way the carton really is.
    """
    img = box_ground(w, h)
    d = ImageDraw.Draw(img)

    f_brand = ImageFont.truetype(SANS, int(h * 0.098))
    d.text((w * 0.05, h * 0.022), "Panasonic", font=f_brand,
           fill=(240, 240, 244, 255))

    # The machine, photographed, on the cream panel the carton printed.
    px0, py0, px1, py1 = w * 0.225, h * 0.165, w * 0.775, h * 0.505
    d.rectangle([px0, py0, px1, py1], fill=(233, 231, 224, 255))
    shot = console_picture(int(h * 0.31))
    shot = fit(shot, int(px1 - px0) - 14, int(py1 - py0) - 14)
    img.alpha_composite(shot, (int((px0 + px1 - shot.width) / 2),
                               int((py0 + py1 - shot.height) / 2)))

    # The lockup and the model name, along the bottom.
    block = real_block(int(w * 0.42))
    img.alpha_composite(block, (int(w * 0.29), int(h * 0.575)))
    f_model = ImageFont.truetype(SANS, int(h * 0.082))
    tracked(d, (w * 0.5, h * 0.715), "FZ-1", f_model, (232, 232, 238, 255),
            tracking=w * 0.006, centre=True)
    f_sub = ImageFont.truetype(SANS_C, int(h * 0.028))
    tracked(d, (w * 0.5, h * 0.795), "3DO INTERACTIVE MULTIPLAYER", f_sub,
            (182, 182, 190, 255), tracking=w * 0.0014, centre=True)

    # The two columns of small print either side of the picture.
    f_bullet = ImageFont.truetype(SANS_R, int(h * 0.0195))
    left = ["• 32 bit RISC CPU", "• jusqu'à 16 millions",
            "  de couleurs", "• 64 millions de pixels",
            "• Full screen, full color", "  digital video"]
    for i, line in enumerate(left):
        d.text((w * 0.020, h * (0.185 + i * 0.030)), line, font=f_bullet,
               fill=(176, 176, 184, 255))
    right = ["• CD stereo sound", "• Multi CD", "  compatibility",
             "• Double-speed", "  CD-ROM drive", "• Two expansion ports"]
    for i, line in enumerate(right):
        d.text((w * 0.980, h * (0.185 + i * 0.030)), line, font=f_bullet,
               fill=(176, 176, 184, 255), anchor="ra")

    badge = badge_art(int(h * 0.10))
    img.alpha_composite(badge, (int(w * 0.955) - badge.width, int(h * 0.028)))

    corner = bonus_triangle(int(w * 0.20))
    img.alpha_composite(corner, (0, h - corner.height))
    return img


def tex_box_back(w=1024, h=826):
    img = box_ground(w, h)
    d = ImageDraw.Draw(img)
    f_brand = ImageFont.truetype(SANS, int(h * 0.055))
    d.text((w * 0.055, h * 0.035), "Panasonic", font=f_brand,
           fill=(226, 226, 232, 255))
    gold_rule(d, int(h * 0.115), int(w * 0.055), int(w * 0.945), max(h // 220, 2))
    # Four picture wells, the way the carton showed what the machine did.
    f_cap = ImageFont.truetype(SANS_C, int(h * 0.026))
    for i in range(4):
        x0 = w * (0.055 + (i % 2) * 0.455)
        y0 = h * (0.150 + (i // 2) * 0.195)
        d.rectangle([x0, y0, x0 + w * 0.40, y0 + h * 0.150], fill=(30, 30, 38, 255),
                    outline=(96, 96, 104, 255))
        d.text((x0, y0 + h * 0.158), "INTERACTIVE MULTIPLAYER", font=f_cap,
               fill=(150, 150, 158, 255))
    f_body = ImageFont.truetype(SANS_R, int(h * 0.028))
    lines = [
        "32-bit RISC processor · double-speed CD-ROM drive",
        "16.7 million colours · full-motion video · CD audio",
        "Plays audio CDs and Photo CDs through your television.",
        "Two controller ports · expansion bay · 3DO logo licensed.",
    ]
    for i, line in enumerate(lines):
        d.text((w * 0.055, h * (0.585 + i * 0.052)), line, font=f_body,
               fill=(186, 186, 194, 255))
    badge = badge_art(int(h * 0.12))
    img.alpha_composite(badge, (int(w * 0.945) - badge.width, int(h * 0.60)))
    return img


def tex_box_side(w=459, h=1024):
    img = box_ground(w, h)
    d = ImageDraw.Draw(img)
    gold_rule(d, int(h * 0.095), int(w * 0.08), int(w * 0.92), max(h // 220, 2))
    f = ImageFont.truetype(SANS, int(h * 0.052))
    tracked(d, (w * 0.5, h * 0.040), "Panasonic", f, (230, 230, 236, 255),
            tracking=w * 0.004, centre=True)
    block = real_block(int(w * 0.62))
    img.alpha_composite(block, (int(w * 0.5) - block.width // 2, int(h * 0.135)))
    f_model = ImageFont.truetype(SANS, int(h * 0.070))
    tracked(d, (w * 0.5, h * 0.355), "FZ-1", f_model, (198, 150, 52, 255),
            tracking=w * 0.012, centre=True)
    badge = badge_art(int(h * 0.10))
    img.alpha_composite(badge, (int(w * 0.5) - badge.width // 2, int(h * 0.435)))
    # The maker's panel, and the barcodes it shipped with.
    d.rectangle([w * 0.10, h * 0.545, w * 0.90, h * 0.685], fill=(232, 230, 222, 255))
    fp = ImageFont.truetype(SANS_R, int(h * 0.021))
    for i, line in enumerate(["Made in Japan", "Fabriqué au Japon",
                              "MATSUSHITA ELECTRIC", "INDUSTRIAL CO., LTD.",
                              "MODEL NO. FZ-1", "SERIAL NO. ________"]):
        d.text((w * 0.13, h * (0.560 + i * 0.021)), line, font=fp,
               fill=(30, 30, 30, 255))
    barcode(d, w * 0.10, h * 0.715, w * 0.90, h * 0.825, 1)
    barcode(d, w * 0.10, h * 0.845, w * 0.90, h * 0.955, 2)
    return img


def tex_box_top(w=1024, h=370):
    """The carton's top face, laid out from docs/fz1-box-angle.png.

    The lid is printed the way it is read from above: the model name and the
    lockup along the back edge, the brand along the front, the machine's
    picture on the right and the retailer's sticker in the middle.
    """
    img = box_ground(w, h)
    d = ImageDraw.Draw(img)

    # The back edge: the model name, then the lockup under it.
    f_model = ImageFont.truetype(SANS, int(h * 0.125))
    tracked(d, (w * 0.065, h * 0.055), "FZ-1", f_model, (232, 232, 238, 255),
            tracking=w * 0.008)
    block = real_block(int(w * 0.30))
    img.alpha_composite(block, (int(w * 0.055), int(h * 0.235)))
    f_sub = ImageFont.truetype(SANS_C, int(h * 0.042))
    tracked(d, (w * 0.070, h * 0.435), "3DO INTERACTIVE MULTIPLAYER", f_sub,
            (182, 182, 190, 255), tracking=w * 0.002)

    # The front edge: the brand, big, the way the carton carries it.
    f_brand = ImageFont.truetype(SANS, int(h * 0.115))
    d.text((w * 0.05, h * 0.685), "Panasonic", font=f_brand,
           fill=(238, 238, 244, 255))

    # The machine's picture on the right, and the retailer's sticker beside it.
    shot = console_picture(int(h * 0.40))
    shot = fit(shot, int(w * 0.32), int(h * 0.40))
    img.alpha_composite(shot, (int(w * 0.63), int(h * 0.42)))
    sticker = with_plug_sticker(int(w * 0.19), int(h * 0.135))
    img.alpha_composite(sticker, (int(w * 0.36), int(h * 0.10)))

    badge = badge_art(int(h * 0.11))
    img.alpha_composite(badge, (int(w * 0.045), int(h * 0.545)))

    return img


# --- meshes ----------------------------------------------------------------


def sub(a, b):
    return (a[0] - b[0], a[1] - b[1], a[2] - b[2])


def cross(a, b):
    return (a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0])


def norm(v):
    n = math.sqrt(v[0] * v[0] + v[1] * v[1] + v[2] * v[2]) or 1.0
    return (v[0] / n, v[1] / n, v[2] / n)


# Everything is laid out in millimetres and written in metres.
MM = 0.001

UNIT_UV = ((0.0, 1.0), (1.0, 1.0), (1.0, 0.0), (0.0, 0.0))


class Prim:
    """One material's worth of triangles, flat shaded."""

    def __init__(self):
        self.pos = []
        self.nrm = []
        self.uv = []
        self.idx = []

    def tri(self, p0, p1, p2, uv0, uv1, uv2, n=None):
        if n is None:
            n = norm(cross(sub(p1, p0), sub(p2, p0)))
        base = len(self.pos)
        for p, t in ((p0, uv0), (p1, uv1), (p2, uv2)):
            self.pos.append((p[0] * MM, p[1] * MM, p[2] * MM))
            self.nrm.append(n)
            self.uv.append(t)
        self.idx += [base, base + 1, base + 2]

    def quad(self, p0, p1, p2, p3, uv=UNIT_UV, n=None):
        if n is None:
            n = norm(cross(sub(p1, p0), sub(p3, p0)))
        self.tri(p0, p1, p2, uv[0], uv[1], uv[2], n)
        self.tri(p0, p2, p3, uv[0], uv[2], uv[3], n)


class Node:
    """A named part of the model: its own transform and its triangles."""

    def __init__(self, name, translation=None, children=()):
        self.name = name
        self.translation = [c * MM for c in translation] if translation else None
        self.children = list(children)
        self.prims = {}

    def prim(self, material):
        return self.prims.setdefault(material, Prim())

    # -- shapes --

    def quad(self, material, p0, p1, p2, p3, uv=UNIT_UV, flip=False):
        p = self.prim(material)
        if flip:
            p.quad(p3, p2, p1, p0, (uv[3], uv[2], uv[1], uv[0]))
        else:
            p.quad(p0, p1, p2, p3, uv)

    def face_z(self, material, x0, x1, y0, y1, z, front=True, uv=UNIT_UV):
        """A wall facing along Z, seen from the front (+Z) or the back."""
        pts = [(x0, y0, z), (x1, y0, z), (x1, y1, z), (x0, y1, z)]
        self.quad(material, *pts, uv=uv, flip=not front)

    def face_y(self, material, x0, x1, z0, z1, y, up=True, uv=UNIT_UV):
        pts = [(x0, y, z1), (x1, y, z1), (x1, y, z0), (x0, y, z0)]
        self.quad(material, *pts, uv=uv, flip=not up)

    def face_x(self, material, z0, z1, y0, y1, x, right=True, uv=UNIT_UV):
        pts = [(x, y0, z0), (x, y0, z1), (x, y1, z1), (x, y1, z0)]
        self.quad(material, *pts, uv=uv, flip=not right)

    def box(self, material, lo, hi, faces=None):
        """An axis-aligned box. `faces` overrides the material or UVs of a
        named side: {"front": (material, uv), ...}."""
        x0, y0, z0 = lo
        x1, y1, z1 = hi
        faces = faces or {}

        def mat_uv(side):
            m, uv = faces.get(side, (material, UNIT_UV))
            return m, uv

        m, uv = mat_uv("front")
        self.face_z(m, x0, x1, y0, y1, z1, True, uv)
        m, uv = mat_uv("back")
        self.face_z(m, x0, x1, y0, y1, z0, False, uv)
        m, uv = mat_uv("right")
        self.face_x(m, z1, z0, y0, y1, x1, True, uv)
        m, uv = mat_uv("left")
        self.face_x(m, z0, z1, y0, y1, x0, False, uv)
        m, uv = mat_uv("top")
        self.face_y(m, x0, x1, z0, z1, y1, True, uv)
        m, uv = mat_uv("bottom")
        self.face_y(m, x0, x1, z0, z1, y0, False, uv)

    def prism(self, material, outline, y0, y1, cap_uv=None, skip=(),
              top_material=None, cap_material=None):
        """An outline in the XZ plane pulled up between two heights.

        `outline` runs anticlockwise seen from above. `skip` names the edge
        indices that are left open, which is how the front of the case is
        left free for the drawer and the panel strip.
        """
        n = len(outline)
        cx = sum(p[0] for p in outline) / n
        cz = sum(p[1] for p in outline) / n
        side = self.prim(material)
        for i in range(n):
            if i in skip:
                continue
            ax, az = outline[i]
            bx, bz = outline[(i + 1) % n]
            mx, mz = (ax + bx) * 0.5, (az + bz) * 0.5
            nrm = norm((-(bz - az), 0.0, bx - ax))
            if nrm[0] * (mx - cx) + nrm[2] * (mz - cz) < 0:
                nrm = (-nrm[0], 0.0, -nrm[2])
            side.quad((ax, y0, az), (bx, y0, bz), (bx, y1, bz), (ax, y1, az),
                      n=nrm)
        top = self.prim(cap_material or top_material or material)
        bottom = self.prim(material)
        for i in range(n):
            ax, az = outline[i]
            bx, bz = outline[(i + 1) % n]
            uvs = [cap_uv(p) if cap_uv else (0.5, 0.5)
                   for p in ((cx, cz), (ax, az), (bx, az if False else bz))]
            top.tri((cx, y1, cz), (bx, y1, bz), (ax, y1, az),
                    uvs[0], uvs[2], uvs[1], n=(0.0, 1.0, 0.0))
            bottom.tri((cx, y0, cz), (ax, y0, az), (bx, y0, bz),
                       (0.5, 0.5), (0.0, 0.0), (1.0, 1.0), n=(0.0, -1.0, 0.0))

    def cylinder(self, material, centre, r, y0, y1, seg=24, caps=True):
        p = self.prim(material)
        cx, cz = centre
        pts = [(cx + r * math.cos(2 * math.pi * i / seg),
                cz + r * math.sin(2 * math.pi * i / seg)) for i in range(seg)]
        for i in range(seg):
            ax, az = pts[i]
            bx, bz = pts[(i + 1) % seg]
            na = norm((ax - cx, 0.0, az - cz))
            nb = norm((bx - cx, 0.0, bz - cz))
            nn = norm(((na[0] + nb[0]) * 0.5, 0.0, (na[2] + nb[2]) * 0.5))
            p.quad((ax, y0, az), (bx, y0, bz), (bx, y1, bz), (ax, y1, az), n=nn)
        if caps:
            for i in range(seg):
                ax, az = pts[i]
                bx, bz = pts[(i + 1) % seg]
                p.tri((cx, y1, cz), (bx, y1, bz), (ax, y1, az),
                      (0.5, 0.5), (1.0, 1.0), (0.0, 0.0), n=(0.0, 1.0, 0.0))
                p.tri((cx, y0, cz), (ax, y0, az), (bx, y0, bz),
                      (0.5, 0.5), (0.0, 0.0), (1.0, 1.0), n=(0.0, -1.0, 0.0))

    def platter(self, material, r, hole, y0, y1, seg=48, uv_square=True):
        """A disc with a spindle hole: the CD itself, or the tray's plate."""
        p = self.prim(material)

        def at(i, radius):
            a = 2 * math.pi * i / seg
            return (radius * math.cos(a), radius * math.sin(a))

        def uv(x, z):
            if not uv_square:
                return (0.5, 0.5)
            return (0.5 + x / (2 * r), 0.5 + z / (2 * r))

        for i in range(seg):
            ax, az = at(i, r)
            bx, bz = at(i + 1, r)
            hax, haz = at(i, hole)
            hbx, hbz = at(i + 1, hole)
            for y, up in ((y1, True), (y0, False)):
                n = (0.0, 1.0, 0.0) if up else (0.0, -1.0, 0.0)
                quad = [(hax, y, haz), (ax, y, az), (bx, y, bz), (hbx, y, hbz)]
                uvs = [uv(q[0], q[2]) for q in quad]
                if up:
                    p.quad(*quad, uv=uvs, n=n)
                else:
                    p.quad(*quad[::-1], uv=uvs[::-1], n=n)
            # The rim and the hole's wall.
            p.quad((ax, y0, az), (bx, y0, bz), (bx, y1, bz), (ax, y1, az),
                   n=norm((ax, 0.0, az)))
            p.quad((hbx, y0, hbz), (hax, y0, haz), (hax, y1, haz),
                   (hbx, y1, hbz), n=norm((-hax, 0.0, -haz)))


def rounded_rect(x0, x1, z0, z1, r, seg=5):
    """A rounded rectangle outline in XZ, anticlockwise from above."""
    pts = []
    corners = [(x1 - r, z1 - r, 0.0), (x0 + r, z1 - r, 90.0),
               (x0 + r, z0 + r, 180.0), (x1 - r, z0 + r, 270.0)]
    for cx, cz, a0 in corners:
        for i in range(seg + 1):
            a = math.radians(a0 + 90.0 * i / seg)
            pts.append((cx + r * math.cos(a), cz + r * math.sin(a)))
    return pts


def ribbed_circle(cx, cz, r, ribs, depth, seg_per_rib=4):
    """A circle scalloped into vertical ribs — the FZ-1's corner pods."""
    pts = []
    n = ribs * seg_per_rib
    for i in range(n):
        a = 2 * math.pi * i / n
        rr = r - depth * (0.5 - 0.5 * math.cos(ribs * a))
        pts.append((cx + rr * math.cos(a), cz + rr * math.sin(a)))
    return pts


# --- glTF writer -----------------------------------------------------------


class Glb:
    def __init__(self):
        self.images = []
        self.materials = []
        self._mat_index = {}

    def image(self, pil):
        import io

        buf = io.BytesIO()
        pil.convert("RGBA").save(buf, "PNG", optimize=True)
        self.images.append(buf.getvalue())
        return len(self.images) - 1

    def material(self, name, color=(0.2, 0.2, 0.2, 1.0), image=None,
                 metallic=0.0, roughness=0.75, emissive=None):
        self._mat_index[name] = len(self.materials)
        self.materials.append({
            "name": name, "color": list(color), "image": image,
            "metallic": metallic, "roughness": roughness,
            "emissive": list(emissive) if emissive else None,
        })
        return name

    def write(self, path, nodes, animation=None):
        """`nodes` is the flat list; children are indices into it."""
        blob = bytearray()
        views = []
        accessors = []

        def view(data, stride=None):
            while len(blob) % 4:
                blob.append(0)
            start = len(blob)
            blob.extend(data)
            v = {"buffer": 0, "byteOffset": start, "byteLength": len(data)}
            if stride:
                v["byteStride"] = stride
            views.append(v)
            return len(views) - 1

        def accessor(values, kind, comp, minmax=False):
            fmt = {"SCALAR": 1, "VEC2": 2, "VEC3": 3}[kind]
            if comp == 5126:
                data = struct.pack("<%df" % (len(values) * fmt),
                                   *[c for v in values for c in
                                     (v if isinstance(v, (tuple, list)) else (v,))])
            else:
                data = struct.pack("<%dI" % len(values), *values)
            acc = {"bufferView": view(data), "componentType": comp,
                   "count": len(values), "type": kind}
            if minmax:
                cols = list(zip(*[v if isinstance(v, (tuple, list)) else (v,)
                                  for v in values]))
                acc["min"] = [min(c) for c in cols]
                acc["max"] = [max(c) for c in cols]
            accessors.append(acc)
            return len(accessors) - 1

        gltf_meshes = []
        gltf_nodes = []
        for node in nodes:
            entry = {"name": node.name}
            if node.translation:
                entry["translation"] = node.translation
            if node.children:
                entry["children"] = node.children
            if node.prims:
                prims = []
                for mat, prim in node.prims.items():
                    if not prim.idx:
                        continue
                    prims.append({
                        "attributes": {
                            "POSITION": accessor(prim.pos, "VEC3", 5126, True),
                            "NORMAL": accessor(prim.nrm, "VEC3", 5126),
                            "TEXCOORD_0": accessor(prim.uv, "VEC2", 5126),
                        },
                        "indices": accessor(prim.idx, "SCALAR", 5125),
                        "material": self._mat_index[mat],
                    })
                if prims:
                    gltf_meshes.append({"name": node.name, "primitives": prims})
                    entry["mesh"] = len(gltf_meshes) - 1
            gltf_nodes.append(entry)

        images = []
        for png in self.images:
            images.append({"bufferView": view(png), "mimeType": "image/png"})
        textures = [{"source": i, "sampler": 0} for i in range(len(images))]
        materials = []
        for m in self.materials:
            pbr = {
                "baseColorFactor": m["color"],
                "metallicFactor": m["metallic"],
                "roughnessFactor": m["roughness"],
            }
            if m["image"] is not None:
                pbr["baseColorTexture"] = {"index": m["image"]}
            entry = {"name": m["name"], "pbrMetallicRoughness": pbr}
            if m["emissive"]:
                entry["emissiveFactor"] = m["emissive"]
            materials.append(entry)

        gltf = {
            "asset": {"version": "2.0", "generator": "retro3do make-fz1-model"},
            "scene": 0,
            "scenes": [{"nodes": [i for i, n in enumerate(nodes)
                                  if not any(i in p.children for p in nodes)]}],
            "nodes": gltf_nodes,
            "meshes": gltf_meshes,
            "materials": materials,
            "accessors": accessors,
            "bufferViews": views,
        }
        if images:
            gltf["images"] = images
            gltf["textures"] = textures
            gltf["samplers"] = [{"magFilter": 9729, "minFilter": 9987,
                                 "wrapS": 33071, "wrapT": 33071}]
        if animation:
            name, node_index, times, values = animation
            t_acc = accessor(times, "SCALAR", 5126, True)
            v_acc = accessor(values, "VEC3", 5126)
            gltf["animations"] = [{
                "name": name,
                "samplers": [{"input": t_acc, "output": v_acc,
                              "interpolation": "LINEAR"}],
                "channels": [{"sampler": 0,
                              "target": {"node": node_index,
                                         "path": "translation"}}],
            }]
        gltf["buffers"] = [{"byteLength": len(blob)}]

        js = json.dumps(gltf, separators=(",", ":")).encode()
        js += b" " * ((-len(js)) % 4)
        while len(blob) % 4:
            blob.append(0)
        out = struct.pack("<III", 0x46546C67, 2,
                          12 + 8 + len(js) + 8 + len(blob))
        out += struct.pack("<II", len(js), 0x4E4F534A) + js
        out += struct.pack("<II", len(blob), 0x004E4942) + bytes(blob)
        path.write_bytes(out)
        print(f"{path.relative_to(ROOT)}  {len(out) / 1024:.0f} KiB, "
              f"{len(gltf_nodes)} nodes, {len(materials)} materials")



# --- the rest of the console's skin ----------------------------------------


def speckle(w, h, rgb, amount=6, seed=3):
    """Moulded plastic: a fine, even grain rather than a flat fill."""
    import random

    rng = random.Random(seed)
    img = Image.new("RGBA", (w, h), rgb + (255,))
    px = img.load()
    for y in range(h):
        for x in range(w):
            n = rng.randint(-amount, amount)
            px[x, y] = (rgb[0] + n, rgb[1] + n, rgb[2] + n, 255)
    return img.filter(ImageFilter.SMOOTH)


def tex_lower(w=1024, h=128):
    """The band under the fascia: plain, with the socket's label beside it.

    The socket itself is geometry; its position on this strip (the strip runs
    -78..78 mm, the socket sits at +31 mm) puts the label just right of it.
    """
    img = speckle(w, h, (8, 8, 9), 2, 11)
    d = ImageDraw.Draw(img)
    f = ImageFont.truetype(SANS, int(h * 0.20))
    tracked(d, (w * 0.56, h * 0.42), "CONTROLLER", f, (100, 100, 104, 255),
            tracking=w * 0.002)
    return img


def tex_side(w=1024, h=512):
    """A flank between the pods: the ribbing that runs back from the corners."""
    img = speckle(w, h, (11, 11, 12), 2, 5)
    d = ImageDraw.Draw(img)
    ribs = 34
    for i in range(ribs):
        x = w * (i + 0.5) / ribs
        d.line([(x - 4, 0), (x - 4, h)], fill=(20, 20, 22, 255), width=3)
        d.line([(x + 3, 0), (x + 3, h)], fill=(4, 4, 5, 255), width=4)
    # The shell's split line, at the height of the button strips.
    y = int(h * (1 - (50 - 8) / 72))
    d.line([(0, y), (w, y)], fill=(2, 2, 3, 255), width=4)
    return img


def tex_rear(w=1024, h=256):
    """The back panel, seen from behind (so left here is the console's right).

    Laid out from the FZ-1's operating manual rear view: the expansion port
    under its cover at one end, the AV and RF outputs and the channel switch in
    the middle, the mains lead and the rating plate at the other end.
    """
    img = speckle(w, h, (10, 10, 11), 2, 9)
    d = ImageDraw.Draw(img)
    lab = (120, 120, 126, 255)
    f = ImageFont.truetype(SANS, int(h * 0.075))
    fs = ImageFont.truetype(SANS_C, int(h * 0.060))
    # Expansion port (console's right, seen on the left from behind).
    d.rounded_rectangle([w * 0.04, h * 0.30, w * 0.30, h * 0.70], radius=8,
                        fill=(12, 12, 13, 255), outline=(60, 60, 64, 255), width=3)
    d.rectangle([w * 0.07, h * 0.44, w * 0.27, h * 0.56], fill=(4, 4, 5, 255))
    tracked(d, (w * 0.17, h * 0.18), "EXPANSION", f, lab, 2, centre=True)
    # Output group.
    tracked(d, (w * 0.40, h * 0.16), "VIDEO", fs, lab, 1, centre=True)
    tracked(d, (w * 0.49, h * 0.16), "L  AUDIO  R", fs, lab, 1, centre=True)
    tracked(d, (w * 0.445, h * 0.76), "AV OUT", fs, lab, 1, centre=True)
    tracked(d, (w * 0.61, h * 0.16), "S-VIDEO", fs, lab, 1, centre=True)
    tracked(d, (w * 0.61, h * 0.76), "OUT", fs, lab, 1, centre=True)
    tracked(d, (w * 0.72, h * 0.16), "RF OUT", fs, lab, 1, centre=True)
    tracked(d, (w * 0.80, h * 0.16), "CH", fs, lab, 1, centre=True)
    tracked(d, (w * 0.80, h * 0.76), "3   4", fs, lab, 1, centre=True)
    d.rounded_rectangle([w * 0.785, h * 0.40, w * 0.815, h * 0.60], radius=4,
                        fill=(8, 8, 9, 255), outline=(70, 70, 74, 255))
    d.rectangle([w * 0.79, h * 0.42, w * 0.81, h * 0.49], fill=(150, 150, 150, 255))
    # Rating plate by the lead.
    d.rectangle([w * 0.86, h * 0.20, w * 0.98, h * 0.82], fill=(92, 92, 96, 255))
    fp = ImageFont.truetype(SANS_C, int(h * 0.050))
    for i, line in enumerate(["Panasonic", "FZ-1", "AC 120V 60Hz", "22 W",
                              "MADE IN JAPAN"]):
        d.text((w * 0.868, h * (0.24 + i * 0.11)), line, font=fp,
               fill=(20, 20, 22, 255))
    return img


def tex_bottom(w=512, h=512):
    """The underside: vent slots and the serial sticker."""
    img = speckle(w, h, (9, 9, 10), 2, 13)
    d = ImageDraw.Draw(img)
    for i in range(14):
        y = h * (0.18 + i * 0.03)
        d.rounded_rectangle([w * 0.12, y, w * 0.48, y + h * 0.012], radius=3,
                            fill=(6, 6, 7, 255))
    d.rectangle([w * 0.56, h * 0.30, w * 0.88, h * 0.56], fill=(140, 136, 128, 255))
    f = ImageFont.truetype(SANS_C, int(h * 0.030))
    for i, line in enumerate(["Panasonic", "REAL 3DO", "INTERACTIVE MULTIPLAYER",
                              "MODEL NO. FZ-1", "SERIAL NO. ________",
                              "MATSUSHITA ELECTRIC"]):
        d.text((w * 0.575, h * (0.32 + i * 0.038)), line, font=f,
               fill=(30, 30, 30, 255))
    return img


def kraft(w=256, h=256):
    """The carton's raw inside: brown board with its corrugation showing."""
    img = speckle(w, h, (104, 80, 54), 6, 17)
    d = ImageDraw.Draw(img)
    for i in range(0, w, 8):
        d.line([(i, 0), (i, h)], fill=(92, 70, 46, 255), width=2)
    return img


# --- the console ----------------------------------------------------------

# Millimetres, x across the face, y up, z toward the viewer. The pods' fronts
# sit at z = +134 and the back pods' at -134: 286 x 272 x 88 overall.
FACE_Z = 112.0
POD_F = ((-111.0, 102.0), (111.0, 102.0))   # front pods, radius 32
POD_B = ((-113.0, -104.0), (113.0, -104.0))  # back pods, radius 30
POD_RF, POD_RB = 32.0, 30.0
BUTTON_Y = (46.0, 54.5)


def arc_band(node, face_mat, side_mat, centre, r0, r1, a0, a1, y0, y1, seg=10):
    """A strip curved round a pod: the bronze buttons and the light behind.

    Angles in degrees, measured from +x toward +z, so on the front of the
    machine a rising angle walks left; the face's U runs the other way so its
    label reads left to right.
    """
    cx, cz = centre
    face = node.prim(face_mat)
    side = node.prim(side_mat)

    def at(a, r):
        t = math.radians(a)
        return (cx + r * math.cos(t), cz + r * math.sin(t))

    for i in range(seg):
        aa = a0 + (a1 - a0) * i / seg
        ab = a0 + (a1 - a0) * (i + 1) / seg
        ua = 1.0 - i / seg
        ub = 1.0 - (i + 1) / seg
        (oax, oaz), (obx, obz) = at(aa, r1), at(ab, r1)
        (iax, iaz), (ibx, ibz) = at(aa, r0), at(ab, r0)
        n = norm(((oax + obx) * 0.5 - cx, 0.0, (oaz + obz) * 0.5 - cz))
        face.quad((oax, y0, oaz), (obx, y0, obz), (obx, y1, obz), (oax, y1, oaz),
                  uv=((ua, 1.0), (ub, 1.0), (ub, 0.0), (ua, 0.0)), n=n)
        side.quad((iax, y1, iaz), (oax, y1, oaz), (obx, y1, obz), (ibx, y1, ibz),
                  n=(0.0, 1.0, 0.0))
        side.quad((iax, y0, iaz), (ibx, y0, ibz), (obx, y0, obz), (oax, y0, oaz),
                  n=(0.0, -1.0, 0.0))
    for a in (a0, a1):
        (ox, oz), (ix, iz) = at(a, r1), at(a, r0)
        side.quad((ix, y0, iz), (ox, y0, oz), (ox, y1, oz), (ix, y1, iz))


def pod_angles(cx, x_lo, x_hi, r):
    """The arc of a pod whose front spans x_lo..x_hi."""
    a = math.degrees(math.acos(max(-1.0, min(1.0, (x_hi - cx) / r))))
    b = math.degrees(math.acos(max(-1.0, min(1.0, (x_lo - cx) / r))))
    return a, b


def build_console():
    g = Glb()
    g.material("Case", (0.034, 0.034, 0.037, 1.0))
    g.material("Foot", (0.022, 0.022, 0.024, 1.0))
    g.material("Panel", (0.012, 0.012, 0.013, 1.0))
    g.material("Port", (0.006, 0.006, 0.007, 1.0))
    g.material("Bronze", (0.25, 0.20, 0.16, 1.0))
    g.material("Tray", (0.028, 0.028, 0.030, 1.0))
    g.material("Jack", (0.55, 0.55, 0.57, 1.0))
    g.material("JackYellow", (0.80, 0.62, 0.08, 1.0))
    g.material("JackWhite", (0.82, 0.82, 0.80, 1.0))
    g.material("JackRed", (0.70, 0.08, 0.06, 1.0))
    g.material("Lid", image=g.image(tex_lid(1024, 946)), color=(1, 1, 1, 1))
    g.material("Fascia", image=g.image(tex_fascia(2048, 274)), color=(1, 1, 1, 1))
    g.material("Door", image=g.image(tex_door(1024, 154)), color=(1, 1, 1, 1))
    g.material("Lower", image=g.image(tex_lower()), color=(1, 1, 1, 1))
    g.material("PortFace", image=g.image(tex_port()), color=(1, 1, 1, 1))
    g.material("PowerFace", image=g.image(tex_button("POWER")), color=(1, 1, 1, 1))
    g.material("OpenFace", image=g.image(tex_button("OPEN/CLOSE")), color=(1, 1, 1, 1))
    g.material("Side", image=g.image(tex_side()), color=(1, 1, 1, 1))
    g.material("Rear", image=g.image(tex_rear()), color=(1, 1, 1, 1))
    g.material("Bottom", image=g.image(tex_bottom()), color=(1, 1, 1, 1))
    g.material("Disc", image=g.image(Image.new("RGBA", (4, 4), (176, 176, 180, 255))),
               color=(1, 1, 1, 1))
    # The lamps: dark until lit; the emissive factor is the colour they glow.
    g.material("ReadyLed", (0.05, 0.012, 0.010, 1.0), emissive=(1.0, 0.0, 0.0))
    g.material("AccessLed", (0.012, 0.04, 0.016, 1.0), emissive=(0.0, 1.0, 0.1))

    body = Node("FZ1_Body")

    # The shell: a lower tub and the upper band, then the lid plate on top.
    shell = rounded_rect(-126.0, 126.0, -124.0, FACE_Z, 10.0)
    body.prism("Case", shell, 8.0, 56.0)
    body.prism("Case", rounded_rect(-125.2, 125.2, -123.2, FACE_Z - 0.8, 9.5),
               55.0, 57.0)  # the split line, a hair inside
    body.prism("Case", shell, 57.0, 80.0)
    lx0, lx1, lz0, lz1 = -118.0, 118.0, -118.0, 100.0
    body.prism("Case", rounded_rect(lx0, lx1, lz0, lz1, 12.0, 6), 80.0, 86.5,
               cap_uv=lambda p: ((p[0] - lx0) / (lx1 - lx0), (p[1] - lz0) / (lz1 - lz0)),
               cap_material="Lid")

    # Corner pods: fluted columns on round feet, the front pair carrying the
    # buttons.
    for cx, cz in POD_F:
        body.prism("Case", ribbed_circle(cx, cz, POD_RF, 26, 1.8), 14.0, 80.0)
        body.cylinder("Foot", (cx, cz - 1.0), POD_RF - 2.5, 0.0, 14.0, seg=32)
    for cx, cz in POD_B:
        body.prism("Case", ribbed_circle(cx, cz, POD_RB, 24, 1.6), 6.0, 80.0)
        body.cylinder("Foot", (cx, cz), POD_RB - 3.0, 0.0, 6.0, seg=32)

    # Flanks: the ribbed side panels between the pods.
    for x, right in ((126.15, True), (-126.15, False)):
        uv = UNIT_UV if right else ((1.0, 1.0), (0.0, 1.0), (0.0, 0.0), (1.0, 0.0))
        body.face_x("Side", 74.0, -74.0, 8.0, 80.0, x, right,
                    uv=((0.0, 1.0), (1.0, 1.0), (1.0, 0.0), (0.0, 0.0)) if right else uv)

    # Back panel, read from behind, and what plugs into it.
    body.face_z("Rear", -84.0, 84.0, 10.0, 74.0, -124.15, front=False,
                uv=((1.0, 1.0), (0.0, 1.0), (0.0, 0.0), (1.0, 0.0)))
    # Panel x runs 84 (u=0) to -84 (u=1); jacks sit where the panel prints them.
    def rear_x(u):
        return 84.0 - 168.0 * u
    for u, mat in ((0.40, "JackYellow"), (0.465, "JackWhite"), (0.515, "JackRed"),
                   (0.72, "Jack")):
        x = rear_x(u)
        body.box("Jack", (x - 3.6, 38.0, -128.0), (x + 3.6, 46.0, -124.0))
        body.box(mat, (x - 2.6, 39.0, -129.0), (x + 2.6, 45.0, -127.9))
        body.box("Port", (x - 1.0, 41.0, -129.2), (x + 1.0, 43.0, -129.0))
    x = rear_x(0.61)
    body.box("Port", (x - 4.5, 37.5, -126.0), (x + 4.5, 46.5, -124.0))
    # The mains lead leaving the back at the rating plate's end: its stub
    # stops a hair past the rear pods (-136 to their -134) so the machine
    # stands flat on its rear edge inside the carton.
    x = rear_x(0.93)
    body.box("Port", (x - 4.0, 18.0, -134.0), (x + 4.0, 26.0, -124.0))
    body.box("Port", (x - 2.4, 19.6, -136.0), (x + 2.4, 24.4, -125.0))

    # Underside.
    body.face_y("Bottom", -110.0, 110.0, -110.0, 96.0, 7.9, up=False)

    # The face, between the pods: the drawer's opening (dark, it shows when
    # the drawer is out), the fascia under it, the low band with the socket.
    body.box("Panel", (-72.0, 54.5, FACE_Z - 1.0), (72.0, 78.5, FACE_Z + 1.0))
    body.box("Panel", (-88.0, 27.0, FACE_Z), (88.0, 52.0, FACE_Z + 2.0))
    body.box("Panel", (-86.0, 28.0, FACE_Z), (86.0, 51.0, FACE_Z + 2.5),
             faces={"front": ("Fascia", UNIT_UV)})
    body.box("Case", (-80.0, 11.0, FACE_Z - 3.0), (80.0, 27.0, FACE_Z + 0.4),
             faces={"front": ("Lower", UNIT_UV)})
    # The controller socket: a small D-shaped well, centred under the fascia.
    px = 0.0
    body.box("Port", (px - 7.0, 15.5, FACE_Z), (px + 7.0, 22.5, FACE_Z + 1.4))
    body.face_z("PortFace", px - 6.0, px + 6.0, 16.3, 21.7, FACE_Z + 1.45)

    # Buttons: bronze strips curved round the front pods, and under each a
    # small lamp lens on the pod itself — red under POWER, green under
    # OPEN/CLOSE. Nothing lit sits on the flat face between the pods.
    pods = {}
    for (cx, cz), name, face, led, gname, span in (
        (POD_F[0], "PowerButton", "PowerFace", "ReadyLed", "ReadyGlow", (-134.0, -101.0)),
        (POD_F[1], "OpenCloseButton", "OpenFace", "AccessLed", "AccessGlow", (101.0, 134.0)),
    ):
        a0, a1 = pod_angles(cx, span[0], span[1], POD_RF + 1.5)
        btn = Node(name)
        arc_band(btn, face, "Bronze", (cx, cz), POD_RF - 1.0, POD_RF + 1.8,
                 a0, a1, BUTTON_Y[0], BUTTON_Y[1])
        glow = Node(gname)
        mid = (a0 + a1) * 0.5
        arc_band(glow, led, led, (cx, cz), POD_RF - 0.5, POD_RF + 1.2,
                 mid - 4.0, mid + 4.0, BUTTON_Y[0] - 5.5, BUTTON_Y[0] - 3.0)
        pods[name] = btn
        pods[gname] = glow

    # The drawer: its gold-lettered door, the plate behind it and the disc.
    tray = Node("FZ1_Tray")
    tray.box("Tray", (-70.0, 55.5, FACE_Z), (70.0, 77.5, FACE_Z + 3.4),
             faces={"front": ("Door", UNIT_UV)})
    tray.box("Tray", (-66.0, 58.0, -62.0), (66.0, 63.0, FACE_Z))
    tray.cylinder("Panel", (0.0, 28.0), 62.0, 63.0, 63.3, seg=48)
    disc = Node("Disc", translation=(0.0, 64.0, 28.0))
    disc.platter("Disc", 60.0, 7.5, -0.6, 0.6)

    nodes = [body, pods["PowerButton"], pods["OpenCloseButton"], pods["ReadyGlow"],
             pods["AccessGlow"], tray, disc]
    tray_root = Node("TrayRoot", translation=(0.0, 0.0, 0.0), children=[5, 6])
    nodes.append(tray_root)
    root = Node("FZ1", children=[0, 1, 2, 3, 4, 7])
    nodes.append(root)
    g.write(ASSETS / "fz1.glb", nodes,
            animation=("TrayOpen", 7, [0.0, 1.0],
                       [(0.0, 0.0, 0.0), (0.0, 0.0, 0.130)]))





# --- the carton ------------------------------------------------------------

# 360 x 290 x 130 mm: the wide, shallow carton the machine really came in.
# The console (286 x 272 x 88) goes in lengthways standing on its rear panel -
# back face down on the foam bed, the CD drawer's face up toward the lid - with
# a foam insert either side filling the rest of the width.
BOX_W, BOX_H, BOX_D = 360.0, 290.0, 130.0


def build_box():
    g = Glb()
    g.material("Kraft", image=g.image(kraft()), color=(1, 1, 1, 1))
    g.material("Foam", (0.40, 0.40, 0.39, 1.0))
    g.material("FoamWhite", (0.86, 0.86, 0.84, 1.0))
    g.material("BoxFront", image=g.image(tex_box_front()), color=(1, 1, 1, 1))
    g.material("BoxBack", image=g.image(tex_box_back()), color=(1, 1, 1, 1))
    g.material("BoxSide", image=g.image(tex_box_side()), color=(1, 1, 1, 1))
    g.material("BoxTop", image=g.image(tex_box_top()), color=(1, 1, 1, 1))
    x1, z1, h = BOX_W / 2, BOX_D / 2, BOX_H
    t = 2.0  # board thickness: the inside is a second skin of raw board
    flip_u = ((1.0, 1.0), (0.0, 1.0), (0.0, 0.0), (1.0, 0.0))
    # The console stood on its rear panel, centred: 286 across, 272 up (the
    # cable stub's tip to the drawer's buttons), 86 front to back.
    cx, cy, pad = 144.0, 280.0, 45.0
    bed = 10.0  # the foam bed the rear panel rests on

    body = Node("Carton")
    body.face_z("BoxFront", -x1, x1, 0.0, h, z1, True)
    body.face_z("BoxBack", -x1, x1, 0.0, h, -z1, False, uv=flip_u)
    body.face_x("BoxSide", z1, -z1, 0.0, h, x1, True)
    body.face_x("BoxSide", -z1, z1, 0.0, h, -x1, False)
    body.face_y("Kraft", -x1, x1, -z1, z1, 0.0, up=False)
    # Inner skin and the rim between the skins.
    body.face_z("Kraft", -x1 + t, x1 - t, t, h, z1 - t, False)
    body.face_z("Kraft", -x1 + t, x1 - t, t, h, -z1 + t, True)
    body.face_x("Kraft", -z1 + t, z1 - t, t, h, x1 - t, False)
    body.face_x("Kraft", z1 - t, -z1 + t, t, h, -x1 + t, True)
    body.face_y("Kraft", -x1 + t, x1 - t, -z1 + t, z1 - t, t, up=True)
    body.face_y("Kraft", -x1, x1, z1 - t, z1, h)
    body.face_y("Kraft", -x1, x1, -z1, -z1 + t, h)
    body.face_y("Kraft", x1 - t, x1, -z1, z1, h)
    body.face_y("Kraft", -x1, -x1 + t, -z1, z1, h)
    # Foam: a bed across the floor for the rear panel to stand on, and an
    # insert either side of the console filling the rest of the width.
    body.box("FoamWhite", (-x1 + t, t, -z1 + t), (x1 - t, bed, z1 - t))
    for sgn in (-1.0, 1.0):
        x0, xa = sgn * cx, sgn * (x1 - t)
        body.box("FoamWhite", (min(x0, xa), bed, -z1 + t),
                 (max(x0, xa), bed + cy - 8.0, z1 - t))
    # End pads front and back: the machine's height leaves ~18 mm of the
    # carton's depth free either side of it.
    body.box("FoamWhite", (-cx, bed, -z1 + t), (cx, bed + cy - 8.0, -pad))
    body.box("FoamWhite", (-cx, bed, pad), (cx, bed + cy - 8.0, z1 - t))

    # The lid, hinged along the top of the back wall; it carries the top art.
    lid = Node("BoxLid", translation=(0.0, h, -z1))
    lid.face_y("BoxTop", -x1, x1, 0.0, BOX_D, 0.0, up=True)
    lid.face_y("Kraft", -x1, x1, 0.0, BOX_D, -t, up=False)
    lid.face_z("BoxFront", -x1, x1, -t, 0.0, BOX_D, True,
               uv=((0.0, 0.01), (1.0, 0.01), (1.0, 0.0), (0.0, 0.0)))
    # The dust flaps under it, hinged along the side walls' tops.
    flaps = []
    fw = BOX_D - 2 * t  # a shallow carton's dust flaps are only as wide as it is deep
    for name, x, s in (("FlapLeft", -x1 + t, 1.0), ("FlapRight", x1 - t, -1.0)):
        f = Node(name, translation=(x, h - t - 0.5, 0.0))
        f.face_y("Kraft", min(0.0, s * fw), max(0.0, s * fw), -z1 + t, z1 - t, 0.0)
        flaps.append(f)

    nodes = [body, lid, flaps[0], flaps[1]]
    nodes.append(Node("FZ1_Box", children=[0, 1, 2, 3]))
    g.write(ASSETS / "fz1-box.glb", nodes)

if __name__ == "__main__":
    build_console()
    build_box()
