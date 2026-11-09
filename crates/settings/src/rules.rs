//! Hand-ported from BambuStudio 79df7ae, ConfigManipulation.cpp:
//! ConfigManipulation::toggle_print_fff_options, called by TabPrint::toggle_options.
//! Field enablement and line visibility are independent; later calls replace an
//! earlier call of the same kind, matching the GUI. No config values are mutated.

use crate::EditablePreset;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OptionState {
    pub enabled: bool,
    pub visible: bool,
}

impl Default for OptionState {
    fn default() -> Self {
        Self {
            enabled: true,
            visible: true,
        }
    }
}

/// Printer and editing context unavailable in a process preset. Defaults describe
/// the global Bambu A1 editor. Pass an explicit context for Objects/parts or other
/// printers. `variant_index` selects vector-valued speed/acceleration settings.
#[derive(Clone, Copy, Debug)]
pub struct ToggleContext<'a> {
    pub is_global: bool,
    pub is_bambu_printer: bool,
    pub gcode_flavor: &'a str,
    pub supports_tower_interface: bool,
    pub supports_wrapping_detection: bool,
    pub variant_index: usize,
}

impl Default for ToggleContext<'_> {
    fn default() -> Self {
        Self {
            is_global: true,
            is_bambu_printer: true,
            gcode_flavor: "marlin",
            supports_tower_interface: false,
            supports_wrapping_detection: false,
            variant_index: 0,
        }
    }
}

/// Whether a control can be edited (both visible and enabled).
pub fn is_enabled(key: &str, preset: &EditablePreset<'_>) -> bool {
    let state = option_state(key, preset);
    state.enabled && state.visible
}

pub fn is_visible(key: &str, preset: &EditablePreset<'_>) -> bool {
    option_state(key, preset).visible
}

pub fn option_state(key: &str, preset: &EditablePreset<'_>) -> OptionState {

    option_state_with_context(key, preset, &ToggleContext::default())
}

