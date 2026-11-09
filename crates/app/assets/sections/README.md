# OrcaSlicer icons

Source: OrcaSlicer v2.4.2, `resources/images/` (local engine distribution).
https://github.com/OrcaSlicer/OrcaSlicer/tree/v2.4.2/resources/images
License: AGPL-3.0, same as this application. Original filenames are preserved.

Only icons referenced by the Bambu and Orca settings mappings / Prepare tools are bundled. Explicit hex
colors are normalized to `currentColor`; GPUI `svg()` renders these as alpha
masks tinted with the current theme text color in both light and dark themes.
The upstream param icons have no separate dark variants.

Group mappings follow `new_optgroup` in [Orca Tab.cpp](https://github.com/OrcaSlicer/OrcaSlicer/blob/v2.4.2/src/slic3r/GUI/Tab.cpp). Schema icons are retained for application-specific groups.
