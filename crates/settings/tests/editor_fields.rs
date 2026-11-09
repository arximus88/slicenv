//! Видимі сторінки редактора, перевизначення філаменту (`nil`) і багаторядковий G-code.

use std::fs;

use serde_json::{Map, Value, json};
use settings::{EditablePreset, PresetType, Schema, UserPresetInfo, is_nil, list_text, nil_like, parse_list};

fn base(pairs: &[(&str, Value)]) -> Map<String, Value> {
    let mut map: Map<String, Value> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect();
    map.insert("version".into(), json!("2.5.0.0"));
    map
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("slicenv-fields-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn titles(preset: &EditablePreset<'_>, advanced: bool) -> Vec<String> {
    let pages = preset.schema().pages(preset.kind());
    preset
        .visible_pages(advanced)
        .into_iter()
        .map(|i| pages[i].title.clone())
        .collect()
}

#[test]
fn notes_dependencies_and_overrides_pages_have_fields_even_when_profile_omits_keys() {
    let schema = Schema::load_orca().unwrap();
    // Як у справжньому A1-профілі: ні filament_notes, ні compatible_prints* немає.
    let preset = EditablePreset::new(
        &schema,
        PresetType::Filament,
        "Bambu PLA Basic @BBL A1",
        base(&[("filament_retraction_length", json!(["nil"]))]),
    );
    let shown = titles(&preset, true);
    for page in ["Setting Overrides", "Dependencies", "Notes"] {
        assert!(shown.iter().any(|t| t == page), "{page} missing from {shown:?}");
    }
    assert!(preset.is_shown("filament_notes", true));
    assert!(preset.is_shown("compatible_prints_condition", true));
    assert!(preset.is_shown("filament_retract_lift_above", true));
}

#[test]
fn pages_without_visible_fields_are_not_tabs() {
    let schema = Schema::load_orca().unwrap();
    let preset = EditablePreset::new(&schema, PresetType::Filament, "Parent", base(&[]));
    let pages = schema.pages(PresetType::Filament);
    for advanced in [false, true] {
        let visible = preset.visible_pages(advanced);
        for (i, page) in pages.iter().enumerate() {
            assert_eq!(
                visible.contains(&i),
                preset.page_has_fields(page, advanced),
                "{}",
                page.title
            );
        }
    }
    // Без «Розширених» сторінки лише з розширеними полями (примітки, залежності) ховаються.
    let simple = titles(&preset, false);
    assert!(!simple.iter().any(|t| t == "Notes" || t == "Dependencies"), "{simple:?}");
}

#[test]
fn nullable_override_round_trips_nil() {
    let schema = Schema::load_orca().unwrap();
    assert!(schema.is_filament_override("filament_retraction_length"));
    assert!(!schema.is_filament_override("filament_flow_ratio"));
    assert_eq!(
        schema.override_base_key("filament_retraction_length"),
        Some("retraction_length")
    );
    let mut preset = EditablePreset::new(
        &schema,
        PresetType::Filament,
        "Parent",
        base(&[
            ("filament_retraction_length", json!(["nil"])),
            ("filament_z_hop", json!(["0.4"])),
        ]),
    );
    assert!(!preset.is_overridden("filament_retraction_length"));
    assert!(preset.is_overridden("filament_z_hop"));

    preset.set("filament_retraction_length", json!(["1.2"])).unwrap();
    assert!(preset.is_overridden("filament_retraction_length"));
    // Знову вимкнули — те саме, що в системному профілі, тож не зміна.
    let off = nil_like(preset.value("filament_retraction_length"));
    assert_eq!(off, json!(["nil"]));
    preset.set("filament_retraction_length", off).unwrap();
    assert!(!preset.is_dirty("filament_retraction_length"));

    // Вимкнене перевизначення зберігається саме як "nil".
    preset
        .set("filament_z_hop", nil_like(preset.value("filament_z_hop")))
        .unwrap();
    let dir = temp_dir("nil");
    let info = UserPresetInfo::default();
    let saved = preset.save_user(&dir, "Nil override", &info).unwrap();
    let delta: Value = serde_json::from_slice(&fs::read(&saved.json).unwrap()).unwrap();
    assert_eq!(delta["filament_z_hop"], json!(["nil"]));
    assert!(is_nil(&delta["filament_z_hop"]));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn multiline_gcode_is_saved_byte_for_byte() {
    let schema = Schema::load_orca().unwrap();
    let gcode = ";start\nG28\r\nM106 P3 S150{elsif(bed_temperature[current_extruder] > 60)}\n\n  M104 S{x}  \n";
    let mut preset = EditablePreset::new(
        &schema,
        PresetType::Machine,
        "Parent",
        base(&[("machine_start_gcode", json!("G28\n"))]),
    );
    preset.set("machine_start_gcode", json!(gcode)).unwrap();
    let dir = temp_dir("gcode");
    let saved = preset
        .save_user(&dir, "Gcode", &UserPresetInfo::default())
        .unwrap();
    let raw = fs::read_to_string(&saved.json).unwrap();
    // Переноси — екрановані "\n" у JSON-рядку, а не справжні рядки файлу.
    assert!(raw.contains(r#"";start\nG28\r\nM106"#), "{raw}");
    let delta: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(delta["machine_start_gcode"].as_str(), Some(gcode));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn compatible_lists_edit_one_name_per_line() {
    let schema = Schema::load_orca().unwrap();
    assert!(schema.is_list_option("compatible_printers"));
    assert!(!schema.is_list_option("filament_type"));
    let names = json!(["Bambu Lab A1 0.4 nozzle", "Bambu Lab A1 0.6 nozzle"]);
    let text = list_text(Some(&names));
    assert_eq!(text, "Bambu Lab A1 0.4 nozzle\nBambu Lab A1 0.6 nozzle");
    assert_eq!(parse_list(&format!("  {text}\n\n")), names);
    assert_eq!(parse_list(""), json!([]));
    let mut preset = EditablePreset::new(&schema, PresetType::Filament, "Parent", base(&[]));
    preset.set("compatible_printers", parse_list(&text)).unwrap();
    assert_eq!(preset.value("compatible_printers"), Some(&names));
}
