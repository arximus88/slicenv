#!/usr/bin/env python3
"""Порівняння двох G-code Bambu Studio / OrcaSlicer / Slicenv після нормалізації шуму.

    python3 -I tools/gcode-diff.py A.gcode.3mf B.gcode [--plate N] [--json]

Вхід — `.gcode` або `.gcode.3mf` (з архіву береться `Metadata/plate_N.gcode`).
Що робить:
  * відкидає шум: заголовок з версією й датою, мініатюри (THUMBNAIL_BLOCK), абсолютні
    шляхи, дати й час у коментарях, маркери прогресу `M73` (їх положення «плаває» навіть
    між двома прогонами того самого рушія; оцінку часу порівнюємо окремо);
  * порівнює CONFIG_BLOCK — параметри, з якими різав рушій. Різні параметри — головна
    причина різного G-code, тож цей список друкується першим;
  * порівнює підсумки: кількість шарів, оцінку часу, довжину й вагу філаменту, Z шарів;
  * порівнює траєкторії пошарово й називає перший різний шар і рядок.

Вердикт: `identical` — ідентичні після нормалізації; `config_differs` — відрізняються
параметри; `trajectory_differs` — параметри однакові, а траєкторії ні.
Код виходу: 0 — ідентичні, 1 — є відмінності, 2 — помилка входу.
Лише стандартна бібліотека.
"""

from __future__ import annotations

import argparse
import difflib
import fnmatch
import json
import re
import sys
import zipfile
from dataclasses import dataclass, field
from pathlib import Path

# Абсолютний шлях Unix чи Windows, що стоїть на початку значення, після пробілу, `=` чи лапок.
PATH_RE = re.compile(r"""(?<![^\s="'])(?:[A-Za-z]:[\\/]|/)(?:[^\s;,"'\\/]+[\\/])+[^\s;,"']*""")
DATE_RE = re.compile(r"\b\d{4}-\d{2}-\d{2}\b|\b\d{4}/\d{2}/\d{2}\b")
TIME_RE = re.compile(r"\b\d{1,2}:\d{2}:\d{2}\b")
DURATION_RE = re.compile(r"(?:(\d+)d\s*)?(?:(\d+)h\s*)?(?:(\d+)m\s*)?(?:(\d+)s)?")
NUMBER_RE = re.compile(r"-?\d+(?:\.\d+)?")


class InputError(Exception):
    pass


@dataclass
class Layer:
    z: float | None
    # (номер рядка у файлі, нормалізований текст)
    lines: list[tuple[int, str]] = field(default_factory=list)


@dataclass
class Gcode:
    source: str
    engine: str | None = None
    config: dict[str, str] = field(default_factory=dict)
    header: dict[str, str] = field(default_factory=dict)
    # Рядок `; key = value` / `; key: value` поза CONFIG_BLOCK (підсумки Orca в кінці).
    footer: dict[str, str] = field(default_factory=dict)
    # Шар 0 — усе до першої зміни шару (стартовий G-code).
    layers: list[Layer] = field(default_factory=list)


def read_text(path: Path, plate: int | None) -> tuple[str, str]:
    """Текст G-code і людська назва джерела."""
    if not path.is_file():
        raise InputError(f"немає файлу {path}")
    if zipfile.is_zipfile(path):
        with zipfile.ZipFile(path) as z:
            names = sorted(
                (n for n in z.namelist() if re.fullmatch(r"Metadata/plate_\d+\.gcode", n)),
                key=lambda n: int(re.search(r"\d+", n.rsplit("/", 1)[1]).group()),
            )
            if not names:
                raise InputError(f"{path}: у 3MF немає Metadata/plate_N.gcode — це не нарізаний файл")
            if plate is None:
                if len(names) > 1:
                    raise InputError(
                        f"{path}: кілька пластин ({', '.join(names)}) — вкажіть --plate N"
                    )
                name = names[0]
            else:
                name = f"Metadata/plate_{plate}.gcode"
                if name not in names:
                    raise InputError(f"{path}: немає {name}; є {', '.join(names)}")
            return z.read(name).decode("utf-8", "replace"), f"{path}!{name}"
    return path.read_text("utf-8", "replace"), str(path)


def normalize_comment(line: str) -> str:
    line = PATH_RE.sub("<PATH>", line)
    line = DATE_RE.sub("<DATE>", line)
    return TIME_RE.sub("<TIME>", line)


