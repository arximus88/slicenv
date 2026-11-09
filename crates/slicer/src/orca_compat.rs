//! Проєкти Bambu Studio в OrcaSlicer: «філамент для елемента» (`wall_filament` тощо).
//!
//! У Bambu Studio ці ключі мають min 0, і 0 означає «філамент об’єкта». OrcaSlicer ≤ 2.3 описує
//! їх як ціле ≥ 1 (1 — типовий філамент) і відкидає 3MF з нулем: CLI завершується з кодом −18
//! «Param values in 3mf/config error: wall_filament: 0 not in range [1,2147483647]».
//! OrcaSlicer ≥ 2.4 (`PrintConfigDef::handle_legacy`) перейменовує їх на `*_filament_id` з min 0
//! і сам переводить легасі «1» у 0 («успадкувати»). Тому 1 — правильне значення для будь-якої
//! версії Orca, а для Bambu Studio нічого не змінюємо.
use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use serde_json::{Map, Value};

/// Легасі-ключ Orca ≤ 2.3 (min 1) → ключ сучасної схеми Orca (min 0), куди його переносить
/// `handle_legacy`. Спільне джерело для нарізання й перевірки сумісності (`settings`).
pub const ORCA_LEGACY_FILAMENT_KEYS: &[(&str, &str)] = &[
    ("wall_filament", "outer_wall_filament_id"),
    ("sparse_infill_filament", "sparse_infill_filament_id"),
    ("solid_infill_filament", "internal_solid_filament_id"),
];

/// Ключі, де Bambu Studio має −1 = «авто» (min −1), а Orca — min 0 без «авто»
/// (порівняння `settings/schema` і `settings/schema-orca`). OrcaSlicer 2.3 відкидає −1 з тим
/// самим кодом −18; «авто» для Orca — відсутній ключ: береться пресет процесу або типове.
pub const BAMBU_AUTO_KEYS: &[&str] = &["raft_first_layer_expansion", "tree_support_wall_count"];

const PROJECT_CONFIG: &str = "Metadata/project_settings.config";

/// Значення < 1 у легасі-ключах → 1 (тип значення зберігається: рядок лишається рядком);
/// «авто» −1 з [`BAMBU_AUTO_KEYS`] вилучається. Повертає, чи щось змінилося.
pub fn normalize_orca_config(config: &mut Map<String, Value>) -> bool {
    let mut changed = false;
    for key in BAMBU_AUTO_KEYS {
        let auto = match config.get(*key) {
            Some(Value::String(s)) => s.trim().parse::<f64>().is_ok_and(|n| n == -1.),
            Some(Value::Number(n)) => n.as_f64() == Some(-1.),
            _ => false,
        };
        if auto {
            config.remove(*key);
            changed = true;
        }
    }
    for (key, _) in ORCA_LEGACY_FILAMENT_KEYS {
        let fixed = match config.get(*key) {
            Some(Value::String(s)) if s.trim().parse::<i64>().is_ok_and(|n| n < 1) => {
                Value::String("1".into())
            }
            Some(Value::Number(n)) if n.as_f64().is_some_and(|n| n < 1.) => Value::from(1),
            _ => continue,
        };
        config.insert((*key).into(), fixed);
        changed = true;
    }
    changed
}

/// Якщо `project_settings.config` у 3MF містить значення, яких не приймає Orca ≤ 2.3, —
/// копія архіву в `dir` з виправленим конфігом (решта записів копіюється без перепакування).
/// `None` — виправляти нічого, нарізати оригінал. Сам `input` не змінюється.
#[cfg(test)]
pub(crate) fn orca_compatible_3mf(input: &Path, dir: &Path) -> Result<Option<PathBuf>> {
    rewrite_project_config(input, dir, normalize_orca_config)
}

