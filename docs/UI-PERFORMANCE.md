# Профілювання редагування параметрів

Перевірка 2026-11-02: release, Xvfb :84 (1600×1000), Vulkan / Radeon 780M,
Orca 2.4.2, A1 0.4 / PLA Basic / 0.20 Standard. Синтетична STL-сфера має
100 352 трикутники (224×224 комірки, два трикутники на комірку).

`SLICENV_PROFILE=1` додає RAII spans у Workspace, редактор пресетів, правила,
валідацію, selected bounds, список об’єктів, перебудову mesh та viewport.
Виводяться лише spans >8 мс. Для повного журналу, включно з кількістю
GPU submit, додати `SLICENV_PROFILE_ALL=1`. Обидва прапорці читаються один раз;
без профілювання виклики не читають годинник. Poll readback — CPU-час
неблокуючого poll; submit — CPU-час submission, а не повний час GPU-кадру.
Workspace render не включає layout/paint GPUI.

## Відтворення

```sh
cargo build --release
cargo test --workspace
python3 tools/profile-ui-fixture.py
Xvfb :84 -screen 0 1600x1000x24 &
xvfb_pid=$!
env -u WAYLAND_DISPLAY DISPLAY=:84 SLICENV_CLOUD_OFF=1 \
  SLICENV_PROFILE=1 SLICENV_PROFILE_ALL=1 \
  ./target/release/slicenv /tmp/bambu-perf-100k.stl >> /tmp/app-profile.log 2>&1 &
app_pid=$!
# Дочекатися повного завантаження моделі й профілів.
python3 tools/profile-ui-replay.py /tmp/app-profile.log
DISPLAY=:84 import -window root /tmp/perf-x.png
kill "$app_pid" "$xvfb_pid"
```

Replay розрахований на початкові доки та вкладку Quality, координати при
1600×1000. Він редагує layer height (Enter), п’ять разів перемикає checkbox,
12 разів перемикає вкладки, п’ять разів вибирає/знімає вибір об’єкта.
Idle CPU рахується через різницю utime+stime `/proc/PID/stat` за 5 секунд,
100% — одне ядро. `SLICENV_REPLAY_DELAY=2` збільшує паузу між кліками на
перевантаженій машині. Для порівняння використовувати однакову паузу.
Журнал відкривати через `>>`, щоб SCENARIO-маркери не перезаписувалися.

## Результати до / після

| Сценарій | GPU submit до → після | Workspace render, середнє мс до → після | Editor render, середнє мс до → після |
|---|---:|---:|---:|
| Layer height + Enter | 1 → 0 | 0.655 → 3.278 | 0.316 → 0.367 |
| 5 checkbox clicks | 5 → 0 | 0.656 → 3.004 | 0.318 → 0.388 |
| 12 перемикань вкладок | 12 → 0 | 0.696 → 1.372 | 0.313 → 0.629 |
| 5 виборів + 5 зняттів вибору | 10 → 11 | 0.700 → 2.911 | 0.318 → 0.349 |
| Idle CPU, одне ядро | 0.20% → 0.60% | — | — |

Це **не доказ зменшення end-to-end latency**. Після baseline хост був
перевантажений чужими паралельними компіляціями (load average 75–95 на 16
логічних CPU, CPU pressure >90%). Тому CPU-часи після вищі; під час раннього
replay також зливалися кліки. Фінальний повтор робив паузи 2 с замість 0.3 с,
щоб перевірити кожен клік. Надійний результат тут — усунення GPU-submit від
редагування налаштувань. 11 кадрів selection включають зміну hover;
сцена продовжує оновлюватися при реальному виборі. Idle у фінальному повторі
не містить Workspace render / GPU submit. Профілювання всіх spans саме додає
логування на UI-потоці.

Додаткові native перевірки:

| Сценарій | До | Після |
|---|---|---|
| Filament diameter | Клік закривав модалку; наступні цифри могли рухати камеру | 1.75 → 1.76, dirty; 0 GPU-submit |
| Printer printable height | Та сама втрата модального редактора | 256 → 255, dirty; 0 GPU-submit |
| Object layer height | Через дефект модалки коректне введення не виміряне | 0.28 після Apply та повторного відкриття; 0 mesh rebuild |
| Drag з release поза viewport | — | Об’єкт перемістився, жест завершився |

Єдиний GPU-submit у фінальному scoped-сценарії належить вибору об’єкта до
відкриття діалогу; Apply не перебудовує mesh. Значення printer/filament
позначені dirty, системні профілі не зберігалися. Тестовий проєкт не зберігався.

Артефакти цього запуску: `/tmp/bambu-perf-baseline.log`,
`/tmp/bambu-perf-baseline2.log`, `/tmp/bambu-perf-final.log`,
`/tmp/bambu-perf-final-run2.log`; screenshots `/tmp/perf-final-filament2.png`,
`/tmp/perf-final-printer.png`, `/tmp/perf-final-scoped-verify.png`,
`/tmp/perf-drag-out.png`, `/tmp/perf-x.png`.

## Зміни

- Mouse-up-out в’юпорту завершує лише жест, який почався у в’юпорті.
  Клік у правому доку більше не запускає GPU render/readback/upload.
  Справжнє завершення drag поза в’юпортом збережене.
- Зміна overrides об’єкта/частини/діапазону скидає нарізку й Preview,
  позначає проєкт зміненим, зберігає поточну геометрію та selection.
  Раніше викликався project_changed з перебудовою всіх plate meshes.
- Dismiss-target модалки відокремлений від поверхні редактора; клік у
  полі не має потрапляти в dismiss-handler чи сцену під модалкою.
- Позначка фонової операції статична: повторна анімація навіть на 12 fps
  змушувала GPUI перемальовувати ціле вікно.

## Межі й наступні hotspots

Текстові input entities вже ізольовані; commit відбувається на Enter/Blur,
не на кожному символі. На базовому replay редактор займав ~0.3 мс,
Workspace ~0.7 мс. Правила/валідація не були основною затримкою, тому
додаткові кеші й debounce не додавалися.

GPU zero-copy лишається відкритим: готовий кадр копіюється в CPU image і
завантажується в GPUI. Picking, selected_bounds і object-tree bounds
можуть повторно обходити вершини; для багатьох великих об’єктів потрібне
окреме вимірювання. Збереження user-пресетів усе ще синхронне (fsync), але
не виконується при звичайному редагуванні поля. Порівняння зі Studio/Orca
за end-to-end input latency цим replay не вимірювалося.

## Перевірки збірки

- `cargo build --release` — фінальна збірка пройшла (5m36s на зайнятому хості).
- Початковий `cargo test --workspace` — 136 passed / 2 ignored.
- Фінальний `cargo test --workspace -- --test-threads=1` — 136 passed / 2 ignored.
  Проміжні паралельні повтори падали на наявному wall-clock порозі parser
  real_samples (<300 ms; спостерігалося 519–703 ms). Тест і поріг не змінені.
- Native Xvfb replay та screenshots виконані для фінального release-бінарника.
  Свої app/Xvfb процеси завершено; вікон Bambu Studio не торкалися.

Для уникнення повторної компіляції залежностей використано
`CARGO_TARGET_DIR=target`; фінальний бінарник
скопійовано в target поточного worktree і запущено як `./target/release/slicenv`.
