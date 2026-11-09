#!/usr/bin/env python3
"""Тести tools/gcode-diff.py на синтетичних файлах: python3 -I tools/test_gcode_diff.py"""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import zipfile

HERE = Path(__file__).resolve().parent
TOOL = HERE / "gcode-diff.py"
FIXTURES = HERE / "fixtures" / "gcode-diff"
BAMBU = FIXTURES / "bambu.gcode"
ORCA = FIXTURES / "orca.gcode"

spec = importlib.util.spec_from_file_location("gcode_diff", TOOL)
gcode_diff = importlib.util.module_from_spec(spec)
sys.modules["gcode_diff"] = gcode_diff
spec.loader.exec_module(gcode_diff)


def run(*args: str) -> tuple[int, str]:
    p = subprocess.run(
        [sys.executable, "-I", str(TOOL), *args], capture_output=True, text=True, check=False
    )
    return p.returncode, p.stdout + p.stderr


def diff_json(a: Path, b: Path, *extra: str) -> tuple[int, dict]:
    code, out = run("--json", str(a), str(b), *extra)
    return code, json.loads(out)


class GcodeDiffTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def variant(self, name: str, old: str, new: str, base: Path = BAMBU) -> Path:
        text = base.read_text()
        self.assertIn(old, text)
        path = self.dir / name
        path.write_text(text.replace(old, new, 1))
        return path

    def test_bambu_and_orca_layouts_are_identical_after_normalization(self) -> None:
        # Різні: заголовок і дата, мініатюри, шляхи (Unix / Windows), положення M73,
        # службові коментарі поза блоками й підсумки Orca наприкінці.
        code, r = diff_json(BAMBU, ORCA)
        self.assertEqual(r["verdict"], "identical", r)
        self.assertEqual(code, 0)
        self.assertEqual(r["a"]["engine"], "BambuStudio 02.07.01.62")
        self.assertEqual(r["b"]["engine"], "OrcaSlicer 2.4.2")
        s = r["summary"]
        self.assertEqual((s["layers"]["a"], s["time_s"]["a"]), (2, 90))
        self.assertEqual((s["filament_mm"]["b"], s["filament_g"]["b"]), (12.34, 0.04))

    def test_config_difference_is_listed_first(self) -> None:
        b = self.variant("b.gcode", "; sparse_infill_density = 15%", "; sparse_infill_density = 20%")
        code, r = diff_json(BAMBU, b)
        self.assertEqual(code, 1)
        self.assertEqual(r["verdict"], "config_differs")
        self.assertEqual(r["config"], [{"key": "sparse_infill_density", "a": "15%", "b": "20%"}])
        _, text = run(str(BAMBU), str(b))
        self.assertIn("ВЕРДИКТ: параметри відрізняються (1): sparse_infill_density", text)
        # Ігнорований ключ не рахується.
        code, r = diff_json(BAMBU, b, "--ignore-key", "sparse_*")
        self.assertEqual((code, r["verdict"]), (0, "identical"))

    def test_trajectory_difference_names_first_layer_and_line(self) -> None:
        b = self.variant("b.gcode", "G1 X10 Y20 E.5 F1500", "G1 X11 Y20 E.5 F1500")
        code, r = diff_json(BAMBU, b)
        self.assertEqual(code, 1)
        self.assertEqual(r["verdict"], "trajectory_differs")
        t = r["trajectory"]
        self.assertEqual((t["different_lines"], t["different_layers"]), (1, 1))
        self.assertEqual(t["first"]["layer"], 2)
        self.assertEqual(t["first"]["z"], 0.4)
        lines = BAMBU.read_text().splitlines()
        self.assertEqual(lines[t["first"]["line_a"] - 1], "G1 X10 Y20 E.5 F1500")
        self.assertEqual(t["first"]["text_b"], "G1 X11 Y20 E.5 F1500")

    def test_summary_and_layer_z_mismatch(self) -> None:
        b = self.variant("b.gcode", "; Z_HEIGHT: 0.4", "; Z_HEIGHT: 0.36")
        b.write_text(b.read_text().replace("total estimated time: 1m 30s", "total estimated time: 1h 2m 3s"))
        _, r = diff_json(BAMBU, b)
        self.assertEqual(r["verdict"], "trajectory_differs")
        self.assertEqual(r["summary"]["z"]["first"], {"layer": 2, "a": 0.4, "b": 0.36})
        self.assertEqual(r["summary"]["time_s"]["b"], 3723)

    def test_m73_kept_on_request(self) -> None:
        code, r = diff_json(BAMBU, ORCA, "--keep-m73")
        self.assertEqual((code, r["verdict"]), (1, "trajectory_differs"))

    def test_reads_plate_gcode_from_3mf(self) -> None:
        archive = self.dir / "plate.gcode.3mf"
        with zipfile.ZipFile(archive, "w") as z:
            z.writestr("3D/3dmodel.model", "<model/>")
            z.writestr("Metadata/plate_1.gcode", BAMBU.read_text())
            z.writestr("Metadata/plate_2.gcode", ORCA.read_text().replace("G28", "G28 X"))
        code, out = run(str(archive), str(ORCA))
        self.assertEqual(code, 2)
        self.assertIn("--plate", out)
        code, r = diff_json(archive, ORCA, "--plate", "1")
        self.assertEqual((code, r["verdict"]), (0, "identical"))
        self.assertTrue(r["a"]["source"].endswith("!Metadata/plate_1.gcode"))
        code, r = diff_json(archive, BAMBU, "--plate", "2")
        self.assertEqual(r["trajectory"]["first"]["layer"], 0)
        code, out = run(str(archive), str(ORCA), "--plate", "7")
        self.assertEqual(code, 2)

    def test_project_without_gcode_is_an_input_error(self) -> None:
        archive = self.dir / "project.3mf"
        with zipfile.ZipFile(archive, "w") as z:
            z.writestr("3D/3dmodel.model", "<model/>")
        code, out = run(str(archive), str(BAMBU))
        self.assertEqual(code, 2)
        self.assertIn("не нарізаний", out)

    def test_paths_and_dates_normalized_but_fractions_kept(self) -> None:
        n = gcode_diff.normalize_comment
        self.assertEqual(n("; out = /tmp/a/b.gcode"), "; out = <PATH>")
        self.assertEqual(n(r'; x = "D:\work\m.3mf"'), '; x = "<PATH>"')
        self.assertEqual(n("; layer num/total_layer_count: 1/200"), "; layer num/total_layer_count: 1/200")
        self.assertEqual(n("; at 2026-11-09 15:08:48"), "; at <DATE> <TIME>")
        self.assertEqual(gcode_diff.duration("1d 2h 3m 4s"), 93784)
        self.assertEqual(gcode_diff.total("1.5, 2.25"), 3.75)


if __name__ == "__main__":
    unittest.main()
