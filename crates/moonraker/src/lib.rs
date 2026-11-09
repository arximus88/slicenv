//! Принтери Klipper через Moonraker HTTP API (<https://moonraker.readthedocs.io>). Без GUI.
//!
//! Окремий крейт, а не модуль `lan`: `lan` — це протоколи Bambu Lab (MQTT, FTPS, BBL CA,
//! rustls), а тут звичайний HTTP з JSON. Спільного коду немає, а `moonraker` лишається з
//! трьома залежностями (`paths`, `serde`, `serde_json`).
//!
//! 1. [`store::SavedPrinter::new`] — назва, адреса, необов’язковий API-ключ;
//! 2. [`Client::probe`] — `/server/info` + `/printer/info` (стан Klippy);
//! 3. [`Client::status`] — `print_stats`, `virtual_sdcard`, `display_status`, `extruder`,
//!    `heater_bed`, `toolhead`;
//! 4. [`Client::upload`] — `POST /server/files/upload` (root=gcodes), друк одразу чи ні;
//! 5. [`Client::pause`] / [`Client::resume`] / [`Client::cancel`];
//! 6. [`Client::webcams`] / [`Client::snapshot`] — знімок камери (`snapshot_url`).
//!
//! Що перевірено і як — docs/KLIPPER.md.
pub mod address;
pub mod error;
#[doc(hidden)]
pub mod fake;
pub mod http;
pub mod status;
pub mod store;

use std::{
    path::Path,
    sync::atomic::AtomicBool,
    time::Duration,
};

pub use address::Endpoint;
pub use error::{MoonrakerError, Result};
pub use status::{
    Heater, JobStatus, KlippyState, PrintState, PrinterInfo, ServerInfo, UploadResult, Webcam,
};
pub use store::{ApiKey, SavedPrinter};

use http::{Request, Response, Upload};

/// Об’єкти Klipper для стану на сторінці «Пристрій».
pub const STATUS_QUERY: &str =
    "/printer/objects/query?print_stats&virtual_sdcard&display_status&extruder&heater_bed&toolhead";

#[derive(Clone, Debug)]
pub struct Client {
    pub endpoint: Endpoint,
    api_key: Option<ApiKey>,
}

/// Результат [`Client::probe`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    pub server: ServerInfo,
    /// `None`, якщо Moonraker не з’єднаний з Klippy (`/printer/info` тоді недоступний).
    pub printer: Option<PrinterInfo>,
}

impl Probe {
    pub fn klippy_state(&self) -> KlippyState {
        self.printer
            .as_ref()
            .map(|p| p.state.clone())
            .unwrap_or_else(|| self.server.klippy_state.clone())
    }
    /// `Err(KlippyNotReady)`, якщо Klippy не `ready`.
    pub fn require_ready(&self) -> Result<()> {
        match self.klippy_state() {
            KlippyState::Ready => Ok(()),
            state => Err(MoonrakerError::KlippyNotReady {
                state,
                message: self
                    .printer
                    .as_ref()
                    .map(|p| p.state_message.clone())
                    .unwrap_or_default(),
            }),
        }
    }
}

/// Як завантажувати G-code.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UploadOptions {
    /// `print=true`: Moonraker запускає друк одразу після завантаження.
    pub start: bool,
    /// Замінити файл з такою самою назвою (інакше — [`MoonrakerError::FileExists`]).
    pub overwrite: bool,
}

/// Назва файлу для принтера: без каталогів і керівних символів, з `.gcode`.
pub fn remote_name(stem: &str) -> String {
    let mut name: String = stem
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ' ' | '(' | ')' | '+') {
                c
            } else {
                '_'
            }
        })
        .take(120)
        .collect();
    name = name.trim_matches(['.', ' ']).to_owned();
    if name.is_empty() {
        name = "slicenv".into();
    }
    if !name.to_ascii_lowercase().ends_with(".gcode") {
        name += ".gcode";
    }
    name
}

