//! Тести шару інструментів на малому 3MF із двома кубами.
mod common;

use mcp::Session;
use mcp::session::*;
use serde_json::{Value, json};
use slicer::PresetStore;

fn open(allow_edit: bool) -> (Session, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = common::write_fixture(dir.path());
    let mut s = Session::new(allow_edit);
    s.open_project(OpenArgs {
        path: path.to_str().unwrap().into(),
    })
    .unwrap();
    (s, dir)
}

fn args<T: serde::de::DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}

fn object(summary: &Value, id: u64) -> Value {
    summary["plates"][0]["objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["object"] == id)
        .unwrap()
        .clone()
}

#[test]
fn summary_reports_plates_objects_and_sizes() {
    let (s, _dir) = open(false);
    let summary = s.project_summary().unwrap();
    assert_eq!(summary["plates"].as_array().unwrap().len(), 1);
    assert_eq!(object(&summary, 1)["size_mm"], json!([20.0, 20.0, 20.0]));
    assert_eq!(object(&summary, 2)["size_mm"], json!([10.0, 10.0, 10.0]));
    assert_eq!(object(&summary, 1)["triangles"], 12);
    assert_eq!(summary["unsaved_changes"], false);
}

#[test]
fn rejects_wrong_extension_and_missing_file() {
    let mut s = Session::new(false);
    let err = s
        .open_project(OpenArgs {
            path: "/etc/passwd".into(),
        })
        .unwrap_err();
    assert!(err.to_string().contains("3mf"), "{err:#}");
    assert!(
        s.open_project(OpenArgs {
            path: "/nope/x.3mf".into()
        })
        .is_err()
    );
    assert!(
        s.project_summary()
            .unwrap_err()
            .to_string()
            .contains("open_project")
    );
}

#[test]
fn read_only_session_refuses_every_edit() {
    let (mut s, dir) = open(false);
    let t: TransformArgs = args(json!({"object": 1, "translate": [1, 0, 0]}));
    assert!(
        s.transform_object(t)
            .unwrap_err()
            .to_string()
            .contains("--allow-edit")
    );
    assert!(s.arrange_plate(args(json!({"plate": 1}))).is_err());
    assert!(s.undo().is_err());
    let save = SaveArgs {
        path: dir.path().join("o.3mf").to_str().unwrap().into(),
    };
    assert!(s.save_project(save).is_err());
    assert!(!dir.path().join("o.3mf").exists());
}

#[test]
fn transform_is_undoable_and_redoable() {
    let (mut s, _dir) = open(true);
    let before = object(&s.project_summary().unwrap(), 1);
    let out = s
        .transform_object(args(
            json!({"object": 1, "scale": [2, 2, 2], "translate": [10, 0, 0], "drop_to_bed": true}),
        ))
        .unwrap();
    assert_eq!(out["size_mm"], json!([40.0, 40.0, 40.0]));
    let summary = s.project_summary().unwrap();
    assert_eq!(summary["unsaved_changes"], true);
    assert!(summary["can_undo"].is_string());

    s.undo().unwrap();
    assert_eq!(object(&s.project_summary().unwrap(), 1), before);
    s.redo().unwrap();
    assert_eq!(
        object(&s.project_summary().unwrap(), 1)["size_mm"],
        json!([40.0, 40.0, 40.0])
    );
    assert!(s.redo().is_err(), "нічого повторювати");
}

#[test]
fn rotate_changes_bounds() {
    let (mut s, _dir) = open(true);
    let out = s
        .transform_object(args(
            json!({"object": 1, "rotate_deg": [0, 0, 45], "drop_to_bed": true}),
        ))
        .unwrap();
    let size = out["size_mm"].as_array().unwrap();
    assert!((size[0].as_f64().unwrap() - 28.28).abs() < 0.02, "{size:?}");
    assert_eq!(out["bounds_min"][2], 0.0);
}

#[test]
fn failed_edit_leaves_project_and_history_untouched() {
    let (mut s, _dir) = open(true);
    let before = s.project_summary().unwrap();
    assert!(
        s.transform_object(args(json!({"object": 99, "translate": [1, 0, 0]})))
            .is_err()
    );
    // Масштаб 0 відхиляється вже після прочитаного instance — стан не змінюється.
    assert!(
        s.transform_object(args(
            json!({"object": 1, "translate": [5, 0, 0], "scale": [0, 1, 1]})
        ))
        .is_err()
    );
    assert_eq!(s.project_summary().unwrap(), before);
    assert!(s.undo().is_err());
}

#[test]
fn arrange_separates_objects_inside_bed() {
    let (mut s, _dir) = open(true);
    // Спершу зсуваємо другий куб на перший.
    s.transform_object(args(json!({"object": 2, "translate": [-70, 0, 0]})))
        .unwrap();
    let out = s
        .arrange_plate(args(json!({"plate": 1, "spacing_mm": 5})))
        .unwrap();
    let boxes: Vec<(Vec<f64>, Vec<f64>)> = out["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| {
            let v = |k: &str| {
                o[k].as_array()
                    .unwrap()
                    .iter()
                    .map(|x| x.as_f64().unwrap())
                    .collect()
            };
            (v("bounds_min"), v("bounds_max"))
        })
        .collect();
    let (a, b) = (&boxes[0], &boxes[1]);
    let overlap = |i: usize| a.0[i] < b.1[i] && b.0[i] < a.1[i];
    assert!(
        !(overlap(0) && overlap(1)),
        "об'єкти перекриваються: {boxes:?}"
    );
    assert!(
        boxes
            .iter()
            .all(|(lo, hi)| lo[0] >= 0.0 && lo[1] >= 0.0 && hi[0] <= 256.0 && hi[1] <= 256.0)
    );
    s.undo().unwrap();
}

