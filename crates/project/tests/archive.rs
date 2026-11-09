use project::*;
use std::{
    fs,
    io::Write,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use zip::{ZipWriter, write::SimpleFileOptions};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "project-test-{}-{}",
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

fn fixture() -> Project {
    let mut p = Project::default();
    p.add_plate("one & \"two\"").unwrap();
    let mut mesh = IndexedMesh {
        positions: vec![[0., 0., 0.], [10., 0., 0.], [0., 10., 0.], [0., 0., 10.]],
        triangles: vec![[0, 2, 1], [0, 1, 3], [1, 2, 3], [2, 0, 3]],
        ..IndexedMesh::default()
    };
    mesh.triangle_attributes.insert(
        1,
        Config::from([("BambuStudio:mmu_segmentation".into(), "0A1B".into())]),
    );
    let mut meta = XmlNode::new("part");
    meta.set("part_guid", "custom-guid");
    let mut custom = XmlNode::new("extension");
    custom.set("unknown", "& < > \"");
    meta.push(custom);
    p.objects.push(Object {
        id: 2,
        name: "obj & <name>".into(),
        volumes: vec![Volume {
            name: "part A".into(),
            mesh: mesh.into(),
            config: Config::from([
                ("extruder".into(), "2".into()),
                ("source_file".into(), "a & b.stl".into()),
            ]),
            metadata: Some(meta),
            ..Volume::default()
        }],
        config: Config::from([("wall_loops".into(), "4".into())]),
        instances: vec![Instance::default()],
        ..Object::default()
    });
    p.metadata
        .insert("Description".into(), "<body>\"a & b\"</body>".into());
    p.config
        .insert("layer_height".into(), serde_json::json!("0.2"));
    p.archive
        .insert("Unknown/data.bin".into(), vec![1, 2, 3, 0, 255].into());
    p.sync_plates();
    p
}

#[test]
fn layer_ranges_use_one_based_model_object_positions_and_follow_reorder() {
    let temp = Temp::new();
    let mut p = fixture();
    let mut second = p.objects[0].clone();
    second.id = 8;
    second.name = "second".into();
    p.objects.push(second);
    let source = temp.file("source.3mf");
    p.save_3mf(&source).unwrap();
    let ranged = temp.file("ranged.3mf");
    {
        let mut input = zip::ZipArchive::new(fs::File::open(&source).unwrap()).unwrap();
        let mut output = ZipWriter::new(fs::File::create(&ranged).unwrap());
        for i in 0..input.len() {
            let mut entry = input.by_index(i).unwrap();
            let name = entry.name().unwrap().into_owned();
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut bytes).unwrap();
            output
                .start_file(name, SimpleFileOptions::default())
                .unwrap();
            output.write_all(&bytes).unwrap();
        }
        output
            .start_file(
                "Metadata/layer_config_ranges.xml",
                SimpleFileOptions::default(),
            )
            .unwrap();
        output.write_all(b"<objects><object id=\"1\"><range min_z=\"0.2\" max_z=\"0.4\"><option opt_key=\"wall_loops\">3</option><metadata key=\"vendor\" value=\"keep\"/><option opt_key=\"custom_gcode\"> G1 X1 &amp; tail </option></range></object><object id=\"0\"><range min_z=\"0.4\" max_z=\"0.6\"/></object><object id=\"9\"><range min_z=\"0.6\" max_z=\"0.8\"/></object></objects>").unwrap();
        output.finish().unwrap();
    }
    let mut q = Project::load_3mf(&ranged).unwrap();
    assert_eq!(
        q.objects.iter().map(|o| o.id).collect::<Vec<_>>(),
        vec![2, 8]
    );
    assert_eq!(q.object_layer_ranges(2).unwrap().len(), 1);
    assert_eq!(q.layer_ranges()[0].source_object_id, 2);
    assert_eq!(
        q.object_layer_ranges(2).unwrap()[0]
            .config
            .get("wall_loops")
            .map(String::as_str),
        Some("3")
    );
    assert_eq!(
        q.object_layer_ranges(2).unwrap()[0].config["custom_gcode"],
        " G1 X1 & tail "
    );
    assert!(
        !q.object_layer_ranges(2).unwrap()[0]
            .config
            .contains_key("vendor")
    );
    assert_eq!(q.unmatched_layer_range_ids(), vec![0, 9]);

    q.objects.swap(0, 1);
    q.config
        .insert("force_rewrite".into(), serde_json::json!(true));
    let reordered = temp.file("reordered.3mf");
    q.save_3mf(&reordered).unwrap();
    let r = Project::load_3mf(&reordered).unwrap();
    assert_eq!(
        r.objects.iter().map(|o| o.id).collect::<Vec<_>>(),
        vec![8, 2]
    );
    assert_eq!(r.object_layer_ranges(2).unwrap().len(), 1);
    assert_eq!(
        r.object_layer_ranges(2).unwrap()[0]
            .config
            .get("wall_loops")
            .map(String::as_str),
        Some("3")
    );
    let rewritten = std::str::from_utf8(&r.archive["Metadata/layer_config_ranges.xml"]).unwrap();
    assert!(rewritten.contains("<option opt_key=\"wall_loops\">3</option>"));
    assert!(rewritten.contains("<metadata key=\"vendor\" value=\"keep\"/>"));
    assert!(rewritten.contains("<option opt_key=\"custom_gcode\"> G1 X1 &amp; tail </option>"));
    assert_eq!(r.unmatched_layer_range_ids(), vec![0, 9]);
}

