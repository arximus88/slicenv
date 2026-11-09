#!/usr/bin/env python3
"""Таблиці Markdown з bench/compare-<дата>.json (tools/bench/compare.py).

Кожна клітинка — «медіана [min–max]» по прогонах (n — у заголовку розділу).
Використання: python3 -I compare_table.py bench/compare-DATE.json
"""
import json
import sys


def cell(summary, key, digits=1):
    v = summary.get(key)
    if not v:
        return '—'
    fmt = f'{{:.{digits}f}}'
    med = fmt.format(v['median'])
    if v['n'] > 1 and v['min'] != v['max']:
        return f'{med} [{fmt.format(v["min"])}–{fmt.format(v["max"])}]'
    return med


def table(title, rows, cols, data, scenario):
    present = [a for a in rows if scenario in data.get(a, {})]
    if not present:
        return ''
    n = max((data[a][scenario].get('ready_s', data[a][scenario].get('wall_s', {})) or {}).get('n', 0)
            for a in present)
    out = [f'### {title} (n = {n})', '', '| Застосунок | ' + ' | '.join(c[0] for c in cols) + ' |',
           '|---|' + '---:|' * len(cols)]
    for a in present:
        s = data[a][scenario]
        line = [cell(s, key, d) for _, key, d in cols]
        errs = s.get('errors') or []
        name = a + (f' ⚠ {len(errs)} помилк.' if errs else '')
        out.append(f'| {name} | ' + ' | '.join(line) + ' |')
    return '\n'.join(out) + '\n'


def main():
    report = json.load(open(sys.argv[1]))
    data = report['summary']
    apps = [a for a in data if not a.startswith('cli:')]
    cli = [a for a in data if a.startswith('cli:')]
    print(f"Дата: {report['date']}, {report['host']['cpu']}, ядро {report['host']['kernel']}\n")
    print(table('Холодний старт без файлу → простій 10 с', apps, [
        ('вікно, с', 'main_window_s', 2), ('готовий, с', 'ready_s', 2),
        ('CPU старту, с', 'startup.cpu_s', 2), ('PSS старту пік, МБ', 'startup.pss_peak_mb', 0),
        ('простій CPU, %', 'idle.cpu_avg_pct', 1), ('простій PSS, МБ', 'idle.pss_end_mb', 0),
        ('простій RSS, МБ', 'idle.rss_end_mb', 0), ('VRAM, МБ', 'idle.vram_end_mb', 0),
        ('GTT, МБ', 'idle.gtt_end_mb', 0)], data, 'start'))
    for fx in ('small', 'heavy'):
        sc = f'open-{fx}'
        print(table(f'Відкриття {fx}.3mf (з аргументу) → готовий → простій 5 с', apps, [
            ('вікно, с', 'main_window_s', 2), ('модель (маркер), с', 'objects_loaded_s', 2),
            ('готовий, с', 'ready_s', 2), ('CPU до готовності, с', 'startup.cpu_s', 2),
            ('PSS пік, МБ', 'startup.pss_peak_mb', 0), ('простій PSS, МБ', 'idle.pss_end_mb', 0),
            ('простій CPU, %', 'idle.cpu_avg_pct', 1), ('VRAM, МБ', 'idle.vram_end_mb', 0),
            ('GTT, МБ', 'idle.gtt_end_mb', 0)], data, sc))
        print(table(f'Камера над сценою {fx}: орбіта (100 кроків) і зум (20 кліків)', apps, [
            ('орбіта CPU, %', 'orbit.cpu_avg_pct', 0), ('орбіта GPU, %', 'orbit.gpu_busy_pct', 1),
            ('композитор GPU, %', 'orbit.compositor_gpu_busy_pct', 1),
            ('зум CPU, %', 'zoom.cpu_avg_pct', 0), ('зум GPU, %', 'zoom.gpu_busy_pct', 1),
            ('VRAM пік, МБ', 'orbit.vram_peak_mb', 0)], data, sc))
        print(table(f'Нарізка пластини 1 ({fx}): Ctrl+R → Preview', apps, [
            ('до тиші CPU, с', 'slice_wall_s', 2), ('slice::background, с', 'slice_background_s', 2),
            ('до шарів у Preview, с', 'slice_to_preview_s', 2), ('CPU, с', 'slice.cpu_s', 1),
            ('CPU пік, %', 'slice.cpu_peak_pct', 0), ('PSS пік, МБ', 'slice.pss_peak_mb', 0),
            ('орбіта Preview CPU, %', 'preview_orbit.cpu_avg_pct', 0),
            ('орбіта Preview GPU, %', 'preview_orbit.gpu_busy_pct', 1),
            ('VRAM Preview, МБ', 'preview_orbit.vram_peak_mb', 0)], data, sc))
        print(table(f'Прямий CLI рушія ({fx}, пластина 1, ті самі пресети й прапорці, що в Slicenv)', cli, [
            ('стіна, с', 'wall_s', 2), ('CPU (rusage), с', 'cpu_s_rusage', 2),
            ('CPU сер., %', 'cpu_avg_pct', 0), ('max RSS процесу, МБ', 'maxrss_mb_rusage', 0),
            ('PSS пік, МБ', 'pss_peak_mb', 0)], data, f'cli-{fx}'))
    print('### Диск\n\n| Застосунок | Встановлення, МБ | Профіль/кеш після прогонів, МБ |\n|---|---:|---:|')
    for a, d in report['disk_bytes'].items():
        inst = sum(v for k, v in d.items() if k != 'profile_after_runs')
        print(f'| {a} | {inst / 2**20:.0f} | {d["profile_after_runs"] / 2**20:.1f} |')


if __name__ == '__main__':
    main()
