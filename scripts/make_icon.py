"""Builds the app icon and the UI's brand mark from the W in the Wedugames logo.

Usage: make_icon.py   (run from the project root)
"""
import numpy as np
from PIL import Image, ImageDraw

LOGO = "assets/wedugames-logo.webp"
W_BOX = (525, 440, 1475, 1075)  # the W alone, inside the ring, measured on the 2000px logo
BACKDROP = (24, 29, 41)  # the logo's own dark fill
TILE = (16, 20, 28)


def mark():
    """The W on transparency: alpha from how far each pixel is from the backdrop."""
    w = Image.open(LOGO).convert("RGB").crop(W_BOX)
    rgb = np.array(w).astype(float)
    distance = np.abs(rgb - BACKDROP).max(axis=2)
    alpha = np.clip(distance * 2.4, 0, 255)
    # Brighten the faint glow edge so it does not read as grey once transparent.
    lifted = np.clip(BACKDROP + (rgb - BACKDROP) * 255 / np.maximum(alpha, 1)[..., None], 0, 255)
    out = np.dstack([lifted, alpha]).astype("uint8")
    return Image.fromarray(out, "RGBA")


def square(item, size, pad):
    item = item.copy()
    item.thumbnail((size - 2 * pad, size - 2 * pad), Image.LANCZOS)
    canvas = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    canvas.alpha_composite(item, ((size - item.width) // 2, (size - item.height) // 2))
    return canvas


def tile(item, size):
    """Rounded dark square so the W stands out on light and dark taskbars."""
    scale = 4
    mask = Image.new("L", (size * scale, size * scale), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, size * scale - 1, size * scale - 1], radius=size * scale // 5, fill=255)
    base = Image.new("RGBA", (size, size), TILE + (0,))
    base.putalpha(mask.resize((size, size), Image.LANCZOS))
    base.alpha_composite(square(item, size, max(1, size // 9)))
    return base


w = mark()
square(w, 128, 4).save("ui/brand.png")
big = tile(w, 256)
big.save("src-tauri/icons/icon.ico", sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)])
big.save("assets/icon-preview.png")
print("ui/brand.png, src-tauri/icons/icon.ico")
