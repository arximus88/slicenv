//! Менеджер закритого мережевого плагіна Bambu Lab (`libbambu_networking.so`).
//!
//! Плагін — власність Bambu Lab, розповсюджувати його разом із програмою не можна. Тому, як
//! в OrcaSlicer (`GUI_App::download_plugin`/`install_plugin`), він завантажується **лише за явною
//! згодою користувача** безпосередньо з серверів Bambu у власну теку застосунку:
//! `~/.local/share/slicenv/bambu-plugin/<версія>/`. Каталоги Bambu Studio й OrcaSlicer лише
//! читаються як запасні джерела.
//!
//! Порядок пошуку: керований нами → `~/.config/BambuStudio/plugins` →
//! `~/.config/OrcaSlicer/plugins`.
use crate::{
    abi::{self, Generation},
    http::{Method, Request, Transport},
};
use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{SystemTime, UNIX_EPOCH},
};
use paths::fs_mode::ModeExt as _;

/// Імена бібліотек плагіна за ОС — як їх шукає OrcaSlicer (`BBLNetworkPlugin.cpp`):
/// `bambu_networking.dll` / `libbambu_networking.dylib` / `libbambu_networking.so`.
/// Завантажуємо поки лише `.so` ([`crate::OS_SUPPORTED`]); решта — для пошуку й повідомлень.
#[cfg(windows)]
pub const LIBRARY: &str = "bambu_networking.dll";
#[cfg(target_os = "macos")]
pub const LIBRARY: &str = "libbambu_networking.dylib";
#[cfg(not(any(windows, target_os = "macos")))]
pub const LIBRARY: &str = "libbambu_networking.so";
/// Бібліотека камери з того самого архіву (`BambuSource`).
#[cfg(windows)]
pub const SOURCE_LIBRARY: &str = "BambuSource.dll";
#[cfg(target_os = "macos")]
pub const SOURCE_LIBRARY: &str = "libBambuSource.dylib";
#[cfg(not(any(windows, target_os = "macos")))]
pub const SOURCE_LIBRARY: &str = "libBambuSource.so";
/// Префікс і суфікс версійованої копії Orca: `libbambu_networking_<версія>.so`.
#[cfg(windows)]
const VERSIONED: (&str, &str) = ("bambu_networking_", ".dll");
#[cfg(target_os = "macos")]
const VERSIONED: (&str, &str) = ("libbambu_networking_", ".dylib");
#[cfg(not(any(windows, target_os = "macos")))]
const VERSIONED: (&str, &str) = ("libbambu_networking_", ".so");
const MANIFEST: &str = "manifest.json";
const CONSENT: &str = "consent.json";
/// Версія тексту згоди: зміна тексту → нова згода.
pub const CONSENT_REVISION: u32 = 1;
/// Текст згоди для майстра (ключ перекладу в застосунку).
pub const CONSENT_TEXT: &str = "Для Bambu Cloud потрібен мережевий плагін Bambu Lab. Це закрита програма Bambu Lab, не частина Slicenv; Slicenv її не розробляє і не перевіряє. Плагін буде завантажено з серверів Bambu Lab і встановлено у вашу теку даних. Використовуєте на власний розсуд.";
/// Архів 02.08.04.60 — 38 МБ; запас на майбутні збірки.
const MAX_ARCHIVE: u64 = 256 * 1024 * 1024;
const MAX_ENTRY: u64 = 128 * 1024 * 1024;
const MAX_TOTAL: u64 = 512 * 1024 * 1024;
const MAX_ENTRIES: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginOrigin {
    /// Завантажений нами за згодою користувача.
    Managed,
    BambuStudio,
    OrcaSlicer,
    /// Відкритий плагін Open Bamboo Networking, який користувач сам вибрав ([`crate::obn`]).
    OpenBamboo,
}

#[derive(Clone, Debug)]
pub struct InstalledPlugin {
    pub origin: PluginOrigin,
    pub library: PathBuf,
    /// Відома з маніфесту чи імені файлу Orca; для Bambu Studio — `None` (видно лише після
    /// завантаження, див. [`crate::read_plugin_version`]).
    pub version: Option<String>,
}

