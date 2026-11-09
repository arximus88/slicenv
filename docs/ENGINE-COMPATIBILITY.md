# Slicer Engine Compatibility & Schema Analysis

This document specifies the compatibility matrix, schema differences, CLI capabilities, and version provenance between **Bambu Studio** and **OrcaSlicer** for this repository.

> **Ownership Notice**: Generated catalog metadata lives in `crates/settings/schema` (Bambu) and `crates/settings/schema-orca` (Orca). Slicer CLI execution is implemented in `crates/slicer`. Profile management and engine switching are isolated across engines.

---

## 1. Source Provenance and Executable Audit

Checked against local upstream reference trees and locally installed binaries on `2026-10-30`.

### Bambu Studio
- **Reference Source**: `<BambuStudio-src>`
- **Source Commit**: `79df7ae309be398ed30c9f397c13c9911c2edc57` (tag/branch: 2.8.4 beta build fixes)
- **Declared Version (`version.inc`)**: `02.08.04.57`
- **Installed Executable**: `/usr/bin/bambustudio` (153,467,240 bytes)
- **Installed CLI Header**: `BambuStudio-02.08.04.57`
- **Provenance Match**: **Verified exact match** between checked reference source (`02.08.04.57`) and installed executable (`02.08.04.57`).

### OrcaSlicer
- **Reference Source**: `<OrcaSlicer-src>`
- **Source Commit**: `d28c71c485cf51949c5ed4f6ee59998b7f17566c`
- **Declared Version (`version.inc`)**: `SoftFever_VERSION = "2.5.0-dev"`, `SLIC3R_VERSION = "02.08.01.55"`
- **Installed Executable**: `/usr/bin/orca-slicer` -> `/opt/orca-slicer/AppRun` (binary: `/opt/orca-slicer/bin/orca-slicer`, 129,133,776 bytes)
- **Installed CLI Header**: `OrcaSlicer-2.4.2`
- **CRITICAL VERSION MISMATCH**:
  - The generated Orca schema (`crates/settings/schema-orca/`) was generated from upstream commit `d28c71c485cf` representing **`2.5.0-dev`** (881 options).
  - The locally installed executable is **`OrcaSlicer-2.4.2`**.
  - **Limitation Warning**: Do **not** pretend unknown newest options (introduced in 2.5.0-dev) are supported by the installed older 2.4.2 executable. Preset payloads containing 2.5-specific features (e.g. plugin configs, experimental belt parameters) may be ignored or cause warnings/errors when executed against `/opt/orca-slicer`.

---

## 2. Settings Catalog & Schema Comparison

Both schemas are extracted using the standard-library tool `crates/settings/tools/generate.py`:
- Bambu Studio: `python3 crates/settings/tools/generate.py <BambuStudio-src> --source bambu --output crates/settings/schema`
- OrcaSlicer: `python3 crates/settings/tools/generate.py <OrcaSlicer-src> --source orca --output crates/settings/schema-orca`

### Summary Counts

| Metric | Bambu Studio (`schema`) | OrcaSlicer (`schema-orca`) | Delta / Notes |
|---|---|---|---|
| **Total Options** | **702** | **881** | +179 net options in Orca |
| Primary (`PrintConfigDef`) | 682 | 881 | Orca has no legacy compatibility file |
| Legacy Compatibility | 20 | 0 | Kept for Bambu A1 installed presets |
| **Object Scope** | 111 | 132 | Overrides object settings |
| **Region Scope** | 147 | 175 | Overrides part/modifier settings |
| **Global Scope** | 444 | 574 | Print/process/machine level |
| **FFF Pages** | 16 (Process: 5, Filament: 6, Machine: 5) | 19 (Process: 6, Filament: 7, Machine: 6) | Orca adds Multimaterial pages |
| **Referenced Layout Keys** | 529 | 650 | +121 UI layout options |
| **Missing Layout Keys** | 2 (`max_volumetric_extrusion_rate_slope_negative`, `..._positive`) | 2 (`extruders_count`, `imex_pre_slice_warnings`) | Documented unmapped layout items |
| **Unparsed Expressions** | 0 | 0 | Strict AST / grammar validation |

### Option Comparison (`compare_catalogs.py`)

Running `python3 crates/settings/tools/compare_catalogs.py crates/settings/schema crates/settings/schema-orca`:
- **Added in Orca (Orca-only)**: **303** options
- **Removed from Orca (Bambu-only)**: **124** options
- **Changed options**: **523** options (shared options with differences in default value, bounds, or enum values)

#### Key Orca-Specific Options (Absent in Bambu)
- **Print Geometry & Overhangs**:
  - `make_overhang_printable`, `make_overhang_printable_angle`, `make_overhang_printable_hole_size`
  - `extra_perimeters_on_overhangs`
  - `fan_speedup_overhangs`
  - `infill_overhang_angle`, `lightning_overhang_angle`
  - `overhang_flow_ratio`, `overhang_reverse`, `overhang_reverse_internal_only`, `overhang_reverse_threshold`
- **Seams & Movement**:
  - `wipe_on_loops`
  - `scarf_joint_flow_ratio`, `scarf_joint_speed`, `scarf_overhang_threshold`
- **Pressure Advance & Bed Mesh**:
  - `adaptive_pressure_advance`, `adaptive_pressure_advance_bridges`, `adaptive_pressure_advance_model`, `adaptive_pressure_advance_overhangs`
  - `adaptive_bed_mesh_margin`, `bed_mesh_max`, `bed_mesh_min`, `bed_mesh_probe_distance`
