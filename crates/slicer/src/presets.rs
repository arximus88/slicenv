//! Профілі Bambu Studio з `~/.config/BambuStudio`.
//!
//! Системні профілі зберігаються «дельтами»: `inherits` вказує на батька, `include` —
//! на шаблони (у т. ч. справжній стартовий G-code A1). Консольний Bambu Studio сам цей
//! ланцюжок не розгортає, тож ми зливаємо його в один повний JSON.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Machine,
    Process,
    Filament,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Machine, Kind::Process, Kind::Filament];

    pub fn dir(self) -> &'static str {
        match self {
            Kind::Machine => "machine",
            Kind::Process => "process",
            Kind::Filament => "filament",
        }
    }

    fn settings_key(self) -> &'static str {
        match self {
            Kind::Machine => "printer_settings_id",
            Kind::Process => "print_settings_id",
            Kind::Filament => "filament_settings_id",
        }
    }
}

fn validate_user_name(name: &str) -> Result<()> {
    if name.trim().is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', '\0', '\n', '\r'])
    {
        bail!("invalid preset name {name:?}");
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct PresetInfo {
    pub name: String,
    path: PathBuf,
    /// `false` — службовий батьківський профіль або шаблон, у списках не показується.
    pub instantiable: bool,
    pub compatible_printers: Vec<String>,
    inherits: Option<String>,
    includes: Vec<String>,
    /// Вендор системного профілю (тека в каталозі), `None` — користувацький.
    vendor: Option<String>,
    /// Збережений цією програмою (тека [`PresetStore::own_root`]).
    pub own: bool,
}

/// Профілі, вибрані в слайсері (Bambu Studio) або збережені в проєкті.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub machine: String,
    pub process: String,
    pub filament: String,
    pub bed_type: String,
}

/// Common upstream plate labels, retained for callers that need an initial UI value.
/// The selected value is passed through unchanged because plate names vary by printer.
pub const BED_TYPES: &[&str] = &[
    "Textured PEI Plate",
    "High Temp Plate",
    "Cool Plate",
    "Engineering Plate",
    "Supertack Plate",
];

/// Files describing the printer's bed in an installed engine (see [`PresetStore::bed_resources`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BedResources {
    pub vendor: String,
    pub model: Option<PathBuf>,
    pub texture: Option<PathBuf>,
    /// `resources/images` with `bbl_bed_*` plate-type SVGs.
    pub images: Option<PathBuf>,
}

#[derive(Clone)]
pub struct PresetStore {
    root: PathBuf,
    own_root: PathBuf,
    presets: HashMap<(Kind, String), PresetInfo>,
    /// Системні профілі кожного вендора окремо. Службові батьки (`fdm_machine_common`,
    /// `fdm_process_common`, `fdm_filament_pla`…) мають однакові імена в усіх вендорів, а
    /// значення різні (Anker: абсолютний E, 3 стінки, cubic). Рушій шукає батька в межах
    /// вендора, тож і ми: інакше чужий батько тихо підміняє ключі.
    vendor_presets: HashMap<(String, Kind, String), PresetInfo>,
    /// Системні профілі так, як їх зводив Slicenv до виправлення: одна мапа за іменем, де
    /// спільне ім’я займав останній вендор у порядку `read_dir`. Лише щоб упізнати в старих
    /// проєктах значення, які туди потрапили від чужого батька ([`Self::stale_values`]).
    legacy_presets: HashMap<(Kind, String), PresetInfo>,
    engine_kind: Option<crate::EngineKind>,
}

/// Recoverable activation error: the UI can fetch JSON from the matching tag.
#[derive(Debug)]
pub struct ProfileCatalogMissing {
    pub family: crate::EngineKind,
    pub version: String,
    pub opc_only: bool,
}

impl std::fmt::Display for ProfileCatalogMissing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self.family {
            crate::EngineKind::Orca => "OrcaSlicer",
            crate::EngineKind::Bambu => "Bambu Studio",
        };
        write!(f, "{name} {} постачається без профілів принтерів", self.version)?;
        if self.opc_only {
            // Технічна деталь — для журналу.
            write!(f, " (лише бінарний кеш .opc)")?;
        }
        Ok(())
    }
}
impl std::error::Error for ProfileCatalogMissing {}

fn resolve_catalog(
    override_root: Option<PathBuf>,
    packages: &[PathBuf],
    library: &Path,
    family: &str,
    version: &str,
    fallbacks: &[PathBuf],
) -> Option<PathBuf> {
    override_root
        .or_else(|| packages.iter().find(|p| valid_json_tree(p, "BBL")).cloned())
        .or_else(|| library_catalog(library, family, version))
        .or_else(|| {
            fallbacks
                .iter()
                .find(|p| valid_json_tree(p, "BBL"))
                .cloned()
        })
}

/// A cover-image directory or private vendor cache is not a JSON catalog.
fn valid_json_tree(root: &Path, vendor: &str) -> bool {
    [root.to_owned(), root.join("system")].iter().any(|p| {
        p.join(format!("{vendor}.json")).is_file()
            && Kind::ALL
                .iter()
                .all(|kind| p.join(vendor).join(kind.dir()).is_dir())
    })
}

// Numeric components, then prerelease ordering (stable > prerelease).
fn version_key(version: &str) -> (Vec<u64>, bool, String) {
    let version = version.trim_start_matches(['v', 'V']);
    let (numbers, suffix) = version.split_once('-').unwrap_or((version, ""));
    (
        numbers.split('.').map(|n| n.parse().unwrap_or(0)).collect(),
        suffix.is_empty(),
        suffix.to_owned(),
    )
}

fn library_catalog(root: &Path, family: &str, version: &str) -> Option<PathBuf> {
    let mut catalogs: Vec<_> = fs::read_dir(root.join(family))
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if entry.file_name().to_string_lossy().starts_with('.')
                || !valid_json_tree(&path, "BBL")
            {
                return None;
            }
            let manifest: Value =
                serde_json::from_slice(&fs::read(path.join("catalog.json")).ok()?).ok()?;
            if manifest["family"].as_str()? != family
                || !manifest["vendors"].as_array()?.iter().any(|v| v == "BBL")
            {
                return None;
            }
            Some((manifest["version"].as_str()?.to_owned(), path))
        })
        .collect();
    catalogs.sort_by_key(|(version, _)| version_key(version));
    catalogs
        .iter()
        .find(|(v, _)| v == version)
        .or_else(|| {
            catalogs
                .iter()
                .rev()
                .find(|(v, _)| version_key(v) <= version_key(version))
        })
        .or_else(|| catalogs.last())
        .map(|(_, p)| p.clone())
}

