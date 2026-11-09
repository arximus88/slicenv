//! Клієнт проти фейкового Moonraker на 127.0.0.1 (без справжніх принтерів).
use std::{io::Write as _, sync::atomic::AtomicBool};

use moonraker::{
    ApiKey, Client, Endpoint, KlippyState, MoonrakerError, PrintState, UploadOptions,
    fake::{FakeServer, FakeState},
};

fn client(server: &FakeServer, key: Option<&str>) -> Client {
    let key = key.map(|k| ApiKey::parse(k).unwrap().unwrap());
    Client::new(Endpoint::parse(&server.address()).unwrap(), key)
}

fn gcode(size: usize) -> tempfile::NamedTempFile {
    let mut f = tempfile::Builder::new().suffix(".gcode").tempfile().unwrap();
    let line = b"G1 X10 Y10 E0.5\n";
    for _ in 0..size / line.len() {
        f.write_all(line).unwrap();
    }
    f.flush().unwrap();
    f
}

const KEY: &str = "0123456789abcdef0123456789abcdef";

#[test]
fn probe_status_and_controls_succeed() {
    let server = FakeServer::start(FakeState::default()).unwrap();
    let c = client(&server, None);
    let probe = c.probe().unwrap();
    assert_eq!(probe.klippy_state(), KlippyState::Ready);
    assert_eq!(probe.server.moonraker_version, "v0.9.3-fake");
    assert_eq!(probe.printer.as_ref().unwrap().hostname, "fake-klipper");
    probe.require_ready().unwrap();

    let s = c.status().unwrap();
    assert_eq!(s.state, PrintState::Standby);
    assert_eq!(s.extruder.temperature, Some(24.1));

    let file = gcode(200_000);
    let mut seen = Vec::new();
    let r = c
        .upload(
            file.path(),
            "cube.gcode",
            UploadOptions { start: true, overwrite: false },
            &mut |sent, total| seen.push((sent, total)),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(r.print_started);
    assert_eq!(r.path, "cube.gcode");
    let total = std::fs::metadata(file.path()).unwrap().len();
    assert_eq!(seen.first(), Some(&(0, total)));
    assert_eq!(seen.last(), Some(&(total, total)));
    assert!(seen.len() > 2, "прогрес по шматках");
    server.with(|s| {
        let up = &s.uploads[0];
        assert_eq!((up.name.as_str(), up.root.as_str(), up.print, up.size as u64), ("cube.gcode", "gcodes", true, total));
    });

    let s = c.status().unwrap();
    assert_eq!(s.state, PrintState::Printing);
    assert_eq!(s.filename, "cube.gcode");
    assert_eq!(s.extruder.target, Some(215.0));
    assert_eq!(s.homed_axes, "xyz");

    // Уже друкує: новий друк не стартує, файл не передається.
    let again = c.upload(file.path(), "other.gcode", UploadOptions { start: true, overwrite: true }, &mut |_, _| {}, &AtomicBool::new(false));
    assert_eq!(again, Err(MoonrakerError::Busy));

    c.pause().unwrap();
    assert_eq!(c.status().unwrap().state, PrintState::Paused);
    c.resume().unwrap();
    c.cancel().unwrap();
    assert_eq!(c.status().unwrap().state, PrintState::Cancelled);
    assert!(matches!(c.resume(), Err(MoonrakerError::Http { status: 400, .. })));
}

#[test]
fn existing_file_is_not_overwritten_unless_asked() {
    let server = FakeServer::start(FakeState { files: vec!["cube.gcode".into()], ..Default::default() }).unwrap();
    let c = client(&server, None);
    let file = gcode(1000);
    let only = UploadOptions { start: false, overwrite: false };
    assert_eq!(
        c.upload(file.path(), "cube.gcode", only, &mut |_, _| {}, &AtomicBool::new(false)),
        Err(MoonrakerError::FileExists("cube.gcode".into()))
    );
    assert!(server.with(|s| s.uploads.is_empty()));
    let r = c
        .upload(file.path(), "cube.gcode", UploadOptions { overwrite: true, ..only }, &mut |_, _| {}, &AtomicBool::new(false))
        .unwrap();
    assert!(!r.print_started);
    server.with(|s| assert!(!s.uploads[0].print && s.print_state == "standby"));
}

#[test]
fn api_key_is_required_and_sent() {
    let server = FakeServer::start(FakeState { api_key: Some(KEY.into()), ..Default::default() }).unwrap();
    assert_eq!(client(&server, None).probe(), Err(MoonrakerError::Unauthorized));
    assert_eq!(client(&server, Some("wrongwrongwrong")).status(), Err(MoonrakerError::Unauthorized));
    let file = gcode(300_000);
    assert_eq!(
        client(&server, None).upload(file.path(), "x.gcode", UploadOptions::default(), &mut |_, _| {}, &AtomicBool::new(false)),
        Err(MoonrakerError::Unauthorized)
    );
    let c = client(&server, Some(KEY));
    c.probe().unwrap();
    c.upload(file.path(), "x.gcode", UploadOptions::default(), &mut |_, _| {}, &AtomicBool::new(false)).unwrap();
    server.with(|s| assert!(s.log.iter().filter(|(_, key)| *key).count() >= 3));
}

#[test]
fn klippy_shutdown_is_reported_and_blocks_printing() {
    let server = FakeServer::start(FakeState {
        klippy_state: "shutdown".into(),
        state_message: "MCU 'mcu' shutdown: Timer too close".into(),
        ..Default::default()
    })
    .unwrap();
    let c = client(&server, None);
    let probe = c.probe().unwrap();
    assert_eq!(probe.klippy_state(), KlippyState::Shutdown);
    let err = probe.require_ready().unwrap_err();
    assert!(err.detail().contains("Timer too close"), "{err}");
    assert!(matches!(c.status(), Err(MoonrakerError::KlippyNotReady { .. })));
    let file = gcode(1000);
    let start = UploadOptions { start: true, overwrite: true };
    assert!(matches!(
        c.upload(file.path(), "x.gcode", start, &mut |_, _| {}, &AtomicBool::new(false)),
        Err(MoonrakerError::KlippyNotReady { state: KlippyState::Shutdown, .. })
    ));
    // Лише завантажити — можна й без Klippy.
    c.upload(file.path(), "x.gcode", UploadOptions::default(), &mut |_, _| {}, &AtomicBool::new(false)).unwrap();

    server.with(|s| s.klippy_state = "disconnected".into());
    let probe = c.probe().unwrap();
    assert!(!probe.server.klippy_connected && probe.printer.is_none());
    assert_eq!(probe.klippy_state(), KlippyState::Disconnected);
}

#[test]
fn connection_drop_during_upload_is_interrupted() {
    let server = FakeServer::start(FakeState { drop_upload_after: Some(64 * 1024), ..Default::default() }).unwrap();
    let c = client(&server, None);
    let file = gcode(8 << 20);
    let err = c
        .upload(file.path(), "big.gcode", UploadOptions::default(), &mut |_, _| {}, &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(err, MoonrakerError::Interrupted(_)), "{err:?}");
    assert!(server.with(|s| s.uploads.is_empty()));
}

#[test]
fn cancelled_upload_stops_and_unreachable_is_typed() {
    let server = FakeServer::start(FakeState::default()).unwrap();
    let c = client(&server, None);
    let file = gcode(1 << 20);
    let cancel = AtomicBool::new(true);
    assert_eq!(
        c.upload(file.path(), "x.gcode", UploadOptions::default(), &mut |_, _| {}, &cancel),
        Err(MoonrakerError::Cancelled)
    );
    let addr = server.address();
    drop(server);
    let gone = Client::new(Endpoint::parse(&addr).unwrap(), None);
    assert!(matches!(gone.probe(), Err(MoonrakerError::Unreachable(_))));
}

#[test]
fn webcam_snapshot_is_fetched() {
    let jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3, 0xFF, 0xD9];
    let server = FakeServer::start(FakeState {
        snapshot: Some(jpeg.clone()),
        api_key: Some(KEY.into()),
        ..Default::default()
    })
    .unwrap();
    let c = client(&server, Some(KEY));
    let cams = c.webcams().unwrap();
    assert_eq!(cams.len(), 1);
    assert_eq!(c.snapshot(&cams[0]).unwrap(), jpeg);
    // Відносний URL: той самий хост; порт не 7125 — лишається порт Moonraker.
    server.with(|s| s.relative_snapshot = true);
    let cams = c.webcams().unwrap();
    let (endpoint, path, same) = c.snapshot_target(&cams[0]).unwrap();
    assert_eq!((endpoint.port, path.as_str(), same), (server.addr.port(), "/webcam/?action=snapshot", true));
    assert_eq!(c.snapshot(&cams[0]).unwrap(), jpeg);
    assert_eq!(c.web_url(), format!("http://127.0.0.1:{}/", server.addr.port()));

    let none = FakeServer::start(FakeState::default()).unwrap();
    assert!(client(&none, None).webcams().unwrap().is_empty());
}

