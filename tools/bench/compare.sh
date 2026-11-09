#!/usr/bin/env bash
# Порівняльний бенч Slicenv ↔ Bambu Studio ↔ OrcaSlicer (docs/BENCHMARK.md).
#
#   tools/bench/compare.sh [--quick] [-- аргументи compare.py]
#
# 1. Збирає release, якщо бінарника немає (CARGO_TARGET_DIR або target/).
# 2. tools/bench/compare.py: приватний headless mutter (Xwayland, апаратний GPU),
#    тимчасові HOME/XDG_* для кожного застосунку, семплінг /proc → bench/compare-<дата>.json.
# 3. tools/bench/compare_table.py: таблиці Markdown (медіана [min–max]) у stdout.
#
# --quick: по 1 прогону (перевірка, що стенд працює; числа не для звіту).
# Потрібні: mutter, dbus-run-session, xdotool, xprop, python3; ImageMagick `import` —
# для знімків (--shots). Рушії й оригінали — з ~/.local/share/slicenv/engines (лише читання)
# і /usr/bin/bambustudio. Справжні ~/.config/{slicenv,BambuStudio,OrcaSlicer} не відкриваються.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
BIN=$TARGET/release/slicenv
OUT=$ROOT/bench/compare-$(date +%F).json
EXTRA=()
while [[ $# -gt 0 ]]; do
  case $1 in
    --quick) EXTRA+=(--runs 1 --open-runs 1); shift ;;
    --) shift; EXTRA+=("$@"); break ;;
    -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
    *) echo "невідомий аргумент: $1" >&2; exit 2 ;;
  esac
done

for tool in mutter dbus-run-session xdotool xprop python3; do
  command -v "$tool" >/dev/null || { echo "немає $tool" >&2; exit 2; }
done
if [[ ! -x $BIN ]]; then
  (cd "$ROOT" && cargo build --release -p app)
fi
python3 -I "$ROOT/tools/bench/compare.py" --bin "$BIN" --out "$OUT" "${EXTRA[@]}"
python3 -I "$ROOT/tools/bench/compare_table.py" "$OUT"
