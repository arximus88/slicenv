# Тестування й бюджети продуктивності

Як перевірити, що зміна нічого не зламала й не сповільнила. Збирати в окремий каталог
цілей, щоб не заважати іншим збіркам: `export CARGO_TARGET_DIR=~/.cache/slicenv-target-ci`.

| Що | Команда | Потрібно | Час |
|---|---|---|---|
| Unit- та інтеграційні тести | `cargo test --workspace` | — (GPU-тести без адаптера пропускаються) | ~2 хв збірки, секунди прогону |
| Те саме, GPU обов’язковий | `SLICENV_REQUIRE_GPU=1 cargo test --workspace` | wgpu-адаптер | |
| Переклади | `python3 -I crates/app/tools/check_translations.py` | python3 | <1 с |
| Clippy, як у CI | див. [CI](#ci) | | |
| Бюджети без вікна | `cargo run --release -p perf` | для GPU-частини — адаптер | ~10 с |
| GUI-бенч | `tools/bench/gui-bench.sh` | Xvfb, xdotool, release-збірка | ~1 хв |
| Порівняння з Bambu Studio / OrcaSlicer | `tools/bench/compare.sh` ([BENCHMARK.md](BENCHMARK.md)) | mutter, xdotool, xprop, рушії в `~/.local/share/slicenv/engines` | ~30–40 хв |
| Приймальні сценарії UI | `crates/app/verification/*.py` | Xvfb, xdotool, фікстури з docstring | різний |
| G-code Slicenv проти Bambu Studio / OrcaSlicer | `python3 -I tools/gcode-diff.py A B` ([нижче](#порівняння-g-code-з-оригінальним-рушієм)) | python3 | секунди |

## Unit- і GPU-тести

`cargo test --workspace` — усі крейти. Тести, яким потрібен wgpu-адаптер (рендер у
`crates/viewer`), беруть рендерер через `viewer::test_renderer(name)`: без адаптера він
друкує `SKIP <name>: немає wgpu-адаптера …` у stderr і тест проходить. Побачити пропуски:
`cargo test -p viewer -- --nocapture 2>&1 | grep SKIP`. На машині, де GPU є обов’язково,
`SLICENV_REQUIRE_GPU=1` робить пропуск провалом. Відтворити CI без GPU локально:
`WGPU_BACKEND=metal cargo test --workspace` (на Linux такого бекенду немає → адаптера немає).

Новий GPU-тест пиши так:

```rust
let Some(mut renderer) = viewer::test_renderer("my_test") else {
    return;
};
```

`#[ignore]` мають тести з зовнішніми фікстурами або вимірами часу:
`project::orient` (release-вимір), `geometry/tests/boolean.rs` (`GEOMETRY_ANGEL_FIXTURE`),
`app/src/migration.rs` (`ANGEL_MIGRATION_FIXTURE` + Orca CLI). Запуск:
`cargo test -p <crate> -- --ignored` з відповідною змінною.

## Бюджети без вікна: `perf`

```bash
cargo run --release -p perf                      # порівняти з bench/baselines.json
cargo run --release -p perf -- --update-baseline # переписати розділ headless
cargo run --release -p perf -- --no-gpu --only bounds
cargo run --release -p perf -- --invariants-only # як у CI
```

Сцена — процедурна UV-сфера 1 000 000 △ (1000 × 501, радіус 40 мм) на одній пластині.
Метрика — медіана N ітерацій (p95 для довідки). Провал (вихід 1), якщо медіана
більша за `max(tolerance × baseline, baseline + floor_ms)` (зараз 2× і 0.05 мс; поріг
`floor` гасить шум мікросекундних метрик) **або** порушено інваріант. Метрика без
baseline позначається «нова» і не валить. Debug-збірка друкує попередження: числа
непорівнянні.

Інваріанти не залежать від машини, тож CI перевіряє лише їх (`--invariants-only`):

- рамка інстанса з `project::bounds_cache` після перенесення точна (дорівнює
  `Project::instance_bounds`);
- `bounds_cache::refresh` після перенесення ≤ max(1 % холодного, 0.05 мс) — записи
  перевикористовуються, вершини не перебираються;
- `Project::translate` ≤ 0.05 мс — не залежить від розміру меша.

GPU-сценарії (без адаптера — «пропущено»): офскрін-кадр 1300 × 900 з
`set_volume_mesh` + `set_scene_items`, як у застосунку; камера трохи обертається щокадру.

### Бюджети на цій машині

Ryzen 7 7840HS / Radeon 780M (RADV), 2026-11-04, `bench/baselines.json`:

| Метрика (1 М △) | baseline p50, мс | межа, мс | Де в застосунку |
|---|---:|---:|---|
| GPU-кадр: encode+submit (CPU) | 0.095 | 0.19 | кожен кадр в’юпорта (`viewer::encode_submit`) |
| GPU-кадр: encode+submit+readback | 2.33 | 4.66 | кадр орбіти без GPUI |
| Кадр перетягування: `set_scene_items(1)` + кадр | 2.18 | 4.37 | кадр живого перетягування |
| `Renderer::set_scene_items(1)` / `(1000)` | 0.0006 / 0.009 | 0.05 / 0.06 | кожен кадр перетягування |
| `Renderer::set_volume_mesh` (завантаження 72 МБ) | 29.2 | 58.5 | відкриття, зміна меша |
| `viewer::local_mesh` | 30.4 | 60.7 | відкриття, зміна меша |
| `viewer::plates_of` | <0.001 | 0.05 | `project_changed` |
| `bounds_cache::refresh` холодний | 3.14 | 6.29 | зміна форми (поворот, масштаб, меш) |
| `bounds_cache::refresh` після перенесення | 0.0002 | 0.05 | кожна правка, перетягування |
| `Project::translate` | <0.0001 | 0.05 | кожна правка |
| `Project::plate_bounds` (hit-test) | 0.78 | 1.56 | mouse-move над сценою |
| правка + undo-знімок (клон, translate, `History::record`) | 0.57 | 1.14 | кожна правка (переважно `Project ==`) |
| `History::undo` | <0.0001 | 0.05 | Ctrl+Z |

## GUI-бенч: `tools/bench/gui-bench.sh`

```bash
cargo build --release -p app
tools/bench/gui-bench.sh                   # виміряти й порівняти
tools/bench/gui-bench.sh --update-baseline # переписати розділ gui
tools/bench/gui-bench.sh --keep            # лишити тимчасову теку (логи, знімки)
tools/bench/gui-bench.sh --binary PATH     # інший бінарник (типово $CARGO_TARGET_DIR/release/slicenv)
```

Що робить:

1. Генерує STL сфери 1 М △ (`tools/bench/sphere_stl.py`, 50 МБ) у тимчасовій теці.
2. Ізолює **всі чотири** `XDG_CONFIG_HOME` / `XDG_DATA_HOME` / `XDG_CACHE_HOME` /
   `XDG_STATE_HOME` у тимчасових теках. Конфіг користувача (`~/.config/slicenv`, інакше
   `~/.config/bambu-lab`) копіюється одразу в тимчасовий `config/slicenv` без `mcp-token`,
   з `left_drag_moves=true` і `mcp_enabled=false`. Міграція `bambu-lab → slicenv` тому
   ніколи не бачить справжніх тек; наприкінці скрипт звіряє список справжніх тек продукту
   до й після і повертає 3, якщо щось змінилось.
3. Піднімає власний Xvfb 1600 × 1000 на першому вільному дисплеї від `:93`, запускає
   застосунок із `SLICENV_PROFILE=1 SLICENV_PROFILE_ALL=1 SLICENV_CLOUD_OFF=1`, чекає
   першого кадру й тиші в лозі.
4. Перетягування: натискання на сфері (`DRAG_XY`, типово `790 540`), 80 кроків по (3, 1)
   через 30 мс. Орбіта: з порожнього місця над столом (`ORBIT_XY`, типово `450 300`),
   120 кроків по (4, 1) через 25 мс. Спершу перетягування — сфера ще там, де її
   поставило відкриття.
5. `tools/bench/stats.py` бере лише spans між початком і кінцем жесту, друкує
   n/avg/p50/p95/max для кожного й порівнює бюджетовані з розділом `gui`.
6. Зупиняє тільки свої процеси за PID (ні `pkill -f`, ні чужі `slicenv`/`Xvfb`).

Прогін недійсний (вихід 2), якщо жест не зробив свого: в орбіті менше 30 кадрів або
є `live_drag_begin` (натиснули на об’єкт); у перетягуванні немає `live_drag_begin`
чи `project_changed`. Змінилась розкладка — перевір точки знімками з `--keep`
(`out/start.png`, `out/drag.png`, `out/orbit.png`) і задай `DRAG_XY`/`ORBIT_XY`.

Межа: `min(budget_ms, max(2 × baseline, baseline + 0.05))`. Spans з менш ніж 5
зразками за прогін (відпускання після перетягування, `live_drag_begin`) порівнюються
лише з абсолютним `budget_ms` — для одного зразка 2× — це шум.

| Фаза / span | baseline avg, мс | межа, мс | Абсолютний бюджет |
|---|---:|---:|---|
| орбіта `viewport::frame` | 3.0 | 6.0 | 16.7 (60 к/с) |
| орбіта `Workspace::render` | 0.28 | 0.57 | 8 |
| перетягування `viewport::frame` | 2.9 | 5.8 | 16.7 |
| перетягування `Workspace::render` | 0.28 | 0.56 | 8 |
| `live_drag_begin` | ≈0.01 | 1.0 | 1.0 |
| відпускання: `refresh_plate_mesh` | ≈0.06 | 2.0 | 2.0 |
| відпускання: `project_changed` | ≈0.8 | 2.0 | 2.0 |

Xvfb не має DRI3, але wgpu рендерить поза екраном через Vulkan напряму, тож це
справжній GPU-час. Числа чутливі до навантаження хоста: при load ≈ 25 орбіта давала
4.7 мс замість 3.0. Baseline записуй на спокійній машині.

## Орбіта Preview (G-code)

```bash
# Без зовнішніх фікстур: 636 шарів синтетичної ажурної оболонки.
cargo run --release -p perf -- --only Preview
# Справжня нарізана angel_medium; baseline синтетики тут не порівнюємо.
SLICENV_PERF_GCODE=/tmp/pp/angel.gcode cargo run --release -p perf -- --only Preview --invariants-only
# Повний шлях UI → GPU → readback, зі знімками й ізольованим конфігом.
tools/bench/gui-bench.sh --preview /tmp/pp/angel.gcode --keep
```

`perf` окремо міряє одноразову побудову/завантаження, орбіту з LOD, без LOD і після
25 мс простою (зміна частоти iGPU). Для стандартної орбіти й кадру після простою
абсолютний бюджет — 16.7 мс, разом із відносним 2× від baseline. Оновлення baseline
зберігає `budget_ms`. Ціль на ненавантаженій Radeon 780M — близько 3 мс.

У GUI фаза `preview_orbit`: ≥30 `viewport::frame`, ≥20 `viewer::gpu_frame`, без
`live_drag_begin`. `viewer::gpu_frame` — час від подання до обробки готового readback;
включає затримку опитування UI, а не лише виконання шейдера. CPU-span `viewport::frame`
сам по собі не показує затримки асинхронного GPU. Бюджети: кадр і GPU — 16.7 мс,
`Workspace::render` — 8 мс, також діє відносний поріг 2×.
Для повільного початкового рендерера збільшуй тривалість жесту через
`PREVIEW_ORBIT_DELAY=0.1` (ті самі 120 позицій, довший інтервал); обидві версії порівнюй
з однаковим інтервалом. Недостатня кількість кадрів — недійсний прогін, а не успіх.

PNG без вікна: `cargo run --release -p viewer --example gcode_preview -- FILE OUT.png`.
`CAM="yaw,pitch,distance[,x,y,z]"` задає однаковий крупний план; `NO_LOD=1` вимикає
спрощення, `FLAT_LINES=1` дає неосвітлену схему кольорів. LOD має допуск 0.05 мм,
працює лише на дальньому плані; краї діапазонів шарів/рухів завжди у повній деталізації.

## Тестовий seam: стан UI

`Workspace::ui_state` (`crates/app/src/ui_state.rs`) збирає один JSON: сторінка, мова,
камера `[yaw, pitch, distance, tx, ty, tz]`, прямокутник в’юпорта, вибір і його
рамки, повзунки Preview, перетин, налаштування, рушій. Окремий об’єкт `shell` — оболонка:
режим (`home/prepare/preview/device/project`), стиснення панелі режимів і дій проєкту
(`project_bar`: `full … stacked`, `null` поза сторінками проєкту), її рамка й ширина центру,
доки, стан нарізки й друку, відкрита модалка. Два споживачі:

- `SLICENV_UI_TRACE=1` — рядок `UI_TRACE {json}` у stderr **лише коли стан змінився**
  (перевірка на кінці малювання в’юпорта й кадру вікна). Без змінної цикл кадру платить одне читання
  статичного прапорця. Скрипти `crates/app/verification/*.py` беруть останній рядок,
  тож формат лишився той самий. Пакунки окремо пишуть `PACKAGE_TRACE`.
- MCP-інструмент `ui_state` (read-only) — той самий об’єкт на запит. Є лише в
  застосунку з увімкненим MCP (Налаштування → «MCP (AI-агенти)»); `slicenv-mcp` без вікна його не
  показує ([MCP.md](MCP.md)). Скрипту не треба парсити лог: `tools/call ui_state`.

## Приймальні сценарії UI

`crates/app/verification/*.py` — xdotool-сценарії на ізольованому Xvfb. Аргументи,
фікстури й розмір вікна описано в docstring кожного (`python3 -I <script> --help`).
Вони не входять у CI: потрібні реальні моделі (`angel_medium.3mf`), пакети рушіїв,
мережа (`packages.py` завантажує Orca ~204 МБ) і фіксовані піксельні координати.

`project_bar.py` — панель режимів і дій проєкту (`crates/app/src/project_bar.rs`): перемикання
Проєкт/Підготовка/Перегляд кліком і клавішами, відсутність панелі на «Головній» і
«Пристрої», жести на панелі не рухають камеру, доки й режими не зсувають панель з осі
вікна, ширини вікна 1560 → 960 (панель ціла, в межах центру, поверх верху сцени),
`--scale` для `GPUI_X11_SCALE_FACTOR=1.25/1.5`. Моделі досить
будь-якої (кубик STL); з рушієм у тимчасовому `XDG_DATA_HOME` сценарій ще нарізає й
відкриває діалог друку фіктивного принтера, не підтверджуючи його.

Фіктивні дані для «Пристрою» й майстра — без мережі, плагіна й принтерів (лише разом із
`SLICENV_CLOUD_OFF=1` / `SLICENV_LAN_OFF=1`):

| Змінна | Що дає |
|---|---|
| `SLICENV_FAKE_PRINTER=1` (`=idle`) | один хмарний принтер зі статусом, історією, мініатюрою |
| `SLICENV_FAKE_PRINT_MODEL=<код>` | разом із `SLICENV_FAKE_PRINTER`: діалог друку вважає, що файл нарізано для цієї моделі (`N7`, `C12`…) — попередження про іншу модель |
| `SLICENV_FAKE_PRINTERS=N` | ще N−1 фіктивних принтерів (масштаб списку доку) |
| `SLICENV_FAKE_FOUND=N` | N «знайдених у мережі» Bambu (LAN вимкнено) |
| `SLICENV_FAKE_CLOUD=cloud-off\|login\|no-server\|connecting` | стан рядка Bambu Cloud |
| `SLICENV_FAKE_PRINT_ROUTE=<стан>[+abs]` | діалог «Надіслати на друк» фіктивного принтера в заданому стані маршрутів (`print_paths::demo`: `connect`, `lan`, `lan-offline`, `lan-found`, `cloud`, `cloud-setup`, `cloud-connecting`, `cloud-offline`, `open-plugin`, `none`, `none-download`); `+abs` — ще й попередження про матеріал. Головна кнопка лише закриває діалог |
| `SLICENV_WIZARD_DEMO=login\|session\|plugin` | стани форми входу в майстрі |
| `SLICENV_SKIN=gnome\|zed` | форми й палітра незалежно від середовища: `gnome` — заголовок, вкладки й плаваючі панелі як у libadwaita з палітрою Adwaita (типово лише коли `XDG_CURRENT_DESKTOP` містить `GNOME`), `zed` — щільний вигляд Zed |

## Порівняння G-code з оригінальним рушієм

`tools/gcode-diff.py` перевіряє, що Slicenv дає той самий G-code, що й GUI Bambu Studio чи
OrcaSlicer. Вхід — `.gcode` або `.gcode.3mf` (з архіву береться `Metadata/plate_N.gcode`,
кілька пластин — `--plate N`). Лише стандартна бібліотека Python.

1. У Bambu Studio (OrcaSlicer) відкрити проєкт, нарізати пластину, «Файл → Експорт →
   Export plate sliced file» → `orig.gcode.3mf`.
2. Той самий проєкт з тим самим принтером, філаментом і процесом нарізати в Slicenv:
   «Перегляд» → «Експортувати…» → `slicenv.gcode.3mf`.
3. Порівняти:

```bash
python3 -I tools/gcode-diff.py orig.gcode.3mf slicenv.gcode.3mf          # звіт
python3 -I tools/gcode-diff.py orig.gcode.3mf slicenv.gcode.3mf --json   # для скриптів
python3 -I tools/gcode-diff.py A B --ignore-key 'filament_settings_id' --all
```

Що відкидається як шум: заголовок з версією й датою, мініатюри (`THUMBNAIL_BLOCK`,
`thumbnail_* begin/end`), абсолютні шляхи Unix і Windows, дати й час у коментарях,
службові коментарі поза блоками, маркери прогресу `M73` (`--keep-m73` — лишити).
Порівнюються:

- **CONFIG_BLOCK** — параметри, з якими різав рушій. Друкується першим: різні параметри —
  головна причина різного G-code (інший пресет, перевизначення в 3MF, версія рушія).
- **Підсумки** — шари, оцінка часу, довжина й вага філаменту (заголовок Bambu або
  підсумки Orca наприкінці файлу), Z кожного шару.
- **Траєкторії** — пошарово (`; CHANGE_LAYER`), перший різний шар і рядки в обох файлах,
  кількість різних рядків і шарів.

Вердикт (код виходу 0 / 1, помилка входу — 2): `identical` — ідентичні після
нормалізації; `config_differs` — список різних параметрів; `trajectory_differs` —
параметри однакові, траєкторії ні.

**Рушії не однаково детерміновані.** Перевірено 2026-11-09 на тому самому 3MF (A1,
0.20mm Standard, 200 шарів) з тимчасовим HOME:

| Рушій | Два прогони CLI | Результат |
|---|---|---|
| OrcaSlicer 2.4.2 | `identical` | 49 241 рядок однаковий |
| Bambu Studio 02.07.01.62 (3 прогони попарно) | `trajectory_differs` | 162–224 рядки з ~43 000 (0.4–0.5 %) у 14–23 верхніх шарах: інша точка старту шва чи дуги, `F` того самого руху; оцінка часу ±3 с; шари, Z і філамент однакові |

Тож для Bambu Studio «ідентичні» недосяжне навіть проти самого себе. Збіг означає:
CONFIG_BLOCK однаковий, шари, Z і філамент однакові, різних рядків менше 1 % — скрипт
тоді додає примітку. Більша частка чи різні шари або філамент — справжня розбіжність.

Тест на синтетичних файлах (`tools/fixtures/gcode-diff/`, у CI не входить):
`python3 -I tools/test_gcode_diff.py`.

## CI

`.github/workflows/ci.yml` (ubuntu-latest, кеш `Swatinem/rust-cache`):

| Job | Кроки |
|---|---|
| `translations` | `check_translations.py` |
| `headless` | `paths project settings slicer gcode geometry mcp cloud` не тягнуть `wgpu`/`gpui` (`cargo tree`), потім `cargo test` цих крейтів без GUI-бібліотек |
| `workspace` | системні бібліотеки GPUI → `cargo clippy --workspace --all-targets -- -D warnings` з поіменним allow-списком → `cargo test --workspace` (GPU-тести — SKIP) → `cargo run --release -p perf -- --invariants-only` |

Свідомо пропущено:

- **`cargo fmt --check`** — дерево не відформатоване (170 фрагментів у 26 файлах на
  2026-11-04). Масове `cargo fmt` — окремим комітом, тоді додати крок.
- **Clippy `-D warnings` без винятків** — 37 наявних попереджень 13 лінтів
  (`collapsible_if` ×12, `needless_pub_self` ×5, …; справжніх багів серед них немає,
  `suspicious_assignment_formatting` у `preferences.rs:186` — навмисне `*field=!*field`).
  Вони дозволені поіменно в `CLIPPY_ALLOW`, усе нове — помилка. Прибрав лінт — прибери
  його зі списку.
- **Числа perf проти baseline і GUI-бенч** — baseline машинозалежний, а на раннері
  немає GPU й Xvfb-сценарію. Запускати локально перед злиттям змін рендеру чи правок.
- **`verification/*.py`** — див. вище.

Перевірити clippy локально, як у CI (bash — zsh не розбиває змінну на слова):

```bash
bash -c 'cargo clippy --workspace --all-targets -- -D warnings \
  $(sed -n "/CLIPPY_ALLOW: >-/,/^$/p" .github/workflows/ci.yml | grep -o -- "-A clippy::[a-z_]*" | tr "\n" " ")'
```

### Бюджет парсингу G-code

`cargo run --release -p perf -- --no-gpu --only gcode` вимірює 800 тис. G1
у пам’яті, без I/O та знищення Toolpath. Для великої локальної нарізки:

```sh
GCODE_PERF_REFERENCE=~/.cache/slicenv/sliced/angel_medium-plate1/plate_1.gcode \
cargo run --release -p perf -- --no-gpu --only gcode
```

Окремі метрики `gcode::parse 800k` та `gcode::parse reference` мають абсолютний
бюджет p50 `max_ms` у bench/baselines.json (125 і 160 мс). Значення max_ms
має пріоритет над глобальним tolerance і зберігається при --update-baseline.
Межі машинозалежні; подробиці звірки — PREVIEW_STATS.md.