fn tiny_jpeg(tag: u8) -> Vec<u8> {
    vec![0xFF, 0xD8, 0xFF, 0xE0, tag, 0x00, 0xFF, 0xD9]
}

#[test]
fn controls_send_expected_gcode() {
    use moonraker::{Axis, Command, HeaterKind, Home};
    let server = FakeServer::start(FakeState::default()).unwrap();
    let c = client(&server, None);
    c.run(&[Command::Heater(HeaterKind::Nozzle, 215), Command::Heater(HeaterKind::Bed, 60)]).unwrap();
    c.run(&[Command::Home(Home::All)]).unwrap();
    c.run(&[Command::Home(Home::Xy)]).unwrap();
    c.run(&[Command::Home(Home::Z)]).unwrap();
    c.run(&[Command::Jog(Axis::X, 10.0)]).unwrap();
    c.run(&[Command::Jog(Axis::Y, 0.1)]).unwrap();
    c.run(&[Command::Jog(Axis::Z, 1.0)]).unwrap();
    c.run(&[Command::PartFan(50)]).unwrap();
    c.run(&[Command::Raw("M117 Привіт & 1+1".into())]).unwrap();
    let scripts = server.with(|s| s.scripts.clone());
    assert_eq!(
        scripts,
        vec![
            "SET_HEATER_TEMPERATURE HEATER=extruder TARGET=215\nSET_HEATER_TEMPERATURE HEATER=heater_bed TARGET=60",
            "G28",
            "G28 X Y",
            "G28 Z",
            "G91\nG1 X10 F3000\nG90",
            "G91\nG1 Y0.1 F3000\nG90",
            "G91\nG1 Z1 F600\nG90",
            "M106 S128",
            "M117 Привіт & 1+1",
        ]
    );
    let s = c.status().unwrap();
    assert_eq!((s.extruder.target, s.bed.target), (Some(215.0), Some(60.0)));
    assert_eq!(s.homed_axes, "xyz");
    assert_eq!(s.position, Some([10.0, 0.1, 1.0]));
    assert!((s.part_fan.unwrap() - 128.0 / 255.0).abs() < 1e-9);

    // Межі перевіряються до мережі: у сервер нічого не йде.
    let before = server.with(|s| s.scripts.len());
    assert!(matches!(
        c.run(&[Command::Heater(HeaterKind::Nozzle, 900)]),
        Err(MoonrakerError::InvalidCommand(_))
    ));
    assert_eq!(server.with(|s| s.scripts.len()), before);
}

