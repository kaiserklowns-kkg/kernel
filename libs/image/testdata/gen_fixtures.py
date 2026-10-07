"""Fixtures for libs/image tests, produced by the reference zlib.

Writes libs/image/src/fixtures.rs with:
- TEXT_ZLIB: zlib level 9 of TEXT (dynamic Huffman codes);
- SHORT_ZLIB: zlib level 9 of b"abcabcabc" (fixed codes, a back reference);
- GRADIENT_PNG: a 40x30 RGB PNG, zlib level 9, rows using each filter type;
  pixel (x, y) = (x * 6, y * 8, (x + y) * 3).
"""
import os
import struct
import zlib

# The repository root, from libs/image/testdata.
os.chdir(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", ".."))

TEXT = (b"Oceans OS decodes PNG images with its own inflater. " * 20
        + bytes(range(256)) + b"The end.")


def chunk(kind, body):
    return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF)


def paeth(a, b, c):
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    if pa <= pb and pa <= pc:
        return a
    if pb <= pc:
        return b
    return c


def gradient_png():
    w, h, bpp = 40, 30, 3
    rows = []
    for y in range(h):
        rows.append(bytes(v & 0xFF for x in range(w) for v in (x * 6, y * 8, (x + y) * 3)))
    raw = bytearray()
    for y, row in enumerate(rows):
        kind = y % 5
        prev = rows[y - 1] if y > 0 else bytes(len(row))
        out = bytearray([kind])
        for i, v in enumerate(row):
            left = row[i - bpp] if i >= bpp else 0
            up = prev[i]
            corner = prev[i - bpp] if i >= bpp else 0
            pred = [0, left, up, (left + up) // 2, paeth(left, up, corner)][kind]
            out.append((v - pred) & 0xFF)
        raw += out
    ihdr = struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr)
            + chunk(b"tEXt", b"Comment\x00made by zlib")
            + chunk(b"IDAT", zlib.compress(bytes(raw), 9)) + chunk(b"IEND", b""))


def rust_bytes(name, data):
    lines = [f"pub const {name}: &[u8] = &["]
    for i in range(0, len(data), 16):
        lines.append("    " + " ".join(f"0x{b:02x}," for b in data[i:i + 16]))
    lines.append("];")
    return "\n".join(lines)


parts = [
    "//! Test fixtures made by the reference zlib (libs/image/testdata/",
    "//! gen_fixtures.py, ADR-0090): compressed streams and a PNG this crate did not write.",
    "",
    rust_bytes("TEXT", TEXT),
    "",
    rust_bytes("TEXT_ZLIB", zlib.compress(TEXT, 9)),
    "",
    rust_bytes("SHORT_ZLIB", zlib.compress(b"abcabcabc", 9)),
    "",
    rust_bytes("GRADIENT_PNG", gradient_png()),
    "",
]
open("libs/image/src/fixtures.rs", "w", encoding="utf-8", newline="\n").write("\n".join(parts))
print("ok", len(zlib.compress(TEXT, 9)), len(gradient_png()))
