#!/usr/bin/env python3
"""Bambu Studio PrintConfigDef, static FFF layout, scopes and Ukrainian metadata.

Usage: python3 -I tools/generate.py /path/to/BambuStudio [--source orca].
Невідомі вирази потрапляють у звіт; сторонні пакети не потрібні.
"""
import argparse
import ast
import copy
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

STRING = r'(?:u8|L)?"(?:\\.|[^"\\])*"'
TOKEN = re.compile(STRING + r'|//[^\n]*|/\*[\s\S]*?\*/')


def clean(s):
    s = TOKEN.sub(lambda m: m[0] if not m[0].startswith('/') else ' ' * len(m[0]), s)
    # Ignore compile-disabled blocks, retain the union of other conditional branches.
    stack, lines = [], []
    for line in s.splitlines(keepends=True):
        directive = re.match(r'\s*#\s*(if|ifdef|ifndef|elif|else|endif)\b(.*)', line)
        if directive:
            op, expr = directive.groups()
            if op in ('if', 'ifdef', 'ifndef'):
                stack.append(expr.strip() == '0' if op == 'if' and expr.strip() in ('0', '1') else None)
            elif op in ('else', 'elif') and stack:
                stack[-1] = not stack[-1] if stack[-1] is not None else None
            elif op == 'endif' and stack:
                stack.pop()
        elif not any(stack):
            lines.append(line)
    return ''.join(lines)


def strings(s):
    return [ast.literal_eval(re.sub(r'^(u8|L)', '', m[0])) for m in re.finditer(STRING, s)]


def split(s):
    parts, start, stack = [], 0, []
    masked = re.sub(STRING, lambda m: ' ' * len(m[0]), s)
    for i, c in enumerate(masked):
        if c in '({[':
            stack.append(c)
        elif c in ')}]':
            stack.pop()
        elif c == ',' and not stack:
            parts.append(s[start:i].strip())
            start = i + 1
    return parts + ([s[start:].strip()] if s[start:].strip() else [])


def balanced(s, start):
    opening = s[start]
    closing = {'(': ')', '{': '}', '[': ']'}[opening]
    masked = re.sub(STRING, lambda m: ' ' * len(m[0]), s)
    depth = 0
    for i in range(start, len(s)):
        if masked[i] == opening:
            depth += 1
        elif masked[i] == closing:
            depth -= 1
            if depth == 0:
                return s[start + 1:i], i + 1
    raise ValueError('unbalanced C++')


def function(s, name):
    m = re.search(re.escape(name) + r'\([^;]*?\)\s*\{', s)
    if not m:
        raise ValueError('function missing: ' + name)
    return balanced(s, m.end() - 1)[0]


def calls(s, pattern):
    for m in re.finditer('(?:' + pattern + r')\s*\(', s):
        body, end = balanced(s, m.end() - 1)
        yield m, split(body), end


def replace_loop(s, pattern, replacements):
    m = re.search(pattern, s)
    if not m:
        raise ValueError('expected loop missing: ' + pattern)
    begin = s.index('{', m.end())
    body, end = balanced(s, begin)
    return s[:m.start()] + '\n'.join(replacements(body)) + s[end:]


def format_documentation_url(path, orca):
    if not path:
        return None
    if orca:
        return f"https://www.orcaslicer.com/wiki/{path}"
    if "#" in path:
        base, anchor = path.split("#", 1)
        anchor = anchor.lower().replace(" ", "-")
        return f"https://wiki.bambulab.com/en/software/bambu-studio/{base}#{anchor}"
    return f"https://wiki.bambulab.com/en/software/bambu-studio/{path}"


