# Upstream status and adoption plan

Checked 2026-10-30 against the official GitHub release pages. Recheck these pages before planning an upgrade because alpha/beta tags and stable releases move independently.

- [OrcaSlicer releases](https://github.com/OrcaSlicer/OrcaSlicer/releases) lists **v2.5.0-alpha** (tag `v2.5.0-alpha`) as the newest 2.5.0 release and **v2.4.2 Official Release** as the latest stable release. Treat 2.5.0 features as experimental; it is not a 2.5.0 stable adoption target yet.
- [Bambu Studio releases](https://github.com/bambulab/BambuStudio/releases) lists **2.8.4 Public Beta** as latest and **2.8.2.61 Public Release** as the latest stable release in the release list. This project's generated Bambu settings catalog currently identifies source commit `79df7ae309be398ed30c9f397c13c9911c2edc57` and installed CLI/presets 2.8.4; see [`crates/settings/README.md`](../crates/settings/README.md).

## What Orca 2.5.0 alpha actually adds

The official [2.5.0 alpha notes](https://github.com/OrcaSlicer/OrcaSlicer/releases/tag/v2.5.0-alpha) describe a large release (950+ PRs) with Python plugins, belt-printer support, an experimental parametric Design tab, color mixing, Texture Displacement, IDEX/IQEX parallel printing, Speed Dial, H2C/A2L and mixed-nozzle support, performance work, Precise Seam modifiers, new infill options, and fixes. This is the release note's feature list, not a claim that each item is present in a stable build or relevant to the A1 target.

| Feature area | What transfers to this project | Cost / boundary |
|---|---|---|
| Slicing engine and G-code generation | Usually nothing to port for parity: this application calls the installed Bambu Studio CLI for slicing. Changes in Orca's forked engine do not appear in Bambu's CLI automatically. | Adopting an Orca engine means selecting/building/integrating a different executable and validating output. Avoid assuming schema compatibility implies engine parity. |
| Settings, presets, and profiles | Potential low-cost catalog work: run the existing generator against an Orca checkout into a separate output directory, compare option metadata/layout, and selectively accept useful entries. | New metadata alone does not add UI semantics, defaults, compatibility, validation rules, or CLI support. Orca's 2.5 schema and feature flags need versioned provenance. |
| Python plugins | Could inspire a future extension point. | No drop-in: requires a plugin host, permissions/isolation, API lifecycle, UI contributions, and support burden. Not a fast A1 workflow win. |
| Design tab / parametric parts | Candidate for a separately scoped feature if users need native parametric modeling. | Experimental and large UI/modeling scope; no immediate reuse in this Rust/GPUI app. |
| Belt, IDEX/IQEX, H2C/A2L, mixed nozzles | Useful only if supported hardware becomes a project target. | The original target was Bambu A1; installed BBL printer/nozzle presets are now selectable, but machine schema, plate model, toolpath semantics, print controls, and hardware-specific UI all need coordinated work. |
| Color mixing, texture displacement, seam modifiers, infill options | Some settings and algorithms may be worth evaluating individually. | Distinguish engine-affecting parameters from visible controls. Controls unsupported by the invoked Bambu CLI can mislead or silently do nothing. |
| Startup/slicing/export performance | Benchmark candidate changes if the matching engine is adopted. | UI/runtime improvements in Orca do not transfer just by copying release notes. |

### Practical adoption recommendation

The easiest safe adoption is **selective settings/profile metadata and small frontend ideas**, not a wholesale Orca 2.5 port. The project already has a generator that can parse Orca source separately (`python3 -I crates/settings/tools/generate.py /path/to/OrcaSlicer --source orca --output /tmp/settings-orca`), while the live slice path uses Bambu Studio. Keep Bambu as the authoritative A1 engine unless a deliberate engine migration is approved and verified against representative projects.

For Orca 2.5 specifically, wait for a stable release before treating its alpha behavior as a compatibility target. In the meantime, identify only A1-relevant settings/features and implement them end-to-end: metadata + UI/rules + profile persistence + proof that the selected slicer honors the value. Do not copy every Orca option into the Bambu catalog or expose unsupported controls.

## Repeatable upstream upgrade workflow

1. Record the exact upstream repository, release/tag, commit SHA, release channel, and date. For Orca, inspect the release page and tag; for Bambu, record both the source snapshot and installed CLI/preset version. Do not use a floating `master` as provenance.
2. Fetch/check out the candidate in a separate source directory. Keep generated output separate from committed Bambu output. Preserve the existing system preset directory as read-only.
3. Generate source metadata with the current standard-library generator and review diagnostics/counts. Reject generation if expressions/layout calls are unparsed; do not treat a higher option count as completion.
4. Compare option keys, wire types, defaults, bounds, enum values, scopes, page/group placement, and localized strings against the current catalog. Classify each difference as add/change/remove/rename/obsolete; preserve legacy keys with provenance instead of silently dropping preset values.

For a readable deterministic metadata diff, run `python3 -I crates/settings/tools/compare_catalogs.py crates/settings/schema /tmp/settings-orca` after generating the candidate catalog; omit `--output FILE` to print Markdown to stdout, or pass it to save a report. The tool compares options, ordered layout locations, and extraction provenance. It is a metadata comparison only and does not establish support in the active slicer engine.

5. For each accepted setting, verify its engine owner. Record whether the active Bambu CLI recognizes and applies it. If it is Orca-only, either keep it out of the Bambu UI or require an explicit Orca engine selection.
6. Review behavior separately: port or intentionally omit option enable/visibility rules, printer capability checks, cross-option corrections, and widget filtering. Generated schema metadata does not implement these frontend behaviors.
7. Run the generator's own fixture tests and settings tests when integrating a change. Then check preset coverage and load/save round trips using copied fixtures. Never refresh committed fixtures from the user's installed config implicitly.
8. For engine or profile changes, slice the same representative A1 projects with a pinned baseline and candidate binary; compare success/failure, G-code/config payloads, time/weight, and relevant geometry. Document nondeterminism and known headless-thumbnail limitations; do not claim GUI or physical-print validation from CLI-only results.
9. Update `crates/settings/README.md`, extraction provenance, and this document with the adopted source and limitations. Keep a short upgrade diff/report so the next release review starts from explicit evidence.

## Current project evidence and open work

- `crates/settings/README.md` documents 702 Bambu option definitions (including compatibility entries), 16 FFF pages, and an Orca generation path reporting 881 options / 19 pages / 650 layout keys for its checked source. These are metadata-generation counts, not runtime or engine support.
- `crates/settings/src/rules.rs` ports selected Bambu process toggle behavior; the settings README explicitly excludes filament/printer toggle routines, some object-list restrictions, event callbacks, and automatic corrections. Plan completion should not imply all Orca settings UI behavior exists.
- `crates/slicer/src/cli.rs` invokes Bambu Studio. Therefore engine improvements in Orca 2.5 alpha are not adopted by this app's current slice path.
- App source includes Main, three project tabs (Prepare, Preview, Project), and a separate device entry, cloud controls, preset editing, toolpath display, and project manipulation. The current checkout has uncommitted app/core changes; this audit did not build or run them, so runtime completion remains unverified.

## Installed printer catalogs

The app discovers the installed `system/BBL` catalog in `BambuStudio`, then `BambuStudioBeta`, then `OrcaSlicer` under `XDG_CONFIG_HOME` (normally `~/.config`). `SLICENV_PROFILE_ROOT=/path/to/slicer-config` explicitly selects a complete catalog for testing. A catalog is loaded as one coherent tree: same-named base templates from Bambu and Orca are never merged, because a child profile could otherwise inherit the wrong start G-code. System and upstream user presets remain read-only; application presets still live under `XDG_DATA_HOME/slicenv/presets`.

Model metadata (`type: machine_model`) and non-instantiable templates are excluded from the printer selector. All installed nozzle variants are shown, with compatible processes and filaments. Switching printers uses the installed machine's default profiles. The bed grid, centering, and arrange use its inherited `printable_area` bounds. Packing uses a rectangular bounding area; the slicer remains responsible for exclusion zones and nonrectangular areas. Plate labels use the common Bambu list and retain an imported custom plate value.

Inspect coverage without slicing or changing configuration:

```sh
cargo run -p slicer --example profiles
```

The current installation exposes 52 printer/nozzle presets, including A1 mini, P1, X1 and newer BBL models. This is catalog coverage, not verification of physical printing or every multi-extruder workflow. The UI still has a single selected filament profile, so advanced mixed-nozzle/multi-extruder behavior requires further end-to-end work. Print sending requires that the upstream profile's model ID matches the selected cloud printer.

An optional machine argument on the existing slice example makes a representative engine check repeatable:

```sh
cargo run -p slicer --example slice -- /absolute/path/project.3mf 1 'Bambu Lab A1 mini 0.4 nozzle'
```

Orca catalog fallback does not change the Bambu executable or prove that it honors Orca-only settings. Prefer Bambu profiles for the current backend. Test alternate catalogs with representative projects before adopting them.
