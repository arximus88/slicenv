//! Report-only preview of settings that are incompatible with a target engine.
use serde_json::{Map, Value};

use crate::{OptionDef, OptionType, Schema, Scope};

/// A config map supplied by the project layer. `label` identifies its owner
/// (for example `global`, `object 4`, `volume 2`, or `layer range 1`).
#[derive(Clone, Debug)]
pub struct ConfigSet<'a> {
    pub label: &'a str,
    pub scope: Scope,
    pub values: &'a Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MigrationStatus {
    Invalid,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MigrationEntry {
    pub label: String,
    pub key: String,
    pub old_value: Value,
    /// Validated target runtime default; absent when no safe suggestion exists.
    pub proposed_value: Option<Value>,
    pub reason: String,
    pub status: MigrationStatus,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MigrationReport {
    pub entries: Vec<MigrationEntry>,
}

/// Preview values against a target schema and the target engine's runtime
/// defaults. This function never mutates any supplied config map.
pub fn preview_migration(
    configs: &[ConfigSet<'_>],
    target: &Schema,
    runtime_defaults: &Map<String, Value>,
) -> MigrationReport {
    let mut entries = Vec::new();
    for config in configs {
        for (key, old_value) in config.values {
            let Some(def) = target.options.get(key) else {
                entries.push(MigrationEntry {
                    label: config.label.to_owned(),
                    key: key.clone(),
                    old_value: old_value.clone(),
                    proposed_value: None,
                    reason: "key is not supported by the target schema; preserve it".into(),
                    status: MigrationStatus::Unsupported,
                });
                continue;
            };
            // A key known to the engine but unsupported at this scope is still
            // config incompatibility; do not silently treat it as unknown.
            if config.scope != Scope::Global && def.scope == Scope::Global {
                entries.push(MigrationEntry {
                    label: config.label.to_owned(),
                    key: key.clone(),
                    old_value: old_value.clone(),
                    proposed_value: None,
                    reason: format!(
                        "target option cannot be applied at {:?} scope",
                        config.scope
                    ),
                    status: MigrationStatus::Invalid,
                });
                continue;
            }
            if let Err(error) = target.validate(key, old_value) {
                let default = || {
                    runtime_defaults
                        .get(key)
                        .filter(|default| target.validate(key, default).is_ok())
                        .cloned()
                };
                let (proposed_value, reason) = match clamp_to_range(def, old_value) {
                    // −1 — «авто» в Bambu Studio (raft_first_layer_expansion,
                    // tree_support_wall_count…): межа діапазону змінила б зміст, тож
                    // береться типове значення рушія.
                    Some(Clamp::AutoSentinel) => (
                        default(),
                        format!(
                            "{}; -1 means auto in Bambu Studio, the target has no auto value — use its default",
                            error.message
                        ),
                    ),
                    Some(Clamp::Value(clamped)) if target.validate(key, &clamped).is_ok() => {
                        let range = match (def.min, def.max) {
                            (Some(min), Some(max)) => format!("[{min}, {max}]"),
                            (Some(min), None) => format!("[{min}, …]"),
                            (None, Some(max)) => format!("[…, {max}]"),
                            (None, None) => String::new(),
                        };
                        (
                            Some(clamped),
                            format!("{}; clamped to the target range {range}", error.message),
                        )
                    }
                    _ => (default(), error.message),
                };
                entries.push(MigrationEntry {
                    label: config.label.to_owned(),
                    key: key.clone(),
                    old_value: old_value.clone(),
                    proposed_value,
                    reason,
                    status: MigrationStatus::Invalid,
                });
            }
        }
    }
    MigrationReport { entries }
}

enum Clamp {
    /// Значення поза межами — це −1 («авто»), а рушій приймає лише ≥ 0.
    AutoSentinel,
    /// Те саме значення (рядок чи масив рядків), числа обрізано до [min, max].
    Value(Value),
}

/// Числові Int/Float (і їхні вектори) поза [min, max] → найближча межа; форма значення
/// (рядок, масив, число) зберігається. `None` — не числовий тип або не розбирається.
fn clamp_to_range(def: &OptionDef, value: &Value) -> Option<Clamp> {
    let integer = match def.kind.scalar() {
        OptionType::Int => true,
        OptionType::Float => false,
        _ => return None,
    };
    let auto = def.min.is_some_and(|min| min >= 0.);
    let clamp = |n: f64| {
        let n = def.min.map_or(n, |min| n.max(min));
        def.max.map_or(n, |max| n.min(max))
    };
    let text = |n: f64| {
        if integer {
            format!("{}", n as i64)
        } else {
            format!("{n}")
        }
    };
    let atom = |v: &Value| -> Option<Result<Value, ()>> {
        let n = match v {
            Value::String(s) => s.trim().parse::<f64>().ok()?,
            Value::Number(n) => n.as_f64()?,
            _ => return None,
        };
        if auto && n == -1. {
            return Some(Err(()));
        }
        Some(Ok(match v {
            Value::String(_) => Value::String(text(clamp(n))),
            _ if integer => Value::from(clamp(n) as i64),
            _ => Value::from(clamp(n)),
        }))
    };
    let clamped = match value {
        Value::Array(items) => items
            .iter()
            .map(atom)
            .collect::<Option<Result<Vec<_>, ()>>>()?
            .map(Value::Array),
        other => atom(other)?,
    };
    Some(match clamped {
        Ok(value) => Clamp::Value(value),
        Err(()) => Clamp::AutoSentinel,
    })
}
