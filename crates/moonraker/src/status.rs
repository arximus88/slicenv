//! Розбір відповідей Moonraker: `/server/info`, `/printer/info`, `/printer/objects/query`,
//! `/server/webcams/list`, `/server/files/list`, `/server/files/upload`.
use serde_json::Value;

use crate::error::{MoonrakerError, Result};

/// Стан Klippy (`klippy_state` у `/server/info`, `state` у `/printer/info`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KlippyState {
    Ready,
    Startup,
    Shutdown,
    Error,
    /// Moonraker живий, але з Klippy не з’єднаний.
    Disconnected,
    Other(String),
}

impl KlippyState {
    pub fn parse(text: &str) -> Self {
        match text {
            "ready" => Self::Ready,
            "startup" => Self::Startup,
            "shutdown" => Self::Shutdown,
            "error" => Self::Error,
            "disconnected" | "" => Self::Disconnected,
            other => Self::Other(other.to_owned()),
        }
    }
    pub fn as_str(&self) -> &str {
        match self {
            Self::Ready => "ready",
            Self::Startup => "startup",
            Self::Shutdown => "shutdown",
            Self::Error => "error",
            Self::Disconnected => "disconnected",
            Self::Other(s) => s,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerInfo {
    pub klippy_connected: bool,
    pub klippy_state: KlippyState,
    pub moonraker_version: String,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrinterInfo {
    pub state: KlippyState,
    pub state_message: String,
    pub hostname: String,
    pub software_version: String,
}

/// `print_stats.state`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PrintState {
    #[default]
    Standby,
    Printing,
    Paused,
    Complete,
    Cancelled,
    Error,
    Other(String),
}

impl PrintState {
    pub fn parse(text: &str) -> Self {
        match text {
            "standby" | "" => Self::Standby,
            "printing" => Self::Printing,
            "paused" => Self::Paused,
            "complete" => Self::Complete,
            "cancelled" => Self::Cancelled,
            "error" => Self::Error,
            other => Self::Other(other.to_owned()),
        }
    }
    /// Іде завдання (друк або пауза).
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Printing | Self::Paused)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Heater {
    pub temperature: Option<f64>,
    pub target: Option<f64>,
}

/// Знімок стану з `/printer/objects/query`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JobStatus {
    pub state: PrintState,
    pub filename: String,
    pub message: String,
    /// 0.0–1.0: `virtual_sdcard.progress`, інакше `display_status.progress`.
    pub progress: Option<f64>,
    /// Секунди чистого друку (без пауз).
    pub print_duration: Option<f64>,
    pub total_duration: Option<f64>,
    pub current_layer: Option<u32>,
    pub total_layer: Option<u32>,
    pub extruder: Heater,
    pub bed: Heater,
    /// `toolhead.homed_axes`, напр. `xyz`.
    pub homed_axes: String,
    /// `toolhead.position` (X, Y, Z), мм.
    pub position: Option<[f64; 3]>,
    /// `fan.speed` — вентилятор обдуву деталі, 0.0–1.0; `None`, якщо `[fan]` немає.
    pub part_fan: Option<f64>,
}

impl JobStatus {
    /// Вісь у домашній позиції (`homed_axes`).
    pub fn homed(&self, axis: crate::command::Axis) -> bool {
        self.homed_axes
            .to_ascii_lowercase()
            .contains(axis.letter().to_ascii_lowercase())
    }
}

impl JobStatus {
    /// Грубе «лишилось, с» з тривалості й прогресу (як у Fluidd без метаданих файлу).
    pub fn remaining_secs(&self) -> Option<f64> {
        let p = self.progress?;
        let spent = self.print_duration?;
        (self.state.is_active() && p > 0.01 && p < 1.0).then(|| spent / p * (1.0 - p))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Webcam {
    pub name: String,
    pub service: String,
    pub snapshot_url: String,
    pub stream_url: String,
    pub flip_horizontal: bool,
    pub flip_vertical: bool,
    pub rotation: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadResult {
    /// Шлях файлу на принтері відносно `gcodes`.
    pub path: String,
    pub print_started: bool,
    pub print_queued: bool,
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
}

/// Обгортка `{"result": …}` (або сам об’єкт для старих відповідей завантаження).
pub fn result(body: &[u8]) -> Result<Value> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|e| MoonrakerError::Protocol(format!("JSON: {e}")))?;
    Ok(match v {
        Value::Object(mut map) if map.contains_key("result") => {
            map.remove("result").unwrap_or_default()
        }
        other => other,
    })
}

/// Повідомлення з `{"error": {"code": …, "message": …}}`; інакше — початок тіла.
pub fn error_message(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| {
            let e = v.get("error")?;
            e.get("message")
                .and_then(Value::as_str)
                .or_else(|| e.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| {
            String::from_utf8_lossy(&body[..body.len().min(200)])
                .trim()
                .to_owned()
        })
}

pub fn server_info(v: &Value) -> ServerInfo {
    ServerInfo {
        klippy_connected: v
            .get("klippy_connected")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        klippy_state: KlippyState::parse(&str_of(v, "klippy_state")),
        moonraker_version: str_of(v, "moonraker_version"),
        warnings: v
            .get("warnings")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default(),
    }
}

pub fn printer_info(v: &Value) -> PrinterInfo {
    PrinterInfo {
        state: KlippyState::parse(&str_of(v, "state")),
        state_message: str_of(v, "state_message"),
        hostname: str_of(v, "hostname"),
        software_version: str_of(v, "software_version"),
    }
}

fn heater(v: Option<&Value>) -> Heater {
    let num = |k: &str| v.and_then(|o| o.get(k)).and_then(Value::as_f64);
    Heater {
        temperature: num("temperature"),
        target: num("target"),
    }
}

pub fn job_status(v: &Value) -> Result<JobStatus> {
    let status = v
        .get("status")
        .ok_or_else(|| MoonrakerError::Protocol("objects/query without status".into()))?;
    let ps = status.get("print_stats");
    let num = |o: Option<&Value>, k: &str| o.and_then(|o| o.get(k)).and_then(Value::as_f64);
    let layer = |k: &str| {
        ps.and_then(|o| o.get("info"))
            .and_then(|i| i.get(k))
            .and_then(Value::as_u64)
            .map(|n| n as u32)
    };
    let sd = status.get("virtual_sdcard");
    let display = status.get("display_status");
    let progress = num(sd, "progress")
        .filter(|p| *p > 0.0 || display.is_none())
        .or_else(|| num(display, "progress"))
        .map(|p| p.clamp(0.0, 1.0));
    Ok(JobStatus {
        state: PrintState::parse(
            ps.and_then(|o| o.get("state"))
                .and_then(Value::as_str)
                .unwrap_or(""),
        ),
        filename: ps.map(|o| str_of(o, "filename")).unwrap_or_default(),
        message: ps.map(|o| str_of(o, "message")).unwrap_or_default(),
        progress,
        print_duration: num(ps, "print_duration"),
        total_duration: num(ps, "total_duration"),
        current_layer: layer("current_layer"),
        total_layer: layer("total_layer"),
        extruder: heater(status.get("extruder")),
        bed: heater(status.get("heater_bed")),
        homed_axes: status
            .get("toolhead")
            .map(|t| str_of(t, "homed_axes"))
            .unwrap_or_default(),
        position: status
            .get("toolhead")
            .and_then(|t| t.get("position"))
            .and_then(Value::as_array)
            .and_then(|p| Some([p.first()?.as_f64()?, p.get(1)?.as_f64()?, p.get(2)?.as_f64()?])),
        part_fan: num(status.get("fan"), "speed").map(|f| f.clamp(0.0, 1.0)),
    })
}

pub fn webcams(v: &Value) -> Vec<Webcam> {
    v.get("webcams")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter(|w| w.get("enabled").and_then(Value::as_bool).unwrap_or(true))
                .map(|w| Webcam {
                    name: str_of(w, "name"),
                    service: str_of(w, "service"),
                    snapshot_url: str_of(w, "snapshot_url"),
                    stream_url: str_of(w, "stream_url"),
                    flip_horizontal: w.get("flip_horizontal").and_then(Value::as_bool).unwrap_or(false),
                    flip_vertical: w.get("flip_vertical").and_then(Value::as_bool).unwrap_or(false),
                    rotation: w.get("rotation").and_then(Value::as_u64).unwrap_or(0) as u16,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Шляхи файлів у `gcodes` (`/server/files/list`: масив об’єктів з `path` чи `filename`).
pub fn file_list(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|f| {
                    f.get("path")
                        .or_else(|| f.get("filename"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn upload_result(v: &Value) -> Result<UploadResult> {
    let path = v
        .get("item")
        .and_then(|i| i.get("path"))
        .and_then(Value::as_str)
        .ok_or_else(|| MoonrakerError::Protocol("upload response without item.path".into()))?;
    let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    Ok(UploadResult {
        path: path.to_owned(),
        print_started: flag("print_started"),
        print_queued: flag("print_queued"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn objects_query_is_parsed_with_fallbacks() {
        let v = json!({"eventtime": 1.0, "status": {
            "print_stats": {"filename": "cube.gcode", "state": "printing", "message": "",
                "print_duration": 600.0, "total_duration": 650.0,
                "info": {"current_layer": 12, "total_layer": 120}},
            "virtual_sdcard": {"progress": 0.25, "is_active": true},
            "display_status": {"progress": 0.24, "message": null},
            "extruder": {"temperature": 214.8, "target": 215.0, "power": 0.4},
            "heater_bed": {"temperature": 59.9, "target": 60.0},
            "toolhead": {"homed_axes": "xyz", "position": [10.0, 20.5, 0.2, 100.0]},
            "fan": {"speed": 0.5, "rpm": null}
        }});
        let s = job_status(&v).unwrap();
        assert_eq!(s.state, PrintState::Printing);
        assert_eq!(s.filename, "cube.gcode");
        assert_eq!(s.progress, Some(0.25));
        assert_eq!((s.current_layer, s.total_layer), (Some(12), Some(120)));
        assert_eq!(s.extruder.target, Some(215.0));
        assert_eq!(s.bed.temperature, Some(59.9));
        assert_eq!(s.homed_axes, "xyz");
        assert!(s.homed(crate::command::Axis::Z));
        assert_eq!(s.position, Some([10.0, 20.5, 0.2]));
        assert_eq!(s.part_fan, Some(0.5));
        assert_eq!(s.remaining_secs(), Some(1800.0));

        // Без heater_bed і virtual_sdcard (принтер без столу, старий Klipper): поля порожні.
        let v = json!({"status": {"print_stats": {"state": "standby", "info": {"current_layer": null}},
            "display_status": {"progress": 0.0}}});
        let s = job_status(&v).unwrap();
        assert_eq!(s.state, PrintState::Standby);
        assert_eq!(s.bed, Heater::default());
        assert_eq!(s.current_layer, None);
        assert_eq!(s.remaining_secs(), None);
        assert_eq!((s.position, s.part_fan), (None, None));
        assert!(!s.homed(crate::command::Axis::X));
        assert!(job_status(&json!({})).is_err());
    }

    #[test]
    fn info_errors_and_lists() {
        let s = server_info(&json!({"klippy_connected": true, "klippy_state": "shutdown",
            "moonraker_version": "v0.9.3", "warnings": ["w"]}));
        assert_eq!(s.klippy_state, KlippyState::Shutdown);
        assert_eq!(s.warnings, vec!["w".to_owned()]);
        assert_eq!(KlippyState::parse("weird"), KlippyState::Other("weird".into()));
        assert_eq!(error_message(br#"{"error":{"code":401,"message":"Unauthorized"}}"#), "Unauthorized");
        assert_eq!(error_message(b"plain"), "plain");
        assert_eq!(result(br#"{"result":"ok"}"#).unwrap(), json!("ok"));
        assert_eq!(
            file_list(&json!([{"path": "a.gcode"}, {"filename": "b/c.gcode"}, {"x": 1}])),
            vec!["a.gcode".to_owned(), "b/c.gcode".to_owned()]
        );
        let cams = webcams(&json!({"webcams": [
            {"name": "cam", "enabled": true, "service": "mjpegstreamer",
             "snapshot_url": "/webcam/?action=snapshot", "stream_url": "/webcam/?action=stream", "rotation": 90},
            {"name": "off", "enabled": false}
        ]}));
        assert_eq!(cams.len(), 1);
        assert_eq!(cams[0].rotation, 90);
        let up = upload_result(&json!({"item": {"path": "x.gcode", "root": "gcodes"},
            "print_started": true, "action": "create_file"})).unwrap();
        assert!(up.print_started && !up.print_queued);
        assert!(upload_result(&json!({"action": "create_file"})).is_err());
    }
}
