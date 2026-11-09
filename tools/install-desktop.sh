#!/usr/bin/env bash
# Іконка й ярлик Slicenv для поточного користувача (GNOME/KDE шукають іконку вікна за
# app_id `app.borys.slicenv` через однойменний .desktop).
#   tools/install-desktop.sh [ШЛЯХ_ДО_БІНАРНИКА]   (типово target/release/slicenv)
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN=$(realpath "${1:-$ROOT/target/release/slicenv}")
[[ -x $BIN ]] || { echo "немає бінарника $BIN (cargo build --release)" >&2; exit 1; }
DATA=${XDG_DATA_HOME:-$HOME/.local/share}
SRC=$ROOT/crates/app/assets/app
install -Dm644 "$SRC/slicenv.svg" "$DATA/icons/hicolor/scalable/apps/app.borys.slicenv.svg"
for s in 16 24 32 48 64 128 256 512; do
  dir=$DATA/icons/hicolor/${s}x${s}/apps
  mkdir -p "$dir"
  rsvg-convert -w "$s" -h "$s" "$SRC/slicenv.svg" -o "$dir/app.borys.slicenv.png"
done
sed "s|@EXEC@|$BIN|" "$SRC/app.borys.slicenv.desktop" >"$DATA/applications/app.borys.slicenv.desktop"
gtk-update-icon-cache -q -t "$DATA/icons/hicolor" 2>/dev/null || true
update-desktop-database -q "$DATA/applications" 2>/dev/null || true
echo "встановлено: $DATA/applications/app.borys.slicenv.desktop → $BIN"
