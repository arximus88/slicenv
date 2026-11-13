//! Відкритий мережевий плагін Open Bamboo Networking (OBN) у збірці persano — **лише за явним
//! вибором користувача** (docs/OPEN-PLUGIN.md).
//!
//! Чому взагалі: закритий плагін Bambu приймає друк лише від програм із підписом Bambu Lab
//! (-26, [`crate::PluginError::UNSIGNED_SOFTWARE`]), а Bambu Connect для Linux немає. Форк
//! persano (`github.com/persano/open-bamboo-networking`, гілка `option-b-cloud-rescue`) сам
//! отримує спільний сертифікат слайсера з ендпойнта Bambu (`appcert_fetch.cpp`; для цього у
//! бінарнику вшитий `client_auth_secret`), ставить його на принтер (`security.app_cert_install`)
//! і підписує `project_file` — тому Developer Mode не потрібен.
//!
//! Що робить Slicenv, а чого ні:
//! - бінарники й секрет форку **ніколи** не потрапляють у репозиторій, наш бінарник чи пакунки;
//! - користувач або дає взяти вже встановлений плагін (Flatpak OrcaSlicer, `~/.config/OrcaSlicer`,
//!   маркетплейс Orca `orca_plugins/`), або погоджується завантажити реліз
//!   `persano/open-bambu-networking-plugin` з GitHub (SHA-256 з API релізу + `RECORD` колеса);
//! - файли копіюються в нашу теку ([`paths::open_plugin_root`]); чужі теки лише читаються;
//! - наш `obn.conf` лежить у [`paths::open_plugin_config_dir`], там же плагін тримає сесію
//!   `obn.auth.json` (токен відкритим текстом, тому тека 0700) і ключі `slicer_*.pem`.
use crate::{
    InstalledPlugin, PluginOrigin,
    http::{Request, Transport},
};
use anyhow::{Context as _, Result, bail, ensure};
use paths::fs_mode::{ModeExt as _, NoFollowExt as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    fs,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{SystemTime, UNIX_EPOCH},
};

/// Відкритий плагін підключаємо лише там, де його збірку перевірено: Linux x86_64.
pub const SUPPORTED: bool = cfg!(all(target_os = "linux", target_arch = "x86_64"));
/// Код форку (для посилання в UI).
pub const SOURCE_URL: &str = "https://github.com/persano/open-bamboo-networking";
/// Репозиторій релізів (колеса маркетплейсу OrcaSlicer).
pub const RELEASE_REPO: &str = "persano/open-bambu-networking-plugin";
pub const RELEASES_URL: &str = "https://github.com/persano/open-bambu-networking-plugin/releases";
/// Колесо для Linux x86_64 у релізі.
pub const WHEEL_ASSET: &str = "open_bambu_networking_linux_x86_64.whl";
/// Де в колесі лежать бібліотеки (так само їх копіює інсталятор маркетплейсу Orca).
const WHEEL_BIN: &str = "open_bambu_networking/bin/linux_x64/";
/// Серія ABI, яку вміє шим ([`crate::abi`]); бібліотека OBN звітує `<серія>.99`.
const SERIES: &str = "02.08.04";
const PLUGIN_VERSION: &str = "02.08.04.99";
/// Кандидати на основну бібліотеку: версійована копія Orca, далі типова (у колесі — 02.08.04).
const MAIN_NAMES: [&str; 2] = ["libbambu_networking_02.08.04.so", "libbambu_networking.so"];
/// Супутні бібліотеки з того самого пакета (камера).
const EXTRA_NAMES: [&str; 2] = ["libBambuSource.so", "liblive555.so"];
const LIBRARY: &str = "libbambu_networking.so";
const MANIFEST: &str = "manifest.json";
const CHOICE: &str = "open-plugin.json";
/// Сесія, яку пише сам плагін.
pub const SESSION_FILE: &str = "obn.auth.json";
const CONF_FILE: &str = "obn.conf";
const MAX_LIBRARY: u64 = 64 * 1024 * 1024;
const MAX_WHEEL: u64 = 64 * 1024 * 1024;
const MAX_WHEEL_ENTRIES: usize = 256;

/// Редакція попередження: зміна тексту → нова згода.
pub const WARNING_REVISION: u32 = 1;
/// Коротке чесне попередження (ключ перекладу в застосунку).
pub const WARNING_TEXT: &str = "Відкритий плагін Open Bamboo Networking (збірка persano) — сторонній, не від Bambu Lab і не від Slicenv. Він друкує без Developer Mode, бо містить ключ, витягнутий із програми Bambu, і представляється як Bambu Studio. Це може порушувати умови Bambu Lab: Bambu може заблокувати такий доступ або акаунт, і друк перестане працювати. Використовуєте на власний розсуд.";

// ---------------------------------------------------------------------------------------------
// Розпізнавання

/// Яка це збірка OBN.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Flavor {
    /// Форк persano: сам отримує ключі слайсера й рятує хмарні `project_file` підписом.
    Persano,
    /// ClusterM без ключів: без Developer Mode друкувати не може.
    Upstream,
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    let Some((&first, rest)) = needle.split_first() else {
        return true;
    };
    let mut i = 0;
    while let Some(pos) = hay[i..].iter().position(|&b| b == first) {
        let at = i + pos;
        if hay[at + 1..].starts_with(rest) {
            return true;
        }
        i = at + 1;
    }
    false
}

/// Збірка OBN за рядками в бінарнику, без завантаження (dlopen). Закритий плагін → `None`.
pub fn flavor_of_bytes(bytes: &[u8]) -> Option<Flavor> {
    if !(contains(bytes, b"open-bamboo-networking") || contains(bytes, b"obn.conf")) {
        return None;
    }
    // Рядки з agent.cpp / appcert_fetch.cpp, яких немає в ClusterM.
    if contains(bytes, b"rescue_cloud_project_file") || contains(bytes, b"appcert fetch") {
        Some(Flavor::Persano)
    } else {
        Some(Flavor::Upstream)
    }
}

fn read_regular(path: &Path, max: u64) -> Result<Vec<u8>> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(meta.is_file(), "{} — не звичайний файл", path.display());
    ensure!(meta.len() <= max, "{} завеликий", path.display());
    let mut file = fs::OpenOptions::new().read(true).no_follow().open(path)?;
    let mut out = Vec::with_capacity(meta.len() as usize);
    std::io::Read::by_ref(&mut file).take(max + 1).read_to_end(&mut out)?;
    ensure!(out.len() as u64 <= max, "{} завеликий", path.display());
    Ok(out)
}

/// Чи це OBN (будь-якої збірки) — щоб пошук закритого плагіна його не підхопив мовчки.
///
/// Результат кешується за (шлях, розмір, час зміни): робочий потік хмари в режимі очікування
/// питає це кожні 2 с, а бібліотека закритого плагіна — десятки мегабайт (D6).
pub fn is_open_plugin(path: &Path) -> bool {
    type Key = (PathBuf, u64, Option<std::time::SystemTime>);
    static CACHE: std::sync::Mutex<Vec<(Key, bool)>> = std::sync::Mutex::new(Vec::new());
    let key = fs::symlink_metadata(path)
        .ok()
        .map(|m| (path.to_owned(), m.len(), m.modified().ok()));
    if let Some(key) = &key
        && let Some(hit) = CACHE
            .lock()
            .ok()
            .and_then(|c| c.iter().find(|(k, _)| k == key).map(|(_, v)| *v))
    {
        return hit;
    }
    let result = read_regular(path, MAX_LIBRARY)
        .ok()
        .is_some_and(|b| flavor_of_bytes(&b).is_some());
    if let (Some(key), Ok(mut cache)) = (key, CACHE.lock()) {
        cache.retain(|(k, _)| k.0 != key.0);
        cache.push((key, result));
    }
    result
}

