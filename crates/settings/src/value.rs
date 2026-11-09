use std::fmt;

use serde_json::Value;

use crate::{OptionDef, OptionType, Schema};

#[derive(Clone, Debug, PartialEq)]
pub enum TypedValue {
    Nil,
    Float(f64),
    Integer(i64),
    Bool(bool),
    String(String),
    Enum(String),
    Percent(f64),
    Point(Vec<f64>),
    PointGroups(Vec<Vec<Vec<f64>>>),
    Vector(Vec<TypedValue>),
    /// Невідомі Bambu-ключі зберігаємо без втрат.
    Unknown(Value),
}

/// Типізоване значення разом з оригінальним записом Bambu.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedValue {
    typed: TypedValue,
    wire: Value,
}

impl ParsedValue {
    pub fn typed(&self) -> &TypedValue {
        &self.typed
    }

    pub fn to_json(&self) -> Value {
        self.wire.clone()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationError {
    pub key: String,
    pub message: String,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.key, self.message)
    }
}

impl std::error::Error for ValidationError {}

type Result<T> = std::result::Result<T, ValidationError>;

fn error(key: &str, message: impl Into<String>) -> ValidationError {
    ValidationError {
        key: key.into(),
        message: message.into(),
    }
}

impl Schema {
    pub fn format(&self, key: &str, value: &TypedValue) -> Result<Value> {
        let def = self
            .options
            .get(key)
            .ok_or_else(|| error(key, "unknown option"))?;
        check_kind(def.kind, def.vector, value).map_err(|message| error(key, message))?;
        let wire = if let TypedValue::PointGroups(groups) = value {
            Value::Array(
                groups
                    .iter()
                    .map(|group| {
                        Value::String(
                            group
                                .iter()
                                .map(|coords| {
                                    coords
                                        .iter()
                                        .map(f64::to_string)
                                        .collect::<Vec<_>>()
                                        .join("x")
                                })
                                .collect::<Vec<_>>()
                                .join(","),
                        )
                    })
                    .collect(),
            )
        } else if let TypedValue::Point(coords) = value
            && matches!(def.kind, OptionType::Point | OptionType::Point3)
        {
            Value::String(
                coords
                    .iter()
                    .map(f64::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            )
        } else if let TypedValue::Vector(values) = value {
            Value::Array(
                values
                    .iter()
                    .map(atom_string)
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|message| error(key, message))?
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            )
        } else if def.vector {
            Value::Array(vec![Value::String(
                atom_string(value).map_err(|message| error(key, message))?,
            )])
        } else {
            Value::String(atom_string(value).map_err(|message| error(key, message))?)
        };
        self.validate(key, &wire)?;
        Ok(wire)
    }

    pub fn parse(&self, key: &str, wire: &Value) -> Result<ParsedValue> {
        let typed = match self.options.get(key) {
            Some(def) => parse(def, wire).map_err(|message| error(key, message))?,
            None => TypedValue::Unknown(wire.clone()),
        };
        Ok(ParsedValue {
            typed,
            wire: wire.clone(),
        })
    }

    pub fn validate(&self, key: &str, wire: &Value) -> Result<ParsedValue> {
        let _profile = crate::profile::Span::new("settings::validate");

        let def = self
            .options
            .get(key)
            .ok_or_else(|| error(key, "unknown option"))?;
        let parsed = self.parse(key, wire)?;
        validate(def, parsed.typed()).map_err(|message| error(key, message))?;
        Ok(parsed)
    }
}

