"""Export RXUI's linear RGBA8 framebuffer captures as sRGB PNG files."""
import argparse
from pathlib import Path
import struct
import zlib


def chunk(kind, data):
    return (
        struct.pack(">I", len(data))
        + kind
        + data
        + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)
    )


def convert(path, width, height):
    pixels = bytearray(path.read_bytes())
    if len(pixels) != width * height * 4:
        raise ValueError(f"{path}: expected {width * height * 4} RGBA8 bytes")
    # The GPU framebuffer is linear; PNG viewers expect sRGB channel values.
    channels = bytes(
        round(255 * (12.92 * x if x <= 0.0031308 else 1.055 * x ** (1 / 2.4) - 0.055))
        for x in (value / 255 for value in range(256))
    )
    for index in range(0, len(pixels), 4):
        for channel in range(3):
            pixels[index + channel] = channels[pixels[index + channel]]
    stride = width * 4
    rows = b"".join(b"\0" + pixels[start:start + stride] for start in range(0, len(pixels), stride))
    image = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
        + chunk(b"sRGB", b"\0")
        + chunk(b"IDAT", zlib.compress(rows))
        + chunk(b"IEND", b"")
    )
    destination = path.with_suffix(".png")
    destination.write_bytes(image)
    print(destination)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("path", type=Path, help="An RGBA8 file or directory of .rgba files")
    parser.add_argument("--width", type=int, required=True)
    parser.add_argument("--height", type=int, required=True)
    args = parser.parse_args()
    if args.width <= 0 or args.height <= 0:
        parser.error("width and height must be positive")
    paths = sorted(args.path.glob("*.rgba")) if args.path.is_dir() else [args.path]
    if not paths:
        parser.error("no .rgba captures found")
    for path in paths:
        convert(path, args.width, args.height)
