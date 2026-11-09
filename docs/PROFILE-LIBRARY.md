# Profile library

JSON catalogs live at `$XDG_DATA_HOME/slicenv/profiles/<family>/<version>/`
(default `~/.local/share`). Families (`bambu`, `orca`) never share templates.
A catalog has `BBL.json`, `BBL/{machine,process,filament}/` and `catalog.json`:
`family`, `version`, `source` (`engine-package`, `git-tag`, reserved `bambu-config`),
`source_ref`, UTC `fetched_at`, and `vendors`.

Resolution: `SLICENV_PROFILE_ROOT` / `ORCA_PROFILE_ROOT`, package JSON tree,
family library (exact version, newest <= engine version, otherwise newest),
then existing system fallbacks. A cover-only `BBL/` directory is insufficient.
OrcaSlicer 2.5 uses private, unstable `.opc` vendor caches; we deliberately do
not decode them. Official git-tag JSON remains the source of truth.

```sh
python3 crates/slicer/tools/manage_engines.py profiles fetch --family orca --ref v2.5.0-beta --vendor BBL
python3 crates/slicer/tools/manage_engines.py profiles import --family bambu --version 02.08.02.61 --package /path/to/resources/profiles
```

Repeat `--vendor` to include another selected printer's vendor; BBL is the default.
`--data-home` overrides XDG data home for isolated automation. Fetch uses official
repositories, the git trees API and vendor JSON blobs only (no cover PNGs), with
HTTPS/redirect restrictions and Git blob hash verification. Downloads validate
JSON and stage beside the destination before rename, with rollback on failure.
Existing vendors in the same catalog are preserved. Profile catalog removal is
independent of engine removal.

The desktop package installer imports JSON or fetches the matching release tag.
A catalog failure leaves the installed engine intact and reports a warning.
Activation errors offer a Ukrainian download action in the settings panel;
a successful fetch retries activation and reloads profiles. Nightlies require a
published tag containing profiles; no cross-family substitution is attempted.

## Acceptance check

```sh
CARGO_TARGET_DIR=~/.cache/bambu-target-profiles cargo run -p slicer --example profiles -- "$HOME/.local/share/slicenv/engines/orca/2.5.0-beta/AppRun" --verify-a1
```

The real `v2.5.0-beta` BBL fetch produced 2497 JSON files. Loading through
`PresetStore::open_for_engine` selected the independent Orca catalog and found
`Bambu Lab A1 0.4 nozzle`, 10 compatible processes and 86 filaments. Flattening
resolved `include` templates to 12310 bytes of `machine_start_gcode`.
