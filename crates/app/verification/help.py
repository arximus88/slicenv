#!/usr/bin/env python3
"""Native illustrated help + pattern icons smoke replay (1560x940 Xvfb).
Uses English, private XDG trees and disabled cloud; no printer or slicing calls.
"""
import argparse,json,os,subprocess,time
from pathlib import Path


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--display',required=True);p.add_argument('--output',type=Path,required=True)
    p.add_argument('--model',type=Path,required=True);p.add_argument('--binary',type=Path,default=Path('target/debug/slicenv'))
    a=p.parse_args();out=a.output.resolve();out.mkdir(parents=True,exist_ok=True)
    env=dict(os.environ,DISPLAY=a.display,SLICENV_CLOUD_OFF='1',SLICENV_PROFILE_ROOT=str(Path.home()/'.config/BambuStudio'))
    env.pop('WAYLAND_DISPLAY',None)
    for key in ('CONFIG','DATA','CACHE'):env[f'XDG_{key}_HOME']=str(out/key.lower())
    root=out/'config/slicenv';root.mkdir(parents=True,exist_ok=True)
    (root/'preferences.json').write_text(json.dumps({'language':'en','light':True}))
    def x(*v,check=True):return subprocess.run(['xdotool',*map(str,v)],env=env,capture_output=True,check=check)
    x('keyup','Control_L','Control_R','q','Escape')
    with (out/'gui.log').open('w') as f:app=subprocess.Popen([str(a.binary.resolve()),str(a.model.resolve())],env=env,stdout=f,stderr=f)
    try:
        deadline=time.monotonic()+20;win=None
        while time.monotonic()<deadline:
            r=x('search','--onlyvisible','--pid',app.pid,check=False)
            if r.returncode==0 and r.stdout.strip():win=r.stdout.decode().splitlines()[0];break
            assert app.poll() is None;time.sleep(.1)
        assert win;x('windowfocus',win);time.sleep(2)
        def click(u,v):x('mousemove','--window',win,u,v,'click','1');time.sleep(.25)
        def shot(name):subprocess.run(['import','-window',win,str(out/name)],env=env,check=True)
        x('mousemove','--window',win,1260,487);time.sleep(.9);shot('hover-light.png')
        click(1356,487);shot('full-light.png');x('key','--window',win,'Escape')
        click(1286,310);click(1450,459);shot('pattern-menu.png');x('key','--window',win,'Escape')
        x('key','--window',win,'ctrl+comma');time.sleep(.3);click(1004,258);x('key','--window',win,'Escape')
        click(1218,310);x('mousemove','--window',win,1260,487);time.sleep(.9);shot('hover-dark.png')
        x('key','--window',win,'ctrl+q',check=False);assert app.wait(timeout=15)==0
        print('Illustrated help light/dark and pattern-menu captures completed for visual inspection.')
    finally:
        if app.poll() is None:app.terminate();app.wait(timeout=15)
        x('keyup','Control_L','Control_R','q','Escape',check=False)


if __name__=='__main__':main()
