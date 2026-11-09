use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::Result;
use serde_json::{Map, Value, json};
use settings::{A1_PRESETS, EditablePreset, Mode, PresetType, Schema, TypedValue, UserPresetInfo};
use slicer::PresetStore;

fn fixture(index: usize) -> Map<String, Value> {
    let bytes = [
        include_str!("fixtures/a1-0.json"),
        include_str!("fixtures/a1-1.json"),
        include_str!("fixtures/a1-2.json"),
        include_str!("fixtures/a1-3.json"),
        include_str!("fixtures/a1-4.json"),
    ];
    serde_json::from_str(bytes[index]).unwrap()
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "bambu-settings-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn schema_and_source_quirks() -> Result<()> {
    let schema = Schema::load()?;
    assert_eq!(schema.options.len(), 702);
    let height = &schema.options["layer_height"];
    assert_eq!(height.default, json!(0.2));
    assert_eq!(height.sidetext, "mm");
    assert_eq!(height.mode, Mode::Simple);
    assert_eq!(
        schema.options["machine_max_acceleration_z"].default,
        json!([500.0, 200.0])
    );
    assert_eq!(
        schema.options["machine_max_speed_x"].full_label,
        "Maximum speed X"
    );
    assert!(schema.options["filament_retraction_length"].nullable);
    assert_eq!(
        schema.options["filament_retraction_length"].default,
        schema.options["retraction_length"].default
    );
    assert_eq!(
        schema.options["top_surface_pattern"].enum_values,
        schema.options["bottom_surface_pattern"].enum_values
    );
    assert!(
        schema.options["filament_type"]
            .enum_values
            .contains(&"PETG".into())
    );
    assert_eq!(
        schema.options["filament_scarf_seam_type"].default,
        json!(["none"])
    );
    assert_eq!(schema.options["nozzle_temperature"].max, Some(360.0));
    assert_eq!(
        schema.missing_layout_keys(),
        [
            "max_volumetric_extrusion_rate_slope_negative",
            "max_volumetric_extrusion_rate_slope_positive"
        ]
    );
    let pages = schema.pages(PresetType::Process);
    assert_eq!(
        pages.iter().map(|p| p.title.as_str()).collect::<Vec<_>>(),
        ["Quality", "Strength", "Speed", "Support", "Others"]
    );
    assert_eq!(pages[0].groups[0].lines[0].options, ["layer_height"]);
    assert_eq!(
        schema
            .pages(PresetType::Machine)
            .iter()
            .map(|p| p.title.as_str())
            .collect::<Vec<_>>(),
        [
            "Basic information",
            "Machine gcode",
            "Motion ability",
            "Extruder",
            "Notes"
        ]
    );
    let filament = schema.pages(PresetType::Filament);
    assert_eq!(filament[2].title, "Setting Overrides");
    assert!(
        filament[2]
            .groups
            .iter()
            .flat_map(|g| &g.lines)
            .flat_map(|l| &l.options)
            .any(|k| k == "filament_wipe")
    );

    Ok(())
}

#[test]
fn roundtrip_every_a1_key() -> Result<()> {
    let schema = Schema::load()?;
    let mut count = 0;
    for (index, &(_, name)) in A1_PRESETS.iter().enumerate() {
        let values = fixture(index);
        let coverage = schema.coverage(&values);
        assert!(
            coverage.missing.is_empty(),
            "{name}: {:?}",
            coverage.missing
        );
        assert_eq!(coverage.percent, 100.0);
        assert_eq!(coverage.raw_total, values.len());
        assert_eq!(coverage.total + coverage.metadata.len(), values.len());
        for (key, raw) in values {
            let parsed = schema
                .parse(&key, &raw)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(parsed.to_json(), raw, "{name}/{key}");
            count += 1;
        }
    }
    assert_eq!(count, 773);
    Ok(())
}