def parse_po(contents):
    """Read singular gettext messages; fuzzy, obsolete and plural entries are excluded.

    Context is part of the lookup key, never guessed from an unrelated entry.
    ast.literal_eval decodes quoted escapes without executing PO contents.
    """
    catalog, entry, active, fuzzy = {}, {}, None, False

    def flush():
        if not fuzzy and 'msgid_plural' not in entry and entry.get('msgid') and entry.get('msgstr'):
            catalog[(entry.get('msgctxt', ''), entry['msgid'])] = entry['msgstr']

    for line in contents.splitlines() + ['']:
        line = line.strip()
        if not line:
            flush()
            entry, active, fuzzy = {}, None, False
        elif line.startswith('#'):
            # A new entry need not be separated by an empty line.
            if active and active.startswith('msgstr'):
                flush()
                entry, active, fuzzy = {}, None, False
            if line.startswith('#,'):
                fuzzy |= 'fuzzy' in line[2:].replace(' ', '').split(',')
        else:
            match = re.fullmatch(r'(msgctxt|msgid|msgid_plural|msgstr(?:\[\d+\])?)\s+(".*")', line)
            if match:
                field, literal = match.groups()
                if field in ('msgctxt', 'msgid') and active and active.startswith('msgstr'):
                    flush()
                    entry, fuzzy = {}, False
                active = field
                entry[active] = ast.literal_eval(literal)
            elif line.startswith('"') and active:
                entry[active] += ast.literal_eval(line)
            else:
                raise ValueError('unsupported PO line: ' + line)
    return catalog


def translation_context(expr):
    if expr.strip().startswith('L_CONTEXT('):
        return ''.join(strings(split(expr[expr.index('(') + 1:-1])[1]))
    return ''


def localize(options, layout, catalog):
    counts = {}

    def translated(value, field, context=''):
        if not value.strip():
            return ''
        count = counts.setdefault(field, dict(total=0, translated=0))
        count['total'] += 1
        result = catalog.get((context, value), '')
        count['translated'] += bool(result)
        return result

    for option in options.values():
        contexts = option.pop('_contexts', {})
        option['uk'] = {field: translated(option[field], field, contexts.get(field, ''))
                        for field in ['label', 'full_label', 'tooltip', 'sidetext', 'category']}
        option['uk']['enum_labels'] = [translated(label, 'enum_labels', context)
                                      for label, context in zip(option['enum_labels'],
                                      (contexts.get('enum_labels', []) + [''] * len(option['enum_labels'])))]
    for pages in layout.values():
        for page in pages:
            page['title_uk'] = translated(page['title'], 'page_titles')
            for group in page['groups']:
                group['title_uk'] = translated(group['title'], 'group_titles')
    for count in counts.values():
        count['percent'] = 100 * count['translated'] / count['total']
    return counts


def add_scopes(options, header):
    for opt in options.values():
        opt['scope'] = 'global'
    counts = {}
    for cls, scope in [('PrintObjectConfig', 'object'), ('PrintRegionConfig', 'region')]:
        matches = [args for _, args, _ in calls(header, r'PRINT_CONFIG_CLASS_DEFINE') if args[0] == cls]
        if len(matches) != 1:
            raise ValueError('expected one config class: ' + cls)
        keys = re.findall(r'\(\(ConfigOption[^,]+,\s*(\w+)\s*\)\)', matches[0][1])
        if not keys:
            raise ValueError('no config members: ' + cls)
        for key in keys:
            if key in options:
                options[key]['scope'] = scope
        counts[scope] = sum(opt['scope'] == scope for opt in options.values())
    counts['global'] = sum(opt['scope'] == 'global' for opt in options.values())
    return counts


