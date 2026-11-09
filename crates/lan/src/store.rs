//! Збережені LAN-принтери: `~/.config/slicenv/lan-printers.json`, права 0600.
//!
//! Код доступу — секрет: `AccessCode` не має `Display`, `Debug` друкує зірочки, а файл
//! пишеться атомарно (тимчасовий файл 0600 у тому ж каталозі → rename).
use std::{
    fmt, fs,
    io::{self, Write as _},
    net::IpAddr,
    path::{Path, PathBuf},
};

use paths::fs_mode::{self, ModeExt as _};
use serde::{Deserialize, Serialize};

use crate::error::{LanError, Result};

/// Код доступу LAN з екрана принтера. Ніколи не друкується.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccessCode(String);

impl AccessCode {
    /// 8 символів на поточних прошивках; приймаємо 4–32 друковані ASCII без пробілів.
    pub fn new(code: impl Into<String>) -> Result<Self> {
        let code = code.into().trim().to_owned();
        if !(4..=32).contains(&code.len()) || !code.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(LanError::Invalid("код доступу — 8 символів з екрана принтера"));
        }
        Ok(Self(code))
    }
    /// Лише для передачі в протокол.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for AccessCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccessCode(***)")
    }
}

/// Усе, що потрібно для з’єднання з принтером у LAN.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanPrinter {
    pub serial: String,
    pub ip: IpAddr,
    pub access_code: AccessCode,
    #[serde(default)]
    pub name: String,
    /// Код моделі (N2S, C12…), якщо відомий з виявлення.
    #[serde(default)]
    pub model: String,
}

impl fmt::Debug for LanPrinter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LanPrinter")
            .field("serial", &redact(&self.serial))
            .field("ip", &self.ip)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

/// Останні 4 символи серійного номера (як `cloud::DeviceId::redacted`).
pub fn redact(serial: &str) -> String {
    let n = serial.chars().count();
    if n <= 4 {
        "****".into()
    } else {
        format!("…{}", serial.chars().skip(n - 4).collect::<String>())
    }
}

impl LanPrinter {
    /// Перевіряє формат введених вручну даних. Серійний номер — у верхній регістр.
    pub fn new(serial: &str, ip: &str, access_code: &str) -> Result<Self> {
        let serial = serial.trim().to_ascii_uppercase();
        if !(8..=24).contains(&serial.len()) || !serial.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(LanError::Invalid("серійний номер — 8–24 латинські літери й цифри"));
        }
        let ip: IpAddr = ip
            .trim()
            .parse()
            .map_err(|_| LanError::Invalid("IP-адреса у форматі 192.168.1.50"))?;
        if ip.is_unspecified() || ip.is_multicast() || ip.is_loopback() {
            return Err(LanError::Invalid("IP-адреса у форматі 192.168.1.50"));
        }
        Ok(Self {
            serial,
            ip,
            access_code: AccessCode::new(access_code)?,
            name: String::new(),
            model: String::new(),
        })
    }
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    printers: Vec<LanPrinter>,
}

pub fn default_path() -> PathBuf {
    paths::config_dir().join("lan-printers.json")
}

/// Відсутній чи пошкоджений файл — порожній список (пошкоджений не перезаписуємо мовчки:
/// `save` замінить його лише після явної дії користувача).
pub fn load_from(path: &Path) -> Vec<LanPrinter> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<File>(&bytes).ok())
        .map(|f| f.printers)
        .unwrap_or_default()
}

pub fn save_to(path: &Path, printers: &[LanPrinter]) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| io::Error::other("no parent"))?;
    if !dir.exists() {
        fs::create_dir_all(dir)?;
        fs_mode::set_mode(dir, 0o700)?;
    }
    let tmp = dir.join(format!(
        ".{}.tmp{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("lan"),
        std::process::id()
    ));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .unix_mode(0o600)
            .open(&tmp)?;
        // mode() діє лише при створенні: залишок від попереднього збою теж робимо 0600.
        fs_mode::set_file_mode(&file, 0o600)?;
        let body = serde_json::to_vec_pretty(&File {
            printers: printers.to_vec(),
        })
        .map_err(io::Error::other)?;
        file.write_all(&body)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

pub fn load() -> Vec<LanPrinter> {
    load_from(&default_path())
}

pub fn save(printers: &[LanPrinter]) -> io::Result<()> {
    save_to(&default_path(), printers)
}

/// Додає або оновлює (за серійним номером) і зберігає. Повертає новий список.
pub fn upsert_in(path: &Path, printer: LanPrinter) -> io::Result<Vec<LanPrinter>> {
    let mut list = load_from(path);
    match list.iter_mut().find(|p| p.serial == printer.serial) {
        Some(existing) => *existing = printer,
        None => list.push(printer),
    }
    save_to(path, &list)?;
    Ok(list)
}

pub fn remove_in(path: &Path, serial: &str) -> io::Result<Vec<LanPrinter>> {
    let mut list = load_from(path);
    list.retain(|p| p.serial != serial);
    save_to(path, &list)?;
    Ok(list)
}

pub fn upsert(printer: LanPrinter) -> io::Result<Vec<LanPrinter>> {
    upsert_in(&default_path(), printer)
}

pub fn remove(serial: &str) -> io::Result<Vec<LanPrinter>> {
    remove_in(&default_path(), serial)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_input_is_validated() {
        let p = LanPrinter::new(" 03919c0000000 ", "192.168.1.137", "12345678").unwrap();
        assert_eq!(p.serial, "03919C0000000");
        assert!(LanPrinter::new("short", "192.168.1.137", "12345678").is_err());
        assert!(LanPrinter::new("03919C0000000", "192.168.1", "12345678").is_err());
        assert!(LanPrinter::new("03919C0000000", "0.0.0.0", "12345678").is_err());
        assert!(LanPrinter::new("03919C0000000", "192.168.1.137", "12 4").is_err());
        assert!(LanPrinter::new("03919C0000000", "192.168.1.137", "").is_err());
    }

    #[test]
    fn secrets_never_appear_in_debug() {
        let p = LanPrinter::new("03919C0000000", "192.168.1.137", "s3cr3tAB").unwrap();
        let text = format!("{p:?} {:?}", p.access_code);
        assert!(!text.contains("s3cr3tAB"));
        assert!(!text.contains("03919C0000000"));
        assert!(text.contains("…0000"));
    }

    #[test]
    fn file_is_private_atomic_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg/lan-printers.json");
        assert!(load_from(&path).is_empty());
        let a = LanPrinter::new("AAAA00000001", "192.168.1.10", "11111111").unwrap();
        let mut b = LanPrinter::new("BBBB00000002", "192.168.1.11", "22222222").unwrap();
        upsert_in(&path, a.clone()).unwrap();
        upsert_in(&path, b.clone()).unwrap();
        b.ip = "192.168.1.12".parse().unwrap();
        let list = upsert_in(&path, b.clone()).unwrap();
        assert_eq!(list, vec![a.clone(), b.clone()]);
        assert_eq!(load_from(&path), vec![a.clone(), b]);
        if cfg!(unix) {
            let mode = |p: &Path| fs_mode::mode(&fs::metadata(p).unwrap()).map(|m| m & 0o777);
            assert_eq!(mode(&path), Some(0o600));
            assert_eq!(mode(path.parent().unwrap()), Some(0o700));
        }
        assert_eq!(remove_in(&path, "BBBB00000002").unwrap(), vec![a]);
        // Жодних тимчасових файлів не лишилось.
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        fs::write(&path, b"{broken").unwrap();
        assert!(load_from(&path).is_empty());
    }
}
