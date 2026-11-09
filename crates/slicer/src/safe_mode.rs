//! Сумісний режим нарізки: Slicenv підлаштовується під рушій, а не вимагає оновлення.
//!
//! Старі збірки CLI Bambu/Orca аварійно завершуються (на Windows — 0xC0000005) не через модель,
//! а через код інтерфейсу, якого в консольному режимі немає. Перевірено на OrcaSlicer 2.3.0
//! (Windows 11, Radeon 780M):
//! - назва пластини (`plater_name` у `Metadata/model_settings.config`) → `PartPlate::set_plate_name`
//!   → `generate_plate_name_texture()` звертається до `m_plater`, якого в CLI немає (null);
//!   upstream OrcaSlicer#7920;
//! - `--export-3mf` без готових мініатюр (STL, `--arrange 1`) → рендер мініатюр через OpenGL
//!   у `render_thumbnail`; upstream OrcaSlicer#16010/#16013. G-code на той момент уже записано.
//!
//! Тому при аварії Slicenv повторює нарізку в безпечнішій [`Stage`] і запам'ятовує стадію для
//! цього рушія (шлях + версія + розмір + час зміни, тобто `EngineInfo::identity`).
use anyhow::{Context as _, Result};
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read as _, Seek as _, SeekFrom, Write as _},
    path::{Path, PathBuf},
};

/// Стадії від повної до найобережнішої. Кожна наступна вмикає все з попередньої.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    /// Як Bambu Studio: рушій сам пакує `.gcode.3mf` (`--export-3mf`).
    Normal,
    /// Копія 3MF без назв пластин; `--export-3mf` лишається.
    PlainPlates,
    /// Лише G-code (`--slice`/`--outputdir`), без `--export-3mf` і `--pipe`; `.gcode.3mf`
    /// збирає Slicenv, час і вагу бере з G-code.
    GcodeOnly,
    /// Копія 3MF без налаштувань проєкту: `project_settings.config` замінено конфігом із
    /// профілів Slicenv (ті самі значення, що в `--load-settings`), вбудовані пресети й
    /// `slice_info.config` прибрано. Геометрія, пластини й налаштування окремих об’єктів
    /// (`model_settings.config`) лишаються. Саме прибрати конфіг не можна: OrcaSlicer 2.4.2 і
    /// Bambu Studio 2.7 CLI без нього падають (SIGSEGV) навіть із `--load-settings`.
    NoProjectSettings,
}

impl Stage {
    pub fn next(self) -> Option<Self> {
        match self {
            Self::Normal => Some(Self::PlainPlates),
            Self::PlainPlates => Some(Self::GcodeOnly),
            Self::GcodeOnly => Some(Self::NoProjectSettings),
            Self::NoProjectSettings => None,
        }
    }
    fn key(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::PlainPlates => "plain_plates",
            Self::GcodeOnly => "gcode_only",
            Self::NoProjectSettings => "no_project_settings",
        }
    }
    fn from_key(key: &str) -> Option<Self> {
        [
            Self::Normal,
            Self::PlainPlates,
            Self::GcodeOnly,
            Self::NoProjectSettings,
        ]
        .into_iter()
            .find(|s| s.key() == key)
    }
}

/// Тихе повідомлення користувачу: не помилка, нарізка вдалася.
pub const COMPATIBLE_NOTE: &str =
    "Рушій нестабільний у звичайному режимі — нарізано в сумісному режимі";

/// Нарізано без налаштувань проєкту з 3MF ([`Stage::NoProjectSettings`]): чесно, що не застосовано.
pub const NO_PROJECT_SETTINGS_NOTE: &str = "Рушій аварійно завершувався на налаштуваннях проєкту з 3MF, тому нарізано без них: застосовано лише вибрані профілі принтера, процесу й філаменту разом із вашими змінами. Не застосовано значення проєкту, яких немає в цих профілях (кольори й типи філаментів по слотах, об’єми промивання, положення вежі очищення, вбудовані в 3MF користувацькі пресети) — для них узято значення профілів або типові значення рушія. Геометрію, пластини й налаштування окремих об’єктів збережено.";

/// Що відомо про рушій: з якої стадії починати і чи падав він навіть у найобережнішій.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EngineCompat {
    pub stage: Option<Stage>,
    /// Аварія і на останній стадії: лише тоді — порада перейти на інший рушій.
    pub crashes_in_safe_mode: bool,
}

/// Пам'ять стадій (`cache_dir()/engine-compat.json`): `identity` → запис.
pub(crate) struct Memory {
    path: PathBuf,
}

