use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use serde_json::{Map, Value, json};
use slicer::PresetStore;

use crate::{
    Group, Line, OptionDef, Page, ParsedValue, PresetType, Schema, TypedValue, ValidationError,
};

pub struct EditablePreset<'a> {
    schema: &'a Schema,
    kind: PresetType,
    parent: String,
    base: Map<String, Value>,
    edits: BTreeMap<String, Value>,
}

impl<'a> EditablePreset<'a> {
    /// `parent` — системний профіль, відносно якого зберігається дельта.
    pub fn from_system(
        schema: &'a Schema,
        store: &PresetStore,
        kind: PresetType,
        parent: &str,
    ) -> Result<Self> {
        Ok(Self::new(
            schema,
            kind,
            parent,
            store.flatten(kind.into(), parent)?,
        ))
    }

    pub fn new(
        schema: &'a Schema,
        kind: PresetType,
        parent: &str,
        base: Map<String, Value>,
    ) -> Self {
        Self {
            schema,
            kind,
            parent: parent.into(),
            base,
            edits: BTreeMap::new(),
        }
    }

    pub fn schema(&self) -> &'a Schema {
        self.schema
    }

    pub fn kind(&self) -> PresetType {
        self.kind
    }

    pub fn parent(&self) -> &str {
        &self.parent
    }

    pub fn base(&self) -> &Map<String, Value> {
        &self.base
    }

    pub fn value(&self, key: &str) -> Option<&Value> {
        self.edits.get(key).or_else(|| self.base.get(key))
    }

    /// Current value, falling back to source defaults for partial/object presets.
    pub fn effective_value(&self, key: &str) -> Option<&Value> {
        self.value(key)
            .or_else(|| self.schema.options.get(key).map(|option| &option.default))
    }

    pub fn is_enabled(&self, key: &str) -> bool {
        crate::is_enabled(key, self)
    }
    pub fn is_visible(&self, key: &str) -> bool {
        crate::is_visible(key, self)
    }

    pub fn parsed(&self, key: &str) -> Option<std::result::Result<ParsedValue, ValidationError>> {
        self.value(key).map(|v| self.schema.parse(key, v))
    }

    pub fn set(&mut self, key: &str, value: Value) -> std::result::Result<(), ValidationError> {
        self.schema.validate(key, &value)?;
        if self.base.get(key) == Some(&value) {
            self.edits.remove(key);
        } else {
            self.edits.insert(key.into(), value);
        }
        Ok(())
    }

    pub fn set_typed(
        &mut self,
        key: &str,
        value: &TypedValue,
    ) -> std::result::Result<(), ValidationError> {
        self.set(key, self.schema.format(key, value)?)
    }

    pub fn is_dirty(&self, key: &str) -> bool {
        self.edits.contains_key(key)
    }

    pub fn dirty_keys(&self) -> impl Iterator<Item = &str> {
        self.edits.keys().map(String::as_str)
    }

    pub fn reset(&mut self, key: &str) {
        self.edits.remove(key);
    }

    pub fn reset_all(&mut self) {
        self.edits.clear();
    }

    pub fn flattened(&self) -> Map<String, Value> {
        let mut values = self.base.clone();
        values.extend(self.edits.iter().map(|(k, v)| (k.clone(), v.clone())));
        values
    }

    pub fn pages(&self) -> impl Iterator<Item = PageView<'_>> {
        self.schema
            .pages(self.kind)
            .iter()
            .map(|page| PageView { page, preset: self })
    }

    /// Новий пресет у переданій теці `user/<id>/<kind>`; наявні файли не перезаписуємо.
    pub fn save_user(
        &self,
        directory: &Path,
        name: &str,
        info: &UserPresetInfo,
    ) -> Result<SavedPreset> {
        if name.trim().is_empty()
            || name == "."
            || name == ".."
            || name.contains(['/', '\\', '\0', '\n', '\r'])
        {
            bail!("invalid preset name {name:?}");
        }
        if name == self.parent {
            bail!("user preset must have a different name than its system parent");
        }
        if self.parent.is_empty() {
            bail!("system parent must not be empty");
        }
        for field in [
            &info.user_id,
            &info.setting_id,
            &info.base_id,
            &info.sync_info,
        ] {
            if field.contains(['\n', '\r', '\0']) {
                bail!("sidecar fields must fit on one line");
            }
        }
        let version = info
            .version
            .as_deref()
            .or_else(|| self.base.get("version").and_then(Value::as_str))
            .context("preset version is required")?;
        let mut delta = Map::new();
        delta.insert("version".into(), json!(version));
        delta.insert("name".into(), json!(name));
        delta.insert("from".into(), json!("User"));
        for (key, value) in &self.edits {
            self.schema.validate(key, value)?;
            let value = if self.schema.options[key].vector && !value.is_array() {
                self.schema
                    .format(key, self.schema.parse(key, value)?.typed())?
            } else {
                value.clone()
            };
            delta.insert(key.clone(), value);
        }
        delta.insert("inherits".into(), json!(self.parent));
        let id_key = self.kind.settings_id();
        let id = if self.kind == PresetType::Filament {
            json!([name])
        } else {
            json!(name)
        };
        delta.insert(id_key.into(), id);
        // Філамент має стабільний ідентифікатор матеріалу окремо від назви пресета.
        if self.kind == PresetType::Filament
            && let Some(id) = self.base.get("filament_id")
        {
            delta.insert("filament_id".into(), id.clone());
        }
        let extruder_keys: &[&str] = match self.kind {
            PresetType::Machine => &["printer_extruder_id", "printer_extruder_variant"],
            PresetType::Process => &["print_extruder_id", "print_extruder_variant"],
            PresetType::Filament => &["filament_extruder_variant"],
        };
        for &key in extruder_keys {
            if let Some(value) = self.value(key) {
                delta.entry(key).or_insert_with(|| value.clone());
            }
        }
        let bytes = serde_json::to_vec_pretty(&delta)?;
        let base_id = if info.base_id.is_empty() {
            self.base
                .get("setting_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
        } else {
            &info.base_id
        };
        let sidecar = format!(
            "sync_info = {}\nuser_id = {}\nsetting_id = {}\nbase_id = {}\nupdated_time = {}\n",
            info.sync_info, info.user_id, info.setting_id, base_id, info.updated_time
        );
        fs::create_dir_all(directory)?;
        let json = directory.join(format!("{name}.json"));
        let info_path = directory.join(format!("{name}.info"));
        // create_new захищає також від симлінків і випадкового перезапису.
        let mut json_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&json)?;
        let result = (|| -> Result<()> {
            let mut info_file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&info_path)?;
            let write_result = (|| -> Result<()> {
                json_file.write_all(&bytes)?;
                json_file.write_all(b"\n")?;
                json_file.sync_all()?;
                info_file.write_all(sidecar.as_bytes())?;
                info_file.sync_all()?;
                Ok(())
            })();
            if write_result.is_err() {
                let _ = fs::remove_file(&info_path);
            }
            write_result
        })();
        if result.is_err() {
            let _ = fs::remove_file(&json);
        }
        result?;
        Ok(SavedPreset {
            json,
            info: info_path,
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct UserPresetInfo {
    pub version: Option<String>,
    pub sync_info: String,
    pub user_id: String,
    pub setting_id: String,
    pub base_id: String,
    pub updated_time: i64,
}

#[derive(Clone, Debug)]
pub struct SavedPreset {
    pub json: PathBuf,
    pub info: PathBuf,
}

pub struct PageView<'a> {
    pub page: &'a Page,
    preset: &'a EditablePreset<'a>,
}

impl<'a> PageView<'a> {
    pub fn groups(&self) -> impl Iterator<Item = GroupView<'a>> + '_ {
        self.page.groups.iter().map(|group| GroupView {
            group,
            preset: self.preset,
        })
    }
}

pub struct GroupView<'a> {
    pub group: &'a Group,
    preset: &'a EditablePreset<'a>,
}

impl<'a> GroupView<'a> {
    pub fn lines(&self) -> impl Iterator<Item = LineView<'a>> + '_ {
        self.group.lines.iter().map(|line| LineView {
            line,
            preset: self.preset,
        })
    }
}

pub struct LineView<'a> {
    pub line: &'a Line,
    preset: &'a EditablePreset<'a>,
}

impl<'a> LineView<'a> {
    pub fn options(&self) -> impl Iterator<Item = OptionView<'a>> + '_ {
        self.line.options.iter().map(|key| OptionView {
            key,
            metadata: self.preset.schema.options.get(key),
            value: self.preset.value(key),
            dirty: self.preset.is_dirty(key),
        })
    }
}

pub struct OptionView<'a> {
    pub key: &'a str,
    pub metadata: Option<&'a OptionDef>,
    pub value: Option<&'a Value>,
    pub dirty: bool,
}