impl PresetStore {
    pub fn engine_kind(&self) -> Option<crate::EngineKind> {
        self.engine_kind
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn user_root(&self) -> &Path {
        &self.own_root
    }

    /// Engine catalogs remain separate; never inherit a same-named template from
    /// another slicer. A packaged resource tree works without starting its GUI.
    pub fn open_for_engine(engine: &crate::Engine) -> Result<Self> {
        let kind = engine.kind;
        let variable = match kind {
            crate::EngineKind::Bambu => "SLICENV_PROFILE_ROOT",
            crate::EngineKind::Orca => "ORCA_PROFILE_ROOT",
        };
        let own = match kind {
            crate::EngineKind::Bambu => Self::own_root(),
            crate::EngineKind::Orca => Self::own_root().join("orca"),
        };
        if let Some(root) = paths::env(variable) {
            let mut store = Self::open_with_own_root(Path::new(&root), &own)?;
            store.engine_kind = Some(kind);
            return Ok(store);
        }
        let binary = fs::canonicalize(&engine.path).unwrap_or_else(|_| engine.path.clone());
        let mut candidates = Vec::new();
        if let Some(dir) = binary.parent() {
            candidates.push(dir.join("resources/profiles"));
            if let Some(parent) = dir.parent() {
                candidates.push(parent.join("resources/profiles"));
                // macOS: `<App>.app/Contents/MacOS/<exe>` → `Contents/Resources/profiles`
                // (на чутливій до регістру APFS `resources` не збігається).
                if cfg!(target_os = "macos") {
                    candidates.push(parent.join("Resources/profiles"));
                }
            }
        }
        let package_candidates = candidates.clone();
        let version = Self::engine_version(engine);
        let config = Self::default_root().parent().unwrap().to_owned();
        match kind {
            crate::EngineKind::Bambu => {
                candidates.extend(
                    [
                        "/usr/share/BambuStudio/profiles",
                        "/usr/share/bambustudio/profiles",
                    ]
                    .map(PathBuf::from),
                );
                if ["/usr/bin/bambustudio", "/usr/bin/bambu-studio"]
                    .iter()
                    .any(|p| fs::canonicalize(p).ok().as_ref() == Some(&binary))
                {
                    candidates
                        .extend(["BambuStudio", "BambuStudioBeta"].map(|app| config.join(app)));
                }
            }
            crate::EngineKind::Orca => {
                // Custom versions must carry resources or use ORCA_PROFILE_ROOT;
                // a system catalog from a different installation is not a fallback.
                if fs::canonicalize("/usr/bin/orca-slicer").ok().as_ref() == Some(&binary) {
                    candidates.push(config.join("OrcaSlicer"));
                }
            }
        }
        let root = resolve_catalog(
            None,
            &package_candidates,
            &Self::library_root(),
            kind.name(),
            &version,
            &candidates[package_candidates.len()..],
        )
        .ok_or_else(|| ProfileCatalogMissing {
            family: kind,
            version,
            opc_only: package_candidates
                .iter()
                .any(|p| p.join("BBL.opc").is_file()),
        })?;
        let mut store = Self::open_with_own_root(&root, &own)?;
        store.engine_kind = Some(kind);
        Ok(store)
    }

    pub fn library_root() -> PathBuf {
        Self::own_root().parent().unwrap().join("profiles")
    }

    /// Номер версії без назви продукту («OrcaSlicer-2.5.0» → «2.5.0»): ним названо теку
    /// каталогу профілів і git-тег `v<версія>`.
    pub fn engine_version(engine: &crate::Engine) -> String {
        let version = crate::engine::manifest_version(engine)
            .or_else(|| engine.probe().ok().map(|info| info.version))
            .unwrap_or_default();
        crate::engine::version_number(&version).to_owned()
    }

    pub fn reload(&self) -> Result<Self> {
        let mut store = Self::open_with_own_root(&self.root, &self.own_root)?;
        store.engine_kind = self.engine_kind;
        Ok(store)
    }

    /// `data_dir` Bambu Studio на цій ОС (див. [`paths::vendor_config_dir`]).
    pub fn default_root() -> PathBuf {
        paths::vendor_config_dir("BambuStudio")
    }

    pub fn open_default() -> Result<Self> {
        let config = paths::vendor_config_home();
        // Prefer Bambu Studio's catalog and selection, with installed beta/Orca fallback.
        // Profile fallback does not imply support for another slicer's engine options.
        let primary = ["BambuStudio", "BambuStudioBeta", "OrcaSlicer"]
            .iter()
            .map(|app| config.join(app))
            .find(|root| root.join("system/BBL").is_dir())
            .unwrap_or_else(Self::default_root);
        // Keep one coherent upstream tree: merging same-named base templates from
        // different slicers can silently change a child printer's start G-code.
        // An explicit root is useful when validating a new pinned upstream catalog.
        let primary = paths::env("SLICENV_PROFILE_ROOT")
            .map(PathBuf::from)
            .unwrap_or(primary);
        let own = Self::own_root();
        let mut store = Self::open_with_own_root(&primary, &own)?;
        store.resolve_compatibility();
        Ok(store)
    }

    /// Версія системних профілів (`system/BBL.json`) — нею позначаються збережені дельти.
    pub fn profile_version(&self) -> Option<String> {
        read_json(&if self.root.join("system/BBL.json").is_file() {
            self.root.join("system/BBL.json")
        } else {
            self.root.join("BBL.json")
        })
        .ok()?
        .get("version")?
        .as_str()
        .map(str::to_owned)
    }

    /// Власні пресети програми: конфіг Bambu Studio лишається тільки для читання.
    pub fn own_root() -> PathBuf {
        paths::data_dir().join("presets")
    }

    /// Користувацькі дельти не мають `compatible_printers` — успадковуємо від батьків.
    fn resolve_compatibility(&mut self) {
        let missing: Vec<(Kind, String)> = self
            .presets
            .iter()
            .filter(|(_, p)| p.compatible_printers.is_empty() && p.inherits.is_some())
            .map(|(k, _)| k.clone())
            .collect();
        for (kind, name) in missing {
            let mut current = self.presets[&(kind, name.clone())].clone();
            for _ in 0..32 {
                let Some(info) = current
                    .inherits
                    .as_deref()
                    .and_then(|parent| self.parent_of(kind, &current, parent))
                else {
                    break;
                };
                if !info.compatible_printers.is_empty() {
                    let list = info.compatible_printers.clone();
                    self.presets
                        .get_mut(&(kind, name.clone()))
                        .unwrap()
                        .compatible_printers = list;
                    break;
                }
                current = info.clone();
            }
        }
    }

    pub fn open(root: &Path) -> Result<Self> {
        Self::open_with_own_root(root, &Self::own_root())
    }

    /// Open an upstream catalog and an application-owned preset directory. Mutations
    /// are restricted to `own_root`; this is public to support isolated app profiles.
    pub fn open_with_own_root(root: &Path, own_root: &Path) -> Result<Self> {
        let system = if root.join("system/BBL").is_dir() {
            root.join("system/BBL")
        } else {
            root.join("BBL")
        };
        if !system.is_dir() {
            bail!("немає каталогу профілів BBL у {}", system.display());
        }
        let mut store = Self {
            root: root.to_owned(),
            own_root: own_root.to_owned(),
            presets: HashMap::new(),
            vendor_presets: HashMap::new(),
            legacy_presets: HashMap::new(),
            engine_kind: None,
        };
        store.scan_root(root);
        for kind in Kind::ALL {
            scan(&own_root.join(kind.dir()), kind, None, &mut store.presets);
        }
        for info in store.presets.values_mut() {
            info.own = info.path.starts_with(own_root);
        }
        store.resolve_compatibility();
        Ok(store)
    }

    /// Rename one application-owned preset while retaining the save-as JSON and
    /// `.info` sidecar format. Profiles with descendants cannot be renamed.
    pub fn rename_user(&mut self, kind: Kind, old: &str, new: &str) -> Result<()> {
        validate_user_name(new)?;
        let info = self.get(kind, old).context("preset not found")?;
        if !info.own {
            bail!("only application-owned presets can be renamed");
        }
        if old == new {
            bail!("new preset name must differ");
        }
        if self.presets.contains_key(&(kind, new.to_owned())) {
            bail!("preset name already exists");
        }
        if self.has_descendants(kind, old) {
            bail!("preset has descendants");
        }
        let source = info.path.clone();
        if !source.starts_with(&self.own_root) {
            bail!("preset is outside the application preset directory");
        }
        let directory = source.parent().context("preset has no parent directory")?;
        let source_info = source.with_extension("info");
        if source_info.exists() && !source_info.is_file() {
            bail!("preset sidecar is not a regular file");
        }
        let dest = directory.join(format!("{new}.json"));
        let dest_info = directory.join(format!("{new}.info"));
        if dest.exists() || dest_info.exists() {
            bail!("preset destination already exists");
        }
        let mut json = read_json(&source)?;
        json.insert("name".into(), Value::String(new.into()));
        for key in [kind.settings_key()] {
            if json.get(key).and_then(Value::as_str) == Some(old) {
                json.insert(key.into(), Value::String(new.into()));
            } else if let Some(values) = json.get_mut(key).and_then(Value::as_array_mut) {
                for value in values {
                    if value.as_str() == Some(old) {
                        *value = Value::String(new.into());
                    }
                }
            }
        }
        let staged = tempfile::Builder::new()
            .prefix(".preset-rename-")
            .tempdir_in(directory)?;
        let tmp = staged.path().join("new.json");
        let tmp_info = staged.path().join("new.info");
        fs::write(&tmp, serde_json::to_vec_pretty(&json)?)?;
        if source_info.exists() {
            fs::copy(&source_info, &tmp_info)?;
        }
        let backup = staged.path().join("old.json");
        let backup_info = staged.path().join("old.info");
        // hard_link creates without replacement; a racing destination is never overwritten.
        fs::hard_link(&tmp, &dest)?;
        if tmp_info.exists()
            && let Err(err) = fs::hard_link(&tmp_info, &dest_info)
        {
            let _ = fs::remove_file(&dest);
            return Err(err.into());
        }
        let result = (|| -> Result<()> {
            fs::rename(&source, &backup)?;
            if source_info.exists() {
                fs::rename(&source_info, &backup_info)?;
            }
            Ok(())
        })();
        if let Err(err) = result {
            if backup.exists() {
                let _ = fs::rename(&backup, &source);
            }
            if backup_info.exists() {
                let _ = fs::rename(&backup_info, &source_info);
            }
            let _ = fs::remove_file(&dest);
            if tmp_info.exists() {
                let _ = fs::remove_file(&dest_info);
            }
            return Err(err);
        }
        drop(staged);
        self.presets.remove(&(kind, old.to_owned()));
        scan(directory, kind, None, &mut self.presets);
        for preset in self.presets.values_mut() {
            preset.own = preset.path.starts_with(&self.own_root);
        }
        self.resolve_compatibility();
        Ok(())
    }

    /// Delete one application-owned preset and its optional `.info` sidecar.
    pub fn delete_user(&mut self, kind: Kind, name: &str) -> Result<()> {
        let info = self.get(kind, name).context("preset not found")?;
        if !info.own {
            bail!("only application-owned presets can be deleted");
        }
        if !info.path.starts_with(&self.own_root) {
            bail!("preset is outside the application preset directory");
        }
        if self.has_descendants(kind, name) {
            bail!("preset has descendants");
        }
        let path = info.path.clone();
        let sidecar = path.with_extension("info");
        if sidecar.exists() && !sidecar.is_file() {
            bail!("preset sidecar is not a regular file");
        }
        let directory = path.parent().context("preset has no parent directory")?;
        let staged = tempfile::Builder::new()
            .prefix(".preset-delete-")
            .tempdir_in(directory)?;
        let backup = staged.path().join("preset.json");
        let backup_sidecar = staged.path().join("preset.info");
        fs::rename(&path, &backup)?;
        if sidecar.exists()
            && let Err(err) = fs::rename(&sidecar, &backup_sidecar)
        {
            let _ = fs::rename(&backup, &path);
            return Err(err.into());
        }
        self.presets.remove(&(kind, name.to_owned()));
        Ok(())
    }

    fn has_descendants(&self, kind: Kind, name: &str) -> bool {
        self.presets.iter().any(|((k, _), p)| {
            *k == kind
                && (p.inherits.as_deref() == Some(name) || p.includes.iter().any(|n| n == name))
        })
    }

    fn scan_root(&mut self, root: &Path) {
        let system = if root.join("system/BBL").is_dir() {
            root.join("system/BBL")
        } else {
            root.join("BBL")
        };
        let base = system.parent().unwrap();
        let listed: Vec<String> = fs::read_dir(base)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|v| v.path().is_dir() && base.join(v.file_name()).with_extension("json").is_file())
            .filter_map(|v| v.file_name().into_string().ok())
            .collect();
        // BBL, потім бібліотека філаментів Orca, далі решта за абеткою: порядок визначає,
        // чий профіль займе спільне ім’я в загальному списку (раніше — порядок `read_dir`).
        let mut vendors: Vec<String> = listed.iter().filter(|v| *v != "BBL").cloned().collect();
        vendors.sort_by_key(|v| (v != LIBRARY_VENDOR, v.clone()));
        vendors.insert(0, "BBL".into());
        for vendor in &vendors {
            for kind in Kind::ALL {
                let mut own = HashMap::new();
                scan(&base.join(vendor).join(kind.dir()), kind, Some(vendor), &mut own);
                for ((kind, name), info) in own {
                    self.presets
                        .entry((kind, name.clone()))
                        .or_insert_with(|| info.clone());
                    self.vendor_presets.insert((vendor.clone(), kind, name), info);
                }
            }
        }
        // Старий порядок: BBL, далі вендори як віддав `read_dir`, кожен перезаписує попередніх.
        for vendor in std::iter::once("BBL").chain(listed.iter().map(String::as_str).filter(|v| *v != "BBL")) {
            for (key, info) in &self.vendor_presets {
                if key.0 == vendor {
                    self.legacy_presets.insert((key.1, key.2.clone()), info.clone());
                }
            }
        }
        // User presets override system presets with the same name.
        if let Ok(users) = fs::read_dir(root.join("user")) {
            for user in users.flatten() {
                for kind in Kind::ALL {
                    scan(&user.path().join(kind.dir()), kind, None, &mut self.presets);
                }
            }
        }
    }