fn valid_remote(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with(['/', '.'])
        && !name.contains("..")
        && !name.chars().any(|c| c.is_control() || matches!(c, '\\' | '"'))
}

impl Client {
    pub fn new(endpoint: Endpoint, api_key: Option<ApiKey>) -> Self {
        Self { endpoint, api_key }
    }

    pub fn from_saved(printer: &SavedPrinter) -> Result<Self> {
        Ok(Self::new(printer.endpoint()?, printer.api_key.clone()))
    }

    fn key(&self) -> Option<&str> {
        self.api_key.as_ref().map(ApiKey::expose)
    }

    fn call(&self, request: &Request<'_>) -> Result<Response> {
        let response = http::send(
            &self.endpoint,
            request,
            None,
            &mut |_, _| {},
            &AtomicBool::new(false),
        )?;
        check(response)
    }

    fn get_json(&self, path: &str) -> Result<serde_json::Value> {
        status::result(&self.call(&Request::get(path, self.key()))?.body)
    }

    fn post_ok(&self, path: &str) -> Result<()> {
        self.call(&Request::post(path, self.key())).map(|_| ())
    }

    pub fn server_info(&self) -> Result<ServerInfo> {
        Ok(status::server_info(&self.get_json("/server/info")?))
    }

    pub fn printer_info(&self) -> Result<PrinterInfo> {
        Ok(status::printer_info(&self.get_json("/printer/info")?))
    }

    /// Перевірка з’єднання й доступу (лише читання). 401 → `Unauthorized`; Klippy не
    /// `ready` — не помилка: стан повертається в [`Probe`].
    pub fn probe(&self) -> Result<Probe> {
        let server = self.server_info()?;
        let printer = if server.klippy_connected {
            match self.printer_info() {
                Ok(info) => Some(info),
                Err(MoonrakerError::KlippyNotReady { .. }) => None,
                Err(e) => return Err(e),
            }
        } else {
            None
        };
        Ok(Probe { server, printer })
    }

    pub fn status(&self) -> Result<JobStatus> {
        status::job_status(&self.get_json(STATUS_QUERY)?)
    }

    pub fn pause(&self) -> Result<()> {
        self.post_ok("/printer/print/pause")
    }
    pub fn resume(&self) -> Result<()> {
        self.post_ok("/printer/print/resume")
    }
    pub fn cancel(&self) -> Result<()> {
        self.post_ok("/printer/print/cancel")
    }

    /// Файли в `gcodes` (шляхи відносно кореня).
    pub fn files(&self) -> Result<Vec<String>> {
        Ok(status::file_list(&self.get_json("/server/files/list?root=gcodes")?))
    }

    /// Завантажує G-code у `gcodes`. Перед друком перевіряє, що Klippy `ready` і принтер
    /// не друкує; без `overwrite` — що файлу з такою назвою ще немає.
    /// `progress(відправлено, усього)` — байти файлу.
    pub fn upload(
        &self,
        gcode: &Path,
        remote: &str,
        options: UploadOptions,
        progress: &mut dyn FnMut(u64, u64),
        cancel: &AtomicBool,
    ) -> Result<UploadResult> {
        if !valid_remote(remote) {
            return Err(MoonrakerError::InvalidFileName);
        }
        if !gcode.is_file() {
            return Err(MoonrakerError::Io(format!("{}: not found", gcode.display())));
        }
        if options.start {
            self.probe()?.require_ready()?;
            if self.status()?.state.is_active() {
                return Err(MoonrakerError::Busy);
            }
        }
        if !options.overwrite && self.files()?.iter().any(|f| f == remote) {
            return Err(MoonrakerError::FileExists(remote.to_owned()));
        }
        let print = if options.start { "true" } else { "false" };
        let fields = [("root", "gcodes"), ("print", print)];
        let upload = Upload {
            fields: &fields,
            file_field: "file",
            file_name: remote,
            path: gcode,
        };
        let mut request = Request::post("/server/files/upload", self.key());
        // Після передачі Moonraker ще пише файл на диск і читає метадані.
        request.timeout = Duration::from_secs(120);
        let response = check(http::send(
            &self.endpoint,
            &request,
            Some(&upload),
            progress,
            cancel,
        )?)?;
        status::upload_result(&status::result(&response.body)?)
    }

