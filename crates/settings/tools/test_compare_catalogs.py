#!/usr/bin/env python3
"""Tests for deterministic, nonmutating generated catalog comparisons."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("compare_catalogs", Path(__file__).with_name("compare_catalogs.py"))
compare_catalogs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compare_catalogs)


def write_catalog(root, options, layout, extraction):
    root.mkdir(parents=True, exist_ok=True)
    for name, value in (("options", options), ("layout", layout), ("extraction", extraction)):
        (root / f"{name}.json").write_text(json.dumps(value), encoding="utf-8")


class CompareCatalogsTests(unittest.TestCase):
    def test_reports_option_fields_layout_and_provenance(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            old, new = root / "old", root / "new"
            common = {"type": "float", "default": 0.2, "min": 0.1, "max": 1.0, "enum_values": []}
            changed = dict(common, type="int", default=2, min=0, max=5, enum_values=[0, 1])
            write_catalog(old, {"keep": common, "gone": {"type": "bool"}},
                          {"process": [{"title": "Quality", "groups": [{"title": "Layer", "lines": [{"options": ["keep"]}]}]}]},
                          {"source": "bambu", "source_commit": "aaa", "sources": {"x.cpp": "111"}})
            write_catalog(new, {"keep": changed, "added": {"type": "string", "default": "x"}},
                          {"process": [{"title": "Quality", "groups": [{"title": "Layer", "lines": [{"options": ["keep", "added"]}]}]}]},
                          {"source": "orca", "source_commit": "bbb", "sources": {"x.cpp": "222"}})
            report = compare_catalogs.compare(compare_catalogs.read_catalog(old), compare_catalogs.read_catalog(new), "old", "new")
            self.assertIn("`added`", report)
            self.assertIn("`gone`", report)
            self.assertIn("`type`: `\"float\"` → `\"int\"`", report)
            self.assertIn("`default`", report)
            self.assertIn("`enum_values`", report)
            self.assertIn("line[0]", report)
            self.assertIn("`aaa`", report)
            self.assertIn("`222`", report)
            self.assertIn("does not establish slicer engine support", report)

    def test_self_comparison_of_real_schema_is_empty_and_deterministic(self):
        schema = Path(__file__).parents[1] / "schema"
        catalog = compare_catalogs.read_catalog(schema)
        one = compare_catalogs.compare(catalog, catalog, "schema", "schema")
        two = compare_catalogs.compare(catalog, catalog, "schema", "schema")
        self.assertEqual(one, two)
        self.assertIn("Added: **0**", one)
        self.assertIn("Changed: **0**", one)
        self.assertIn("Added locations: **0**", one)
        self.assertIn("Changed locations: **0**", one)


if __name__ == "__main__":
    unittest.main()
