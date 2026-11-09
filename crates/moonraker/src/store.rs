//! Збережені принтери Klipper: `~/.config/slicenv/klipper-printers.json`, права 0600 —
//! так само, як коди доступу Bambu в `lan-printers.json`.
//!
//! API-ключ — секрет: `ApiKey` не має `Display`, `Debug` друкує зірочки, файл пишеться
//! атомарно (тимчасовий 0600 у тому ж каталозі → rename).
use std::{
    fmt, fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
};

use paths::fs_mode::{self, ModeExt as _};
use serde::{Deserialize, Serialize};

use crate::{
    address::Endpoint,
    error::{MoonrakerError, Result},
};

/// API-ключ Moonraker (`[authorization]`, 32 hex-символи). Ніколи не друкується.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ApiKey(String);

impl ApiKey {
    /// Порожній рядок — без ключа (`None`). Інакше 8–256 друкованих ASCII без пробілів.
    pub fn parse(text: &str) -> Result<Option<Self>> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(None);
        }
        if !(8..=256).contains(&text.len()) || !text.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(MoonrakerError::InvalidApiKey);
        }
        Ok(Some(Self(text.to_owned())))
    }
    /// Лише для заголовка `X-Api-Key`.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(***)")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedPrinter {
    /// Стабільний ідентифікатор (`klipper:<адреса>`), він же `DeviceId` у застосунку.
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// Нормалізована адреса (`Endpoint::display`).
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<ApiKey>,
}

impl fmt::Debug for SavedPrinter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SavedPrinter")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("api_key", &self.api_key.as_ref().map(|_| "***"))
            .finish()
    }
}

pub const ID_PREFIX: &str = "klipper:";

impl SavedPrinter {
    /// Перевіряє ручний ввід. Порожня назва — «Klipper <хост>».
    pub fn new(name: &str, address: &str, api_key: &str) -> Result<Self> {
        let endpoint = Endpoint::parse(address)?;
        let api_key = ApiKey::parse(api_key)?;
        let name = name.trim();
        let name = if name.is_empty() {
            format!("Klipper {}", endpoint.host)
        } else {
            name.chars().filter(|c| !c.is_control()).take(64).collect()
        };
        let address = endpoint.display();
        Ok(Self {
            id: format!("{ID_PREFIX}{address}"),
            name,
            address,
            api_key,
        })
    }
    pub fn endpoint(&self) -> Result<Endpoint> {
        Endpoint::parse(&self.address)
    }
}

pub fn is_klipper_id(id: &str) -> bool {
    id.starts_with(ID_PREFIX)
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    printers: Vec<SavedPrinter>,
}

pub fn default_path() -> PathBuf {
    paths::config_dir().join("klipper-printers.json")
}

/// Відсутній чи пошкоджений файл — порожній список.
pub fn load_from(path: &Path) -> Vec<SavedPrinter> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<File>(&bytes).ok())
        .map(|f| f.printers)
        .unwrap_or_default()
}

pub fn save_to(path: &Path, printers: &[SavedPrinter]) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| io::Error::other("no parent"))?;
    if !dir.exists() {
        fs::create_dir_all(dir)?;
        fs_mode::set_mode(dir, 0o700)?;
    }
    let tmp = dir.join(format!(
        ".{}.tmp{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("klipper"),
        std::process::id()
    ));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .unix_mode(0o600)
            .open(&tmp)?;
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

/// Додає або оновлює (за `id`) і зберігає. Повертає новий список.
pub fn upsert_in(path: &Path, printer: SavedPrinter) -> io::Result<Vec<SavedPrinter>> {
    let mut list = load_from(path);
    match list.iter_mut().find(|p| p.id == printer.id) {
        Some(existing) => *existing = printer,
        None => list.push(printer),
    }
    save_to(path, &list)?;
    Ok(list)
}

pub fn remove_in(path: &Path, id: &str) -> io::Result<Vec<SavedPrinter>> {
    let mut list = load_from(path);
    list.retain(|p| p.id != id);
    save_to(path, &list)?;
    Ok(list)
}

pub fn load() -> Vec<SavedPrinter> {
    load_from(&default_path())
}

pub fn upsert(printer: SavedPrinter) -> io::Result<Vec<SavedPrinter>> {
    upsert_in(&default_path(), printer)
}

pub fn remove(id: &str) -> io::Result<Vec<SavedPrinter>> {
    remove_in(&default_path(), id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_is_validated_and_key_never_printed() {
        let p = SavedPrinter::new("", "192.168.1.60", " 0123456789abcdef0123456789abcdef ").unwrap();
        assert_eq!(p.id, "klipper:192.168.1.60:7125");
        assert_eq!(p.name, "Klipper 192.168.1.60");
        let text = format!("{p:?} {:?}", p.api_key);
        assert!(!text.contains("0123456789abcdef"));
        assert!(SavedPrinter::new("x", "192.168.1.60", "short").is_err());
        assert!(SavedPrinter::new("x", "192.168.1.60", "with space inside").is_err());
        assert!(SavedPrinter::new("x", "bad host", "").is_err());
        assert_eq!(SavedPrinter::new("Ender", "http://ender.local", "").unwrap().api_key, None);
        assert!(is_klipper_id(&p.id) && !is_klipper_id("03919C0000000"));
    }

    #[test]
    fn file_is_private_atomic_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg/klipper-printers.json");
        assert!(load_from(&path).is_empty());
        let a = SavedPrinter::new("A", "10.0.0.2", "").unwrap();
        let mut b = SavedPrinter::new("B", "10.0.0.3", "secretkey123").unwrap();
        upsert_in(&path, a.clone()).unwrap();
        upsert_in(&path, b.clone()).unwrap();
        b.name = "B2".into();
        assert_eq!(upsert_in(&path, b.clone()).unwrap(), vec![a.clone(), b.clone()]);
        assert_eq!(load_from(&path), vec![a.clone(), b.clone()]);
        if cfg!(unix) {
            let mode = |p: &Path| fs_mode::mode(&fs::metadata(p).unwrap()).map(|m| m & 0o777);
            assert_eq!(mode(&path), Some(0o600));
            assert_eq!(mode(path.parent().unwrap()), Some(0o700));
        }
        assert_eq!(remove_in(&path, &b.id).unwrap(), vec![a]);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        fs::write(&path, b"{broken").unwrap();
        assert!(load_from(&path).is_empty());
    }
}
