#!/usr/bin/env python3
"""3MF-проєкт для скриншотів: дві пластини з моделями з models.py, пресети A1.

  пластина 1 — торовий вузол, дві шестерні, кашпо;
  пластина 2 — кручена ваза.

Формат і пресети — як у tools/bench/fixture_3mf.py (звідти й беруться функції).
  python3 -I project.py <тека з STL> OUT.3mf --profiles <каталог профілів рушія з BBL/>
"""
import argparse
import os
import struct
import sys
import zipfile

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'bench'))
import fixture_3mf as fx  # noqa: E402

LAYOUT = [  # (файл, назва, пластина, x, y відносно центру пластини)
    ('torus-knot.stl', 'Torus knot', 0, -45, -45),
    ('gear.stl', 'Gear', 0, 55, 60),
    ('gear.stl', 'Gear', 0, 55, -10),
    ('hex-planter.stl', 'Hex planter', 0, -50, 55),
    ('twist-vase.stl', 'Twisted vase', 1, 0, 0),
]


def read_stl(path):
    data = open(path, 'rb').read()
    n = struct.unpack('<I', data[80:84])[0]
    index, verts, tris = {}, [], []
    for i in range(n):
        v = struct.unpack('<12f', data[84 + 50 * i:84 + 50 * i + 48])
        tri = []
        for k in range(3):
            p = tuple(round(x, 4) for x in v[3 + 3 * k:6 + 3 * k])
            if p not in index:
                index[p] = len(verts)
                verts.append(p)
            tri.append(index[p])
        tris.append(tuple(tri))
    # Центр по XY, низ на z = 0 — позицію задає transform.
    xs, ys, zs = zip(*verts)
    cx, cy, z0 = (min(xs) + max(xs)) / 2, (min(ys) + max(ys)) / 2, min(zs)
    return [(x - cx, y - cy, z - z0) for x, y, z in verts], tris


def settings_xml(objects, plates):
    out = ['<?xml version="1.0" encoding="UTF-8"?>\n<config>\n']
    for oid, name, tris in objects:
        out.append(f'  <object id="{oid}">\n'
                   f'    <metadata key="name" value="{name}"/>\n'
                   f'    <metadata key="extruder" value="1"/>\n'
                   f'    <metadata face_count="{tris}"/>\n'
                   # id частини = id сітки в 3dmodel.model: рушій зіставляє за ним.
                   f'    <part id="{oid}" subtype="normal_part">\n'
                   f'      <metadata key="name" value="{name}"/>\n'
                   f'      <metadata key="matrix" value="1 0 0 0 0 1 0 0 0 0 1 0 0 0 0 1"/>\n'
                   f'    </part>\n  </object>\n')
    for k, oids in enumerate(plates):
        out.append(f'  <plate>\n    <metadata key="plater_id" value="{k + 1}"/>\n'
                   f'    <metadata key="locked" value="false"/>\n')
        for oid in oids:
            out.append(f'    <model_instance>\n'
                       f'      <metadata key="object_id" value="{oid}"/>\n'
                       f'      <metadata key="instance_id" value="0"/>\n'
                       f'    </model_instance>\n')
        out.append('  </plate>\n')
    out.append('</config>\n')
    return ''.join(out)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('models')
    p.add_argument('out')
    p.add_argument('--profiles', required=True)
    a = p.parse_args()
    meshes, objects, items, plates = {}, [], [], [[], []]
    for oid, (file, name, plate, x, y) in enumerate(LAYOUT, start=1):
        mesh = meshes.setdefault(file, read_stl(os.path.join(a.models, file)))
        objects.append((oid, mesh))
        items.append((oid, (fx.BED_CENTER + plate * fx.STRIDE + x, fx.BED_CENTER + y)))
        plates[plate].append(oid)
    model = fx.model_xml(objects, items).replace('slicenv bench fixture', 'Slicenv demo')
    with zipfile.ZipFile(a.out, 'w', zipfile.ZIP_DEFLATED, compresslevel=6) as z:
        z.writestr('[Content_Types].xml', fx.CONTENT_TYPES)
        z.writestr('_rels/.rels', fx.RELS)
        z.writestr('3D/3dmodel.model', model)
        z.writestr('Metadata/model_settings.config',
                   settings_xml([(oid, LAYOUT[oid - 1][1], len(m[1])) for oid, m in objects], plates))
        z.writestr('Metadata/project_settings.config', fx.project_settings(a.profiles))
    print(a.out)


if __name__ == '__main__':
    main()
