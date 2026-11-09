#!/usr/bin/env python3
"""Процедурні моделі для скриншотів (власні, без сторонніх ліцензій), бінарний STL.

  torus-knot.stl  — торовий вузол (2,3), трубка r 4.5 мм;
  twist-vase.stl  — ваза: восьмипроменева зірка з поворотом 90° і розширенням;
  gear.stl        — шестерня 24 зуби з отвором під вал;
  hex-planter.stl — шестигранне кашпо з порожниною;
  trefoil-5-3, spiral-vase, gear-16, octo-planter — варіації для бібліотеки моделей.

Лише stdlib. Використання: python3 -I models.py <тека>
"""
import math
import os
import struct
import sys


def write_stl(path, tris):
    with open(path, 'wb') as f:
        f.write(b'slicenv screenshot model'.ljust(80, b'\0'))
        f.write(struct.pack('<I', len(tris)))
        for a, b, c in tris:
            ux, uy, uz = (b[i] - a[i] for i in range(3))
            vx, vy, vz = (c[i] - a[i] for i in range(3))
            n = (uy * vz - uz * vy, uz * vx - ux * vz, ux * vy - uy * vx)
            l = math.sqrt(sum(x * x for x in n)) or 1.0
            f.write(struct.pack('<12fH', *(x / l for x in n), *a, *b, *c, 0))


def grid(rows, cols, point, close_rows=True):
    """Трикутники поверхні, заданої point(i, j) на замкненій по j сітці."""
    tris = []
    last = rows if close_rows else rows - 1
    for i in range(last):
        for j in range(cols):
            a, b = point(i, j), point(i, (j + 1) % cols)
            c, d = point((i + 1) % rows, j), point((i + 1) % rows, (j + 1) % cols)
            tris += [(a, c, b), (b, c, d)]
    return tris


def cap(ring, center, flip):
    tris = []
    for j in range(len(ring)):
        a, b = ring[j], ring[(j + 1) % len(ring)]
        tris.append((center, b, a) if flip else (center, a, b))
    return tris


def torus_knot(p=2, q=3, R=26.0, r=10.0, tube=4.5, n=600, m=28):
    def curve(t):
        k = 2 * math.pi * t
        rr = R + r * math.cos(q * k)
        return (rr * math.cos(p * k), rr * math.sin(p * k), r * math.sin(q * k))

    frames = []
    for i in range(n):
        c0, c1 = curve(i / n), curve((i + 1) / n)
        t = [c1[k] - c0[k] for k in range(3)]
        tl = math.sqrt(sum(x * x for x in t))
        t = [x / tl for x in t]
        up = (0, 0, 1)
        nrm = (t[1] * up[2] - t[2] * up[1], t[2] * up[0] - t[0] * up[2], t[0] * up[1] - t[1] * up[0])
        nl = math.sqrt(sum(x * x for x in nrm))
        nrm = [x / nl for x in nrm]
        bi = (t[1] * nrm[2] - t[2] * nrm[1], t[2] * nrm[0] - t[0] * nrm[2], t[0] * nrm[1] - t[1] * nrm[0])
        frames.append((c0, nrm, bi))
    lift = r + tube

    def point(i, j):
        c, nn, bb = frames[i]
        a = 2 * math.pi * j / m
        return tuple(c[k] + tube * (math.cos(a) * nn[k] + math.sin(a) * bb[k]) + (lift if k == 2 else 0)
                     for k in range(3))

    return grid(n, m, lambda i, j: point(i, m - 1 - j))


def star(rr, k, a0, points=8, steps=12, depth=0.18):
    ring = []
    for s in range(points * steps):
        a = 2 * math.pi * s / (points * steps)
        w = 1 - depth * (0.5 + 0.5 * math.cos(points * a))
        ring.append((rr * w * math.cos(a + a0), rr * w * math.sin(a + a0), k))
    return ring


def twist_vase(h=110.0, layers=90, points=8, twist=math.pi / 2, base=24.0, bulge=10.0):
    rings = []
    for i in range(layers + 1):
        z = h * i / layers
        t = i / layers
        radius = base + bulge * math.sin(math.pi * t * 0.9) + 4 * t
        rings.append(star(radius, z, twist * t, points=points))
    tris = []
    cols = len(rings[0])
    for i in range(layers):
        for j in range(cols):
            a, b = rings[i][j], rings[i][(j + 1) % cols]
            c, d = rings[i + 1][j], rings[i + 1][(j + 1) % cols]
            tris += [(a, b, c), (b, d, c)]
    tris += cap(rings[0], (0, 0, 0), True) + cap(rings[-1], (0, 0, h), False)
    return tris


