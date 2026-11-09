#!/usr/bin/env python3
"""Копія Flathub-маніфесту, де джерело slicenv — локальне дерево (type: dir), а не git-тег.

Для локальної перевірки (tools/package.sh flatpak) і CI (package.yml, job flatpak):
збірка та сама, що на Flathub, але з поточного коду. Решта маніфесту не змінюється.
  python3 -I tools/flatpak-local-manifest.py <маніфест> <вихід> <корінь дерева>
"""
import re
import sys

src, dst, root = sys.argv[1:]
text = open(src, encoding="utf-8").read()
git = re.compile(r"      - type: git\n        url: https://github.com/arximus88/slicenv.git\n(?:        .*\n)+")
local = f"      - type: dir\n        path: {root}\n        skip: [target, dist, .git, .flatpak-builder]\n"
text, n = git.subn(local, text)
if n != 1:
    sys.exit("не знайдено git-джерело slicenv у маніфесті")
open(dst, "w", encoding="utf-8").write(text)
