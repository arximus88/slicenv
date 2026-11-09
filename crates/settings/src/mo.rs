//! Каталоги перекладів рушія (gettext `.mo`, `resources/i18n/<мова>/*.mo`). Ключ —
//! англійський текст з `PrintConfig.cpp`, той самий, що в нашій схемі, тож підписи,
//! підказки й категорії перекладаються словами самого рушія.
use std::{collections::HashMap, path::Path};

use anyhow::{Context, Result, ensure};

use crate::{Page, Schema};

#[derive(Debug, Default)]
pub struct Catalog {
    strings: HashMap<String, String>,
}

impl Catalog {
    /// Розбирає бінарний `.mo`. Множина → перша форма; `контекст\x04текст` доступний і за
    /// самим текстом, якщо безконтекстного запису немає.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() >= 20, "закороткий файл перекладу");
        let little = match bytes[..4] {
            [0xde, 0x12, 0x04, 0x95] => true,
            [0x95, 0x04, 0x12, 0xde] => false,
            _ => anyhow::bail!("це не каталог gettext"),
        };
        let word = |at: usize| -> Result<usize> {
            let raw: [u8; 4] = bytes
                .get(at..at + 4)
                .context("обрізаний каталог перекладу")?
                .try_into()?;
            Ok(if little { u32::from_le_bytes(raw) } else { u32::from_be_bytes(raw) } as usize)
        };
        let text = |table: usize, index: usize| -> Result<&str> {
            let entry = table + index * 8;
            let (len, offset) = (word(entry)?, word(entry + 4)?);
            let raw = bytes
                .get(offset..offset + len)
                .context("рядок поза межами каталогу")?;
            Ok(std::str::from_utf8(raw).unwrap_or_default())
        };
        let (count, originals, translations) = (word(8)?, word(12)?, word(16)?);
        ensure!(count < 1_000_000, "завеликий каталог перекладу");
        let mut strings = HashMap::with_capacity(count);
        let mut contextual = Vec::new();
        for i in 0..count {
            let (msgid, msgstr) = (text(originals, i)?, text(translations, i)?);
            let msgid = msgid.split('\0').next().unwrap_or_default();
            let msgstr = msgstr.split('\0').next().unwrap_or_default();
            if msgid.is_empty() || msgstr.is_empty() {
                continue; // заголовок каталогу або неперекладений рядок
            }
            match msgid.split_once('\x04') {
                Some((_, bare)) => contextual.push((bare.to_owned(), msgstr.to_owned())),
                None => {
                    strings.insert(msgid.to_owned(), msgstr.to_owned());
                }
            }
        }
        for (bare, msgstr) in contextual {
            strings.entry(bare).or_insert(msgstr);
        }
        Ok(Self { strings })
    }

    pub fn merge(&mut self, other: Catalog) {
        for (k, v) in other.strings {
            self.strings.entry(k).or_insert(v);
        }
    }

    pub fn len(&self) -> usize {
        self.strings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }

    pub fn get(&self, english: &str) -> Option<&str> {
        if english.is_empty() {
            return None;
        }
        self.strings.get(english).map(String::as_str)
    }
}

/// Каталоги мови `lang` поруч із виконуваним файлом рушія: `<bin>/resources/i18n/<lang>`,
/// `<bin>/../resources/…` (AppImage, portable zip) і `Contents/Resources` на macOS.
pub fn engine_catalog(binary: &Path, lang: &str) -> Catalog {
    let binary = std::fs::canonicalize(binary).unwrap_or_else(|_| binary.to_owned());
    let mut roots = Vec::new();
    if let Some(dir) = binary.parent() {
        roots.push(dir.join("resources"));
        if let Some(parent) = dir.parent() {
            roots.push(parent.join("resources"));
            roots.push(parent.join("Resources"));
        }
    }
    let mut catalog = Catalog::default();
    for dir in roots.iter().map(|r| r.join("i18n").join(lang)) {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut files: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "mo"))
            .collect();
        files.sort();
        for file in files {
            match std::fs::read(&file).map_err(anyhow::Error::from).and_then(|b| Catalog::parse(&b)) {
                Ok(found) => catalog.merge(found),
                Err(e) => eprintln!("[i18n] {}: {e:#}", file.display()),
            }
        }
        if !catalog.is_empty() {
            break;
        }
    }
    catalog
}

