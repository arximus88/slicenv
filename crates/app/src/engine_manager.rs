//! Менеджер рушіїв: офіційні збірки Bambu Studio й OrcaSlicer з GitHub, кожна версія у
//! власній теці `<дані Slicenv>/engines/<родина>/<версія>/` з `manifest.json`.
//! Порт `crates/slicer/tools/manage_engines.py` на Rust, щоб працювати без Python і на
//! Windows/macOS.
//!
//! - Linux: AppImage (unsquashfs або `--appimage-extract`), tar-архіви.
//! - Windows: portable `.zip` (Orca `*_Windows_*_portable.zip`, Bambu `*_win-*.zip`) —
//!   без інсталятора, прав адміністратора, реєстру й ярликів.
//! - macOS: `.dmg` через `hdiutil` (лише в коді — **на справжньому Mac не перевірено**).
//!
//! Безпека як у Python-версії: лише офіційні репозиторії, обов’язковий SHA-256 з метаданих
//! GitHub, жодних шляхів і посилань за межі теки версії, атомарна заміна через `.staging`,
//! активний рушій у `engines.json` не змінюється.
use std::{
    io::{Read as _, Write as _},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context as _, Result, bail, ensure};
use cloud::http::{Request, Transport as _};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

const REPOS: [(&str, &str); 2] = [("bambu", "bambulab/BambuStudio"), ("orca", "OrcaSlicer/OrcaSlicer")];
/// Найбільший пакет (Bambu Studio для Windows) — ~0.5 ГБ; запас удвічі.
const MAX_PACKAGE: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Os {
    Linux,
    Windows,
    Mac,
}

impl Os {
    pub(crate) const CURRENT: Os = if cfg!(windows) {
        Os::Windows
    } else if cfg!(target_os = "macos") {
        Os::Mac
    } else {
        Os::Linux
    };
}

/// Тека, куди менеджер ставить рушії. Бібліотека моделей її не сканує.
pub(crate) fn engines_dir() -> PathBuf {
    paths::data_dir().join("engines")
}

/// Де лежать версії рушіїв і `engines.json`; у тестах — тимчасові теки.
#[derive(Clone, Debug)]
pub(crate) struct Dirs {
    pub engines: PathBuf,
    pub config: PathBuf,
}

impl Default for Dirs {
    fn default() -> Self {
        Self {
            engines: engines_dir(),
            config: paths::config_dir().join("engines.json"),
        }
    }
}

/// Що просить інтерфейс.
#[derive(Clone, Debug)]
pub(crate) enum Op {
    ListReleases { channel: String, limit: usize },
    Install { family: String, tag: String, asset_id: u64, vendors: Vec<String> },
    Remove { family: String, version: String },
}

/// Подія прогресу в тому самому форматі, що й NDJSON Python-менеджера.
pub(crate) type Events<'a> = &'a mut dyn FnMut(Value);

pub(crate) fn run(op: &Op, events: Events<'_>, cancel: &AtomicBool) -> Result<Value> {
    let dirs = Dirs::default();
    match op {
        Op::ListReleases { channel, limit } => Ok(Value::Array(list_releases(channel, *limit)?)),
        Op::Install { family, tag, asset_id, vendors } => {
            install_release(&dirs, family, tag, *asset_id, vendors, events, cancel)
        }
        Op::Remove { family, version } => remove(&dirs, family, version),
    }
}

pub(crate) fn list_default() -> Result<Value> {
    Ok(Value::Array(list(&Dirs::default())?))
}

// ---- перевірки ----

fn repo(family: &str) -> Result<&'static str> {
    REPOS
        .iter()
        .find(|(f, _)| *f == family)
        .map(|(_, r)| *r)
        .with_context(|| format!("невідома родина рушія «{family}»"))
}

pub(crate) fn valid_version(version: &str) -> bool {
    let mut chars = version.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && version.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
}

fn valid_sha256(sha: &str) -> bool {
    sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Жодна складова шляху не є символьним посиланням (інакше теку версії можна підмінити).
fn reject_symlinks(path: &Path) -> Result<()> {
    let mut cursor = Some(path);
    while let Some(p) = cursor {
        if p.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
            bail!("у шляху до рушіїв є символьне посилання: {}", p.display());
        }
        cursor = p.parent();
    }
    Ok(())
}

/// Лексична нормалізація (без звернення до диска): `a/b/../c` → `a/c`.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

// ---- випуски ----

/// Архітектура процесора, під яку вибирається пакет.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arch {
    X64,
    Arm64,
}

impl Arch {
    /// Справжній процесор, а не збірка Slicenv: x64-Slicenv під емуляцією на Windows ARM
    /// має ставити рідну arm64-збірку рушія (`PROCESSOR_ARCHITEW6432` / `PROCESSOR_ARCHITECTURE`).
    pub(crate) fn current() -> Arch {
        if cfg!(windows) {
            let machine = std::env::var("PROCESSOR_ARCHITEW6432")
                .or_else(|_| std::env::var("PROCESSOR_ARCHITECTURE"))
                .unwrap_or_default();
            if machine.eq_ignore_ascii_case("ARM64") {
                return Arch::Arm64;
            }
            return Arch::X64;
        }
        if cfg!(target_arch = "aarch64") { Arch::Arm64 } else { Arch::X64 }
    }
}