    /// Батько `child`, як його знаходить рушій: системний профіль — у своєму вендорі, далі в
    /// бібліотеці філаментів Orca; користувацький — серед усіх профілів.
    fn parent_of(&self, kind: Kind, child: &PresetInfo, parent: &str) -> Option<&PresetInfo> {
        if let Some(vendor) = &child.vendor {
            for vendor in [vendor.as_str(), LIBRARY_VENDOR] {
                if let Some(info) =
                    self.vendor_presets.get(&(vendor.to_owned(), kind, parent.to_owned()))
                {
                    return Some(info);
                }
            }
        }
        self.get(kind, parent)
    }

    /// Vendor of the system ancestor, also for an application-owned machine.
    pub fn vendor_for_machine(&self, machine: &str) -> Option<String> {
        let mut info = self.get(Kind::Machine, machine)?;
        for _ in 0..32 {
            if let Some(vendor) = &info.vendor {
                return Some(vendor.clone());
            }
            if !info.own {
                let relative = info.path.strip_prefix(&self.root).ok()?;
                let mut parts = relative.components();
                let first = parts.next()?.as_os_str().to_str()?;
                let vendor = if first == "system" {
                    parts.next()?.as_os_str().to_str()?
                } else {
                    first
                };
                if vendor != "user" {
                    return Some(vendor.to_owned());
                }
            }
            info = self.parent_of(Kind::Machine, info, info.inherits.as_deref()?)?;
        }
        None
    }

