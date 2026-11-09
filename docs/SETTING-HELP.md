# Setting Documentation & Help URLs

## Overview

Settings in the Bambu Studio and OrcaSlicer ecosystems provide contextual documentation and wiki URLs for print, filament, and printer parameters. This document details the upstream source provenance, schema integration, URL mapping rules, and handling of long descriptions.

## Schema Integration

The `settings` crate exposes documentation URLs and description helpers through `OptionDef` and `Schema`:

```rust
// crates/settings/src/schema.rs

#[derive(Clone, Debug, Deserialize)]
pub struct OptionDef {
    // ... existing fields ...
    #[serde(default)]
    pub documentation_url: Option<String>,
}

impl OptionDef {
    /// Returns the upstream documentation URL if one exists.
    pub fn documentation_url(&self) -> Option<&str>;

    /// Localized tooltip if present, falling back to full_label, then label.
    pub fn description(&self, locale: Locale) -> &str;

    /// Bounded description clamped to max_chars on UTF-8 char boundaries.
    pub fn bounded_description(&self, max_chars: usize, locale: Locale) -> &str;
}

impl Schema {
    /// Look up documentation URL for a setting key.
    pub fn documentation_url(&self, key: &str) -> Option<&str>;

    /// Alias for documentation_url.
    pub fn help_url(&self, key: &str) -> Option<&str>;
}
```

The field uses `#[serde(default)]`, so existing or custom option dictionaries without `documentation_url` deserialize seamlessly as `None`.

## Upstream Provenance & Evidence

Documentation links are extracted directly from the upstream C++ GUI definitions in:
- `<BambuStudio-src>/src/slic3r/GUI/Tab.cpp`
- `<OrcaSlicer-src>/src/slic3r/GUI/Tab.cpp`

### 1. Bambu Studio Mechanics

In Bambu Studio:
- Tab option lines (`TabPrint::build`, `TabFilament::build`, `TabPrinter::build_fff`, etc.) assign `label_path` strings either directly (`line.label_path = "..."`) or via `append_single_option_line("key", "path")`.
- `OptionsGroup::get_url` (`src/slic3r/GUI/OptionsGroup.cpp`:1397) formats the link:
  ```cpp
  wxString str = from_u8(path_end);
  auto pos = str.find(L'#');
  if (pos != size_t(-1)) {
      pos++;
      wxString anchor = str.Mid(pos).Lower();
      anchor.Replace(L" ", "-");
      str = str.Left(pos) + anchor;
  }
  return wxString::Format(L"https://wiki.bambulab.com/%s/software/bambu-studio/%s", region, str);
  ```
- Anchors are lowercased and spaces replaced with hyphens.
- Resulting URL pattern: `https://wiki.bambulab.com/en/software/bambu-studio/{base}#{anchor}`.

### 2. OrcaSlicer Mechanics

In OrcaSlicer:
- Option rows declare `label_path` in `Tab.cpp` using `append_single_option_line`, `append_option_line`, or `create_line_with_widget`.
- `OptionsGroup::get_url` (`src/slic3r/GUI/OptionsGroup.cpp`:1404) formats the link:
  ```cpp
  return wxString::Format(L"https://www.orcaslicer.com/wiki/%s", from_u8(path_end));
  ```
- Resulting URL pattern: `https://www.orcaslicer.com/wiki/{path_end}`.
- Setting index and action registry (`SettingsIndex.hpp`, `ActionRegistry.hpp`) use `Line::label_path` for Speed Dial and help actions.

## Catalog Metrics & Ownership Distinction

| Catalog | Total Options | Options With Docs | Coverage | Target Domain |
|---|---|---|---|---|
| **Bambu Studio** (`schema/`) | 702 | 166 | 23.6% | `https://wiki.bambulab.com/en/software/bambu-studio/` |
| **OrcaSlicer** (`schema-orca/`) | 881 | 604 | 68.6% | `https://www.orcaslicer.com/wiki/` |

### Shared Options Example

For settings shared across both slicers (e.g. `wall_loops`), the documentation URL accurately distinguishes slicer ownership:
- **Bambu**: `https://wiki.bambulab.com/en/software/bambu-studio/wall-generator`
- **Orca**: `https://www.orcaslicer.com/wiki/strength_settings_walls#wall-loops`

### Orca-Specific Options Example

- `adaptive_bed_mesh_margin`: `https://www.orcaslicer.com/wiki/printer_basic_information_adaptive_bed_mesh#mesh-margin`
- `make_overhang_printable`: `https://www.orcaslicer.com/wiki/quality_settings_overhangs#make-overhang-printable`
- `scarf_joint_flow_ratio`: `https://www.orcaslicer.com/wiki/quality_settings_seam#scarf-joint-seam`

### Bambu-Specific Options Example

- `z_hop`: `https://wiki.bambulab.com/en/software/bambu-studio/parameter/retraction#z-hop-when-retracting`
- `top_surface_pattern`: `https://wiki.bambulab.com/en/software/bambu-studio/fill-patterns#infill-of-the-top-surface-and-bottom-surface`
- `fuzzy_skin`: `https://wiki.bambulab.com/en/software/bambu-studio/parameter/fuzzy-skin`

## Strict Non-Invention Policy (Fallback)

Options without an explicit upstream `label_path` mapping return `None` (`documentation_url: null` or omitted in JSON).

- We do not synthesize unverified URLs or guess slugs from parameter names.
- Options like `sparse_infill_density` or internal G-code variables lack per-key upstream wiki pages and strictly return `None`.
- The parent application (`crates/app`) handles UI tooltips. When `opt.documentation_url` is `None`, the parent app can optionally provide a generic Wiki index link (`https://wiki.bambulab.com` or `https://www.orcaslicer.com/wiki/`), but the catalog metadata never claims unverified per-key coverage.

## Tooltip Bounds & Descriptions

Some slicer options have very long tooltips:
- Bambu `support_style`: 750 characters.
- Orca `enable_extra_bridge_layer`: 1646 characters.
- Orca `slowdown_for_curled_perimeters`: 1565 characters.

To support both rich hover cards (handled by parent tooltip UI) and constrained UI widgets (status bars, compact popups, summary lists), `OptionDef::bounded_description(max_chars, locale)` truncates safely at char boundaries without breaking multi-byte UTF-8 sequences.
