"""Rebuilds covers from the sharper sheet. Its checkerboard is painted in, so
each object is masked by shape, or cleared by low-saturation flood fill."""
import pathlib
import sys

import numpy as np
from PIL import Image, ImageDraw, ImageFilter, ImageFont

sys.path.insert(0, str(pathlib.Path(__file__).parent))
SOURCE = pathlib.Path(sys.argv[1])
OUT = pathlib.Path(sys.argv[2])
W, H = 640, 400

THEMES = {
    "faces": ((12, 94, 74), (6, 26, 28), (46, 230, 166)),
    "logos": ((120, 24, 36), (22, 10, 18), (255, 96, 96)),
    "other": ((92, 58, 12), (20, 14, 12), (255, 178, 36)),
}

PACKS = [
    ("cut-out-player-faces", (138, 38, 378, 376), "faces", 340, "flood"),
    ("df11-classic", (612, 46, 886, 374), "faces", 340, "rect"),
    ("fmg-logos", (1588, 48, 1912, 372), "logos", 310, "flood"),
    ("fmg-standard-logos", (97, 440, 408, 751), "logos", 310, "circle"),
    ("fmv-logos", (610, 440, 890, 752), "logos", 310, "flood"),
    ("fmg-steel-logos", (1097, 440, 1408, 751), "logos", 310, "circle"),
    ("fmg-vapour-logos", (1597, 442, 1902, 750), "logos", 310, "circle"),
    ("fmg-plastic-logos", (94, 816, 410, 1118), "logos", 310, "flood"),
    ("fmg-grunge-logos", (630, 798, 868, 1122), "logos", 310, "flood"),
    ("fmg-metallic-logos", (1097, 813, 1408, 1122), "logos", 310, "circle"),
    ("tcm-logos", (1596, 813, 1904, 1122), "logos", 310, "circle"),
    ("real-name-fix", (605, 1175, 965, 1490), "other", 330, "rnf"),
]

def background(theme):
    top, bottom, glow = THEMES[theme]
    y, x = np.mgrid[0:H, 0:W]
    mix = ((x / W + y / H) / 2)[..., None]
    base = np.array(top) * (1 - mix) + np.array(bottom) * mix
    distance = np.hypot((x - W / 2) / (W * 0.46), (y - H / 2) / (H * 0.62))[..., None]
    base = base + np.array(glow) * np.clip(1 - distance, 0, 1) ** 2 * 0.34
    return Image.fromarray(np.clip(base, 0, 255).astype("uint8"), "RGB")


def shape_mask(size, kind):
    scale = 3
    big = (size[0] * scale, size[1] * scale)
    mask = Image.new("L", big, 0)
    draw = ImageDraw.Draw(mask)
    inset = 2 * scale
    box = [inset, inset, big[0] - inset, big[1] - inset]
    if kind == "circle":
        draw.ellipse(box, fill=255)
    elif kind == "hex":
        # The frame's corners, measured on the source image.
        corners = [(66, 4), (284, 4), (343, 156), (320, 322), (70, 322), (3, 162)]
        draw.polygon([(x * scale, y * scale) for x, y in corners], fill=255)
    else:
        draw.rounded_rectangle(box, radius=16 * scale, fill=255)
    return mask.resize(size, Image.LANCZOS)


def flood_mask(crop, floor=140, shrink=7):
    """The painted checker is pale and grey; clear what of it touches the edge."""
    rgb = np.array(crop.convert("RGB")).astype(int)
    spread = rgb.max(axis=2) - rgb.min(axis=2)
    pale = (rgb.min(axis=2) > floor) & (spread < 12)
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
    alpha = Image.fromarray(np.where(outside, 0, 255).astype("uint8"))
    return alpha.filter(ImageFilter.MinFilter(shrink)).filter(ImageFilter.GaussianBlur(1.0))


