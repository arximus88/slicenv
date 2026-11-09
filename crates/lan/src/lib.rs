//! Режим LAN для принтерів Bambu Lab на чистому Rust: без `libbambu_networking.so` і без
//! акаунта Bambu. Протоколи, обмеження прошивок і що перевірено наживо — docs/LAN.md.
//!
//! API для майстра першого запуску:
//! 1. [`discovery::discover`] (або фоновий [`discovery::Discovery`]) — принтери в мережі;
//! 2. [`LanPrinter::new`] — дані з виявлення чи ручного вводу (IP, серійник, код);
//! 3. [`test_connection`] — перевірка з’єднання з кодом доступу (лише читання);
//! 4. [`store::upsert`] — зберегти (файл 0600 у `~/.config/slicenv`).
//!
//! Робота з принтером: [`LanClient`] (статус, керування, команда друку),
//! [`ftps::upload`] (файл на принтер), [`camera::CameraStream`] (A1/P1).
pub mod camera;
pub mod discovery;
pub mod error;
pub mod ftps;
pub mod mqtt;
pub mod print;
pub mod store;
pub mod tls;

use std::{
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

pub use cloud::{PrinterStatus, SdCard};
pub use discovery::Discovered;
pub use error::{LanError, Result};
pub use mqtt::{LanClient, LanEvent};
pub use print::PrintJob;
pub use store::{AccessCode, LanPrinter};

/// Назва моделі за внутрішнім кодом (`DevModel.bambu.com`, `resources/printers/*.json`).
pub fn model_name(code: &str) -> &str {
    match code {
        "N2S" => "A1",
        "N1" => "A1 mini",
        "N9" => "A2L",
        "C11" => "P1P",
        "C12" => "P1S",
        "N7" => "P2S",
        "BL-P001" => "X1 Carbon",
        "BL-P002" => "X1",
        "C13" => "X1E",
        "N6" => "X2D",
        "O1D" => "H2D",
        "O1E" => "H2D Pro",
        "O1S" => "H2S",
        "O1C" | "O1C2" => "H2C",
        other => other,
    }
}

/// Моделі без вбудованої пам’яті: друк лише з microSD (`ftp_folder: sdcard/` в Orca).
pub fn needs_sd_card(model: &str) -> bool {
    matches!(model, "N2S" | "N1" | "N9" | "C11" | "C12")
}

/// Результат [`test_connection`].
#[derive(Debug)]
pub struct ProbeReport {
    pub status: PrinterStatus,
    /// Версія прошивки (OTA), якщо принтер встиг відповісти на `get_version`.
    pub firmware: Option<String>,
}

/// Підключається по MQTT, чекає повного знімка стану й версії прошивки, від’єднується.
/// Нічого не змінює на принтері. Типові помилки: [`LanError::BadAccessCode`],
/// [`LanError::Unreachable`], [`LanError::SerialMismatch`].
pub fn test_connection(printer: &LanPrinter, timeout: Duration) -> Result<ProbeReport> {
    let deadline = Instant::now() + timeout;
    let (client, events) = LanClient::connect(printer)?;
    let mut status: Option<PrinterStatus> = None;
    let mut firmware = None;
    let mut asked_version = false;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match events.recv_timeout(left.min(Duration::from_millis(500))) {
            Ok(LanEvent::Status(s)) => {
                if !asked_version {
                    asked_version = true;
                    client.request_version().ok();
                }
                status = Some(*s);
            }
            Ok(LanEvent::Message(m)) => {
                if let Some(v) = cloud::ota_version(&m) {
                    firmware = Some(v);
                }
            }
            Ok(LanEvent::Disconnected(e)) => return Err(e),
            Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if status.as_ref().is_some_and(|s| s.gcode_state.is_some()) && firmware.is_some() {
            break;
        }
    }
    client.disconnect();
    match status {
        Some(status) => Ok(ProbeReport { status, firmware }),
        None => Err(LanError::Timeout("звіт стану")),
    }
}

/// Перевіряє стан microSD перед надсиланням (A1/P1). `None` у статусі — невідомо, не блокуємо.
pub fn check_storage(model: &str, status: Option<&PrinterStatus>) -> Result<()> {
    if !needs_sd_card(model) {
        return Ok(());
    }
    match status.and_then(PrinterStatus::sd_card) {
        Some(SdCard::None | SdCard::Abnormal | SdCard::ReadOnly) => Err(LanError::NoSdCard),
        _ => Ok(()),
    }
}

/// Повний шлях надсилання: перевірка SD, MD5, FTPS-завантаження, `project_file`.
/// `job.remote_name` і `job.md5` заповнюються тут.
pub fn send_print(
    client: &LanClient,
    printer: &LanPrinter,
    status: Option<&PrinterStatus>,
    gcode_3mf: &Path,
    mut job: PrintJob,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<PrintJob> {
    check_storage(&printer.model, status)?;
    if !client.is_connected() {
        return Err(LanError::Unreachable {
            port: mqtt::MQTT_PORT,
            detail: "немає з’єднання".into(),
        });
    }
    job.md5 = print::md5_file(gcode_3mf)?;
    if job.remote_name.is_empty() {
        job.remote_name = print::remote_name(&job.subtask_name);
    }
    ftps::upload(printer, gcode_3mf, &job.remote_name, progress, cancel)?;
    client.start_print(&job)?;
    Ok(job)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_check_only_for_sd_models() {
        let none = PrinterStatus {
            home_flag: Some(0),
            ..Default::default()
        };
        let ok = PrinterStatus {
            home_flag: Some(0x100),
            ..Default::default()
        };
        assert_eq!(check_storage("N2S", Some(&none)), Err(LanError::NoSdCard));
        assert_eq!(check_storage("N2S", Some(&ok)), Ok(()));
        assert_eq!(check_storage("N2S", None), Ok(()));
        assert_eq!(check_storage("BL-P001", Some(&none)), Ok(()));
        assert_eq!(model_name("O1D"), "H2D");
        assert_eq!(model_name("N7"), "P2S");
    }
}
