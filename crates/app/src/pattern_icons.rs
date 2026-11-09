//! OrcaSlicer v2.4.2 icons, rendered as theme-tinted SVG masks.
use gpui::SharedString;

/// Only pattern settings may resolve enum values to pattern artwork.
pub fn option_icons(key: &str, values: &[String]) -> Vec<Option<SharedString>> {
    values
        .iter()
        .map(|value| {
            if !key.ends_with("_pattern") {
                return None;
            }
            let name = match value.as_str() {
                "2dlattice" => "param_lateral-lattice",
                "3dhoneycomb" => "param_3dhoneycomb",
                "adaptivecubic" => "param_adaptivecubic",
                "alignedrectilinear" => "param_alignedrectilinear",
                "archimedeanchords" => "param_archimedeanchords",
                "auto" => "param_rectilinear",
                "concentric" => "param_concentric",
                "crosshatch" => "param_crosshatch",
                "crosszag" => "param_crosszag",
                "cubic" => "param_cubic",
                "default" => "param_rectilinear",
                "globalmonotonicline" => "param_monotonicline",
                "grid" => "param_grid",
                "gyroid" => "param_gyroid",
                "hilbertcurve" => "param_hilbertcurve",
                "hollow" => "param_hollow",
                "honeycomb" => "param_honeycomb",
                "ironingarchimedeanspiral" => "param_archimedeanchords",
                "lateral-honeycomb" => "param_lateral-honeycomb",
                "lateral-lattice" => "param_lateral-lattice",
                "lightning" => "param_lightning",
                "line" => "param_line",
                "lockedzag" => "param_lockedzag",
                "monotonic" => "param_monotonic",
                "monotonicline" => "param_monotonicline",
                "octagramspiral" => "param_octagramspiral",
                "quartercubic" => "param_quartercubic",
                "rectilinear" => "param_rectilinear",
                "rectilinear-grid" => "param_rectilinear-grid",
                "rectilinear_interlaced" => "param_rectilinear_interlaced",
                "spiralinset" => "param_concentric",
                "supportcubic" => "param_supportcubic",
                "tpmsd" => "param_tpmsd",
                "tpmsfk" => "param_tpmsfk",
                "tri-hexagon" => "param_tri-hexagon",
                "triangles" => "param_triangles",
                "zig-zag" => "param_rectilinear",
                "zigzag" => "param_zigzag",
                _ => return None,
            };
            Some(format!("patterns/{name}.svg").into())
        })
        .collect()
}