fn elf(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x7fELF")
}

// ---------------------------------------------------------------------------------------------
// obn.conf

/// `obn.conf` у форматі OBN (`config.cpp` `parse_line`): рядок, що починається з `#`, —
/// коментар; інакше `ключ = значення` з обрізаними пробілами; останнє входження виграє.
/// Коментарі й невідомі ключі зберігаються як є.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Conf {
    lines: Vec<String>,
}

fn conf_entry(line: &str) -> Option<(&str, &str)> {
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    (!key.is_empty() && !line.starts_with('=')).then(|| (key, value.trim()))
}

impl Conf {
    pub fn parse(text: &str) -> Self {
        Self {
            lines: text.lines().map(str::to_owned).collect(),
        }
    }
    pub fn get(&self, key: &str) -> Option<&str> {
        self.lines
            .iter()
            .rev()
            .filter_map(|l| conf_entry(l))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
    }
    /// Замінює значення всіх активних входжень ключа; немає — дописує в кінець.
    pub fn set(&mut self, key: &str, value: &str) {
        let mut found = false;
        for line in &mut self.lines {
            if conf_entry(line).is_some_and(|(k, _)| k == key) {
                *line = format!("{key} = {value}");
                found = true;
            }
        }
        if !found {
            self.lines.push(format!("{key} = {value}"));
        }
    }
    pub fn render(&self) -> String {
        let mut out = self.lines.join("\n");
        out.push('\n');
        out
    }
}

/// Ключі, які Slicenv ставить у свій `obn.conf`.
/// - `block_cloud`: 1 — лише LAN і REST акаунта (як у дизайнера 2026-11-09); 0 — хмарний
///   MQTT і хмарний друк (README OBN, «Option B»).
/// - `client_name = BambuStudio`: з іншим іменем хмара відповідає 403 на `/my/task`.
/// - логи вимкнено ([`QUIET_KEYS`]): за README OBN вони містять токени, коди доступу й
///   серійники без маскування.
pub fn required_keys(cloud: bool) -> Vec<(&'static str, &'static str)> {
    let mut keys = vec![
        ("block_cloud", if cloud { "0" } else { "1" }),
        ("client_name", "BambuStudio"),
        ("cloud_print", "cloud_only"),
    ];
    keys.extend_from_slice(QUIET_KEYS);
    keys
}

/// Ключі `obn.conf`, що вимикають усі стоки логів OBN: stderr і файли основного плагіна
/// (`log.cpp`: порожній `log_file` — без файлу) і libBambuSource (`source_log.cpp`:
/// `bambusource_log_file = off` — без файлу, навіть якщо хтось увімкне `*_to_file`).
pub const QUIET_KEYS: &[(&str, &str)] = &[
    ("log_stderr", "0"),
    ("log_to_file", "0"),
    ("log_file", ""),
    ("bambusource_log_stderr", "0"),
    ("bambusource_log_to_file", "0"),
    ("bambusource_log_file", "off"),
];

/// Змінні оточення OBN, які мають пріоритет над `obn.conf` (`log.cpp` `ensure_initialized_locked`
/// і `apply_config`; `source_log.cpp` через `lan_tls::env_var_get`). Порожній `OBN_LOG_FILE`
/// означає «явний шлях не задано» і блокує файл `<log_dir>/obn.log` та `log_file` з конфігу.
///
/// `OBN_BAMBUSOURCE_LOG_*` плагін сам перезаписує значеннями з конфігу, прочитаного в
/// `create_agent` (`lan_tls::propagate_cross_so_env`), тож головний захист — [`seed_log_dir`];
/// змінні тут — запобіжник для libBambuSource і для коду, що логує до `create_agent`.
pub const QUIET_ENV: &[(&str, &str)] = &[
    ("OBN_LOG_STDERR", "0"),
    ("OBN_LOG_TO_FILE", "0"),
    ("OBN_LOG_FILE", ""),
    ("OBN_BAMBUSOURCE_LOG_STDERR", "0"),
    ("OBN_BAMBUSOURCE_LOG_TO_FILE", "0"),
    ("OBN_BAMBUSOURCE_LOG_FILE", "off"),
];

/// Задає [`QUIET_ENV`] (перезаписуючи — журнал застосунку не має отримувати сирі логи
/// стороннього плагіна, навіть якщо хтось виставив `OBN_LOG_STDERR=1`).
///
/// # Safety
/// Змінює оточення процесу: викликати на старті, до створення інших потоків.
pub unsafe fn quiet_process_env() {
    for (k, v) in QUIET_ENV {
        unsafe { std::env::set_var(k, v) };
    }
}

/// OBN у `bambu_network_create_agent(log_dir)` читає `obn.conf` **з `log_dir`**
/// (`abi_lifecycle.cpp`: `config::load_or_create(log_dir)` → `log::apply_config`), а не з теки
/// конфігурації: та приходить пізніше через `set_config_dir`, коли налаштування логів уже
/// застосовано (одноразово, `config_applied`). Без файлу в `log_dir` плагін створює шаблон із
/// `log_stderr = 1` — і пише в stderr застосунку e-mail, user id, серійники.
///
/// Тому перед `create_agent` кладемо в `log_dir` копію нашого `obn.conf` (ті самі
/// `block_cloud`, `client_name`…) з примусово вимкненими логами ([`QUIET_KEYS`]).
/// Немає `obn.conf` у `config_dir` (закритий плагін Bambu) — нічого не робимо.
pub fn seed_log_dir(config_dir: &Path, log_dir: &Path) -> Result<bool> {
    let Ok(bytes) = read_regular(&config_dir.join(CONF_FILE), 1024 * 1024) else {
        return Ok(false);
    };
    let mut conf = Conf::parse(&String::from_utf8_lossy(&bytes));
    for (k, v) in QUIET_KEYS {
        conf.set(k, v);
    }
    write_private(&log_dir.join(CONF_FILE), conf.render().as_bytes())?;
    Ok(true)
}

fn header() -> &'static str {
    "# obn.conf, записаний Slicenv. Ключі нижче Slicenv перезаписує при кожному запуску\n\
     # відкритого плагіна; решту (якщо додасте) не чіпає. Формат — README Open Bamboo Networking."
}

/// Оновлює (або створює) `obn.conf` у теці `dir`; атомарно, файл 0600.
pub fn write_conf(dir: &Path, cloud: bool) -> Result<()> {
    let path = dir.join(CONF_FILE);
    let existing = read_regular(&path, 1024 * 1024).ok();
    let text = match &existing {
        Some(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        None => header().to_owned(),
    };
    let mut conf = Conf::parse(&text);
    for (k, v) in required_keys(cloud) {
        conf.set(k, v);
    }
    let rendered = conf.render();
    // Без змін — не чіпаємо файл: mtime теки сесії стежить робочий потік хмари.
    if existing.as_deref() == Some(rendered.as_bytes()) {
        return Ok(());
    }
    write_private(&path, rendered.as_bytes())
}

fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .unix_mode(0o600)
        .open(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, path)?;
    Ok(())
}

