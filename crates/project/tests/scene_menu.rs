//! Дії контекстного меню сцени: додавання моделей, заміна сіток, дзеркало, експорт.
use project::*;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "scene-menu-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const LO: [f64; 2] = [0.0, 0.0];
const HI: [f64; 2] = [256.0, 256.0];

fn empty(plates: u32) -> Project {
    let mut p = Project::default();
    for _ in 0..plates {
        p.add_plate("").unwrap();
    }
    p
}

fn add(p: &mut Project, name: &str, shape: Primitive, plate: u32, at: [f64; 2]) -> InstanceRef {
    p.add_mesh_object(name, shape.mesh(), plate, at, LO, HI)
        .unwrap()
}

#[test]
fn adding_a_primitive_places_it_on_the_clicked_plate_at_the_bed() {
    let mut p = empty(2);
    let key = add(&mut p, "Куб", Primitive::Cube, 2, [100.0, 60.0]);
    assert_eq!(p.instance(key).unwrap().plate, Some(2));
    let b = p.instance_bounds(key).unwrap().unwrap();
    assert!((b.center().x - 100.0).abs() < 1e-9 && (b.center().y - 60.0).abs() < 1e-9);
    assert!(b.min.z.abs() < 1e-9);
    assert_eq!(b.size(), DVec3::splat(20.0));
    assert_eq!(p.plates[1].objects, vec![key]);
    assert!(p.plates[0].objects.is_empty());
    // Другий примітив отримує новий ID.
    let second = add(&mut p, "Сфера", Primitive::Sphere, 1, [10.0, 10.0]);
    assert_ne!(second.object, key.object);
}

#[test]
fn placement_is_clamped_to_the_bed_and_locked_plates_refuse() {
    let mut p = empty(1);
    let key = add(&mut p, "Куб", Primitive::Cube, 1, [-50.0, 9999.0]);
    let b = p.instance_bounds(key).unwrap().unwrap();
    assert!((b.min.x - 0.0).abs() < 1e-9 && (b.max.y - 256.0).abs() < 1e-9);
    p.set_plate_locked(1, true).unwrap();
    let before = p.clone();
    assert!(
        p.add_mesh_object("x", Primitive::Cube.mesh(), 1, [10.0; 2], LO, HI)
            .is_err()
    );
    assert!(
        p.add_mesh_object("x", Primitive::Cube.mesh(), 7, [10.0; 2], LO, HI)
            .is_err()
    );
    assert_eq!(p, before, "помилка не змінює проєкт");
}

#[test]
fn stl_and_obj_files_import_centered_with_a_remembered_source() {
    let dir = Temp::new();
    let stl = dir.file("деталь.stl");
    let mut source = empty(1);
    let key = add(&mut source, "a", Primitive::Cube, 1, [50.0, 50.0]);
    source.export_stl(&[key], &stl).unwrap();
    let obj = dir.file("tri.obj");
    fs::write(
        &obj,
        "# трикутна піраміда\nv 0 0 5\nv 10 0 5\nv 0 10 5\nv 0 0 15\nf 1//1 3 2\nf 1 2 4\nf 2 3 4\nf 3 1 4\n",
    )
    .unwrap();

    let mut p = empty(1);
    let keys = p
        .add_model(read_model(&stl).unwrap(), 1, [128.0, 128.0], LO, HI)
        .unwrap();
    let b = p.instance_bounds(keys[0]).unwrap().unwrap();
    assert!((b.center().x - 128.0).abs() < 1e-4 && b.min.z.abs() < 1e-4);
    assert_eq!(p.object(keys[0].object).unwrap().name, "деталь");
    let volume = &p.object(keys[0].object).unwrap().volumes[0];
    assert!(volume.source_file().unwrap().ends_with("деталь.stl"));

    let keys = p
        .add_model(read_model(&obj).unwrap(), 1, [30.0, 30.0], LO, HI)
        .unwrap();
    assert_eq!(p.object(keys[0].object).unwrap().volumes[0].mesh.triangles.len(), 4);
    assert!(p.instance_bounds(keys[0]).unwrap().unwrap().min.z.abs() < 1e-9);
    assert!(read_model(&dir.file("model.step")).is_err());
}

