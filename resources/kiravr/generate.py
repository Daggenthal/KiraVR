#!/usr/bin/env python3
# Generates the KiraVR icon set (concept B, "Kira Star") from code, so every
# size and format comes from one source. Needs rsvg-convert.
#   python3 resources/kiravr/generate.py
import os, struct, subprocess

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
BG = "#111827"

def sparkle(cx, cy, r, k=0.14):
    c = lambda sx, sy: f"{cx + sx*r*k:.1f},{cy + sy*r*k:.1f}"
    return (f"M{cx},{cy-r} Q{c(1,-1)} {cx+r},{cy} Q{c(1,1)} {cx},{cy+r} "
            f"Q{c(-1,1)} {cx-r},{cy} Q{c(-1,-1)} {cx},{cy-r} Z")

DEFS = ('<radialGradient id="glow" cx="0.5" cy="0.5" r="0.5"><stop offset="0" stop-color="#FFB547" stop-opacity="0.35"/>'
        '<stop offset="1" stop-color="#FFB547" stop-opacity="0"/></radialGradient>'
        '<linearGradient id="star" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#FFF8E1"/>'
        '<stop offset="1" stop-color="#FFAA33"/></linearGradient>')

FULL = ('<circle cx="128" cy="132" r="100" fill="url(#glow)"/>'
        f'<path d="{sparkle(126, 134, 92)}" fill="url(#star)"/>'
        f'<path d="{sparkle(196, 66, 20, 0.16)}" fill="#FFF3CF"/>'
        f'<path d="{sparkle(62, 200, 12, 0.16)}" fill="#FFD27A"/>')
# 16 to 32 px: Bigger star, no glow, one companion. Fine detail turns to mush there.
SMALL = (f'<path d="{sparkle(122, 136, 104, 0.16)}" fill="url(#star)"/>'
         f'<path d="{sparkle(204, 58, 30, 0.18)}" fill="#FFF3CF"/>')

def svg(body, background):
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256" width="256" height="256">'
            f'<defs>{DEFS}</defs>{background}{body}</svg>\n')

ROUNDED = f'<rect width="256" height="256" rx="56" fill="{BG}"/>'
SQUARE = f'<rect width="256" height="256" fill="{BG}"/>'
VARIANTS = {
    # App icon with rounded corners: Menus, AppImage, docs.
    "kiravr-icon": svg(FULL, ROUNDED),
    "kiravr-icon-small": svg(SMALL, ROUNDED),
    # Full square, for places that cut their own shape (avatars, stores).
    "kiravr-icon-square": svg(FULL, SQUARE),
    # Just the sparkles on transparent, for use on any background.
    # Stronger companion colors, so they also show on white.
    "kiravr-mark": svg(
        FULL.replace('<circle cx="128" cy="132" r="100" fill="url(#glow)"/>', "")
        .replace("#FFF3CF", "#FFC24D")
        .replace("#FFD27A", "#F5A524"),
        "",
    ),
}
SIZES = {
    "kiravr-icon": [48, 64, 96, 128, 256, 512, 1024],
    "kiravr-icon-small": [16, 24, 32],
    "kiravr-icon-square": [512, 1024],
    "kiravr-mark": [64, 128, 256, 512, 1024],
}

def render(name, size):
    path = f"{HERE}/png/{name}-{size}.png"
    subprocess.run(["rsvg-convert", "-w", str(size), "-h", str(size), f"{HERE}/{name}.svg", "-o", path], check=True)
    return path

def write_ico(path, pngs):
    # ICO with PNG entries (Windows Vista and newer, and the ico crate).
    header = struct.pack("<HHH", 0, 1, len(pngs))
    offset = 6 + 16 * len(pngs)
    entries, data = b"", b""
    for size, png in pngs:
        blob = open(png, "rb").read()
        dim = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(blob), offset + len(data))
        data += blob
    open(path, "wb").write(header + entries + data)

os.makedirs(f"{HERE}/png", exist_ok=True)
for name, content in VARIANTS.items():
    open(f"{HERE}/{name}.svg", "w").write(content)
rendered = {(n, s): render(n, s) for n, sizes in SIZES.items() for s in sizes}

def best(size):
    return rendered.get(("kiravr-icon-small", size)) or rendered[("kiravr-icon", size)]

# The dashboard uses the first entry as its window icon, so the largest goes first.
ico_sizes = [256, 128, 64, 48, 32, 24, 16]
write_ico(f"{HERE}/kiravr.ico", [(s, best(s)) for s in ico_sizes])

# Wordmark for the README, one per GitHub theme, rendered at 2x for sharp text.
def wordmark(text_color):
    mark = (FULL.replace('<circle cx="128" cy="132" r="100" fill="url(#glow)"/>', "")
            .replace("#FFF3CF", "#FFC24D").replace("#FFD27A", "#F5A524"))
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 900 260" width="900" height="260">'
            f'<defs>{DEFS}</defs><g transform="translate(10,2)">{mark}</g>'
            f'<text x="282" y="178" font-family="Noto Sans, DejaVu Sans, sans-serif" font-weight="700" '
            f'font-size="150" letter-spacing="-3" fill="{text_color}">KiraVR</text></svg>\n')

for theme, color in (("light", "#111827"), ("dark", "#F3F4F6")):
    name = f"kiravr-wordmark-{theme}"
    open(f"{HERE}/{name}.svg", "w").write(wordmark(color))
    subprocess.run(["rsvg-convert", "-w", "1800", "-h", "520", f"{HERE}/{name}.svg", "-o", f"{HERE}/png/{name}.png"], check=True)

# Social preview for GitHub (Settings, Social preview), 1280x640.
social = (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1280 640" width="1280" height="640"><defs>{DEFS}</defs>'
          f'<rect width="1280" height="640" fill="{BG}"/>'
          f'<g transform="translate(150,170) scale(1.25)">{FULL}</g>'
          f'<text x="520" y="330" font-family="Noto Sans, DejaVu Sans, sans-serif" font-weight="700" font-size="128" '
          f'letter-spacing="-3" fill="#F3F4F6">KiraVR</text>'
          f'<text x="526" y="400" font-family="Noto Sans, DejaVu Sans, sans-serif" font-size="30" fill="#AEB5C4">'
          f'Stream SteamVR to your standalone headset.</text>'
          f'<text x="526" y="448" font-family="Noto Sans, DejaVu Sans, sans-serif" font-size="30" fill="#AEB5C4">'
          f'Built for Linux and SteamOS.</text></svg>\n')
open(f"{HERE}/kiravr-social-preview.svg", "w").write(social)
subprocess.run(["rsvg-convert", "-w", "1280", "-h", "640", f"{HERE}/kiravr-social-preview.svg", "-o", f"{HERE}/png/kiravr-social-preview.png"], check=True)

print("Icon set written to", HERE)