fn private_dir(dir: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .unix_mode(0o700)
        .create(dir)
        .with_context(|| format!("не вдалося створити теку {}", dir.display()))?;
    paths::fs_mode::set_mode(dir, 0o700)?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Вибір користувача

/// Звідки взято плагін.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    OrcaFlatpak,
    Orca,
    BambuStudioFlatpak,
    BambuStudio,
    GitHub,
}

impl Source {
    /// Для UI (ключ перекладу не потрібен — власні назви).
    pub fn label(self) -> &'static str {
        match self {
            Source::OrcaFlatpak => "OrcaSlicer (Flatpak)",
            Source::Orca => "OrcaSlicer",
            Source::BambuStudioFlatpak => "Bambu Studio (Flatpak)",
            Source::BambuStudio => "Bambu Studio",
            Source::GitHub => "GitHub persano/open-bambu-networking-plugin",
        }
    }
    fn key(self) -> &'static str {
        match self {
            Source::OrcaFlatpak => "orca-flatpak",
            Source::Orca => "orca",
            Source::BambuStudioFlatpak => "studio-flatpak",
            Source::BambuStudio => "studio",
            Source::GitHub => "github",
        }
    }
}

/// Запам’ятований вибір (`~/.local/share/slicenv/cloud/open-plugin.json`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub source: Source,
    /// Версія пакета persano (0.2.48), якщо відома.
    pub release: Option<String>,
    pub flavor: Flavor,
    /// Тека в [`paths::open_plugin_root`].
    pub dir_name: String,
    /// Хмара увімкнена (`block_cloud = 0`).
    pub cloud: bool,
    pub warning_revision: u32,
    pub chosen_at: u64,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    source: Source,
    release: Option<String>,
    flavor: Flavor,
    plugin_version: String,
    /// Ім’я файлу → SHA-256 (hex).
    files: Vec<(String, String)>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Де лежать наші файли відкритого плагіна (тести підставляють тимчасові теки).
#[derive(Clone, Debug)]
pub struct Places {
    /// Копії бібліотек.
    pub root: PathBuf,
    /// Файл вибору.
    pub choice: PathBuf,
    /// Тека конфігурації плагіна (`obn.conf`, сесія).
    pub config: PathBuf,
}

impl Places {
    pub fn standard() -> Self {
        Self {
            root: paths::open_plugin_root(),
            choice: paths::data_dir().join("cloud").join(CHOICE),
            config: paths::open_plugin_config_dir(),
        }
    }
    pub fn under(dir: &Path) -> Self {
        Self {
            root: dir.join("open-plugin"),
            choice: dir.join("cloud").join(CHOICE),
            config: dir.join("cloud/obn"),
        }
    }
}

fn choice_in(places: &Places) -> Option<Choice> {
    let choice: Choice = serde_json::from_slice(&read_regular(&places.choice, 64 * 1024).ok()?).ok()?;
    (choice.warning_revision == WARNING_REVISION).then_some(choice)
}

/// Вибір, якщо користувач його зробив і погодився з поточною редакцією попередження.
pub fn choice() -> Option<Choice> {
    if !SUPPORTED {
        return None;
    }
    choice_in(&Places::standard())
}

fn save_choice(places: &Places, choice: &Choice) -> Result<()> {
    private_dir(places.choice.parent().context("no parent")?)?;
    write_private(&places.choice, &serde_json::to_vec_pretty(choice)?)
}

fn plain_dir_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b'_')
        && !name.starts_with('.')
}

/// Вибраний і встановлений плагін — для [`crate::plugin::find_plugins`].
pub fn active() -> Option<InstalledPlugin> {
    if !SUPPORTED {
        return None;
    }
    active_in(&Places::standard())
}

fn active_in(places: &Places) -> Option<InstalledPlugin> {
    let choice = choice_in(places)?;
    if !plain_dir_name(&choice.dir_name) {
        return None;
    }
    let dir = places.root.join(&choice.dir_name);
    let manifest: Manifest = serde_json::from_slice(&read_regular(&dir.join(MANIFEST), 64 * 1024).ok()?).ok()?;
    let library = dir.join(LIBRARY);
    fs::symlink_metadata(&library)
        .is_ok_and(|m| m.is_file())
        .then_some(InstalledPlugin {
            origin: PluginOrigin::OpenBamboo,
            library,
            version: Some(manifest.plugin_version),
        })
}

/// Чи увімкнена хмара для відкритого плагіна (інакше лише LAN).
pub fn cloud_enabled() -> bool {
    choice().is_some_and(|c| c.cloud)
}

/// Тека конфігурації плагіна з актуальним `obn.conf` (викликати перед `Client::load`).
pub fn prepare_config_dir() -> Result<PathBuf> {
    prepare_config_in(&Places::standard())
}

fn prepare_config_in(places: &Places) -> Result<PathBuf> {
    private_dir(&places.config)?;
    write_conf(&places.config, choice_in(places).is_some_and(|c| c.cloud))?;
    Ok(places.config.clone())
}

/// Увімкнути чи вимкнути хмару: запам’ятовується й одразу переписує `obn.conf`.
pub fn set_cloud(enabled: bool) -> Result<()> {
    set_cloud_in(&Places::standard(), enabled)
}

fn set_cloud_in(places: &Places, enabled: bool) -> Result<()> {
    let mut choice = choice_in(places).context("відкритий плагін не вибрано")?;
    choice.cloud = enabled;
    save_choice(places, &choice)?;
    prepare_config_in(places).map(|_| ())
}

/// Чи є сесія відкритого плагіна (`obn.auth.json`).
pub fn has_session() -> bool {
    fs::symlink_metadata(paths::open_plugin_config_dir().join(SESSION_FILE))
        .is_ok_and(|m| m.is_file() && m.len() > 2)
}

/// Вихід з акаунта відкритого плагіна локально: видаляється лише `obn.auth.json`. Вибір
/// плагіна, бібліотеки й `obn.conf` лишаються, мережевих запитів немає.
pub fn forget_session() -> Result<()> {
    forget_session_in(&paths::open_plugin_config_dir())
}

