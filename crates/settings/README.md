# settings

Bambu Studio option metadata, ordered FFF settings pages, Ukrainian localization,
object/part scopes, process control dependencies, lossless values, validation, and
editable user-preset deltas. Inheritance/includes are resolved by
`slicer::PresetStore::flatten`.

## Generated catalog

```sh
python3 -I crates/settings/tools/generate.py <BambuStudio-src>
# Alternate source; keep its output separate from the committed Bambu catalog:
python3 -I crates/settings/tools/generate.py <OrcaSlicer-src> \
  --source orca --output /tmp/settings-orca
python3 -I crates/settings/tools/test_generate.py
```

Committed source: BambuStudio `79df7ae309be398ed30c9f397c13c9911c2edc57`
(2.8.3 beta build fixes); installed CLI/presets: 2.8.4.
[`schema/extraction.json`](schema/extraction.json) records source hashes,
translation/scope counts and diagnostics.

- **691 active PrintConfigDef definitions**, including SLA definitions.
- **11 explicit compatibility definitions**, **702 options total**.
- **16 FFF pages**: process Quality / Strength / Speed / Support / Others;
  6 filament pages; 5 machine pages. **458 distinct layout keys**.
- **0 unparsed option expressions; 0 unparsed layout calls**.
- Two layout keys have commented-out definitions in this source:
  `max_volumetric_extrusion_rate_slope_positive` and
  `max_volumetric_extrusion_rate_slope_negative`. Their metadata remains absent.

The reference snapshot has removed or renamed eleven keys still present in
installed A1 presets. [`tools/bambu_compatibility.json`](tools/bambu_compatibility.json)
keeps their wire types and provenance separately from active source definitions:
`adaptive_layer_height`, `extruder_height_gap`, `extruder_clearance_radius`,
`nozzle_type`, `layer_time_smoothing`, `layer_time_smoothing_threshold`,
`only_one_wall_top`, `reduce_infill_retraction`, `wall_infill_order`,
`filament_long_retractions_when_ec`, `filament_retraction_distances_when_ec`.
These entries expose `origin == "bambu_compatibility"` and `source_note`; they have
no invented default or bounds and are not inserted into the current GUI layout.
They preserve old keys without silently migrating their values. This is catalog
coverage, not a claim that the current CLI honors obsolete settings.

The standard-library-only generator handles C++ translation wrappers/context,
adjacent literals/escapes, copied enums, nullable overrides, axis loops and
source-specific helpers. It rejects unparsed expressions. Layout is a static
union of conditional FFF pages; literal `#if 0`/`#if 1` branches are respected.
Orca still regenerates its original **881 options / 19 pages / 650 layout keys**.

`vector` describes C++ vector storage, including polygons/string lists.
`per_extruder` comes from `m_extruder_option_keys`; `per_filament` combines
`m_filament_option_keys` and vector `filament_*` definitions. Neither implies
that vector length always equals the extruder count.

## API and localization

Existing English fields and method signatures remain available. Locale accessors
are additive; callers do not have to pass a locale to existing APIs.

```rust,no_run
use settings::{EditablePreset, Locale, PresetType, Schema, TypedValue};
use slicer::PresetStore;

let schema = Schema::load()?;
let store = PresetStore::open_default()?;
let mut preset = EditablePreset::from_system(
    &schema, &store, PresetType::Process, "0.20mm Standard @BBL A1",
)?;
preset.set_typed("layer_height", &TypedValue::Float(0.24))?;
assert!(preset.is_dirty("layer_height"));
let option = &schema.options["layer_height"];
assert_eq!(option.label(Locale::Uk), "Висота шару");
assert_eq!(option.label(Locale::En), option.label);
let support_type_enabled = preset.is_enabled("support_type");
for page in preset.pages() {
    let title = page.page.title(Locale::Uk);
    for group in page.groups() {
        let title = group.group.title(Locale::Uk);
        for line in group.lines() {
            for option in line.options() {
                // option.key, metadata, value, dirty; existing view API
            }
        }
    }
}
# Ok::<(), anyhow::Error>(())
```

