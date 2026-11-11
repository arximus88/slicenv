use std::{
    collections::BTreeSet,
    env, fs,
    io::{Read, Seek},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::Duration,
};

use anyhow::{Context as _, Result, bail};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EngineKind {
    Bambu,
    Orca,
}

impl EngineKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Bambu => "bambu",
            Self::Orca => "orca",
        }
    }
    fn env_var(self) -> &'static str {
        match self {
            Self::Bambu => "BAMBU_STUDIO",
            Self::Orca => "ORCA_SLICER",
        }
    }
    /// Імена виконуваних файлів у `PATH`. На Windows і macOS CMake Bambu/Orca не перейменовує
    /// ціль у `bambu-studio`, окрім GUI-обгортки Windows (`OUTPUT_NAME "bambu-studio"` /
    /// `"orca-slicer"` у `src/CMakeLists.txt`).
    fn binaries(self) -> &'static [&'static str] {
        #[cfg(windows)]
        return match self {
            Self::Bambu => &["bambu-studio.exe", "BambuStudio.exe"],
            Self::Orca => &["orca-slicer.exe", "OrcaSlicer.exe"],
        };
        #[cfg(not(windows))]
        return match self {
            Self::Bambu => &["bambu-studio", "bambustudio", "BambuStudio"],
            Self::Orca => &["orca-slicer", "OrcaSlicer"],
        };
    }
    /// Типові місця встановлення поза `PATH`.
    fn installed(self) -> Vec<PathBuf> {
        installed_candidates(self)
    }
}

#[cfg(target_os = "macos")]
fn installed_candidates(kind: EngineKind) -> Vec<PathBuf> {
    // Бандли `BambuStudio.app` / `OrcaSlicer.app` (ціль CMake з MACOSX_BUNDLE), виконуваний
    // файл — `Contents/MacOS/<ціль>`. Спершу системні /Applications, потім ~/Applications.
    let bundles: &[(&str, &str)] = match kind {
        EngineKind::Bambu => &[
            ("BambuStudio.app", "BambuStudio"),
            ("Bambu Studio.app", "BambuStudio"),
        ],
        EngineKind::Orca => &[("OrcaSlicer.app", "OrcaSlicer")],
    };
    let roots = [
        PathBuf::from("/Applications"),
        paths::home_dir().join("Applications"),
    ];
    roots
        .iter()
        .flat_map(|root| {
            bundles
                .iter()
                .map(move |(app, exe)| root.join(app).join("Contents/MacOS").join(exe))
        })
        .collect()
}

#[cfg(windows)]
fn installed_candidates(kind: EngineKind) -> Vec<PathBuf> {
    // Інсталятори кладуть у `%ProgramFiles%\Bambu Studio` і `%ProgramFiles%\OrcaSlicer`
    // (CPACK_PACKAGE_INSTALL_DIRECTORY = CPACK_PACKAGE_NAME); winget/scoop — у свої каталоги
    // з `PATH`. Реєстр (`App Paths`) не читаємо: шлях можна задати змінною або вибрати вручну.
    let dirs: &[&str] = match kind {
        EngineKind::Bambu => &["Bambu Studio", "BambuStudio"],
        EngineKind::Orca => &["OrcaSlicer", "Orca Slicer"],
    };
    let mut roots: Vec<PathBuf> = ["ProgramFiles", "ProgramW6432", "LOCALAPPDATA"]
        .iter()
        .filter_map(|v| env::var_os(v).filter(|v| !v.is_empty()).map(PathBuf::from))
        .map(|p| {
            if p.ends_with("Local") {
                p.join("Programs")
            } else {
                p
            }
        })
        .collect();
    if roots.is_empty() {
        roots.push(PathBuf::from(r"C:\Program Files"));
    }
    roots.dedup();
    roots
        .iter()
        .flat_map(|root| {
            dirs.iter().flat_map(move |dir| {
                kind.binaries().iter().map(move |exe| root.join(dir).join(exe))
            })
        })
        .collect()
}

#[cfg(not(any(target_os = "macos", windows)))]
fn installed_candidates(kind: EngineKind) -> Vec<PathBuf> {
    let list: &[&str] = match kind {
        EngineKind::Bambu => &[
            "/opt/bambu-studio/bambu-studio",
            "/opt/BambuStudio/bambu-studio",
        ],
        EngineKind::Orca => &[
            "/opt/orca-slicer/orca-slicer",
            "/opt/OrcaSlicer/orca-slicer",
            "/opt/orca-slicer/bin/orca-slicer",
        ],
    };
    list.iter().map(PathBuf::from).collect()
}

