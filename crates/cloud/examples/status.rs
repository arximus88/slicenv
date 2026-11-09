//! Лише сесія, список принтерів і телеметрія; жодних фізичних команд.
use anyhow::{Result, bail};
use cloud::{Client, DeviceId, Event, Options};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    os::fd::{AsRawFd, FromRawFd},
    time::{Duration, Instant},
};

// Плагін може друкувати секрети напряму в stdout/stderr. Наш вивід має окремий fd.
fn private_output() -> Result<File> {
    let null = OpenOptions::new().write(true).open("/dev/null")?;
    unsafe {
        let saved = libc::dup(libc::STDOUT_FILENO);
        if saved < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let output = File::from_raw_fd(saved);
        if libc::dup2(null.as_raw_fd(), libc::STDOUT_FILENO) < 0
            || libc::dup2(null.as_raw_fd(), libc::STDERR_FILENO) < 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(output)
    }
}

fn run(out: &mut File) -> Result<()> {
    // Однопотоковий старт: можна змінювати оточення.
    unsafe { cloud::configure_tls_env() };
    let mut options = if let Some(path) = std::env::var_os("BAMBU_STUDIO_CONFIG") {
        Options::from_studio_dir(std::path::PathBuf::from(path))?
    } else {
        Options::from_bambu_studio()?
    };
    if let Some(path) = std::env::var_os("BAMBU_PLUGIN") {
        options.plugin_path = path.into();
    }
    if let Ok(country) = std::env::var("BAMBU_COUNTRY") {
        options.country_code = country;
    }
    if let Some(cert) = std::env::var_os("BAMBU_CERTIFICATE") {
        options.certificate = Some(cert.into());
    }
    let (client, mut events) = Client::load(options)?;
    writeln!(
        out,
        "Plugin: {} (exact 02.06 ABI)",
        client.plugin_version()?
    )?;
    client.start()?;
    let login_deadline = Instant::now() + Duration::from_secs(5);
    while !client.is_user_login()? && Instant::now() < login_deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let logged_in = client.is_user_login()?;
    writeln!(out, "Logged in: {logged_in} (isolated session copy)")?;
    if !logged_in {
        bail!(
            "No restorable session. Log in interactively in Bambu Studio on this machine, then rerun this example. No token should be pasted into chat."
        );
    }
    client.connect_server()?;
    let connect_deadline = Instant::now() + Duration::from_secs(15);
    while !client.is_server_connected()? && Instant::now() < connect_deadline {
        let _ = events.recv_timeout(Duration::from_millis(100));
    }
    writeln!(out, "Cloud connected: {}", client.is_server_connected()?)?;
    let selected_dev =
        if let Some(dev) = std::env::var("BAMBU_DEV_ID").ok().filter(|s| !s.is_empty()) {
            DeviceId::new(dev)
        } else {
            let printers = client.printers()?;
            writeln!(
                out,
                "Printers: {} (custom names omitted for privacy)",
                printers.len()
            )?;
            for (i, printer) in printers.iter().enumerate() {
                // Невідомі рядки моделі також не друкуємо.
                let model = match printer.model.as_str() {
                    "N2S" => "A1",
                    "N1" => "A1 mini",
                    "A1" | "A1 mini" | "X1" | "X1 Carbon" | "P1P" | "P1S" | "H2D" => {
                        printer.model.as_str()
                    }
                    _ => "[model available through API]",
                };
                writeln!(
                    out,
                    "  #{}: {} model={} online={}",
                    i + 1,
                    printer.dev_id,
                    model,
                    printer.online
                )?;
            }
            let Some(printer) = printers.into_iter().find(|p| p.online) else {
                writeln!(out, "No online printer; no subscription.")?;
                return Ok(());
            };
            printer.dev_id
        };
    client.subscribe(&selected_dev)?;
    if client.request_status(&selected_dev).is_err() {
        writeln!(
            out,
            "Telemetry channel is warming up; listening before one retry."
        )?;
    }
    writeln!(
        out,
        "Streaming 30 seconds of read-only status for {}…",
        selected_dev
    )?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let retry_at = Instant::now() + Duration::from_secs(5);
    let mut retried = false;
    let mut count = 0;
    while Instant::now() < deadline {
        if !retried && Instant::now() >= retry_at {
            retried = true;
            if client.request_status(&selected_dev).is_err() {
                writeln!(
                    out,
                    "Telemetry request unavailable; continuing passive subscription."
                )?;
            }
        }
        match events.recv_timeout(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_secs(1)),
        ) {
            Ok(Event::Status { dev_id, status, .. }) if dev_id == selected_dev => {
                count += 1;
                let state = status
                    .gcode_state
                    .as_deref()
                    .filter(|s| {
                        matches!(
                            *s,
                            "IDLE"
                                | "RUNNING"
                                | "PAUSE"
                                | "FINISH"
                                | "FAILED"
                                | "PREPARE"
                                | "SLICING"
                        )
                    })
                    .unwrap_or("UNKNOWN");
                writeln!(
                    out,
                    "  {} state={} progress={:?}% remaining={:?}min layer={:?}/{:?} nozzle={:?}/{:?}C bed={:?}/{:?}C error={:?} speed={:?} AMS={} light_reports={}",
                    dev_id,
                    state,
                    status.mc_percent,
                    status.mc_remaining_time,
                    status.layer_num,
                    status.total_layer_num,
                    status.nozzle_temper,
                    status.nozzle_target_temper,
                    status.bed_temper,
                    status.bed_target_temper,
                    status.print_error,
                    status.spd_lvl,
                    status.ams.as_ref().map_or(0, |a| a.units.len()),
                    status.lights.len()
                )?;
            }
            Ok(Event::Lagged { dropped }) => {
                writeln!(
                    out,
                    "  Dropped {dropped} events; requesting a fresh snapshot."
                )?;
                client.request_status(&selected_dev)?;
            }
            Ok(Event::ServerConnected {
                return_code,
                reason_code,
            }) => {
                writeln!(
                    out,
                    "  Connection callback: result={return_code} reason={reason_code}"
                )?;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                bail!("plugin event channel disconnected")
            }
            _ => {}
        }
    }
    writeln!(
        out,
        "Status updates: {count}; no control or print commands sent."
    )?;
    if count == 0 {
        bail!("no live status received; check the cloud connection and printer availability");
    }
    client.shutdown()?;
    Ok(())
}

fn main() {
    let Ok(mut out) = private_output() else {
        std::process::exit(1);
    };
    if let Err(error) = run(&mut out) {
        let _ = writeln!(out, "Status example: {error}");
        std::process::exit(1);
    }
}
