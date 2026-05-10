#!/usr/bin/env python3
"""Render assets/hippo-logo.txt (ANSI truecolor) to a PNG."""
import re
import sys
from PIL import Image, ImageDraw, ImageFont

SRC = "assets/hippo-logo.txt"
OUT = "docs/hippo-logo.png"
FONT_PATH = "/System/Library/Fonts/Menlo.ttc"
FONT_SIZE = 36
PAD = 24

ANSI_RE = re.compile(r"\x1b\[([0-9;]*)m")


def parse_segments(line: str):
    """Yield (text, fg, bg) tuples for the line. fg/bg are (r,g,b) or None."""
    fg = None
    bg = None
    pos = 0
    for m in ANSI_RE.finditer(line):
        if m.start() > pos:
            yield line[pos : m.start()], fg, bg
        codes = [c for c in m.group(1).split(";") if c != ""]
        i = 0
        while i < len(codes):
            c = codes[i]
            if c == "0" or c == "":
                fg = None
                bg = None
                i += 1
            elif c == "38" and i + 4 < len(codes) and codes[i + 1] == "2":
                fg = (int(codes[i + 2]), int(codes[i + 3]), int(codes[i + 4]))
                i += 5
            elif c == "48" and i + 4 < len(codes) and codes[i + 1] == "2":
                bg = (int(codes[i + 2]), int(codes[i + 3]), int(codes[i + 4]))
                i += 5
            else:
                i += 1
        pos = m.end()
    if pos < len(line):
        yield line[pos:], fg, bg


def main():
    with open(SRC, "r", encoding="utf-8") as f:
        lines = f.read().rstrip("\n").split("\n")

    font = ImageFont.truetype(FONT_PATH, FONT_SIZE)

    # Use the font's advance width for a proper monospace cell.
    cell_w = int(round(font.getlength("█")))
    ascent, descent = font.getmetrics()
    cell_h = ascent + descent

    max_cols = max(
        sum(len(text) for text, _, _ in parse_segments(line)) for line in lines
    )
    img_w = max_cols * cell_w + PAD * 2
    img_h = len(lines) * cell_h + PAD * 2

    img = Image.new("RGBA", (img_w, img_h), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)

    for row, line in enumerate(lines):
        x = PAD
        y = PAD + row * cell_h
        for text, fg, bg in parse_segments(line):
            if not text:
                continue
            seg_w = len(text) * cell_w
            if bg is not None:
                draw.rectangle(
                    [x, y, x + seg_w, y + cell_h], fill=bg + (255,)
                )
            for ch in text:
                if ch == " ":
                    x += cell_w
                    continue
                # For full-block characters, fill the cell directly with the
                # fg color so adjacent block cells join seamlessly without
                # antialiased glyph seams.
                if ch == "█" and fg is not None:
                    draw.rectangle(
                        [x, y, x + cell_w, y + cell_h], fill=fg + (255,)
                    )
                else:
                    draw.text(
                        (x, y),
                        ch,
                        font=font,
                        fill=(fg or (200, 200, 200)) + (255,),
                    )
                x += cell_w

    img.save(OUT, "PNG")
    print(f"wrote {OUT} ({img.size[0]}x{img.size[1]})")


if __name__ == "__main__":
    main()
