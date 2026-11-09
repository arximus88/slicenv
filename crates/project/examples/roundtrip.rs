//! Перевіряє структуру й записує копію; вихідний файл лише читає.
use anyhow::{Result, ensure};
use project::*;

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 2 || (args.len() == 3 && args[2] == "--rewrite"),
        "roundtrip INPUT OUTPUT [--rewrite]"
    );
    let mut p = Project::load(&args[0])?;
    if args.len() == 3 {
        p.source_fingerprint = None;
    }
    p.save_3mf(&args[1])?;
    let q = Project::load(&args[1])?;
    ensure!(p.objects.len() == q.objects.len(), "кількість обʼєктів");
    ensure!(
        p.config == q.config
            && p.metadata.iter().all(|(k, v)| q.metadata.get(k) == Some(v))
            && q.metadata.keys().all(|k| p.metadata.contains_key(k)
                || (p.root.is_none() && k == "BambuStudio:3mfVersion")),
        "налаштування проєкту"
    );
    ensure!(p.plates.len() == q.plates.len(), "кількість пластин");
    for (a, b) in p.plates.iter().zip(&q.plates) {
        ensure!(
            a.index == b.index
                && a.name == b.name
                && a.locked == b.locked
                && a.bed_type == b.bed_type
                && a.objects == b.objects,
            "пластина {}",
            a.index
        );
    }
    for (a, b) in p.objects.iter().zip(&q.objects) {
        ensure!(
            a.id == b.id && a.name == b.name && a.config == b.config,
            "обʼєкт {}",
            a.id
        );
        ensure!(
            a.volumes.len() == b.volumes.len() && a.instances.len() == b.instances.len(),
            "частини/інстанси {}",
            a.id
        );
        for (a, b) in a.volumes.iter().zip(&b.volumes) {
            ensure!(
                a.name == b.name && a.kind == b.kind && a.mesh == b.mesh && a.config == b.config,
                "частина {}",
                a.name
            );
            close(a.transform, b.transform)?;
        }
        for (a, b) in a.instances.iter().zip(&b.instances) {
            ensure!(a.plate == b.plate && a.printable == b.printable, "інстанс");
            close(a.transform, b.transform)?;
        }
    }
    let triangles: usize = p
        .objects
        .iter()
        .flat_map(|o| &o.volumes)
        .map(|v| v.mesh.triangles.len())
        .sum();
    let off_plate = p
        .objects
        .iter()
        .flat_map(|o| &o.instances)
        .filter(|i| i.plate.is_none())
        .count();
    println!(
        "{}",
        serde_json::json!({"objects":p.objects.len(),"volumes":p.objects.iter().map(|o| o.volumes.len()).sum::<usize>(),
        "instances":p.objects.iter().map(|o| o.instances.len()).sum::<usize>(),"plates":p.plates.len(),"off_plate":off_plate,"triangles":triangles,"structure":"match"})
    );
    Ok(())
}
fn close(a: DAffine3, b: DAffine3) -> Result<()> {
    ensure!(
        a.to_cols_array()
            .iter()
            .zip(b.to_cols_array())
            .all(|(a, b)| (a - b).abs() < 1e-9),
        "трансформа"
    );
    Ok(())
}