impl Memory {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
    pub fn default_path() -> PathBuf {
        crate::cli::cache_dir().join("engine-compat.json")
    }
    fn read(&self) -> BTreeMap<String, Value> {
        fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
    pub fn get(&self, identity: &str) -> EngineCompat {
        let all = self.read();
        let Some(entry) = all.get(identity) else {
            return EngineCompat::default();
        };
        EngineCompat {
            stage: entry["stage"].as_str().and_then(Stage::from_key),
            crashes_in_safe_mode: entry["crashes_in_safe_mode"].as_bool().unwrap_or(false),
        }
    }
    pub fn set(&self, identity: &str, compat: &EngineCompat, reason: &str) -> Result<()> {
        let mut all = self.read();
        if *compat == EngineCompat::default() {
            all.remove(identity);
        } else {
            all.insert(
                identity.to_owned(),
                json!({
                    "stage": compat.stage.map(Stage::key),
                    "crashes_in_safe_mode": compat.crashes_in_safe_mode,
                    "reason": reason,
                }),
            );
        }
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = self
            .path
            .with_extension(format!("tmp{}", std::process::id()));
        fs::write(&tmp, serde_json::to_vec_pretty(&all)?)?;
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// Що відомо про рушій з цим `identity` (для налаштувань рушія в застосунку).
pub fn engine_compat(identity: &str) -> EngineCompat {
    Memory::new(Memory::default_path()).get(identity)
}

/// Чи падав цей виконуваний файл навіть у сумісному режимі — без запуску рушія: `identity`
/// ([`crate::EngineInfo::identity`]) — «вид:шлях:версія:розмір:час зміни», версію з `--help`
/// тут не знаємо, тож звіряємо решту.
pub fn engine_crashes_in_safe_mode(engine: &crate::Engine) -> bool {
    Memory::new(Memory::default_path()).crashes_for(engine)
}

impl Memory {
    fn crashes_for(&self, engine: &crate::Engine) -> bool {
        let canonical = fs::canonicalize(&engine.path).unwrap_or_else(|_| engine.path.clone());
        let metadata = fs::metadata(&engine.path).ok();
        let prefix = format!("{}:{}:", engine.kind.name(), canonical.display());
        let suffix = format!(
            ":{}:{:?}",
            metadata.as_ref().map(|m| m.len()).unwrap_or(0),
            metadata.and_then(|m| m.modified().ok()),
        );
        self.read().iter().any(|(identity, entry)| {
            identity.starts_with(&prefix)
                && identity.ends_with(&suffix)
                && entry["crashes_in_safe_mode"].as_bool() == Some(true)
        })
    }
}

/// Копія 3MF без `plater_name` у `Metadata/model_settings.config` (решта — без перепакування).
/// `None` — назв немає або це не 3MF.
pub(crate) fn without_plate_names(input: &Path, dir: &Path) -> Result<Option<PathBuf>> {
    const SETTINGS: &str = "Metadata/model_settings.config";
    let Ok(file) = fs::File::open(input) else {
        return Ok(None);
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return Ok(None);
    };
    let settings = match archive.by_name(SETTINGS) {
        Ok(mut file) => {
            let mut text = String::new();
            file.read_to_string(&mut text)?;
            text
        }
        Err(_) => return Ok(None),
    };
    let stripped = strip_metadata(&settings, "plater_name");
    if stripped == settings {
        return Ok(None);
    }
    fs::create_dir_all(dir)?;
    let output = dir.join(input.file_name().unwrap_or("project.3mf".as_ref()));
    let mut writer = zip::ZipWriter::new(fs::File::create(&output)?);
    for i in 0..archive.len() {
        let file = archive.by_index_raw(i)?;
        if file.name().is_ok_and(|n| n == SETTINGS) {
            drop(file);
            writer.start_file(
                SETTINGS,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated),
            )?;
            writer.write_all(stripped.as_bytes())?;
        } else {
            writer.raw_copy_file(file)?;
        }
    }
    writer.finish()?;
    Ok(Some(output))
}

/// Прибирає елементи `<metadata key="{key}" … />` (з відступом рядка), решту тексту не чіпає.
fn strip_metadata(xml: &str, key: &str) -> String {
    let needle = format!("<metadata key=\"{key}\"");
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(start) = rest.find(&needle) {
        let Some(len) = rest[start..].find("/>") else {
            break;
        };
        // Відступ перед елементом теж іде, щоб не лишати порожніх рядків.
        let head = &rest[..start];
        let trimmed = head.trim_end_matches([' ', '\t']);
        let trimmed = trimmed.strip_suffix('\n').unwrap_or(trimmed);
        let trimmed = trimmed.strip_suffix('\r').unwrap_or(trimmed);
        out.push_str(trimmed);
        rest = &rest[start + len + 2..];
    }
    out.push_str(rest);
    out
}

const PROJECT_SETTINGS: &str = "Metadata/project_settings.config";

/// Записи 3MF з налаштуваннями принтера, процесу й філаменту, а не з моделлю: конфіг проєкту,
/// вбудовані користувацькі пресети (`*_settings_N.config`) і відомості про нарізку з принтером.
/// `model_settings.config` (об’єкти, модифікатори, пластини) сюди не входить.
fn is_project_settings_entry(name: &str) -> bool {
    let Some(file) = name.strip_prefix("Metadata/") else {
        return false;
    };
    if name == PROJECT_SETTINGS || file == "slice_info.config" {
        return true;
    }
    ["process_settings_", "filament_settings_", "machine_settings_", "print_settings_"]
        .iter()
        .any(|prefix| {
            file.strip_prefix(prefix)
                .and_then(|rest| rest.strip_suffix(".config"))
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
}

/// Чи є в 3MF налаштування проєкту, які замінює [`without_project_settings`].
pub(crate) fn has_project_settings(input: &Path) -> bool {
    fs::File::open(input)
        .ok()
        .and_then(|f| zip::ZipArchive::new(f).ok())
        .is_some_and(|a| a.file_names().any(|n| n.is_ok_and(|n| is_project_settings_entry(&n))))
}

/// Копія 3MF без налаштувань проєкту ([`Stage::NoProjectSettings`]): `project_settings.config`
/// — це `profiles` (розгорнуті профілі Slicenv із правками), вбудовані пресети й
/// `slice_info.config` прибрано, решта записів — без перепакування. `None` — це не 3MF або
/// налаштувань проєкту в ньому немає.
pub(crate) fn without_project_settings(
    input: &Path,
    dir: &Path,
    profiles: &Map<String, Value>,
) -> Result<Option<PathBuf>> {
    let Ok(file) = fs::File::open(input) else {
        return Ok(None);
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return Ok(None);
    };
    if !archive
        .file_names()
        .any(|n| n.is_ok_and(|n| is_project_settings_entry(&n)))
    {
        return Ok(None);
    }
    fs::create_dir_all(dir)?;
    let output = dir.join(input.file_name().unwrap_or("project.3mf".as_ref()));
    let mut writer = zip::ZipWriter::new(fs::File::create(&output)?);
    let mut dropped = Vec::new();
    for i in 0..archive.len() {
        let file = archive.by_index_raw(i)?;
        let name = file.name()?.into_owned();
        if is_project_settings_entry(&name) {
            dropped.push(name);
        } else {
            writer.raw_copy_file(file)?;
        }
    }
    writer.start_file(
        PROJECT_SETTINGS,
        zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated),
    )?;
    writer.write_all(&serde_json::to_vec_pretty(profiles)?)?;
    writer.finish()?;
    eprintln!(
        "[slice] нарізка без налаштувань проєкту: конфіг — з профілів, прибрано {}",
        dropped.join(", ")
    );
    Ok(Some(output))
}

/// Відомості, які в звичайному режимі дають `result.json` і `slice_info.config`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct GcodeSummary {
    pub print_time: Option<f64>,
    pub filament_grams: Option<f64>,
    pub layers: Option<u32>,
    pub layer_height: Option<f64>,
    /// Витрата по слотах філаменту (мм і г), як `; filament used [mm] = a, b`.
    pub filament_mm: Vec<f64>,
    pub filament_g: Vec<f64>,
}

/// Шапка (`; HEADER_BLOCK`) і хвіст G-code Bambu/Orca; файл великий, тож читаємо лише краї.
pub(crate) fn summarize_gcode(path: &Path) -> Result<GcodeSummary> {
    const HEAD: u64 = 64 * 1024;
    const TAIL: u64 = 1024 * 1024;
    let mut file = fs::File::open(path).with_context(|| format!("немає {}", path.display()))?;
    let len = file.metadata()?.len();
    let mut head = vec![0; HEAD.min(len) as usize];
    file.read_exact(&mut head)?;
    let mut tail = Vec::new();
    if len > HEAD {
        file.seek(SeekFrom::Start(len.saturating_sub(TAIL).max(HEAD)))?;
        file.read_to_end(&mut tail)?;
    }
    let text = String::from_utf8_lossy(&head).into_owned() + &String::from_utf8_lossy(&tail);
    Ok(parse_summary(&text))
}

fn parse_summary(text: &str) -> GcodeSummary {
    let mut s = GcodeSummary::default();
    let mut footer_time = None;
    for line in text.lines() {
        let Some(comment) = line.trim().strip_prefix(';') else {
            continue;
        };
        let comment = comment.trim();
        if comment.contains("total estimated time:") {
            for part in comment.split(';') {
                if let Some(v) = part.trim().strip_prefix("total estimated time:") {
                    s.print_time = s.print_time.or(parse_duration(v));
                }
            }
        } else if let Some(v) = comment.strip_prefix("estimated printing time (normal mode) =") {
            footer_time = footer_time.or(parse_duration(v));
        } else if let Some(v) = comment.strip_prefix("total layer number:") {
            s.layers = s.layers.or(v.trim().parse().ok());
        } else if let Some(v) = comment.strip_prefix("total filament weight [g] :") {
            s.filament_grams = s.filament_grams.or(sum(v));
            if s.filament_g.is_empty() {
                s.filament_g = list(v);
            }
        } else if let Some(v) = comment.strip_prefix("total filament length [mm] :") {
            if s.filament_mm.is_empty() {
                s.filament_mm = list(v);
            }
        } else if let Some(v) = comment.strip_prefix("filament used [g] =") {
            s.filament_grams = s.filament_grams.or(sum(v));
            if s.filament_g.is_empty() {
                s.filament_g = list(v);
            }
        } else if let Some(v) = comment.strip_prefix("filament used [mm] =") {
            if s.filament_mm.is_empty() {
                s.filament_mm = list(v);
            }
        } else if let Some(v) = comment.strip_prefix("total filament used [g] =") {
            s.filament_grams = s.filament_grams.or(sum(v));
        } else if let Some(v) = comment.strip_prefix("layer_height =") {
            s.layer_height = s.layer_height.or(v.trim().parse().ok());
        }
    }
    s.print_time = s.print_time.or(footer_time);
    s
}

fn list(text: &str) -> Vec<f64> {
    text.split(',')
        .filter_map(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .collect()
}

fn sum(list: &str) -> Option<f64> {
    let values: Option<Vec<f64>> = list
        .split(',')
        .map(|v| v.trim().parse::<f64>().ok())
        .collect();
    values
        .map(|v| v.iter().sum::<f64>())
        .filter(|v| v.is_finite() && *v >= 0.0)
}

/// «1d 2h 3m 4s» / «5h 18m 52s» / «59s» → секунди.
fn parse_duration(text: &str) -> Option<f64> {
    let mut total = 0.0;
    let mut any = false;
    for part in text.split_whitespace() {
        let (number, unit) = part.split_at(part.find(|c: char| c.is_ascii_alphabetic())?);
        let number: f64 = number.parse().ok()?;
        total += number
            * match unit {
                "d" => 86400.0,
                "h" => 3600.0,
                "m" => 60.0,
                "s" => 1.0,
                _ => return None,
            };
        any = true;
    }
    any.then_some(total)
}

/// Що відомо про принтер і філаменти для `slice_info.config` (Bambu Connect і принтер
/// читають звідти модель, сопло, філаменти й прогноз).
#[derive(Clone, Debug, Default)]
pub struct PackageMeta {
    /// `model_id` моделі принтера (A1 — «N2S»); порожній, якщо невідомий (так пише й Orca 2.5).
    pub printer_model_id: String,
    /// Кольори слотів `#RRGGBB` з проєкту; бракує — беремо з профілю або білий.
    pub filament_colours: Vec<String>,
}

/// Мініатюри та опис пластини, які Slicenv рендерить сам, якщо рушій їх не дав.
#[derive(Clone, Debug, Default)]
pub struct PlateAssets {
    /// `plate_N.png` (512×512), `plate_N_small.png`, `plate_no_light_N.png`, `top_N.png`, `pick_N.png`.
    pub plate: Option<Vec<u8>>,
    pub small: Option<Vec<u8>>,
    pub no_light: Option<Vec<u8>>,
    pub top: Option<Vec<u8>>,
    pub pick: Option<Vec<u8>>,
    /// `plate_N.json` (рамка об’єктів, тип столу, кольори) — як пише Bambu Studio.
    pub json: Option<Vec<u8>>,
}

impl PlateAssets {
    fn files(&self, plate: u32) -> Vec<(String, &[u8])> {
        [
            (format!("Metadata/plate_{plate}.png"), &self.plate),
            (format!("Metadata/plate_{plate}_small.png"), &self.small),
            (format!("Metadata/plate_no_light_{plate}.png"), &self.no_light),
            (format!("Metadata/top_{plate}.png"), &self.top),
            (format!("Metadata/pick_{plate}.png"), &self.pick),
            (format!("Metadata/plate_{plate}.json"), &self.json),
        ]
        .into_iter()
        .filter_map(|(name, data)| data.as_deref().map(|d| (name, d)))
        .collect()
    }
}

/// Версія формату Bambu, яку пише й OrcaSlicer 2.5 (`X-BBL-Client-Version`, `Application`).
const BBL_FORMAT_VERSION: &str = "02.08.01.55";

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn first_str(settings: &Map<String, Value>, key: &str) -> Option<String> {
    match settings.get(key)? {
        Value::Array(items) => items.first()?.as_str().map(str::to_owned),
        Value::String(s) => s.split(';').next().map(str::to_owned),
        _ => None,
    }
}

fn strings(settings: &Map<String, Value>, key: &str) -> Vec<String> {
    match settings.get(key) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        Some(Value::String(s)) => s.split(';').map(str::to_owned).collect(),
        _ => Vec::new(),
    }
}

fn truthy(settings: &Map<String, Value>, key: &str) -> bool {
    first_str(settings, key).is_some_and(|v| v == "1" || v == "true")
        || settings.get(key).and_then(Value::as_bool) == Some(true)
}

/// `slice_info.config` у форматі Bambu Studio / OrcaSlicer 2.5.
fn slice_info_xml(
    plate: u32,
    summary: &GcodeSummary,
    settings: &Map<String, Value>,
    meta: &PackageMeta,
) -> String {
    let nozzle = first_str(settings, "nozzle_diameter").unwrap_or_else(|| "0.4".into());
    let types = strings(settings, "filament_type");
    // Проєкт Bambu пише `filament_ids`, пресет філаменту — `filament_id` («GFA00»).
    let ids = Some(strings(settings, "filament_ids"))
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| strings(settings, "filament_id"));
    let profile_colours = strings(settings, "filament_colour");
    let slots = types
        .len()
        .max(summary.filament_g.len())
        .max(meta.filament_colours.len().min(1))
        .max(1);
    let mut x = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<config>\n  <header>\n");
    x += "    <header_item key=\"X-BBL-Client-Type\" value=\"slicer\"/>\n";
    x += &format!("    <header_item key=\"X-BBL-Client-Version\" value=\"{BBL_FORMAT_VERSION}\"/>\n");
    x += &format!(
        "    <header_item key=\"Slicenv-Version\" value=\"{}\"/>\n  </header>\n  <plate>\n",
        env!("CARGO_PKG_VERSION")
    );
    let mut meta_line = |key: &str, value: &str| {
        x += &format!(
            "    <metadata key=\"{key}\" value=\"{}\"/>\n",
            xml_escape(value)
        );
    };
    meta_line("index", &plate.to_string());
    meta_line("extruder_type", "0");
    meta_line("nozzle_volume_type", "0");
    meta_line("printer_model_id", &meta.printer_model_id);
    meta_line("nozzle_diameters", &nozzle);
    meta_line("timelapse_type", "0");
    if let Some(time) = summary.print_time {
        meta_line("prediction", &format!("{}", time.round()));
    }
    if let Some(weight) = summary.filament_grams {
        meta_line("weight", &format!("{weight:.2}"));
    }
    meta_line("outside", "false");
    meta_line(
        "support_used",
        if truthy(settings, "enable_support") { "true" } else { "false" },
    );
    meta_line("label_object_enabled", "true");
    meta_line("filament_maps", &vec!["1"; slots].join(" "));
    for slot in 0..slots {
        let used_g = summary.filament_g.get(slot).copied().unwrap_or(if slots == 1 {
            summary.filament_grams.unwrap_or(0.0)
        } else {
            0.0
        });
        // Слоти без витрати Bambu Studio не перелічує.
        if used_g <= 0.0 && slots > 1 {
            continue;
        }
        let used_m = summary.filament_mm.get(slot).copied().unwrap_or(0.0) / 1000.0;
        let colour = meta
            .filament_colours
            .get(slot)
            .or(profile_colours.get(slot))
            .cloned()
            .filter(|c| c.starts_with('#'))
            .unwrap_or_else(|| "#FFFFFF".into());
        x += &format!(
            "    <filament id=\"{}\" tray_info_idx=\"{}\" type=\"{}\" color=\"{}\" used_m=\"{used_m:.2}\" used_g=\"{used_g:.2}\" group_id=\"0\" nozzle_diameter=\"{}\" volume_type=\"Standard\" used_for_object=\"true\" used_for_support=\"false\"/>\n",
            slot + 1,
            xml_escape(ids.get(slot).map_or("", String::as_str)),
            xml_escape(types.get(slot).map_or("PLA", String::as_str)),
            xml_escape(&colour),
            xml_escape(&nozzle),
        );
    }
    x += &format!(
        "    <nozzle id=\"0\" extruder_id=\"1\" nozzle_diameter=\"{}\" volume_type=\"Standard\"/>\n",
        xml_escape(&nozzle)
    );
    if let Some(layers) = summary.layers.filter(|l| *l > 0) {
        x += &format!(
            "    <layer_filament_lists>\n      <layer_filament_list filament_list=\"0\" layer_ranges=\"0 {}\" />\n    </layer_filament_lists>\n",
            layers - 1
        );
    }
    x += "  </plate>\n</config>\n";
    x
}

/// `model_settings.config`: пластина з посиланнями на G-code і мініатюри.
fn model_settings_xml(plate: u32, with_thumbnails: bool) -> String {
    let mut x = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<config>\n  <plate>\n    <metadata key=\"plater_id\" value=\"{plate}\"/>\n    <metadata key=\"plater_name\" value=\"\"/>\n    <metadata key=\"locked\" value=\"false\"/>\n    <metadata key=\"filament_map_mode\" value=\"Auto For Flush\"/>\n    <metadata key=\"filament_maps\" value=\"1\"/>\n    <metadata key=\"gcode_file\" value=\"Metadata/plate_{plate}.gcode\"/>\n"
    );
    if with_thumbnails {
        x += &format!(
            "    <metadata key=\"thumbnail_file\" value=\"Metadata/plate_{plate}.png\"/>\n    <metadata key=\"thumbnail_no_light_file\" value=\"Metadata/plate_no_light_{plate}.png\"/>\n    <metadata key=\"top_file\" value=\"Metadata/top_{plate}.png\"/>\n    <metadata key=\"pick_file\" value=\"Metadata/pick_{plate}.png\"/>\n"
        );
    }
    x += "  </plate>\n</config>\n";
    x
}

