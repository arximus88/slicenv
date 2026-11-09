mod common;
use anyhow::Result;
use cloud::{CameraStream, TaskQuery};
use std::io::Write;
fn run(out: &mut std::fs::File) -> Result<()> {
    let (client, mut events) = common::session()?;
    let tasks = client.get_user_tasks(&TaskQuery {
        limit: 1,
        ..Default::default()
    })?;
    let task = &tasks[0];
    writeln!(
        out,
        "Поля історії: id={}, title={}, design={}, status={}, dev={}, start={}, end={}, cost={}, weight={}, plate={}, cover={}",
        !task.id.is_empty(),
        task.title.is_some(),
        task.design_name.is_some(),
        task.status.is_some(),
        task.dev_id.is_some(),
        task.start_time.is_some(),
        task.end_time.is_some(),
        task.cost_time.is_some(),
        task.weight.is_some(),
        task.plate_index.is_some(),
        task.cover_url.is_some()
    )?;
    writeln!(
        out,
        "Деталі підзавдання: {}",
        match client.get_subtask_info(&task.id) {
            Ok(_) => "успіх".into(),
            Err(e) => e.to_string(),
        }
    )?;
    let dev = common::device_override()
        .or_else(|| task.dev_id.clone())
        .unwrap();
    client.connect_server()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !client.is_server_connected()? && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    writeln!(
        out,
        "Камера: {dev}; пристрій з історії; op 10 не викликався"
    )?;
    client.subscribe(&dev)?;
    std::thread::sleep(std::time::Duration::from_secs(5));
    client.request_version(&dev)?;
    client.request_status(&dev)?;
    let mut firmware = String::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        match events.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(cloud::Event::PrinterMessage { payload, .. }) => {
                if let Some(modules) = payload.as_value()["info"]["module"].as_array() {
                    for m in modules {
                        if m["name"] == "ota" {
                            firmware = m["sw_ver"].as_str().unwrap_or("").to_owned();
                        }
                    }
                }
            }
            Ok(cloud::Event::HttpError { status_code }) => {
                writeln!(out, "HTTP: {status_code}")?;
            }
            _ => {}
        }
    }
    writeln!(out, "OTA-версію отримано: {}", !firmware.is_empty())?;
    let result = CameraStream::open_with_firmware(&client, &dev, &firmware);
    if result.is_err() {
        while let Ok(event) = events.recv_timeout(std::time::Duration::from_millis(10)) {
            if let cloud::Event::HttpError { status_code } = event {
                writeln!(out, "HTTP: {status_code}")?;
            }
        }
        // Дозволені лише статичні маркери; жодних рядків логів.
        for word in [
            "camera",
            "ttcode",
            "agora",
            "invalid",
            "failed",
            "timeout",
            "Unauthorized",
            "Forbidden",
        ] {
            let mut count = 0;
            for item in std::fs::read_dir(client.private_dir().join("log"))? {
                if let Ok(data) = std::fs::read(item?.path()) {
                    count += String::from_utf8_lossy(&data).matches(word).count();
                }
            }
            writeln!(out, "Маркер {word}: {count}")?;
        }
    }
    let mut camera = result?;
    for i in 0..3 {
        let f = camera.next_frame()?;
        writeln!(
            out,
            "Кадр {}: {}×{}, {} байтів",
            i + 1,
            f.width,
            f.height,
            f.jpeg.len()
        )?;
        if i == 0 {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open("/tmp/bambu-w5-camera.jpg")?
                .write_all(&f.jpeg)?;
        }
    }
    camera.stop()?;
    drop(camera);
    client.shutdown()
}
fn main() {
    let mut out = common::private_output().unwrap();
    if let Err(e) = run(&mut out) {
        let _ = writeln!(out, "Перевірка: {e}");
        std::process::exit(1);
    }
}