#[test]
fn klipper_errors_become_gcode_errors_and_estop_shuts_down() {
    use moonraker::{Axis, Command};
    let server = FakeServer::start(FakeState::default()).unwrap();
    let c = client(&server, None);
    match c.run(&[Command::Jog(Axis::X, 1.0)]) {
        Err(MoonrakerError::Gcode(m)) => assert!(m.contains("Must home axis first"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(c.gcode("FOO_BAR"), Err(MoonrakerError::Gcode(_))));

    c.emergency_stop().unwrap();
    assert_eq!(server.with(|s| s.emergency_stops), 1);
    assert_eq!(c.probe().unwrap().klippy_state(), KlippyState::Shutdown);
    // У shutdown Moonraker G-code не приймає.
    assert!(matches!(c.gcode("G28"), Err(MoonrakerError::KlippyNotReady { .. })));
    c.firmware_restart().unwrap();
    assert_eq!(c.probe().unwrap().klippy_state(), KlippyState::Ready);
}

#[test]
fn mjpeg_stream_yields_frames_and_falls_back_cleanly() {
    let server = FakeServer::start(FakeState {
        snapshot: Some(tiny_jpeg(1)),
        stream_interval: std::time::Duration::from_millis(20),
        ..Default::default()
    })
    .unwrap();
    let c = client(&server, None);
    let cams = c.webcams().unwrap();
    let mut stream = c.open_stream(&cams[0], std::time::Duration::from_secs(2)).unwrap();
    assert_eq!(stream.next_frame().unwrap(), tiny_jpeg(1));
    server.with(|s| s.snapshot = Some(tiny_jpeg(2)));
    let mut got = Vec::new();
    for _ in 0..20 {
        got = stream.next_frame().unwrap();
        if got == tiny_jpeg(2) {
            break;
        }
    }
    assert_eq!(got, tiny_jpeg(2));
    drop(stream);

    // Без потоку — помилка відкриття, знімок лишається.
    server.with(|s| s.stream = false);
    assert!(c.open_stream(&cams[0], std::time::Duration::from_secs(2)).is_err());
    assert_eq!(c.snapshot(&cams[0]).unwrap(), tiny_jpeg(2));
    // Сторінка замість MJPEG (WebRTC-камера) — NotMjpeg.
    let mut html = cams[0].clone();
    html.stream_url = html.snapshot_url.replace("action=snapshot", "action=info");
    assert!(c.open_stream(&html, std::time::Duration::from_secs(2)).is_err());
}

#[test]
fn mdns_discovery_against_fake_responder() {
    use moonraker::{fake::FakeMdns, mdns};
    let found = mdns::Found {
        instance: "Moonraker Instance on fake-klipper".into(),
        host: "fake-klipper.local".into(),
        port: 7125,
        addresses: vec!["127.0.0.1".parse().unwrap()],
    };
    let responder = FakeMdns::start("127.0.0.1:0".parse().unwrap(), vec![found.clone()]).unwrap();
    let list = mdns::discover_at(responder.addr, std::time::Duration::from_millis(600)).unwrap();
    assert_eq!(list, vec![found]);
    assert_eq!(list[0].address(), "127.0.0.1:7125");
    drop(responder);
}
