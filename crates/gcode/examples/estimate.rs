//! Друкує власну оцінку часу й витрату філаменту: `cargo run --release -p gcode --example estimate -- file.gcode`.
fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("G-code path");
    let started = std::time::Instant::now();
    let t = gcode::parse_file(&path)?;
    println!(
        "parsed in {:.0} ms, {} segments, {} layers",
        started.elapsed().as_secs_f64() * 1e3,
        t.segments.len(),
        t.layers.len()
    );
    let e = &t.estimate;
    println!(
        "total {:.1} prepare {:.1} timelapse {:.1}",
        e.total, e.prepare, e.timelapse
    );
    for (r, s) in &e.roles {
        println!("role {r:?}: {s:.1}");
    }
    for (k, s) in &e.moves {
        println!("move {k:?}: {s:.1}");
    }
    for f in &t.filament {
        println!("filament {:?} T{}: {:.1} mm", f.role, f.tool, f.mm);
    }
    let seams = t
        .markers
        .iter()
        .filter(|m| m.kind == gcode::Role::Seam)
        .count();
    println!("markers {} seams {seams}", t.markers.len());
    let mut old = std::collections::HashMap::new();
    for segment in &t.segments {
        if segment.is_extrusion && segment.feedrate > 0.0 {
            *old.entry(segment.role).or_insert(0.0f32) +=
                (segment.to - segment.from).length() / (segment.feedrate / 60.0);
        }
    }
    let old_total: f32 = old.values().sum();
    let outer = old
        .get(&gcode::Role::ExternalPerimeter)
        .copied()
        .unwrap_or(0.0);
    println!(
        "old extrusion-only total {old_total:.1}, outer {outer:.1} ({:.2}%)",
        outer / old_total * 100.0
    );
    println!(
        "header {:?} / {:?}",
        t.stats.total_estimated_time, t.stats.model_printing_time
    );
    let engine = std::env::args()
        .nth(2)
        .and_then(|p| gcode::EngineReport::read(p.as_ref(), Some(1)));
    println!("{:#?}", t.report(engine.as_ref()));
    Ok(())
}
