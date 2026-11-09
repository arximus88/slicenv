#!/usr/bin/env bash
# GUI-бенч: орбіта й перетягування сфери 1 М △ у Xvfb, spans SLICENV_PROFILE_ALL → бюджети.
#
#   tools/bench/gui-bench.sh [--binary PATH] [--update-baseline] [--keep] [--preview GCODE]
#
# --preview GCODE: замість сфери відкрити нарізаний G-code (сторінка «Перегляд») і
# виміряти лише орбіту над траєкторією (фаза preview_orbit). Файл копіюється в тимчасову теку.
#
# Потрібні: Xvfb, xdotool, python3; ImageMagick `import` — для знімків (необов’язково).
# Застосунок бачить лише тимчасові XDG_CONFIG/DATA/CACHE/STATE_HOME: конфіг користувача
# (~/.config/slicenv або ~/.config/bambu-lab, без mcp-token) копіюється в тимчасовий
# config/slicenv, тож міграція bambu-lab → slicenv ніколи не торкається справжніх тек.
# Зупиняємо лише власні процеси за PID — не `pkill -f` і не чужі slicenv/Xvfb.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
BIN=$TARGET/release/slicenv
UPDATE=()
KEEP=0
PREVIEW=
while [[ $# -gt 0 ]]; do
  case $1 in
    --binary) BIN=$2; shift 2 ;;
    --update-baseline) UPDATE=(--update-baseline); shift ;;
    --keep) KEEP=1; shift ;;
    --preview) PREVIEW=$2; shift 2 ;;
    -h|--help) sed -n '2,13p' "$0"; exit 0 ;;
    *) echo "невідомий аргумент: $1" >&2; exit 2 ;;
  esac
done
# Точки жестів у вікні 1600×1000: сфера в центрі пластини після відкриття; орбіта — з
# порожнього місця над столом (див. docs/TESTING.md, як перевірити знімком).
DRAG_XY=${DRAG_XY:-"790 540"}
ORBIT_XY=${ORBIT_XY:-"450 300"}
SETTLE_TIMEOUT=${SETTLE_TIMEOUT:-90}

for tool in Xvfb xdotool python3; do
  command -v "$tool" >/dev/null || { echo "немає $tool" >&2; exit 2; }
done
[[ -x $BIN ]] || { echo "немає $BIN — зберіть: cargo build --release -p app" >&2; exit 2; }

# Справжні теки продукту до запуску: після — ті самі (перевіряється наприкінці).
real_dirs() {
  local d
  for d in "${XDG_CONFIG_HOME:-$HOME/.config}" "${XDG_DATA_HOME:-$HOME/.local/share}" \
           "${XDG_CACHE_HOME:-$HOME/.cache}" "${XDG_STATE_HOME:-$HOME/.local/state}"; do
    for name in slicenv bambu-lab; do
      [[ -e $d/$name ]] && echo "$d/$name"
    done
  done
  return 0
}
BEFORE=$(real_dirs)

TMP=$(mktemp -d "${TMPDIR:-/tmp}/slicenv-gui-bench.XXXXXX")
APP_PID=
XVFB_PID=
cleanup() {
  [[ -n $APP_PID ]] && kill "$APP_PID" 2>/dev/null && wait "$APP_PID" 2>/dev/null || true
  [[ -n $XVFB_PID ]] && kill "$XVFB_PID" 2>/dev/null && wait "$XVFB_PID" 2>/dev/null || true
  if [[ $KEEP == 1 ]]; then echo "тимчасові файли: $TMP"; else rm -rf "$TMP"; fi
}
trap cleanup EXIT

mkdir -p "$TMP"/{config,data,cache,state,out}
SRC_CONFIG=
for candidate in "${XDG_CONFIG_HOME:-$HOME/.config}/slicenv" "${XDG_CONFIG_HOME:-$HOME/.config}/bambu-lab"; do
  [[ -d $candidate ]] && { SRC_CONFIG=$candidate; break; }
done
mkdir -p "$TMP/config/slicenv"
if [[ -n $SRC_CONFIG ]]; then
  cp -r "$SRC_CONFIG"/. "$TMP/config/slicenv/"
  rm -f "$TMP/config/slicenv/mcp-token"
fi
# Детермінований жест: ліва кнопка на об’єкті тягне його; MCP-сервер не стартує.
python3 -I - "$TMP/config/slicenv/preferences.json" <<'PY'
import json, sys
path = sys.argv[1]
try:
    prefs = json.load(open(path))
except (FileNotFoundError, ValueError):
    prefs = {}
prefs.update(left_drag_moves=True, mcp_enabled=False)
json.dump(prefs, open(path, 'w'))
PY

if [[ -n $PREVIEW ]]; then
  [[ -f $PREVIEW ]] || { echo "немає $PREVIEW" >&2; exit 2; }
  MODEL=$TMP/preview.gcode
  cp "$PREVIEW" "$MODEL"
else
  MODEL=$TMP/sphere-1m.stl
  python3 -I "$ROOT/tools/bench/sphere_stl.py" "$MODEL"
fi

