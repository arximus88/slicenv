//! `SliceCancellation::gcode_ready`: G-code пластини видно, щойно рушій його записав, ще до
//! виходу процесу (Preview не чекає на експорт `.gcode.3mf`). Окремий тестовий процес: тека
//! кешу підміняється змінною оточення.
#![cfg(unix)]
use slicer::{
    Engine, EngineKind, Overrides, PresetStore, Selection, SliceCancellation, SliceRequest,
    slice_with_engine_cancellable,
};
use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    time::{Duration, Instant},
};

#[test]
fn gcode_is_ready_before_the_engine_exits() {
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: один тест у процесі, інших потоків ще немає.
    unsafe {
        std::env::set_var("XDG_CACHE_HOME", dir.path().join("cache"));
        std::env::set_var("HOME", dir.path().join("home"));
    }
    // Як Bambu/Orca: `plate_N.gcode.tmp` → перейменування, далі ще «експорт» (пауза).
    let script = dir.path().join("orca");
    fs::write(
        &script,
        r#"#!/bin/sh
if [ "$1" = "--help" ]; then printf 'OrcaSlicer-01.10.01.50\n --slice --outputdir --load-settings --load-filaments\n'; exit 0; fi
cat > plate_1.gcode.tmp <<'G'
; HEADER_BLOCK_START
; model printing time: 50s; total estimated time: 1m 0s
; total layer number: 10
; HEADER_BLOCK_END
G1 X1 Y1
; filament used [g] = 1.25
; layer_height = 0.2
G
mv plate_1.gcode.tmp plate_1.gcode
sleep 1
exit 0
"#,
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let engine = Engine {
        kind: EngineKind::Orca,
        path: script,
    };
    for (kind, name) in [("machine", "m"), ("process", "p"), ("filament", "f")] {
        let path = dir.path().join("profiles/BBL").join(kind);
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join(format!("{name}.json")),
            format!(r#"{{"name":"{name}","type":"{kind}","filament_settings_id":["{name}"]}}"#),
        )
        .unwrap();
    }
    let store =
        PresetStore::open_with_own_root(&dir.path().join("profiles"), &dir.path().join("own"))
            .unwrap();
    let model = dir.path().join("part.stl");
    fs::write(&model, "solid part\nendsolid part\n").unwrap();
    let request = SliceRequest {
        input: model,
        plate: 1,
        allow_newer_file: false,
        selection: Selection {
            machine: "m".into(),
            process: "p".into(),
            filament: "f".into(),
            bed_type: "Textured PEI Plate".into(),
        },
        overrides: Overrides::default(),
    };

    let token = SliceCancellation::default();
    assert_eq!(token.gcode_ready(), None);
    let job = {
        let token = token.clone();
        std::thread::spawn(move || slice_with_engine_cancellable(&engine, &store, &request, &token))
    };
    let started = Instant::now();
    let ready = loop {
        if let Some(path) = token.gcode_ready() {
            break path;
        }
        assert!(!job.is_finished(), "нарізка завершилась, а G-code так і не став готовим");
        assert!(started.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(!job.is_finished(), "G-code має бути видно ще до виходу рушія");
    assert!(fs::read_to_string(&ready).unwrap().contains("total layer number: 10"));
    let out = job.join().unwrap().unwrap();
    assert_eq!(ready, out.gcode_3mf.with_file_name("plate_1.gcode"));
}