#[test]
fn obj_parser_handles_polygons_negative_indices_and_errors() {
    let mesh = parse_obj("v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1 2 3 4\nf -4 -3 -2/1/1\n").unwrap();
    assert_eq!(mesh.triangles, vec![[0, 1, 2], [0, 2, 3], [0, 1, 2]]);
    assert!(parse_obj("v 0 0 0\nf 1 2 3\n").is_err());
    assert!(parse_obj("v 0 0\n").is_err());
    assert!(parse_obj("v 0 0 0\n").is_err());
}

#[test]
fn importing_a_3mf_keeps_the_layout_and_clears_foreign_xml() {
    let dir = Temp::new();
    let path = dir.file("two.3mf");
    let mut source = empty(1);
    let a = add(&mut source, "a", Primitive::Cube, 1, [40.0, 40.0]);
    let b = add(&mut source, "b", Primitive::Cube, 1, [100.0, 40.0]);
    assert_ne!(a, b);
    source.save_3mf(&path).unwrap();

    let mut p = empty(2);
    add(&mut p, "існуючий", Primitive::Cone, 1, [128.0, 128.0]);
    let keys = p
        .add_model(read_model(&path).unwrap(), 2, [128.0, 128.0], LO, HI)
        .unwrap();
    assert_eq!(keys.len(), 2);
    assert!(keys.iter().all(|k| p.instance(*k).unwrap().plate == Some(2)));
    let (x0, x1) = (
        p.instance_bounds(keys[0]).unwrap().unwrap().center().x,
        p.instance_bounds(keys[1]).unwrap().unwrap().center().x,
    );
    assert!((x1 - x0 - 60.0).abs() < 1e-6, "відстань між об’єктами збережена");
    assert!(((x0 + x1) / 2.0 - 128.0).abs() < 1e-6, "група по центру");
    let ids: std::collections::BTreeSet<_> = p.objects.iter().map(|o| o.id).collect();
    assert_eq!(ids.len(), p.objects.len(), "ID унікальні");
    // Результат зберігається й читається назад.
    let out = dir.file("merged.3mf");
    p.save_3mf(&out).unwrap();
    assert_eq!(Project::load_3mf(&out).unwrap().objects.len(), 3);
}

#[test]
fn replacing_a_mesh_keeps_transforms_and_reloading_an_unchanged_file_is_a_no_op() {
    let dir = Temp::new();
    let file = dir.file("part.stl");
    let mut source = empty(1);
    let k = add(&mut source, "p", Primitive::Cylinder, 1, [10.0; 2]);
    source.export_stl(&[k], &file).unwrap();

    let mut p = empty(1);
    let key = p
        .add_model(read_model(&file).unwrap(), 1, [80.0, 90.0], LO, HI)
        .unwrap()[0];
    p.rotate(key, DQuat::from_rotation_z(0.7)).unwrap();
    let before = p.clone();
    let sources = p.reloadable_sources(key.object);
    assert_eq!(sources.len(), 1);
    let loaded = p.read_reload_sources(&[key.object]);
    let report = p.apply_reloaded(loaded).unwrap();
    assert_eq!(report, ReplaceReport { replaced: 1, skipped: 0 });
    let (a, b) = (
        before.instance_bounds(key).unwrap().unwrap(),
        p.instance_bounds(key).unwrap().unwrap(),
    );
    assert!((a.min - b.min).length() < 1e-4 && (a.max - b.max).length() < 1e-4);
    assert_eq!(p.instance(key).unwrap().transform, before.instance(key).unwrap().transform);

    // Файл змінився: нова висота, XY-центр і низ на місці, поворот лишається.
    let mut taller = empty(1);
    let t = taller
        .add_mesh_object("t", IndexedMesh::cylinder(10.0, 40.0, 64), 1, [10.0; 2], LO, HI)
        .unwrap();
    taller.export_stl(&[t], &file).unwrap();
    let loaded = p.read_reload_sources(&[key.object]);
    p.apply_reloaded(loaded).unwrap();
    let b = p.instance_bounds(key).unwrap().unwrap();
    assert!((b.size().z - 40.0).abs() < 1e-4 && b.min.z.abs() < 1e-4);
    assert_eq!(p.instance(key).unwrap().transform, before.instance(key).unwrap().transform);

    // Файл зник — пропущено без помилки.
    fs::remove_file(&file).unwrap();
    assert!(p.reloadable_sources(key.object).is_empty());
}

