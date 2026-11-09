//! Регресійні тести менеджера рушіїв (порт `test_manage_engines.py`): локальні пакети без
//! мережі, справжній формат AppImage-розпакування через фальшивий рантайм.
use super::*;
use zip::write::SimpleFileOptions;

struct Sandbox {
    _dir: tempfile::TempDir,
    dirs: Dirs,
    root: PathBuf,
}

fn sandbox() -> Sandbox {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let dirs = Dirs { engines: root.join("data/engines"), config: root.join("config/engines.json") };
    Sandbox { _dir: dir, dirs, root }
}

fn zip_package(path: &Path, files: &[(&str, &[u8], u32)]) {
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    for (name, data, mode) in files {
        zip.start_file(*name, SimpleFileOptions::default().unix_permissions(*mode)).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
}

fn package<'a>(family: &'a str, version: &'a str, asset: &'a str, sha: &'a str) -> Package<'a> {
    Package {
        family,
        version,
        asset_name: asset,
        sha256: sha,
        asset_id: Some(7),
        channel: Some("stable"),
        release_tag: Some("v2.4.2"),
        source_url: None,
    }
}

fn install_local(s: &Sandbox, family: &str, version: &str, file: &Path) -> Result<Value> {
    let sha = sha256_file(file).unwrap();
    let name = file.file_name().unwrap().to_string_lossy().into_owned();
    let mut events = |_: Value| {};
    let mut reporter = Reporter::new(&mut events);
    install(
        &s.dirs,
        &package(family, version, &name, &sha),
        Source::Local(file),
        &mut reporter,
        &AtomicBool::new(false),
    )
}

/// Лише нормальні елементи в теці родини: жодних `.staging`, `.download-`, `.trash`.
fn leftovers(s: &Sandbox, family: &str) -> Vec<String> {
    std::fs::read_dir(s.dirs.engines.join(family))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('.'))
        .collect()
}

fn linux_zip(s: &Sandbox) -> PathBuf {
    let file = s.root.join("OrcaSlicer_Linux_V2.4.2.zip");
    zip_package(
        &file,
        &[
            ("bin/orca-slicer", b"#!/bin/sh\necho OrcaSlicer 2.4.2\n", 0o755),
            ("resources/profiles/BBL.json", b"{}", 0o644),
            ("resources/i18n/uk/OrcaSlicer.mo", b"", 0o644),
            ("OrcaSlicer.pdb", b"debug symbols", 0o644),
        ],
    );
    file
}

#[test]
fn assets_are_picked_per_platform_from_real_release_names() {
    let names = [
        "OrcaSlicer_Linux_AppImage_Ubuntu2404_V2.4.2.AppImage",
        "OrcaSlicer_Mac_universal_V2.5.0.dmg",
        "OrcaSlicer_Windows_Installer_V2.5.0_x64.exe",
        "OrcaSlicer_Windows_V2.5.0_arm64_portable.zip",
        "OrcaSlicer_Windows_V2.5.0_x64_portable.zip",
        "OrcaSlicer_Windows_x64_nightly_portable.zip",
        "OrcaSlicer_profile_validator_Windows_nightly.exe",
        "OrcaSlicer_profile_validator_Mac_universal_nightly.dmg",
        "orca_custom_preset_tests.zip",
        "Bambu_Studio_win-v02.09.01.70-20261103183404.zip",
        "Bambu_Studio_win-v02.09.01.70-20261103183404.exe",
        "Bambu_Studio_mac-v02.09.01.70-20261103183404.dmg",
        "Bambu_Studio_linux_ubuntu_24.04_v02.09.01.70-20261103183404.AppImage",
        "BambuStudio_linux_fedora-v02.09.01.70.AppImage",
        "OrcaSlicer-Linux-aarch64.AppImage",
        "OrcaSlicer.flatpak",
    ];
    let pick = |os| names.iter().filter(|n| asset_matches(os, Arch::X64, n)).copied().collect::<Vec<_>>();
    assert_eq!(
        pick(Os::Windows),
        [
            "OrcaSlicer_Windows_V2.5.0_x64_portable.zip",
            "OrcaSlicer_Windows_x64_nightly_portable.zip",
            "Bambu_Studio_win-v02.09.01.70-20261103183404.zip",
        ]
    );
    // Windows на ARM: лише arm64-збірка (у Bambu такої немає).
    let arm: Vec<_> = names.iter().filter(|n| asset_matches(Os::Windows, Arch::Arm64, n)).copied().collect();
    assert_eq!(arm, ["OrcaSlicer_Windows_V2.5.0_arm64_portable.zip"]);
    let arm_linux: Vec<_> = names.iter().filter(|n| asset_matches(Os::Linux, Arch::Arm64, n)).copied().collect();
    assert_eq!(arm_linux, ["OrcaSlicer-Linux-aarch64.AppImage"]);
    assert_eq!(asset_arch("OrcaSlicer_Windows_V2.5.0_x64_portable.zip"), Some(Arch::X64));
    assert_eq!(asset_arch("Bambu_Studio_win-v02.09.01.70.zip"), None);
    assert_eq!(
        pick(Os::Mac),
        ["OrcaSlicer_Mac_universal_V2.5.0.dmg", "Bambu_Studio_mac-v02.09.01.70-20261103183404.dmg"]
    );
    assert_eq!(
        pick(Os::Linux),
        [
            "OrcaSlicer_Linux_AppImage_Ubuntu2404_V2.4.2.AppImage",
            "Bambu_Studio_linux_ubuntu_24.04_v02.09.01.70-20261103183404.AppImage",
            "BambuStudio_linux_fedora-v02.09.01.70.AppImage",
        ]
    );
}

