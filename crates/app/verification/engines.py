#!/usr/bin/env python3
"""Native 1560x940 replay: Bambu/Orca switching, slicing, Preview, persistence.

Requires an isolated Xvfb display, xdotool, ImageMagick import, installed engines
and BBL profiles. Uses only a supplied STL and private config/data/cache. Cloud
is disabled. No printer commands or source file writes.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--display', required=True)
    parser.add_argument('--binary', type=Path, default=Path('target/debug/slicenv'))
    parser.add_argument('--model', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--bambu-profiles', type=Path, default=Path.home() / '.config/BambuStudio')
    parser.add_argument('--orca-profiles', type=Path, default=Path('/opt/orca-slicer/resources/profiles'))
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, DISPLAY=args.display, SLICENV_CLOUD_OFF='1', SLICENV_UI_TRACE='1',
               SLICENV_PROFILE_ROOT=str(args.bambu_profiles), ORCA_PROFILE_ROOT=str(args.orca_profiles))
    env.pop('WAYLAND_DISPLAY', None)
    for name in ('CONFIG', 'DATA', 'CACHE'):
        env[f'XDG_{name}_HOME'] = str(args.output.resolve() / name.lower())
    results = []
    process = None
    window = None
    log = None

    def command(*items):
        return subprocess.run(['xdotool', *map(str, items)], env=env, capture_output=True, check=True)

    def state():
        rows = [line.split('UI_TRACE ', 1)[1] for line in log.read_text().splitlines() if 'UI_TRACE ' in line]
        return json.loads(rows[-1]) if rows else None

    def wait_for(predicate, timeout=60):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise AssertionError(f'app exited {process.returncode}: {log}')
            found = state()
            if found and predicate(found):
                return found
            time.sleep(.05)
        raise AssertionError(f'timeout; last state={state()}; log={log}')

    def launch(name):
        nonlocal process, window, log
        log = args.output / f'{name}.log'
        with log.open('w') as stream:
            process = subprocess.Popen([str(args.binary.resolve()), str(args.model.resolve())], env=env,
                                       stdout=stream, stderr=stream)
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            found = subprocess.run(['xdotool', 'search', '--onlyvisible', '--pid', str(process.pid)],
                                   env=env, capture_output=True, text=True)
            if found.returncode == 0 and found.stdout.strip():
                window = found.stdout.splitlines()[0]
                command('windowfocus', window)
                return
            time.sleep(.05)
        raise AssertionError('window did not open')

    def close():
        nonlocal process
        if process and process.poll() is None:
            subprocess.run(['xdotool', 'key', '--window', window, 'ctrl+q'], env=env, capture_output=True)
            process.wait(timeout=20)
            assert process.returncode == 0
        process = None

    try:
        launch('switching')
        wait_for(lambda s: s['engine'] is not None and not s['engine_loading'])
        for index, family in enumerate(('bambu', 'orca')):
            command('key', '--window', window, 'ctrl+2')
            wait_for(lambda s: s['page'] == 'prepare')
            command('mousemove', '--window', window, 1300, 76, 'click', '1')
            time.sleep(.15)
            command('mousemove', '--window', window, 625, 210 + index * 32, 'click', '1')
            ready = wait_for(lambda s: s['engine'] == family and not s['engine_loading'] and s['layers'] is None)
            command('key', '--window', window, 'ctrl+r')
            preview = wait_for(lambda s: s['engine'] == family and s['page'] == 'preview' and s['layers'] is not None)
            assert preview['layers'][1] > 0 and preview['moves'] > 0
            subprocess.run(['import', '-window', window, str(args.output / f'{family}-preview.png')], env=env, check=True)
            results.append({'engine': family, 'version': ready['engine_label'], 'schema_options': ready['schema_options'],
                            'layers': preview['layers'], 'moves': preview['moves']})
        close()
        launch('restart')
        persisted = wait_for(lambda s: s['engine'] == 'orca' and not s['engine_loading'])
        results.append({'persisted_engine': persisted['engine'], 'version': persisted['engine_label']})
        close()
        (args.output / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
        print('Engine switching, both slices/Preview, and restart persistence passed.')
    finally:
        if process and process.poll() is None:
            process.terminate()
            process.wait(timeout=10)


if __name__ == '__main__':
    main()
