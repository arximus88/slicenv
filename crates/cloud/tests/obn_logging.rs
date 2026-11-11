//! Логи відкритого плагіна (OBN) не потрапляють у stderr і файли застосунку: модель
//! `create_agent` з тими самими правилами, що в OBN (конфіг із `log_dir`, `OBN_LOG_*`).
//! Кожен сценарій — окремий дочірній процес: агент один на процес, а stderr перехоплюється
//! лише там (у процесі тестів stderr не чіпаємо). Значення в логах вигадані.
#![cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(windows, target_arch = "x86_64", target_env = "msvc")
))]
mod native;
use cloud::{Client, Options};
use std::{path::PathBuf, process::Command};

const CHILD: &str = "SLICENV_TEST_OBN_CHILD";
const TEST: &str = "obn_logs_never_reach_stderr_or_files";
const SECRETS: [&str; 3] = ["synthetic-user@example.invalid", "999000111", "SYNTHETIC12345678"];

/// Дочірній процес: `mode` — `seeded` (наш obn.conf, без змінних), `env` (без obn.conf, лише
/// змінні), `bare` (ні того, ні іншого — контроль, що модель справді «тече»).
fn child(mode: &str) {
    let plugin = PathBuf::from(std::env::var_os("SLICENV_TEST_OBN_PLUGIN").unwrap());
    let config = PathBuf::from(std::env::var_os("SLICENV_TEST_OBN_CONFIG").unwrap());
    if mode == "env" {
        // Однопотоковий дочірній процес: оточення ще ніхто не читає.
        unsafe { cloud::obn::quiet_process_env() };
    }
    let (client, _events) = Client::load(Options {
        plugin_path: plugin,
        studio_config_dir: config,
        country_code: "US".into(),
        certificate: None,
        allow_control: false,
        http_headers: Vec::new(),
        persistent_session: true,
    })
    .unwrap();
    let log = client.private_dir().join("log");
    println!("file-log={}", log.join("obn.log").exists());
    let seeded = std::fs::read_to_string(log.join("obn.conf")).unwrap_or_default();
    println!("block-cloud-kept={}", seeded.contains("block_cloud = 0"));
}

#[test]
fn obn_logs_never_reach_stderr_or_files() {
    if let Ok(mode) = std::env::var(CHILD) {
        return child(&mode);
    }
    let dir = tempfile::tempdir().unwrap();
    let plugin = native::shared_library(
        "fake_plugin.cpp",
        &dir.path().join(native::library_name("fake-obn")),
        &[
            "-DFAKE_VERSION=\"02.08.04.99\"".into(),
            "-DFAKE_0208".into(),
            "-DFAKE_OBN_LOGGING".into(),
        ],
    );
    let run = |mode: &str| {
        let config = dir.path().join(mode);
        std::fs::create_dir_all(&config).unwrap();
        if mode == "seeded" {
            // Наш obn.conf, у який користувач дописав увімкнені логи.
            let mut text = String::from("block_cloud = 0\n");
            for (k, v) in cloud::obn::required_keys(true) {
                text += &format!("{k} = {v}\n");
            }
            text += "log_stderr = 1\nlog_to_file = 1\n";
            std::fs::write(config.join("obn.conf"), text).unwrap();
        }
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(CHILD, mode)
            .env("SLICENV_TEST_OBN_PLUGIN", &plugin)
            .env("SLICENV_TEST_OBN_CONFIG", &config);
        for (k, _) in cloud::obn::QUIET_ENV {
            command.env_remove(k);
        }
        let out = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "{mode}: {stdout}");
        (String::from_utf8_lossy(&out.stderr).into_owned(), stdout)
    };

    // Режим `env` спирається на те, що плагін бачить змінні, виставлені після старту процесу.
    // У Windows CRT бібліотеки тримає власну копію оточення й `SetEnvironmentVariable` з Rust
    // туди не доходить; відкритого плагіна для Windows немає, тож там перевіряється лише конфіг.
    let modes: &[&str] = if cfg!(windows) { &["seeded"] } else { &["seeded", "env"] };
    for &mode in modes {
        let (stderr, stdout) = run(mode);
        for secret in SECRETS {
            assert!(!stderr.contains(secret), "{mode}: stderr містить вигаданий ідентифікатор");
        }
        assert!(!stderr.contains("[obn]"), "{mode}: сирий рядок OBN у stderr");
        assert!(stdout.contains("file-log=false"), "{mode}: {stdout}");
        if mode == "seeded" {
            assert!(stdout.contains("block-cloud-kept=true"), "{stdout}");
        }
    }
    // Контроль: без нашого конфігу й змінних модель поводиться як OBN і пише в stderr і файл.
    let (stderr, stdout) = run("bare");
    assert!(stderr.contains("synthetic-user@example.invalid"));
    assert!(stdout.contains("file-log=true"), "{stdout}");
}
