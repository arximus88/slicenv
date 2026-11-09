//! Остання стадія сумісного режиму через справжній `slice_with_engine`: рушій падає, поки в
//! 3MF є налаштування проєкту (як OrcaSlicer 2.3.0 на проєкті Bambu Studio 2.7 для X1C), і
//! нарізає копію, де конфіг проєкту замінено конфігом із профілів Slicenv. Окремий тестовий процес: тека кешу підміняється змінною оточення.
#![cfg(unix)]
use slicer::{
    Engine, EngineKind, NO_PROJECT_SETTINGS_NOTE, Overrides, PresetStore, Selection, SliceRequest,
    Stage, engine_compat, slice_with_engine,
};
use std::{
    fs,
    io::{Read as _, Write as _},
    os::unix::fs::PermissionsExt as _,
    path::Path,
};

const HELP: &str = "--slice --outputdir --load-settings --load-filaments --load-defaultfila --orient --arrange --export-3mf --debug --allow-newer-file";

fn engine(dir: &Path) -> (Engine, std::path::PathBuf) {
    let calls = dir.join("orca.calls");
    let script = dir.join("orca-project-crash");
    fs::write(
        &script,
        format!(
            r#"#!/bin/sh
if [ "$1" = "--help" ]; then printf 'OrcaSlicer-02.03.00.70\n {HELP}\n'; exit 0; fi
printf '%s\n' "$*" >> '{calls}'
plate=1; prev=; input=
for a in "$@"; do
  if [ "$prev" = "--slice" ]; then plate=$a; fi
  prev=$a; input=$a
done
if grep -q 'X1C-PROJECT-CONFIG' "$input"; then kill -SEGV $$; fi
cat > plate_$plate.gcode <<'G'
; HEADER_BLOCK_START
; model printing time: 50s; total estimated time: 1m 0s
; total layer number: 10
; HEADER_BLOCK_END
G1 X1 Y1
; filament used [g] = 1.25
; layer_height = 0.2
G
exit 0
"#,
            calls = calls.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    (
        Engine {
            kind: EngineKind::Orca,
            path: script,
        },
        calls,
    )
}

fn store(dir: &Path) -> PresetStore {
    for (kind, name) in [("machine", "m"), ("process", "p"), ("filament", "f")] {
        let path = dir.join("profiles/BBL").join(kind);
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join(format!("{name}.json")),
            format!(r#"{{"name":"{name}","type":"{kind}","filament_settings_id":["{name}"]}}"#),
        )
        .unwrap();
    }
    PresetStore::open_with_own_root(&dir.join("profiles"), &dir.join("own")).unwrap()
}

const MODEL_SETTINGS: &str = r#"<config><object id="1"><metadata key="wall_loops" value="5"/></object><plate><metadata key="plater_id" value="1"/></plate></config>"#;

/// Fixture-3MF: модель, налаштування об’єкта й пластини, конфіг проєкту на два філаменти,
/// вбудований пресет процесу.
fn project(path: &Path) {
    let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
    // Без стиснення: фейковий рушій шукає маркер конфігу проєкту в байтах архіву.
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored);
    for (name, bytes) in [
        ("3D/3dmodel.model", "<model/>"),
        ("Metadata/model_settings.config", MODEL_SETTINGS),
        ("Metadata/layer_config_ranges.xml", "<objects/>"),
        (
            "Metadata/project_settings.config",
            r##"{"printer_settings_id":"Bambu Lab X1 Carbon 0.4 nozzle","marker":"X1C-PROJECT-CONFIG","filament_settings_id":["a","b"],"filament_colour":["#FF0000","#00FF00"]}"##,
        ),
        ("Metadata/process_settings_1.config", r#"{"name":"mine"}"#),
    ] {
        zip.start_file(name, options).unwrap();
        zip.write_all(bytes.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

#[test]
fn engine_crashing_on_project_settings_slices_without_them() {
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: один тест у процесі, інших потоків ще немає.
    unsafe {
        std::env::set_var("XDG_CACHE_HOME", dir.path().join("cache"));
        std::env::set_var("HOME", dir.path().join("home"));
    }
    let store = store(dir.path());
    let model = dir.path().join("x1c.3mf");
    project(&model);
    let original = fs::read(&model).unwrap();
    let request = SliceRequest {
        input: model.clone(),
        plate: 1,
        allow_newer_file: true,
        selection: Selection {
            machine: "m".into(),
            process: "p".into(),
            filament: "f".into(),
            bed_type: "Textured PEI Plate".into(),
        },
        overrides: Overrides::default(),
    };
    let (orca, log) = engine(dir.path());
    let out = slice_with_engine(&orca, &store, &request).unwrap();
    let runs: Vec<String> = fs::read_to_string(&log).unwrap().lines().map(str::to_owned).collect();
    // Звичайна → без назв пластин → лише G-code → без налаштувань проєкту.
    assert_eq!(runs.len(), 4, "{runs:#?}");
    let last = &runs[3];
    assert!(!last.contains("--export-3mf"), "{last}");
    assert!(last.contains("input-bare"), "{last}");
    // Слоти філаменту — з повного проєкту, хоч у копії конфігу вже немає.
    assert!(last.contains("filament.json;filament.json"), "{last}");
    assert_eq!(out.compatibility_note, Some(NO_PROJECT_SETTINGS_NOTE));
    assert_eq!(fs::read(&model).unwrap(), original, "файл користувача не змінюється");

    // Копія, яку нарізав рушій: модель і налаштування об’єкта на місці, конфігу проєкту немає.
    let bare = last.rsplit(' ').next().unwrap();
    let mut archive = zip::ZipArchive::new(fs::File::open(bare).unwrap()).unwrap();
    let mut settings = String::new();
    archive
        .by_name("Metadata/model_settings.config")
        .unwrap()
        .read_to_string(&mut settings)
        .unwrap();
    assert_eq!(settings, MODEL_SETTINGS);
    assert!(archive.by_name("3D/3dmodel.model").is_ok());
    assert!(archive.by_name("Metadata/layer_config_ranges.xml").is_ok());
    // Конфіг проєкту — з профілів Slicenv (без маркера проєкту), а не прибраний: без нього
    // справжні Orca 2.4.2 і Bambu 2.7 CLI падають.
    let config: serde_json::Value =
        serde_json::from_reader(archive.by_name("Metadata/project_settings.config").unwrap()).unwrap();
    assert!(config.get("marker").is_none(), "{config}");
    assert_eq!(config["curr_bed_type"], "Textured PEI Plate");
    assert!(archive.by_name("Metadata/process_settings_1.config").is_err());

    // Стадію запам’ятовано: наступна нарізка одразу без налаштувань проєкту, без аварій.
    let identity = orca.probe().unwrap().identity;
    let compat = engine_compat(&identity);
    assert_eq!(compat.stage, Some(Stage::NoProjectSettings));
    assert!(!compat.crashes_in_safe_mode);
    let out = slice_with_engine(&orca, &store, &request).unwrap();
    assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 5);
    assert_eq!(out.compatibility_note, Some(NO_PROJECT_SETTINGS_NOTE));
}