fn root_rels_xml(plate: u32, with_thumbnails: bool) -> String {
    let mut x = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\n <Relationship Target=\"/3D/3dmodel.model\" Id=\"rel-1\" Type=\"http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel\"/>\n");
    if with_thumbnails {
        x += &format!(" <Relationship Target=\"/Metadata/plate_{plate}.png\" Id=\"rel-2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail\"/>\n <Relationship Target=\"/Metadata/plate_{plate}.png\" Id=\"rel-4\" Type=\"http://schemas.bambulab.com/package/2021/cover-thumbnail-middle\"/>\n <Relationship Target=\"/Metadata/plate_{plate}_small.png\" Id=\"rel-5\" Type=\"http://schemas.bambulab.com/package/2021/cover-thumbnail-small\"/>\n");
    }
    x += "</Relationships>\n";
    x
}

fn model_xml(plate: u32, with_thumbnails: bool) -> String {
    let mut x = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<model unit=\"millimeter\" xml:lang=\"en-US\" xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\" xmlns:BambuStudio=\"http://schemas.bambulab.com/package/2021\">\n <metadata name=\"Application\">BambuStudio-{BBL_FORMAT_VERSION}</metadata>\n <metadata name=\"Slicenv\">{}</metadata>\n <metadata name=\"BambuStudio:3mfVersion\">1</metadata>\n", env!("CARGO_PKG_VERSION"));
    if with_thumbnails {
        x += &format!(" <metadata name=\"Thumbnail_Middle\">/Metadata/plate_{plate}.png</metadata>\n <metadata name=\"Thumbnail_Small\">/Metadata/plate_{plate}_small.png</metadata>\n");
    }
    x += " <resources/>\n <build/>\n</model>\n";
    x
}

