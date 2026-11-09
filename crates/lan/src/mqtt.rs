//! MQTT принтера в LAN: `mqtts://<ip>:8883`, користувач `bblp`, пароль — код доступу.
//!
//! Звіти йдуть у `device/<sn>/report` (частковими `push_status`, повний — після `pushall`),
//! команди — у `device/<sn>/request`. Формат той самий, що й у хмарі, тому розбір і
//! побудова команд — з `cloud` (`PrinterStatus`, `merge_status`, `Control::message`).
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use cloud::{Control, ControlContext, PrinterStatus};
use rumqttc::{
    Client, ConnectReturnCode, ConnectionError, Event, MqttOptions, Packet, QoS, RecvTimeoutError,
    TlsConfiguration, Transport,
};
use serde_json::{Value, json};

use crate::{
    error::{LanError, Result},
    print::PrintJob,
    store::LanPrinter,
    tls::{self, ProblemSlot},
};

pub const MQTT_PORT: u16 = 8883;
pub const USERNAME: &str = "bblp";
/// Як `REQUEST_PUSH_MIN_TIME` в Orca: частіші `pushall` прошивка може скинути разом із сесією.
pub const PUSHALL_MIN_INTERVAL: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(12);

/// Обмежувач `pushall`: не частіше 1 разу на [`PUSHALL_MIN_INTERVAL`].
#[derive(Debug, Default)]
pub struct PushLimiter {
    last: Option<Instant>,
}
impl PushLimiter {
    pub fn allow(&mut self, now: Instant) -> bool {
        if self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < PUSHALL_MIN_INTERVAL)
        {
            return false;
        }
        self.last = Some(now);
        true
    }
}

/// Накопичений стан принтера з часткових звітів (як `cloud::Events`).
#[derive(Debug, Default)]
pub struct StatusAccumulator {
    state: Value,
}
impl StatusAccumulator {
    /// Приймає вміст `print` із `push_status`; `msg == 0` — повний знімок, стан скидається.
    pub fn apply(&mut self, patch: &Value) -> PrinterStatus {
        if patch["msg"] == 0 {
            self.state = Value::Null;
        }
        cloud::merge_status(&mut self.state, patch);
        PrinterStatus::from_json(&self.state)
    }
    pub fn clear(&mut self) {
        self.state = Value::Null;
    }
}

#[derive(Debug)]
pub enum LanEvent {
    /// MQTT-сесію встановлено (уперше або після перепідключення).
    Connected,
    Status(Box<PrinterStatus>),
    /// Будь-який інший звіт (наприклад відповідь `info/get_version`). Може містити серійні
    /// номери модулів — не логувати цілком.
    Message(Value),
    /// Принтер відповів на команду відмовою.
    Rejected(LanError),
    /// З’єднання втрачено; клієнт сам перепідключається.
    Disconnected(LanError),
}

/// Відповідь принтера з `result: fail…` → типізована помилка. Authorization Control (прошивки
/// 2025+) без Developer Mode відхиляє керування з повідомленням про перевірку підпису.
pub fn classify_reply(report: &Value) -> Option<LanError> {
    let object = report.as_object()?;
    for section in object.values() {
        let result = section["result"].as_str().unwrap_or_default();
        if !result.to_ascii_lowercase().starts_with("fail") {
            continue;
        }
        let command = section["command"].as_str().unwrap_or("?").to_owned();
        let reason = section["reason"].as_str().unwrap_or_default().to_owned();
        let lower = reason.to_ascii_lowercase();
        let locked = lower.contains("verify")
            || lower.contains("auth")
            || lower.contains("permission")
            || lower.contains("sign");
        return Some(if locked {
            LanError::ControlLocked { command, reason }
        } else {
            LanError::Protocol(format!(
                "{command}: {}",
                if reason.is_empty() { result } else { &reason }
            ))
        });
    }
    None
}