    pub fn get(&self, kind: Kind, name: &str) -> Option<&PresetInfo> {
        self.presets.get(&(kind, name.to_owned()))
    }

    /// Профілі, які можна вибрати для цього принтера, за абеткою.
    pub fn compatible(&self, kind: Kind, machine: &str) -> Vec<&PresetInfo> {
        // Власний принтер сумісний з усім, з чим сумісні його системні предки.
        let mut machines = vec![machine.to_owned()];
        let mut current = self.get(Kind::Machine, machine);
        while let Some(info) = current {
            let Some(parent) = info.inherits.as_deref() else { break };
            if machines.len() > 32 || machines.iter().any(|m| m == parent) {
                break;
            }
            machines.push(parent.to_owned());
            current = self.parent_of(Kind::Machine, info, parent);
        }
        let mut list: Vec<_> = self
            .presets
            .iter()
            .filter(|((k, _), p)| {
                *k == kind
                    && p.instantiable
                    && (kind == Kind::Machine
                        || p.compatible_printers.iter().any(|m| machines.contains(m)))
            })
            .map(|(_, p)| p)
            .collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        list
    }

    /// Upstream defaults associated with a printer preset, if installed and selectable.
    pub fn defaults_for_machine(&self, machine: &str) -> Option<Selection> {
        let preset = self.get(Kind::Machine, machine)?;
        let config = self.flatten(Kind::Machine, machine).ok()?;
        let process = config.get("default_print_profile")?.as_str()?.to_owned();
        let filament = config.get("default_filament_profile")?;
        let filament = filament.as_str().map(str::to_owned).or_else(|| {
            filament
                .as_array()?
                .iter()
                .find_map(Value::as_str)
                .map(str::to_owned)
        })?;
        let selection = Selection {
            machine: preset.name.clone(),
            process,
            filament,
            bed_type: config
                .get("default_bed_type")
                .and_then(Value::as_str)
                .unwrap_or(BED_TYPES[0])
                .into(),
        };
        let selection = self.sanitize(selection);
        (!selection.process.is_empty() && !selection.filament.is_empty()).then_some(selection)
    }

    /// Bambu device code declared by the upstream machine model (including user children).
    pub fn machine_model_id(&self, machine: &str) -> Option<String> {
        let config = self.flatten(Kind::Machine, machine).ok()?;
        let model = config.get("printer_model")?.as_str()?;
        if model.contains(['/', '\\']) || model == "." || model == ".." {
            return None;
        }
        let base = if self.root.join("system/BBL").is_dir() {
            self.root.join("system")
        } else {
            self.root.clone()
        };
        let vendor = self.vendor_for_machine(machine)?;
        let json = read_json(
            &base
                .join(vendor)
                .join("machine")
                .join(format!("{model}.json")),
        )
        .ok()?;
        json.get("model_id")?.as_str().map(str::to_owned)
    }

    /// Bed visuals declared by the upstream machine model (`bed_model`, `bed_texture`)
    /// and the engine's `images` directory with plate-type SVGs. Paths are only
    /// returned when the files exist; nothing is copied.
    pub fn bed_resources(&self, machine: &str) -> Option<BedResources> {
        let config = self.flatten(Kind::Machine, machine).ok()?;
        let model = config.get("printer_model")?.as_str()?;
        if model.contains(['/', '\\']) || model == "." || model == ".." {
            return None;
        }
        let base = if self.root.join("system/BBL").is_dir() {
            self.root.join("system")
        } else {
            self.root.clone()
        };
        let vendor = self.vendor_for_machine(machine)?;
        let dir = base.join(&vendor);
        let json = read_json(&dir.join("machine").join(format!("{model}.json"))).ok()?;
        let file = |key: &str| {
            json.get(key)
                .and_then(Value::as_str)
                .filter(|f| !f.is_empty() && !f.contains(['/', '\\']) && *f != "..")
                .map(|f| dir.join(f))
                .filter(|p| p.is_file())
        };
        let images = [self.root.parent(), base.parent()]
            .into_iter()
            .flatten()
            .map(|p| p.join("images"))
            .find(|p| p.join("bbl_bed_pte_left.svg").is_file());
        Some(BedResources {
            vendor,
            model: file("bed_model"),
            texture: file("bed_texture"),
            images,
        })
    }

    /// Axis-aligned bounds of the upstream printer's printable area, in millimeters.
    /// Bambu machines have rectangular beds; polygon/exclusion validation remains
    /// the slicing engine's responsibility.
    pub fn bed_bounds(&self, machine: &str) -> Option<([f64; 2], [f64; 2])> {
        let config = self.flatten(Kind::Machine, machine).ok()?;
        let points = config.get("printable_area")?.as_array()?;
        let mut lo = [f64::INFINITY; 2];
        let mut hi = [f64::NEG_INFINITY; 2];
        for point in points {
            let (x, y) = point.as_str()?.split_once('x')?;
            let point = [x.trim().parse::<f64>().ok()?, y.trim().parse::<f64>().ok()?];
            if !point.iter().all(|v| v.is_finite()) {
                return None;
            }
            for axis in 0..2 {
                lo[axis] = lo[axis].min(point[axis]);
                hi[axis] = hi[axis].max(point[axis]);
            }
        }
        (hi[0] > lo[0] && hi[1] > lo[1]).then_some((lo, hi))
    }

    /// Повний профіль: батьки → шаблони з `include` → власні ключі.
    pub fn flatten(&self, kind: Kind, name: &str) -> Result<Map<String, Value>> {
        let info = self
            .get(kind, name)
            .with_context(|| format!("немає профілю {} «{name}»", kind.dir()))?;
        self.flatten_info(kind, info, 0, &|child, parent| self.parent_of(kind, child, parent))
    }

    /// Як [`Self::flatten`] до виправлення: батьки й шаблони — за одним лише іменем, з мапи,
    /// де спільне службове ім’я займав останній просканований вендор.
    fn legacy_flatten(&self, kind: Kind, name: &str) -> Result<Map<String, Value>> {
        let lookup = |name: &str| {
            self.get(kind, name)
                .filter(|p| p.vendor.is_none())
                .or_else(|| self.legacy_presets.get(&(kind, name.to_owned())))
        };
        let info = lookup(name).with_context(|| format!("немає профілю {} «{name}»", kind.dir()))?;
        self.flatten_info(kind, info, 0, &|_, parent| lookup(parent))
    }

