//! Покоління 02.08.04 через справжній шим: макет PrintParams за значенням, вхід
//! токенами у власну теку, квиток SSO, локальний вихід. Окремий процес: агент один на процес.
// Шим і фейковий плагін збираються лише там, де є шим (див. build.rs).
#![cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(windows, target_arch = "x86_64", target_env = "msvc")
))]
mod native;
use cloud::{Client, DeviceId, Event, Options, PrintRequest, Tokens};
use std::{path::Path, time::Duration};

fn fixture(dir: &Path, version: &str, defines: &[&str]) -> std::path::PathBuf {
    let mut defines: Vec<String> = defines.iter().map(|d| d.to_string()).collect();
    defines.push(format!("-DFAKE_VERSION=\"{version}\""));
    native::shared_library(
        "fake_plugin.cpp",
        &dir.join(native::library_name(&format!("fake-{version}"))),
        &defines,
    )
}

#[test]
fn current_generation_prints_logs_in_and_out_through_the_shim() {
    let dir = tempfile::tempdir().unwrap();
    // Непідтримувана версія — зрозуміла помилка з номером, без створення агента.
    let legacy = fixture(dir.path(), "01.10.01.01", &[]);
    assert_eq!(cloud::read_plugin_version(&legacy).unwrap(), "01.10.01.01");
    let own = dir.path().join("own-session");
    let options = |plugin| Options {
        plugin_path: plugin,
        studio_config_dir: own.clone(),
        country_code: "US".into(),
        certificate: None,
        allow_control: true,
        http_headers: Vec::new(),
        persistent_session: true,
    };
    let error = Client::load(options(legacy)).err().unwrap().to_string();
    assert!(
        error.contains("01.10.01.01") && error.contains("02.08.04"),
        "{error}"
    );

    let library = fixture(dir.path(), "02.08.04.60", &["-DFAKE_0208"]);
    let (client, mut events) = Client::load(options(library)).unwrap();
    assert_eq!(client.plugin_version().unwrap(), "02.08.04.60");
    client.start().unwrap();
    assert!(!client.is_user_login().unwrap());
    let tokens = client.tokens_from_ticket("syn-ticket").unwrap();
    assert!(client.tokens_from_ticket("bad").is_err());
    let wrong = Tokens {
        access_token: "other".into(),
        ..tokens.clone()
    };
    let error = client.login_with_tokens(&wrong).unwrap_err().to_string();
    assert!(error.contains("-11401") && !error.contains("other"));
    client.login_with_tokens(&tokens).unwrap();
    assert!(client.is_user_login().unwrap());
    // Плагін пише сесію у власну теку, яка переживає Client.
    assert!(own.join("BambuNetworkEngine.conf").is_file());

    let gcode = dir.path().join("plate.gcode.3mf");
    let config = dir.path().join("plate.config.3mf");
    std::fs::write(&gcode, "x").unwrap();
    std::fs::write(&config, "x").unwrap();
    let mut request = PrintRequest::new(
        DeviceId::new("SYNTHETIC12345678"),
        gcode,
        config,
        "synthetic".into(),
    );
    request.plate_index = 2;
    request.use_ams = true;
    request.bed_type = "textured_plate".into();
    request.extra_options = "extra".into();
    client.start_print(&request).unwrap();
    let mut progress = None;
    for _ in 0..50 {
        if let Ok(Event::PrintProgress { stage, .. }) =
            events.recv_timeout(Duration::from_millis(50))
        {
            progress = Some(stage);
            break;
        }
    }
    assert_eq!(progress, Some(7));

    client.logout().unwrap();
    assert!(!client.is_user_login().unwrap());
    client.shutdown().unwrap();
    assert!(own.join("BambuNetworkEngine.conf").is_file());
}
