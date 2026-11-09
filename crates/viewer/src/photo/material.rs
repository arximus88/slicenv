//! Матеріали режиму «Фото»: таблиця пресетів із TOML (`assets/photo-materials.toml`) і
//! вибір пресету за типом і назвою філаменту. Чисті функції — без GPU.

use anyhow::{Context as _, Result};
use serde::Deserialize;

/// Вбудована таблиця пресетів.
pub const BUILTIN_TOML: &str = include_str!("../../assets/photo-materials.toml");

/// Процедурний візерунок поверхні (шум у шейдері, без текстур).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pattern {
    #[default]
    None,
    Marble,
    Sparkle,
    Carbon,
    Wood,
}

impl Pattern {
    /// Номер для шейдера (`photo.wgsl`, `PATTERN_*`).
    pub fn id(self) -> u32 {
        self as u32
    }
}

/// Пресет із TOML. Відсутні числа — нулі, крім `roughness` (0.5) і `specular` (0.5).
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub id: String,
    /// Назва для інтерфейсу (українською; переклад — через `tr()` у застосунку).
    pub label: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub types: Vec<String>,
    #[serde(default = "half")]
    pub roughness: f32,
    #[serde(default)]
    pub metallic: f32,
    #[serde(default = "half")]
    pub specular: f32,
    #[serde(default)]
    pub clearcoat: f32,
    #[serde(default = "tenth")]
    pub clearcoat_roughness: f32,
    #[serde(default)]
    pub sheen: f32,
    #[serde(default)]
    pub anisotropy: f32,
    #[serde(default)]
    pub translucency: f32,
    #[serde(default)]
    pub emission: f32,
    #[serde(default)]
    pub pattern: Pattern,
    #[serde(default = "one")]
    pub pattern_scale: f32,
    #[serde(default)]
    pub pattern_strength: f32,
    #[serde(default, deserialize_with = "hex_opt")]
    pub pattern_color: Option<[f32; 3]>,
    #[serde(default, deserialize_with = "hex_opt")]
    pub tint: Option<[f32; 3]>,
    #[serde(default)]
    pub tint_amount: f32,
}

fn half() -> f32 {
    0.5
}
fn tenth() -> f32 {
    0.1
}
fn one() -> f32 {
    1.0
}

fn hex_opt<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<[f32; 3]>, D::Error> {
    let s = String::deserialize(d)?;
    parse_hex(&s)
        .map(Some)
        .ok_or_else(|| serde::de::Error::custom(format!("колір має бути \"#RRGGBB\", а не {s:?}")))
}

/// "#RRGGBB" → sRGB 0…1.
pub fn parse_hex(s: &str) -> Option<[f32; 3]> {
    let hex = s.trim().trim_start_matches('#');
    if hex.len() < 6 || !hex.is_char_boundary(6) {
        return None;
    }
    let v = u32::from_str_radix(&hex[..6], 16).ok()?;
    Some([
        ((v >> 16) & 0xff) as f32 / 255.0,
        ((v >> 8) & 0xff) as f32 / 255.0,
        (v & 0xff) as f32 / 255.0,
    ])
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TableFile {
    default: String,
    preset: Vec<Preset>,
}

/// Таблиця пресетів у порядку пріоритету й індекс типового.
#[derive(Clone, Debug)]
pub struct MaterialTable {
    pub presets: Vec<Preset>,
    pub default: usize,
}

impl MaterialTable {
    /// Розбір TOML. Помилка — невідомий ключ, повторений `id`, відсутній `default`,
    /// числа поза 0…1 (крім `pattern_scale` > 0).
    pub fn parse(text: &str) -> Result<Self> {
        let file: TableFile = toml::from_str(text).context("таблиця матеріалів «Фото»")?;
        anyhow::ensure!(!file.preset.is_empty(), "таблиця матеріалів порожня");
        for (i, p) in file.preset.iter().enumerate() {
            anyhow::ensure!(
                !file.preset[..i].iter().any(|q| q.id == p.id),
                "пресет «{}» повторюється",
                p.id
            );
            for (name, v) in [
                ("roughness", p.roughness),
                ("metallic", p.metallic),
                ("specular", p.specular),
                ("clearcoat", p.clearcoat),
                ("clearcoat_roughness", p.clearcoat_roughness),
                ("sheen", p.sheen),
                ("anisotropy", p.anisotropy),
                ("translucency", p.translucency),
                ("emission", p.emission),
                ("pattern_strength", p.pattern_strength),
                ("tint_amount", p.tint_amount),
            ] {
                anyhow::ensure!(
                    (0.0..=1.0).contains(&v),
                    "пресет «{}»: {name} = {v} поза 0…1",
                    p.id
                );
            }
            anyhow::ensure!(
                p.pattern_scale > 0.0 && p.pattern_scale.is_finite(),
                "пресет «{}»: pattern_scale має бути > 0",
                p.id
            );
        }
        let default = file
            .preset
            .iter()
            .position(|p| p.id == file.default)
            .with_context(|| format!("типового пресету «{}» немає в таблиці", file.default))?;
        Ok(Self {
            presets: file.preset,
            default,
        })
    }

    /// Вбудована таблиця (перевіряється тестом, тож не падає).
    pub fn builtin() -> Self {
        Self::parse(BUILTIN_TOML).expect("вбудована таблиця матеріалів")
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.presets.iter().position(|p| p.id == id)
    }

    /// Пресет для філаменту: ключове слово в назві чи типі → тип → типовий.
    pub fn select(&self, filament_type: &str, name: &str) -> usize {
        let words: Vec<String> = words(name).chain(words(filament_type)).collect();
        if let Some(i) = self
            .presets
            .iter()
            .position(|p| p.keywords.iter().any(|k| words.iter().any(|w| keyword_matches(w, k))))
        {
            return i;
        }
        let kind = filament_type.trim().to_ascii_uppercase();
        let base = kind.split(['-', ' ', '+']).next().unwrap_or("").to_owned();
        if !kind.is_empty()
            && let Some(i) = self.presets.iter().position(|p| {
                p.types
                    .iter()
                    .any(|t| t.eq_ignore_ascii_case(&kind) || t.eq_ignore_ascii_case(&base))
            })
        {
            return i;
        }
        self.default
    }
}

/// Слова назви в нижньому регістрі: «Bambu PLA Silk+ @BBL A1» → bambu, pla, silk, bbl, a1.
fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
}

