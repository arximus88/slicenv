//! Inspect installed printer/nozzle coverage without modifying presets or slicing.
use slicer::{Kind, PresetStore};

fn main() -> anyhow::Result<()> {
    let store = if let Some(path) = std::env::args().nth(1) {
        let engine = slicer::Engine {
            kind: slicer::EngineKind::Orca,
            path: path.into(),
        };
        PresetStore::open_for_engine(&engine)?
    } else {
        PresetStore::open_default()?
    };
    println!("Catalog: {}", store.root().display());
    if std::env::args().any(|arg| arg == "--verify-a1") {
        let machine = "Bambu Lab A1 0.4 nozzle";
        anyhow::ensure!(store.get(Kind::Machine, machine).is_some(), "A1 is missing");
        anyhow::ensure!(
            !store.compatible(Kind::Process, machine).is_empty(),
            "A1 processes missing"
        );
        anyhow::ensure!(
            !store.compatible(Kind::Filament, machine).is_empty(),
            "A1 filaments missing"
        );
        let config = store.flatten(Kind::Machine, machine)?;
        let gcode = config
            .get("machine_start_gcode")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        anyhow::ensure!(!gcode.is_empty(), "A1 includes failed to resolve");
        println!(
            "A1 verified: {} processes, {} filaments, {} bytes machine_start_gcode",
            store.compatible(Kind::Process, machine).len(),
            store.compatible(Kind::Filament, machine).len(),
            gcode.len()
        );
    }
    let machines = store.compatible(Kind::Machine, "");
    println!("{} installed printer/nozzle profiles", machines.len());
    for machine in machines {
        let process = store.compatible(Kind::Process, &machine.name).len();
        let filament = store.compatible(Kind::Filament, &machine.name).len();
        let bed = store.bed_bounds(&machine.name);
        let defaults = store.defaults_for_machine(&machine.name);
        println!(
            "{}: {process} processes, {filament} filaments, bed {bed:?}, defaults {}",
            machine.name,
            if defaults.is_some() {
                "available"
            } else {
                "missing"
            }
        );
    }
    Ok(())
}
