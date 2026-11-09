//! Жива перевірка схеми релізу без Bambu Studio-плагіна: завантажити плагін поточної серії
//! з серверів Bambu у ТИМЧАСОВУ теку даних, завантажити його шимом, відновити копію наявної
//! сесії, підключитися, прочитати список принтерів та історію. Лише читання: жодних команд
//! принтеру, жодного виходу з акаунта.
//!
//! ```sh
//! XDG_DATA_HOME=$(mktemp -d) cargo run -p cloud --example plugin_live
//! ```
//! Вивід без персональних даних: лише версії, кількості й коди.
// Лише приватний вивід; відновлення сесії тут власне.
#[allow(dead_code)]
mod common;
use anyhow::{Context as _, Result, ensure};
use cloud::{
    Client, Options, SessionOrigin, TaskQuery, account,
    http::Curl,
    plugin::{self, Consent, InstallProgress},
};
use std::{
    fs::File,
    io::Write,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

fn run(out: &mut File) -> Result<()> {
    let data = std::env::var("XDG_DATA_HOME").context("задайте XDG_DATA_HOME=тимчасова тека")?;
    ensure!(
        data.starts_with("/tmp/"),
        "XDG_DATA_HOME має бути в /tmp: перевірка не чіпає справжніх даних"
    );
    unsafe { cloud::configure_tls_env() };

    let remote = plugin::fetch_remote(&Curl, "US", cloud::LATEST_SERIES)?;
    writeln!(
        out,
        "Сервер Bambu: плагін {} ({:?} байт)",
        remote.version, remote.size
    )?;
    // Згода в тимчасовій теці: користувач запускає перевірку свідомо.
    let consent = Consent::accept()?;
    let mut last = -1i32;
    let cancel = AtomicBool::new(false);
    let installed = plugin::install(
        &Curl,
        &consent,
        &remote,
        &mut |p| {
            let pct = p.fraction().map_or(-1, |f| (f * 10.0) as i32);
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
    let version = cloud::read_plugin_version(&installed.library)?;
    writeln!(out, "Версія з бібліотеки: {version}")?;

    let state = plugin::plugin_state();
    writeln!(
        out,
        "Стан плагіна: {}",
        match &state {
            cloud::PluginState::Ready(p) => format!("готовий ({:?})", p.origin),
            other => format!("{other:?}"),
        }
    )?;
    let sessions = account::find_sessions();
    writeln!(
        out,
        "Знайдені сесії: {:?}",
        sessions.iter().map(|s| s.origin).collect::<Vec<_>>()
    )?;
    let studio = sessions
        .iter()
        .find(|s| s.origin == SessionOrigin::BambuStudio)
        .context("немає сесії Bambu Studio для перевірки")?;

    // 1) Чужа сесія — тимчасовою копією.
    check(out, Options::for_session(&installed, studio), true)?;
    // 2) «Використати сесію»: копія у власну (тимчасову) теку, плагін працює в ній напряму.
    let adopted = account::adopt_session(studio)?;
    ensure!(
        adopted.dir.starts_with(&data),
        "власна сесія поза тимчасовою текою"
    );
    writeln!(
        out,
        "Сесію скопійовано у власну теку (походження {:?})",
        adopted.adopted_from
    )?;
    check(out, Options::for_session(&installed, &adopted), false)?;
    account::forget_own_session()?;
    writeln!(out, "Власну копію сесії видалено")?;
    // 3) Сесія OrcaSlicer (може бути записана старшим плагіном) — лише відновлення.
    if let Some(orca) = sessions
        .iter()
        .find(|s| s.origin == SessionOrigin::OrcaSlicer)
    {
        if let Err(e) = check(out, Options::for_session(&installed, orca), false) {
            writeln!(out, "  сесія OrcaSlicer: {e}")?;
        }
    }
    Ok(())
}

fn check(out: &mut File, options: Options, full: bool) -> Result<()> {
    writeln!(
        out,
        "— сесія {}",
        if options.persistent_session {
            "власна"
        } else {
            "копія"
        }
    )?;
    let (client, mut events) = Client::load(options)?;
    writeln!(out, "  шим завантажив плагін {}", client.plugin_version()?)?;
    client.start()?;
    let deadline = Instant::now() + Duration::from_secs(8);
    while !client.is_user_login()? && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let login = client.is_user_login()?;
    writeln!(out, "  is_user_login: {login}")?;
    ensure!(login, "сесія не відновилася");
    let nick = client.user_nickname()?;
    writeln!(
        out,
        "  нік отримано: {} (значення приховано)",
        !nick.is_empty()
    )?;
    if full {
        client.connect_server()?;
        let deadline = Instant::now() + Duration::from_secs(15);
        while !client.is_server_connected()? && Instant::now() < deadline {
            let _ = events.recv_timeout(Duration::from_millis(100));
        }
        writeln!(out, "  connect_server: {}", client.is_server_connected()?)?;
        match client.printers() {
            Ok(list) => writeln!(
                out,
                "  принтерів: {}, онлайн: {}",
                list.len(),
                list.iter().filter(|p| p.online).count()
            )?,
            Err(e) => writeln!(out, "  список принтерів: {e}")?,
        }
        match client.get_user_tasks(&TaskQuery {
            limit: 5,
            ..Default::default()
        }) {
            Ok(tasks) => writeln!(out, "  завдань в історії (до 5): {}", tasks.len())?,
            Err(e) => writeln!(out, "  історія: {e}")?,
        }
    }
    client.shutdown()?;
    Ok(())
}

fn main() {
    let Ok(mut out) = common::private_output() else {
        std::process::exit(1)
    };
    if let Err(error) = run(&mut out) {
        let _ = writeln!(out, "Помилка: {error:#}");
        std::process::exit(1);
    }
}
