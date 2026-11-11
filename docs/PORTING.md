# Перенесення на macOS і Windows

Стан на 2026-11-08. Windows 11 перевірено на справжній машині (розділ «Windows: перевірка
на залізі» нижче). Машини з macOS немає: усе, що нижче позначено «лише CI»,
перевіриться тільки запуском `.github/workflows/package-cross.yml` (workflow_dispatch).

## Що де перевірено

| Перевірка | Linux x86_64 | Windows x86_64 (msvc) | macOS aarch64 | macOS x86_64 |
|---|---|---|---|---|
| `cargo clippy --workspace --all-targets` з allow-list CI | ✅ локально, 1.97.1 | — | — | — |
| `cargo clippy` lib/bins/tests усього workspace | ✅ | ✅ локально, 1.97.1 (1) | ✅ локально, 1.99.1 (1)(2)(3) | — |
| `cargo check` lib/bins/tests усього workspace | ✅ | ✅ локально, 1.99.1 (1) | ✅ локально, 1.99.1 (1)(2) | ✅ локально, 1.99.1 (1)(2) |
| `cargo build --release`, лінкування | ✅ локально | ✅ Windows-машина, 1.99.1 + VS Build Tools 2022 | лише CI | лише CI |
| `cargo test` headless-крейтів | ✅ локально (тимчасовий HOME/XDG) | ✅ `slicer` на Windows-машині, решта — CI | лише CI | лише CI |
| Пакет | tar/AppImage/deb/rpm (`package.yml`) | zip + Inno Setup, лише CI | .app → zip/dmg, лише CI | те саме |
| Запуск GUI | ✅ (раніше) | ✅ Windows-машина (RX 6800 XT, Vulkan, 115 %) | ніколи | ніколи |

(1) Крос-перевірка з Linux без SDK цілі: C/C++ build-скрипти `ring` і `manifold-csg-sys`
замінено заглушками (`--config 'target.<T>.ring_core_0_17_14_.rustc-link-lib=[]'`,
`…manifold.rustc-link-lib=[]` — ключ `links` дозволяє не запускати build.rs). Перевіряється
весь Rust-код, але не компіляція C і не лінкування.

(2) Build-скрипт `gpui-pre-apple` генерує Metal-шейдери лише на macOS-хості
(`#[cfg(target_os = "macos")]` у build.rs), тож для локального `check` у `target/` підкладено
порожні `shaders.metallib`/`stitched_shaders.metal`. На CI їх компілює справжній `xcrun metal`.

(3) Clippy 1.99 додає `chunks_exact_to_as_chunks` (`project/src/cut.rs`) і
`needless_late_init` (`app/src/tool_panels.rs`) у наявному коді — для крос-перевірки дозволені
поверх allow-list CI; виправляти разом із переходом `ci.yml` на 1.99. `package-cross.yml`
clippy не запускає.

**Rust для macOS: ≥ 1.99.1.** `gpui-pre-macos 0.3.10` викликає `Option::map_or_default`,
нестабільний у 1.97.1 (`error[E0658]: result_option_map_or_default`), а з 1.99.1 збирається.
`ci.yml` лишається на 1.97.1 (там clippy, і новий stable приносить нові лінти);
`package-cross.yml` використовує 1.99.1 для обох ОС. Перехід `ci.yml` на 1.99 — окремим
комітом разом із новими лінтами (наприклад `chunks_exact_to_as_chunks`).

## Матриця можливостей