#[test]
fn replacement_folder_matches_objects_by_file_name() {
    let dir = Temp::new();
    let mut source = empty(1);
    let big = source
        .add_mesh_object("Кронштейн", IndexedMesh::cuboid(DVec3::new(50.0, 20.0, 10.0)), 1, [100.0; 2], LO, HI)
        .unwrap();
    source.export_stl(&[big], &dir.file("Кронштейн.STL")).unwrap();
    fs::write(dir.file("notes.txt"), "x").unwrap();

    let mut p = empty(1);
    let a = add(&mut p, "кронштейн", Primitive::Cube, 1, [60.0, 60.0]);
    let b = add(&mut p, "інший", Primitive::Cube, 1, [150.0, 60.0]);
    let before_b = p.object(b.object).unwrap().clone();
    p.scale(a, DVec3::new(1.0, 1.0, 2.0)).unwrap();
    p.drop_to_bed(a).unwrap();
    let transform = p.instance(a).unwrap().transform;

    let files = Project::scan_replacement_folder(&dir.0).unwrap();
    assert_eq!(files.len(), 1);
    let (matches, skipped) = p.replacement_matches(&files);
    assert_eq!(matches.len(), 1);
    assert_eq!(skipped, 1);
    let (object, volume, path) = matches.into_iter().next().unwrap();
    assert_eq!(object, a.object);
    let mesh = read_mesh_file(&path).unwrap();
    p.replace_volume_mesh(object, volume, mesh, Some(&path)).unwrap();
    assert_eq!(p.instance(a).unwrap().transform, transform);
    let size = p.instance_bounds(a).unwrap().unwrap().size();
    assert!((size.x - 50.0).abs() < 1e-4 && (size.z - 20.0).abs() < 1e-4);
    assert_eq!(p.object(b.object).unwrap(), &before_b, "решта не змінилась");
}

#[test]
fn mirror_flips_geometry_in_place_and_keeps_normals_outward() {
    let mut p = empty(1);
    let mut mesh = IndexedMesh::cuboid(DVec3::new(10.0, 10.0, 10.0));
    // Асиметрія: зсунути одну вершину у X.
    mesh.positions[1][0] += 6.0;
    mesh.positions[2][0] += 6.0;
    let key = p
        .add_mesh_object("a", mesh, 1, [100.0, 100.0], LO, HI)
        .unwrap();
    let before = p.instance_bounds(key).unwrap().unwrap();
    let volume_before = p.object(key.object).unwrap().volumes[0].mesh.signed_volume();
    let first = p.world_triangles(key).unwrap()[0].2;

    let mirrored = p.mirror_instance(key, 0).unwrap();
    assert_eq!(mirrored, key);
    let after = p.instance_bounds(key).unwrap().unwrap();
    assert!((after.center() - before.center()).length() < 1e-9);
    assert!((after.size() - before.size()).length() < 1e-9);
    let volume_after = p.object(key.object).unwrap().volumes[0].mesh.signed_volume();
    assert!(volume_after > 0.0 && (volume_after - volume_before).abs() < 1e-6);
    // Перша вершина відбилась відносно центру X.
    let now = p.world_triangles(key).unwrap()[0].2;
    let cx = before.center().x;
    assert!((now[0].x - (2.0 * cx - first[0].x)).abs() < 1e-9);

    // Подвійне дзеркало повертає вихідну геометрію.
    p.mirror_instance(key, 0).unwrap();
    let again = p.world_triangles(key).unwrap()[0].2;
    assert!((again[0] - first[0]).length() < 1e-9);

    // Z-дзеркало лишає об’єкт на столі.
    p.mirror_instance(key, 2).unwrap();
    assert!(p.instance_bounds(key).unwrap().unwrap().min.z.abs() < 1e-9);
    assert!(p.object(key.object).unwrap().volumes[0].mesh.signed_volume() > 0.0);
}

