//! App labels are source-keyed; engine metadata independently selects En/Uk.
//! Extra UTF-8 packs live in $XDG_CONFIG_HOME/slicenv/locales/<code>.json.
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{OnceLock, RwLock},
};

#[derive(Default)]
struct Current {
    code: String,
    strings: HashMap<String, String>,
}
fn current() -> &'static RwLock<Current> {
    static CURRENT: OnceLock<RwLock<Current>> = OnceLock::new();
    CURRENT.get_or_init(|| {
        RwLock::new(Current {
            code: "uk".into(),
            strings: HashMap::new(),
        })
    })
}
fn english() -> &'static HashMap<String, String> {
    static EN: OnceLock<HashMap<String, String>> = OnceLock::new();
    EN.get_or_init(|| {
        serde_json::from_str(include_str!("../locales/en.json"))
            .expect("valid embedded English catalog")
    })
}
fn pack_root() -> PathBuf {
    paths::config_dir().join("locales")
}
fn valid_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 35
        && code
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}
fn read_pack(code: &str) -> anyhow::Result<HashMap<String, String>> {
    anyhow::ensure!(valid_code(code), "Invalid language code");
    Ok(serde_json::from_slice(&std::fs::read(
        pack_root().join(format!("{code}.json")),
    )?)?)
}
pub fn set_language(code: &str) -> anyhow::Result<()> {
    let strings = match code {
        "uk" | "en" => HashMap::new(),
        _ => read_pack(code)?,
    };
    *current().write().unwrap() = Current {
        code: code.into(),
        strings,
    };
    Ok(())
}
pub fn locale() -> settings::Locale {
    if current().read().unwrap().code == "uk" {
        settings::Locale::Uk
    } else {
        settings::Locale::En
    }
}
pub fn tr(source: &str) -> String {
    let selected = current().read().unwrap();
    if selected.code == "uk" {
        return source.into();
    }
    selected
        .strings
        .get(source)
        .or_else(|| english().get(source))
        .map_or_else(|| source.into(), Clone::clone)
}
pub fn languages() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![
        ("uk".into(), "Українська".into()),
        ("en".into(), "English".into()),
    ];
    if let Ok(entries) = std::fs::read_dir(pack_root()) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|v| v != "json") {
                continue;
            }
            let Some(code) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if matches!(code, "uk" | "en") || !valid_code(code) {
                continue;
            }
            if let Ok(pack) = read_pack(code) {
                out.push((
                    code.into(),
                    pack.get("_language_name")
                        .cloned()
                        .unwrap_or_else(|| code.into()),
                ));
            }
        }
    }
    out[2..].sort_by(|a, b| a.0.cmp(&b.0));
    out
}

pub fn plate_title(plate: &viewer::Plate) -> String {
    if plate.number == 0 {
        return tr("Поза пластинами");
    }
    match &plate.name {
        Some(name) => format!("{} · {name}", plate.number),
        None => crate::tr_format!("Пластина {}", plate.number),
    }
}