#[test]
fn release_channels_match_the_python_manager() {
    assert_eq!(classify_channel("v2.4.2", "OrcaSlicer 2.4.2", false), "stable");
    assert_eq!(classify_channel("v2.5.0-beta", "", false), "beta");
    assert_eq!(classify_channel("v2.5.0-rc", "", false), "beta");
    assert_eq!(classify_channel("v2.5.0-rc2", "", false), "beta");
    assert_eq!(classify_channel("v02.08.04.57", "Bambu Studio", true), "beta");
    assert_eq!(classify_channel("nightly-builds", "", true), "nightly");
    assert_eq!(classify_channel("v1.0", "Daily build", false), "nightly");
    // «rc» усередині слова — не позначка кандидата.
    assert_eq!(classify_channel("v2.4.2", "OrcaSlicer source", false), "stable");
    assert_eq!(clean_tag("v2.4.2"), "2.4.2");
    assert_eq!(clean_tag("02.09.01.70"), "02.09.01.70");
}

#[test]
fn release_listing_keeps_official_metadata_and_digests() {
    let data = vec![
        json!({"tag_name":"nightly-builds","name":"","prerelease":true,"id":1,"assets":[
            {"id":11,"name":"OrcaSlicer_Windows_x64_nightly_portable.zip","size":5,"browser_download_url":"https://github.com/OrcaSlicer/OrcaSlicer/releases/download/nightly-builds/a.zip","digest":"sha256:ab"}]}),
        json!({"tag_name":"v2.4.2","name":"OrcaSlicer 2.4.2","prerelease":false,"id":2,"html_url":"https://github.com/OrcaSlicer/OrcaSlicer/releases/tag/v2.4.2","assets":[
            {"id":21,"name":"OrcaSlicer_Windows_V2.4.2_x64_portable.zip","size":9,"browser_download_url":"https://github.com/OrcaSlicer/OrcaSlicer/releases/download/v2.4.2/b.zip","digest":format!("sha256:{}", "f".repeat(64))},
            {"id":22,"name":"OrcaSlicer_Windows_Installer_V2.4.2_x64.exe","size":9,"browser_download_url":"x","digest":""}]}),
        json!({"tag_name":"v2.4.1","draft":true,"assets":[]}),
    ];
    let stable = releases_from(Os::Windows, Arch::X64, "orca", &data, "stable", 10);
    assert_eq!(stable.len(), 1);
    assert_eq!(stable[0]["tag"], "v2.4.2");
    assert_eq!(stable[0]["channel"], "stable");
    assert_eq!(stable[0]["repo"], "OrcaSlicer/OrcaSlicer");
    assert_eq!(stable[0]["assets"].as_array().unwrap().len(), 1);
    assert_eq!(stable[0]["assets"][0]["sha256"], "f".repeat(64));
    assert_eq!(releases_from(Os::Windows, Arch::X64, "orca", &data, "nightly", 10)[0]["tag"], "nightly-builds");
    assert_eq!(releases_from(Os::Windows, Arch::X64, "orca", &data, "all", 1).len(), 1);
    assert!(releases_from(Os::Linux, Arch::X64, "orca", &data, "all", 10).is_empty());
}