#[test]
fn mirroring_a_rotated_instance_uses_world_axes() {
    let mut p = empty(1);
    let key = add(&mut p, "a", Primitive::Cone, 1, [100.0, 100.0]);
    p.rotate(key, DQuat::from_rotation_z(0.5)).unwrap();
    let before = p.world_triangles(key).unwrap();
    let c = p.instance_bounds(key).unwrap().unwrap().center();
    p.mirror_instance(key, 1).unwrap();
    let after = p.world_triangles(key).unwrap();
    // Кожна вершина — відбиття по світовій осі Y відносно центру рамки.
    for ((_, _, a), (_, _, b)) in before.iter().zip(&after) {
        assert!((b[0].y - (2.0 * c.y - a[0].y)).abs() < 1e-9);
        assert!((b[0].x - a[0].x).abs() < 1e-9 && (b[0].z - a[0].z).abs() < 1e-9);
    }
}

#[test]
fn mirroring_one_of_several_instances_splits_it_off() {
    let mut p = empty(1);
    let key = add(&mut p, "a", Primitive::Cone, 1, [60.0, 60.0]);
    let copy = p.duplicate_instance(key).unwrap();
    p.translate(copy, DVec3::new(80.0, 0.0, 0.0)).unwrap();
    let original_mesh = p.object(key.object).unwrap().volumes[0].mesh.clone();
    let mirrored = p.mirror_instance(copy, 0).unwrap();
    assert_ne!(mirrored.object, key.object);
    assert_eq!(p.object(key.object).unwrap().instances.len(), 1);
    assert_eq!(p.object(key.object).unwrap().volumes[0].mesh, original_mesh);
    assert_eq!(p.objects.len(), 2);
    assert_eq!(p.plates[0].objects.len(), 2);
}

#[test]
fn stl_export_roundtrips_world_geometry() {
    let dir = Temp::new();
    let mut p = empty(1);
    let a = add(&mut p, "a", Primitive::Cube, 1, [50.0, 50.0]);
    let b = add(&mut p, "b", Primitive::Cube, 1, [150.0, 50.0]);
    p.rotate(a, DQuat::from_rotation_z(0.3)).unwrap();
    let path = dir.file("out.stl");
    assert_eq!(p.export_stl(&[a, b], &path).unwrap(), 24);
    let back = Project::load_stl(&path).unwrap();
    let mesh = &back.objects[0].volumes[0].mesh;
    assert_eq!(mesh.triangles.len(), 24);
    assert!((mesh.signed_volume() - 16000.0).abs() < 1e-2);
    assert!(p.export_stl(&[], &path).is_err());
}

#[test]
fn delete_instances_handles_shifting_indexes_and_delete_all() {
    let mut p = empty(1);
    let key = add(&mut p, "a", Primitive::Cube, 1, [40.0, 40.0]);
    let c1 = p.duplicate_instance(key).unwrap();
    let c2 = p.duplicate_instance(key).unwrap();
    let other = add(&mut p, "b", Primitive::Cone, 1, [150.0, 150.0]);
    p.delete_instances(&[key, c2, c1]).unwrap();
    assert_eq!(p.objects.len(), 1);
    assert_eq!(p.objects[0].id, other.object);
    assert!(p.delete_instances(&[key]).is_err());
    p.delete_all_objects();
    assert!(p.objects.is_empty() && p.plates[0].objects.is_empty());
}

