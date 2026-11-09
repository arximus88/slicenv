# Пакування Slicenv

Linux — розділи нижче; macOS і Windows — розділ [«macOS і Windows»](#macos-і-windows).

App id — `app.borys.slicenv`. Бінарники — `slicenv` (GUI) і `slicenv-mcp` (MCP-сервер
stdio). Ліцензія — AGPL-3.0-or-later ([`LICENSE`](../LICENSE),
[`THIRD-PARTY-NOTICES.md`](../THIRD-PARTY-NOTICES.md)).

**Нічого звідси не публікується автоматично.** `tools/package.sh` і
`.github/workflows/package.yml` лише збирають артефакти; GitHub Release, AUR, Flathub,
Snap Store, COPR, PPA — ручні кроки з чек-листа внизу.

## Матриця каналів

Остання перевірка — 2026-11-09: tar.gz/AppImage/.deb/.rpm зібрано в контейнері `ubuntu:22.04`
(як CI, glibc 2.35), Flatpak — за Flathub-маніфестом; розділ [«Перевірка на дистрибутивах»](#перевірка-на-дистрибутивах).
Публікація — [docs/PUBLISHING.md](../docs/PUBLISHING.md).

| Канал | Файли | Стан | Примітка |
|---|---|---|---|
| tar.gz | `tools/package.sh tar` | ✅ CI; перевірено на Fedora 44 (GNOME 50) | `usr/` + LICENSE; основа для AUR `-bin` |
| AppImage | `packaging/appimage/AppRun`, `tools/package.sh appimage` | ✅ CI; перевірено на Fedora 44 (FUSE) і Ubuntu 22.04 (без FUSE) | без FUSE — `--appimage-extract-and-run`; `libxcb.so.1` очікується від системи |
| .deb | `packaging/deb/control.in`, `tools/package.sh deb` | ✅ CI; `apt install` на Ubuntu 22.04/24.04, Debian 13 | без dpkg-deb: ar + tar (див. нижче) |
| .rpm | `packaging/rpm/slicenv.spec`, `tools/package.sh rpm` | ✅ CI; `dnf`/`zypper` на Fedora 44, openSUSE Tumbleweed | rpmbuild з готового дерева |
| AUR `slicenv-bin` | `packaging/aur/slicenv-bin/` | ✅ makepkg + `pacman -U` у контейнері `archlinux` | у PKGBUILD — URL майбутнього релізу; локально підставляється tarball |
| AUR `slicenv-git` | `packaging/aur/slicenv-git/` | ✅ makepkg + `pacman -U` у контейнері `archlinux` | локально — `git+file://` робочого дерева |
| Flatpak (Flathub) | `packaging/flatpak/app.borys.slicenv.yml`, `tools/package.sh flatpak` | ✅ зібрано офлайн (GNOME 51 + rust-stable), `flatpak-builder-lint manifest` чистий; нарізка OrcaSlicer 2.5.0 і Bambu Studio 2.9 у пісочниці | `cargo-sources.json` — `tools/flatpak-cargo-sources.sh`; рушії й дозволи — розділ «Flatpak» |
| Snap | `packaging/snap/snapcraft.yaml` | ⚠️ лише конфіг | snapcraft не встановлено; classic confinement потребує схвалення |
| Nix | `flake.nix` | ✅ `nix build`/`nix run` на NixOS 26.05 | рушії OrcaSlicer/Bambu Studio і плагін Bambu у FHS — перевірено |
| CI | `.github/workflows/package.yml` | ✅ запускався (run 42422379868); job `flatpak` додано 2026-11-09, ще не запускався | `workflow_dispatch` і теги `v*`; лише `upload-artifact` |

## Збірка

```sh
tools/package.sh all                 # усе, що можна зібрати локально → dist/
tools/package.sh tar appimage        # вибірково: tar|appimage|deb|rpm|aur-bin|aur-git|aur|srcinfo|flatpak
SLICENV_SKIP_BUILD=1 tools/package.sh deb   # без cargo build, з наявного target/release
```

`dist/` у `.gitignore`. Скрипт нічого не встановлює в систему й не потребує sudo.
Спільне дерево `dist/stage/usr` містить бінарники, `.desktop`, іконки (SVG + PNG
16…512), metainfo, LICENSE, THIRD-PARTY-NOTICES, перелік крейтів і
`THIRD-PARTY-LICENSES.txt` — повні тексти ліцензій Rust-залежностей
(`tools/third-party-licenses.py`, без cargo-about).

Потрібно: Rust/cargo, `rsvg-convert` або ImageMagick, `python3`; для AppImage —
`linuxdeploy`, `appimagetool`; для rpm — `rpmbuild`; для AUR — `makepkg`; для
Flatpak — `flatpak-builder`, `org.gnome.Sdk//51`, `org.freedesktop.Sdk.Extension.rust-stable//26.08`, `uv`
(генератор `cargo-sources.json`). metainfo встановлюється в усі формати (`usr/share/metainfo`).

### Сумісність glibc

Бінарник прив’язаний до glibc системи збірки. Локальна збірка на CachyOS вимагає
**GLIBC_2.44** і не запуститься на Ubuntu/Debian/Fedora старших випусків. (Flatpak
збирається з джерел у своєму SDK, його це не стосується.) Тому `.deb` отримує `Depends: libc6 (>= <версія з
objdump>)`, а пакети для розповсюдження треба збирати в CI на `ubuntu-22.04`
(glibc 2.35) — так налаштовано `package.yml`.

### AppImage

- `linuxdeploy` кладе в `usr/lib` лише прямі залежності (libxkbcommon, libxcb-xkb,
  libXau, libXdmcp) і ставить RUNPATH `$ORIGIN/../lib`. `libstdc++`/`libgcc_s`
  свідомо **не** бандляться: плагін Bambu, підключений dlopen, мусить отримати хостову
  (новішу) libstdc++. Vulkan-loader, Mesa й libwayland теж хостові.
- `AppRun` зберігає початкові `PATH`, `LD_LIBRARY_PATH`, `XDG_DATA_DIRS`,
  `PYTHONHOME`… у `SLICENV_ORIG_*`. Для дочірніх процесів (рушії, `python3
  manage_engines.py`, `curl`, `tar`, `xdg-open`) `paths::host::command` прибирає
  `APPDIR`/`APPIMAGE`/`ARGV0`/`OWD` і відновлює ці змінні — рушії-AppImage не бачать
  нашого бандла. Перезапуск після майстра запускає сам `$APPIMAGE`.
- `Slicenv.AppImage mcp [--allow-edit]` запускає `slicenv-mcp` з того самого образу.
- Рантайм type2 береться з самого `linuxdeploy` (байти до `--appimage-offset`), тож
  appimagetool не ходить у мережу; інакше задайте `SLICENV_APPIMAGE_RUNTIME`.
- AppImage-рушії FUSE не потребують: менеджер рушіїв розпаковує їх (`unsquashfs` або
  `--appimage-extract`) у `~/.local/share/slicenv/engines`.

### .deb — чому ar + tar

Локально немає ні `cargo-deb`, ні `nfpm`, ні `dpkg-deb`; ставити їх глобально
заборонено умовою. Формат `.deb` — це `ar`-архів із трьох членів (`debian-binary`,
`control.tar.xz`, `data.tar.xz`), тож `tools/package.sh` збирає його `tar` + `ar rcD`
(детерміновано, власник 0:0). Якщо `dpkg-deb` є (як у CI на Ubuntu), використовується
він — заодно перевіряє `control`. Залежності: libc6 (від objdump), libxkbcommon,
libxcb, libwayland-client0, libvulkan1, python3, curl, tar, xdg-utils; рекомендовано
squashfs-tools.

### .rpm

`rpmbuild` є локально (пакет `rpm-tools` Arch), тож `slicenv.spec` пакує готове
дерево; залежності за sonames rpmbuild виводить сам. Vulkan-loader і libwayland-client
відкриваються dlopen, тож вони — явні `Requires` **за sonames** (`libvulkan.so.1()(64bit)`,
`libwayland-client.so.0()(64bit)`): назви пакетів різні у Fedora й openSUSE. Решта —
`python3`, `curl`, `tar`, `xdg-utils` (однакові назви). Для COPR потрібен окремий spec зі збіркою з джерел (`%cargo_build`,
офлайн-вендоринг крейтів і manifold3d) — не написано.

### AUR

- `slicenv-bin` розпаковує tar.gz релізу; `source_x86_64` вказує на
  `https://github.com/arximus88/slicenv/releases/download/v$pkgver/…` — такого
  релізу ще немає, `sha256sums` = `SKIP` до публікації.
- `slicenv-git` збирає `-p app -p mcp` з git; `manifold-csg-sys` під час збірки клонує
  manifold3d (потрібні мережа, cmake, C++17).
- `tools/package.sh aur-bin aur-git` копіює PKGBUILD у `dist/aur/*`, підміняє джерело
  на локальний tarball / `git+file://<робоче дерево>#branch=<поточна>` і запускає
  `makepkg -f --nodeps` (без `-i`). У `-git` потрапляє лише закомічене.
- `.SRCINFO` у `packaging/aur/*` генерує `tools/package.sh srcinfo` з комітнутих
  PKGBUILD (з публічними URL).
- `depends` — `libgcc` і `libstdc++` (Arch розділив `gcc-libs`). namcap далі попереджає
  «Dependency included, but may not be needed» про `wayland`, `vulkan-icd-loader`,
  `python`, `curl`, `tar`, `xdg-utils`: вони потрібні через dlopen і дочірні процеси, а
  namcap бачить лише DT_NEEDED. «ELF file is unstripped» — свідомо (`!strip`).
  «Unused shared library ld-linux-x86-64.so.2» — хибне: це інтерпретатор ELF.
- `pkgdesc` (обидва): «Fast native 3D printing slicer front-end for OrcaSlicer and Bambu
  Studio engines» (+ « (git)»). У джерелі `slicenv-bin` — `${CARCH}`, як радить namcap.
- **Оновлення `slicenv-bin`**: `pkgver`, `pkgrel=1`, `sha256sums_x86_64` — з
  `slicenv-<версія>-x86_64-linux.tar.gz.sha256` релізу, `tools/package.sh srcinfo`, push в AUR.
  Без нової версії (лише PKGBUILD) — `pkgrel+1`. Деталі — [docs/PUBLISHING.md](../docs/PUBLISHING.md).

## Flatpak

Один маніфест — `app.borys.slicenv.yml`, той самий для Flathub і для локальної перевірки:
- Flathub збирає з git-тегу (`tag` + `commit` у джерелі slicenv);
- `tools/package.sh flatpak` і job `flatpak` у CI підміняють це джерело на робоче дерево
  (`tools/flatpak-local-manifest.py`), решта — без змін.

Збірка офлайн, як вимагає Flathub: rust-stable з SDK, крейти — `cargo-sources.json`
(`tools/flatpak-cargo-sources.sh`, генератор flatpak-builder-tools з фіксованим комітом;
файл не в git, генерується з `Cargo.lock`), manifold3d + Clipper2 — окремі модулі з
фіксованими комітами, статичні бібліотеки → `MANIFOLD_CSG_LIB_DIR` + `MANIFOLD_CSG_LIB_KIND=static`
(версія manifold — та, що зашита в `manifold-csg-sys`: `MANIFOLD_VERSION` у його build.rs).

```sh
tools/package.sh flatpak                     # → dist/Slicenv-<версія>-x86_64.flatpak
SLICENV_KEEP_BUILD=1 tools/package.sh flatpak  # лишити dist/flatpak/state для швидкої перезбірки
flatpak run --command=flatpak-builder-lint org.flatpak.Builder manifest packaging/flatpak/app.borys.slicenv.yml
flatpak run --command=flatpak-builder-lint org.flatpak.Builder repo dist/flatpak/repo
```
Потрібні `org.gnome.Sdk//51`, `org.freedesktop.Sdk.Extension.rust-stable//26.08` і
`org.flatpak.Builder` (`flatpak install --user flathub …`), `uv` для генератора.

### Рантайм: GNOME 51, а не freedesktop

Розпаковані рушії (OrcaSlicer, Bambu Studio) слінковані з GTK 3 і **WebKitGTK 4.1**
(`libwebkit2gtk-4.1.so.0`, `libjavascriptcoregtk-4.1.so.0`) — навіть CLI не стартує без них
(DT_NEEDED). У `org.freedesktop.Platform` WebKitGTK немає, у `org.gnome.Platform` — є.
Перевірено `ldd` усередині рантаймів (2026-11-09):

| Рушій | freedesktop 25.08 | GNOME 50 | GNOME 51 |
|---|---|---|---|
| OrcaSlicer 2.4.2 | webkit2gtk, jsc, GLU, mspack | GLU, mspack | GLU, mspack |
| OrcaSlicer 2.5.0(-beta) | webkit2gtk, jsc, mspack | mspack | FFmpeg 7, mspack |
| Bambu Studio 02.07 | webkit2gtk, jsc | — | FFmpeg 7 |

(перелічено, чого бракує). `libmspack`/`libGLU` рушії мають у власній `lib/` (запуск через їхній
`AppRun` проходить). FFmpeg 7 (`libavcodec.so.61`, `libavformat.so.61`, `libavutil.so.59`,
`libswscale.so.8`) — модуль `ffmpeg7-libs` без жодного кодека: рушію потрібні лише символи
для завантаження, нарізка кодеків не використовує. GNOME 51 — найновіший рантайм на момент
подання (вимога Flathub), FFmpeg 8 рантайму має інші sonames, тож нічого не перекривається.

### Рушії в пісочниці — рішення

| Варіант | Рішення |
|---|---|
| Менеджер рушіїв Slicenv завантажує AppImage рушія | ✅ **основний шлях.** FUSE у пісочниці немає й не потрібен: менеджер розпаковує AppImage через `unsquashfs` (модуль `squashfs-tools` у маніфесті), запасний шлях — `--appimage-extract`. Рушій лягає в `~/.var/app/app.borys.slicenv/data/slicenv/engines` і запускається з бібліотеками GNOME 51 + `/app/lib`. |
| Рушій, встановлений на хості (пакет, AppImage, Flatpak) через `flatpak-spawn --host` | ❌ потребує `--talk-name=org.freedesktop.Flatpak` — це повний вихід із пісочниці. Linter Flathub дає лише попередження, але рецензенти дозволяють його хіба IDE/терміналам; для слайсера — ні. Код `flatpak-spawn` не використовує. |
| Чужий Flatpak рушія (`com.bambulab.BambuStudio`, OrcaSlicer) | ❌ його файли в `~/.var/app/<інший id>` — доступ туди linter Flathub забороняє (`finish-args-flatpak-appdata-folder-access`). |
| Профілі й сесії Bambu Studio / OrcaSlicer з хоста | ✅ лише читання: `--filesystem=xdg-config/BambuStudio:ro`, `xdg-config/OrcaSlicer:ro`; у Flatpak `paths::vendor_config_home()` бере хостовий корінь (`HOST_XDG_CONFIG_HOME`, інакше `~/.config`), а не `~/.var/app/…/config`. Профілі для нарізки беруться з каталогу самого рушія і з бібліотеки профілів Slicenv. |

Перевірено 2026-11-09 (бандл з цього маніфесту, `flatpak install --user`, тимчасовий HOME,
`--nofilesystem` на хостові теки): майстер першого запуску встановив OrcaSlicer 2.5.0 з
GitHub (SHA256, `unsquashfs`, 2496 файлів профілів), відкрито STL і 3MF з двома пластинами,
нарізка рушієм у пісочниці → G-code, Preview «Шари» і «Фото» на Radeon 780M (Vulkan,
Xwayland). Скриншоти для Flathub знято саме з цього бандла.

Обмеження:
1. **Плагін Bambu Cloud** (`libbambu_networking.so`, закритий) завантажується за згодою в
   дані застосунку й відкривається dlopen проти бібліотек рантайму — у Flatpak не перевірено.
   Друк через хмару однаково неможливий (-26), статус і камера — не перевірені.
2. **`slicenv-mcp` у Flatpak** (`flatpak run --command=slicenv-mcp app.borys.slicenv`) шукає
   профілі лише в теках Bambu Studio / OrcaSlicer (`PresetStore::open_default`), а не в
   бібліотеці профілів керованого рушія; без них `slice_plate` повертає «немає каталогу
   профілів BBL». Обхід — `SLICENV_PROFILE_ROOT`. Це обмеження MCP загалом, не лише Flatpak.
3. **Мережа** (`--share=network`) потрібна для завантаження рушіїв, LAN-принтерів
   (MQTT 8883, FTPS 990, камера 6000, SSDP — принтер шле на UDP 2021) і Bambu Cloud.
4. **Файли:** `xdg-documents` і `xdg-download` — для бібліотеки моделей на головній; решта
   файлів відкривається й зберігається через портал (gpui → xdg-desktop-portal).
5. **Flatpak поруч зі «звичайним» пакетом.** Обидва мають `app.borys.slicenv.desktop`;
   GNOME Shell не зіставляє вікно неізольованого процесу з ID Flatpak, тож вікно версії з
   tar.gz/rpm/AppImage отримує загальну іконку. Ставити щось одне.
6. **Видалення:** `flatpak uninstall --user --delete-data app.borys.slicenv`.

### flatpak-builder-lint (2026-11-09, org.flatpak.Builder stable)

| Що | Результат |
|---|---|
| `manifest packaging/flatpak/app.borys.slicenv.yml` | ✅ без зауважень |
| `repo dist/flatpak/repo` | ⚠️ `appstream-missing-screenshots`, `appstream-screenshots-not-mirrored-in-ostree` — скриншоти за URL `raw.githubusercontent.com/…/v0.1.0/…` недоступні, поки репозиторій приватний і тегу немає. Зникнуть після кроків 1–2 [docs/PUBLISHING.md](../docs/PUBLISHING.md); на інфраструктурі Flathub скриншоти дзеркаляться автоматично. |
| `appstream <metainfo>` | ⚠️ лише `url-not-reachable` / `screenshot-image-not-found` — та сама причина |
| `appstreamcli validate --pedantic --no-net` | ✅ |

## Snap

`snapcraft.yaml` — `base: core24`, плагін rust, **classic confinement**: рушії
запускаються з `$HOME`, плагін Bambu тягне бібліотеки хоста, профілі Bambu Studio лежать
у прихованому `~/.config/BambuStudio`, а strict-інтерфейс `home` прихованих каталогів не
дає. Classic потребує ручного схвалення Snap Store. Локально не зібрано: немає
snapcraft і LXD/Multipass.

## Nix

`flake.nix` (корінь репозиторію): `packages.unwrapped` — `rustPlatform.buildRustPackage`
з `cargoLock`, manifold3d з nixpkgs через `MANIFOLD_CSG_LIB_DIR` (у пісочниці Nix немає
мережі для build.rs), RUNPATH доповнено dlopen-бібліотеками (wayland, vulkan-loader,
libxkbcommon). `packages.default` — `buildFHSEnv`: рушії-AppImage й закритий плагін Bambu
очікують FHS (`/lib64/ld-linux-x86-64.so.2`, бібліотеки в `/usr/lib`), тож вони
запускаються в FHS-оточенні з GTK/WebKit/OpenSSL/zlib. Альтернатива на NixOS —
`programs.nix-ld.enable = true` з тим самим набором бібліотек і `.#unwrapped`.
`devShells.default` — cargo, clippy, rustfmt і системні бібліотеки.

Перевірено 2026-11-08 на NixOS 26.05 (nixpkgs nixos-unstable, manifold 3.5.4 з `manifoldc`):
`nix build`, `nix build .#unwrapped`, `nix run`, `nix run .#mcp`, `nix flake check`. У FHS
`manage_engines.py` ставить OrcaSlicer 2.5.0 і Bambu Studio 2.9.1 (AppImage, розпаковка
`unsquashfs`), обидва CLI запускаються; плагін Bambu 02.08.04.60 відкривається dlopen.

- `buildFHSEnv` кладе в `/usr/lib` лише **перелічені** пакети, не їхні залежності: тому
  pango/cairo/atk/harfbuzz/gdk-pixbuf, FFmpeg 7 (`libavcodec.so.61`) і libmspack
  перелічено явно. Новий рушій може вимагати ще щось — перевірка:
  `ldd bin/<рушій> | grep "not found"` усередині FHS (з `LD_LIBRARY_PATH` на `lib/` рушія).
- X11-бібліотеки — під новими іменами (`libx11`, `libxcb`…); `xorg.*` у nixpkgs застарілі.

## CI

`.github/workflows/package.yml`: `workflow_dispatch` і push тегів `v*`. Job `linux`
(ubuntu-22.04) збирає tar.gz, AppImage, .deb, .rpm, перевіряє їх (`dpkg-deb -c`,
`rpm -qlp`, `--appimage-extract-and-run --version`) і кладе в artifacts. Job `aur`
(контейнер `archlinux:base-devel`) збирає `slicenv-bin` з того самого tarball і
`.SRCINFO`. Релізи не створюються, нічого не публікується. Наявний `ci.yml` не змінено.

У job `aur` git ставиться **до** `actions/checkout`: без git checkout вивантажує дерево
через REST API без `.git`, і перевірка `.SRCINFO` (`git diff`) падала з хибним
попередженням. `.SRCINFO` — прихований файл, тому `upload-artifact` має
`include-hidden-files: true`.

## Перевірка на дистрибутивах

### 2026-11-09 — підготовка до Flathub і AUR

tar.gz, AppImage, .deb, .rpm зібрано локально в контейнері `ubuntu:22.04` (як CI); Flatpak —
бандл із Flathub-маніфесту (`tools/package.sh flatpak`), AUR — з `packaging/aur/`.

| Де | Пакет | Що перевірено | Результат |
|---|---|---|---|
| Ubuntu 24.04, Debian 13 (podman) | .deb | `apt install ./…`, `--version` обох бінарників, ldd, `desktop-file-validate`, `appstreamcli validate`, іконки, GUI у Xvfb (майстер), `apt purge` | ✅ |
| Fedora 44 (VM) | .rpm | sudo з паролем → `rpm2cpio`: `--version`, ldd, усі `rpm -qpR` задоволені, desktop і metainfo валідні | ✅ без `dnf install` |
| Fedora 44 (VM) | AppImage | GUI у сесії GNOME (тема й акцент з порталу) | ✅ |
| Fedora 44 (VM) | Flatpak `--user` | встановлення (тягне GNOME 51), `--version` обох, GUI (radv у VM не стартує → запасний рендер), `flatpak uninstall --delete-data` + рантайми | ✅ |
| NixOS 26.05 (VM) | tar.gz, AppImage напряму | stub-ld NixOS | ❌ очікувано — на NixOS `flake.nix` |
| NixOS 26.05 (VM) | AppImage через `nix run nixpkgs#appimage-run` | `--version` обох | ✅ |
| Arch (podman `archlinux:base-devel`) | AUR `slicenv-bin` | makepkg, namcap (PKGBUILD і пакет), `.SRCINFO` збігається, `pacman -U`, `--version`, `pacman -R` | ✅ |
| Arch (podman) | AUR `slicenv-git` | makepkg з `git+file://` гілки, namcap, `pacman -U`, `--version`, `pacman -R` | ✅ `0.1.0.r276.g77b820a-1`; namcap — лише попередження про залежності, які завантажуються через dlopen (wayland, vulkan-icd-loader) або викликаються як процеси (python, curl, tar, xdg-utils) |
| хост CachyOS, Radeon 780M | Flatpak | майстер ставить OrcaSlicer 2.5.0 і Bambu Studio 02.09 у пісочниці, нарізка обома, Preview «Шари» і «Фото» | ✅ |

### 2026-11-08 — пакети з CI

Пакети з CI (run 42422379868). Знімки екрана — у звіті до коміту, не в репозиторії.

| Дистрибутив | Пакет | Встановлення | `--version` (обидва) | desktop/іконка | GUI, майстер | тема ОС | видалення |
|---|---|---|---|---|---|---|---|
| Fedora 44, GNOME 50, Wayland (VM) | .rpm | rpm2cpio (sudo з паролем) | ✅ | ✅ шляхи; `desktop-file-validate` | ✅ | ✅ світла/темна й акцент наживо | — |
| Fedora 44 (VM) | AppImage | FUSE | ✅ | — | ✅ | ✅ | — |
| Fedora 44 (VM) | tar.gz → `~/.local` | вручну | ✅ | ✅ `gtk-launch`, іконка в Shell | ✅ | ✅ | ✅ |
| Fedora 44 (VM) | Flatpak (локальний бандл, коміт 6433a8b) | `flatpak install --user` | ✅ | ✅ exports | ✅ | бандл старший за код теми | ✅ |
| NixOS 26.05 (VM) | `flake.nix` | `nix build`/`nix run` | ✅ | ✅ `share/` | ✅ Xvfb (сесії без пароля немає) | — | — |
| Fedora 44 (podman) | .rpm | `dnf install` | ✅ | ✅ | — | — | ✅ `rpm -e` чисто |
| openSUSE Tumbleweed (podman) | .rpm | `zypper in` | ✅ | ✅ | — | — | ✅ |
| Ubuntu 22.04 / 24.04, Debian 13 (podman) | .deb | `apt install ./…` | ✅ ldd без «not found» | ✅ | 24.04 ✅ Xvfb; 22.04 — чорне вікно в Xvfb (lavapipe Mesa 23) | — | ✅ `purge` чисто |
| Ubuntu 22.04 (podman) | AppImage | без FUSE | ✅ `--appimage-extract-and-run` | — | — | — | — |
| Arch (podman `archlinux`) | AUR `-bin`, `-git` | makepkg + `pacman -U` | ✅ | ✅ | — | — | ✅ |

Не перевірено: живий Omarchy (VM з LUKS — лише юніт-тести `system_theme`), GUI на
реальному GPU поза VM, плагін Bambu у Flatpak, Snap.

Спостереження (не пакування): за `color-scheme=default` у GNOME («без переваги», портал
повертає 0) Slicenv обирає темну тему (`theme::resolve`), хоча GNOME-застосунки в цьому
режимі світлі.

## macOS і Windows

Що з коду працює на цих ОС і що перевірено лише в CI — [`docs/PORTING.md`](../docs/PORTING.md).
Збирає `.github/workflows/package-cross.yml` (лише `workflow_dispatch`, лише
`upload-artifact`): macos-15 (aarch64), macos-15-intel (x86_64; macos-13 знято з підтримки),
windows-latest. Rust 1.99.1 (gpui-pre-macos не збирається на 1.97). **Ще не запускався.**

| Артефакт | Файли | Стан |
|---|---|---|
| macOS `.app` → `.zip`, `.dmg` | `tools/package-macos.sh`, `packaging/macos/Info.plist.in` | ⚠️ лише CI; ad-hoc підпис (`codesign -s -`), без нотаризації |
| Windows портативний `.zip` | `tools/package-windows.ps1` | ⚠️ лише CI; без підпису |
| Windows інсталятор Inno Setup | `packaging/windows/slicenv.iss` | ⚠️ лише CI (iscc з образу windows-latest); per-user, без адміністратора |
| winget | `packaging/winget/Borys.Slicenv.yaml` | 📝 шаблон, не публікується |
| scoop | `packaging/scoop/slicenv.json` | 📝 шаблон, не публікується |

- **Bundle:** `Slicenv.app/Contents/MacOS/{slicenv,slicenv-mcp}`, `Info.plist`
  (`CFBundleIdentifier` = `app.borys.slicenv`, `LSMinimumSystemVersion` 11.0,
  `NSLocalNetworkUsageDescription` для LAN-принтерів), `Resources/slicenv.icns` з
  `crates/app/assets/app/slicenv.svg` (rsvg-convert → `iconutil`), LICENSE і
  THIRD-PARTY-NOTICES. DMG — `hdiutil create -format UDZO` з посиланням на `/Applications`.
- **Windows:** `slicenv.exe` у release зібрано з `windows_subsystem = "windows"` (без
  консольного вікна); `--version` видно лише з перенаправленим stdout. Іконки в `.exe` поки
  немає (потрібен `.ico` і ресурс через `winresource`).
- **Без підпису:** на macOS Gatekeeper скаже «неможливо перевірити розробника» —
  правий клік → «Відкрити», або `xattr -dr com.apple.quarantine /Applications/Slicenv.app`.
  На Windows SmartScreen: «Докладніше» → «Однаково запустити».

### Підпис (на майбутнє)

macOS — Apple Developer Program (99 $/рік):
1. Сертифікат **Developer ID Application** (Xcode → Settings → Accounts або
   developer.apple.com → Certificates); експорт `.p12` у секрет CI
   (`MACOS_CERT_P12`, `MACOS_CERT_PASSWORD`), імпорт у тимчасовий keychain на раннері.
2. Підпис з hardened runtime замість ad-hoc:
   `codesign --force --options runtime --timestamp --sign "Developer ID Application: … (TEAMID)" Slicenv.app/Contents/MacOS/slicenv-mcp Slicenv.app`
   (вкладені бінарники — першими; `--deep` Apple не радить для фінального підпису).
   Entitlements: JIT/unsigned memory не потрібні; мережа клієнта дозволена за
   замовчуванням поза App Sandbox.
3. Нотаризація: App Store Connect API key (`.p8`, Key ID, Issuer ID) у секретах →
   `xcrun notarytool submit slicenv.dmg --key AuthKey.p8 --key-id … --issuer … --wait`,
   потім `xcrun stapler staple slicenv.dmg` (і `.app` перед упакуванням у zip).
4. Перевірка: `spctl -a -vvv -t install slicenv.dmg`, `codesign -dv --verbose=4`.

Windows — варіанти:
- **Azure Trusted Signing** (Artifact Signing): найдешевше (~10 $/міс), без апаратного
  токена; потрібні Azure-підписка, перевірка особи/організації (для фізичних осіб доступна
  не в усіх країнах — перевірити для України). CI: `azure/trusted-signing-action`
  з federated credentials (OIDC), без секретів-сертифікатів.
- **OV/EV-сертифікат** від CA (Sectigo, DigiCert, SSL.com): з 2023 ключ лише на HSM/токені
  або в хмарному сервісі CA (SSL.com eSigner, DigiCert KeyLocker), `signtool sign /fd sha256
  /tr <timestamp> /td sha256`. EV одразу дає репутацію SmartScreen, OV — накопичує.
- **SignPath Foundation** — безкоштовно для open source (потрібен публічний репозиторій).
- Підписувати `slicenv.exe`, `slicenv-mcp.exe` і сам інсталятор (Inno Setup: директива
  `SignTool=`), завжди з timestamp.

### Windows: winget і scoop (шаблони)

- **winget:** `packaging/winget/Borys.Slicenv.yaml` (singleton-маніфест, `InstallerType:
  inno`, `Scope: user`). Після релізу: підставити версію й SHA256 інсталятора,
  `winget validate`, `winget install --manifest`, PR у `microsoft/winget-pkgs`
  (зручно через `wingetcreate update`). Непідписаний інсталятор приймають, але SmartScreen
  і сканування Defender у пайплайні winget частіше його затримують.
- **scoop:** `packaging/scoop/slicenv.json` на портативний zip з `checkver`/`autoupdate`.
  Власний bucket (репозиторій `scoop-slicenv`) — `scoop bucket add slicenv <url>`; до
  `extras` — лише після стабільних релізів.

## Чек-лист майбутньої публікації

Загальне:
- [ ] Вирішити, що робити з приватним репозиторієм: AGPL вимагає давати джерельний код
      кожному, хто отримав бінарник, — для публічних каналів потрібен публічний репозиторій
      (або архів джерел поруч із кожним релізом).
- [x] Homepage, bugtracker, vcs-browser, branding, скриншоти в metainfo (2026-11-09);
      URL запрацюють, коли репозиторій стане публічним і з’явиться тег `v0.1.0`.
- [x] `Maintainer` у `deb/control.in`, `Packager` і `%changelog` у `rpm/slicenv.spec` —
      `Borys Kharchenko <7512615+arximus88@users.noreply.github.com>` (2026-11-09).
- [ ] Ключ підпису: GPG для AUR-тегів/релізів, підпис AppImage (`appimagetool --sign`),
      ключ репозиторію для apt/rpm, якщо буде власний.
- [ ] Збірки для релізу — лише з CI (ubuntu-22.04), не з машини розробника (glibc 2.44).
- [ ] Іконки рушіїв Bambu Studio/OrcaSlicer — торговельні марки; для Flathub/Snap Store
      оцінити ризик (правило «назви й іконки не порушують торговельні марки»).

GitHub Release:
- [ ] Тег `vX.Y.Z` → `package.yml` → вручну завантажити artifacts у реліз (workflow
      навмисно не має `contents: write`).
- [ ] macOS/Windows: вручну запустити `package-cross.yml` для того самого коміту; підписати й
      нотаризувати (розділ «Підпис») перед завантаженням у реліз.

AUR і Flathub — покроково в [docs/PUBLISHING.md](../docs/PUBLISHING.md) (кроки 3–6):
- [ ] AUR: акаунт, SSH-ключ; `slicenv-bin` — після релізу (sha256), `slicenv-git` — після
      відкриття репозиторію.
- [ ] Flathub: PR у `flathub/flathub` (база `new-pr`) з маніфестом і `cargo-sources.json`;
      після прийняття — `https://borys.app/.well-known/org.flathub.VerifiedApps.txt` з
      токеном з Developer Portal. Зараз (2026-11-09) там 404 — очікувано.
- [ ] Обґрунтування дозволів і завантаження рушіїв/плагіна — в описі PR (текст у PUBLISHING.md).

Snap Store:
- [ ] Акаунт Ubuntu One, `snapcraft register slicenv`.
- [ ] Запит на classic confinement на forum.snapcraft.io (обґрунтування — розділ «Snap»).

COPR / PPA:
- [ ] spec зі збіркою з джерел (COPR) / `debian/` для Launchpad (PPA вимагає збірку з
      джерел і офлайн-вендоринг крейтів) — не підготовлено.
