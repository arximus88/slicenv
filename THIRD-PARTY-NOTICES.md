# Сторонній код, ресурси й залежності Slicenv

Slicenv розповсюджується під **GNU Affero General Public License v3.0 or later**
(`AGPL-3.0-or-later`, повний текст — [`LICENSE`](LICENSE)). Нижче перелічено, що
в репозиторії й пакетах походить від інших проєктів і під якими умовами.

## 1. Код, портований або перенесений з інших слайсерів

Bambu Studio й OrcaSlicer — форки PrusaSlicer (який походить від Slic3r); усі три
розповсюджуються під AGPL-3.0. Перелічені нижче файли Slicenv є похідними творами цих
проєктів і разом з усім застосунком ліцензуються під AGPL-3.0-or-later. Авторські права
на перенесені алгоритми належать авторам Slic3r, PrusaSlicer (Prusa Research),
Bambu Studio (Bambu Lab) та OrcaSlicer (SoftFever і учасники).

| Файл Slicenv | Джерело | Що саме |
|---|---|---|
| `crates/settings/src/rules.rs` | Bambu Studio 79df7ae, `src/slic3r/GUI/ConfigManipulation.cpp` | ручний порт `toggle_print_fff_options` (доступність і видимість полів) |
| `crates/settings/schema/**`, `crates/settings/schema-orca/**` | Bambu Studio / OrcaSlicer: `src/libslic3r/PrintConfig.{cpp,hpp}`, `Preset.cpp`, `Tab.cpp`, `bbl/i18n/uk/BambuStudio_uk.po` | метадані опцій, сторінки налаштувань і український переклад, згенеровані `crates/settings/tools/generate.py` |
| `crates/gcode/src/time.rs` (і використання в `model.rs`, `report.rs`) | Bambu Studio, `GCodeProcessor::TimeMachine` (походить від PrusaSlicer) | порт планувальника оцінки часу друку |
| `crates/gcode/src/report.rs` | Bambu Studio, `ExtrusionEntity::role_to_string` | назви ролей ліній як ключі статистики |
| `crates/gcode/src/view.rs` | OrcaSlicer / PrusaSlicer | колірна шкала «синій → червоний» і схеми кольорів перегляду |
| `crates/viewer/src/measure.rs`, `crates/viewer/src/measure/analysis.rs`, `crates/viewer/src/measure/pairs.rs` | OrcaSlicer `src/libslic3r/Measure.cpp` (походить від PrusaSlicer) | порт `MeasuringImpl::extract_features`, `circle_ransac`, `get_measurement` |
| `crates/viewer/src/shader.wgsl` | OrcaSlicer / PrusaSlicer, шейдер `gouraud_light` | модель освітлення |
| `crates/viewer/src/plate_icons.rs` | OrcaSlicer `PartPlate::calc_vertex_for_icons` | порядок і розкладка кнопок пластини |
| `crates/app/src/pattern_icons.rs` | OrcaSlicer v2.4.2 `src/slic3r/GUI/Tab.cpp` | відповідність груп налаштувань іконкам |
| `crates/project/src/orient.rs` | OrcaSlicer `src/libslic3r/Orient.cpp` | ідеї авто-орієнтації (кандидати нормалей, оцінка звисів); не дослівний порт |
| `crates/cloud/native/print_params.hpp`, `crates/cloud/native/task_params.hpp` | Bambu Studio v02.06.00.51, `bambu_networking.hpp` / `NetworkAgent.hpp` | розкладка структур ABI мережевого плагіна (для сумісності) |
| `crates/cloud/src/account.rs`, `crates/cloud/src/lib.rs`, `crates/cloud/src/plugin.rs`, `crates/cloud/native/shim.cpp` | OrcaSlicer `HttpServer`, `WebUserLoginDialog`, `download_plugin` | формати payload і відповідей (інтероперабельність, не копіювання коду) |

Багато інших файлів містять коментарі «як у Bambu Studio / Orca» — це опис поведінки
інтерфейсу, яку Slicenv відтворює власним кодом, а не перенесений код.

## 2. Вшиті ресурси