`OptionDef::{label, full_label, tooltip, sidetext, category, enum_label,
enum_labels}` and `Page::title` / `Group::title` accept `Locale::{En, Uk}`.
An absent/empty translation falls back to the corresponding English string;
`enum_label` returns `None` for an invalid index. Original public string fields
remain English. `uk`, `title_uk`, and `LocalizedOption` expose translated data.

Translations come only from `bbl/i18n/uk/BambuStudio_uk.po`. The parser preserves
`msgctxt`, multiline strings, Unicode and quoted escapes; fuzzy, obsolete, empty
and plural entries are excluded. Contextual translations never substitute for a
different context. Coverage counts nonblank source fields per option/title,
including repeated occurrences and compatibility entries:

| Field | Translated / total | Coverage |
|---|---:|---:|
| Label | 343 / 562 | 61.03% |
| Full label | 43 / 60 | 71.67% |
| Tooltip | 309 / 514 | 60.12% |
| Side text | 298 / 302 | 98.68% |
| Enum label | 195 / 289 | 67.47% |
| Page title | 15 / 16 | 93.75% |
| Group title | 62 / 66 | 93.94% |

## Object and part overrides

`OptionDef::scope` is generated from active `PrintObjectConfig` and
`PrintRegionConfig` members in PrintConfig.hpp:

- `Scope::Object`: **93** settings, e.g. `layer_height`, `enable_support`.
- `Scope::Region`: **120** settings, e.g. `wall_loops`, `ironing_type`.
- `Scope::Global`: **489** other settings, including compatibility-only entries.

`can_override_object()` accepts object or region scope; `can_override_part()`
accepts region scope. These describe configuration ownership. Object-list menus
can further restrict settings for support/blocker/negative/modifier volume kinds;
this crate does not reproduce every object-list menu restriction. For object
editing, pass an inherited/merged base to `EditablePreset::new` so dependencies
see the effective global values as well as local overrides.

## Enable and visibility rules

[`src/rules.rs`](src/rules.rs) hand-ports the config-driven rules in
`ConfigManipulation::toggle_print_fff_options`, invoked by `TabPrint::toggle_options`.
**189 distinct keys** have rules spanning all five process pages:

- Walls/seam placement; shell layers and spiral mode; Arachne/thin walls;
  circle compensation and filament scarf-seam override.
- Infill density, anchors, multiline/zigzag/crosszag/lockedzag/lattice/conformal
  controls; periodic modifiers and solid-infill-dependent fields.
- Acceleration, jerk, overhang and height slowdown; vertical-shell controls.
- Supports/raft, automatic/tree styles, interfaces, soluble raft separation,
  support ironing, ordinary ironing, and skirt-dependent support speeds.
- Skirt/draft shield, brim (including auto/painted brim), prime tower/ribs/flush,
  travel avoidance, fuzzy skin/noise, beam interlocking, and Klipper controls.
- A small additional compatibility rule for `layer_time_smoothing_threshold`.

`is_enabled(key, &preset)` / `EditablePreset::is_enabled` mean **visible and enabled**.
`is_visible` reports line visibility. `option_state` returns independent
`OptionState { enabled, visible }`; later rules replace earlier rules of the same
kind, as in Bambu's GUI. Rules read edits immediately, never modify values, and
fall back to schema defaults for absent keys through `effective_value`.
Unruled keys remain enabled and visible, including controls missing metadata.

The default context is global Bambu A1, vector index 0.
`option_state_with_context` accepts `ToggleContext` for Objects/parts,
`variant_index`, Bambu-vendor/jerk visibility, G-code flavor, tower-interface and
wrapping-detection capability. Printer capabilities must be supplied by the UI;
a process preset cannot determine them. Filament/printer tabs' own toggle routines,
widget choices (e.g. filtering support-style enums), per-part special controls,
GUI event callbacks and cross-option auto-corrections are outside this API.

## Values and editing

