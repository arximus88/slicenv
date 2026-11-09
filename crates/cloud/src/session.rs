//! Ізольована копія сесії; оригінальний каталог лише читається.
use anyhow::{Context as _, Result, bail};
use std::{
    fs,
    path::{Path, PathBuf},
};
use paths::fs_mode::{ModeExt as _, NoFollowExt as _};
use tempfile::TempDir;

pub(crate) struct Session {
    pub root: TempDir,
    pub config: PathBuf,
    pub log: PathBuf,
}

impl Session {
    fn private_root() -> Result<(TempDir, PathBuf)> {
        remove_orphans();
        let root = tempfile::Builder::new().prefix("bambu-cloud-").tempdir()?;
        // Власник копії: після аварійного завершення наступний запуск її прибере.
        fs::write(
            root.path().join("owner.pid"),
            std::process::id().to_string(),
        )?;
        let log = root.path().join("log");
        fs::DirBuilder::new().unix_mode(0o700).create(&log)?;
        Ok((root, log))
    }

    /// Чужа сесія (Bambu Studio/Orca): лише копія файлу в тимчасову теку.
    pub fn copy_from(source: &Path) -> Result<Self> {
        let (root, log) = Self::private_root()?;
        let config = root.path().join("config");
        fs::DirBuilder::new().unix_mode(0o700).create(&config)?;
        let session = source.join("BambuNetworkEngine.conf");
        match copy_regular(&session, &config.join("BambuNetworkEngine.conf")) {
            Ok(()) => {}
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
            Err(e) => return Err(e),
        }
        Ok(Self { root, config, log })
    }

    /// Власна тека застосунку: плагін пише сесію (вхід, оновлення токена) просто туди.
    pub fn persistent(dir: &Path) -> Result<Self> {
        crate::account::private_dir(dir)?;
        let (root, log) = Self::private_root()?;
        Ok(Self {
            root,
            config: dir.to_owned(),
            log,
        })
    }
}

/// Копіює звичайний файл (симлінки відхиляються, O_NOFOLLOW) у новий файл 0600.
pub(crate) fn copy_regular(from: &Path, to: &Path) -> Result<()> {
    let info = fs::symlink_metadata(from)?;
    if !info.is_file() || info.file_type().is_symlink() {
        bail!("session must be a regular file");
    }
    // O_NOFOLLOW не дає підмінити файл симлінком між перевіркою та open.
    let mut input = fs::OpenOptions::new()
        .read(true)
        .no_follow()
        .open(from)
        .context("read Bambu session")?;
    let mut output = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .unix_mode(0o600)
        .open(to)?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    Ok(())
}

