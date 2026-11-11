#!/usr/bin/env python3
"""Детерміновані 3MF-фікстури для порівняльного бенча (tools/bench/compare.py).

  small  — 1 пластина, 1 UV-сфера ≈20 000 △ (r 20 мм);
  heavy  — 4 пластини по одній UV-сфері 250 000 △ (r 30 мм), разом ≈1 000 000 △;
  plates4, plates12 — 4 / 12 пластин з легкими сферами різного радіуса (колонка пластин
           «Перегляду», нарізка всіх пластин справжнім рушієм за секунди).

Формат — як пише Bambu Studio: метадані `Application`, пластини в
`Metadata/model_settings.config`, глобальна сітка пластин із кроком стіл × 1.2
(256 мм → 307.2, як `project::PLATE_STRIDE`). Меш — у самому 3dmodel.model, без
production-розширення: його читають Slicenv, Bambu Studio і OrcaSlicer.

`Metadata/project_settings.config` — системні пресети A1 0.4 / 0.20mm Standard /
PLA Basic, розгорнуті по `inherits` з каталогу профілів рушія (`--profiles
<resources/profiles>`). Без нього Orca CLI 2.4.2 падає (SIGSEGV у `CLI::run`) після
запису G-code, а GUI відкривають файл з іншим принтером. Назв пластин (`plater_name`)
немає навмисно: вони валять Orca CLI (OrcaSlicer#7920, див. docs/PORTING.md).

Лише stdlib. Використання:
  python3 -I fixture_3mf.py {small|heavy} OUT.3mf --profiles ENGINE/resources/profiles
"""
import argparse
import json
import math
import os
import zipfile

PRESETS = {
    'machine': 'Bambu Lab A1 0.4 nozzle',
    'process': '0.20mm Standard @BBL A1',
    'filament': 'Bambu PLA Basic @BBL A1',
}
ID_KEYS = {'machine': 'printer_settings_id', 'process': 'print_settings_id',
           'filament': 'filament_settings_id'}

STRIDE = 256.0 * 1.2
BED_CENTER = 128.0
KINDS = {
    # (пластин, slices, stacks, радіус): △ = 2 · slices · (stacks − 1)
    'small': (1, 100, 101, 20.0),
    'heavy': (4, 500, 251, 30.0),
    'plates4': (4, 48, 25, 10.0),
    'plates12': (12, 32, 17, 8.0),
}
# Радіус сфери пластини k множиться на цей ряд (лише plates*): мініатюри відрізняються.
SCALES = (1.0, 1.6, 2.2, 1.3)


def sphere(n, m, r):
    """Індексована UV-сфера, що стоїть на z = 0 з центром у (0, 0, r)."""
    verts = [(0.0, 0.0, 2 * r)]
    for i in range(1, m):
        phi = math.pi * i / m
        s, c = math.sin(phi) * r, math.cos(phi) * r + r
        for j in range(n):
            t = 2 * math.pi * j / n
            verts.append((s * math.cos(t), s * math.sin(t), c))
    verts.append((0.0, 0.0, 0.0))
    bottom = len(verts) - 1
    ring = lambda i, j: 1 + i * n + (j % n)  # noqa: E731
    tris = []
    for j in range(n):
        tris.append((0, ring(0, j), ring(0, j + 1)))
        tris.append((bottom, ring(m - 2, j + 1), ring(m - 2, j)))
    for i in range(m - 2):
        for j in range(n):
            tris.append((ring(i, j), ring(i + 1, j), ring(i, j + 1)))
            tris.append((ring(i, j + 1), ring(i + 1, j), ring(i + 1, j + 1)))
    return verts, tris


def find_preset(vendor_dir, kind, name):
    """Файл пресета `name` у вендорі (з підтеками), далі в OrcaFilamentLibrary — як рушій."""
    library = os.path.join(os.path.dirname(vendor_dir), 'OrcaFilamentLibrary')
    for root in (vendor_dir, library):
        for dp, _, fn in os.walk(os.path.join(root, kind)):
            for f in fn:
                if not f.endswith('.json'):
                    continue
                path = os.path.join(dp, f)
                with open(path, encoding='utf-8') as fh:
                    data = json.load(fh)
                if data.get('name') == name:
                    return data
    raise FileNotFoundError(f'{kind}/{name} у {vendor_dir}')


def resolve(vendor_dir, kind, name):
    """Пресет із розгорнутими `inherits` і `include` (як `PresetBundle` рушія).

    Батька шукаємо лише в тому ж вендорі: `fdm_machine_common` тощо є в кожного вендора
    з різними значеннями."""
    own = find_preset(vendor_dir, kind, name)
    parent = own.get('inherits')
    merged = resolve(vendor_dir, kind, parent) if parent else {}
    for include in own.get('include', []):
        template = resolve(vendor_dir, kind, include)
        merged.update({k: v for k, v in template.items() if k not in ('name', 'instantiation')})
    merged.update(own)
    merged.pop('include', None)
    return merged


