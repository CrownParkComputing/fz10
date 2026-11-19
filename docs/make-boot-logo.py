#!/usr/bin/env python3
"""Bakes the recreated Panasonic FZ-1 boot screen and the standalone 3DO
badge, modelled on docs/boot-logo-reference.png (and the frame the BIOS
itself draws). Outputs:

    app/assets/boot-logo-real.png   1280x960 — the full boot card on black
    app/assets/3do-mark.png          240x480 — the badge alone, transparent

Run from the repo root: python3 docs/make-boot-logo.py
"""

from PIL import Image, ImageDraw, ImageFont, ImageFilter

ROOT = __file__.rsplit("/", 2)[0] if "/" in __file__ else "."
OUT = f"{ROOT}/app/assets"

SERIF = "/usr/share/fonts/TTF/DejaVuSerifCondensed-Bold.ttf"
SERIF_FULL = "/usr/share/fonts/TTF/DejaVuSerif-Bold.ttf"
SANS_C = "/usr/share/fonts/TTF/DejaVuSansCondensed-Bold.ttf"
SANS = "/usr/share/fonts/liberation/LiberationSans-Bold.ttf"

BLUE = (28, 40, 220, 255)          # Panasonic / underline blue
RED = (224, 30, 40, 255)
GREEN = (30, 190, 70, 255)
NAVY = (40, 30, 150, 255)          # badge's middle square
GOLD = (238, 178, 40, 255)         # badge's circle
BADGE_GREY = (150, 150, 155, 255)


def chrome_ramp(t):
    """Vertical chrome ramp, 0 top to 1 bottom: bright, dark horizon mid,
    bright again at the foot — the flattened metal look of the wordmark."""
    stops = [
        (0.00, 235),
        (0.30, 205),
        (0.46, 120),
        (0.52, 70),
        (0.58, 110),
        (0.80, 185),
        (1.00, 225),
    ]
    for i in range(len(stops) - 1):
        (t0, v0), (t1, v1) = stops[i], stops[i + 1]
        if t <= t1:
            f = (t - t0) / max(t1 - t0, 1e-6)
            v = v0 + (v1 - v0) * f
            return (int(v), int(v), int(v + 4), 255)
    return (225, 225, 229, 255)


def chrome_fill(mask):
    """Fill a white-on-black alpha mask with the chrome ramp, plus a thin
    dark outline so the letters read on black."""
    w, h = mask.size
    outline = mask.filter(ImageFilter.MaxFilter(5)).point(lambda v: min(v, 200))
    ramp = Image.new("RGBA", (w, h))
    px = ramp.load()
    for y in range(h):
        c = chrome_ramp(y / max(h - 1, 1))
        for x in range(w):
            px[x, y] = c
    dark = Image.new("RGBA", (w, h), (40, 40, 46, 255))
    out = Image.new("RGBA", (w, h))
    out.paste(dark, (0, 0), outline)
    out.paste(ramp, (0, 0), mask)
    return out


def tracked(draw, xy, text, font, fill, tracking=0.0, anchor_mid=None):
    """Draw text with per-letter tracking, horizontally centred at xy[0]."""
    widths = [draw.textlength(ch, font=font) for ch in text]
    total = sum(widths) + tracking * (len(text) - 1)
    x = xy[0] - total / 2 if anchor_mid else xy[0]
    for ch, w in zip(text, widths):
        draw.text((x, xy[1]), ch, font=font, fill=fill)
        x += w + tracking
    return total


def draw_badge(canvas, cx, top, h):
    """The 3DO badge: white rounded card, red diamond / navy square / gold
    circle stacked over their shadow, black '3DO' along the foot."""
    d = ImageDraw.Draw(canvas)
    w = h * 0.52
    box = (cx - w / 2, top, cx + w / 2, top + h)
    d.rounded_rectangle(box, radius=h * 0.055, fill=(246, 246, 248, 255),
                        outline=BADGE_GREY, width=max(2, int(h * 0.012)))
    s = h * 0.075  # half-size of the shapes
    # red diamond
    cy = top + h * 0.155
    d.polygon([(cx, cy - s), (cx + s, cy), (cx, cy + s), (cx - s, cy)], fill=RED)
    # navy square
    cy = top + h * 0.30
    d.rounded_rectangle((cx - s, cy - s, cx + s, cy + s), radius=s * 0.15, fill=NAVY)
    # gold circle with a darker foot
    cy = top + h * 0.455
    d.ellipse((cx - s, cy - s, cx + s, cy + s), fill=GOLD, outline=(150, 110, 20, 255),
              width=max(1, int(h * 0.008)))
    d.chord((cx - s, cy - s, cx + s, cy + s), 200, 340, fill=(205, 148, 25, 255))
    # cast shadow
    cy = top + h * 0.575
    d.ellipse((cx - h * 0.085, cy - h * 0.022, cx + h * 0.085, cy + h * 0.022),
              fill=(120, 120, 125, 255))
    # '3DO' along the foot
    font = ImageFont.truetype(SERIF_FULL, int(h * 0.165))
    tracked(d, (cx, top + h * 0.80), "3DO", font, (15, 15, 18, 255),
            tracking=h * 0.03, anchor_mid=True)


