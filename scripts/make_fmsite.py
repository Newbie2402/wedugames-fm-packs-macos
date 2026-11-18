"""Rebuilds the FMSite cover, cutting the sharp artwork with the clean image's outline."""
import pathlib
import sys

import numpy as np
from PIL import Image, ImageFilter

IMAGES = pathlib.Path(sys.argv[1])
OUT = pathlib.Path(sys.argv[2])
W, H = 640, 400
TOP, BOTTOM, GLOW = (12, 94, 74), (6, 26, 28), (46, 230, 166)

clean = Image.open(IMAGES / "28.png")
rgba = clean.convert("RGBA")
alpha = np.array(rgba)[..., 3]
print(clean.mode, clean.size, "transparent share", round(float((alpha < 10).mean()), 3))
if (alpha < 10).mean() < 0.05:
    sys.exit("no real transparency")

outline = rgba.getchannel("A")
outline = outline.crop(outline.getbbox())
sharp = Image.open(IMAGES / "21.jpg").convert("RGB").crop((1078, 48, 1422, 372))

item = sharp.convert("RGBA")
scaled = outline.resize(item.size, Image.LANCZOS).filter(ImageFilter.GaussianBlur(0.6))
item.putalpha(scaled)

y, x = np.mgrid[0:H, 0:W]
mix = ((x / W + y / H) / 2)[..., None]
base = np.array(TOP) * (1 - mix) + np.array(BOTTOM) * mix
distance = np.hypot((x - W / 2) / (W * 0.46), (y - H / 2) / (H * 0.62))[..., None]
base = base + np.array(GLOW) * np.clip(1 - distance, 0, 1) ** 2 * 0.34
card = Image.fromarray(np.clip(base, 0, 255).astype("uint8"), "RGB").convert("RGBA")

item = item.crop(item.getchannel("A").getbbox())
scale = 340 / item.height
item = item.resize((round(item.width * scale), 340), Image.LANCZOS)
at = ((W - item.width) // 2, 30)
shadow = Image.new("RGBA", (W, H), (0, 0, 0, 0))
shadow.paste((0, 0, 0, 150), (at[0], at[1] + 8), item.getchannel("A"))
card = Image.alpha_composite(card, shadow.filter(ImageFilter.GaussianBlur(12)))
card.alpha_composite(item, at)
card.convert("RGB").save(OUT / "fmsite-facepack.jpg", quality=92, optimize=True)