#[derive(Clone, Debug)]
pub struct Engine {
    pub kind: EngineKind,
    pub path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct EngineInfo {
    /// Рядок версії з `--help` як є («OrcaSlicer-01.10.01.50», «BambuStudio-02.03.00.70»):
    /// входить в `identity` і в пошук каталогу профілів — для показу не призначений.
    pub version: String,
    /// Для людей: «OrcaSlicer 2.3.0». Orca ≤ 2.3 у `--help` друкує не свою версію, а
    /// Bambu-сумісну `SLIC3R_VERSION` (2.3.0 → «01.10.01.50»); див. [`release_version`].
    pub label: String,
    pub identity: String,
    pub flags: BTreeSet<String>,
}

impl EngineInfo {
    pub fn supports_flag(&self, flag: &str) -> bool {
        self.flags.contains(flag)
    }
}

impl Engine {
    pub fn discover(kind: EngineKind) -> Result<Self> {
        if let Some(path) = env::var_os(kind.env_var()) {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Ok(Self { kind, path });
            }
            bail!(
                "{} points to a missing executable: {}",
                kind.env_var(),
                path.display()
            );
        }
        let path_dirs = env::var_os("PATH")
            .map(|p| env::split_paths(&p).collect::<Vec<_>>())
            .unwrap_or_default();
        if let Some(path) = path_dirs
            .into_iter()
            .flat_map(|dir| kind.binaries().iter().map(move |name| dir.join(name)))
            .chain(kind.installed())
            .find(|p| p.is_file())
        {
            return Ok(Self { kind, path });
        }
        bail!(
            "{} slicer was not found (set {} or install it in PATH or {})",
            kind.name(),
            kind.env_var(),
            if cfg!(target_os = "macos") {
                "/Applications"
            } else if cfg!(windows) {
                "Program Files"
            } else {
                "/opt"
            }
        )
    }

    /// Рушій, з яким стартує Slicenv: активний з `engines.json`, потім `PATH`/системні
    /// установки, потім версії, встановлені менеджером рушіїв. Спільне для застосунку й
    /// `slicenv-mcp`, щоб обидва бачили той самий рушій і ті самі профілі.
    pub fn startup() -> Result<Self> {
        Self::startup_in(
            &paths::config_dir().join("engines.json"),
            &paths::data_dir().join("engines"),
        )
    }

