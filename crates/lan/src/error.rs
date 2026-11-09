//! Типізовані помилки LAN. `key()` — стала українська фраза (ключ перекладу в застосунку),
//! `Display` додає технічну деталь. Жоден варіант не містить коду доступу.
use std::fmt;

use crate::tls::TlsProblem;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LanError {
    /// TCP не з’єднався: неправильний IP, принтер вимкнений, інша підмережа, файрвол.
    Unreachable { port: u16, detail: String },
    /// MQTT CONNACK 4/5 або FTP 530: код доступу неправильний (чи вже змінений на принтері).
    BadAccessCode,
    /// Сертифікат не від Bambu Lab (BBL CA) або пошкоджений ланцюжок.
    UntrustedCertificate(String),
    /// Сертифікат справжній, але виданий іншому принтеру (CN ≠ серійний номер).
    SerialMismatch,
    /// Прошивка з Authorization Control відхиляє керування й друк зі сторонніх програм.
    ControlLocked { command: String, reason: String },
    /// A1/P1 друкують лише з microSD; картки немає або вона недоступна для запису.
    NoSdCard,
    /// FTP повернув помилку (код відповіді й текст сервера).
    Ftp { code: u16, message: String },
    /// Принтер не відповів вчасно.
    Timeout(&'static str),
    /// Неочікувана відповідь протоколу.
    Protocol(String),
    /// Неправильні вхідні дані (IP, серійний номер, код доступу, файл).
    Invalid(&'static str),
    /// Локальна помилка вводу-виводу (файл, сокет після з’єднання).
    Io(String),
    /// Операцію скасовано користувачем.
    Cancelled,
}

impl LanError {
    /// Стала фраза для користувача (ключ перекладу).
    pub fn key(&self) -> &'static str {
        match self {
            Self::Unreachable { .. } => {
                "Принтер недоступний у мережі: перевір IP-адресу, що принтер увімкнений і в тій самій мережі"
            }
            Self::BadAccessCode => {
                "Невірний код доступу: звір його з екраном принтера (Налаштування → Мережа / LAN)"
            }
            Self::UntrustedCertificate(_) => {
                "Сертифікат принтера не від Bambu Lab — з’єднання розірвано (можлива підміна)"
            }
            Self::SerialMismatch => {
                "За цією IP-адресою інший принтер: серійний номер у сертифікаті не збігається"
            }
            Self::ControlLocked { .. } => {
                "Керування заблоковано прошивкою — увімкни на принтері LAN Only Mode і Developer Mode"
            }
            Self::NoSdCard => "Немає microSD-картки або вона недоступна: A1 і P1 друкують лише з неї",
            Self::Ftp { .. } => "Принтер відхилив передачу файлу",
            Self::Timeout(_) => "Принтер не відповів вчасно",
            Self::Protocol(_) => "Неочікувана відповідь принтера",
            Self::Invalid(_) => "Неправильні дані принтера",
            Self::Io(_) => "Помилка вводу-виводу",
            Self::Cancelled => "Скасовано",
        }
    }

    /// Технічна деталь без секретів (може бути порожньою).
    pub fn detail(&self) -> String {
        match self {
            Self::Unreachable { port, detail } => format!("порт {port}: {detail}"),
            Self::UntrustedCertificate(d) | Self::Protocol(d) | Self::Io(d) => d.clone(),
            Self::ControlLocked { command, reason } => format!("{command}: {reason}"),
            Self::Ftp { code, message } => format!("{code} {message}"),
            Self::Timeout(what) | Self::Invalid(what) => (*what).into(),
            _ => String::new(),
        }
    }

    pub(crate) fn from_tls(problem: TlsProblem) -> Self {
        match problem {
            TlsProblem::SerialMismatch => Self::SerialMismatch,
            other => Self::UntrustedCertificate(other.to_string()),
        }
    }

    pub(crate) fn connect(port: u16, error: &std::io::Error) -> Self {
        if matches!(
            error.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ) {
            return Self::Unreachable {
                port,
                detail: "тайм-аут з’єднання".into(),
            };
        }
        Self::Unreachable {
            port,
            detail: error.to_string(),
        }
    }
}

impl fmt::Display for LanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let detail = self.detail();
        if detail.is_empty() {
            f.write_str(self.key())
        } else {
            write!(f, "{} ({detail})", self.key())
        }
    }
}

impl std::error::Error for LanError {}

impl From<std::io::Error> for LanError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

pub type Result<T, E = LanError> = std::result::Result<T, E>;