impl Schema {
    /// Доповнює українські поля перекладами рушія. Наявні (вивірені) переклади не
    /// чіпаються; чого немає в каталозі — лишається англійською. Повертає кількість
    /// заповнених полів.
    pub fn fill_ukrainian(&mut self, catalog: &Catalog) -> usize {
        fn fill(slot: &mut String, english: &str, catalog: &Catalog, filled: &mut usize) {
            if slot.is_empty()
                && let Some(text) = catalog.get(english)
            {
                *slot = text.to_owned();
                *filled += 1;
            }
        }
        let mut filled = 0;
        for def in self.options.values_mut() {
            let uk = &mut def.uk;
            fill(&mut uk.label, &def.label, catalog, &mut filled);
            fill(&mut uk.full_label, &def.full_label, catalog, &mut filled);
            fill(&mut uk.tooltip, &def.tooltip, catalog, &mut filled);
            fill(&mut uk.sidetext, &def.sidetext, catalog, &mut filled);
            fill(&mut uk.category, &def.category, catalog, &mut filled);
            if uk.enum_labels.len() < def.enum_labels.len() {
                uk.enum_labels.resize(def.enum_labels.len(), String::new());
            }
            for (slot, english) in uk.enum_labels.iter_mut().zip(&def.enum_labels) {
                fill(slot, english, catalog, &mut filled);
            }
        }
        for page in self.layout.values_mut().flatten() {
            let Page { title, title_uk, groups, .. } = page;
            fill(title_uk, title, catalog, &mut filled);
            for group in groups {
                fill(&mut group.title_uk, &group.title, catalog, &mut filled);
            }
        }
        filled
    }
}