pub fn option_state_with_context(
    key: &str,
    preset: &EditablePreset<'_>,
    context: &ToggleContext<'_>,
) -> OptionState {
    let _profile = crate::profile::Span::new("settings::rules");
    let value = |key: &str| {
        preset.effective_value(key).map(|v| {
            v.as_array()
                .and_then(|a| a.get(context.variant_index))
                .unwrap_or(v)
        })
    };
    let string = |key: &str| value(key).and_then(serde_json::Value::as_str).unwrap_or("");
    let number = |key: &str| {
        value(key)
            .and_then(|v| {
                v.as_f64()
                    .or_else(|| v.as_str()?.trim_end_matches('%').parse().ok())
            })
            .unwrap_or(0.0)
    };
    let flag = |key: &str| {
        value(key).is_some_and(|v| {
            v.as_bool()
                .unwrap_or_else(|| v.as_str() == Some("1") || v.as_i64() == Some(1))
        })
    };
    let mut state = OptionState::default();
    let mut field = |keys: &[&str], enabled| {
        if keys.contains(&key) {
            state.enabled = enabled;
        }
    };
    let mut line = |keys: &[&str], visible| {
        if keys.contains(&key) {
            state.visible = visible;
        }
    };

    let walls = number("wall_loops") > 0.0;
    field(
        &[
            "ensure_vertical_shell_thickness",
            "detect_thin_wall",
            "detect_overhang_wall",
            "seam_position",
            "seam_placement_away_from_overhangs",
            "seam_gap",
            "wipe_speed",
            "wall_sequence",
            "outer_wall_line_width",
            "inner_wall_speed",
            "outer_wall_speed",
            "small_perimeter_speed",
            "small_perimeter_threshold",
        ],
        walls,
    );
    line(
        &["seam_placement_away_from_overhangs"],
        matches!(string("seam_position"), "aligned" | "back"),
    );

    let infill = number("sparse_infill_density") > 0.0;
    line(
        &[
            "sparse_infill_pattern",
            "sparse_infill_anchor_max",
            "infill_combination",
            "minimum_sparse_infill_area",
            "sparse_infill_filament",
            "infill_shift_step",
            "infill_rotate_step",
            "symmetric_infill_y_axis",
            "sparse_infill_lattice_angle_1",
            "sparse_infill_lattice_angle_2",
        ],
        infill,
    );
    let pattern = string("sparse_infill_pattern");
    line(
        &["fill_multiline"],
        infill
            && matches!(
                pattern,
                "cubic"
                    | "grid"
                    | "zig-zag"
                    | "tri-hexagon"
                    | "alignedrectilinear"
                    | "gyroid"
                    | "honeycomb"
                    | "lightning"
                    | "3dhoneycomb"
                    | "adaptivecubic"
                    | "supportcubic"
            ),
    );
    line(
        &["sparse_infill_anchor"],
        infill && number("sparse_infill_anchor_max") > 0.0,
    );
    let zig = infill && pattern == "zigzag";
    let cross = infill && pattern == "crosszag";
    let locked = infill && pattern == "lockedzag";
    let conformal = infill
        && matches!(
            pattern,
            "zigzag" | "crosszag" | "lockedzag" | "zig-zag" | "alignedrectilinear"
        );
    line(&["conformal_infill"], conformal);
    line(
        &[
            "conformal_stagger",
            "conformal_link_keep_layers",
            "conformal_link_flip_layers",
            "conformal_pole",
            "conformal_ray_count",
            "conformal_hub_radius",
        ],
        conformal && flag("conformal_infill"),
    );
    line(
        &[
            "infill_instead_top_bottom_surfaces",
            "skeleton_infill_density",
            "skin_infill_density",
            "infill_lock_depth",
            "skin_infill_depth",
            "skin_infill_line_width",
            "skeleton_infill_line_width",
            "locked_skin_infill_pattern",
            "locked_skeleton_infill_pattern",
        ],
        locked,
    );
    line(&["infill_rotate_step"], zig);
    line(&["infill_shift_step"], cross || locked);
    line(&["symmetric_infill_y_axis"], zig || cross || locked);
    line(
        &[
            "sparse_infill_lattice_angle_1",
            "sparse_infill_lattice_angle_2",
        ],
        infill && pattern == "2dlattice",
    );
    line(
        &[
            "periodic_modifier_skip_layers",
            "periodic_modifier_apply_layers",
        ],
        flag("periodic_modifier"),
    );

    let spiral = flag("spiral_mode");
    line(&["spiral_mode_smooth"], spiral);
    line(
        &["spiral_mode_max_xy_smoothing"],
        flag("spiral_mode_smooth"),
    );
    field(&["z_direction_outwall_speed_continuous"], !spiral);
    let top = number("top_shell_layers") > 0.0;
    let bottom = number("bottom_shell_layers") > 0.0;
    let solid = top || bottom;
    field(
        &[
            "top_surface_pattern",
            "bottom_surface_pattern",
            "top_surface_density",
            "bottom_surface_density",
            "internal_solid_infill_pattern",
            "sub_top_surface_pattern",
            "solid_infill_filament",
        ],
        solid,
    );
    field(
        &[
            "infill_direction",
            "sparse_infill_line_width",
            "bridge_angle",
            "sparse_infill_speed",
            "bridge_speed",
        ],
        infill || solid,
    );
    field(&["top_shell_thickness"], !spiral && top);
    field(&["bottom_shell_thickness"], !spiral && bottom);
    field(&["gap_infill_speed"], walls);
    field(
        &["top_surface_line_width", "top_surface_speed"],
        top || (spiral && bottom),
    );
    field(
        &[
            "initial_layer_acceleration",
            "outer_wall_acceleration",
            "top_surface_acceleration",
            "inner_wall_acceleration",
            "sparse_infill_acceleration",
        ],
        number("default_acceleration") > 0.0,
    );
    line(
        &[
            "default_jerk",
            "outer_wall_jerk",
            "inner_wall_jerk",
            "infill_jerk",
            "top_surface_jerk",
            "initial_layer_jerk",
            "travel_jerk",
        ],
        !context.is_bambu_printer,
    );
    if !context.is_bambu_printer {
        field(
            &[
                "outer_wall_jerk",
                "inner_wall_jerk",
                "infill_jerk",
                "top_surface_jerk",
                "initial_layer_jerk",
                "travel_jerk",
            ],
            number("default_jerk") > 0.0,
        );
    }

    let skirt = number("skirt_loops") > 0.0;
    field(
        &["skirt_height"],
        skirt && string("draft_shield") != "enabled",
    );
    field(&["skirt_distance", "draft_shield"], skirt);
    let brim = string("brim_type") != "no_brim";
    field(&["brim_object_gap"], brim);
    field(
        &["brim_width"],
        brim && !matches!(string("brim_type"), "auto_brim" | "brim_ears"),
    );
    field(&["wall_filament"], walls || brim);
    let raft = number("raft_layers") > 0.0;
    let support = flag("enable_support") || raft;
    let support_type = string("support_type");
    let interface = number("support_interface_top_layers") > 0.0
        || number("support_interface_bottom_layers") > 0.0;
    let soluble = support && number("support_top_z_distance") == 0.0;
    field(
        &[
            "support_style",
            "support_base_pattern",
            "support_base_pattern_spacing",
            "support_expansion",
            "support_angle",
            "support_interface_pattern",
            "support_interface_top_layers",
            "bridge_no_support",
            "max_bridge_length",
            "support_top_z_distance",
            "support_bottom_z_distance",
            "support_type",
            "support_on_build_plate_only",
            "support_remove_small_overhang",
            "support_interface_not_for_body",
            "support_object_xy_distance",
            "support_object_first_layer_gap",
        ],
        support,
    );
    field(
        &["support_threshold_angle"],
        support && support_type.ends_with("(auto)"),
    );
    let tree = flag("enable_support") && support_type.starts_with("tree");
    field(
        &[
            "tree_support_branch_angle",
            "tree_support_branch_distance",
            "tree_support_branch_diameter",
            "tree_support_branch_diameter_angle",
        ],
        tree,
    );
    line(
        &[
            "tree_support_branch_angle",
            "tree_support_branch_distance",
            "tree_support_branch_diameter",
            "tree_support_branch_diameter_angle",
            "max_bridge_length",
        ],
        tree,
    );
    line(
        &["support_critical_regions_only"],
        tree && support_type.ends_with("(auto)"),
    );
    line(
        &["detect_floating_vertical_shell", "vertical_shell_speed"],
        flag("detect_narrow_internal_solid_infill"),
    );
    line(
        &[
            "bridge_no_support",
            "support_bottom_interface_spacing",
            "support_interface_bottom_layers",
        ],
        !tree,
    );
    field(
        &[
            "support_interface_spacing",
            "support_interface_filament",
            "support_interface_loop_pattern",
        ],
        support && interface,
    );
    line(
        &["support_speed"],
        support || (skirt && (number("skirt_height") > 1.0 || string("draft_shield") != "enabled")),
    );
    line(&["support_interface_speed"], support && interface);
    field(&["inner_wall_line_width"], walls || skirt || brim);
    field(&["support_filament"], support || skirt);
    line(&["raft_contact_distance"], raft && !soluble);
    line(
        &[
            "ironing_pattern",
            "ironing_speed",
            "ironing_flow",
            "ironing_spacing",
            "ironing_direction",
            "ironing_inset",
        ],
        string("ironing_type") != "no ironing",
    );
    let iron_support = number("raft_layers") > 1.0
        || (flag("enable_support") && number("support_interface_top_layers") > 0.0);
    field(&["enable_support_ironing"], iron_support);
    line(
        &[
            "support_ironing_pattern",
            "support_ironing_speed",
            "support_ironing_flow",
            "support_ironing_spacing",
            "support_ironing_direction",
            "support_ironing_inset",
        ],
        iron_support && flag("enable_support_ironing"),
    );
    field(&["standby_temperature_delta"], flag("ooze_prevention"));

    let tower = flag("enable_prime_tower");
    line(
        &[
            "prime_tower_width",
            "prime_tower_brim_width",
            "prime_tower_skip_points",
            "prime_tower_rib_wall",
            "prime_tower_infill_gap",
            "prime_tower_enable_framework",
            "prime_tower_max_speed",
        ],
        tower,
    );
    line(
        &["enable_tower_interface_features"],
        tower && context.supports_tower_interface,
    );
    let rib = tower && flag("prime_tower_rib_wall");
    line(
        &[
            "prime_tower_extra_rib_length",
            "prime_tower_rib_width",
            "prime_tower_fillet_wall",
        ],
        rib,
    );
    field(&["prime_tower_width"], !rib);
    field(
        &[
            "flush_into_infill",
            "flush_into_support",
            "flush_into_objects",
        ],
        tower,
    );
    line(
        &[
            "max_travel_detour_distance",
            "avoid_crossing_wall_includes_support",
        ],
        flag("reduce_crossing_wall"),
    );
    line(
        &[
            "overhang_1_4_speed",
            "overhang_2_4_speed",
            "overhang_3_4_speed",
            "overhang_4_4_speed",
        ],
        flag("enable_overhang_speed"),
    );
    line(
        &[
            "slowdown_start_height",
            "slowdown_start_speed",
            "slowdown_start_acc",
            "slowdown_end_height",
            "slowdown_end_speed",
            "slowdown_end_acc",
        ],
        flag("enable_height_slowdown"),
    );
    line(
        &[
            "flush_into_objects",
            "print_flow_ratio",
            "wall_filament",
            "solid_infill_filament",
            "sparse_infill_filament",
        ],
        !context.is_global,
    );
    line(
        &["support_interface_not_for_body"],
        number("support_interface_filament") != 0.0 && number("support_filament") == 0.0,
    );

    let fuzzy = string("fuzzy_skin") != "disabled_fuzzy";
    line(
        &[
            "fuzzy_skin_thickness",
            "fuzzy_skin_point_distance",
            "fuzzy_skin_first_layer",
            "fuzzy_skin_noise_type",
            "fuzzy_skin_mode",
        ],
        fuzzy,
    );
    let noise = string("fuzzy_skin_noise_type");
    line(&["fuzzy_skin_scale"], fuzzy && noise != "classic");
    line(
        &["fuzzy_skin_octaves"],
        fuzzy && !matches!(noise, "classic" | "voronoi"),
    );
    line(
        &["fuzzy_skin_persistence"],
        fuzzy && matches!(noise, "perlin" | "billow"),
    );
    let arachne = string("wall_generator") == "arachne";
    line(
        &[
            "wall_transition_length",
            "wall_transition_filter_deviation",
            "wall_transition_angle",
            "min_feature_size",
            "min_bead_width",
            "wall_distribution_count",
        ],
        arachne,
    );
    field(&["detect_thin_wall"], !arachne);
    line(
        &[
            "accel_to_decel_enable",
            "accel_to_decel_factor",
            "exclude_object",
        ],
        context.gcode_flavor == "klipper",
    );
    if context.gcode_flavor == "klipper" {
        field(&["accel_to_decel_factor"], flag("accel_to_decel_enable"));
    }
    let beam = flag("interlocking_beam");
    line(&["mmu_segmented_region_interlocking_depth"], !beam);
    line(
        &[
            "interlocking_beam_width",
            "interlocking_orientation",
            "interlocking_beam_layer_count",
            "interlocking_depth",
            "interlocking_boundary_avoidance",
        ],
        beam,
    );
    field(
        &["xy_hole_compensation", "xy_contour_compensation"],
        !flag("enable_circle_compensation"),
    );
    line(
        &["circle_compensation_manual_offset"],
        flag("enable_circle_compensation"),
    );
    line(
        &[
            "seam_slope_type",
            "seam_slope_start_height",
            "seam_slope_gap",
            "seam_slope_min_length",
        ],
        flag("override_filament_scarf_seam_setting"),
    );
    line(
        &["enable_wrapping_detection"],
        context.supports_wrapping_detection,
    );
    // Compatibility-only setting absent from the reference's toggle routine.
    line(
        &["layer_time_smoothing_threshold"],
        flag("layer_time_smoothing"),
    );
    state
}