    /// [`Engine::startup`] з явними шляхами (тести).
    pub fn startup_in(config: &Path, engines_dir: &Path) -> Result<Self> {
        if let Some(saved) = fs::read(config)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|config| Self::decode(&config["active"]))
            .filter(|e| e.path.is_file())
        {
            return Ok(saved);
        }
        let discovered = Self::discover(EngineKind::Bambu).or_else(|_| Self::discover(EngineKind::Orca));
        if discovered.is_ok() {
            return discovered;
        }
        let managed = Self::managed(engines_dir);
        [EngineKind::Bambu, EngineKind::Orca]
            .into_iter()
            .find_map(|kind| managed.iter().rev().find(|e| e.kind == kind).cloned())
            .map_or(discovered, Ok)
    }

    /// Запис `{"kind": "bambu"|"orca", "path": …}` з `engines.json`.
    pub fn decode(value: &serde_json::Value) -> Option<Self> {
        let kind = match value["kind"].as_str()? {
            "bambu" => EngineKind::Bambu,
            "orca" => EngineKind::Orca,
            _ => return None,
        };
        Some(Self {
            kind,
            path: value["path"].as_str()?.into(),
        })
    }

    /// Версії, встановлені менеджером Slicenv (`<engines_dir>/<family>/<версія>/manifest.json`),
    /// у порядку назв тек. Лише з маніфестів, без обходу дерева версії.
    pub fn managed(engines_dir: &Path) -> Vec<Self> {
        let mut out = Vec::new();
        for kind in [EngineKind::Bambu, EngineKind::Orca] {
            let Ok(entries) = fs::read_dir(engines_dir.join(kind.name())) else {
                continue;
            };
            let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
            entries.sort();
            for entry in entries {
                if entry
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'))
                {
                    continue;
                }
                let Ok(bytes) = fs::read(entry.join("manifest.json")) else {
                    continue;
                };
                let Ok(data) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                    continue;
                };
                if data["managed"] == true
                    && data["family"] == kind.name()
                    && let Some(exe) = data["executable_path"].as_str().map(PathBuf::from)
                    && exe.is_file()
                    && exe.starts_with(&entry)
                {
                    out.push(Self { kind, path: exe });
                }
            }
        }
        out
    }

    pub fn probe(&self) -> Result<EngineInfo> {
        self.probe_cancellable(&crate::SliceCancellation::default())
    }

    /// Версія й прапорці рушія. Результат `--help` кешується в процесі за шляхом, розміром і
    /// часом зміни файлу: застосунок питає рушій під час завантаження, а кожна нарізка інакше
    /// запускала б `--help` ще раз (Orca 2.4 через `AppRun` — 0,3–0,5 с стіни, 0,3 с CPU).
    /// Оновлений на місці рушій має інший розмір або час зміни — і пробується знову.
    pub fn probe_cancellable(&self, cancellation: &crate::SliceCancellation) -> Result<EngineInfo> {
        let binary = fs::canonicalize(&self.path)
            .with_context(|| format!("missing engine {}", self.path.display()))?;
        let key = ProbeKey::new(self.kind, &self.path, &binary);
        if let Some(info) = key.as_ref().and_then(|key| PROBES.lock().unwrap().get(key).cloned()) {
            return Ok(info);
        }
        let info = self.probe_uncached(&binary, cancellation)?;
        if let Some(key) = key {
            PROBES.lock().unwrap().insert(key, info.clone());
        }
        Ok(info)
    }

    fn probe_uncached(&self, binary: &Path, cancellation: &crate::SliceCancellation) -> Result<EngineInfo> {
        // Windows, OrcaSlicer: `--help` не викликаємо. З 2.4 `CLI::print_help` робить
        // `AttachConsole`/`AllocConsole` + `freopen("CONOUT$")`: довідка йде в консоль, а не в наші
        // дескриптори (порожній вивід → «did not identify»), а з GUI Slicenv без консолі Orca
        // відкриває нове консольне вікно. Рушій упізнаємо за `OrcaSlicer.dll` поруч з exe.
        #[cfg(windows)]
        if self.kind == EngineKind::Orca
            && let Some(dir) = binary.parent()
            && dir.join("OrcaSlicer.dll").is_file()
        {
            let facts = dll_facts(&dir.join("OrcaSlicer.dll"));
            let version = windows_uninstall_version(self)
                .or_else(|| facts.as_ref().and_then(|f| f.release.clone()))
                .map(|v| format!("OrcaSlicer-{v}"))
                .unwrap_or_else(|| "unknown".into());
            let flags = facts
                .map(|f| f.flags)
                .filter(|f| f.contains("--slice"))
                .unwrap_or_else(|| ORCA_CLI_FLAGS.iter().map(|f| (*f).to_owned()).collect());
            return Ok(self.info(version, flags));
        }
        let probe_dir = tempfile::tempdir()?;
        let output = bounded_output_cancellable(
            engine_command(binary)
                .arg("--help")
                .current_dir(probe_dir.path()),
            cancellation,
        )
        .with_context(|| {
            format!(
                "could not probe {} at {}",
                self.kind.name(),
                self.path.display()
            )
        })?;
        let text = String::from_utf8_lossy(&output.stdout).to_string()
            + &String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            bail!("{} --help failed ({})", self.kind.name(), output.status);
        }
        let identity_name = match self.kind {
            EngineKind::Bambu => "bambustudio",
            EngineKind::Orca => "orcaslicer",
        };
        let lower = text.to_ascii_lowercase();
        if !lower.contains(identity_name) {
            bail!(
                "{} --help did not identify as {}",
                self.kind.name(),
                match self.kind {
                    EngineKind::Bambu => "BambuStudio",
                    EngineKind::Orca => "OrcaSlicer",
                }
            );
        }
        let version = extract_version(&text, identity_name);
        let flags = text
            .split_whitespace()
            .filter_map(|word| {
                let token = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
                (token.starts_with("--") && token.len() > 2).then(|| token.to_owned())
            })
            .collect();
        Ok(self.info(version, flags))
    }

    pub(crate) fn info(&self, version: String, flags: BTreeSet<String>) -> EngineInfo {
        let canonical = fs::canonicalize(&self.path).unwrap_or_else(|_| self.path.clone());
        let metadata = fs::metadata(&self.path).ok();
        let identity = format!(
            "{}:{}:{}:{}:{:?}",
            self.kind.name(),
            canonical.display(),
            version,
            metadata.as_ref().map(|m| m.len()).unwrap_or(0),
            metadata.and_then(|m| m.modified().ok()),
        );
        let label = match (self.kind, release_version(self, &version)) {
            (EngineKind::Orca, Some(release)) => format!("OrcaSlicer {release}"),
            (EngineKind::Orca, None) if slic3r_style(version_number(&version)) => {
                format!("OrcaSlicer (SLIC3R {})", version_number(&version))
            }
            (EngineKind::Orca, None) if version == "unknown" => "OrcaSlicer".into(),
            _ => version.clone(),
        };
        EngineInfo {
            version,
            label,
            identity,
            flags,
        }
    }

    pub fn export_defaults(&self) -> Result<serde_json::Map<String, serde_json::Value>> {
        let info = self.probe()?;
        if !info.supports_flag("--export-settings") {
            bail!("{} does not support --export-settings", self.kind.name());
        }
        let binary = fs::canonicalize(&self.path)?;
        let temp = tempfile::tempdir()?;
        let output_path = temp.path().join("defaults.json");
        let output = bounded_output(
            engine_command(&binary)
                .arg("--export-settings")
                .arg(&output_path)
                .current_dir(temp.path()),
        )
        .with_context(|| format!("could not export {} defaults", self.kind.name()))?;
        if !output.status.success() {
            bail!(
                "{} --export-settings failed ({})",
                self.kind.name(),
                output.status
            );
        }
        let bytes = fs::read(&output_path)
            .with_context(|| format!("{} did not create defaults output", self.kind.name()))?;
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).context("invalid exported defaults JSON")?;
        value
            .as_object()
            .cloned()
            .context("exported defaults must be a JSON object")
    }
}