#[test]
fn downloads_only_from_official_release_urls() {
    assert!(validate_download_url("https://github.com/OrcaSlicer/OrcaSlicer/releases/download/v2.4.2/a.zip", "orca").is_ok());
    for bad in [
        "http://github.com/OrcaSlicer/OrcaSlicer/releases/download/v2.4.2/a.zip",
        "https://github.com/evil/OrcaSlicer/releases/download/v2.4.2/a.zip",
        "https://github.com.evil.example/OrcaSlicer/OrcaSlicer/releases/download/a.zip",
        "https://github.com/bambulab/BambuStudio/releases/download/v1/a.zip",
    ] {
        assert!(validate_download_url(bad, "orca").is_err(), "{bad}");
    }
    assert!(github("https://api.github.com/repos/evil/repo/releases").is_err());
}

#[cfg(unix)]
#[test]
fn zip_install_registers_without_touching_the_active_engine() {
    let s = sandbox();
    std::fs::create_dir_all(s.dirs.config.parent().unwrap()).unwrap();
    let active = s.root.join("system/bambu-studio");
    std::fs::create_dir_all(active.parent().unwrap()).unwrap();
    std::fs::write(&active, b"").unwrap();
    std::fs::write(
        &s.dirs.config,
        serde_json::to_vec(&json!({"active":{"kind":"bambu","path":active},"installed":[{"kind":"bambu","path":active}]})).unwrap(),
    )
    .unwrap();
    let manifest = install_local(&s, "orca", "2.4.2", &linux_zip(&s)).unwrap();
    let target = s.dirs.engines.join("orca/2.4.2");
    let exe = target.join("bin/orca-slicer");
    assert_eq!(manifest["executable_path"], json!(exe));
    assert_eq!(manifest["profile_paths"], json!([target.join("resources/profiles")]));
    assert_eq!(manifest["managed"], true);
    assert!(manifest["installed_at"].as_str().unwrap().ends_with("+00:00"));
    assert!(is_executable(&exe));
    assert!(!target.join("OrcaSlicer.pdb").exists(), "символи налагодження не розпаковуються");
    let config = read_config(&s.dirs.config).unwrap();
    assert_eq!(config["active"]["path"], json!(active));
    let installed = config["installed"].as_array().unwrap();
    assert!(installed.iter().any(|e| e["path"] == json!(exe) && e["version"] == "2.4.2" && e["kind"] == "orca"));
    assert!(installed.iter().any(|e| e["path"] == json!(active)));
    assert!(leftovers(&s, "orca").is_empty());
    // Та сама версія вдруге — відмова без змін.
    let again = install_local(&s, "orca", "2.4.2", &linux_zip(&s)).unwrap_err().to_string();
    assert!(again.contains("уже встановлено"), "{again}");
    assert!(leftovers(&s, "orca").is_empty());

    let listed = list(&s.dirs).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["valid"], true);
    assert_eq!(managed_executables(&s.dirs), [("orca".to_owned(), exe.clone())]);
    assert_eq!(manifest_version(&exe).as_deref(), Some("2.4.2"));
    assert_eq!(manifest_version(&active), None);
    assert_eq!(listed[0]["asset_id"], 7);
    assert!(listed[0]["size_bytes"].as_u64().unwrap() > 0);
}

