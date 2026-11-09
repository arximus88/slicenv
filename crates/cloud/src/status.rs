//! Поля push_status; відсутнє поле не означає нуль.
use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrinterStatus {
    pub gcode_state: Option<String>,
    pub mc_percent: Option<f64>,
    /// Хвилини, як у MQTT Bambu.
    pub mc_remaining_time: Option<f64>,
    pub layer_num: Option<u32>,
    pub total_layer_num: Option<u32>,
    pub nozzle_temper: Option<f64>,
    pub nozzle_target_temper: Option<f64>,
    pub bed_temper: Option<f64>,
    pub bed_target_temper: Option<f64>,
    pub print_error: Option<i64>,
    pub spd_lvl: Option<u32>,
    pub wifi_signal: Option<String>,
    pub lights: Vec<Light>,
    pub ams: Option<AmsStatus>,
    pub vt_tray: Option<Tray>,
    pub chamber_temper: Option<f64>,
    pub fans: Fans,
    /// Активні повідомлення HMS (порожньо, якщо принтер їх не надсилав).
    pub hms: Vec<HmsItem>,
    /// Біти 0..2 — X/Y/Z у домашній позиції; біти 8..9 — стан SD (як `parse_home_flag` у Studio).
    pub home_flag: Option<u32>,
    /// Шістнадцятковий рядок можливостей `fun` (див. `Capabilities::from_fun`).
    pub fun: Option<String>,
    /// Шістнадцятковий рядок `aux`; біти 12..13 — стан SD, якщо `home_flag` відсутній.
    pub aux: Option<String>,
    pub print_type: Option<String>,
    pub gcode_file: Option<String>,
    pub subtask_name: Option<String>,
    pub task_id: Option<String>,
    pub subtask_id: Option<String>,
    pub project_id: Option<String>,
    pub profile_id: Option<String>,
    pub job_id: Option<String>,
    pub upgrade: Option<UpgradeState>,
}

/// Швидкості вентиляторів у відсотках; `None` — принтер не повідомив.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fans {
    pub part: Option<u8>,
    pub aux: Option<u8>,
    pub chamber: Option<u8>,
    pub heatbreak: Option<u8>,
}

