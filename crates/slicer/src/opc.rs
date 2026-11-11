//! Читач `<vendor>.opc` — бінарного кешу системних профілів OrcaSlicer ≥ 2.5
//! (`src/libslic3r/PresetCacheFormat.{hpp,cpp}`, `CACHE_VERSION = 2`).
//!
//! Файл: 20 байтів заголовка (magic `ZCRO`, версія, розмір тіла, CRC-32) і тіло cereal
//! (little-endian, рядки й вектори з 64-бітною довжиною): штампи, словник опцій (назви,
//! типи, назви значень enum), профіль виробника з моделями принтерів і три списки пресетів
//! (процеси, філаменти, принтери) у «вихідному» вигляді — власні значення пресета без
//! успадкованих, з назвами `inherits`/`include`.
//!
//! Словник описує сам себе (тип кожної опції записано у файлі), тож визначення опцій
//! OrcaSlicer тут не потрібні. Результат — ті самі JSON, що лежать у git-тезі випуску:
//! скаляр — рядок, вектор — масив рядків. Невідома версія формату чи пошкоджений файл —
//! помилка; тоді профілі беруться з мережі, як раніше.
use anyhow::{Context as _, Result, bail, ensure};
use serde_json::{Map, Value, json};
use std::path::Path;

const MAGIC: u32 = 0x4F52_435A; // "ZCRO" у файлі
const VERSION: u32 = 2;
const VECTOR: u16 = 0x4000;
/// Запобіжник від зіпсованих довжин: жоден рядок профілю не більший за це.
const MAX_STRING: usize = 4 * 1024 * 1024;

/// Один пресет або модель принтера: відносний шлях у теці виробника та JSON.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub sub_path: String,
    pub json: Value,
}

/// Розібраний кеш одного виробника.
#[derive(Debug, Clone, PartialEq)]
pub struct Catalog {
    pub vendor: String,
    /// Версія профілю виробника (`BBL.json` → `version`).
    pub version: String,
    /// `<vendor>.json`: списки назв і шляхів.
    pub index: Value,
    /// Моделі принтерів (`type: machine_model`) і пресети.
    pub entries: Vec<Entry>,
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(n).context("opc: переповнення довжини")?;
        ensure!(end <= self.data.len(), "opc: файл обривається");
        let out = &self.data[self.at..end];
        self.at = end;
        Ok(out)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into()?))
    }
    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into()?))
    }
    /// Кількість елементів, кожен щонайменше `min_size` байтів: більша за залишок файлу
    /// довжина — ознака зсуву потоку, а не привід виділяти пам’ять.
    fn count(&mut self, min_size: usize) -> Result<usize> {
        let n = usize::try_from(self.u64()?).context("opc: завелика довжина")?;
        ensure!(
            n.saturating_mul(min_size.max(1)) <= self.data.len() - self.at,
            "opc: довжина {n} більша за залишок файлу"
        );
        Ok(n)
    }
    fn string(&mut self) -> Result<String> {
        let n = self.count(1)?;
        ensure!(n <= MAX_STRING, "opc: завеликий рядок");
        String::from_utf8(self.take(n)?.to_vec()).context("opc: рядок не UTF-8")
    }
    fn strings(&mut self) -> Result<Vec<String>> {
        let n = self.count(8)?;
        (0..n).map(|_| self.string()).collect()
    }
}

/// CRC-32 (IEEE 802.3), як `boost::crc_32_type`.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (!(crc & 1)).wrapping_add(1));
        }
    }
    !crc
}

/// Число так, як його розуміє рушій: найкоротший запис, що читається назад у те саме
/// значення; ціле — без дробової частини.
fn float(v: f64) -> String {
    if v.is_nan() {
        "nil".to_owned()
    } else {
        format!("{v}")
    }
}

fn float_or_percent(v: f64, percent: bool) -> String {
    if v.is_nan() {
        "nil".to_owned()
    } else if percent {
        format!("{v}%")
    } else {
        format!("{v}")
    }
}

fn point(x: f64, y: f64) -> String {
    format!("{}x{}", float(x), float(y))
}

fn array(values: Vec<String>) -> Value {
    Value::Array(values.into_iter().map(Value::String).collect())
}

struct Dictionary {
    keys: Vec<String>,
    types: Vec<u16>,
    enums: Vec<String>,
}

