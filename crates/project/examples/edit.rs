//! Вправа з реальним проєктом; записує тільки заданий вихідний файл.
use anyhow::{Result, ensure};
use project::*;
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(args.len() == 2, "edit INPUT OUTPUT");
    let mut p = Project::load(&args[0])?;
    let key = *p.plates[0]
        .objects
        .first()
        .ok_or_else(|| anyhow::anyhow!("порожня пластина"))?;
    p.rename_plate(1, "Edited plate")?;
    p.objects[0].name = "Edited object".into();
    let copy = p.duplicate_instance(key)?;
    p.scale(copy, DVec3::splat(0.5))?;
    p.rotate(copy, DQuat::from_rotation_z(0.2))?;
    p.drop_to_bed(copy)?;
    let second = p.add_plate("Copy")?;
    p.move_to_plate(copy, Some(second))?;
    p.center_on_plate(copy)?;
    p.save_3mf(&args[1])?;
    let q = Project::load(&args[1])?;
    ensure!(
        q.objects.len() == p.objects.len() && q.plates.len() == p.plates.len(),
        "структура"
    );
    let expected = p.instance_bounds(copy)?.unwrap();
    let actual = q.instance_bounds(copy)?.unwrap();
    ensure!(
        (expected.min - actual.min).abs().max_element() < 1e-8
            && (expected.max - actual.max).abs().max_element() < 1e-8,
        "координати"
    );
    println!(
        "{}",
        serde_json::json!({"objects": q.objects.len(), "plates": q.plates.len(), "copy_plate": second,
        "bounds": [actual.min.to_array(), actual.max.to_array()]})
    );
    Ok(())
}
