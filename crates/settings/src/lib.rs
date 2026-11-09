//! Схема Bambu Studio, значення Bambu та зміни користувацьких пресетів.

mod editable;
mod fields;
pub mod mo;
mod migration;
mod rules;
mod schema;
mod value;

pub use editable::{
    EditablePreset, GroupView, LineView, OptionView, PageView, SavedPreset, UserPresetInfo,
};
pub use fields::{OVERRIDES_PAGE, is_nil, list_text, nil_like, parse_list};
pub use migration::{
    ConfigSet, MigrationEntry, MigrationReport, MigrationStatus, preview_migration,
};
pub use rules::{
    OptionState, ToggleContext, is_enabled, is_visible, option_state, option_state_with_context,
};
pub use schema::{
    Group, Line, Locale, LocalizedOption, Mode, OptionDef, OptionType, Page, PresetType, Schema,
    Scope,
};
pub use value::{ParsedValue, TypedValue, ValidationError};

use serde_json::{Map, Value};

pub const A1_PRESETS: &[(PresetType, &str)] = &[
    (PresetType::Machine, "Bambu Lab A1 0.4 nozzle"),
    (PresetType::Process, "0.20mm Standard @BBL A1"),
    (PresetType::Process, "0.08mm Extra Fine @BBL A1"),
    (PresetType::Filament, "Bambu PLA Basic @BBL A1"),
    (PresetType::Filament, "Bambu PETG Translucent @BBL A1"),
];

#[derive(Clone, Debug, serde::Serialize)]
pub struct Coverage {
    pub total: usize,
    pub covered: usize,
    pub percent: f64,
    pub missing: Vec<String>,
    /// Preset identity, inheritance, compatibility and descriptive fields.
    pub metadata: Vec<String>,
    pub raw_total: usize,
}

/// Keys stored alongside slicing options in Bambu preset JSON.
/// Classification takes priority even for keys also accepted by PrintConfigDef.
pub fn is_metadata_key(key: &str) -> bool {
    matches!(
        key,
        "type"
            | "name"
            | "from"
            | "instantiation"
            | "inherits"
            | "include"
            | "setting_id"
            | "filament_id"
            | "description"
            | "version"
            | "compatible_printers"
            | "compatible_printers_condition"
            | "compatible_prints"
            | "compatible_prints_condition"
            | "print_settings_id"
            | "filament_settings_id"
            | "printer_settings_id"
            | "different_settings_to_system"
            | "inherits_group"
            | "is_custom_defined"
    )
}

impl Schema {
    pub fn coverage(&self, values: &Map<String, Value>) -> Coverage {
        let missing: Vec<_> = values
            .keys()
            .filter(|key| !is_metadata_key(key) && !self.options.contains_key(*key))
            .cloned()
            .collect();
        let metadata: Vec<_> = values
            .keys()
            .filter(|key| is_metadata_key(key))
            .cloned()
            .collect();
        let total = values.len() - metadata.len();
        let covered = total - missing.len();
        Coverage {
            total,
            raw_total: values.len(),
            metadata,
            covered,
            percent: if total == 0 {
                100.0
            } else {
                covered as f64 / total as f64 * 100.0
            },
            missing,
        }
    }
}

pub use paths::profile;