#[test]
fn new_project_and_unknown_metadata_roundtrip() {
    let temp = Temp::new();
    let a = fixture();
    let path = temp.file("saved.3mf");
    a.save_3mf(&path).unwrap();
    let b = Project::load_3mf(&path).unwrap();
    assert_eq!(a.objects[0].name, b.objects[0].name);
    assert_eq!(a.objects[0].config, b.objects[0].config);
    assert_eq!(a.objects[0].volumes[0].mesh, b.objects[0].volumes[0].mesh);
    assert_eq!(
        a.objects[0].volumes[0].config,
        b.objects[0].volumes[0].config
    );
    assert_eq!(a.config, b.config);
    for (key, value) in &a.metadata {
        assert_eq!(b.metadata.get(key), Some(value));
    }
    assert_eq!(
        b.metadata.get("BambuStudio:3mfVersion").map(String::as_str),
        Some("1")
    );
    assert_eq!(a.plates[0].name, b.plates[0].name);
    let part = b.objects[0].volumes[0].metadata.as_ref().unwrap();
    assert_eq!(part.attr("part_guid"), Some("custom-guid"));
    assert_eq!(
        part.element("extension").unwrap().attr("unknown"),
        Some("& < > \"")
    );
    assert_eq!(&*b.archive["Unknown/data.bin"], &[1, 2, 3, 0, 255]);
}

#[test]
fn all_volume_kinds_survive_save() {
    let temp = Temp::new();
    let mut p = fixture();
    let base = p.objects[0].volumes[0].clone();
    for kind in [
        VolumeKind::NegativeVolume,
        VolumeKind::Modifier,
        VolumeKind::SupportBlocker,
        VolumeKind::SupportEnforcer,
    ] {
        p.objects[0].volumes.push(Volume {
            kind,
            ..base.clone()
        });
    }
    p.save_3mf(temp.file("all.3mf")).unwrap();
    let q = Project::load(temp.file("all.3mf")).unwrap();
    assert_eq!(
        q.objects[0]
            .volumes
            .iter()
            .map(|v| v.kind)
            .collect::<Vec<_>>(),
        vec![
            VolumeKind::Part,
            VolumeKind::NegativeVolume,
            VolumeKind::Modifier,
            VolumeKind::SupportBlocker,
            VolumeKind::SupportEnforcer
        ]
    );
}

