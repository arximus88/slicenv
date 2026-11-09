#!/usr/bin/env python3
"""Native language switching, dirty-preset preservation and custom-pack fallback.
Requires an isolated Xvfb display at 1560x940. Cloud is disabled; private XDG
configuration only, no slicing, file saves or printer actions.
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
    p.add_argument('--output',type=Path,required=True)
    p.add_argument('--model',type=Path,required=True)
    p.add_argument('--binary',type=Path,default=Path('target/debug/slicenv'))
    args=p.parse_args();output=args.output.resolve();output.mkdir(parents=True,exist_ok=True)
    env=dict(os.environ,DISPLAY=args.display,SLICENV_CLOUD_OFF='1',SLICENV_UI_TRACE='1',SLICENV_PROFILE_ROOT=str(Path.home()/'.config/BambuStudio'))
    env.pop('WAYLAND_DISPLAY',None)
    for key in ('CONFIG','DATA','CACHE'):env[f'XDG_{key}_HOME']=str(output/key.lower())
    root=output/'config/slicenv';(root/'locales').mkdir(parents=True,exist_ok=True)
    (root/'preferences.json').write_text('{"language":"uk"}')
    (root/'locales/es.json').write_text(json.dumps({'_language_name':'Español','Головна':'Inicio'},ensure_ascii=False))
    app=None;window=None;log=None
    def x(*items,check=True):return subprocess.run(['xdotool',*map(str,items)],env=env,capture_output=True,check=check)
    def state():
        rows=[json.loads(l.split('UI_TRACE ',1)[1]) for l in log.read_text().splitlines() if 'UI_TRACE ' in l]
        return rows[-1] if rows else {}
    def wait(predicate):
        end=time.monotonic()+30
        while time.monotonic()<end:
            assert app.poll() is None,f'app exited {app.returncode}'
            s=state()
            if predicate(s):return s
            time.sleep(.05)
        raise AssertionError(f'timeout: {state()}')
    def launch(name):
        nonlocal app,window,log
        x('keyup','Control_L','Control_R','q','Escape')
        log=output/f'{name}.log'
        with log.open('w') as f:app=subprocess.Popen([str(args.binary.resolve()),str(args.model.resolve())],env=env,stdout=f,stderr=f)
        s=wait(lambda s:s.get('engine') and not s['engine_loading'])
        window=x('search','--onlyvisible','--pid',app.pid).stdout.decode().splitlines()[0];x('windowfocus',window)
        return s
    def click(a,b):x('mousemove','--window',window,a,b,'click','1');time.sleep(.2)
    def screenshot(name):subprocess.run(['import','-window',window,str(output/name)],env=env,check=True)
    def close():
        x('key','--window',window,'Escape','ctrl+q',check=False);assert app.wait(timeout=15)==0
    try:
        launch('switching')
        # Dirty the layer height; changing language must keep the editor override.
        click(1430,397);x('key','--window',window,'ctrl+a');x('type','--window',window,'0.24');x('key','--window',window,'Return')
        x('mousemove','--window',window,800,500,'click','4')
        wait(lambda s:s.get('process_dirty'))
        x('key','--window',window,'ctrl+comma');time.sleep(.2)
        click(608,174);english=wait(lambda s:s.get('language')=='en')
        assert english['process_dirty'] and not english['edited'];screenshot('preferences-en.png')
        x('key','--window',window,'Escape');time.sleep(.2);screenshot('prepare-en.png')
        x('key','--window',window,'ctrl+comma');time.sleep(.2);click(530,174)
        ukrainian=wait(lambda s:s.get('language')=='uk');assert ukrainian['process_dirty']
        screenshot('preferences-uk.png')
        click(690,174);custom=wait(lambda s:s.get('language')=='es');assert custom['process_dirty']
        screenshot('preferences-es-fallback.png')
        assert json.loads((root/'preferences.json').read_text())['language']=='es'
        close();restored=launch('restart');assert restored['language']=='es'
        screenshot('custom-restored.png');close()
        (output/'results.json').write_text(json.dumps({'english':english['language'],'ukrainian':ukrainian['language'],
            'custom':custom['language'],'dirty_preserved':english['process_dirty'] and ukrainian['process_dirty'] and custom['process_dirty'],
            'restored':restored['language']},indent=2)+'\n')
        print('Language switching, dirty preset, custom English fallback and restart passed.')
    finally:
        if app and app.poll() is None:app.terminate();app.wait(timeout=15)
        x('keyup','Control_L','Control_R','q','Escape',check=False)


if __name__=='__main__':main()