def key_value(line: str) -> tuple[str, str] | None:
    body = line[1:].strip()
    for sep in (" = ", ": ", " : "):
        if sep in body:
            k, v = body.split(sep, 1)
            return k.strip(), v.strip()
    if body.endswith(" =") or body.endswith(":"):
        return body.rstrip("=:").strip(), ""
    return None


def parse(text: str, source: str, keep_m73: bool = False) -> Gcode:
    g = Gcode(source=source)
    layers = [Layer(z=None)]
    block = None
    seen_blocks = False
    in_thumbnail = False
    for number, raw in enumerate(text.splitlines(), 1):
        line = raw.rstrip()
        stripped = line.strip()
        if stripped.startswith(";"):
            marker = stripped.lstrip("; ").upper()
            if marker.endswith("_BLOCK_START"):
                block = marker[: -len("_BLOCK_START")]
                seen_blocks = True
                continue
            if marker.endswith("_BLOCK_END"):
                block = None
                continue
            low = stripped.lower()
            # Мініатюри: `; thumbnail begin`, `; thumbnail_JPG begin`, `; thumbnail_QOI begin`…
            if re.match(r";\s*thumbnail(_\w+)?\s+begin", low):
                in_thumbnail = True
                continue
            if re.match(r";\s*thumbnail(_\w+)?\s+end", low):
                in_thumbnail = False
                continue
        if in_thumbnail or block == "THUMBNAIL":
            continue
        if block == "HEADER":
            if stripped.startswith(";"):
                body = stripped[1:].strip()
                if g.engine is None and re.match(r"(generated by |BambuStudio|OrcaSlicer|Slicenv)", body):
                    g.engine = re.sub(r"\s+on\s+.*$", "", body.removeprefix("generated by ")).strip()
                # Bambu: «model printing time: …; total estimated time: …» в одному рядку.
                for part in body.split("; "):
                    kv = key_value(";" + part)
                    if kv:
                        g.header[kv[0].lower()] = kv[1]
            continue
        if block == "CONFIG":
            kv = key_value(stripped) if stripped.startswith(";") else None
            if kv:
                g.config[kv[0]] = PATH_RE.sub("<PATH>", kv[1])
            continue
        # Поза блоками у файлі з блоками лежать лише службові коментарі (ширини ліній,
        # підсумки Orca наприкінці) — це не траєкторія.
        if seen_blocks and block is None and stripped.startswith(";"):
            kv = key_value(stripped)
            if kv:
                g.footer[kv[0].lower()] = kv[1]
            continue
        if not seen_blocks and number <= 3 and stripped.startswith(";"):
            # Файл без блоків: версія — у першому коментарі.
            if g.engine is None and re.search(r"(generated by|BambuStudio|OrcaSlicer|PrusaSlicer)", stripped):
                g.engine = re.sub(r"\s+on\s+.*$", "", stripped.lstrip("; ").removeprefix("generated by ")).strip()
                continue
        if not stripped:
            continue
        upper = stripped.upper()
        if not keep_m73 and (upper.startswith("M73 ") or upper == "M73"):
            continue
        if stripped.startswith(";"):
            body = stripped[1:].strip()
            if body in ("CHANGE_LAYER", "LAYER_CHANGE"):
                layers.append(Layer(z=None))
            m = re.match(r"(?:Z_HEIGHT:|Z:)\s*(-?\d+(?:\.\d+)?)", body)
            if m and layers[-1].z is None and len(layers) > 1:
                layers[-1].z = float(m.group(1))
            line = normalize_comment(line)
        layers[-1].lines.append((number, line.strip()))
    g.layers = layers
    return g


def duration(text: str | None) -> int | None:
    if not text:
        return None
    m = DURATION_RE.search(text.strip())
    if not m or not any(m.groups()):
        return None
    d, h, mi, s = (int(x) if x else 0 for x in m.groups())
    return ((d * 24 + h) * 60 + mi) * 60 + s


def total(text: str | None) -> float | None:
    """Сума через кому (кілька філаментів): «12.3, 4.5» → 16.8."""
    if text is None:
        return None
    values = [float(x) for x in NUMBER_RE.findall(text)]
    return round(sum(values), 4) if values else None


def summary(g: Gcode) -> dict:
    h, f = g.header, g.footer
    real = [layer for layer in g.layers[1:]]
    layers = h.get("total layer number")
    return {
        "layers": int(layers) if layers and layers.isdigit() else len(real),
        "time_s": duration(h.get("total estimated time"))
        or duration(f.get("estimated printing time (normal mode)"))
        or duration(h.get("estimated printing time (normal mode)")),
        "filament_mm": total(h.get("total filament length [mm]") or f.get("filament used [mm]")),
        "filament_g": total(h.get("total filament weight [g]") or f.get("total filament used [g]") or f.get("filament used [g]")),
        "z": [layer.z for layer in real],
    }