/// Копії сесії від процесів, які вже не живуть (kill/аварія не викликають Drop), містять
/// токени — видаляємо їх. Чіпаємо лише свої каталоги з `owner.pid` мертвого процесу.
fn remove_orphans() {
    let Ok(entries) = fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let ours = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("bambu-cloud-"));
        let owned = paths::fs_mode::owned_by_current_user(&match fs::symlink_metadata(&path) {
            Ok(m) if m.is_dir() => m,
            _ => continue,
        });
        if !ours || !owned {
            continue;
        }
        let Some(pid) = fs::read_to_string(path.join("owner.pid"))
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
        else {
            continue;
        };
        // Живість власника: /proc на Linux, OpenProcess на Windows; невідомо — не чіпаємо.
        if crate::os::process_alive(pid) == Some(false) {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

pub fn default_studio_config_dir() -> Result<PathBuf> {
    anyhow::ensure!(
        !paths::home_dir().as_os_str().is_empty(),
        "HOME is not set"
    );
    Ok(paths::vendor_config_dir("BambuStudio"))
}

pub fn default_plugin_path() -> Result<PathBuf> {
    Ok(default_studio_config_dir()?
        .join("plugins")
        .join(crate::plugin::LIBRARY))
}

pub(crate) fn studio_country_code(source: &Path) -> String {
    country_code_from(&source.join("BambuStudio.conf"))
}

/// Код країни з конфігу Bambu Studio або OrcaSlicer (той самий ключ `app.region`).
pub(crate) fn country_code_from(conf: &Path) -> String {
    let config = fs::read_to_string(conf)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    // AppConfig::get_country_code використовує US і для Європи.
    match config
        .as_ref()
        .and_then(|v| v["app"]["region"].as_str().or_else(|| v["region"].as_str()))
    {
        Some("China" | "CHN") => "CN",
        Some("Europe" | "USA" | "North America") => "US",
        _ => "Others",
    }
    .into()
}

/// Заголовки HTTP, які Bambu Studio передає плагіну (`GUI_App::get_extra_header`).
/// Без них частина REST-запитів (зокрема список принтерів) відхиляється.
pub(crate) fn studio_http_headers(source: &Path) -> Vec<(String, String)> {
    http_headers(source, None)
}

/// Версія клієнта: зі Studio, якщо є; інакше — версія Bambu Studio, що постачалася з цим
/// поколінням плагіна (Orca так само шле власну версію).
pub(crate) fn http_headers(
    source: &Path,
    generation: Option<crate::Generation>,
) -> Vec<(String, String)> {
    let config = fs::read_to_string(source.join("BambuStudio.conf"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let app = |key: &str| {
        config
            .as_ref()
            .and_then(|v| v["app"][key].as_str())
            .unwrap_or_default()
            .to_owned()
    };
    let version = Some(app("version"))
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| match generation {
            Some(crate::Generation::V0208) => "02.08.04.57".into(),
            _ => "02.06.00.51".into(),
        });
    let language = app("language");
    let language = language
        .split('_')
        .next()
        .filter(|l| !l.is_empty())
        .unwrap_or("en");
    let os_version = crate::os::version()
        .unwrap_or_else(|| "0.0.0".into());
    let mut headers = vec![
        ("X-BBL-Client-Type".into(), "slicer".into()),
        ("X-BBL-Client-Name".into(), "BambuStudio".into()),
        ("X-BBL-Client-Version".into(), version),
        ("X-BBL-OS-Type".into(), crate::plugin::OS_TYPE.into()),
        ("X-BBL-OS-Version".into(), os_version),
        ("X-BBL-Language".into(), language.into()),
    ];
    let uuid = Some(app("slicer_uuid"))
        .filter(|u| !u.is_empty())
        .or_else(own_device_id)
        .unwrap_or_default();
    if !uuid.is_empty() {
        headers.push(("X-BBL-Device-ID".into(), uuid));
    }
    headers
}

/// Власний X-BBL-Device-ID, коли Bambu Studio не встановлено (аналог `slicer_uuid`).
fn own_device_id() -> Option<String> {
    let path = paths::data_dir().join("cloud/device-id");
    if let Ok(id) = fs::read_to_string(&path) {
        let id = id.trim();
        if id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
            return Some(id.to_owned());
        }
    }
    let id = crate::os::random_uuid()?;
    fs::create_dir_all(path.parent()?).ok()?;
    fs::write(&path, &id).ok()?;
    Some(id)
}

/// Публічний сертифікат `resources/cert/slicer_base64.cer` з репозиторію Bambu Studio:
/// без нього `connect_server` неможливий, а встановлений слайсер може бути AppImage, де
/// файл не лежить на диску. Тому вбудований — запасний варіант.
const BUNDLED_CERTIFICATE: &[u8] = include_bytes!("../assets/slicer_base64.cer");

pub(crate) fn find_certificate() -> Option<PathBuf> {
    let installed = [
        "/opt/bambu-studio/resources/cert/slicer_base64.cer",
        "/usr/share/BambuStudio/resources/cert/slicer_base64.cer",
        "/usr/share/bambu-studio/resources/cert/slicer_base64.cer",
        "/opt/orca-slicer/resources/cert/slicer_base64.cer",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.is_file());
    installed.or_else(bundled_certificate)
}

fn bundled_certificate() -> Option<PathBuf> {
    let path = paths::cache_dir().join("cert/slicer_base64.cer");
    if std::fs::read(&path).ok().as_deref() != Some(BUNDLED_CERTIFICATE) {
        std::fs::create_dir_all(path.parent()?).ok()?;
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(&tmp, BUNDLED_CERTIFICATE).ok()?;
        std::fs::rename(&tmp, &path).ok()?;
    }
    Some(path)
}
