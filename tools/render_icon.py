#!/usr/bin/env python3
"""Draw the gauge icon directly, instead of generating it.

    tools/uv run --group icon tools/render_icon.py --preview
    tools/uv run --group icon tools/render_icon.py --value 0.72 --size 1024 -o icon.png
    tools/uv run --group icon tools/render_icon.py --icns     # assets/AppIcon.icns

Pillow lives in the `icon` dependency group, so the app's own .venv does not
carry it; `--group icon` brings it in for the run. PNGs land in icons/
(git-ignored) unless -o says otherwise.

The diffusion model would not take needle position — "seven o'clock" and
"twelve o'clock" produced the same angle — so the needle is drawn here, where
it is a number. Everything else (sweep, tick count, ramp, weights) is a
parameter too, and it renders crisp at any size, which an app icon needs.

Geometry follows the grid5 B3/C3/G2 direction: an open speedometer arc with a
deep blue -> teal -> green -> amber ramp on a dark rounded square.
"""

import argparse
import math
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent

# PIL angles: 0 deg is 3 o'clock, increasing clockwise, y axis down.
# 150 deg is 8 o'clock; sweeping 240 deg clockwise ends at 4 o'clock, passing
# through 12 o'clock at the midpoint. That is the standard speedometer sweep.
START = 150.0
SWEEP = 240.0

BACKGROUND = (23, 26, 33)
NEEDLE = (255, 255, 255)
HUB = (44, 48, 58)
TICK = (255, 255, 255)

# deep blue -> teal -> green -> amber
RAMP = [(29, 78, 216), (8, 145, 178), (22, 163, 74), (245, 158, 11)]


def ramp_at(t):
    """Sample the ramp at 0..1 with linear interpolation between stops."""
    t = min(max(t, 0.0), 1.0) * (len(RAMP) - 1)
    i = min(int(t), len(RAMP) - 2)
    f = t - i
    a, b = RAMP[i], RAMP[i + 1]
    return tuple(round(a[c] + (b[c] - a[c]) * f) for c in range(3))


def draw_icon(size, value=0.72, ticks=9, arc_width=0.105, show_ticks=True):
    """Render at `size` px. `value` is the needle position, 0..1."""
    from PIL import ImageFilter

    scale = 4  # supersample, then downscale for clean edges
    s = size * scale
    image = Image.new("RGB", (s, s), BACKGROUND)

    cx = cy = s / 2
    radius = s * 0.365
    width = s * arc_width
    box = [cx - radius, cy - radius, cx + radius, cy + radius]
    steps = 320

    # Bloom: the arc drawn fat and blurred underneath, which is what gives the
    # generated dials their depth. Drawn on its own layer so the blur does not
    # touch the crisp arc on top.
    glow = Image.new("RGB", (s, s), BACKGROUND)
    gdraw = ImageDraw.Draw(glow)
    for i in range(steps):
        a0 = START + SWEEP * (i / steps)
        a1 = START + SWEEP * ((i + 1) / steps) + 0.8
        gdraw.arc(box, a0, a1, fill=ramp_at(i / steps), width=round(width * 2.1))
    glow = glow.filter(ImageFilter.GaussianBlur(s * 0.055))
    image = Image.blend(image, glow, 0.55)
    draw = ImageDraw.Draw(image)

    # The arc is drawn as many short segments so the ramp runs along it.
    for i in range(steps):
        a0 = START + SWEEP * (i / steps)
        a1 = START + SWEEP * ((i + 1) / steps) + 0.6  # overlap hides seams
        draw.arc(box, a0, a1, fill=ramp_at(i / steps), width=round(width))

    if show_ticks:
        # PIL grows an arc's width inward from the bounding ellipse, so the
        # band spans radius-width .. radius. Ticks are centred on that band and
        # kept to half its thickness, so they read as notches cut into the
        # colour rather than spokes reaching into the middle.
        centre = radius - width * 0.5
        half = width * 0.26
        for i in range(ticks):
            angle = math.radians(START + SWEEP * (i / (ticks - 1)))
            dx, dy = math.cos(angle), math.sin(angle)
            draw.line([cx + dx * (centre + half), cy + dy * (centre + half),
                       cx + dx * (centre - half), cy + dy * (centre - half)],
                      fill=TICK, width=max(1, round(s * 0.018)))

    # Needle: a triangle tapering from the hub to the tip, so it reads at
    # small sizes where a hairline would disappear.
    angle = math.radians(START + SWEEP * min(max(value, 0.0), 1.0))
    tip = (cx + math.cos(angle) * radius * 0.80,
           cy + math.sin(angle) * radius * 0.80)
    half = math.radians(90)
    base = s * 0.028
    draw.polygon([tip,
                  (cx + math.cos(angle + half) * base,
                   cy + math.sin(angle + half) * base),
                  (cx + math.cos(angle - half) * base,
                   cy + math.sin(angle - half) * base)], fill=NEEDLE)

    hub = s * 0.052
    draw.ellipse([cx - hub, cy - hub, cx + hub, cy + hub], fill=HUB)
    draw.ellipse([cx - hub * 0.34, cy - hub * 0.34,
                  cx + hub * 0.34, cy + hub * 0.34], fill=NEEDLE)

    return image.resize((size, size), Image.LANCZOS)