def compare_config(a: dict[str, str], b: dict[str, str], ignore: list[str]) -> list[dict]:
    out = []
    for key in sorted(set(a) | set(b)):
        if any(fnmatch.fnmatchcase(key, pat) for pat in ignore):
            continue
        va, vb = a.get(key), b.get(key)
        if va != vb:
            out.append({"key": key, "a": va, "b": vb})
    return out


def compare_trajectory(a: Gcode, b: Gcode) -> dict:
    first = None
    lines = 0
    layers = 0
    for i in range(max(len(a.layers), len(b.layers))):
        la = a.layers[i].lines if i < len(a.layers) else []
        lb = b.layers[i].lines if i < len(b.layers) else []
        ta, tb = [t for _, t in la], [t for _, t in lb]
        if ta == tb:
            continue
        layers += 1
        sm = difflib.SequenceMatcher(None, ta, tb, autojunk=False)
        for op, i1, i2, j1, j2 in sm.get_opcodes():
            if op == "equal":
                continue
            lines += max(i2 - i1, j2 - j1)
            if first is None:
                z = (a.layers[i].z if i < len(a.layers) else None) or (
                    b.layers[i].z if i < len(b.layers) else None
                )
                first = {
                    "layer": i,
                    "z": z,
                    "line_a": la[i1][0] if i1 < len(la) else None,
                    "line_b": lb[j1][0] if j1 < len(lb) else None,
                    "text_a": ta[i1] if i1 < i2 else None,
                    "text_b": tb[j1] if j1 < j2 else None,
                }
    total_lines = max(sum(len(x.lines) for x in a.layers), sum(len(x.lines) for x in b.layers))
    return {
        "identical": first is None,
        "different_lines": lines,
        "different_share": round(lines / total_lines, 5) if total_lines else 0.0,
        "different_layers": layers,
        "total_lines": total_lines,
        "first": first,
    }


def compare(a: Gcode, b: Gcode, ignore_keys: list[str] | None = None) -> dict:
    config = compare_config(a.config, b.config, ignore_keys or [])
    sa, sb = summary(a), summary(b)
    z_first = None
    for i, (za, zb) in enumerate(zip(sa["z"], sb["z"])):
        if za != zb:
            z_first = {"layer": i + 1, "a": za, "b": zb}
            break
    if z_first is None and len(sa["z"]) != len(sb["z"]):
        n = min(len(sa["z"]), len(sb["z"]))
        z_first = {
            "layer": n + 1,
            "a": sa["z"][n] if n < len(sa["z"]) else None,
            "b": sb["z"][n] if n < len(sb["z"]) else None,
        }
    summaries = {
        k: {"a": sa[k], "b": sb[k], "same": sa[k] == sb[k]}
        for k in ("layers", "time_s", "filament_mm", "filament_g")
    }
    summaries["z"] = {"same": z_first is None, "first": z_first}
    trajectory = compare_trajectory(a, b)
    if config:
        verdict = "config_differs"
    elif trajectory["identical"] and all(v["same"] for v in summaries.values()):
        verdict = "identical"
    else:
        verdict = "trajectory_differs"
    return {
        "verdict": verdict,
        "a": {"source": a.source, "engine": a.engine, "config_keys": len(a.config)},
        "b": {"source": b.source, "engine": b.engine, "config_keys": len(b.config)},
        "config": config,
        "summary": summaries,
        "trajectory": trajectory,
    }


def fmt_time(s: int | None) -> str:
    if s is None:
        return "—"
    h, rest = divmod(s, 3600)
    m, sec = divmod(rest, 60)
    return f"{h}h {m}m {sec}s" if h else f"{m}m {sec}s"


def short(v: str | None, width: int = 60) -> str:
    if v is None:
        return "(немає)"
    v = v.replace("\n", "\\n")
    return v if len(v) <= width else v[: width - 1] + "…"