    /// Ключі, які Slicenv до виправлення розгортав інакше (від батька чужого вендора), з тим
    /// старим значенням. Таке значення в проєкті, збереженому тоді, — не правка користувача,
    /// а наслідок помилки. Сюди входять і ключі, яких у правильному ланцюжку немає зовсім
    /// (рушій бере свій дефолт): `use_relative_e_distances` = 0 від Anker у пресеті BBL.
    pub fn stale_values(&self, kind: Kind, name: &str) -> Map<String, Value> {
        let (Ok(legacy), Ok(current)) = (self.legacy_flatten(kind, name), self.flatten(kind, name)) else {
            return Map::new();
        };
        legacy
            .into_iter()
            .filter(|(key, value)| current.get(key) != Some(value))
            .collect()
    }

    /// [`Self::stale_values`], які справді є в конфігу проєкту `config` з тим самим значенням і
    /// не позначені Bambu Studio як змінені користувачем (`different_settings_to_system`):
    /// навмисні 3 стінки в проєкті Bambu Studio лишаються правкою.
    pub fn stale_project_values(&self, kind: Kind, name: &str, config: &Map<String, Value>) -> Map<String, Value> {
        let edited: std::collections::HashSet<&str> = config
            .get("different_settings_to_system")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .flat_map(|s| s.split(';'))
            .collect();
        self.stale_values(kind, name)
            .into_iter()
            .filter(|(key, old)| !edited.contains(key.as_str()) && config.get(key) == Some(old))
            .collect()
    }

    fn flatten_info<'a>(
        &'a self,
        kind: Kind,
        info: &'a PresetInfo,
        depth: u32,
        lookup: &dyn Fn(&'a PresetInfo, &str) -> Option<&'a PresetInfo>,
    ) -> Result<Map<String, Value>> {
        if depth > 32 {
            bail!("зациклений inherits у профілі «{}»", info.name);
        }
        let own = read_json(&info.path)?;
        let related = |name: &str| {
            lookup(info, name)
                .with_context(|| format!("немає профілю {} «{name}» (батько «{}»)", kind.dir(), info.name))
        };

        let mut out = match own
            .get("inherits")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            Some(parent) => self.flatten_info(kind, related(parent)?, depth + 1, lookup)?,
            None => Map::new(),
        };
        for include in own
            .get("include")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(include) = include.as_str() else {
                continue;
            };
            let template = self.flatten_info(kind, related(include)?, depth + 1, lookup)?;
            out.extend(
                template
                    .into_iter()
                    .filter(|(k, _)| k != "name" && k != "instantiation"),
            );
        }
        out.extend(own);
        out.remove("inherits");
        out.remove("include");
        Ok(out)
    }

    /// Що зараз вибрано в Bambu Studio (`BambuStudio.conf` → `presets`).
    pub fn studio_selection(&self) -> Option<Selection> {
        let conf = read_json(&self.root.join("BambuStudio.conf"))
            .or_else(|_| read_json(&self.root.join("OrcaSlicer.conf")))
            .ok()?;
        let presets = conf.get("presets")?;
        let str_of = |key: &str| presets.get(key)?.as_str().map(str::to_owned);
        Some(Selection {
            machine: str_of("machine")?,
            process: str_of("process")?,
            filament: presets.get("filaments")?.get(0)?.as_str()?.to_owned(),
            bed_type: conf
                .get("app")
                .and_then(|a| a.get("curr_bed_type"))
                .and_then(Value::as_str)
                .map(|v| match v {
                    "1" => "Cool Plate",
                    "2" => "Engineering Plate",
                    "3" => "High Temp Plate",
                    "4" => "Textured PEI Plate",
                    "5" => "Supertack Plate",
                    other => other,
                })
                .unwrap_or(BED_TYPES[0])
                .to_owned(),
        })
    }

    /// Робить вибір сумісним: невідоме або несумісне замінює на перший підхожий профіль.
    pub fn sanitize(&self, mut selection: Selection) -> Selection {
        let pick = |kind: Kind, current: &str, machine: &str| -> String {
            let list = self.compatible(kind, machine);
            if list.iter().any(|p| p.name == current) {
                current.to_owned()
            } else {
                list.first().map(|p| p.name.clone()).unwrap_or_default()
            }
        };
        if self
            .get(Kind::Machine, &selection.machine)
            .is_none_or(|p| !p.instantiable)
        {
            selection.machine = self
                .compatible(Kind::Machine, "")
                .first()
                .map(|p| p.name.clone())
                .unwrap_or_default();
        }
        selection.process = pick(Kind::Process, &selection.process, &selection.machine);
        selection.filament = pick(Kind::Filament, &selection.filament, &selection.machine);
        if selection.bed_type.is_empty() {
            selection.bed_type = BED_TYPES[0].into();
        }
        selection
    }
}

/// Профілі, з якими проєкт був збережений (`Metadata/project_settings.config` у 3MF).
pub fn project_selection(path: &Path) -> Option<Selection> {
    use std::io::Read as _;
    let file = fs::File::open(path).ok()?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file)).ok()?;
    let mut entry = archive.by_name("Metadata/project_settings.config").ok()?;
    let mut text = String::new();
    entry.read_to_string(&mut text).ok()?;
    let config: Map<String, Value> = serde_json::from_str(&text).ok()?;
    let str_of = |key: &str| -> Option<String> {
        match config.get(key)? {
            Value::String(s) => Some(s.clone()),
            Value::Array(items) => items.first()?.as_str().map(str::to_owned),
            _ => None,
        }
    };
    Some(Selection {
        machine: str_of("printer_settings_id")?,
        process: str_of("print_settings_id")?,
        filament: str_of("filament_settings_id")?,
        bed_type: str_of("curr_bed_type").unwrap_or_else(|| BED_TYPES[0].into()),
    })
}

/// Бібліотека філаментів Orca: спільна база для системних профілів усіх вендорів.
const LIBRARY_VENDOR: &str = "OrcaFilamentLibrary";

fn scan(
    dir: &Path,
    kind: Kind,
    vendor: Option<&str>,
    out: &mut HashMap<(Kind, String), PresetInfo>,
) {
    let Ok(read) = fs::read_dir(dir) else { return };
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with('.'))
            {
                continue;
            }
            scan(&path, kind, vendor, out);
            continue;
        }
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let Ok(json) = read_json(&path) else { continue };
        let Some(name) = json.get("name").and_then(Value::as_str) else {
            continue;
        };
        if json.get("type").and_then(Value::as_str) == Some("machine_model") {
            continue;
        }
        let instantiable = json.get("instantiation").and_then(Value::as_str) != Some("false")
            && json.get("instantiation").and_then(Value::as_bool) != Some(false);
        let compatible_printers = json
            .get("compatible_printers")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let inherits = json
            .get("inherits")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let includes = json
            .get("include")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        out.insert(
            (kind, name.to_owned()),
            PresetInfo {
                name: name.to_owned(),
                path,
                instantiable,
                compatible_printers,
                inherits,
                includes,
                vendor: vendor.map(str::to_owned),
                own: false,
            },
        );
    }
}

