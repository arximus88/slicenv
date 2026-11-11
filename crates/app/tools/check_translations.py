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
    # Помилки Klipper/Moonraker: tr(MoonrakerError::key()).
    ROOT / "crates/moonraker/src/error.rs",
]
CYRILLIC = re.compile(r"[А-Яа-яІіЇїЄєҐґ]")
RAW = re.compile(r'b?r(#*)"')
CHAR = re.compile(r"'(\\.[^']*|[^\\'])'")


def literals(text: str) -> list[tuple[int, int, str]]:
    """String literals of Rust source as (start, end, value), outside comments and
    `#[cfg(test)] mod` blocks. A real lexer: `"//"` inside a string, `'"'` char literals
    and raw strings `r#"…"#` must not desynchronise the quotes for the rest of the file."""
    out: list[tuple[int, int, str]] = []
    code = list(text)
    n, i = len(text), 0

    def blank(a: int, b: int) -> None:
        for k in range(a, b):
            if code[k] != "\n":
                code[k] = " "

    while i < n:
        if text.startswith("//", i):
            j = text.find("\n", i)
            j = n if j < 0 else j
            blank(i, j); i = j
        elif text.startswith("/*", i):
            j, depth = i + 2, 1
            while j < n and depth:
                if text.startswith("/*", j): depth += 1; j += 2
                elif text.startswith("*/", j): depth -= 1; j += 2
                else: j += 1
            blank(i, j); i = j
        elif (raw := RAW.match(text, i)) and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_")):
            close = '"' + raw.group(1)
            j = text.find(close, raw.end())
            j = n if j < 0 else j
            out.append((i, j + len(close), text[raw.end():j]))
            blank(i, j + len(close)); i = j + len(close)
        elif text[i] == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            out.append((i, j + 1, text[i + 1:j]))
            blank(i, j + 1); i = j + 1
        elif char := CHAR.match(text, i):
            blank(i, char.end()); i = char.end()
        else:
            i += 1
    clean = "".join(code)
    tests = []
    for match in re.finditer(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]\s*mod\b", clean):
        opening = clean.find("{", match.end())
        if opening < 0: continue
        depth, j = 1, opening + 1
        while j < len(clean) and depth:
            if clean[j] == "{": depth += 1
            elif clean[j] == "}": depth -= 1
            j += 1
        tests.append((match.start(), j))
    return [lit for lit in out if not any(a <= lit[0] < b for a, b in tests)]


# Ukrainian literal that becomes a UI string without tr(): `"…".into()`, `.to_string()`,
# `.to_owned()`, `String::from("…")`, `SharedString::from("…")`, `.child("…")`. The key
# exists, so the old check passed, yet the English UI showed Ukrainian text.
BYPASS_AFTER = re.compile(r"\s*\.\s*(into|to_string|to_owned)\s*\(\s*\)")
BYPASS_BEFORE = re.compile(r"(\.child|(?<![\w:])(String|SharedString)\s*::\s*from)\s*\(\s*$")
TRANSLATED_BEFORE = re.compile(r"(\btr|\btranslated|\btr_format!)\s*\(\s*$")
# Source keys translated where they are shown, or texts that are not UI.
BYPASS_ALLOWED = {
    # CloudState::phase stores the Ukrainian source; every view shows tr(&phase).
    "Підключення до Bambu Cloud…", "Підключено", "Хмару вимкнено (SLICENV_CLOUD_OFF)",
    "Запуск…", "Хмара недоступна",
    "Відкритий плагін: хмару вимкнено, друк — по локальній мережі",
    # packages.status — shown as tr(&status).
    "Операцію не завершено",
    "Скасовано",
    # migration Config::label — shown as scope_label() = tr(label).
    "Проєкт",
    # SliceJob label — shown as tr(&title).
    "Нарізка…",
    # ui::ghost_button / accent_button translate their label.
    "Нарізати",
    # Error detail inside an anyhow! message and a log line.
    "unsquashfs недоступний", "тему не знайдено",
}


def bypasses(text: str, values: list[tuple[int, int, str]]) -> list[tuple[int, str]]:
    found = []
    for start, end, value in values:
        if not CYRILLIC.search(value) or value in BYPASS_ALLOWED:
            continue
        before = text[max(0, start - 80):start]
        if TRANSLATED_BEFORE.search(before):
            continue
        if BYPASS_AFTER.match(text, end) or BYPASS_BEFORE.search(before):
            found.append((text.count("\n", 0, start) + 1, value))
    return found


def main() -> int:
    catalog = json.loads(CATALOG.read_text(encoding="utf-8"))
    missing: set[tuple[Path, str]] = set()
    untranslated: set[tuple[Path, str]] = set()
    for source in SOURCES:
        text = source.read_text(encoding="utf-8")
        values = literals(text)
        if source.is_relative_to(APP):
            for line, key in bypasses(text, values):
                untranslated.add((source.relative_to(ROOT), f"{line}: {key}"))
        for start, _, key in values:
            # These are logger payloads, test-fixture labels, or interpolation
            # arguments; they are not displayed as independently translated text.
            if key.startswith(("[cloud]", "[lan]", "[klipper]")) or key in {
                "сопла", "столу", "Тестовий A1 (фіктивний)",
                "Потік хмари завершився з помилкою під час закриття",
                "не стартував потік хмари",
            }:
                continue
            if CYRILLIC.search(key) and key not in catalog:
                line = text.count("\n", 0, start) + 1
                missing.add((source.relative_to(ROOT), f"{line}: {key}"))
    if untranslated:
        for source, entry in sorted(untranslated):
            print(f"{source}:{entry}")
        print(f"{len(untranslated)} Ukrainian literal(s) reach the UI without tr()/tr_format!")
    if missing:
        for source, entry in sorted(missing):
            print(f"{source}:{entry}")
        print(f"Missing {len(missing)} literal(s) from {CATALOG.relative_to(ROOT)}")
    if missing or untranslated:
        return 1
    print(f"All scanned Cyrillic literals are present in {CATALOG.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
