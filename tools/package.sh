#!/usr/bin/env bash
# Локальна збірка пакетів Slicenv у dist/. Нічого не публікує й нічого не встановлює.
#   tools/package.sh <tar|appimage|deb|rpm|aur-bin|aur-git|srcinfo|flatpak|all>…
# Змінні:
#   SLICENV_SKIP_BUILD=1     не запускати cargo build (взяти готові бінарники)
#   CARGO_TARGET_DIR         де лежать бінарники (типово ./target)
#   DIST                     каталог результатів (типово ./dist)
#   SLICENV_APPIMAGE_RUNTIME готовий type2-runtime для appimagetool (інакше — див. appimage_runtime)
# Формати й обмеження — packaging/README.md.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DIST=${DIST:-$ROOT/dist}
TARGET_DIR=${CARGO_TARGET_DIR:-$ROOT/target}
PKG=$ROOT/packaging
APP_ID=app.borys.slicenv
NAME=slicenv
VERSION=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml")
ARCH=x86_64
STAGE=$DIST/stage
TARBASE=$NAME-$VERSION-$ARCH-linux
ASSETS=$ROOT/crates/app/assets/app

log() { printf '\033[1m==> %s\033[0m\n' "$*" >&2; }
die() { printf 'помилка: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

build_bins() {
  if [[ -z ${SLICENV_SKIP_BUILD:-} ]]; then
    log "cargo build --release (slicenv, slicenv-mcp)"
    (cd "$ROOT" && cargo build --release --locked -p app -p mcp)
  fi
  for b in slicenv slicenv-mcp; do
    [[ -x $TARGET_DIR/release/$b ]] || die "немає $TARGET_DIR/release/$b"
  done
}

render_png() { # svg size out
  if have rsvg-convert; then rsvg-convert -w "$2" -h "$2" "$1" -o "$3"
  elif have magick; then magick -background none -density 384 "$1" -resize "$2x$2" "$3"
  else die "потрібен rsvg-convert або ImageMagick для PNG-іконок"; fi
}

# Спільне дерево /usr для всіх форматів.
stage() {
  [[ -f $STAGE/.done ]] && return
  build_bins
  log "stage → $STAGE"
  rm -rf "$STAGE"
  local u=$STAGE/usr
  install -Dm755 "$TARGET_DIR/release/slicenv" "$u/bin/slicenv"
  install -Dm755 "$TARGET_DIR/release/slicenv-mcp" "$u/bin/slicenv-mcp"
  sed 's|@EXEC@|slicenv|' "$ASSETS/$APP_ID.desktop" >"$DIST/$APP_ID.desktop"
  install -Dm644 "$DIST/$APP_ID.desktop" "$u/share/applications/$APP_ID.desktop"
  install -Dm644 "$ASSETS/$APP_ID.metainfo.xml" "$u/share/metainfo/$APP_ID.metainfo.xml"
  install -Dm644 "$ASSETS/slicenv.svg" "$u/share/icons/hicolor/scalable/apps/$APP_ID.svg"
  for s in 16 24 32 48 64 128 256 512; do
    mkdir -p "$u/share/icons/hicolor/${s}x$s/apps"
    render_png "$ASSETS/slicenv.svg" "$s" "$u/share/icons/hicolor/${s}x$s/apps/$APP_ID.png"
  done
  local doc=$u/share/doc/$NAME
  install -Dm644 "$ROOT/LICENSE" "$doc/LICENSE"
  install -Dm644 "$ROOT/THIRD-PARTY-NOTICES.md" "$doc/THIRD-PARTY-NOTICES.md"
  install -Dm644 "$PKG/THIRD-PARTY-RUST-CRATES.md" "$doc/THIRD-PARTY-RUST-CRATES.md"
  install -Dm644 "$ROOT/README.md" "$doc/README.md"
  (cd "$ROOT" && python3 -I tools/third-party-licenses.py --texts "$doc/THIRD-PARTY-LICENSES.txt")
  touch "$STAGE/.done"
}

validate_meta() {
  if have desktop-file-validate; then
    desktop-file-validate "$STAGE/usr/share/applications/$APP_ID.desktop"
  fi
  if have appstreamcli; then
    # Код 3 = лише попередження (немає homepage, поки репозиторій приватний) — не провал.
    appstreamcli validate --no-net "$STAGE/usr/share/metainfo/$APP_ID.metainfo.xml" || [[ $? == 3 ]]
  fi
}

fmt_tar() {
  stage; validate_meta
  local out=$DIST/$TARBASE.tar.gz tmp
  log "tar.gz → $out"
  tmp=$(mktemp -d)
  mkdir "$tmp/$TARBASE"
  cp -a "$STAGE/usr" "$tmp/$TARBASE/"
  cp "$ROOT/LICENSE" "$tmp/$TARBASE/"
  tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@${SOURCE_DATE_EPOCH:-$(git -C "$ROOT" log -1 --format=%ct)}" \
    -C "$tmp" -czf "$out" "$TARBASE"
  rm -rf "$tmp"
  (cd "$DIST" && sha256sum "$TARBASE.tar.gz" >"$TARBASE.tar.gz.sha256")
}

# type2-runtime для appimagetool без мережі: беремо ELF-рантайм із самого linuxdeploy
# (він теж type2 AppImage; рантайм — байти до --appimage-offset). Інакше appimagetool
# сам завантажить runtime з GitHub.
appimage_runtime() {
  if [[ -n ${SLICENV_APPIMAGE_RUNTIME:-} ]]; then echo "$SLICENV_APPIMAGE_RUNTIME"; return; fi
  local rt=$DIST/cache/runtime-$ARCH ld off
  if [[ ! -s $rt ]] && ld=$(command -v linuxdeploy) && off=$("$ld" --appimage-offset 2>/dev/null) \
    && [[ $off =~ ^[0-9]+$ ]]; then
    mkdir -p "$DIST/cache"
    head -c "$off" "$ld" >"$rt"
  fi
  [[ -s $rt ]] && echo "$rt"
}

fmt_appimage() {
  stage; validate_meta
  have linuxdeploy || die "немає linuxdeploy"
  have appimagetool || die "немає appimagetool"
  local appdir=$DIST/AppDir out=$DIST/Slicenv-$VERSION-$ARCH.AppImage rt
  log "AppImage → $out"
  rm -rf "$appdir"
  cp -a "$STAGE" "$appdir"
  rm -f "$appdir/.done"
  # libstdc++/libgcc_s не бандлимо: закритий плагін Bambu, завантажений dlopen, мусить
  # отримати хостову (новішу) libstdc++, інакше впаде на GLIBCXX_*.
  ARCH=$ARCH NO_STRIP=1 linuxdeploy --appdir "$appdir" \
    -e "$appdir/usr/bin/slicenv" -e "$appdir/usr/bin/slicenv-mcp" \
    -d "$appdir/usr/share/applications/$APP_ID.desktop" \
    -i "$appdir/usr/share/icons/hicolor/256x256/apps/$APP_ID.png" \
    --custom-apprun "$PKG/appimage/AppRun" \
    --exclude-library 'libstdc++.so*' --exclude-library 'libgcc_s.so*' >"$DIST/linuxdeploy.log" 2>&1 \
    || { tail -30 "$DIST/linuxdeploy.log"; die "linuxdeploy"; }
  local args=()
  rt=$(appimage_runtime) && args+=(--runtime-file "$rt")
  ARCH=$ARCH appimagetool "${args[@]}" --no-appstream "$appdir" "$out" >"$DIST/appimagetool.log" 2>&1 \
    || { tail -30 "$DIST/appimagetool.log"; die "appimagetool"; }
  chmod +x "$out"
  (cd "$DIST" && sha256sum "$(basename "$out")" >"$(basename "$out").sha256")
}

# .deb без dpkg: формат — ar-архів (debian-binary, control.tar.xz, data.tar.xz).
# Якщо є dpkg-deb — використовуємо його (він ще й перевіряє control).
deb_depends() {
  local glibc
  glibc=$(objdump -T "$STAGE/usr/bin/slicenv" 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -Vu | tail -1)
  printf 'libc6 (>= %s), libgcc-s1, libstdc++6, libxkbcommon0, libxkbcommon-x11-0, libxcb1, libxcb-xkb1, libwayland-client0, libvulkan1, mesa-vulkan-drivers | vulkan-icd, python3 (>= 3.10), curl, tar, xdg-utils' "${glibc:-2.35}"
}

fmt_deb() {
  stage
  local debarch=amd64 ver=$VERSION-1 work=$DIST/deb out
  out=$DIST/${NAME}_${ver}_$debarch.deb
  log ".deb → $out"
  rm -rf "$work"; mkdir -p "$work/root/DEBIAN"
  cp -a "$STAGE/usr" "$work/root/"
  install -Dm644 "$ROOT/LICENSE" "$work/root/usr/share/doc/$NAME/copyright"
  local size
  size=$(du -sk --apparent-size "$work/root/usr" | cut -f1)
  sed -e "s#@VERSION@#$ver#" -e "s#@ARCH@#$debarch#" -e "s#@SIZE@#$size#" -e "s#@DEPENDS@#$(deb_depends)#" \
    "$PKG/deb/control.in" >"$work/root/DEBIAN/control"
  (cd "$work/root" && find usr -type f -print0 | sort -z | xargs -0 md5sum >DEBIAN/md5sums)
  if have dpkg-deb; then
    dpkg-deb --root-owner-group -Zxz --build "$work/root" "$out"
  else
    local mt
    mt=${SOURCE_DATE_EPOCH:-$(git -C "$ROOT" log -1 --format=%ct)}
    local tarflags=(--sort=name --owner=0 --group=0 --numeric-owner --mtime="@$mt" -cJf)
    printf '2.0\n' >"$work/debian-binary"
    tar "${tarflags[@]}" "$work/control.tar.xz" -C "$work/root/DEBIAN" ./control ./md5sums
    tar "${tarflags[@]}" "$work/data.tar.xz" -C "$work/root" ./usr
    rm -f "$out"
    (cd "$work" && ar rcD "$out" debian-binary control.tar.xz data.tar.xz)
  fi
}

fmt_rpm() {
  stage
  have rpmbuild || { log "rpmbuild не знайдено — .rpm пропущено (конфіг: packaging/rpm/slicenv.spec)"; return; }
  local top=$DIST/rpmbuild
  log ".rpm → $DIST"
  rm -rf "$top"; mkdir -p "$top"/{BUILD,RPMS,SOURCES,SPECS,SRPMS}
  rpmbuild -bb --target "$ARCH" \
    --define "_topdir $top" --define "stagedir $STAGE" --define "pkgversion $VERSION" \
    --define "_build_id_links none" \
    "$PKG/rpm/slicenv.spec" >"$DIST/rpmbuild.log" 2>&1 || { tail -30 "$DIST/rpmbuild.log"; die "rpmbuild"; }
  find "$top/RPMS" -name '*.rpm' -exec cp {} "$DIST/" \;
}

# Локальна збірка AUR-пакетів: PKGBUILD копіюється в dist/aur/*, джерело підмінюється
# на локальний tarball / file:// клон. Пакет лише збирається (makepkg без -i).
makepkg_local() { # dir
  (cd "$1" && PKGDEST="$DIST" SRCDEST="$1/src-cache" BUILDDIR="$1/build" \
    makepkg -f --nodeps --noconfirm --skippgpcheck >"$1/makepkg.log" 2>&1) \
    || { tail -40 "$1/makepkg.log"; die "makepkg у $1"; }
}

fmt_aur_bin() {
  have makepkg || { log "makepkg не знайдено — AUR пропущено"; return; }
  [[ -f $DIST/$TARBASE.tar.gz ]] || fmt_tar
  local dir=$DIST/aur/slicenv-bin sum
  log "AUR slicenv-bin → $DIST"
  rm -rf "$dir"; mkdir -p "$dir"
  cp "$DIST/$TARBASE.tar.gz" "$dir/"
  sum=$(cut -d' ' -f1 "$DIST/$TARBASE.tar.gz.sha256")
  sed -e "s|^source_x86_64=.*|source_x86_64=(\"$TARBASE.tar.gz\")|" \
      -e "s|^sha256sums_x86_64=.*|sha256sums_x86_64=('$sum')|" \
      -e "s|^pkgver=.*|pkgver=$VERSION|" \
      "$PKG/aur/slicenv-bin/PKGBUILD" >"$dir/PKGBUILD"
  makepkg_local "$dir"
}

fmt_aur_git() {
  have makepkg || { log "makepkg не знайдено — AUR пропущено"; return; }
  local dir=$DIST/aur/slicenv-git branch
  branch=$(git -C "$ROOT" branch --show-current)
  log "AUR slicenv-git (file://$ROOT#branch=$branch) → $DIST"
  [[ -z $(git -C "$ROOT" status --porcelain --untracked-files=no) ]] \
    || log "увага: незакомічені зміни не потрапляють у -git пакет"
  rm -rf "$dir"; mkdir -p "$dir"
  sed -e "s|^source=(\"slicenv::git+[^\"]*\")|source=(\"slicenv::git+file://$ROOT#branch=$branch\")|" \
    "$PKG/aur/slicenv-git/PKGBUILD" >"$dir/PKGBUILD"
  grep -q "git+file://" "$dir/PKGBUILD" || die "не вдалося підмінити source у slicenv-git"
  # Окремий target: makepkg.conf додає власні RUSTFLAGS (CachyOS: target-cpu=native), і
  # спільний target/ перезбирався б щоразу. Після збірки проміжні файли прибираються.
  CARGO_TARGET_DIR=$dir/target makepkg_local "$dir"
  [[ -n ${SLICENV_KEEP_BUILD:-} ]] || rm -rf "$dir/target" "$dir/build" "$dir/src-cache"
}

# .SRCINFO для комітнутих PKGBUILD (з публічними URL, не з локальними підмінами).
fmt_srcinfo() {
  have makepkg || { log "makepkg не знайдено — .SRCINFO пропущено"; return; }
  for p in slicenv-bin slicenv-git; do
    (cd "$PKG/aur/$p" && makepkg --printsrcinfo >.SRCINFO)
  done
  log ".SRCINFO оновлено в packaging/aur/*"
}

# Flathub-маніфест, але джерело slicenv — робоче дерево (type: dir), а не git-тег.
# Збірка офлайн, як на Flathub: rust-stable з SDK, крейти з cargo-sources.json.
fmt_flatpak() {
  have flatpak-builder || { log "flatpak-builder не знайдено — Flatpak пропущено"; return; }
  local dir=$DIST/flatpak man=$PKG/flatpak/$APP_ID.yml sources=$PKG/flatpak/cargo-sources.json
  [[ -s $sources && $sources -nt $ROOT/Cargo.lock ]] || "$ROOT/tools/flatpak-cargo-sources.sh" "$sources"
  log "Flatpak (маніфест Flathub, джерело — робоче дерево) → $dir"
  rm -rf "$dir/build" "$dir/repo"; mkdir -p "$dir"
  cp "$sources" "$dir/cargo-sources.json"
  python3 -I "$ROOT/tools/flatpak-local-manifest.py" "$man" "$dir/$APP_ID.yml" "$ROOT"
  if flatpak-builder --user --disable-rofiles-fuse --force-clean --repo="$dir/repo" --state-dir="$dir/state" \
      --install-deps-from=flathub "$dir/build" "$dir/$APP_ID.yml" >"$dir/flatpak-builder.log" 2>&1; then
    flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo \
      "$dir/repo" "$DIST/Slicenv-$VERSION-$ARCH.flatpak" "$APP_ID" >>"$dir/flatpak-builder.log" 2>&1 \
      || { tail -20 "$dir/flatpak-builder.log"; log "flatpak build-bundle не вдався"; return; }
  else
    tail -20 "$dir/flatpak-builder.log"
    log "flatpak-builder не вдався (див. $dir/flatpak-builder.log) — Flatpak пропущено"
  fi
  [[ -n ${SLICENV_KEEP_BUILD:-} ]] || rm -rf "$dir/build" "$dir/state"
}

[[ $# -gt 0 ]] || set -- all
mkdir -p "$DIST"
for fmt in "$@"; do
  case $fmt in
    tar) fmt_tar ;;
    appimage) fmt_appimage ;;
    deb) fmt_deb ;;
    rpm) fmt_rpm ;;
    aur-bin) fmt_aur_bin ;;
    aur-git) fmt_aur_git ;;
    aur) fmt_aur_bin; fmt_aur_git ;;
    srcinfo) fmt_srcinfo ;;
    flatpak) fmt_flatpak ;;
    all) rm -f "$STAGE/.done"; fmt_tar; fmt_appimage; fmt_deb; fmt_rpm; fmt_aur_bin; fmt_aur_git; fmt_srcinfo; fmt_flatpak ;;
    *) die "невідомий формат $fmt (tar|appimage|deb|rpm|aur-bin|aur-git|aur|srcinfo|flatpak|all)" ;;
  esac
done
log "готово:"
find "$DIST" -maxdepth 1 -type f \( -name '*.tar.gz' -o -name '*.AppImage' -o -name '*.deb' -o -name '*.rpm' -o -name '*.pkg.tar.*' -o -name '*.flatpak' \) \
  -printf '%f\t%s\n' | sort | numfmt --field=2 --to=iec >&2
