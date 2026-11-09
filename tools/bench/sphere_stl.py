#!/usr/bin/env python3
"""Binary STL of a UV sphere: 2 * slices * (stacks - 1) triangles (default 1 000 000).

The same mesh as `crates/perf` (slices=1000, stacks=501, radius 40 mm), centred
above the bed so the app drops it onto the plate unchanged. Pure stdlib.
Usage: python3 -I sphere_stl.py OUT.stl [--slices N] [--stacks N] [--radius MM]
"""
import argparse
import math
import struct


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('out')
    p.add_argument('--slices', type=int, default=1000)
    p.add_argument('--stacks', type=int, default=501)
    p.add_argument('--radius', type=float, default=40.0)
    a = p.parse_args()
    n, m, r = a.slices, a.stacks, a.radius
    rings = []
    for i in range(1, m):
        phi = math.pi * i / m
        s, c = math.sin(phi) * r, math.cos(phi) * r + r
        rings.append([(s * math.cos(2 * math.pi * j / n), s * math.sin(2 * math.pi * j / n), c) for j in range(n)])
    top, bottom = (0.0, 0.0, 2 * r), (0.0, 0.0, 0.0)
    triangles = []
    for j in range(n):
        k = (j + 1) % n
        triangles.append((top, rings[0][j], rings[0][k]))
        triangles.append((bottom, rings[-1][k], rings[-1][j]))
    for i in range(m - 2):
        a0, a1 = rings[i], rings[i + 1]
        for j in range(n):
            k = (j + 1) % n
            triangles.append((a0[j], a1[j], a0[k]))
            triangles.append((a0[k], a1[j], a1[k]))
    record = struct.Struct('<12fH')
    out = bytearray(84 + record.size * len(triangles))
    out[:80] = b'slicenv bench sphere'.ljust(80, b' ')
    struct.pack_into('<I', out, 80, len(triangles))
    offset = 84
    for p0, p1, p2 in triangles:
        ux, uy, uz = p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]
        vx, vy, vz = p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]
        nx, ny, nz = uy * vz - uz * vy, uz * vx - ux * vz, ux * vy - uy * vx
        length = math.sqrt(nx * nx + ny * ny + nz * nz) or 1.0
        record.pack_into(out, offset, nx / length, ny / length, nz / length, *p0, *p1, *p2, 0)
        offset += record.size
    with open(a.out, 'wb') as f:
        f.write(out)
    print(f'{a.out}: {len(triangles)} triangles')


if __name__ == '__main__':
    main()