def generate(root, output, source="bambu"):
    orca = source == "orca"
    paths = ['src/libslic3r/PrintConfig.cpp', 'src/libslic3r/PrintConfig.hpp',
             'src/libslic3r/PrintConfigConstants.hpp', 'src/libslic3r/Config.hpp',
             'src/libslic3r/MaterialType.cpp', 'src/libslic3r/PublishSettings.cpp',
             'src/libslic3r/Preset.cpp', 'src/slic3r/GUI/ConfigManipulation.cpp',
             'bbl/i18n/uk/BambuStudio_uk.po', 'src/slic3r/GUI/Tab.cpp']
    sources = {p: (root / p).read_text() for p in paths if (root / p).exists()}
    cpp = clean(sources[paths[0]])
    tab = clean(sources[paths[-1]])
    warnings = []
    constants = {'true': True, 'false': False, 'FLT_MAX': 3.4028234663852886e38,
                 'INT_MAX': 2147483647, 'INT_MIN': -2147483648}
    for text in sources.values():
        for k, v in re.findall(r'^#define\s+(\w+)\s+([-\d.]+|true|false)\s*$', text, re.M):
            constants[k] = constants.get(v, float(v) if v not in ('true', 'false') else v == 'true')
    for k, v in re.findall(r'const (?:int|double) (\w+) = ([\d.]+);', cpp):
        constants[k] = float(v)
    constants['-max_temp'] = -constants['max_temp']
    maps = {}
    for m in re.finditer(r'static (?:const )?t_config_enum_values\s+s_keys_map_(\w+)\s*(?:=\s*)?\{', cpp):
        body, _ = balanced(cpp, m.end() - 1)
        maps[m[1]] = {value.strip().removeprefix('int(').removesuffix(')').split('::')[-1]: key
                      for key, value in re.findall(r'\{\s*"([^"]+)"\s*,\s*([^{}]+)\}', body)}

    def number(expr):
        expr = expr.strip()
        if expr in constants:
            return constants[expr]
        if re.fullmatch(r'[-+]?\d+(?:\.\d*)?(?:[eE][-+]?\d+)?[fFuUlL]*', expr):
            return float(re.sub(r'[fFuUlL]+$', '', expr))
        raise ValueError(expr)

    def text(expr):
        if re.fullmatch(r'(?:L|L_CONTEXT)?\s*\(?\s*' + STRING + r'(?:\s*' + STRING + r')*\s*\)?', expr.strip()):
            return ''.join(strings(expr))
        if expr.strip().startswith('L_CONTEXT('):
            return ''.join(strings(split(expr[expr.index('(') + 1:-1])[0]))
        raise ValueError(expr)

    def default(expr, opt):
        m = re.fullmatch(r'new ConfigOption(\w+)(?:<([^>]+)>)?\s*([({])([\s\S]*)[)}]', expr.strip())
        if not m:
            raise ValueError(expr)
        cls, enum, _, args = m.groups()
        args = args.strip()
        if args.startswith('{') and args.endswith('}'):
            args = args[1:-1]
        args = split(args)
        vector = opt['vector']
        if not args:
            return [] if vector else ('' if opt['type'] == 'string' else False if opt['type'] == 'bool' else 0)
        vals = []
        for arg in args:
            if 'nil_value()' in arg:
                vals.append('nil')
            elif cls.startswith('Enum'):
                enum_type = enum or opt.get('enum_type')
                symbol = re.sub(r'\(int\)|[()\s]', '', arg).split('::')[-1]
                vals.append(opt['enum_values'][int(symbol)] if symbol.isdigit() else maps[enum_type][symbol])
            elif cls.startswith(('FloatOrPercent', 'FloatsOrPercent')):
                pair = split(arg[arg.index('(') + 1:-1]) if 'FloatOrPercent(' in arg else args
                val = number(pair[0])
                vals.append(format(val, 'g') + ('%' if number(pair[1]) else ''))
                if not vector:
                    break
            elif cls.startswith(('Point',)):
                if 'Vec2d' not in arg and 'Vec3d' not in arg:
                    raise ValueError(arg)
                vals.append('x'.join(format(number(x), 'g') for x in split(arg[arg.index('(') + 1:-1])))
            elif opt['type'].startswith('string'):
                vals.append(text(arg))
            else:
                if 'NozzleVolumeType::' in arg:
                    symbol = re.sub(r'\(int\)|[()\s]', '', arg).split('::')[-1]
                    enum_source = clean(sources['src/libslic3r/PrintConfig.hpp'])
                    vals.append(number(re.search(r'\b' + symbol + r'\s*=\s*([^,]+)', enum_source)[1]))
                else:
                    vals.append(number(arg))
        return vals if vector else vals[0]

    body = '\n'.join(function(cpp, 'PrintConfigDef::' + n) for n in ['init_common_params', 'init_fff_params', 'init_sla_params'])
    # Розгортаємо три генератори опцій до звичайних визначень.
    if orca:
        body = replace_loop(body, r'for \(const char\* key : \{[^}]+\}\)', lambda b: [re.sub(r'\bkey\b', json.dumps(k), b) for k in
            ['print_plugin_config_overrides', 'printer_plugin_config_overrides', 'filament_plugin_config_overrides']])
    axes = re.search(r'std::vector<AxisDefault> axes\s*\{([\s\S]*?)\};', body)[1]
    axis_defs = re.findall(r'\{\s*"(\w)"\s*,\s*\{([^}]+)\}\s*,\s*\{([^}]+)\}\s*,\s*\{([^}]+)\}\s*\}', axes)
    def expand_axes(b):
        for axis, speed, accel, jerk in axis_defs:
            b2 = b.replace(' + axis.name', '').replace('axis.name', json.dumps(axis))
            b2 = re.sub(r'(this->add\("machine_max_\w+_)"', lambda m: m[1] + axis + '"', b2)
            for member, values in [('max_feedrate', speed), ('max_acceleration', accel), ('max_jerk', jerk)]:
                b2 = b2.replace('(' + 'axis.' + member + ')', '{' + values + '}')
            b2 = re.sub(r'\(boost::format\(("[^"]+")\) % axis_upper\)\.str\(\)',
                        lambda m: json.dumps(strings(m[1])[0].replace('%1%', axis.upper())), b2)
            yield b2
    body = replace_loop(body, r'for \(const AxisDefault &axis : axes\)', expand_axes)
    if orca:
        helper = re.search(r'auto add_belt_remap =', body).start()
        start = body.index('{', helper)
        helper_body, end = balanced(body, start)
        expanded = []
        for _, args, _ in calls(body[end:], r'\badd_belt_remap'):
            b = helper_body
            for var, val in zip(['key', 'label', 'tooltip', 'default_axis', 'mode'], args):
                b = re.sub(r'(?<!->)\b' + var + r'\b', lambda _: val, b)
            expanded.append(b)
        body = body[:helper] + '\n'.join(expanded) + re.sub(r'add_belt_remap\([\s\S]*?;', '', body[end:])
    # Цикл nullable копіює метадані вже визначених опцій.
    override_lists = ['filament_extruder_override_keys'] + ([] if orca else ['filament_overhang_override_keys'])
    for override_list in override_lists:
        override_match = re.search(r'for \(auto& opt_key : ' + override_list + r'\)', body)
        override_start = body.index('{', override_match.end())
        _, override_end = balanced(body, override_start)
        body = body[:override_match.start()] + body[override_end:]
    options, aliases = {}, {}
    type_map = {'Float':'float', 'Floats':'floats', 'Int':'int', 'Ints':'ints', 'Bool':'bool', 'Bools':'bools',
                'String':'string', 'Strings':'strings', 'Percent':'percent', 'Percents':'percents',
                'FloatOrPercent':'float_or_percent', 'FloatsOrPercents':'floats_or_percents',
                'Enum':'enum', 'Enums':'enums', 'Point':'point', 'Points':'points', 'Point3':'point3',
                'PointsGroups':'points_groups'}
    adds = list(calls(body, r'this->add'))
    for i, (m, args, end) in enumerate(adds):
        key, co = args
        key = text(key)
        alias = re.search(r'auto (\w+) = def = $', body[max(0,m.start()-100):m.start()])
        if alias:
            aliases[alias[1]] = key
        typ = type_map[co.removeprefix('co')]
        block = body[end:adds[i + 1][0].start() if i + 1 < len(adds) else len(body)]
        opt = dict(type=typ, label='', full_label='', tooltip='', sidetext='', category='', min=None, max=None,
                   enum_values=[], enum_labels=[], default=None, mode='simple', gui_type='undefined', multiline=False,
                   nullable=False, vector=typ in ['floats','ints','bools','strings','percents','floats_or_percents','enums','points','points_groups'],
                   per_extruder=False, per_filament=False, origin='print_config_def', _contexts={})
        enum_match = re.search(r'(?:ConfigOptionEnum<([^>]+)>::get_enum_values|&s_keys_map_(\w+))', block)
        if enum_match:
            opt['enum_type'] = enum_match[1] or enum_match[2]
        masked_block = re.sub(STRING, lambda m: ' ' * len(m[0]), block)
        for assignment in re.finditer(r'def->(label|full_label|tooltip|sidetext|category|min|max|mode|gui_type|multiline|nullable|enum_values|enum_labels)\s*=\s*', block):
            field = assignment[1]
            expr = block[assignment.end():masked_block.index(';', assignment.end())]
            ref = re.fullmatch(r'(\w+)->(\w+)', expr.strip())
            if ref:
                opt[field] = copy.deepcopy(options[aliases[ref[1]]][ref[2]])
                if ref[2] in options[aliases[ref[1]]]['_contexts']:
                    opt['_contexts'][field] = copy.deepcopy(options[aliases[ref[1]]]['_contexts'][ref[2]])
                continue
            if field in ['enum_values', 'enum_labels']:
                continue
            try:
                if field in ['label', 'full_label', 'tooltip', 'sidetext', 'category']:
                    opt[field] = text(expr)
                    opt['_contexts'][field] = translation_context(expr)
                elif field in ['min', 'max', 'multiline', 'nullable']:
                    opt[field] = number(expr)
                elif field == 'mode':
                    opt[field] = {'comSimple':'simple', 'comAdvanced':'advanced', 'comExpert':'expert', 'comDevelop':'develop'}[expr.strip()]
                else:
                    opt[field] = expr.strip().split('::')[-1]
            except (ValueError, KeyError):
                warnings.append(dict(key=key, field=field, expression=expr.strip()))
        for field in ['enum_values','enum_labels']:
            assigned = re.search(r'def->' + field + r'\s*=\s*\{', block)
            if assigned:
                values, _ = balanced(block, assigned.end() - 1)
                opt[field] = [text(v) for v in split(values)]
                opt['_contexts'][field] = [translation_context(v) for v in split(values)]
            for _, args2, _ in calls(block, r'def->' + field + r'\.(?:push_back|emplace_back)'):
                try:
                    if args2[0] == 'filament.name':
                        opt[field].extend(re.findall(r'\{\s*"([^"]+)"\s*,', function(clean(sources['src/libslic3r/MaterialType.cpp']), 'MaterialType::all')))
                    else:
                        opt[field].append(text(args2[0]))
                        opt['_contexts'].setdefault(field, []).append(translation_context(args2[0]))
                except ValueError:
                    warnings.append(dict(key=key, field=field, expression=args2[0]))
        for _, args2, _ in calls(block, r'def->set_default_value'):
            opt['default_expression'] = args2[0]
            try:
                opt['default'] = default(args2[0], opt)
            except (ValueError, KeyError):
                warnings.append(dict(key=key, field='default', expression=args2[0]))
        if 'Nullable' in opt.get('default_expression', ''):
            opt['nullable'] = True
        options[key] = opt
    for override_list in override_lists:
        overrides = re.search(override_list + r'\s*=\s*\{([\s\S]*?)\};', cpp)[1]
        for key in strings(overrides):
            opt = copy.deepcopy(options[key.removeprefix('filament_')])
            if override_list == 'filament_extruder_override_keys':
                opt['nullable'] = True
                opt['mode'] = 'simple' if key in ['filament_retraction_length','filament_z_hop','filament_long_retractions_when_cut','filament_retraction_distances_when_cut'] else 'advanced'
            else:
                opt['mode'] = 'advanced'
            opt['default_expression'] = 'copy printer/process override default'
            options[key] = opt
    for field, source_name in [('per_extruder','m_extruder_option_keys'),('per_filament','m_filament_option_keys')]:
        for key in strings(re.search(source_name + r'\s*=\s*\{([\s\S]*?)\};', cpp)[1]):
            options[key][field] = True
    for key, opt in options.items():
        opt['per_filament'] |= opt['vector'] and key.startswith('filament_')

    primary_count = len(options)
    if not orca:
        compatibility = json.loads((Path(__file__).parent / 'bambu_compatibility.json').read_text())
        for key, entry in compatibility.items():
            if key in options:
                raise ValueError('compatibility entry now has a primary definition: ' + key)
            opt = dict(type=entry['type'], label=entry['label'], full_label='', tooltip='',
                       sidetext='', category='', min=None, max=None, enum_values=[], enum_labels=[],
                       default=None, mode='advanced', gui_type='undefined', multiline=False,
                       nullable=False, vector=entry['type'] in ('floats', 'bools', 'strings'),
                       per_extruder=False, per_filament=key.startswith('filament_'), scope='global',
                       origin='bambu_compatibility', source_note=entry['source_note'])
            opt.update({k: v for k, v in entry.items() if k != 'source_note'})
            options[key] = opt
    layout, layout_warnings, doc_urls = extract_layout(tab, clean(sources.get('src/libslic3r/PublishSettings.cpp', '')), orca)
    for key, opt in options.items():
        if key in doc_urls:
            opt['documentation_url'] = doc_urls[key]
    scope_counts = add_scopes(options, clean(sources['src/libslic3r/PrintConfig.hpp']))
    catalog = parse_po(sources.get('bbl/i18n/uk/BambuStudio_uk.po', ''))
    translation_counts = localize(options, layout, catalog)
    referenced = {key for pages in layout.values() for p in pages for g in p['groups'] for line in g['lines'] for key in line['options']}
    doc_count = sum(1 for opt in options.values() if opt.get('documentation_url'))
    report = dict(compatibility_sha256=hashlib.sha256((Path(__file__).parent / 'bambu_compatibility.json').read_bytes()).hexdigest() if not orca else None,
                  source=source, source_commit=subprocess.check_output(['git','-C',str(root),'rev-parse','HEAD'], text=True).strip(),
                  sources={p: hashlib.sha256(s.encode()).hexdigest() for p,s in sources.items()},
                  option_count=len(options), primary_option_count=primary_count,
                  documentation_count=doc_count,
                  compatibility_keys=sorted(k for k,v in options.items() if v.get("origin") == "bambu_compatibility"), layout_pages={k:len(v) for k,v in layout.items()},
                  layout_option_count=len(referenced), missing_layout_keys=sorted(referenced-options.keys()),
                  unparsed=warnings, layout_unparsed=layout_warnings,
                  scope_counts=scope_counts, translations=translation_counts)
    output.mkdir(parents=True, exist_ok=True)
    for filename, data in [('options.json',dict(sorted(options.items()))),('layout.json',layout),('extraction.json',report)]:
        (output / filename).write_text(json.dumps(data, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps({k:v for k,v in report.items() if k != 'sources'}, indent=2))
    if warnings or layout_warnings:
        raise SystemExit('Unparsed expressions: inspect schema/extraction.json')


def extract_layout(tab, publish, orca=True):
    # Статичне об'єднання умовних сторінок; динамічну видимість визначає UI.
    result, warnings, doc_urls = {}, [], {}
    for kind, methods in [('process',['TabPrint::build']),('filament',['TabFilament::build']),
                          ('machine',['TabPrinter::build_fff','TabPrinter::build_kinematics_page','TabPrinter::build_unregular_pages'])]:
        pages = []
        for method in methods:
            body = function(tab, method)
            if method == 'TabFilament::build':
                body = body.replace('add_filament_overrides_page();', function(tab, 'TabFilament::add_filament_overrides_page'))
            # Розгортаємо прості цикли по літеральних списках і publishable-таблицях.
            lists = {m[1]: strings(m[2]) for m in re.finditer(r'(?:std::vector<std::string>|std::vector<t_config_option_key>)\s+(\w+)\s*\{([^}]+)\}', body)}
            loop_pattern = r'for\s*\(\s*const (?:std::string\s*&?|PublishablePrinterOption\s*&)\s*(\w+)\s*:\s*(\{[^}]*\}|\w+(?:\(\))?)\s*\)'
            for m in list(re.finditer(loop_pattern, body))[::-1]:
                var, collection = m.groups()
                rest = body[m.end():]
                offset = len(rest) - len(rest.lstrip())
                begin = m.end() + offset
                if body[begin] == '{':
                    loop, end = balanced(body, begin)
                else:
                    end = body.index(';', begin) + 1
                    loop = body[begin:end]
                if collection.startswith('{'):
                    values = strings(collection)
                elif collection.endswith('()'):
                    helper = function(publish, collection[:-2])
                    values = re.findall(r'\{\s*"([^"]+)"\s*,', helper)
                    loop = loop.replace(var + '.key', var)
                else:
                    values = lists.get(collection, [])
                if not values:
                    warnings.append(dict(method=method, expression=m[0]))
                    continue
                expanded = []
                for val in values:
                    line = re.sub(r'\b' + var + r'\b', json.dumps(val), loop)
                    line = re.sub(r'("[^"]*")\s*\+\s*("[^"]*")', lambda m: json.dumps(''.join(strings(m[0]))), line)
                    expanded.append(line)
                body = body[:m.start()] + '\n'.join(expanded) + body[end:]
            groups, variables, lines, line_paths = {}, {'page_name': 'Extruder'} if not orca else {}, {}, {}
            page = None
            # Process label_path assignments and append calls in source order. A path
            # belongs only to the next row built with that line variable.
            # Події розбираємо у вихідному порядку, зберігаючи спільні рядки.
            pattern = r'\bOption option|add_options_page|\w+->new_optgroup|\w+->get_option|\w+->append_single_option_line|\w+\.append_option|\w+->append_line|append_option_line|create_line_with_widget|append_retraction_option|append_ironing_option|(?<!->)\bappend_single_option_line'
            events = [(m.start(), 'call', m, args) for m, args, _ in calls(body, pattern)]
            events.extend((m.start(), 'path', m, []) for m in re.finditer(r'(\w+)\.label_path\s*=\s*([^;]+);', body))
            for _, event, m, args in sorted(events, key=lambda item: item[0]):
                if event == 'path':
                    lvar, expr = m.group(1), m.group(2).strip()
                    vals = strings(expr)
                    path = ''.join(vals) if vals else variables.get(expr)
                    if path:
                        line_paths[lvar] = path
                    else:
                        line_paths.pop(lvar, None)
                    continue
                name = m[0].split('(')[0].strip()
                before = body[max(0,m.start()-100):m.start()]
                assign = re.search(r'(\w+)\s*=\s*$', before)
                var = assign[1] if assign else None
                def resolve(expr):
                    vals = strings(expr)
                    return ''.join(vals) if vals else variables.get(expr.strip())
                if name == 'Option option':
                    variables['option'] = strings(args[-1])[0]
                elif name == 'add_options_page':
                    page = dict(title=resolve(args[0]), icon=resolve(args[1]), groups=[])
                    pages.append(page)
                elif name.endswith('->new_optgroup'):
                    if page is None:
                        continue
                    group = dict(title=resolve(args[0]), icon=resolve(args[1]) if len(args)>1 else '', lines=[])
                    page['groups'].append(group)
                    groups[var or 'optgroup'] = group
                elif name.endswith('->get_option'):
                    key = resolve(args[0])
                    if var and key:
                        variables[var] = key
                elif name.endswith('.append_option'):
                    linevar = name.split('.')[0]
                    key = resolve(args[0]) if 'get_option' not in args[0] else (strings(args[0]) or [None])[0]
                    if not key and args[0].strip() in variables:
                        key = variables[args[0].strip()]
                    if key:
                        lines.setdefault(linevar, []).append(key)
                elif name.endswith('->append_line'):
                    group = groups.get(name.split('->')[0])
                    linevar = args[0].strip()
                    keys = lines.pop(linevar, [])
                    path = line_paths.pop(linevar, None)
                    if keys and group is not None:
                        group['lines'].append(dict(options=keys))
                    if path:
                        url = format_documentation_url(path, orca)
                        if url:
                            for k in keys:
                                doc_urls[k] = url
                else:
                    groupvar = name.split('->')[0] if '->' in name else args[0].replace('.get()', '').strip()
                    group = groups.get(groupvar)
                    expr = args[0] if '->' in name else args[1] if len(args)>1 else ''
                    if name == 'append_single_option_line':
                        group = groups.get('optgroup')
                        expr = args[0]
                    if name == 'append_ironing_option':
                        group = groups.get('ironing_optgroup')
                        expr = args[0]
                    key = resolve(expr)
                    path = None
                    if 'append_single_option_line' in name and len(args) > 1:
                        path = resolve(args[1])
                    elif name == 'append_option_line' and len(args) > 2:
                        path = resolve(args[2])
                    elif name == 'create_line_with_widget' and len(args) > 2:
                        path = resolve(args[2])
                    if group is not None and key:
                        group['lines'].append(dict(options=[key]))
                    elif page is not None and group is not None:
                        warnings.append(dict(method=method, expression=m[0] + ', '.join(args) + ')'))
                    if key and path:
                        url = format_documentation_url(path, orca)
                        if url:
                            doc_urls[key] = url
        # Відтворюємо m_pages.insert для FFF з увімкненою кінематикою.
        if kind == 'machine':
            routine = function(tab, 'TabPrinter::build_unregular_pages')
            before = int(re.search(r'n_before_extruders\s*=\s*(\d+)', routine)[1])
            after = int(re.search(r'n_after_single_extruder_MM\s*=\s*(\d+)', routine)[1])
            dynamic = {p['title']: p for p in pages if p['title'] in ['Motion ability','Multimaterial','Extruder']}
            pages = [p for p in pages if p['title'] not in dynamic]
            pages.insert(before, dynamic['Motion ability'])
            if 'Multimaterial' in dynamic:
                pages.insert(len(pages) - after, dynamic['Multimaterial'])
            pages.insert(before + 1, dynamic['Extruder'])
        result[kind] = pages
    return result, warnings, doc_urls


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('root', type=Path)
    parser.add_argument('--source', choices=['bambu', 'orca'], default='bambu')
    parser.add_argument('--output', type=Path, default=Path(__file__).resolve().parents[1] / 'schema')
    args = parser.parse_args()
    generate(args.root, args.output, args.source)
