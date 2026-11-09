#!/usr/bin/env python3
"""Write a binary STL sphere with 100,352 triangles for the UI replay."""
import math
import struct
import sys

n = 224
path = sys.argv[1] if len(sys.argv) > 1 else "/tmp/bambu-perf-100k.stl"


def point(i, j):
    a, b = math.pi * i / n, 2 * math.pi * j / n
    return (100 + 35 * math.sin(a) * math.cos(b),
            100 + 35 * math.sin(a) * math.sin(b), 36 + 35 * math.cos(a))


with open(path, "wb") as output:
    output.write(b"UI performance fixture".ljust(80, b"\0"))
    output.write(struct.pack("<I", 2 * n * n))
    for i in range(n):
        for j in range(n):
            a, b, c, d = point(i, j), point(i + 1, j), point(i + 1, j + 1), point(i, j + 1)
            for tri in [(a, b, c), (a, c, d)]:
                output.write(struct.pack("<12fH", 0, 0, 0, *tri[0], *tri[1], *tri[2], 0))
print(path)