def project_settings(profiles):
    vendor = os.path.join(profiles, 'BBL')
    config = {}
    for kind, name in PRESETS.items():
        preset = resolve(vendor, kind, name)
        for key in ('name', 'inherits', 'from', 'type', 'instantiation', 'setting_id',
                    'filament_id', 'description', 'compatible_printers',
                    'compatible_printers_condition', 'compatible_prints',
                    'compatible_prints_condition'):
            preset.pop(key, None)
        config.update(preset)
        config[ID_KEYS[kind]] = [name] if kind == 'filament' else name
    # Як у GUI після вибору принтера: типова пластина моделі принтера, не дефолт рушія (Cool Plate).
    model = os.path.join(vendor, 'machine', f"{config.get('printer_model', '')}.json")
    if os.path.isfile(model):
        with open(model, encoding='utf-8') as f:
            # Каталог із кешу .opc не несе default_bed_type: тоді, як і Slicenv, перша зі
            # стандартних пластин (у всіх моделей Bambu Lab це вона ж).
            bed = json.load(f).get('default_bed_type') or 'Textured PEI Plate'
        config['curr_bed_type'] = bed
    config.update({'name': 'project_settings', 'from': 'project', 'version': '01.10.00.00'})
    return json.dumps(config, indent=4, ensure_ascii=False, sort_keys=True)


def model_xml(objects, items):
    out = ['<?xml version="1.0" encoding="UTF-8"?>\n'
           '<model unit="millimeter" xml:lang="en-US" '
           'xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">\n'
           ' <metadata name="Application">BambuStudio-01.10.00.00</metadata>\n'
           ' <metadata name="BambuStudio:3mfVersion">1</metadata>\n'
           ' <metadata name="Title">slicenv bench fixture</metadata>\n'
           ' <resources>\n']
    for oid, (verts, tris) in objects:
        out.append(f'  <object id="{oid}" type="model">\n   <mesh>\n    <vertices>\n')
        out.extend(f'     <vertex x="{x:.4f}" y="{y:.4f}" z="{z:.4f}"/>\n' for x, y, z in verts)
        out.append('    </vertices>\n    <triangles>\n')
        out.extend(f'     <triangle v1="{a}" v2="{b}" v3="{c}"/>\n' for a, b, c in tris)
        out.append('    </triangles>\n   </mesh>\n  </object>\n')
    out.append(' </resources>\n <build>\n')
    for oid, (x, y) in items:
        out.append(f'  <item objectid="{oid}" transform="1 0 0 0 1 0 0 0 1 {x:.3f} {y:.3f} 0" printable="1"/>\n')
    out.append(' </build>\n</model>\n')
    return ''.join(out)


def settings_xml(count, tri_counts):
    out = ['<?xml version="1.0" encoding="UTF-8"?>\n<config>\n']
    for k in range(count):
        oid = k + 1
        out.append(f'  <object id="{oid}">\n'
                   f'    <metadata key="name" value="sphere_{oid}"/>\n'
                   f'    <metadata key="extruder" value="1"/>\n'
                   f'    <metadata face_count="{tri_counts[k]}"/>\n'
                   f'    <part id="1" subtype="normal_part">\n'
                   f'      <metadata key="name" value="sphere_{oid}"/>\n'
                   f'      <metadata key="matrix" value="1 0 0 0 0 1 0 0 0 0 1 0 0 0 0 1"/>\n'
                   f'    </part>\n  </object>\n')
    for k in range(count):
        out.append(f'  <plate>\n    <metadata key="plater_id" value="{k + 1}"/>\n'
                   f'    <metadata key="locked" value="false"/>\n'
                   f'    <model_instance>\n'
                   f'      <metadata key="object_id" value="{k + 1}"/>\n'
                   f'      <metadata key="instance_id" value="0"/>\n'
                   f'    </model_instance>\n  </plate>\n')
    out.append('</config>\n')
    return ''.join(out)


CONTENT_TYPES = ('<?xml version="1.0" encoding="UTF-8"?>\n'
                 '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">\n'
                 ' <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>\n'
                 ' <Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/>\n'
                 '</Types>\n')
RELS = ('<?xml version="1.0" encoding="UTF-8"?>\n'
        '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">\n'
        ' <Relationship Target="/3D/3dmodel.model" Id="rel0" '
        'Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/>\n'
        '</Relationships>\n')


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('kind', choices=sorted(KINDS))
    p.add_argument('out')
    p.add_argument('--profiles', required=True,
                   help='resources/profiles рушія (з теки BBL беруться пресети A1)')
    a = p.parse_args()
    plates, n, m, r = KINDS[a.kind]
    varied = a.kind.startswith('plates')
    meshes = [sphere(n, m, r * (SCALES[k % len(SCALES)] if varied else 1.0)) for k in range(plates)]
    mesh = meshes[0]
    columns = max(1, math.ceil(math.sqrt(plates)))
    objects, items = [], []
    for k in range(plates):
        oid = k + 1
        objects.append((oid, meshes[k]))
        items.append((oid, (BED_CENTER + (k % columns) * STRIDE, BED_CENTER - (k // columns) * STRIDE)))
    with zipfile.ZipFile(a.out, 'w', zipfile.ZIP_DEFLATED, compresslevel=6) as z:
        z.writestr('[Content_Types].xml', CONTENT_TYPES)
        z.writestr('_rels/.rels', RELS)
        z.writestr('3D/3dmodel.model', model_xml(objects, items))
        z.writestr('Metadata/model_settings.config', settings_xml(plates, [len(mesh[1])] * plates))
        z.writestr('Metadata/project_settings.config', project_settings(a.profiles))
    print(f'{a.out}: {plates} пластин, {plates * len(mesh[1])} △')


if __name__ == '__main__':
    main()