#[test]
fn save_writes_a_reopenable_3mf() {
    let (mut s, dir) = open(true);
    s.transform_object(args(json!({"object": 1, "translate": [0, 25, 0]})))
        .unwrap();
    let out = dir.path().join("edited.3mf");
    s.save_project(SaveArgs {
        path: out.to_str().unwrap().into(),
    })
    .unwrap();
    assert_eq!(s.project_summary().unwrap()["unsaved_changes"], false);
    assert_eq!(s.project_summary().unwrap()["path"], out.to_str().unwrap());

    let mut t = Session::new(false);
    let summary = t
        .open_project(OpenArgs {
            path: out.to_str().unwrap().into(),
        })
        .unwrap();
    assert_eq!(object(&summary, 1)["bounds_min"][1], 65.0);
}

#[test]
fn explain_setting_uses_schema_and_suggests_on_typo() {
    let s = Session::new(false);
    let v = s
        .explain_setting(ExplainArgs {
            key: "sparse_infill_density".into(),
        })
        .unwrap();
    assert_eq!(v["max"], 100.0);
    assert!(!v["description_uk"].as_str().unwrap().contains("%%"));
    let err = s
        .explain_setting(ExplainArgs {
            key: "sparse_infill".into(),
        })
        .unwrap_err();
    assert!(err.to_string().contains("sparse_infill_density"), "{err:#}");
}

#[test]
fn gcode_stats_reads_plain_gcode_and_gcode_3mf() {
    let dir = tempfile::tempdir().unwrap();
    let gcode = dir.path().join("a.gcode");
    std::fs::write(&gcode, common::GCODE).unwrap();
    let s = Session::new(false);
    let v = s
        .gcode_stats(GcodeArgs {
            path: gcode.to_str().unwrap().into(),
        })
        .unwrap();
    assert_eq!(v["layers"], 2);
    assert_eq!(v["print_time_s"], 120);
    assert_eq!(v["filament_g"], 1.5);

    let packed = dir.path().join("a.gcode.3mf");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&packed).unwrap());
    zip.start_file(
        "Metadata/plate_1.gcode",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    std::io::Write::write_all(&mut zip, common::GCODE.as_bytes()).unwrap();
    zip.finish().unwrap();
    let v = s
        .gcode_stats(GcodeArgs {
            path: packed.to_str().unwrap().into(),
        })
        .unwrap();
    assert_eq!(v["layers"], 2);

    let err = s
        .gcode_stats(GcodeArgs {
            path: "/etc/shadow".into(),
        })
        .unwrap_err();
    assert!(err.to_string().contains("gcode"), "{err:#}");
}

#[test]
fn profiles_are_listed_compatibly_and_flattened_with_key_filter() {
    let root = tempfile::tempdir().unwrap();
    common::write_profiles(root.path());
    let own = tempfile::tempdir().unwrap();
    let store = PresetStore::open_with_own_root(root.path(), own.path()).unwrap();
    let mut s = Session::new(false).with_store(store);

    let v = s
        .list_profiles(args(json!({"kind": "process", "machine": "Printer A"})))
        .unwrap();
    assert_eq!(v["names"], json!(["Fine"]), "службовий Base не показується");
    assert!(
        s.list_profiles(args(json!({"kind": "process"}))).is_err(),
        "без проєкту потрібен machine"
    );
    let v = s.list_profiles(args(json!({"kind": "machine"}))).unwrap();
    assert_eq!(v["names"], json!(["Printer A"]));

    let v = s
        .get_profile(args(
            json!({"kind": "process", "name": "Fine", "keys": ["layer_height", "wall_loops"]}),
        ))
        .unwrap();
    assert_eq!(
        v["values"],
        json!({"layer_height": "0.1", "wall_loops": "2"})
    );
    assert!(v["total_keys"].as_u64().unwrap() > 2);
    let v = s
        .get_profile(args(
            json!({"kind": "process", "name": "Fine", "contains": "LAYER"}),
        ))
        .unwrap();
    assert_eq!(v["returned"], 1);
    assert!(
        s.get_profile(args(json!({"kind": "process", "name": "Missing"})))
            .is_err()
    );
}
