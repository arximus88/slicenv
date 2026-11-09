#!/usr/bin/env python3
"""Replay native slider gestures against an isolated X11 app with SLICENV_UI_TRACE=1.

Uses angel_medium.3mf at 1560x940 in an isolated X11 display. Changes only
in-memory geometry; use a temporary XDG_CACHE_HOME and SLICENV_CLOUD_OFF=1.
Requires an already running app with the model sliced into Preview, xdotool,
ImageMagick import, and --log pointing at the app's stderr/stdout file.
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
    parser.add_argument('--window', required=True)
    parser.add_argument('--log', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    env = dict(os.environ, DISPLAY=args.display)
    args.output.mkdir(parents=True, exist_ok=True)

    def command(*items):
        subprocess.run(['xdotool', *map(str, items)], env=env, check=True, capture_output=True)

    def state():
        states = [json.loads(line.split('UI_TRACE ', 1)[1]) for line in args.log.read_text().splitlines() if 'UI_TRACE ' in line]
        return states[-1] if states else None

    def wait_for(predicate, timeout=30):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            found = state()
            if found and predicate(found):
                return found
            time.sleep(0.05)
        raise AssertionError(f'timed out waiting for UI state; last={state()}')

    def drag(x, y, dx, dy):
        command('mousemove', '--window', args.window, int(x), int(y))
        command('mousedown', '1')
        for step in range(1, 9):
            command('mousemove', '--window', args.window, int(x + dx * step / 8), int(y + dy * step / 8))
            time.sleep(0.03)
        command('mouseup', '1')

    command('windowfocus', args.window)
    command('key', '--window', args.window, 'ctrl+3')
    initial = wait_for(lambda s: s['page'] == 'preview' and s['layers'] is not None)
    camera = initial['camera']
    x, y, width, height = initial['viewport']
    drag(x + width * .5, y + height - 28, 120, 0)
    moves = wait_for(lambda s: s['moves'] != initial['moves'])
    assert moves['camera'] == camera, 'move slider rotated/panned/zoomed the camera'
    drag(x + width - 38, y + height * .25, 0, 110)
    layers = wait_for(lambda s: s['layers'] != initial['layers'])
    assert layers['camera'] == camera, 'layer slider rotated/panned/zoomed the camera'
    command('mousemove', '--window', args.window, int(x + width - 38), int(y + 100), 'click', '4')
    time.sleep(.2)
    assert state()['camera'] == camera, 'wheel over a slider card zoomed the camera'
    subprocess.run(['import', '-window', args.window, str(args.output / 'preview-sliders.png')], env=env, check=True)
    drag(x + width * .4, y + height * .4, 65, 25)
    orbit = wait_for(lambda s: s['camera'] != camera)
    command('key', '--window', args.window, 'ctrl+2')
    wait_for(lambda s: s['page'] == 'prepare')
    command('mousemove', '--window', args.window, 100, 105, 'click', '1')
    selected = wait_for(lambda s: s['selected'] is not None)
    command('mousemove', '--window', args.window, 100, 153, 'click', '1')
    part = wait_for(lambda s: s['volume'] == 1)
    assert part['selected'] == selected['selected'], 'part selection changed assembly identity'
    subprocess.run(['import', '-window', args.window, str(args.output / 'assembly-part.png')], env=env, check=True)
    # The fixed fixture/camera replay places the X arrow at this viewport-relative point.
    count = len([line for line in args.log.read_text().splitlines() if 'UI_TRACE ' in line])
    px, py, _, _ = part['viewport']
    drag(px + 560, py + 534, 32, -16)
    moved_part = wait_for(lambda s: s.get('selected_bounds') != part.get('selected_bounds') and not s.get('gizmo'))
    recent = [json.loads(line.split('UI_TRACE ', 1)[1]) for line in args.log.read_text().splitlines() if 'UI_TRACE ' in line][count:]
    assert any(s.get('gizmo') for s in recent), 'axis drag did not reach the gizmo handler'
    assert moved_part['camera'] == part['camera'], 'gizmo drag moved the camera'
    assert moved_part['object_bounds'] == part['object_bounds'], 'part gizmo moved the positive assembly'
    assert moved_part['layers'] is None, 'geometry edit retained the old slice preview'

    # Section card controls must also own pointer gestures.
    command('mousemove', '--window', args.window, 707, 876, 'click', '1')
    section = wait_for(lambda s: s['section'])
    drag(1040, 325, -50, 0)
    time.sleep(.3)
    assert state()['camera'] == section['camera'], 'section slider moved the camera'
    subprocess.run(['import', '-window', args.window, str(args.output / 'prepare-section.png')], env=env, check=True)
    command('mousemove', '--window', args.window, 707, 876, 'click', '1')
    wait_for(lambda s: not s['section'])

    # A duplicated halo can be assembled and split without touching the angel's ranges.
    command('mousemove', '--window', args.window, 120, 273, 'click', '1')
    wait_for(lambda s: s['selected'] == [2, 0])
    command('key', '--window', args.window, 'ctrl+d')
    wait_for(lambda s: s['selected'] == [2, 1])
    command('keydown', 'ctrl')
    command('mousemove', '--window', args.window, 120, 273, 'click', '1')
    command('keyup', 'ctrl')
    wait_for(lambda s: s.get('selection_count') == 2)
    command('key', '--window', args.window, 'ctrl+g')
    merged = wait_for(lambda s: s.get('volume_count') == 2 and s.get('selection_count') == 1)
    command('key', '--window', args.window, 'ctrl+shift+g')
    split = wait_for(lambda s: s.get('volume_count') == 1 and s.get('selection_count') == 2)
    assert split['object_count'] == merged['object_count'] + 1
    subprocess.run(['import', '-window', args.window, str(args.output / 'assembly-split.png')], env=env, check=True)
    result = {'part_gizmo_changes_only_part': True, 'geometry_edit_invalidates_preview': True,
              'section_slider_camera_unchanged': True, 'merge_split_workflow': True,
              'move_slider_camera_unchanged': True, 'layer_slider_camera_unchanged': True,
              'card_wheel_camera_unchanged': True, 'scene_orbit_still_works': True,
              'assembly_part_selectable': True, 'initial': initial, 'moves': moves,
              'layers': layers, 'orbit': orbit, 'part': part}
    (args.output / 'desktop-result.json').write_text(json.dumps(result, indent=2) + '\n')
    print('Native slider/camera, part gizmo, section, and merge/split checks passed.')


if __name__ == '__main__':
    main()