impl Dictionary {
    fn read(r: &mut Reader) -> Result<Self> {
        let keys = r.strings()?;
        let n = r.count(2)?;
        let types = (0..n).map(|_| r.u16()).collect::<Result<Vec<_>>>()?;
        let enums = r.strings()?;
        ensure!(keys.len() == types.len(), "opc: словник: різна довжина таблиць");
        ensure!(
            enums.first().is_some_and(String::is_empty),
            "opc: словник без порожнього значення enum"
        );
        Ok(Self { keys, types, enums })
    }

    fn enum_text(&self, r: &mut Reader) -> Result<String> {
        let index = usize::from(r.u16()?);
        let name = self.enums.get(index).context("opc: індекс enum поза словником")?;
        if index != 0 {
            return Ok(name.clone());
        }
        // Значення, якому рушій не мав назви: число йде слідом як є.
        let raw = r.i32()?;
        Ok(if raw == i32::MAX {
            "nil".to_owned()
        } else {
            raw.to_string()
        })
    }

    /// Значення однієї опції у вигляді, в якому його несе JSON-профіль.
    fn value(&self, r: &mut Reader, kind: u16) -> Result<Value> {
        let vector = kind & VECTOR != 0;
        Ok(match (kind & !VECTOR, vector) {
            (1, false) => Value::String(float(r.f64()?)),
            (1, true) => {
                let n = r.count(8)?;
                array((0..n).map(|_| r.f64().map(float)).collect::<Result<_>>()?)
            }
            (2, false) => Value::String(r.i32()?.to_string()),
            (2, true) => {
                let n = r.count(4)?;
                array(
                    (0..n)
                        .map(|_| {
                            r.i32().map(|v| {
                                if v == i32::MAX {
                                    "nil".to_owned()
                                } else {
                                    v.to_string()
                                }
                            })
                        })
                        .collect::<Result<_>>()?,
                )
            }
            (3, false) => Value::String(r.string()?),
            (3, true) => array(r.strings()?),
            (4, false) => Value::String(float_or_percent(r.f64()?, true)),
            (4, true) => {
                let n = r.count(8)?;
                array(
                    (0..n)
                        .map(|_| r.f64().map(|v| float_or_percent(v, true)))
                        .collect::<Result<_>>()?,
                )
            }
            (5, false) => {
                let v = r.f64()?;
                Value::String(float_or_percent(v, r.u8()? != 0))
            }
            (5, true) => {
                let n = r.count(9)?;
                array(
                    (0..n)
                        .map(|_| {
                            let v = r.f64()?;
                            Ok(float_or_percent(v, r.u8()? != 0))
                        })
                        .collect::<Result<_>>()?,
                )
            }
            (6, false) => Value::String(point(r.f64()?, r.f64()?)),
            (6, true) => {
                let n = r.count(16)?;
                array(
                    (0..n)
                        .map(|_| Ok(point(r.f64()?, r.f64()?)))
                        .collect::<Result<_>>()?,
                )
            }
            (7, false) => {
                let (x, y, z) = (r.f64()?, r.f64()?, r.f64()?);
                Value::String(format!("{}x{}x{}", float(x), float(y), float(z)))
            }
            (8, false) => Value::String(u8::from(r.u8()? != 0).to_string()),
            (8, true) => {
                let n = r.count(1)?;
                array(
                    (0..n)
                        .map(|_| {
                            r.u8().map(|v| match v {
                                0 => "0".to_owned(),
                                1 => "1".to_owned(),
                                _ => "nil".to_owned(),
                            })
                        })
                        .collect::<Result<_>>()?,
                )
            }
            (9, _) => {
                let n = r.u32()?;
                ensure!(vector || n == 1, "opc: скалярний enum із {n} значеннями");
                let mut values = Vec::new();
                for _ in 0..n {
                    values.push(self.enum_text(r)?);
                }
                if vector {
                    array(values)
                } else {
                    Value::String(values.remove(0))
                }
            }
            // Групи точок: кожна група — рядок `XxY,XxY`.
            (10, true) => {
                let groups = r.count(8)?;
                let mut out = Vec::new();
                for _ in 0..groups {
                    let n = r.count(16)?;
                    let points = (0..n)
                        .map(|_| Ok(point(r.f64()?, r.f64()?)))
                        .collect::<Result<Vec<_>>>()?;
                    out.push(points.join(","));
                }
                array(out)
            }
            // Групи цілих: кожна група — рядок `1,2,3`.
            (11, true) => {
                let groups = r.count(8)?;
                let mut out = Vec::new();
                for _ in 0..groups {
                    let n = r.count(4)?;
                    let ints = (0..n)
                        .map(|_| r.i32().map(|v| v.to_string()))
                        .collect::<Result<Vec<_>>>()?;
                    out.push(ints.join(","));
                }
                array(out)
            }
            _ => bail!("opc: невідомий тип опції {kind:#x}"),
        })
    }

