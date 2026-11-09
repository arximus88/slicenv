//! Перевірка плагіна без входу в акаунт: завантажити плагін поточної серії з серверів Bambu
//! у ТИМЧАСОВУ теку даних, завантажити його шимом (create_agent, set_config_dir, init_log),
//! запустити агента, переконатися, що сесії немає, і знищити агента (destroy_agent).
//! Жодного логіну, MQTT чи команд принтеру. Працює на Linux і Windows.
//!
//! ```sh
//! XDG_DATA_HOME=$(mktemp -d) XDG_CONFIG_HOME=$(mktemp -d) cargo run -p cloud --example plugin_probe
//! ```
//! ```bat
//! rem Windows: дані Slicenv у %LOCALAPPDATA%, сесії слайсерів у %APPDATA% — обидва тимчасові.
//! set LOCALAPPDATA=%TEMP%\probe-data& set APPDATA=%TEMP%\probe-config& cargo run -p cloud --example plugin_probe
//! ```
//! `--version <бібліотека>` — лише прочитати версію чужого плагіна (`bambu_network_get_version`),
//! без створення агента: так безпечно дивитися на плагін, встановлений іншим слайсером.
// Лише приватний вивід.
#[allow(dead_code)]
mod common;
use anyhow::{Context as _, Result, ensure};
use cloud::{
    Client, Options,
    http::Curl,
    plugin::{self, Consent, InstallProgress},
};
use std::{fs::File, io::Write, path::PathBuf, sync::atomic::AtomicBool, time::Duration};

fn run(out: &mut File) -> Result<()> {
    let mut args = std::env::args().skip(1);
    if let Some(flag) = args.next() {
        ensure!(flag == "--version", "невідомий аргумент {flag}");
        let library = PathBuf::from(args.next().context("--version <шлях до бібліотеки>")?);
        let version = cloud::read_plugin_version(&library)?;
        let supported = cloud::LATEST_SERIES;
        writeln!(
            out,
            "Версія плагіна: {version} (підтримувана серія {supported})"
        )?;
        return Ok(());
    }
    let temp = std::env::temp_dir();
    let data = paths::data_dir();
    ensure!(
        data.starts_with(&temp) || data.starts_with("/tmp/"),
        "тека даних {} не тимчасова: перевірка не чіпає справжніх даних",
        data.display()
    );
    unsafe { cloud::configure_tls_env() };

    let remote = plugin::fetch_remote(&Curl, "US", cloud::LATEST_SERIES)?;
    writeln!(
        out,
        "Сервер Bambu ({}): плагін {} ({:?} байт)",
        plugin::OS_TYPE,
        remote.version,
        remote.size
    )?;
    let consent = Consent::accept()?;
    let cancel = AtomicBool::new(false);
    let mut last = -1i32;
    let installed = plugin::install(
        &Curl,
        &consent,
        &remote,
        &mut |p| {
            let pct = p.fraction().map_or(-1, |f| (f * 4.0) as i32);
            if pct != last || p == InstallProgress::Done {
                last = pct;
                let _ = writeln!(out, "  прогрес: {p:?}");
            }
        },
        &cancel,
    )?;
    writeln!(
        out,
        "Установлено: {:?}, покоління {:?}, у керованій теці: {}",
        installed.version,
        installed.generation(),
        installed.library.starts_with(plugin::managed_root())
    )?;
    if let Some(dir) = installed.library.parent() {
        let mut files: Vec<_> = std::fs::read_dir(dir)?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        files.sort();
        writeln!(out, "Файли плагіна: {}", files.join(", "))?;
    }
    writeln!(
        out,
        "get_version (без агента): {}",
        cloud::read_plugin_version(&installed.library)?
    )?;
    let ready = matches!(plugin::plugin_state(), cloud::PluginState::Ready(p) if p.library == installed.library);
    writeln!(out, "plugin_state знаходить завантажений плагін: {ready}")?;

    // Порожня тимчасова тека сесії: агент стартує без входу.
    let empty = tempfile::tempdir()?;
    let mut options = Options::from_studio_dir(empty.path().to_owned())?;
    options.plugin_path = installed.library.clone();
    ensure!(
        options.certificate.is_some(),
        "немає сертифіката слайсера (вбудований мав би бути в кеші)"
    );
    let (client, _events) = Client::load(options)?;
    writeln!(
        out,
        "create_agent + set_config_dir + init_log: так, плагін {}",
        client.plugin_version()?
    )?;
    client.start()?;
    writeln!(out, "start: так")?;
    std::thread::sleep(Duration::from_millis(500));
    writeln!(out, "is_user_login: {}", client.is_user_login()?)?;
    writeln!(
        out,
        "is_server_connected: {}",
        client.is_server_connected()?
    )?;
    client.shutdown()?;
    writeln!(out, "destroy_agent: так")?;
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