fn keyword_matches(word: &str, keyword: &str) -> bool {
    let keyword = keyword.to_lowercase();
    word == keyword || (keyword.chars().count() >= 4 && word.starts_with(&keyword))
}

/// Готові параметри матеріалу слота (колір уже лінійний). Те, що бачить шейдер.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhotoMaterial {
    /// sRGB, як `filament_colour`.
    pub color: [f32; 3],
    pub roughness: f32,
    pub metallic: f32,
    pub specular: f32,
    pub clearcoat: f32,
    pub clearcoat_roughness: f32,
    pub sheen: f32,
    pub anisotropy: f32,
    pub translucency: f32,
    pub emission: f32,
    pub pattern: Pattern,
    pub pattern_scale: f32,
    pub pattern_strength: f32,
    /// sRGB.
    pub pattern_color: [f32; 3],
}

impl Default for PhotoMaterial {
    fn default() -> Self {
        MaterialTable::builtin().resolve(MaterialTable::builtin().default, [0.8, 0.8, 0.8])
    }
}

impl MaterialTable {
    /// Параметри пресету `index` для кольору філаменту `color` (sRGB).
    pub fn resolve(&self, index: usize, color: [f32; 3]) -> PhotoMaterial {
        let p = &self.presets[index.min(self.presets.len() - 1)];
        let color = match p.tint {
            Some(t) => std::array::from_fn(|i| color[i] + (t[i] - color[i]) * p.tint_amount),
            None => color,
        };
        // Без явного другого кольору: прожилки — темніший тон філаменту, блискітки — світліший.
        let pattern_color = p.pattern_color.unwrap_or(match p.pattern {
            Pattern::Sparkle => color.map(|c| 0.55 + c * 0.45),
            _ => color.map(|c| c * 0.4),
        });
        PhotoMaterial {
            color,
            roughness: p.roughness,
            metallic: p.metallic,
            specular: p.specular,
            clearcoat: p.clearcoat,
            clearcoat_roughness: p.clearcoat_roughness,
            sheen: p.sheen,
            anisotropy: p.anisotropy,
            translucency: p.translucency,
            emission: p.emission,
            pattern: p.pattern,
            pattern_scale: p.pattern_scale,
            pattern_strength: p.pattern_strength,
            pattern_color,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(table: &MaterialTable, kind: &str, name: &str) -> String {
        table.presets[table.select(kind, name)].id.clone()
    }

    #[test]
    fn builtin_table_parses_and_covers_required_families() {
        let t = MaterialTable::builtin();
        for family in [
            "basic", "matte", "silk", "petg", "translucent", "marble", "sparkle", "glow", "wood",
            "metal", "cf", "tpu", "abs", "pa",
        ] {
            assert!(t.index_of(family).is_some(), "немає пресету {family}");
        }
        assert_eq!(t.presets[t.default].id, "basic");
        assert_eq!(t.presets[t.index_of("marble").unwrap()].pattern, Pattern::Marble);
        assert_eq!(
            t.presets[t.index_of("marble").unwrap()].pattern_color,
            Some([0x2e as f32 / 255.0, 0x2b as f32 / 255.0, 0x29 as f32 / 255.0])
        );
        // Розведені за референсами: PETG глянцевий, Basic — легкий глянець, CF і Matte
        // матові; Silk — анізотропний блиск.
        let p = |id: &str| &t.presets[t.index_of(id).unwrap()];
        assert!(p("petg").roughness < p("basic").roughness);
        assert!(p("basic").roughness + 0.4 < p("cf").roughness);
        assert!(p("cf").roughness < p("matte").roughness);
        assert!(p("petg").clearcoat > p("basic").clearcoat);
        assert!(p("silk").anisotropy >= 0.7);
    }

    #[test]
    fn preset_follows_name_keywords_then_type_then_default() {
        let t = MaterialTable::builtin();
        assert_eq!(id(&t, "PLA", "Bambu PLA Basic @BBL A1"), "basic");
        assert_eq!(id(&t, "PLA", "Bambu PLA Matte @BBL X1C"), "matte");
        assert_eq!(id(&t, "PLA", "Bambu PLA Silk+ @BBL A1"), "silk");
        assert_eq!(id(&t, "PETG", "Bambu PETG HF @BBL A1"), "petg");
        // Ключове слово сильніше за тип: прозорий PETG — не звичайний PETG.
        assert_eq!(id(&t, "PETG", "Bambu PETG Translucent @BBL X1C"), "translucent");
        assert_eq!(id(&t, "PLA", "Bambu PLA Marble @BBL A1"), "marble");
        assert_eq!(id(&t, "PLA", "Bambu PLA Sparkle @BBL A1"), "sparkle");
        assert_eq!(id(&t, "PLA", "Bambu PLA Galaxy @BBL A1"), "sparkle");
        assert_eq!(id(&t, "PLA", "Bambu PLA Glow @BBL A1"), "glow");
        assert_eq!(id(&t, "PLA", "Bambu PLA Wood @BBL A1"), "wood");
        assert_eq!(id(&t, "PLA", "Bambu PLA Metal @BBL A1"), "metal");
        assert_eq!(id(&t, "PLA-CF", "Bambu PLA-CF @BBL X1C"), "cf");
        // CF лише в типі.
        assert_eq!(id(&t, "PA6-CF", "Generic PA"), "cf");
        assert_eq!(id(&t, "TPU", "Bambu TPU 95A HF @BBL A1"), "tpu");
        assert_eq!(id(&t, "ABS", "Bambu ABS @BBL A1"), "abs");
        assert_eq!(id(&t, "ASA", "Generic ASA"), "abs");
        assert_eq!(id(&t, "PA", "Generic PA"), "pa");
        assert_eq!(id(&t, "PAHT", "Bambu PAHT"), "pa");
        // Тип без збігу назви; тип із суфіксом порівнюється й за основою.
        assert_eq!(id(&t, "PLA", "Generic PLA"), "basic");
        assert_eq!(id(&t, "petg", ""), "petg");
        assert_eq!(id(&t, "PLA-S", "Bambu Support W"), "basic");
        // Невідомий тип і назва — типовий пресет.
        assert_eq!(id(&t, "PVA", "Mystery"), "basic");
        assert_eq!(id(&t, "", ""), "basic");
        // Частина слова не збігається з коротким ключовим словом: «pace» ≠ pa, «abstract» ≠ abs.
        assert_eq!(id(&t, "", "Space Abstract"), "basic");
        // Довге ключове слово — і як префікс слова.
        assert_eq!(id(&t, "", "Sparkles Red"), "sparkle");
    }

    #[test]
    fn toml_errors_are_reported() {
        let ok = "default = \"a\"\n[[preset]]\nid = \"a\"\nlabel = \"A\"\n";
        let t = MaterialTable::parse(ok).unwrap();
        assert_eq!(t.presets[0].roughness, 0.5);
        assert_eq!(t.presets[0].specular, 0.5);
        assert_eq!(t.presets[0].pattern, Pattern::None);
        assert_eq!(t.select("PLA", "x"), 0);
        for bad in [
            "default = \"b\"\n[[preset]]\nid = \"a\"\nlabel = \"A\"\n",
            "default = \"a\"\n[[preset]]\nid = \"a\"\nlabel = \"A\"\nroughness = 1.5\n",
            "default = \"a\"\n[[preset]]\nid = \"a\"\nlabel = \"A\"\nshine = 1\n",
            "default = \"a\"\n[[preset]]\nid = \"a\"\nlabel = \"A\"\npattern = \"plaid\"\n",
            "default = \"a\"\n[[preset]]\nid = \"a\"\nlabel = \"A\"\npattern_color = \"red\"\n",
            "default = \"a\"\n[[preset]]\nid = \"a\"\nlabel = \"A\"\n[[preset]]\nid = \"a\"\nlabel = \"B\"\n",
            "default = \"a\"\npreset = []\n",
        ] {
            assert!(MaterialTable::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn resolve_applies_tint_and_derived_pattern_color() {
        let t = MaterialTable::builtin();
        let wood = t.resolve(t.index_of("wood").unwrap(), [1.0, 1.0, 1.0]);
        assert!(wood.color[2] < 1.0 && wood.color[0] > wood.color[2]);
        let sparkle = t.resolve(t.index_of("sparkle").unwrap(), [0.0, 0.0, 0.2]);
        assert!(sparkle.pattern_color.iter().all(|&c| c >= 0.55));
        let basic = t.resolve(t.default, [0.2, 0.4, 0.6]);
        assert_eq!(basic.color, [0.2, 0.4, 0.6]);
        assert_eq!(parse_hex("#00AE42"), Some([0.0, 174.0 / 255.0, 66.0 / 255.0]));
        assert_eq!(parse_hex("#12"), None);
    }
}
