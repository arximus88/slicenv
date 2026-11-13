# CLAUDE.md — Slicenv

Інструкції для AI-агентів (Claude Code, Codex через `AGENTS.md`). Що це за проєкт — [README.md](README.md), уся документація — [docs/INDEX.md](docs/INDEX.md), тести й CI — [docs/TESTING.md](docs/TESTING.md). Машинно-специфічне — у `CLAUDE.local.md` (не в git).

## Крейти
| Крейт | За що відповідає |
|---|---|
| `app` | бінарник `slicenv`: GPUI-інтерфейс (сторінки Home/Prepare/Preview/Device), менеджер рушіїв, i18n (`locales/en.json`), MCP-сервер у застосунку |
| `viewer` | офскрін-3D на wgpu, кадр читається назад у BGRA-зображення |
| `project` | редагований 3MF: пластини, сітки, метадані, undo |
| `slicer` | пошук рушіїв, виклик CLI Bambu Studio / OrcaSlicer, пресети, сумісний режим |
| `settings` | схема параметрів рушія, значення, локалізовані підписи |
| `gcode` | потоковий парсер G-code |
| `geometry` | CSG-прев’ю через Manifold (`manifold-csg`) |
| `lan` | принтери Bambu Lab у LAN на чистому Rust ([docs/LAN.md](docs/LAN.md)) |
| `cloud` | Bambu Cloud через офіційний плагін Bambu Lab ([docs/CLOUD.md](docs/CLOUD.md)) |
| `moonraker` | принтери Klipper через Moonraker HTTP API, без GUI ([docs/KLIPPER.md](docs/KLIPPER.md)) |
| `mcp` | бінарник `slicenv-mcp` (stdio) ([docs/MCP.md](docs/MCP.md)) |
| `paths` | каталоги XDG / ОС, міграція `bambu-lab → slicenv`, змінні середовища |
| `perf` | бюджети продуктивності без вікна (`cargo run --release -p perf`) |

`paths project settings slicer gcode geometry mcp cloud lan moonraker` не залежать від wgpu/GPUI — CI це перевіряє (`cargo tree`). Не тягни туди GUI.

## Перевірка
Перед комітом — skill `verify` (`bash .claude/skills/verify/verify.sh`): переклади, clippy, тести, підсумок passed/failed. Окремо:

```bash
# Переклади
python3 -I crates/app/tools/check_translations.py
# Clippy як у CI (allow-список береться з ci.yml; bash, бо zsh не розбиває змінну на слова)
bash -c 'A=(); for l in $(grep -oE "clippy::[a-z_]+" .github/workflows/ci.yml); do A+=(-A "$l"); done; cargo clippy --workspace --all-targets -- -D warnings "${A[@]}"'
# Тести лише з тимчасовим HOME; R — справжній HOME, запам’ятати ДО підміни
R=$HOME; T=$(mktemp -d); HOME=$T XDG_CONFIG_HOME=$T/c XDG_DATA_HOME=$T/d XDG_CACHE_HOME=$T/k XDG_STATE_HOME=$T/s \
  RUSTUP_HOME=$R/.rustup CARGO_HOME=$R/.cargo cargo test --workspace 2>&1 | tee /tmp/t.log
grep "^test result" /tmp/t.log | awk '{p+=$4; f+=$6} END {print "passed", p, "failed", f}'
```

- Rust зафіксовано на **1.100.0**: `rust-toolchain.toml` (rustup бере сам) = `dtolnay/rust-toolchain@1.100.0` у CI. Новий stable приносить нові лінти clippy — оновлення версії окремим комітом, усі місця разом.
- `rust-version` у `Cargo.toml` — мінімум для збірки (1.97), **не** версія розробки: Flatpak (rust-stable SDK) і AUR (rust з Arch) збирають своїм, старішим toolchain. API новіші за `rust-version` не використовувати; піднімати лише після перевірки обох.
- `.cargo/config.toml`: `global-min-publish-age = "3 days"` — `cargo update` не бере щойно опубліковані версії крейтів. Терміновий фікс: `CARGO_RESOLVER_INCOMPATIBLE_PUBLISH_AGE=allow cargo update <крейт> --precise <версія>`.
- **Не запускати `cargo fmt --all`**: дерево свідомо не відформатоване, масове форматування — окремий коміт. Форматуй лише свої рядки.
- Новий лінт clippy не додавати в `CLIPPY_ALLOW` у ci.yml — виправити код. Прибрав останнє місце лінту — прибери його зі списку.
- Manifold (`manifold-csg-sys`) при першій збірці клонує й збирає C++ з GitHub. Без мережі: `MANIFOLD_CSG_LIB_DIR=<тека з libmanifoldc і libmanifold тієї самої версії>`, для статичних бібліотек ще `MANIFOLD_CSG_LIB_KIND=static`.

