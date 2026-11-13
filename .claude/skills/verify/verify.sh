#!/usr/bin/env bash
# Повна перевірка Slicenv перед комітом або злиттям: переклади, clippy як у CI,
# тести workspace з тимчасовим HOME/XDG. Вихід 0 — усе пройшло, 1 — щось упало.
# Використання: bash .claude/skills/verify/verify.sh [--no-clippy] [--no-tests]
set -u
cd "$(git rev-parse --show-toplevel)" || exit 1

run_clippy=1 run_tests=1
for arg in "$@"; do
  case $arg in
    --no-clippy) run_clippy=0 ;;
    --no-tests) run_tests=0 ;;
    *) echo "невідомий аргумент: $arg" >&2; exit 2 ;;
  esac
done

LOG=$(mktemp -d -t slicenv-verify.XXXXXX)
# Справжні шляхи тулчейну — ДО підміни HOME, інакше cargo/rustup шукатимуть у тимчасовій теці.
REAL_HOME=$HOME
RUSTUP=${RUSTUP_HOME:-$REAL_HOME/.rustup}
CARGO_H=${CARGO_HOME:-$REAL_HOME/.cargo}
declare -A result
status=0

step() {
  local name=$1; shift
  echo "==> $name (лог: $LOG/$name.log)"
  if "$@" >"$LOG/$name.log" 2>&1; then
    result[$name]=passed
  else
    result[$name]=failed; status=1
    tail -40 "$LOG/$name.log"
  fi
}

# Версія CI — з rust-toolchain.toml; rustup бере її сама, розбіжність — лише при перевизначенні
# (RUSTUP_TOOLCHAIN, `cargo +…`, rustc не від rustup).
ci=$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)
rust=$(rustc --version 2>/dev/null)
case $rust in
  "rustc $ci "*) ;;
  *) echo "увага: $rust, а CI — $ci; інша версія може дати інші лінти clippy" ;;
esac

step translations python3 -I crates/app/tools/check_translations.py

if [ $run_clippy = 1 ]; then
  # Той самий список дозволів, що й CLIPPY_ALLOW у CI.
  allow=()
  for lint in $(grep -oE 'clippy::[a-z_]+' .github/workflows/ci.yml | sort -u); do
    allow+=(-A "$lint")
  done
  step clippy cargo clippy --workspace --all-targets -- -D warnings "${allow[@]}"
fi

if [ $run_tests = 1 ]; then
  T=$(mktemp -d -t slicenv-home.XXXXXX)
  step tests env HOME="$T" XDG_CONFIG_HOME="$T/c" XDG_DATA_HOME="$T/d" \
    XDG_CACHE_HOME="$T/k" XDG_STATE_HOME="$T/s" \
    RUSTUP_HOME="$RUSTUP" CARGO_HOME="$CARGO_H" \
    cargo test --workspace
  read -r passed failed < <(grep '^test result' "$LOG/tests.log" | awk '{p+=$4; f+=$6} END {print p+0, f+0}')
  echo "    тестів: passed $passed, failed $failed"
  rm -rf "$T"
fi

echo
echo "Підсумок:"
for name in translations clippy tests; do
  [ -n "${result[$name]:-}" ] && printf '  %-12s %s\n' "$name" "${result[$name]}"
done
if [ $status = 0 ]; then echo "VERIFY: passed"; else echo "VERIFY: failed (логи: $LOG)"; fi
exit $status
