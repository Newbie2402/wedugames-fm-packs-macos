"""Composes a kits cover from one render, or three shirts side by side. A
checker painted into an image is cleared by a grey flood fill from the edges.

Usage: make_kits.py <3d|2d> <covers dir> <pack id[,pack id]> <image>... [--ball x,y,r]... [--crop x0,y0,x1,y1]
With three images the first stands left, the second front and centre, the third right.
--ball keeps a ball's disc whole, in source pixels: its grey shading touches the checker. Repeat it for
each ball. --crop takes one part of a single image, after the checker is cleared.

--alpha takes the cut-out from another image, such as a small transparent copy made by a background remover,
scaled to the source. --keep x0,x1 keeps only the pieces (player, ball) centred between those source columns.

The 3D covers (12.jpg is the sharp render, 13.png the same picture cut out at a quarter of the size):
  make_kits.py 3d covers sortitoutsi-3d-unity-kits 12.jpg --alpha 13.png --keep 0,1000
  make_kits.py 3d covers sortitoutsi-3d-kits 12.jpg --alpha 13.png --keep 1000,2000
"""
import argparse
import pathlib

import numpy as np
from PIL import Image, ImageFilter
from scipy import ndimage

parser = argparse.ArgumentParser()
parser.add_argument("kind", choices=["3d", "2d"])
parser.add_argument("out", type=pathlib.Path)
parser.add_argument("ids")
parser.add_argument("sources", nargs="+", type=pathlib.Path)
parser.add_argument("--ball", action="append", default=[], type=lambda s: tuple(int(v) for v in s.split(",")))
parser.add_argument("--crop", type=lambda s: tuple(int(v) for v in s.split(",")))
parser.add_argument("--alpha", type=pathlib.Path)
parser.add_argument("--keep", type=lambda s: tuple(int(v) for v in s.split(",")))
ARGS = parser.parse_args()
KIND, OUT, IDS, SOURCES = ARGS.kind, ARGS.out, ARGS.ids.split(","), ARGS.sources
W, H = 640, 400
TOP, BOTTOM, GLOW = (18, 52, 120), (8, 12, 28), (80, 150, 255)

SETTINGS = {
    # Player render: checker tones run about 76-187; hair is darker, the ball's white brighter.
    # White kits shade into the same greys, so only grey with the checker's light-dark swing around it
    # (squares about 16px) counts as background.
    "3d": {"grey": (70, 200, 16), "ball": ARGS.ball, "height": 360, "shrink": 3, "square": 16},
    # Shirts: checker tones 204 and white, both neutral; the soft shadow under a shirt is darker
    # grey and goes with it. Edges blend into the checker, so a wider rim is trimmed.
    "2d": {"grey": (90, 256, 14), "ball": ARGS.ball, "height": 330, "shrink": 7},
}


def cut(path, settings):
    image = Image.open(path)
    rgba = image.convert("RGBA")
    alpha = np.array(rgba)[..., 3]
    print(path.name, image.mode, image.size, "transparent share", round(float((alpha < 10).mean()), 3))
    if (alpha < 10).mean() > 0.05:
        return rgba
    rgb = np.array(rgba.convert("RGB")).astype(int)
    low, high, spread = settings["grey"]
    grey = (rgb.min(axis=2) > low) & (rgb.max(axis=2) < high) & ((rgb.max(axis=2) - rgb.min(axis=2)) < spread)
    if settings.get("square"):
        light = rgb.mean(axis=2)
        size = settings["square"] * 2 + 1
        mean = ndimage.uniform_filter(light, size)
        swing = np.sqrt(np.maximum(ndimage.uniform_filter(light * light, size) - mean * mean, 0))
        grey &= swing > 18
    outside = np.zeros_like(grey)
    outside[0, :], outside[-1, :], outside[:, 0], outside[:, -1] = grey[0, :], grey[-1, :], grey[:, 0], grey[:, -1]
    while True:
        grown = outside.copy()
        grown[1:, :] |= outside[:-1, :]
        grown[:-1, :] |= outside[1:, :]
        grown[:, 1:] |= outside[:, :-1]
        grown[:, :-1] |= outside[:, 1:]
        grown &= grey
        if (grown == outside).all():
            break
        outside = grown
    solid = ~outside
    y, x = np.mgrid[0:solid.shape[0], 0:solid.shape[1]]
    for bx, by, radius in settings["ball"]:
        solid |= np.hypot(x - bx, y - by) <= radius
    # Specks of checker left between blocks: keep only the large pieces.
    labels, count = ndimage.label(solid)
    sizes = ndimage.sum(solid, labels, range(1, count + 1))
    solid = np.isin(labels, 1 + np.flatnonzero(sizes > 2000))
    mask = Image.fromarray((solid * 255).astype("uint8"))
    rgba.putalpha(mask.filter(ImageFilter.MinFilter(settings["shrink"])).filter(ImageFilter.GaussianBlur(0.8)))
    return rgba


