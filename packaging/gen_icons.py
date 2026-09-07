#!/usr/bin/env python3
"""Draws Ymemo's icon everywhere a file of it is needed.

The picture is a **sticky note**: a square of paper on the app's gold, one corner lifted, a
few lines of writing on it. Flat — no outline, no gradient — because the icon has to survive
16px in a tray and a taskbar, and an outline at that size turns the paper into a smudge.
That is also why the paper is white against a saturated gold rather than cream against a pale
one: at 16px the only thing left is the silhouette and its contrast.

One picture, three framings, because the platforms crop differently:

- **Desktop** (`packaging/assets/`, used by the .desktop entry, the .rpm/.deb and the Windows
  installer) has no mask, so the note fills the badge — [`DESKTOP_NOTE`].
- **Android adaptive** (`mipmap-anydpi-v26/ic_launcher.xml`) is a 108dp canvas of which only
  the middle 72dp survives the launcher's mask, so the note sits small and centred. That one
  is a vector and is not drawn here; `res/drawable/ic_launcher_foreground.xml` is the source
  and this file has to keep agreeing with it, as does `ic_launcher_monochrome.xml`.
- **Android pre-adaptive** (`mipmap-<density>/ic_launcher.png`, API 24-25) is the adaptive
  framing as a bitmap, so the same phone does not show two different icons.

The desktop app draws the same picture in code rather than shipping a file — see
`crates/ymemo-desktop/src/icon.rs`, which mirrors the geometry below. Change one, change all.

    python3 packaging/gen_icons.py        # run from the repo root
"""

from PIL import Image, ImageDraw

DESKTOP_OUT = "packaging/assets"
ANDROID_OUT = "apps/mobile/android/app/src/main/res"

# mipmap density -> icon edge in px, the size Android asks each bucket for.
DENSITIES = {"mdpi": 48, "hdpi": 72, "xhdpi": 96, "xxhdpi": 144, "xxxhdpi": 192}
DESKTOP_SIZES = [16, 32, 48, 64, 128, 256, 512]
ICO_SIZES = [(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]

GOLD = (226, 194, 42, 255)    # the badge
PAPER = (255, 253, 245, 255)  # the note
INK = (92, 80, 16, 255)       # the writing
UNDER = (186, 158, 30, 255)   # the underside of the lifted corner

SS = 8       # supersampling; the fold's diagonal and the rounded corners need it
BADGE_R = 24  # the badge's corner radius, in viewport units

# The note, in the 108-unit viewport, framed for Android's launcher mask: a square well
# inside the middle 72dp the mask is guaranteed to keep.
PAGE = (32.0, 32.0, 76.0, 76.0)
PAGE_R = 8.0
FOLD = 13.0   # the lifted corner's legs, cut off the top right
RULE_H = 5.5
# x0, y, x1 per line. The first stops short of the fold and the last is the end of a
# sentence; three equal bars read as a list rather than as writing.
RULES = [(38.0, 50.0, 64.0), (38.0, 58.5, 70.0), (38.0, 67.0, 55.0)]

# How much bigger the note is drawn when nothing is going to mask the icon.
DESKTOP_NOTE = 1.30


def draw(size, note=1.0):
    """The icon at `size` px, drawn in a 108-unit viewport and scaled down.

    `note` scales the paper about the middle of the canvas; the fold, the corner radii and
    the writing scale with it, so the two framings are one drawing.
    """
    s = size * SS
    u = s / 108.0  # one viewport unit in pixels

    def p(x, y):
        """A point in the note's own coordinates, placed on the canvas."""
        return ((54 + (x - 54) * note) * u, (54 + (y - 54) * note) * u)

    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle([(0, 0), (s - 1, s - 1)], radius=BADGE_R * u, fill=GOLD)

    x0, y0, x1, y1 = PAGE
    # The paper goes on its own layer: the corner is *cut away* rather than drawn over, so
    # the gold shows through the notch whatever is behind it.
    paper = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    pd = ImageDraw.Draw(paper)
    pd.rounded_rectangle([p(x0, y0), p(x1, y1)], radius=PAGE_R * note * u, fill=PAPER)
    # Cut past the edges, so none of the rounded corner survives under the fold.
    pd.polygon([p(x1 - FOLD, y0 - 3), p(x1 + 3, y0 - 3), p(x1 + 3, y0 + FOLD)], fill=(0, 0, 0, 0))
    img.alpha_composite(paper)

    # The lifted corner, one step darker than the badge so it reads as the back of the sheet.
    d.polygon([p(x1 - FOLD, y0), p(x1, y0 + FOLD), p(x1 - FOLD, y0 + FOLD)], fill=UNDER)

    for rx0, ry, rx1 in RULES:
        d.rounded_rectangle([p(rx0, ry), p(rx1, ry + RULE_H)],
                            radius=RULE_H / 2 * note * u, fill=INK)

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
