//! Команда друку `print/project_file` для файлу, уже завантаженого на принтер через FTPS.
//!
//! Поля — за OpenBambuAPI (`mqtt.md`, `print.project_file`) і ha-bambulab (`commands.py`);
//! сам JSON у Studio/Orca формує закритий плагін, тож назви звірені з відкритими клієнтами,
//! а смисл прапорців — з `PrintParams` в `bambu_networking.hpp` (task_bed_leveling тощо).
//! MD5 — верхнім регістром, як `bbl_calc_md5` у Studio.
use std::{fs::File, io::Read as _, path::Path};

use md5::{Digest as _, Md5};
use serde_json::{Value, json};

use crate::error::{LanError, Result};

#[derive(Clone, Debug, PartialEq)]
pub struct PrintJob {
    /// Номер пластини, з 1 (`Metadata/plate_<N>.gcode` усередині .gcode.3mf).
    pub plate: u32,
    /// Ім’я файлу в корені FTP принтера (на A1/P1 — корінь microSD).
    pub remote_name: String,
    /// MD5 файлу, 32 шістнадцяткові цифри верхнім регістром.
    pub md5: String,
    pub subtask_name: String,
    pub use_ams: bool,
    /// Для кожного філаменту проєкту — слот AMS (0..), `-1` — зовнішня котушка / без AMS.
    pub ams_mapping: Vec<i32>,
    /// `textured_plate`, `cool_plate`… або `auto`.
    pub bed_type: String,
    pub bed_leveling: bool,
    pub flow_cali: bool,
    pub vibration_cali: bool,
    pub layer_inspect: bool,
    pub timelapse: bool,
}

impl PrintJob {
    pub fn message(&self, sequence: u64) -> Value {
        json!({"print":{
            "sequence_id": sequence.to_string(),
            "command": "project_file",
            "param": format!("Metadata/plate_{}.gcode", self.plate.max(1)),
            "url": format!("ftp:///{}", self.remote_name),
            "md5": self.md5,
            // Для локального друку — завжди "0" (OpenBambuAPI).
            "project_id": "0",
            "profile_id": "0",
            "task_id": "0",
            "subtask_id": "0",
            "subtask_name": self.subtask_name,
            "use_ams": self.use_ams,
            "ams_mapping": self.ams_mapping,
            "bed_type": if self.bed_type.is_empty() { "auto" } else { &self.bed_type },
            "bed_leveling": self.bed_leveling,
            "flow_cali": self.flow_cali,
            "vibration_cali": self.vibration_cali,
            "layer_inspect": self.layer_inspect,
            "timelapse": self.timelapse,
        }})
    }
}

/// MD5 файлу верхнім регістром.
pub fn md5_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect())
}

/// Безпечне ім’я на принтері: латиниця, цифри, `-`, `_`, `.`; завжди `.gcode.3mf`.
/// Прошивка показує його в списку файлів, тож кирилиця стає `_`.
pub fn remote_name(project: &str) -> String {
    let stem = project
        .trim()
        .trim_end_matches(".3mf")
        .trim_end_matches(".gcode");
    let mut name: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .take(60)
        .collect();
    let name_trimmed = name.trim_matches(['.', '_']).to_owned();
    name = if name_trimmed.is_empty() {
        "slicenv".into()
    } else {
        name_trimmed
    };
    format!("{name}.gcode.3mf")
}

/// `"[-1,-1]"` (рядок для хмарного `PrintRequest`) → вектор для LAN.
pub fn parse_ams_mapping(text: &str) -> Result<Vec<i32>> {
    if text.trim().is_empty() {
        return Ok(vec![-1]);
    }
    serde_json::from_str::<Vec<i32>>(text).map_err(|_| LanError::Invalid("ams_mapping"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job() -> PrintJob {
        PrintJob {
            plate: 2,
            remote_name: "Door_D.gcode.3mf".into(),
            md5: "D41D8CD98F00B204E9800998ECF8427E".into(),
            subtask_name: "Door D".into(),
            use_ams: false,
            ams_mapping: vec![-1, -1],
            bed_type: "textured_plate".into(),
            bed_leveling: true,
            flow_cali: false,
            vibration_cali: true,
            layer_inspect: false,
            timelapse: false,
        }
    }

    #[test]
    fn project_file_message_has_all_fields() {
        let m = job().message(42);
        let p = &m["print"];
        assert_eq!(p["command"], "project_file");
        assert_eq!(p["sequence_id"], "42");
        assert_eq!(p["param"], "Metadata/plate_2.gcode");
        assert_eq!(p["url"], "ftp:///Door_D.gcode.3mf");
        assert_eq!(p["md5"], "D41D8CD98F00B204E9800998ECF8427E");
        assert_eq!(p["ams_mapping"], json!([-1, -1]));
        assert_eq!(p["use_ams"], false);
        assert_eq!(p["bed_type"], "textured_plate");
        assert_eq!(p["bed_leveling"], true);
        assert_eq!(p["flow_cali"], false);
        assert_eq!(p["vibration_cali"], true);
        for id in ["project_id", "profile_id", "task_id", "subtask_id"] {
            assert_eq!(p[id], "0");
        }
        let mut auto = job();
        auto.bed_type.clear();
        auto.plate = 0;
        let a = auto.message(1);
        assert_eq!(a["print"]["bed_type"], "auto");
        assert_eq!(a["print"]["param"], "Metadata/plate_1.gcode");
    }

    #[test]
    fn md5_is_uppercase_like_studio() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x");
        std::fs::write(&path, b"").unwrap();
        assert_eq!(md5_file(&path).unwrap(), "D41D8CD98F00B204E9800998ECF8427E");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(md5_file(&path).unwrap(), "900150983CD24FB0D6963F7D28E17F72");
    }

    #[test]
    fn remote_names_are_safe() {
        assert_eq!(remote_name("Door D"), "Door_D.gcode.3mf");
        assert_eq!(remote_name("Дверцята"), "slicenv.gcode.3mf");
        assert_eq!(remote_name("part.gcode.3mf"), "part.gcode.3mf");
        assert_eq!(remote_name("../../etc/passwd"), "etc_passwd.gcode.3mf");
        assert!(remote_name(&"x".repeat(200)).len() <= 70);
    }

    #[test]
    fn ams_mapping_from_cloud_string() {
        assert_eq!(parse_ams_mapping("[-1,-1,2]").unwrap(), vec![-1, -1, 2]);
        assert_eq!(parse_ams_mapping("").unwrap(), vec![-1]);
        assert!(parse_ams_mapping("[{\"x\":1}]").is_err());
    }
}
