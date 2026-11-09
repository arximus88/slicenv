#!/usr/bin/env python3
"""Replay on a dedicated :84 Xvfb display, 1600x1000, default docks.
Usage: python3 tools/profile-ui-replay.py /tmp/app-profile.log
Launch the app with >> so SCENARIO markers share its append-only log safely.
Run only after the model and profiles have finished loading.
"""
import os
import subprocess
import sys
import time

ENV = {**os.environ, "DISPLAY": ":84"}
DELAY = float(os.environ.get("SLICENV_REPLAY_DELAY", os.environ.get("BAMBU_REPLAY_DELAY", "0.3")))


def xd(*args):
    return subprocess.check_output(["xdotool", *map(str, args)], env=ENV)


def mark(scenario):
    with open(sys.argv[1], "a") as log:
        log.write(f"SCENARIO {scenario}\n")


def click(x, y):
    xd("mousemove", x, y, "click", 1)
    time.sleep(DELAY)


window = xd("search", "--name", "^Bambu").splitlines()[-1].decode()
xd("windowfocus", window)
pid = int(xd("getwindowpid", window))
time.sleep(2)
mark("typing")
click(1430, 453)
xd("key", "ctrl+a")
xd("type", "--delay", 100, "0.24")
xd("key", "Return")
time.sleep(1)
mark("checkbox")
for _ in range(5):
    click(1403, 605)
mark("tabs")
for x in [1304, 1374, 1436, 1238] * 3:
    click(x, 340)
mark("selection")
for _ in range(5):
    click(120, 136)
    click(550, 300)
time.sleep(3)
mark("idle")


def ticks():
    with open(f"/proc/{pid}/stat") as stat:
        # Skip the comm field, which can contain spaces in parentheses.
        fields = stat.read().rsplit(")", 1)[1].split()
    return int(fields[11]) + int(fields[12])


started_ticks = ticks()
started = time.monotonic()
time.sleep(5)
cpu = 100 * (ticks() - started_ticks) / os.sysconf("SC_CLK_TCK")
print(f"Idle CPU: {cpu / (time.monotonic() - started):.2f}% of one core")
mark("end")
