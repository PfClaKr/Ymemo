#!/usr/bin/env python3
"""Draws Ymemo's icon everywhere a file of it is needed.

The picture is **two sticky notes**: a gold one in front with its bottom-right corner
peeled up and two lines of writing, and a blue one behind it, turned a little. Flat and
tonal, in the manner of Google's current product icons — no outline, no gradient, no badge
behind it — because it has to hold at 16px in a tray, where a stroke is under a pixel and
only the silhouette and the two colours are left.

One drawing, in a 1024-unit art space, framed two ways:

- **Unmasked** (`packaging/assets/`: the .desktop entry, the .rpm/.deb, the Windows `.ico`,
  and — embedded — the desktop app's tray and window icon, see `crates/ymemo-desktop/src/
  icon.rs`): the art fills the canvas, on transparency.
- **Android** (`mipmap-<density>/ic_launcher.png`, the pre-adaptive bitmap): the art inside
  the middle 72 of a 108 viewport, on white, the way the adaptive icon frames it. The
  adaptive icon itself is the vector `res/drawable/ic_launcher_foreground.xml` (plus
  `ic_launcher_monochrome.xml`) and uses the **same coordinates** through a group transform —
  change the numbers below, change them there.

    python3 packaging/gen_icons.py        # run from the repo root
"""

from PIL import Image, ImageDraw

DESKTOP_OUT = "packaging/assets"
ANDROID_OUT = "apps/mobile/android/app/src/main/res"

# mipmap density -> icon edge in px, the size Android asks each bucket for.
DENSITIES = {"mdpi": 48, "hdpi": 72, "xhdpi": 96, "xxhdpi": 144, "xxxhdpi": 192}
DESKTOP_SIZES = [16, 22, 32, 48, 64, 128, 256, 512]
ICO_SIZES = [(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]

BLUE = (66, 133, 244, 255)      # the note behind
GOLD = (253, 214, 99, 255)      # the note in front
PEEL = (229, 168, 0, 255)       # its peeled corner, the back of the sheet
INK = (120, 84, 0, 255)         # the writing
WHITE = (255, 255, 255, 255)    # Android's launcher ground

SS = 8  # supersampling; the rotated edges and rounded corners need it

# The art, in 1024 units. The back note is turned BACK_TURN degrees clockwise about
# BACK_PIVOT; the front note loses FOLD off its bottom-right corner.
BACK = (250, 170, 850, 770)
BACK_R = 120
BACK_TURN = 10
BACK_PIVOT = (550, 470)
FRONT = (170, 290, 770, 890)
FRONT_R = 120
FOLD = 190
LINE_W = 64
LINES = [(290, 480, 640), (290, 600, 580)]  # x0, y, x1 — the second is a sentence ending


def art(px):
    """The art alone, `px` square, on transparency (1024 art units across)."""
    s = px * SS
    k = s / 1024.0
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))

    back = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    x0, y0, x1, y1 = BACK
    ImageDraw.Draw(back).rounded_rectangle([x0 * k, y0 * k, x1 * k, y1 * k], radius=BACK_R * k, fill=BLUE)
    # PIL turns counter-clockwise for a positive angle.
    back = back.rotate(-BACK_TURN, resample=Image.BICUBIC,
                       center=(BACK_PIVOT[0] * k, BACK_PIVOT[1] * k))
    img.alpha_composite(back)

    x0, y0, x1, y1 = FRONT
    mask = Image.new("L", (s, s), 0)
    md = ImageDraw.Draw(mask)
    md.rounded_rectangle([x0 * k, y0 * k, x1 * k, y1 * k], radius=FRONT_R * k, fill=255)
    md.polygon([((x1 - FOLD) * k, y1 * k), (x1 * k + 4, y1 * k + 4), (x1 * k, (y1 - FOLD) * k)], fill=0)
    img.paste(Image.new("RGBA", (s, s), GOLD), (0, 0), mask)
    d = ImageDraw.Draw(img)
    d.polygon([((x1 - FOLD) * k, y1 * k), ((x1 - FOLD) * k, (y1 - FOLD) * k), (x1 * k, (y1 - FOLD) * k)], fill=PEEL)
    for lx0, ly, lx1 in LINES:
        d.rounded_rectangle([(lx0 - LINE_W / 2) * k, (ly - LINE_W / 2) * k,
                             (lx1 + LINE_W / 2) * k, (ly + LINE_W / 2) * k],
                            radius=LINE_W / 2 * k, fill=INK)
    return img.resize((px, px), Image.LANCZOS)


def android(px):
    """The pre-adaptive launcher bitmap: the art in the middle 72 of 108, on white."""
    img = Image.new("RGBA", (px, px), (0, 0, 0, 0))
    ImageDraw.Draw(img).ellipse([0, 0, px - 1, px - 1], fill=WHITE)
    inner = round(px * 72 / 108)
    off = (px - inner) // 2
    img.alpha_composite(art(inner), (off, off))
    return img


def main():
    for size in DESKTOP_SIZES:
        name = "ymemo.png" if size == 512 else f"ymemo-{size}.png"
        path = f"{DESKTOP_OUT}/{name}"
        art(size).save(path)
        print(f"{path}  {size}x{size}")

    # The Windows icon carries every size in one file; Pillow makes them from the largest.
    ico = f"{DESKTOP_OUT}/ymemo.ico"
    art(256).save(ico, format="ICO", sizes=ICO_SIZES)
    print(f"{ico}  {len(ICO_SIZES)} sizes")

    for density, size in DENSITIES.items():
        path = f"{ANDROID_OUT}/mipmap-{density}/ic_launcher.png"
        android(size).save(path)
        print(f"{path}  {size}x{size}")


if __name__ == "__main__":
    main()
