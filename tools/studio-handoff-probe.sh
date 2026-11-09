#!/bin/bash
# Замір варіанта «передати .gcode.3mf у Bambu Studio» (docs/LINUX-CLOUD-PRINT.md).
#
#   tools/studio-handoff-probe.sh managed|flatpak <файл.gcode.3mf> [cold]
#
# Bambu Studio запускається в окремому Xvfb (:77), з тимчасовим HOME, без мережі
# (керований — `unshare -rn`, Flatpak — `--unshare=network`), без входу в акаунт.
# Друк не надсилається: скрипт лише чекає, поки «Print plate» стане зеленою, і робить
# знімок екрана. Потрібні Xvfb, xdotool, ImageMagick (import), python3.
# `cold` скидає page cache файлів рушія (posix_fadvise, без root) перед запуском.
set -u
MODE=${1:?managed|flatpak}; FILE=$(readlink -f "${2:?файл .gcode.3mf}"); COLD=${3:-}
OUT=${OUT:-/tmp/linux-studio-handoff}; mkdir -p "$OUT"
T=$(mktemp -d /tmp/studio-probe.XXXX); mkdir -p "$T/rt"; chmod 700 "$T/rt"
export DISPLAY=:77
Xvfb :77 -screen 0 1600x1000x24 >/dev/null 2>&1 & XVFB=$!
sleep 1

# Без майстра першого запуску: firstguide.finish = true.
conf_dir=$T/c/BambuStudio
[ "$MODE" = flatpak ] && conf_dir=$T/.var/app/com.bambulab.BambuStudio/config/BambuStudio
mkdir -p "$conf_dir"
printf '{\n "app": {"language": "en_US"},\n "firstguide": {"finish": "true", "privacyuse": "false"}\n}\n' \
  > "$conf_dir/BambuStudio.conf"

evict() {
  python3 -I - "$@" <<'EOF'
import os, sys
for top in sys.argv[1:]:
    for root, _, files in os.walk(top):
        for f in files:
            try:
                fd = os.open(os.path.join(root, f), os.O_RDONLY)
                os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED); os.close(fd)
            except OSError:
                pass
EOF
}

if [ "$MODE" = managed ]; then
  APP=$(ls -d "$HOME"/.local/share/slicenv/engines/bambu/*/AppRun | tail -1)
  [ -n "$COLD" ] && evict "$(dirname "$APP")"
  CMD=(env -u WAYLAND_DISPLAY -u DBUS_SESSION_BUS_ADDRESS HOME="$T" XDG_CONFIG_HOME="$T/c"
       XDG_DATA_HOME="$T/d" XDG_CACHE_HOME="$T/k" XDG_RUNTIME_DIR="$T/rt" GDK_BACKEND=x11
       SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt unshare -rn "$APP" "$FILE")
else
  [ -n "$COLD" ] && evict /var/lib/flatpak/app/com.bambulab.BambuStudio/current/active/files
  # Приватна сесійна шина: портал документів (`--file-forwarding`) і glycin працюють у ній,
  # справжня сесія користувача не зачіпається.
  CMD=(env -u WAYLAND_DISPLAY -u DBUS_SESSION_BUS_ADDRESS dbus-run-session -- env HOME="$T"
       XDG_RUNTIME_DIR="$T/rt" flatpak run --unshare=network --nosocket=wayland
       --file-forwarding com.bambulab.BambuStudio @@ "$FILE" @@)
fi

stem=$(basename "$FILE" .gcode.3mf)
before=$(flatpak ps --columns=instance 2>/dev/null | sort)
t0=$(date +%s.%N)
"${CMD[@]}" >"$T/studio.log" 2>&1 & APP_PID=$!
W=
for _ in $(seq 1 600); do
  W=$(xdotool search --name "$stem" 2>/dev/null | head -1); [ -n "$W" ] && break
  # Інформаційні діалоги («newer version», замінені параметри Orca) — натиснути OK.
  for d in $(xdotool search --name "Bambu Studio information|Newer 3mf version" 2>/dev/null); do
    eval "$(xdotool getwindowgeometry --shell "$d")"
    xdotool mousemove $((X + WIDTH - 60)) $((Y + HEIGHT - 22)) click 1; echo "діалог OK"
  done
  sleep 0.1
done
t1=$(date +%s.%N)
ok=no
if [ -n "$W" ]; then
  xdotool windowmove "$W" 0 0 windowsize "$W" 1600 1000
  for _ in $(seq 1 600); do
    for x in 1530 1560; do
      c=$(import -window root -crop 1x1+$x+49 -depth 8 txt:- 2>/dev/null | grep -o '#[0-9A-F]\{6\}')
      [ "$c" = "#00AE42" ] && ok=yes
    done
    [ $ok = yes ] && break; sleep 0.1
  done
fi
t2=$(date +%s.%N)
import -window root "$OUT/probe-$MODE-$stem.png"
LC_NUMERIC=C printf '%s %s: вікно %.1f с, «Print plate» активна: %s за %.1f с; знімок %s\n' \
  "$MODE" "$stem" "$(echo "$t1-$t0" | bc)" "$ok" "$(echo "$t2-$t0" | bc)" "$OUT/probe-$MODE-$stem.png"

kill "$APP_PID" 2>/dev/null; sleep 2
if [ "$MODE" = managed ]; then
  for p in $(pgrep -f "bin/bambu-studio"); do
    tr '\0' '\n' <"/proc/$p/environ" 2>/dev/null | grep -q "^HOME=$T\$" && kill "$p"
  done
fi
# Flatpak лишає пісочниці glycin-завантажувачів зображень після смерті застосунку:
# зупиняємо лише екземпляри, що з’явилися під час цього запуску.
if [ "$MODE" = flatpak ]; then
  sleep 2
  for i in $(comm -13 <(echo "$before") <(flatpak ps --columns=instance 2>/dev/null | sort)); do
    c=$(flatpak ps --columns=instance,child-pid | awk -v i="$i" '$1 == i {print $2}')
    [ -n "$c" ] && for g in $(ps -o pid= --ppid "$c"); do kill "$g"; done
  done
fi
for m in $(grep -o "$T/rt/doc" /proc/mounts | sort -u); do fusermount3 -u "$m" 2>/dev/null; done
kill "$XVFB"; rm -rf "$T"