#[test]
fn edit_then_save_multi_instance_plates_and_off_plate() {
    let temp = Temp::new();
    let mut p = fixture();
    let k = InstanceRef {
        object: 2,
        instance: 0,
    };
    let k2 = p.duplicate_instance(k).unwrap();
    let p2 = p.add_plate("plate 2").unwrap();
    p.move_to_plate(k2, Some(p2)).unwrap();
    p.translate(k2, DVec3::new(100., 110., 2.)).unwrap();
    let k3 = p.duplicate_instance(k).unwrap();
    p.move_to_plate(k3, None).unwrap();
    p.save_3mf(temp.file("p.3mf")).unwrap();
    let q = Project::load(temp.file("p.3mf")).unwrap();
    assert_eq!(q.instance(k).unwrap().plate, Some(1));
    assert_eq!(q.instance(k2).unwrap().plate, Some(2));
    assert_eq!(q.instance(k3).unwrap().plate, None);
    assert_eq!(
        q.instance(k2).unwrap().transform,
        p.instance(k2).unwrap().transform
    );
    let original_geometry = q
        .archive
        .iter()
        .find(|(k, _)| k.starts_with("3D/Objects/"))
        .unwrap()
        .1
        .clone();
    let mut q = q;
    q.rotate(k2, DQuat::from_rotation_z(0.7)).unwrap();
    q.delete_instance(k).unwrap();
    q.rename_plate(2, "changed").unwrap();
    q.save_3mf(temp.file("q.3mf")).unwrap();
    let r = Project::load(temp.file("q.3mf")).unwrap();
    assert_eq!(r.objects[0].instances.len(), 2);
    assert_eq!(r.plates[1].name, "changed");
    assert!(r.archive.values().any(|v| *v == original_geometry));
}

#[test]
fn changed_mesh_is_written_and_painted_facets_preserved() {
    let temp = Temp::new();
    let p = fixture();
    p.save_3mf(temp.file("a.3mf")).unwrap();
    let mut q = Project::load(temp.file("a.3mf")).unwrap();
    std::sync::Arc::make_mut(&mut q.objects[0].volumes[0].mesh).positions[1][0] = 12.5;
    q.objects[0].volumes[0].transform = DAffine3::from_translation(DVec3::new(4., 5., 6.));
    q.save_3mf(temp.file("b.3mf")).unwrap();
    let r = Project::load(temp.file("b.3mf")).unwrap();
    assert_eq!(r.objects[0].volumes[0].mesh, q.objects[0].volumes[0].mesh);
    assert_eq!(
        r.objects[0].volumes[0].transform,
        q.objects[0].volumes[0].transform
    );
}

#[test]
fn invalid_project_does_not_replace_destination() {
    let temp = Temp::new();
    let path = temp.file("existing.3mf");
    fs::write(&path, b"previous").unwrap();
    let mut p = fixture();
    std::sync::Arc::make_mut(&mut p.objects[0].volumes[0].mesh).triangles[0][0] = 999;
    assert!(p.save_3mf(&path).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"previous");
}

#[test]
fn ascii_stl_loads_indexed_and_centers() {
    let temp = Temp::new();
    let path = temp.file("part.stl");
    fs::write(&path, "solid part\nfacet normal 0 0 1\nouter loop\nvertex 0 0 5\nvertex 10 0 5\nvertex 0 10 5\nendloop\nendfacet\nendsolid part\n").unwrap();
    let p = Project::load(&path).unwrap();
    assert_eq!(p.objects[0].volumes[0].mesh.triangles.len(), 1);
    let b = p
        .instance_bounds(InstanceRef {
            object: 1,
            instance: 0,
        })
        .unwrap()
        .unwrap();
    assert_eq!(b.min, DVec3::new(123., 123., 0.));
}

