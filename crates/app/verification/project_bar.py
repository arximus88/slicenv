#!/usr/bin/env python3
"""Replay the project mode/action bar on an isolated app window.

Open a small model in Prepare with temporary HOME/XDG_*, SLICENV_UI_TRACE=1,
SLICENV_NO_WIZARD=1, SLICENV_CLOUD_OFF=1, SLICENV_LAN_OFF=1, SLICENV_FAKE_PRINTER=1;
pass the app window ID and trace log. --scale is GPUI_X11_SCALE_FACTOR of that run
(the trace is in logical pixels, xdotool works in physical ones). With a slicing engine
in the temporary data dir the scenario also slices and opens the print dialog; it never
confirms it. No source saves or printer actions.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import time

ORDER = ['full', 'no-keys', 'short-slice', 'icon-modes', 'icons', 'stacked']


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--display', required=True)
    p.add_argument('--window', required=True)
    p.add_argument('--log', type=Path, required=True)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--scale', type=float, default=1.0)
    p.add_argument('--widths', default='1560,1280,1100,960',
                   help='logical window widths, widest first')
    p.add_argument('--height', type=int, default=940)
    args = p.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, DISPLAY=args.display)
    k = args.scale

    def command(*items):
        subprocess.run(['xdotool', *map(str, items)], env=env, check=True, capture_output=True)

    def state():
        rows = [l.split('UI_TRACE ', 1)[1] for l in args.log.read_text().splitlines() if 'UI_TRACE ' in l]
        return json.loads(rows[-1]) if rows else None

    def wait(predicate, what, timeout=20):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            s = state()
            if s and predicate(s):
                return s
            time.sleep(.05)
        raise AssertionError(f'timeout: {what}: {state()}')

    def settled(pause=.6):
        """State after the UI stops changing (layout re-measures once after a resize)."""
        last = None
        end = time.monotonic() + 15
        while time.monotonic() < end:
            time.sleep(pause)
            s = state()
            if s == last:
                return s
            last = s
        return last

    def move(x, y):
        command('mousemove', '--window', args.window, round(x * k), round(y * k))

    def click(x, y):
        move(x, y)
        command('click', '1')
        time.sleep(.3)

    def key(combo):
        command('key', '--window', args.window, combo)
        time.sleep(.3)

    def drag(x, y, dx, dy, button=1):
        move(x, y)
        command('mousedown', button)
        time.sleep(.1)
        for i in range(1, 9):
            move(x + dx * i / 8, y + dy * i / 8)
            time.sleep(.035)
        command('mouseup', button)
        time.sleep(.3)

    def shot(name):
        time.sleep(.3)
        subprocess.run(['import', '-window', args.window, str(args.output / name)], env=env, check=True)

    def resize(width):
        command('windowsize', args.window, round(width * k), round(args.height * k))
        return settled()

    def bar(s):
        return s['shell']['project_bar_bounds']

    def check_bar(s, width, label):
        """The bar is whole, floats over the top of the scene, clear of the docks, and sits
        on the window axis unless the centre is too narrow or off to one side for that."""
        shell = s['shell']
        assert shell['project_bar'], f'{label}: no bar on {shell["mode"]}'
        x, y, w, h = bar(s)
        room = shell['project_bar_room']
        assert w <= room - 8, f'{label}: bar {w} wider than the centre {room}'
        assert x >= 0 and x + w <= width + 1, f'{label}: bar leaves the window'
        if shell['mode'] in ('prepare', 'preview') and s['viewport']:
            vx, vy, vw, _ = s['viewport']
            assert x >= vx + 3 and x + w <= vx + vw - 3, f'{label}: bar overlaps a dock: {bar(s)} vs {s["viewport"]}'
            assert vy <= y and y + h <= vy + 80, f'{label}: bar is not at the top of the scene: {bar(s)} vs {s["viewport"]}'
            mid = x + w / 2
            on_axis = abs(mid - width / 2) <= 1.5
            # Off the axis only when pushed against an edge of the centre or centred in it.
            pushed = x - vx <= 40 or vx + vw - (x + w) <= 40 or abs(mid - (vx + vw / 2)) <= 1.5
            assert on_axis or pushed, f'{label}: bar neither on the window axis nor against the centre edge: {bar(s)} vs {s["viewport"]}'
        return x, y, w, h

    def action(s, which):
        """Centre of Slice or Print in an icon-only bar (one row or stacked)."""
        x, y, w, h = bar(s)
        if s['shell']['project_bar'] == 'stacked':
            right, cy = x + w / 2 + 42, y + h - 5 - 12
        else:
            right, cy = x + w - 5, y + h / 2
        return (right - 14, cy) if which == 'print' else (right - 28 - 2 - 14, cy)

    widths = [int(w) for w in args.widths.split(',')]
    resize(widths[0])
    start = wait(lambda s: s['shell']['mode'] == 'prepare' and not s['engine_loading']
                 and s['shell']['project_bar_bounds'], 'prepare with bar', 90)
    start = settled()
    report = {'scale': k, 'project': start['shell']['project'], 'widths': {}}

    # --- modes: icon first, then Prepare, Preview; the bar survives every project page ---
    x, y, w, h = check_bar(start, widths[0], 'start')
    mid = y + h / 2
    shot('prepare.png')
    click(x + 5 + 12, mid)                       # project icon — first in the bar
    s = wait(lambda s: s['shell']['mode'] == 'project', 'project via icon')
    s = settled()
    check_bar(s, widths[0], 'project')
    assert s['shell']['project'] == start['shell']['project'], 'project page lost the document'
    shot('project.png')
    px_, py_, pw_, ph_ = bar(s)
    click(px_ + 5 + 24 + 2 + 14, py_ + ph_ / 2)   # Prepare — second
    wait(lambda s: s['shell']['mode'] == 'prepare', 'prepare via bar')
    key('ctrl+3')
    s = wait(lambda s: s['shell']['mode'] == 'preview', 'preview via Ctrl+3')
    s = settled()
    check_bar(s, widths[0], 'preview')
    shot('preview-unsliced.png')
    px_, py_, pw_, ph_ = bar(s)
    click(px_ + 5 + 12, py_ + ph_ / 2)
    wait(lambda s: s['shell']['mode'] == 'project', 'project from preview')
    key('ctrl+2')
    wait(lambda s: s['shell']['mode'] == 'prepare', 'prepare via Ctrl+2')
    key('ctrl+4')
    wait(lambda s: s['shell']['mode'] == 'project', 'project via Ctrl+4')
    key('ctrl+2')
    wait(lambda s: s['shell']['mode'] == 'prepare', 'prepare via Ctrl+2')

    # --- Home and Device have no project actions; the title tab leads back ---
    key('ctrl+1')
    s = wait(lambda s: s['shell']['mode'] == 'home', 'home')
    s = settled()
    assert s['shell']['project_bar'] is None, 'bar on Home'
    shot('home.png')
    key('ctrl+5')
    s = wait(lambda s: s['shell']['mode'] == 'device', 'device')
    s = settled()
    assert s['shell']['project_bar'] is None, 'bar on Device'
    shot('device.png')
    click(36 + 9 + 40, 17)                       # project tab in the title bar
    s = wait(lambda s: s['shell']['mode'] == 'prepare', 'prepare via title tab')
    s = settled()
    assert s['shell']['project'] == start['shell']['project']
    x, y, w, h = check_bar(s, widths[0], 'back from device')
    mid = y + h / 2

    # --- the bar is not part of the scene: gestures on it leave camera and edits alone ---
    vx, vy, vw, vh = s['viewport']
    camera = s['camera']
    drag(x + w / 2, mid, 0, 220)                 # starts on the bar, ends over the scene
    drag(x + w / 2, mid, 60, 180, button=3)
    move(x + w / 2, mid)
    command('click', '4', 'click', '4', 'click', '5')
    s = settled()
    assert s['camera'] == camera, f'gesture on the bar moved the camera: {camera} -> {s["camera"]}'
    assert s['edited'] == start['edited'] and s['selection_count'] == start['selection_count']
    assert s['shell']['mode'] == 'prepare' and not s['shell']['slicing']
    move(vx + vw / 2, vy + vh * .3)              # the same wheel over the scene does zoom
    command('click', '4', 'click', '4')
    zoomed = wait(lambda s: s['camera'] != camera, 'wheel over the scene zooms')
    assert zoomed['shell']['project_bar'], 'bar moved with the camera'
    assert bar(settled()) == [x, y, w, h], 'bar moved with the camera'

    # --- docks and modes: the centre changes, the bar stays where it was in the window ---
    home = [x, y, w, h]
    assert abs((x + w / 2) - widths[0] / 2) <= 1.5, f'bar off the window axis: {home}'
    key('ctrl+b')
    s = wait(lambda s: s['shell']['docks'] == [False, True], 'left dock closed')
    s = settled()
    check_bar(s, widths[0], 'left dock closed')
    assert s['shell']['project_bar_room'] > start['shell']['project_bar_room']
    assert bar(s) == home, f'closing the left dock moved the bar: {home} -> {bar(s)}'
    key('ctrl+alt+b')
    s = wait(lambda s: s['shell']['docks'] == [False, False], 'both docks closed')
    s = settled()
    check_bar(s, widths[0], 'no docks')
    assert bar(s) == home, f'closing both docks moved the bar: {home} -> {bar(s)}'
    shot('no-docks.png')
    key('ctrl+3')
    s = wait(lambda s: s['shell']['mode'] == 'preview', 'preview without docks')
    s = settled()
    assert bar(s) == home, f'Preview moved the bar: {home} -> {bar(s)}'
    key('ctrl+4')
    s = wait(lambda s: s['shell']['mode'] == 'project', 'project without docks')
    s = settled()
    check_bar(s, widths[0], 'project without docks')
    assert bar(s) == home, f'Project moved the bar: {home} -> {bar(s)}'
    key('ctrl+2')
    key('ctrl+b')
    key('ctrl+alt+b')
    wait(lambda s: s['shell']['docks'] == [True, True] and s['shell']['mode'] == 'prepare', 'docks back')

    # --- widths: labels give way in order, nothing is clipped, Print stays ---
    previous = 0
    for width in widths:
        s = resize(width)
        for mode, combo in (('prepare', 'ctrl+2'), ('preview', 'ctrl+3'), ('project', 'ctrl+4')):
            key(combo)
            wait(lambda s: s['shell']['mode'] == mode, f'{mode} at {width}')
            s = settled()
            check_bar(s, width, f'{mode} at {width}')
            shot(f'{mode}-{width}.png')
        key('ctrl+2')
        s = settled()
        level = ORDER.index(s['shell']['project_bar'])
        assert level >= previous, f'{width}: bar grew back ({s["shell"]["project_bar"]})'
        previous = level
        report['widths'][width] = {'density': s['shell']['project_bar'], 'bar': bar(s),
                                   'room': s['shell']['project_bar_room']}
    # Narrow window with one dock closed: the bar gets its labels back.
    key('ctrl+b')
    s = settled()
    check_bar(s, widths[-1], 'narrow, left dock closed')
    assert ORDER.index(s['shell']['project_bar']) <= previous
    report['narrow_left_closed'] = s['shell']['project_bar']
    shot(f'prepare-{widths[-1]}-left-closed.png')
    key('ctrl+b')
    s = settled()

    # --- actions, on the narrowest (icon-only when both docks are open) bar ---
    x, y, w, h = check_bar(s, widths[-1], 'actions')
    mid = y + h / 2
    icons = s['shell']['project_bar'] in ('icons', 'stacked')
    report['actions'] = {'can_slice': s['shell']['can_slice'], 'icons': icons}
    if not s['shell']['can_slice']:
        # No engine in the temporary data dir: Slice and Print are inert, clicks do nothing.
        click(x + w - 5 - 14, mid)
        if icons:
            click(*action(s, 'slice'))
        s = settled()
        assert not s['shell']['slicing'] and not s['shell']['modal'] and not s['shell']['sliced']
        assert s['shell']['mode'] == 'prepare'
    else:
        if icons:
            click(*action(s, 'slice'))           # Slice — between Save and Print
        else:
            key('ctrl+r')
        s = wait(lambda s: s['shell']['slicing'] or s['shell']['sliced'], 'slice started')
        report['actions']['slice_started_by'] = 'button' if icons else 'Ctrl+R'
        if s['shell']['slicing']:
            shot('slicing.png')
        s = wait(lambda s: s['shell']['sliced'] and not s['shell']['slicing'], 'sliced', 240)
        s = settled()
        report['actions']['sliced_plate'] = s['active_plate']
        check_bar(s, widths[-1], 'sliced')
        key('ctrl+3')
        s = wait(lambda s: s['shell']['mode'] == 'preview' and s['layers'], 'sliced preview', 60)
        s = settled()
        check_bar(s, widths[-1], 'sliced preview')
        shot('preview-sliced.png')
        report['actions']['can_print'] = s['shell']['can_print']
        report['actions']['sliced_preview'] = {'density': s['shell']['project_bar'],
                                               'room': s['shell']['project_bar_room']}
        click(*action(s, 'print'))               # Print — last
        s = settled()
        assert s['shell']['modal'] == s['shell']['can_print'], \
            f'print dialog {s["shell"]["modal"]} with can_print {s["shell"]["can_print"]}'
        if s['shell']['modal']:
            shot('print-dialog.png')
            key('Escape')                         # never confirmed
            wait(lambda s: not s['shell']['modal'], 'print dialog closed')
        key('ctrl+2')
    resize(widths[0])
    (args.output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=1))
    print(json.dumps(report, ensure_ascii=False))


if __name__ == '__main__':
    main()
