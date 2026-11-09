#!/usr/bin/env python3
"""Порівняльний бенч Slicenv ↔ Bambu Studio ↔ OrcaSlicer (docs/BENCHMARK.md).

Кожен GUI-застосунок — X11-клієнт приватного headless `mutter` (Xwayland з апаратним
GL/Vulkan на справжньому GPU, не llvmpipe Xvfb), керування — xdotool. Кожен застосунок
бачить лише власні тимчасові HOME/XDG_*; справжні ~/.config/{slicenv,BambuStudio,
OrcaSlicer} і ~/.local/share/slicenv не відкриваються (рушії звідти — лише бінарники
для читання). Процеси зупиняються за PID власного дерева, ніколи `pkill -f`.

Сценарії (кожен — нова копія процесу, порядок застосунків чергується по колах):
  start         запуск без файлу → вікно → «готовий» (вікно є і CPU дерева тихе) → 10 с простою;
  open-<fx>     запуск із 3MF → готовий → простій → орбіта/зум сцени → Ctrl+R (нарізка
                пластини 1) → готовий Preview → простій → орбіта траєкторії;
  cli-<fx>      прямий виклик CLI рушія з тими самими аргументами й пресетами, що й
                Slicenv (нижня межа часу нарізки).

Метрики з /proc кожні 100 мс по всьому дереву процесів (разом із рушієм-нащадком):
CPU (utime+stime), RSS (сума VmRSS), PSS (smaps_rollup, кожні 500 мс), GPU з
/proc/<pid>/fdinfo (drm-memory-vram/gtt, drm-engine-gfx → зайнятість), окремо — GPU
композитора (mutter+Xwayland).

  python3 -I tools/bench/compare.py --bin target/release/slicenv --out bench/compare-DATE.json
"""
import argparse
import datetime
import json
import os
import platform
import re
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CLK_TCK = os.sysconf('SC_CLK_TCK')
PAGE = os.sysconf('SC_PAGE_SIZE')
ENGINES = os.path.expanduser('~/.local/share/slicenv/engines')
WIN_W, WIN_H = 1560, 940
FIXTURES = ('small', 'heavy')
PRESETS = {'machine': 'Bambu Lab A1 0.4 nozzle', 'process': '0.20mm Standard @BBL A1',
           'filament': 'Bambu PLA Basic @BBL A1'}


def now():
    return time.monotonic()


# ---------------------------------------------------------------- /proc sampling

def children(pid):
    out = []
    try:
        for tid in os.listdir(f'/proc/{pid}/task'):
            try:
                with open(f'/proc/{pid}/task/{tid}/children') as f:
                    out.extend(int(x) for x in f.read().split())
            except OSError:
                pass
    except OSError:
        pass
    return out


def tree(root):
    seen, stack = [], [root]
    while stack:
        p = stack.pop()
        if p in seen:
            continue
        seen.append(p)
        stack.extend(children(p))
    return seen


def read_stat(pid):
    try:
        with open(f'/proc/{pid}/stat') as f:
            s = f.read()
    except OSError:
        return None
    rp = s.rfind(')')
    comm = s[s.find('(') + 1:rp]
    fields = s[rp + 2:].split()
    # fields[0] = state (field 3); utime=14, stime=15, starttime=22, rss=24
    return comm, int(fields[11]) + int(fields[12]), int(fields[19]), int(fields[21]) * PAGE


def read_pss(pid):
    try:
        with open(f'/proc/{pid}/smaps_rollup') as f:
            for line in f:
                if line.startswith('Pss:'):
                    return int(line.split()[1]) * 1024
    except OSError:
        pass
    return 0


def drm_fds(pid):
    out = []
    try:
        for fd in os.listdir(f'/proc/{pid}/fd'):
            try:
                if os.readlink(f'/proc/{pid}/fd/{fd}').startswith('/dev/dri/'):
                    out.append(fd)
            except OSError:
                pass
    except OSError:
        pass
    return out


def read_drm(pid, fds, clients):
    """Додає до clients{id: (gfx_ns, vram, gtt)} клієнтів DRM процесу (дедуп за id)."""
    for fd in fds:
        try:
            with open(f'/proc/{pid}/fdinfo/{fd}') as f:
                text = f.read()
        except OSError:
            continue
        cid = re.search(r'^drm-client-id:\s*(\d+)', text, re.M)
        if not cid:
            continue
        gfx = re.search(r'^drm-engine-gfx:\s*(\d+)', text, re.M)
        vram = re.search(r'^drm-memory-vram:\s*(\d+)', text, re.M)
        gtt = re.search(r'^drm-memory-gtt:\s*(\d+)', text, re.M)
        clients[cid.group(1)] = (int(gfx.group(1)) if gfx else 0,
                                 int(vram.group(1)) * 1024 if vram else 0,
                                 int(gtt.group(1)) * 1024 if gtt else 0)