| Можливість | Linux | macOS | Windows | Чому / що потрібно |
|---|---|---|---|---|
| Вікно, 3D, Preview (gpui + wgpu) | ✅ Wayland/X11, Vulkan/GL | лише CI: Metal (gpui), wgpu → Metal | лише CI: DirectX 11 (gpui), wgpu → DX12/Vulkan | viewer рендерить через wgpu окремо від gpui і віддає картинку — платформної інтеграції не треба |
| Текст у gpui | ✅ | `font-kit` увімкнено (`Cargo.toml`) | ✅ DirectWrite | без `font-kit` gpui_macos «no text will be rendered» |
| Нарізання CLI (Bambu Studio / Orca) | ✅ | ✅ Orca 2.4.2; ❌ Orca 2.3.0 падає (0xC0000005) | лише CI | ті самі аргументи; пошук рушія — нижче |
| Прогрес і номер шару | ✅ з логу `--debug 4` | ✅ з логу | ✅ з логу | `--pipe` не використовується: номера шару в ньому немає (`slicer/src/cli.rs`) |
| Пошук встановлених рушіїв | `PATH`, `/opt` | `PATH`, `/Applications`, `~/Applications` | `PATH`, `%ProgramFiles%`, `%LOCALAPPDATA%\Programs` | плюс `BAMBU_STUDIO`/`ORCA_SLICER` і ручний вибір файлу |
| Менеджер рушіїв (завантаження з GitHub) | ✅ AppImage/tar | ⚠️ dmg (`hdiutil`), на Mac не перевірено | ✅ portable zip Orca і Bambu | `app/src/engine_manager.rs`; тека `paths::data_dir()/engines` |
| Профілі Bambu/Orca | `~/.config/BambuStudio` | `~/Library/Application Support/BambuStudio` | `%APPDATA%\BambuStudio` | `paths::vendor_config_dir`; resources з каталогу рушія |
| Bambu Cloud | ✅ x86_64 | ❌ «не підтримується на цій ОС» | ✅ x64: вхід через браузер і статус перевірено на справжній машині | шим передає `std::string` через межу: на Windows — MSVC-шим під плагін 02.08.04.60; для macOS потрібен libc++-шим |
| Камера X1 (BambuSource, хмара) | ✅ | ❌ | ⚠️ зібрано, наживо не перевірено | та сама бібліотека плагіна |
| LAN: пошук, MQTT, FTPS | ✅ | лише CI | лише CI | чистий Rust (rustls/ring, socket2); `SO_REUSEPORT` лише Unix |
| LAN-камера A1/P1 (JPEG по TLS) | ✅ | лише CI | лише CI | чистий Rust |
| Тема: Omarchy | ✅ | — | — | лише Linux-дистрибутив |
| Тема: портал (колір, акцент) | ✅ GNOME/KDE | — | — | xdg-desktop-portal |
| Тема: світла/темна ОС наживо | через портал | `observe_window_appearance` | `observe_window_appearance` | gpui дає appearance вікна |
| «Показати у теці» | FileManager1 / xdg-open | `open -R` | `explorer /select,` | `file_menu::reveal` |
| MCP stdio (`slicenv-mcp`) | ✅ | ✅ Windows-машина (open_project, slice_plate) | лише CI | ізоляція stdout через `dup2` — лише Unix (на Windows плагін не вантажиться) |
| Support Fins | ✅ | лише CI | лише CI | `curl`, `tar`, Node є в macOS і Windows 10+ |

## Каталоги

`paths::config_home()` — там, де настільні застосунки тримають налаштування; саме туди Bambu
Studio й OrcaSlicer кладуть свій `data_dir`. Перевірено за вихідниками (`~/Projects/ref`):
`GUI_App::init_app_config` → `SetAppName(SLIC3R_APP_KEY)` (`"BambuStudio"` / `"OrcaSlicer"`
у `version.inc`), далі на Linux `$XDG_CONFIG_HOME/<App>` (або `~/.config`), на інших ОС
`wxStandardPaths::Get().GetUserDataDir()` = `%APPDATA%\<App>` (Windows) і
`~/Library/Application Support/<App>` (macOS). Bambu Beta-збірки дописують `Beta`
(`BBL_INTERNAL_TESTING`) → `BambuStudioBeta`. Orca на Windows/macOS ще перевіряє портативний
`data_dir` поруч із виконуваним файлом — його ми не шукаємо.

