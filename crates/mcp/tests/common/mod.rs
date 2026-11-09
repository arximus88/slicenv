#![allow(dead_code)]
use std::path::{Path, PathBuf};

use project::{DAffine3, DVec3, IndexedMesh, Instance, Object, Project, Volume};

fn cube(side: f64) -> IndexedMesh {
    let s = side;
    IndexedMesh {
        positions: vec![
            [0., 0., 0.],
            [s, 0., 0.],
            [s, s, 0.],
            [0., s, 0.],
            [0., 0., s],
            [s, 0., s],
            [s, s, s],
            [0., s, s],
        ],
        triangles: vec![
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [1, 2, 6],
            [1, 6, 5],
            [2, 3, 7],
            [2, 7, 6],
            [3, 0, 4],
            [3, 4, 7],
        ],
        ..IndexedMesh::default()
    }
}

/// Дві кубики 20 мм і 10 мм на пластині 1, збережені як справжній 3MF.
pub fn write_fixture(dir: &Path) -> PathBuf {
    let mut p = Project::default();
    p.add_plate("Plate 1").unwrap();
    for (id, side, x) in [(1, 20.0, 30.0), (2, 10.0, 100.0)] {
        p.objects.push(Object {
            id,
            name: format!("cube{id}"),
            volumes: vec![Volume {
                name: "part".into(),
                mesh: cube(side).into(),
                ..Volume::default()
            }],
            instances: vec![Instance {
                transform: DAffine3::from_translation(DVec3::new(x, 40.0, 0.0)),
                ..Instance::default()
            }],
            ..Object::default()
        });
    }
    p.sync_plates();
    let path = dir.join("fixture.3mf");
    p.save_3mf(&path).unwrap();
    path
}

/// Мінімальний G-code у форматі Bambu: 2 шари, одна екструзія на шар.
pub const GCODE: &str = "\
; HEADER_BLOCK_START
; total layer number: 2
; total filament weight [g] : 1.5
; model printing time: 1m 0s; total estimated time: 2m 0s
; HEADER_BLOCK_END
;LAYER_CHANGE
;Z:0.2
; LAYER_HEIGHT: 0.2
; FEATURE: Outer wall
G1 X10 Y10 F3000
G1 X20 Y10 E1
;LAYER_CHANGE
;Z:0.4
; LAYER_HEIGHT: 0.2
G1 X20 Y20 E1
";

/// Каталог профілів із трьох файлів: принтер, процес і філамент із успадкуванням.
pub fn write_profiles(root: &Path) {
    let system = root.join("system/BBL");
    for (dir, name, body) in [
        (
            "machine",
            "Printer A",
            r#"{"name":"Printer A","type":"machine","printable_area":["0x0","256x256"],"nozzle_diameter":["0.4"]}"#,
        ),
        (
            "process",
            "Base",
            r#"{"name":"Base","type":"process","instantiation":"false","layer_height":"0.2","wall_loops":"2","compatible_printers":["Printer A"]}"#,
        ),
        (
            "process",
            "Fine",
            r#"{"name":"Fine","type":"process","inherits":"Base","layer_height":"0.1"}"#,
        ),
        (
            "filament",
            "PLA",
            r#"{"name":"PLA","type":"filament","compatible_printers":["Printer A"],"nozzle_temperature":["220"]}"#,
        ),
    ] {
        std::fs::create_dir_all(system.join(dir)).unwrap();
        std::fs::write(system.join(dir).join(format!("{name}.json")), body).unwrap();
    }
    std::fs::write(
        root.join("system/BBL.json"),
        r#"{"name":"BBL","version":"1"}"#,
    )
    .unwrap();
}