/// Архітектура з назви файлу: `None` — без позначки (Bambu для Windows — x64, macOS —
/// universal).
pub(crate) fn asset_arch(name: &str) -> Option<Arch> {
    let l = name.to_lowercase();
    if ["aarch64", "arm64"].iter().any(|a| l.contains(a)) {
        Some(Arch::Arm64)
    } else if ["x64", "x86_64", "amd64"].iter().any(|a| l.contains(a)) {
        Some(Arch::X64)
    } else {
        None
    }
}

/// Пакет для цієї системи й процесора. Інсталятори (`.exe`) не використовуються.
pub(crate) fn asset_matches(os: Os, arch: Arch, name: &str) -> bool {
    let l = name.to_lowercase();
    if ["validator", "preset_tests", "custom_preset", "symbols", ".pdb", ".sha256"]
        .iter()
        .any(|t| l.contains(t))
        || ["armv7", "armhf", "i386", "i686"].iter().any(|a| l.contains(a))
    {
        return false;
    }
    // Без позначки архітектури — x64 (так публікують Bambu й старі Linux-збірки Orca).
    let arch_ok = asset_arch(name).unwrap_or(Arch::X64) == arch;
    match os {
        Os::Linux => {
            arch_ok
                && (l.ends_with(".appimage") || l.ends_with(".tar.gz") || l.ends_with(".tar.xz"))
                && !["windows", "_win", "win-", "mac", "flatpak"].iter().any(|t| l.contains(t))
        }
        Os::Windows => {
            arch_ok
                && l.ends_with(".zip")
                && !l.contains("installer")
                && ["windows", "_win-", "_win_", "-win-", "_win."].iter().any(|t| l.contains(t))
        }
        // Orca — universal; Bambu — без позначки архітектури.
        Os::Mac => l.ends_with(".dmg") && ["mac", "osx", "darwin"].iter().any(|t| l.contains(t)),
    }
}

/// stable / beta / nightly — як `classify_release_channel` у Python.
pub(crate) fn classify_channel(tag: &str, name: &str, prerelease: bool) -> &'static str {
    let text = format!("{tag} {name}").to_lowercase();
    if text.contains("nightly")
        || text.contains("daily")
        || ["dev-build", "dev_build", "dev build", "devbuild"].iter().any(|t| text.contains(t))
    {
        return "nightly";
    }
    let marked = ["alpha", "beta", "rc"].iter().any(|word| {
        text.match_indices(word).any(|(i, _)| {
            match text[i + word.len()..].chars().next() {
                None => true,
                Some(c) => c.is_ascii_digit() || c == '.' || !(c.is_alphanumeric() || c == '_'),
            }
        })
    });
    if prerelease || marked { "beta" } else { "stable" }
}

pub(crate) fn clean_tag(tag: &str) -> &str {
    tag.strip_prefix(['v', 'V']).unwrap_or(tag)
}

fn github(url: &str) -> Result<Value> {
    ensure!(
        REPOS.iter().any(|(_, r)| url.starts_with(&format!("https://api.github.com/repos/{r}/"))),
        "запит не до офіційного репозиторію"
    );
    let headers = [
        ("Accept".to_owned(), "application/vnd.github+json".to_owned()),
        ("User-Agent".to_owned(), "slicenv-engine-manager/1.0".to_owned()),
    ];
    let response = cloud::http::Curl.request(&Request {
        timeout_secs: 30,
        max_bytes: 48 * 1024 * 1024,
        ..Request::get(url, &headers)
    })?;
    match response.status {
        200 => {}
        403 | 429 => bail!("{}", crate::i18n::tr("GitHub тимчасово обмежив запити — спробуйте за кілька хвилин")),
        404 => bail!("{}", crate::i18n::tr("GitHub не знайшов такого випуску")),
        code => bail!("GitHub відповів {code}"),
    }
    serde_json::from_slice(&response.body).context("відповідь GitHub не JSON")
}

