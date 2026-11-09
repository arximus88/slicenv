---
name: verify
description: Повна перевірка Slicenv перед комітом, злиттям чи звітом «готово» — переклади, clippy з allow-списком CI, тести workspace з тимчасовим HOME. Запускай після будь-якої зміни коду, en.json чи Cargo.toml; коли людина просить «перевір», «прожени тести», «чи пройде CI».
---

# verify

1. З кореня репозиторію (будь-який worktree):
   ```bash
   bash .claude/skills/verify/verify.sh
   ```
   Холодна збірка — кілька хвилин; запускай у фоні й чекай завершення, не опитуй.
   Прапорці: `--no-clippy`, `--no-tests` (наприклад, коли змінено лише en.json).
2. Скрипт робить по черзі, кожен крок з окремим логом у `/tmp/slicenv-verify.*/`:
   | Крок | Що |
   |---|---|
   | `translations` | `python3 -I crates/app/tools/check_translations.py` |
   | `clippy` | `cargo clippy --workspace --all-targets -- -D warnings` + `-A` для кожного `clippy::…` з `.github/workflows/ci.yml` |
   | `tests` | `cargo test --workspace` з тимчасовими `HOME`, `XDG_{CONFIG,DATA,CACHE,STATE}_HOME`; `RUSTUP_HOME`/`CARGO_HOME` зі справжнього HOME |
3. Останні рядки — підсумок по кроках, кількість тестів (`passed N, failed M`) і `VERIFY: passed` або `VERIFY: failed`. Вихід 0/1.
4. Упало:
   - `translations` — skill `add-ui-string`;
   - `clippy` — виправити код; не додавати лінт у `CLIPPY_ALLOW`, не запускати `cargo fmt --all`;
   - `tests` — `rg -n "panicked|FAILED" /tmp/slicenv-verify.*/tests.log`.
5. Попередження про версію rustc ≠ 1.97 означає, що clippy може розходитися з CI.
6. У звіті людині — фактичні рядки підсумку, не переказ.

Не входять (див. [docs/TESTING.md](../../../docs/TESTING.md)): `perf` проти baseline, GUI-бенч, `crates/app/verification/*.py`, тести `#[ignore]` зі справжніми рушіями (skill `real-engine-test`).
