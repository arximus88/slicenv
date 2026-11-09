# MCP: керування слайсером з AI-агентів

Крейт `crates/mcp` — сервер [Model Context Protocol](https://modelcontextprotocol.io) (stdio) на офіційному
Rust SDK `rmcp 3.8`. Claude Code, Claude Desktop та інші клієнти відкривають 3MF, читають профілі й налаштування,
редагують розкладку, нарізають пластину та читають статистику G-code.

## Архітектура

Два транспорти використовують спільний шар інструментів `Session` через `host::Host`:

```text
stdio → Server → Host (Mutex<Session>) → власний проєкт та History
HTTP  → Server → Host (GUI Bridge) → async_channel → GPUI Workspace
```

`Server` запускає синхронні host-виклики через `spawn_blocking`. GUI-міст надсилає завдання обмеженим
асинхронним каналом; `cx.spawn_in` приймає їх у головному потоці. Серверний потік не має посилання на
Workspace. Інструменти використовують короткоживучий знімок Session (сітки Arc-shared), а успішні зміни
повертаються через `Workspace::checkpoint` та `project_changed`: сітки, Preview і viewport оновлюються,
Ctrl+Z і MCP undo/redo використовують спільну історію.
`open_project` завершується через спільний з UI `install_open_project`: скидає історію, вибір, камеру та застосовує профілі нового файлу. Невдала зміна не застосовується.
`slice_plate` забирає знімок із GUI, готує й запускає нарізку в робочому потоці.

Нативний cloud-плагін у stdio-процесі не пише в протокольний stdout: `main.rs` використовує приватну
копію fd 1 і перенаправляє вивід плагіна до `/dev/null`.

### Безпека

* HTTP слухає тільки `127.0.0.1`. Кожен запит (включно з initialize і GET) потребує `Authorization: Bearer TOKEN`.
  Будь-який Origin, крім http(s) loopback IP / localhost, повертає 403; без токена — 401.
* 256-бітний випадковий токен зберігається у `$XDG_CONFIG_HOME/slicenv/mcp-token` (типово `~/.config/slicenv/mcp-token`),
  права 0600. Регенерація атомарно замінює файл і перезапускає сервер, закриваючи старі MCP-сесії.
* HTTP і редагування вимкнені за замовчуванням. `open_project` у GUI теж потребує дозволу редагування,
  оскільки замінює відкритий проєкт; у headless це початкове завантаження власної копії.
* **Лише читання за замовчуванням.** Без `--allow-edit` інструментів зміни немає в `tools/list`, а `Session` відхиляє їх і при прямому виклику.
* Інструментів керування принтером немає. `printer_status` — тільки читання (сесія Bambu Studio → один знімок телеметрії;
  `allow_control` лишається `false`). Без збереженої сесії повертає зрозумілу помилку.
* `open_project` приймає лише `.3mf`/`.stl`, `gcode_stats` — `.gcode`/`.3mf`, `save_project` — лише `.3mf` і тільки явний шлях
  (оригінал не перезаписується мовчки).

## Інструменти

Довжини — мм, час — секунди. Помилка інструмента — результат із `isError: true` і текстом українською, не збій протоколу.

| Інструмент | Аргументи | Результат |
|---|---|---|
| `open_project` | `path` (`.3mf`/`.stl`) | як `project_summary`; скидає undo |
| `project_summary` | — | пластини, об’єкти (`position`, `rotation_deg`, `scale`, `bounds_min/max`, `size_mm`, `triangles`), профілі проєкту, `unsaved_changes`, `can_undo/can_redo` |
| `list_profiles` | `kind`: `machine`\|`process`\|`filament`; `machine?`; `contains?` | `names` сумісних профілів (за замовчуванням — принтер проєкту) |
| `get_profile` | `kind`, `name`, `keys?: string[]`, `contains?` | розгорнутий (`inherits` розкритий) профіль; фільтр за ключами |
| `explain_setting` | `key` | назва, опис (en/uk), тип, межі, типове, enum, `documentation_url`; для опечатки — схожі ключі |
| `gcode_stats` | `path` (`.gcode`/`.gcode.3mf`) | шари, час, філамент (г/м/см³), межі, довжина екструзії за ролями |
| `slice_plate` | `plate`, `machine?`, `process?`, `filament?` | час, г філаменту, шари, висота шару, `warnings`, шлях до `.gcode.3mf` |
| `printer_status` | `printer?` | стан, прогрес, температури, шари (потрібна сесія Bambu Studio) |
| `ui_state` | — | лише застосунок (не `slicenv-mcp`): знімок стану вікна — сторінка, камера, прямокутник в’юпорта, вибір і його рамки, повзунки Preview, налаштування; той самий JSON, що `SLICENV_UI_TRACE` (див. `docs/TESTING.md`) |
| `transform_object` ✎ | `object`, `instance?`, `scale?[3]`, `rotate_deg?[3]`, `translate?[3]`, `center_on_plate?`, `drop_to_bed?` | оновлений об’єкт; операції — відносні, у цьому порядку |
| `arrange_plate` ✎ | `plate`, `spacing_mm?` (5) | об’єкти пластини після розкладання (стіл — з профілю принтера) |
| `orient_plate` ✎ | `plate` | об’єкти пластини після автоорієнтації (`project::orient`, алгоритм Orca); заблоковану пластину відхиляє |
| `undo` / `redo` ✎ | — | `{applied, label}` |
| `save_project` ✎ | `path` (`.3mf`) | `{saved}` |

✎ — лише з `--allow-edit`. JSON-схеми аргументів віддає `tools/list` (генеруються з Rust-структур у `session.rs`).
`orient_plate` використовує той самий `Project::orient_instance`, що й кнопка Q в апці; на Phone Stand дає 95.23 × 80.75 × 98 мм — як `orca-slicer --orient 1`.

## Підключення до відкритого вікна

Ctrl+, → **MCP (AI-агенти)** → увімкни сервер. Статус показує фактичний порт (типово 7357, 0 — випадковий).
Окремий перемикач дозволяє редагування; зміни порту, дозволу й токена перезапускають HTTP-сесії.
Токен замасковано; кнопки копіюють повний токен, команду Claude Code та JSON із заголовком авторизації.

```sh
claude mcp add --transport http slicenv http://127.0.0.1:7357/mcp --header "Authorization: Bearer TOKEN"
```

```json
{"mcpServers":{"slicenv":{"type":"http","url":"http://127.0.0.1:7357/mcp","headers":{"Authorization":"Bearer TOKEN"}}}}
```

JSON підходить `.mcp.json` та клієнтам із підтримкою Streamable HTTP. Для Claude Desktop без підтримки
віддалених HTTP-серверів використай stdio-конфіг нижче (вкладка також копіює його з шляхом до бінарника,
якщо він знайдений поряд із застосунком або у PATH). HTTP бачить відкритий проєкт; stdio має власну копію.

## Headless-підключення

```bash
cargo install --path crates/mcp --locked          # кладе slicenv-mcp у ~/.cargo/bin
claude mcp add slicenv -- slicenv-mcp                 # лише читання
claude mcp add slicenv-edit -- slicenv-mcp --allow-edit   # з редагуванням
```

Або вручну: `.mcp.json` у корені репозиторію (у git не входить, Claude Code запитає дозвіл при першому запуску):

```json
{ "mcpServers": { "slicenv": { "command": "slicenv-mcp", "args": ["--allow-edit"] } } }
```

Під час розробки замість `slicenv-mcp` можна вказати `target/release/slicenv-mcp` — шлях відносно кореня репозиторію. Для Claude Desktop — той самий блок
`mcpServers` у `claude_desktop_config.json`. Слайсер береться з `PATH`/`/opt` або змінних `BAMBU_STUDIO`, `ORCA_SLICER`.

### Приклад (реальний запуск Claude Code, `--allow-edit`)

```
> Open Phone+Stand+v3.3mf, tell me object size, scale to 50% (drop to bed, center), explain sparse_infill_density.
CALL mcp__slicenv__open_project      {"path": "~/Downloads/Phone+Stand+v3.3mf"}
CALL mcp__slicenv__explain_setting   {"key": "sparse_infill_density"}
CALL mcp__slicenv__transform_object  {"object": 2, "scale": [0.5,0.5,0.5], "drop_to_bed": true, "center_on_plate": true}
Claude: Об'єкт «Iphone Stand v3.stl» мав розмір 95.23 × 106.64 × 79.8 мм … тепер 47.62 × 53.32 × 39.9 мм, стоїть на столі
        (Z = 0) і відцентрований на пластині 1. Зміни поки не збережені у файл.
```

`slice_plate` на тому ж файлі (Bambu Studio 02.08, X1C, `0.20mm Standard`): 4845 с, 49.88 г, 399 шарів; після масштабу 50% і `0.12mm Fine`: 2243 с, 11.51 г, 332 шари.

## Тести

`cargo test -p mcp`: `tests/tools.rs` — шар інструментів на 3MF із двох кубів (зміни, undo/redo, атомарність,
розкладання, збереження, профілі з фейкового каталогу, G-code); `tests/stdio.rs` — справжній процес:
initialize → tools/list → tools/call (у т.ч. відсутність інструментів зміни без `--allow-edit`, чистота stdout, реальний
`~/Downloads/Phone+Stand+v3.3mf`, якщо є).

`tests/http.rs` перевіряє host-інтерфейс і справжній HTTP на випадковому порту: initialize → tools/list → project_summary, 401 без токена і 403 для зовнішнього Origin. Unit-тест HTTP перевіряє регенерацію токена й права 0600.

## Далі (за пріоритетом)

2. **`lay_on_face`** як інструмент (автоорієнтація вже є: `orient_plate`).
3. **Редагування налаштувань:** `set_overrides` (per-object/per-plate/процес) з валідацією через `settings::Schema`, undo і передача в `slice_plate`.
4. **Ресурси MCP:** `bambu://project`, `bambu://profile/{kind}/{name}`, `bambu://setting/{key}` — клієнт може прикріпити їх без виклику інструмента.
5. **Обмеження шляхів** (`--root DIR`) і підтвердження запису поза коренем; журнал викликів.
6. **Прогрес і скасування `slice_plate`** (`notifications/progress`, `notifications/cancelled`) — нарізка може тривати хвилини.
7. **Принтер:** кеш клієнта `cloud` між викликами (зараз кожен `printer_status` вантажить плагін), потім — керування, але лише за
   окремим прапорцем і з підтвердженням (elicitation) на кожну дію.
8. Знімок в’юпорта як `image`-контент (`render_plate`) — агент «бачить» розкладку.

## Перевірка GUI на Xvfb

В ізольованому `XDG_CONFIG_HOME` (лише копія `engines.json`, `SLICENV_CLOUD_OFF=1`), дисплей `:94`:

```text
Preferences → MCP → увімкнути сервер: Слухає 127.0.0.1:7357
curl без Authorization → HTTP 401
initialize → notifications/initialized → tools/call project_summary
object 2 size_mm: [95.23, 106.64, 79.8]
Preferences → дозволити редагування (перезапуск сесій)
tools/call transform_object {"object":2,"scale":[0.5,0.5,0.5]}
object 2 size_mm: [47.62, 53.32, 39.9]; viewport показує меншу модель
Ctrl+Z → project_summary: [95.23, 106.64, 79.8]; viewport відновлено
```

Артефакти перевірки: `/tmp/mcp-prefs.png`, `/tmp/mcp-viewport-before.png`,
`/tmp/mcp-viewport-scaled.png`, `/tmp/mcp-viewport-undo.png`, `/tmp/mcp-curl-transcript.txt`
(токен замінено на `TOKEN`). `cargo test --workspace`: 224 passed, 3 ignored, 0 failed.