def make_mark():
    h = 480
    w = int(h * 0.52)
    img = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    draw_badge(img, w / 2, 0, h)
    img.save(f"{OUT}/3do-mark.png")


def make_card():
    W, H = 1280, 960
    img = Image.new("RGBA", (W, H), (0, 0, 0, 255))

    badge_h = int(H * 0.28)
    draw_badge(img, W / 2, H * 0.065, badge_h)

    # --- R·E·A·L in chrome -------------------------------------------------
    # Each letter is rendered on its own, stretched wide, then set with a
    # gap holding a small coloured square — red, green, blue like the card.
    lh = int(H * 0.140)
    stretch = 1.55
    font = ImageFont.truetype(SERIF, lh)
    pad = int(lh * 0.30)
    letters, dots = "REAL", [RED, GREEN, BLUE]
    gap = int(lh * 0.34)
    dot = int(lh * 0.20)
    parts = []
    for c in letters:
        cell = Image.new("L", (int(lh * 1.4), lh + pad * 2), 0)
        cd = ImageDraw.Draw(cell)
        cd.text((0, pad * 0.6), c, font=font, fill=255)
        cell = cell.crop(cell.getbbox() or (0, 0, 1, 1))
        cell = cell.resize((int(cell.width * stretch), cell.height),
                           Image.LANCZOS)
        parts.append(cell)
    word_h = max(p.height for p in parts) + pad
    word_w = int(sum(p.width for p in parts) + gap * 3 + pad * 2)
    mask = Image.new("L", (word_w, word_h), 0)
    xs = []
    x = pad
    for i, cell in enumerate(parts):
        mask.paste(cell, (x, (word_h - cell.height) // 2), cell)
        xs.append((x, cell.width))
        x += cell.width + (gap if i < 3 else 0)
    word = chrome_fill(mask)

    wx = (W - word_w) // 2
    wy = int(H * 0.39)
    img.paste(word, (wx, wy), word)
    d = ImageDraw.Draw(img)
    # separator squares, centred in each gap, a touch above mid-height
    for i in range(3):
        gx = wx + xs[i][0] + xs[i][1]
        cy = wy + word_h * 0.58
        d.rectangle((gx + (gap - dot) / 2, cy - dot / 2,
                     gx + (gap + dot) / 2, cy + dot / 2), fill=dots[i])
    end_x = wx + xs[-1][0] + xs[-1][1]

    # ™
    tm_font = ImageFont.truetype(SANS, int(lh * 0.30))
    tm_x = end_x + int(lh * 0.10)
    d.text((tm_x, wy + word_h * 0.28), "™", font=tm_font,
           fill=(200, 200, 205, 255))

    # blue rule under the wordmark
    rule_y = int(H * 0.592)
    d.rectangle((wx + pad * 0.3, rule_y, tm_x + int(lh * 0.55), rule_y + int(H * 0.007)),
                fill=BLUE)

    # caption
    cap_font = ImageFont.truetype(SANS_C, int(H * 0.031))
    tracked(d, (W / 2, int(H * 0.620)), "3DO™  INTERACTIVE  MULTIPLAYER™",
            cap_font, (228, 228, 232, 255), tracking=W * 0.006, anchor_mid=True)

    # Panasonic
    pan_font = ImageFont.truetype(SANS, int(H * 0.105))
    tw = d.textlength("Panasonic", font=pan_font)
    d.text((W / 2 - tw / 2, int(H * 0.74)), "Panasonic", font=pan_font, fill=BLUE)

    img.save(f"{OUT}/boot-logo-real.png")


if __name__ == "__main__":
    make_mark()
    make_card()
    print("wrote app/assets/boot-logo-real.png and app/assets/3do-mark.png")