    fn config(&self, r: &mut Reader) -> Result<Map<String, Value>> {
        let mut out = Map::new();
        for _ in 0..r.u32()? {
            let index = usize::from(r.u16()?);
            let key = self.keys.get(index).context("opc: індекс опції поза словником")?;
            out.insert(key.clone(), self.value(r, self.types[index])?);
        }
        Ok(out)
    }
}

fn joined(values: &[String]) -> Value {
    Value::String(values.join(";"))
}

/// Модель принтера — файл `machine/<назва>.json` з `type: machine_model`.
fn model(r: &mut Reader) -> Result<Entry> {
    let _id = r.string()?;
    let name = r.string()?;
    let model_id = r.string()?;
    let technology = r.u8()?;
    let family = r.string()?;
    let variants = (0..r.count(8)?)
        .map(|_| r.string())
        .collect::<Result<Vec<_>>>()?;
    let default_materials = r.strings()?;
    let not_support_bed_types = r.strings()?;
    let mut json = Map::new();
    json.insert("type".into(), "machine_model".into());
    json.insert("name".into(), name.clone().into());
    json.insert("model_id".into(), model_id.into());
    json.insert("nozzle_diameter".into(), joined(&variants));
    json.insert(
        "machine_tech".into(),
        if technology == 1 { "SLA" } else { "FFF" }.into(),
    );
    json.insert("family".into(), family.into());
    json.insert("default_materials".into(), joined(&default_materials));
    if !not_support_bed_types.is_empty() {
        json.insert("not_support_bed_type".into(), joined(&not_support_bed_types));
    }
    for key in [
        "bed_model",
        "bed_texture",
        "image_bed_type",
        "bottom_texture_end_name",
        "use_double_extruder_default_texture",
        "bottom_texture_rect",
        "bottom_texture_rect_longer",
        "middle_texture_rect",
        "hotend_model",
    ] {
        let value = r.string()?;
        if !value.is_empty() {
            json.insert(key.into(), value.into());
        }
    }
    Ok(Entry {
        sub_path: format!("machine/{name}.json"),
        json: Value::Object(json),
    })
}

/// Профілі виробників у файлі; повертає моделі та версію того, що зветься `vendor`.
fn vendor_profiles(r: &mut Reader, vendor: &str) -> Result<(Vec<Entry>, Option<String>)> {
    let mut found = (Vec::new(), None);
    for _ in 0..r.count(8)? {
        let key = r.string()?;
        let _name = r.string()?;
        let _id = r.string()?;
        let version = r.string()?;
        let _update_url = r.string()?;
        let _changelog_url = r.string()?;
        let models = (0..r.count(8)?)
            .map(|_| model(r))
            .collect::<Result<Vec<_>>>()?;
        let _default_filaments = r.strings()?;
        let _default_sla_materials = r.strings()?;
        if key == vendor {
            found = (models, Some(version));
        }
    }
    Ok(found)
}

