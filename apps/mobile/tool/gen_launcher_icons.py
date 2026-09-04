#!/usr/bin/env python3
"""Draws the launcher icon PNGs for Android's pre-adaptive densities (API 24-25).

Everything from API 26 up uses the vector adaptive icon
(`res/mipmap-anydpi-v26/ic_launcher.xml`), so this exists only for the two API levels
below it -- and it has to draw **the same picture**, or the app has two icons depending
on the phone. The geometry is `ic_launcher_foreground.xml` in its own 108-unit viewport,
on the gold the background layer uses; the note is the desktop's `icon.rs` drawing, so
all three platforms show one icon. Change one, change the others.

Run from apps/mobile:

    python3 tool/gen_launcher_icons.py
"""

from math import cos, radians, sin

from PIL import Image, ImageDraw

OUT = "android/app/src/main/res"
# mipmap density -> icon edge in px, the size Android asks each bucket for.
DENSITIES = {"mdpi": 48, "hdpi": 72, "xhdpi": 96, "xxhdpi": 144, "xxxhdpi": 192}

GOLD = (230, 210, 74, 255)       # background: the app's Material seed color
PAPER = (255, 252, 227, 255)     # note body: the yellow palette's paper
EDGE = (140, 123, 30, 255)       # outline
FOLD = (216, 198, 92, 255)       # the dog-eared corner
RULE = (184, 166, 58, 255)       # the ruled lines

SS = 8  # supersampling; the diagonal fold and the rounded corners need it
R = 6   # corner radius, in viewport units


def _corner(cx, cy, start_deg, u):
    """A quarter circle of radius R, as points: PIL polygons have no arcs."""
    return [
        ((cx + R * cos(radians(start_deg + t))) * u, (cy + R * sin(radians(start_deg + t))) * u)
        for t in range(0, 91, 6)
    ]


def draw(size):
    """The icon at `size` px, drawn in the 108-unit viewport and scaled down."""
    s = size * SS
    u = s / 108.0  # one viewport unit in pixels

    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    # Pre-API-26 launchers draw the bitmap as it is, so it brings its own shape; the
    # adaptive icon leaves the masking to the system and its background is a flat color.
    d.rounded_rectangle([(0, 0), (s - 1, s - 1)], radius=22 * u, fill=GOLD)
    stroke = max(1, round(2.5 * u))

    # The page: three rounded corners, with the top-right one cut away by the fold.
    body = (
        [(37 * u, 27 * u), (63 * u, 27 * u), (77 * u, 41 * u), (77 * u, 75 * u)]
        + _corner(71, 75, 0, u)      # bottom right
        + _corner(37, 75, 90, u)     # bottom left
        + [(31 * u, 33 * u)]
        + _corner(37, 33, 180, u)    # top left
    )
    d.polygon(body, fill=PAPER, outline=EDGE, width=stroke)

    # The fold, and the diagonal that separates it from the body.
    d.polygon([(63 * u, 27 * u), (77 * u, 41 * u), (63 * u, 41 * u)], fill=FOLD)
    d.line([(63 * u, 27 * u), (63 * u, 41 * u), (77 * u, 41 * u)], fill=EDGE, width=stroke)

    # Three ruled lines, the last one short, as on the desktop icon.
    for y, right in ((50, 69), (58, 69), (66, 57)):
        d.rounded_rectangle([(39 * u, y * u), (right * u, (y + 3) * u)], radius=1.5 * u, fill=RULE)

    return img.resize((size, size), Image.LANCZOS)


def main():
    for density, size in DENSITIES.items():
        path = f"{OUT}/mipmap-{density}/ic_launcher.png"
        draw(size).save(path)
        print(f"{path}  {size}x{size}")


if __name__ == "__main__":
    main()
