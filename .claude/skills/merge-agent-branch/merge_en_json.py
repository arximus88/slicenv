#!/usr/bin/env python3
"""Розв’язати конфлікт злиття crates/app/locales/en.json об’єднанням ключів.

Порядок: усі ключі ours (стадія :2:) у їхньому порядку, далі нові ключі theirs
(стадія :3:) у порядку theirs. Нічого не сортується. Якщо той самий ключ має різні
значення — лишається ours, а розбіжності друкуються (вихід 3, перевірити вручну).
Запуск з кореня репозиторію під час злиття: python3 -I <цей файл>
"""
import json
import subprocess
import sys

PATH = "crates/app/locales/en.json"


def stage(n: int) -> dict:
    text = subprocess.run(
        ["git", "show", f":{n}:{PATH}"], check=True, capture_output=True, text=True, encoding="utf-8"
    ).stdout
    return json.loads(text)


def main() -> int:
    ours, theirs = stage(2), stage(3)
    merged = dict(ours)
    added = differ = 0
    for key, value in theirs.items():
        if key not in merged:
            merged[key] = value
            added += 1
        elif merged[key] != value:
            differ += 1
            print(f"різні значення, лишено ours: {key!r}: {merged[key]!r} ≠ {value!r}")
    with open(PATH, "w", encoding="utf-8", newline="\n") as f:
        f.write(json.dumps(merged, ensure_ascii=False, indent=2) + "\n")
    print(f"{PATH}: ours {len(ours)}, додано з theirs {added}, разом {len(merged)}")
    return 3 if differ else 0


if __name__ == "__main__":
    sys.exit(main())