| Корінь | Linux / BSD | macOS | Windows |
|---|---|---|---|
| `config_home` | `$XDG_CONFIG_HOME` | `~/Library/Application Support` | `%APPDATA%` |
| `data_home` | `$XDG_DATA_HOME` | `~/Library/Application Support` | `%LOCALAPPDATA%` |
| `cache_home` | `$XDG_CACHE_HOME` | `~/Library/Caches` | `%LOCALAPPDATA%` |
| `config_dir()` | `…/slicenv` | `…/Application Support/slicenv` | `%APPDATA%\slicenv` |
| `data_dir()` | `…/slicenv` | `…/Application Support/slicenv` | `%LOCALAPPDATA%\slicenv` |
| `cache_dir()` | `…/slicenv` | `~/Library/Caches/slicenv` | `%LOCALAPPDATA%\slicenv\cache` |
| `vendor_config_dir("BambuStudio")` | `~/.config/BambuStudio` | `~/Library/Application Support/BambuStudio` | `%APPDATA%\BambuStudio` |

На macOS `config_dir` і `data_dir` збігаються — імена всередині не перетинаються
(`preferences.json`, `engines.json`, `lan-printers.json`, `locales/` проти `presets/`,
`cloud/`, `bambu-plugin/`, `plugins/`). Міграція `bambu-lab → slicenv` працює лише на Linux.
`paths::home_dir()` — `HOME`, на Windows `USERPROFILE`.

## Рушії

- **Імена.** CMake перейменовує ціль у `bambu-studio`/`orca-slicer` лише на Linux
  (`if (NOT WIN32 AND NOT APPLE)`), а на Windows — GUI-обгортку `*_app_gui`
  (`OUTPUT_NAME "bambu-studio"` / `"orca-slicer"`). Тому Windows: `bambu-studio.exe`,
  `orca-slicer.exe`; macOS: `BambuStudio.app/Contents/MacOS/BambuStudio`,
  `OrcaSlicer.app/Contents/MacOS/OrcaSlicer`.
- **Каталоги встановлення Windows:** `CPACK_PACKAGE_INSTALL_DIRECTORY = CPACK_PACKAGE_NAME`
  → `%ProgramFiles%\OrcaSlicer`; Bambu — `%ProgramFiles%\Bambu Studio`. Реєстр (`App Paths`)
  не читаємо: якщо рушій поставлено деінде, — `BAMBU_STUDIO`/`ORCA_SLICER` або вибір файлу.
- **Профілі рушія:** `<каталог exe>/resources/profiles` (Windows), `Contents/Resources/profiles`
  (macOS, додано явно для регістрочутливої APFS).
- **Обгортка Windows без консолі.** `orca-slicer.exe` / `bambu-studio.exe` — підсистема `WIN32`;
  stdout/stderr ми передаємо файлами, і для нарізання це працює (перевірено з Orca 2.3.0 і 2.4.2:
  лог, `[error]`-рядки, `result.json`). **Виняток — `--help` в Orca ≥ 2.4:** `CLI::print_help`
  робить `AttachConsole`/`AllocConsole` + `freopen("CONOUT$")`, тож довідка не потрапляє в наші
  файли (0 байт), а з GUI без консолі Orca відкриває нове консольне вікно. Тому на Windows
  `Engine::probe` для Orca `--help` не викликає: рушій упізнається за `OrcaSlicer.dll`, прапорці —
  відомий набір (`ORCA_CLI_FLAGS`), версія — з реєстру деінсталятора (`DisplayVersion`) або
  рядка «OrcaSlicer X.Y.Z» в `OrcaSlicer.dll` (портативна збірка).
- **Версія Orca.** Orca ≤ 2.3 у `--help` друкує не свою версію, а Bambu-сумісну `SLIC3R_VERSION`
  (2.3.0 → «OrcaSlicer-01.10.01.50»). `EngineInfo::version` (identity, каталог профілів) лишається
  машинним рядком, для показу — `EngineInfo::label` («OrcaSlicer 2.3.0»).
- **Коди виходу.** `-62` → `0xFFFFFFC2` на Windows; `code as u8 as i8` дає ті самі −62.
  Сигналів на Windows немає: `paths::host::exit_signal` повертає NTSTATUS класу помилки
  (`0xC0000005`…) як «аварію».
- **`--pipe`** — FIFO (`libc::mkfifo` + `O_NONBLOCK`) лише на Linux. Named pipe на Windows не
  допоможе: `g_cli_callback_mgr.start(pipe_name)` в обох рушіях стоїть під
  `#if defined(__linux__)` (`BambuStudio.cpp`, `OrcaSlicer.cpp`), на macOS теж. Прогрес там —
  `progress_percent` з логу.