fn asset_json(asset: &Value) -> Value {
    let sha = asset["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).unwrap_or("");
    json!({
        "id": asset["id"],
        "name": asset["name"],
        "size": asset["size"].as_u64().unwrap_or(0),
        "download_url": asset["browser_download_url"],
        "sha256": sha,
        "asset_updated_at": asset["updated_at"],
    })
}

/// Опубліковані випуски для цієї системи; формат — як `list-releases --json`.
pub(crate) fn releases_from(os: Os, arch: Arch, family: &str, data: &[Value], channel: &str, limit: usize) -> Vec<Value> {
    let repo = repo(family).unwrap_or_default();
    data.iter()
        .filter(|r| r["draft"] != true)
        .filter_map(|r| {
            let tag = r["tag_name"].as_str().unwrap_or("");
            let name = r["name"].as_str().unwrap_or(tag);
            let release_channel = classify_channel(tag, name, r["prerelease"] == true);
            if channel != "all" && release_channel != channel {
                return None;
            }
            let assets: Vec<Value> = r["assets"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|a| a["name"].as_str().is_some_and(|n| asset_matches(os, arch, n)))
                .map(asset_json)
                .collect();
            (!assets.is_empty()).then(|| {
                json!({
                    "family": family,
                    "repo": repo,
                    "tag": tag,
                    "release_id": r["id"],
                    "html_url": r["html_url"],
                    "name": if name.is_empty() { tag } else { name },
                    "channel": release_channel,
                    "prerelease": r["prerelease"] == true,
                    "published_at": r["published_at"],
                    "assets": assets,
                })
            })
        })
        .take(limit)
        .collect()
}

pub(crate) fn list_releases(channel: &str, limit: usize) -> Result<Vec<Value>> {
    ensure!(
        ["stable", "beta", "nightly", "all"].contains(&channel),
        "канал має бути stable, beta, nightly або all"
    );
    let mut out = Vec::new();
    for (family, repo) in REPOS {
        let mut data = Vec::new();
        let pages = if channel == "stable" { 1 } else { 5 };
        for page in 1..=pages {
            let batch = github(&format!("https://api.github.com/repos/{repo}/releases?per_page=100&page={page}"))?;
            let batch = batch.as_array().cloned().unwrap_or_default();
            let last = batch.len() < 100;
            data.extend(batch);
            if last {
                break;
            }
        }
        out.extend(releases_from(Os::CURRENT, Arch::current(), family, &data, channel, limit));
    }
    Ok(out)
}

// ---- прогрес ----

pub(crate) struct Reporter<'a> {
    events: Events<'a>,
    last: Instant,
}

impl<'a> Reporter<'a> {
    pub(crate) fn new(events: Events<'a>) -> Self {
        Self { events, last: Instant::now() - Duration::from_secs(1) }
    }
    fn emit(&mut self, stage: &str, done: Option<u64>, total: Option<u64>) {
        let counted = matches!(stage, "download" | "profiles");
        if counted && done.is_some() && done != total && self.last.elapsed() < Duration::from_millis(200) {
            return;
        }
        self.last = Instant::now();
        let mut event = json!({"event": "progress", "stage": stage});
        if let Some(done) = done {
            event["downloaded"] = done.into();
        }
        if let (Some(total), Some(done)) = (total.filter(|t| *t > 0), done) {
            event["total"] = total.into();
            if counted {
                event["percent"] = ((done as f64 * 1000. / total as f64).round() / 10.).min(100.).into();
            }
        }
        (self.events)(event);
    }
}

// ---- встановлення ----

/// Звідки пакет: завантаження з GitHub або локальний файл (тести, ручне встановлення).
#[derive(Clone, Copy)]
pub(crate) enum Source<'a> {
    Download { url: &'a str, size: Option<u64> },
    #[cfg_attr(not(test), allow(dead_code))]
    Local(&'a Path),
}

pub(crate) struct Package<'a> {
    pub family: &'a str,
    pub version: &'a str,
    pub asset_name: &'a str,
    pub sha256: &'a str,
    pub asset_id: Option<u64>,
    pub channel: Option<&'a str>,
    pub release_tag: Option<&'a str>,
    pub source_url: Option<&'a str>,
}

fn validate_download_url(url: &str, family: &str) -> Result<()> {
    let repo = repo(family)?;
    ensure!(
        url.starts_with(&format!("https://github.com/{repo}/releases/download/")),
        "посилання не з випусків офіційного репозиторію {repo}"
    );
    ensure!(!url.bytes().any(|b| b.is_ascii_control() || b == b' '), "недопустимі символи в посиланні");
    Ok(())
}

