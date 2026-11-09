#!/usr/bin/env python3
"""Compare two generated settings catalogs without modifying either one.

Usage: python3 -I crates/settings/tools/compare_catalogs.py OLD_DIR NEW_DIR
"""
import argparse
import json
from pathlib import Path
import sys


def read_catalog(directory):
    directory = Path(directory)
    result = {}
    for name in ("options.json", "layout.json", "extraction.json"):
        with (directory / name).open(encoding="utf-8") as stream:
            result[name[:-5]] = json.load(stream)
    if not isinstance(result["options"], dict) or not isinstance(result["layout"], dict):
        raise ValueError(f"{directory}: options/layout must be JSON objects")
    if not isinstance(result["extraction"], dict):
        raise ValueError(f"{directory}: extraction must be a JSON object")
    return result


def stable(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def md(value):
    return "`" + stable(value).replace("`", "\\`").replace("|", "\\|").replace("\n", " ") + "`"


def source_lines(label, report):
    lines = [f"- **{label}:** source `{report.get('source', 'unknown')}`, commit `{report.get('source_commit', 'unknown')}`."]
    sources = report.get("sources", {})
    if sources:
        lines.append("  Source file hashes:")
        for path, digest in sorted(sources.items()):
            lines.append(f"  - `{path}`: `{digest}`")
    return lines


def layout_entries(layout):
    """Map each ordered layout location to its JSON value for precise reporting."""
    entries = {}
    for page_set, pages in sorted(layout.items()):
        if not isinstance(pages, list):
            entries[f"{page_set}"] = pages
            continue
        for pi, page in enumerate(pages):
            ptitle = page.get("title", "") if isinstance(page, dict) else ""
            entries[f"{page_set}/page[{pi}] {ptitle}"] = {k: v for k, v in page.items() if k != "groups"} if isinstance(page, dict) else page
            for gi, group in enumerate(page.get("groups", []) if isinstance(page, dict) else []):
                gtitle = group.get("title", "") if isinstance(group, dict) else ""
                base = f"{page_set}/page[{pi}] {ptitle}/group[{gi}] {gtitle}"
                entries[base] = {k: v for k, v in group.items() if k != "lines"} if isinstance(group, dict) else group
                for li, line in enumerate(group.get("lines", []) if isinstance(group, dict) else []):
                    entries[f"{base}/line[{li}]"] = line
    return entries


def compare(old, new, old_label="OLD", new_label="NEW"):
    """Return deterministic Markdown describing catalog differences."""
    before, after = old["options"], new["options"]
    added, removed = sorted(after.keys() - before.keys()), sorted(before.keys() - after.keys())
    changed = []
    for key in sorted(before.keys() & after.keys()):
        a, b = before[key], after[key]
        fields = sorted(set(a) | set(b))
        diffs = [(field, a.get(field, _MISSING), b.get(field, _MISSING))
                 for field in fields if a.get(field, _MISSING) != b.get(field, _MISSING)]
        if diffs:
            changed.append((key, diffs))

    lines = ["# Settings catalog comparison", "", f"Compared `{old_label}` → `{new_label}`.", "",
             "> This report compares extracted metadata only. It does not establish slicer engine support.", "",
             "## Source provenance", ""]
    lines += source_lines(old_label, old["extraction"])
    lines += source_lines(new_label, new["extraction"])
    lines += ["", "## Options", "", f"- Added: **{len(added)}**", f"- Removed: **{len(removed)}**",
              f"- Changed: **{len(changed)}**", ""]
    for title, keys in (("Added options", added), ("Removed options", removed)):
        lines += [f"### {title}", ""]
        if not keys:
            lines += ["None.", ""]
        else:
            lines += [f"- `{key}`" + (f": {md(after[key])}" if title.startswith("Added") else f": {md(before[key])}") for key in keys]
            lines.append("")
    lines += ["### Changed options", ""]
    if not changed:
        lines += ["None.", ""]
    else:
        for key, diffs in changed:
            lines.append(f"- `{key}`")
            for field, a, b in diffs:
                av = "(missing)" if a is _MISSING else md(a)
                bv = "(missing)" if b is _MISSING else md(b)
                lines.append(f"  - `{field}`: {av} → {bv}")
        lines.append("")

    old_layout, new_layout = layout_entries(old["layout"]), layout_entries(new["layout"])
    layout_added = sorted(new_layout.keys() - old_layout.keys())
    layout_removed = sorted(old_layout.keys() - new_layout.keys())
    layout_changed = [(p, old_layout[p], new_layout[p]) for p in sorted(old_layout.keys() & new_layout.keys())
                      if old_layout[p] != new_layout[p]]
    lines += ["## Layout", "", f"- Added locations: **{len(layout_added)}**",
              f"- Removed locations: **{len(layout_removed)}**", f"- Changed locations: **{len(layout_changed)}**", ""]
    for heading, paths, mapping in (("Added locations", layout_added, new_layout), ("Removed locations", layout_removed, old_layout)):
        lines += [f"### {heading}", ""]
        lines += ([f"- `{p}`: {md(mapping[p])}" for p in paths] if paths else ["None."])
        lines.append("")
    lines += ["### Changed locations", ""]
    if layout_changed:
        lines.extend(f"- `{p}`: {md(a)} → {md(b)}" for p, a, b in layout_changed)
    else:
        lines.append("None.")
    lines.append("")
    return "\n".join(lines)


class _Missing:
    pass


_MISSING = _Missing()


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("old", help="older generated catalog directory")
    parser.add_argument("new", help="newer generated catalog directory")
    parser.add_argument("-o", "--output", type=Path, help="write Markdown here (default: stdout)")
    args = parser.parse_args(argv)
    try:
        report = compare(read_catalog(args.old), read_catalog(args.new), args.old, args.new)
        if args.output:
            args.output.write_text(report, encoding="utf-8")
        else:
            sys.stdout.write(report)
    except (OSError, json.JSONDecodeError, ValueError) as error:
        parser.error(str(error))


if __name__ == "__main__":
    main()
