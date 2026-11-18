"""Builds src-tauri/icons/icon.icns from the W in assets/wedugames-logo.webp.

The Windows build only ever needed icon.ico; macOS requires icon.icns, so this
script produces it from the same logo and dark tile as scripts/make_icon.py.
Icon generation is part of the macOS build (scripts/build-macos.sh runs this
first), so the .icns always matches the latest logo.

Usage: python3 scripts/make_icns.py   (run from the project root)
"""
import io
import struct

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
    lifted = np.clip(BACKDROP + (rgb - BACKDROP) * 255 / np.maximum(alpha, 1)[..., None], 0, 255)
    return Image.fromarray(np.dstack([lifted, alpha]).astype("uint8"), "RGBA")


def tile(item, size):
    """Rounded dark square so the W stands out on light and dark docks/menus."""
    item = item.copy()
    item.thumbnail((size - 2 * max(1, size // 9), size - 2 * max(1, size // 9)), Image.LANCZOS)
    canvas = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    canvas.alpha_composite(item, ((size - item.width) // 2, (size - item.height) // 2))
    scale = 4
    mask = Image.new("L", (size * scale, size * scale), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, size * scale - 1, size * scale - 1], radius=size * scale // 5, fill=255)
    base = Image.new("RGBA", (size, size), TILE + (0,))
    base.putalpha(mask.resize((size, size), Image.LANCZOS))
    base.alpha_composite(canvas)
    return base


# An .icns is a container of PNG chunks; 128..1024 covers every size macOS shows.
CHUNKS = [("ic07", 128), ("ic08", 256), ("ic09", 512), ("ic10", 1024)]

w = mark()
body = b""
for kind, size in CHUNKS:
    buf = io.BytesIO()
    tile(w, size).save(buf, format="PNG")
    png = buf.getvalue()
    body += struct.pack(">4sI", kind.encode("ascii"), len(png) + 8) + png
icns = struct.pack(">4sI", b"icns", 8 + len(body)) + body
with open("src-tauri/icons/icon.icns", "wb") as f:
    f.write(icns)
print("src-tauri/icons/icon.icns")

# Tauri's generate_context! embeds a PNG as the default window icon on macOS/Linux
# (the .icns is only the bundle/dock icon), so emit a 1024px icon.png too.
tile(w, 1024).save("src-tauri/icons/icon.png", format="PNG")
print("src-tauri/icons/icon.png")