def rounded(image, radius_ratio=0.225):
    """Apply the macOS-style rounded-square mask with a transparent surround."""
    size = image.size[0]
    mask = Image.new("L", (size * 4, size * 4), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        [0, 0, size * 4 - 1, size * 4 - 1],
        radius=int(size * 4 * radius_ratio), fill=255)
    mask = mask.resize((size, size), Image.LANCZOS)
    out = image.convert("RGBA")
    out.putalpha(mask)
    return out


def build_icns(out_path, value=0.28, ticks=9):
    """Write a macOS .icns, rendering each size natively rather than resampling.

    macOS icons sit inset in their canvas. Measured against Terminal, Notes and
    Chrome, the artwork occupies 214/256 = 83.6% — not the 80% often quoted.
    Anything smaller reads as a slightly undersized icon in the Dock.
    """
    import shutil
    import subprocess
    import tempfile

    entries = [(16, "16x16"), (32, "16x16@2x"), (32, "32x32"), (64, "32x32@2x"),
               (128, "128x128"), (256, "128x128@2x"), (256, "256x256"),
               (512, "256x256@2x"), (512, "512x512"), (1024, "512x512@2x")]

    with tempfile.TemporaryDirectory() as tmp:
        iconset = Path(tmp) / "AppIcon.iconset"
        iconset.mkdir()
        for px, name in entries:
            inner = max(1, round(px * 0.836))
            art = rounded(draw_icon(inner, value, ticks))
            canvas = Image.new("RGBA", (px, px), (0, 0, 0, 0))
            offset = (px - inner) // 2
            canvas.paste(art, (offset, offset), art)
            canvas.save(iconset / f"icon_{name}.png")

        if not shutil.which("iconutil"):
            raise SystemExit("iconutil not found — macOS only")
        out_path.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(out_path)],
                       check=True)
    return out_path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--icns", action="store_true",
                        help="write assets/AppIcon.icns")
    parser.add_argument("--value", type=float, default=0.72)
    parser.add_argument("--size", type=int, default=1024)
    parser.add_argument("--ticks", type=int, default=9)
    parser.add_argument("--no-ticks", action="store_true")
    parser.add_argument("-o", "--out", default=None)
    parser.add_argument("--preview", action="store_true",
                        help="strip of needle positions plus a size test")
    args = parser.parse_args()

    if args.icns:
        out = build_icns(ROOT / "assets" / "AppIcon.icns",
                         value=0.28 if args.value == 0.72 else args.value,
                         ticks=args.ticks)
        print(f"wrote {out.relative_to(ROOT)} "
              f"({out.stat().st_size // 1024} KB)")
        return

    if args.preview:
        cell = 260
        values = [0.0, 0.17, 0.33, 0.5, 0.67, 0.83, 1.0]
        strip = Image.new("RGB", (cell * len(values), cell * 2), (13, 13, 13))
        for i, v in enumerate(values):
            strip.paste(rounded(draw_icon(cell - 20, v)).convert("RGB"),
                        (i * cell + 10, 10))
        # Same design, Dock sizes, needle held at 0.72.
        x = 10
        for px in (32, 48, 64, 128, 256):
            strip.paste(rounded(draw_icon(px, 0.72)).convert("RGB"),
                        (x, cell + 30))
            x += px + 22
        out = ROOT / "icons" / "render-preview.png"
        out.parent.mkdir(parents=True, exist_ok=True)
        strip.save(out)
        print(f"wrote {out.relative_to(ROOT)}")
        return

    image = rounded(draw_icon(args.size, args.value, args.ticks,
                              show_ticks=not args.no_ticks))
    out = Path(args.out) if args.out else ROOT / "icons" / "render.png"
    out.parent.mkdir(parents=True, exist_ok=True)
    image.save(out)
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
