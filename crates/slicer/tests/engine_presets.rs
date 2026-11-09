//! Регресія коректності на справжніх рушіях: нарізка через Slicenv (`machine.json`,
//! `process.json`, `filament.json`) дає ту саму вагу й час, що й рушій на 3MF із системними
//! пресетами без Slicenv (як у GUI), а CONFIG_BLOCK у G-code відповідає вибраним пресетам.
//!
//! Раніше службові батьки з однаковими іменами різних вендорів (`fdm_machine_common` Anker)
//! підмінялися: Bambu 2.7 різав сферу 11,4 г як 1,6 кг, Orca 2.5 — з 3 стінками й 210 °C.
//!
//! Рушії: керовані Slicenv (`~/.local/share/slicenv/engines/{bambu,orca}/*/AppRun`, лише
//! запуск), `/usr/bin/bambustudio`, `/usr/bin/orca-slicer`, або явно
//! `SLICENV_REGRESSION_ENGINES="bambu=/path/AppRun;orca=/path/AppRun"`. Немає рушія — тест
//! пропускається. JSON-профілі: `resources/profiles` поруч із рушієм, вміст AppImage
//! (`--appimage-extract` у тимчасову теку) або `SLICENV_REGRESSION_PROFILES=<тека>` з
//! `<family>-<версія>/BBL.json` (для збірок лише з `.opc`, як Orca 2.5). Фікстури —
//! `tools/bench/fixture_3mf.py` (потрібен `python3`). Усі процеси — з тимчасовим HOME/XDG_*.
#![cfg(target_os = "linux")]

use slicer::{Engine, EngineKind, Kind, Overrides, PresetStore, Selection, SliceRequest};
use std::{
    collections::BTreeMap,
    fs,
    io::Read as _,
    path::{Path, PathBuf},
    process::Command,
};

const MACHINE: &str = "Bambu Lab A1 0.4 nozzle";
const PROCESS: &str = "0.20mm Standard @BBL A1";
const FILAMENT: &str = "Bambu PLA Basic @BBL A1";

/// Ключі, які мають збігатися з розгорнутими пресетами.
const KEYS: &[(Kind, &str)] = &[
    (Kind::Process, "layer_height"),
    (Kind::Process, "initial_layer_print_height"),
    (Kind::Process, "wall_loops"),
    (Kind::Process, "sparse_infill_density"),
    (Kind::Process, "sparse_infill_pattern"),
    (Kind::Process, "top_shell_layers"),
    (Kind::Process, "bottom_shell_layers"),
    (Kind::Process, "line_width"),
    (Kind::Process, "outer_wall_line_width"),
    (Kind::Process, "sparse_infill_line_width"),
    (Kind::Process, "skirt_loops"),
    (Kind::Machine, "nozzle_diameter"),
    (Kind::Machine, "use_relative_e_distances"),
    (Kind::Machine, "gcode_flavor"),
    (Kind::Machine, "printable_area"),
    (Kind::Filament, "nozzle_temperature"),
    (Kind::Filament, "filament_flow_ratio"),
    (Kind::Filament, "filament_diameter"),
    (Kind::Filament, "filament_density"),
];

/// Відрізняються від еталона з причин поза пресетами: рушій вимикає prime tower для одного
/// філаменту, сумісність і id пресетів пише лише для `--load-settings`, а частину ключів
/// хостів/плагінів Orca 2.5 у CONFIG_BLOCK додає тільки з 3MF-проєкту.
const IGNORED: &[&str] = &[
    "compatible_printers_condition",
    "different_settings_to_system",
    "enable_prime_tower",
    "filament_ids",
    "filament_prime_volume",
    "filament_self_index",
    "machine_max_junction_deviation",
    "print_compatible_printers",
];

