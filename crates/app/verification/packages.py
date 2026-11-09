#!/usr/bin/env python3
"""Native package-panel acceptance on an isolated 1560x940 Xvfb display.

Downloads official Orca 2.5.0-beta (~204 MB), checks live progress, duplicate
clicks, channel switching and installed-row removal. Uses private XDG trees,
cloud disabled; never changes the user's packages or sends printer commands.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import time


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--display', required=True)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--binary', type=Path, default=Path('target/debug/slicenv'))
    p.add_argument('--model', type=Path, required=True)
    args = p.parse_args()
    output = args.output.resolve(); output.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, DISPLAY=args.display, SLICENV_CLOUD_OFF='1', SLICENV_UI_TRACE='1',
               SLICENV_PROFILE_ROOT=str(Path.home()/'.config/BambuStudio'))
    env.pop('WAYLAND_DISPLAY', None)
    for name in ('CONFIG', 'DATA', 'CACHE'):
        env[f'XDG_{name}_HOME'] = str(output/name.lower())
    log = output/'gui.log'
    def x(*items, check=True):
        return subprocess.run(['xdotool', *map(str, items)], env=env, capture_output=True, check=check)
    x('keyup', 'Control_L', 'Control_R', 'Shift_L', 'Shift_R', 'q', 'Escape')
    with log.open('w') as f:
        app = subprocess.Popen([str(args.binary.resolve()), str(args.model.resolve())], env=env, stdout=f, stderr=f)
    def records(prefix='PACKAGE_TRACE '):
        return [json.loads(l.split(prefix, 1)[1]) for l in log.read_text().splitlines() if prefix in l]
    def wait(predicate, timeout=60, prefix='PACKAGE_TRACE '):
        deadline = time.monotonic()+timeout
        while time.monotonic()<deadline:
            assert app.poll() is None, f'app exited {app.returncode}'
            rows = records(prefix)
            if rows and predicate(rows[-1]): return rows[-1]
            time.sleep(.05)
        raise AssertionError(f'timeout: {records(prefix)[-1:]}')
    def click(a,b):
        x('mousemove', '--window', window, a,b,'click','1'); time.sleep(.2)
    def screenshot(name):
        subprocess.run(['import','-window',window,str(output/name)], env=env, check=True)
    try:
        wait(lambda s:s.get('engine') and not s['engine_loading'], prefix='UI_TRACE ')
        window=x('search','--onlyvisible','--pid',app.pid).stdout.decode().splitlines()[0]
        x('windowfocus',window)
        click(1300,76);click(625,396)
        wait(lambda s:not s['busy'] and s['channel']=='stable');screenshot('stable.png')
        click(464,162)
        wait(lambda s:not s['busy'] and 'v2.5.0-beta' in s['releases'])
        x('mousemove','--window',window,780,610,'click','5','click','5','click','5');time.sleep(.2)
        screenshot('beta.png')
        click(1124,472)
        wait(lambda s:s['busy'] and s['stage']=='download' and 0<(s['percent'] or 0)<100)
        screenshot('download.png')
        # A second click while busy must not start another operation.
        operation=records()[-1]['operation'];click(1124,486)
        done=wait(lambda s:not s['busy'],timeout=180)
        assert not done['error'] and done['operation']==operation and '2.5.0-beta' in done['installed'],done
        screenshot('installed.png')
        manifest=output/'data/slicenv/engines/orca/2.5.0-beta/manifest.json'
        receipt=json.loads(manifest.read_text());stamp=manifest.stat().st_mtime_ns
        click(1110,472);click(1110,472)
        assert manifest.stat().st_mtime_ns==stamp and records()[-1]['operation']==operation
        probe=subprocess.run([receipt['executable_path'],'--help'],env=env,capture_output=True,text=True,timeout=30)
        assert probe.returncode==0 and 'OrcaSlicer-2.5.0-beta' in probe.stdout+probe.stderr
        click(535,162);nightly=wait(lambda s:not s['busy'] and s['channel']=='nightly')
        assert 'nightly-builds' in nightly['releases'],nightly
        screenshot('nightly.png')
        # The installed Beta stays visible below Nightly even though its release
        # isn't in the current channel. Remove only this test-owned version.
        click(1176,334);screenshot('delete-confirm.png');click(963,178)
        removed=wait(lambda s:not s['busy'] and '2.5.0-beta' not in s['installed'])
        assert not removed['error'] and not manifest.exists(),removed
        screenshot('removed.png')
        progress=[s for s in records() if s['stage']=='download' and s['percent'] is not None]
        (output/'results.json').write_text(json.dumps({'manifest':receipt,'progress_samples':len(progress),
            'progress_percent':[s['percent'] for s in progress], 'duplicate_click_operation':operation,
            'nightly_tags':nightly['releases'], 'removed':True},indent=2)+'\n')
        x('key','--window',window,'Escape','ctrl+q',check=False)
        assert app.wait(timeout=15)==0
        print('Native package progress, installed state, duplicate clicks, Nightly and removal passed.')
    finally:
        if app.poll() is None: app.terminate();app.wait(timeout=15)
        x('keyup','Control_L','Control_R','q','Escape',check=False)


if __name__=='__main__':main()