    pub fn webcams(&self) -> Result<Vec<Webcam>> {
        Ok(status::webcams(&self.get_json("/server/webcams/list")?))
    }

    /// Куди йти по знімок: відносний `snapshot_url` — від вебсервера (Mainsail/Fluidd) того
    /// ж хоста, абсолютний — як є. Ключ надсилається лише на адресу самого Moonraker.
    pub fn snapshot_target(&self, webcam: &Webcam) -> Option<(Endpoint, String, bool)> {
        let url = webcam.snapshot_url.trim();
        if url.is_empty() {
            return None;
        }
        let (endpoint, path) = match Endpoint::split_url(url) {
            Some(pair) => pair,
            None if url.contains("://") => return None,
            None => {
                let path = if url.starts_with('/') {
                    url.to_owned()
                } else {
                    format!("/{url}")
                };
                if path.bytes().any(|b| b.is_ascii_control() || b == b' ') {
                    return None;
                }
                (self.endpoint.web(), path)
            }
        };
        let same = endpoint.host == self.endpoint.host && endpoint.port == self.endpoint.port;
        Some((endpoint, path, same))
    }

    /// Один кадр JPEG з `snapshot_url`.
    pub fn snapshot(&self, webcam: &Webcam) -> Result<Vec<u8>> {
        let (endpoint, path, same) = self
            .snapshot_target(webcam)
            .ok_or_else(|| MoonrakerError::Protocol("webcam without snapshot_url".into()))?;
        let mut request = Request::get(&path, if same { self.key() } else { None });
        request.raw_path = true;
        request.timeout = Duration::from_secs(5);
        let response = check(http::send(
            &endpoint,
            &request,
            None,
            &mut |_, _| {},
            &AtomicBool::new(false),
        )?)?;
        if !response.body.starts_with(&[0xFF, 0xD8]) {
            return Err(MoonrakerError::Protocol("snapshot is not JPEG".into()));
        }
        Ok(response.body)
    }

    /// Адреса вебінтерфейсу (Mainsail / Fluidd) для кнопки «Відкрити».
    pub fn web_url(&self) -> String {
        format!("{}/", self.endpoint.web().origin())
    }
}

/// HTTP-код → типізована помилка.
fn check(response: Response) -> Result<Response> {
    match response.status {
        200..=299 => Ok(response),
        401 | 403 => Err(MoonrakerError::Unauthorized),
        status => {
            let message = status::error_message(&response.body);
            let lower = message.to_ascii_lowercase();
            if status == 503 || lower.contains("klippy") && (lower.contains("disconnect") || lower.contains("not connected")) {
                return Err(MoonrakerError::KlippyNotReady {
                    state: KlippyState::Disconnected,
                    message,
                });
            }
            Err(MoonrakerError::Http { status, message })
        }
    }
}

/// Шлях до чистого G-code для Moonraker: `plate_N.gcode` поруч із `*.gcode.3mf` (його
/// пише рушій). Метод-помічник для застосунку, щоб не дублювати домовленість про назву.
pub fn plate_gcode(gcode_3mf: &Path, plate: u32) -> std::path::PathBuf {
    gcode_3mf.with_file_name(format!("plate_{plate}.gcode"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_names_are_safe() {
        assert_eq!(remote_name("Кубик 1"), "Кубик 1.gcode");
        assert_eq!(remote_name("../etc/passwd"), "_etc_passwd.gcode");
        assert_eq!(remote_name("a\"b\n"), "a_b.gcode");
        assert_eq!(remote_name(""), "slicenv.gcode");
        assert_eq!(remote_name("x.GCODE"), "x.GCODE");
        assert!(valid_remote("sub/dir.gcode") && !valid_remote("../x") && !valid_remote("/abs"));
    }
}