fn install_release(
    dirs: &Dirs,
    family: &str,
    tag: &str,
    asset_id: u64,
    vendors: &[String],
    events: Events<'_>,
    cancel: &AtomicBool,
) -> Result<Value> {
    let repo = repo(family)?;
    let mut reporter = Reporter::new(events);
    reporter.emit("prepare", None, None);
    let release = github(&format!(
        "https://api.github.com/repos/{repo}/releases/tags/{}",
        crate::profile_fetch::encode_path(tag)
    ))?;
    let asset = release["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["id"].as_u64() == Some(asset_id))
        .context("у випуску немає вибраного файлу")?;
    let asset = asset_json(asset);
    let name = asset["name"].as_str().unwrap_or("");
    ensure!(asset_matches(Os::CURRENT, Arch::current(), name), "{name} — збірка не для цієї системи");
    let sha = asset["sha256"].as_str().unwrap_or("");
    ensure!(
        valid_sha256(sha),
        "{}",
        crate::i18n::tr("GitHub не повідомив SHA-256 для цього файлу, тож його неможливо перевірити")
    );
    let channel = classify_channel(
        release["tag_name"].as_str().unwrap_or(tag),
        release["name"].as_str().unwrap_or(""),
        release["prerelease"] == true,
    );
    let version = if channel == "nightly" {
        let id = release["id"].as_u64().context("випуск без ідентифікатора")?;
        format!("nightly-{id}-{asset_id}")
    } else {
        clean_tag(tag).to_owned()
    };
    let url = asset["download_url"].as_str().unwrap_or("");
    validate_download_url(url, family)?;
    let manifest = install(
        dirs,
        &Package {
            family,
            version: &version,
            asset_name: name,
            sha256: sha,
            asset_id: Some(asset_id),
            channel: Some(channel),
            release_tag: release["tag_name"].as_str(),
            source_url: release["html_url"].as_str(),
        },
        Source::Download { url, size: asset["size"].as_u64() },
        &mut reporter,
        cancel,
    )?;
    // Пакет без JSON-профілів (Orca 2.5 — лише бінарний кеш .opc): каталог з тегу випуску.
    let packaged = manifest["profile_paths"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .any(|p| Path::new(p).join("BBL.json").is_file());
    let mut profile_error = Value::Null;
    if !packaged && channel != "nightly" {
        let kind = if family == "orca" { slicer::EngineKind::Orca } else { slicer::EngineKind::Bambu };
        let mut wanted = vec!["BBL".to_owned()];
        wanted.extend(vendors.iter().cloned());
        wanted.dedup();
        let mut on_progress = |done: usize, total: usize| reporter.emit("profiles", Some(done as u64), Some(total as u64));
        if let Err(e) = crate::profile_fetch::fetch(kind, &version, &wanted, &mut on_progress, cancel) {
            profile_error = format!("{e:#}").into();
        }
    }
    reporter.emit("done", None, None);
    let mut result = manifest;
    result["profile_error"] = profile_error;
    Ok(result)
}

/// Перевіряє, розпаковує й реєструє пакет. Помилка на будь-якому кроці не лишає ні
/// часткової теки версії, ні запису в `engines.json`.
pub(crate) fn install(
    dirs: &Dirs,
    package: &Package<'_>,
    source: Source<'_>,
    reporter: &mut Reporter<'_>,
    cancel: &AtomicBool,
) -> Result<Value> {
    let Package { family, version, asset_name, sha256, .. } = *package;
    repo(family)?;
    ensure!(valid_version(version), "некоректна версія «{version}»");
    ensure!(
        !asset_name.is_empty()
            && Path::new(asset_name).file_name().is_some_and(|n| n == asset_name)
            && !matches!(asset_name, "." | ".."),
        "назва пакета має бути одним ім’ям файлу"
    );
    ensure!(valid_sha256(sha256), "для встановлення потрібен SHA-256");
    reject_symlinks(&dirs.engines)?;
    let family_dir = dirs.engines.join(family);
    reject_symlinks(&family_dir)?;
    std::fs::create_dir_all(&family_dir)?;
    ensure!(
        family_dir.canonicalize()?.parent() == Some(dirs.engines.canonicalize()?.as_path()),
        "тека родини рушія вказує за межі сховища"
    );
    let target = family_dir.join(version);
    reject_symlinks(&target)?;
    ensure!(
        !target.exists(),
        "{}",
        crate::tr_format!("{} {} уже встановлено", family, version)
    );
    if dirs.config.is_file() {
        read_config(&dirs.config)?;
    }

    let scratch = tempfile::Builder::new().prefix(".download-").tempdir_in(&family_dir)?;
    let archive = match source {
        Source::Download { url, size } => {
            validate_download_url(url, family)?;
            let path = scratch.path().join(asset_name);
            let mut file = std::fs::File::create(&path)?;
            let mut hasher = Sha256::new();
            let mut sink = HashingWriter { file: &mut file, hasher: &mut hasher };
            let headers = [("User-Agent".to_owned(), "slicenv-engine-manager/1.0".to_owned())];
            cloud::http::Curl.download(
                url,
                &headers,
                MAX_PACKAGE,
                &mut sink,
                &mut |done| reporter.emit("download", Some(done), size),
                cancel,
            )?;
            file.flush()?;
            reporter.emit("verify", None, None);
            let actual = hex(&hasher.finalize());
            ensure!(
                actual.eq_ignore_ascii_case(sha256),
                "SHA-256 завантаженого файлу не збігається з опублікованим (очікувано {sha256}, отримано {actual})"
            );
            path
        }
        Source::Local(path) => {
            ensure!(path.is_file(), "файл пакета не знайдено: {}", path.display());
            reporter.emit("verify", None, None);
            let actual = sha256_file(path)?;
            ensure!(
                actual.eq_ignore_ascii_case(sha256),
                "SHA-256 файлу {} не збігається (очікувано {sha256}, отримано {actual})",
                path.display()
            );
            path.to_owned()
        }
    };
    ensure!(!cancel.load(Ordering::Acquire), "{}", crate::i18n::tr("Скасовано"));

    let staging = family_dir.join(format!(".{version}.staging.{}", unique()));
    std::fs::create_dir(&staging)?;
    let result = (|| -> Result<Value> {
        reporter.emit("extract", None, None);
        extract(&archive, &staging, scratch.path(), cancel)?;
        audit_tree(&staging)?;
        let executable = find_executable(&staging, family, Os::CURRENT)?;
        let relative = executable.strip_prefix(&staging)?.to_owned();
        let profiles: Vec<String> = profile_dirs(&staging)
            .iter()
            .map(|p| target.join(p.strip_prefix(&staging).unwrap()).to_string_lossy().into_owned())
            .collect();
        let final_executable = target.join(&relative);
        let manifest = json!({
            "family": family,
            "version": version,
            "asset_name": asset_name,
            "asset_url": match source {
                Source::Download { url, .. } => url.to_owned(),
                Source::Local(p) => format!("file://{}", p.display()),
            },
            "asset_id": package.asset_id,
            "sha256": sha256.to_ascii_lowercase(),
            "executable_path": final_executable,
            "profile_paths": profiles,
            "installed_at": utc_now(),
            "managed": true,
            "channel": package.channel,
            "release_tag": package.release_tag,
            "source_url": package.source_url.map(str::to_owned).or_else(|| match source {
                Source::Download { url, .. } => Some(url.to_owned()),
                Source::Local(_) => None,
            }),
        });
        std::fs::write(staging.join("manifest.json"), serde_json::to_vec_pretty(&manifest)?)?;
        std::fs::rename(&staging, &target)?;
        reporter.emit("register", None, None);
        if let Err(e) = register(&dirs.config, family, &final_executable, version) {
            std::fs::remove_dir_all(&target).ok();
            return Err(e);
        }
        Ok(manifest)
    })();
    if staging.exists() {
        std::fs::remove_dir_all(&staging).ok();
    }
    result
}

struct HashingWriter<'a> {
    file: &'a mut std::fs::File,
    hasher: &'a mut Sha256,
}

impl std::io::Write for HashingWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.file.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn unique() -> String {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{}-{nanos}", std::process::id())
}

/// ISO 8601 UTC без сторонніх крейтів.
fn utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant, civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}+00:00",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