#[test]
fn hostile_names_and_bad_checksums_change_nothing() {
    let s = sandbox();
    let file = s.root.join("pkg.zip");
    zip_package(&file, &[("orca-slicer", b"x", 0o755)]);
    let sha = sha256_file(&file).unwrap();
    let mut events = |_: Value| {};
    let mut reporter = Reporter::new(&mut events);
    let cancel = AtomicBool::new(false);
    for (family, version, asset, sha) in [
        ("evil", "1.0", "pkg.zip", sha.as_str()),
        ("orca", "../1.0", "pkg.zip", sha.as_str()),
        ("orca", ".hidden", "pkg.zip", sha.as_str()),
        ("orca", "1.0", "../pkg.zip", sha.as_str()),
        ("orca", "1.0", "pkg.zip", "abc"),
        ("orca", "1.0", "pkg.zip", &"0".repeat(64)),
    ] {
        let result = install(&s.dirs, &package(family, version, asset, sha), Source::Local(&file), &mut reporter, &cancel);
        assert!(result.is_err(), "{family} {version} {asset}");
    }
    assert!(!s.dirs.engines.join("orca/1.0").exists());
    assert!(leftovers(&s, "orca").is_empty());
    assert!(!s.dirs.config.exists());
}

#[test]
fn zip_traversal_is_rejected_and_leaves_no_staging() {
    let s = sandbox();
    let file = s.root.join("evil.zip");
    zip_package(&file, &[("orca-slicer", b"x", 0o755), ("../../outside.txt", b"pwned", 0o644)]);
    let error = install_local(&s, "orca", "1.0", &file).unwrap_err().to_string();
    assert!(error.contains("небезпечний шлях"), "{error}");
    assert!(!s.root.join("outside.txt").exists() && !s.dirs.engines.join("outside.txt").exists());
    assert!(!s.dirs.engines.join("orca/1.0").exists());
    assert!(leftovers(&s, "orca").is_empty());
}