struct Target {
    label: String,
    kind: EngineKind,
    path: PathBuf,
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn targets(real_home: &Path) -> Vec<Target> {
    let mut out = Vec::new();
    let mut push = |kind, path: PathBuf| {
        if path.is_file() && !out.iter().any(|t: &Target| t.path == path) {
            let label = format!("{} {}", kind_name(kind), path.display());
            out.push(Target { label, kind, path });
        }
    };
    if let Ok(list) = std::env::var("SLICENV_REGRESSION_ENGINES") {
        for item in list.split(';').filter(|s| !s.is_empty()) {
            let (kind, path) = item.split_once('=').expect("kind=path");
            push(if kind == "orca" { EngineKind::Orca } else { EngineKind::Bambu }, PathBuf::from(path));
        }
        return out;
    }
    for (kind, family) in [(EngineKind::Bambu, "bambu"), (EngineKind::Orca, "orca")] {
        let dir = real_home.join(".local/share/slicenv/engines").join(family);
        let mut versions: Vec<_> = fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
        versions.sort();
        for version in versions {
            push(kind, version.join("AppRun"));
        }
    }
    push(EngineKind::Bambu, PathBuf::from("/usr/bin/bambustudio"));
    push(EngineKind::Orca, PathBuf::from("/usr/bin/orca-slicer"));
    out
}

fn kind_name(kind: EngineKind) -> &'static str {
    match kind {
        EngineKind::Bambu => "bambu",
        EngineKind::Orca => "orca",
    }
}

fn json_tree(root: &Path) -> bool {
    root.join("BBL.json").is_file() && root.join("BBL/machine").is_dir()
}

/// JSON-каталог профілів саме цієї збірки рушія (див. заголовок модуля).
fn profiles(target: &Target, version: &str, scratch: &Path) -> Option<PathBuf> {
    let dir = fs::canonicalize(&target.path).ok()?.parent()?.to_owned();
    let packaged = dir.join("resources/profiles");
    if json_tree(&packaged) {
        return Some(packaged);
    }
    if let Some(library) = std::env::var_os("SLICENV_REGRESSION_PROFILES") {
        let root = Path::new(&library).join(format!("{}-{version}", kind_name(target.kind)));
        if json_tree(&root) {
            return Some(root);
        }
    }
    // AppImage без розпакованих ресурсів поруч (наприклад /usr/bin/bambustudio).
    let out = scratch.join("appimage");
    fs::create_dir_all(&out).ok()?;
    Command::new(&target.path)
        .args(["--appimage-extract", "resources/profiles/*"])
        .current_dir(&out)
        .output()
        .ok()?;
    let extracted = out.join("squashfs-root/resources/profiles");
    json_tree(&extracted).then_some(extracted)
}

fn fixture(kind: &str, out: &Path, profiles: &Path) -> bool {
    Command::new("python3")
        .arg("-I")
        .arg(repo().join("tools/bench/fixture_3mf.py"))
        .args([kind, &out.to_string_lossy(), "--profiles", &profiles.to_string_lossy()])
        .output()
        .is_ok_and(|o| o.status.success())
}

#[derive(Debug)]
struct Gcode {
    grams: f64,
    seconds: f64,
    config: BTreeMap<String, String>,
}

fn duration(text: &str) -> f64 {
    let mut total = 0.0;
    let mut number = String::new();
    for c in text.chars() {
        match c {
            '0'..='9' => number.push(c),
            'd' | 'h' | 'm' | 's' if !number.is_empty() => {
                let unit = match c { 'd' => 86400.0, 'h' => 3600.0, 'm' => 60.0, _ => 1.0 };
                total += number.parse::<f64>().unwrap() * unit;
                number.clear();
            }
            _ => number.clear(),
        }
    }
    total
}

fn parse(text: &str) -> Gcode {
    let after = |prefix: &str| text.lines().find_map(|l| l.split_once(prefix).map(|(_, v)| v.trim().to_owned()));
    let grams = after("; total filament weight [g] :")
        .or_else(|| after("; filament used [g] ="))
        .and_then(|v| v.split(',').next()?.trim().parse().ok())
        .expect("вага в G-code");
    let seconds = after("total estimated time:")
        .map(|v| v.split(';').next().unwrap_or_default().to_owned())
        .or_else(|| after("; estimated printing time (normal mode) ="))
        .map(|v| duration(&v))
        .expect("час у G-code");
    let block = &text[text.find("; CONFIG_BLOCK_START").expect("CONFIG_BLOCK")..];
    let config = block
        .lines()
        .take_while(|l| !l.starts_with("; CONFIG_BLOCK_END"))
        .filter_map(|l| l.strip_prefix("; ")?.split_once(" = "))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    Gcode { grams, seconds, config }
}

