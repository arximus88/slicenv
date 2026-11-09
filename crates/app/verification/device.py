#!/usr/bin/env python3
"""Four printer tabs and motion states on an isolated fake printer, never live.
Requires Xvfb 1560x940. Commands have no transport: SLICENV_CLOUD_OFF is mandatory.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import time


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--display',required=True);p.add_argument('--output',type=Path,required=True)
    p.add_argument('--binary',type=Path,default=Path('target/debug/slicenv'))
    p.add_argument('--model',type=Path,required=True)
    args=p.parse_args();out=args.output.resolve();out.mkdir(parents=True,exist_ok=True)
    env=dict(os.environ,DISPLAY=args.display,SLICENV_CLOUD_OFF='1',SLICENV_PROFILE_ROOT=str(Path.home()/'.config/BambuStudio'))
    env.pop('WAYLAND_DISPLAY',None)
    for key in ('CONFIG','DATA','CACHE'):env[f'XDG_{key}_HOME']=str(out/key.lower())
    root=out/'config/slicenv';root.mkdir(parents=True,exist_ok=True)
    app=None
    def x(*items,check=True):return subprocess.run(['xdotool',*map(str,items)],env=env,capture_output=True,check=check)
    def click(a,b):x('mousemove','--window',win,a,b,'click','1');time.sleep(.3)
    def shot(name):subprocess.run(['import','-window',win,str(out/name)],env=env,check=True)
    try:
        for mode,light in [('printing',True),('idle',False)]:
            (root/'preferences.json').write_text(json.dumps({'language':'en','light':light}))
            env['SLICENV_FAKE_PRINTER']='idle' if mode=='idle' else '1'
            x('keyup','Control_L','Control_R','q','Escape')
            with (out/f'{mode}.log').open('w') as f:app=subprocess.Popen([str(args.binary.resolve()),str(args.model.resolve())],env=env,stdout=f,stderr=f)
            deadline=time.monotonic()+20;win=None
            while time.monotonic()<deadline:
                result=x('search','--onlyvisible','--pid',app.pid,check=False)
                if result.returncode==0 and result.stdout.strip():win=result.stdout.decode().splitlines()[0];break
                assert app.poll() is None;time.sleep(.1)
            assert win;x('windowfocus',win);time.sleep(2)
            x('key','--window',win,'ctrl+5');time.sleep(1);shot(f'{mode}-status.png')
            if mode=='printing':
                # A grey jog button still traverses the guard; it cannot enter the transport.
                click(1356,578);shot('printing-motion-blocked.png')
                for label,a in [('storage',415),('update',525),('hms',630)]:
                    click(a,90);shot(f'{label}.png')
            else:
                click(1356,610);shot('idle-home-confirm.png');x('key','--window',win,'Escape')
            x('key','--window',win,'Escape','ctrl+q',check=False);assert app.wait(timeout=15)==0
            app=None
        (out/'results.json').write_text(json.dumps({'fake_only':True,'language':'en','tabs':['status','storage','update','hms'],
            'scenarios':['printing','idle'],'screenshots_recorded':True,'real_commands':0},indent=2)+'\n')
        print('Fake printer tab and motion-state GUI replay completed; inspect screenshots alongside guard unit tests.')
    finally:
        if app and app.poll() is None:app.terminate();app.wait(timeout=15)
        x('keyup','Control_L','Control_R','q','Escape',check=False)


if __name__=='__main__':main()
