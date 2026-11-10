//! Порівняння мережевих плагінів через наш шим без входу в акаунт і без мережі: будь-яка
//! `libbambu_networking.so` (закрита Bambu Lab або відкрита ClusterM/open-bamboo-networking)
//! завантажується з порожньою тимчасовою сесією; міряється час кожного кроку.
//! Жодного логіну, MQTT, друку чи команд принтеру: `start_print` не викликається, а
//! `printers()` / `connect_server()` наш API без сесії відхиляє ще до виклику плагіна.
//!
//! ```sh
//! # мережа відрізана (unshare -rn), дані й конфіг — тимчасові
//! T=$(mktemp -d); XDG_DATA_HOME=$T/d XDG_CONFIG_HOME=$T/c XDG_CACHE_HOME=$T/k \
//!   unshare -rn target/debug/examples/open_plugin_probe /tmp/obn/libbambu_networking.so
//! ```
//! Плагін копіюється в тимчасову теку (чужі теки лише читаються). Звіт —
//! docs/LINUX-CLOUD-PRINT.md.
// Лише приватний вивід.
#[allow(dead_code)]
mod common;
use anyhow::{Context as _, Result, ensure};
use cloud::{Client, Options};
use std::{
    fs::File,
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};

fn step<T>(out: &mut File, name: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let t = Instant::now();
    let r = f();
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    match &r {
        Ok(_) => writeln!(out, "  {name}: так, {ms:.0} мс")?,
        Err(e) => writeln!(out, "  {name}: ні, {ms:.0} мс — {e:#}")?,
    }
    r
}

fn run(out: &mut File) -> Result<()> {
    let source = PathBuf::from(
        std::env::args()
            .nth(1)
            .context("open_plugin_probe <шлях до libbambu_networking.so>")?,
    );
    let temp = std::env::temp_dir();
    for dir in [paths::data_dir(), paths::config_dir(), paths::cache_dir()] {
        ensure!(
            dir.starts_with(&temp) || dir.starts_with("/tmp/"),
            "тека {} не тимчасова: перевірка не чіпає справжніх даних",
            dir.display()
        );
    }
    unsafe { cloud::configure_tls_env() };

    // Копія плагіна поруч з тимчасовою сесією: dlopen не тримає чужу теку.
    let work = tempfile::tempdir()?;
    let plugin_dir = work.path().join("plugins");
    std::fs::create_dir_all(&plugin_dir)?;
    let library = plugin_dir.join("libbambu_networking.so");
    std::fs::copy(&source, &library)?;
    let size = std::fs::metadata(&library)?.len();
    let version = cloud::read_plugin_version(&library)?;
    writeln!(
        out,
        "Плагін: {} ({:.1} МБ), get_version {version}",
        source.display(),
        size as f64 / 1_048_576.0
    )?;

    let total = Instant::now();
    let mut options = Options::from_studio_dir(work.path().to_owned())?;
    options.plugin_path = library.clone();
    options.country_code = "US".into();
    let (client, _events) = step(out, "dlopen + create_agent + set_config_dir + init_log", || {
        Client::load(options)
    })?;
    step(out, "start", || client.start())?;
    std::thread::sleep(Duration::from_millis(300));
    let login = step(out, "is_user_login", || client.is_user_login())?;
    writeln!(out, "    → {login}")?;
    let connected = step(out, "is_server_connected", || client.is_server_connected())?;
    writeln!(out, "    → {connected}")?;
    // Без сесії API відмовляє ще до плагіна — фіксуємо саме це.
    let _ = step(out, "printers() без входу", || client.printers().map(|p| p.len()));
    let _ = step(out, "connect_server() без входу", || client.connect_server());
    step(out, "destroy_agent", || client.shutdown())?;
    writeln!(
        out,
        "Разом: {:.0} мс",
        total.elapsed().as_secs_f64() * 1000.0
    )?;
    Ok(())
}

fn main() {
    let Ok(mut out) = common::private_output() else {
        std::process::exit(1)
    };
    match run(&mut out) {
        Ok(()) => {
            let _ = writeln!(out, "Готово");
        }
        Err(error) => {
            let _ = writeln!(out, "Помилка: {error:#}");
            std::process::exit(1);
        }
    }
}