impl InstalledPlugin {
    pub fn generation(&self) -> Option<Generation> {
        self.version.as_deref().and_then(abi::generation)
    }
    /// Невідома версія — кандидат: остаточно вирішує шим при завантаженні.
    pub fn maybe_supported(&self) -> bool {
        self.version.is_none() || self.generation().is_some()
    }
    /// libBambuSource.so (камера) лежить поруч у тому самому архіві.
    pub fn source_library(&self) -> PathBuf {
        self.library.with_file_name(SOURCE_LIBRARY)
    }
}

/// Стан для UI.
#[derive(Clone, Debug)]
pub enum PluginState {
    Ready(InstalledPlugin),
    /// Знайдено лише непідтримувані версії (наприклад, legacy 01.10 від Orca).
    Unsupported(Vec<InstalledPlugin>),
    Missing,
}

/// Окремо від `plugins/` застосунку (там власні розширення Slicenv, напр. support-fins).
pub fn managed_root() -> PathBuf {
    paths::data_dir().join("bambu-plugin")
}

fn managed_in(root: &Path) -> Vec<InstalledPlugin> {
    let mut list: Vec<InstalledPlugin> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            if !abi::is_version(&name) || !e.file_type().ok()?.is_dir() {
                return None;
            }
            let dir = e.path();
            let manifest: Manifest =
                serde_json::from_slice(&fs::read(dir.join(MANIFEST)).ok()?).ok()?;
            let library = dir.join(LIBRARY);
            (manifest.version == name && is_regular(&library)).then_some(InstalledPlugin {
                origin: PluginOrigin::Managed,
                library,
                version: Some(name),
            })
        })
        .collect();
    // Новіша збірка першою; рядки AA.BB.CC.DD порівнюються лексикографічно.
    list.sort_by(|a, b| b.version.cmp(&a.version));
    list
}

fn is_regular(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
}

fn foreign_in(dir: &Path, origin: PluginOrigin) -> Vec<InstalledPlugin> {
    let mut list = Vec::new();
    let plain = dir.join(LIBRARY);
    if plain.is_file() {
        list.push(InstalledPlugin {
            origin,
            library: plain,
            version: None,
        });
    }
    // Orca перейменовує бібліотеку на libbambu_networking_<версія>.so.
    let mut versioned: Vec<InstalledPlugin> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let version = name
                .strip_prefix(VERSIONED.0)?
                .strip_suffix(VERSIONED.1)?;
            abi::is_version(version).then(|| InstalledPlugin {
                origin,
                library: e.path(),
                version: Some(version.to_owned()),
            })
        })
        .collect();
    versioned.sort_by(|a, b| b.version.cmp(&a.version));
    list.extend(versioned);
    list
}

/// Усі знайдені плагіни за пріоритетом; чужі каталоги лише читаються.
///
/// Відкритий плагін іде першим, лише якщо користувач його вибрав ([`crate::obn::active`]).
/// Інакше OBN у теці Bambu Studio / Orca сюди не потрапляє: мовчки підхопити сторонній
/// плагін замість закритого не можна (і його сесія — не `BambuNetworkEngine.conf`).
pub fn find_plugins() -> Vec<InstalledPlugin> {
    let mut list: Vec<InstalledPlugin> = crate::obn::active().into_iter().collect();
    list.extend(managed_in(&managed_root()));
    let foreign = foreign_in(
        &paths::vendor_config_dir("BambuStudio").join("plugins"),
        PluginOrigin::BambuStudio,
    )
    .into_iter()
    .chain(foreign_in(
        &paths::vendor_config_dir("OrcaSlicer").join("plugins"),
        PluginOrigin::OrcaSlicer,
    ))
    .filter(|p| !crate::obn::is_open_plugin(&p.library));
    list.extend(foreign);
    list
}

pub fn select(list: Vec<InstalledPlugin>) -> PluginState {
    if let Some(p) = list.iter().find(|p| p.maybe_supported()) {
        return PluginState::Ready(p.clone());
    }
    if list.is_empty() {
        PluginState::Missing
    } else {
        PluginState::Unsupported(list)
    }
}

pub fn plugin_state() -> PluginState {
    select(find_plugins())
}

/// Явна згода користувача на завантаження закритого плагіна. Створюється лише
/// [`Consent::accept`] (UI після натискання «Погоджуюсь») або з раніше збереженої згоди.
pub struct Consent {
    _private: (),
}

