//! Жива перевірка лише на читання: виявлення, MQTT-статус, (опційно) вхід FTPS і один
//! кадр камери. Жодних команд керування, друку чи завантаження файлів.
//!
//! ```text
//! cargo run -p lan --example probe                       # лише виявлення
//! SLICENV_LAN_CODE_FILE=/шлях/до/файлу-з-кодом \
//!   cargo run -p lan --example probe -- [--model N2S] [--watch] [--ftp] [--camera] [--serial SN --ip IP]
//! ```
//! Код доступу читається з файлу й ніколи не друкується; серійник — лише останні 4 символи.
use std::time::{Duration, Instant};

use lan::{LanEvent, LanPrinter, discovery, store::redact};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| args.iter().any(|a| a == name);
    let value = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    };

    let t = Instant::now();
    let found = discovery::discover(Duration::from_secs(8)).expect("UDP 2021");
    println!("виявлення: {} принтер(ів) за {:.1} с", found.len(), t.elapsed().as_secs_f32());
    for d in &found {
        println!(
            "  {} {} ip={} connect={:?} bind={:?} fw={:?} signal={:?}",
            redact(&d.serial),
            lan::model_name(&d.model),
            d.ip,
            d.connect,
            d.bind,
            d.version,
            d.signal
        );
    }

    // `--tls`: лише TLS-рукостискання з кожним принтером (без входу, без коду доступу).
    if flag("--tls") {
        for d in &found {
            for port in [8883u16, 990, 6000] {
                println!("  TLS {} :{port} → {}", redact(&d.serial), tls_only(&d.serial, d.ip, port));
            }
        }
    }

    let Some(code_file) = std::env::var_os("SLICENV_LAN_CODE_FILE") else {
        println!("SLICENV_LAN_CODE_FILE не задано — лише виявлення");
        return;
    };
    let code = std::fs::read_to_string(code_file).expect("файл коду");
    let (serial, ip, model) = match (value("--serial"), value("--ip")) {
        (Some(s), Some(ip)) => (s, ip, String::new()),
        _ => {
            // `--model N2S` — вибрати виявлений принтер за кодом моделі.
            let want = value("--model");
            let d = found
                .iter()
                .find(|d| want.as_ref().is_none_or(|m| &d.model == m))
                .expect("немає виявлених принтерів");
            (d.serial.clone(), d.ip.to_string(), d.model.clone())
        }
    };
    let mut printer = LanPrinter::new(&serial, &ip, code.trim()).expect("дані принтера");
    printer.model = model;

    let t = Instant::now();
    match lan::test_connection(&printer, Duration::from_secs(15)) {
        Ok(report) => {
            let s = &report.status;
            println!(
                "MQTT: OK за {:.1} с · стан {:?} · сопло {:?}/{:?} · стіл {:?}/{:?} · SD {:?} · прошивка {:?} · HMS {}",
                t.elapsed().as_secs_f32(),
                s.gcode_state,
                s.nozzle_temper,
                s.nozzle_target_temper,
                s.bed_temper,
                s.bed_target_temper,
                s.sd_card(),
                report.firmware,
                s.hms.len()
            );
        }
        Err(e) => println!("MQTT: {e}"),
    }

    // Окрема сесія: часткові звіти й обмежувач pushall.
    if flag("--watch") {
        match lan::LanClient::connect(&printer) {
            Ok((client, events)) => {
                let end = Instant::now() + Duration::from_secs(12);
                let (mut statuses, mut skipped) = (0, 0);
                while Instant::now() < end {
                    if let Ok(e) = events.recv_timeout(Duration::from_millis(500)) {
                        match e {
                            LanEvent::Status(_) => statuses += 1,
                            LanEvent::Rejected(e) | LanEvent::Disconnected(e) => println!("  подія: {e}"),
                            _ => {}
                        }
                    }
                    if !client.request_status().unwrap_or(false) {
                        skipped += 1;
                    }
                }
                println!("watch: звітів {statuses}, pushall пропущено обмежувачем {skipped} разів");
            }
            Err(e) => println!("watch: {e}"),
        }
    }

    if flag("--ftp") {
        match lan::ftps::check_login(&printer) {
            Ok(()) => println!("FTPS: вхід OK (файлів не чіпали)"),
            Err(e) => println!("FTPS: {e}"),
        }
    }
    if flag("--camera") {
        match lan::camera::CameraStream::open(&printer).and_then(|mut c| c.next_frame()) {
            Ok(jpeg) => println!("камера: кадр JPEG {} байт", jpeg.len()),
            Err(e) => println!("камера: {e}"),
        }
    }
}

fn tls_only(serial: &str, ip: std::net::IpAddr, port: u16) -> String {
    use rustls::{ClientConnection, pki_types::ServerName};
    let (config, problem) = lan::tls::client_config(serial);
    let addr = std::net::SocketAddr::new(ip, port);
    let Ok(mut tcp) = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(5)) else {
        return "TCP недоступний".into();
    };
    tcp.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut conn = ClientConnection::new(config, ServerName::IpAddress(ip.into())).unwrap();
    while conn.is_handshaking() {
        if let Err(e) = conn.complete_io(&mut tcp) {
            return match problem.take() {
                Some(p) => format!("відхилено: {p}"),
                None => format!("помилка: {e}"),
            };
        }
    }
    format!("OK ({:?})", conn.protocol_version())
}
