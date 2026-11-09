#!/usr/bin/env python3
"""Check Ukrainian source keys in implementation Rust files."""
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
CATALOG = ROOT / "crates/app/locales/en.json"
APP = ROOT / "crates/app/src"
SOURCES = [
    *(p for p in sorted(APP.rglob("*.rs")) if p.name != "i18n.rs"),
    ROOT / "crates/gcode/src/model.rs",
    ROOT / "crates/gcode/src/view.rs",
    ROOT / "crates/cloud/src/status.rs",
    ROOT / "crates/cloud/src/control.rs",
    ROOT / "crates/cloud/src/cover.rs",
    # Тексти помилок LAN показуються через tr(LanError::key()/detail()).
    ROOT / "crates/lan/src/error.rs",
    ROOT / "crates/lan/src/tls.rs",
]
CYRILLIC = re.compile(r"[А-Яа-яІіЇїЄєҐґ]")
LITERAL = re.compile(r'"((?:[^"\\\n]|\\.)*)"')


def blank(text: str, start: int, end: int) -> str:
    return re.sub(r"[^\n]", " ", text[start:end])


def implementation_text(text: str) -> str:
    """Mask comments and cfg(test) modules while preserving source line numbers."""
    masked = list(text)
    # Rust line and block comments (including nested block comments).
    i = 0
    while i < len(text):
        if text.startswith("//", i):
            j = text.find("\n", i)
            j = len(text) if j < 0 else j
            masked[i:j] = blank(text, i, j)
            i = j
        elif text.startswith("/*", i):
            j, depth = i + 2, 1
            while j < len(text) and depth:
                if text.startswith("/*", j): depth += 1; j += 2
                elif text.startswith("*/", j): depth -= 1; j += 2
                else: j += 1
            masked[i:j] = blank(text, i, j)
            i = j
        else:
            i += 1
    clean = "".join(masked)
    # Remove #[cfg(test)] mod ... { ... } blocks, accounting for nested braces.
    for match in list(re.finditer(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]\s*mod\b", clean)):
        opening = clean.find("{", match.end())
        if opening < 0: continue
        depth, j = 1, opening + 1
        while j < len(clean) and depth:
            if clean[j] == "{": depth += 1
            elif clean[j] == "}": depth -= 1
            j += 1
        masked[match.start():j] = blank(text, match.start(), j)
        clean = "".join(masked)
    return clean


def main() -> int:
    catalog = json.loads(CATALOG.read_text(encoding="utf-8"))
    missing: set[tuple[Path, str]] = set()
    for source in SOURCES:
        text = source.read_text(encoding="utf-8")
        scanned = implementation_text(text)
        for match in LITERAL.finditer(scanned):
            key = match.group(1)
            # These are logger payloads, test-fixture labels, or interpolation
            # arguments; they are not displayed as independently translated text.
            if key.startswith(("[cloud]", "[lan]")) or key in {
                "сопла", "столу", "Тестовий A1 (фіктивний)",
                "Потік хмари завершився з помилкою під час закриття",
                "не стартував потік хмари",
            }:
                continue
            if CYRILLIC.search(key) and key not in catalog:
                line = scanned.count("\n", 0, match.start()) + 1
                missing.add((source.relative_to(ROOT), f"{line}: {key}"))
    if missing:
        for source, entry in sorted(missing):
            print(f"{source}:{entry}")
        print(f"Missing {len(missing)} literal(s) from {CATALOG.relative_to(ROOT)}")
        return 1
    print(f"All scanned Cyrillic literals are present in {CATALOG.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