/// Exact group mapping from Orca Tab.cpp, with schema fallbacks for app groups.
pub fn section_icon(title: &str) -> Option<SharedString> {
    let name = match title {
        "Acceleration" => "param_acceleration",
        "Acceleration limitation" => "param_acceleration",
        "Accessory" => "param_accessory",
        "Adaptive bed mesh" => "param_adaptive_mesh",
        "Advanced" => "param_advanced",
        "Auxiliary part cooling fan" => "param_cooling_aux_fan",
        "Basic information" => "param_information",
        "Bed adhension" => "param_adhension",
        "Bed temperature" => "param_bed_temp",
        "Before layer change G-code" => "param_gcode",
        "Belt printer" => "param_advanced",
        "Belt purge tower" => "param_tower",
        "Bridging" => "param_bridge",
        "Brim" => "param_adhension",
        "Change extrusion role G-code" => "param_gcode",
        "Change filament G-code" => "param_gcode",
        "Clumping Detection G-code" => "param_gcode",
        "Compatible printers" => "param_dependencies_printers",
        "Compatible process profiles" => "param_dependencies_presets",
        "Cooling Fan" => "param_cooling_fan",
        "Cooling for specific layer" => "param_cooling_specific_layer",
        "Exhaust fan" => "param_cooling_exhaust",
        "Extruder Clearance" => "param_extruder_clearance",
        "Filament end G-code" => "param_gcode",
        "Filament for Features" => "param_filament_for_features",
        "Filament for Supports" => "param_support_filament",
        "Filament scarf seam settings" => "param_volumetric_speed",
        "Filament start G-code" => "param_gcode",
        "File header G-code" => "param_gcode",
        "First layer speed" => "param_speed_first",
        "Flow ratio and Pressure Advance" => "param_flow_ratio_and_pressure_advance",
        "Flush options" => "param_flush",
        "Fuzzy skin" => "fuzzy_skin",
        "G-code output" => "param_gcode",
        "IDEX/IQEX Configuration" => "param_advanced",
        "Infill" => "param_infill",
        "Initial layer speed" => "param_speed_first",
        "Ironing" => "param_ironing",
        "Jerk limitation" => "param_jerk",
        "Jerk(XY)" => "param_jerk",
        "Junction Deviation" => "param_junction_deviation",
        "Layer change G-code" => "param_gcode",
        "Layer height" => "param_layer_height",
        "Layer height limits" => "param_layer_height",
        "Line width" => "param_line_width",
        "Machine end G-code" => "param_gcode",
        "Machine frame transforms" => "param_advanced",
        "Machine start G-code" => "param_gcode",
        "Notes" => "note",
        "Ooze prevention" => "param_ooze_prevention",
        "Other layers speed" => "param_speed",
        "Overhang speed" => "param_overhang_speed",
        "Overhangs" => "param_overhang",
        "Part cooling fan" => "param_cooling_part_fan",
        "Pause G-code" => "param_gcode",
        "Plugin Configuration" => "param_gcode",
        "Position" => "param_position",
        "Post-processing Scripts" => "param_gcode",
        "Post-processing scripts" => "param_gcode",
        "Precision" => "param_precision",
        "Prime tower" => "param_tower",
        "Print chamber temperature" => "param_chamber_temp",
        "Print temperature" => "param_extruder_temp",
        "Printable space" => "param_printable_space",
        "Printing by object G-code" => "param_gcode",
        "Raft" => "param_raft",
        "Resonance Compensation" => "param_resonance_avoidance",
        "Retraction" => "param_retraction",
        "Retraction when switching material" => "param_retraction_material_change",
        "Seam" => "param_seam",
        "Single extruder multi-material parameters" => "param_settings",
        "Single extruder multi-material setup" => "param_multi_material",
        "Skirt" => "param_skirt",
        "Slicing Pipeline Plugin" => "param_gcode",
        "Sparse infill" => "param_infill",
        "Special mode" => "param_special",
        "Speed" => "param_speed",
        "Speed limitation" => "param_speed",
        "Support" => "param_support",
        "Support filament" => "param_support_filament",
        "Support ironing" => "param_ironing",
        "Template Custom G-code" => "param_gcode",
        "Time lapse G-code" => "param_gcode",
        "Timelapse G-code" => "param_gcode",
        "Tool change parameters with multi extruder MM printers" => {
            "param_toolchange_multi_extruder"
        }
        "Tool change parameters with single extruder MM printers" => "param_toolchange",
        "Top/bottom shells" => "param_shell",
        "Travel speed" => "param_travel_speed",
        "Tree Support" => "param_support_tree",
        "Tree supports" => "param_support_tree",
        "Volumetric speed limitation" => "param_volumetric_speed",
        "Wall generator" => "param_wall_generator",
        "Walls" => "param_wall",
        "Walls and surfaces" => "param_wall_surface",
        "Wipe tower" => "param_tower",
        "Wipe tower parameters" => "param_tower",
        "Z contouring" => "param_advanced",
        "Z-Hop" => "param_extruder_lift_enforcement",
        _ => return None,
    };
    Some(format!("sections/{name}.svg").into())
}