## Безпека й дані
- Тести, рушії й GUI — лише з тимчасовими HOME/XDG_*. Не змінювати `~/.config/BambuStudio`, `~/.config/OrcaSlicer`, `~/.config/slicenv`, `~/.local/share/slicenv`. `slicenv <файл>` відкриває вікно зі справжньою конфігурацією. Як запускати безпечно — skill `real-engine-test`.
- Жодних команд на справжні принтери (друк, стоп, пауза, світло, рух, завантаження файлів, MQTT) без людини. Жодних звернень до Bambu Cloud у тестах. Для UI — `SLICENV_FAKE_PRINTER=1`, `SLICENV_CLOUD_OFF=1`, `SLICENV_LAN_OFF=1`.
- Не друкувати, не логувати й не комітати токени, access codes, паролі, email, повні серійні номери. Access code не вгадувати.
- Сторонній форк плагіна (Open Bamboo Networking від persano) — лише як **явний вибір користувача**: він сам бере вже встановлений в OrcaSlicer / Bambu Studio або погоджується завантажити реліз persano з GitHub ([docs/OPEN-PLUGIN.md](docs/OPEN-PLUGIN.md)). Бінарники форку й вшитий у них секрет ніколи не потрапляють у репозиторій, наш бінарник, zip чи пакунки; без вибору OBN не підхоплювати. Власних вшитих секретів Bambu не додавати.
- Процеси зупиняти за PID (`kill <pid>`), ніколи `pkill -f` — збігається із самою командою й чужими процесами.

## Інструменти
- Пошук — `rg` з **явним шляхом** (`rg -n x .`): без шляху в неінтерактивному запуску rg читає stdin і зависає. Не `grep -rn`. Виклики функцій — `ast-grep -p 'f($$$)'`.
- Python — `python3 -I`. JS — лише bun.
- zsh не розбиває змінну зі списком файлів на слова: `rg -l0 … . | xargs -0 …` або `bash -c`.

## Git
- Агенти працюють у worktree. Наприкінці — `rm -rf target/` у своєму worktree.
- Коміти українською, апостроф `’` (не `'`), останній рядок — `Co-Authored-By: <модель> <noreply@…>`.
- Не пушити й не зливати в main без явного дозволу людини. Злиття — skill `merge-agent-branch`.
- `crates/app/locales/en.json`: нові ключі лише в кінець, не сортувати — інакше конфлікт на весь файл. Деталі — skill `add-ui-string`.

## Пастки рушіїв і принтера
- Друк через Bambu Cloud із закритого плагіна завжди дає **-26** (`BAMBU_NETWORK_SIGNED_ERROR`, `cloud::PluginError::UNSIGNED_SOFTWARE`): лише програми з підписом Bambu Lab. Друк через інтернет — Bambu Connect (`bambu-connect://import-file`, параметри тільки `path`, `name`, `version`), інакше LAN. На Linux, якщо користувач вибрав відкритий плагін (`cloud::obn`), LAN-друк іде через `start_local_print` у дочірньому процесі `--obn-local-print`, а хмарний — лише з `block_cloud = 0`.
- OrcaSlicer пише лог у `00000.log` у робочій теці. Прогрес і номер шару — з `--debug 4` (`default_status_callback: percent=…, message=Generating G-code: layer N`), не з `--pipe` (`crates/slicer/src/cli.rs`).
- Рушій падає на 3MF (OrcaSlicer 2.3.0 CLI на Windows — 0xC0000005 на багатьох файлах) → сумісний режим зі стадіями (`cli.rs`, `safe_mode.rs`), запам’ятовується в `engine-compat.json` у кеші.
- Батьківський пресет шукається в тому самому вендорі, потім в `OrcaFilamentLibrary` (`presets.rs`). Пошук по всіх вендорах підмінював однойменних службових батьків (Anker).

## Пастки GPUI (gpui-pre 0.3.10, gpui-component 0.8.1)
- Zero-copy 3D немає: кадр wgpu читається в зображення (`viewer`). Не шукай `paint_surface` на Linux.
- Рамку вікна з тінню 20 px малює `Root` gpui-component. Не додавай другу рамку: перемикання `set_client_inset` дає секунди повних перемальовок (`workspace.rs`).
- Довгий текст: контейнер з `max_w(…)` і `whitespace_normal()`.
- Трикрапка: `min_w_0()` + `truncate()` на дочірньому div. `ui::ghost_button` має `flex_none` і не стискається — для обрізання `ghost_button_bare` + свій дочірній div.
- Шрифти Windows ширші — лишай запас ширини.
- Кола й крапки — парних розмірів (16/8 px), інакше на масштабах 125/150 % з’їжджають.
- `ui::*` хелпери самі перекладають підписи через `tr()`; кожен кириличний літерал потребує ключа в en.json.

## Windows
- PowerShell 5.1 читає `.ps1` без BOM як ANSI: кожен `.ps1` з кирилицею — UTF-8 **з BOM**.
- Пакування — `tools/package-windows.ps1` → `dist\slicenv-<версія>-x86_64-windows.zip`; без Inno Setup — лише zip. Деталі — [docs/PORTING.md](docs/PORTING.md), [packaging/README.md](packaging/README.md).

## Skills (`.claude/skills/`)
| Skill | Коли |
|---|---|
| `verify` | перед комітом і злиттям |
| `add-ui-string` | новий чи змінений український рядок, впав check_translations.py |
| `real-engine-test` | зміни в slicer/пресетах, запуск рушія чи GUI |
| `merge-agent-branch` | злиття гілки агента в main (лише з дозволу) |
| `ux-dialog-check` | новий чи перероблений діалог, модалка, форма з вибором варіантів |