`Schema::parse` checks representation/type and exposes `ParsedValue::typed()`:
numbers, percentages, booleans, enums, nil, points, point groups and vectors.
`to_json()` returns the exact original value, preserving spelling, suffixes,
scalar/array shape and order. Unknown keys remain `TypedValue::Unknown`.
`Schema::validate` adds bounds, enum membership and nullability checks;
`Schema::format` produces validated Bambu strings/arrays for new typed values.
Validation covers individual options, not printer safety or cross-option constraints.

`EditablePreset::{set, set_typed}` validate before editing. `reset`, `reset_all`,
`dirty_keys`, `flattened`, and existing layout views retain their round-1 behavior.
An absent current value remains `None`; the new `effective_value` separately
exposes source-default fallback. Unknown keys survive loading/saving but cannot
be edited without a definition. Preset types remain process/filament/machine,
with print/printer deserialization aliases and `slicer::Kind` conversions.

## User-preset save-as format

`save_user(directory, name, &UserPresetInfo)` requires an explicit destination
and a flattened system parent. `from_system` relies on the caller selecting a
system preset because `PresetStore` does not expose origins. The crate does not
choose a user-config write directory.

The format follows Bambu's Preset.cpp (`Preset::save`, `save_info`,
`get_extruder_names_and_keysets`, `PresetCollection::save_current_preset`),
Config.cpp (`ConfigBase::save_to_json`), and PresetBundle.cpp.
JSON stores name/version/from/inherits, the appropriate `*_settings_id`, edited
options only, required available extruder ID/variant fields, and `filament_id`
for filament presets. Vector edits are arrays of strings. A caller-supplied
version is required when the parent lacks one. No cloud IDs are invented.

The companion `.info` contains `sync_info`, `user_id`, `setting_id`, `base_id`,
and `updated_time` as `key = value` lines, defaulting to local/empty IDs and time
0; `base_id` falls back to the parent's setting_id. Saves reject unsafe names,
use `create_new`, refuse overwrites and roll back newly created files on ordinary
write failures. Saving does not clear dirty state. Two-file writes are not
crash-atomic and take no Bambu application lock. Save/reload tests use temporary
folders; GUI import acceptance has not been tested. Installed Bambu config is
read only.

## A1 preset coverage

```sh
cargo run -p settings --bin coverage
# Optional alternate read-only preset root:
cargo run -p settings --bin coverage -- /path/to/BambuStudio
# Explicitly refresh frozen fixtures from that store:
cargo run -p settings --bin coverage -- --fixtures crates/settings/tests/fixtures
```

`is_metadata_key` classifies preset identity, inheritance, compatibility, IDs,
version and description. `Coverage::total` now excludes these fields;
`raw_total` and `metadata` retain their count/list. `covered`, `percent` and
`missing` refer to slicing settings only. The report separately lists active
source coverage and compatibility keys. No metadata is mislabeled as missing.

| Preset | Round 1, settings only | Round 2 | Active source / compatibility |
|---|---:|---:|---:|
| Bambu Lab A1 0.4 nozzle | 97/102 (95.10%) | 102/102 (100%) | 99 / 3 |
| 0.20mm Standard @BBL A1 | 145/188 (77.13%) | 188/188 (100%) | 182 / 6 |
| 0.08mm Extra Fine @BBL A1 | 145/188 (77.13%) | 188/188 (100%) | 182 / 6 |
| Bambu PLA Basic @BBL A1 | 93/127 (73.23%) | 127/127 (100%) | 125 / 2 |
| Bambu PETG Translucent @BBL A1 | 93/127 (73.23%) | 127/127 (100%) | 125 / 2 |

Round 1's original metadata-inclusive numbers were respectively 98/108 (90.74%),
148/197 (75.13%), 148/197 (75.13%), 95/135 (70.37%), 95/136 (69.85%).
All **773 values** (732 settings + 41 metadata occurrences) still round-trip.
Frozen fixtures make tests portable; the live test also checks all five installed
presets if available (`SLICENV_PRESET_ROOT` overrides the root).

See [`VERIFICATION.md`](VERIFICATION.md) for check outputs. No new dependency was
needed. The root Cargo.lock is left unchanged under the requested file-scope rule;
Cargo recreates the missing settings package entry during local builds.