// ---- розпакування ----

fn extract(archive: &Path, staging: &Path, scratch: &Path, cancel: &AtomicBool) -> Result<()> {
    let lower = archive.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
    if lower.ends_with(".zip") {
        extract_zip(archive, staging, cancel)
    } else if lower.ends_with(".appimage") {
        let tree = extract_appimage(archive, scratch)?;
        for entry in std::fs::read_dir(&tree)? {
            let entry = entry?;
            std::fs::rename(entry.path(), staging.join(entry.file_name()))?;
        }
        Ok(())
    } else if lower.ends_with(".tar.gz") || lower.ends_with(".tar.xz") || lower.ends_with(".tar") {
        extract_tar(archive, staging)
    } else if lower.ends_with(".dmg") {
        extract_dmg(archive, staging, scratch)
    } else {
        bail!("непідтримуваний формат пакета: {}", archive.display())
    }
}

/// Zip без виходу за теку (`enclosed_name`), без символьних посилань; `.pdb` (символи
/// налагодження Windows, сотні МБ) не розпаковуються — рушію вони не потрібні.
fn extract_zip(archive: &Path, staging: &Path, cancel: &AtomicBool) -> Result<()> {
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(std::fs::File::open(archive)?))?;
    for i in 0..zip.len() {
        ensure!(!cancel.load(Ordering::Acquire), "{}", crate::i18n::tr("Скасовано"));
        let mut entry = zip.by_index(i)?;
        let Some(relative) = entry.enclosed_name() else {
            bail!("небезпечний шлях в архіві: {}", entry.name().unwrap_or_default());
        };
        if entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
            bail!("символьне посилання в архіві: {}", relative.display());
        }
        if relative.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdb")) {
            continue;
        }
        let dest = staging.join(&relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&dest)?;
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&dest)?;
        std::io::copy(&mut entry, &mut out)?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(mode & 0o755))?;
        }
    }
    Ok(())
}

/// `unsquashfs` не запускає код з пакета; інакше — рантайм самого (вже перевіреного)
/// AppImage з `--appimage-extract`.
fn extract_appimage(archive: &Path, scratch: &Path) -> Result<PathBuf> {
    let root = scratch.join("squashfs-root");
    let archive = archive.canonicalize()?;
    let unsquashed = paths::host::command("unsquashfs")
        .arg("-d")
        .arg(&root)
        .arg(&archive)
        .current_dir(scratch)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output();
    if !unsquashed.as_ref().is_ok_and(|o| o.status.success()) || !root.is_dir() {
        if root.exists() {
            std::fs::remove_dir_all(&root).ok();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&archive, std::fs::Permissions::from_mode(0o755))?;
        }
        let run = paths::host::command(&archive)
            .arg("--appimage-extract")
            .current_dir(scratch)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output()
            .context("не вдалося запустити AppImage для розпакування")?;
        if !run.status.success() || !root.is_dir() {
            let detail = match &unsquashed {
                Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_owned(),
                Err(_) => "unsquashfs недоступний".into(),
            };
            bail!(
                "AppImage не розпакувався (unsquashfs: {detail}; рантайм: {})",
                String::from_utf8_lossy(&run.stderr).trim()
            );
        }
    }
    audit_tree(&root)?;
    Ok(root)
}

