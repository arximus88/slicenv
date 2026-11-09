# Engine settings migration

`settings::preview_migration` compares caller-supplied global, object, volume, and layer-range config maps with a target `Schema` and the target engine's runtime defaults. Each config map carries a caller-provided label and `Scope`. The function only returns a `MigrationReport`; it does not modify the maps or a project.

Known values are validated with the same parser and validator used elsewhere in `settings`, including Bambu string and array encodings, enum membership, and numeric bounds. An invalid value receives a proposed replacement only when the target runtime default exists and also validates against the target schema. A missing or invalid runtime default leaves `proposed_value` empty. Unsupported keys receive an `Unsupported` report entry with no proposal and must remain preserved in the project data.

The project layer owns applying explicitly accepted `proposed_value`s to an in-memory project. It can then export a temporary slice project through the existing 3MF save path, which preserves archive and painter data. The source header and engine identity must remain truthful; migration does not rename settings or rewrite source metadata.

File-version compatibility is a separate gate from option migration. The caller must handle any “newer file” condition through an explicit `allow-newer-file` decision before proceeding; a successful settings preview does not authorize bypassing that gate.