- **Менеджер рушіїв** (`app/src/engine_manager.rs`, порт `manage_engines.py` на Rust):
  Linux — AppImage (`unsquashfs` або `--appimage-extract`) і tar; Windows — portable zip
  (`OrcaSlicer_Windows_*_x64_portable.zip`, `Bambu_Studio_win-*.zip`: exe у корені, без
  інсталятора, реєстру й ярликів; `.pdb` не розпаковуються); macOS — `hdiutil attach` → `ditto`
  `.app` (лише в коді, на справжньому Mac не перевірено). Python-скрипт лишився CLI для Linux.

## Хмара

- Шим (`crates/cloud/native/shim.cpp`) викликає функції плагіна, що приймають і повертають
  `std::string`/`std::map`/`std::function` за значенням, тож збирається тим самим компілятором і
  C++-рантаймом, що й плагін: Linux x86_64 — libstdc++ з `_GLIBCXX_USE_CXX11_ABI=1`; Windows x64 —
  MSVC з динамічним CRT (`/MD`, `_ITERATOR_DEBUG_LEVEL=0`; плагін імпортує MSVCP140/VCRUNTIME140).
  `std::function` у MSVC — 64 байти, у libstdc++ — 32 (static_assert за компілятором);
  `PrintParams` має лише `std::string`/`int`/`bool`, макет однаковий. Завантаження —
  `native/dl.hpp` (`dlopen` / `LoadLibraryExW` з UTF-16 шляхом і закріпленням модуля).
  macOS (libc++) — ще ні: там `cfg(bambu_plugin)` вимкнено і FFI — заглушки.
- Перевірено на Windows 11: `tests/bridge.rs` і `tests/abi0208.rs` з фейковими
  плагінами, зібраними `cl.exe`; справжній плагін 02.08.04.60 для Windows (47,9 МБ) —
  завантаження, `create_agent`/`set_config_dir`/`init_log`/`start`/`destroy_agent` без входу
  (`examples/plugin_probe.rs`), вихід з кодом 0; `get_version` плагіна Orca 2.3 — 01.10.01.09
  (не підтримується, агент не створюється).
- Версія ОС, device-id і живість власника осиротілої сесії — `src/os.rs` (`/proc` на Linux,
  `RtlGetVersion`/`CoCreateGuid`/`OpenProcess` на Windows).

## Аудит Linux-специфічного коду

`rg -n "cfg\(target_os|unix|libc::|/usr/|\.so\b|AppImage|XDG|\.config|mkfifo|ExitStatusExt|std::os::unix"`
та пошук `Command::new`/`host::command`. Рішення по кожному місцю:

| Місце | Що | Рішення |
|---|---|---|
| `paths/src/lib.rs` `config_home`…`state_home` | XDG | cfg-гілки macOS/Windows (таблиця вище) |
| `paths/src/lib.rs` `migrate_xdg`, `copy_tree` | міграція `bambu-lab`, `unix::fs::symlink` | лише Linux; симлінк через `fs_mode::symlink` |
| `paths/src/lib.rs` тести міграції | `PermissionsExt`, `UnixListener` | `#[cfg(unix)]` |
| `paths/src/host.rs` | AppImage: `APPDIR`, `:`-списки, `OsStrExt` | лише Linux (`plan` порожній деінде); `CREATE_NO_WINDOW` на Windows |
| `paths/src/fs_mode.rs` (новий) | 0600/0700, `O_NOFOLLOW`, uid | обгортки; на Windows no-op / `FILE_FLAG_OPEN_REPARSE_POINT` / «так» |
| `slicer/src/cli.rs` `open_pipe` | `libc::mkfifo`, `O_NONBLOCK` | лише Linux (див. «Рушії») |
| `slicer/src/cli.rs` `ExitStatusExt::signal` | сигнал рушія | `paths::host::exit_signal` |
| `slicer/src/cli.rs` `BINARIES` | імена Bambu в `PATH` | `.exe` на Windows |
| `slicer/src/engine.rs` `binaries`/`installed` | `/opt/...` | cfg-гілки macOS/Windows |
| `slicer/src/engine.rs`, `cli.rs` тести | `#!/bin/sh`-макети | `#[cfg(unix)]` |
| `slicer/src/presets.rs` | `/usr/share/BambuStudio/profiles`, `/usr/bin/...` | лише Linux-пакети, деінде не знайдуться — без змін; `Contents/Resources` для macOS |
| `cloud/build.rs` | `assert linux x86_64`, `-ldl`, libstdc++ | збірка шима лише там; інакше `cfg(not(bambu_plugin))` |
| `cloud/src/ffi.rs` | `extern "C"` шима | заглушки без шима |
| `cloud/src/session.rs` | `DirBuilderExt`, `O_NOFOLLOW`, `getuid`, `/proc` | `fs_mode`; `/proc` — лише Linux |
| `cloud/src/account.rs`, `plugin.rs` | режими 0600/0700, `.so`, `"linux"` | `fs_mode`; `LIBRARY`/`SOURCE_LIBRARY`/`OS_TYPE` за ОС |
| `cloud/tests/*.rs` | фейковий плагін через `c++ -shared -fPIC` | `#![cfg(all(target_os = "linux", target_arch = "x86_64"))]` |
| `cloud/examples/*` | `libc::dup2`, `/dev/null` | лише Linux; CI на macOS/Windows запускає `--lib --bins --tests` |
| `mcp/src/main.rs` | `dup`/`dup2` stdout → `/dev/null` | `#[cfg(unix)]`; на Windows — звичайний stdout |
| `mcp/src/http.rs` | 0600 токена | `fs_mode` |
| `lan/src/store.rs` | 0600/0700 | `fs_mode` |
| `lan/src/discovery.rs` | `set_reuse_port` | `#[cfg(unix)]` |
| `app/src/system_theme.rs` | Omarchy, `fc-match`, портал (`ashpd`) | лише Linux (уже було); `Event::Portal` dead code деінде |
| `app/src/preferences.rs` | тема ОС | + `observe_window_appearance` для macOS/Windows |
| `app/src/file_menu.rs` | `dbus-send`, `xdg-open` | `open -R` (macOS), `explorer /select,` (Windows) |
| `app/src/device.rs`, `wizard.rs` | `OpenOptionsExt`, `ExitStatusExt` | `fs_mode`, `exit_signal`; майстер: стан `UnsupportedOs` для хмари |
| `app/src/packages.rs` | менеджер рушіїв | `engine_manager.rs` на всіх ОС, без Python |
| `app/src/library.rs`, `main.rs` | `HOME` | `paths::home_dir()` |
| `app/src/main.rs` | консольне вікно Windows | `windows_subsystem = "windows"` у release |
| `app/src/preferences.rs` | `slicenv-mcp` у підказці MCP | `EXE_SUFFIX` |
| `app/Cargo.toml` | `ashpd` | уже `cfg(target_os = "linux")` |
| `Cargo.toml` `gpui_platform` | `wayland`, `x11` | ігноруються поза Linux; + `font-kit` для macOS |
| `packaging/appimage/AppRun`, `tools/package.sh` | AppImage/deb/rpm | Linux; macOS/Windows — `tools/package-macos.sh`, `tools/package-windows.ps1` |

## Що лишилось

1. **Перший прогін `package-cross.yml`** і виправлення того, що покаже справжнє лінкування
   й тести на macOS/Windows (C++ `manifold`, `ring` на MSVC, тести зі шляхами `/`).
2. **Запуск GUI** на macOS і Windows: рендер wgpu → gpui, шрифти, діалоги файлів, перезапуск
   після майстра, `--cloud-probe`-дочірній процес.
3. **Нарізання на справжніх рушіях** macOS/Windows: лог обгортки Windows без консолі,
   коди виходу, шлях до profiles у `.app`.
4. **Менеджер рушіїв** для dmg/zip/exe.
5. **Хмара:** шим під libc++ (macOS) і MSVC (Windows), перевірка ABI на справжньому плагіні
   (`docs/CLOUD.md`), `LoadLibraryW` замість `dlopen`.