/// Ключ кешу [`Engine::probe_cancellable`]: шлях, як його задано, і справжній файл (для
/// `AppRun` — сам скрипт), з розміром і часом зміни обох.
#[derive(Clone, PartialEq, Eq, Hash)]
struct ProbeKey {
    kind: EngineKind,
    files: Vec<(PathBuf, u64, Option<std::time::SystemTime>)>,
}

impl ProbeKey {
    fn new(kind: EngineKind, path: &Path, binary: &Path) -> Option<Self> {
        let stat = |p: &Path| {
            let m = fs::metadata(p).ok()?;
            Some((p.to_owned(), m.len(), m.modified().ok()))
        };
        Some(Self {
            kind,
            files: vec![stat(path)?, stat(binary)?],
        })
    }
}

static PROBES: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<ProbeKey, EngineInfo>>> =
    std::sync::LazyLock::new(Default::default);

/// Команда запуску рушія (довідка, нарізка, експорт налаштувань).
///
/// `AppRun` OrcaSlicer під Wayland щоразу запускає `glxinfo`, щоб на NVIDIA з драйвером
/// новішим за 555 перемкнути GL на zink (0,1–0,2 с стіни на кожен запуск). Без пропрієтарного
/// драйвера NVIDIA (`/proc/driver/nvidia`) ця гілка нічого не змінює, тож її вимикаємо
/// тим самим прапорцем, який читає `AppRun`: `ZINK_DISABLE_OVERRIDE=1`. Значення
/// користувача не перекриваємо.
pub(crate) fn engine_command(binary: &Path) -> Command {
    let mut command = paths::host::command(binary);
    if cfg!(target_os = "linux")
        && env::var_os("ZINK_DISABLE_OVERRIDE").is_none()
        && !Path::new("/proc/driver/nvidia").exists()
    {
        command.env("ZINK_DISABLE_OVERRIDE", "1");
    }
    command
}

// File-backed capture avoids a full stdout pipe blocking a child before wait.
// Probes never keep the settings UI in a permanently loading state.
fn bounded_output(command: &mut Command) -> Result<Output> {
    bounded_output_cancellable(command, &crate::SliceCancellation::default())
}
fn bounded_output_cancellable(
    command: &mut Command,
    cancellation: &crate::SliceCancellation,
) -> Result<Output> {
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let status = cancellation.run(
        command
            .stdin(Stdio::null())
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?),
        Some(Duration::from_secs(15)),
    )?;
    stdout.rewind()?;
    stderr.rewind()?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    stdout.take(4 * 1024 * 1024).read_to_end(&mut out)?;
    stderr.take(4 * 1024 * 1024).read_to_end(&mut err)?;
    Ok(Output {
        status,
        stdout: out,
        stderr: err,
    })
}

