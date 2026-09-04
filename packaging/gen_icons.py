#!/usr/bin/env python3
"""Draws Ymemo's icon everywhere a file of it is needed.

One picture, three framings, because the platforms crop differently:

- **Desktop** (`packaging/assets/`, used by the .desktop entry, the .rpm/.deb and the Windows
  installer) has no mask, so the note fills the badge.
- **Android adaptive** (`mipmap-anydpi-v26/ic_launcher.xml`) is a 108dp canvas of which only
  the middle 72dp survives the launcher's mask, so the note sits small and centred. That one
  is a vector and is not drawn here; `res/drawable/ic_launcher_foreground.xml` is the source
  and this file has to keep agreeing with it.
- **Android pre-adaptive** (`mipmap-<density>/ic_launcher.png`, API 24-25) is the adaptive
  framing as a bitmap, so the same phone does not show two different icons.

The desktop app draws the same picture in code rather than shipping a file — see
`crates/ymemo-desktop/src/icon.rs`, which mirrors the geometry below. Change one, change all.

    python3 packaging/gen_icons.py        # run from the repo root
"""

from math import cos, radians, sin

from PIL import Image, ImageDraw

DESKTOP_OUT = "packaging/assets"
ANDROID_OUT = "apps/mobile/android/app/src/main/res"

# mipmap density -> icon edge in px, the size Android asks each bucket for.
DENSITIES = {"mdpi": 48, "hdpi": 72, "xhdpi": 96, "xxhdpi": 144, "xxxhdpi": 192}
DESKTOP_SIZES = [16, 32, 48, 64, 128, 256, 512]
ICO_SIZES = [(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]

GOLD = (230, 210, 74, 255)       # background: the app's Material seed color
PAPER = (255, 252, 227, 255)     # note body: the yellow palette's paper
EDGE = (140, 123, 30, 255)       # outline
FOLD = (216, 198, 92, 255)       # the dog-eared corner
RULE = (184, 166, 58, 255)       # the ruled lines

SS = 8    # supersampling; the diagonal fold and the rounded corners need it
R = 6     # the page's corner radius, in viewport units
BADGE = 22  # the background's corner radius

# How much bigger the note is drawn when nothing is going to mask the icon.
DESKTOP_NOTE = 1.44


def _corner(cx, cy, start_deg, u, r=R):
    """A quarter circle, as points: PIL polygons have no arcs."""
    return [
        ((cx + r * cos(radians(start_deg + t))) * u, (cy + r * sin(radians(start_deg + t))) * u)
        for t in range(0, 91, 6)
    ]


def draw(size, note=1.0):
    """The icon at `size` px, drawn in a 108-unit viewport and scaled down.

    `note` scales the page about the middle of the canvas; everything else -- the corner
    radii, the fold, the stroke -- scales with it, so the two framings are one drawing.
    """
    s = size * SS
    u = s / 108.0  # one viewport unit in pixels

    def p(x, y):
        """A point in the note's own coordinates, placed on the canvas."""
        return ((54 + (x - 54) * note) * u, (54 + (y - 54) * note) * u)

    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle([(0, 0), (s - 1, s - 1)], radius=BADGE * u, fill=GOLD)
    # The outline is 2.5 units, but never thinner than most of a finished pixel: at 16px
    # that works out under a half and the note washes out into a white blob on gold.
    stroke = max(round(0.9 * SS), round(2.5 * note * u))

    # The page: three rounded corners, with the top-right one cut away by the fold.
    corner = lambda cx, cy, deg: [  # noqa: E731 - a local alias, not a definition
        (54 * u + (px - 54 * u) * note, 54 * u + (py - 54 * u) * note)
        for px, py in _corner(cx, cy, deg, u)
    ]
    body = (
        [p(37, 27), p(63, 27), p(77, 41), p(77, 75)]
        + corner(71, 75, 0)      # bottom right
        + corner(37, 75, 90)     # bottom left
        + [p(31, 33)]
        + corner(37, 33, 180)    # top left
    )
    d.polygon(body, fill=PAPER, outline=EDGE, width=stroke)

    # The fold, and the diagonal that separates it from the body.
    d.polygon([p(63, 27), p(77, 41), p(63, 41)], fill=FOLD)
    d.line([p(63, 27), p(63, 41), p(77, 41)], fill=EDGE, width=stroke)

    # Three ruled lines, the last one short, as on the desktop icon.
    for y, right in ((50, 69), (58, 69), (66, 57)):
        d.rounded_rectangle([p(39, y), p(right, y + 3)], radius=1.5 * note * u, fill=RULE)

    return img.resize((size, size), Image.LANCZOS)


def main():
    for size in DESKTOP_SIZES:
        name = "ymemo.png" if size == 512 else f"ymemo-{size}.png"
        path = f"{DESKTOP_OUT}/{name}"
        draw(size, DESKTOP_NOTE).save(path)
        print(f"{path}  {size}x{size}")

    # The Windows icon carries every size in one file; Pillow makes them from the largest.
    ico = f"{DESKTOP_OUT}/ymemo.ico"
    draw(256, DESKTOP_NOTE).save(ico, format="ICO", sizes=ICO_SIZES)
    print(f"{ico}  {len(ICO_SIZES)} sizes")

    for density, size in DENSITIES.items():
        path = f"{ANDROID_OUT}/mipmap-{density}/ic_launcher.png"
        draw(size).save(path)
        print(f"{path}  {size}x{size}")


if __name__ == "__main__":
    main()