DISPLAY_NUM=
for n in $(seq 93 120); do
  [[ -e /tmp/.X11-unix/X$n || -e /tmp/.X$n-lock ]] || { DISPLAY_NUM=$n; break; }
done
[[ -n $DISPLAY_NUM ]] || { echo "немає вільного X-дисплея" >&2; exit 2; }
Xvfb ":$DISPLAY_NUM" -screen 0 1600x1000x24 -nolisten tcp >/dev/null 2>&1 &
XVFB_PID=$!
for _ in $(seq 50); do [[ -e /tmp/.X11-unix/X$DISPLAY_NUM ]] && break; sleep 0.1; done
export DISPLAY=:$DISPLAY_NUM

LOG=$TMP/app.log
env -u WAYLAND_DISPLAY -u SLICENV_UI_TRACE -u BAMBU_UI_TRACE \
  DISPLAY=":$DISPLAY_NUM" SLICENV_CLOUD_OFF=1 SLICENV_PROFILE=1 SLICENV_PROFILE_ALL=1 \
  XDG_CONFIG_HOME="$TMP/config" XDG_DATA_HOME="$TMP/data" \
  XDG_CACHE_HOME="$TMP/cache" XDG_STATE_HOME="$TMP/state" \
  "$BIN" "$MODEL" >"$LOG" 2>&1 &
APP_PID=$!

lines() { wc -l <"$LOG"; }
# Готовність: сцена намальована (є viewport::frame) і лог мовчить 3 с — без анімацій
# застосунок у спокої не малює.
deadline=$((SECONDS + SETTLE_TIMEOUT))
until grep -q 'SLICENV_PROFILE viewport::frame' "$LOG" 2>/dev/null; do
  kill -0 "$APP_PID" 2>/dev/null || { echo "застосунок завершився:"; tail -20 "$LOG"; exit 2; }
  ((SECONDS < deadline)) || { echo "немає першого кадру за ${SETTLE_TIMEOUT} с" >&2; exit 2; }
  sleep 0.5
done
settle() {
  local last now quiet=0 until=$((SECONDS + 30))
  last=$(lines)
  while ((quiet < 6 && SECONDS < until)); do
    sleep 0.5
    now=$(lines)
    if [[ $now == "$last" ]]; then quiet=$((quiet + 1)); else quiet=0; last=$now; fi
  done
}
settle
WINDOW=$(xdotool search --onlyvisible --pid "$APP_PID" | head -1)
[[ -n $WINDOW ]] && xdotool windowfocus "$WINDOW" 2>/dev/null || true
shot() { command -v import >/dev/null && import -window root "$TMP/out/$1.png" 2>/dev/null || true; }
shot start

# phase NAME X Y STEPS DX DY DELAY
phase() {
  local name=$1 x=$2 y=$3 steps=$4 dx=$5 dy=$6 delay=$7 a b
  xdotool mousemove "$x" "$y"; sleep 0.3
  settle
  a=$(lines)
  xdotool mousedown 1
  for _ in $(seq "$steps"); do xdotool mousemove_relative -- "$dx" "$dy"; sleep "$delay"; done
  sleep 0.2
  xdotool mouseup 1
  sleep 1
  settle
  b=$(lines)
  sed -n "$((a + 1)),${b}p" "$LOG" | grep '^SLICENV_PROFILE ' >"$TMP/out/$name.log" || true
  shot "$name"
}
status=0
if [[ -n $PREVIEW ]]; then
  # «Перегляд»: лівий жест будь-де у в’юпорті обертає камеру — об’єктів для захоплення немає.
  read -r ox oy <<<"${PREVIEW_ORBIT_XY:-790 540}"
  phase preview_orbit "$ox" "$oy" 120 4 1 "${PREVIEW_ORBIT_DELAY:-0.025}"
  python3 -I "$ROOT/tools/bench/stats.py" --baselines "$ROOT/bench/baselines.json" "${UPDATE[@]}" \
    --json "$TMP/out/summary.json" preview_orbit="$TMP/out/preview_orbit.log" || status=$?
else
  # Спершу перетягування (сфера там, де її поставило відкриття), потім орбіта з порожнього місця.
  read -r dx dy <<<"$DRAG_XY"
  phase drag "$dx" "$dy" 80 3 1 0.03
  read -r ox oy <<<"$ORBIT_XY"
  phase orbit "$ox" "$oy" 120 4 1 0.025
  python3 -I "$ROOT/tools/bench/stats.py" --baselines "$ROOT/bench/baselines.json" "${UPDATE[@]}" \
    --json "$TMP/out/summary.json" orbit="$TMP/out/orbit.log" drag="$TMP/out/drag.log" || status=$?
fi

kill "$APP_PID" 2>/dev/null; wait "$APP_PID" 2>/dev/null || true; APP_PID=
AFTER=$(real_dirs)
if [[ $BEFORE != "$AFTER" ]]; then
  echo "УВАГА: змінились справжні теки продукту:" >&2
  diff <(echo "$BEFORE") <(echo "$AFTER") >&2 || true
  status=3
fi
exit "$status"