def place(card, item, height):
    item = item.crop(item.getchannel("A").getbbox())
    scale = min(height / item.height, (W - 70) / item.width)
    size = (round(item.width * scale), round(item.height * scale))
    item = item.resize(size, Image.LANCZOS)
    at = ((W - size[0]) // 2, (H - size[1]) // 2)
    shadow = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    shadow.paste((0, 0, 0, 150), (at[0], at[1] + 8), item.getchannel("A"))
    shadow = shadow.filter(ImageFilter.GaussianBlur(12))
    result = Image.alpha_composite(card.convert("RGBA"), shadow)
    result.alpha_composite(item, at)
    return result.convert("RGB")


sheet_source = Image.open(SOURCE).convert("RGB")
print("source", sheet_source.size)
for pack_id, box, theme, height, kind in PACKS:
    crop = sheet_source.crop(box)
    item = crop.convert("RGBA")
    if kind == "rnf":
        # White card; the frame's bottom bar runs behind the feet.
        rgb = np.array(crop).astype(int)
        red = (rgb[..., 0] > 190) & (rgb[..., 1] < 150) & (np.arange(crop.height)[:, None] > crop.height - 34)
        crop = Image.fromarray(np.where(red[..., None], 255, rgb).astype("uint8"))
        item = crop.convert("RGBA")
    if kind == "rnf":
        rgb = np.array(crop).astype(int)
        solid = ((rgb.max(axis=2) - rgb.min(axis=2)) > 28) | (rgb.max(axis=2) < 165)
        solid[-42:] = rgb[-42:].max(axis=2) < 150
        alpha = Image.fromarray((solid * 255).astype("uint8"))
        item.putalpha(alpha.filter(ImageFilter.GaussianBlur(0.7)))
    elif kind == "flood":
        item.putalpha(flood_mask(crop))
    else:
        item.putalpha(shape_mask(crop.size, kind))
    if kind == "rnf":
        item = Image.open(SOURCE.parent / "22.png").convert("RGBA")
    if pack_id == "trophies-pack":
        cups = Image.open(SOURCE.parent / "24.webp")
        print("trophies source", cups.mode, cups.size)
        item = cups.convert("RGBA")
        if np.array(item)[..., 3].min() == 255:
            alpha = flood_mask(cups.convert("RGB"), floor=150, shrink=3)
            # Checker showing through the handles: pure grey outside the cup bodies.
            rgb = np.array(cups.convert("RGB")).astype(int)
            grey = ((rgb.max(axis=2) - rgb.min(axis=2)) <= 12) & (rgb.min(axis=2) > 140)
            bodies = Image.new("L", cups.size, 0)
            shapes = ImageDraw.Draw(bodies)
            shapes.polygon([(345, 150), (680, 150), (680, 330), (580, 590), (640, 650), (640, 740), (375, 740), (375, 660), (450, 590), (345, 330)], fill=255)
            shapes.polygon([(150, 345), (318, 335), (318, 500), (262, 625), (308, 660), (300, 690), (172, 690), (172, 650), (212, 625), (150, 480)], fill=255)
            shapes.polygon([(190, 235), (300, 235), (318, 335), (150, 345)], fill=255)
            shapes.polygon([(728, 262), (912, 262), (905, 470), (870, 505), (930, 625), (925, 660), (700, 660), (700, 620), (765, 505), (722, 470)], fill=255)
            grey &= np.array(bodies) == 0
            hole = Image.fromarray((grey * 255).astype("uint8")).filter(ImageFilter.MaxFilter(3))
            alpha = Image.fromarray(np.minimum(np.array(alpha), 255 - np.array(hole)))
            item.putalpha(alpha.filter(ImageFilter.GaussianBlur(0.7)))
    card = place(background(theme), item, height)
    card.save(OUT / f"{pack_id}.jpg", quality=92, optimize=True)
    print(pack_id)

# A preview laid out like the app's cards.
order = [
    ("cut-out-player-faces", "Cut-Out Player Faces"), ("df11-classic", "DF11 Classic"), ("fmsite-facepack", "FMSite Facepack"),
    ("fmg-logos", "FMG Logos"), ("fmg-standard-logos", "FMG Standard Logos"), ("fmv-logos", "FMV Logos"),
    ("fmg-steel-logos", "FMG Steel Logos"), ("fmg-vapour-logos", "FMG Vapour Logos"), ("fmg-plastic-logos", "FMG Plastic Logos"),
    ("fmg-grunge-logos", "FMG Grunge Logos"), ("fmg-metallic-logos", "FMG Metallic Logos"), ("tcm-logos", "TCM Logos"),
    ("real-name-fix", "Real Name Fix"), ("trophies-pack", "Trophies Pack"),
]
card_w, card_h, gap = 300, 188, 16
rows = (len(order) + 2) // 3
page = Image.new("RGB", (3 * card_w + 4 * gap, rows * (card_h + 46 + gap) + gap), (10, 14, 19))
draw = ImageDraw.Draw(page)
try:
    font = ImageFont.truetype("bahnschrift.ttf", 17)
except OSError:
    font = ImageFont.load_default()
for index, (pack_id, name) in enumerate(order):
    x = gap + index % 3 * (card_w + gap)
    y = gap + index // 3 * (card_h + 46 + gap)
    draw.rounded_rectangle([x, y, x + card_w, y + card_h + 46], 12, fill=(16, 22, 29), outline=(36, 48, 60))
    art = Image.open(OUT / f"{pack_id}.jpg").resize((card_w - 2, card_h), Image.LANCZOS)
    page.paste(art, (x + 1, y + 1))
    draw.text((x + 14, y + card_h + 13), name, fill=(238, 243, 247), font=font)
page.save(OUT / "_preview.png")
print("preview", page.size)
