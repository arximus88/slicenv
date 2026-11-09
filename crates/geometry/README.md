# Geometry preview

Safe, read-only native CSG through pinned `manifold-csg = 0.4.1`.

```rust,ignore
let mesh: viewer::Mesh = geometry::plate_preview(&project, Some(1))?;
let local: viewer::Mesh = geometry::object_preview(&project.objects[0])?;
let (above, below) = geometry::split_object_by_plane(
    &project.objects[0], [0.0, 0.0, 1.0], 20.0,
)?;
```

`plate_preview` selects instances by exact plate equality; `None` selects objects
outside plates. It unions each object's Part volumes, subtracts all NegativeVolume
volumes, then places the result for each matching instance. Volume transforms
enter the f64 object-local boolean; instance transforms follow it. Mirrored
transforms reverse winding, and normals are recomputed after placement. Multiple
objects remain separate solids. Modifiers and support controls do not enter CSG;
non-printable instances stay visible like `Project::plate_mesh`.

Plane split returns capped positive/negative halves in object-local coordinates.
Its equation is `normal · point = offset`; non-unit normals work. It evaluates
negative volumes first. It is a preview API, not a project editing operation.

Errors include source object/volume context for invalid, open or nonmanifold
input. Invalid indices, nonfinite vertices and singular transforms also fail.
Keep the previous/raw preview and show the error. No input, archive entry,
metadata, mesh attribute or painter data is modified; no mesh repair is attempted.
Results are viewer triangle soups with flat normals, not painted meshes suitable
for writing back to a project. f64 kernel topology is checked before export;
f32 viewer export can collapse microscopic edges (observed on angel), so do not
reuse viewer output as boolean input or as a watertight manufacturing mesh.

Run this synchronous CPU API on a worker. CSG executes once per selected object,
not per instance; placement/export cost scales with output triangles × instances.
There is no persistent cache or cancellation API here; the caller should discard
stale generations. `object_preview` allows local-result caching by a caller.

## Build requirements

Rust 1.85+, git, CMake >=3.18 and a C++17 compiler. The sys crate initially clones
manifold3d v3.5.3 and fetches its native dependencies, then caches the build.
Network access is needed on that first build. Parallel/TBB support is disabled
to avoid an extra native dependency and leave scheduling to the caller.
For prebuilt/offline installations see `MANIFOLD_CSG_LIB_DIR` in the
[official crate README](https://github.com/zmerlynn/manifold-csg#build-requirements).

## Verification

```sh
cargo test -p geometry --tests --examples
cargo clippy -p geometry --all-targets -- -D warnings
GEOMETRY_ANGEL_FIXTURE=/path/angel_medium.3mf \
  cargo test -p geometry --test boolean real_angel -- --ignored --nocapture
cargo run -p geometry --example snapshot -- /path/angel_medium.3mf /tmp/angel-boolean.png
```

The external-fixture test is ignored by default. Synthetic tests check difference
volume, closed oppositely directed edges, union-before-subtraction, capped split
volume conservation, mirror/rotation/nonuniform scale, plate selection, errors,
and complete project equality including archive and triangle paint attributes.
The real-fixture test checks subtraction, successful kernel validation, finite
normals, and project equality. Its f32 soup is not subjected to strict edge
identity checks because tiny edges can collapse during viewer conversion.

Observed locally (dev profile, kernel dependency optimized): angel load
107–114 ms, boolean + export 190–199 ms, 124,314 output triangles. Raw signed
volume 23,698.8961 mm³; boolean volume 23,685.1159 mm³ (13.7802 mm³ removed).
These are single-run observations, not benchmark guarantees. Six synthetic tests
and the separately enabled real angel test pass. No workspace-wide tests run.

The snapshot example writes the boolean PNG and `.raw.png`, plus matched
`.underside.png` / `.raw.underside.png` views and, where negative volumes exist,
`.cut-detail.png` / `.raw.cut-detail.png` views framed around the cutter bounds.
Actual rendered outputs were inspected using the external fixture at
`~/Documents/3d-models/angel_medium.3mf`; generated PNGs are in `/tmp` and
are not committed.
