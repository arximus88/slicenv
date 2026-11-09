#!/usr/bin/env bash
# Headless-сесія для скриншотів: приватний `mutter --headless` з Xwayland на справжній GPU,
# тимчасовий HOME, керування xdotool. Нічого не відкривається на робочому столі.
#
#   tools/screenshots/headless.sh start <тека> [WxH]   сесія (типово 1600x900), env → <тека>/env
#   tools/screenshots/headless.sh flatpak <тека> [файл] Flatpak Slicenv у сесії, вікно розгорнуте
#   tools/screenshots/headless.sh click <тека> X Y      клік (окремі press/release — так GPUI їх бачить)
#   tools/screenshots/headless.sh shot <тека> out.png   знімок вікна Slicenv
#   tools/screenshots/headless.sh stop <тека>           зупинити застосунок і сесію
#
# Дані Flatpak — у <тека>/home/.var/app/app.borys.slicenv (HOME сесії), установка flatpak —
# справжня користувацька (FLATPAK_USER_DIR). Хостові теки (xdg-config/BambuStudio тощо),
# хмара й LAN вимкнені; SLICENV_FAKE_PRINTER=1 дає фіктивний принтер для сторінки «Пристрій».
# Мова й тема — у <тека>/home/.var/app/app.borys.slicenv/config/slicenv/preferences.json.
# Перемальовка в headless mutter буває запізнілою: якщо знімок застарів — ще один клік у
# нейтральне місце (заголовок діалогу, порожній в’юпорт).
set -euo pipefail

cmd=${1:?команда}; W=$(realpath -m "${2:?тека}"); shift 2
H=$W/home
x() { (set -a; . "$W/env"; set +a; exec xdotool "$@"); }
win() { x search --onlyvisible --name '^Slicenv' | tail -1; }

case $cmd in
start)
  mkdir -p "$H"
  cat >"$W/bus.conf" <<EOF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><keep_umask/><listen>unix:tmpdir=/tmp</listen><auth>EXTERNAL</auth>
<standard_session_servicedirs/>
<policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>
EOF
  rm -f "$W/env"
  setsid env -i PATH=/usr/bin:/bin HOME="$H" LANG=en_US.UTF-8 XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" \
    XDG_CONFIG_HOME="$H/.config" XDG_DATA_HOME="$H/.local/share" XDG_CACHE_HOME="$H/.cache" \
    XDG_STATE_HOME="$H/.local/state" FLATPAK_USER_DIR="$HOME/.local/share/flatpak" \
    dbus-run-session --config-file="$W/bus.conf" -- mutter --headless --wayland \
    --wayland-display="slicenv-shots-$$" --virtual-monitor "${1:-1600x900}" -- \
    sh -c "env | grep -E '^(DISPLAY|XAUTHORITY|DBUS_SESSION_BUS_ADDRESS)=' >'$W/env.tmp'; mv '$W/env.tmp' '$W/env'; exec xprop -root -spy _NET_ACTIVE_WINDOW >/dev/null" \
    >"$W/session.log" 2>&1 &
  echo $! >"$W/session.pid"
  for _ in $(seq 60); do [[ -f $W/env ]] && break; sleep 0.5; done
  [[ -f $W/env ]] || { tail "$W/session.log"; exit 1; }
  echo "сесія: PID $(cat "$W/session.pid")" ;;
flatpak)
  flatpak kill app.borys.slicenv 2>/dev/null || true
  # --nosocket=wayland обов’язково: інакше Flatpak бере типовий wayland-0 — справжній стіл.
  (set -a; . "$W/env"; set +a; unset WAYLAND_DISPLAY
   setsid env HOME="$H" XDG_CONFIG_HOME="$H/.config" XDG_DATA_HOME="$H/.local/share" \
     XDG_CACHE_HOME="$H/.cache" XDG_STATE_HOME="$H/.local/state" LANG=en_US.UTF-8 \
     flatpak run --user --nosocket=wayland --socket=x11 \
     --nofilesystem=xdg-config/BambuStudio --nofilesystem=xdg-config/OrcaSlicer \
     --nofilesystem=xdg-documents --nofilesystem=xdg-download \
     --env=SLICENV_CLOUD_OFF=1 --env=SLICENV_LAN_OFF=1 --env=SLICENV_FAKE_PRINTER=1 \
     app.borys.slicenv "$@" >"$W/app.log" 2>&1 &)
  for _ in $(seq 60); do [[ -n $(win 2>/dev/null) ]] && break; sleep 0.5; done
  sleep 2
  x search --onlyvisible --name '^Slicenv' windowstate --add MAXIMIZED_VERT,MAXIMIZED_HORZ windowactivate
  sleep 6; x mousemove 700 500; sleep 1; x mousemove 702 502 ;;
click)
  X=${1:?x} Y=${2:?y}
  x mousemove $((X - 6)) $((Y - 3)); sleep 0.3; x mousemove "$X" "$Y"; sleep 0.3
  x mousedown 1; sleep 0.15; x mouseup 1; sleep 1.2; x mousemove $((X + 3)) $((Y + 2)); sleep 0.8 ;;
shot)
  (set -a; . "$W/env"; set +a; import -window "$(win)" "${1:?out.png}") ;;
stop)
  flatpak kill app.borys.slicenv 2>/dev/null || true
  [[ -f $W/session.pid ]] && kill -- "-$(cat "$W/session.pid")" 2>/dev/null || true ;;
*) echo "невідома команда $cmd" >&2; exit 2 ;;
esac
