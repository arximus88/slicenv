#!/usr/bin/env bash
# Генерує packaging/flatpak/cargo-sources.json — усі крейти з Cargo.lock як джерела
# flatpak-builder, щоб cargo збирав без мережі (вимога Flathub).
#   tools/flatpak-cargo-sources.sh [вихідний файл]
# Генератор — flatpak-cargo-generator.py з flatpak-builder-tools (фіксований коміт,
# клонується в тимчасову теку). Залежності Python ставить `uv run` в ізольоване оточення.
# Після кожної зміни Cargo.lock файл треба перегенерувати (і в репозиторії Flathub теж).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=${1:-$ROOT/packaging/flatpak/cargo-sources.json}
# flatpak-builder-tools, 2026-10-25.
FBT_COMMIT=9d86d336a39b9af38b86f114b6ce5820fe260f91

command -v uv >/dev/null || { echo "потрібен uv (https://docs.astral.sh/uv/)" >&2; exit 1; }
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
git -C "$tmp" init -q fbt
git -C "$tmp/fbt" fetch -q --depth 1 https://github.com/flatpak/flatpak-builder-tools.git "$FBT_COMMIT"
git -C "$tmp/fbt" checkout -q FETCH_HEAD
uv run --no-project --with 'aiohttp>=3.9.5,<4' --with 'tomlkit>=0.13.3,<1' \
  python -I "$tmp/fbt/cargo/flatpak-cargo-generator.py" "$ROOT/Cargo.lock" -o "$OUT"
echo "→ $OUT" >&2
