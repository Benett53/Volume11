"""Build the Volume11 icon from the logo artwork.

    python tools/make-icon.py              # icon from assets/logo-source.png
    python tools/make-icon.py artwork.png  # re-import artwork first

Importing artwork removes its dark matte: the logo is drawn on a near-black
square, which as an icon would sit in the taskbar as a dark tile. Every pixel
is treated as the logo composited over that background colour, and the
compositing is undone, so the glow survives as real transparency instead of a
dark halo. The result is cropped to the artwork and stored as the source.

Frames are written as classic 32bpp DIBs. PIL stores icon frames
PNG-compressed, which Windows only decodes reliably at 256x256; at tray sizes
they render as a monochrome blob.

Requires Pillow.
"""
import struct
import sys
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "assets" / "logo-source.png"
ICON = ROOT / "assets" / "icon.ico"
LOGO = ROOT / "docs" / "logo.png"

SIZES = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]

# Brightness above the matte below which a pixel is treated as background.
NOISE_FLOOR = 0.07

# At tray sizes the soft glow only blurs the outline. Up to this edge length the
# faint end of the alpha range is cut so the shapes stay crisp.
CRISP_UP_TO = 32


def background_colour(image: Image.Image) -> tuple:
    """Median of the four corner regions, which in the artwork are plain matte."""
    rgb = image.convert("RGB")
    w, h = rgb.size
    patch = max(4, min(w, h) // 40)
    samples = []
    for x0, y0 in ((0, 0), (w - patch, 0), (0, h - patch), (w - patch, h - patch)):
        for y in range(y0, y0 + patch):
            for x in range(x0, x0 + patch):
                samples.append(rgb.getpixel((x, y)))
    return tuple(sorted(c[i] for c in samples)[len(samples) // 2] for i in range(3))


def remove_matte(image: Image.Image) -> Image.Image:
    """Undo `pixel = alpha * colour + (1 - alpha) * background`."""
    bg = background_colour(image)
    rgb = image.convert("RGB")
    out = Image.new("RGBA", rgb.size)
    src = rgb.load()
    dst = out.load()

    for y in range(rgb.size[1]):
        for x in range(rgb.size[0]):
            p = src[x, y]
            # The artwork only adds light to the background, so only channels
            # brighter than it count. Darker pixels are texture and compression
            # noise in the matte; counting them left grey grain around the logo
            # on light backgrounds.
            alpha = 0.0
            for c, b in zip(p, bg):
                if c > b:
                    alpha = max(alpha, (c - b) / (255 - b))
            if alpha <= NOISE_FLOOR:
                dst[x, y] = (0, 0, 0, 0)
                continue
            # Colour is recovered with the true alpha; the stored alpha is
            # ramped from the noise floor so faint matte noise fades to nothing.
            colour = tuple(
                max(0, min(255, round((c - (1 - alpha) * b) / alpha))) for c, b in zip(p, bg)
            )
            shown = (alpha - NOISE_FLOOR) / (1 - NOISE_FLOOR)
            dst[x, y] = (*colour, round(shown * 255))
    return out


def crop_square(image: Image.Image, margin: float = 0.04) -> Image.Image:
    """Crop to the visible artwork, padded to a centred square."""
    alpha = image.getchannel("A").point(lambda a: 255 if a > 24 else 0)
    box = alpha.getbbox()
    if box:
        image = image.crop(box)
    side = round(max(image.size) * (1 + 2 * margin))
    canvas = Image.new("RGBA", (side, side), (0, 0, 0, 0))
    canvas.paste(image, ((side - image.width) // 2, (side - image.height) // 2))
    return canvas


def dib_frame(image: Image.Image) -> bytes:
    """32bpp BGRA bottom-up, followed by a 1bpp AND mask."""
    width, height = image.size
    px = image.load()
    header = struct.pack("<IiiHHIIiiII", 40, width, height * 2, 1, 32, 0, 0, 0, 0, 0, 0)

    xor = bytearray()
    for y in range(height - 1, -1, -1):
        for x in range(width):
            r, g, b, a = px[x, y]
            xor += bytes((b, g, r, a))

    row_bytes = ((width + 31) // 32) * 4
    mask = bytearray()
    for y in range(height - 1, -1, -1):
        row = bytearray(row_bytes)
        for x in range(width):
            if px[x, y][3] == 0:
                row[x // 8] |= 0x80 >> (x % 8)
        mask += row

    return header + bytes(xor) + bytes(mask)


def resize(image: Image.Image, size: int) -> Image.Image:
    # Resampling premultiplied colour avoids dark fringes along soft edges.
    premultiplied = image.convert("RGBa")
    return premultiplied.resize((size, size), Image.LANCZOS).convert("RGBA")


def crisp(image: Image.Image) -> Image.Image:
    """Drop the faint glow and steepen the edge, for very small frames."""
    r, g, b, a = image.split()
    a = a.point(lambda v: max(0, min(255, round((v - 70) * 255 / 160))))
    return Image.merge("RGBA", (r, g, b, a))


def frame(source: Image.Image, size: int) -> Image.Image:
    image = resize(source, size)
    return crisp(image) if size <= CRISP_UP_TO else image


def write_icon(source: Image.Image) -> None:
    frames = [(s, dib_frame(frame(source, s))) for s in SIZES]
    out = bytearray(struct.pack("<HHH", 0, 1, len(frames)))
    offset = 6 + 16 * len(frames)
    for size, data in frames:
        edge = 0 if size >= 256 else size
        out += struct.pack("<BBBBHHII", edge, edge, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    for _, data in frames:
        out += data
    ICON.write_bytes(bytes(out))


def main() -> None:
    if len(sys.argv) > 1:
        artwork = Image.open(sys.argv[1])
        source = crop_square(remove_matte(artwork))
        if source.width > 1024:
            source = resize(source, 1024)
        source.save(SOURCE, optimize=True)
        print(f"imported {sys.argv[1]} -> {SOURCE.name} ({source.width}px)")

    source = Image.open(SOURCE).convert("RGBA")
    write_icon(source)
    resize(source, 256).save(LOGO, optimize=True)
    print(f"wrote {ICON.relative_to(ROOT)} ({len(SIZES)} frames) and {LOGO.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
