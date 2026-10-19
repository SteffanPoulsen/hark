#!/usr/bin/env python3
"""Draws kuulo's logo from its construction and writes every file made from it.

The wordmark is round-capped strokes of one weight on Inter's proportions: a k built from
a stem and a right-angled chevron, the uu as one square-ish wave whose outer stems stop at
half the ascender, an l, and a square-ish o. The icon is the same wave on a dark tile, worn
from copper at its peaks to verdigris in its hollows.

The strokes are outlined and united by VectorCraft, so every file holds plain filled paths.
Needs vectorcraft-cli and rsvg-convert on PATH. Run from anywhere: python3 docs/brand/logo.py
"""
import math
import re
import subprocess
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
BRAND = REPO / "docs/brand"
ASSETS = REPO / "crates/kuulo/assets"

COPPER = "#d98a56"
VERDIGRIS = "#5fbfa8"
TILE = "#1e1e1e"
WEAR = ((0.30, COPPER), (0.92, VERDIGRIS))
WEAR_ANGLE = 6

# Inter's x-height and ascender in its 2048 units, so the wordmark sits with the app's type.
XH, ASC = 1118, 1490
OVERSHOOT = 14  # the bowls dip below the baseline, as round letters do
STROKE = 240
GAP = 220  # between every pair of letters, stroke edge to stroke edge
SIDE = ASC / 2  # where the outer stems of the uu stop
SQUARE, ARCH_SQUARE = 3.0, 2.4  # superellipse exponents: 2 is a circle, higher is squarer
BOWL, ARCH = 600, 480  # uu stem spacing; the arch's gap equals the stroke
O_WIDTH = 830
K_OVERLAP = 40  # how far the chevron's tip runs into the stem

ICON_STROKE = 280  # in the icon every space equals the stroke: bowls, arch and side margins
ICON_SIZES = (16, 32, 48, 64, 128, 256, 512)

# macOS 26 sets an icon that strays outside Apple's squircle on a grey plate, so the Mac icon
# is that squircle on Apple's 1024 grid (824 wide), drawn a few pixels inside it for margin.
MAC_TILE, MAC_SQUARE = 816, 5.0
# icns entry types by pixel size; the 2x entries repeat a size under its retina name.
ICNS = (("icp4", 16), ("icp5", 32), ("ic11", 32), ("ic12", 64), ("ic07", 128), ("ic13", 256),
        ("ic08", 256), ("ic14", 512), ("ic09", 512), ("ic10", 1024))


def sp(v, n):
    return math.copysign(abs(v) ** (2 / n), v)


def wave(sx, w, n_bowl, n_arch, k=96):
    """The joined uu through stems sx, y down with the baseline at 0."""
    yb = w / 2 - OVERSHOOT
    a1, a2, a3 = (sx[1] - sx[0]) / 2, (sx[2] - sx[1]) / 2, (sx[3] - sx[2]) / 2
    up = [(sx[0], SIDE - w / 2)]
    up += [(sx[0] + a1 + a1 * sp(math.cos(t), n_bowl), yb + a1 + a1 * sp(math.sin(t), n_bowl))
           for t in (math.pi + math.pi * i / k for i in range(k + 1))]
    arch_c = XH - w / 2 - a2
    up += [(sx[1] + a2 + a2 * sp(math.cos(t), n_arch), arch_c + a2 * sp(math.sin(t), n_arch))
           for t in (math.pi - math.pi * i / k for i in range(k + 1))]
    up += [(sx[2] + a3 + a3 * sp(math.cos(t), n_bowl), yb + a3 + a3 * sp(math.sin(t), n_bowl))
           for t in (math.pi + math.pi * i / k for i in range(k + 1))]
    up.append((sx[3], SIDE - w / 2))
    return [(x, -y) for x, y in up]


def superellipse(cx, cy, a, b, n, k=240):
    return [(cx + a * sp(math.cos(2 * math.pi * i / k), n), cy + b * sp(math.sin(2 * math.pi * i / k), n)) for i in range(k)]