class Sampler(threading.Thread):
    """Ряд вимірів дерева процесів; дані — список dict із полем t (monotonic)."""

    def __init__(self, root, extra_roots=(), period=0.1):
        super().__init__(daemon=True)
        self.root, self.extra_roots, self.period = root, list(extra_roots), period
        self.samples, self.stop_flag = [], threading.Event()
        self.ticks = {}       # (pid, start) -> останні utime+stime
        self.comms = {}
        self.fd_cache, self.fd_cache_t = {}, {}
        self.lock = threading.Lock()

    def gpu(self, pids, now_t):
        clients = {}
        for p in pids:
            if now_t - self.fd_cache_t.get(p, -10) > 2.0:
                self.fd_cache[p] = drm_fds(p)
                self.fd_cache_t[p] = now_t
            read_drm(p, self.fd_cache.get(p, ()), clients)
        gfx = sum(c[0] for c in clients.values())
        return gfx, sum(c[1] for c in clients.values()), sum(c[2] for c in clients.values())

    def run(self):
        k = 0
        while not self.stop_flag.is_set():
            t = now()
            pids = tree(self.root)
            rss = 0
            live = 0
            for p in pids:
                st = read_stat(p)
                if not st:
                    continue
                comm, ticks, start, r = st
                self.ticks[(p, start)] = ticks
                self.comms[(p, start)] = comm
                rss += r
                live += 1
            sample = {'t': t, 'cpu_ticks': sum(self.ticks.values()), 'rss': rss, 'nproc': live}
            if k % 5 == 0:
                sample['pss'] = sum(read_pss(p) for p in pids)
                gfx, vram, gtt = self.gpu(pids, t)
                sample.update(gfx_ns=gfx, vram=vram, gtt=gtt)
                if self.extra_roots:
                    cpids = [q for r in self.extra_roots for q in tree(r)]
                    cgfx, cvram, cgtt = self.gpu(cpids, t)
                    sample.update(comp_gfx_ns=cgfx, comp_vram=cvram, comp_gtt=cgtt)
            with self.lock:
                self.samples.append(sample)
            k += 1
            self.stop_flag.wait(max(0.0, self.period - (now() - t)))

    def snapshot(self):
        with self.lock:
            return list(self.samples)

    def stop(self):
        self.stop_flag.set()
        self.join(2)

    def cpu_pct(self, window):
        """CPU дерева у % одного ядра за останні window секунд."""
        s = self.snapshot()
        if len(s) < 2:
            return None
        last = s[-1]
        # Останній семпл не пізніше межі вікна: вікно завжди ≥ window.
        first = next((x for x in reversed(s) if x['t'] <= last['t'] - window), None)
        if first is None:
            return None
        dt = last['t'] - first['t']
        return 100.0 * (last['cpu_ticks'] - first['cpu_ticks']) / CLK_TCK / dt


def timeline(samples, t0, step=0.5):
    """Стислий ряд [t, CPU %, RSS МБ, PSS МБ, GPU %] кожні step с — для графіків і розбору."""
    out, prev, prev_g = [], None, None
    for x in samples:
        if prev is not None and x['t'] - prev['t'] < step:
            continue
        if prev is not None:
            row = [round(x['t'] - t0, 2),
                   round(100.0 * (x['cpu_ticks'] - prev['cpu_ticks']) / CLK_TCK / (x['t'] - prev['t']), 1),
                   round(x['rss'] / 2**20, 1)]
            if 'pss' in x:
                row.append(round(x['pss'] / 2**20, 1))
                if prev_g is not None and x['t'] > prev_g['t']:
                    row.append(round(100.0 * (x['gfx_ns'] - prev_g['gfx_ns']) / 1e9 / (x['t'] - prev_g['t']), 1))
                prev_g = x
            out.append(row)
        prev = x
    return out


def phase_stats(samples, t0, t1):
    """CPU-час, середній/піковий CPU%, RSS/PSS, GPU за інтервал [t0, t1]."""
    s = [x for x in samples if t0 - 0.11 <= x['t'] <= t1 + 0.11]
    if len(s) < 2:
        return None
    dt = s[-1]['t'] - s[0]['t']
    cpu_s = (s[-1]['cpu_ticks'] - s[0]['cpu_ticks']) / CLK_TCK
    peak = 0.0
    for i in range(len(s)):  # пік — по вікнах 0.5 с
        j = next((k for k in range(i, len(s)) if s[k]['t'] - s[i]['t'] >= 0.5), None)
        if j is None:
            break
        peak = max(peak, 100.0 * (s[j]['cpu_ticks'] - s[i]['cpu_ticks']) / CLK_TCK / (s[j]['t'] - s[i]['t']))
    out = {'wall_s': round(dt, 3), 'cpu_s': round(cpu_s, 3),
           'cpu_avg_pct': round(100.0 * cpu_s / dt, 1) if dt > 0 else None,
           'cpu_peak_pct': round(peak, 1),
           'rss_peak_mb': round(max(x['rss'] for x in s) / 2**20, 1),
           'rss_end_mb': round(s[-1]['rss'] / 2**20, 1)}
    g = [x for x in s if 'pss' in x]
    if g:
        out['pss_peak_mb'] = round(max(x['pss'] for x in g) / 2**20, 1)
        out['pss_end_mb'] = round(g[-1]['pss'] / 2**20, 1)
        out['vram_peak_mb'] = round(max(x['vram'] for x in g) / 2**20, 1)
        out['gtt_peak_mb'] = round(max(x['gtt'] for x in g) / 2**20, 1)
        out['vram_end_mb'] = round(g[-1]['vram'] / 2**20, 1)
        out['gtt_end_mb'] = round(g[-1]['gtt'] / 2**20, 1)
        if len(g) >= 2 and g[-1]['t'] > g[0]['t']:
            gd = g[-1]['t'] - g[0]['t']
            out['gpu_busy_pct'] = round(100.0 * (g[-1]['gfx_ns'] - g[0]['gfx_ns']) / 1e9 / gd, 1)
            if 'comp_gfx_ns' in g[0]:
                out['compositor_gpu_busy_pct'] = round(
                    100.0 * (g[-1]['comp_gfx_ns'] - g[0]['comp_gfx_ns']) / 1e9 / gd, 1)
    return out


# ---------------------------------------------------------------- headless session

