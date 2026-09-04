#!/usr/bin/env python3
"""Draws the pictures the widget picker shows for each widget.

Android 12 and up render the real layout (`previewLayout`), so these are what everything
below it falls back to -- without them the picker offers three rows that all say Ymemo and
show the launcher icon. They are mock-ups on purpose: no text, because a PNG cannot be
translated and a preview in the wrong language is worse than none.

Run from apps/mobile:

    python3 tool/gen_widget_previews.py
"""

from PIL import Image, ImageDraw

OUT = "android/app/src/main/res/drawable-nodpi"
SS = 4  # supersampling

CARD = (255, 255, 255, 255)
INK = (27, 27, 23, 255)
MUTED = (110, 107, 94, 190)
FAINT = (27, 27, 23, 38)
ACCENT = (140, 123, 30, 255)
PAPER = (255, 252, 227, 255)
BAR = (244, 233, 140, 255)
BAR_INK = (92, 92, 37, 255)
SWATCHES = [(255, 225, 92, 255), (255, 159, 192, 255), (125, 184, 236, 255)]


def canvas(w, h):
    img = Image.new("RGBA", (w * SS, h * SS), (0, 0, 0, 0))
    return img, ImageDraw.Draw(img)


def bar(d, x, y, w, h, color, radius=None):
    """A rounded bar; text is drawn as these rather than as words."""
    d.rounded_rectangle(
        [(x * SS, y * SS), ((x + w) * SS, (y + h) * SS)],
        radius=(radius if radius is not None else h / 2) * SS,
        fill=color,
    )


def note_glyph(d, x, y, size, body=PAPER, edge=ACCENT):
    """The app's dog-eared note, small enough to stand in for the icon."""
    u = size / 24.0
    fold = 7 * u
    page = [
        (x + 3 * u, y + 2 * u),
        (x + 17 * u, y + 2 * u),
        (x + 17 * u + fold, y + 2 * u + fold),
        (x + 17 * u + fold, y + 22 * u),
        (x + 3 * u, y + 22 * u),
    ]
    d.polygon([(px * SS, py * SS) for px, py in page], fill=body, outline=edge,
              width=max(1, round(1.3 * u * SS)))
    for i, right in enumerate((15, 15, 10)):
        ly = y + (9 + i * 4) * u
        d.line([((x + 6 * u) * SS, ly * SS), ((x + right * u) * SS, ly * SS)],
               fill=edge, width=max(1, round(1.2 * u * SS)))


def pencil_glyph(d, x, y, size):
    """A pencil, nib pointing down-left, as on the widget's edit button."""
    u = size / 24.0
    d.polygon([((x + 4 * u) * SS, (y + 20 * u) * SS), ((x + 8 * u) * SS, (y + 20 * u) * SS),
               ((x + 20 * u) * SS, (y + 8 * u) * SS), ((x + 16 * u) * SS, (y + 4 * u) * SS),
               ((x + 4 * u) * SS, (y + 16 * u) * SS)], fill=ACCENT)


def camera_glyph(d, x, y, size):
    """A camera, as on the widget's photo button."""
    u = size / 24.0
    d.rounded_rectangle([((x + 2 * u) * SS, (y + 7 * u) * SS), ((x + 22 * u) * SS, (y + 20 * u) * SS)],
                        radius=3 * u * SS, fill=ACCENT)
    d.rounded_rectangle([((x + 8 * u) * SS, (y + 4 * u) * SS), ((x + 16 * u) * SS, (y + 9 * u) * SS)],
                        radius=1.5 * u * SS, fill=ACCENT)
    r = 4 * u
    cx, cy = x + 12 * u, y + 13.5 * u
    d.ellipse([((cx - r) * SS, (cy - r) * SS), ((cx + r) * SS, (cy + r) * SS)], fill=CARD)


def quick(w=320, h=80):
    img, d = canvas(w, h)
    d.rounded_rectangle([(4 * SS, 12 * SS), ((w - 4) * SS, (h - 12) * SS)],
                        radius=28 * SS, fill=CARD)
    note_glyph(d, 18, 26, 28)
    bar(d, 58, 36, 120, 8, MUTED)
    camera_glyph(d, w - 72, 30, 20)
    pencil_glyph(d, w - 40, 30, 20)
    return img


def note(w=160, h=160):
    img, d = canvas(w, h)
    d.rounded_rectangle([(4 * SS, 4 * SS), ((w - 4) * SS, (h - 4) * SS)],
                        radius=18 * SS, fill=PAPER)
    # Title bar: the card's top corners only, squared off where it meets the paper.
    d.rounded_rectangle([(4 * SS, 4 * SS), ((w - 4) * SS, 40 * SS)], radius=18 * SS, fill=BAR)
    d.rectangle([(4 * SS, 26 * SS), ((w - 4) * SS, 40 * SS)], fill=BAR)
    bar(d, 16, 17, 74, 9, BAR_INK)
    bar(d, w - 44, 18, 28, 7, (92, 92, 37, 110))
    for i, width in enumerate((110, 118, 96, 64)):
        bar(d, 16, 56 + i * 18, width, 8, FAINT)
    return img


def memo_list(w=320, h=160):
    img, d = canvas(w, h)
    d.rounded_rectangle([(4 * SS, 4 * SS), ((w - 4) * SS, (h - 4) * SS)],
                        radius=18 * SS, fill=CARD)
    note_glyph(d, 16, 14, 22)
    bar(d, 46, 22, 84, 9, INK)
    for cx in (w - 66, w - 34):
        d.ellipse([(cx * SS, 18 * SS), ((cx + 22) * SS, 40 * SS)], fill=(0, 0, 0, 16))
    d.rectangle([(4 * SS, 47 * SS), ((w - 4) * SS, 48 * SS)], fill=FAINT)
    for i, swatch in enumerate(SWATCHES):
        top = 58 + i * 34
        bar(d, 16, top, 4, 22, swatch, radius=2)
        bar(d, 30, top + 2, 150 - i * 24, 8, INK)
        bar(d, 30, top + 14, 190 - i * 40, 6, MUTED)
        if i < len(SWATCHES) - 1:
            d.rectangle([(4 * SS, (top + 28) * SS), ((w - 4) * SS, (top + 29) * SS)], fill=FAINT)
    return img


def main():
    for name, img in (
        ("widget_preview_quick", quick()),
        ("widget_preview_note", note()),
        ("widget_preview_list", memo_list()),
    ):
        path = f"{OUT}/{name}.png"
        img.resize((img.width // SS, img.height // SS), Image.LANCZOS).save(path)
        print(path)


if __name__ == "__main__":
    main()