fn map_connection_error(error: &ConnectionError, problem: &ProblemSlot) -> LanError {
    if let Some(p) = problem.take() {
        return LanError::from_tls(p);
    }
    match error {
        ConnectionError::ConnectionRefused(
            ConnectReturnCode::BadUserNamePassword | ConnectReturnCode::NotAuthorized,
        ) => LanError::BadAccessCode,
        ConnectionError::ConnectionRefused(code) => {
            LanError::Protocol(format!("MQTT CONNACK {code:?}"))
        }
        ConnectionError::Io(io) => LanError::connect(MQTT_PORT, io),
        ConnectionError::Tls(rumqttc::TlsError::Io(io)) => {
            // tokio-rustls загортає помилку rustls в io::Error.
            if io.get_ref().is_some_and(|e| e.is::<rustls::Error>()) {
                LanError::UntrustedCertificate(io.to_string())
            } else {
                LanError::connect(MQTT_PORT, io)
            }
        }
        ConnectionError::Tls(other) => LanError::UntrustedCertificate(other.to_string()),
        ConnectionError::NetworkTimeout | ConnectionError::FlushTimeout => LanError::Unreachable {
            port: MQTT_PORT,
            detail: "тайм-аут".into(),
        },
        other => LanError::Protocol(other.to_string()),
    }
}

fn client_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    format!("slicenv-{:x}{:08x}", std::process::id(), nanos)
}

fn report_topic(serial: &str) -> String {
    format!("device/{serial}/report")
}

/// З’єднання з одним принтером. Команди надсилаються з будь-якого потоку; події — у каналі.
pub struct LanClient {
    client: Client,
    serial: String,
    sequence: AtomicU64,
    limiter: Mutex<PushLimiter>,
    connected: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl LanClient {
    /// Підключається й чекає CONNACK (до 12 с), підписується на звіти й просить `pushall`.
    /// Помилка тут — точна причина: невірний код, недоступний IP, чужий сертифікат.
    pub fn connect(printer: &LanPrinter) -> Result<(Self, mpsc::Receiver<LanEvent>)> {
        let (config, problem) = tls::client_config(&printer.serial);
        let mut options = MqttOptions::new(client_id(), printer.ip.to_string(), MQTT_PORT);
        options
            .set_credentials(USERNAME, printer.access_code.expose())
            .set_keep_alive(Duration::from_secs(30))
            .set_clean_session(true)
            // Повний звіт X1/H2D — десятки КБ; запас для AMS і HMS.
            .set_max_packet_size(4 << 20, 1 << 20)
            .set_transport(Transport::tls_with_config(TlsConfiguration::Rustls(config)));
        let (client, mut connection) = Client::new(options, 32);
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        loop {
            let left = deadline
                .checked_duration_since(Instant::now())
                .ok_or(LanError::Timeout("MQTT"))?;
            match connection.recv_timeout(left) {
                Ok(Ok(Event::Incoming(Packet::ConnAck(ack)))) => {
                    if ack.code == ConnectReturnCode::Success {
                        break;
                    }
                    return Err(map_connection_error(
                        &ConnectionError::ConnectionRefused(ack.code),
                        &problem,
                    ));
                }
                Ok(Ok(_)) => {}
                Ok(Err(error)) => return Err(map_connection_error(&error, &problem)),
                Err(RecvTimeoutError::Timeout) => return Err(LanError::Timeout("MQTT")),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(LanError::Protocol("MQTT: канал закрито".into()));
                }
            }
        }
        let serial = printer.serial.clone();
        client
            .subscribe(report_topic(&serial), QoS::AtMostOnce)
            .map_err(|e| LanError::Protocol(e.to_string()))?;
        let connected = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        tx.send(LanEvent::Connected).ok();
        let this = Self {
            client: client.clone(),
            serial: serial.clone(),
            sequence: AtomicU64::new(0),
            limiter: Mutex::new(PushLimiter::default()),
            connected: connected.clone(),
            stop: stop.clone(),
        };
        this.request_status()?;
        thread::Builder::new()
            .name("lan-mqtt".into())
            .spawn(move || {
                read_loop(connection, client, serial, tx, connected, stop, problem)
            })
            .map_err(LanError::from)?;
        Ok((this, rx))
    }