/// Версія з `manifest.json` керованої установки (менеджер рушіїв), до двох рівнів вище exe.
pub(crate) fn manifest_version(engine: &Engine) -> Option<String> {
    let binary = fs::canonicalize(&engine.path).unwrap_or_else(|_| engine.path.clone());
    [
        engine.path.parent(),
        binary.parent(),
        binary.parent().and_then(Path::parent),
        // macOS: `<версія>/<App>.app/Contents/MacOS/<exe>`.
        binary.ancestors().nth(4),
    ]
    .into_iter()
    .flatten()
    .find_map(|dir| {
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join("manifest.json")).ok()?).ok()?;
        (value["family"].as_str() == Some(engine.kind.name()))
            .then(|| value["version"].as_str().map(str::to_owned))
            .flatten()
    })
}

/// «OrcaSlicer-01.10.01.50» → «01.10.01.50».
pub fn version_number(version: &str) -> &str {
    let lower = version.to_ascii_lowercase();
    ["orcaslicer", "bambustudio"]
        .iter()
        .find(|p| lower.starts_with(*p))
        .map_or(version, |p| version[p.len()..].trim_start_matches(['-', '_', ' ', 'v', 'V']))
}

/// `SLIC3R_VERSION` у стилі Bambu: чотири двоцифрові частини («01.10.01.50»).
fn slic3r_style(number: &str) -> bool {
    let parts: Vec<_> = number.split('.').collect();
    parts.len() == 4 && parts.iter().all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Справжня версія OrcaSlicer: `manifest.json` менеджера, на Windows — запис деінсталятора
/// (`DisplayVersion`, той самий каталог, що й exe), інакше — з `--help`, якщо там не
/// внутрішня `SLIC3R_VERSION` (Orca ≥ 2.4 друкує власну «OrcaSlicer-2.5.0-dev»).
/// Для Bambu Studio версія в `--help` і є версією продукту — `None`.
fn release_version(engine: &Engine, help_version: &str) -> Option<String> {
    if engine.kind != EngineKind::Orca {
        return None;
    }
    if let Some(version) = manifest_version(engine) {
        return Some(version.trim_start_matches(['v', 'V']).to_owned());
    }
    #[cfg(windows)]
    if let Some(version) = windows_uninstall_version(engine) {
        return Some(version);
    }
    let number = version_number(help_version);
    (number.contains('.') && !slic3r_style(number)).then(|| number.to_owned())
}

/// Прапорці CLI, на які спирається Slicenv (`PrintConfig.cpp`, `CLIActionsConfigDef`/
/// `CLIMiscConfigDef`). На Windows довідку Orca не прочитати (див. `probe_cancellable`), тож
/// наявність кожного перевіряється за ключем опції в `OrcaSlicer.dll` ([`dll_facts`]); якщо
/// DLL не прочитати — вважаємо, що є всі.
#[cfg_attr(not(windows), allow(dead_code))]
const ORCA_CLI_FLAGS: &[&str] = &[
    "--help",
    "--debug",
    "--load-settings",
    "--load-filaments",
    "--load-defaultfila",
    "--orient",
    "--arrange",
    "--slice",
    "--outputdir",
    "--export-3mf",
    "--allow-newer-file",
    "--export-settings",
    "--no-check",
    "--pipe",
];

/// Що видно в `OrcaSlicer.dll` без запуску рушія.
#[derive(Clone, Debug, Default)]
#[cfg_attr(not(windows), allow(dead_code))]
struct DllFacts {
    /// Рядок «OrcaSlicer 2.4.2\0» (назва для About/User-Agent; є в 2.3.0 і 2.4.2) — для
    /// портативної Orca без запису в реєстрі.
    release: Option<String>,
    /// Прапорці з [`ORCA_CLI_FLAGS`], чий ключ опції (`allow_newer_file`) є в DLL окремим
    /// рядком. Так старі збірки (1.9/2.0 без `--allow-newer-file`) не отримують невідомих
    /// прапорців без жодного списку версій.
    flags: BTreeSet<String>,
}

/// Кеш — за шляхом, розміром і часом зміни: DLL читається один раз (≈ 100 МБ).
#[cfg_attr(not(windows), allow(dead_code))]
fn dll_facts(dll: &Path) -> Option<DllFacts> {
    use std::{collections::HashMap, sync::Mutex};
    type Key = (PathBuf, u64, Option<std::time::SystemTime>);
    static CACHE: Mutex<Option<HashMap<Key, DllFacts>>> = Mutex::new(None);
    let meta = fs::metadata(dll).ok()?;
    let key = (dll.to_path_buf(), meta.len(), meta.modified().ok());
    if let Some(hit) = CACHE.lock().unwrap().get_or_insert_default().get(&key) {
        return Some(hit.clone());
    }
    let bytes = fs::read(dll).ok()?;
    let facts = DllFacts {
        release: release_in_binary(&bytes),
        flags: flags_in_binary(&bytes),
    };
    CACHE.lock().unwrap().get_or_insert_default().insert(key, facts.clone());
    Some(facts)
}

/// `--allow-newer-file` ↔ рядок `allow_newer_file\0`, перед яким теж `\0` (рядки в секції
/// даних розділені нулями). Один прохід по файлу.
fn flags_in_binary(bytes: &[u8]) -> BTreeSet<String> {
    let keys: std::collections::HashMap<Vec<u8>, &str> = ORCA_CLI_FLAGS
        .iter()
        .map(|flag| (flag[2..].replace('-', "_").into_bytes(), *flag))
        .collect();
    let mut found: BTreeSet<String> = bytes
        .split(|&b| b == 0)
        .filter(|s| s.len() <= 32)
        .filter_map(|s| keys.get(s).map(|f| (*f).to_owned()))
        .collect();
    // `--help` обробляє сам CLI, без ключа конфігурації.
    found.insert("--help".into());
    found
}

fn release_in_binary(bytes: &[u8]) -> Option<String> {
    const PREFIX: &[u8] = b"OrcaSlicer ";
    bytes
        .windows(PREFIX.len())
        .enumerate()
        .filter(|(_, w)| *w == PREFIX)
        .find_map(|(i, _)| {
            let rest = &bytes[i + PREFIX.len()..];
            let end = rest.iter().position(|&b| b == 0)?;
            let version = std::str::from_utf8(&rest[..end]).ok()?;
            let numeric = version.split('-').next()?;
            (numeric.split('.').count() == 3
                && numeric.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                && version.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-'))
            .then(|| version.to_owned())
        })
}

/// `reg query` ключа деінсталятора Orca (NSIS пише його у WOW6432Node, «для себе» — у HKCU).
#[cfg(windows)]
fn windows_uninstall_version(engine: &Engine) -> Option<String> {
    let dir = engine.path.parent()?;
    [
        r"HKLM\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\OrcaSlicer",
        r"HKLM\Software\Microsoft\Windows\CurrentVersion\Uninstall\OrcaSlicer",
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\OrcaSlicer",
    ]
    .into_iter()
    .find_map(|key| {
        let output = bounded_output(paths::host::command("reg").args(["query", key])).ok()?;
        output.status.success().then_some(())?;
        uninstall_version(&String::from_utf8_lossy(&output.stdout), dir)
    })
}

/// Розбір виводу `reg query`: `DisplayVersion`, якщо `UninstallString` лежить у `dir`.
#[cfg_attr(not(windows), allow(dead_code))]
fn uninstall_version(reg_output: &str, dir: &Path) -> Option<String> {
    let value = |name: &str| {
        reg_output.lines().find_map(|line| {
            let mut parts = line.trim().splitn(3, "    ");
            (parts.next()? == name).then(|| parts.nth(1).map(|v| v.trim().to_owned()))?
        })
    };
    // Порівняння рядком: формат шляхів Windows однаковий і в тестах на Unix.
    let uninstaller = value("UninstallString")?;
    let uninstaller = uninstaller.trim_matches('"');
    let parent = &uninstaller[..uninstaller.rfind(['\\', '/'])?];
    let dir = dir.to_string_lossy();
    let same_dir = parent.trim_end_matches(['\\', '/']).eq_ignore_ascii_case(dir.trim_end_matches(['\\', '/']));
    same_dir.then(|| value("DisplayVersion")).flatten().filter(|v| !v.is_empty())
}

fn extract_version(text: &str, identity: &str) -> String {
    for line in text.lines() {
        let line_lower = line.to_ascii_lowercase();
        if (line_lower.contains(identity) || line_lower.contains("version"))
            && let Some(version) = line
                .split(|c: char| !(c.is_ascii_alphanumeric() || ".-_+".contains(c)))
                .find(|part| part.chars().any(|c| c.is_ascii_digit()) && part.contains('.'))
        {
            return version.to_owned();
        }
    }
    "unknown".to_owned()
}

#[cfg(test)]
mod version_tests {
    use super::*;

    #[test]
    fn orca_internal_slic3r_version_is_not_shown_as_release() {
        assert_eq!(version_number("OrcaSlicer-01.10.01.50"), "01.10.01.50");
        assert!(slic3r_style("01.10.01.50"));
        assert!(!slic3r_style("2.5.0-dev"));
        let engine = Engine {
            kind: EngineKind::Orca,
            path: PathBuf::from("/nonexistent/orca-slicer"),
        };
        assert_eq!(release_version(&engine, "OrcaSlicer-01.10.01.50"), None);
        assert_eq!(
            release_version(&engine, "OrcaSlicer-2.5.0-dev").as_deref(),
            Some("2.5.0-dev")
        );
        let bambu = Engine {
            kind: EngineKind::Bambu,
            ..engine
        };
        assert_eq!(release_version(&bambu, "BambuStudio-02.03.00.70"), None);
    }

    #[test]
    fn managed_manifest_gives_the_release() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("bin").join("orca-slicer");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, "").unwrap();
        fs::write(
            dir.path().join("manifest.json"),
            r#"{"family":"orca","version":"v2.3.0","managed":true}"#,
        )
        .unwrap();
        let engine = Engine {
            kind: EngineKind::Orca,
            path: exe,
        };
        assert_eq!(release_version(&engine, "OrcaSlicer-01.10.01.50").as_deref(), Some("2.3.0"));
    }

    #[test]
    fn release_is_found_in_orca_dll_strings() {
        let dll = b"\0x OrcaSlicer was\0OrcaSlicer 2.4.0-alpha\x01\0OrcaSlicer 2.4.2\0OrcaSlicer 9.9.9\0";
        assert_eq!(release_in_binary(dll).as_deref(), Some("2.4.2"));
        assert_eq!(release_in_binary(b"OrcaSlicer 2.3.0\0").as_deref(), Some("2.3.0"));
        assert_eq!(release_in_binary(b"OrcaSlicer 01.10.01.50\0"), None);
        assert_eq!(release_in_binary(b"OrcaSlicer 2.3.0"), None);
    }

    #[test]
    fn cli_flags_are_read_from_option_keys_in_the_dll() {
        // Як OrcaSlicer 1.9.1/2.0.0: ключа allow_newer_file немає (перевірено на справжніх DLL).
        let old = b"\0slice\0outputdir\0load_settings\0load_filaments\0export_3mf\0allow_newer\0";
        let flags = flags_in_binary(old);
        assert!(flags.contains("--export-3mf") && flags.contains("--slice"));
        assert!(!flags.contains("--allow-newer-file"));
        let new = b"\0slice\0allow_newer_file\0Allow 3mf with newer version\0";
        assert!(flags_in_binary(new).contains("--allow-newer-file"));
    }

    #[test]
    fn windows_uninstall_entry_is_matched_by_install_directory() {
        // Вивід `reg query` з Windows-машини (OrcaSlicer 2.3.0, інсталятор NSIS).
        let output = concat!(
            "\r\nHKEY_LOCAL_MACHINE\\Software\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\OrcaSlicer\r\n",
            "    DisplayName    REG_SZ    OrcaSlicer\r\n",
            "    DisplayVersion    REG_SZ    2.3.0\r\n",
            "    UninstallString    REG_SZ    \"C:\\Program Files\\OrcaSlicer\\Uninstall.exe\"\r\n",
        );
        let found = |dir: &str| uninstall_version(output, Path::new(dir));
        assert_eq!(found(r"C:\Program Files\OrcaSlicer").as_deref(), Some("2.3.0"));
        assert_eq!(found(r"c:\program files\orcaslicer\").as_deref(), Some("2.3.0"));
        // Інша копія Orca (портативна) не отримує версію чужого інсталятора.
        assert_eq!(found(r"D:\Tools\OrcaSlicer"), None);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn startup_prefers_active_engine_and_lists_managed_versions() {
        let dir = tempfile::tempdir().unwrap();
        let engines = dir.path().join("engines");
        let mut exes = Vec::new();
        for (family, version) in [("orca", "2.4.2"), ("orca", "2.5.0"), ("bambu", ".partial")] {
            let root = engines.join(family).join(version);
            fs::create_dir_all(&root).unwrap();
            let exe = root.join("AppRun");
            fs::write(&exe, "").unwrap();
            fs::write(
                root.join("manifest.json"),
                serde_json::json!({"managed": true, "family": family, "version": version,
                                   "executable_path": exe})
                .to_string(),
            )
            .unwrap();
            exes.push(exe);
        }
        // Тека з крапкою — незавершене встановлення.
        let managed = Engine::managed(&engines);
        assert_eq!(
            managed.iter().map(|e| e.path.clone()).collect::<Vec<_>>(),
            exes[..2].to_vec()
        );
        assert!(managed.iter().all(|e| e.kind == EngineKind::Orca));

        let config = dir.path().join("engines.json");
        fs::write(
            &config,
            serde_json::json!({"active": {"kind": "orca", "path": exes[0]}}).to_string(),
        )
        .unwrap();
        let active = Engine::startup_in(&config, &engines).unwrap();
        assert_eq!((active.kind, active.path), (EngineKind::Orca, exes[0].clone()));
        // Активний рушій зник — не береться; далі PATH/системні або керовані.
        fs::write(
            &config,
            serde_json::json!({"active": {"kind": "orca", "path": dir.path().join("gone")}}).to_string(),
        )
        .unwrap();
        let fallback = Engine::startup_in(&config, &engines).unwrap();
        assert_ne!(fallback.path, dir.path().join("gone"));
    }

    #[test]
    fn installed_candidates_follow_os_layout() {
        let bambu = EngineKind::Bambu.installed();
        #[cfg(target_os = "macos")]
        assert!(bambu.iter().any(|p| p.ends_with("BambuStudio.app/Contents/MacOS/BambuStudio")));
        #[cfg(not(target_os = "macos"))]
        assert!(bambu.contains(&PathBuf::from("/opt/bambu-studio/bambu-studio")));
    }

    fn mock(contents: &str) -> (tempfile::TempDir, Engine) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mock-engine");
        fs::write(&path, contents).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        (
            dir,
            Engine {
                kind: EngineKind::Orca,
                path,
            },
        )
    }

    #[test]
    fn probe_skips_leading_log_and_extracts_orca_version_and_flags() {
        let (_dir, engine) = mock(
            "#!/bin/sh\necho 'info: startup complete' >&2\necho 'OrcaSlicer version 2.3.1'\necho '  --export-settings FILE'\n",
        );
        let info = engine.probe().unwrap();
        assert_eq!(info.version, "2.3.1");
        assert!(info.supports_flag("--export-settings"));
    }

    #[test]
    fn probe_is_cached_until_the_engine_file_changes() {
        let (dir, engine) = mock("");
        let count = dir.path().join("runs");
        let script = |version: &str| {
            format!(
                "#!/bin/sh\necho x >> '{}'\necho 'OrcaSlicer version {version}'\necho '  --slice'\n",
                count.display()
            )
        };
        let runs = || fs::read_to_string(&count).unwrap_or_default().lines().count();
        fs::write(&engine.path, script("2.4.2")).unwrap();
        assert_eq!(engine.probe().unwrap().version, "2.4.2");
        assert_eq!(engine.probe().unwrap().version, "2.4.2");
        assert_eq!(runs(), 1, "другий probe — з кешу");
        // Оновлення на місці: інший розмір файлу — новий запуск `--help`.
        fs::write(&engine.path, script("2.5.0-beta")).unwrap();
        assert_eq!(engine.probe().unwrap().version, "2.5.0-beta");
        assert_eq!(runs(), 2);
    }

    #[test]
    fn probe_rejects_failed_help_even_with_text_and_wrong_family() {
        let (_dir, failed) = mock("#!/bin/sh\necho 'OrcaSlicer version 2.3.1'\nexit 2\n");
        assert!(failed.probe().is_err());
        let (_dir, wrong) = mock("#!/bin/sh\necho 'BambuStudio version 2.3.1'\n");
        assert!(wrong.probe().is_err());
    }
}