#[derive(Serialize, Deserialize)]
struct ConsentRecord {
    revision: u32,
    accepted_at: u64,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Consent {
    /// Викликати лише у відповідь на дію користувача, якому показано текст: плагін закритий,
    /// від Bambu Lab, завантажується з серверів Bambu Lab, програма його не розробляє.
    pub fn accept() -> Result<Self> {
        Self::accept_in(&managed_root())
    }
    fn accept_in(root: &Path) -> Result<Self> {
        create_private_dir(root)?;
        let record = ConsentRecord {
            revision: CONSENT_REVISION,
            accepted_at: now(),
        };
        write_private(&root.join(CONSENT), &serde_json::to_vec(&record)?)?;
        Ok(Self { _private: () })
    }
    /// Збережена раніше згода поточної редакції (для оновлень без повторного питання).
    pub fn recorded() -> Option<Self> {
        Self::recorded_in(&managed_root())
    }
    fn recorded_in(root: &Path) -> Option<Self> {
        let record: ConsentRecord =
            serde_json::from_slice(&fs::read(root.join(CONSENT)).ok()?).ok()?;
        (record.revision == CONSENT_REVISION).then_some(Self { _private: () })
    }
    pub fn revoke() -> Result<()> {
        match fs::remove_file(managed_root().join(CONSENT)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemotePlugin {
    pub version: String,
    url: String,
    /// Content-Length з HEAD, якщо сервер його дав.
    pub size: Option<u64>,
}

fn api_host(country_code: &str) -> &'static str {
    // GUI_App::get_http_url: CN — свій домен, решта (US/Others) — .com.
    if country_code == "CN" {
        "https://api.bambulab.cn/"
    } else {
        "https://api.bambulab.com/"
    }
}

fn trusted_download(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    !host.contains(['@', ':'])
        && ["bblmw.com", "bambulab.com", "bambulab.cn"]
            .iter()
            .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

/// Розбір відповіді `v1/iot-service/api/slicer/resource` (формат як у Orca download_plugin).
pub(crate) fn parse_resource(body: &[u8]) -> Result<(String, String)> {
    let v: Value = serde_json::from_slice(body).context("Сервер Bambu: некоректна відповідь")?;
    ensure!(
        v["message"] == "success",
        "Сервер Bambu не віддав плагін для цієї ОС"
    );
    let r = v["resources"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|r| r["type"] == "slicer/plugins/cloud")
        .context("Сервер Bambu не віддав плагін для цієї ОС")?;
    let version = r["version"].as_str().unwrap_or_default().to_owned();
    let url = r["url"].as_str().unwrap_or_default().to_owned();
    ensure!(
        abi::is_version(&version),
        "Сервер Bambu: некоректна версія плагіна"
    );
    ensure!(
        trusted_download(&url),
        "Сервер Bambu: посилання не на домен Bambu Lab"
    );
    Ok((version, url))
}

/// Сегмент ОС для сервера плагінів — як у OrcaSlicer `GUI_App::download_plugin`
/// (`windows`/`windows_arm`/`macos`/`linux`). Сервер за ним віддає збірку для ОС.
const OS_HEADER: (&str, &str) = ("X-BBL-OS-Type", OS_TYPE);
#[cfg(all(windows, target_arch = "aarch64"))]
pub const OS_TYPE: &str = "windows_arm";
#[cfg(all(windows, not(target_arch = "aarch64")))]
pub const OS_TYPE: &str = "windows";
#[cfg(target_os = "macos")]
pub const OS_TYPE: &str = "macos";
#[cfg(not(any(windows, target_os = "macos")))]
pub const OS_TYPE: &str = "linux";

/// Яку збірку серії зараз віддає Bambu (`?slicer/plugins/cloud=<серія>.00`). Нічого не завантажує.
pub fn fetch_remote(
    transport: &dyn Transport,
    country_code: &str,
    series: &str,
) -> Result<RemotePlugin> {
    ensure!(
        abi::is_version(&format!("{series}.00")),
        "некоректна серія плагіна"
    );
    let url = format!(
        "{}v1/iot-service/api/slicer/resource?slicer/plugins/cloud={series}.00",
        api_host(country_code)
    );
    let headers = [(OS_HEADER.0.to_owned(), OS_HEADER.1.to_owned())];
    let response = transport.request(&Request::get(&url, &headers))?;
    ensure!(
        response.status == 200,
        "Сервер Bambu відповів HTTP {}",
        response.status
    );
    let (version, url) = parse_resource(&response.body)?;
    let size = transport
        .request(&Request {
            method: Method::Head,
            ..Request::get(&url, &headers)
        })
        .ok()
        .filter(|r| r.status == 200)
        .and_then(|r| r.header("content-length")?.parse().ok());
    Ok(RemotePlugin { version, url, size })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InstallProgress {
    Downloading { received: u64, total: Option<u64> },
    Extracting,
    Done,
}

impl InstallProgress {
    /// 0..=1 для смужки; `None` — невизначений прогрес.
    pub fn fraction(&self) -> Option<f32> {
        match *self {
            Self::Downloading {
                received,
                total: Some(t),
            } if t > 0 => Some((received as f32 / t as f32).min(1.0) * 0.9),
            Self::Downloading { .. } => None,
            Self::Extracting => Some(0.95),
            Self::Done => Some(1.0),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    version: String,
    installed_at: u64,
    archive_size: u64,
    files: Vec<String>,
}

fn create_private_dir(dir: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .unix_mode(0o700)
        .create(dir)
        .with_context(|| format!("не вдалося створити {}", dir.display()))
}

fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let mut f = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .unix_mode(0o600)
        .open(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
}

/// Розпаковує лише плоскі звичайні файли з безпечними іменами (жодних тек, симлінків,
/// `..` чи абсолютних шляхів) і з лімітами розміру. CRC перевіряє zip-рідер.
/// Заголовок бібліотеки для цієї ОС: ELF, Mach-O (64-біт або universal) чи PE (`MZ`).
fn is_library_magic(magic: &[u8; 4]) -> bool {
    if cfg!(target_os = "macos") {
        matches!(magic, [0xcf, 0xfa, 0xed, 0xfe] | [0xca, 0xfe, 0xba, 0xbe])
    } else if cfg!(windows) {
        magic.starts_with(b"MZ")
    } else {
        magic == b"\x7fELF"
    }
}

pub(crate) fn extract(archive: &Path, into: &Path) -> Result<Vec<String>> {
    let mut zip =
        zip::ZipArchive::new(fs::File::open(archive)?).context("Архів плагіна пошкоджений")?;
    ensure!(zip.len() <= MAX_ENTRIES, "Архів плагіна: забагато файлів");
    let mut files = Vec::new();
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).context("Архів плагіна пошкоджений")?;
        let name = entry
            .name()
            .context("Архів плагіна: некоректне ім’я")?
            .into_owned();
        if entry.is_dir() {
            bail!("Архів плагіна: неочікувана тека");
        }
        let mode = entry.unix_mode().unwrap_or(0o100644);
        ensure!(
            !entry.is_symlink() && mode & 0o170000 != 0o120000,
            "Архів плагіна: символьні посилання заборонені"
        );
        ensure!(
            plain_name(&name),
            "Архів плагіна: файл поза текою установки"
        );
        ensure!(!files.contains(&name), "Архів плагіна: повторне ім’я файлу");
        let size = entry.size();
        ensure!(size <= MAX_ENTRY, "Архів плагіна: файл завеликий");
        total += size;
        ensure!(total <= MAX_TOTAL, "Архів плагіна завеликий");
        let mut out = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .unix_mode(0o644)
            .open(into.join(&name))?;
        let copied = std::io::copy(&mut (&mut entry).take(size + 1), &mut out)
            .context("Архів плагіна пошкоджений")?;
        ensure!(copied == size, "Архів плагіна пошкоджений");
        out.sync_all()?;
        files.push(name);
    }
    let lib = into.join(LIBRARY);
    let mut magic = [0u8; 4];
    fs::File::open(&lib)
        .and_then(|mut f| f.read_exact(&mut magic))
        .with_context(|| format!("В архіві немає {LIBRARY}"))?;
    ensure!(
        is_library_magic(&magic),
        "{LIBRARY} в архіві — не бібліотека"
    );
    Ok(files)
}

/// Завантажує й атомарно встановлює плагін у `managed_root()/<версія>`.
/// Без [`Consent`] викликати неможливо.
pub fn install(
    transport: &dyn Transport,
    _consent: &Consent,
    remote: &RemotePlugin,
    progress: &mut dyn FnMut(InstallProgress),
    cancel: &AtomicBool,
) -> Result<InstalledPlugin> {
    ensure!(crate::OS_SUPPORTED, "{}", crate::UNSUPPORTED_OS);
    install_in(&managed_root(), transport, remote, progress, cancel)
}

pub(crate) fn install_in(
    root: &Path,
    transport: &dyn Transport,
    remote: &RemotePlugin,
    progress: &mut dyn FnMut(InstallProgress),
    cancel: &AtomicBool,
) -> Result<InstalledPlugin> {
    ensure!(
        abi::generation(&remote.version).is_some(),
        "{}",
        abi::unsupported_message(&remote.version)
    );
    ensure!(
        trusted_download(&remote.url),
        "посилання не на домен Bambu Lab"
    );
    create_private_dir(root)?;
    // Каталог-чернетка в тій самій теці: rename у кінці атомарний; при помилці TempDir прибирає.
    let staging = tempfile::Builder::new()
        .prefix(".staging-")
        .tempdir_in(root)?;
    let archive = staging.path().join("plugin.zip");
    let total = remote.size;
    progress(InstallProgress::Downloading { received: 0, total });
    let received = {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .unix_mode(0o600)
            .open(&archive)?;
        let headers = [(OS_HEADER.0.to_owned(), OS_HEADER.1.to_owned())];
        let n = transport.download(
            &remote.url,
            &headers,
            MAX_ARCHIVE,
            &mut file,
            &mut |received| progress(InstallProgress::Downloading { received, total }),
            cancel,
        )?;
        file.sync_all()?;
        n
    };
    if let Some(expected) = total {
        ensure!(
            received == expected,
            "Плагін завантажився не повністю ({received} з {expected} байт)"
        );
    }
    progress(InstallProgress::Extracting);
    let dir = staging.path().join("plugin");
    fs::DirBuilder::new().unix_mode(0o755).create(&dir)?;
    let files = extract(&archive, &dir)?;
    fs::remove_file(&archive)?;
    let manifest = Manifest {
        version: remote.version.clone(),
        installed_at: now(),
        archive_size: received,
        files,
    };
    write_private(&dir.join(MANIFEST), &serde_json::to_vec_pretty(&manifest)?)?;
    let target = root.join(&remote.version);
    let old = root.join(format!(".old-{}-{}", remote.version, std::process::id()));
    let had_old = fs::symlink_metadata(&target).is_ok();
    if had_old {
        fs::rename(&target, &old)?;
    }
    if let Err(e) = fs::rename(&dir, &target) {
        if had_old {
            let _ = fs::rename(&old, &target);
        }
        return Err(e.into());
    }
    if had_old {
        // Уже завантажена в процес стара копія лишається відображеною до виходу.
        let _ = fs::remove_dir_all(&old);
    }
    progress(InstallProgress::Done);
    Ok(InstalledPlugin {
        origin: PluginOrigin::Managed,
        library: target.join(LIBRARY),
        version: Some(remote.version.clone()),
    })
}

/// Видаляє одну керовану версію (лише з нашої теки).
pub fn remove(version: &str) -> Result<()> {
    remove_in(&managed_root(), version)
}

fn remove_in(root: &Path, version: &str) -> Result<()> {
    ensure!(abi::is_version(version), "некоректна версія");
    let dir = root.join(version);
    match fs::symlink_metadata(&dir) {
        Ok(m) if m.is_dir() => Ok(fs::remove_dir_all(dir)?),
        Ok(_) => bail!("не тека плагіна"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Видаляє всі керовані версії та згоду. Bambu Studio й Orca не чіпає.
pub fn remove_all() -> Result<()> {
    for p in managed_in(&managed_root()) {
        if let Some(v) = &p.version {
            remove(v)?;
        }
    }
    Consent::revoke()
}

// Синтетичні архіви з магією й іменами бібліотек цієї ОС (ELF/`.so` на Linux, MZ/`.dll` на
// Windows).
#[cfg(all(test, any(target_os = "linux", windows)))]
mod tests {
    use super::*;
    use crate::http::Response;
    use std::sync::Mutex;
    use zip::write::SimpleFileOptions;

    struct Mock {
        archive: Vec<u8>,
        requests: Mutex<Vec<(Method, String)>>,
    }
    impl Transport for Mock {
        fn request(&self, r: &Request<'_>) -> Result<Response> {
            self.requests
                .lock()
                .unwrap()
                .push((r.method, r.url.to_owned()));
            assert!(
                r.headers
                    .iter()
                    .any(|(k, v)| k == "X-BBL-OS-Type" && v == OS_TYPE)
            );
            Ok(match r.method {
                Method::Head => Response {
                    status: 200,
                    headers: vec![("content-length".into(), self.archive.len().to_string())],
                    body: vec![],
                },
                _ => Response {
                    status: 200,
                    headers: vec![],
                    body: br#"{"message":"success","resources":[{"type":"slicer/plugins/cloud","version":"02.08.04.60","url":"https://public-cdn.bblmw.com/upgrade/studio/plugins/02.08.04.60/x/linux_02.08.04.60.zip","force_update":false}]}"#.to_vec(),
                },
            })
        }
        fn download(
            &self,
            _url: &str,
            _h: &[(String, String)],
            _max: u64,
            sink: &mut dyn std::io::Write,
            progress: &mut dyn FnMut(u64),
            _cancel: &AtomicBool,
        ) -> Result<u64> {
            for chunk in self.archive.chunks(7) {
                sink.write_all(chunk)?;
            }
            progress(self.archive.len() as u64);
            Ok(self.archive.len() as u64)
        }
    }

    fn zip_of(entries: &[(&str, &[u8])], symlink: Option<&str>) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut out);
        for (name, data) in entries {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(data).unwrap();
        }
        if let Some(name) = symlink {
            zip.add_symlink(name, "/etc/passwd", SimpleFileOptions::default())
                .unwrap();
        }
        zip.finish().unwrap();
        out.into_inner()
    }

    #[cfg(windows)]
    const ELF: &[u8] = b"MZ-synthetic";
    #[cfg(not(windows))]
    const ELF: &[u8] = b"\x7fELF-synthetic";

    #[test]
    fn resource_parsing_requires_bambu_domain_and_version() {
        let (v, url) = parse_resource(br#"{"message":"success","resources":[{"type":"slicer/plugins/cloud","version":"02.06.00.50","url":"https://public-cdn.bblmw.com/a.zip"}]}"#).unwrap();
        assert_eq!(v, "02.06.00.50");
        assert!(url.ends_with("a.zip"));
        for bad in [
            r#"{"message":"fail"}"#,
            r#"{"message":"success","resources":[]}"#,
            r#"{"message":"success","resources":[{"type":"slicer/plugins/cloud","version":"02.08.04.60","url":"https://evil.example/a.zip"}]}"#,
            r#"{"message":"success","resources":[{"type":"slicer/plugins/cloud","version":"02.08.04.60","url":"https://bblmw.com@evil.example/a.zip"}]}"#,
            r#"{"message":"success","resources":[{"type":"slicer/plugins/cloud","version":"../x","url":"https://public-cdn.bblmw.com/a.zip"}]}"#,
            r#"{"message":"success","resources":[{"type":"slicer/plugins/cloud","version":"02.08.04.60","url":"http://public-cdn.bblmw.com/a.zip"}]}"#,
        ] {
            assert!(parse_resource(bad.as_bytes()).is_err(), "{bad}");
        }
    }

    #[test]
    fn install_is_atomic_and_listed_then_removed() {
        let root = tempfile::tempdir().unwrap();
        let mock = Mock {
            archive: zip_of(&[(LIBRARY, ELF), (SOURCE_LIBRARY, b"src")], None),
            requests: Mutex::default(),
        };
        let remote = fetch_remote(&mock, "US", abi::LATEST_SERIES).unwrap();
        assert_eq!(remote.version, "02.08.04.60");
        assert_eq!(remote.size, Some(mock.archive.len() as u64));
        assert!(
            mock.requests.lock().unwrap()[0]
                .1
                .ends_with("?slicer/plugins/cloud=02.08.04.00")
        );
        let mut seen = Vec::new();
        let cancel = AtomicBool::new(false);
        let p = install_in(root.path(), &mock, &remote, &mut |p| seen.push(p), &cancel).unwrap();
        assert_eq!(seen.last(), Some(&InstallProgress::Done));
        assert_eq!(p.generation(), Some(Generation::V0208));
        assert_eq!(fs::read(&p.library).unwrap(), ELF);
        assert!(p.source_library().is_file());
        // Друга установка тієї самої версії замінює теку, не лишаючи сміття.
        install_in(root.path(), &mock, &remote, &mut |_| {}, &cancel).unwrap();
        let names: Vec<_> = fs::read_dir(root.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, vec!["02.08.04.60".to_string()]);
        let found = managed_in(root.path());
        assert_eq!(found.len(), 1);
        assert!(matches!(select(found), PluginState::Ready(_)));
        remove_in(root.path(), "02.08.04.60").unwrap();
        assert!(managed_in(root.path()).is_empty());
        assert!(remove_in(root.path(), "../etc").is_err());
    }

    #[test]
    fn hostile_archives_are_rejected_without_leftovers() {
        let cancel = AtomicBool::new(false);
        for archive in [
            zip_of(&[("../evil.so", ELF), (LIBRARY, ELF)], None),
            zip_of(&[("sub/x.so", ELF), (LIBRARY, ELF)], None),
            zip_of(&[(LIBRARY, ELF)], Some(SOURCE_LIBRARY)),
            zip_of(&[(SOURCE_LIBRARY, b"x")], None),
            zip_of(&[(LIBRARY, b"not elf")], None),
            b"not a zip".to_vec(),
        ] {
            let root = tempfile::tempdir().unwrap();
            let mock = Mock {
                archive,
                requests: Mutex::default(),
            };
            let remote = fetch_remote(&mock, "US", "02.08.04").unwrap();
            assert!(install_in(root.path(), &mock, &remote, &mut |_| {}, &cancel).is_err());
            assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
            assert!(!root.path().parent().unwrap().join("evil.so").exists());
        }
    }

    #[test]
    fn truncated_download_and_unsupported_versions_fail() {
        let root = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let mock = Mock {
            archive: zip_of(&[(LIBRARY, ELF)], None),
            requests: Mutex::default(),
        };
        let mut remote = fetch_remote(&mock, "US", "02.08.04").unwrap();
        remote.size = Some(remote.size.unwrap() + 1);
        assert!(install_in(root.path(), &mock, &remote, &mut |_| {}, &cancel).is_err());
        remote.version = "01.10.01.01".into();
        let e = install_in(root.path(), &mock, &remote, &mut |_| {}, &cancel)
            .unwrap_err()
            .to_string();
        assert!(e.contains("не підтримується"));
    }

    #[test]
    fn foreign_plugins_keep_priority_and_orca_versions() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path()
                .join(format!("{}01.10.01.01{}", VERSIONED.0, VERSIONED.1)),
            ELF,
        )
        .unwrap();
        let orca = foreign_in(dir.path(), PluginOrigin::OrcaSlicer);
        assert_eq!(orca[0].version.as_deref(), Some("01.10.01.01"));
        assert!(matches!(select(orca.clone()), PluginState::Unsupported(_)));
        fs::write(dir.path().join(LIBRARY), ELF).unwrap();
        let both = foreign_in(dir.path(), PluginOrigin::OrcaSlicer);
        assert!(both[0].version.is_none());
        assert!(matches!(select(both), PluginState::Ready(_)));
        assert!(matches!(select(vec![]), PluginState::Missing));
    }

    #[test]
    fn consent_is_persisted_per_revision() {
        let root = tempfile::tempdir().unwrap();
        assert!(Consent::recorded_in(root.path()).is_none());
        Consent::accept_in(root.path()).unwrap();
        assert!(Consent::recorded_in(root.path()).is_some());
        fs::write(
            root.path().join(CONSENT),
            r#"{"revision":0,"accepted_at":1}"#,
        )
        .unwrap();
        assert!(Consent::recorded_in(root.path()).is_none());
    }
}