    pub fn serial(&self) -> &str {
        &self.serial
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    fn next_sequence(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn publish(&self, message: &Value, qos: QoS) -> Result<()> {
        if !self.is_connected() {
            return Err(LanError::Unreachable {
                port: MQTT_PORT,
                detail: "немає з’єднання".into(),
            });
        }
        self.client
            .try_publish(
                format!("device/{}/request", self.serial),
                qos,
                false,
                message.to_string().into_bytes(),
            )
            .map_err(|e| LanError::Protocol(e.to_string()))
    }

    /// `pushall` з обмеженням частоти. `Ok(false)` — запит пропущено обмежувачем.
    pub fn request_status(&self) -> Result<bool> {
        if !self
            .limiter
            .lock()
            .map(|mut l| l.allow(Instant::now()))
            .unwrap_or(false)
        {
            return Ok(false);
        }
        self.publish(&pushall_message(self.next_sequence()), QoS::AtMostOnce)?;
        Ok(true)
    }

    /// Лише читання: `info/get_version` (версії модулів, OTA).
    pub fn request_version(&self) -> Result<()> {
        self.publish(
            &json!({"info":{"command":"get_version","sequence_id":self.next_sequence().to_string()}}),
            QoS::AtMostOnce,
        )
    }

    /// Фізична дія (пауза, стоп, світло…). Межі перевіряє `Control::message`; помилка —
    /// у мережу нічого не пішло. Відмова прошивки прийде подією [`LanEvent::Rejected`].
    pub fn control(&self, control: Control, ctx: &ControlContext) -> Result<()> {
        let message = control
            .message(self.next_sequence(), ctx)
            .map_err(|e| LanError::Protocol(format!("{e:#}")))?;
        let qos = if control.qos() == 1 {
            QoS::AtLeastOnce
        } else {
            QoS::AtMostOnce
        };
        self.publish(&message, qos)
    }

    /// Команда друку вже завантаженого на принтер файлу (див. `ftps::upload`).
    pub fn start_print(&self, job: &PrintJob) -> Result<()> {
        self.publish(&job.message(self.next_sequence()), QoS::AtLeastOnce)
    }

    /// Закриває сесію; потік читання завершиться сам.
    pub fn disconnect(&self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.client.try_disconnect();
    }
}

impl Drop for LanClient {
    fn drop(&mut self) {
        self.disconnect();
    }
}

pub fn pushall_message(sequence: u64) -> Value {
    json!({"pushing":{"command":"pushall","sequence_id":sequence.to_string(),"version":1,"push_target":1}})
}

fn read_loop(
    mut connection: rumqttc::Connection,
    client: Client,
    serial: String,
    tx: mpsc::Sender<LanEvent>,
    connected: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    problem: ProblemSlot,
) {
    let mut status = StatusAccumulator::default();
    let mut backoff = Duration::from_secs(1);
    let mut reported_down = false;
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        let event = match connection.recv() {
            Ok(event) => event,
            Err(_) => break, // усі Client закриті
        };
        match event {
            Ok(Event::Incoming(Packet::ConnAck(ack))) if ack.code == ConnectReturnCode::Success => {
                // Перепідключення: clean session губить підписку — відновлюємо й просимо знімок.
                backoff = Duration::from_secs(1);
                reported_down = false;
                connected.store(true, Ordering::Release);
                status.clear();
                let _ = client.try_subscribe(report_topic(&serial), QoS::AtMostOnce);
                let _ = client.try_publish(
                    format!("device/{serial}/request"),
                    QoS::AtMostOnce,
                    false,
                    pushall_message(0).to_string().into_bytes(),
                );
                if tx.send(LanEvent::Connected).is_err() {
                    break;
                }
            }
            Ok(Event::Incoming(Packet::Publish(publish))) => {
                let Ok(report) = serde_json::from_slice::<Value>(&publish.payload) else {
                    continue;
                };
                let event = if report["print"]["command"] == "push_status" {
                    LanEvent::Status(Box::new(status.apply(&report["print"])))
                } else if let Some(error) = classify_reply(&report) {
                    LanEvent::Rejected(error)
                } else {
                    LanEvent::Message(report)
                };
                if tx.send(event).is_err() {
                    break;
                }
            }
            Ok(_) => {}
            Err(ConnectionError::RequestsDone) => break,
            Err(error) => {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                connected.store(false, Ordering::Release);
                if !reported_down {
                    reported_down = true;
                    if tx
                        .send(LanEvent::Disconnected(map_connection_error(&error, &problem)))
                        .is_err()
                    {
                        break;
                    }
                }
                // Наступний recv() перепідключиться сам; не довбаємо принтер.
                thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
    connected.store(false, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pushall_is_limited_to_once_per_three_seconds() {
        let mut limiter = PushLimiter::default();
        let t0 = Instant::now();
        assert!(limiter.allow(t0));
        assert!(!limiter.allow(t0 + Duration::from_millis(100)));
        assert!(!limiter.allow(t0 + Duration::from_millis(2999)));
        assert!(limiter.allow(t0 + Duration::from_secs(3)));
        assert!(!limiter.allow(t0 + Duration::from_secs(4)));
    }

    #[test]
    fn partial_reports_merge_and_full_snapshot_resets() {
        let mut acc = StatusAccumulator::default();
        let full = json!({"command":"push_status","msg":0,"gcode_state":"IDLE","nozzle_temper":25,
            "bed_temper":24,"home_flag":0x107,
            "ams":{"ams":[{"id":"0","tray":[{"id":"0","tray_type":"PLA"},{"id":"1","tray_type":"PETG"}]}]}});
        let s = acc.apply(&full);
        assert_eq!(s.gcode_state.as_deref(), Some("IDLE"));
        // Частковий звіт A1/P1: лише змінені поля.
        let s = acc.apply(&json!({"command":"push_status","msg":1,"nozzle_temper":180.5,
            "ams":{"ams":[{"id":"0","tray":[{"id":"1","remain":40}]}]}}));
        assert_eq!(s.nozzle_temper, Some(180.5));
        assert_eq!(s.bed_temper, Some(24.0));
        assert_eq!(s.gcode_state.as_deref(), Some("IDLE"));
        let trays = &s.ams.as_ref().unwrap().units[0].trays;
        assert_eq!(trays.len(), 2);
        assert_eq!(trays[1].tray_type.as_deref(), Some("PETG"));
        assert_eq!(trays[1].remain, Some(40.0));
        // Новий повний знімок не тягне старі поля.
        let s = acc.apply(&json!({"command":"push_status","msg":0,"gcode_state":"RUNNING"}));
        assert_eq!(s.nozzle_temper, None);
        assert_eq!(s.gcode_state.as_deref(), Some("RUNNING"));
    }

    #[test]
    fn command_wire_format() {
        assert_eq!(
            pushall_message(7),
            json!({"pushing":{"command":"pushall","sequence_id":"7","version":1,"push_target":1}})
        );
        let ctx = ControlContext::default();
        let stop = Control::Stop.message(3, &ctx).unwrap();
        assert_eq!(stop, json!({"print":{"command":"stop","param":"","sequence_id":"3"}}));
        assert_eq!(Control::Stop.qos(), 1);
        let light = Control::Light(false).message(4, &ctx).unwrap();
        assert_eq!(light["system"]["command"], "ledctrl");
        assert_eq!(light["system"]["led_mode"], "off");
    }

    #[test]
    fn rejected_replies_are_classified() {
        assert_eq!(classify_reply(&json!({"print":{"command":"pause","result":"success"}})), None);
        assert_eq!(classify_reply(&json!({"print":{"command":"push_status"}})), None);
        let locked = classify_reply(&json!({"print":{"command":"project_file","result":"failed",
            "reason":"mqtt message verify failed"}}));
        assert!(matches!(locked, Some(LanError::ControlLocked { ref command, .. }) if command == "project_file"));
        let other = classify_reply(&json!({"system":{"command":"ledctrl","result":"FAIL"}}));
        assert!(matches!(other, Some(LanError::Protocol(ref t)) if t.contains("ledctrl")));
    }
}
