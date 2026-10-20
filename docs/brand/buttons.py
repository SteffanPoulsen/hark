#!/usr/bin/env python3
"""Draws the README's download buttons in the shape people know from store badges: black
with a grey edge, the platform's own logo, and "Download for" over the platform's name in
Inter, outlined into paths so they render alike everywhere. Every badge has the same padding
around its content, measured from the ink.

The logos in platforms/ are Wikimedia Commons' files, unchanged: "Windows logo - 2021.svg",
"Apple logo white.svg" and "Tux.svg", the last by Larry Ewing (lewing@isc.tamu.edu) and The
GIMP, drawn as SVG by Simon Budig and Garrett LeSage. Each is a trademark of its owner.

Needs fontTools. Run from anywhere: python3 docs/brand/buttons.py
"""
import math
import xml.etree.ElementTree as ET

from fontTools.pens.boundsPen import BoundsPen
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont

from logo import BRAND, REPO, svg

SVG_NS, XLINK_NS = "http://www.w3.org/2000/svg", "http://www.w3.org/1999/xlink"
FILL, EDGE, INK = "#2d2d2d", "#a6a6a6", "#fff"  # a store badge's black would swallow Tux's black body
# The logos' heights differ so they weigh alike: a square fills its box, an apple and a penguin do not.
PLATFORMS = (("windows", "Windows", "windows.svg", 20), ("macos", "macOS", "apple.svg", 24), ("linux", "Linux", "tux.svg", 27))

HEIGHT, RADIUS = 44, 8
OVER, NAME = 12, 18  # em sizes in px: "Download for", the platform
LEADING = 5  # from the small line's baseline to the cap top of the name
GAP, PAD = 10, 16
MARGIN = 4  # transparent, so neighbouring buttons in a README line stand apart and wrap with space


def kerning(font):
    """The pair kerning Latin text gets from the font's GPOS kern feature, in font units."""
    gpos = font["GPOS"].table
    script = next(s.Script for s in gpos.ScriptList.ScriptRecord if s.ScriptTag == "latn")
    features = gpos.FeatureList.FeatureRecord
    lookups = [gpos.LookupList.Lookup[i] for f in script.DefaultLangSys.FeatureIndex
               if features[f].FeatureTag == "kern" for i in features[f].Feature.LookupListIndex]
    tables = [[getattr(s, "ExtSubTable", s) for s in lookup.SubTable] for lookup in lookups]

    def kern(a, b):
        total = 0
        for subtables in tables:
            for s in subtables:
                if a not in s.Coverage.glyphs:
                    continue
                if s.Format == 1:
                    pairs = s.PairSet[s.Coverage.glyphs.index(a)].PairValueRecord
                    value = next((r.Value1 for r in pairs if r.SecondGlyph == b), None)
                    if value is None:
                        continue
                else:
                    c1, c2 = s.ClassDef1.classDefs.get(a, 0), s.ClassDef2.classDefs.get(b, 0)
                    value = s.Class1Record[c1].Class2Record[c2].Value1
                total += getattr(value, "XAdvance", 0) or 0
                break
        return total

    return kern


def number(v):
    return f"{v:.2f}".rstrip("0").rstrip(".")


class Text:
    """A line set in one font at one em size, outlined, with its ink's horizontal extent and its cap height in px."""

    def __init__(self, font, text, em):
        self.glyphs, self.scale = font.getGlyphSet(), em / font["head"].unitsPerEm
        self.cap = font["OS/2"].sCapHeight * self.scale
        cmap, kern = font.getBestCmap(), kerning(font)
        names = [cmap[ord(c)] for c in text]
        self.placed, x, xs = [], 0, []
        for i, name in enumerate(names):
            self.placed.append((name, x))
            bounds = BoundsPen(self.glyphs)
            self.glyphs[name].draw(bounds)
            if bounds.bounds:
                xs += [x + bounds.bounds[0], x + bounds.bounds[2]]
            x += font["hmtx"][name][0] + (kern(name, names[i + 1]) if i + 1 < len(names) else 0)
        self.ink_left, self.width = min(xs) * self.scale, (max(xs) - min(xs)) * self.scale

    def path(self, left, baseline):
        """The outline with its ink starting at left."""
        pen = SVGPathPen(self.glyphs, ntos=number)
        s = self.scale
        for name, x in self.placed:
            self.glyphs[name].draw(TransformPen(pen, (s, 0, 0, -s, left - self.ink_left + x * s, baseline)))
        return pen.getCommands()


class Logo:
    """A logo file at a height, nested whole as an svg element; its viewBox is its ink."""

    def __init__(self, file, height):
        self.root = ET.parse(BRAND / "platforms" / file).getroot()
        for title in self.root.findall(f"{{{SVG_NS}}}title"):
            self.root.remove(title)
        box = self.root.get("viewBox") or f"0 0 {self.root.get('width')} {self.root.get('height')}"
        _, _, w, h = (float(v) for v in box.replace(",", " ").split())
        self.root.attrib = {"viewBox": box}
        self.height, self.width = height, height * w / h

    def element(self, left, middle):
        self.root.attrib.update(x=number(left), y=number(middle - self.height / 2),
                                width=number(self.width), height=number(self.height))
        return ET.tostring(self.root, encoding="unicode")


def main():
    ET.register_namespace("", SVG_NS)
    ET.register_namespace("xlink", XLINK_NS)
    regular = TTFont(REPO / "crates/kuulo/assets/fonts/Inter-Regular.ttf")
    semibold = TTFont(REPO / "crates/kuulo/assets/fonts/Inter-SemiBold.ttf")
    for slug, name, file, logo_height in PLATFORMS:
        logo, over, big = Logo(file, logo_height), Text(regular, "Download for", OVER), Text(semibold, name, NAME)
        content = logo.width + GAP + max(over.width, big.width)
        width = math.ceil(content + 2 * PAD)
        left = MARGIN + (width - content) / 2
        middle = MARGIN + HEIGHT / 2
        top = middle - (over.cap + LEADING + big.cap) / 2
        text = left + logo.width + GAP
        d = over.path(text, top + over.cap) + big.path(text, top + over.cap + LEADING + big.cap)
        body = (f'  <rect x="{MARGIN + 0.5}" y="{MARGIN + 0.5}" width="{width - 1}" height="{HEIGHT - 1}" rx="{RADIUS - 0.5}" '
                f'fill="{FILL}" stroke="{EDGE}"/>\n'
                f'  {logo.element(left, middle)}\n'
                f'  <path d="{d}" fill="{INK}"/>\n')
        out = svg(width + 2 * MARGIN, HEIGHT + 2 * MARGIN, body, f"Download for {name}")
        (BRAND / f"download-{slug}.svg").write_text(out.replace("<svg ", f'<svg xmlns:xlink="{XLINK_NS}" ', 1))
        print(f"{slug}: {width}×{HEIGHT} px")


if __name__ == "__main__":
    main()