/// Системний `tar` (GNU tar на Linux, bsdtar на Windows/macOS); шляхи перевіряються до
/// розпакування, посилання — після.
fn extract_tar(archive: &Path, staging: &Path) -> Result<()> {
    let listing = paths::host::command("tar").arg("-tf").arg(archive).output()?;
    ensure!(listing.status.success(), "архів не читається");
    for name in String::from_utf8_lossy(&listing.stdout).lines() {
        let path = Path::new(name);
        ensure!(
            !path.is_absolute()
                && !name.starts_with('/')
                && !path.components().any(|c| matches!(c, Component::ParentDir | Component::Prefix(_))),
            "небезпечний шлях в архіві: {name}"
        );
    }
    let status = paths::host::command("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(staging)
        .arg("--no-same-owner")
        .status()?;
    ensure!(status.success(), "архів не розпакувався");
    Ok(())
}

/// macOS: образ монтується лише для читання в тимчасову теку, `.app` копіюється `ditto`.
/// Не перевірено на справжньому Mac.
fn extract_dmg(archive: &Path, staging: &Path, scratch: &Path) -> Result<()> {
    ensure!(cfg!(target_os = "macos"), "образ .dmg відкривається лише на macOS");
    let mount = scratch.join("mnt");
    std::fs::create_dir_all(&mount)?;
    let attached = paths::host::command("hdiutil")
        .args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint"])
        .arg(&mount)
        .arg(archive)
        .stdout(std::process::Stdio::null())
        .status()?;
    ensure!(attached.success(), "hdiutil не підключив образ");
    let copied = (|| -> Result<()> {
        let app = std::fs::read_dir(&mount)?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "app") && !p.is_symlink())
            .context("в образі немає .app")?;
        let status = paths::host::command("ditto")
            .arg(&app)
            .arg(staging.join(app.file_name().unwrap()))
            .status()?;
        ensure!(status.success(), "ditto не скопіював застосунок");
        Ok(())
    })();
    paths::host::command("hdiutil").arg("detach").arg(&mount).arg("-quiet").status().ok();
    copied
}

/// Посилання лише всередину теки, жодних пристроїв і FIFO.
fn audit_tree(root: &Path) -> Result<()> {
    let base = root.canonicalize()?;
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                let link = std::fs::read_link(&path)?;
                let resolved = if link.is_absolute() { link } else { path.parent().unwrap().join(link) };
                let inside = match resolved.canonicalize() {
                    Ok(real) => real.starts_with(&base),
                    Err(_) => normalize(&resolved).starts_with(normalize(root)),
                };
                ensure!(inside, "посилання {} веде за межі пакета", path.strip_prefix(root).unwrap_or(&path).display());
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::FileTypeExt as _;
                ensure!(
                    !(kind.is_fifo() || kind.is_char_device() || kind.is_block_device() || kind.is_socket()),
                    "у пакеті є файл пристрою: {}",
                    path.display()
                );
            }
            if kind.is_dir() {
                stack.push(path);
            }
        }
    }
    Ok(())
}

fn executable_names(family: &str, os: Os) -> &'static [&'static str] {
    match (family, os) {
        ("orca", Os::Windows) => &["orca-slicer.exe", "OrcaSlicer.exe"],
        ("bambu", Os::Windows) => &["bambu-studio.exe", "BambuStudio.exe"],
        ("orca", _) => &["orca-slicer", "OrcaSlicer"],
        _ => &["bambu-studio", "BambuStudio"],
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_: &Path) -> Result<()> {
    Ok(())
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

pub(crate) fn find_executable(root: &Path, family: &str, os: Os) -> Result<PathBuf> {
    let names = executable_names(family, os);
    let regular = |p: &Path| p.symlink_metadata().is_ok_and(|m| m.is_file());
    match os {
        Os::Windows => {
            // Portable zip: exe у корені або в одній підтеці.
            let mut dirs = vec![root.to_owned()];
            dirs.extend(std::fs::read_dir(root)?.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
            for dir in dirs {
                if let Some(found) = names.iter().map(|n| dir.join(n)).find(|p| regular(p)) {
                    return Ok(found);
                }
            }
        }
        Os::Mac => {
            for app in std::fs::read_dir(root)?.flatten().map(|e| e.path()) {
                let macos = app.join("Contents/MacOS");
                if let Some(found) = names.iter().map(|n| macos.join(n)).find(|p| regular(p)) {
                    return Ok(found);
                }
            }
        }
        Os::Linux => {
            let apprun = root.join("AppRun");
            if regular(&apprun) {
                make_executable(&apprun)?;
                return Ok(apprun);
            }
            let candidates = names
                .iter()
                .map(|n| root.join("bin").join(n))
                .chain(names.iter().map(|n| root.join(n)));
            for candidate in candidates {
                if regular(&candidate) {
                    make_executable(&candidate)?;
                    return Ok(candidate);
                }
            }
            let mut stack = vec![root.to_owned()];
            while let Some(dir) = stack.pop() {
                for path in std::fs::read_dir(&dir)?.flatten().map(|e| e.path()) {
                    if path.is_symlink() {
                        continue;
                    }
                    if path.is_dir() {
                        stack.push(path);
                    } else if is_executable(&path) {
                        let name = path.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
                        if name.contains("slicer") || name.contains("studio") {
                            return Ok(path);
                        }
                    }
                }
            }
        }
    }
    bail!("у пакеті {family} не знайдено виконуваного файлу")
}

fn profile_dirs(root: &Path) -> Vec<PathBuf> {
    // По складовій: `join("resources/profiles")` дав би на Windows змішані скісні.
    let mut found: Vec<PathBuf> = [&["resources", "profiles"][..], &["resources", "printers"], &["profiles"]]
        .iter()
        .map(|parts| parts.iter().fold(root.to_owned(), |p, part| p.join(part)))
        .filter(|p| p.is_dir())
        .collect();
    // macOS: `<App>.app/Contents/Resources/profiles`.
    if let Ok(entries) = std::fs::read_dir(root) {
        for app in entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "app")) {
            let profiles = app.join("Contents").join("Resources").join("profiles");
            if profiles.is_dir() {
                found.push(profiles);
            }
        }
    }
    found
}

// ---- engines.json ----

fn read_config(path: &Path) -> Result<Value> {
    if !path.is_file() {
        return Ok(json!({}));
    }
    let value: Value = serde_json::from_slice(&std::fs::read(path)?)
        .with_context(|| format!("пошкоджений список рушіїв: {}", path.display()))?;
    ensure!(value.is_object(), "engines.json має містити об’єкт: {}", path.display());
    Ok(value)
}

fn write_config(path: &Path, config: &Value) -> Result<()> {
    let parent = path.parent().context("шлях engines.json без теки")?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut file, config)?;
    file.write_all(b"\n")?;
    file.persist(path)?;
    Ok(())
}

