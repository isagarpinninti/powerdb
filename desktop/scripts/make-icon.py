"""Draws the PowerDB app icon (the dashboard's mark: framed dark tile, a bolt, one amber dot) at 1024x1024.

Usage: python3 make-icon.py app-icon.png      then: npx tauri icon app-icon.png
Drawn at 4x and scaled down so the edges are smooth.
"""
import sys
from PIL import Image, ImageDraw

S = 4096
img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
d = ImageDraw.Draw(img)

pad = int(S * 0.098)            # the margin macOS expects around an app icon
size = S - 2 * pad
u = size / 32.0                  # one unit of the 32x32 mark
ox = oy = pad

def P(x, y):
    return (ox + x * u, oy + y * u)

# tile
d.rounded_rectangle([P(1.5, 1.5), P(30.5, 30.5)], radius=8 * u, fill=(10, 10, 11, 255))
d.rounded_rectangle([P(1.5, 1.5), P(30.5, 30.5)], radius=8 * u, outline=(250, 250, 250, 90), width=int(1.5 * u))

# bolt: M18.5 6.5 10 17.5h5.6L13.8 25.5 22 14.2h-5.4z
bolt = [P(18.5, 6.5), P(10, 17.5), P(15.6, 17.5), P(13.8, 25.5), P(22, 14.2), P(16.6, 14.2)]
w = int(1.8 * u)
d.line(bolt + [bolt[0]], fill=(250, 250, 250, 255), width=w, joint="curve")
for x, y in bolt:                # round the joins
    d.ellipse([x - w / 2, y - w / 2, x + w / 2, y + w / 2], fill=(250, 250, 250, 255))

# amber accent
cx, cy = P(23.5, 8.5)
r = 2.2 * u
d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=(245, 158, 11, 255))

img.resize((1024, 1024), Image.LANCZOS).save(sys.argv[1] if len(sys.argv) > 1 else "app-icon.png")