/// Одне повідомлення HMS: `attr`/`code` у вигляді, в якому їх шле принтер.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HmsItem {
    pub attr: u32,
    pub code: u32,
}
impl HmsItem {
    pub fn module_id(self) -> u8 {
        (self.attr >> 24) as u8
    }
    pub fn module_num(self) -> u8 {
        (self.attr >> 16) as u8
    }
    pub fn part_id(self) -> u8 {
        (self.attr >> 8) as u8
    }
    /// 1 fatal, 2 serious, 3 common, 4 info; решта — невідомо (0).
    pub fn level(self) -> u8 {
        let level = self.code >> 16;
        if (1..=4).contains(&level) {
            level as u8
        } else {
            0
        }
    }
    pub fn message_code(self) -> u16 {
        self.code as u16
    }
    /// 16 hex-цифр, як `DevHMSItem::get_long_error_code` у Studio.
    pub fn long_code(self) -> String {
        format!(
            "{:02X}{:02X}{:02X}{:02X}00{:02X}{:04X}",
            self.module_id(),
            self.module_num(),
            self.part_id(),
            self.attr as u8,
            self.level(),
            self.message_code()
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SdCard {
    None,
    Normal,
    Abnormal,
    ReadOnly,
}

/// Поле `upgrade_state` із push_status; нічого не вгадується, відсутнє — `None`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UpgradeState {
    pub status: Option<String>,
    pub progress: Option<String>,
    pub message: Option<String>,
    pub module: Option<String>,
    /// 0 немає, 1 доступне, 2 триває, 3 завершено (`DevFirmwareUpgradeState`).
    pub display_state: Option<i64>,
    pub ota_new_version: Option<String>,
    pub new_versions: Vec<NewVersion>,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NewVersion {
    pub name: String,
    pub current: Option<String>,
    pub new: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Light {
    pub node: String,
    pub mode: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AmsStatus {
    pub ams_exist_bits: Option<String>,
    pub tray_now: Option<String>,
    pub tray_tar: Option<String>,
    pub tray_pre: Option<String>,
    pub units: Vec<AmsUnit>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AmsUnit {
    pub id: Option<String>,
    pub humidity: Option<String>,
    pub temp: Option<f64>,
    pub trays: Vec<Tray>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tray {
    pub id: Option<String>,
    pub tray_type: Option<String>,
    pub tray_color: Option<String>,
    pub remain: Option<f64>,
    pub nozzle_temp_min: Option<f64>,
    pub nozzle_temp_max: Option<f64>,
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}
fn number(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str()?.parse().ok())
}
fn uint(v: &Value) -> Option<u32> {
    v.as_u64()
        .and_then(|v| v.try_into().ok())
        .or_else(|| v.as_str()?.parse().ok())
}
fn array(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or_default()
}

impl Tray {
    fn parse(v: &Value) -> Self {
        Self {
            id: text(&v["id"]),
            tray_type: text(&v["tray_type"]),
            tray_color: text(&v["tray_color"]),
            remain: number(&v["remain"]),
            nozzle_temp_min: number(&v["nozzle_temp_min"]),
            nozzle_temp_max: number(&v["nozzle_temp_max"]),
        }
    }
}
/// Стара шкала Bambu: 0..=15 → кроки по 1.5 → 0..=10 → відсотки.
fn fan_legacy(v: &Value) -> Option<u8> {
    let raw = number(v)?;
    (raw >= 0.0).then(|| (((raw / 1.5).floor() * 10.0) as u32).min(100) as u8)
}
/// `fan_gear`: три байти 0..=255 (part | big1 << 8 | big2 << 16).
fn fan_byte(gear: u32, shift: u32) -> u8 {
    let byte = (gear >> shift) & 0xFF;
    ((byte as f64) * 100.0 / 255.0).round() as u8
}
fn hex_bits(text: &str, start: u32, count: u32) -> Option<u32> {
    let value = u64::from_str_radix(text.trim().trim_start_matches("0x"), 16).ok()?;
    Some(((value >> start) & ((1 << count) - 1)) as u32)
}
fn opt_text(v: &Value) -> Option<String> {
    text(v).filter(|s| !s.is_empty())
}

impl PrinterStatus {
    /// Стан SD за `home_flag` (біти 8..9), інакше за `aux` (біти 12..13). `None` — невідомо.
    pub fn sd_card(&self) -> Option<SdCard> {
        let raw = self
            .home_flag
            .map(|flag| (flag >> 8) & 0b11)
            .or_else(|| hex_bits(self.aux.as_deref()?, 12, 2))?;
        Some(match raw {
            0 => SdCard::None,
            1 => SdCard::Normal,
            2 => SdCard::Abnormal,
            _ => SdCard::ReadOnly,
        })
    }
    /// Вісь у домашній позиції: 0 = X, 1 = Y, 2 = Z. `home_flag == 0` Studio трактує як «так».
    pub fn axis_homed(&self, axis: usize) -> Option<bool> {
        let flag = self.home_flag?;
        Some(flag == 0 || (flag >> axis) & 1 == 1)
    }

    pub fn from_json(v: &Value) -> Self {
        Self {
            gcode_state: text(&v["gcode_state"]),
            mc_percent: number(&v["mc_percent"]),
            mc_remaining_time: number(&v["mc_remaining_time"]),
            layer_num: uint(&v["layer_num"]),
            total_layer_num: uint(&v["total_layer_num"]),
            nozzle_temper: number(&v["nozzle_temper"]),
            nozzle_target_temper: number(&v["nozzle_target_temper"]),
            bed_temper: number(&v["bed_temper"]),
            bed_target_temper: number(&v["bed_target_temper"]),
            print_error: v["print_error"]
                .as_i64()
                .or_else(|| v["print_error"].as_str()?.parse().ok()),
            spd_lvl: uint(&v["spd_lvl"]),
            wifi_signal: text(&v["wifi_signal"]),
            lights: array(&v["lights_report"])
                .iter()
                .map(|l| Light {
                    node: text(&l["node"]).unwrap_or_default(),
                    mode: text(&l["mode"]).unwrap_or_default(),
                })
                .collect(),
            ams: v.get("ams").filter(|a| a.is_object()).map(|a| AmsStatus {
                ams_exist_bits: text(&a["ams_exist_bits"]),
                tray_now: text(&a["tray_now"]),
                tray_tar: text(&a["tray_tar"]),
                tray_pre: text(&a["tray_pre"]),
                units: array(&a["ams"])
                    .iter()
                    .map(|u| AmsUnit {
                        id: text(&u["id"]),
                        humidity: text(&u["humidity"]),
                        temp: number(&u["temp"]),
                        trays: array(&u["tray"]).iter().map(Tray::parse).collect(),
                    })
                    .collect(),
            }),
            vt_tray: v.get("vt_tray").filter(|t| t.is_object()).map(Tray::parse),
            chamber_temper: number(&v["chamber_temper"]),
            fans: Fans::parse(v),
            hms: array(&v["hms"])
                .iter()
                .filter_map(|h| {
                    Some(HmsItem {
                        attr: h["attr"].as_u64()?.try_into().ok()?,
                        code: h["code"].as_u64()?.try_into().ok()?,
                    })
                })
                .collect(),
            home_flag: v["home_flag"].as_u64().and_then(|f| f.try_into().ok()),
            fun: opt_text(&v["fun"]),
            aux: opt_text(&v["aux"]),
            print_type: opt_text(&v["print_type"]),
            gcode_file: opt_text(&v["gcode_file"]),
            subtask_name: opt_text(&v["subtask_name"]),
            task_id: opt_text(&v["task_id"]),
            subtask_id: opt_text(&v["subtask_id"]),
            project_id: opt_text(&v["project_id"]),
            profile_id: opt_text(&v["profile_id"]),
            job_id: opt_text(&v["job_id"]),
            upgrade: v
                .get("upgrade_state")
                .filter(|u| u.is_object())
                .map(UpgradeState::parse),
        }
    }
}

impl Fans {
    fn parse(v: &Value) -> Self {
        if let Some(gear) = v["fan_gear"].as_u64().and_then(|g| u32::try_from(g).ok()) {
            return Self {
                part: Some(fan_byte(gear, 0)),
                aux: Some(fan_byte(gear, 8)),
                chamber: Some(fan_byte(gear, 16)),
                heatbreak: fan_legacy(&v["heatbreak_fan_speed"]),
            };
        }
        Self {
            part: fan_legacy(&v["cooling_fan_speed"]),
            aux: fan_legacy(&v["big_fan1_speed"]),
            chamber: fan_legacy(&v["big_fan2_speed"]),
            heatbreak: fan_legacy(&v["heatbreak_fan_speed"]),
        }
    }
}
impl UpgradeState {
    fn parse(v: &Value) -> Self {
        Self {
            status: opt_text(&v["status"]),
            progress: opt_text(&v["progress"]),
            message: opt_text(&v["message"]),
            module: opt_text(&v["module"]),
            display_state: v["dis_state"].as_i64(),
            ota_new_version: opt_text(&v["ota_new_version_number"]),
            new_versions: array(&v["new_ver_list"])
                .iter()
                .map(|n| NewVersion {
                    name: text(&n["name"]).unwrap_or_default(),
                    current: opt_text(&n["cur_ver"]),
                    new: opt_text(&n["new_ver"]),
                })
                .collect(),
        }
    }
}

/// Зливає частковий `push_status` у накопичений стан. Масиви AMS/лотків приходять
/// частково; зливаємо за id (лампи — за node). Спільне для хмари й LAN (той самий формат).
pub fn merge(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(a), Value::Object(b)) => {
            for (key, value) in b {
                merge(a.entry(key).or_insert(Value::Null), value);
            }
        }
        (Value::Array(a), Value::Array(b))
            if !b.is_empty()
                && b.iter()
                    .all(|v| v.get("id").is_some() || v.get("node").is_some()) =>
        {
            for value in b {
                let key = if value.get("id").is_some() {
                    "id"
                } else {
                    "node"
                };
                if let Some(existing) = a.iter_mut().find(|v| v[key] == value[key]) {
                    merge(existing, value);
                } else {
                    a.push(value.clone());
                }
            }
        }
        (base, patch) => *base = patch.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sparse_status_preserves_temperatures_and_other_trays() {
        let mut full = json!({"nozzle_temper":210,"print_error":"0","ams":{"tray_now":"0","ams":[{"id":"0","humidity":"3","tray":[{"id":"0","tray_type":"PLA"},{"id":"1","tray_type":"PETG"}]}]}});
        merge(
            &mut full,
            &json!({"mc_percent":22,"ams":{"ams":[{"id":"0","tray":[{"id":"0","remain":74}]}]}}),
        );
        let status = PrinterStatus::from_json(&full);
        assert_eq!(status.nozzle_temper, Some(210.0));
        assert_eq!(status.mc_percent, Some(22.0));
        assert_eq!(status.print_error, Some(0));
        let unit = &status.ams.unwrap().units[0];
        assert_eq!(unit.trays.len(), 2);
        assert_eq!(unit.trays[0].tray_type.as_deref(), Some("PLA"));
        assert_eq!(unit.trays[0].remain, Some(74.0));
    }

    #[test]
    fn job_hms_fans_and_storage_fields_parse_from_synthetic_snapshot() {
        let status = PrinterStatus::from_json(&json!({
            "gcode_state":"RUNNING","subtask_id":"123","task_id":"77","project_id":"9","profile_id":"8",
            "subtask_name":"Part","gcode_file":"Metadata/plate_1.gcode","job_id":55,
            "cooling_fan_speed":"15","big_fan1_speed":"0","big_fan2_speed":"6","heatbreak_fan_speed":"15",
            "home_flag": 0x105,
            "hms":[{"attr":0x0300_0100u64,"code":0x0002_0003u64},{"attr":"bad"}],
            "upgrade_state":{"status":"IDLE","dis_state":1,"ota_new_version_number":"01.09","new_ver_list":[{"name":"ota","cur_ver":"01.08","new_ver":"01.09"}]},
            "chamber_temper": 31.5
        }));
        assert_eq!(status.subtask_id.as_deref(), Some("123"));
        assert_eq!(status.job_id.as_deref(), Some("55"));
        assert_eq!(status.fans.part, Some(100));
        assert_eq!(status.fans.aux, Some(0));
        assert_eq!(status.fans.chamber, Some(40));
        assert_eq!(status.sd_card(), Some(SdCard::Normal));
        assert_eq!(status.axis_homed(0), Some(true));
        assert_eq!(status.axis_homed(1), Some(false));
        assert_eq!(status.axis_homed(2), Some(true));
        assert_eq!(status.hms.len(), 1);
        let item = status.hms[0];
        assert_eq!(
            (item.module_id(), item.level(), item.message_code()),
            (3, 2, 3)
        );
        assert_eq!(item.long_code(), "0300010000020003");
        let upgrade = status.upgrade.unwrap();
        assert_eq!(upgrade.display_state, Some(1));
        assert_eq!(upgrade.new_versions[0].new.as_deref(), Some("01.09"));
        assert_eq!(status.chamber_temper, Some(31.5));
    }

    #[test]
    fn fan_gear_overrides_legacy_and_unknown_stays_none() {
        let gear = PrinterStatus::from_json(&json!({"fan_gear": 0x0080_FF00u32}));
        assert_eq!(gear.fans.part, Some(0));
        assert_eq!(gear.fans.aux, Some(100));
        assert_eq!(gear.fans.chamber, Some(50));
        let none = PrinterStatus::from_json(&json!({}));
        assert_eq!(none.fans, Fans::default());
        assert_eq!(none.sd_card(), None);
        assert_eq!(none.axis_homed(0), None);
        // aux — запасне джерело стану SD, коли home_flag відсутній.
        let aux = PrinterStatus::from_json(&json!({"aux":"3000"}));
        assert_eq!(aux.sd_card(), Some(SdCard::ReadOnly));
    }

    #[test]
    fn absent_or_malformed_fields_stay_unknown() {
        let status = PrinterStatus::from_json(
            &json!({"nozzle_temper":"unknown", "layer_num":"9", "vt_tray":{"id":254,"tray_type":"PLA"},"lights_report":[{"node":"chamber_light","mode":"on"}]}),
        );
        assert_eq!(status.nozzle_temper, None);
        assert_eq!(status.layer_num, Some(9));
        assert_eq!(status.vt_tray.unwrap().id.as_deref(), Some("254"));
        assert_eq!(status.lights[0].mode, "on");
    }
}
