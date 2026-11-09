//! Історія завдань: невідомі поля та значення лишаються в початковому JSON.
use crate::{Client, DeviceId, JsonPayload};
use anyhow::{Result, ensure};
use serde_json::Value;
use std::fmt;

#[derive(Clone, Debug)]
pub struct TaskQuery {
    pub dev_id: Option<DeviceId>,
    /// 0 — усі; інші значення передаються плагіну без змін.
    pub status: i32,
    pub offset: i32,
    pub limit: i32,
}
impl Default for TaskQuery {
    fn default() -> Self {
        Self {
            dev_id: None,
            status: 0,
            offset: 0,
            limit: 20,
        }
    }
}

/// Ідентифікатори, назви й URL доступні явно; Debug їх не друкує.
#[derive(Clone)]
pub struct Task {
    pub id: String,
    pub title: Option<String>,
    pub design_name: Option<String>,
    /// 1/4 — виконується, 2 — успіх, 3 — невдача; невідомі коди збережено.
    pub status: Option<i32>,
    pub dev_id: Option<DeviceId>,
    /// Час у форматі сервера; часовий пояс не вгадується.
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub cost_time: Option<f64>,
    pub weight: Option<f64>,
    pub plate_index: Option<i32>,
    pub cover_url: Option<String>,
    pub raw: JsonPayload,
}
impl fmt::Debug for Task {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Task")
            .field("status", &self.status)
            .field("dev_id", &self.dev_id)
            .finish_non_exhaustive()
    }
}
fn text(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::to_owned)
        .or_else(|| v.as_number().map(ToString::to_string))
}
fn number(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str()?.parse().ok())
        .filter(|n| n.is_finite())
}
fn integer(v: &Value) -> Option<i32> {
    v.as_i64()
        .and_then(|n| n.try_into().ok())
        .or_else(|| v.as_str()?.parse().ok())
}
impl Task {
    fn parse(v: Value) -> Result<Self> {
        let id =
            text(&v["id"]).ok_or_else(|| anyhow::anyhow!("Завдання без id (JSON приховано)"))?;
        Ok(Self {
            id,
            title: text(&v["title"]),
            design_name: text(&v["designTitle"]),
            status: integer(&v["status"]),
            dev_id: text(&v["deviceId"]).map(DeviceId::new),
            start_time: text(&v["startTime"]),
            end_time: text(&v["endTime"]),
            cost_time: number(&v["costTime"]),
            weight: number(&v["weight"]),
            plate_index: integer(&v["plateIndex"]),
            cover_url: text(&v["cover"]),
            raw: JsonPayload(v),
        })
    }
}
fn parse_tasks(body: &str) -> Result<Vec<Task>> {
    let v: Value = serde_json::from_str(body)
        .map_err(|_| anyhow::anyhow!("Некоректна історія завдань (JSON приховано)"))?;
    let hits = v["hits"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Історія не містить hits (JSON приховано)"))?;
    hits.iter().cloned().map(Task::parse).collect()
}
/// Номер пластини й URL мініатюри з відповіді `get_subtask_info`
/// (`content.info.plate_idx`, `context.plates[].{index,thumbnail.url}`), як в
/// `MachineObject::update_slice_info` у Studio. Якщо номер пластини невідомий, а пластина
/// в контексті лише одна — беремо її; інакше `None` (мініатюру не вгадуємо).
pub fn plate_thumbnail(info: &Value) -> Option<String> {
    let plate = info["content"]
        .as_str()
        .and_then(|c| serde_json::from_str::<Value>(c).ok())
        .and_then(|c| c["info"]["plate_idx"].as_i64());
    let plates = info["context"]["plates"].as_array()?;
    let chosen = match plate {
        Some(index) => plates.iter().find(|p| p["index"].as_i64() == Some(index))?,
        None if plates.len() == 1 => &plates[0],
        None => return None,
    };
    chosen["thumbnail"]["url"]
        .as_str()
        .filter(|u| !u.is_empty())
        .map(str::to_owned)
}

impl Client {
    /// Синхронний REST-запит; список принтерів і MQTT-підписка не потрібні.
    pub fn get_user_tasks(&self, query: &TaskQuery) -> Result<Vec<Task>> {
        ensure!(
            self.started.get() && self.is_user_login()?,
            "Потрібна відновлена сесія та start()"
        );
        ensure!(
            query.offset >= 0 && (1..=100).contains(&query.limit) && query.status >= 0,
            "Некоректні параметри історії (limit: 1..=100, offset/status >= 0)"
        );
        let body = self.call(
            18,
            query.dev_id.as_ref().map_or("", DeviceId::as_str),
            &format!("{}:{}", query.status, query.limit),
            query.offset,
        )?;
        parse_tasks(&body)
    }
    /// Деталі підзавдання без виводу HTTP-відповіді або ідентифікатора в помилках.
    pub fn get_subtask_info(&self, subtask_id: &str) -> Result<JsonPayload> {
        ensure!(
            self.started.get() && self.is_user_login()?,
            "Потрібна відновлена сесія та start()"
        );
        ensure!(!subtask_id.is_empty(), "Порожній id підзавдання");
        let body = self.call(19, subtask_id, "", 0)?;
        serde_json::from_str(&body)
            .map(JsonPayload)
            .map_err(|_| anyhow::anyhow!("Некоректні деталі підзавдання (JSON приховано)"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn thumbnail_is_matched_to_the_plate_of_the_job() {
        use serde_json::json;
        let info = json!({
            "content": "{\"info\":{\"plate_idx\":2}}",
            "context": {"plates": [
                {"index": 1, "thumbnail": {"url": "https://x/one.png"}},
                {"index": 2, "thumbnail": {"url": "https://x/two.png"}}
            ]}
        });
        assert_eq!(plate_thumbnail(&info).as_deref(), Some("https://x/two.png"));
        // Номер пластини відсутній, а пластин кілька — не вгадуємо.
        let ambiguous = json!({"context": {"plates": info["context"]["plates"].clone()}});
        assert_eq!(plate_thumbnail(&ambiguous), None);
        let single =
            json!({"context": {"plates": [{"index": 5, "thumbnail": {"url": "https://x/s.png"}}]}});
        assert_eq!(plate_thumbnail(&single).as_deref(), Some("https://x/s.png"));
        // Пластини з таким номером немає.
        let missing =
            json!({"content": "{\"info\":{\"plate_idx\":9}}", "context": info["context"].clone()});
        assert_eq!(plate_thumbnail(&missing), None);
        assert_eq!(plate_thumbnail(&json!({})), None);
    }

    #[test]
    fn sparse_and_mixed_tasks_keep_raw_without_debug_leaks() {
        let tasks = parse_tasks(r#"{"hits":[{"id":123,"title":"secret-name","designTitle":"secret-design","deviceId":"PRIVATE12345678","status":99,"costTime":"12.5","weight":1.2,"plateIndex":"2","cover":"secret-url","unknown":true},{"id":"456"}]}"#).unwrap();
        assert_eq!(tasks[0].id, "123");
        assert_eq!(tasks[0].status, Some(99));
        assert_eq!(tasks[0].cost_time, Some(12.5));
        assert_eq!(tasks[0].plate_index, Some(2));
        assert_eq!(tasks[0].raw.as_value()["unknown"], true);
        assert!(tasks[1].weight.is_none());
        let debug = format!("{tasks:?}");
        for secret in ["secret-name", "secret-design", "PRIVATE", "secret-url"] {
            assert!(!debug.contains(secret));
        }
        for invalid in ["{", r#"{"error":"private"}"#, r#"{"hits":[{}]}"#] {
            assert!(parse_tasks(invalid).is_err());
        }
    }
}
