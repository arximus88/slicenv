//! Перевірка межі C++/Rust без справжнього плагіна або мережі.
// Шим і фейковий плагін збираються лише там, де є шим (див. build.rs).
#![cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(windows, target_arch = "x86_64", target_env = "msvc")
))]
mod native;
use cloud::{CameraStream, Client, DeviceId, Event, Options, TaskQuery};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

fn fixture(dir: &Path, version: &str, name: &str) -> PathBuf {
    native::shared_library(
        "fake_plugin.cpp",
        &dir.join(native::library_name(name)),
        &[format!("-DFAKE_VERSION=\"{version}\"")],
    )
}
fn options(plugin_path: PathBuf, dir: &Path) -> Options {
    Options {
        plugin_path,
        studio_config_dir: dir.to_owned(),
        country_code: "US".into(),
        certificate: None,
        allow_control: false,
        http_headers: Vec::new(),
        persistent_session: false,
    }
}

#[test]
fn version_gate_isolation_and_late_callbacks_cross_the_real_shim() {
    let dir = tempfile::tempdir().unwrap();
    let unknown = fixture(dir.path(), "99.99.99.99", "unknown");
    let error = Client::load(options(unknown, dir.path()))
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("-10002"));
    assert!(!dir.path().join("test.marker").exists());

    let library = fixture(dir.path(), "02.06.00.50", "known");
    let (client, mut events) = Client::load(options(library.clone(), dir.path())).unwrap();
    assert_eq!(client.plugin_version().unwrap(), "02.06.00.50");
    assert!(client.private_dir().join("config/test.marker").is_file());
    assert!(!dir.path().join("test.marker").exists());
    assert!(Client::load(options(library, dir.path())).is_err());
    assert!(client.connect_server().is_err()); // Агент ще не запущений.
    client.start().unwrap();
    assert!(client.start().is_err());
    assert!(client.connect_server().is_err()); // Немає сертифіката: виклик не доходить до плагіна.
    let query = TaskQuery {
        dev_id: Some(DeviceId::new("SYNTHETIC12345678")),
        status: 2,
        offset: 7,
        limit: 5,
    };
    let tasks = client.get_user_tasks(&query).unwrap();
    assert_eq!(tasks[0].id, "101");
    assert_eq!(tasks[0].cost_time, Some(100.0));
    assert_eq!(tasks[0].weight, Some(2.5));
    assert_eq!(
        client.get_subtask_info("101").unwrap().as_value()["id"],
        101
    );
    let error = client
        .get_subtask_info("private-id")
        .unwrap_err()
        .to_string();
    assert!(error.contains("-11403") && !error.contains("private"));
    assert!(
        client
            .get_user_tasks(&TaskQuery { limit: 0, ..query })
            .is_err()
    );
    native::shared_library(
        "fake_source.cpp",
        &dir.path().join(cloud::plugin::SOURCE_LIBRARY),
        &[],
    );
    let mut camera = CameraStream::open(&client, &DeviceId::new("SYNTHETIC12345678")).unwrap();
    let first = camera.next_frame().unwrap();
    let second = camera.next_frame().unwrap();
    assert_eq!(
        (first.width, first.height, first.timestamp),
        (640, 480, 2_000_000)
    );
    assert_eq!(second.width, 576);
    assert_eq!(first.jpeg[16], 128); // Дані не позичені з native-буфера.
    // Повна черга не повинна блокувати stop; повторний stop безпечний.
    std::thread::sleep(Duration::from_millis(100));
    camera.stop().unwrap();
    camera.stop().unwrap();
    drop(camera);
    // Receiver можна обробляти поза потоком агента.
    let receiver = std::thread::spawn(move || {
        let event = events.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(event, Event::Status { status, .. } if status.bed_temper == Some(25.0)));
        events
    });
    let mut events = receiver.join().unwrap();
    let private = client.private_dir().to_owned();
    client.shutdown().unwrap();
    assert!(!private.exists());
    std::thread::sleep(Duration::from_millis(100));
    assert!(matches!(
        events.recv_timeout(Duration::from_millis(10)),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
    ));
}
