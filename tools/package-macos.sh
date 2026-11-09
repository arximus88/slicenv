#!/usr/bin/env bash
# macOS: Slicenv.app → .zip і .dmg у dist/. Без підпису й нотаризації (див. packaging/README.md).
# Нічого не публікує й не встановлює. Запускається на macOS (CI: .github/workflows/package-cross.yml).
#   tools/package-macos.sh            # cargo build --release + пакування
#   SLICENV_SKIP_BUILD=1 tools/package-macos.sh
# Змінні: CARGO_TARGET_DIR, DIST, SLICENV_TARGET (напр. x86_64-apple-darwin; типово — хост).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DIST=${DIST:-$ROOT/dist}
TARGET_DIR=${CARGO_TARGET_DIR:-$ROOT/target}
TRIPLE=${SLICENV_TARGET:-}
VERSION=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml")
ASSETS=$ROOT/crates/app/assets/app

log() { printf '\033[1m==> %s\033[0m\n' "$*" >&2; }
die() { printf 'помилка: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

[[ $(uname -s) == Darwin ]] || die "лише macOS (потрібні iconutil, hdiutil)"

if [[ -n $TRIPLE ]]; then
  BIN=$TARGET_DIR/$TRIPLE/release
  ARCH=${TRIPLE%%-*}
else
  BIN=$TARGET_DIR/release
  ARCH=$(uname -m)
fi
[[ $ARCH == arm64 ]] && ARCH=aarch64

if [[ -z ${SLICENV_SKIP_BUILD:-} ]]; then
  log "cargo build --release (slicenv, slicenv-mcp)"
  (cd "$ROOT" && cargo build --release --locked -p app -p mcp ${TRIPLE:+--target "$TRIPLE"})
fi
for b in slicenv slicenv-mcp; do
  [[ -x $BIN/$b ]] || die "немає $BIN/$b"
done

APP=$DIST/macos/Slicenv.app
log "bundle → $APP"
rm -rf "$DIST/macos"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
install -m755 "$BIN/slicenv" "$APP/Contents/MacOS/slicenv"
install -m755 "$BIN/slicenv-mcp" "$APP/Contents/MacOS/slicenv-mcp"
sed "s|@VERSION@|$VERSION|g" "$ROOT/packaging/macos/Info.plist.in" >"$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist"
printf 'APPL????' >"$APP/Contents/PkgInfo"
for f in LICENSE THIRD-PARTY-NOTICES.md; do
  install -m644 "$ROOT/$f" "$APP/Contents/Resources/$f"
done

# Іконка: SVG → PNG (rsvg-convert з brew librsvg) → .iconset → .icns. Без конвертера —
# bundle без іконки (Finder покаже стандартну), але збірка не падає.
render_png() { # svg size out
  if have rsvg-convert; then rsvg-convert -w "$2" -h "$2" "$1" -o "$3"
  elif have magick; then magick -background none -density 384 "$1" -resize "$2x$2" "$3"
  else return 1; fi
}
ICONSET=$DIST/macos/slicenv.iconset
mkdir -p "$ICONSET"
icon_ok=1
for s in 16 32 128 256 512; do
  render_png "$ASSETS/slicenv.svg" "$s" "$ICONSET/icon_${s}x${s}.png" || { icon_ok=; break; }
  render_png "$ASSETS/slicenv.svg" $((s * 2)) "$ICONSET/icon_${s}x${s}@2x.png" || { icon_ok=; break; }
done
if [[ -n $icon_ok ]]; then
  iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/slicenv.icns"
else
  printf 'попередження: немає rsvg-convert/magick — .app без іконки\n' >&2
fi
rm -rf "$ICONSET"

# Ad-hoc підпис (`-`): не Developer ID і не нотаризація, але на Apple Silicon бінарник без
# жодного підпису не запуститься взагалі. Gatekeeper усе одно попередить про невідомого
# розробника — обхід описано в packaging/README.md.
codesign --force --deep --sign - "$APP"
codesign --verify --verbose "$APP"

BASE=slicenv-$VERSION-$ARCH-macos
log "zip → $DIST/$BASE.zip"
(cd "$DIST/macos" && ditto -c -k --sequesterRsrc --keepParent Slicenv.app "$DIST/$BASE.zip")

log "dmg → $DIST/$BASE.dmg"
DMG_SRC=$DIST/macos/dmg
mkdir -p "$DMG_SRC"
cp -R "$APP" "$DMG_SRC/"
ln -s /Applications "$DMG_SRC/Applications"
hdiutil create -volname "Slicenv $VERSION" -srcfolder "$DMG_SRC" -ov -format UDZO "$DIST/$BASE.dmg"
rm -rf "$DMG_SRC"

(cd "$DIST" && shasum -a 256 "$BASE.zip" >"$BASE.zip.sha256" && shasum -a 256 "$BASE.dmg" >"$BASE.dmg.sha256")
log "готово: $DIST/$BASE.{zip,dmg}"
