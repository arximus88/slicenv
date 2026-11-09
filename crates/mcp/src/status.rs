//! Стан принтера лише для читання: сесія Bambu Studio → хмара → один знімок телеметрії.
//!
//! `cloud::Client` прив'язаний до потоку й вантажить нативний плагін, тому знімок береться
//! в одному блокуючому виклику; команд керування тут немає (`allow_control` лишається `false`).

use std::time::{Duration, Instant};

use anyhow::{Result, bail, ensure};
use cloud::{Client, Event, Options};
use serde_json::{Value, json};

pub fn printer_status(wanted: Option<&str>) -> Result<Value> {
    // Той самий перевизначувач каталогу, що й в examples/status.rs.
    let options = match std::env::var_os("BAMBU_STUDIO_CONFIG") {
        Some(dir) => Options::from_studio_dir(dir),
        None => Options::from_bambu_studio(),
    }
    .map_err(|e| {
        anyhow::anyhow!("немає конфігурації Bambu Studio ({e:#}); увійди в акаунт у Bambu Studio")
    })?;
    ensure!(
        options.plugin_path.is_file(),
        "немає мережевого плагіна Bambu ({}); запусти Bambu Studio й увійди в акаунт",
        options.plugin_path.display()
    );
    let (client, mut events) = Client::load(options)?;
    client.start()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !client.is_user_login()? && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    ensure!(
        client.is_user_login()?,
        "немає збереженої сесії: увійди в акаунт у Bambu Studio на цій машині"
    );
    client.connect_server()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while !client.is_server_connected()? && Instant::now() < deadline {
        let _ = events.recv_timeout(Duration::from_millis(100));
    }
    ensure!(
        client.is_server_connected()?,
        "хмара Bambu не відповіла за 15 с"
    );

    let printers = client.printers()?;
    let Some(printer) = printers
        .iter()
        .find(|p| wanted.map_or(p.online, |w| p.name == w))
    else {
        bail!(
            "принтер не знайдено; доступні: {:?}",
            printers
                .iter()
                .map(|p| (&p.name, p.online))
                .collect::<Vec<_>>()
        );
    };
    client.subscribe(&printer.dev_id)?;
    client.request_status(&printer.dev_id)?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        if let Ok(Event::Status { dev_id, status, .. }) =
            events.recv_timeout(left.min(Duration::from_secs(1)))
            && dev_id == printer.dev_id
        {
            return Ok(json!({
                "printer": {"name": printer.name, "model": printer.model, "online": printer.online, "id": printer.dev_id.redacted()},
                "state": status.gcode_state, "progress_percent": status.mc_percent,
                "remaining_min": status.mc_remaining_time, "layer": status.layer_num, "total_layers": status.total_layer_num,
                "job": status.subtask_name, "nozzle_c": status.nozzle_temper, "nozzle_target_c": status.nozzle_target_temper,
                "bed_c": status.bed_temper, "bed_target_c": status.bed_target_temper, "chamber_c": status.chamber_temper,
                "print_error": status.print_error, "hms_active": status.hms.len(),
            }));
        }
    }
    bail!("принтер не надіслав стан за 15 с")
}
