//! `cargo run -p slicer --example slice -- model.3mf [пластина] [принтер]` — нарізка без GUI.
//! Профілі: з проєкту (якщо 3MF і вони сумісні), інакше вибрані в Bambu Studio.

use std::path::PathBuf;

use slicer::{PresetStore, SliceRequest, project_selection};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let input = PathBuf::from(args.next().expect("шлях до моделі"));
    let plate = args.next().map_or(Ok(1), |p| p.parse())?;

    let machine = args.next();
    let kind = match args.next().as_deref() {
        Some("orca") => slicer::EngineKind::Orca,
        Some("bambu") | None => slicer::EngineKind::Bambu,
        Some(other) => anyhow::bail!("невідомий рушій {other}"),
    };
    let engine = slicer::Engine::discover(kind)?;
    let store = PresetStore::open_for_engine(&engine)?;
    println!(
        "Рушій: {} · профілі: {}",
        engine.probe()?.version,
        store.root().display()
    );
    let selection = if let Some(machine) = machine {
        store
            .defaults_for_machine(&machine)
            .ok_or_else(|| anyhow::anyhow!("немає сумісних типових профілів для {machine}"))?
    } else {
        project_selection(&input)
            .or_else(|| store.studio_selection())
            .ok_or_else(|| anyhow::anyhow!("немає ні профілів проєкту, ні вибору в Bambu Studio"))?
    };
    let selection = store.sanitize(selection);
    println!("{selection:#?}");

    let out = slicer::slice_with_engine(
        &engine,
        &store,
        &SliceRequest {
            allow_newer_file: args.next().as_deref() == Some("--allow-newer-file"),
            input,
            plate,
            selection,
            overrides: Default::default(),
        },
    )?;
    let t = out.print_time.as_secs();
    println!(
        "{} — {}:{:02} год, {:.2} г, шар {} мм (нарізано за {:.1} с)",
        out.gcode_3mf.display(),
        t / 3600,
        t % 3600 / 60,
        out.filament_grams,
        out.layer_height,
        out.elapsed.as_secs_f64()
    );
    Ok(())
}
