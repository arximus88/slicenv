#!/usr/bin/env python3
"""Replay navigation cube and ghost section controls on an isolated 1560x940 app.

Open angel_medium.3mf in Prepare with SLICENV_CLOUD_OFF=1, SLICENV_UI_TRACE=1;
pass the app window ID and trace log. No source saves or printer actions.
"""
import argparse
import json
import math
import os
from pathlib import Path
import subprocess
import time


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--display',required=True)
    p.add_argument('--window',required=True)
    p.add_argument('--log',type=Path,required=True)
    p.add_argument('--output',type=Path,required=True)
    args=p.parse_args()
    args.output.mkdir(parents=True,exist_ok=True)
    env=dict(os.environ,DISPLAY=args.display)
    def command(*items):
        subprocess.run(['xdotool',*map(str,items)],env=env,check=True,capture_output=True)
    def state():
        rows=[json.loads(l.split('UI_TRACE ',1)[1]) for l in args.log.read_text().splitlines() if 'UI_TRACE ' in l]
        return rows[-1] if rows else None
    def wait(predicate,timeout=20):
        end=time.monotonic()+timeout
        while time.monotonic()<end:
            s=state()
            if s and predicate(s):return s
            time.sleep(.05)
        raise AssertionError(f'timeout: {state()}')
    def click(x,y):
        command('mousemove','--window',args.window,int(x),int(y),'click','1')
        time.sleep(.25)
    def shot(name):
        time.sleep(.2)
        subprocess.run(['import','-window',args.window,str(args.output/name)],env=env,check=True)
    def drag(x,y,dx,dy):
        command('mousemove','--window',args.window,int(x),int(y),'mousedown','1')
        time.sleep(.1)
        for i in range(1,9):
            command('mousemove','--window',args.window,int(x+dx*i/8),int(y+dy*i/8))
            time.sleep(.035)
        command('mouseup','1')
        time.sleep(.3)
    initial=wait(lambda s:s['page']=='prepare' and not s['engine_loading'])
    x,y,w,h=initial['viewport']
    command('windowfocus',args.window,'key','--window',args.window,'0')
    iso=wait(lambda s:abs(s['camera'][1]-math.radians(35))<.001)
    # Top face; snapping changes only angles.
    click(x+w-12-132+66,y+12+40)
    top=wait(lambda s:s['camera'][1]>1.55 and not s['navigation_tween'])
    assert top['camera'][2:]==iso['camera'][2:],'snap changed target/zoom'
    assert top['edited']==initial['edited'],'cube click edited geometry'
    shot('cube-top.png')
    # Capture continues outside the cube over the settings dock.
    drag(x+w-12-132+66,y+12+60,100,32)
    orbit=wait(lambda s:s['camera']!=top['camera'] and not s['navigation_drag'])
    assert orbit['camera'][2:]==iso['camera'][2:],'cube drag panned/zoomed'
    assert orbit['edited']==initial['edited'],'cube drag edited geometry'
    command('key','--window',args.window,'0')
    iso=wait(lambda s:abs(s['camera'][1]-math.radians(35))<.001)
    click(x+w/2+54,y+h-35)
    cut=wait(lambda s:s['section'])
    camera=cut['camera']
    # Card is below the cube: move the cut to the middle.
    drag(x+w-30,y+174+53,-110,0)
    middle=wait(lambda s:200<s['section_position']<800)
    assert middle['camera']==camera,'section slider moved camera'
    assert middle['section_ghost'],'ghost guide not enabled by default'
    shot('section-z.png')
    # Plane axes X and Y without changing camera or source geometry.
    for axis,offset in ((0,98),(1,66)):
        click(x+w-offset,y+174+23)
        changed=wait(lambda s:s['section_axis']==axis)
        assert changed['camera']==camera
        shot(f'section-{axis}.png')
    click(x+w-150,y+174+83)
    hidden=wait(lambda s:not s['section_ghost'])
    assert hidden['section_position']==middle['section_position']
    assert hidden['camera']==camera
    click(x+w-150,y+174+83)
    wait(lambda s:s['section_ghost'])
    final=state()
    assert final['edited']==initial['edited']
    (args.output/'results.json').write_text(json.dumps({'top':top['camera'],'drag':orbit['camera'],
        'section_position':middle['section_position'],'source_edited':final['edited']},indent=2)+'\n')
    print('Cube snapping/drag capture and X/Y/Z ghost sections passed; geometry and camera isolation preserved.')


if __name__=='__main__':main()