fn gcode_of_package(path: &Path) -> Gcode {
    let mut zip = zip::ZipArchive::new(fs::File::open(path).unwrap()).unwrap();
    let mut text = String::new();
    zip.by_name("Metadata/plate_1.gcode").unwrap().read_to_string(&mut text).unwrap();
    parse(&text)
}

/// Значення пресета так, як рушій пише його в CONFIG_BLOCK (масиви — через кому).
fn rendered(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) => items.iter().map(rendered).collect::<Vec<_>>().join(","),
        other => other.to_string(),
    }
}

/// Копія 3MF, де `project_settings.config` доповнено `extra` (решта записів — як є).
fn with_project_settings(input: &Path, output: &Path, extra: &serde_json::Map<String, serde_json::Value>) {
    use std::io::Write as _;
    const CONFIG: &str = "Metadata/project_settings.config";
    let mut archive = zip::ZipArchive::new(fs::File::open(input).unwrap()).unwrap();
    let mut config: serde_json::Map<String, serde_json::Value> =
        serde_json::from_reader(archive.by_name(CONFIG).unwrap()).unwrap();
    config.extend(extra.clone());
    let mut writer = zip::ZipWriter::new(fs::File::create(output).unwrap());
    for i in 0..archive.len() {
        let file = archive.by_index_raw(i).unwrap();
        if file.name().is_ok_and(|n| n == CONFIG) {
            drop(file);
            writer.start_file(CONFIG, zip::write::SimpleFileOptions::default()).unwrap();
            writer.write_all(&serde_json::to_vec_pretty(&config).unwrap()).unwrap();
        } else {
            writer.raw_copy_file(file).unwrap();
        }
    }
    writer.finish().unwrap();
}

fn close(label: &str, ours: f64, reference: f64) {
    let ratio = ours / reference;
    assert!((0.95..=1.05).contains(&ratio), "{label}: Slicenv {ours:.2} проти еталона {reference:.2} ({ratio:.3}×)");
}

