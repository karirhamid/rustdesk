"""Renders the text of the launcher splash (src/res/splash.bin): the RTDesk logo in the client's
fonts and weights (RtdeskLogo in flutter/lib/common.dart), as 8-bit coverage masks at each display scale.

The launcher composes colours, card, shadow and animation itself; only the glyphs come from here,
since it has no font engine. Run after changing the logo: python splash.py (needs Pillow).

Format, little endian: b"RTSP", u8 scale count, then per scale: u16 scale x100, u16 group width,
u16 group height, u8 mask count (3: "RT", "Desk", tagline), and per mask: i16 x, i16 y, u16 w,
u16 h, w*h coverage bytes (row major, top down).
"""
import os
import struct

from PIL import Image, ImageDraw, ImageFont

HERE = os.path.dirname(os.path.abspath(__file__))
FONTS = os.path.join(HERE, '..', '..', 'flutter', 'assets', 'fonts')
OUT = os.path.join(HERE, 'src', 'res', 'splash.bin')

SIZE = 44  # logo font size at 100 %, logical pixels
SCALES = (100, 125, 150, 175, 200, 250, 300)
SUPER = 4  # rendered this much larger, then box-filtered down: smooth edges, sub-pixel advances


def font(name, px):
    return ImageFont.truetype(os.path.join(FONTS, name), px)


def line(parts, px):
    """Coverage masks of one line drawn part after part on a shared baseline, at `px`, with the
    client's -0.035 em letter spacing (kerned pairs as each font defines them). Each part is
    (text, font file); returns [mask, x, y] per part in one coordinate system (super-sampled),
    and the baseline's y."""
    spacing = -0.035 * px * SUPER
    out, pen = [], 0.0
    for text, face in parts:
        f = font(face, px * SUPER)
        ascent, descent = f.getmetrics()
        img = Image.new('L', (int(f.getlength(text) + len(text) * SUPER * 2 + 4 * SUPER), ascent + descent), 0)
        draw = ImageDraw.Draw(img)
        x = 0.0
        for i, ch in enumerate(text):
            draw.text((x, 0), ch, font=f, fill=255)
            x = f.getlength(text[:i + 1]) + (i + 1) * spacing
        out.append([img, pen, 0])
        pen += x
    return out, ascent


def tracked(text, face, px, tracking):
    """One line with `tracking` (em) between letters, as the tagline is drawn."""
    f = font(face, px * SUPER)
    ascent, descent = f.getmetrics()
    gap = tracking * px * SUPER
    img = Image.new('L', (int(f.getlength(text) + len(text) * gap + 4 * SUPER), ascent + descent), 0)
    draw = ImageDraw.Draw(img)
    for i, ch in enumerate(text):
        draw.text((f.getlength(text[:i]) + i * gap, 0), ch, font=f, fill=255)
    return img


def place(img, x, y):
    """Box-filters a super-sampled mask placed at (x, y) super-sampled pixels down to display
    pixels, keeping the sub-pixel part of the position. Returns [crop, x, y] in display pixels."""
    ox, oy = int(x // SUPER), int(y // SUPER)
    canvas = Image.new('L', (img.width + SUPER, img.height + SUPER), 0)
    canvas.paste(img, (int(x - ox * SUPER), int(y - oy * SUPER)))
    small = shrink(canvas)
    box = small.getbbox() or (0, 0, 1, 1)
    return [small.crop(box), ox + box[0], oy + box[1]]


def shrink(img):
    w = (img.width + SUPER - 1) // SUPER
    h = (img.height + SUPER - 1) // SUPER
    padded = Image.new('L', (w * SUPER, h * SUPER), 0)
    padded.paste(img, (0, 0))
    return padded.resize((w, h), Image.BOX)


def scale_set(scale):
    size = SIZE * scale / 100
    parts, ascent = line((('RT', 'Inter-ExtraBold.ttf'), ('Desk', 'Inter-Regular.ttf')), size)
    masks = [place(img, x, y) for img, x, y in parts]

    # The splash centres the tagline under the logo, smaller and less tracked than the client's
    # left-aligned one so that the two lines come out about as wide. Its capitals start 0.37 em
    # (of the logo) under the logo's baseline, the gap the client leaves.
    tag_size = size * 0.30
    tag = tracked('RIRAKTECH HELPDESK', 'Inter-Medium.ttf', tag_size, 0.20)
    tag_y = ascent + 0.37 * size * SUPER - tag.getbbox()[1]
    masks.append(place(tag, 0, tag_y))

    # Centre both lines, then start the group's box at the top of the capitals.
    logo_l = min(m[1] for m in masks[:2])
    logo_r = max(m[1] + m[0].width for m in masks[:2])
    tag_m = masks[2]
    shift = (logo_l + logo_r - tag_m[0].width) // 2 - tag_m[1]
    tag_m[1] += shift
    min_x = min(m[1] for m in masks)
    min_y = min(m[2] for m in masks)
    for m in masks:
        m[1] -= min_x
        m[2] -= min_y
    width = max(m[1] + m[0].width for m in masks)
    height = max(m[2] + m[0].height for m in masks)
    return width, height, masks


def main():
    out = bytearray(b'RTSP')
    out += struct.pack('<B', len(SCALES))
    for scale in SCALES:
        width, height, masks = scale_set(scale)
        out += struct.pack('<HHHB', scale, width, height, len(masks))
        for img, x, y in masks:
            out += struct.pack('<hhHH', x, y, img.width, img.height)
            out += img.tobytes()
        print(f'{scale}%: group {width}x{height}, ' + ', '.join(f'{m[0].width}x{m[0].height}@{m[1]},{m[2]}' for m in masks))
    with open(OUT, 'wb') as f:
        f.write(out)
    print(f'{OUT}: {len(out)} bytes')


if __name__ == '__main__':
    main()
