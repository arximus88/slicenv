#!/usr/bin/env python3
"""Per-span statistics of SLICENV_PROFILE_ALL logs and GUI budget check.

Input: one log per phase, lines `SLICENV_PROFILE <span> <ms>ms` (gui-bench.sh keeps
only the lines emitted during the gesture). Prints n/avg/p50/p95/max for every span,
then compares the budgeted spans with the `gui` section of bench/baselines.json:

  fail if avg > max(tolerance * baseline_avg, baseline_avg + floor_ms)
       or avg > budget_ms (absolute ceiling).

Spans with fewer than MIN_BASELINE_SAMPLES samples per run (the release after a drag,
live_drag_begin) are compared with budget_ms only: one sample is too noisy for 2x.

Also checks that each phase did what it claims (orbit has frames and no object drag,
drag starts a live drag and ends with a release). Exit 0 ok, 1 budget exceeded,
2 invalid run. `--update-baseline` rewrites gui.*.*.avg_ms, keeping budget_ms.
Usage: python3 -I stats.py --baselines bench/baselines.json orbit=orbit.log drag=drag.log
"""
import argparse
import collections
import json
import statistics
import sys

# Spans with budgets per phase; budget_ms is the hard ceiling used when no baseline exists.
BUDGETED = {
    'orbit': {'viewport::frame': 16.7, 'Workspace::render': 8.0},
    'drag': {'viewport::frame': 16.7, 'Workspace::render': 8.0, 'live_drag_begin': 1.0,
             'refresh_plate_mesh': 2.0, 'project_changed': 2.0},
    # Орбіта над нарізаним G-code (gui-bench.sh --preview); gpu_frame — від подання кадру
    # до готових пікселів, тобто затримка, яку відчуває рука.
    'preview_orbit': {'viewport::frame': 16.7, 'Workspace::render': 8.0, 'viewer::gpu_frame': 16.7},
}
# Minimal evidence that a phase exercised the intended path.
REQUIRED = {
    'orbit': {'viewport::frame': 30},
    'drag': {'viewport::frame': 20, 'live_drag_begin': 1, 'project_changed': 1},
    'preview_orbit': {'viewport::frame': 30, 'viewer::gpu_frame': 20},
}
FORBIDDEN = {'orbit': ['live_drag_begin'], 'preview_orbit': ['live_drag_begin']}
MIN_BASELINE_SAMPLES = 5


def parse(path):
    spans = collections.defaultdict(list)
    with open(path) as f:
        for line in f:
            parts = line.split()
            if len(parts) == 3 and parts[0] == 'SLICENV_PROFILE' and parts[2].endswith('ms'):
                spans[parts[1]].append(float(parts[2][:-2]))
    return spans


def summary(values):
    v = sorted(values)
    return {'n': len(v), 'avg_ms': statistics.mean(v), 'p50_ms': v[len(v) // 2],
            'p95_ms': v[min(int(len(v) * .95), len(v) - 1)], 'max_ms': v[-1]}


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('phases', nargs='+', metavar='PHASE=LOG')
    p.add_argument('--baselines')
    p.add_argument('--update-baseline', action='store_true')
    p.add_argument('--json', help='write the summary here')
    a = p.parse_args()

    stats = {}
    for item in a.phases:
        phase, _, path = item.partition('=')
        stats[phase] = {name: summary(v) for name, v in sorted(parse(path).items())}

    for phase, spans in stats.items():
        print(f'== {phase}')
        print(f'{"span":34} {"n":>5} {"avg":>8} {"p50":>8} {"p95":>8} {"max":>8}  (ms)')
        for name, s in spans.items():
            print(f'{name:34} {s["n"]:5} {s["avg_ms"]:8.3f} {s["p50_ms"]:8.3f} {s["p95_ms"]:8.3f} {s["max_ms"]:8.3f}')

    invalid = []
    for phase, spans in stats.items():
        for name, minimum in REQUIRED.get(phase, {}).items():
            if spans.get(name, {}).get('n', 0) < minimum:
                invalid.append(f'{phase}: {name} n={spans.get(name, {}).get("n", 0)} < {minimum}')
        for name in FORBIDDEN.get(phase, []):
            if name in spans:
                invalid.append(f'{phase}: {name} present — the gesture hit an object')

    data = {}
    if a.baselines:
        try:
            with open(a.baselines) as f:
                data = json.load(f)
        except FileNotFoundError:
            data = {}
    tolerance = data.get('tolerance', 2.0)
    floor = data.get('floor_ms', 0.05)
    gui = data.setdefault('gui', {})
    failed = False
    print(f'== budgets (fail: avg > max({tolerance}x baseline, baseline+{floor}) or avg > budget_ms)')
    for phase, spans in stats.items():
        for name, default_budget in BUDGETED.get(phase, {}).items():
            if name not in spans:
                continue
            entry = gui.setdefault(phase, {}).setdefault(name, {})
            budget = entry.get('budget_ms', default_budget)
            avg = spans[name]['avg_ms']
            base = entry.get('avg_ms')
            limit = budget if base is None else min(budget, max(base * tolerance, base + floor))
            ok = avg <= limit
            failed |= not ok and not a.update_baseline
            base_text = '—' if base is None else f'{base:.3f}'
            print(f'{phase:6} {name:28} avg {avg:8.3f}  baseline {base_text:>7}  limit {limit:7.3f}  '
                  f'{"ok" if ok else "OVER BUDGET"}')
            if a.update_baseline:
                entry['budget_ms'] = budget
                if spans[name]['n'] >= MIN_BASELINE_SAMPLES:
                    entry.update(avg_ms=round(avg, 4), p95_ms=round(spans[name]['p95_ms'], 4))
                else:
                    entry.pop('avg_ms', None)
                    entry.pop('p95_ms', None)

    if a.json:
        with open(a.json, 'w') as f:
            json.dump(stats, f, indent=2)
    if invalid:
        print('INVALID RUN:\n  ' + '\n  '.join(invalid))
        return 2
    if a.update_baseline and a.baselines:
        with open(a.baselines, 'w') as f:
            json.dump(data, f, indent=2, ensure_ascii=False)
            f.write('\n')
        print(f'baseline updated: {a.baselines}')
    print('gui bench: OVER BUDGET' if failed else 'gui bench: all budgets met')
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(main())
