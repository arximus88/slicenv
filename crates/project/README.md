# Editable Bambu projects

`project` provides the W4 model independently of the GPU and UI crates. The workspace
already discovers it through `crates/*`; it uses existing workspace dependencies.

`Project::load` accepts Bambu 3MF and binary/ASCII STL. `load_3mf`, `load_stl`,
`save_3mf`, and `validate` are also available. Saving validates first, writes a
sibling temporary file, flushes it, and atomically replaces the destination.

The model exposes objects, indexed volume meshes, volume types, object/volume
configuration overrides, instances, plates, and the complete project settings
JSON map. Geometry and transforms use `f64` (`DAffine3`), in millimetres. Volume
transforms map mesh coordinates into object coordinates; instance transforms map
object coordinates into **plate-local coordinates**. `Instance::plate == None`
means off-plate, where its transform uses project coordinates. `plate_offset`
returns Bambu's global grid offset when needed.

```rust,no_run
use project::{DVec3, Project};

fn edit(input: &str, output: &str) -> anyhow::Result<()> {
    let mut project = Project::load(input)?;
    let instance = project.plates[0].objects[0];
    project.translate(instance, DVec3::new(10.0, 0.0, 0.0))?;
    project.drop_to_bed(instance)?;
    project.save_3mf(output)?;
    Ok(())
}
```

Editing methods include `translate`, `rotate`, `scale`, `drop_to_bed`,
`lay_on_face(instance, volume_index, triangle_index)`, `center_on_plate`,
`duplicate_instance`, `delete_instance`, `duplicate_object`, `delete_object`,
`move_to_plate`, `add_plate`, `delete_plate`, `rename_plate`, and `arrange`.
Rotations and scaling act around the current world bounding-box centre.
A face is oriented using its transformed geometric normal, then dropped to Z=0.
Arrangement packs world bounding boxes into shelves in 256 × 256 mm, with the
specified spacing; failure leaves all placements unchanged. Locked plates reject
arrangement.

`merge_instances(&[InstanceRef])` consumes at least two selected instances on
the same plate and returns the new instance. It creates one multi-volume object,
baking each source instance transform into that source's volume transforms so
the visible geometry stays in place. Volume mesh, role, name, config, and scoped
metadata are retained. `split_instance(InstanceRef)` creates one object per
volume and returns their instance refs; each retains the original instance
transform, so placement and volume roles stay intact. Both operations stage on a
copy and commit only after validating their selection. They operate on the
chosen instance only; other instances of a source object remain. Merge requires
matching object configs and refuses source objects with layer ranges. Split
also refuses layer ranges because the ranges cannot yet be divided between
volumes safely. `set_volume_kind(object_id, volume_index, kind)` edits one
volume's role without changing its geometry or scoped config. Object and volume
configs can be replaced with `set_object_config` and `set_volume_config`.

`history::History` stores up to 50 undo/redo snapshots by default. Create an
`Entry` before a transaction and call `record` after it succeeds; unchanged
projects do not consume history or clear redo. Undo/redo swap the full project
and restore the accompanying `history::Selection`, including the active plate.
A new edit after undo clears redo. Opening a different project must clear history.
`Volume::mesh` is `Arc<IndexedMesh>`, archive bytes and raw XML mesh fragments
are also shared. Use `Arc::make_mut(&mut volume.mesh)` before editing geometry
or per-triangle attributes; transforms and configs need no mesh copy.

`layer_ranges_for_object` and `object_layer_ranges` return ranges resolved from
the 1-based model-object position used by Bambu, rather than matching the range
ID against a root object ID. `layer_ranges` returns the resolved ranges, and
`unmatched_layer_range_ids` reports positional IDs that do not resolve. On save,
resolved ranges follow the current exported object order. The setter APIs accept
the resolved root object ID and require the source range file. These APIs do not evaluate Boolean operations: negative volumes
remain overlays until the downstream slicer interprets them.

Object IDs remain stable. Instance indexes shift after deletion; deleting an
object's last instance removes the object. Plate indexes are consecutive and
one-based. Deleting a plate renumbers subsequent plates and their membership;
its instances become off-plate. Deleting the last plate creates an empty plate.
`Instance::plate` is authoritative; `Plate::objects` is a derived list of
`InstanceRef { object, instance }`. All editing methods update it. Call
`sync_plates` after directly changing public membership fields.

`plate_mesh(Some(index))` returns `TriangleSoup { positions, normals }` with
`Vec<[f32; 3]>` fields, directly transferable into `viewer::Mesh`. It contains
normal parts with flat normals and includes both volume and instance transforms.
`plate_mesh(None)` renders off-plate objects. `plate_mesh_with_kinds` can produce
separate modifier, negative-volume, or support overlays. `instance_bounds` and
`plate_bounds` provide physical-part world bounding boxes for picking and edits;
`instance_bounds_with_kinds` also supports picking overlays. These coordinates
are relative to the selected bed, without automatic centering or dropping.
Negative/modifier volumes are geometry overlays, not Boolean-evaluated results.

Object/volume `config` maps retain all scoped key/value metadata except `name`,
which has its own field. This includes source matrices and source-file details;
Bambu's source `matrix` metadata must not be applied again to baked geometry.
Scoped `metadata` XML nodes preserve attributes and unknown children. Root XML
and model-settings templates, all archive entry contents, and per-triangle
attributes are retained. Unchanged saves preserve **every uncompressed archive
entry byte-for-byte**, including thumbnails, profiles, painted facets, assembly
views, and unknown extensions. ZIP container compression/timestamps may change.
Edits reuse untouched external mesh files; changed/new meshes get new resources.
Legacy triangle-range volumes are split into indexed volumes when loading and
written as modern Bambu parts after editing. Instance assembly transforms and
identity metadata follow duplication/deletion independently of print placement.

Current boundaries:

- 3MF units other than millimetres and unknown volume types are rejected.
- General nested component graphs are flattened when an edited project is saved;
  extension semantics attached to intermediate assembly nodes are not modelled.
  The normal Bambu layout and unchanged archives preserve their data.
- Thumbnails, plate JSON, auxiliary files, and existing slice data are retained,
  not regenerated or validated after edits. Reslice edited projects before using
  their print output. New STL projects need the caller to select/populate a print
  configuration; importing STL does not invent printer/filament presets.
- Arrangement uses bounding boxes; it does not optimize rotations, nest shapes,
  or reserve printer-specific purge/calibration/exclusion areas.
- Loss-preserving loading retains decompressed archive data in memory alongside
  geometry. The 119 MB XML stress fixture was tested, but loading is synchronous.


