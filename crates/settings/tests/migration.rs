use serde_json::{Map, json};
use settings::{ConfigSet, MigrationStatus, Schema, Scope, preview_migration};

#[test]
fn previews_invalid_values_without_mutating_config_and_preserves_unknowns() {
    let target = Schema::load_orca().unwrap();
    let mut global = Map::new();
    global.insert("raft_first_layer_expansion".into(), json!("-1"));
    global.insert("future_engine_key".into(), json!(["keep", "as-is"]));
    let original = global.clone();
    let defaults = Map::from_iter([("raft_first_layer_expansion".into(), json!("2"))]);
    let report = preview_migration(
        &[ConfigSet {
            label: "global",
            scope: Scope::Global,
            values: &global,
        }],
        &target,
        &defaults,
    );
    assert_eq!(global, original);
    let invalid = report
        .entries
        .iter()
        .find(|entry| entry.key == "raft_first_layer_expansion")
        .unwrap();
    assert_eq!(invalid.label, "global");
    assert_eq!(invalid.old_value, json!("-1"));
    assert_eq!(invalid.proposed_value, Some(json!("2")));
    assert!(invalid.reason.contains("minimum"));
    assert_eq!(invalid.status, MigrationStatus::Invalid);
    let unknown = report
        .entries
        .iter()
        .find(|entry| entry.key == "future_engine_key")
        .unwrap();
    assert_eq!(unknown.proposed_value, None);
    assert_eq!(unknown.old_value, json!(["keep", "as-is"]));
    assert_eq!(unknown.status, MigrationStatus::Unsupported);
}

#[test]
fn reports_supplied_object_volume_and_layer_range_labels() {
    let target = Schema::load_orca().unwrap();
    let mut configs = Vec::new();
    let mut maps = Vec::new();
    for (label, scope) in [
        ("object 7", Scope::Object),
        ("volume 2", Scope::Region),
        ("layer range 1", Scope::Object),
    ] {
        let mut values = Map::new();
        values.insert("raft_first_layer_expansion".into(), json!("-1"));
        maps.push(values);
        configs.push((label, scope));
    }
    let sets: Vec<_> = configs
        .iter()
        .zip(&maps)
        .map(|((label, scope), values)| ConfigSet {
            label,
            scope: *scope,
            values,
        })
        .collect();
    let defaults = Map::from_iter([("raft_first_layer_expansion".into(), json!("2"))]);
    let report = preview_migration(&sets, &target, &defaults);
    assert_eq!(
        report
            .entries
            .iter()
            .map(|entry| entry.label.as_str())
            .collect::<Vec<_>>(),
        ["object 7", "volume 2", "layer range 1"]
    );
}

#[test]
fn rejects_an_invalid_runtime_default_as_a_proposal() {
    let target = Schema::load_orca().unwrap();
    let values = Map::from_iter([("raft_first_layer_expansion".into(), json!("-1"))]);
    let defaults = Map::from_iter([("raft_first_layer_expansion".into(), json!(-4.0))]);
    let report = preview_migration(
        &[ConfigSet {
            label: "global",
            scope: Scope::Global,
            values: &values,
        }],
        &target,
        &defaults,
    );
    assert_eq!(report.entries[0].proposed_value, None);
}

#[test]
fn validates_minimum_boundary_and_enum_membership() {
    let target = Schema::load_orca().unwrap();
    let enum_key = target
        .options
        .iter()
        .find(|(_, def)| !def.enum_values.is_empty())
        .map(|(key, _)| key.clone())
        .unwrap();
    let values = Map::from_iter([
        ("raft_first_layer_expansion".into(), json!("0")),
        (enum_key.clone(), json!("not-a-valid-enum-value")),
    ]);
    let report = preview_migration(
        &[ConfigSet {
            label: "global",
            scope: Scope::Global,
            values: &values,
        }],
        &target,
        &Map::new(),
    );
    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.entries[0].key, enum_key);
    assert!(report.entries[0].reason.contains("not one of"));
}

/// 3MF з Bambu Studio 2.7 (3mf_Spider_Web_S-XL.3mf): нулі «філамент для елемента», на яких
/// OrcaSlicer 2.3 падає з −18, і −1 («авто» Bambu), які Orca 2.3 позначає як недопустимі.
#[test]
fn bambu_project_values_outside_orca_ranges_get_fixes() {
    let target = Schema::load_orca().unwrap();
    let global = Map::from_iter(
        [
            ("wall_filament", json!("0")),
            ("sparse_infill_filament", json!("0")),
            ("solid_infill_filament", json!("0")),
            ("support_filament", json!("0")),
            ("tree_support_wall_count", json!("-1")),
            ("raft_first_layer_expansion", json!("-1")),
            ("wall_loops", json!("-3")),
            ("additional_cooling_fan_speed", json!(["50", "150"])),
        ]
        .map(|(k, v)| (k.to_owned(), v)),
    );
    let defaults = Map::from_iter([
        ("tree_support_wall_count".into(), json!("0")),
        ("raft_first_layer_expansion".into(), json!("2")),
    ]);
    let report = preview_migration(
        &[ConfigSet {
            label: "global",
            scope: Scope::Global,
            values: &global,
        }],
        &target,
        &defaults,
    );
    let entry = |key: &str| report.entries.iter().find(|e| e.key == key);
    for key in ["wall_filament", "sparse_infill_filament", "solid_infill_filament"] {
        let entry = entry(key).unwrap_or_else(|| panic!("{key} не знайдено"));
        assert_eq!(entry.status, MigrationStatus::Invalid, "{key}");
        assert_eq!(entry.proposed_value, Some(json!("1")), "{key}");
        assert!(entry.reason.contains("clamped"), "{}", entry.reason);
    }
    // support_filament у будь-якій Orca має min 0 — нуль коректний.
    assert!(entry("support_filament").is_none());
    // −1 — «авто» Bambu: не межа діапазону, а типове значення рушія.
    assert_eq!(entry("tree_support_wall_count").unwrap().proposed_value, Some(json!("0")));
    assert_eq!(entry("raft_first_layer_expansion").unwrap().proposed_value, Some(json!("2")));
    assert!(entry("raft_first_layer_expansion").unwrap().reason.contains("auto"));
    // Звичайне порушення меж обрізається, вектор — поелементно.
    let wall_loops = entry("wall_loops").unwrap();
    assert_eq!(wall_loops.proposed_value, Some(json!("0")));
    assert!(wall_loops.reason.contains("[0, 1000]"), "{}", wall_loops.reason);
    assert_eq!(
        entry("additional_cooling_fan_speed").unwrap().proposed_value,
        Some(json!(["50", "100"]))
    );
}
