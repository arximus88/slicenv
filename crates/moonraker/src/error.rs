//! Типізовані помилки Moonraker. `key()` — стала українська фраза (ключ перекладу в
//! застосунку), `detail()` — технічна деталь без секретів. Жоден варіант не містить API-ключа.
use std::fmt;

use crate::status::KlippyState;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MoonrakerError {
    /// Адресу не вдалося розібрати (host, порт, схема).
    InvalidAddress,
    /// API-ключ має недопустимі символи чи довжину.
    InvalidApiKey,
    /// Ім’я файлу на принтері порожнє чи недопустиме.
    InvalidFileName,
    /// TCP не з’єднався або ім’я хоста не розв’язалося.
    Unreachable(String),
    /// Moonraker не відповів вчасно.
    Timeout,
    /// 401/403: потрібен (правильний) API-ключ або клієнт не в `trusted_clients`.
    Unauthorized,
    /// Klippy не в стані `ready` (shutdown, error, startup, не з’єднаний).
    KlippyNotReady { state: KlippyState, message: String },
    /// На принтері вже є файл з такою назвою, а заміну не дозволено.
    FileExists(String),
    /// Друк уже йде або на паузі: новий запускати не можна.
    Busy,
    /// Інша HTTP-помилка Moonraker (код і повідомлення сервера).
    Http { status: u16, message: String },
    /// З’єднання обірвалося, поки файл передавався.
    Interrupted(String),
    /// Неочікувана відповідь (не JSON, без потрібних полів, не JPEG…).
    Protocol(String),
    /// Локальна помилка вводу-виводу (файл G-code тощо).
    Io(String),
    /// Для https потрібен системний curl, а його немає.
    NoCurl,
    /// Операцію скасовано користувачем.
    Cancelled,
    /// Значення команди поза межами або порожній рядок G-code (у мережу нічого не пішло).
    InvalidCommand(String),
    /// Klipper відхилив G-code (повідомлення Klipper, напр. `Must home axis first`).
    Gcode(String),
    /// Потік камери не MJPEG (WebRTC, HLS, сторінка) або обірвався.
    NotMjpeg(String),
}

pub type Result<T> = std::result::Result<T, MoonrakerError>;

impl MoonrakerError {
    /// Стала фраза для користувача (ключ перекладу).
    pub fn key(&self) -> &'static str {
        match self {
            Self::InvalidAddress => {
                "Адреса принтера — host або host:порт, напр. 192.168.1.60:7125 чи http://ender.local"
            }
            Self::InvalidApiKey => "API-ключ Moonraker — латинські літери й цифри без пробілів",
            Self::InvalidFileName => "Недопустима назва файлу для принтера",
            Self::Unreachable(_) => {
                "Принтер Klipper недоступний: перевір адресу й порт (Moonraker зазвичай на 7125) і що принтер увімкнений"
            }
            Self::Timeout => "Moonraker не відповів вчасно",
            Self::Unauthorized => {
                "Moonraker відхилив доступ (401): потрібен правильний API-ключ або дозвіл для цієї мережі в trusted_clients"
            }
            Self::KlippyNotReady { .. } => "Klipper не готовий до друку",
            Self::FileExists(_) => "Файл з такою назвою вже є на принтері",
            Self::Busy => "Принтер уже друкує: дочекайся кінця або скасуй поточне завдання",
            Self::Http { .. } => "Moonraker повернув помилку",
            Self::Interrupted(_) => "З’єднання обірвалося під час передачі файлу",
            Self::Protocol(_) => "Неочікувана відповідь Moonraker",
            Self::Io(_) => "Помилка вводу-виводу",
            Self::NoCurl => "Для https потрібна програма curl: встановіть її або використайте http",
            Self::Cancelled => "Скасовано",
            Self::InvalidCommand(_) => "Недопустима команда або значення поза межами",
            Self::Gcode(_) => "Klipper відхилив команду",
            Self::NotMjpeg(_) => "Камера не віддає MJPEG-потік",
        }
    }

    /// Технічна деталь (англійською від сервера або системи); порожньо, якщо нічого додати.
    pub fn detail(&self) -> String {
        match self {
            Self::Unreachable(d)
            | Self::Interrupted(d)
            | Self::Protocol(d)
            | Self::Io(d)
            | Self::InvalidCommand(d)
            | Self::Gcode(d)
            | Self::NotMjpeg(d) => {
                d.clone()
            }
            Self::KlippyNotReady { state, message } => {
                if message.is_empty() {
                    state.as_str().to_owned()
                } else {
                    format!("{}: {}", state.as_str(), message.trim())
                }
            }
            Self::FileExists(name) => name.clone(),
            Self::Http { status, message } => format!("HTTP {status}: {message}"),
            _ => String::new(),
        }
    }

    pub(crate) fn io(error: &std::io::Error) -> Self {
        use std::io::ErrorKind::*;
        match error.kind() {
            TimedOut | WouldBlock => Self::Timeout,
            _ => Self::Io(error.to_string()),
        }
    }
}

impl fmt::Display for MoonrakerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let detail = self.detail();
        if detail.is_empty() {
            f.write_str(self.key())
        } else {
            write!(f, "{} ({detail})", self.key())
        }
    }
}

impl std::error::Error for MoonrakerError {}