/// Копія 3MF у `dir`, якщо `fix` змінив `project_settings.config` (повертає `true`); решта
/// записів копіюється без перепакування. `None` — змінювати нічого, нарізати оригінал.
pub(crate) fn rewrite_project_config(
    input: &Path,
    dir: &Path,
    fix: impl FnOnce(&mut Map<String, Value>) -> bool,
) -> Result<Option<PathBuf>> {
    let mut archive = zip::ZipArchive::new(fs::File::open(input)?)
        .with_context(|| format!("{} не є 3MF", input.display()))?;
    let mut config: Map<String, Value> = match archive.by_name(PROJECT_CONFIG) {
        Ok(file) => match serde_json::from_reader(file) {
            Ok(config) => config,
            // Не JSON (старі 3MF): рушій розбере сам, ми не втручаємось.
            Err(_) => return Ok(None),
        },
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !fix(&mut config) {
        return Ok(None);
    }
    fs::create_dir_all(dir)?;
    let output = dir.join(input.file_name().unwrap_or("project.3mf".as_ref()));
    let mut writer = zip::ZipWriter::new(fs::File::create(&output)?);
    for i in 0..archive.len() {
        let file = archive.by_index_raw(i)?;
        if file.name().is_ok_and(|name| name == PROJECT_CONFIG) {
            drop(file);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            writer.start_file(PROJECT_CONFIG, options)?;
            writer.write_all(&serde_json::to_vec_pretty(&config)?)?;
        } else {
            writer.raw_copy_file(file)?;
        }
    }
    writer.finish()?;
    Ok(Some(output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Read as _;

    fn project(dir: &Path, config: &Value) -> PathBuf {
        let path = dir.join("bambu.3mf");
        let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("3D/3dmodel.model", options).unwrap();
        zip.write_all(b"<model/>").unwrap();
        zip.start_file(PROJECT_CONFIG, options).unwrap();
        zip.write_all(&serde_json::to_vec(config).unwrap()).unwrap();
        zip.finish().unwrap();
        path
    }

    fn read(path: &Path, name: &str) -> Vec<u8> {
        let mut archive = zip::ZipArchive::new(fs::File::open(path).unwrap()).unwrap();
        let mut out = Vec::new();
        archive
            .by_name(name)
            .unwrap()
            .read_to_end(&mut out)
            .unwrap();
        out
    }

    #[test]
    fn zero_feature_filaments_become_orca_default_and_others_stay() {
        let mut config = serde_json::from_value(json!({
            "wall_filament": "0", "sparse_infill_filament": 0, "solid_infill_filament": "2",
            "support_filament": "0", "layer_height": "0.2",
            "raft_first_layer_expansion": "-1", "tree_support_wall_count": -1,
        }))
        .unwrap();
        assert!(normalize_orca_config(&mut config));
        assert!(!config.contains_key("raft_first_layer_expansion"));
        assert!(!config.contains_key("tree_support_wall_count"));
        assert_eq!(config["wall_filament"], json!("1"));
        assert_eq!(config["sparse_infill_filament"], json!(1));
        // Явний вибір і ключі з min 0 в Orca (support_filament) не чіпаємо.
        assert_eq!(config["solid_infill_filament"], json!("2"));
        assert_eq!(config["support_filament"], json!("0"));
        assert!(!normalize_orca_config(&mut config));
    }

    #[test]
    fn bambu_project_with_zero_filaments_is_copied_with_fixed_config() {
        let dir = tempfile::tempdir().unwrap();
        // Як у 3MF з Bambu Studio 2.7 (3mf_Spider_Web_S-XL.3mf), з якого Orca 2.3 падала з −18.
        let original = json!({
            "wall_filament": "0", "sparse_infill_filament": "0", "solid_infill_filament": "0",
            "support_filament": "0", "support_interface_filament": "0",
            "raft_first_layer_expansion": "-1", "tree_support_wall_count": "-1",
            "raft_layers": "0",
        });
        let input = project(dir.path(), &original);
        let before = fs::read(&input).unwrap();
        let copy = orca_compatible_3mf(&input, &dir.path().join("orca"))
            .unwrap()
            .expect("потрібна виправлена копія");
        assert_eq!(
            fs::read(&input).unwrap(),
            before,
            "файл користувача не змінюється"
        );
        assert_eq!(copy.file_name(), input.file_name());
        let config: Value = serde_json::from_slice(&read(&copy, PROJECT_CONFIG)).unwrap();
        for key in [
            "wall_filament",
            "sparse_infill_filament",
            "solid_infill_filament",
        ] {
            assert_eq!(config[key], json!("1"), "{key}");
        }
        assert_eq!(config["support_filament"], json!("0"));
        assert!(config.get("raft_first_layer_expansion").is_none());
        assert!(config.get("tree_support_wall_count").is_none());
        assert_eq!(config["raft_layers"], json!("0"));
        assert_eq!(read(&copy, "3D/3dmodel.model"), b"<model/>");
    }

    #[test]
    fn compatible_project_is_sliced_as_is() {
        let dir = tempfile::tempdir().unwrap();
        let input = project(dir.path(), &json!({"wall_filament": "1"}));
        assert_eq!(orca_compatible_3mf(&input, dir.path()).unwrap(), None);
    }
}