/// Додає версію в `installed`; активний рушій не змінюється.
fn register(config_path: &Path, family: &str, executable: &Path, version: &str) -> Result<()> {
    let mut config = read_config(config_path)?;
    let path = executable.to_string_lossy();
    let mut installed: Vec<Value> = config["installed"].as_array().cloned().unwrap_or_default();
    installed.retain(|e| {
        e["path"].as_str().is_some_and(|p| Path::new(p).is_file())
            && !(e["kind"] == family && e["path"].as_str() == Some(&path))
    });
    installed.push(json!({"kind": family, "path": path, "version": version}));
    config["installed"] = installed.into();
    write_config(config_path, &config)
}

/// Прибирає версію зі списку; повертає, чи була вона активною (тоді активного рушія немає).
fn unregister(config_path: &Path, family: &str, executable: &Path) -> Result<bool> {
    if !config_path.is_file() {
        return Ok(false);
    }
    let mut config = read_config(config_path)?;
    let path = executable.to_string_lossy();
    if let Some(installed) = config["installed"].as_array_mut() {
        installed.retain(|e| {
            e["path"].as_str().is_some_and(|p| Path::new(p).is_file())
                && !(e["kind"] == family && e["path"].as_str() == Some(&path))
        });
    }
    let was_active = config["active"]["kind"] == family && config["active"]["path"].as_str() == Some(&path);
    if was_active {
        config.as_object_mut().unwrap().remove("active");
    }
    write_config(config_path, &config)?;
    Ok(was_active)
}

// ---- список і видалення ----

fn validate_managed(target: &Path, family: &str, version: &str) -> Result<Value> {
    reject_symlinks(target)?;
    let data: Value = serde_json::from_slice(&std::fs::read(target.join("manifest.json"))?)
        .context("пошкоджений manifest.json")?;
    ensure!(
        data["managed"] == true && data["family"] == family && data["version"] == version,
        "тека не є керованою версією {family} {version}"
    );
    let executable = PathBuf::from(data["executable_path"].as_str().context("у маніфесті немає виконуваного файлу")?);
    ensure!(!executable.is_symlink(), "виконуваний файл — посилання");
    let inside = executable
        .canonicalize()
        .ok()
        .zip(target.canonicalize().ok())
        .is_some_and(|(exe, dir)| exe.starts_with(dir));
    ensure!(inside, "виконуваний файл поза текою версії");
    ensure!(is_executable(&executable), "виконуваного файлу немає або він не запускається");
    Ok(data)
}

/// Тека даних колись звалася інакше (bambu-lab → slicenv): шляхи в маніфесті переносяться
/// на поточне розташування, якщо відносна будова та сама.
fn relocate(entry: &Path, family: &str, data: Value) -> Value {
    let raw = data["executable_path"].as_str().unwrap_or("");
    if raw.is_empty() || Path::new(raw).is_file() {
        return data;
    }
    let version = entry.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let marker = format!("/{family}/{version}/");
    let rebase = |raw: &str| -> String {
        let unified = raw.replace('\\', "/");
        match unified.split_once(&marker) {
            Some((_, rest)) => {
                let candidate = entry.join(rest);
                if candidate.exists() && !candidate.is_symlink() {
                    candidate.to_string_lossy().into_owned()
                } else {
                    raw.to_owned()
                }
            }
            None => raw.to_owned(),
        }
    };
    let healed_exec = rebase(raw);
    if healed_exec == raw {
        return data;
    }
    let mut healed = data.clone();
    healed["executable_path"] = healed_exec.into();
    if let Some(paths) = data["profile_paths"].as_array() {
        healed["profile_paths"] = paths.iter().map(|p| Value::from(rebase(p.as_str().unwrap_or("")))).collect();
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(&healed) {
        let temp = entry.join(format!(".manifest.json.tmp.{}", std::process::id()));
        if std::fs::write(&temp, bytes).is_ok() {
            std::fs::rename(&temp, entry.join("manifest.json")).ok();
        }
    }
    healed
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![path.to_owned()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(entry.path()),
                Ok(t) if t.is_file() => total += entry.metadata().map(|m| m.len()).unwrap_or(0),
                _ => {}
            }
        }
    }
    total
}