class Session:
    """Приватний `dbus-run-session mutter --headless` з Xwayland; HOME — тимчасовий."""

    def __init__(self, work):
        self.dir = os.path.join(work, 'session')
        os.makedirs(os.path.join(self.dir, 'home'), exist_ok=True)
        info = os.path.join(self.dir, 'info')
        if os.path.exists(info):
            os.unlink(info)
        home = os.path.join(self.dir, 'home')
        env = {'PATH': '/usr/bin:/bin', 'HOME': home, 'LANG': 'en_US.UTF-8',
               'XDG_CONFIG_HOME': home + '/.config', 'XDG_DATA_HOME': home + '/.local/share',
               'XDG_CACHE_HOME': home + '/.cache', 'XDG_STATE_HOME': home + '/.local/state',
               'XDG_RUNTIME_DIR': os.environ.get('XDG_RUNTIME_DIR', f'/run/user/{os.getuid()}')}
        # Шина без каталогів служб: активація (gnome-keyring/libsecret, gvfs, at-spi,
        # портали) одразу дає ServiceUnknown, а не 25-секундний таймаут, як у
        # приватній сесії без systemd. У робочому GNOME ці служби вже запущені й
        # відповідають миттєво, тож «швидка відмова» ближча до реальності.
        services = os.path.join(self.dir, 'dbus-services')
        os.makedirs(services, exist_ok=True)
        # Виняток — Secret Service: Orca 2.4.2 без нього падає (SIGSEGV), тож його
        # дає справжній gnome-keyring-daemon із тимчасовим HOME сесії.
        if shutil.which('gnome-keyring-daemon'):
            with open(os.path.join(services, 'org.freedesktop.secrets.service'), 'w') as f:
                f.write('[D-BUS Service]\nName=org.freedesktop.secrets\n'
                        'Exec=/usr/bin/gnome-keyring-daemon --start --foreground --components=secrets\n')
        bus_conf = os.path.join(self.dir, 'dbus-session.conf')
        with open(bus_conf, 'w') as f:
            f.write('<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"'
                    ' "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">\n'
                    '<busconfig><type>session</type><keep_umask/>'
                    '<listen>unix:tmpdir=/tmp</listen><auth>EXTERNAL</auth>'
                    f'<servicedir>{services}</servicedir>'
                    '<policy context="default"><allow send_destination="*" eavesdrop="true"/>'
                    '<allow eavesdrop="true"/><allow own="*"/></policy></busconfig>\n')
        script = f'echo "$DISPLAY $XAUTHORITY $DBUS_SESSION_BUS_ADDRESS" > {info}.tmp && mv {info}.tmp {info}; exec sleep infinity'
        self.log = open(os.path.join(self.dir, 'mutter.log'), 'w')
        self.proc = subprocess.Popen(
            ['dbus-run-session', f'--config-file={bus_conf}', '--', 'mutter', '--headless', '--wayland',
             f'--wayland-display=slicenv-bench-{os.getpid()}', '--virtual-monitor', '1920x1080',
             '--', 'sh', '-c', script],
            env=env, stdout=self.log, stderr=subprocess.STDOUT, start_new_session=True)
        deadline = now() + 30
        while not os.path.exists(info):
            if self.proc.poll() is not None or now() > deadline:
                raise SystemExit('headless mutter не стартував, див. ' + self.log.name)
            time.sleep(0.2)
        self.display, self.xauth, self.dbus = open(info).read().split(maxsplit=2)
        self.dbus = self.dbus.strip()
        self.env = {'DISPLAY': self.display, 'XAUTHORITY': self.xauth,
                    'DBUS_SESSION_BUS_ADDRESS': self.dbus}
        # Xwayland у mutter стартує на першого X-клієнта й закривається без них:
        # тримаємо постійного клієнта, щоб старт Xwayland не потрапляв у виміри.
        self.keepalive = subprocess.Popen(['xprop', '-root', '-spy', '_NET_ACTIVE_WINDOW'],
                                          env={**os.environ, **self.env},
                                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(2)

    def compositor_pids(self):
        return [p for p in tree(self.proc.pid) if read_stat(p) and read_stat(p)[0] in ('mutter', 'Xwayland')]

    def xdo(self, *args, check=False):
        r = subprocess.run(['xdotool', *map(str, args)], env={**os.environ, **self.env},
                           capture_output=True, text=True)
        if check and r.returncode:
            raise RuntimeError(f'xdotool {args}: {r.stderr}')
        return r.stdout

    def windows(self, pids):
        """Видимі вікна процесів pids (_NET_WM_PID): [(id, x, y, w, h)].

        Пошук за PID, а не всіх вікон дисплея: у mutter є власне повноекранне вікно."""
        out, seen = [], set()
        for pid in pids:
            for wid in self.xdo('search', '--onlyvisible', '--pid', pid).split():
                if wid in seen:
                    continue
                seen.add(wid)
                g = self.xdo('getwindowgeometry', '--shell', wid)
                d = dict(line.split('=', 1) for line in g.split() if '=' in line)
                try:
                    out.append((wid, int(d['X']), int(d['Y']), int(d['WIDTH']), int(d['HEIGHT'])))
                except (KeyError, ValueError):
                    pass
        return out

    def screenshot(self, wid, path):
        try:
            subprocess.run(['import', '-window', wid, path], env={**os.environ, **self.env},
                           capture_output=True, timeout=10)
        except subprocess.TimeoutExpired:
            pass  # знімок — лише для перевірки людиною

    def close(self):
        for p in (self.keepalive, self.proc):
            if p.poll() is None:
                try:
                    os.killpg(p.pid, signal.SIGTERM) if p is self.proc else p.terminate()
                except ProcessLookupError:
                    pass
        try:
            self.proc.wait(10)
        except subprocess.TimeoutExpired:
            os.killpg(self.proc.pid, signal.SIGKILL)
        self.log.close()


# ---------------------------------------------------------------- applications

def appimage_version(path):
    try:
        return json.load(open(os.path.join(os.path.dirname(path), 'manifest.json')))['version']
    except (OSError, ValueError, KeyError):
        return None


class App:
    """Один застосунок з власним тимчасовим HOME (переживає прогони: перший — розігрів)."""

    def __init__(self, name, kind, cmd, work, engine=None, version=None, install=()):
        self.name, self.kind, self.cmd, self.engine = name, kind, cmd, engine
        self.version, self.install = version, list(install)
        self.home = os.path.join(work, 'homes', name)
        os.makedirs(self.home, exist_ok=True)
        self.prepare()

    def xdg(self):
        h = self.home
        return {'HOME': h, 'XDG_CONFIG_HOME': h + '/.config', 'XDG_DATA_HOME': h + '/.local/share',
                'XDG_CACHE_HOME': h + '/.cache', 'XDG_STATE_HOME': h + '/.local/state'}

    def prepare(self):
        cfg = self.xdg()['XDG_CONFIG_HOME']
        if self.kind == 'slicenv':
            d = os.path.join(cfg, 'slicenv')
            os.makedirs(d, exist_ok=True)
            json.dump({'mcp_enabled': False, 'wizard_done': True, 'language': 'en', 'theme': 'dark', 'open_3mf': 'project',
                       'left_drag_moves': True, 'printer': PRESETS['machine']},
                      open(os.path.join(d, 'preferences.json'), 'w'))
            kind, path = self.engine
            e = {'kind': kind, 'path': path, 'version': appimage_version(path)}
            json.dump({'active': e, 'installed': [e]}, open(os.path.join(d, 'engines.json'), 'w'))
            return
        # Bambu Studio / OrcaSlicer: мінімальний конфіг, щоб не було майстра першого
        # запуску; той самий принтер/процес/філамент, без резервних копій проєкту
        # (інакше після зупинки за PID наступний запуск питає про відновлення).
        app_dir = 'BambuStudio' if self.kind == 'bambu' else 'OrcaSlicer'
        d = os.path.join(cfg, app_dir)
        os.makedirs(d, exist_ok=True)
        conf = os.path.join(d, app_dir + '.conf')
        try:
            data = json.load(open(conf))
        except (OSError, ValueError):
            data = {}
        app = data.setdefault('app', {})
        app.update({'language': 'en_US', 'backup_switch': False, 'show_daily_tips': False,
                    'show_hints': False, 'sync_user_preset': False, 'privacy_version': '00.00.00.00',
                    'firstguide': True, 'check_stable_update_only': True,
                    'preset_bundle_auto_update': False, 'show_shared_profiles_notification': False})
        data['firstguide'] = {'finish': True, 'privacyuse': False}
        data.setdefault('models', [{'model': 'Bambu Lab A1', 'nozzle_diameter': '0.4', 'vendor': 'BBL'}])
        presets = data.setdefault('presets', {})
        presets.update({'machine': PRESETS['machine'], 'process': PRESETS['process'],
                        'filaments': [PRESETS['filament']]})
        tmp = conf + '.tmp'
        with open(tmp, 'w') as f:
            json.dump(data, f, indent=4)
        os.replace(tmp, conf)

    def env(self, session):
        base = {'PATH': os.environ.get('PATH', '/usr/bin:/bin'), 'LANG': 'en_US.UTF-8',
                'XDG_RUNTIME_DIR': os.environ.get('XDG_RUNTIME_DIR', f'/run/user/{os.getuid()}'),
                **self.xdg(), **session.env}
        if self.kind == 'slicenv':
            base.update(SLICENV_CLOUD_OFF='1', SLICENV_PROFILE='1', SLICENV_UI_TRACE='1')
            if os.environ.get('COMPARE_PROFILE_ALL'):
                base['SLICENV_PROFILE_ALL'] = '1'  # діагностика: усі spans у app.log
        else:
            # Без SSL_CERT_FILE Bambu/Orca на старті показують модальне питання
            # «use system SSL certificate…?» і далі не йдуть.
            # NO_AT_BRIDGE: у приватній D-Bus-сесії немає at-spi, і GTK чекає таймаут
            # активації org.a11y.atspi.Registry (~15–25 с) — у звичайному GNOME його немає.
            # WEBKIT_DISABLE_DMABUF_RENDERER: у headless mutter DMABUF-рендерер WebKitGTK
            # вішає Orca 2.4.2 на створенні головного вікна (чорне вікно назавжди).
            base.update(GDK_BACKEND='x11', SSL_CERT_FILE='/etc/ssl/certs/ca-certificates.crt',
                        NO_AT_BRIDGE='1', GTK_A11Y='none', WEBKIT_DISABLE_DMABUF_RENDERER='1')
        return base

    def disk(self):
        out = {}
        for label, path in self.install:
            out[label] = du(path)
        out['profile_after_runs'] = du(self.home)
        return out


def du(path):
    total = 0
    if os.path.isfile(path):
        return os.path.getsize(path)
    for dp, dn, fn in os.walk(path):
        for f in fn:
            try:
                st = os.lstat(os.path.join(dp, f))
                total += st.st_blocks * 512
            except OSError:
                pass
    return total


def discover_apps(args, work):
    apps = {}
    orca = {v: os.path.join(ENGINES, 'orca', v, 'AppRun') for v in ('2.4.2', '2.5.0-beta')}
    bambu_dirs = sorted(d for d in os.listdir(os.path.join(ENGINES, 'bambu')) if d.startswith('02.07'))
    bambu27 = os.path.join(ENGINES, 'bambu', bambu_dirs[-1], 'AppRun') if bambu_dirs else None
    sl = os.path.abspath(args.bin)
    if os.path.exists(orca['2.4.2']):
        apps['slicenv+orca-2.4.2'] = App('slicenv+orca-2.4.2', 'slicenv', [sl], work, ('orca', orca['2.4.2']),
                                         install=[('binary', sl)])
    if bambu27:
        apps['slicenv+bambu-2.7'] = App('slicenv+bambu-2.7', 'slicenv', [sl], work, ('bambu', bambu27),
                                        install=[('binary', sl)])
        apps['bambu-2.7'] = App('bambu-2.7', 'bambu', [bambu27], work, version=bambu_dirs[-1],
                                install=[('package', os.path.dirname(bambu27))])
    if os.path.exists('/usr/bin/bambustudio'):
        apps['bambu-2.8-appimage'] = App('bambu-2.8-appimage', 'bambu', ['/usr/bin/bambustudio'], work,
                                         version='02.08.04.57', install=[('appimage', os.path.realpath('/usr/bin/bambustudio'))])
    for v, p in orca.items():
        if os.path.exists(p):
            apps[f'orca-{v}'] = App(f'orca-{v}', 'orca', [p], work, version=v,
                                    install=[('package', os.path.dirname(p))])
    if args.apps:
        wanted = args.apps.split(',')
        apps = {k: v for k, v in apps.items() if k in wanted}
    return apps


# ---------------------------------------------------------------- one launch

class Launch:
    def __init__(self, app, session, argv, logpath):
        self.app, self.session, self.events = app, session, {}
        self.log = open(logpath, 'w')
        self.t0 = now()
        self.proc = subprocess.Popen(app.cmd + argv, env=app.env(session), cwd=app.home,
                                     stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                     stderr=subprocess.STDOUT, start_new_session=True)
        self.sampler = Sampler(self.proc.pid, session.compositor_pids())
        self.sampler.start()
        self.reader = threading.Thread(target=self.read, daemon=True)
        self.reader.start()
        self.state = {}

    def read(self):
        for raw in self.proc.stdout:
            t = now()
            line = raw.decode('utf-8', 'replace')
            self.log.write(line)
            if line.startswith('UI_TRACE '):
                try:
                    st = json.loads(line[9:])
                except ValueError:
                    continue
                self.events.setdefault('first_ui_trace', t)
                if st.get('engine') and not st.get('engine_loading'):
                    self.events.setdefault('engine_ready', t)
                if st.get('object_count'):
                    self.events.setdefault('objects_loaded', t)
                if st.get('layers') is not None:
                    self.events.setdefault('preview_layers', t)
                self.state = st
            elif line.startswith('SLICENV_PROFILE slice::background'):
                self.events['slice_background_ms'] = float(line.split()[-1][:-2])
                self.events['slice_background_end'] = t
            elif line.startswith('[slice]'):
                self.events.setdefault('slice_notes', []).append(line.strip())

    def rel(self, t):
        return None if t is None else round(t - self.t0, 3)

    def alive(self):
        return self.proc.poll() is None

    def main_window(self):
        wins = [w for w in self.session.windows(tree(self.proc.pid)) if w[3] >= 900 and w[4] >= 550]
        return max(wins, key=lambda w: w[3] * w[4]) if wins else None

    def wait_window(self, timeout):
        first = main = None
        deadline = now() + timeout
        while now() < deadline and self.alive():
            wins = self.session.windows(tree(self.proc.pid))
            if wins and first is None:
                first = now()
            big = [w for w in wins if w[3] >= 900 and w[4] >= 550]
            if big:
                main = now()
                break
            time.sleep(0.1)
        return first, main

    def wait_quiet(self, threshold, quiet_for, timeout, need=None, after=None):
        """Чекає, поки CPU дерева < threshold % за quiet_for с, що почались не раніше after
        (і подія need, якщо задана). Повертає початок тихого вікна."""
        deadline = now() + timeout
        after = now() if after is None else after
        while now() < deadline and self.alive():
            pct = self.sampler.cpu_pct(quiet_for)
            if (pct is not None and pct < threshold and now() - quiet_for >= after
                    and (need is None or need in self.events)):
                return now() - quiet_for
            time.sleep(0.1)
        return None

    def wait_busy(self, threshold, timeout):
        deadline = now() + timeout
        while now() < deadline:
            pct = self.sampler.cpu_pct(0.3)
            if pct is not None and pct > threshold:
                return now()
            time.sleep(0.05)
        return None

    def stop(self):
        pids = tree(self.proc.pid)
        for p in reversed(pids):
            try:
                os.kill(p, signal.SIGTERM)
            except ProcessLookupError:
                pass
        try:
            self.proc.wait(5)
        except subprocess.TimeoutExpired:
            pass
        for p in tree(self.proc.pid) + pids:
            try:
                os.kill(p, signal.SIGKILL)
            except ProcessLookupError:
                pass
        try:
            self.proc.wait(5)
        except subprocess.TimeoutExpired:
            pass
        self.sampler.stop()
        self.reader.join(2)
        self.log.close()
        cleanup_backups({pid for pid, _ in self.sampler.ticks})


def cleanup_backups(pids):
    """Резервні копії проєкту, які Bambu/Orca пишуть у /tmp/<app>_model/<дата>/<час>#PID#N:
    видаляємо лише записи наших PID (чужі — користувача й інших сесій — не чіпаємо)."""
    for base in ('/tmp/bamboo_model', '/tmp/orcaslicer_model'):
        if not os.path.isdir(base):
            continue
        for day in os.listdir(base):
            d = os.path.join(base, day)
            if not os.path.isdir(d):
                continue
            for entry in os.listdir(d):
                m = re.search(r'#(\d+)#', entry)
                if m and int(m.group(1)) in pids:
                    shutil.rmtree(os.path.join(d, entry), ignore_errors=True)


def gesture_orbit(session, win, fx, fy, steps=100, dx=4, dy=1, delay=0.02):
    wid, x, y, w, h = win
    session.xdo('mousemove', x + int(w * fx), y + int(h * fy))
    time.sleep(0.2)
    session.xdo('mousedown', 1)
    for _ in range(steps):
        session.xdo('mousemove_relative', '--', dx, dy)
        time.sleep(delay)
    session.xdo('mouseup', 1)


def gesture_zoom(session, win, fx, fy, clicks=20, delay=0.05):
    wid, x, y, w, h = win
    session.xdo('mousemove', x + int(w * fx), y + int(h * fy))
    for i in range(clicks):
        session.xdo('click', 4 if i < clicks // 2 else 5)
        time.sleep(delay)


# Точка орбіти — порожнє місце над столом у в’юпорті кожного застосунку (частки вікна
# 1560×940, перевірено знімками з --shots).
ORBIT_POINT = {'slicenv': (0.45, 0.2), 'bambu': (0.62, 0.2), 'orca': (0.62, 0.2)}


def run_launch(app, session, scenario, fixture, run_dir, args, shots):
    os.makedirs(run_dir, exist_ok=True)
    argv = [fixture] if fixture else []
    load = os.getloadavg()
    L = Launch(app, session, argv, os.path.join(run_dir, 'app.log'))
    result = {'app': app.name, 'scenario': scenario, 'loadavg_before': [round(x, 2) for x in load]}
    try:
        first, main = L.wait_window(args.timeout)
        result['first_window_s'] = L.rel(first)
        result['main_window_s'] = L.rel(main)
        if main is None:
            result['error'] = 'немає головного вікна'
            return result
        win = L.main_window()
        if win and (win[3], win[4]) != (WIN_W, WIN_H):
            session.xdo('windowsize', win[0], WIN_W, WIN_H)
            session.xdo('windowmove', win[0], 180, 70)
        need = 'objects_loaded' if (fixture and app.kind == 'slicenv') else None
        ready = L.wait_quiet(args.quiet_pct, args.quiet_s, args.timeout, need=need, after=main)
        result['ready_s'] = L.rel(ready)
        for k in ('first_ui_trace', 'engine_ready', 'objects_loaded'):
            if k in L.events:
                result[k + '_s'] = L.rel(L.events[k])
        if ready is None:
            result['error'] = 'не дочекались тиші CPU'
            return result
        win = L.main_window()
        result['window'] = list(win[1:]) if win else None
        samples = L.sampler.snapshot
        result['startup'] = phase_stats(samples(), L.t0, ready)
        if shots and win:
            session.screenshot(win[0], os.path.join(run_dir, 'ready.png'))
        idle = 10.0 if scenario == 'start' else 5.0
        t = now()
        time.sleep(idle)
        result['idle'] = phase_stats(samples(), t, now())
        if scenario == 'start':
            return result
        fx, fy = ORBIT_POINT[app.kind]
        session.xdo('windowactivate', '--sync', win[0])
        t = now()
        gesture_orbit(session, win, fx, fy)
        result['orbit'] = phase_stats(samples(), t, now())
        t = now()
        gesture_zoom(session, win, 0.5, 0.5)
        result['zoom'] = phase_stats(samples(), t, now())
        if shots:
            session.screenshot(win[0], os.path.join(run_dir, 'after-orbit.png'))
        time.sleep(1.0)
        # Нарізка пластини 1: Ctrl+R у всіх трьох застосунках.
        session.xdo('windowactivate', '--sync', win[0])
        t_slice = now()
        session.xdo('key', '--window', win[0], 'ctrl+r')
        busy = L.wait_busy(60, 15)
        result['slice_busy_s'] = round(busy - t_slice, 3) if busy else None
        need = 'preview_layers' if app.kind == 'slicenv' else None
        done = L.wait_quiet(args.quiet_pct, 2.0, args.slice_timeout, need=need, after=busy) if busy else None
        result['slice_wall_s'] = round(done - t_slice, 3) if done else None
        if done is None:
            result['error'] = 'нарізка не стартувала або не завершилась'
        else:
            result['slice'] = phase_stats(samples(), t_slice, done)
        if 'slice_background_ms' in L.events:
            result['slice_background_s'] = round(L.events['slice_background_ms'] / 1000, 3)
            result['slice_to_preview_s'] = round(L.events.get('preview_layers', now()) - t_slice, 3)
        if 'slice_notes' in L.events:
            result['slice_notes'] = L.events['slice_notes']
        if shots:
            session.screenshot(win[0], os.path.join(run_dir, 'sliced.png'))
        if done is None:
            return result
        t = now()
        time.sleep(3.0)
        result['preview_idle'] = phase_stats(samples(), t, now())
        t = now()
        gesture_orbit(session, win, fx, fy)
        result['preview_orbit'] = phase_stats(samples(), t, now())
        if shots:
            session.screenshot(win[0], os.path.join(run_dir, 'preview-orbit.png'))
        return result
    finally:
        L.stop()
        result['whole'] = phase_stats(L.sampler.samples, L.t0, now())
        result['timeline'] = timeline(L.sampler.samples, L.t0)
        result['events'] = {k: (L.rel(v) if isinstance(v, float) and k != 'slice_background_ms' else v)
                            for k, v in L.events.items()}


# ---------------------------------------------------------------- direct engine CLI

def capture_slicenv_inputs(app):
    """Каталоги нарізки Slicenv з machine/process/filament.json (кеш у тимчасовому HOME)."""
    root = os.path.join(app.home, '.cache', 'slicenv', 'sliced')
    found = {}
    for dp, dn, fn in os.walk(root):
        if 'machine.json' in fn and 'provenance.json' in fn:
            m = re.match(r'(small|heavy)-plate1-', os.path.basename(dp))
            if m:
                found[m.group(1)] = dp
    return found


def run_cli(app, session, fixture_path, inputs_dir, out_dir):
    """Прямий виклик рушія як у Slicenv (Stage::Normal, без --pipe): нижня межа нарізки."""
    os.makedirs(out_dir, exist_ok=True)
    for f in ('machine.json', 'process.json', 'filament.json'):
        shutil.copy(os.path.join(inputs_dir, f), out_dir)
    kind, path = app.engine
    args = [path]
    if kind == 'orca':
        args.append('--allow-newer-file')
    args += ['--debug', '2', '--load-settings', 'machine.json;process.json', '--load-filaments',
             'filament.json', '--load-defaultfila', '--orient', '0', '--arrange', '0', '--slice', '1',
             '--outputdir', '.', '--export-3mf', 'out.gcode.3mf', fixture_path]
    env = {**app.env(session)}
    for k in ('SLICENV_CLOUD_OFF', 'SLICENV_PROFILE', 'SLICENV_UI_TRACE'):
        env.pop(k, None)
    t0 = now()
    proc = subprocess.Popen(args, env=env, cwd=out_dir, stdout=open(os.path.join(out_dir, 'cli.log'), 'w'),
                            stderr=subprocess.STDOUT, start_new_session=True)
    s = Sampler(proc.pid, period=0.05)
    s.start()
    _, status, ru = os.wait4(proc.pid, 0)
    t1 = now()
    proc.returncode = os.waitstatus_to_exitcode(status)
    s.stop()
    cleanup_backups({pid for pid, _ in s.ticks} | {proc.pid})  # CLI теж пише /tmp/*_model
    r = {'wall_s': round(t1 - t0, 3), 'exit': proc.returncode,
         'cpu_s_rusage': round(ru.ru_utime + ru.ru_stime, 3),
         'maxrss_mb_rusage': round(ru.ru_maxrss / 1024, 1),
         'gcode_3mf': os.path.exists(os.path.join(out_dir, 'out.gcode.3mf')),
         'gcode': os.path.exists(os.path.join(out_dir, 'plate_1.gcode'))}
    st = phase_stats(s.samples, t0, t1)
    if st:
        r.update({k: st[k] for k in ('cpu_avg_pct', 'cpu_peak_pct', 'pss_peak_mb', 'rss_peak_mb') if k in st})
    return r


# ---------------------------------------------------------------- summary

def summarize(results):
    """Медіана і розкид (min–max) для числових полів по прогонах кожного (app, scenario)."""
    groups = {}
    for r in results:
        if r.get('warmup'):
            continue
        groups.setdefault((r['app'], r['scenario']), []).append(r)
    out = {}
    for (app, sc), runs in groups.items():
        flat = {}
        for r in runs:
            for k, v in r.items():
                if isinstance(v, (int, float)) and not isinstance(v, bool):
                    flat.setdefault(k, []).append(v)
                elif isinstance(v, dict) and k not in ('events',):
                    for kk, vv in v.items():
                        if isinstance(vv, (int, float)) and not isinstance(vv, bool):
                            flat.setdefault(f'{k}.{kk}', []).append(vv)
        out.setdefault(app, {})[sc] = {
            k: {'median': round(statistics.median(v), 3), 'min': round(min(v), 3),
                'max': round(max(v), 3), 'n': len(v)}
            for k, v in flat.items() if not k.startswith('loadavg')}
        out[app][sc]['errors'] = [r.get('error') for r in runs if r.get('error')]
    return out


def wait_load(max_load, log):
    if max_load <= 0:
        return
    deadline = now() + 600
    while os.getloadavg()[0] > max_load and now() < deadline:
        log(f'  load {os.getloadavg()[0]:.1f} > {max_load}, чекаю…')
        time.sleep(15)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('--bin', default=os.path.join(os.environ.get('CARGO_TARGET_DIR', os.path.join(ROOT, 'target')), 'release', 'slicenv'))
    p.add_argument('--work', help='робоча тека (типово ~/.cache/slicenv-compare-<pid>, видаляється)')
    p.add_argument('--keep', action='store_true', help='не видаляти робочу теку (логи, знімки)')
    p.add_argument('--out', default=os.path.join(ROOT, 'bench', f'compare-{datetime.date.today()}.json'))
    p.add_argument('--apps', help='кома: slicenv+orca-2.4.2,slicenv+bambu-2.7,bambu-2.7,bambu-2.8-appimage,orca-2.4.2,orca-2.5.0-beta')
    p.add_argument('--scenarios', default='start,open-small,open-heavy,cli-small,cli-heavy')
    p.add_argument('--runs', type=int, default=5, help='прогонів start і cli-*')
    p.add_argument('--open-runs', type=int, default=3, help='прогонів open-*')
    p.add_argument('--timeout', type=float, default=120)
    p.add_argument('--slice-timeout', type=float, default=180)
    p.add_argument('--quiet-pct', type=float, default=15.0, help='поріг «тихого» CPU, %% одного ядра')
    p.add_argument('--quiet-s', type=float, default=1.5)
    p.add_argument('--max-load', type=float, default=6.0, help='чекати, поки loadavg(1) нижче (0 — не чекати)')
    p.add_argument('--orca-gui', action='store_true',
                   help='сценарії open-* і в GUI OrcaSlicer (у headless mutter нестабільно: чорне '
                        '3D-полотно, SIGSEGV після Secret Service — див. docs/BENCHMARK.md)')
    p.add_argument('--shots', action='store_true', help='знімки вікна в першому прогоні кожного сценарію')
    args = p.parse_args()
    for tool in ('mutter', 'dbus-run-session', 'xdotool', 'xprop'):
        if not shutil.which(tool):
            raise SystemExit(f'немає {tool}')
    if not os.access(args.bin, os.X_OK):
        raise SystemExit(f'немає {args.bin} — зберіть: cargo build --release -p app')
    work = args.work or tempfile.mkdtemp(prefix='slicenv-compare-', dir=os.path.expanduser('~/.cache'))
    os.makedirs(work, exist_ok=True)
    real = [os.path.expanduser(p) for p in ('~/.config/slicenv', '~/.config/BambuStudio', '~/.config/OrcaSlicer', '~/.local/share/slicenv')]
    before = {p: os.stat(p).st_mtime_ns if os.path.exists(p) else None for p in real}

    def log(msg):
        print(msg, flush=True)

    scen = args.scenarios.split(',')
    session = Session(work)
    results, cli = [], []
    try:
        apps = discover_apps(args, work)
        log(f'дисплей {session.display}, застосунки: {", ".join(apps)}')
        fx = {}
        orca_profiles = os.path.join(ENGINES, 'orca', '2.4.2', 'resources', 'profiles')
        for name in FIXTURES:
            fx[name] = os.path.join(work, 'fixtures', f'{name}.3mf')
            os.makedirs(os.path.dirname(fx[name]), exist_ok=True)
            subprocess.run([sys.executable, '-I', os.path.join(ROOT, 'tools/bench/fixture_3mf.py'), name, fx[name],
                            '--profiles', orca_profiles], check=True, stdout=subprocess.DEVNULL)
        # Розігрів: перший запуск кожного застосунку (копіювання системних профілів тощо)
        # не входить у статистику.
        for app in apps.values():
            wait_load(args.max_load, log)
            log(f'розігрів {app.name}')
            r = run_launch(app, session, 'start', None, os.path.join(work, 'runs', app.name, 'warmup'), args, args.shots)
            r['warmup'] = True
            results.append(r)
            if app.kind != 'slicenv':
                app.prepare()  # повернути наші прапорці, якщо застосунок переписав конфіг
        plan = []
        for sc in scen:
            if sc == 'start':
                plan += [(sc, None, i) for i in range(args.runs)]
            elif sc.startswith('open-'):
                plan += [(sc, sc[5:], i) for i in range(args.open_runs)]
        for sc, f, i in plan:
            for app in apps.values():  # по колу: дрейф навантаження ділиться між застосунками
                if f and app.kind == 'orca' and not args.orca_gui:
                    continue  # див. --orca-gui
                wait_load(args.max_load, log)
                run_dir = os.path.join(work, 'runs', app.name, f'{sc}-{i}')
                r = run_launch(app, session, sc, fx[f] if f else None, run_dir, args, args.shots and i == 0)
                r['run'] = i
                results.append(r)
                short = {k: r.get(k) for k in ('main_window_s', 'ready_s', 'slice_wall_s', 'slice_background_s', 'error')}
                log(f'{sc} #{i} {app.name}: {short}')
                if app.kind != 'slicenv':
                    app.prepare()
        for sc in scen:
            if not sc.startswith('cli-'):
                continue
            f = sc[4:]
            for app in apps.values():
                if app.kind != 'slicenv':
                    continue
                inputs = capture_slicenv_inputs(app).get(f)
                if not inputs:
                    log(f'{sc} {app.name}: немає пресетів від нарізки Slicenv (спершу open-{f})')
                    continue
                for i in range(args.runs):
                    wait_load(args.max_load, log)
                    r = run_cli(app, session, fx[f], inputs, os.path.join(work, 'cli', app.name, f'{f}-{i}'))
                    r.update(app='cli:' + app.engine[0] + '-' + (appimage_version(app.engine[1]) or ''), scenario=sc, run=i)
                    cli.append(r)
                    log(f'{sc} #{i} {r["app"]}: {r["wall_s"]} с, exit {r["exit"]}')
        disk = {a.name: a.disk() for a in apps.values()}
    finally:
        session.close()
    after = {p: os.stat(p).st_mtime_ns if os.path.exists(p) else None for p in real}
    changed = [p for p in real if before[p] != after[p]]
    report = {
        'date': datetime.datetime.now().isoformat(timespec='seconds'),
        'host': {'node': platform.node(), 'kernel': platform.release(),
                 'cpu': next((l.split(':', 1)[1].strip() for l in open('/proc/cpuinfo') if l.startswith('model name')), None),
                 'nproc': os.cpu_count()},
        'method': {'display': 'mutter --headless + Xwayland (апаратний GL/Vulkan)', 'window': [WIN_W, WIN_H],
                   'quiet_pct': args.quiet_pct, 'quiet_s': args.quiet_s, 'sample_period_s': 0.1,
                   'pss_gpu_period_s': 0.5, 'max_load': args.max_load, 'presets': PRESETS,
                   'fixtures': {'small': '1 пластина, 20 000 △', 'heavy': '4 пластини × 250 000 △'}},
        'summary': summarize(results + cli),
        'disk_bytes': disk,
        'real_dirs_changed': changed,
        'runs': results,
        'cli_runs': cli,
    }
    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    with open(args.out, 'w') as f:
        json.dump(report, f, indent=1, ensure_ascii=False)
    log(f'записано {args.out}')
    if changed:
        # Попередження, не провал: їх міг змінити запущений паралельно справжній застосунок.
        log(f'УВАГА: змінились mtime справжніх тек (перевір, чи не працював там твій застосунок): {changed}')
    if not args.keep and not args.work:
        shutil.rmtree(work, ignore_errors=True)
    elif args.keep or args.work:
        log(f'робоча тека: {work}')
    return 0


if __name__ == '__main__':
    sys.exit(main())
