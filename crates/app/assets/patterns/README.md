# OrcaSlicer icons

Source: OrcaSlicer v2.4.2, `resources/images/` (local engine distribution).
https://github.com/OrcaSlicer/OrcaSlicer/tree/v2.4.2/resources/images
License: AGPL-3.0, same as this application. Original filenames are preserved.

Only icons referenced by the Bambu and Orca settings mappings / Prepare tools are bundled. Explicit hex
colors are normalized to `currentColor`; GPUI `svg()` renders these as alpha
masks tinted with the current theme text color in both light and dark themes.
The upstream param icons have no separate dark variants.

Serialized aliases: `spiralinset` → `param_concentric`, `zig-zag` → `param_rectilinear`, `globalmonotonicline` → `param_monotonicline`, `ironingarchimedeanspiral` → `param_archimedeanchords`, `2dlattice` → `param_lateral-lattice`, `default` → `param_rectilinear`, `auto` → `param_rectilinear`. Auto/default use the rectilinear default; global monotonic and ironing spiral use their closest upstream geometry.

Both `schema` and `schema-orca` enum catalogs are covered. The `center_of_surface_pattern` placement selector has no matching upstream param icons and deliberately remains text-only.
