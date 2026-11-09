---
name: real-engine-test
description: Запуск справжніх рушіїв Bambu Studio / OrcaSlicer або застосунку slicenv для перевірки нарізки, пресетів, сумісного режиму, міграції. Запускай, коли зміна торкається crates/slicer (cli.rs, presets.rs, safe_mode.rs, engine.rs), профілів, каталогів налаштувань рушія або коли треба відкрити GUI з моделлю.
---

# Справжні рушії й GUI — лише в ізоляції

Рушії й сам Slicenv пишуть у `~/.config/BambuStudio`, `~/.config/OrcaSlicer`, `~/.config/slicenv`, `~/.local/share/slicenv`, `~/.cache/slicenv`. Будь-який запуск — з тимчасовими HOME і XDG_*. Шляхи тулчейну беруться зі справжнього HOME **до** підміни.

## 1. Де рушії
| Джерело | Шлях |
|---|---|
| керовані Slicenv | `~/.local/share/slicenv/engines/{bambu,orca}/<версія>/AppRun` (лише запуск, не змінювати) |
| системні | `/usr/bin/bambustudio`, `/usr/bin/orca-slicer` |
| явно для тесту | `SLICENV_REGRESSION_ENGINES="bambu=/шлях/AppRun;orca=/шлях/AppRun"` |

```bash
ls -d ~/.local/share/slicenv/engines/*/*/AppRun /usr/bin/bambustudio /usr/bin/orca-slicer 2>/dev/null
```

## 2. Регресія пресетів (`crates/slicer/tests/engine_presets.rs`)
Тест сам підміняє HOME/XDG_*/XDG_RUNTIME_DIR на тимчасові, але керовані рушії шукає у **справжньому** `$HOME`. Тому під тимчасовим HOME передавай їх явно:
```bash
R=$HOME
E=""
for f in bambu orca; do
  a=$(ls -d "$R"/.local/share/slicenv/engines/$f/*/AppRun 2>/dev/null | tail -1)
  [ -n "$a" ] && E="$E$f=$a;"
done
T=$(mktemp -d)
HOME=$T XDG_CONFIG_HOME=$T/c XDG_DATA_HOME=$T/d XDG_CACHE_HOME=$T/k XDG_STATE_HOME=$T/s \
RUSTUP_HOME=$R/.rustup CARGO_HOME=$R/.cargo SLICENV_REGRESSION_ENGINES="$E" \
  cargo test -p slicer --test engine_presets -- --nocapture
rm -rf "$T"
```
Немає рушія або `python3` — тест друкує `пропущено` і проходить: це не підтвердження. Перевір у виводі, що рушій справді різав. Збірки лише з `.opc`-профілями (Orca 2.5) — `SLICENV_REGRESSION_PROFILES=<тека з <family>-<версія>/BBL.json>`.

## 3. Інші тести з рушієм (`#[ignore]`, запускати з тим самим префіксом HOME/XDG)
| Тест | Команда |
|---|---|
| підписи з каталогу рушія | `SLICENV_TEST_ENGINE=<бінарник> SLICENV_TEST_KIND=orca\|bambu cargo test -p settings real_engine -- --ignored --nocapture` |
| міграція на реальній моделі | `ANGEL_MIGRATION_FIXTURE=<3mf> cargo test -p app --bin slicenv real_angel_migration -- --ignored --nocapture` (потрібен Orca CLI) |
| встановлення рушія з GitHub | `SLICENV_LIVE_INSTALL=orca:v2.4.2 cargo test -p app --bin slicenv live_install -- --ignored --nocapture` (~150 МБ мережі) |

## 4. GUI з моделлю
`slicenv` з аргументами відкриває вікно зі **справжньою** конфігурацією. Лише так:
```bash
R=$HOME; T=$(mktemp -d)
HOME=$T XDG_CONFIG_HOME=$T/c XDG_DATA_HOME=$T/d XDG_CACHE_HOME=$T/k XDG_STATE_HOME=$T/s \
RUSTUP_HOME=$R/.rustup CARGO_HOME=$R/.cargo SLICENV_CLOUD_OFF=1 SLICENV_LAN_OFF=1 \
  cargo run -p app -- model.3mf &
echo "PID $!"     # зупиняти: kill <PID>, ніколи pkill -f
```
`SLICENV_CLOUD_OFF=1` — без Bambu Cloud, `SLICENV_LAN_OFF=1` — без LAN-пошуку принтерів, `SLICENV_FAKE_PRINTER=1` (або `=idle`) — фіктивний принтер, команди нікуди не йдуть. З тимчасовим HOME керованих рушіїв не видно: встанови рушій у тимчасовий HOME через менеджер або скопіюй теку версії в `$T/d/slicenv/engines/<family>/`.

## Чого знати про рушії
- OrcaSlicer пише лог у `00000.log` у робочій теці запуску. Прогрес і номер шару — з `--debug 4`: рядки `default_status_callback: percent=…, message=Generating G-code: layer N` (`crates/slicer/src/cli.rs`); `--pipe` не використовується.
- Рушій падає на 3MF → сумісний режим зі стадіями (`cli.rs`, `safe_mode.rs`); результат запам’ятовується в `cache_dir()/engine-compat.json`. OrcaSlicer 2.3.0 CLI на Windows падає з 0xC0000005 на багатьох 3MF ([docs/PORTING.md](../../../docs/PORTING.md)).
- Батьківський пресет шукається в межах того самого вендора, потім `OrcaFilamentLibrary` (`presets.rs`). Пошук по всіх вендорах підмінював службових батьків з однаковими іменами (Anker `fdm_machine_common`).
- Жодних команд на справжні принтери й звернень до Bambu Cloud у тестах.