6. **Іконка в `.exe`** (`.ico` + ресурс через `winresource`/`embed-resource`); зараз іконка
   лише в ярликах Inno Setup за замовчуванням.
7. **Підпис і нотаризація** — `packaging/README.md`, розділ «macOS і Windows».

## Windows: перевірка на залізі (2026-11-08)

Windows 11 10.0.26200, RX 6800 XT, 2560×1440 при 115 % (AppliedDPI 110), світла тема ОС.
Збірка — `cargo +1.99.1 build --release` з VS Build Tools 2022 (MSVC, Windows SDK `rc.exe`).

- **GUI:** вікно, 3D (Vulkan), текст DirectWrite з кирилицею, іконки, HiDPI — без зауважень.
  Системний заголовок прибрано (`appears_transparent` лише на Windows): перетягування, подвійний
  клік і кнопки ─ □ ✕ дає `TitleBar` з gpui-component через `WindowControlArea` → `WM_NCHITTEST`.
- **Іконка й групування:** `build.rs` (winresource) вбудовує `slicenv.ico` як ресурс ID 1 і
  VERSIONINFO; gpui бере ID 1 для класу вікна (`LoadImageW(…, 1)`). AppUserModelID
  `app.borys.slicenv` (`cx.set_app_identity`) збігається з ярликом Inno Setup — два різні exe
  групуються в одну кнопку панелі завдань.
- **Діалог відкриття:** `rfd` (IFileOpenDialog) з фільтром типів і «Усі файли».
- **Orca, знайдена в `%ProgramFiles%\OrcaSlicer`:** версія 2.3.0 з реєстру.
- **3MF з Bambu Studio 2.7 в Orca:** −24 без `--allow-newer-file` (тепер і в MCP, як у GUI),
  −18 на `wall/sparse_infill/solid_infill_filament = 0` і на −1 («авто» Bambu) у
  `raft_first_layer_expansion`/`tree_support_wall_count` — `slicer::orca_compat` нарізає копію з
  виправленим конфігом; перевірка сумісності пропонує ті самі виправлення.
- **OrcaSlicer 2.3.0 CLI на Windows падає з 0xC0000005** (Application Error: модуль
  `OrcaSlicer.dll`). Дві незалежні причини, обидві — код інтерфейсу в консольному режимі:
  - назва пластини (`plater_name` у `Metadata/model_settings.config`, її пишуть Bambu Studio 2.7
    і Slicenv) → `PartPlate::set_plate_name` → `generate_plate_name_texture()` бере
    `m_plater`, якого в CLI немає (зсув 0xc687d0; upstream OrcaSlicer#7920). Бісекція 3MF:
    без цього одного рядка файл нарізається;
  - `--export-3mf`, коли мініатюр немає або `--arrange 1` (STL) → рендер мініатюр через
    GLFW/OpenGL у `render_thumbnail` (зсув 0xe84ece; upstream #16010/#16013). G-code на той
    момент уже записано; `--min-save`, `--no-check`, без `--load-*` — те саме.
  Slicenv не вимагає оновлення: `slicer::safe_mode` після аварії нарізає копію без назв
  пластин, далі лише G-code (без `--export-3mf`) і сам збирає `.gcode.3mf`; стадія
  запам’ятовується для рушія (`cache/sliced/engine-compat.json`). На Windows-машині: Spider (3MF
  Bambu 2.7) — 3MF від Orca без назв пластин; куб STL — лише G-code (19 хв 45 с, 4,13 г).
- **OrcaSlicer 1.9.1 і 2.0.0 (портативні) на Windows** падають у CLI на будь-якому вході, зокрема
  на кубі без профілів, одразу після `PartPlateList::init` — до читання налаштувань і моделі;
  жодна комбінація прапорців не допомагає. Slicenv пробує всі стадії й лише тоді показує м’яку
  пораду спробувати іншу версію. Довідки `--help` вони через ssh/без консолі теж не друкують,
  тож набір прапорців Orca на Windows читається з ключів опцій у `OrcaSlicer.dll`
  (в 1.9/2.0 немає `allow_newer_file`).