#[cfg(unix)]
#[test]
fn zip_symlinks_are_rejected() {
    let s = sandbox();
    let file = s.root.join("link.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&file).unwrap());
    zip.add_symlink("orca-slicer", "/bin/sh", SimpleFileOptions::default()).unwrap();
    zip.finish().unwrap();
    assert!(install_local(&s, "orca", "1.0", &file).unwrap_err().to_string().contains("символьне посилання"));
    assert!(leftovers(&s, "orca").is_empty());
}

/// Фальшивий рантайм AppImage: unsquashfs його не прочитає, тож менеджер запускає
/// `--appimage-extract`, як із справжнім пакетом.
#[cfg(unix)]
fn fake_appimage(s: &Sandbox, escape: bool) -> PathBuf {
    let file = s.root.join("OrcaSlicer_Linux_V2.4.2.AppImage");
    let link = if escape { "ln -s /etc squashfs-root/usr/etc-link\n" } else { "" };
    std::fs::write(
        &file,
        format!(
            "#!/bin/sh\n[ \"$1\" = --appimage-extract ] || exit 2\nmkdir -p squashfs-root/usr/bin squashfs-root/resources/profiles\n\
             printf '#!/bin/sh\\n' > squashfs-root/AppRun\nchmod 644 squashfs-root/AppRun\n\
             printf x > squashfs-root/usr/bin/orca-slicer\nprintf '{{}}' > squashfs-root/resources/profiles/BBL.json\n\
             ln -s usr/bin/orca-slicer squashfs-root/orca-slicer\n{link}"
        ),
    )
    .unwrap();
    file
}

#[cfg(unix)]
#[test]
fn appimage_is_extracted_and_apprun_becomes_the_engine() {
    let s = sandbox();
    let manifest = install_local(&s, "orca", "2.4.2", &fake_appimage(&s, false)).unwrap();
    let target = s.dirs.engines.join("orca/2.4.2");
    assert_eq!(manifest["executable_path"], json!(target.join("AppRun")));
    assert!(is_executable(&target.join("AppRun")));
    assert!(target.join("orca-slicer").is_symlink(), "посилання всередині пакета лишаються");
    assert!(leftovers(&s, "orca").is_empty());
}

#[cfg(unix)]
#[test]
fn appimage_links_out_of_the_package_are_rejected() {
    let s = sandbox();
    let error = install_local(&s, "orca", "2.4.2", &fake_appimage(&s, true)).unwrap_err().to_string();
    assert!(error.contains("за межі пакета"), "{error}");
    assert!(!s.dirs.engines.join("orca/2.4.2").exists());
    assert!(leftovers(&s, "orca").is_empty());
}

#[cfg(unix)]
#[test]
fn symlinked_storage_is_refused() {
    let s = sandbox();
    let elsewhere = s.root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::create_dir_all(&s.dirs.engines).unwrap();
    std::os::unix::fs::symlink(&elsewhere, s.dirs.engines.join("orca")).unwrap();
    let error = install_local(&s, "orca", "2.4.2", &linux_zip(&s)).unwrap_err().to_string();
    assert!(error.contains("символьне посилання"), "{error}");
    assert!(std::fs::read_dir(&elsewhere).unwrap().next().is_none());
    assert!(remove(&s.dirs, "orca", "2.4.2").is_err());
}

#[cfg(unix)]
#[test]
fn remove_unregisters_and_refuses_active_or_foreign_directories() {
    let s = sandbox();
    install_local(&s, "orca", "2.4.2", &linux_zip(&s)).unwrap();
    install_local(&s, "orca", "2.4.1", &linux_zip(&s)).unwrap();
    let exe = s.dirs.engines.join("orca/2.4.2/bin/orca-slicer");
    // Активний через альтернативний шлях (з `..`) — однаково відмова.
    let mut config = read_config(&s.dirs.config).unwrap();
    config["active"] = json!({"kind":"orca","path": s.dirs.engines.join("orca/2.4.1/../2.4.2/bin/orca-slicer")});
    write_config(&s.dirs.config, &config).unwrap();
    let error = remove(&s.dirs, "orca", "2.4.2").unwrap_err().to_string();
    assert!(error.contains("активний рушій"), "{error}");
    assert!(exe.is_file());

    let removed = remove(&s.dirs, "orca", "2.4.1").unwrap();
    assert_eq!(removed["was_active"], false);
    assert!(!s.dirs.engines.join("orca/2.4.1").exists());
    let config = read_config(&s.dirs.config).unwrap();
    assert!(!config["installed"].as_array().unwrap().iter().any(|e| e["path"].as_str().unwrap().contains("2.4.1")));
    assert!(leftovers(&s, "orca").is_empty());

    // Тека без маніфесту менеджера не видаляється.
    let foreign = s.dirs.engines.join("orca/9.9.9");
    std::fs::create_dir_all(&foreign).unwrap();
    std::fs::write(foreign.join("keep.txt"), b"x").unwrap();
    assert!(remove(&s.dirs, "orca", "9.9.9").unwrap_err().to_string().contains("не створена менеджером"));
    assert!(foreign.join("keep.txt").exists());
    assert!(remove(&s.dirs, "orca", "0.0.1").unwrap_err().to_string().contains("не встановлено"));
}

#[cfg(unix)]
#[test]
fn listing_skips_malformed_manifests_and_marks_missing_executables() {
    let s = sandbox();
    install_local(&s, "orca", "2.4.2", &linux_zip(&s)).unwrap();
    let broken = s.dirs.engines.join("orca/2.4.0");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join("manifest.json"), b"{not json").unwrap();
    let unmanaged = s.dirs.engines.join("orca/2.3.0");
    std::fs::create_dir_all(&unmanaged).unwrap();
    std::fs::write(unmanaged.join("manifest.json"), br#"{"managed":false}"#).unwrap();
    std::fs::remove_file(s.dirs.engines.join("orca/2.4.2/bin/orca-slicer")).unwrap();
    let listed = list(&s.dirs).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["version"], "2.4.2");
    assert_eq!(listed[0]["valid"], false);
}

#[cfg(unix)]
#[test]
fn moved_data_directory_heals_the_manifest() {
    let s = sandbox();
    install_local(&s, "orca", "2.4.2", &linux_zip(&s)).unwrap();
    let manifest_path = s.dirs.engines.join("orca/2.4.2/manifest.json");
    let mut data: Value = serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    data["executable_path"] = "/old/bambu-lab/engines/orca/2.4.2/bin/orca-slicer".into();
    std::fs::write(&manifest_path, serde_json::to_vec(&data).unwrap()).unwrap();
    let listed = list(&s.dirs).unwrap();
    assert_eq!(listed[0]["executable_path"], json!(s.dirs.engines.join("orca/2.4.2/bin/orca-slicer")));
    assert_eq!(listed[0]["valid"], true);
}

