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
