//! Читання хмарної камери; без збереження, якщо --save не задано явно.
mod common;
use anyhow::{Result, bail, ensure};
use cloud::{CameraStream, Client};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn arguments() -> Result<(usize, Option<PathBuf>)> {
    let mut frames = 5;
    let mut save = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => {
                frames = args
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| anyhow::anyhow!("Потрібне число після --frames"))?;
            }
            "--save" => {
                save =
                    Some(PathBuf::from(args.next().ok_or_else(|| {
                        anyhow::anyhow!("Потрібен шлях після --save")
                    })?));
            }
            _ => bail!("Використання: camera [--frames N] [--save /tmp/x.jpg]"),
        }
    }
    ensure!(
        (1..=10_000).contains(&frames),
        "Кількість кадрів: 1..=10000"
    );
    if let Some(path) = &save {
        // Перевіряємо до мережевих запитів. create_new нижче відмовляє симлінкам/перезапису.
        ensure!(
            path.is_absolute()
                && path
                    .parent()
                    .and_then(|p| p.canonicalize().ok())
                    .is_some_and(|p| p.starts_with(Path::new("/tmp"))),
            "Зображення можна зберігати лише в /tmp"
        );
        ensure!(!path.exists(), "Файл для кадру вже існує");
    }
    Ok((frames, save))
}
fn connect(client: &Client) -> Result<()> {
    client.connect_server()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while !client.is_server_connected()? && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    ensure!(
        client.is_server_connected()?,
        "Хмарне з’єднання не встановлено"
    );
    Ok(())
}
fn run(out: &mut File) -> Result<()> {
    let (count, save) = arguments()?;
    let (client, mut events) = common::session()?;
    connect(&client)?;
    let dev = if let Some(dev) = common::device_override() {
        dev
    } else {
        // Одне звернення без повторів; BAMBU_DEV_ID повністю оминає цей endpoint.
        // Список принтерів часто відхиляється (-11) — тоді беремо пристрій з історії завдань.
        match client.printers() {
            Ok(list) => {
                list.into_iter()
                    .find(|p| p.online)
                    .ok_or_else(|| anyhow::anyhow!("Немає принтера онлайн; задайте BAMBU_DEV_ID"))?
                    .dev_id
            }
            Err(_) => client
                .get_user_tasks(&cloud::TaskQuery::default())?
                .into_iter()
                .find_map(|t| t.dev_id)
                .ok_or_else(|| {
                    anyhow::anyhow!("Ні списку принтерів, ні історії завдань; задайте BAMBU_DEV_ID")
                })?,
        }
    };
    writeln!(out, "Камера: {dev}; лише читання")?;
    let mut firmware = std::env::var("BAMBU_CAMERA_FIRMWARE").unwrap_or_default();
    let mut proto = String::from("?");
    if firmware.is_empty() {
        // Як Studio: підписка й читання версій модулів (лише info/get_version + pushall).
        client.subscribe(&dev)?;
        // Канал телеметрії прогрівається кілька секунд (-2) — одна повторна спроба.
        if client.request_version(&dev).is_err() {
            std::thread::sleep(Duration::from_secs(6));
            client.request_version(&dev)?;
        }
        client.request_status(&dev).ok();
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && (firmware.is_empty() || proto == "?") {
            let Ok(event) = events.recv_timeout(Duration::from_millis(500)) else {
                continue;
            };
            let payload = match &event {
                cloud::Event::PrinterMessage { payload, .. } => payload.as_value().clone(),
                cloud::Event::Status { patch, .. } => patch.as_value().clone(),
                _ => continue,
            };
            if let Some(version) = cloud::ota_version(&payload) {
                firmware = version;
            }
            if let Some(ipcam) = payload["print"]["ipcam"].as_object() {
                proto = format!(
                    "ipcam_dev={} tutk_server={} agora={}",
                    ipcam
                        .get("ipcam_dev")
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                    ipcam
                        .get("tutk_server")
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                    ipcam
                        .get("agora_service")
                        .map(|v| v.to_string())
                        .unwrap_or_default()
                );
            }
        }
        writeln!(
            out,
            "Прошивка: {}; камера: {proto}",
            if firmware.is_empty() { "?" } else { &firmware }
        )?;
    }
    let mut camera = CameraStream::open_with_firmware(&client, &dev, &firmware)?;
    let started = Instant::now();
    let mut first_at = None;
    for i in 0..count {
        let frame = camera.next_frame()?;
        first_at.get_or_insert_with(Instant::now);
        writeln!(
            out,
            "Кадр {}: {}×{}, {} байтів JPEG",
            i + 1,
            frame.width,
            frame.height,
            frame.jpeg.len()
        )?;
        if i == 0
            && let Some(path) = &save
        {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)
                .map_err(|_| anyhow::anyhow!("Не вдалося створити приватний файл кадру в /tmp"))?;
            file.write_all(&frame.jpeg)?;
            writeln!(out, "Перший кадр збережено в /tmp (0600)")?;
        }
    }
    let fps = if count > 1 {
        (count - 1) as f64 / first_at.unwrap().elapsed().as_secs_f64()
    } else {
        0.0
    };
    writeln!(
        out,
        "Кадрів: {count}; час: {:.2} с; fps між першим і останнім: {fps:.2}",
        started.elapsed().as_secs_f64()
    )?;
    camera.stop()?;
    drop(camera);
    client.shutdown()
}
fn main() {
    let Ok(mut out) = common::private_output() else {
        std::process::exit(1)
    };
    if let Err(error) = run(&mut out) {
        let _ = writeln!(out, "Камера: {error}");
        std::process::exit(1);
    }
}