const CONTENT_TYPES: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
 <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
 <Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/>
 <Default Extension="png" ContentType="image/png"/>
 <Default Extension="json" ContentType="application/json"/>
 <Default Extension="gcode" ContentType="text/x.gcode"/>
</Types>
"#;

/// `.gcode.3mf` як від Bambu Studio / OrcaSlicer 2.5: G-code з `.md5`, `slice_info.config`
/// (модель, сопло, філаменти, прогноз), `model_settings.config`, налаштування, rels і типи.
/// Мініатюри додає [`attach_plate_assets`] — їх рендерить застосунок.
pub(crate) fn package_gcode(
    gcode: &Path,
    output: &Path,
    plate: u32,
    summary: &GcodeSummary,
    settings: &Map<String, Value>,
    meta: &PackageMeta,
) -> Result<()> {
    use md5::Digest as _;
    let data = fs::read(gcode)?;
    let md5: String = md5::Md5::digest(&data)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect();
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let tmp = output.with_extension("partial");
    let mut zip = zip::ZipWriter::new(fs::File::create(&tmp)?);
    let put = |zip: &mut zip::ZipWriter<fs::File>, name: &str, bytes: &[u8]| -> Result<()> {
        zip.start_file(name, options)?;
        zip.write_all(bytes)?;
        Ok(())
    };
    put(&mut zip, "[Content_Types].xml", CONTENT_TYPES)?;
    put(&mut zip, "_rels/.rels", root_rels_xml(plate, false).as_bytes())?;
    put(&mut zip, "3D/3dmodel.model", model_xml(plate, false).as_bytes())?;
    put(&mut zip, &format!("Metadata/plate_{plate}.gcode"), &data)?;
    put(&mut zip, &format!("Metadata/plate_{plate}.gcode.md5"), md5.as_bytes())?;
    put(
        &mut zip,
        "Metadata/slice_info.config",
        slice_info_xml(plate, summary, settings, meta).as_bytes(),
    )?;
    put(
        &mut zip,
        "Metadata/model_settings.config",
        model_settings_xml(plate, false).as_bytes(),
    )?;
    put(
        &mut zip,
        "Metadata/_rels/model_settings.config.rels",
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\n <Relationship Target=\"/Metadata/plate_{plate}.gcode\" Id=\"rel-1\" Type=\"http://schemas.bambulab.com/package/2021/gcode\"/>\n</Relationships>\n").as_bytes(),
    )?;
    put(
        &mut zip,
        "Metadata/filament_sequence.json",
        format!("{{\"plate_{plate}\":{{\"nozzle_sequence\":[0],\"optimal_assignment\":[0],\"sequence\":[1]}}}}").as_bytes(),
    )?;
    put(
        &mut zip,
        "Metadata/project_settings.config",
        &serde_json::to_vec_pretty(settings)?,
    )?;
    zip.finish()?;
    fs::rename(&tmp, output)?;
    Ok(())
}

