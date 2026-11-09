//! Розклад однієї нарізки на етапи без вікна: те саме, що робить `Workspace::start_slice`
//! і `load_preview`, але з таймером на кожному кроці.
//!
//! ```text
//! SLICENV_PROFILE=1 cargo run --release -p perf --bin slice-profile -- \
//!     --engine orca=/шлях/AppRun --input heavy.3mf --plate 1 --runs 5
//! ```
//! Запускати лише з тимчасовими HOME/XDG_* (skill `real-engine-test`): рушій пише свій конфіг
//! і кеш. Етапи всередині `slicer` (`slice::probe`, `slice::flatten`, `slice::engine_process`…)
//! друкує `paths::profile::Span` у stderr, якщо задано `SLICENV_PROFILE`.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
use project::{InstanceRef, Project};
use slicer::{Engine, EngineKind, PresetStore, Selection, SliceRequest};

fn main() -> Result<()> {
    let mut engine = None;
    let mut input = None;
    let mut plate = 1u32;
    let mut runs = 3usize;
    let mut keep = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().context("бракує значення");
        match arg.as_str() {
            "--engine" => {
                let v = value()?;
                let (kind, path) = v.split_once('=').context("--engine orca|bambu=ШЛЯХ")?;
                let kind = match kind {
                    "orca" => EngineKind::Orca,
                    "bambu" => EngineKind::Bambu,
                    _ => bail!("невідомий рушій {kind}"),
                };
                engine = Some(Engine { kind, path: PathBuf::from(path) });
            }
            "--input" => input = Some(PathBuf::from(value()?)),
            "--plate" => plate = value()?.parse()?,
            "--runs" => runs = value()?.parse()?,
            // Зберегти вхід рушія (3MF лише з пластиною) для ручних прогонів CLI.
            "--keep-input" => keep = Some(PathBuf::from(value()?)),
            _ => bail!("невідомий аргумент {arg}"),
        }
    }
    let engine = engine.context("--engine")?;
    let input = std::fs::canonicalize(input.context("--input")?)?;

    let t = Instant::now();
    let store = PresetStore::open_for_engine(&engine)?;
    eprintln!("store::open {:.0} ms (раз на рушій у застосунку)", ms(t.elapsed()));
    let selection = slicer::project_selection(&input).unwrap_or(Selection {
        machine: "Bambu Lab A1 0.4 nozzle".into(),
        process: "0.20mm Standard @BBL A1".into(),
        filament: "Bambu PLA Basic @BBL A1".into(),
        bed_type: "Textured PEI Plate".into(),
    });
    // Застосунок питає рушій під час завантаження (`engines.rs`), нарізка бере це з кешу.
    let t = Instant::now();
    engine.probe()?;
    eprintln!("engine::probe {:.0} ms (при завантаженні рушія)", ms(t.elapsed()));
    let t = Instant::now();
    let loaded = Project::load(&input)?;
    eprintln!("project::load {:.0} ms (при відкритті файлу)", ms(t.elapsed()));

    for run in 1..=runs {
        let total = Instant::now();
        // slice::prepare_input: лише інстанси пластини → 3MF у тимчасову теку.
        let t = Instant::now();
        let dir = tempfile::tempdir()?;
        let mut project = loaded.clone();
        let others: Vec<InstanceRef> = project
            .objects
            .iter()
            .flat_map(|o| {
                o.instances
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| p.plate != Some(plate))
                    .map(|(instance, _)| InstanceRef { object: o.id, instance })
            })
            .collect();
        if !others.is_empty() {
            project.ensure_identify_ids();
            project.delete_instances(&others)?;
        }
        let path = dir.path().join(input.file_name().unwrap());
        if std::env::var_os("SLICE_PROFILE_DURABLE_INPUT").is_some() {
            project.save_3mf(&path)?;
        } else {
            project.save_3mf_for_engine(&path)?;
        }
        let prepare = t.elapsed();
        if let Some(keep) = &keep {
            std::fs::copy(&path, keep)?;
        }

        let t = Instant::now();
        let request = SliceRequest {
            input: path,
            plate,
            allow_newer_file: false,
            selection: selection.clone(),
            overrides: Default::default(),
        };
        // Як `Workspace::watch_early_gcode`: Preview з G-code, поки рушій пакує `.gcode.3mf`.
        let token = slicer::SliceCancellation::default();
        let job = {
            let (engine, store, token) = (engine.clone(), store.clone(), token.clone());
            std::thread::spawn(move || slicer::slice_with_engine_cancellable(&engine, &store, &request, &token))
        };
        let mut early = None;
        while !job.is_finished() {
            if let Some(gcode) = token.gcode_ready() {
                let ready = total.elapsed();
                let toolpath = gcode::parse_file(&gcode)?;
                early = Some((ready, total.elapsed(), toolpath));
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let out = job.join().unwrap()?;
        let slice = t.elapsed();
        let ready = total.elapsed();

        // preview::parse_gcode + report — у застосунку у фоні після показу підсумку.
        let t = Instant::now();
        let gcode = out.gcode_3mf.with_file_name(format!("plate_{plate}.gcode"));
        let (gcode_ready, layers, toolpath) = match early {
            Some((ready, layers, toolpath)) => (ready, layers, toolpath),
            None => {
                let toolpath = gcode::parse_file(&gcode)?;
                (ready, total.elapsed(), toolpath)
            }
        };
        // Розбір — від готовності G-code до шарів (ранній) або після кінця нарізки.
        let parse = (layers - gcode_ready).max(t.elapsed());
        let t = Instant::now();
        let report = gcode::EngineReport::read(&gcode.with_file_name("result.json"), Some(plate));
        let _ = toolpath.report(report.as_ref());
        let report = t.elapsed();
        let gcode_bytes = std::fs::metadata(&gcode).map(|m| m.len()).unwrap_or(0);
        let archive_bytes = std::fs::metadata(&out.gcode_3mf).map(|m| m.len()).unwrap_or(0);
        println!(
            "run {run}: prepare_input {:.0} ms · slice_with_engine {:.0} ms · G-code готовий {:.0} ms · шари в Preview {:.0} ms · підсумок готовий {:.0} ms · parse_gcode {:.0} ms · report {:.0} ms · разом {:.0} ms · G-code {:.1} МБ · .gcode.3mf {:.1} МБ · {:.1} г · {}",
            ms(prepare),
            ms(slice),
            ms(gcode_ready),
            ms(layers),
            ms(ready),
            ms(parse),
            ms(report),
            ms(total.elapsed()),
            gcode_bytes as f64 / 1e6,
            archive_bytes as f64 / 1e6,
            out.filament_grams,
            out.gcode_3mf.parent().unwrap().display(),
        );
    }
    Ok(())
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}
