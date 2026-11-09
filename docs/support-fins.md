# Support Fins external tool

Source: https://github.com/gittrahan/support-fins (MIT, license retained at installation).
Pinned commit: a6c9fa2c667db600a721f15270f323666feccc08
Archive: https://codeload.github.com/gittrahan/support-fins/tar.gz/a6c9fa2c667db600a721f15270f323666feccc08
SHA-256: 034a5aa3c9c44a185094ccc91321f5704beeadb09858dbe1a7fa2e40d7cc61ab

The app downloads and verifies the source archive, retains its LICENSE and this
provenance, and runs plugins/cli/support-fins.js with bun, Node 20.10+ or Deno 2.
The installer adds only a private `package.json` with `type: module` so Node 20
recognises the upstream .js files as ESM. No JavaScript dependencies are installed and no upstream engine is vendored.
Install/update means reinstalling this audited pin, never following a mutable release.
Requires curl and tar. Engine errors include stderr. Installation and generation
run on background workers. Only the plugin directory is changed.

CLI --fins-only preserves input alignment (unlike the combined export, which
centres/seats the object). The app exports all positive parts with volume and
instance transforms, reverses mirrored winding, then imports fins as a separate
Part with the inverse instance transform. Original geometry remains unchanged.
Fins belong to the object and are shared by its instances, as other parts are.
Regenerate after rotating/scaling an object. Negative volumes are refused because
raw positive STL would not represent their subtraction. Draw requires upstream's
interactive canvas and is deliberately unavailable; Auto and Full are supported.
The Orca plugin uses the same engine but merges cross-sections at slice time;
this integration adds editable geometry before slicing instead.

Options follow plugins/shared/engine/options.json and docs/FIN-SPEC.md; the
threshold is measured from the plate. Nozzle, layer height and top support gap
start from active profile values. Tine grip uses CLI percent units. Pad and
cutout choices are passed unchanged. Defaults remain upstream's otherwise.
Calibration: https://github.com/gittrahan/support-fins/blob/a6c9fa2c667db600a721f15270f323666feccc08/docs/CALIBRATION.md

## Verification (2026-11-03)

`CARGO_TARGET_DIR=~/.cache/bambu-target-fins cargo test --workspace`:
229 passed, 0 failed, 3 existing ignored. New tests cover world-coordinate STL
round trips including mirrored instances, invalid transforms/indices, part
identity/removal, 3MF persistence, history undo/redo, and runtime versions.
`cargo build -p app` succeeds with the same target directory.

Xvfb :98, 1600×1000, isolated XDG_CONFIG_HOME with copied engines.json,
SLICENV_CLOUD_OFF=1: installed through UI; loaded knob_12_6.3mf, rotated Y 35°,
generated Full fins, verified separate Support fins part, then undid it.
Active profile defaults correctly read 0.4 mm nozzle and 0.08 mm layer height.
Its support gap is also 0.08 mm, below upstream's 0.1–0.4 mm range; this smoke
run explicitly used 0.2 mm. The app preserves profile defaults rather than
silently clamping them; unsupported values produce the engine's error toast.

Slicing the rotated model after undo reported Orca exit 156 (empty first layer).
Regenerating the fins and slicing succeeded: 248 layers, 57 minutes, 1.6 g.
Screenshots: /tmp/fins.png (geometry), /tmp/fins-undo.png (undo),
/tmp/fins-installed.png (plugin), /tmp/fins-slice-result.png (successful slice).
Logs: /tmp/fins-tests.log, /tmp/fins-build.log, /tmp/fins-app.log.