#[test]
fn windows_and_mac_executables_are_found_in_their_layouts() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("orca-slicer.exe"), b"").unwrap();
    assert_eq!(find_executable(root, "orca", Os::Windows).unwrap(), root.join("orca-slicer.exe"));
    let nested = root.join("Bambu Studio");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("bambu-studio.exe"), b"").unwrap();
    assert_eq!(find_executable(root, "bambu", Os::Windows).unwrap(), nested.join("bambu-studio.exe"));
    let macos = root.join("OrcaSlicer.app/Contents/MacOS");
    std::fs::create_dir_all(&macos).unwrap();
    std::fs::write(macos.join("OrcaSlicer"), b"").unwrap();
    std::fs::create_dir_all(root.join("OrcaSlicer.app/Contents/Resources/profiles")).unwrap();
    assert_eq!(find_executable(root, "orca", Os::Mac).unwrap(), macos.join("OrcaSlicer"));
    assert!(profile_dirs(root).contains(&root.join("OrcaSlicer.app/Contents/Resources/profiles")));
    assert!(find_executable(root, "bambu", Os::Mac).is_err());
}

#[test]
fn progress_events_keep_the_python_ndjson_shape() {
    let mut seen = Vec::new();
    {
        let mut events = |v: Value| seen.push(v);
        let mut reporter = Reporter::new(&mut events);
        reporter.emit("download", Some(5), Some(10));
        reporter.emit("download", Some(6), Some(10)); // приглушено (< 200 мс)
        reporter.emit("download", Some(10), Some(10)); // фінальний — завжди
        reporter.emit("download", Some(3), None);
        reporter.emit("extract", None, None);
    }
    assert_eq!(seen[0], json!({"event":"progress","stage":"download","downloaded":5,"total":10,"percent":50.0}));
    assert_eq!(seen[1]["percent"], 100.0);
    assert!(seen.iter().all(|e| e["downloaded"] != 6));
    assert!(seen.iter().any(|e| e["stage"] == "extract" && e.get("percent").is_none()));
}

#[test]
fn utc_timestamps_are_iso8601() {
    let now = utc_now();
    assert_eq!(now.len(), "2026-11-08T17:00:00+00:00".len(), "{now}");
    assert!(now.starts_with("20") && now.as_bytes()[10] == b'T', "{now}");
}

/// Живий список випусків: `cargo test -p app --bin slicenv live_releases -- --ignored`.
#[test]
#[ignore]
fn live_releases_have_assets_for_this_platform_with_digests() {
    let releases = list_releases("stable", 2).unwrap();
    assert!(releases.iter().any(|r| r["family"] == "orca"));
    for release in &releases {
        for asset in release["assets"].as_array().unwrap() {
            assert!(asset_matches(Os::CURRENT, Arch::current(), asset["name"].as_str().unwrap()));
        }
    }
    println!("{}", serde_json::to_string_pretty(&releases).unwrap());
}

/// Справжнє встановлення в тимчасові теки: `SLICENV_LIVE_INSTALL=orca:v2.4.2 cargo test -p app
/// --bin slicenv live_install -- --ignored --nocapture` (завантажує ~150 МБ).
#[test]
#[ignore]
fn live_install_into_temporary_directories() {
    let spec = std::env::var("SLICENV_LIVE_INSTALL").unwrap_or_else(|_| "orca:v2.4.2".into());
    let (family, tag) = spec.split_once(':').unwrap();
    let release = github(&format!("https://api.github.com/repos/{}/releases/tags/{tag}", repo(family).unwrap())).unwrap();
    let asset = release["assets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"].as_str().is_some_and(|n| asset_matches(Os::CURRENT, Arch::current(), n)))
        .expect("немає пакета для цієї системи");
    let s = sandbox();
    let mut events = |e: Value| {
        if e["stage"] != "download" {
            println!("{e}");
        }
    };
    let result = install_release(&s.dirs, family, tag, asset["id"].as_u64().unwrap(), &[], &mut events, &AtomicBool::new(false)).unwrap();
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
    let exe = PathBuf::from(result["executable_path"].as_str().unwrap());
    let engine = slicer::Engine {
        kind: if family == "orca" { slicer::EngineKind::Orca } else { slicer::EngineKind::Bambu },
        path: exe,
    };
    println!("probe: {:?}", engine.probe().map(|i| i.label));
    assert!(remove(&s.dirs, family, result["version"].as_str().unwrap()).is_ok());
}