fn presets(r: &mut Reader, dict: &Dictionary, kind: &str) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    for _ in 0..r.u32()? {
        let name = r.string()?;
        let sub_path = r.string()?;
        let config = dict.config(r)?;
        let inherits = r.string()?;
        let includes = r.strings()?;
        let description = r.string()?;
        let instantiation = r.string()?;
        let setting_id = r.string()?;
        let filament_id = r.string()?;
        let renamed_from = r.strings()?;
        ensure!(
            !sub_path.is_empty()
                && !sub_path.contains('\\')
                && sub_path
                    .split('/')
                    .all(|p| !p.is_empty() && p != "." && p != ".."),
            "opc: небезпечний шлях пресета"
        );
        let mut json = Map::new();
        json.insert("type".into(), kind.into());
        json.insert("name".into(), name.into());
        if !inherits.is_empty() {
            json.insert("inherits".into(), inherits.into());
        }
        json.insert("from".into(), "system".into());
        for (key, value) in [
            ("setting_id", setting_id),
            ("filament_id", filament_id),
            ("instantiation", instantiation),
            ("description", description),
        ] {
            if !value.is_empty() {
                json.insert(key.into(), value.into());
            }
        }
        if !renamed_from.is_empty() {
            json.insert("renamed_from".into(), joined(&renamed_from));
        }
        if !includes.is_empty() {
            json.insert("include".into(), array(includes));
        }
        json.extend(config);
        out.push(Entry {
            sub_path,
            json: Value::Object(json),
        });
    }
    Ok(out)
}

fn listing(entries: &[Entry]) -> Value {
    Value::Array(
        entries
            .iter()
            .map(|e| json!({"name": e.json["name"], "sub_path": e.sub_path}))
            .collect(),
    )
}

/// Розбирає вміст `<vendor>.opc`.
pub fn decode(data: &[u8], vendor: &str) -> Result<Catalog> {
    ensure!(data.len() > 20, "opc: файл закороткий");
    let mut header = Reader { data: &data[..20], at: 0 };
    ensure!(header.u32()? == MAGIC, "opc: не кеш профілів OrcaSlicer");
    let version = header.u32()?;
    ensure!(version == VERSION, "opc: невідома версія формату {version}");
    let body = &data[20..];
    ensure!(
        header.u64()? == body.len() as u64,
        "opc: розмір тіла не збігається із заголовком"
    );
    ensure!(header.u32()? == crc32(body), "opc: контрольна сума не збігається");

    let mut r = Reader { data: body, at: 0 };
    ensure!(r.u32()? == VERSION, "opc: невідома версія тіла");
    let stamped = r.string()?;
    ensure!(stamped == vendor, "opc: це кеш «{stamped}», а не «{vendor}»");
    let stamped_version = r.string()?;
    let dict = Dictionary::read(&mut r)?;
    let (models, profile_version) = vendor_profiles(&mut r, vendor)?;
    let profile_version = profile_version.context("opc: у кеші немає профілю свого виробника")?;
    let process = presets(&mut r, &dict, "process")?;
    let filament = presets(&mut r, &dict, "filament")?;
    let machine = presets(&mut r, &dict, "machine")?;
    let _parse_errors = r.u64()?;
    ensure!(r.at == body.len(), "opc: зайві дані після пресетів");

    let index = json!({
        "name": vendor,
        "version": if stamped_version.is_empty() { profile_version.clone() } else { stamped_version },
        "machine_model_list": listing(&models),
        "process_list": listing(&process),
        "filament_list": listing(&filament),
        "machine_list": listing(&machine),
    });
    let mut entries = models;
    entries.extend(process);
    entries.extend(filament);
    entries.extend(machine);
    Ok(Catalog {
        vendor: vendor.to_owned(),
        version: profile_version,
        index,
        entries,
    })
}

/// Записує каталог виробника як JSON-дерево: `<stage>/<vendor>.json` і
/// `<stage>/<vendor>/{machine,process,filament}/…`.
pub fn write_json_tree(catalog: &Catalog, stage: &Path) -> Result<usize> {
    let root = stage.join(&catalog.vendor);
    for kind in ["machine", "process", "filament"] {
        std::fs::create_dir_all(root.join(kind))?;
    }
    for entry in &catalog.entries {
        let path = root.join(&entry.sub_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, serde_json::to_vec_pretty(&entry.json)?)
            .with_context(|| format!("не записано {}", path.display()))?;
    }
    std::fs::write(
        stage.join(format!("{}.json", catalog.vendor)),
        serde_json::to_vec_pretty(&catalog.index)?,
    )?;
    Ok(catalog.entries.len())
}

