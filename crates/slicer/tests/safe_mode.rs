//! Сумісний режим через справжній `slice_with_engine` і фейкові рушії (shell-скрипти):
//! аварія на `--export-3mf` (як OrcaSlicer 2.3.0 на Windows), аварія завжди, старий рушій без
//! `--export-3mf`. Окремий тестовий процес: тека кешу підміняється змінною оточення.
#![cfg(unix)]
use slicer::{
    COMPATIBLE_NOTE, Engine, EngineKind, Overrides, PresetStore, SAFE_MODE_FAILED_HINT, Selection,
    SliceFailure, SliceRequest, engine_compat, slice_with_engine,
};
use std::{fs, os::unix::fs::PermissionsExt as _, path::Path};

const FULL_HELP: &str = "--slice --outputdir --load-settings --load-filaments --load-defaultfila --orient --arrange --export-3mf --debug --allow-newer-file";

/// `crash`: `export` — аварія лише з `--export-3mf`; `always` — завжди; `never` — ніколи.
fn engine(dir: &Path, name: &str, help: &str, crash: &str) -> (Engine, std::path::PathBuf) {
    let calls = dir.join(format!("{name}.calls"));
    let script = dir.join(name);
    fs::write(
        &script,
        format!(
            r#"#!/bin/sh
if [ "$1" = "--help" ]; then printf 'OrcaSlicer-01.10.01.50\n {help}\n'; exit 0; fi
printf '%s\n' "$*" >> '{calls}'
plate=1; prev=
for a in "$@"; do
  if [ "$prev" = "--slice" ]; then plate=$a; fi
  if [ "{crash}" = always ] || {{ [ "{crash}" = export ] && [ "$a" = "--export-3mf" ]; }}; then
    printf 'partial' > plate_$plate.gcode
    kill -SEGV $$
  fi
  prev=$a
done
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

fn calls(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn crashing_engine_is_sliced_in_safe_mode_and_remembered() {
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: один тест у процесі, інших потоків ще немає.
    unsafe {
        std::env::set_var("XDG_CACHE_HOME", dir.path().join("cache"));
        std::env::set_var("HOME", dir.path().join("home"));
    }
    let store = store(dir.path());
    let model = dir.path().join("part.stl");
    fs::write(&model, "solid part\nendsolid part\n").unwrap();
    let request = SliceRequest {
        input: model,
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

    // 1. Падає лише з --export-3mf: звичайна → без назв пластин → лише G-code.
    let (crashy, log) = engine(dir.path(), "orca-export-crash", FULL_HELP, "export");
    let out = slice_with_engine(&crashy, &store, &request).unwrap();
    let runs = calls(&log);
    assert_eq!(runs.len(), 3, "{runs:#?}");
    assert!(runs[..2].iter().all(|r| r.contains("--export-3mf")));
    assert!(!runs[2].contains("--export-3mf"), "{}", runs[2]);
    assert_eq!(out.compatibility_note, Some(COMPATIBLE_NOTE));
    assert_eq!(out.print_time.as_secs(), 60);
    assert_eq!(out.filament_grams, 1.25);
    assert_eq!(out.layer_height, 0.2);
    assert!(
        out.gcode_3mf
            .to_string_lossy()
            .ends_with("part-plate1.gcode.3mf")
    );
    let archive = zip::ZipArchive::new(fs::File::open(&out.gcode_3mf).unwrap()).unwrap();
    assert!(
        archive
            .file_names()
            .any(|n| n.is_ok_and(|n| n == "Metadata/plate_1.gcode"))
    );
    let identity = crashy.probe().unwrap().identity;
    assert_eq!(
        engine_compat(&identity).stage,
        Some(slicer::Stage::GcodeOnly)
    );

    // 2. Повторна нарізка одразу в сумісному режимі: без нових аварій.
    let out = slice_with_engine(&crashy, &store, &request).unwrap();
    assert_eq!(calls(&log).len(), 4);
    assert!(!calls(&log)[3].contains("--export-3mf"));
    assert_eq!(out.compatibility_note, Some(COMPATIBLE_NOTE));

    // 3. Падає й без --export-3mf: зрозуміла помилка з м’якою порадою, факт запам’ятовано.
    let (broken, log) = engine(dir.path(), "orca-always-crash", FULL_HELP, "always");
    let error = slice_with_engine(&broken, &store, &request).unwrap_err();
    let failure = error.downcast_ref::<SliceFailure>().expect("SliceFailure");
    assert!(failure.crashed && failure.signal == Some(11));
    assert_eq!(failure.hint, SAFE_MODE_FAILED_HINT);
    assert!(failure.safe_mode_exhausted);
    // Рушій упав, нічого не повідомивши: без вигаданої «невідомої помилки».
    assert!(failure.reason.is_empty());
    assert!(
        failure.to_string().starts_with("orca не нарізав: аварійно завершився до того, як повідомив причину"),
        "{failure}"
    );
    assert_eq!(calls(&log).len(), 3);
    let identity = broken.probe().unwrap().identity;
    assert!(engine_compat(&identity).crashes_in_safe_mode);

    // 4. Старий рушій без --export-3mf і --allow-newer-file: нарізає без них, без примітки.
    let (old, log) = engine(
        dir.path(),
        "orca-old",
        "--slice --outputdir --load-settings --load-filaments",
        "never",
    );
    let out = slice_with_engine(&old, &store, &request).unwrap();
    let runs = calls(&log);
    assert_eq!(runs.len(), 1);
    assert!(!runs[0].contains("--export-3mf") && !runs[0].contains("--allow-newer-file"));
    assert!(!runs[0].contains("--debug"), "{}", runs[0]);
    assert_eq!(out.compatibility_note, None);
    assert!(out.gcode_3mf.is_file());
}