def wordmark():
    """Centrelines of the letters and of the wave, y down from the ascender, and the width."""
    h = STROKE / 2
    kx = h
    top, bottom = -(XH - h), -h
    reach = (bottom - top) / 2  # a right-angled chevron reaches as far as it is half tall
    tip = kx + STROKE - K_OVERLAP
    letters = [([(kx, -ASC + h), (kx, -h)], False),
               ([(tip + reach, top), (tip, -XH / 2), (tip + reach, bottom)], False)]
    x = tip + reach + h + GAP + h
    sx = [x, x + BOWL, x + BOWL + ARCH, x + 2 * BOWL + ARCH]
    lx = sx[3] + h + GAP + h
    letters.append(([(lx, -ASC + h), (lx, -h)], False))
    a, b = O_WIDTH / 2, (XH + 2 * 12 - STROKE) / 2
    ox = lx + h + GAP + h + a
    letters.append((superellipse(ox, -XH / 2, a, b, SQUARE), True))
    width = ox + a + h
    down = lambda pts: [(px, py + ASC) for px, py in pts]
    return [(down(p), c) for p, c in letters], [(down(wave(sx, STROKE, SQUARE, ARCH_SQUARE)), False)], width


def icon_wave():
    """The icon's wave, with every space equal to the stroke, shifted so its box starts at 0."""
    w = ICON_STROKE
    sx = [0, 2 * w, 4 * w, 6 * w]
    dx, dy = w / 2, XH + 20
    pts = [(x + dx, y + dy) for x, y in wave(sx, w, SQUARE, ARCH_SQUARE)]
    return pts, sx[3] + w, XH + 40


def poly(points, closed):
    return "M" + " L".join(f"{x:.2f},{y:.2f}" for x, y in points) + (" Z" if closed else "")


def outline(paths, w, view_w, view_h):
    """Round-capped strokes of width w, outlined and united by VectorCraft, as one path."""
    with tempfile.TemporaryDirectory() as tmp:
        src, dst = Path(tmp, "strokes.svg"), Path(tmp, "outlined.svg")
        body = "".join(f'<path d="{poly(p, c)}" fill="none" stroke="#000" stroke-width="{w}" '
                       f'stroke-linecap="round" stroke-linejoin="round"/>' for p, c in paths)
        src.write_text(f'<svg xmlns="http://www.w3.org/2000/svg" width="{view_w:.0f}" height="{view_h:.0f}" '
                       f'viewBox="0 0 {view_w:.0f} {view_h:.0f}">{body}</svg>')
        subprocess.run(["vectorcraft-cli", "run", "--in", str(src),
                        "--cmd", "select.all", "--cmd", "object.path.simplify", "--params", '{"tolerance": 0.5}',
                        "--cmd", "object.path.outlineStroke", "--cmd", "select.all", "--cmd", "object.pathfinder.unite",
                        "--export", str(dst)], check=True, capture_output=True)
        ds = re.findall(r'<path d="([^"]+)"', dst.read_text())
    if len(ds) != 1:
        raise SystemExit(f"expected one united path from VectorCraft, got {len(ds)}")
    return ds[0]


def gradient(gid, x0, y0, x1, y1):
    """The wear: copper at the top turning to verdigris at the foot, leaning by WEAR_ANGLE."""
    a = math.radians(WEAR_ANGLE)
    cx, cy = (x0 + x1) / 2, (y0 + y1) / 2
    half = (y1 - y0) / 2 / math.cos(a)
    dx, dy = math.sin(a) * half, math.cos(a) * half
    stops = "".join(f'<stop offset="{o}" stop-color="{c}"/>' for o, c in WEAR)
    return (f'<linearGradient id="{gid}" gradientUnits="userSpaceOnUse" x1="{cx - dx:.1f}" y1="{cy - dy:.1f}" '
            f'x2="{cx + dx:.1f}" y2="{cy + dy:.1f}">{stops}</linearGradient>')


def svg(w, h, body, title="kuulo"):
    return (f'<?xml version="1.0" encoding="UTF-8"?>\n<svg xmlns="http://www.w3.org/2000/svg" width="{w:.0f}" height="{h:.0f}" '
            f'viewBox="0 0 {w:.0f} {h:.0f}">\n  <title>{title}</title>\n{body}</svg>\n')