// Extract already-formatted placeholders so Rust keeps formatting precision and
// implicit captures. Only declared UI templates use this path; user values are
// never passed through dictionary matching. Unsupported templates stay intact.
#[derive(Debug)]
enum Token {
    Text(String),
    Field(String),
}
fn tokens(template: &str) -> Option<Vec<Token>> {
    let mut chars = template.chars().peekable();
    let mut text = String::new();
    let mut out = Vec::new();
    let mut index = 0;
    while let Some(c) = chars.next() {
        if c == '{' {
            if chars.peek() == Some(&'{') {
                chars.next();
                text.push('{');
                continue;
            }
            if !text.is_empty() {
                out.push(Token::Text(std::mem::take(&mut text)));
            }
            let mut field = String::new();
            loop {
                match chars.next() {
                    Some('}') => break,
                    Some('{') | None => return None,
                    Some(c) => field.push(c),
                }
            }
            let name = field.split(':').next()?;
            let key = if name.is_empty() {
                let key = index.to_string();
                index += 1;
                key
            } else {
                name.into()
            };
            out.push(Token::Field(key));
        } else if c == '}' {
            if chars.next() != Some('}') {
                return None;
            }
            text.push('}');
        } else {
            text.push(c);
        }
    }
    if !text.is_empty() {
        out.push(Token::Text(text));
    }
    Some(out)
}
fn format_into(source: &str, translated: &str, rendered: &str) -> Option<String> {
    let source = tokens(source)?;
    let target = tokens(translated)?;
    let mut values = HashMap::new();
    let mut rest = rendered;
    for (i, token) in source.iter().enumerate() {
        match token {
            Token::Text(text) => rest = rest.strip_prefix(text)?,
            Token::Field(key) => {
                let end = match source.get(i + 1) {
                    Some(Token::Text(next)) => rest.find(next)?,
                    Some(Token::Field(_)) => return None,
                    None => rest.len(),
                };
                let value = &rest[..end];
                if values.insert(key, value).is_some_and(|old| old != value) {
                    return None;
                }
                rest = &rest[end..];
            }
        }
    }
    if !rest.is_empty() {
        return None;
    }
    let mut result = String::new();
    for token in target {
        match token {
            Token::Text(text) => result.push_str(&text),
            Token::Field(key) => result.push_str(values.get(&key)?),
        }
    }
    Some(result)
}
pub fn formatted(source: &str, rendered: &str) -> String {
    let translated = tr(source);
    if translated == source {
        rendered.into()
    } else {
        format_into(source, &translated, rendered).unwrap_or_else(|| rendered.into())
    }
}
/// Переклад уже відформатованого тексту з інших крейтів (помилки `cloud`, `lan`): точний ключ
/// або шаблон з `{полями}` зі словника. Не знайшлося — текст лишається як є.
pub fn tr_rendered(rendered: &str) -> String {
    let selected = current().read().unwrap();
    if selected.code == "uk" {
        return rendered.into();
    }
    [&selected.strings, english()]
        .into_iter()
        .find_map(|strings| strings.get(rendered).cloned().or_else(|| match_template(strings, rendered)))
        .unwrap_or_else(|| rendered.into())
}
/// Найдовший шаблон словника, під який підходить текст. Шаблони майже без сталого тексту
/// (`{}`, `{a}: {b}`) не розглядаються — вони підійшли б до будь-чого.
fn match_template(strings: &HashMap<String, String>, rendered: &str) -> Option<String> {
    strings
        .iter()
        .filter(|(source, _)| source.contains('{') && source.chars().count() >= 24)
        .filter_map(|(source, translated)| Some((source.len(), format_into(source, translated, rendered)?)))
        .max_by_key(|(len, _)| *len)
        .map(|(_, text)| text)
}
/// Ланцюжок помилки мовою інтерфейсу: кожна ланка перекладається окремо.
pub fn tr_error(error: &anyhow::Error) -> String {
    error.chain().map(|link| tr_rendered(&link.to_string())).collect::<Vec<_>>().join(": ")
}
#[macro_export]
macro_rules! tr_format {
    ($template:literal $(, $($args:tt)*)?) => {{
        $crate::i18n::formatted($template, &format!($template $(, $($args)*)?))
    }};
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rendered_errors_match_the_longest_template_and_ignore_bare_fields() {
        let strings: HashMap<String, String> = [
            ("{}", "{}"),
            ("Камера через хмару недоступна{detail}. Перевірте принтер.", "Camera unavailable{detail}. Check the printer."),
        ]
        .into_iter()
        .map(|(a, b)| (a.to_owned(), b.to_owned()))
        .collect();
        assert_eq!(
            match_template(&strings, "Камера через хмару недоступна (Bambu 403). Перевірте принтер.").unwrap(),
            "Camera unavailable (Bambu 403). Check the printer."
        );
        assert_eq!(match_template(&strings, "Невідомий текст"), None);
    }
    #[test]
    fn templates_preserve_values_precision_and_escaped_braces() {
        assert_eq!(
            format_into(
                "Шар {n} · {z:.2} мм",
                "Layer {n} · {z:.2} mm",
                "Шар 12 · 0.25 мм"
            )
            .unwrap(),
            "Layer 12 · 0.25 mm"
        );
        assert_eq!(
            format_into("{count} з {}", "{} of {count}", "5 з 20").unwrap(),
            "20 of 5"
        );
        assert_eq!(
            format_into("{{{name}}}", "Name: {name}", "{my Ukrainian файл}").unwrap(),
            "Name: my Ukrainian файл"
        );
        assert!(format_into("{a}{b}", "{b}{a}", "ab").is_none());
        assert!(format_into("{x}", "{unknown}", "user filename").is_none());
        assert_eq!(
            format_into("Модуль {other:02X}", "Module {other:02X}", "Модуль 0A").unwrap(),
            "Module 0A"
        );
    }
    #[test]
    fn embedded_catalog_and_language_paths_are_valid() {
        assert_eq!(english()["Головна"], "Home");
        for invalid in ["../en", "/tmp/en", "", "a/b", "."] {
            assert!(!valid_code(invalid));
        }
        assert!(valid_code("pt-BR"));
    }
}
