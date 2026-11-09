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
    /// Частка світла, що проходить крізь пластик: > 0 — прозорий прохід (поглинання за
    /// товщиною, просвічування внутрішніх стінок, кольорова тінь).
    #[serde(default)]
    pub transmission: f32,
    /// Каламутність прозорого (розсіяння в об’ємі): 0 — скло.
    #[serde(default)]
    pub haze: f32,
    /// Товщина (мм), на якій пластик має колір профілю.
    #[serde(default = "absorption_distance")]
    pub absorption_distance: f32,
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
fn absorption_distance() -> f32 {
    ABSORPTION_DISTANCE
}

/// Типова товщина (мм), на якій прозорий пластик має колір профілю: стінка з 2–4 ліній.
pub const ABSORPTION_DISTANCE: f32 = 1.5;
/// Найменша хроматична оптична густина кольору прозорого на `absorption_distance`: світліший
/// (пастельний) колір підсилюється до неї, інакше «Translucent Blue» виглядає сіро-блакитним.
pub const MIN_CHROMA_DENSITY: f32 = 1.2;

/// Колір вкраплень мармуру (sRGB): на світлому філаменті — майже чорний з теплим відтінком,
/// на темному — сірий. Межа — відносна яскравість ~0.3.
pub fn marble_speck(color: [f32; 3]) -> [f32; 3] {
    let luma = 0.2126 * srgb_to_linear(color[0])
        + 0.7152 * srgb_to_linear(color[1])
        + 0.0722 * srgb_to_linear(color[2]);
    if luma > 0.06 {
        [0x2e as f32 / 255.0, 0x2b as f32 / 255.0, 0x29 as f32 / 255.0]
    } else {
        [0xa8 as f32 / 255.0, 0xa4 as f32 / 255.0, 0x9c as f32 / 255.0]
    }
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Коефіцієнт поглинання σₐ (1/мм, лінійні RGB) прозорого пластику кольору `color` (sRGB).
///
/// Колір профілю — пропускання шару товщиною `distance`: σₐ = −ln(колір) / distance
/// (Бугер–Ламберт, T = e^(−σₐ·d)). Оптична густина D = −ln(колір) розкладається на сіру частину
/// (найменша з каналів) і хроматичну (решта). Якщо найбільша хроматична густина менша за
/// [`MIN_CHROMA_DENSITY`] (світлий, пастельний колір), хроматична частина множиться до неї:
/// колір стає насиченішим і трохи темнішим, а відтінок (співвідношення каналів) не змінюється.
/// Білий і сірі кольори лишаються як є (прозорий безбарвний, димчастий).
pub fn absorption(color: [f32; 3], distance: f32) -> [f32; 3] {
    let density = color.map(|c| -srgb_to_linear(c).clamp(0.005, 1.0).ln());
    let gray = density.iter().copied().fold(f32::INFINITY, f32::min);
    let chroma = density.map(|d| d - gray);
    let peak = chroma.iter().copied().fold(0.0, f32::max);
    let boost = if peak > 0.02 { (MIN_CHROMA_DENSITY / peak).max(1.0) } else { 1.0 };
    std::array::from_fn(|i| (gray + chroma[i] * boost) / distance.max(0.05))
}

/// Колір (sRGB) пластику товщиною `distance` з поглинанням `sigma` — те, що бачить шейдер як
/// «колір» прозорого (дифуз, розсіяння).
pub fn transmitted_color(sigma: [f32; 3], distance: f32) -> [f32; 3] {
    sigma.map(|s| linear_to_srgb((-s * distance).exp()))
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
                ("transmission", p.transmission),
                ("haze", p.haze),
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
            anyhow::ensure!(
                (0.05..=50.0).contains(&p.absorption_distance),
                "пресет «{}»: absorption_distance має бути 0.05…50 мм",
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
    /// > 0 — прозорий (окремий прохід). Див. [`Preset::transmission`].
    pub transmission: f32,
    pub haze: f32,
    /// мм, див. [`absorption`].
    pub absorption_distance: f32,
    pub emission: f32,
    pub pattern: Pattern,
    pub pattern_scale: f32,
    pub pattern_strength: f32,
    /// sRGB.
    pub pattern_color: [f32; 3],
}

impl PhotoMaterial {
    pub fn is_transparent(&self) -> bool {
        self.transmission > 0.0
    }

    /// σₐ (1/мм, лінійні RGB) для прозорого; для непрозорого — нулі.
    pub fn absorption(&self) -> [f32; 3] {
        if self.is_transparent() {
            absorption(self.color, self.absorption_distance)
        } else {
            [0.0; 3]
        }
    }

    /// Колір, яким шейдер фарбує поверхню (sRGB): для прозорих — пропускання шару
    /// `absorption_distance` після компенсації пастельних кольорів, інакше колір як є.
    pub fn shading_color(&self) -> [f32; 3] {
        if self.is_transparent() {
            transmitted_color(self.absorption(), self.absorption_distance)
        } else {
            self.color
        }
    }
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
        // Без явного другого кольору: вкраплення мармуру — контрастні (темні на світлому,
        // світлі на темному), блискітки — світліший тон, решта — темніший.
        let pattern_color = p.pattern_color.unwrap_or(match p.pattern {
            Pattern::Marble => marble_speck(color),
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
            transmission: p.transmission,
            haze: p.haze,
            absorption_distance: p.absorption_distance,
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
        assert_eq!(t.presets[t.index_of("marble").unwrap()].pattern_color, None);
        // Розведені за референсами: PETG глянцевий, Basic — легкий глянець, CF і Matte
        // матові; Silk — анізотропний блиск.
        let p = |id: &str| &t.presets[t.index_of(id).unwrap()];
        assert!(p("petg").roughness < p("basic").roughness);
        assert!(p("basic").roughness + 0.4 < p("cf").roughness);
        assert!(p("cf").roughness < p("matte").roughness);
        assert!(p("petg").clearcoat > p("basic").clearcoat);
        assert!(p("silk").anisotropy >= 0.7);
        // Прозорий — лише «Напівпрозорий»: решта малюються як і раніше, непрозорими.
        for preset in &t.presets {
            assert_eq!(preset.transmission > 0.0, preset.id == "translucent", "{}", preset.id);
        }
        assert!(p("translucent").roughness <= p("petg").roughness);
    }

    #[test]
    fn absorption_follows_profile_colour_and_boosts_pastels() {
        let close = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 2e-3);
        // Насичений колір (Bambu PETG Translucent Light Blue #61B0FF) — без змін: на 1.5 мм
        // пропускання дорівнює кольору профілю.
        let blue = parse_hex("#61B0FF").unwrap();
        let sigma = absorption(blue, 1.5);
        assert!(close(transmitted_color(sigma, 1.5), blue), "{:?}", transmitted_color(sigma, 1.5));
        assert_eq!(sigma[2], 0.0);
        // Товще — темніше й насиченіше (Бугер–Ламберт).
        let thick = transmitted_color(sigma, 4.0);
        assert!(thick[0] < blue[0] * 0.5 && thick[1] < blue[1] && thick[2] > 0.999);
        // Пастельний (Ice Blue #B8CDE9) підсилюється: відтінок той самий, синій — синіший.
        let ice = parse_hex("#B8CDE9").unwrap();
        let s = absorption(ice, 1.5);
        let shown = transmitted_color(s, 1.5);
        assert!(shown[0] < ice[0] - 0.1 && shown[2] > shown[1] && shown[1] > shown[0], "{shown:?}");
        let peak = (s[0] - s[2]) * 1.5;
        assert!((peak - MIN_CHROMA_DENSITY).abs() < 1e-3, "{peak}");
        // Білий (Clear) — безбарвний і майже без поглинання; сірий — лише сірий.
        assert!(absorption([1.0; 3], 1.5).iter().all(|&a| a < 1e-6));
        let gray = absorption(parse_hex("#8E8E8E").unwrap(), 1.5);
        assert!((gray[0] - gray[2]).abs() < 1e-6 && gray[0] > 0.5);
        // Непрозорий матеріал: колір як у профілі, поглинання немає.
        let t = MaterialTable::builtin();
        let basic = t.resolve(t.default, ice);
        assert_eq!((basic.shading_color(), basic.absorption()), (ice, [0.0; 3]));
        let clear = t.resolve(t.index_of("translucent").unwrap(), ice);
        assert!(clear.is_transparent() && close(clear.shading_color(), shown));
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
            "default = \"a\"\n[[preset]]\nid = \"a\"\nlabel = \"A\"\nabsorption_distance = 0.0\n",
            "default = \"a\"\n[[preset]]\nid = \"a\"\nlabel = \"A\"\ntransmission = 2.0\n",
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
        // Мармур: вкраплення темні на білому й червоному, світлі на чорному й темно-синьому.
        let marble = t.index_of("marble").unwrap();
        let sum = |c: [f32; 3]| c.iter().sum::<f32>();
        for light in [[1.0; 3], [0.86, 0.15, 0.15], [0.96, 0.85, 0.35]] {
            assert!(sum(t.resolve(marble, light).pattern_color) < 0.6, "{light:?}");
        }
        for dark in [[0.05; 3], [0.05, 0.08, 0.3]] {
            assert!(sum(t.resolve(marble, dark).pattern_color) > 1.8, "{dark:?}");
        }
        let basic = t.resolve(t.default, [0.2, 0.4, 0.6]);
        assert_eq!(basic.color, [0.2, 0.4, 0.6]);
        assert_eq!(parse_hex("#00AE42"), Some([0.0, 174.0 / 255.0, 66.0 / 255.0]));
        assert_eq!(parse_hex("#12"), None);
    }
}
