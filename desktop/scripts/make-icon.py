"""Draws the PowerDB logo (docs/brand/logo-mark.svg) as PNG files: a P made of a stem and a closed bowl, with an amber bar beneath
it (the file that belongs to the record), on a dark rounded tile.

    python3 desktop/scripts/make-icon.py app-icon.png [margin]     then: npx tauri icon app-icon.png

`margin` is the transparent border as a fraction of the image (macOS app icons want about 0.098). With no margin the tile fills the
image, which is what the docs/brand PNGs use. Drawn at 4x and scaled down so the edges are smooth.
"""
import sys
from PIL import Image, ImageDraw

S = 4096
BG, INK, ACCENT = (10, 10, 11, 255), (250, 250, 250, 255), (245, 158, 11, 255)


def tile(margin=0.0):
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    pad = int(S * margin)
    u = (S - 2 * pad) / 32.0
    P = lambda x, y: (pad + x * u, pad + y * u)
    box = lambda x0, y0, x1, y1: [P(x0, y0), P(x1, y1)]
    pill = lambda x, y, w, h, r, c: d.rounded_rectangle(box(x, y, x + w, y + h), radius=r * u, fill=c)

    pill(0, 0, 32, 32, 7.5, BG)
    d.rounded_rectangle(box(0.5, 0.5, 31.5, 31.5), radius=7 * u, outline=(255, 255, 255, 36), width=max(2, int(u)))
    pill(8, 6, 4.5, 20, 2.25, INK)                      # the stem
    d.rectangle(box(10.25, 6, 17.5, 20), fill=INK)      # the bowl: a block...
    d.ellipse(box(10.5, 6, 24.5, 20), fill=INK)         # ...with a half-circle on the right
    d.rectangle(box(13.5, 10.5, 17.5, 15.5), fill=BG)   # the counter (the hole in the P): a block,
    d.ellipse(box(15, 10.5, 20, 15.5), fill=BG)         # a half-circle on the right,
    pill(12.5, 10.5, 2, 5, 1, BG)                       # and a small radius on the left, like every other corner
    pill(14.5, 22.5, 10, 3.5, 1.75, ACCENT)             # the file
    return img


if __name__ == "__main__":
    out = sys.argv[1] if len(sys.argv) > 1 else "app-icon.png"
    margin = float(sys.argv[2]) if len(sys.argv) > 2 else 0.098
    tile(margin).resize((1024, 1024), Image.LANCZOS).save(out)