def render(path, png, width):
    subprocess.run(["rsvg-convert", "-w", str(width), "-o", str(png), str(path)], check=True)


def icns(svg_path, out):
    """An .icns of PNG entries rendered from svg_path."""
    entries = b""
    with tempfile.TemporaryDirectory() as tmp:
        for kind, size in ICNS:
            png = Path(tmp, f"{size}.png")
            render(svg_path, png, size)
            data = png.read_bytes()
            entries += kind.encode() + (8 + len(data)).to_bytes(4, "big") + data
    out.write_bytes(b"icns" + (8 + len(entries)).to_bytes(4, "big") + entries)


def main():
    letters, uu, word_w = wordmark()
    word_h = ASC + OVERSHOOT
    letters_d = outline(letters, STROKE, word_w, word_h)
    wave_d = outline(uu, STROKE, word_w, word_h)
    icon_pts, icon_w, icon_h = icon_wave()
    icon_d = outline([(icon_pts, False)], ICON_STROKE, icon_w, icon_h)

    # The icon: the wave centred on the tile, its side margins equal to its stroke.
    fit = 1 / (1 + 2 * ICON_STROKE / icon_w)
    s = 256 * fit / icon_w
    tx, ty = (256 - icon_w * s) / 2, (256 - icon_h * s) / 2
    icon_wear = gradient("wear", 0, 20, icon_w, XH + 20)

    def icon_wave_on(size, x, y):
        k = size / 256
        return f'  <path transform="translate({x + tx * k:.3f},{y + ty * k:.3f}) scale({s * k:.6f})" d="{icon_d}" fill="url(#wear)"/>\n'

    def icon_body(size, x=0.0, y=0.0):
        return (f'  <defs>{icon_wear}</defs>\n'
                f'  <rect x="{x:.1f}" y="{y:.1f}" width="{size:.1f}" height="{size:.1f}" rx="{56 * size / 256:.1f}" fill="{TILE}"/>\n'
                + icon_wave_on(size, x, y))

    icon = ASSETS / "icon/kuulo.svg"
    icon.write_text(svg(256, 256, icon_body(256)))
    for size in ICON_SIZES:
        render(icon, ASSETS / f"icon/kuulo-{size}.png", size)

    m, r = (1024 - MAC_TILE) / 2, MAC_TILE / 2
    squircle = poly(superellipse(512, 512, r, r, MAC_SQUARE), True)
    with tempfile.TemporaryDirectory() as tmp:
        mac = Path(tmp, "kuulo-mac.svg")
        mac.write_text(svg(1024, 1024, f'  <defs>{icon_wear}</defs>\n  <path d="{squircle}" fill="{TILE}"/>\n' + icon_wave_on(MAC_TILE, m, m)))
        icns(mac, ASSETS / "icon/kuulo.icns")

    # The app tints these from its palette: the letters in the accent, the wave in the second accent.
    (ASSETS / "wordmark").mkdir(exist_ok=True)
    (ASSETS / "wordmark/letters.svg").write_text(svg(word_w, word_h, f'  <path d="{letters_d}" fill="#fff"/>\n', "kuulo letters"))
    (ASSETS / "wordmark/wave.svg").write_text(svg(word_w, word_h, f'  <path d="{wave_d}" fill="#fff"/>\n', "kuulo wave"))

    # The lockup: the icon at 1.6 times the ascender, the wordmark's ascender-to-baseline block centred on it.
    tile = 1.6 * ASC
    gap = 0.44 * ASC
    wx, wy = tile + gap, (tile - ASC) / 2
    body = icon_body(tile)
    body += (f'  <g transform="translate({wx:.1f},{wy:.1f})">\n'
             f'    <path d="{letters_d}" fill="{COPPER}"/>\n    <path d="{wave_d}" fill="{VERDIGRIS}"/>\n  </g>\n')
    lockup = BRAND / "kuulo-logo.svg"
    lockup.write_text(svg(wx + word_w, tile, body))
    render(lockup, BRAND / "kuulo-logo.png", 1000)
    print(f"lockup baseline at {(wy + ASC) / tile:.4f} of its height")


if __name__ == "__main__":
    main()
