//! Профілі принтерів для рушія без JSON-каталогу: OrcaSlicer ≥ 2.5 постачає системні профілі
//! лише як бінарний кеш `<vendor>.opc` (cereal, `PresetCacheFormat.hpp`), який Slicenv не
//! читає. Ті самі JSON беремо з офіційного git-тегу випуску: дерево — GitHub API, файли —
//! raw.githubusercontent.com одним процесом `curl --parallel`, кожен перевіряється за git-SHA1
//! з дерева. Каталог з’являється атомарно (`<library>/<family>/<version>`), як у
//! `manage_engines.py install_profiles`, тож `PresetStore` знаходить його без змін.
//! Тека рушія користувача не змінюється.
use anyhow::{Context as _, Result, bail, ensure};
use cloud::http::{Request, Transport as _};
use serde_json::Value;
use sha1::Digest as _;
use std::{
    io::{BufRead as _, BufReader, Write as _},
    path::Path,
    process::Stdio,
    sync::atomic::{AtomicBool, Ordering},
};

/// Один файл профілю з git-дерева.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Blob {
    /// Шлях у репозиторії (`resources/profiles/BBL/machine/….json`).
    pub path: String,
    /// Шлях у каталозі (`BBL/machine/….json`).
    pub relative: String,
    pub sha: String,
}

fn repo(kind: slicer::EngineKind) -> &'static str {
    match kind {
        slicer::EngineKind::Orca => "OrcaSlicer/OrcaSlicer",
        slicer::EngineKind::Bambu => "bambulab/BambuStudio",
    }
}

/// Версія — лише цифри й крапки (з можливим суфіксом `-beta`): вона стає тегом і текою.
fn valid_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 40
        && version.chars().next().is_some_and(|c| c.is_ascii_digit())
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

fn valid_vendor(vendor: &str) -> bool {
    !vendor.is_empty()
        && vendor.len() <= 64
        && !vendor.starts_with('.')
        && vendor
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
}

/// Файли вибраних виробників із відповіді `git/trees/<tag>?recursive=1`.
pub(crate) fn blobs(tree: &Value, vendors: &[String]) -> Result<Vec<Blob>> {
    ensure!(
        tree["truncated"] != true,
        "GitHub повернув неповне дерево профілів"
    );
    let mut out = Vec::new();
    for entry in tree["tree"].as_array().context("немає дерева профілів")? {
        let Some(path) = entry["path"].as_str() else {
            continue;
        };
        let Some(relative) = path.strip_prefix("resources/profiles/") else {
            continue;
        };
        if entry["type"] != "blob" || !relative.ends_with(".json") {
            continue;
        }
        if !vendors
            .iter()
            .any(|v| relative == format!("{v}.json") || relative.starts_with(&format!("{v}/")))
        {
            continue;
        }
        ensure!(
            entry["mode"] != "120000"
                && !relative.contains('\\')
                && relative
                    .split('/')
                    .all(|p| !p.is_empty() && p != "." && p != ".."),
            "небезпечний шлях у дереві профілів"
        );
        let sha = entry["sha"].as_str().context("немає sha у дереві")?;
        ensure!(
            sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()),
            "некоректний sha у дереві"
        );
        out.push(Blob {
            path: path.to_owned(),
            relative: relative.to_owned(),
            sha: sha.to_ascii_lowercase(),
        });
    }
    for vendor in vendors {
        ensure!(
            out.iter().any(|b| b.relative == format!("{vendor}.json")),
            "у випуску немає профілів {vendor}"
        );
    }
    Ok(out)
}