#[test]
fn engine_slices_like_system_presets_without_slicenv() {
    let real_home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let targets = targets(&real_home);
    if targets.is_empty() || Command::new("python3").arg("-V").output().is_err() {
        eprintln!("пропущено: немає рушія Bambu/Orca або python3");
        return;
    }
    // Ні рушій, ні Slicenv не мають бачити справжній профіль користувача.
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let runtime = scratch.path().join("run");
    fs::create_dir_all(&runtime).unwrap();
    fs::set_permissions(&runtime, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    // SAFETY: єдиний тест у цьому бінарнику, інших потоків, що читають оточення, немає.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
        std::env::set_var("XDG_DATA_HOME", home.join(".local/share"));
        std::env::set_var("XDG_CACHE_HOME", home.join(".cache"));
        std::env::set_var("XDG_STATE_HOME", home.join(".local/state"));
        std::env::set_var("XDG_RUNTIME_DIR", &runtime);
        std::env::remove_var("SLICENV_PROFILE_ROOT");
        std::env::remove_var("ORCA_PROFILE_ROOT");
    }

    let mut checked = 0;
    for (index, target) in targets.iter().enumerate() {
        let engine = Engine { kind: target.kind, path: target.path.clone() };
        let Ok(info) = engine.probe() else {
            eprintln!("{}: рушій не відповідає на --help, пропущено", target.label);
            continue;
        };
        let version = PresetStore::engine_version(&engine);
        let work = scratch.path().join(format!("engine-{index}"));
        fs::create_dir_all(&work).unwrap();
        let Some(root) = profiles(target, &version, &work) else {
            eprintln!("{} {version}: немає JSON-профілів, пропущено", target.label);
            continue;
        };
        let variable = match target.kind {
            EngineKind::Bambu => "SLICENV_PROFILE_ROOT",
            EngineKind::Orca => "ORCA_PROFILE_ROOT",
        };
        // SAFETY: див. вище.
        unsafe { std::env::set_var(variable, &root) };
        let mut store = PresetStore::open_for_engine(&engine).unwrap();
        let newer = info.supports_flag("--allow-newer-file");
        let selection = Selection {
            machine: MACHINE.into(),
            process: PROCESS.into(),
            filament: FILAMENT.into(),
            bed_type: "Textured PEI Plate".into(),
        };
        let presets: BTreeMap<_, _> = Kind::ALL
            .iter()
            .map(|&kind| {
                let name = match kind { Kind::Machine => MACHINE, Kind::Process => PROCESS, Kind::Filament => FILAMENT };
                (kind as u8, store.flatten(kind, name).unwrap())
            })
            .collect();

        let mut small_grams = 0.0;
        let mut small_config = BTreeMap::new();
        for case in ["small", "heavy"] {
            let model = work.join(format!("{case}.3mf"));
            assert!(fixture(case, &model, &root), "фікстура {case} для {}", target.label);
            // Еталон: рушій сам, лише проєкт із системними пресетами (як відкриття в GUI).
            let reference_dir = work.join(format!("{case}-reference"));
            fs::create_dir_all(&reference_dir).unwrap();
            let mut command = Command::new(&target.path);
            if newer {
                command.arg("--allow-newer-file");
            }
            let status = command
                .args(["--slice", "1", "--outputdir", "."])
                .arg(&model)
                .current_dir(&reference_dir)
                .output()
                .unwrap();
            let reference = fs::read_to_string(reference_dir.join("plate_1.gcode"))
                .unwrap_or_else(|_| panic!("{}: еталон не нарізано: {status:?}", target.label));
            let reference = parse(&reference);

            let request = SliceRequest {
                input: model.clone(),
                plate: 1,
                allow_newer_file: newer,
                selection: selection.clone(),
                overrides: Overrides::default(),
            };
            let output = slicer::slice_with_engine(&engine, &store, &request)
                .unwrap_or_else(|e| panic!("{} {case}: {e:#}", target.label));
            let ours = gcode_of_package(&output.gcode_3mf);
            let label = format!("{} ({version}) {case}", target.label);
            eprintln!(
                "{label}: еталон {:.2} г / {:.0} с, Slicenv {:.2} г / {:.0} с",
                reference.grams, reference.seconds, ours.grams, ours.seconds
            );
            if case == "small" {
                small_grams = reference.grams;
                small_config = reference.config.clone();
            }
            close(&format!("{label}: вага, г"), ours.grams, reference.grams);
            close(&format!("{label}: час, с"), ours.seconds, reference.seconds);
            close(&format!("{label}: вага в результаті"), output.filament_grams, reference.grams);

            for (kind, key) in KEYS {
                let Some(expected) = presets[&(*kind as u8)].get(*key) else { continue };
                assert_eq!(
                    ours.config.get(*key).map(String::as_str),
                    Some(rendered(expected).as_str()),
                    "{label}: {key} у CONFIG_BLOCK не з пресета"
                );
            }
            let diverged: Vec<_> = reference
                .config
                .iter()
                .filter(|(k, v)| !IGNORED.contains(&k.as_str()) && ours.config.get(*k).is_some_and(|o| o != *v))
                .map(|(k, v)| format!("{k}: еталон {v:.60} / Slicenv {:.60}", ours.config[k]))
                .collect();
            assert!(diverged.is_empty(), "{label}: CONFIG_BLOCK розходиться з еталоном:\n{}", diverged.join("\n"));
        }

        // Проєкт, збережений Slicenv до виправлення: у project_settings — значення від батька
        // чужого вендора (`stale_values`), зокрема ключі, яких у пресетах BBL немає зовсім.
        let old_project = work.join("old-slicenv.3mf");
        let stale: serde_json::Map<String, serde_json::Value> = Kind::ALL
            .iter()
            .flat_map(|&kind| {
                let name = match kind { Kind::Machine => MACHINE, Kind::Process => PROCESS, Kind::Filament => FILAMENT };
                store.stale_values(kind, name)
            })
            .collect();
        eprintln!("{} ({version}): застарілих ключів у старому проєкті {}", target.label, stale.len());
        with_project_settings(&work.join("small.3mf"), &old_project, &stale);
        let request = SliceRequest {
            input: old_project,
            plate: 1,
            allow_newer_file: newer,
            selection: selection.clone(),
            overrides: Overrides::default(),
        };
        let output = slicer::slice_with_engine(&engine, &store, &request)
            .unwrap_or_else(|e| panic!("{} старий проєкт: {e:#}", target.label));
        let old = gcode_of_package(&output.gcode_3mf);
        let label = format!("{} ({version}) старий проєкт Slicenv", target.label);
        close(&format!("{label}: вага, г"), old.grams, small_grams);
        for key in ["use_relative_e_distances", "wall_loops", "sparse_infill_pattern", "skirt_loops"] {
            assert_eq!(old.config.get(key), small_config.get(key), "{label}: {key}");
        }

        // Користувацькі зміни: збережений пресет-дельта (inherits системного) і незбережені
        // правки редактора (`Overrides`) мають дійти до рушія поверх повного ланцюжка.
        let own = store.user_root().join("process");
        fs::create_dir_all(&own).unwrap();
        fs::write(
            own.join("My 0.20mm.json"),
            serde_json::to_vec(&serde_json::json!({
                "name": "My 0.20mm", "inherits": PROCESS, "from": "User", "version": "2.0.0.0",
                "print_settings_id": "My 0.20mm", "wall_loops": "4",
            }))
            .unwrap(),
        )
        .unwrap();
        store = store.reload().unwrap();
        // Spiral Inset є лише в Orca ≥ 2.5; старіший рушій тихо підставляє типовий візерунок.
        let numbers: Vec<u32> = version.split(['.', '-']).take(2).map(|n| n.parse().unwrap_or(0)).collect();
        let pattern = if target.kind == EngineKind::Orca && numbers >= vec![2, 5] {
            "spiralinset"
        } else {
            "concentric"
        };
        let mut overrides = Overrides::default();
        overrides.process.insert("top_surface_pattern".into(), pattern.into());
        let request = SliceRequest {
            input: work.join("small.3mf"),
            plate: 1,
            allow_newer_file: newer,
            selection: Selection { process: "My 0.20mm".into(), ..selection.clone() },
            overrides,
        };
        let output = slicer::slice_with_engine(&engine, &store, &request)
            .unwrap_or_else(|e| panic!("{} користувацький пресет: {e:#}", target.label));
        let custom = gcode_of_package(&output.gcode_3mf);
        let label = format!("{} ({version}) користувацький пресет", target.label);
        let get = |k: &str| custom.config.get(k).map(String::as_str);
        assert_eq!(get("wall_loops"), Some("4"), "{label}");
        assert_eq!(get("top_surface_pattern"), Some(pattern), "{label}");
        assert_eq!(get("print_settings_id"), Some("My 0.20mm"), "{label}");
        for key in ["layer_height", "sparse_infill_density", "use_relative_e_distances", "nozzle_temperature"] {
            let (kind, _) = KEYS.iter().find(|(_, k)| *k == key).unwrap();
            // Немає в пресеті (`use_relative_e_distances` у BBL Bambu) — дефолт рушія, як і в GUI.
            if let Some(expected) = presets[&(*kind as u8)].get(key) {
                assert_eq!(get(key), Some(rendered(expected).as_str()), "{label}: {key}");
            }
        }
        // 4 стінки замість 2: пластику більше, але того самого порядку.
        let ratio = custom.grams / small_grams;
        assert!((1.02..1.6).contains(&ratio), "{label}: {:.2} г проти {small_grams:.2} г", custom.grams);
        fs::remove_file(own.join("My 0.20mm.json")).unwrap();
        // SAFETY: див. вище.
        unsafe { std::env::remove_var(variable) };
        checked += 1;
    }
    eprintln!("перевірено рушіїв: {checked}");
}
