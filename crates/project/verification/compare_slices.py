#!/usr/bin/env python3
"""Порівнює лише тимчасові копії; CLI має ізольований datadir."""
import concurrent.futures
import json
import pathlib
import re
import subprocess
import sys

root = pathlib.Path(sys.argv[1]).resolve()
cases = {
    "TypeC4PinHolder_v3.3mf": [1],
    "pin+stand.3mf": [1],
    "120mm+Fan+Mesh+Bambu+Slicer+v4.3mf": [1],
    "grosh-102.3mf": [1],
    "ariesbox-case.3mf": [1],
    "klipsa_v2a.3mf": [1, 2],
    "laptop+stand.3mf": [1, 2],
    "All+Shelves.3mf": [1, 2],
    "Zen-Assembly.3mf": [1, 2],
    "mini-pc-trigkey+g4+pc+case.3mf": [1, 2],
}


def run(case):
    name, plate, variant = case
    work = root / "slices" / name.removesuffix(".3mf") / f"plate-{plate}" / variant
    work.mkdir(parents=True, exist_ok=True)
    source = root / (("saved-" if variant == "saved" else "") + name)
    command = ["/usr/bin/bambustudio", "--datadir", str(work / "data"), "--slice", str(plate),
               "--outputdir", str(work), "--export-3mf", "out.gcode.3mf", str(source)]
    process = subprocess.run(command, cwd=work, capture_output=True, text=True, timeout=300)
    log = process.stdout + process.stderr
    (work / "cli.log").write_text(log)
    result_path = work / "result.json"
    result = json.loads(result_path.read_text()) if result_path.exists() else {}
    plates = result.get("sliced_plates", [])
    sliced = plates[0] if plates else {}
    summary = {"file": name, "plate": plate, "variant": variant, "exit": process.returncode,
               "return_code": result.get("return_code"), "error_string": result.get("error_string"),
               "seconds": sliced.get("main_predication"),
               "grams": [{"id": f["id"], "total_used_g": f["total_used_g"]}
                         for f in sliced.get("filaments", [])],
               "errors": re.findall(r"\[error\]\s*(.*)", log), "log": str(work / "cli.log")}
    print(json.dumps(summary), flush=True)
    return summary


jobs = [(name, plate, variant) for name, plates in cases.items() for plate in plates
        for variant in ["original", "saved"]]
with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
    results = list(pool.map(run, jobs))
(root / "slices.json").write_text(json.dumps(results, indent=2))
for name, plates in cases.items():
    for plate in plates:
        a, b = [r for r in results if r["file"] == name and r["plate"] == plate]
        if a["exit"] != 0 or b["exit"] != 0:
            status = "BASELINE FAILURE" if a["return_code"] == b["return_code"] else "FAILURE"
        else:
            status = "MATCH" if a["seconds"] == b["seconds"] and a["grams"] == b["grams"] else "DIFFERENT"
        print(name, plate, status, flush=True)