pub(crate) fn list(dirs: &Dirs) -> Result<Vec<Value>> {
    reject_symlinks(&dirs.engines)?;
    let mut out = Vec::new();
    for (family, _) in REPOS {
        let family_dir = dirs.engines.join(family);
        if family_dir.is_symlink() || !family_dir.is_dir() {
            continue;
        }
        let mut entries: Vec<_> = std::fs::read_dir(&family_dir)?.flatten().map(|e| e.path()).collect();
        entries.sort();
        for entry in entries {
            let name = entry.file_name().unwrap_or_default().to_string_lossy().into_owned();
            if name.starts_with('.') || !entry.is_dir() {
                continue;
            }
            let Ok(bytes) = std::fs::read(entry.join("manifest.json")) else { continue };
            let Ok(data) = serde_json::from_slice::<Value>(&bytes) else { continue };
            if data["managed"] != true {
                continue;
            }
            let data = relocate(&entry, family, data);
            let valid = validate_managed(&entry, family, &name).is_ok();
            out.push(json!({
                "family": family,
                "version": data["version"].as_str().unwrap_or(&name),
                "valid": valid,
                "executable_path": data["executable_path"].as_str().unwrap_or(""),
                "profile_paths": data.get("profile_paths").cloned().unwrap_or_else(|| json!([])),
                "sha256": data["sha256"].as_str().unwrap_or(""),
                "asset_id": data["asset_id"],
                "channel": data["channel"],
                "release_tag": data["release_tag"],
                "source_url": data["source_url"],
                "installed_at": data["installed_at"].as_str().unwrap_or(""),
                "size_bytes": dir_size(&entry),
                "directory": entry,
            }));
        }
    }
    Ok(out)
}

/// Виконувані файли встановлених менеджером версій — лише з маніфестів, без обходу тек
/// (для списку рушіїв, який перечитується щокадру).
pub(crate) fn managed_executables(dirs: &Dirs) -> Vec<(String, PathBuf)> {
    slicer::Engine::managed(&dirs.engines)
        .into_iter()
        .map(|e| (e.kind.name().to_owned(), e.path))
        .collect()
}

/// Версія з маніфесту менеджера поруч із виконуваним файлом (корінь теки версії — до
/// чотирьох рівнів угору: `bin/`, `<App>.app/Contents/MacOS/`).
pub(crate) fn manifest_version(executable: &Path) -> Option<String> {
    executable.ancestors().skip(1).take(4).find_map(|dir| {
        let data: Value = serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).ok()?).ok()?;
        (data["managed"] == true).then(|| data["version"].as_str().map(str::to_owned)).flatten()
    })
}

pub(crate) fn remove(dirs: &Dirs, family: &str, version: &str) -> Result<Value> {
    repo(family)?;
    ensure!(valid_version(version), "некоректна версія «{version}»");
    let family_dir = dirs.engines.join(family);
    reject_symlinks(&family_dir)?;
    let real_family = family_dir.canonicalize().context("версію не знайдено")?;
    ensure!(
        real_family.parent() == Some(dirs.engines.canonicalize()?.as_path()),
        "тека родини рушія вказує за межі сховища"
    );
    let target = family_dir.join(version);
    reject_symlinks(&target)?;
    ensure!(target.is_dir(), "{}", crate::tr_format!("{} {} не встановлено", family, version));
    ensure!(
        target.join("manifest.json").is_file(),
        "тека {} не створена менеджером — не видаляю",
        target.display()
    );
    let manifest = validate_managed(&target, family, version)?;
    let executable = PathBuf::from(manifest["executable_path"].as_str().unwrap_or_default());
    let config = read_config(&dirs.config)?;
    if let Some(active) = config["active"]["path"].as_str() {
        let active = Path::new(active);
        let active = active.canonicalize().unwrap_or_else(|_| normalize(active));
        ensure!(
            !active.starts_with(target.canonicalize()?),
            "{}",
            crate::i18n::tr("Це активний рушій — спершу виберіть інший")
        );
    }
    let trash = family_dir.join(format!(".{version}.trash.{}", unique()));
    std::fs::rename(&target, &trash)?;
    let was_active = match unregister(&dirs.config, family, &executable) {
        Ok(v) => v,
        Err(e) => {
            std::fs::rename(&trash, &target).ok();
            return Err(e);
        }
    };
    std::fs::remove_dir_all(&trash)?;
    Ok(json!({
        "family": family,
        "version": version,
        "removed_directory": target,
        "was_active": was_active,
    }))
}

#[cfg(test)]
mod tests;