/// Каталог профілів із кешів пакета рушія: `<library>/<family>/<version>` у тому самому
/// вигляді, що й каталог із git-тегу (`catalog.json`, `source: engine-cache`). Нічого не
/// робить, якщо каталог цієї версії вже є. Повертає кількість записаних профілів.
///
/// Кеш несе те, що розуміє сам OrcaSlicer: ключі, яких він не знає, у ньому відсутні, а
/// значення вже приведені до його типів. Для нарізки цим рушієм цього досить; точні JSON
/// з тегу випуску можуть замінити каталог пізніше.
pub fn import_catalog(
    package: &Path,
    library: &Path,
    family: &str,
    version: &str,
    vendors: &[&str],
) -> Result<usize> {
    ensure!(
        !version.is_empty()
            && version != "."
            && version != ".."
            && !version.contains(['/', '\\']),
        "opc: некоректна версія «{version}»"
    );
    let root = library.join(family);
    let target = root.join(version);
    if target.join("catalog.json").is_file() {
        return Ok(0);
    }
    std::fs::create_dir_all(&root)?;
    let scratch = tempfile::Builder::new().prefix(".profiles-").tempdir_in(&root)?;
    let stage = scratch.path().join("catalog");
    std::fs::create_dir_all(&stage)?;
    let mut files = 0;
    let mut imported = Vec::new();
    for vendor in vendors {
        let path = package.join(format!("{vendor}.opc"));
        let data = std::fs::read(&path).with_context(|| format!("немає {}", path.display()))?;
        files += write_json_tree(&decode(&data, vendor)?, &stage)?;
        imported.push(*vendor);
    }
    let fetched_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default();
    std::fs::write(
        stage.join("catalog.json"),
        serde_json::to_vec_pretty(&json!({
            "family": family,
            "version": version,
            "source": "engine-cache",
            "source_ref": package.to_string_lossy(),
            "fetched_at": fetched_at,
            "vendors": imported,
        }))?,
    )?;
    // Тека версії без catalog.json (обірване завантаження) не є каталогом — прибрати.
    if target.exists() {
        std::fs::remove_dir_all(&target)?;
    }
    std::fs::rename(&stage, &target)?;
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Writer(Vec<u8>);

    impl Writer {
        fn u8(&mut self, v: u8) -> &mut Self {
            self.0.push(v);
            self
        }
        fn u16(&mut self, v: u16) -> &mut Self {
            self.0.extend(v.to_le_bytes());
            self
        }
        fn u32(&mut self, v: u32) -> &mut Self {
            self.0.extend(v.to_le_bytes());
            self
        }
        fn u64(&mut self, v: u64) -> &mut Self {
            self.0.extend(v.to_le_bytes());
            self
        }
        fn f64(&mut self, v: f64) -> &mut Self {
            self.0.extend(v.to_le_bytes());
            self
        }
        fn s(&mut self, v: &str) -> &mut Self {
            self.u64(v.len() as u64);
            self.0.extend(v.as_bytes());
            self
        }
        fn list(&mut self, v: &[&str]) -> &mut Self {
            self.u64(v.len() as u64);
            for item in v {
                self.s(item);
            }
            self
        }
    }

    fn file(body: Vec<u8>) -> Vec<u8> {
        let mut out = Writer::default();
        out.u32(MAGIC).u32(VERSION).u64(body.len() as u64).u32(crc32(&body));
        out.0.extend(body);
        out.0
    }

    /// Мінімальний кеш: одна модель, один процес, один філамент, один принтер.
    fn sample() -> Vec<u8> {
        let mut w = Writer::default();
        w.u32(VERSION).s("Test").s("1.2.3");
        // Словник: назви, типи, значення enum.
        w.list(&["layer_height", "nozzle_temperature", "seam_position", "sparse_infill_density", "printable_area", "machine_start_gcode", "wall_speed"]);
        w.u64(7);
        for kind in [1, VECTOR | 2, 9, 4, VECTOR | 6, 3, VECTOR | 5] {
            w.u16(kind);
        }
        w.list(&["", "aligned"]);
        // Профіль виробника з однією моделлю.
        w.u64(1).s("Test").s("Test vendor").s("Test").s("1.2.3").s("").s("");
        w.u64(1).s("Printer").s("Printer").s("N1").u8(0).s("FAM");
        w.u64(2).s("0.4").s("0.2");
        w.list(&["PLA @Printer"]).list(&[]);
        w.s("bed.stl").s("bed.svg");
        for _ in 0..7 {
            w.s("");
        }
        w.list(&[]).list(&[]);
        // Процеси.
        w.u32(1).s("0.20mm").s("process/0.20mm.json");
        w.u32(3);
        w.u16(0).f64(0.2);
        w.u16(2).u32(1).u16(1);
        w.u16(3).f64(15.0);
        w.s("base").list(&[]).s("").s("true").s("GP004").s("").list(&["old name"]);
        // Філаменти.
        w.u32(1).s("PLA @Printer").s("filament/PLA @Printer.json");
        w.u32(2);
        w.u16(1).u64(2).u32(220).u32(i32::MAX as u32);
        w.u16(6).u64(2).f64(60.0).u8(0).f64(50.0).u8(1);
        w.s("").list(&["template"]).s("").s("true").s("GFSA00").s("GFA00").list(&[]);
        // Принтери.
        w.u32(1).s("Printer 0.4 nozzle").s("machine/Printer 0.4 nozzle.json");
        w.u32(2);
        w.u16(4).u64(2).f64(0.0).f64(0.0).f64(256.0).f64(0.5);
        w.u16(5).s("G28\nM400");
        w.s("").list(&[]).s("").s("true").s("GM001").s("").list(&[]);
        w.u64(0);
        file(w.0)
    }

    #[test]
    fn decodes_models_and_presets_as_profile_json() {
        let catalog = decode(&sample(), "Test").unwrap();
        assert_eq!(catalog.version, "1.2.3");
        assert_eq!(catalog.index["machine_model_list"][0]["sub_path"], "machine/Printer.json");
        assert_eq!(catalog.index["filament_list"][0]["name"], "PLA @Printer");
        let get = |path: &str| {
            &catalog
                .entries
                .iter()
                .find(|e| e.sub_path == path)
                .unwrap()
                .json
        };
        let model = get("machine/Printer.json");
        assert_eq!(model["type"], "machine_model");
        assert_eq!(model["nozzle_diameter"], "0.4;0.2");
        assert_eq!(model["model_id"], "N1");
        assert_eq!(model["bed_model"], "bed.stl");
        assert!(model.get("hotend_model").is_none());

        let process = get("process/0.20mm.json");
        assert_eq!(process["type"], "process");
        assert_eq!(process["inherits"], "base");
        assert_eq!(process["layer_height"], "0.2");
        assert_eq!(process["seam_position"], "aligned");
        assert_eq!(process["sparse_infill_density"], "15%");
        assert_eq!(process["renamed_from"], "old name");

        let filament = get("filament/PLA @Printer.json");
        assert_eq!(filament["nozzle_temperature"], json!(["220", "nil"]));
        assert_eq!(filament["wall_speed"], json!(["60", "50%"]));
        assert_eq!(filament["include"], json!(["template"]));
        assert_eq!(filament["filament_id"], "GFA00");
        assert!(filament.get("inherits").is_none());

        let machine = get("machine/Printer 0.4 nozzle.json");
        assert_eq!(machine["printable_area"], json!(["0x0", "256x0.5"]));
        assert_eq!(machine["machine_start_gcode"], "G28\nM400");
    }

    #[test]
    fn rejects_damaged_foreign_and_unknown_files() {
        let good = sample();
        assert!(decode(&good, "Other").is_err(), "чужий виробник");
        let mut flipped = good.clone();
        *flipped.last_mut().unwrap() ^= 1;
        assert!(decode(&flipped, "Test").is_err(), "контрольна сума");
        let mut future = good.clone();
        future[4] = 3;
        assert!(decode(&future, "Test").is_err(), "нова версія формату");
        assert!(decode(&good[..good.len() - 4], "Test").is_err(), "обрізаний файл");
        assert!(decode(b"ZCRO", "Test").is_err());
    }

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn writes_the_json_tree_of_a_vendor() {
        let catalog = decode(&sample(), "Test").unwrap();
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(write_json_tree(&catalog, dir.path()).unwrap(), 4);
        assert!(dir.path().join("Test.json").is_file());
        let saved: Value = serde_json::from_slice(
            &std::fs::read(dir.path().join("Test/filament/PLA @Printer.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(saved["name"], "PLA @Printer");
    }

    #[test]
    fn imports_a_package_cache_into_the_library_once() {
        let dir = tempfile::tempdir().unwrap();
        let (package, library) = (dir.path().join("package"), dir.path().join("library"));
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("Test.opc"), sample()).unwrap();
        assert_eq!(import_catalog(&package, &library, "orca", "2.5.0", &["Test"]).unwrap(), 4);
        let target = library.join("orca/2.5.0");
        let manifest: Value =
            serde_json::from_slice(&std::fs::read(target.join("catalog.json")).unwrap()).unwrap();
        assert_eq!(manifest["source"], "engine-cache");
        assert_eq!(manifest["vendors"], json!(["Test"]));
        assert!(target.join("Test/machine/Printer.json").is_file());
        // Наявний каталог не чіпається; без кешу чи з небезпечною версією — помилка.
        assert_eq!(import_catalog(&package, &library, "orca", "2.5.0", &["Test"]).unwrap(), 0);
        assert!(import_catalog(&package, &library, "orca", "2.6.0", &["Absent"]).is_err());
        assert!(!library.join("orca/2.6.0").exists());
        assert!(import_catalog(&package, &library, "orca", "../x", &["Test"]).is_err());
        let leftovers: Vec<_> = std::fs::read_dir(library.join("orca"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(leftovers, ["2.5.0"]);
    }

    /// Справжній кеш проти JSON з git-тегу тієї самої версії:
    /// `SLICENV_OPC_FILE=…/BBL.opc SLICENV_OPC_REFERENCE=…/profiles/orca/2.5.0 \
    ///  cargo test -p slicer --lib opc -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_cache_matches_the_release_json() {
        let file = std::env::var("SLICENV_OPC_FILE").unwrap();
        let reference = std::path::PathBuf::from(std::env::var("SLICENV_OPC_REFERENCE").unwrap());
        let catalog = decode(&std::fs::read(file).unwrap(), "BBL").unwrap();
        // Числа порівнюються як числа: «0.20» і «0.2» — те саме значення.
        fn norm(v: &Value) -> Value {
            match v {
                Value::String(s) => {
                    let t = s.trim();
                    let (number, percent) = t.strip_suffix('%').map_or((t, ""), |n| (n, "%"));
                    match number.parse::<f64>() {
                        Ok(n) if !number.is_empty() => Value::String(format!("{n}{percent}")),
                        _ => Value::String(s.clone()),
                    }
                }
                Value::Array(a) => Value::Array(a.iter().map(norm).collect()),
                Value::Number(n) => Value::String(format!("{}", n.as_f64().unwrap())),
                Value::Bool(b) => Value::String(u8::from(*b).to_string()),
                other => other.clone(),
            }
        }
        let (mut files, mut missing_files, mut differing) = (0, 0, 0);
        let mut missing_keys = std::collections::BTreeMap::<String, usize>::new();
        let mut extra_keys = std::collections::BTreeMap::<String, usize>::new();
        for entry in &catalog.entries {
            let path = reference.join("BBL").join(&entry.sub_path);
            let Ok(data) = std::fs::read(&path) else {
                missing_files += 1;
                continue;
            };
            files += 1;
            let want: Value = serde_json::from_slice(&data).unwrap();
            let (want, got) = (want.as_object().unwrap(), entry.json.as_object().unwrap());
            for (key, value) in want {
                match got.get(key) {
                    None => *missing_keys.entry(key.clone()).or_default() += 1,
                    Some(ours) if norm(ours) != norm(value) => {
                        differing += 1;
                        if key != "default_materials" {
                            let cut = |v: &Value| v.to_string().chars().take(60).collect::<String>();
                            println!("≠ {} · {key}: {} → {}", entry.sub_path, cut(value), cut(ours));
                        }
                    }
                    Some(_) => {}
                }
            }
            for key in got.keys().filter(|k| !want.contains_key(*k)) {
                *extra_keys.entry(key.clone()).or_default() += 1;
            }
        }
        println!("файлів {files}, без еталона {missing_files}, різних значень {differing}");
        println!("MISSING {}", missing_keys.keys().cloned().collect::<Vec<_>>().join(" "));
        println!("ключі лише в кеші: {extra_keys:?}");
        // Розбіжності — нормалізація самого OrcaSlicer (типи, застарілі назви), тож це звіт,
        // а не рівність; структурно кеш має покривати весь каталог.
        assert!(files > 2000);
        assert_eq!(missing_files, 0);
        assert!(differing < 200, "{differing}");
    }
}