fn forget_session_in(dir: &Path) -> Result<()> {
    match fs::remove_file(dir.join(SESSION_FILE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Відмова від відкритого плагіна: вибір, копії бібліотек і тека конфігурації (сесія з
/// токеном відкритим текстом, ключі слайсера) видаляються. Чужі теки не чіпаються.
pub fn disable() -> Result<()> {
    disable_in(&Places::standard())
}

fn disable_in(places: &Places) -> Result<()> {
    let _ = fs::remove_file(&places.choice);
    for dir in [&places.root, &places.config] {
        match fs::remove_dir_all(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("не вдалося видалити {}", dir.display())),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Пошук уже встановленого

/// Плагін OBN, знайдений у чужій теці. Нічого не завантажено й не змінено.
#[derive(Clone, Debug)]
pub struct Found {
    pub source: Source,
    pub library: PathBuf,
    pub extras: Vec<PathBuf>,
    pub flavor: Flavor,
    /// Версія пакета маркетплейсу Orca (`.install_state.json`), якщо є.
    pub release: Option<String>,
}

/// Усі знайдені OBN за пріоритетом (Flatpak Orca → Orca → Bambu Studio).
pub fn find_installed() -> Vec<Found> {
    if !SUPPORTED {
        return Vec::new();
    }
    find_in(&paths::home_dir(), &paths::vendor_config_home())
}

/// Пошук від заданих коренів (тести — на фейковому дереві тек).
pub fn find_in(home: &Path, config_home: &Path) -> Vec<Found> {
    let roots = [
        (
            Source::OrcaFlatpak,
            home.join(".var/app/com.orcaslicer.OrcaSlicer/config/OrcaSlicer"),
        ),
        (Source::Orca, config_home.join("OrcaSlicer")),
        (
            Source::BambuStudioFlatpak,
            home.join(".var/app/com.bambulab.BambuStudio/config/BambuStudio"),
        ),
        (Source::BambuStudio, config_home.join("BambuStudio")),
    ];
    let mut out = Vec::new();
    for (source, root) in roots {
        let mut dirs = vec![(root.join("plugins"), None)];
        dirs.extend(marketplace_dirs(&root));
        for (dir, release) in dirs {
            if let Some(found) = found_in_dir(&dir, source, release) {
                out.push(found);
            }
        }
    }
    // Копія в plugins/ (її вантажить Orca) без версії — та сама, що в колесі маркетплейсу?
    let digest = |p: &Path| read_regular(p, MAX_LIBRARY).ok().map(|b| sha256_hex(&b));
    let known: Vec<(Source, Option<String>, String)> = out
        .iter()
        .filter_map(|f| Some((f.source, digest(&f.library)?, f.release.clone()?)))
        .map(|(s, d, r)| (s, Some(r), d))
        .collect();
    for f in out.iter_mut().filter(|f| f.release.is_none()) {
        if let Some(d) = digest(&f.library) {
            f.release = known
                .iter()
                .find(|(s, _, kd)| *s == f.source && *kd == d)
                .and_then(|(_, r, _)| r.clone());
        }
    }
    out
}

/// Розпаковані колеса маркетплейсу Orca:
/// `orca_plugins/_subscribed/<автор>/<плагін>/__whl_extracted__/open_bambu_networking/open_bambu_networking/bin/linux_x64`.
fn marketplace_dirs(root: &Path) -> Vec<(PathBuf, Option<String>)> {
    let mut out = Vec::new();
    let subscribed = root.join("orca_plugins/_subscribed");
    for author in fs::read_dir(&subscribed).into_iter().flatten().flatten() {
        for plugin in fs::read_dir(author.path()).into_iter().flatten().flatten() {
            let base = plugin.path();
            let state: Option<serde_json::Value> = read_regular(&base.join(".install_state.json"), 64 * 1024)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok());
            if state
                .as_ref()
                .is_some_and(|s| s["plugin_name"].as_str().is_some_and(|n| n != "open_bambu_networking"))
            {
                continue;
            }
            let release = state
                .as_ref()
                .and_then(|s| s["installed_version"].as_str())
                .filter(|v| release_version_ok(v))
                .map(str::to_owned);
            let bin = base.join("__whl_extracted__/open_bambu_networking").join(WHEEL_BIN);
            out.push((bin, release));
        }
    }
    out
}

fn release_version_ok(v: &str) -> bool {
    !v.is_empty() && v.len() <= 16 && v.bytes().all(|b| b.is_ascii_digit() || b == b'.')
}

fn found_in_dir(dir: &Path, source: Source, release: Option<String>) -> Option<Found> {
    let (library, bytes) = MAIN_NAMES.iter().find_map(|name| {
        let path = dir.join(name);
        let bytes = read_regular(&path, MAX_LIBRARY).ok()?;
        Some((path, bytes))
    })?;
    if !elf(&bytes) {
        return None;
    }
    let flavor = flavor_of_bytes(&bytes)?;
    let extras = EXTRA_NAMES
        .iter()
        .map(|n| dir.join(n))
        .filter(|p| fs::symlink_metadata(p).is_ok_and(|m| m.is_file()))
        .collect();
    Some(Found {
        source,
        library,
        extras,
        flavor,
        release,
    })
}

// ---------------------------------------------------------------------------------------------
// Установка

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Перевірки основної бібліотеки до копіювання: ELF, OBN, серія ABI, яку вміє шим.
fn check_main(bytes: &[u8]) -> Result<Flavor> {
    ensure!(elf(bytes), "це не бібліотека Linux (ELF)");
    let flavor = flavor_of_bytes(bytes).context("це не Open Bamboo Networking")?;
    ensure!(
        contains(bytes, PLUGIN_VERSION.as_bytes()),
        "бібліотека не серії {SERIES} — Slicenv вміє лише цю серію ABI"
    );
    Ok(flavor)
}

/// Записує файли в чернетку й атомарно перейменовує на `<джерело>-<версія>`.
fn finalize(
    places: &Places,
    source: Source,
    release: Option<String>,
    flavor: Flavor,
    files: Vec<(&'static str, Vec<u8>)>,
) -> Result<Choice> {
    let root = places.root.clone();
    private_dir(&root)?;
    let main_sha = files
        .iter()
        .find(|(n, _)| *n == LIBRARY)
        .map(|(_, b)| sha256_hex(b))
        .context("немає основної бібліотеки")?;
    let dir_name = format!(
        "{}-{}",
        source.key(),
        release.clone().unwrap_or_else(|| main_sha[..12].to_owned())
    );
    ensure!(plain_dir_name(&dir_name), "недопустима назва теки");
    let staging = root.join(format!(".staging-{}-{}", std::process::id(), now()));
    let _ = fs::remove_dir_all(&staging);
    private_dir(&staging)?;
    let result = (|| -> Result<()> {
        let mut manifest = Manifest {
            source,
            release: release.clone(),
            flavor,
            plugin_version: PLUGIN_VERSION.into(),
            files: Vec::new(),
        };
        for (name, bytes) in &files {
            let mut f = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .unix_mode(0o700)
                .open(staging.join(name))?;
            f.write_all(bytes)?;
            f.sync_all()?;
            manifest.files.push(((*name).into(), sha256_hex(bytes)));
        }
        write_private(&staging.join(MANIFEST), &serde_json::to_vec_pretty(&manifest)?)?;
        let target = root.join(&dir_name);
        match fs::remove_dir_all(&target) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        fs::rename(&staging, &target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result?;
    // Інші копії (старі версії) прибираємо: активна лише одна.
    for entry in fs::read_dir(&root).into_iter().flatten().flatten() {
        if entry.file_name().to_str() != Some(&dir_name) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
    let choice = Choice {
        source,
        release,
        flavor,
        dir_name,
        cloud: choice_in(places).is_some_and(|c| c.cloud),
        warning_revision: WARNING_REVISION,
        chosen_at: now(),
    };
    save_choice(places, &choice)?;
    prepare_config_in(places)?;
    Ok(choice)
}

/// «Використати встановлений»: копія файлів у нашу теку. Користувач уже погодився з
/// попередженням ([`WARNING_TEXT`]) — інакше UI цю функцію не викликає.
pub fn install_found(found: &Found) -> Result<Choice> {
    ensure!(SUPPORTED, "відкритий плагін підключається лише на Linux x86_64");
    install_found_in(&Places::standard(), found)
}

fn install_found_in(places: &Places, found: &Found) -> Result<Choice> {
    let bytes = read_regular(&found.library, MAX_LIBRARY)?;
    let flavor = check_main(&bytes)?;
    let mut files = vec![(LIBRARY, bytes)];
    for extra in &found.extras {
        let Some(name) = EXTRA_NAMES
            .iter()
            .find(|n| extra.file_name().and_then(|f| f.to_str()) == Some(**n))
        else {
            continue;
        };
        let bytes = read_regular(extra, MAX_LIBRARY)?;
        ensure!(elf(&bytes), "{name} — не бібліотека ELF");
        files.push((name, bytes));
    }
    finalize(places, found.source, found.release.clone(), flavor, files)
}

/// `RECORD` колеса: `шлях,sha256=<base64url без =>,розмір`.
fn record_hashes(record: &str) -> Vec<(String, String)> {
    record
        .lines()
        .filter_map(|l| {
            let mut parts = l.split(',');
            let path = parts.next()?;
            let hash = parts.next()?.strip_prefix("sha256=")?;
            Some((path.to_owned(), hash.to_owned()))
        })
        .collect()
}

fn b64url_nopad(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (u32::from(b) << (16 - 8 * i)));
        let chars = chunk.len() + 1;
        for i in 0..chars {
            out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

/// Колесо маркетплейсу Orca (zip): беремо лише бібліотеки з `bin/linux_x64/`, кожну звіряємо
/// з `RECORD`. Нічого не виконується (Python-інсталятор колеса нам не потрібен).
pub fn install_wheel(wheel: &Path, source: Source, release: Option<String>) -> Result<Choice> {
    ensure!(SUPPORTED, "відкритий плагін підключається лише на Linux x86_64");
    install_wheel_in(&Places::standard(), wheel, source, release)
}

fn install_wheel_in(places: &Places, wheel: &Path, source: Source, release: Option<String>) -> Result<Choice> {
    let meta = fs::symlink_metadata(wheel)?;
    ensure!(meta.is_file() && meta.len() <= MAX_WHEEL, "архів плагіна завеликий");
    let mut zip = zip::ZipArchive::new(fs::File::open(wheel)?).context("архів плагіна пошкоджено")?;
    ensure!(zip.len() <= MAX_WHEEL_ENTRIES, "в архіві забагато файлів");
    let record_name = (0..zip.len())
        .filter_map(|i| zip.name_for_index(i).and_then(|n| n.ok()).map(|n| n.into_owned()))
        .find(|n| n.ends_with(".dist-info/RECORD") && n.matches('/').count() == 1)
        .context("в архіві немає RECORD")?;
    let record = {
        let mut entry = zip.by_name(&record_name)?;
        ensure!(entry.size() <= 1024 * 1024, "RECORD завеликий");
        let mut s = String::new();
        entry.by_ref().take(1024 * 1024).read_to_string(&mut s)?;
        s
    };
    let hashes = record_hashes(&record);
    let mut read = |name: &str| -> Result<Option<Vec<u8>>> {
        let path = format!("{WHEEL_BIN}{name}");
        let mut entry = match zip.by_name(&path) {
            Ok(e) => e,
            Err(zip::result::ZipError::FileNotFound) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        ensure!(entry.is_file() && entry.size() <= MAX_LIBRARY, "{name}: завеликий");
        let mut bytes = Vec::new();
        entry.by_ref().take(MAX_LIBRARY + 1).read_to_end(&mut bytes)?;
        ensure!(bytes.len() as u64 <= MAX_LIBRARY, "{name}: завеликий");
        let expected = hashes
            .iter()
            .find(|(p, _)| *p == path)
            .map(|(_, h)| h.as_str())
            .with_context(|| format!("{name} немає в RECORD"))?;
        ensure!(
            b64url_nopad(&Sha256::digest(&bytes)) == expected,
            "{name}: SHA-256 не збігається з RECORD"
        );
        Ok(Some(bytes))
    };
    let main = match read(MAIN_NAMES[0])? {
        Some(b) => b,
        None => read(MAIN_NAMES[1])?.context("в архіві немає libbambu_networking.so")?,
    };
    let flavor = check_main(&main)?;
    let mut files = vec![(LIBRARY, main)];
    for name in EXTRA_NAMES {
        if let Some(bytes) = read(name)? {
            ensure!(elf(&bytes), "{name} — не бібліотека ELF");
            files.push((name, bytes));
        }
    }
    finalize(places, source, release, flavor, files)
}

// ---------------------------------------------------------------------------------------------
// Реліз на GitHub

/// Останній реліз persano: що показати користувачеві перед згодою.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    /// Версія пакета (тег без `v`).
    pub version: String,
    pub url: String,
    pub size: u64,
    /// SHA-256 з API GitHub (`assets[].digest`), hex.
    pub sha256: String,
    pub page: String,
}

const API_LATEST: &str = "https://api.github.com/repos/persano/open-bambu-networking-plugin/releases/latest";
const DOWNLOAD_PREFIX: &str = "https://github.com/persano/open-bambu-networking-plugin/releases/download/";

fn github_headers() -> Vec<(String, String)> {
    vec![
        ("Accept".into(), "application/vnd.github+json".into()),
        ("X-GitHub-Api-Version".into(), "2022-11-28".into()),
        ("User-Agent".into(), "slicenv".into()),
    ]
}

/// Розбір відповіді `releases/latest`. Без `digest` реліз не приймаємо: перевіряти нічим.
pub fn parse_release(body: &[u8]) -> Result<Release> {
    let v: serde_json::Value = serde_json::from_slice(body).context("відповідь GitHub — не JSON")?;
    ensure!(
        v["draft"] != true && v["prerelease"] != true,
        "останній реліз — чернетка або пре-реліз"
    );
    let tag = v["tag_name"].as_str().context("у релізі немає тегу")?.to_owned();
    let version = tag.strip_prefix('v').unwrap_or(&tag).to_owned();
    ensure!(release_version_ok(&version), "незвичний тег релізу");
    let asset = v["assets"]
        .as_array()
        .and_then(|a| a.iter().find(|a| a["name"] == WHEEL_ASSET))
        .with_context(|| format!("у релізі {tag} немає {WHEEL_ASSET}"))?;
    let url = asset["browser_download_url"].as_str().context("немає посилання")?.to_owned();
    ensure!(
        url.starts_with(DOWNLOAD_PREFIX) && url.ends_with(WHEEL_ASSET) && !url.contains(".."),
        "посилання не на реліз persano"
    );
    let size = asset["size"].as_u64().context("немає розміру")?;
    ensure!(size > 0 && size <= MAX_WHEEL, "незвичний розмір архіву");
    let sha256 = asset["digest"]
        .as_str()
        .and_then(|d| d.strip_prefix("sha256:"))
        .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        .context("GitHub не опублікував SHA-256 цього файлу — завантаження не перевірити")?
        .to_ascii_lowercase();
    Ok(Release {
        page: format!("{RELEASES_URL}/tag/{tag}"),
        tag,
        version,
        url,
        size,
        sha256,
    })
}

/// Відомості про останній реліз (лише GitHub API, без Bambu).
pub fn fetch_release(transport: &dyn Transport) -> Result<Release> {
    let headers = github_headers();
    let response = transport.request(&Request::get(API_LATEST, &headers))?;
    if response.status == 404 {
        bail!("У {RELEASE_REPO} немає опублікованих релізів");
    }
    ensure!(response.status == 200, "GitHub відповів {}", response.status);
    parse_release(&response.body)
}

/// Завантажити колесо (розмір і SHA-256 — як в API) і встановити. Лише після згоди.
pub fn install_release(
    transport: &dyn Transport,
    release: &Release,
    progress: &mut dyn FnMut(u64),
    cancel: &AtomicBool,
) -> Result<Choice> {
    ensure!(SUPPORTED, "відкритий плагін підключається лише на Linux x86_64");
    let root = Places::standard().root;
    private_dir(&root)?;
    let mut file = tempfile::Builder::new().prefix(".download-").tempfile_in(&root)?;
    let got = transport.download(&release.url, &[("User-Agent".into(), "slicenv".into())], release.size, file.as_file_mut(), progress, cancel)?;
    ensure!(got == release.size, "завантажено {got} байт замість {}", release.size);
    file.as_file_mut().sync_all()?;
    let bytes = read_regular(file.path(), MAX_WHEEL)?;
    ensure!(sha256_hex(&bytes) == release.sha256, "SHA-256 завантаженого файлу не збігається з релізом");
    install_wheel(file.path(), Source::GitHub, Some(release.version.clone()))
}

// ---------------------------------------------------------------------------------------------
// Друк по LAN в окремому процесі

/// Завдання для дочірнього процесу (stdin, JSON): код доступу не йде через argv.
#[derive(Clone, Serialize, Deserialize)]
pub struct LocalJob {
    pub serial: String,
    pub ip: String,
    pub access_code: String,
    pub name: String,
    pub gcode_3mf: PathBuf,
    pub plate: i32,
    pub task_name: String,
    pub project_name: String,
    pub bed_type: String,
    pub ams_mapping: String,
    pub ams_mapping2: String,
    pub use_ams: bool,
    pub bed_leveling: bool,
    pub flow_cali: bool,
    pub vibration_cali: bool,
    pub layer_inspect: bool,
    pub timelapse: bool,
}

impl std::fmt::Debug for LocalJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LocalJob([redacted])")
    }
}

impl LocalJob {
    pub fn request(&self) -> (crate::PrintRequest, crate::LanTarget) {
        let mut r = crate::PrintRequest::new(
            crate::DeviceId::new(self.serial.clone()),
            self.gcode_3mf.clone(),
            self.gcode_3mf.clone(),
            self.project_name.clone(),
        );
        r.plate_index = self.plate;
        r.task_name = self.task_name.clone();
        r.preset_name = format!("{}_plate_{}", self.project_name, self.plate);
        r.bed_type = self.bed_type.clone();
        r.ams_mapping = self.ams_mapping.clone();
        r.ams_mapping2 = self.ams_mapping2.clone();
        r.use_ams = self.use_ams;
        r.bed_leveling = self.bed_leveling;
        r.flow_cali = self.flow_cali;
        r.vibration_cali = self.vibration_cali;
        r.layer_inspect = self.layer_inspect;
        r.timelapse = self.timelapse;
        let lan = crate::LanTarget {
            ip: self.ip.clone(),
            access_code: self.access_code.clone(),
            name: self.name.clone(),
        };
        (r, lan)
    }
}

/// Опції клієнта для відкритого плагіна: наша тека конфігурації (сесія переживає клієнт).
pub fn options(plugin: &InstalledPlugin) -> Result<crate::Options> {
    let dir = prepare_config_dir()?;
    Ok(crate::Options {
        plugin_path: plugin.library.clone(),
        country_code: crate::account::country_of(&dir),
        http_headers: crate::session::http_headers(
            &paths::vendor_config_dir("BambuStudio"),
            Some(crate::Generation::V0208),
        ),
        studio_config_dir: dir,
        certificate: crate::session::find_certificate(),
        allow_control: false,
        persistent_session: true,
    })
}

/// Тіло дочірнього процесу: завантажити вибраний OBN і викликати `start_local_print`.
/// `progress(stage, code)` — події `PrintingStage*` плагіна (Upload: code = відсотки).
pub fn run_local_print(job: &LocalJob, progress: Box<dyn FnMut(i32, i32) + Send>) -> Result<()> {
    let plugin = active().context("відкритий плагін не вибрано")?;
    let mut options = options(&plugin)?;
    options.allow_control = true;
    let (client, mut events) = crate::Client::load(options)?;
    client.start()?;
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let reader = {
        let stop = stop.clone();
        let mut progress = progress;
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Acquire) {
                if let Ok(crate::Event::PrintProgress { stage, code }) =
                    events.recv_timeout(std::time::Duration::from_millis(100))
                {
                    progress(stage, code);
                }
            }
        })
    };
    let (request, lan) = job.request();
    let result = client.start_local_print(&request, &lan);
    stop.store(true, std::sync::atomic::Ordering::Release);
    let _ = reader.join();
    let _ = client.shutdown();
    result
}

/// Дочірній процес: власний канал звітів, а stdout/stderr плагіна — у /dev/null (OBN може
/// друкувати в лог токени й коди доступу, якщо хтось увімкне `log_stderr`).
#[cfg(unix)]
pub fn private_stdout() -> Result<fs::File> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    let null = fs::OpenOptions::new().write(true).open("/dev/null")?;
    unsafe {
        let saved = libc::dup(libc::STDOUT_FILENO);
        ensure!(saved >= 0, "dup stdout");
        let out = fs::File::from_raw_fd(saved);
        ensure!(
            libc::dup2(null.as_raw_fd(), libc::STDOUT_FILENO) >= 0
                && libc::dup2(null.as_raw_fd(), libc::STDERR_FILENO) >= 0,
            "dup2 /dev/null"
        );
        Ok(out)
    }
}

/// Текст для людини за кодом `BAMBU_NETWORK_ERR_*` друку по LAN (`bambu_networking.hpp`).
pub fn local_print_error(code: i32) -> &'static str {
    match code {
        -6010 => "Немає з’єднання з принтером у мережі — перевірте IP і код доступу",
        -4020 => "Не вдалося вивантажити файл на принтер (FTPS)",
        -4030 => "Файл вивантажено, але команду друку не надіслано (MQTT)",
        -4010 => "Файл завеликий для принтера",
        -14 => "Файл для друку не знайдено",
        -18 => "Друк скасовано",
        _ => "Відкритий плагін не надіслав друк",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_dir_gets_our_conf_with_logs_forced_off() {
        let config = tempfile::tempdir().unwrap();
        let log = tempfile::tempdir().unwrap();
        // Закритий плагін: obn.conf немає — log_dir не чіпаємо.
        assert!(!seed_log_dir(config.path(), log.path()).unwrap());
        assert!(!log.path().join(CONF_FILE).exists());
        fs::write(
            config.path().join(CONF_FILE),
            "# мій коментар\nblock_cloud = 0\nlog_stderr = 1\nlog_to_file = 1\nlog_file = /tmp/x.log\nbambusource_log_file = stderr\n",
        )
        .unwrap();
        assert!(seed_log_dir(config.path(), log.path()).unwrap());
        let seeded = Conf::parse(&fs::read_to_string(log.path().join(CONF_FILE)).unwrap());
        assert_eq!(seeded.get("block_cloud"), Some("0"));
        for (k, v) in QUIET_KEYS {
            assert_eq!(seeded.get(k), Some(*v), "{k}");
        }
        assert!(seeded.render().starts_with("# мій коментар"));
        // Конфіг користувача не змінюється.
        assert!(fs::read_to_string(config.path().join(CONF_FILE)).unwrap().contains("log_stderr = 1"));
    }

    #[test]
    fn conf_keeps_comments_and_unknown_keys_and_overrides_ours() {
        let text = "# коментар\nlog_level = info\nblock_cloud = 0\n  # відступ — не коментар для OBN\nclient_name=OpenBambooNetworking\nblock_cloud = 0\n";
        let mut conf = Conf::parse(text);
        assert_eq!(conf.get("block_cloud"), Some("0"));
        assert_eq!(conf.get("client_name"), Some("OpenBambooNetworking"));
        for (k, v) in required_keys(false) {
            conf.set(k, v);
        }
        let out = conf.render();
        assert!(out.starts_with("# коментар\nlog_level = info\n"));
        assert_eq!(out.matches("block_cloud = 1").count(), 2, "{out}");
        assert!(!out.contains("block_cloud = 0"));
        assert_eq!(conf.get("client_name"), Some("BambuStudio"));
        assert_eq!(conf.get("log_stderr"), Some("0"));
        assert_eq!(conf.get("log_level"), Some("info"));
        // Коментар із «=» не вважається ключем.
        assert_eq!(Conf::parse("# block_cloud = 0\n").get("block_cloud"), None);
        assert_eq!(Conf::parse("=x\n").get(""), None);
    }

    #[test]
    fn conf_is_written_private_and_rewritten_on_mode_change() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(CONF_FILE), "mytask_pop = auto\nblock_cloud = 1\n").unwrap();
        write_conf(dir.path(), true).unwrap();
        let text = fs::read_to_string(dir.path().join(CONF_FILE)).unwrap();
        let conf = Conf::parse(&text);
        assert_eq!(conf.get("block_cloud"), Some("0"));
        assert_eq!(conf.get("mytask_pop"), Some("auto"));
        assert_eq!(conf.get("cloud_print"), Some("cloud_only"));
        write_conf(dir.path(), false).unwrap();
        let conf = Conf::parse(&fs::read_to_string(dir.path().join(CONF_FILE)).unwrap());
        assert_eq!(conf.get("block_cloud"), Some("1"));
        // Повторний запис без змін не чіпає файл.
        let before = fs::metadata(dir.path().join(CONF_FILE)).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_conf(dir.path(), false).unwrap();
        assert_eq!(fs::metadata(dir.path().join(CONF_FILE)).unwrap().modified().unwrap(), before);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(dir.path().join(CONF_FILE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Новий файл — із заголовком Slicenv.
        let fresh = tempfile::tempdir().unwrap();
        write_conf(fresh.path(), false).unwrap();
        assert!(fs::read_to_string(fresh.path().join(CONF_FILE)).unwrap().starts_with("# obn.conf"));
    }

    #[test]
    fn logout_removes_only_the_session_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(SESSION_FILE), "{\"token\":\"x\"}").unwrap();
        fs::write(dir.path().join(CONF_FILE), "block_cloud = 0\n").unwrap();
        forget_session_in(dir.path()).unwrap();
        assert!(!dir.path().join(SESSION_FILE).exists());
        assert!(dir.path().join(CONF_FILE).exists(), "налаштування плагіна мають лишитися");
        // Повторний вихід — не помилка.
        forget_session_in(dir.path()).unwrap();
    }

    #[test]
    fn open_plugin_detection_is_cached_until_the_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("libbambu_networking.so");
        fs::write(&lib, fake_library(b"")).unwrap();
        assert!(!is_open_plugin(&lib));
        assert!(!is_open_plugin(&lib));
        // Інший вміст і розмір — кеш не підставляє старої відповіді.
        fs::write(&lib, fake_library(b" obn.conf open-bamboo-networking ")).unwrap();
        assert!(is_open_plugin(&lib));
    }

    fn fake_library(extra: &[u8]) -> Vec<u8> {
        let mut b = b"\x7fELF\x02\x01\x01 synthetic ".to_vec();
        b.extend_from_slice(extra);
        b
    }
    fn persano() -> Vec<u8> {
        fake_library(b"/src/open-bamboo-networking/agent.cpp rescue_cloud_project_file 02.08.04.99")
    }

    #[test]
    fn flavors_are_told_apart_without_loading() {
        assert_eq!(flavor_of_bytes(&persano()), Some(Flavor::Persano));
        assert_eq!(
            flavor_of_bytes(&fake_library(b"obn.conf 02.08.04.99")),
            Some(Flavor::Upstream)
        );
        assert_eq!(flavor_of_bytes(&fake_library(b"bambu_network_start_print")), None);
        assert!(check_main(&persano()).is_ok());
        assert!(check_main(&fake_library(b"obn.conf 02.09.01.99")).is_err());
        assert!(check_main(b"not elf obn.conf 02.08.04.99").is_err());
        assert!(contains(b"aab", b"ab") && !contains(b"a", b"ab"));
    }

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn installed_plugins_are_found_on_a_fake_tree_and_closed_ones_ignored() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let config = h.join(".config");
        let flatpak = h.join(".var/app/com.orcaslicer.OrcaSlicer/config/OrcaSlicer");
        write(&flatpak.join("plugins/libbambu_networking_02.08.04.so"), &persano());
        write(&flatpak.join("plugins/libbambu_networking.so"), &fake_library(b"closed"));
        write(&flatpak.join("plugins/libBambuSource.so"), &fake_library(b"src"));
        let market = flatpak.join("orca_plugins/_subscribed/author-1/plugin-1");
        write(
            &market.join(".install_state.json"),
            br#"{"plugin_name":"open_bambu_networking","installed_version":"0.2.48"}"#,
        );
        write(
            &market.join("__whl_extracted__/open_bambu_networking").join(WHEEL_BIN).join("libbambu_networking.so"),
            &persano(),
        );
        let other = flatpak.join("orca_plugins/_subscribed/author-2/plugin-2");
        write(&other.join(".install_state.json"), br#"{"plugin_name":"something_else"}"#);
        write(
            &other.join("__whl_extracted__/open_bambu_networking").join(WHEEL_BIN).join("libbambu_networking.so"),
            &persano(),
        );
        // Звичайна Orca — лише закритий плагін: не наш кандидат.
        write(&config.join("OrcaSlicer/plugins/libbambu_networking_02.08.04.so"), &fake_library(b"closed"));
        // Bambu Studio — ClusterM без ключів.
        write(&config.join("BambuStudio/plugins/libbambu_networking.so"), &fake_library(b"obn.conf 02.08.04.99"));
        // Симлінк на OBN — відкидається (O_NOFOLLOW, лише звичайні файли).
        let studio = h.join(".var/app/com.bambulab.BambuStudio/config/BambuStudio/plugins");
        fs::create_dir_all(&studio).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            flatpak.join("plugins/libbambu_networking_02.08.04.so"),
            studio.join("libbambu_networking.so"),
        )
        .unwrap();

        let found = find_in(h, &config);
        let summary: Vec<_> = found.iter().map(|f| (f.source, f.flavor, f.release.clone())).collect();
        assert_eq!(
            summary,
            vec![
                // Ті самі байти, що в колесі маркетплейсу, — версія звідти.
                (Source::OrcaFlatpak, Flavor::Persano, Some("0.2.48".into())),
                (Source::OrcaFlatpak, Flavor::Persano, Some("0.2.48".into())),
                (Source::BambuStudio, Flavor::Upstream, None),
            ],
            "симлінк і закриті плагіни відкидаються"
        );
        assert!(found[0].library.ends_with("libbambu_networking_02.08.04.so"));
        assert_eq!(found[0].extras.len(), 1);
    }

    #[test]
    fn record_hash_encoding_matches_wheel_format() {
        // RECORD колеса persano 0.2.48: icon.png, 15440 байт → KmWz…; тут — відомі вектори.
        assert_eq!(b64url_nopad(&Sha256::digest(b"")), "47DEQpj8HBSa-_TImW-5JCeuQeRkm5NMpJWZG3hSuFU");
        assert_eq!(b64url_nopad(b"f"), "Zg");
        assert_eq!(b64url_nopad(b"fo"), "Zm8");
        assert_eq!(b64url_nopad(b"foo"), "Zm9v");
        let r = record_hashes("a/b.so,sha256=abc,12\nx/RECORD,,\n");
        assert_eq!(r, vec![("a/b.so".to_owned(), "abc".to_owned())]);
    }

    #[test]
    fn release_json_needs_persano_url_and_published_digest() {
        let ok = serde_json::json!({
            "tag_name": "v0.2.48", "draft": false, "prerelease": false,
            "assets": [{
                "name": WHEEL_ASSET, "size": 4454732,
                "digest": "sha256:c4c1c1119d37a603eaa74d7fc979af78658be691f6211a22a4d0623f40bf5d04",
                "browser_download_url": format!("{DOWNLOAD_PREFIX}v0.2.48/{WHEEL_ASSET}")
            }]
        });
        let r = parse_release(ok.to_string().as_bytes()).unwrap();
        assert_eq!(r.version, "0.2.48");
        assert_eq!(r.size, 4454732);
        assert!(r.page.ends_with("/tag/v0.2.48"));
        let mut no_digest = ok.clone();
        no_digest["assets"][0]["digest"] = serde_json::Value::Null;
        assert!(parse_release(no_digest.to_string().as_bytes()).is_err());
        let mut foreign = ok.clone();
        foreign["assets"][0]["browser_download_url"] = format!("https://example.com/{WHEEL_ASSET}").into();
        assert!(parse_release(foreign.to_string().as_bytes()).is_err());
        let mut pre = ok;
        pre["prerelease"] = true.into();
        assert!(parse_release(pre.to_string().as_bytes()).is_err());
    }

    #[test]
    fn found_copy_is_installed_privately_and_choice_survives() {
        let tmp = tempfile::tempdir().unwrap();
        let places = Places::under(&tmp.path().join("data"));
        let src = tmp.path().join("orca/plugins");
        write(&src.join("libbambu_networking_02.08.04.so"), &persano());
        write(&src.join("libBambuSource.so"), &fake_library(b"source"));
        let found = found_in_dir(&src, Source::OrcaFlatpak, Some("0.2.48".into())).unwrap();
        let before = fs::read(&found.library).unwrap();
        let choice = install_found_in(&places, &found).unwrap();
        assert_eq!(choice.dir_name, "orca-flatpak-0.2.48");
        assert!(!choice.cloud);
        // Чужа тека не змінилась.
        assert_eq!(fs::read(&found.library).unwrap(), before);
        let plugin = active_in(&places).unwrap();
        assert_eq!(plugin.origin, PluginOrigin::OpenBamboo);
        assert_eq!(plugin.version.as_deref(), Some(PLUGIN_VERSION));
        assert!(plugin.library.ends_with("orca-flatpak-0.2.48/libbambu_networking.so"));
        // Відкритий плагін існує лише для Linux, і його бібліотека камери зветься `.so`;
        // `source_library()` на macOS і Windows шукає назву закритого плагіна тієї ОС.
        #[cfg(target_os = "linux")]
        assert!(plugin.source_library().is_file());
        let conf = Conf::parse(&fs::read_to_string(places.config.join(CONF_FILE)).unwrap());
        assert_eq!(conf.get("block_cloud"), Some("1"));
        assert_eq!(conf.get("client_name"), Some("BambuStudio"));
        set_cloud_in(&places, true).unwrap();
        let conf = Conf::parse(&fs::read_to_string(places.config.join(CONF_FILE)).unwrap());
        assert_eq!(conf.get("block_cloud"), Some("0"));
        assert!(choice_in(&places).unwrap().cloud);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&places.config).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        // Інша редакція попередження — вибору немає (згоду треба дати знову).
        let mut old = choice_in(&places).unwrap();
        old.warning_revision = 0;
        save_choice(&places, &old).unwrap();
        assert!(active_in(&places).is_none());
        disable_in(&places).unwrap();
        assert!(!places.root.exists() && !places.config.exists() && !places.choice.exists());

        // Закритий плагін під іменем OBN не встановлюється.
        let closed = tmp.path().join("closed");
        write(&closed.join("libbambu_networking.so"), &fake_library(b"closed"));
        assert!(found_in_dir(&closed, Source::Orca, None).is_none());
    }

    fn wheel(dir: &Path, main: &[u8], tamper: bool) -> PathBuf {
        use std::io::Write as _;
        let path = dir.join("obn.whl");
        let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        let files: Vec<(String, Vec<u8>)> = vec![
            ("open_bambu_networking/__init__.py".into(), b"print('not run')".to_vec()),
            (format!("{WHEEL_BIN}libbambu_networking.so"), main.to_vec()),
            (format!("{WHEEL_BIN}libBambuSource.so"), fake_library(b"source")),
        ];
        let mut record = String::new();
        for (name, bytes) in &files {
            let mut hash = b64url_nopad(&Sha256::digest(bytes));
            if tamper && name.ends_with("libbambu_networking.so") {
                hash = b64url_nopad(&Sha256::digest(b"other"));
            }
            record += &format!("{name},sha256={hash},{}\n", bytes.len());
            zip.start_file(name.as_str(), opts).unwrap();
            zip.write_all(bytes).unwrap();
        }
        record += "open_bambu_networking-0.2.48.dist-info/RECORD,,\n";
        zip.start_file("open_bambu_networking-0.2.48.dist-info/RECORD", opts).unwrap();
        zip.write_all(record.as_bytes()).unwrap();
        zip.finish().unwrap();
        path
    }

    #[test]
    fn wheel_libraries_are_checked_against_record() {
        let tmp = tempfile::tempdir().unwrap();
        let places = Places::under(&tmp.path().join("data"));
        let good = wheel(tmp.path(), &persano(), false);
        let choice = install_wheel_in(&places, &good, Source::GitHub, Some("0.2.48".into())).unwrap();
        assert_eq!(choice.dir_name, "github-0.2.48");
        let dir = places.root.join("github-0.2.48");
        assert_eq!(fs::read(dir.join(LIBRARY)).unwrap(), persano());
        assert!(dir.join("libBambuSource.so").is_file());
        assert!(!dir.join("__init__.py").exists());

        let bad = tmp.path().join("bad");
        fs::create_dir_all(&bad).unwrap();
        let tampered = wheel(&bad, &persano(), true);
        let err = install_wheel_in(&places, &tampered, Source::GitHub, None).unwrap_err();
        assert!(format!("{err:#}").contains("RECORD"), "{err:#}");
        // Попередня установка лишилась робочою.
        assert!(active_in(&places).is_some());

        let other = tmp.path().join("other");
        fs::create_dir_all(&other).unwrap();
        let wrong_series = wheel(&other, &fake_library(b"rescue_cloud_project_file obn.conf 02.09.01.99"), false);
        assert!(install_wheel_in(&places, &wrong_series, Source::GitHub, None).is_err());
    }
}
