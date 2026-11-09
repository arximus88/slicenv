use std::collections::BTreeMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use slicer::Kind;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PresetType {
    #[serde(alias = "print")]
    Process,
    Filament,
    #[serde(alias = "printer")]
    Machine,
}

impl From<Kind> for PresetType {
    fn from(kind: Kind) -> Self {
        match kind {
            Kind::Machine => Self::Machine,
            Kind::Process => Self::Process,
            Kind::Filament => Self::Filament,
        }
    }
}

impl From<PresetType> for Kind {
    fn from(kind: PresetType) -> Self {
        match kind {
            PresetType::Machine => Self::Machine,
            PresetType::Process => Self::Process,
            PresetType::Filament => Self::Filament,
        }
    }
}

impl PresetType {
    pub fn directory(self) -> &'static str {
        match self {
            Self::Machine => "machine",
            Self::Process => "process",
            Self::Filament => "filament",
        }
    }

    pub fn settings_id(self) -> &'static str {
        match self {
            Self::Machine => "printer_settings_id",
            Self::Process => "print_settings_id",
            Self::Filament => "filament_settings_id",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OptionType {
    Float,
    Floats,
    Int,
    Ints,
    Bool,
    Bools,
    String,
    Strings,
    Percent,
    Percents,
    FloatOrPercent,
    FloatsOrPercents,
    Enum,
    Enums,
    Point,
    Points,
    Point3,
    PointsGroups,
}

impl OptionType {
    pub fn scalar(self) -> Self {
        match self {
            Self::Floats => Self::Float,
            Self::Ints => Self::Int,
            Self::Bools => Self::Bool,
            Self::Strings => Self::String,
            Self::Percents => Self::Percent,
            Self::FloatsOrPercents => Self::FloatOrPercent,
            Self::Enums => Self::Enum,
            Self::Points => Self::Point,
            other => other,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Simple,
    Advanced,
    Expert,
    Develop,
}

/// Metadata language. Missing Ukrainian strings fall back to the English source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Locale {
    #[default]
    En,
    Uk,
}

/// Smallest config unit that accepts the setting. Region settings can be applied
/// to parts/modifier volumes as well as whole objects; object settings affect a
/// whole object's slicing. Global settings belong to the print/preset.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Object,
    Region,
    #[default]
    Global,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct LocalizedOption {
    pub label: String,
    pub full_label: String,
    pub tooltip: String,
    pub sidetext: String,
    pub category: String,
    pub enum_labels: Vec<String>,
}

fn localized<'a>(english: &'a str, ukrainian: &'a str, locale: Locale) -> &'a str {
    if locale == Locale::Uk && !ukrainian.is_empty() {
        ukrainian
    } else {
        english
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct OptionDef {
    #[serde(rename = "type")]
    pub kind: OptionType,
    pub label: String,
    pub full_label: String,
    pub tooltip: String,
    pub sidetext: String,
    pub category: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub enum_values: Vec<String>,
    pub enum_labels: Vec<String>,
    pub default: Value,
    pub mode: Mode,
    pub gui_type: String,
    pub multiline: bool,
    pub nullable: bool,
    pub vector: bool,
    pub per_extruder: bool,
    pub per_filament: bool,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub uk: LocalizedOption,
    /// `print_config_def` or `bambu_compatibility`; the latter is preserved legacy data.
    #[serde(default)]
    pub origin: String,
    #[serde(default)]
    pub source_note: String,
    #[serde(default)]
    pub documentation_url: Option<String>,
}

impl OptionDef {
    pub fn label(&self, locale: Locale) -> &str {
        localized(&self.label, &self.uk.label, locale)
    }
    pub fn full_label(&self, locale: Locale) -> &str {
        localized(&self.full_label, &self.uk.full_label, locale)
    }
    pub fn tooltip(&self, locale: Locale) -> &str {
        localized(&self.tooltip, &self.uk.tooltip, locale)
    }
    pub fn sidetext(&self, locale: Locale) -> &str {
        localized(&self.sidetext, &self.uk.sidetext, locale)
    }
    pub fn category(&self, locale: Locale) -> &str {
        localized(&self.category, &self.uk.category, locale)
    }
    pub fn enum_label(&self, index: usize, locale: Locale) -> Option<&str> {
        self.enum_labels.get(index).map(|english| {
            localized(
                english,
                self.uk
                    .enum_labels
                    .get(index)
                    .map(String::as_str)
                    .unwrap_or_default(),
                locale,
            )
        })
    }
    pub fn enum_labels(&self, locale: Locale) -> impl Iterator<Item = &str> {
        (0..self.enum_labels.len()).map(move |i| self.enum_label(i, locale).unwrap())
    }
    pub fn can_override_object(&self) -> bool {
        self.scope != Scope::Global
    }
    pub fn can_override_part(&self) -> bool {
        self.scope == Scope::Region
    }
    pub fn documentation_url(&self) -> Option<&str> {
        self.documentation_url.as_deref()
    }
    /// Returns the localized tooltip if present, otherwise falls back to full_label, then label.
    pub fn description(&self, locale: Locale) -> &str {
        let tt = self.tooltip(locale);
        if !tt.is_empty() {
            tt
        } else {
            let fl = self.full_label(locale);
            if !fl.is_empty() {
                fl
            } else {
                self.label(locale)
            }
        }
    }
    /// Returns a bounded description clamped to `max_chars`, respecting UTF-8 boundaries.
    pub fn bounded_description(&self, max_chars: usize, locale: Locale) -> &str {
        let text = self.description(locale);
        if text.chars().count() <= max_chars {
            return text;
        }
        let boundary = text
            .char_indices()
            .take(max_chars)
            .last()
            .map(|(idx, c)| idx + c.len_utf8())
            .unwrap_or(text.len());
        &text[..boundary]
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Page {
    pub title: String,
    #[serde(default)]
    pub title_uk: String,
    pub icon: String,
    pub groups: Vec<Group>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Group {
    pub title: String,
    #[serde(default)]
    pub title_uk: String,
    pub icon: String,
    pub lines: Vec<Line>,
}

impl Page {
    pub fn title(&self, locale: Locale) -> &str {
        localized(&self.title, &self.title_uk, locale)
    }
}

impl Group {
    pub fn title(&self, locale: Locale) -> &str {
        localized(&self.title, &self.title_uk, locale)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Line {
    pub options: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Schema {
    pub options: BTreeMap<String, OptionDef>,
    pub layout: BTreeMap<PresetType, Vec<Page>>,
}

impl Schema {
    pub fn load() -> Result<Self> {
        Ok(Self {
            options: serde_json::from_str(include_str!("../schema/options.json"))?,
            layout: serde_json::from_str(include_str!("../schema/layout.json"))?,
        })
    }

    pub fn load_orca() -> Result<Self> {
        let mut options: BTreeMap<String, OptionDef> =
            serde_json::from_str(include_str!("../schema-orca/options.json"))?;
        add_orca_legacy_filament_keys(&mut options);
        Ok(Self {
            options,
            layout: serde_json::from_str(include_str!("../schema-orca/layout.json"))?,
        })
    }

    pub fn pages(&self, kind: PresetType) -> &[Page] {
        self.layout
            .get(&kind)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn documentation_url(&self, key: &str) -> Option<&str> {
        self.options
            .get(key)
            .and_then(|opt| opt.documentation_url.as_deref())
    }

    pub fn help_url(&self, key: &str) -> Option<&str> {
        self.documentation_url(key)
    }

    pub fn missing_layout_keys(&self) -> Vec<&str> {
        let mut keys: Vec<_> = self
            .layout
            .values()
            .flatten()
            .flat_map(|p| &p.groups)
            .flat_map(|g| &g.lines)
            .flat_map(|l| &l.options)
            .filter(|key| !self.options.contains_key(*key))
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        keys.dedup();
        keys
    }
}

/// Каталог Orca згенеровано з гілки, де `wall_filament`, `sparse_infill_filament` і
/// `solid_infill_filament` уже перейменовано на `*_filament_id` (min 0). Але проєкти Bambu Studio
/// приносять саме старі ключі, а OrcaSlicer ≤ 2.3 (стабільні збірки 2025 року) приймає їх лише
/// з min 1 — без цих описів перевірка сумісності вважала б їх «невідомими» й пропускала нуль,
/// на якому рушій падає з −18. Опис береться з нового ключа, межа — від Orca ≤ 2.3; значення 1
/// коректне для всіх версій (див. `slicer::ORCA_LEGACY_FILAMENT_KEYS`). У layout їх немає.
fn add_orca_legacy_filament_keys(options: &mut BTreeMap<String, OptionDef>) {
    for (legacy, current) in slicer::ORCA_LEGACY_FILAMENT_KEYS {
        let Some(def) = options.get(*current) else {
            continue;
        };
        let mut def = def.clone();
        def.min = Some(1.);
        def.default = Value::from(1);
        def.origin = "orca_legacy".into();
        def.source_note = format!(
            "OrcaSlicer ≤ 2.3: ціле ≥ 1, 1 — типовий філамент; з 2.4 `handle_legacy` переносить у {current} (1 → 0)."
        );
        options.entry((*legacy).into()).or_insert(def);
    }
}