pub fn asset(path: &str) -> Option<&'static [u8]> {
    match path {
        "patterns/param_3dhoneycomb.svg" => {
            Some(include_bytes!("../assets/patterns/param_3dhoneycomb.svg"))
        }
        "patterns/param_adaptivecubic.svg" => {
            Some(include_bytes!("../assets/patterns/param_adaptivecubic.svg"))
        }
        "patterns/param_alignedrectilinear.svg" => Some(include_bytes!(
            "../assets/patterns/param_alignedrectilinear.svg"
        )),
        "patterns/param_archimedeanchords.svg" => Some(include_bytes!(
            "../assets/patterns/param_archimedeanchords.svg"
        )),
        "patterns/param_concentric.svg" => {
            Some(include_bytes!("../assets/patterns/param_concentric.svg"))
        }
        "patterns/param_crosshatch.svg" => {
            Some(include_bytes!("../assets/patterns/param_crosshatch.svg"))
        }
        "patterns/param_crosszag.svg" => {
            Some(include_bytes!("../assets/patterns/param_crosszag.svg"))
        }
        "patterns/param_cubic.svg" => Some(include_bytes!("../assets/patterns/param_cubic.svg")),
        "patterns/param_grid.svg" => Some(include_bytes!("../assets/patterns/param_grid.svg")),
        "patterns/param_gyroid.svg" => Some(include_bytes!("../assets/patterns/param_gyroid.svg")),
        "patterns/param_hilbertcurve.svg" => {
            Some(include_bytes!("../assets/patterns/param_hilbertcurve.svg"))
        }
        "patterns/param_hollow.svg" => Some(include_bytes!("../assets/patterns/param_hollow.svg")),
        "patterns/param_honeycomb.svg" => {
            Some(include_bytes!("../assets/patterns/param_honeycomb.svg"))
        }
        "patterns/param_lateral-honeycomb.svg" => Some(include_bytes!(
            "../assets/patterns/param_lateral-honeycomb.svg"
        )),
        "patterns/param_lateral-lattice.svg" => Some(include_bytes!(
            "../assets/patterns/param_lateral-lattice.svg"
        )),
        "patterns/param_lightning.svg" => {
            Some(include_bytes!("../assets/patterns/param_lightning.svg"))
        }
        "patterns/param_line.svg" => Some(include_bytes!("../assets/patterns/param_line.svg")),
        "patterns/param_lockedzag.svg" => {
            Some(include_bytes!("../assets/patterns/param_lockedzag.svg"))
        }
        "patterns/param_monotonic.svg" => {
            Some(include_bytes!("../assets/patterns/param_monotonic.svg"))
        }
        "patterns/param_monotonicline.svg" => {
            Some(include_bytes!("../assets/patterns/param_monotonicline.svg"))
        }
        "patterns/param_octagramspiral.svg" => Some(include_bytes!(
            "../assets/patterns/param_octagramspiral.svg"
        )),
        "patterns/param_quartercubic.svg" => {
            Some(include_bytes!("../assets/patterns/param_quartercubic.svg"))
        }
        "patterns/param_rectilinear-grid.svg" => Some(include_bytes!(
            "../assets/patterns/param_rectilinear-grid.svg"
        )),
        "patterns/param_rectilinear.svg" => {
            Some(include_bytes!("../assets/patterns/param_rectilinear.svg"))
        }
        "patterns/param_rectilinear_interlaced.svg" => Some(include_bytes!(
            "../assets/patterns/param_rectilinear_interlaced.svg"
        )),
        "patterns/param_supportcubic.svg" => {
            Some(include_bytes!("../assets/patterns/param_supportcubic.svg"))
        }
        "patterns/param_tpmsd.svg" => Some(include_bytes!("../assets/patterns/param_tpmsd.svg")),
        "patterns/param_tpmsfk.svg" => Some(include_bytes!("../assets/patterns/param_tpmsfk.svg")),
        "patterns/param_tri-hexagon.svg" => {
            Some(include_bytes!("../assets/patterns/param_tri-hexagon.svg"))
        }
        "patterns/param_triangles.svg" => {
            Some(include_bytes!("../assets/patterns/param_triangles.svg"))
        }
        "patterns/param_zigzag.svg" => Some(include_bytes!("../assets/patterns/param_zigzag.svg")),
        "sections/fuzzy_skin.svg" => Some(include_bytes!("../assets/sections/fuzzy_skin.svg")),
        "sections/note.svg" => Some(include_bytes!("../assets/sections/note.svg")),
        "sections/param_acceleration.svg" => {
            Some(include_bytes!("../assets/sections/param_acceleration.svg"))
        }
        "sections/param_accessory.svg" => {
            Some(include_bytes!("../assets/sections/param_accessory.svg"))
        }
        "sections/param_adaptive_mesh.svg" => {
            Some(include_bytes!("../assets/sections/param_adaptive_mesh.svg"))
        }
        "sections/param_adhension.svg" => {
            Some(include_bytes!("../assets/sections/param_adhension.svg"))
        }
        "sections/param_advanced.svg" => {
            Some(include_bytes!("../assets/sections/param_advanced.svg"))
        }
        "sections/param_bed_temp.svg" => {
            Some(include_bytes!("../assets/sections/param_bed_temp.svg"))
        }
        "sections/param_bridge.svg" => Some(include_bytes!("../assets/sections/param_bridge.svg")),
        "sections/param_chamber_temp.svg" => {
            Some(include_bytes!("../assets/sections/param_chamber_temp.svg"))
        }
        "sections/param_cooling_aux_fan.svg" => Some(include_bytes!(
            "../assets/sections/param_cooling_aux_fan.svg"
        )),
        "sections/param_cooling_exhaust.svg" => Some(include_bytes!(
            "../assets/sections/param_cooling_exhaust.svg"
        )),
        "sections/param_cooling_fan.svg" => {
            Some(include_bytes!("../assets/sections/param_cooling_fan.svg"))
        }
        "sections/param_cooling_part_fan.svg" => Some(include_bytes!(
            "../assets/sections/param_cooling_part_fan.svg"
        )),
        "sections/param_cooling_specific_layer.svg" => Some(include_bytes!(
            "../assets/sections/param_cooling_specific_layer.svg"
        )),
        "sections/param_dependencies_presets.svg" => Some(include_bytes!(
            "../assets/sections/param_dependencies_presets.svg"
        )),
        "sections/param_dependencies_printers.svg" => Some(include_bytes!(
            "../assets/sections/param_dependencies_printers.svg"
        )),
        "sections/param_extruder_clearance.svg" => Some(include_bytes!(
            "../assets/sections/param_extruder_clearance.svg"
        )),
        "sections/param_extruder_lift_enforcement.svg" => Some(include_bytes!(
            "../assets/sections/param_extruder_lift_enforcement.svg"
        )),
        "sections/param_extruder_temp.svg" => {
            Some(include_bytes!("../assets/sections/param_extruder_temp.svg"))
        }
        "sections/param_filament_for_features.svg" => Some(include_bytes!(
            "../assets/sections/param_filament_for_features.svg"
        )),
        "sections/param_flow_ratio_and_pressure_advance.svg" => Some(include_bytes!(
            "../assets/sections/param_flow_ratio_and_pressure_advance.svg"
        )),
        "sections/param_flush.svg" => Some(include_bytes!("../assets/sections/param_flush.svg")),
        "sections/param_gcode.svg" => Some(include_bytes!("../assets/sections/param_gcode.svg")),
        "sections/param_infill.svg" => Some(include_bytes!("../assets/sections/param_infill.svg")),
        "sections/param_information.svg" => {
            Some(include_bytes!("../assets/sections/param_information.svg"))
        }
        "sections/param_ironing.svg" => {
            Some(include_bytes!("../assets/sections/param_ironing.svg"))
        }
        "sections/param_jerk.svg" => Some(include_bytes!("../assets/sections/param_jerk.svg")),
        "sections/param_junction_deviation.svg" => Some(include_bytes!(
            "../assets/sections/param_junction_deviation.svg"
        )),
        "sections/param_layer_height.svg" => {
            Some(include_bytes!("../assets/sections/param_layer_height.svg"))
        }
        "sections/param_line_width.svg" => {
            Some(include_bytes!("../assets/sections/param_line_width.svg"))
        }
        "sections/param_multi_material.svg" => Some(include_bytes!(
            "../assets/sections/param_multi_material.svg"
        )),
        "sections/param_ooze_prevention.svg" => Some(include_bytes!(
            "../assets/sections/param_ooze_prevention.svg"
        )),
        "sections/param_overhang.svg" => {
            Some(include_bytes!("../assets/sections/param_overhang.svg"))
        }
        "sections/param_overhang_speed.svg" => Some(include_bytes!(
            "../assets/sections/param_overhang_speed.svg"
        )),
        "sections/param_position.svg" => {
            Some(include_bytes!("../assets/sections/param_position.svg"))
        }
        "sections/param_precision.svg" => {
            Some(include_bytes!("../assets/sections/param_precision.svg"))
        }
        "sections/param_printable_space.svg" => Some(include_bytes!(
            "../assets/sections/param_printable_space.svg"
        )),
        "sections/param_raft.svg" => Some(include_bytes!("../assets/sections/param_raft.svg")),
        "sections/param_resonance_avoidance.svg" => Some(include_bytes!(
            "../assets/sections/param_resonance_avoidance.svg"
        )),
        "sections/param_retraction.svg" => {
            Some(include_bytes!("../assets/sections/param_retraction.svg"))
        }
        "sections/param_retraction_material_change.svg" => Some(include_bytes!(
            "../assets/sections/param_retraction_material_change.svg"
        )),
        "sections/param_seam.svg" => Some(include_bytes!("../assets/sections/param_seam.svg")),
        "sections/param_settings.svg" => {
            Some(include_bytes!("../assets/sections/param_settings.svg"))
        }
        "sections/param_shell.svg" => Some(include_bytes!("../assets/sections/param_shell.svg")),
        "sections/param_skirt.svg" => Some(include_bytes!("../assets/sections/param_skirt.svg")),
        "sections/param_special.svg" => {
            Some(include_bytes!("../assets/sections/param_special.svg"))
        }
        "sections/param_speed.svg" => Some(include_bytes!("../assets/sections/param_speed.svg")),
        "sections/param_speed_first.svg" => {
            Some(include_bytes!("../assets/sections/param_speed_first.svg"))
        }
        "sections/param_support.svg" => {
            Some(include_bytes!("../assets/sections/param_support.svg"))
        }
        "sections/param_support_filament.svg" => Some(include_bytes!(
            "../assets/sections/param_support_filament.svg"
        )),
        "sections/param_support_tree.svg" => {
            Some(include_bytes!("../assets/sections/param_support_tree.svg"))
        }
        "sections/param_toolchange.svg" => {
            Some(include_bytes!("../assets/sections/param_toolchange.svg"))
        }
        "sections/param_toolchange_multi_extruder.svg" => Some(include_bytes!(
            "../assets/sections/param_toolchange_multi_extruder.svg"
        )),
        "sections/param_tower.svg" => Some(include_bytes!("../assets/sections/param_tower.svg")),
        "sections/param_travel_speed.svg" => {
            Some(include_bytes!("../assets/sections/param_travel_speed.svg"))
        }
        "sections/param_volumetric_speed.svg" => Some(include_bytes!(
            "../assets/sections/param_volumetric_speed.svg"
        )),
        "sections/param_wall.svg" => Some(include_bytes!("../assets/sections/param_wall.svg")),
        "sections/param_wall_generator.svg" => Some(include_bytes!(
            "../assets/sections/param_wall_generator.svg"
        )),
        "sections/param_wall_surface.svg" => {
            Some(include_bytes!("../assets/sections/param_wall_surface.svg"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_schema_pattern_and_section_icon_is_embedded() {
        for source in [
            include_str!("../../settings/schema/options.json"),
            include_str!("../../settings/schema-orca/options.json"),
        ] {
            let options: serde_json::Value = serde_json::from_str(source).unwrap();
            for (key, def) in options.as_object().unwrap() {
                if !key.ends_with("_pattern") || key == "center_of_surface_pattern" {
                    continue;
                }
                let values: Vec<String> = def["enum_values"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap().to_owned())
                    .collect();
                for (value, path) in values.iter().zip(option_icons(key, &values)) {
                    assert!(
                        path.as_ref().and_then(|p| asset(p)).is_some(),
                        "{key}={value}"
                    );
                }
            }
        }
        for source in [
            include_str!("../../settings/schema/layout.json"),
            include_str!("../../settings/schema-orca/layout.json"),
        ] {
            let layout: serde_json::Value = serde_json::from_str(source).unwrap();
            for pages in layout.as_object().unwrap().values() {
                for page in pages.as_array().unwrap() {
                    for group in page["groups"].as_array().unwrap() {
                        if let Some(path) = section_icon(group["title"].as_str().unwrap()) {
                            assert!(asset(&path).is_some());
                        }
                    }
                }
            }
        }
        assert!(option_icons("seam_position", &["concentric".into()])[0].is_none());
        assert!(option_icons("future_pattern", &["unknown".into()])[0].is_none());
    }
}