def gear(teeth=24, module=2.5, width=10.0, bore=8.0, steps=10):
    rp = teeth * module / 2
    ra, rf = rp + module, rp - 1.25 * module
    outer = []
    for t in range(teeth):
        for s in range(steps * 2):
            u = s / (steps * 2)
            a = 2 * math.pi * (t + u) / teeth
            ph = u * 2 * math.pi
            k = 0.5 + 0.5 * math.cos(ph)
            k = min(1.0, max(0.0, (k - 0.25) * 1.6))
            rr = rf + (ra - rf) * k
            outer.append((rr * math.cos(a), rr * math.sin(a)))
    inner = [(bore * math.cos(2 * math.pi * s / len(outer)), bore * math.sin(2 * math.pi * s / len(outer)))
             for s in range(len(outer))]
    n = len(outer)
    tris = []
    for j in range(n):
        o0, o1 = outer[j], outer[(j + 1) % n]
        i0, i1 = inner[j], inner[(j + 1) % n]
        tris += [((*o0, 0), (*o1, width), (*o1, 0)), ((*o0, 0), (*o0, width), (*o1, width))]
        tris += [((*i0, 0), (*i1, 0), (*i1, width)), ((*i0, 0), (*i1, width), (*i0, width))]
        tris += [((*o0, width), (*i0, width), (*i1, width)), ((*o0, width), (*i1, width), (*o1, width))]
        tris += [((*o0, 0), (*i1, 0), (*i0, 0)), ((*o0, 0), (*o1, 0), (*i1, 0))]
    return [(a, c, b) for a, b, c in tris]  # нормалі назовні


def hex_planter(r=32.0, h=48.0, wall=3.0, floor=3.0, sides=6):
    def ring(rr, z, rot=0.0):
        return [(rr * math.cos(2 * math.pi * s / sides + rot), rr * math.sin(2 * math.pi * s / sides + rot), z)
                for s in range(sides)]

    layers = 24
    outer = [ring(r * (0.82 + 0.18 * i / layers), h * i / layers, 0.35 * i / layers) for i in range(layers + 1)]
    ri = lambda i: r * (0.82 + 0.18 * i / layers) - wall / math.cos(math.pi / sides)
    inner = [ring(ri(i), floor + (h - floor) * i / layers, 0.35 * i / layers) for i in range(layers + 1)]
    tris = []
    for i in range(layers):
        for j in range(sides):
            a, b = outer[i][j], outer[i][(j + 1) % sides]
            c, d = outer[i + 1][j], outer[i + 1][(j + 1) % sides]
            tris += [(a, b, c), (b, d, c)]
            a, b = inner[i][j], inner[i][(j + 1) % sides]
            c, d = inner[i + 1][j], inner[i + 1][(j + 1) % sides]
            tris += [(a, c, b), (b, c, d)]
    top_o, top_i = outer[-1], inner[-1]
    for j in range(sides):
        a, b = top_o[j], top_o[(j + 1) % sides]
        c, d = top_i[j], top_i[(j + 1) % sides]
        tris += [(a, b, c), (b, d, c)]
    tris += cap(outer[0], (0, 0, 0), True) + cap(inner[0], (0, 0, floor), False)
    return tris


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else '.'
    os.makedirs(out, exist_ok=True)
    models = [
        ('torus-knot', torus_knot()),
        ('twist-vase', twist_vase()),
        ('gear', gear()),
        ('hex-planter', hex_planter()),
        ('trefoil-5-3', torus_knot(p=3, q=5, R=30.0, r=9.0, tube=3.6, n=900)),
        ('spiral-vase', twist_vase(h=140.0, layers=120, points=6, twist=math.pi, base=20.0, bulge=14.0)),
        ('gear-16', gear(teeth=16, module=3.0, width=8.0, bore=6.0)),
        ('octo-planter', hex_planter(r=26.0, h=40.0, sides=8)),
    ]
    for name, tris in models:
        write_stl(os.path.join(out, name + '.stl'), tris)
        print(name, len(tris))


if __name__ == '__main__':
    main()