fn read_json(path: &Path) -> Result<Map<String, Value>> {
    let text =
        fs::read_to_string(path).with_context(|| format!("не читається {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("зламаний JSON {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "slicer-presets-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&path);
            for dir in [
                "system/BBL/machine",
                "system/BBL/process",
                "system/BBL/filament",
            ] {
                fs::create_dir_all(path.join(dir)).unwrap();
            }
            Self(path)
        }

        fn put(&self, kind: Kind, name: &str, value: Value) {
            fs::write(
                self.0
                    .join("system/BBL")
                    .join(kind.dir())
                    .join(format!("{name}.json")),
                serde_json::to_vec(&value).unwrap(),
            )
            .unwrap();
        }

        fn open(&self) -> PresetStore {
            PresetStore::open_with_own_root(&self.0, &self.0.join("own")).unwrap()
        }

        fn open_with_own(&self, own: &Path) -> PresetStore {
            PresetStore::open_with_own_root(&self.0, own).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Регресія: службові батьки з однаковими іменами в різних вендорів (`fdm_machine_common`
    /// Anker — абсолютний E, 3 стінки) не підміняють батьків BBL. Через це Bambu 2.7 різав
    /// сферу 11 г як 1,6 кг: G-code з `M82`, а рахунок ваги — як для відносного E.
    #[test]
    fn inherits_resolve_within_the_vendor_and_orca_library() {
        let f = Fixture::new();
        let write = |vendor: &str, kind: Kind, value: Value| {
            let dir = f.0.join("system").join(vendor).join(kind.dir());
            fs::create_dir_all(&dir).unwrap();
            let name = value["name"].as_str().unwrap().to_owned();
            fs::write(dir.join(format!("{name}.json")), serde_json::to_vec(&value).unwrap()).unwrap();
        };
        // Вендори з власними теками видно лише з `<vendor>.json` поруч (як у рушія).
        for vendor in ["BBL", "Anker", "Zeta", LIBRARY_VENDOR] {
            fs::write(f.0.join("system").join(format!("{vendor}.json")), "{}").unwrap();
        }
        // Як у справжньому каталозі: у BBL `use_relative_e_distances` немає (дефолт рушія — 1).
        for (vendor, walls) in [("Anker", "3"), ("BBL", "2"), ("Zeta", "4")] {
            let mut common = serde_json::json!({
                "name": "fdm_machine_common", "instantiation": "false", "vendor_mark": vendor,
            });
            if vendor != "BBL" {
                common["use_relative_e_distances"] = "0".into();
            }
            write(vendor, Kind::Machine, common);
            write(vendor, Kind::Process, serde_json::json!({
                "name": "fdm_process_common", "instantiation": "false", "wall_loops": walls,
            }));
            write(vendor, Kind::Machine, serde_json::json!({
                "name": "template gcode", "instantiation": "false", "machine_start_gcode": vendor,
            }));
        }
        write("BBL", Kind::Machine, serde_json::json!({
            "name": "Printer 0.4", "instantiation": "true", "inherits": "fdm_machine_common",
            "include": ["template gcode"],
        }));
        write("Anker", Kind::Machine, serde_json::json!({
            "name": "Anker 0.4", "instantiation": "true", "inherits": "fdm_machine_common",
        }));
        write("BBL", Kind::Process, serde_json::json!({
            "name": "0.20mm", "instantiation": "true", "inherits": "fdm_process_common",
            "compatible_printers": ["Printer 0.4"],
        }));
        // Філамент BBL успадковує базу, якої немає в BBL: рушій Orca бере її з бібліотеки,
        // а не з першого-ліпшого вендора з таким самим ім’ям.
        write(LIBRARY_VENDOR, Kind::Filament, serde_json::json!({
            "name": "fdm_filament_pla", "instantiation": "false", "nozzle_temperature": ["220"],
        }));
        write("Anker", Kind::Filament, serde_json::json!({
            "name": "fdm_filament_pla", "instantiation": "false", "nozzle_temperature": ["210"],
        }));
        write("BBL", Kind::Filament, serde_json::json!({
            "name": "PLA", "instantiation": "true", "inherits": "fdm_filament_pla",
        }));
        // Користувацький процес: батько — системний профіль, далі ланцюжок BBL.
        let own = f.0.join("own");
        fs::create_dir_all(own.join("process")).unwrap();
        fs::write(
            own.join("process/My 0.20mm.json"),
            serde_json::to_vec(&serde_json::json!({
                "name": "My 0.20mm", "inherits": "0.20mm", "from": "User", "top_surface_pattern": "concentric",
            }))
            .unwrap(),
        )
        .unwrap();

        let store = f.open_with_own(&own);
        let machine = store.flatten(Kind::Machine, "Printer 0.4").unwrap();
        assert!(machine.get("use_relative_e_distances").is_none());
        assert_eq!(machine["vendor_mark"], "BBL");
        assert_eq!(machine["machine_start_gcode"], "BBL");
        let anker = store.flatten(Kind::Machine, "Anker 0.4").unwrap();
        assert_eq!(anker["use_relative_e_distances"], "0");
        assert_eq!(store.flatten(Kind::Process, "0.20mm").unwrap()["wall_loops"], "2");
        assert_eq!(store.flatten(Kind::Filament, "PLA").unwrap()["nozzle_temperature"], serde_json::json!(["220"]));
        let user = store.flatten(Kind::Process, "My 0.20mm").unwrap();
        assert_eq!(user["wall_loops"], "2");
        assert_eq!(user["top_surface_pattern"], "concentric");
        assert!(user.get("inherits").is_none() && user.get("include").is_none());
        // Сумісність користувацького профілю — від його системного предка.
        assert_eq!(store.get(Kind::Process, "My 0.20mm").unwrap().compatible_printers, ["Printer 0.4"]);
        assert_eq!(store.vendor_for_machine("Anker 0.4").as_deref(), Some("Anker"));
        // У загальному списку спільне ім’я належить BBL, незалежно від порядку `read_dir`.
        assert_eq!(store.flatten(Kind::Machine, "fdm_machine_common").unwrap()["vendor_mark"], "BBL");

        // Старий Slicenv брав батька останнього вендора (Anker чи Zeta — як віддасть
        // `read_dir`): ці значення в збереженому тоді проєкті — не правки користувача.
        let stale = store.stale_values(Kind::Machine, "Printer 0.4");
        assert_eq!(stale["use_relative_e_distances"], "0");
        assert!(matches!(stale["vendor_mark"].as_str(), Some("Anker" | "Zeta")));
        let stale_process = store.stale_values(Kind::Process, "My 0.20mm");
        assert!(matches!(stale_process["wall_loops"].as_str(), Some("3" | "4")));
        assert!(!stale_process.contains_key("top_surface_pattern"));
        assert!(store.stale_values(Kind::Machine, "Anker 0.4").get("use_relative_e_distances").is_none());

        // Нарізка: із конфігу проєкту зникає лише застаріле значення, якого не перекриють наші
        // JSON; правка, позначена Bambu Studio в `different_settings_to_system`, лишається.
        let selection = Selection {
            machine: "Printer 0.4".into(),
            process: "My 0.20mm".into(),
            filament: "PLA".into(),
            bed_type: BED_TYPES[0].into(),
        };
        let mut effective = store.flatten(Kind::Machine, "Printer 0.4").unwrap();
        effective.extend(store.flatten(Kind::Process, "My 0.20mm").unwrap());
        let mut project: Map<String, Value> = serde_json::from_value(serde_json::json!({
            "use_relative_e_distances": "0", "wall_loops": stale_process["wall_loops"],
            "filament_colour": ["#FF0000"], "version": "2.0.0.0",
        }))
        .unwrap();
        assert!(crate::cli::drop_stale_keys(&store, &selection, &effective, &mut project));
        assert!(!project.contains_key("use_relative_e_distances"));
        assert!(project.contains_key("wall_loops"), "перекриває process.json, чіпати не треба");
        assert!(project.contains_key("filament_colour") && project.contains_key("version"));
        let mut edited = project.clone();
        edited.insert("use_relative_e_distances".into(), "0".into());
        edited.insert("different_settings_to_system".into(), serde_json::json!(["", "", "use_relative_e_distances;wipe"]));
        assert!(!crate::cli::drop_stale_keys(&store, &selection, &effective, &mut edited));
        assert_eq!(edited["use_relative_e_distances"], "0");
    }

    fn profile(name: &str, instantiation: &str) -> Value {
        serde_json::json!({"name": name, "instantiation": instantiation})
    }

    #[test]
    fn packaged_catalog_and_separate_user_roots_survive_reload() {
        let f = Fixture::new();
        f.put(
            Kind::Process,
            "Base process",
            serde_json::json!({"name":"Base process", "layer_height":"0.2"}),
        );
        let users = tempfile::tempdir().unwrap();
        let a = users.path().join("bambu");
        let b = users.path().join("orca");
        for (root, height) in [(&a, "0.12"), (&b, "0.28")] {
            fs::create_dir_all(root.join("process")).unwrap();
            fs::write(root.join("process/User process.json"), serde_json::to_vec(&serde_json::json!({"name":"User process", "inherits":"Base process", "layer_height":height})).unwrap()).unwrap();
        }
        // A packaged resources/profiles tree contains BBL directly, no system/ prefix.
        let resource_root = f.0.join("system");
        let first = PresetStore::open_with_own_root(&resource_root, &a).unwrap();
        let second = PresetStore::open_with_own_root(&resource_root, &b).unwrap();
        assert_eq!(
            first.flatten(Kind::Process, "User process").unwrap()["layer_height"],
            "0.12"
        );
        assert_eq!(
            second
                .reload()
                .unwrap()
                .flatten(Kind::Process, "User process")
                .unwrap()["layer_height"],
            "0.28"
        );
        assert_eq!(second.user_root(), b);
    }

    #[test]
    fn studio_numeric_plate_selection_is_a_plate_name() {
        let f = Fixture::new();
        fs::write(f.0.join("BambuStudio.conf"), serde_json::to_vec(&serde_json::json!({"app":{"curr_bed_type":"4"},"presets":{"machine":"M","process":"P","filaments":["F"]}})).unwrap()).unwrap();
        assert_eq!(
            f.open().studio_selection().unwrap().bed_type,
            "Textured PEI Plate"
        );
    }

    #[test]
    fn device_model_identity_comes_from_upstream_metadata() {
        let f = Fixture::new();
        f.put(
            Kind::Machine,
            "Printer model",
            serde_json::json!({"type":"machine_model", "name":"Printer model", "model_id":"N1"}),
        );
        f.put(
            Kind::Machine,
            "Printer nozzle",
            serde_json::json!({"name":"Printer nozzle", "printer_model":"Printer model"}),
        );
        let store = f.open();
        assert_eq!(
            store.vendor_for_machine("Printer nozzle").as_deref(),
            Some("BBL")
        );
        assert_eq!(
            store.machine_model_id("Printer nozzle").as_deref(),
            Some("N1")
        );
        assert!(store.get(Kind::Machine, "Printer model").is_none());
    }

    #[test]
    fn bed_bounds_follow_machine_inheritance_and_reject_bad_geometry() {
        let f = Fixture::new();
        f.put(Kind::Machine, "small", serde_json::json!({
            "name":"small", "instantiation":"false", "printable_area":["-10x5", "170x5", "170x185 ", "-10x185"]
        }));
        f.put(
            Kind::Machine,
            "Small 0.4",
            serde_json::json!({"name":"Small 0.4", "inherits":"small"}),
        );
        f.put(
            Kind::Machine,
            "bad",
            serde_json::json!({"name":"bad", "printable_area":["NaNx0", "180x180"]}),
        );
        let store = f.open();
        assert_eq!(
            store.bed_bounds("Small 0.4"),
            Some(([-10.0, 5.0], [170.0, 185.0]))
        );
        assert!(store.bed_bounds("bad").is_none());
    }

    #[test]
    fn discovers_printer_and_nozzle_variants_and_filters_by_compatibility() {
        let f = Fixture::new();
        for machine in [
            "Bambu Lab P1S 0.4 nozzle",
            "Bambu Lab P1S 0.6 nozzle",
            "Bambu Lab X1 Carbon 0.4 nozzle",
        ] {
            f.put(Kind::Machine, machine, profile(machine, "true"));
        }
        let mut p1_process = profile("P1 quality", "true");
        p1_process["compatible_printers"] =
            serde_json::json!(["Bambu Lab P1S 0.4 nozzle", "Bambu Lab P1S 0.6 nozzle"]);
        f.put(Kind::Process, "p1", p1_process);
        let mut x1_filament = profile("X1 filament", "true");
        x1_filament["compatible_printers"] = serde_json::json!(["Bambu Lab X1 Carbon 0.4 nozzle"]);
        f.put(Kind::Filament, "x1", x1_filament);

        f.put(
            Kind::Machine,
            "Printer metadata",
            serde_json::json!({"type":"machine_model", "name":"Printer metadata"}),
        );
        let store = f.open();
        let machines = store.compatible(Kind::Machine, "");
        assert_eq!(machines.len(), 3);
        assert!(
            store
                .compatible(Kind::Process, "Bambu Lab P1S 0.6 nozzle")
                .iter()
                .any(|p| p.name == "P1 quality")
        );
        assert!(
            store
                .compatible(Kind::Process, "Bambu Lab X1 Carbon 0.4 nozzle")
                .is_empty()
        );
        assert!(
            store
                .compatible(Kind::Filament, "Bambu Lab X1 Carbon 0.4 nozzle")
                .iter()
                .any(|p| p.name == "X1 filament")
        );
    }

    #[test]
    fn inherits_compatibility_and_uses_upstream_printer_defaults() {
        let f = Fixture::new();
        f.put(
            Kind::Machine,
            "Printer 0.4 nozzle",
            serde_json::json!({
                "name":"Printer 0.4 nozzle", "instantiation":"true",
                "default_print_profile":"Fine process", "default_filament_profile":["PLA"],
            }),
        );
        f.put(Kind::Process, "base process", serde_json::json!({
            "name":"base process", "instantiation":"false", "compatible_printers":["Printer 0.4 nozzle"]
        }));
        f.put(
            Kind::Process,
            "Fine process",
            serde_json::json!({
                "name":"Fine process", "instantiation":"true", "inherits":"base process"
            }),
        );
        f.put(
            Kind::Filament,
            "PLA",
            serde_json::json!({
                "name":"PLA", "instantiation":"true", "compatible_printers":["Printer 0.4 nozzle"]
            }),
        );

        let store = f.open();
        assert!(
            store
                .compatible(Kind::Process, "Printer 0.4 nozzle")
                .iter()
                .any(|p| p.name == "Fine process")
        );
        let defaults = store.defaults_for_machine("Printer 0.4 nozzle").unwrap();
        assert_eq!(defaults.machine, "Printer 0.4 nozzle");
        assert_eq!(defaults.process, "Fine process");
        assert_eq!(defaults.filament, "PLA");
    }

    #[test]
    fn sanitizing_unknown_printer_chooses_an_installed_printer_without_a1_assumptions() {
        let f = Fixture::new();
        f.put(
            Kind::Machine,
            "Installed Delta 0.6 nozzle",
            profile("Installed Delta 0.6 nozzle", "true"),
        );
        let store = f.open();
        let selection = store.sanitize(Selection {
            machine: "removed printer".into(),
            process: "missing process".into(),
            filament: "missing filament".into(),
            bed_type: "Printer Specific Plate".into(),
        });
        assert_eq!(selection.machine, "Installed Delta 0.6 nozzle");
        assert_eq!(selection.bed_type, "Printer Specific Plate");
    }

    #[test]
    fn app_owned_rename_and_delete_update_json_and_sidecar_without_touching_upstream() {
        let f = Fixture::new();
        f.put(Kind::Process, "parent", profile("parent", "false"));
        let own = f.0.join("app-presets");
        let dir = own.join("process");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("Old.json"),
            serde_json::json!({"name":"Old", "inherits":"parent", "print_settings_id":"Old"})
                .to_string(),
        )
        .unwrap();
        fs::write(dir.join("Old.info"), "user_id = test\n").unwrap();
        fs::write(
            dir.join("Sibling.json"),
            serde_json::to_vec(
                &serde_json::json!({"name":"Sibling", "instantiation":"true", "inherits":"parent"}),
            )
            .unwrap(),
        )
        .unwrap();
        let mut store = f.open_with_own(&own);
        store.rename_user(Kind::Process, "Old", "New").unwrap();
        assert!(store.get(Kind::Process, "Sibling").unwrap().own);
        let renamed: Value =
            serde_json::from_slice(&fs::read(dir.join("New.json")).unwrap()).unwrap();
        assert_eq!(renamed["name"], "New");
        assert_eq!(renamed["print_settings_id"], "New");
        assert_eq!(
            fs::read_to_string(dir.join("New.info")).unwrap(),
            "user_id = test\n"
        );
        assert!(dir.join("Old.json").exists() == false);
        assert!(store.get(Kind::Process, "New").unwrap().own);
        store.delete_user(Kind::Process, "New").unwrap();
        assert!(!dir.join("New.json").exists());
        assert!(!dir.join("New.info").exists());
        assert!(f.0.join("system/BBL/process/parent.json").exists());
    }

    #[test]
    fn mutations_reject_invalid_missing_system_and_parent_profiles_without_changes() {
        let f = Fixture::new();
        f.put(Kind::Process, "System", profile("System", "true"));
        let own = f.0.join("app-presets");
        fs::create_dir_all(own.join("process")).unwrap();
        let child = own.join("process/Child.json");
        fs::write(&child, r#"{"name":"Child","inherits":"Parent"}"#).unwrap();
        let parent = own.join("process/Parent.json");
        fs::write(&parent, r#"{"name":"Parent"}"#).unwrap();
        let mut store = f.open_with_own(&own);
        assert!(
            store
                .rename_user(Kind::Process, "Parent", "bad/name")
                .is_err()
        );
        assert!(
            store
                .rename_user(Kind::Process, "Parent", "Renamed")
                .is_err()
        );
        assert!(store.delete_user(Kind::Process, "Parent").is_err());
        assert!(store.delete_user(Kind::Process, "absent").is_err());
        assert!(
            store
                .rename_user(Kind::Process, "System", "Renamed")
                .is_err()
        );
        assert!(store.delete_user(Kind::Process, "System").is_err());
        assert!(parent.exists() && child.exists());
        assert!(f.0.join("system/BBL/process/System.json").exists());
    }

    #[test]
    fn invalid_sidecar_preflight_leaves_preset_and_destination_untouched() {
        let f = Fixture::new();
        let own = f.0.join("app-presets");
        let dir = own.join("process");
        fs::create_dir_all(dir.join("Bad.info")).unwrap();
        fs::write(dir.join("Bad.json"), r#"{"name":"Bad"}"#).unwrap();
        let mut store = f.open_with_own(&own);
        assert!(store.rename_user(Kind::Process, "Bad", "Good").is_err());
        assert!(store.delete_user(Kind::Process, "Bad").is_err());
        assert!(dir.join("Bad.json").is_file());
        assert!(dir.join("Bad.info").is_dir());
        assert!(!dir.join("Good.json").exists());
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;
    fn tree(root: &Path) {
        fs::create_dir_all(root).unwrap();
        fs::write(root.join("BBL.json"), "{}").unwrap();
        for kind in Kind::ALL {
            fs::create_dir_all(root.join("BBL").join(kind.dir())).unwrap();
        }
    }
    fn catalog(root: &Path, family: &str, version: &str) -> PathBuf {
        let path = root.join(family).join(version);
        tree(&path);
        fs::write(
            path.join("catalog.json"),
            serde_json::json!({"family":family,"version":version,"vendors":["BBL"]}).to_string(),
        )
        .unwrap();
        path
    }
    #[test]
    fn catalog_resolution_order_and_family_isolation() {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("library");
        let package = temp.path().join("package");
        tree(&package);
        let fallback = temp.path().join("fallback");
        tree(&fallback);
        let older = catalog(&library, "orca", "2.4.2");
        let exact = catalog(&library, "orca", "2.5.0-beta");
        let newest = catalog(&library, "orca", "2.10.0");
        let resolve = |override_root, packages: &[PathBuf], family, version| {
            resolve_catalog(
                override_root,
                packages,
                &library,
                family,
                version,
                std::slice::from_ref(&fallback),
            )
        };
        assert_eq!(
            resolve(
                Some(temp.path().join("override")),
                std::slice::from_ref(&package),
                "orca",
                "2.5.0-beta"
            ),
            Some(temp.path().join("override"))
        );
        assert_eq!(
            resolve(None, std::slice::from_ref(&package), "orca", "2.5.0-beta"),
            Some(package)
        );
        assert_eq!(resolve(None, &[], "orca", "2.5.0-beta"), Some(exact));
        assert_eq!(resolve(None, &[], "orca", "2.4.9"), Some(older));
        assert_eq!(resolve(None, &[], "orca", "1.0.0"), Some(newest));
        assert_eq!(resolve(None, &[], "bambu", "2.5.0-beta"), Some(fallback));
    }
    #[test]
    fn opc_and_cover_directory_are_not_json_profiles() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("BBL")).unwrap();
        fs::write(temp.path().join("BBL.opc"), b"ZCRO").unwrap();
        assert!(!valid_json_tree(temp.path(), "BBL"));
        assert!(
            resolve_catalog(
                None,
                &[temp.path().to_owned()],
                &temp.path().join("missing"),
                "orca",
                "2.5.0-beta",
                &[]
            )
            .is_none()
        );
        tree(temp.path());
        assert!(valid_json_tree(temp.path(), "BBL"));
    }
}
