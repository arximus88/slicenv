#!/usr/bin/env python3
"""Native Preferences replay in an isolated Xvfb display (1560x940).

Tests immediate Light/Dark, actual wheel behavior, drag behavior, persistence,
Preview scheme restoration and bounded setting help. Cloud is disabled and
config/data/cache live under --output. No source file saves or printer actions.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import time


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--display',required=True)
    p.add_argument('--model',type=Path,required=True)
    p.add_argument('--binary',type=Path,default=Path('target/debug/slicenv'))
    p.add_argument('--output',type=Path,required=True)
    args=p.parse_args();args.output.mkdir(parents=True,exist_ok=True)
    env=dict(os.environ,DISPLAY=args.display,SLICENV_CLOUD_OFF='1',SLICENV_UI_TRACE='1',
             SLICENV_PROFILE_ROOT=str(Path.home()/'.config/BambuStudio'),ORCA_PROFILE_ROOT='/opt/orca-slicer/resources/profiles')
    env.pop('WAYLAND_DISPLAY',None)
    for key in ('CONFIG','DATA','CACHE'):env[f'XDG_{key}_HOME']=str(args.output.resolve()/key.lower())
    process=None;window=None;log=None
    config=args.output/'config/slicenv/preferences.json'
    def command(*items):return subprocess.run(['xdotool',*map(str,items)],env=env,capture_output=True,check=True)
    def state():
        rows=[json.loads(l.split('UI_TRACE ',1)[1]) for l in log.read_text().splitlines() if 'UI_TRACE ' in l]
        return rows[-1] if rows else {}
    def wait(predicate,timeout=30):
        end=time.monotonic()+timeout
        while time.monotonic()<end:
            if process.poll() is not None:raise AssertionError(f'app exited: {process.returncode}')
            s=state()
            if predicate(s):return s
            time.sleep(.05)
        raise AssertionError(f'timeout: {state()}')
    def launch(name):
        nonlocal process,window,log
        command('keyup','Control_L','Control_R','Shift_L','Shift_R','q','Escape')
        window=None
        log=args.output/f'{name}.log'
        with log.open('w') as f:process=subprocess.Popen([str(args.binary.resolve()),str(args.model.resolve())],env=env,stdout=f,stderr=f)
        end=time.monotonic()+20
        while time.monotonic()<end:
            found=subprocess.run(['xdotool','search','--onlyvisible','--pid',str(process.pid)],env=env,capture_output=True,text=True)
            if found.returncode==0 and found.stdout.strip():window=found.stdout.splitlines()[0];break
            time.sleep(.05)
        assert window
        command('windowfocus',window)
        return wait(lambda s:s.get('engine') and not s['engine_loading'])
    def click(x,y):command('mousemove','--window',window,int(x),int(y),'click','1');time.sleep(.2)
    def prefs():command('key','--window',window,'ctrl+comma');time.sleep(.2)
    def close_dialog():command('key','--window',window,'Escape');time.sleep(.15)
    def screenshot(name):
        time.sleep(.2);subprocess.run(['import','-window',window,str(args.output/name)],env=env,check=True)
    def close():
        nonlocal process
        subprocess.run(['xdotool','windowfocus',window,'key','--window',window,'Escape','ctrl+q'],env=env,capture_output=True)
        assert process.wait(timeout=15)==0;process=None
        command('keyup','Control_L','Control_R','Shift_L','Shift_R','q','Escape')
    try:
        initial=launch('changes');assert not initial['preferences_light']
        prefs();screenshot('preferences-dark.png')
        click(1042,174);wait(lambda s:s.get('preferences_light'));screenshot('preferences-light.png')
        click(1059,260)  # Cursor-centered zoom off.
        click(1059,336)  # Left drag moves off.
        click(1059,374)  # Cube off.
        ready=wait(lambda s:not s.get('zoom_to_cursor',True) and not s.get('left_drag_moves',True) and not s.get('navigation_cube',True))
        close_dialog();before=state()['camera']
        command('mousemove','--window',window,710,500,'click','4')
        zoomed=wait(lambda s:s['camera'][2]<before[2]);assert zoomed['camera'][3:]==before[3:]
        prefs();click(1059,298);wait(lambda s:s.get('reverse_zoom'));close_dialog()
        before=state()['camera'];command('mousemove','--window',window,710,500,'click','4')
        reversed_zoom=wait(lambda s:s['camera'][2]>before[2]);assert reversed_zoom['camera'][3:]==before[3:]
        command('mousemove','--window',window,726,520,'mousedown','1');time.sleep(.1)
        command('mousemove','--window',window,776,545);time.sleep(.2);command('mouseup','1')
        orbit=wait(lambda s:s['camera'][:2]!=reversed_zoom['camera'][:2]);assert not orbit['edited']
        saved=json.loads(config.read_text());assert saved['theme']=='light' and saved['reverse_zoom'] and not saved['left_drag_moves']
        close()
        # Also exercise restoration of a non-default Preview scheme after restart.
        saved['last_scheme']=3;config.write_text(json.dumps(saved))
        restarted=launch('restart');assert restarted['preferences_light'] and restarted['reverse_zoom'] and not restarted['navigation_cube']
        command('key','--window',window,'ctrl+r')
        preview=wait(lambda s:s.get('page')=='preview' and s.get('layers') is not None)
        assert preview['preview_scheme']=='Швидкість',preview['preview_scheme']
        screenshot('preview-light-restored.png')
        command('key','--window',window,'ctrl+2');wait(lambda s:s.get('page')=='prepare')
        command('mousemove','--window',window,1240,397);time.sleep(.8);screenshot('tooltip-light.png')
        click(1354,397);screenshot('setting-help-light.png')
        (args.output/'results.json').write_text(json.dumps({'saved':saved,'restored_scheme':preview['preview_scheme'],'source_edited':orbit['edited']},indent=2)+'\n')
        close();print('Preferences theme, mouse controls, persistence, Preview scheme and setting help passed.')
    finally:
        if process and process.poll() is None:process.terminate();process.wait(timeout=10)


if __name__=='__main__':main()