impl Schema {
    /// Прогалини після каталогу рушія — з вивіреного перекладу іншої схеми за тим самим
    /// ключем. Підпис береться завжди (нова Orca перейменувала «Initial layer height» на
    /// «First layer height», каталог 2.3 знає лише стару назву); підказка, одиниці й
    /// варіанти — лише коли англійський текст збігається, щоб не описати чужу поведінку.
    pub fn fill_ukrainian_from(&mut self, other: &Schema) -> usize {
        let mut filled = 0;
        let mut take = |slot: &mut String, ours: &str, theirs: &str, uk: &str, by_key: bool| {
            if slot.is_empty() && !uk.is_empty() && !ours.is_empty() && (by_key || ours == theirs) {
                *slot = uk.to_owned();
                filled += 1;
            }
        };
        for (key, def) in &mut self.options {
            let Some(src) = other.options.get(key) else { continue };
            let uk = &mut def.uk;
            take(&mut uk.label, &def.label, &src.label, &src.uk.label, true);
            take(&mut uk.full_label, &def.full_label, &src.full_label, &src.uk.full_label, true);
            take(&mut uk.tooltip, &def.tooltip, &src.tooltip, &src.uk.tooltip, false);
            take(&mut uk.sidetext, &def.sidetext, &src.sidetext, &src.uk.sidetext, false);
            take(&mut uk.category, &def.category, &src.category, &src.uk.category, false);
            if def.enum_values == src.enum_values {
                uk.enum_labels.resize(def.enum_labels.len().max(uk.enum_labels.len()), String::new());
                for (i, slot) in uk.enum_labels.iter_mut().enumerate() {
                    let (ours, theirs) = (def.enum_labels.get(i), src.enum_labels.get(i));
                    let translated = src.uk.enum_labels.get(i);
                    if let (Some(ours), Some(theirs), Some(translated)) = (ours, theirs, translated) {
                        take(slot, ours, theirs, translated, false);
                    }
                }
            }
        }
        filled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Мінімальний `.mo` (little-endian), як його пише msgfmt.
    fn mo(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut sorted = entries.to_vec();
        sorted.sort();
        let n = sorted.len();
        let (orig, trans) = (28, 28 + n * 8);
        let mut data = Vec::new();
        let mut offsets = (Vec::new(), Vec::new());
        let base = 28 + n * 16;
        for (id, _) in &sorted {
            offsets.0.push((id.len(), base + data.len()));
            data.extend_from_slice(id.as_bytes());
            data.push(0);
        }
        for (_, s) in &sorted {
            offsets.1.push((s.len(), base + data.len()));
            data.extend_from_slice(s.as_bytes());
            data.push(0);
        }
        let mut out = Vec::new();
        for w in [0x950412de_u32, 0, n as u32, orig as u32, trans as u32, 0, 0] {
            out.extend_from_slice(&w.to_le_bytes());
        }
        for (len, off) in offsets.0.iter().chain(&offsets.1) {
            out.extend_from_slice(&(*len as u32).to_le_bytes());
            out.extend_from_slice(&(*off as u32).to_le_bytes());
        }
        out.extend_from_slice(&data);
        out
    }

    #[test]
    fn parses_plain_plural_and_context_entries() {
        let bytes = mo(&[
            ("", "Content-Type: text/plain; charset=UTF-8\n"),
            ("Wall loops", "Кількість периметрів"),
            ("%d layer\0%d layers", "%d шар\0%d шари\0%d шарів"),
            ("Quality\x04Top", "Верх"),
            ("Untranslated", ""),
        ]);
        let catalog = Catalog::parse(&bytes).unwrap();
        assert_eq!(catalog.get("Wall loops"), Some("Кількість периметрів"));
        assert_eq!(catalog.get("%d layer"), Some("%d шар"));
        assert_eq!(catalog.get("Top"), Some("Верх"));
        assert_eq!(catalog.get("Untranslated"), None);
        assert_eq!(catalog.get(""), None);
        assert_eq!(catalog.len(), 3);
        assert!(Catalog::parse(b"not a catalog at all").is_err());
        assert!(Catalog::parse(&bytes[..30]).is_err());
    }

    #[test]
    fn orca_schema_gets_engine_words_without_overwriting_curated_ones() {
        let mut schema = Schema::load_orca().unwrap();
        let def = &schema.options["wall_loops"];
        let (label, tooltip) = (def.label.clone(), def.tooltip.clone());
        let page = schema.layout.values().flatten().next().unwrap().title.clone();
        let catalog = Catalog::parse(&mo(&[
            (&label, "Периметри"),
            (&tooltip, "Підказка"),
            (&page, "Сторінка"),
        ]))
        .unwrap();
        assert!(schema.fill_ukrainian(&catalog) >= 3);
        let def = &schema.options["wall_loops"];
        assert_eq!(def.label(crate::Locale::Uk), "Периметри");
        assert_eq!(def.tooltip(crate::Locale::Uk), "Підказка");
        assert_eq!(def.label(crate::Locale::En), label);

        let mut bambu = Schema::load().unwrap();
        let (key, curated) = bambu
            .options
            .iter()
            .find(|(_, d)| !d.uk.label.is_empty())
            .map(|(k, d)| (k.clone(), d.uk.label.clone()))
            .unwrap();
        let english = bambu.options[&key].label.clone();
        bambu.fill_ukrainian(&Catalog::parse(&mo(&[(&english, "інше")])).unwrap());
        assert_eq!(bambu.options[&key].uk.label, curated);
    }

    #[test]
    fn renamed_orca_labels_fall_back_to_the_bambu_translation_by_key() {
        let mut orca = Schema::load_orca().unwrap();
        let bambu = Schema::load().unwrap();
        assert!(orca.options["initial_layer_print_height"].uk.label.is_empty());
        assert!(orca.fill_ukrainian_from(&bambu) > 0);
        let def = &orca.options["initial_layer_print_height"];
        assert_eq!(def.uk.label, bambu.options["initial_layer_print_height"].uk.label);
        assert_eq!(def.label(crate::Locale::En), "First layer height");
    }

    /// `SLICENV_TEST_ENGINE=<шлях до бінарника> SLICENV_TEST_KIND=orca|bambu cargo test -p settings
    /// real_engine -- --ignored --nocapture` — покриття підписів реальним каталогом рушія.
    #[test]
    #[ignore]
    fn real_engine_catalog_covers_most_labels() {
        let binary = std::path::PathBuf::from(std::env::var("SLICENV_TEST_ENGINE").unwrap());
        let mut schema = match std::env::var("SLICENV_TEST_KIND").as_deref() {
            Ok("bambu") => Schema::load().unwrap(),
            _ => Schema::load_orca().unwrap(),
        };
        let catalog = engine_catalog(&binary, "uk");
        let filled = schema.fill_ukrainian(&catalog);
        let labelled = schema.options.values().filter(|d| !d.label.is_empty()).count();
        let uk = schema.options.values().filter(|d| !d.uk.label.is_empty()).count();
        let tips = schema.options.values().filter(|d| !d.tooltip.is_empty()).count();
        let uk_tips = schema.options.values().filter(|d| !d.uk.tooltip.is_empty()).count();
        let pages: Vec<_> = schema.layout.values().flatten().map(|p| p.title(crate::Locale::Uk)).collect();
        println!(
            "каталог {} рядків; заповнено {filled}; підписи {uk}/{labelled}; підказки {uk_tips}/{tips}; сторінки {pages:?}",
            catalog.len()
        );
        for key in ["wall_loops", "top_shell_layers", "sparse_infill_density"] {
            if let Some(d) = schema.options.get(key) {
                println!("{key}: {} | {}", d.label(crate::Locale::Uk), d.category(crate::Locale::Uk));
            }
        }
        assert!(uk * 2 > labelled, "перекладено менше половини підписів");
    }
}