/// Додає мініатюри й `plate_N.json` до пакета, який зібрав Slicenv (сумісний режим), і
/// оновлює посилання на них. Архів рушія не чіпає: там мініатюри вже є.
pub fn attach_plate_assets(archive: &Path, plate: u32, assets: &PlateAssets) -> Result<()> {
    let files = assets.files(plate);
    if files.is_empty() {
        return Ok(());
    }
    anyhow::ensure!(
        is_slicenv_package(archive),
        "мініатюри додаються лише до пакета, зібраного Slicenv"
    );
    let mut source = zip::ZipArchive::new(fs::File::open(archive)?)?;
    let with_thumbnails = assets.plate.is_some();
    let replaced = [
        "_rels/.rels".to_owned(),
        "3D/3dmodel.model".to_owned(),
        "Metadata/model_settings.config".to_owned(),
    ];
    let tmp = archive.with_extension("partial");
    let mut zip = zip::ZipWriter::new(fs::File::create(&tmp)?);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for i in 0..source.len() {
        let entry = source.by_index_raw(i)?;
        let name = entry.name()?.into_owned();
        if replaced.contains(&name) || files.iter().any(|(n, _)| *n == name) {
            continue;
        }
        zip.raw_copy_file(entry)?;
    }
    for (name, bytes) in &files {
        zip.start_file(name.as_str(), options)?;
        zip.write_all(bytes)?;
    }
    zip.start_file("_rels/.rels", options)?;
    zip.write_all(root_rels_xml(plate, with_thumbnails).as_bytes())?;
    zip.start_file("3D/3dmodel.model", options)?;
    zip.write_all(model_xml(plate, with_thumbnails).as_bytes())?;
    zip.start_file("Metadata/model_settings.config", options)?;
    zip.write_all(model_settings_xml(plate, with_thumbnails).as_bytes())?;
    zip.finish()?;
    drop(source);
    fs::rename(&tmp, archive)?;
    Ok(())
}