fn parse(def: &OptionDef, wire: &Value) -> std::result::Result<TypedValue, String> {
    if def.kind == OptionType::PointsGroups {
        let groups: Vec<&str> = match wire {
            Value::Array(items) => items
                .iter()
                .map(|item| item.as_str().ok_or("expected polygon strings"))
                .collect::<std::result::Result<_, _>>()?,
            Value::String(s) => s.split('#').collect(),
            _ => return Err("expected polygon strings".into()),
        };
        let groups = groups
            .into_iter()
            .map(|group| {
                group
                    .split(',')
                    .filter(|p| !p.is_empty())
                    .map(|point| {
                        match parse_atom(OptionType::Point, &Value::String(point.trim().into()))? {
                            TypedValue::Point(coords) => Ok(coords),
                            _ => Err("expected a point".into()),
                        }
                    })
                    .collect::<std::result::Result<Vec<_>, String>>()
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        return Ok(TypedValue::PointGroups(groups));
    }
    if let Value::Array(items) = wire {
        if !def.vector {
            return Err("expected a scalar string, got an array".into());
        }
        return items
            .iter()
            .map(|v| parse_atom(def.kind.scalar(), v))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map(TypedValue::Vector);
    }
    if def.vector {
        let s = wire
            .as_str()
            .ok_or("expected a string or an array of strings")?;
        // Полігони трапляються також одним рядком з комами.
        if def.kind == OptionType::Points && s != "nil" {
            return s
                .split(',')
                .filter(|s| !s.is_empty())
                .map(|s| parse_atom(OptionType::Point, &Value::String(s.trim().into())))
                .collect::<std::result::Result<Vec<_>, _>>()
                .map(TypedValue::Vector);
        }
        return Ok(TypedValue::Vector(vec![parse_atom(
            def.kind.scalar(),
            wire,
        )?]));
    }
    parse_atom(def.kind, wire)
}

fn parse_atom(kind: OptionType, wire: &Value) -> std::result::Result<TypedValue, String> {
    let s = wire.as_str().ok_or("Bambu values must be strings")?;
    if s == "nil" && kind != OptionType::String {
        return Ok(TypedValue::Nil);
    }
    let number = |s: &str| -> std::result::Result<f64, String> {
        let n: f64 = s
            .parse()
            .map_err(|_| format!("expected a finite number, got {s:?}"))?;
        if n.is_finite() {
            Ok(n)
        } else {
            Err("number must be finite".into())
        }
    };
    Ok(match kind {
        OptionType::String => TypedValue::String(s.into()),
        OptionType::Enum => TypedValue::Enum(s.into()),
        OptionType::Int => TypedValue::Integer(i64::from(
            s.parse::<i32>()
                .map_err(|_| format!("expected a 32-bit integer, got {s:?}"))?,
        )),
        OptionType::Bool => TypedValue::Bool(match s {
            "0" => false,
            "1" => true,
            _ => return Err(format!("expected 0 or 1, got {s:?}")),
        }),
        OptionType::Percent | OptionType::FloatOrPercent if s.ends_with('%') => {
            TypedValue::Percent(number(&s[..s.len() - 1])?)
        }
        OptionType::Float | OptionType::Percent | OptionType::FloatOrPercent => {
            let n = number(s)?;
            if kind == OptionType::Percent {
                TypedValue::Percent(n)
            } else {
                TypedValue::Float(n)
            }
        }
        OptionType::Point | OptionType::Point3 => {
            let coords = s
                .split(if s.contains('x') { 'x' } else { ',' })
                .map(|coordinate| number(coordinate.trim()))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let expected = if kind == OptionType::Point3 { 3 } else { 2 };
            if coords.len() != expected {
                return Err(format!("expected {expected} coordinates separated by x"));
            }
            TypedValue::Point(coords)
        }
        _ => return Err(format!("unsupported scalar type: {kind:?}")),
    })
}

fn check_kind(
    kind: OptionType,
    vector: bool,
    value: &TypedValue,
) -> std::result::Result<(), String> {
    if vector {
        if let TypedValue::Vector(values) = value {
            for item in values {
                check_kind(kind.scalar(), false, item)?;
            }
            return Ok(());
        }
        return check_kind(kind.scalar(), false, value);
    }
    let valid = matches!(
        (kind, value),
        (_, TypedValue::Nil)
            | (
                OptionType::Float | OptionType::FloatOrPercent,
                TypedValue::Float(_)
            )
            | (OptionType::Int, TypedValue::Integer(_))
            | (OptionType::Bool, TypedValue::Bool(_))
            | (OptionType::String, TypedValue::String(_))
            | (OptionType::Enum, TypedValue::Enum(_))
            | (
                OptionType::Percent | OptionType::FloatOrPercent,
                TypedValue::Percent(_)
            )
            | (OptionType::Point | OptionType::Point3, TypedValue::Point(_))
            | (OptionType::PointsGroups, TypedValue::PointGroups(_))
    );
    if valid {
        Ok(())
    } else {
        Err(format!("value does not match {kind:?}"))
    }
}

fn atom_string(value: &TypedValue) -> std::result::Result<String, String> {
    Ok(match value {
        TypedValue::Nil => "nil".into(),
        TypedValue::Float(n) => n.to_string(),
        TypedValue::Integer(n) => n.to_string(),
        TypedValue::Bool(b) => if *b { "1" } else { "0" }.into(),
        TypedValue::String(s) | TypedValue::Enum(s) => s.clone(),
        TypedValue::Percent(n) => format!("{n}%"),
        TypedValue::Point(coords) => coords
            .iter()
            .map(f64::to_string)
            .collect::<Vec<_>>()
            .join("x"),
        _ => return Err("expected a scalar value".into()),
    })
}

fn validate(def: &OptionDef, value: &TypedValue) -> std::result::Result<(), String> {
    match value {
        TypedValue::Vector(values) => {
            for (index, item) in values.iter().enumerate() {
                validate(def, item).map_err(|message| format!("item {index}: {message}"))?;
            }
        }
        TypedValue::Nil if !def.nullable => {
            return Err("nil is only allowed for nullable options".into());
        }
        TypedValue::Enum(value)
            if !def.enum_values.is_empty() && !def.enum_values.contains(value) =>
        {
            return Err(format!(
                "{value:?} is not one of: {}",
                def.enum_values.join(", ")
            ));
        }
        TypedValue::Float(n) | TypedValue::Percent(n) => bounds(def, *n)?,
        TypedValue::Integer(n) => bounds(def, *n as f64)?,
        _ => {}
    }
    Ok(())
}

fn bounds(def: &OptionDef, n: f64) -> std::result::Result<(), String> {
    if let Some(min) = def.min
        && n < min
    {
        return Err(format!("{n} is below minimum {min}"));
    }
    if let Some(max) = def.max
        && n > max
    {
        return Err(format!("{n} exceeds maximum {max}"));
    }
    Ok(())
}
