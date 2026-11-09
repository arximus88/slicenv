#!/usr/bin/env python3
"""Check normal Ctrl+Q shutdown on an existing isolated X11 display.

--with-cloud additionally loads the installed networking SDK. It does not start
the camera or send printer controls. Outputs stay in --output.
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
    parser.add_argument('--binary', default='target/debug/slicenv')
    parser.add_argument('--model', required=True)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--with-cloud', action='store_true')
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    cases = [('offline', True, .5)]
    if args.with_cloud:
        cases += [('cloud-startup', False, .2), ('cloud-enabled', False, 3.0)]
    results = []
    for name, offline, delay in cases:
        env = dict(os.environ, DISPLAY=args.display, XDG_CACHE_HOME=str(args.output / 'cache'))
        for key in ('WAYLAND_DISPLAY', 'SLICENV_CAMERA_TEST', 'SLICENV_CLOUD_ONLY'):
            env.pop(key, None)
        if offline:
            env['SLICENV_CLOUD_OFF'] = '1'
        else:
            env.pop('SLICENV_CLOUD_OFF', None)
        with (args.output / f'{name}.log').open('w') as log:
            process = subprocess.Popen([args.binary, args.model], env=env, stdout=log, stderr=log)
            deadline = time.monotonic() + 15
            window = None
            while time.monotonic() < deadline and process.poll() is None:
                found = subprocess.run(['xdotool', 'search', '--onlyvisible', '--pid', str(process.pid)],
                                       env=env, capture_output=True, text=True)
                if found.returncode == 0 and found.stdout.strip():
                    window = found.stdout.splitlines()[0]
                    break
                time.sleep(.05)
            if window:
                time.sleep(delay)
                # A fast quit can destroy the target before synthetic key-up;
                # the application exit code is the assertion, not xdotool's.
                subprocess.run(['xdotool', 'windowfocus', window, 'key', '--window', window, 'ctrl+q'],
                               env=env, capture_output=True)
            try:
                code = process.wait(timeout=25)
            except subprocess.TimeoutExpired:
                process.terminate()
                process.wait(timeout=10)
                code = 'timeout'
            results.append({'case': name, 'exit_code': code, 'window': window is not None})
        (args.output / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
        assert code == 0 and window, results[-1]
    print('Normal GUI shutdown checks passed.')


if __name__ == '__main__':
    main()
