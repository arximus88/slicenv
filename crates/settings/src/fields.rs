//! Що й у якому вигляді показує редактор пресета: видимі сторінки, перевизначення філаменту
//! (nullable-опції з `nil`), списки сумісності й багаторядковий текст.

use serde_json::Value;

use crate::{EditablePreset, Mode, OptionDef, OptionType, Page, PresetType, Schema};

/// Сторінка філаменту з перевизначеннями налаштувань принтера (Tab.cpp, «Setting Overrides»).
pub const OVERRIDES_PAGE: &str = "Setting Overrides";

/// Списки імен пресетів, які редагуються по одному на рядок.
const LIST_OPTIONS: &[&str] = &["compatible_printers", "compatible_prints"];

/// Значення `nil` у Bambu/Orca: «не перевизначено, береться з принтера». Вектор — коли nil
/// усі елементи (так їх пише Studio: `["nil"]`).
pub fn is_nil(value: &Value) -> bool {
    match value {
        Value::String(s) => s == "nil",
        Value::Array(items) => !items.is_empty() && items.iter().all(is_nil),
        _ => false,
    }
}

/// `nil` у формі `like`: вектор із тією самою кількістю елементів або рядок.
pub fn nil_like(like: Option<&Value>) -> Value {
    match like {
        Some(Value::Array(items)) => Value::Array(
            (0..items.len().max(1))
                .map(|_| Value::String("nil".into()))
                .collect(),
        ),
        Some(Value::String(_)) => Value::String("nil".into()),
        _ => Value::Array(vec![Value::String("nil".into())]),
    }
}

/// Список рядків для показу — по одному на рядок.
pub fn list_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::String(s)) => s.split(';').collect::<Vec<_>>().join("\n"),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// Текст поля → список: порожні рядки відкидаються, пробіли по краях — теж.
pub fn parse_list(text: &str) -> Value {
    Value::Array(
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| Value::String(l.to_owned()))
            .collect(),
    )
}

impl Schema {
    /// Перевизначення філаменту: nullable-опція зі сторінки «Setting Overrides».
    pub fn is_filament_override(&self, key: &str) -> bool {
        self.options.get(key).is_some_and(|def| def.nullable)
            && self
                .pages(PresetType::Filament)
                .iter()
                .filter(|p| p.title == OVERRIDES_PAGE)
                .flat_map(|p| &p.groups)
                .flat_map(|g| &g.lines)
                .any(|l| l.options.iter().any(|k| k == key))
    }

    /// Ключ принтера/процесу, який перевизначає `filament_<key>`.
    pub fn override_base_key<'k>(&self, key: &'k str) -> Option<&'k str> {
        key.strip_prefix("filament_")
            .filter(|base| self.options.contains_key(*base))
    }

    /// Список імен (сумісні принтери/процеси), а не одне значення на екструдер.
    pub fn is_list_option(&self, key: &str) -> bool {
        LIST_OPTIONS.contains(&key)
            && self
                .options
                .get(key)
                .is_some_and(|d| d.kind == OptionType::Strings)
    }
}

/// Текстові опції профілі часто пропускають, бо типове значення порожнє, а перевизначення —
/// бо типово воно `nil`. Studio/Orca показують їх усе одно (зі значенням за замовчуванням).
fn shows_default(schema: &Schema, key: &str, def: &OptionDef) -> bool {
    def.multiline
        || matches!(def.kind, OptionType::String | OptionType::Strings)
        || schema.is_filament_override(key)
}

impl EditablePreset<'_> {
    /// Значення для показу: з пресета, а для текстових полів і перевизначень — типове.
    pub fn shown_value(&self, key: &str) -> Option<&Value> {
        self.value(key).or_else(|| {
            let def = self.schema().options.get(key)?;
            shows_default(self.schema(), key, def)
                .then_some(&def.default)
                .filter(|v| !v.is_null())
        })
    }

    /// Чи вмикач «Перевизначити» для nullable-опції філаменту стоїть.
    pub fn is_overridden(&self, key: &str) -> bool {
        self.shown_value(key).is_some_and(|v| !is_nil(v))
    }

    /// Чи показувати рядок `key` у редакторі (`advanced` — перемикач «Розширені»).
    pub fn is_shown(&self, key: &str, advanced: bool) -> bool {
        let mode_ok = match self.schema().options.get(key) {
            Some(def) => advanced || def.mode == Mode::Simple,
            None => advanced,
        };
        mode_ok && self.shown_value(key).is_some() && self.is_visible(key)
    }

    /// Чи є на сторінці хоч одне видиме поле.
    pub fn page_has_fields(&self, page: &Page, advanced: bool) -> bool {
        page.groups
            .iter()
            .flat_map(|g| &g.lines)
            .flat_map(|l| &l.options)
            .any(|key| self.is_shown(key, advanced))
    }

    /// Індекси сторінок, які варто показувати вкладками: без жодного поля сторінка — помилка UX.
    pub fn visible_pages(&self, advanced: bool) -> Vec<usize> {
        self.schema()
            .pages(self.kind())
            .iter()
            .enumerate()
            .filter(|(_, page)| self.page_has_fields(page, advanced))
            .map(|(i, _)| i)
            .collect()
    }
}
