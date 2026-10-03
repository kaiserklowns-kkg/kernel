#!/usr/bin/env python3
"""Sparse disk images: only the non-zero 512-byte sectors are stored.

Format (little-endian): b"OCSPARSE", the image size (u64), then runs of
[offset u64][length u32][bytes]. `pack IMAGE OUT` writes one; `unpack IN
IMAGE` expands it (the tests and xtask do the same in Rust).
"""
import struct
import sys

SECTOR = 512
MAGIC = b"OCSPARSE"


def pack(image_path, out_path):
    data = open(image_path, "rb").read()
    runs = []
    start = None
    for offset in range(0, len(data), SECTOR):
        nonzero = any(data[offset:offset + SECTOR])
        if nonzero and start is None:
            start = offset
        elif not nonzero and start is not None:
            runs.append((start, offset))
            start = None
    if start is not None:
        runs.append((start, len(data)))
    with open(out_path, "wb") as out:
        out.write(MAGIC + struct.pack("<Q", len(data)))
        for begin, end in runs:
            out.write(struct.pack("<QI", begin, end - begin) + data[begin:end])


def unpack(in_path, image_path):
    blob = open(in_path, "rb").read()
    assert blob[:8] == MAGIC
    (size,) = struct.unpack_from("<Q", blob, 8)
    image = bytearray(size)
    at = 16
    while at < len(blob):
        offset, length = struct.unpack_from("<QI", blob, at)
        at += 12
        image[offset:offset + length] = blob[at:at + length]
        at += length
    open(image_path, "wb").write(image)


if __name__ == "__main__":
    {"pack": pack, "unpack": unpack}[sys.argv[1]](sys.argv[2], sys.argv[3])