| Шлях | Походження | Ліцензія |
|---|---|---|
| `crates/app/assets/fonts/IBMPlex*.ttf` | IBM Plex Sans / Mono, © 2017 IBM Corp., Reserved Font Name «Plex» | SIL Open Font License 1.1 — [`crates/app/assets/fonts/LICENSE.txt`](crates/app/assets/fonts/LICENSE.txt) |
| Іконки інтерфейсу (`IconName::*` з `gpui-component`) | Lucide, вшиті крейтом `gpui-kit-assets` 0.8.1 | ISC (Lucide), Apache-2.0 (крейт); текст — у `THIRD-PARTY-LICENSES.txt` пакета (`LICENSE-LUCIDE`) |
| `crates/app/assets/patterns/*.svg`, `crates/app/assets/sections/*.svg` | OrcaSlicer v2.4.2, `resources/images/` (кольори замінено на `currentColor`) | AGPL-3.0 |
| `crates/viewer/assets/plate/*.svg` | OrcaSlicer v2.5.0-beta, `resources/images/plate_*.svg`, без змін | AGPL-3.0 |
| `crates/app/assets/help/*.png` | Bambu Studio, `resources/tooltip/images/`, без змін | AGPL-3.0 |
| `crates/app/assets/engines/bambu.png`, `orca.png` | іконки Bambu Studio й OrcaSlicer (`resources/images/*_32px.png`) | AGPL-3.0; назви й логотипи — торговельні марки їхніх власників, використані лише для позначення зовнішнього рушія |
| `crates/lan/assets/bbl-ca.pem` | публічні кореневі/проміжні CA Bambu Lab з `resources/cert/printer.cer` Bambu Studio | публічні сертифікати (факти для перевірки TLS принтера) |
| `crates/cloud/assets/slicer_base64.cer` | публічний сертифікат `resources/cert/slicer_base64.cer` Bambu Studio | публічний сертифікат |
| `crates/app/assets/app/slicenv.svg` | власна іконка Slicenv | AGPL-3.0-or-later |

Тестові сертифікати й ключі в `crates/lan/tests/fixtures/` згенеровані для тестів і в
пакети не потрапляють.

## 3. Rust-залежності

Перелік крейтів, що лінкуються в `slicenv` і `slicenv-mcp`, з ліцензіями (SPDX) —
[`packaging/THIRD-PARTY-RUST-CRATES.md`](packaging/THIRD-PARTY-RUST-CRATES.md).
Його генерує `tools/third-party-licenses.py` з `cargo metadata --locked` (cargo-about
не використовується). Повні тексти ліцензій крейтів (LICENSE/COPYING/NOTICE із пакета
кожного крейта) `tools/package.sh` кладе в кожен артефакт як
`THIRD-PARTY-LICENSES.txt`.

Переважають MIT / Apache-2.0 / BSD / ISC / Zlib / Unicode-3.0 — усі сумісні з
AGPL-3.0. Окремо варто знати:

- `self_cell` — `Apache-2.0 OR GPL-2.0-only`; використовується за Apache-2.0.
- `option-ext` — `MPL-2.0` (файлова copyleft, сумісна з AGPL; використовується без змін,
  джерельний код — на crates.io).
- `manifold-csg-sys` статично збирає C++-бібліотеку **manifold3d** v3.5.3
  (Apache-2.0) з її залежностями (Clipper2 — BSL-1.0).
- `gpui-pre*` (знімок GPUI із Zed) і `gpui-component` — Apache-2.0.

## 4. Закритий мережевий плагін Bambu Lab — НЕ постачається

Хмарні функції Bambu Cloud працюють через пропрієтарну бібліотеку
`libbambu_networking.so` від Bambu Lab. **Жоден пакет Slicenv її не містить і не
перерозповсюджує.** Застосунок завантажує плагін із серверів Bambu Lab лише після явної
згоди користувача (кнопка «Погоджуюсь — завантажити» в майстрі першого запуску) і
зберігає його в `~/.local/share/slicenv/bambu-plugin/`; також може використати плагін,
уже встановлений Bambu Studio / OrcaSlicer (`~/.config/BambuStudio/plugins`,
`~/.config/OrcaSlicer/plugins`). Плагін підключається через `dlopen` під час виконання
й не є частиною твору Slicenv; на нього поширюється ліцензія Bambu Lab. Щоб відкликати
згоду, достатньо видалити теку `bambu-plugin`. Режим LAN (`crates/lan`) працює без плагіна й без акаунта.

Рушії нарізки (OrcaSlicer, Bambu Studio CLI) також не входять до пакетів: їх
завантажує менеджер рушіїв (`crates/slicer/tools/manage_engines.py`) з офіційних
релізів за дією користувача. Вони розповсюджуються їхніми авторами під AGPL-3.0.
