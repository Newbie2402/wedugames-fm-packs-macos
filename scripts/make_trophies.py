"""Composes the Trophies cover from three separate trophy images."""
import pathlib
import sys

import numpy as np
from PIL import Image, ImageDraw, ImageFilter

IMAGES = pathlib.Path(sys.argv[1])
OUT = pathlib.Path(sys.argv[2])
W, H = 640, 400
TOP, BOTTOM, GLOW = (92, 58, 12), (20, 14, 12), (255, 178, 36)


def cut(path):
    image = Image.open(path)
    rgba = image.convert("RGBA")
    alpha = np.array(rgba)[..., 3]
    print(path.name, image.mode, image.size, "transparent share", round(float((alpha < 10).mean()), 3))
    if (alpha < 10).mean() > 0.05:
        return rgba
    rgb = np.array(rgba.convert("RGB")).astype(int)
    pale = (rgb.min(axis=2) > 170) & ((rgb.max(axis=2) - rgb.min(axis=2)) < 14)
    outside = np.zeros_like(pale)
    outside[0, :], outside[-1, :], outside[:, 0], outside[:, -1] = pale[0, :], pale[-1, :], pale[:, 0], pale[:, -1]
    while True:
        grown = outside.copy()
        grown[1:, :] |= outside[:-1, :]
        grown[:-1, :] |= outside[1:, :]
        grown[:, 1:] |= outside[:, :-1]
        grown[:, :-1] |= outside[:, 1:]
        grown &= pale
        if (grown == outside).all():
            break
        outside = grown
    mask = Image.fromarray(np.where(outside, 0, 255).astype("uint8"))
    rgba.putalpha(mask.filter(ImageFilter.MinFilter(3)).filter(ImageFilter.GaussianBlur(0.8)))
    return rgba


def background():
    y, x = np.mgrid[0:H, 0:W]
    mix = ((x / W + y / H) / 2)[..., None]
    base = np.array(TOP) * (1 - mix) + np.array(BOTTOM) * mix
    distance = np.hypot((x - W / 2) / (W * 0.46), (y - H / 2) / (H * 0.62))[..., None]
    base = base + np.array(GLOW) * np.clip(1 - distance, 0, 1) ** 2 * 0.34
    return Image.fromarray(np.clip(base, 0, 255).astype("uint8"), "RGB").convert("RGBA")


def paste(card, item, height, center_x, floor):
    item = item.crop(item.getchannel("A").getbbox())
    scale = height / item.height
    size = (round(item.width * scale), height)
    item = item.resize(size, Image.LANCZOS)
    at = (center_x - size[0] // 2, floor - height)
    shadow = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    shadow.paste((0, 0, 0, 170), (at[0], at[1] + 6), item.getchannel("A"))
    card = Image.alpha_composite(card, shadow.filter(ImageFilter.GaussianBlur(10)))
    card.alpha_composite(item, at)
    return card


league = cut(IMAGES / "25.png")
cup = cut(IMAGES / "26.png")
champions = cut(IMAGES / "27.webp")

card = background()
card = paste(card, league, 300, 155, 372)
card = paste(card, cup, 262, 488, 372)
card = paste(card, champions, 350, 320, 385)
card.convert("RGB").save(OUT / "trophies-pack.jpg", quality=92, optimize=True)