def report(r: dict, limit: int | None = 40) -> str:
    out = [
        f"A: {r['a']['source']} ({r['a']['engine'] or 'рушій невідомий'})",
        f"B: {r['b']['source']} ({r['b']['engine'] or 'рушій невідомий'})",
        "",
    ]
    cfg = r["config"]
    if cfg:
        out.append(f"Параметри (CONFIG_BLOCK): відрізняються {len(cfg)}")
        shown = cfg if limit is None else cfg[:limit]
        width = max(len(c["key"]) for c in shown)
        for c in shown:
            out.append(f"  {c['key']:<{width}}  A: {short(c['a'])}")
            out.append(f"  {'':<{width}}  B: {short(c['b'])}")
        if len(shown) < len(cfg):
            out.append(f"  … ще {len(cfg) - len(shown)} (усі — з --all або --json)")
    else:
        out.append(
            f"Параметри (CONFIG_BLOCK): однакові ({r['a']['config_keys']} ключів)"
            if r["a"]["config_keys"]
            else "Параметри (CONFIG_BLOCK): блоку немає в жодному файлі"
        )
    out.append("")
    out.append("Підсумки:")
    s = r["summary"]
    rows = [
        ("шарів", s["layers"], str),
        ("оцінка часу", s["time_s"], fmt_time),
        ("філамент, мм", s["filament_mm"], lambda v: "—" if v is None else f"{v:g}"),
        ("філамент, г", s["filament_g"], lambda v: "—" if v is None else f"{v:g}"),
    ]
    for name, v, f in rows:
        mark = "=" if v["same"] else "≠"
        out.append(f"  {mark} {name:<13} A: {f(v['a']):<14} B: {f(v['b'])}")
    z = s["z"]
    if z["same"]:
        out.append("  = Z шарів      однакові")
    else:
        fz = z["first"]
        out.append(f"  ≠ Z шарів      перший різний шар {fz['layer']}: A {fz['a']}, B {fz['b']}")
    out.append("")
    t = r["trajectory"]
    if t["identical"]:
        out.append(f"Траєкторії: однакові ({t['total_lines']} рядків після нормалізації)")
    else:
        f = t["first"]
        where = "стартовий G-code (до першого шару)" if f["layer"] == 0 else f"шар {f['layer']} (Z {f['z']})"
        out.append(
            f"Траєкторії: різних рядків {t['different_lines']} з {t['total_lines']}, "
            f"різних шарів {t['different_layers']}"
        )
        out.append(f"  перша відмінність: {where}")
        out.append(f"    A рядок {f['line_a']}: {short(f['text_a'], 100)}")
        out.append(f"    B рядок {f['line_b']}: {short(f['text_b'], 100)}")
        same_shape = all(s[k]["same"] for k in ("layers", "filament_mm", "z"))
        if not cfg and same_shape and t["different_share"] < 0.01:
            out.append(
                "  примітка: <1 % рядків, шари й філамент однакові — так виглядають два прогони"
                " того самого Bambu Studio (рушій недетермінований, див. docs/TESTING.md)"
            )
    out.append("")
    verdict = {
        "identical": "ВЕРДИКТ: ідентичні після нормалізації",
        "config_differs": f"ВЕРДИКТ: параметри відрізняються ({len(cfg)}): "
        + ", ".join(c["key"] for c in cfg[:12])
        + (" …" if len(cfg) > 12 else ""),
        "trajectory_differs": "ВЕРДИКТ: параметри однакові, але траєкторії різні",
    }[r["verdict"]]
    out.append(verdict)
    return "\n".join(out)


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("a", type=Path, help=".gcode або .gcode.3mf")
    p.add_argument("b", type=Path, help=".gcode або .gcode.3mf")
    p.add_argument("--plate", type=int, help="номер пластини в 3MF (типово — єдина пластина)")
    p.add_argument("--json", action="store_true", help="машинний вивід JSON")
    p.add_argument(
        "--ignore-key",
        action="append",
        default=[],
        metavar="GLOB",
        help="не порівнювати параметр (можна кілька, glob: 'filament_*')",
    )
    p.add_argument("--all", action="store_true", help="друкувати всі різні параметри, не лише перші 40")
    p.add_argument("--keep-m73", action="store_true", help="не відкидати маркери прогресу M73")
    args = p.parse_args(argv)
    try:
        ga = parse(*read_text(args.a, args.plate), keep_m73=args.keep_m73)
        gb = parse(*read_text(args.b, args.plate), keep_m73=args.keep_m73)
    except (InputError, zipfile.BadZipFile, OSError) as e:
        print(f"gcode-diff: {e}", file=sys.stderr)
        return 2
    r = compare(ga, gb, args.ignore_key)
    if args.json:
        json.dump(r, sys.stdout, ensure_ascii=False, indent=2)
        print()
    else:
        print(report(r, None if args.all else 40))
    return 0 if r["verdict"] == "identical" else 1


if __name__ == "__main__":
    sys.exit(main())