/// Git-SHA1 вмісту файлу, як у дереві: `sha1("blob <len>\0" + data)`.
pub(crate) fn git_sha1(data: &[u8]) -> String {
    let mut hasher = sha1::Sha1::new();
    hasher.update(format!("blob {}\0", data.len()).as_bytes());
    hasher.update(data);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Відсоткове кодування сегментів шляху для URL (пробіли, `+`, `@` у назвах філаментів).
pub(crate) fn encode_path(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            segment
                .bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                        (b as char).to_string()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Рядок конфігу curl у лапках (зворотні скісні й лапки екрануються).
fn quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Конфіг `curl -K -`: пари url/output для кожного файлу.
pub(crate) fn curl_config(repo: &str, tag: &str, blobs: &[Blob], stage: &Path) -> String {
    let mut cfg = String::new();
    for blob in blobs {
        let url = format!(
            "https://raw.githubusercontent.com/{repo}/{}/{}",
            encode_path(tag),
            encode_path(&blob.path)
        );
        // Прямі скісні працюють і на Windows, і їх не треба екранувати.
        let output = stage.join(&blob.relative).to_string_lossy().replace('\\', "/");
        cfg += &format!("url = {}\noutput = {}\n", quoted(&url), quoted(&output));
    }
    cfg
}

pub(crate) struct Fetched {
    pub version: String,
    pub files: usize,
}

/// Завантажує JSON-профілі `vendors` з тегу `v<version>` у бібліотеку профілів Slicenv.
pub(crate) fn fetch(
    kind: slicer::EngineKind,
    version: &str,
    vendors: &[String],
    progress: &mut dyn FnMut(usize, usize),
    cancel: &AtomicBool,
) -> Result<Fetched> {
    ensure!(valid_version(version), "некоректна версія рушія «{version}»");
    let mut vendors: Vec<String> = vendors.to_vec();
    vendors.dedup();
    ensure!(
        vendors.iter().all(|v| valid_vendor(v)),
        "некоректна назва виробника"
    );
    let repo = repo(kind);
    let tag = format!("v{version}");
    let headers = [
        ("Accept".to_owned(), "application/vnd.github+json".to_owned()),
        ("User-Agent".to_owned(), "Slicenv".to_owned()),
    ];
    let url = format!(
        "https://api.github.com/repos/{repo}/git/trees/{}?recursive=1",
        encode_path(&tag)
    );
    let response = cloud::http::Curl.request(&Request {
        timeout_secs: 60,
        max_bytes: 32 * 1024 * 1024,
        ..Request::get(&url, &headers)
    })?;
    match response.status {
        200 => {}
        404 | 422 => bail!("у репозиторії {repo} немає випуску {tag}"),
        403 | 429 => bail!("GitHub тимчасово обмежив запити — спробуйте за кілька хвилин"),
        code => bail!("GitHub відповів {code}"),
    }
    let tree: Value = serde_json::from_slice(&response.body).context("відповідь GitHub не JSON")?;
    let blobs = blobs(&tree, &vendors)?;

    let root = slicer::PresetStore::library_root().join(kind.name());
    std::fs::create_dir_all(&root)?;
    let scratch = tempfile::Builder::new().prefix(".profiles-").tempdir_in(&root)?;
    let stage = scratch.path().join("catalog");
    std::fs::create_dir_all(&stage)?;
    download(repo, &tag, &blobs, &stage, progress, cancel)?;

    for blob in &blobs {
        let data = std::fs::read(stage.join(&blob.relative))
            .with_context(|| format!("не завантажено {}", blob.relative))?;
        ensure!(
            git_sha1(&data) == blob.sha,
            "файл {} змінився під час завантаження",
            blob.relative
        );
        serde_json::from_slice::<Value>(&data)
            .with_context(|| format!("{} не JSON", blob.relative))?;
    }
    for vendor in &vendors {
        for dir in ["machine", "process", "filament"] {
            std::fs::create_dir_all(stage.join(vendor).join(dir))?;
        }
    }
    let manifest = serde_json::json!({
        "family": kind.name(),
        "version": version,
        "source": "git-tag",
        "source_ref": tag,
        "fetched_at": chrono_now(),
        "vendors": vendors,
    });
    std::fs::write(
        stage.join("catalog.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    publish(&stage, &root.join(version))?;
    Ok(Fetched {
        version: version.to_owned(),
        files: blobs.len(),
    })
}

/// Секунди Unix — без залежності від chrono; поле лише інформаційне.
fn chrono_now() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

/// Заміна каталогу без проміжного стану: стара версія стає резервною, доки нова не на місці.
fn publish(stage: &Path, target: &Path) -> Result<()> {
    let backup = target.with_file_name(format!(
        ".backup-{}-{}",
        std::process::id(),
        chrono_now()
    ));
    if target.exists() {
        std::fs::rename(target, &backup)?;
    }
    if let Err(e) = std::fs::rename(stage, target) {
        if backup.exists() {
            std::fs::rename(&backup, target).ok();
        }
        return Err(e.into());
    }
    if backup.exists() {
        std::fs::remove_dir_all(&backup).ok();
    }
    Ok(())
}

fn download(
    repo: &str,
    tag: &str,
    blobs: &[Blob],
    stage: &Path,
    progress: &mut dyn FnMut(usize, usize),
    cancel: &AtomicBool,
) -> Result<()> {
    let total = blobs.len();
    progress(0, total);
    let mut child = paths::host::command("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--connect-timeout",
            "15",
            "--max-time",
            "120",
            "--retry",
            "2",
            "--create-dirs",
            "--parallel",
            "--parallel-max",
            "16",
            "--write-out",
            "%{http_code}\\n",
            "-K",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("не вдалося запустити curl")?;
    let cfg = curl_config(repo, tag, blobs, stage);
    let mut stdin = child.stdin.take().context("curl без stdin")?;
    // Конфіг великий (тисячі рядків): пишемо з окремого потоку, щоб не заблокуватися
    // на заповненому stdout.
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(cfg.as_bytes());
    });
    let stdout = child.stdout.take().context("curl без stdout")?;
    let mut done = 0;
    for line in BufReader::new(stdout).lines() {
        let Ok(_) = line else { break };
        done += 1;
        progress(done.min(total), total);
        if cancel.load(Ordering::Acquire) {
            child.kill().ok();
            break;
        }
    }
    writer.join().ok();
    let status = child.wait()?;
    ensure!(!cancel.load(Ordering::Acquire), "завантаження профілів скасовано");
    if !status.success() {
        bail!(
            "не всі файли профілів завантажилися (код curl {:?})",
            status.code()
        );
    }
    Ok(())
}

/// Тека профілів виробника, яку слід завантажити разом із BBL (для вибраного принтера).
pub(crate) fn vendors_for(extra: Option<String>) -> Vec<String> {
    let mut vendors = vec!["BBL".to_owned()];
    if let Some(v) = extra.filter(|v| v != "BBL" && valid_vendor(v)) {
        vendors.push(v);
    }
    vendors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> Value {
        serde_json::json!({
            "truncated": false,
            "tree": [
                {"path": "resources/profiles/BBL.json", "type": "blob", "mode": "100644", "sha": "a".repeat(40)},
                {"path": "resources/profiles/BBL/machine/Bambu Lab A1 0.4 nozzle.json", "type": "blob", "mode": "100644", "sha": "b".repeat(40)},
                {"path": "resources/profiles/BBL/machine", "type": "tree", "mode": "040000", "sha": "c".repeat(40)},
                {"path": "resources/profiles/BBL/cover.png", "type": "blob", "mode": "100644", "sha": "d".repeat(40)},
                {"path": "resources/profiles/Creality.json", "type": "blob", "mode": "100644", "sha": "e".repeat(40)},
                {"path": "src/main.cpp", "type": "blob", "mode": "100644", "sha": "f".repeat(40)},
            ]
        })
    }

    #[test]
    fn only_json_of_requested_vendors_is_taken() {
        let found = blobs(&tree(), &["BBL".into()]).unwrap();
        let names: Vec<_> = found.iter().map(|b| b.relative.as_str()).collect();
        assert_eq!(names, ["BBL.json", "BBL/machine/Bambu Lab A1 0.4 nozzle.json"]);
        assert!(blobs(&tree(), &["Anker".into()]).is_err(), "немає Anker.json");
    }

    #[test]
    fn unsafe_or_truncated_trees_are_rejected() {
        let mut t = tree();
        t["truncated"] = true.into();
        assert!(blobs(&t, &["BBL".into()]).is_err());
        let mut t = tree();
        t["tree"][1]["path"] = "resources/profiles/BBL/../../evil.json".into();
        assert!(blobs(&t, &["BBL".into()]).is_err());
        let mut t = tree();
        t["tree"][1]["mode"] = "120000".into();
        assert!(blobs(&t, &["BBL".into()]).is_err());
    }

    #[test]
    fn git_sha1_matches_git_hash_object() {
        // `printf 'hello\n' | git hash-object --stdin`
        assert_eq!(git_sha1(b"hello\n"), "ce013625030ba8dba906f756967f9e9ca394464a");
    }

    #[test]
    fn curl_config_encodes_urls_and_quotes_paths() {
        let blob = Blob {
            path: "resources/profiles/BBL/filament/Bambu PLA+ @BBL A1.json".into(),
            relative: "BBL/filament/Bambu PLA+ @BBL A1.json".into(),
            sha: "0".repeat(40),
        };
        let cfg = curl_config("OrcaSlicer/OrcaSlicer", "v2.5.0", &[blob], Path::new("C:\\data\\stage"));
        assert!(cfg.contains("url = \"https://raw.githubusercontent.com/OrcaSlicer/OrcaSlicer/v2.5.0/resources/profiles/BBL/filament/Bambu%20PLA%2B%20%40BBL%20A1.json\""));
        assert!(cfg.contains("/BBL/filament/Bambu PLA+ @BBL A1.json\""));
        assert!(!cfg.contains('\\'), "без зворотних скісних");
    }

    #[test]
    fn versions_and_vendors_are_validated() {
        assert!(valid_version("2.5.0"));
        assert!(valid_version("02.03.00.62"));
        assert!(!valid_version("OrcaSlicer-2.5.0"));
        assert!(!valid_version("../x"));
        assert_eq!(vendors_for(Some("BBL".into())), ["BBL"]);
        assert_eq!(vendors_for(Some("../x".into())), ["BBL"]);
        assert_eq!(vendors_for(Some("Creality".into())), ["BBL", "Creality"]);
    }

    /// Справжня мережа: `cargo test -p app -- --ignored real_orca_tag` у тимчасовому HOME.
    #[test]
    #[ignore]
    fn real_orca_tag_downloads_a_verified_bbl_catalog() {
        let cancel = AtomicBool::new(false);
        let mut last = (0, 0);
        let fetched = fetch(
            slicer::EngineKind::Orca,
            "2.5.0",
            &vendors_for(None),
            &mut |d, t| last = (d, t),
            &cancel,
        )
        .unwrap();
        assert!(fetched.files > 1000, "{}", fetched.files);
        assert_eq!(last.0, last.1);
        let root = slicer::PresetStore::library_root().join("orca").join("2.5.0");
        assert!(root.join("BBL.json").is_file());
        assert!(root.join("catalog.json").is_file());
    }
}