/// Чи це пакет, який Slicenv зібрав сам (сумісний режим) — тоді мініатюри малює застосунок.
pub fn is_slicenv_package(archive: &Path) -> bool {
    (|| {
        let mut zip = zip::ZipArchive::new(fs::File::open(archive).ok()?).ok()?;
        let mut text = String::new();
        zip.by_name("3D/3dmodel.model").ok()?.read_to_string(&mut text).ok()?;
        Some(text.contains("<metadata name=\"Slicenv\">"))
    })()
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_escalate_and_are_remembered_per_engine() {
        let dir = tempfile::tempdir().unwrap();
        let memory = Memory::new(dir.path().join("compat.json"));
        assert_eq!(memory.get("orca:a"), EngineCompat::default());
        let compat = EngineCompat {
            stage: Some(Stage::GcodeOnly),
            crashes_in_safe_mode: false,
        };
        memory.set("orca:a", &compat, "0xC0000005").unwrap();
        assert_eq!(memory.get("orca:a"), compat);
        assert_eq!(memory.get("orca:b"), EngineCompat::default());
        memory.set("orca:a", &EngineCompat::default(), "").unwrap();
        assert_eq!(memory.get("orca:a"), EngineCompat::default());
        assert_eq!(Stage::Normal.next(), Some(Stage::PlainPlates));
        assert_eq!(Stage::NoProjectSettings.next(), None);
        let saved = EngineCompat {
            stage: Some(Stage::NoProjectSettings),
            crashes_in_safe_mode: false,
        };
        memory.set("orca:c", &saved, "0xC0000005").unwrap();
        assert_eq!(memory.get("orca:c"), saved);
    }

    #[test]
    fn crash_mark_is_found_by_binary_without_probing() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("orca-slicer");
        fs::write(&exe, b"binary").unwrap();
        let engine = crate::Engine { kind: crate::EngineKind::Orca, path: exe.clone() };
        let info = engine.info("2.3.0".into(), Default::default());
        let memory = Memory::new(dir.path().join("compat.json"));
        assert!(!memory.crashes_for(&engine));
        let crashed = EngineCompat { stage: None, crashes_in_safe_mode: true };
        memory.set(&info.identity, &crashed, "").unwrap();
        assert!(memory.crashes_for(&engine));
        // Інший бінарник за тим самим шляхом (оновили рушій) — позначка не його.
        fs::write(&exe, b"another binary").unwrap();
        assert!(!memory.crashes_for(&engine));
    }

    #[test]
    fn stages_run_in_order_with_project_settings_last() {
        let mut order = vec![Stage::Normal];
        while let Some(next) = order.last().unwrap().next() {
            order.push(next);
        }
        assert_eq!(
            order,
            [
                Stage::Normal,
                Stage::PlainPlates,
                Stage::GcodeOnly,
                Stage::NoProjectSettings
            ]
        );
        assert!(order.windows(2).all(|w| w[0] < w[1]));
        for stage in order {
            assert_eq!(Stage::from_key(stage.key()), Some(stage));
        }
    }

    /// 3MF як від Bambu Studio 2.7: модель, пластини з модифікатором об’єкта, конфіг проєкту,
    /// вбудований пресет і відомості про нарізку.
    fn bambu_project(path: &Path) -> Vec<(&'static str, &'static [u8])> {
        let entries: Vec<(&str, &[u8])> = vec![
            ("[Content_Types].xml", b"<Types/>"),
            ("3D/3dmodel.model", b"<model><resources><object id=\"1\"/></resources></model>"),
            (
                "Metadata/model_settings.config",
                b"<config><object id=\"1\"><metadata key=\"wall_loops\" value=\"5\"/></object><plate><metadata key=\"plater_id\" value=\"2\"/></plate></config>",
            ),
            ("Metadata/layer_config_ranges.xml", b"<objects/>"),
            ("Metadata/custom_gcode_per_layer.xml", b"<custom_gcodes_per_layer/>"),
            ("Metadata/plate_2.png", b"png"),
            ("Metadata/project_settings.config", b"{\"printer_settings_id\":\"Bambu Lab X1 Carbon 0.4 nozzle\"}"),
            ("Metadata/process_settings_1.config", b"{\"name\":\"mine\"}"),
            ("Metadata/filament_settings_1.config", b"{\"name\":\"mine\"}"),
            ("Metadata/slice_info.config", b"<config/>"),
        ];
        let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
        for (name, bytes) in &entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
        entries
    }

    #[test]
    fn project_settings_copy_keeps_geometry_plates_and_object_settings() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("p.3mf");
        let entries = bambu_project(&input);
        let before = fs::read(&input).unwrap();
        assert!(has_project_settings(&input));
        let profiles: Map<String, Value> =
            serde_json::from_value(json!({"layer_height": "0.2", "printer_settings_id": "m"})).unwrap();
        let copy = without_project_settings(&input, &dir.path().join("bare"), &profiles)
            .unwrap()
            .unwrap();
        assert_eq!(fs::read(&input).unwrap(), before, "файл користувача не змінюється");
        let mut archive = zip::ZipArchive::new(fs::File::open(&copy).unwrap()).unwrap();
        let names: Vec<String> = archive.file_names().filter_map(|n| n.ok().map(|n| n.into_owned())).collect();
        let dropped = [
            "Metadata/project_settings.config",
            "Metadata/process_settings_1.config",
            "Metadata/filament_settings_1.config",
            "Metadata/slice_info.config",
        ];
        // Конфіг проєкту — з профілів Slicenv, а не з 3MF.
        let config: Map<String, Value> =
            serde_json::from_reader(archive.by_name("Metadata/project_settings.config").unwrap()).unwrap();
        assert_eq!(config, profiles);
        for (name, bytes) in entries {
            if name == "Metadata/project_settings.config" {
                continue;
            }
            if dropped.contains(&name) {
                assert!(!names.iter().any(|n| n == name), "{name} лишився");
                continue;
            }
            let mut kept = Vec::new();
            archive.by_name(name).unwrap().read_to_end(&mut kept).unwrap();
            assert_eq!(kept, bytes, "{name} змінився");
        }
        assert_eq!(names.len(), 7, "{names:?}");
        // Не 3MF — нічого не робимо.
        let stl = dir.path().join("p.stl");
        fs::write(&stl, "solid p\nendsolid p\n").unwrap();
        assert_eq!(
            without_project_settings(&stl, &dir.path().join("stl"), &profiles).unwrap(),
            None
        );
        // 3MF без налаштувань проєкту — теж нічого.
        let plain = dir.path().join("plain.3mf");
        let mut zip = zip::ZipWriter::new(fs::File::create(&plain).unwrap());
        zip.start_file("3D/3dmodel.model", zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(b"<model/>").unwrap();
        zip.finish().unwrap();
        assert!(!has_project_settings(&plain));
        assert_eq!(
            without_project_settings(&plain, &dir.path().join("plain"), &profiles).unwrap(),
            None
        );
    }

    #[test]
    fn only_settings_entries_are_project_settings() {
        for name in [
            "Metadata/project_settings.config",
            "Metadata/machine_settings_1.config",
            "Metadata/process_settings_12.config",
            "Metadata/slice_info.config",
        ] {
            assert!(is_project_settings_entry(name), "{name}");
        }
        for name in [
            "Metadata/model_settings.config",
            "Metadata/process_settings_.config",
            "Metadata/process_settings_a.config",
            "Metadata/layer_config_ranges.xml",
            "project_settings.config",
            "3D/3dmodel.model",
        ] {
            assert!(!is_project_settings_entry(name), "{name}");
        }
    }

    #[test]
    fn plate_names_are_removed_without_touching_the_rest() {
        let xml = "<config>\n  <plate>\n    <metadata key=\"plater_id\" value=\"1\"/>\n    <metadata key=\"plater_name\" value=\"Spider &amp; web\"/>\n    <metadata key=\"locked\" value=\"false\"/>\n  </plate>\n</config>";
        assert_eq!(
            strip_metadata(xml, "plater_name"),
            "<config>\n  <plate>\n    <metadata key=\"plater_id\" value=\"1\"/>\n    <metadata key=\"locked\" value=\"false\"/>\n  </plate>\n</config>"
        );
        assert_eq!(strip_metadata("<config/>", "plater_name"), "<config/>");
    }

    #[test]
    fn plate_name_copy_keeps_original_file() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("p.3mf");
        let mut zip = zip::ZipWriter::new(fs::File::create(&input).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("3D/3dmodel.model", options).unwrap();
        zip.write_all(b"<model/>").unwrap();
        zip.start_file("Metadata/model_settings.config", options)
            .unwrap();
        zip.write_all(
            b"<config><plate>\n<metadata key=\"plater_name\" value=\"A\"/>\n</plate></config>",
        )
        .unwrap();
        zip.finish().unwrap();
        let before = fs::read(&input).unwrap();
        let copy = without_plate_names(&input, &dir.path().join("safe"))
            .unwrap()
            .unwrap();
        assert_eq!(fs::read(&input).unwrap(), before);
        let mut archive = zip::ZipArchive::new(fs::File::open(&copy).unwrap()).unwrap();
        let mut text = String::new();
        archive
            .by_name("Metadata/model_settings.config")
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert!(!text.contains("plater_name"), "{text}");
        assert!(archive.by_name("3D/3dmodel.model").is_ok());
        // Без назв нічого не копіюється.
        assert_eq!(
            without_plate_names(&copy, &dir.path().join("again")).unwrap(),
            None
        );
    }

    #[test]
    fn orca_and_bambu_gcode_headers_give_time_weight_and_layers() {
        // Шапка й хвіст OrcaSlicer 2.3.0 (Windows, Spider Web XL), скорочено.
        let orca = "; HEADER_BLOCK_START\n; generated by OrcaSlicer 2.3.0\n; model printing time: 5h 12m 10s; total estimated time: 5h 18m 52s\n; total layer number: 80\n; HEADER_BLOCK_END\nG1 X1\n; filament used [mm] = 38250.55\n; filament used [g] = 115.92\n; CONFIG_BLOCK_START\n; layer_height = 0.2\n";
        assert_eq!(
            parse_summary(orca),
            GcodeSummary {
                print_time: Some(5.0 * 3600.0 + 18.0 * 60.0 + 52.0),
                filament_grams: Some(115.92),
                layers: Some(80),
                layer_height: Some(0.2),
                filament_mm: vec![38250.55],
                filament_g: vec![115.92],
            }
        );
        let bambu = "; model printing time: 50s; total estimated time: 1m 0s\n; total filament weight [g] : 0.01,2.5\n";
        let s = parse_summary(bambu);
        assert_eq!(s.print_time, Some(60.0));
        assert_eq!(s.filament_grams, Some(2.51));
        assert_eq!(parse_duration("1d 1s"), Some(86401.0));
        assert_eq!(parse_duration("soon"), None);
        let prusa =
            "; estimated printing time (normal mode) = 2h 0m 0s\n; total filament used [g] = 7.5\n";
        assert_eq!(parse_summary(prusa).print_time, Some(7200.0));
        assert_eq!(parse_summary(prusa).filament_grams, Some(7.5));
    }

    #[test]
    fn packaged_gcode_reads_back_like_an_engine_archive() {
        let dir = tempfile::tempdir().unwrap();
        let gcode = dir.path().join("plate_2.gcode");
        fs::write(&gcode, "; total estimated time: 1m 0s\nG1 X1\n").unwrap();
        let output = dir.path().join("p-plate2.gcode.3mf");
        let summary = GcodeSummary {
            print_time: Some(60.0),
            filament_grams: Some(1.5),
            ..Default::default()
        };
        let mut settings = Map::new();
        settings.insert("filament_settings_id".into(), json!(["PLA", "PETG"]));
        package_gcode(&gcode, &output, 2, &summary, &settings, &PackageMeta::default()).unwrap();
        let (time, weight, _) = crate::cli::read_slice_info(&output, 2).unwrap();
        assert_eq!((time, weight), (Some(60.0), Some(1.5)));
        let mut archive = zip::ZipArchive::new(fs::File::open(&output).unwrap()).unwrap();
        let mut md5 = String::new();
        archive
            .by_name("Metadata/plate_2.gcode.md5")
            .unwrap()
            .read_to_string(&mut md5)
            .unwrap();
        assert_eq!(md5.len(), 32);
        assert!(archive.by_name("Metadata/plate_2.gcode").is_ok());
        assert!(!output.with_extension("partial").exists());
    }

    fn entry(archive: &Path, name: &str) -> String {
        let mut zip = zip::ZipArchive::new(fs::File::open(archive).unwrap()).unwrap();
        let mut text = String::new();
        zip.by_name(name).unwrap().read_to_string(&mut text).unwrap();
        text
    }

    /// Структура як у `.gcode.3mf` OrcaSlicer 2.5 (еталон — нарізка Spider на Windows):
    /// Bambu Connect і принтер читають модель, сопло, філаменти, прогноз і мініатюри.
    #[test]
    fn compatibility_package_has_the_structure_of_an_engine_archive() {
        let dir = tempfile::tempdir().unwrap();
        let gcode = dir.path().join("plate_1.gcode");
        fs::write(&gcode, "; HEADER_BLOCK_START\n; total layer number: 198\n; HEADER_BLOCK_END\nG1 X1\n; filament used [mm] = 3990.0\n; filament used [g] = 9.98\n").unwrap();
        let summary = parse_summary(&fs::read_to_string(&gcode).unwrap());
        let summary = GcodeSummary { print_time: Some(8185.0), ..summary };
        let mut settings = Map::new();
        settings.insert("nozzle_diameter".into(), json!(["0.4"]));
        settings.insert("filament_type".into(), json!(["ABS"]));
        settings.insert("filament_ids".into(), json!(["GFB00"]));
        let meta = PackageMeta {
            printer_model_id: "N2S".into(),
            filament_colours: vec!["#8E9089".into()],
        };
        let output = dir.path().join("x.gcode.3mf");
        package_gcode(&gcode, &output, 1, &summary, &settings, &meta).unwrap();
        let names: std::collections::BTreeSet<String> = {
            let zip = zip::ZipArchive::new(fs::File::open(&output).unwrap()).unwrap();
            zip.file_names().map(|n| n.unwrap().into_owned()).collect()
        };
        for required in [
            "[Content_Types].xml",
            "_rels/.rels",
            "3D/3dmodel.model",
            "Metadata/plate_1.gcode",
            "Metadata/plate_1.gcode.md5",
            "Metadata/slice_info.config",
            "Metadata/model_settings.config",
            "Metadata/_rels/model_settings.config.rels",
            "Metadata/project_settings.config",
            "Metadata/filament_sequence.json",
        ] {
            assert!(names.contains(required), "немає {required}: {names:?}");
        }
        let info = entry(&output, "Metadata/slice_info.config");
        for needle in [
            r#"key="X-BBL-Client-Type" value="slicer""#,
            r#"key="printer_model_id" value="N2S""#,
            r#"key="nozzle_diameters" value="0.4""#,
            r#"key="prediction" value="8185""#,
            r#"key="weight" value="9.98""#,
            r##"tray_info_idx="GFB00" type="ABS" color="#8E9089" used_m="3.99" used_g="9.98""##,
            r#"layer_ranges="0 197""#,
        ] {
            assert!(info.contains(needle), "{needle}\n{info}");
        }
        assert_eq!(crate::cli::read_slice_info(&output, 1).unwrap().0, Some(8185.0));
        assert!(entry(&output, "Metadata/model_settings.config").contains(r#"key="gcode_file" value="Metadata/plate_1.gcode""#));
        assert!(is_slicenv_package(&output));

        // Мініатюри від застосунку: файли, посилання в model_settings і rels.
        let png = vec![0x89, b'P', b'N', b'G'];
        let assets = PlateAssets {
            plate: Some(png.clone()),
            small: Some(png.clone()),
            no_light: Some(png.clone()),
            top: Some(png.clone()),
            pick: Some(png),
            json: Some(br#"{"version":2}"#.to_vec()),
        };
        attach_plate_assets(&output, 1, &assets).unwrap();
        let zip = zip::ZipArchive::new(fs::File::open(&output).unwrap()).unwrap();
        for name in ["Metadata/plate_1.png", "Metadata/plate_1_small.png", "Metadata/plate_no_light_1.png", "Metadata/top_1.png", "Metadata/pick_1.png", "Metadata/plate_1.json", "Metadata/plate_1.gcode"] {
            assert!(zip.file_names().any(|n| n.unwrap() == name), "немає {name}");
        }
        assert_eq!(zip.len(), names.len() + 6, "без дублікатів");
        let settings_xml = entry(&output, "Metadata/model_settings.config");
        assert!(settings_xml.contains(r#"key="thumbnail_file" value="Metadata/plate_1.png""#));
        assert!(settings_xml.contains(r#"key="pick_file" value="Metadata/pick_1.png""#));
        assert!(entry(&output, "_rels/.rels").contains("cover-thumbnail-middle"));
        // Архів рушія не переписується.
        let foreign = dir.path().join("engine.gcode.3mf");
        let mut zip = zip::ZipWriter::new(fs::File::create(&foreign).unwrap());
        zip.start_file("3D/3dmodel.model", zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(b"<model/>").unwrap();
        zip.finish().unwrap();
        assert!(attach_plate_assets(&foreign, 1, &assets).is_err());
    }
}