#[test]
fn arranging_a_subset_leaves_the_rest_in_place_and_avoids_it() {
    let mut p = empty(1);
    let fixed = add(&mut p, "fixed", Primitive::Cube, 1, [10.0, 10.0]);
    let moved_a = add(&mut p, "a", Primitive::Cube, 1, [200.0, 200.0]);
    let moved_b = add(&mut p, "b", Primitive::Cube, 1, [220.0, 120.0]);
    let fixed_before = p.instance_bounds(fixed).unwrap().unwrap();
    p.arrange_instances_on_bed(1, &[moved_a, moved_b], 5.0, LO, HI)
        .unwrap();
    assert_eq!(p.instance_bounds(fixed).unwrap().unwrap(), fixed_before);
    let boxes: Vec<_> = [fixed, moved_a, moved_b]
        .iter()
        .map(|k| p.instance_bounds(*k).unwrap().unwrap())
        .collect();
    for i in 0..3 {
        for j in i + 1..3 {
            let (a, b) = (boxes[i], boxes[j]);
            let overlap = a.min.x < b.max.x && b.min.x < a.max.x && a.min.y < b.max.y && b.min.y < a.max.y;
            assert!(!overlap, "{i} і {j} перетинаються");
        }
    }
    for b in &boxes {
        assert!(b.min.x >= 0.0 && b.max.x <= 256.0 && b.min.y >= 0.0 && b.max.y <= 256.0);
    }
    // Не вміщається — проєкт не змінюється; інстанс не з цієї пластини — помилка.
    let before = p.clone();
    assert!(p
        .arrange_instances_on_bed(1, &[moved_a], 5.0, [0.0, 0.0], [10.0, 10.0])
        .is_err());
    assert_eq!(p, before);
    p.set_plate_locked(1, true).unwrap();
    assert!(p.arrange_instances_on_bed(1, &[moved_a], 5.0, LO, HI).is_err());
}

#[test]
fn handy_models_import_like_any_mesh() {
    let mut p = empty(1);
    for handy in Handy::ALL {
        let key = p
            .add_mesh_object("h", handy.mesh(), 1, [128.0, 128.0], LO, HI)
            .unwrap();
        assert!(p.instance_bounds(key).unwrap().unwrap().min.z.abs() < 1e-9);
    }
    let out = std::env::temp_dir().join(format!("handy-{}.3mf", std::process::id()));
    p.save_3mf(&out).unwrap();
    let back = Project::load_3mf(&out).unwrap();
    fs::remove_file(&out).unwrap();
    assert_eq!(back.objects.len(), 3);
}

#[test]
fn plate_grid_follows_the_printer_bed_like_bambu() {
    // A1 mini: стіл 180 мм → крок пластин 216, як у Bambu; без столу — 256 × 1.2.
    let mut p = empty(2);
    assert!((p.plate_offset(2).unwrap().x - 256.0 * 1.2).abs() < 1e-9);
    p.config.insert(
        "printable_area".into(),
        serde_json::json!(["0x0", "180x0", "180x180", "0x180"]),
    );
    assert!((p.plate_offset(2).unwrap().x - 216.0).abs() < 1e-9);

    // У файлі об’єкт другої пластини лежить у межах її столу, а після читання —
    // на тому ж місці відносно пластини.
    let key = add(&mut p, "Куб", Primitive::Cube, 2, [90.0, 90.0]);
    let tmp = Temp::new();
    let file = tmp.file("a1mini.3mf");
    p.save_3mf(&file).unwrap();
    let model = String::from_utf8(
        zip::ZipArchive::new(fs::File::open(&file).unwrap())
            .unwrap()
            .by_name("3D/3dmodel.model")
            .map(|mut f| {
                let mut s = Vec::new();
                std::io::Read::read_to_end(&mut f, &mut s).unwrap();
                s
            })
            .unwrap(),
    )
    .unwrap();
    assert!(model.contains(" 306 90 "), "{model}");
    let loaded = Project::load_3mf(&file).unwrap();
    let b = loaded.instance_bounds(key).unwrap().unwrap();
    assert!((b.center().x - 90.0).abs() < 1e-6, "{b:?}");
}