- **Belt & Hardware Features**:
  - `belt_printer`, `belt_printer_infinite_y`, `belt_frame_tilt_angle`, `belt_frame_tilt_decouple`
  - `activate_chamber_temp_control`, `activate_air_filtration_during_print`, `activate_air_filtration_on_completion`

#### Key Bambu-Specific Options (Absent in Orca)
- `adaptive_layer_height`
- `ams_filament_load_time_ams`, `ams_filament_load_time_ams_lite`, `ams_filament_load_time_n3f_s`, `ams_filament_unload_time_ams`
- `apply_scarf_seam_on_circles`
- `bed_heat_soak_area`, `chamber_temperatures`
- `circle_compensation_manual_offset`, `circle_compensation_speed`
- `conformal_infill`, `conformal_hub_radius`, `conformal_link_flip_layers`, `conformal_link_keep_layers`, `conformal_pole`, `conformal_ray_count`, `conformal_stagger`
- `cooling_slowdown_logic`, `cooling_perimeter_transition_distance`

---

## 3. Slicer CLI Comparison & Capabilities

Examining `bambu-studio --help` vs `orca-slicer --help`:

| Capability | Bambu Studio CLI (`02.08.04.57`) | OrcaSlicer CLI (`2.4.2`) | Compatibility Impact |
|---|---|---|---|
| **Core Slice** (`--slice <idx>`) | Yes | Yes | Identical syntax |
| **Settings Loading** (`--load-settings`, `--load-filaments`, `--load-defaultfila`) | Yes | Yes | Identical syntax |
| **Output Control** (`--outputdir`, `--export-3mf`) | Yes | Yes | Identical syntax |
| **Arrange / Orient** (`--arrange`, `--orient`, `--ensure-on-bed`, `--scale`, `--rotate`) | Yes | Yes | Identical syntax |
| **Export PNG** (`--export-png`) | **Yes** | **No** | **Capability Gap**: Orca CLI cannot generate plate preview PNGs headless. |
| **Camera View** (`--camera-view`) | **Yes** | **No** | Bambu-specific option for headless PNG renders. |
| **Preset Verification** (`--check-preset`) | **Yes** | **No** | Bambu-specific CLI check. |
| **Estimation Mode** (`--estimate-mode`) | **Yes** | **No** | Bambu-specific machine estimation CLI flag. |
| **Log File Redirection** (`--logfile <file>`) | **No** | **Yes** | Orca provides CLI logfile flag; Bambu relies on stdout/stderr redirection. |
| **Skip Useless Pick** (`--skip-useless-pick`) | **Yes** | **No** | Bambu-specific image optimization. |

---

## 4. Settings Rust API

The `settings` crate (`crates/settings`) exposes schemas for both engines:

```rust
use settings::Schema;

// Load standard Bambu Studio schema (702 options, 16 FFF pages)
let bambu_schema = Schema::load()?;
assert_eq!(bambu_schema.options.len(), 702);

// Load OrcaSlicer schema (881 options, 19 FFF pages)
let orca_schema = Schema::load_orca()?;
assert_eq!(orca_schema.options.len(), 881);

// Known Orca-specific options are easily inspected
assert!(orca_schema.options.contains_key("make_overhang_printable"));
assert!(!bambu_schema.options.contains_key("make_overhang_printable"));

// Known Bambu-specific options
assert!(bambu_schema.options.contains_key("adaptive_layer_height"));
assert!(!orca_schema.options.contains_key("adaptive_layer_height"));
```

### Verification & Testing
All settings tests run with:
```sh
cargo test -p settings
```
Verification covers:
- Loading both `Schema::load()` (Bambu, 702 options) and `Schema::load_orca()` (Orca, 881 options).
- Asserting the presence and absence of known engine-specific options.
- Verifying exact types, bounds, scopes (Object vs Region vs Global), defaults, and layout page structures.
- Confirming that existing Bambu behavior and presets round-trip without regressions.

---

## 5. Architectural Guidance for Multi-Engine Integration

1. **Profile Isolation**:
   - Bambu Studio configurations reside in `~/.config/BambuStudio/system/BBL` and `~/.config/BambuStudio/user`.
   - OrcaSlicer configurations reside in `~/.config/OrcaSlicer/system` and `~/.config/OrcaSlicer/user`.
   - **Never cross-inherit base profiles**: An active Bambu slice job must not inherit an Orca printer profile, and vice versa. Starting G-code and macros differ significantly between engines.
2. **Execution Working Directories**:
   - Working directories should be partitioned by engine and identity hash:
     `~/.cache/slicenv/sliced/<engine>/<identity>/...`
   - Slicer output logs (`slicer.log`, `slicer.err.log`) and metadata provenance (`provenance.json`) must be preserved alongside `*.gcode.3mf`.
3. **Headless Limitations**:
   - Because OrcaSlicer CLI lacks `--export-png`, thumbnail generation for Orca-sliced 3MFs must rely on 3MF internal archive extraction or the app's internal 3D renderer rather than slicer CLI flags.
4. **Executable vs Schema Versioning**:
   - The UI and settings validator must not expose unreleased 2.5-dev settings when targeting the installed 2.4.2 OrcaSlicer binary without an engine capability check.


## 6. Implemented runtime integration

The source schema counts above are not the runtime UI key counts and do not certify enum/value compatibility.