def background():
    y, x = np.mgrid[0:H, 0:W]
    mix = ((x / W + y / H) / 2)[..., None]
    base = np.array(TOP) * (1 - mix) + np.array(BOTTOM) * mix
    distance = np.hypot((x - W / 2) / (W * 0.46), (y - H / 2) / (H * 0.62))[..., None]
    base = base + np.array(GLOW) * np.clip(1 - distance, 0, 1) ** 2 * 0.34
    return Image.fromarray(np.clip(base, 0, 255).astype("uint8"), "RGB").convert("RGBA")


def paste(card, item, height, center_x=W // 2, width=W - 70):
    item = item.crop(item.getchannel("A").getbbox())
    scale = min(height / item.height, width / item.width)
    size = (round(item.width * scale), round(item.height * scale))
    item = item.resize(size, Image.LANCZOS)
    at = (center_x - size[0] // 2, (H - size[1]) // 2)
    shadow = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    shadow.paste((0, 0, 0, 160), (at[0], at[1] + 8), item.getchannel("A"))
    card = Image.alpha_composite(card, shadow.filter(ImageFilter.GaussianBlur(12)))
    card.alpha_composite(item, at)
    return card


def borrowed_alpha(path, alpha_path):
    rgba = Image.open(path).convert("RGBA")
    cutout = Image.open(alpha_path).convert("RGBA").getchannel("A").resize(rgba.size, Image.LANCZOS)
    rgba.putalpha(cutout)
    return rgba


def keep_between(item, x0, x1):
    alpha = np.array(item.getchannel("A"))
    labels, count = ndimage.label(alpha > 10)
    centres = ndimage.center_of_mass(alpha > 10, labels, range(1, count + 1))
    sizes = ndimage.sum(alpha > 10, labels, range(1, count + 1))
    keep = [i + 1 for i, ((_, cx), size) in enumerate(zip(centres, sizes)) if x0 <= cx < x1 and size > 500]
    item = item.copy()
    item.putalpha(Image.fromarray(np.where(np.isin(labels, keep), alpha, 0).astype("uint8")))
    return item


settings = SETTINGS[KIND]
if ARGS.alpha:
    items = [borrowed_alpha(source, ARGS.alpha) for source in SOURCES]
else:
    items = [cut(source, settings) for source in SOURCES]
if ARGS.keep:
    items = [keep_between(item, *ARGS.keep) for item in items]
if ARGS.crop:
    items = [item.crop(ARGS.crop) for item in items]
card = background()
if len(items) == 3:
    left, front, right = items
    card = paste(card, left, 290, 168, 280)
    card = paste(card, right, 290, W - 168, 280)
    card = paste(card, front, 340, W // 2, 320)
else:
    card = paste(card, items[0], settings["height"])
card = card.convert("RGB")
for pack_id in IDS:
    card.save(OUT / f"{pack_id}.jpg", quality=92, optimize=True)
    print(pack_id)