#[test]
fn live_a1_roundtrip_when_installed() -> Result<()> {
    let root = slicer::product::env("SLICENV_PRESET_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(PresetStore::default_root);
    if !root.join("system/BBL").is_dir() {
        eprintln!("Bambu presets unavailable; the frozen A1 fixtures are tested separately");
        return Ok(());
    }
    let store = PresetStore::open(&root)?;
    let schema = Schema::load()?;
    for &(kind, name) in A1_PRESETS {
        let values = store.flatten(kind.into(), name)?;
        assert_eq!(
            schema.coverage(&values).missing,
            Vec::<String>::new(),
            "{name}"
        );
        for (key, raw) in values {
            assert_eq!(schema.parse(&key, &raw)?.to_json(), raw, "{name}/{key}");
        }
    }
    Ok(())
}

#[test]
fn typed_values_and_exact_spelling() -> Result<()> {
    let schema = Schema::load()?;
    for (key, raw, typed) in [
        ("layer_height", json!("0.2000"), TypedValue::Float(0.2)),
        (
            "sparse_infill_density",
            json!("15%"),
            TypedValue::Percent(15.0),
        ),
        (
            "filament_retraction_length",
            json!(["nil"]),
            TypedValue::Vector(vec![TypedValue::Nil]),
        ),
        (
            "nozzle_diameter",
            json!(["1", "2"]),
            TypedValue::Vector(vec![TypedValue::Float(1.0), TypedValue::Float(2.0)]),
        ),
        (
            "printable_area",
            json!("0x0,256x0"),
            TypedValue::Vector(vec![
                TypedValue::Point(vec![0.0, 0.0]),
                TypedValue::Point(vec![256.0, 0.0]),
            ]),
        ),
    ] {
        let parsed = schema.parse(key, &raw)?;
        assert_eq!(parsed.typed(), &typed);
        assert_eq!(parsed.to_json(), raw);
    }
    assert_eq!(
        schema.format("sparse_infill_density", &TypedValue::Percent(15.0))?,
        json!("15%")
    );
    assert_eq!(
        schema.format("nozzle_diameter", &TypedValue::Float(0.4))?,
        json!(["0.4"])
    );
    assert_eq!(
        schema.format("enable_support", &TypedValue::Bool(true))?,
        json!("1")
    );
    let raw = json!(["0x0,1x0", "2x2,3x3"]);
    let parsed = schema.parse("extruder_printable_area", &raw)?;
    assert_eq!(
        schema.format("extruder_printable_area", parsed.typed())?,
        raw
    );
    let legacy = json!("0x0,1x0#2x2,3x3");
    let legacy_parsed = schema.parse("extruder_printable_area", &legacy)?;
    assert_eq!(legacy_parsed.typed(), parsed.typed());
    assert_eq!(legacy_parsed.to_json(), legacy);
    assert_eq!(
        schema.format("extruder_printable_area", legacy_parsed.typed())?,
        raw
    );
    let point = TypedValue::Point(vec![0.5, 0.5]);
    assert_eq!(schema.format("best_object_pos", &point)?, json!("0.5,0.5"));
    assert_eq!(
        schema.parse("best_object_pos", &json!("0.5,0.5"))?.typed(),
        &point
    );
    assert!(
        schema
            .format("layer_height", &TypedValue::Bool(true))
            .is_err()
    );
    Ok(())
}

#[test]
fn validation_checks_shape_bounds_enums_and_nil() -> Result<()> {
    let schema = Schema::load()?;
    for (key, raw, message) in [
        ("layer_height", json!("-1"), "minimum"),
        ("sparse_infill_density", json!("101%"), "maximum"),
        ("layer_height", json!("NaN"), "finite"),
        ("wall_loops", json!("1.5"), "integer"),
        ("enable_support", json!("true"), "0 or 1"),
        ("layer_height", json!(["0.2"]), "scalar"),
        ("nozzle_diameter", json!([0.4]), "strings"),
        ("seam_position", json!("missing"), "not one of"),
        ("layer_height", json!("nil"), "nullable"),
        ("printable_area", json!(["0x0x1"]), "2 coordinates"),
        ("missing_bambu_key", json!("1"), "unknown option"),
    ] {
        let err = schema.validate(key, &raw).unwrap_err().to_string();
        assert!(err.contains(key) && err.contains(message), "{err}");
    }
    schema.validate("filament_retraction_length", &json!(["nil", "0.8"]))?;
    schema.validate("filament_z_hop_types", &json!(["nil"]))?;
    schema.validate("sparse_infill_density", &json!("15"))?;
    assert!(matches!(
        schema.parse("bambu_only", &json!({"x":1}))?.typed(),
        TypedValue::Unknown(_)
    ));
    Ok(())
}

#[test]
fn edits_reset_and_layout_values() -> Result<()> {
    let schema = Schema::load()?;
    let (kind, parent) = A1_PRESETS[1];
    let base = fixture(1);
    let mut preset = EditablePreset::new(&schema, kind, parent, base.clone());
    assert_eq!(preset.dirty_keys().count(), 0);
    preset.set("layer_height", json!("0.24"))?;
    assert!(preset.is_dirty("layer_height"));
    assert_eq!(preset.dirty_keys().collect::<Vec<_>>(), ["layer_height"]);
    let page = preset.pages().next().unwrap();
    let group = page.groups().next().unwrap();
    let line = group.lines().next().unwrap();
    let option = line.options().next().unwrap();
    assert_eq!(option.key, "layer_height");
    assert_eq!(option.value, Some(&json!("0.24")));
    assert_eq!(option.metadata.unwrap().label, "Layer height");
    assert!(option.dirty);
    assert!(preset.set("layer_height", json!("-1")).is_err());
    assert_eq!(preset.value("layer_height"), Some(&json!("0.24")));
    preset.reset("layer_height");
    assert_eq!(preset.flattened(), base);
    preset.set("layer_height", base["layer_height"].clone())?;
    assert!(!preset.is_dirty("layer_height"));
    Ok(())
}

fn write_parent(root: &Path, kind: PresetType, base: &Map<String, Value>) -> Result<()> {
    let directory = root.join("system/BBL").join(kind.directory());
    fs::create_dir_all(&directory)?;
    fs::write(directory.join("parent.json"), serde_json::to_vec(base)?)?;
    Ok(())
}

#[test]
fn save_all_kinds_and_reload_through_slicer() -> Result<()> {
    let schema = Schema::load()?;
    let temp = TempDir::new();
    for (index, key, raw) in [
        (0, "nozzle_diameter", json!(["0.6"])),
        (1, "layer_height", json!("0.24")),
        (3, "filament_flow_ratio", json!(["0.97"])),
    ] {
        let (kind, parent) = A1_PRESETS[index];
        write_parent(&temp.0, kind, &fixture(index))?;
        let store = PresetStore::open(&temp.0)?;
        let mut preset = EditablePreset::from_system(&schema, &store, kind, parent)?;
        preset.set(key, raw.clone())?;
        let directory = temp.0.join("user/test").join(kind.directory());
        let name = format!("Edited {}", kind.directory());
        let info = UserPresetInfo {
            version: Some("2.8.4.0".into()),
            user_id: "test".into(),
            base_id: "base".into(),
            ..Default::default()
        };
        let saved = preset.save_user(&directory, &name, &info)?;
        let delta: Map<String, Value> = serde_json::from_slice(&fs::read(saved.json)?)?;
        assert_eq!(delta["inherits"], parent);
        assert_eq!(delta["from"], "User");
        assert_eq!(delta["name"], name);
        assert_eq!(delta["version"], "2.8.4.0");
        assert_eq!(delta[key], raw);
        assert_eq!(delta.len(), 8);
        let sidecar = fs::read_to_string(saved.info)?;
        assert_eq!(
            sidecar,
            "sync_info = \nuser_id = test\nsetting_id = \nbase_id = base\nupdated_time = 0\n"
        );
        let reloaded = PresetStore::open(&temp.0)?.flatten(kind.into(), &name)?;
        assert_eq!(reloaded[key], raw);
        assert_eq!(reloaded["name"], name);
        for (base_key, value) in preset.base() {
            if ![key, "name", "from", "version", kind.settings_id()].contains(&base_key.as_str()) {
                assert_eq!(&reloaded[base_key], value, "lost {base_key}");
            }
        }
        assert!(preset.save_user(&directory, &name, &info).is_err());
    }
    Ok(())
}

#[test]
fn save_rejects_paths_and_rolls_back_on_sidecar_collision() -> Result<()> {
    let schema = Schema::load()?;
    let temp = TempDir::new();
    let (kind, parent) = A1_PRESETS[1];
    let preset = EditablePreset::new(&schema, kind, parent, fixture(1));
    let info = UserPresetInfo {
        version: Some("2.8.4.0".into()),
        ..Default::default()
    };
    for name in ["../escape", "..", "", "a/b", "a\\b", "a\nb", parent] {
        assert!(preset.save_user(&temp.0, name, &info).is_err(), "{name:?}");
    }
    fs::write(temp.0.join("collision.info"), b"keep")?;
    assert!(preset.save_user(&temp.0, "collision", &info).is_err());
    assert!(!temp.0.join("collision.json").exists());
    assert_eq!(fs::read(temp.0.join("collision.info"))?, b"keep");
    Ok(())
}

#[test]
fn save_normalizes_vector_edits_and_derives_parent_id() -> Result<()> {
    let schema = Schema::load()?;
    let temp = TempDir::new();
    let (kind, parent) = A1_PRESETS[0];
    let base = fixture(0);
    let mut preset = EditablePreset::new(&schema, kind, parent, base.clone());
    preset.set("nozzle_diameter", json!("0.6"))?;
    preset.set("printer_extruder_id", json!("1"))?;
    assert!(
        preset
            .save_user(&temp.0, "Missing version", &UserPresetInfo::default())
            .is_err()
    );
    assert!(!temp.0.join("Missing version.json").exists());
    let info = UserPresetInfo {
        version: Some("2.8.4.0".into()),
        ..Default::default()
    };
    let saved = preset.save_user(&temp.0, "Scalar input", &info)?;
    let delta: Value = serde_json::from_slice(&fs::read(saved.json)?)?;
    assert_eq!(delta["nozzle_diameter"], json!(["0.6"]));
    assert_eq!(delta["printer_extruder_id"], json!(["1"]));
    assert_eq!(
        delta["printer_extruder_variant"],
        base["printer_extruder_variant"]
    );
    assert!(fs::read_to_string(saved.info)?.contains(&format!(
        "base_id = {}\n",
        base["setting_id"].as_str().unwrap()
    )));
    assert!(schema.validate("wall_loops", &json!("2147483648")).is_err());
    Ok(())
}

#[test]
fn ukrainian_metadata_and_english_fallback() -> Result<()> {
    use settings::Locale::{En, Uk};
    let schema = Schema::load()?;
    let height = &schema.options["layer_height"];
    assert_eq!(height.label(En), "Layer height");
    assert_eq!(height.label(Uk), "Висота шару");
    assert_eq!(height.sidetext(Uk), "мм");
    assert_eq!(height.category(Uk), "Якість");
    assert!(height.tooltip(Uk).starts_with("Висота нарізки"));
    assert_eq!(
        schema.options["machine_max_speed_x"].full_label(Uk),
        "Максимальна швидкість X"
    );
    assert_eq!(
        schema.options["ironing_type"].enum_label(0, Uk),
        Some("Без згладжування")
    );
    assert_eq!(schema.options["ironing_type"].enum_labels(Uk).count(), 4);
    assert_eq!(schema.options["ironing_type"].enum_label(99, Uk), None);
    let page = &schema.pages(PresetType::Process)[0];
    assert_eq!(page.title(Uk), "Якість");
    assert_eq!(page.groups[0].title(Uk), "Висота шару");
    // These newer upstream strings are untranslated in the pinned PO catalog.
    let missing = schema
        .options
        .values()
        .find(|o| !o.tooltip.is_empty() && o.uk.tooltip.is_empty())
        .unwrap();
    assert_eq!(missing.tooltip(Uk), missing.tooltip(En));
    let untranslated_enum = schema
        .options
        .values()
        .find(|o| {
            o.enum_labels.iter().enumerate().any(|(i, label)| {
                !label.is_empty() && o.uk.enum_labels.get(i).is_none_or(String::is_empty)
            })
        })
        .unwrap();
    for (i, english) in untranslated_enum.enum_labels.iter().enumerate() {
        if untranslated_enum
            .uk
            .enum_labels
            .get(i)
            .is_none_or(String::is_empty)
        {
            assert_eq!(untranslated_enum.enum_label(i, Uk), Some(english.as_str()));
        }
    }
    Ok(())
}

#[test]
fn scopes_and_metadata_classification() -> Result<()> {
    use settings::{Scope, is_metadata_key};
    let schema = Schema::load()?;
    assert_eq!(schema.options["layer_height"].scope, Scope::Object);
    assert_eq!(schema.options["enable_support"].scope, Scope::Object);
    assert_eq!(schema.options["wall_loops"].scope, Scope::Region);
    assert_eq!(schema.options["ironing_type"].scope, Scope::Region);
    assert_eq!(schema.options["nozzle_temperature"].scope, Scope::Global);
    assert!(schema.options["wall_loops"].can_override_object());
    assert!(schema.options["wall_loops"].can_override_part());
    assert!(!schema.options["layer_height"].can_override_part());
    assert!(!schema.options["nozzle_temperature"].can_override_object());
    for key in [
        "type",
        "name",
        "inherits",
        "setting_id",
        "filament_id",
        "compatible_printers",
        "description",
    ] {
        assert!(is_metadata_key(key));
    }
    assert!(!is_metadata_key("filament_scarf_height"));
    assert_eq!(
        schema.options["wall_infill_order"].origin,
        "bambu_compatibility"
    );
    assert!(!schema.options["wall_infill_order"].source_note.is_empty());
    Ok(())
}

#[test]
fn process_support_ironing_brim_and_speed_dependencies() -> Result<()> {
    use settings::{ToggleContext, option_state, option_state_with_context};
    let schema = Schema::load()?;
    let mut preset = EditablePreset::new(&schema, PresetType::Process, A1_PRESETS[1].1, fixture(1));
    preset.set("enable_support", json!("0"))?;
    preset.set("raft_layers", json!("0"))?;
    assert!(!preset.is_enabled("support_type"));
    preset.set("enable_support", json!("1"))?;
    assert!(preset.is_enabled("support_type"));
    preset.set("support_type", json!("tree(auto)"))?;
    assert!(preset.is_enabled("tree_support_branch_angle"));
    assert!(!preset.is_visible("bridge_no_support"));
    preset.set("support_type", json!("normal(manual)"))?;
    assert!(!preset.is_enabled("support_threshold_angle"));
    assert!(!preset.is_visible("tree_support_branch_angle"));
    preset.set("enable_support", json!("0"))?;
    preset.set("raft_layers", json!("2"))?;
    assert!(preset.is_enabled("support_type"));
    assert!(preset.is_enabled("enable_support_ironing"));
    preset.set("enable_support_ironing", json!("1"))?;
    assert!(preset.is_visible("support_ironing_speed"));
    preset.set("raft_layers", json!("0"))?;
    assert!(!preset.is_visible("support_ironing_speed"));
    preset.set("ironing_type", json!("no ironing"))?;
    assert!(!preset.is_enabled("ironing_speed"));
    preset.set("ironing_type", json!("top"))?;
    assert!(preset.is_enabled("ironing_speed"));
    for (brim, enabled) in [
        ("no_brim", false),
        ("auto_brim", false),
        ("brim_ears", false),
        ("outer_only", true),
    ] {
        preset.set("brim_type", json!(brim))?;
        assert_eq!(preset.is_enabled("brim_width"), enabled);
    }
    preset.set("enable_height_slowdown", json!(["0"]))?;
    assert!(!preset.is_enabled("slowdown_start_height"));
    preset.set("enable_height_slowdown", json!(["1"]))?;
    assert!(preset.is_enabled("slowdown_start_height"));
    preset.set("enable_circle_compensation", json!("1"))?;
    assert!(!preset.is_enabled("xy_hole_compensation"));
    assert!(preset.is_enabled("circle_compensation_manual_offset"));
    preset.set("default_acceleration", json!(["0", "1000"]))?;
    assert!(!preset.is_enabled("outer_wall_acceleration"));
    let object = ToggleContext {
        is_global: false,
        variant_index: 1,
        ..Default::default()
    };
    assert!(option_state_with_context("outer_wall_acceleration", &preset, &object).enabled);
    assert!(!option_state("print_flow_ratio", &preset).visible);
    assert!(option_state_with_context("print_flow_ratio", &preset, &object).visible);
    assert!(!preset.is_visible("default_jerk"));
    // Toggling never edits dependent settings or destroys their original values.
    assert_eq!(
        preset.value("ironing_speed"),
        fixture(1).get("ironing_speed")
    );
    Ok(())
}

#[test]
fn quality_strength_others_rules_and_partial_preset_defaults() -> Result<()> {
    let schema = Schema::load()?;
    let mut preset = EditablePreset::new(&schema, PresetType::Process, "partial", Map::new());
    assert!(preset.value("wall_loops").is_none());
    assert_eq!(preset.effective_value("wall_loops"), Some(&json!(2.0)));
    assert!(!preset.is_enabled("ironing_speed"));
    preset.set("wall_loops", json!("0"))?;
    assert!(!preset.is_enabled("outer_wall_speed"));
    preset.set("sparse_infill_density", json!("15%"))?;
    preset.set("sparse_infill_pattern", json!("2dlattice"))?;
    assert!(preset.is_visible("sparse_infill_lattice_angle_1"));
    preset.set("sparse_infill_density", json!("0%"))?;
    assert!(!preset.is_visible("sparse_infill_lattice_angle_1"));
    preset.set("sparse_infill_density", json!("15%"))?;
    preset.set("sparse_infill_pattern", json!("zigzag"))?;
    assert!(preset.is_visible("infill_rotate_step"));
    preset.set("sparse_infill_pattern", json!("lockedzag"))?;
    assert!(preset.is_visible("skin_infill_density"));
    preset.set("wall_generator", json!("classic"))?;
    assert!(!preset.is_visible("wall_transition_angle"));
    preset.set("wall_generator", json!("arachne"))?;
    assert!(preset.is_visible("wall_transition_angle"));
    assert!(!preset.is_enabled("detect_thin_wall"));
    preset.set("fuzzy_skin", json!("disabled_fuzzy"))?;
    assert!(!preset.is_visible("fuzzy_skin_thickness"));
    preset.set("fuzzy_skin", json!("none"))?; // Bambu allows painting in None mode.
    assert!(preset.is_visible("fuzzy_skin_thickness"));
    preset.set("enable_prime_tower", json!("0"))?;
    assert!(!preset.is_enabled("prime_tower_width"));
    assert!(!preset.is_enabled("flush_into_infill"));
    preset.set("enable_prime_tower", json!("1"))?;
    preset.set("prime_tower_rib_wall", json!("0"))?;
    assert!(preset.is_enabled("prime_tower_width"));
    preset.set("prime_tower_rib_wall", json!("1"))?;
    assert!(!preset.is_enabled("prime_tower_width"));
    preset.set("override_filament_scarf_seam_setting", json!("0"))?;
    assert!(!preset.is_visible("seam_slope_gap"));
    preset.set("override_filament_scarf_seam_setting", json!("1"))?;
    assert!(preset.is_visible("seam_slope_gap"));
    assert!(preset.is_enabled("unruled_future_option"));
    Ok(())
}

#[test]
fn orca_schema_loads_and_has_orca_specific_options() -> Result<()> {
    let bambu = Schema::load()?;
    let orca = Schema::load_orca()?;

    // Bambu catalog remains 702 options
    assert_eq!(bambu.options.len(), 702);
    // Orca catalog has 881 options, plus 3 legacy Bambu-project keys added at load time
    // (min 1 in OrcaSlicer ≤ 2.3; see Schema::load_orca).
    assert_eq!(
        orca.options.values().filter(|o| o.origin != "orca_legacy").count(),
        881
    );
    for (legacy, current) in slicer::ORCA_LEGACY_FILAMENT_KEYS {
        let def = &orca.options[*legacy];
        assert_eq!((def.origin.as_str(), def.min), ("orca_legacy", Some(1.)), "{legacy}");
        assert_eq!(orca.options[*current].min, Some(0.), "{current}");
        assert!(orca.validate(legacy, &serde_json::json!("0")).is_err());
        assert!(orca.validate(legacy, &serde_json::json!("1")).is_ok());
    }

    // Known Orca-specific options present in Orca but absent in Bambu
    for key in [
        "make_overhang_printable",
        "wipe_on_loops",
        "adaptive_pressure_advance",
        "belt_printer",
        "extra_perimeters_on_overhangs",
        "scarf_joint_flow_ratio",
        "activate_chamber_temp_control",
        "adaptive_bed_mesh_margin",
    ] {
        assert!(orca.options.contains_key(key), "missing in Orca: {key}");
        assert!(
            !bambu.options.contains_key(key),
            "should not be in Bambu: {key}"
        );
    }

    // Known Bambu-specific options present in Bambu but absent in Orca
    for key in [
        "adaptive_layer_height",
        "ams_filament_load_time_ams",
        "apply_scarf_seam_on_circles",
        "bed_heat_soak_area",
    ] {
        assert!(bambu.options.contains_key(key), "missing in Bambu: {key}");
        assert!(
            !orca.options.contains_key(key),
            "should not be in Orca: {key}"
        );
    }

    // Shared options present in both
    for key in [
        "layer_height",
        "pressure_advance",
        "wall_loops",
        "nozzle_temperature",
        "filament_type",
    ] {
        assert!(
            orca.options.contains_key(key),
            "missing shared in Orca: {key}"
        );
        assert!(
            bambu.options.contains_key(key),
            "missing shared in Bambu: {key}"
        );
    }

    // Specific Orca option properties
    let overhang = &orca.options["make_overhang_printable"];
    assert_eq!(overhang.kind, settings::OptionType::Bool);
    assert_eq!(overhang.default, json!(false));
    assert_eq!(overhang.scope, settings::Scope::Region);
    assert_eq!(overhang.mode, Mode::Advanced);

    let wipe = &orca.options["wipe_on_loops"];
    assert_eq!(wipe.kind, settings::OptionType::Bool);
    assert_eq!(wipe.default, json!(false));
    assert_eq!(wipe.scope, settings::Scope::Region);

    let margin = &orca.options["adaptive_bed_mesh_margin"];
    assert_eq!(margin.kind, settings::OptionType::Float);
    assert_eq!(margin.default, json!(0.0));
    assert_eq!(margin.sidetext, "mm");

    // Orca layout verification
    assert_eq!(
        orca.missing_layout_keys(),
        ["extruders_count", "imex_pre_slice_warnings"]
    );

    let process_pages = orca.pages(PresetType::Process);
    assert_eq!(
        process_pages
            .iter()
            .map(|p| p.title.as_str())
            .collect::<Vec<_>>(),
        [
            "Quality",
            "Strength",
            "Speed",
            "Support",
            "Multimaterial",
            "Others"
        ]
    );

    let machine_pages = orca.pages(PresetType::Machine);
    assert_eq!(
        machine_pages
            .iter()
            .map(|p| p.title.as_str())
            .collect::<Vec<_>>(),
        [
            "Basic information",
            "Machine G-code",
            "Multimaterial",
            "Extruder",
            "Motion ability",
            "Notes"
        ]
    );

    let filament_pages = orca.pages(PresetType::Filament);
    assert_eq!(
        filament_pages
            .iter()
            .map(|p| p.title.as_str())
            .collect::<Vec<_>>(),
        [
            "Filament",
            "Cooling",
            "Setting Overrides",
            "Advanced",
            "Multimaterial",
            "Dependencies",
            "Notes"
        ]
    );

    Ok(())
}

#[test]
fn setting_documentation_urls_and_descriptions() -> Result<()> {
    use settings::Locale;
    let bambu = Schema::load()?;
    let orca = Schema::load_orca()?;

    // Bambu verified per-key documentation URLs
    assert_eq!(
        bambu.documentation_url("z_hop"),
        Some(
            "https://wiki.bambulab.com/en/software/bambu-studio/parameter/retraction#z-hop-when-retracting"
        )
    );
    assert_eq!(
        bambu.help_url("wall_loops"),
        Some("https://wiki.bambulab.com/en/software/bambu-studio/wall-generator")
    );
    assert_eq!(
        bambu.documentation_url("top_surface_pattern"),
        Some(
            "https://wiki.bambulab.com/en/software/bambu-studio/fill-patterns#infill-of-the-top-surface-and-bottom-surface"
        )
    );
    assert_eq!(
        bambu.documentation_url("fuzzy_skin"),
        Some("https://wiki.bambulab.com/en/software/bambu-studio/parameter/fuzzy-skin")
    );

    // Orca verified per-key documentation URLs
    assert_eq!(
        orca.documentation_url("adaptive_bed_mesh_margin"),
        Some(
            "https://www.orcaslicer.com/wiki/printer_basic_information_adaptive_bed_mesh#mesh-margin"
        )
    );
    assert_eq!(
        orca.documentation_url("make_overhang_printable"),
        Some("https://www.orcaslicer.com/wiki/quality_settings_overhangs#make-overhang-printable")
    );
    assert_eq!(
        orca.documentation_url("scarf_joint_flow_ratio"),
        Some("https://www.orcaslicer.com/wiki/quality_settings_seam#scarf-joint-seam")
    );
    assert_eq!(
        orca.documentation_url("wall_loops"),
        Some("https://www.orcaslicer.com/wiki/strength_settings_walls#wall-loops")
    );

    // Distinct ownership between Bambu and Orca
    let b_wall = bambu.options["wall_loops"].documentation_url().unwrap();
    let o_wall = orca.options["wall_loops"].documentation_url().unwrap();
    assert!(b_wall.starts_with("https://wiki.bambulab.com/en/software/bambu-studio/"));
    assert!(o_wall.starts_with("https://www.orcaslicer.com/wiki/"));
    assert_ne!(b_wall, o_wall);

    // Fallback on unknown options or options without explicit upstream mappings
    assert_eq!(bambu.documentation_url("sparse_infill_density"), None);
    assert_eq!(bambu.documentation_url("completely_unknown_key"), None);
    assert_eq!(orca.documentation_url("completely_unknown_key"), None);

    // Documented options count
    let bambu_documented = bambu
        .options
        .values()
        .filter(|o| o.documentation_url.is_some())
        .count();
    let orca_documented = orca
        .options
        .values()
        .filter(|o| o.documentation_url.is_some() && o.origin != "orca_legacy")
        .count();
    assert_eq!(bambu_documented, 166);
    assert_eq!(orca_documented, 604);

    // Tooltip length and bounded description
    let support_style = &bambu.options["support_style"];
    assert!(support_style.tooltip.len() >= 700);
    assert_eq!(
        support_style.description(Locale::En),
        support_style.tooltip.as_str()
    );
    let bounded = support_style.bounded_description(100, Locale::En);
    assert!(bounded.chars().count() <= 100);
    assert!(support_style.tooltip.starts_with(bounded));

    let bridge = &orca.options["enable_extra_bridge_layer"];
    assert!(bridge.tooltip.len() >= 1500);
    let bounded_bridge = bridge.bounded_description(120, Locale::En);
    assert!(bounded_bridge.chars().count() <= 120);
    assert!(bridge.tooltip.starts_with(bounded_bridge));

    // Fallback from empty tooltip to full_label or label
    let empty_tt_opt = bambu
        .options
        .values()
        .find(|o| o.tooltip.is_empty())
        .unwrap();
    assert_eq!(
        empty_tt_opt.description(Locale::En),
        if !empty_tt_opt.full_label.is_empty() {
            empty_tt_opt.full_label.as_str()
        } else {
            empty_tt_opt.label.as_str()
        }
    );

    Ok(())
}