#[test]
fn component_cycles_fail_without_recursing_forever() {
    let temp = Temp::new();
    let path = temp.file("cycle.3mf");
    let mut zip = ZipWriter::new(fs::File::create(&path).unwrap());
    zip.start_file("3D/3dmodel.model", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(br#"<model><resources><object id="1"><components><component objectid="1"/></components></object></resources><build><item objectid="1"/></build></model>"#).unwrap();
    zip.finish().unwrap();
    assert!(
        Project::load(path)
            .unwrap_err()
            .to_string()
            .contains("компоненти")
    );
}

#[test]
fn unchanged_save_preserves_every_archive_entry_exactly() {
    let temp = Temp::new();
    let a = fixture();
    a.save_3mf(temp.file("a.3mf")).unwrap();
    let b = Project::load(temp.file("a.3mf")).unwrap();
    b.save_3mf(temp.file("b.3mf")).unwrap();
    let c = Project::load(temp.file("b.3mf")).unwrap();
    assert_eq!(b.archive, c.archive);
}

#[test]
fn bed_type_and_instance_assembly_follow_edits() {
    let temp = Temp::new();
    let mut p = fixture();
    p.plates[0].bed_type = Some("Textured PEI Plate".into());
    let k = InstanceRef {
        object: 2,
        instance: 0,
    };
    p.duplicate_instance(k).unwrap();
    for (i, instance) in p.objects[0].instances.iter_mut().enumerate() {
        let mut m = XmlNode::new("model_instance");
        let mut a = XmlNode::new("project:assemble_item");
        a.set("object_id", 2);
        a.set("instance_id", i);
        a.set("transform", format!("1 0 0 0 1 0 0 0 1 {} 0 0", i * 100));
        a.set("offset", "0 0 0");
        m.push(a);
        instance.metadata = Some(m);
    }
    p.save_3mf(temp.file("a.3mf")).unwrap();
    let mut q = Project::load(temp.file("a.3mf")).unwrap();
    assert_eq!(q.plates[0].bed_type.as_deref(), Some("Textured PEI Plate"));
    q.delete_instance(k).unwrap();
    q.save_3mf(temp.file("b.3mf")).unwrap();
    let r = Project::load(temp.file("b.3mf")).unwrap();
    let assembly = r.objects[0].instances[0]
        .metadata
        .as_ref()
        .unwrap()
        .element("assemble_item")
        .unwrap();
    assert_eq!(assembly.attr("instance_id"), Some("0"));
    assert_eq!(
        assembly.attr("transform"),
        Some("1 0 0 0 1 0 0 0 1 100 0 0")
    );
}

#[test]
fn deleting_plate_renumbers_membership_and_can_save_empty_project() {
    let temp = Temp::new();
    let mut p = fixture();
    let k = InstanceRef {
        object: 2,
        instance: 0,
    };
    p.add_plate("two").unwrap();
    p.move_to_plate(k, Some(2)).unwrap();
    p.delete_plate(1).unwrap();
    assert_eq!(p.instance(k).unwrap().plate, Some(1));
    p.save_3mf(temp.file("a.3mf")).unwrap();
    let q = Project::load(temp.file("a.3mf")).unwrap();
    assert_eq!(q.plates[0].name, "two");
    assert_eq!(q.instance(k).unwrap().plate, Some(1));
    p.delete_plate(1).unwrap();
    assert_eq!(p.plates.len(), 1);
    assert_eq!(p.instance(k).unwrap().plate, None);
    p.delete_object(2).unwrap();
    p.save_3mf(temp.file("empty.3mf")).unwrap();
    let q = Project::load(temp.file("empty.3mf")).unwrap();
    assert!(q.objects.is_empty());
    assert_eq!(q.plates.len(), 1);
}

#[test]
fn deleting_last_instance_removes_object_and_still_saves() {
    let temp = Temp::new();
    let mut p = fixture();
    p.delete_instance(InstanceRef {
        object: 2,
        instance: 0,
    })
    .unwrap();
    assert!(p.objects.is_empty());
    p.save_3mf(temp.file("empty.3mf")).unwrap();
    assert!(
        Project::load(temp.file("empty.3mf"))
            .unwrap()
            .objects
            .is_empty()
    );
}

#[test]
fn binary_stl_loads_and_saves_to_bambu_project() {
    let temp = Temp::new();
    let path = temp.file("binary.stl");
    let mut data = vec![0_u8; 80];
    data.extend_from_slice(&1_u32.to_le_bytes());
    for v in [0_f32, 0., 1., 0., 0., 5., 10., 0., 5., 0., 10., 5.] {
        data.extend_from_slice(&v.to_le_bytes());
    }
    data.extend_from_slice(&0_u16.to_le_bytes());
    fs::write(&path, data).unwrap();
    let p = Project::load(path).unwrap();
    p.save_3mf(temp.file("converted.3mf")).unwrap();
    let q = Project::load(temp.file("converted.3mf")).unwrap();
    assert_eq!(p.objects[0].volumes[0].mesh, q.objects[0].volumes[0].mesh);
    assert_eq!(
        p.plate_mesh(Some(1)).unwrap(),
        q.plate_mesh(Some(1)).unwrap()
    );
}

#[test]
fn legacy_triangle_ranges_become_separate_volumes() {
    let temp = Temp::new();
    let path = temp.file("legacy.3mf");
    let mut zip = ZipWriter::new(fs::File::create(&path).unwrap());
    zip.start_file("3D/3dmodel.model", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(br#"<model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02"><resources><object id="1"><mesh><vertices><vertex x="0" y="0" z="0"/><vertex x="10" y="0" z="0"/><vertex x="0" y="10" z="0"/><vertex x="100" y="0" z="0"/><vertex x="110" y="0" z="0"/><vertex x="100" y="10" z="0"/></vertices><triangles><triangle v1="0" v2="1" v3="2"/><triangle v1="3" v2="4" v3="5"/></triangles></mesh></object></resources><build><item objectid="1"/></build></model>"#).unwrap();
    zip.start_file(
        "Metadata/model_settings.config",
        SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(br#"<config><object id="1"><volume firstid="0" lastid="0"><metadata key="name" value="first"/></volume><volume firstid="1" lastid="1"><metadata key="name" value="second"/><metadata key="part_type" value="negative_part"/></volume></object></config>"#).unwrap();
    zip.finish().unwrap();
    let mut p = Project::load(&path).unwrap();
    assert_eq!(p.objects[0].volumes.len(), 2);
    assert_eq!(p.objects[0].volumes[1].name, "second");
    assert_eq!(p.objects[0].volumes[1].kind, VolumeKind::NegativeVolume);
    assert_eq!(p.objects[0].volumes[0].mesh.positions.len(), 3);
    assert_eq!(
        p.instance_bounds(InstanceRef {
            object: 1,
            instance: 0
        })
        .unwrap()
        .unwrap()
        .max
        .x,
        10.
    );
    p.rename_plate(1, "edited").unwrap();
    p.save_3mf(temp.file("modern.3mf")).unwrap();
    let q = Project::load(temp.file("modern.3mf")).unwrap();
    assert_eq!(q.objects[0].volumes.len(), 2);
    assert_eq!(p.objects[0].volumes[1].mesh, q.objects[0].volumes[1].mesh);
}

#[test]
fn real_angel_ranges_and_negative_parts_survive_edits_when_available() {
    // Справжній проєкт розробника; шлях — змінна SLICENV_ANGEL_3MF, без неї тест пропускається.
    let Some(path) = std::env::var_os("SLICENV_ANGEL_3MF").map(std::path::PathBuf::from) else {
        return;
    };
    if !path.exists() {
        return;
    }
    let temp = Temp::new();
    let mut project = Project::load(&path).unwrap();
    let assembly = project
        .objects
        .iter()
        .find(|o| o.name == "Assembly")
        .unwrap()
        .id;
    let ranges = project.object_layer_ranges(assembly).unwrap();
    assert_eq!(ranges.len(), 2);
    assert_eq!(
        ranges[0].config.get("layer_height").map(String::as_str),
        Some("0.2")
    );
    assert_eq!(
        ranges[0]
            .config
            .get("sparse_infill_density")
            .map(String::as_str),
        Some("0%")
    );
    assert!(project.unmatched_layer_range_ids().is_empty());
    let before = project.clone();
    let duplicate = project.duplicate_object(assembly).unwrap();
    assert_eq!(project.object_layer_ranges(duplicate).unwrap().len(), 2);
    project.delete_object(duplicate).unwrap();
    assert!(
        project
            .layer_ranges()
            .iter()
            .all(|r| r.source_object_id != duplicate)
    );
    project
        .objects
        .iter_mut()
        .find(|o| o.id == assembly)
        .unwrap()
        .name = "Renamed assembly".into();
    project.save_3mf(temp.file("angel-edited.3mf")).unwrap();
    let result = Project::load(temp.file("angel-edited.3mf")).unwrap();
    assert_eq!(
        result.object_layer_ranges(assembly).unwrap()[0].config,
        ranges_from(&before, assembly)
    );
    assert_eq!(
        result
            .object(assembly)
            .unwrap()
            .volumes
            .iter()
            .filter(|v| v.kind == VolumeKind::NegativeVolume)
            .count(),
        2
    );
    let mut changed = project
        .object_layer_ranges(assembly)
        .unwrap()
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    changed[0]
        .config
        .insert("layer_height".into(), "0.16".into());
    project.set_layer_ranges(assembly, changed).unwrap();
    project.save_3mf(temp.file("angel-ranges.3mf")).unwrap();
    let result = Project::load(temp.file("angel-ranges.3mf")).unwrap();
    assert_eq!(
        result.object_layer_ranges(assembly).unwrap()[0].config["layer_height"],
        "0.16"
    );
}
fn ranges_from(p: &Project, id: u32) -> Config {
    p.object_layer_ranges(id).unwrap()[0].config.clone()
}

/// Бібліотека показує OBJ — `Project::load` має його відкривати, як STL (регістр розширення не важить).
#[test]
fn load_opens_obj_like_stl() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Cube.OBJ");
    std::fs::write(
        &path,
        "v 0 0 0\nv 10 0 0\nv 0 10 0\nv 0 0 10\nf 1 3 2\nf 1 2 4\nf 1 4 3\nf 2 3 4\n",
    )
    .unwrap();
    let project = project::Project::load(&path).unwrap();
    assert_eq!(project.objects.len(), 1);
    assert_eq!(project.objects[0].name, "Cube");
    assert_eq!(project.objects[0].volumes[0].mesh.triangles.len(), 4);
}

#[test]
fn printable_accepts_xml_booleans_and_survives_edited_roundtrip() {
    let temp = Temp::new();
    for (value, expected) in [
        (None, true),
        (Some("0"), false),
        (Some("false"), false),
        (Some(" false "), false),
        (Some("1"), true),
        (Some("true"), true),
        (Some("invalid"), true),
    ] {
        let attr = value.map(|v| format!(" printable=\"{v}\"")).unwrap_or_default();
        let xml = format!(r#"<model><resources><object id="1"><mesh><vertices><vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/></vertices><triangles><triangle v1="0" v2="1" v3="2"/></triangles></mesh></object></resources><build><item objectid="1"{attr}/></build></model>"#);
        let input = temp.file("boolean.3mf");
        let mut zip = ZipWriter::new(fs::File::create(&input).unwrap());
        zip.start_file("3D/3dmodel.model", SimpleFileOptions::default()).unwrap();
        zip.write_all(xml.as_bytes()).unwrap();
        zip.finish().unwrap();
        let loaded = Project::load_3mf(&input);
        if value == Some("invalid") {
            assert!(loaded.is_err());
            continue;
        }
        let mut p = loaded.unwrap();
        assert_eq!(p.objects[0].instances[0].printable, expected, "{value:?}");
        p.objects[0].name = "edited".into();
        let output = temp.file("edited.3mf");
        p.save_3mf(&output).unwrap();
        let q = Project::load_3mf(&output).unwrap();
        assert_eq!(q.objects[0].instances[0].printable, expected, "{value:?}");
    }
}

/// Нові сітки різних об’єктів — з різними id: рушії зіставляють `part id` з сіткою лише за
/// id, і однакові id у різних `3D/Objects/*.model` давали всім об’єктам сітку першого.
#[test]
fn new_meshes_of_different_objects_get_unique_ids() {
    let temp = Temp::new();
    let mut p = fixture();
    let mut second = p.objects[0].clone();
    second.id = 3;
    second.name = "second".into();
    std::sync::Arc::make_mut(&mut second.volumes[0].mesh).positions[3][2] = 25.;
    p.objects.push(second);
    p.save_3mf(temp.file("two.3mf")).unwrap();

    let file = fs::File::open(temp.file("two.3mf")).unwrap();
    let mut zip = zip::ZipArchive::new(file).unwrap();
    let mut read = |name: &str| {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut zip.by_name(name).unwrap(), &mut s).unwrap();
        s
    };
    let ids = |xml: &str, attr: &str| -> Vec<String> {
        xml.split(&format!("{attr}=\"")[..])
            .skip(1)
            .map(|rest| rest.split('"').next().unwrap().to_owned())
            .collect()
    };
    let model = read("3D/3dmodel.model");
    let components: Vec<_> = model
        .split("<component ")
        .skip(1)
        .flat_map(|c| ids(&format!(" {}", c.split('>').next().unwrap()), " objectid"))
        .collect();
    assert_eq!(components.len(), 2, "{model}");
    assert_ne!(components[0], components[1], "{model}");
    let settings = read("Metadata/model_settings.config");
    for id in &components {
        assert!(
            settings.contains(&format!("<part id=\"{id}\"")),
            "немає part id={id}: {settings}"
        );
    }

    let q = Project::load(temp.file("two.3mf")).unwrap();
    assert_eq!(q.objects[1].volumes[0].mesh.positions[3][2], 25.);
    assert_eq!(q.objects[0].volumes[0].mesh.positions[3][2], 10.);
}

#[test]
fn zip_bomb_entry_is_rejected_by_decompressed_size() {
    use std::io::Read as _;
    // 8 МБ нулів стискаються в кілька КБ: межу перевіряють розпаковані дані, а не заголовок.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bomb.zip");
    let mut zip = ZipWriter::new(std::fs::File::create(&path).unwrap());
    zip.start_file(
        "3D/3dmodel.model",
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
    )
    .unwrap();
    zip.write_all(&vec![0u8; 8 * 1024 * 1024]).unwrap();
    zip.finish().unwrap();
    assert!(std::fs::metadata(&path).unwrap().len() < 64 * 1024);

    let mut archive = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let mut entry = archive.by_index(0).unwrap();
    let err = read_limited(&mut entry, 1024 * 1024).unwrap_err();
    assert!(err.to_string().contains("zip-бомба"), "{err}");
    drop(entry);

    let mut entry = archive.by_index(0).unwrap();
    let all = read_limited(&mut entry, 8 * 1024 * 1024).unwrap();
    assert_eq!(all.len(), 8 * 1024 * 1024);
    let mut rest = Vec::new();
    entry.read_to_end(&mut rest).unwrap();
    assert!(rest.is_empty());
}
