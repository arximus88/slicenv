use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, Result};
use glam::Vec3;

use crate::arc::tessellate_arc;
use crate::model::{FilamentUse, GcodeStats, Layer, Marker, Role, Segment, Toolpath};
use crate::time::{Limits, Motion, MoveKind, MoveMeta, TimeMachine};

#[derive(Default)]
struct MachineState {
    pos: Vec3,
    e: f32,
    feedrate: f32,
    xyz_relative: bool,
    e_relative: bool,
    tool: u8,
    role: Role,
    width: f32,
    height: f32,
    z_height: f32,
    is_wiping: bool,
    fan: u8,
    temp: u16,
}

/// Детектор швів `GCodeProcessor::SeamsDetector` (Bambu Studio / OrcaSlicer): шов — середина
/// між початком петлі зовнішньої стінки й місцем, де вона закінчилась, якщо вони ближче 0.25 мм.
#[derive(Default)]
struct SeamsDetector {
    active: bool,
    first: Option<Vec3>,
    /// `spiral_mode` або `has_scarf_joint_seam`: похила петля — початок оновлюється з ростом Z.
    layer_based_on_tag: bool,
}

impl SeamsDetector {
    fn activate(&mut self, active: bool) {
        if self.active != active {
            self.active = active;
            if active {
                self.first = None;
            }
        }
    }

    /// Рух `kind` ролі `role` з початком у `start`; повертає позицію шва, якщо петля замкнулась.
    fn process(&mut self, kind: MoveKind, role: Role, start: Vec3) -> Option<Vec3> {
        let outer = kind == MoveKind::Extrude && role == Role::ExternalPerimeter;
        if !self.active {
            if outer {
                self.activate(true);
                self.first = Some(start);
            }
            return None;
        }
        match self.first {
            None if outer => self.first = Some(start),
            Some(first) if outer => {
                if self.layer_based_on_tag && start.z > first.z {
                    self.first = Some(start);
                }
            }
            Some(first)
                if kind != MoveKind::Extrude
                    || (role != Role::ExternalPerimeter && role != Role::OverhangPerimeter) =>
            {
                self.activate(false);
                if (start - first).length_squared() < 0.0625 {
                    return Some((start + first) * 0.5);
                }
            }
            _ => {}
        }
        None
    }
}

/// Тип руху як `move_type` у Bambu для G0/G1.
fn linear_kind(d: [f32; 4], wiping: bool) -> Option<MoveKind> {
    let xyz = d[0] != 0.0 || d[1] != 0.0 || d[2] != 0.0;
    Some(if wiping {
        MoveKind::Wipe
    } else if d[3] < 0.0 {
        if xyz {
            MoveKind::Travel
        } else {
            MoveKind::Retract
        }
    } else if d[3] > 0.0 {
        if d[0] == 0.0 && d[1] == 0.0 {
            if d[2] == 0.0 {
                MoveKind::Unretract
            } else {
                MoveKind::Travel
            }
        } else {
            MoveKind::Extrude
        }
    } else if xyz {
        MoveKind::Travel
    } else {
        return None;
    })
}

/// Геометрія дуги для оцінки часу: довжина в XY, радіус і дотичні на кінцях.
fn arc_motion(
    start: Vec3,
    end: Vec3,
    center: Vec3,
    ccw: bool,
    full_turns: Option<f32>,
) -> (f32, f32, Vec3, Vec3) {
    let (a, b) = ((start - center).truncate(), (end - center).truncate());
    let radius = a.length();
    let length = match full_turns {
        Some(p) => p * 2.0 * std::f32::consts::PI * radius,
        None => {
            let mut angle = b.y.atan2(b.x) - a.y.atan2(a.x);
            if !ccw {
                angle = -angle;
            }
            if (a - b).length() < 1e-6 {
                angle = 2.0 * std::f32::consts::PI;
            } else if angle < 0.0 {
                angle += 2.0 * std::f32::consts::PI;
            }
            angle * radius
        }
    };
    let tangent = |v: glam::Vec2| {
        let t = if ccw {
            glam::Vec2::new(-v.y, v.x)
        } else {
            glam::Vec2::new(v.y, -v.x)
        };
        t.normalize_or_zero().extend(0.0)
    };
    (length, radius, tangent(a), tangent(b))
}

/// Парсить G-code із рядка в памʼяті.
pub fn parse(src: &str) -> Result<Toolpath> {
    let mut stats = GcodeStats::default();
    let mut segments = Vec::with_capacity(src.len() / 40);
    let mut layers = Vec::with_capacity(300);

    // Конфіг потрібен до рухів (межі принтера), а Orca пише його в кінці файлу.
    if let Some(start) = src.find("; CONFIG_BLOCK_START") {
        let rest = &src[start..];
        let block = rest
            .find("; CONFIG_BLOCK_END")
            .map_or(rest, |end| &rest[..end]);
        for line in block.lines().skip(1) {
            parse_config_line(line.trim(), &mut stats);
        }
    }
    let flag = |key: &str| stats.config.get(key).is_some_and(|v| v.trim() == "1");
    let mut seams = SeamsDetector {
        layer_based_on_tag: flag("spiral_mode") || flag("has_scarf_joint_seam"),
        ..Default::default()
    };
    let mut time = TimeMachine::new(Limits::from_config(&stats.config));
    let mut markers: Vec<Marker> = Vec::new();
    let mut filament_by_role = vec![[0.0f64; Role::COUNT]; 256];
    let orca = src.contains("generated by OrcaSlicer");
    let flavor = stats
        .config
        .get("gcode_flavor")
        .cloned()
        .unwrap_or_default();
    let bbl = src.contains("BambuStudio")
        || stats
            .config
            .get("printer_model")
            .is_some_and(|v| v.starts_with("Bambu"));
    let config_time = |key: &str| {
        stats
            .config
            .get(key)
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(0.0)
    };
    let prepare_compensation = config_time("machine_prepare_compensation_time");
    let load_time = config_time("machine_load_filament_time");
    let unload_time = config_time("machine_unload_filament_time");
    let mut loaded_tool = None;
    let mut measure_g29 = false;
    let mut g1_line_id = 0u64;
    let mut prepare = false;
    let mut timelapse = false;
    let mut in_skippable = false;
    let mut layer_id = 0u32;

    let mut state = MachineState {
        width: 0.42,
        height: 0.2,
        feedrate: 3000.0,
        ..Default::default()
    };

    let mut in_header = false;
    let mut in_config = false;
    let has_layer_tags = src.lines().any(|line| {
        matches!(
            line.trim().strip_prefix(';').map(str::trim),
            Some("CHANGE_LAYER" | "LAYER_CHANGE")
        )
    });
    let mut in_layers = !has_layer_tags;
    let mut current_layer_start = 0usize;
    let mut current_layer_z = 0.0f32;
    let mut current_layer_h = 0.2f32;

    for line in src.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Блоки заголовка та конфігурації
        if line.starts_with("; HEADER_BLOCK_START") {
            in_header = true;
            continue;
        }
        if line.starts_with("; HEADER_BLOCK_END") {
            in_header = false;
            continue;
        }
        if line.starts_with("; CONFIG_BLOCK_START") {
            in_config = true;
            continue;
        }
        if line.starts_with("; CONFIG_BLOCK_END") {
            in_config = false;
            continue;
        }

        if in_header {
            parse_header_line(line, &mut stats);
            continue;
        }

        if in_config {
            continue;
        }

        // Коментарі з тегами
        if let Some(comment) = line.strip_prefix(';') {
            let comment = comment.trim();

            if comment.starts_with("FEATURE:") {
                let tag = comment.strip_prefix("FEATURE:").unwrap();
                state.role = Role::from_feature_tag(tag);
                if state.role == Role::ExternalPerimeter {
                    seams.activate(true);
                }
                prepare = state.role == Role::Custom && g1_line_id == 0;
                continue;
            }

            if comment == "SKIPPABLE_START" {
                in_skippable = true;
                continue;
            }
            if comment == "SKIPPABLE_END" {
                in_skippable = false;
                timelapse = false;
                continue;
            }
            if let Some(kind) = comment.strip_prefix("SKIPTYPE:") {
                timelapse = in_skippable && kind.trim() == "timelapse";
                continue;
            }

            if comment == "CHANGE_LAYER" || comment == "LAYER_CHANGE" {
                layer_id += 1;
                if in_layers {
                    let range = current_layer_start..segments.len();
                    layers.push(Layer {
                        z: current_layer_z,
                        height: current_layer_h,
                        segment_range: range,
                        time_estimate: None,
                    });
                }
                in_layers = true;
                current_layer_start = segments.len();
                current_layer_z = state.z_height;
                current_layer_h = state.height;
                continue;
            }

            if let Some(val_str) = comment.strip_prefix("Z_HEIGHT:") {
                if let Ok(val) = val_str.trim().parse::<f32>() {
                    state.z_height = val;
                    current_layer_z = val;
                }
                continue;
            }

            if let Some(val_str) = comment.strip_prefix("LAYER_HEIGHT:") {
                if let Ok(val) = val_str.trim().parse::<f32>() {
                    state.height = val;
                    current_layer_h = val;
                }
                continue;
            }

            if let Some(val_str) = comment.strip_prefix("LINE_WIDTH:") {
                if let Ok(val) = val_str.trim().parse::<f32>() {
                    state.width = val;
                }
                continue;
            }

            if comment == "WIPE_START" || comment == "_WIPE_START" {
                state.is_wiping = true;
                continue;
            }

            if comment == "WIPE_END" || comment == "_WIPE_END" {
                state.is_wiping = false;
                continue;
            }

            // Інші коментарі пропускаємо
            continue;
        }

        // Відокремлюємо команду від можливого коментаря в кінці рядка
        let cmd_part = match line.split_once(';') {
            Some((before, _)) => before.trim(),
            None => line,
        };

        if cmd_part.is_empty() {
            continue;
        }

        let cmd_word = cmd_part.split_ascii_whitespace().next().unwrap_or("");

        match cmd_word {
            "G0" | "G1" | "G28" | "G2" | "G3" => {
                let arc = matches!(cmd_word, "G2" | "G3");
                let params = motion_params(cmd_part);
                let (x, y, z, e) = if cmd_word == "G28" {
                    // Bambu: G28 — рух G1 у нуль заданих осей (або всіх).
                    let has = |c: char| has_word(cmd_part, c);
                    let all = !has('X') && !has('Y') && !has('Z');
                    let zero = |c: char| (all || has(c)).then_some(0.0);
                    (zero('X'), zero('Y'), zero('Z'), None)
                } else {
                    (params[0], params[1], params[2], params[3])
                };
                let f = params[4];
                g1_line_id += 1;

                if let Some(feed) = f {
                    state.feedrate = feed;
                }

                let axis = |value: Option<f32>, current: f32| {
                    if state.xyz_relative && cmd_word != "G28" {
                        current + value.unwrap_or(0.0)
                    } else {
                        value.unwrap_or(current)
                    }
                };
                let target_pos = Vec3::new(
                    axis(x, state.pos.x),
                    axis(y, state.pos.y),
                    axis(z, state.pos.z),
                );

                let delta_e = match e {
                    Some(e_val) => {
                        if state.e_relative || state.xyz_relative {
                            state.e += e_val;
                            e_val
                        } else {
                            let d = e_val - state.e;
                            state.e = e_val;
                            d
                        }
                    }
                    None => 0.0,
                };
                let start = state.pos;
                let d = target_pos - start;
                let delta = [d.x, d.y, d.z, delta_e];

                // Дуга: центр з I/J або R; P — лише повний оберт на місці.
                let arc_geometry = if arc {
                    let p = extract_param(cmd_part, 'P');
                    let same_xy = d.x.abs() < 1e-6 && d.y.abs() < 1e-6;
                    arc_center(start, target_pos, cmd_part, cmd_word == "G3")
                        .filter(|_| p.is_none_or(|p| same_xy && p as i32 == 1))
                        .map(|center| {
                            arc_motion(
                                start,
                                target_pos,
                                center,
                                cmd_word == "G3",
                                p.map(f32::trunc),
                            )
                        })
                } else {
                    None
                };
                let kind = if arc {
                    arc_geometry.and_then(|(length, ..)| {
                        (length != 0.0 || d.z != 0.0).then_some(if delta_e == 0.0 {
                            MoveKind::Travel
                        } else {
                            MoveKind::Extrude
                        })
                    })
                } else {
                    linear_kind(delta, state.is_wiping)
                };

                let first_segment = segments.len();
                if let Some(kind) = kind {
                    if let Some(seam) = seams.process(kind, state.role, start)
                        && in_layers
                    {
                        markers.push(Marker {
                            pos: seam,
                            kind: Role::Seam,
                            segment: segments.len().saturating_sub(1) as u32,
                            width: state.width,
                            height: state.height,
                        });
                    }
                    if kind == MoveKind::Extrude {
                        filament_by_role[state.tool as usize][state.role as usize] +=
                            f64::from(delta_e);
                    }
                    if in_layers && matches!(kind, MoveKind::Retract | MoveKind::Unretract) {
                        markers.push(Marker {
                            pos: start,
                            kind: if kind == MoveKind::Retract {
                                Role::Retract
                            } else {
                                Role::Unretract
                            },
                            segment: segments.len().saturating_sub(1) as u32,
                            width: state.width,
                            height: state.height,
                        });
                    }
                }

                let xyz_moved = d.length_squared() > 1e-8;
                if in_layers && (xyz_moved || arc) {
                    let is_extrusion = if arc {
                        delta_e > 1e-6
                    } else {
                        kind == Some(MoveKind::Extrude)
                    };
                    let role = if state.is_wiping {
                        Role::Wipe
                    } else if is_extrusion {
                        state.role
                    } else {
                        Role::Travel
                    };
                    let proto = Segment {
                        from: start,
                        to: target_pos,
                        role,
                        width: state.width,
                        height: state.height,
                        feedrate: state.feedrate,
                        tool: state.tool,
                        is_extrusion,
                        fan: state.fan,
                        temp: state.temp,
                        extruded: delta_e,
                        time: 0.0,
                    };
                    if arc {
                        let center_offset =
                            match (extract_param(cmd_part, 'I'), extract_param(cmd_part, 'J')) {
                                (None, None) => None,
                                (i, j) => Some((i.unwrap_or(0.0), j.unwrap_or(0.0))),
                            };
                        tessellate_arc(
                            start,
                            target_pos,
                            center_offset,
                            extract_param(cmd_part, 'R'),
                            cmd_word == "G2",
                            proto,
                            &mut segments,
                        );
                    } else {
                        segments.push(proto);
                    }
                }

                if let Some(kind) = kind {
                    // Bambu: переміщення поза Custom не належать ролі (Undefined).
                    let role = if kind != MoveKind::Travel || state.role == Role::Custom {
                        state.role
                    } else {
                        Role::None
                    };
                    let meta = MoveMeta {
                        kind,
                        role,
                        layer: layer_id,
                        prepare,
                        timelapse,
                        segments: (first_segment as u32, segments.len() as u32),
                    };
                    if orca && let Some(geometry) = arc_geometry {
                        let center =
                            arc_center(start, target_pos, cmd_part, cmd_word == "G3").unwrap();
                        push_orca_arc(
                            &mut time,
                            start,
                            target_pos,
                            center,
                            geometry,
                            delta_e,
                            state.feedrate / 60.0,
                            flavor == "marlin2",
                            meta,
                        )?;
                    } else {
                        time.push(
                            &Motion {
                                delta,
                                arc: arc_geometry,
                                feedrate: state.feedrate / 60.0,
                            },
                            MoveMeta {
                                kind,
                                role,
                                layer: layer_id,
                                prepare,
                                timelapse,
                                segments: (first_segment as u32, segments.len() as u32),
                            },
                        );
                    }
                }
                state.pos = target_pos;
            }

            // Обдув деталі: M106 без P або з P1; P2/P3 — допоміжний і камерний.
            "M106" => {
                let fan = extract_param(cmd_part, 'P').unwrap_or(1.0);
                if fan == 1.0 || fan == 0.0 {
                    let s = extract_param(cmd_part, 'S').unwrap_or(255.0);
                    state.fan = (s / 255.0 * 100.0).round().clamp(0.0, 100.0) as u8;
                }
            }
            "M107" => state.fan = 0,
            "M104" | "M109" => {
                if let Some(t) = extract_param(cmd_part, 'S') {
                    state.temp = t.clamp(0.0, 500.0) as u16;
                }
            }

            "M622" => {
                if extract_param(cmd_part, 'J').is_some_and(|j| j.round() == 1.0) {
                    measure_g29 = true;
                }
            }
            "M623" => measure_g29 = false,
            "G29" => {
                if !bbl || measure_g29 {
                    time.synchronize(prepare_compensation, None);
                }
            }
            "SET_VELOCITY_LIMIT" => {
                let named = |key: &str| {
                    let mut words = cmd_part
                        .split(|c: char| c.is_ascii_whitespace() || c == '=')
                        .filter(|w| !w.is_empty());
                    while let Some(word) = words.next() {
                        if word == key {
                            return words.next()?.parse::<f32>().ok();
                        }
                    }
                    None
                };
                if let Some(v) = named("VELOCITY") {
                    time.set_axes(203, [Some(v), Some(v), None, None]);
                }
                if let Some(v) = named("SQUARE_CORNER_VELOCITY") {
                    time.set_axes(205, [Some(v), Some(v), None, None]);
                }
                if let Some(v) = named("ACCEL") {
                    time.set_m204(None, Some(v), None, Some(v));
                }
            }
            "M204" => time.set_m204(
                extract_param(cmd_part, 'S'),
                extract_param(cmd_part, 'P'),
                extract_param(cmd_part, 'R'),
                extract_param(cmd_part, 'T'),
            ),
            "M201" | "M203" | "M205" => {
                let code = cmd_word[1..].parse().unwrap_or(0);
                let axes = ['X', 'Y', 'Z', 'E'].map(|c| {
                    extract_param(cmd_part, c).map(|v| {
                        if code == 203
                            && matches!(
                                flavor.as_str(),
                                "reprap" | "reprapfirmware" | "reprapsprinter"
                            )
                        {
                            v / 60.0
                        } else {
                            v
                        }
                    })
                });
                time.set_axes(code, axes);
                if code == 205 {
                    time.set_min_rates(extract_param(cmd_part, 'S'), extract_param(cmd_part, 'T'));
                }
            }
            "M221" => {
                if let Some(s) = extract_param(cmd_part, 'S')
                    && extract_param(cmd_part, 'T').is_none()
                {
                    time.set_extrude_factor(s * 0.01);
                }
            }
            // Паузи: час додається після спорожнення черги планувальника.
            "M400" | "G4" => {
                let s = extract_param(cmd_part, 'S');
                let p = extract_param(cmd_part, 'P');
                if s.is_some() || p.is_some() {
                    time.synchronize(s.unwrap_or(0.0) + p.unwrap_or(0.0) * 0.001, None);
                }
            }
            "M1" => time.synchronize(0.0, None),

            "G90" => state.xyz_relative = false,
            "G91" => state.xyz_relative = true,
            "M82" => state.e_relative = false,
            "M83" => state.e_relative = true,

            "G92" => {
                if let Some(e_val) = extract_param(cmd_part, 'E') {
                    state.e = e_val;
                } else {
                    time.synchronize(0.0, None);
                }
                if let Some(x_val) = extract_param(cmd_part, 'X') {
                    state.pos.x = x_val;
                }
                if let Some(y_val) = extract_param(cmd_part, 'Y') {
                    state.pos.y = y_val;
                }
                if let Some(z_val) = extract_param(cmd_part, 'Z') {
                    state.pos.z = z_val;
                }
            }

            word if word.starts_with('T') => {
                if let Ok(tool_id) = word[1..].parse::<u32>()
                    && tool_id < 255
                {
                    if loaded_tool != Some(tool_id) {
                        time.synchronize(
                            load_time
                                + if loaded_tool.is_some() {
                                    unload_time
                                } else {
                                    0.0
                                },
                            None,
                        );
                        loaded_tool = Some(tool_id);
                    }
                    state.tool = tool_id as u8;
                }
            }

            "M620" | "M621" => {
                if let Some(tool_id) = extract_tool_from_m620(cmd_part) {
                    state.tool = tool_id;
                }
            }

            _ => {}
        }
    }

    // Завершуємо останній шар, якщо він відкритий
    if in_layers && (has_layer_tags || !segments.is_empty()) {
        let range = current_layer_start..segments.len();
        layers.push(Layer {
            z: if has_layer_tags { current_layer_z } else { state.pos.z },
            height: current_layer_h,
            segment_range: range,
            time_estimate: None,
        });
    }

    let (mut estimate, segment_times) = time.finish(segments.len());
    anyhow::ensure!(estimate.total.is_finite(), "некоректний час у G-code");
    estimate.layers.resize(layers.len(), 0.0);
    for (segment, t) in segments.iter_mut().zip(segment_times) {
        segment.time = t;
    }
    for (i, layer) in layers.iter_mut().enumerate() {
        let secs = estimate.layers.get(i).copied().unwrap_or(0.0);
        layer.time_estimate = Some(
            Duration::try_from_secs_f32(secs).context("некоректний час у G-code")?,
        );
    }
    let mut filament: Vec<_> = filament_by_role
        .into_iter()
        .enumerate()
        .flat_map(|(tool, values)| {
            Role::ALL
                .into_iter()
                .zip(values)
                .filter(|(_, mm)| *mm != 0.0)
                .map(move |(role, mm)| FilamentUse {
                    role,
                    tool: tool as u8,
                    mm,
                })
        })
        .collect();
    filament.sort_by(|a, b| b.mm.total_cmp(&a.mm));

    Ok(Toolpath {
        layers,
        segments,
        stats,
        markers,
        estimate,
        filament,
    })
}

/// Чи є в команді слово з літерою `c` (наприклад `G28 X`).
fn has_word(line: &str, c: char) -> bool {
    line.split_ascii_whitespace()
        .skip(1)
        .any(|w| w.starts_with(c) || w.starts_with(c.to_ascii_lowercase()))
}

/// Читає та парсить G-code безпосередньо з файлу.
pub fn parse_file(path: impl AsRef<Path>) -> Result<Toolpath> {
    let path = path.as_ref();
    let content = fs::read_to_string(path)
        .with_context(|| format!("не вдалося прочитати G-code файл {}", path.display()))?;
    parse(&content)
}

fn parse_header_line(line: &str, stats: &mut GcodeStats) {
    let comment = line.trim_start_matches(';').trim();
    if comment.is_empty() {
        return;
    }

    if comment.starts_with("model printing time:") || comment.contains("total estimated time:") {
        for part in comment.split(';') {
            let part = part.trim();
            if let Some(s) = part.strip_prefix("model printing time:") {
                stats.model_printing_time = parse_duration(s);
            } else if let Some(s) = part.strip_prefix("total estimated time:") {
                stats.total_estimated_time = parse_duration(s);
            }
        }
    } else if let Some(s) = comment.strip_prefix("total layer number:") {
        stats.total_layer_number = s.trim().parse().ok();
    } else if let Some(s) = comment.strip_prefix("total filament length [mm] :") {
        stats.total_filament_length_mm = s.trim().parse().ok();
    } else if let Some(s) = comment.strip_prefix("total filament volume [cm^3] :") {
        stats.total_filament_volume_cm3 = s.trim().parse().ok();
    } else if let Some(s) = comment.strip_prefix("total filament weight [g] :") {
        stats.total_filament_weight_g = s.trim().parse().ok();
    } else if let Some(s) = comment.strip_prefix("max_z_height:") {
        stats.max_z_height = s.trim().parse().ok();
    } else if let Some(s) = comment.strip_prefix("filament_density:") {
        stats.filament_density = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
    } else if let Some(s) = comment.strip_prefix("filament_diameter:") {
        stats.filament_diameter = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
    }
}

fn parse_config_line(line: &str, stats: &mut GcodeStats) {
    let comment = line.trim_start_matches(';').trim();
    if let Some((k, v)) = comment.split_once('=') {
        stats
            .config
            .insert(k.trim().to_string(), v.trim().to_string());
    }
}

fn parse_duration(s: &str) -> Option<Duration> {
    let mut total_secs = 0u64;
    let mut num_buf = 0u64;
    let mut has_digits = false;
    for b in s.bytes() {
        match b {
            b'0'..=b'9' => {
                num_buf = num_buf.checked_mul(10)?.checked_add((b - b'0') as u64)?;
                has_digits = true;
            }
            b'd' | b'D' if has_digits => {
                total_secs = total_secs.checked_add(num_buf.checked_mul(86400)?)?;
                num_buf = 0;
                has_digits = false;
            }
            b'h' | b'H' if has_digits => {
                total_secs = total_secs.checked_add(num_buf.checked_mul(3600)?)?;
                num_buf = 0;
                has_digits = false;
            }
            b'm' | b'M' if has_digits => {
                total_secs = total_secs.checked_add(num_buf.checked_mul(60)?)?;
                num_buf = 0;
                has_digits = false;
            }
            b's' | b'S' if has_digits => {
                total_secs = total_secs.checked_add(num_buf)?;
                num_buf = 0;
                has_digits = false;
            }
            _ => {}
        }
    }
    if total_secs > 0 {
        Some(Duration::from_secs(total_secs))
    } else {
        None
    }
}

#[inline(always)]
fn extract_param(line: &str, letter: char) -> Option<f32> {
    let bytes = line.as_bytes();
    let l_byte = letter as u8;
    let l_upper = l_byte.to_ascii_uppercase();
    let l_lower = l_byte.to_ascii_lowercase();

    let mut i = 0;
    while i < bytes.len() {
        if (bytes[i] == l_upper || bytes[i] == l_lower)
            && (i == 0 || bytes[i - 1].is_ascii_whitespace())
        {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && !bytes[end].is_ascii_whitespace() && bytes[end] != b';' {
                end += 1;
            }
            if let Ok(val) = line[start..end].parse::<f32>() {
                return Some(val);
            }
        }
        i += 1;
    }
    None
}

#[inline(always)]
fn extract_tool_from_m620(line: &str) -> Option<u8> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if (bytes[i] == b'S' || bytes[i] == b's') && (i == 0 || bytes[i - 1].is_ascii_whitespace())
        {
            let mut j = i + 1;
            let mut num: u32 = 0;
            let mut has_digit = false;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                num = num.checked_mul(10)?.checked_add((bytes[j] - b'0') as u32)?;
                has_digit = true;
                j += 1;
            }
            if has_digit && num < 255 {
                return Some(num as u8);
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: &str = "; CONFIG_BLOCK_START\n; gcode_flavor = marlin\n; machine_max_acceleration_x = 1000,1000\n; machine_max_acceleration_y = 1000,1000\n; machine_max_acceleration_z = 1000,1000\n; machine_max_acceleration_e = 1000,1000\n; machine_max_acceleration_extruding = 1000,1000\n; machine_max_acceleration_retracting = 1000,1000\n; machine_max_speed_x = 500,500\n; machine_max_speed_y = 500,500\n; machine_max_speed_z = 30,30\n; machine_max_speed_e = 30,30\n; machine_max_jerk_x = 0,0\n; machine_max_jerk_y = 0,0\n; machine_max_jerk_z = 0,0\n; machine_max_jerk_e = 0,0\n; CONFIG_BLOCK_END\n";

    /// Квадратна петля зовнішньої стінки 10 × 10 зі стартом у (0,0); `close` — де вона закінчується.
    fn square(close: (f32, f32)) -> String {
        format!(
            "; CHANGE_LAYER\n; Z_HEIGHT: 0.2\nG1 X0 Y0 Z0.2 F6000\n; FEATURE: Outer wall\n\
             G1 X10 Y0 E1 F1200\nG1 X10 Y10 E2\nG1 X0 Y10 E3\nG1 X{} Y{} E4\n\
             G1 E3.2 F1800\n; FEATURE: Inner wall\nG1 X5 Y5 F6000\nG1 X6 Y5 E3.5 F1200\n",
            close.0, close.1
        )
    }

    #[test]
    fn seam_is_where_outer_wall_loop_closes() {
        let toolpath = parse(&square((0.1, 0.1))).unwrap();
        let seams: Vec<_> = toolpath
            .markers
            .iter()
            .filter(|m| m.kind == Role::Seam)
            .collect();
        assert_eq!(seams.len(), 1);
        // Середина між початком (0,0) і кінцем петлі (0.1,0.1), як у GCodeProcessor.
        assert!((seams[0].pos - Vec3::new(0.05, 0.05, 0.2)).length() < 1e-5);
        // Шов стоїть після останнього сегмента петлі (індекс 4: переміщення + 4 сторони).
        assert_eq!(seams[0].segment, 4);
        // Ретракт без руху — окрема позначка.
        assert_eq!(
            toolpath
                .markers
                .iter()
                .filter(|m| m.kind == Role::Retract)
                .count(),
            1
        );
        // Петля, що не замкнулась (далі 0.25 мм), шва не дає.
        let open = parse(&square((0.0, 0.3))).unwrap();
        assert!(open.markers.iter().all(|m| m.kind != Role::Seam));
    }

    #[test]
    fn time_follows_trapezoid_with_acceleration() {
        // 100 мм з місця й до зупинки, 100 мм/с, 1000 мм/с²: по 5 мм розгону й гальмування
        // (0.1 с кожне) і 90 мм крейсерського руху (0.9 с) — 1.1 с; jerk 0 — розворот із зупинкою.
        let src = format!("{LIMITS}M204 S1000\n; CHANGE_LAYER\nG1 X100 F6000\nG1 X0\n");
        let toolpath = parse(&src).unwrap();
        assert!(
            (toolpath.estimate.total - 2.2).abs() < 1e-3,
            "{}",
            toolpath.estimate.total
        );
        assert!((toolpath.estimate.moves(MoveKind::Travel) - 2.2).abs() < 1e-3);
        assert!((toolpath.segments[0].time - 1.1).abs() < 1e-3);
        // M400 P500 додає паузу після спорожнення черги.
        let src = format!(
            "{LIMITS}M204 S1000\n; CHANGE_LAYER\nG1 X100 F6000\nG1 X0\nM400 P500\nG1 X100\nG1 X0\n"
        );
        let toolpath = parse(&src).unwrap();
        assert!(
            (toolpath.estimate.total - 4.9).abs() < 1e-3,
            "{}",
            toolpath.estimate.total
        );
        // Половинне прискорення M204 S500: 0.2 + 0.8 + 0.2 = 1.2 с на рух.
        let src = format!("{LIMITS}M204 S500\n; CHANGE_LAYER\nG1 X100 F6000\nG1 X0\n");
        assert!((parse(&src).unwrap().estimate.total - 2.4).abs() < 1e-3);
    }

    #[test]
    fn time_separates_travel_and_retract_from_extrusion() {
        let src = format!(
            "{LIMITS}M83\nM204 S1000\n; CHANGE_LAYER\n; FEATURE: Outer wall\nG1 X100 E1 F6000\nG1 X0\nG1 E-1 F600\nG1 E1\n"
        );
        let t = parse(&src).unwrap();
        let e = &t.estimate;
        let outer = e.role(Role::ExternalPerimeter);
        let extrusion = e.moves(MoveKind::Extrude);
        let retract = e.moves(MoveKind::Retract) + e.moves(MoveKind::Unretract);
        assert!((outer - extrusion - retract).abs() < 1e-4);
        assert!((e.role(Role::None) - e.moves(MoveKind::Travel)).abs() < 1e-4);
        assert!((e.roles.iter().map(|(_, t)| t).sum::<f32>() - e.total).abs() < 1e-4);
        assert!(e.moves(MoveKind::Travel) > 1.0);
        // Один рух теж має час, навіть без наступного блоку планувальника.
        let t = parse(&format!("{LIMITS}G1 X100 F6000\n")).unwrap();
        assert!((t.estimate.total - 1.1).abs() < 1e-4);
    }

    #[test]
    fn missing_material_properties_do_not_invent_mass_or_cost() {
        let t = parse(&square((0.0, 0.0))).unwrap();
        let report = t.report(None);
        assert!(report.rows.iter().all(|r| r.filament_g.is_none()));
        assert!(report.rows.iter().any(|r| r.filament_m > 0.0));
        assert!(report.total_filament.is_none());
        assert!(report.cost.is_none());
    }

    #[test]
    fn seams_allow_overhang_and_restart_after_travel() {
        let src =
            square((0.0, 0.0)).replace("G1 X0 Y10 E3", "; FEATURE: Overhang wall\nG1 X0 Y10 E3");
        let t = parse(&(src.clone() + "G92 E0\n" + &src)).unwrap();
        assert_eq!(t.markers.iter().filter(|m| m.kind == Role::Seam).count(), 2);
        assert!(t.markers.windows(2).all(|m| m[0].segment <= m[1].segment));
        let inner = src.replace("FEATURE: Outer wall", "FEATURE: Inner wall");
        assert!(
            parse(&inner)
                .unwrap()
                .markers
                .iter()
                .all(|m| m.kind != Role::Seam)
        );
    }

    #[test]
    fn report_prefers_engine_times_and_header_totals() {
        let src = format!(
            "; HEADER_BLOCK_START\n; model printing time: 50s; total estimated time: 1m 0s\n\
             ; total filament length [mm] : 4\n; total filament weight [g] : 0.01\n; HEADER_BLOCK_END\n\
             ; CONFIG_BLOCK_START\n; filament_cost = 20\n; filament_density = 1.25\n; filament_diameter = 1.75\n\
             ; CONFIG_BLOCK_END\n{}",
            square((0.1, 0.1))
        );
        let toolpath = parse(&src).unwrap();
        let engine = crate::EngineReport::parse(
            r#"{"sliced_plates":[{"id":1,"total_predication":60.0,"feature_type_times":{"Outer wall":30.0,"Travel":6.0}}]}"#,
            Some(1),
        )
        .unwrap();
        let report = toolpath.report(Some(&engine));
        assert_eq!(report.rows_source, crate::Source::Engine);
        let outer = report
            .rows
            .iter()
            .find(|r| r.role == Role::ExternalPerimeter)
            .unwrap();
        assert_eq!(outer.time, 30.0);
        assert!((outer.share - 30.0 / 36.0).abs() < 1e-6);
        assert!((outer.filament_m - 0.004).abs() < 1e-9);
        assert_eq!(report.travel, Some((6.0, 6.0 / 36.0)));
        assert_eq!(report.prepare, Some(10.0));
        assert_eq!(report.seams, 1);
        assert_eq!(report.total_filament, Some((0.004, 0.01)));
        assert!((report.cost.unwrap() - 0.0002).abs() < 1e-12);
        // Без рушія — власна оцінка, рядки ті самі.
        let own = toolpath.report(None);
        assert_eq!(own.rows_source, crate::Source::Estimate);
        assert_eq!(own.rows.len(), report.rows.len());
    }
}

/// Центр I/J або R; знак R вибирає коротку чи довгу дугу.
fn arc_center(start: Vec3, end: Vec3, line: &str, ccw: bool) -> Option<Vec3> {
    if let Some(r) = extract_param(line, 'R') {
        let d = (end - start).truncate();
        let length = d.length();
        if length < 1e-6 || length > 2.0 * r.abs() {
            return None;
        }
        let h = (r * r - length * length * 0.25).max(0.0).sqrt();
        let sign = if ccw == (r > 0.0) { 1.0 } else { -1.0 };
        let center =
            (start + end).truncate() * 0.5 + glam::Vec2::new(-d.y, d.x) * (sign * h / length);
        Some(center.extend(start.z))
    } else {
        let i = extract_param(line, 'I');
        let j = extract_param(line, 'J');
        (i.is_some() || j.is_some())
            .then(|| start + Vec3::new(i.unwrap_or(0.0), j.unwrap_or(0.0), 0.0))
    }
}

/// Внутрішні G1 Orca: відхилення 0,0125 мм; для Marlin 2 — часовий крок 50 Гц.
fn push_orca_arc(
    time: &mut TimeMachine,
    start: Vec3,
    end: Vec3,
    center: Vec3,
    geometry: (f32, f32, Vec3, Vec3),
    extrusion: f32,
    feedrate: f32,
    marlin2: bool,
    meta: MoveMeta,
) -> Result<()> {
    let (length, radius, tangent, _) = geometry;
    anyhow::ensure!(
        length.is_finite() && radius.is_finite(),
        "надто складна або некоректна дуга G-code"
    );
    if radius <= 0.0 {
        return Ok(());
    }
    let angle = length / radius;
    let steps = if marlin2 {
        let mm = (8.0 * radius * 0.02)
            .sqrt()
            .min(feedrate / 50.0)
            .clamp(0.1, 2.0);
        (length / mm + 0.8).max(1.0) as usize
    } else if radius <= 0.0125 {
        if angle < std::f32::consts::PI
            || radius * (1.0 + (std::f32::consts::PI - angle * 0.5).cos()) < 0.0125
        {
            1
        } else {
            2
        }
    } else {
        (f64::from(angle) / (2.0 * (1.0 - 0.0125 / f64::from(radius)).acos()))
            .ceil()
            .max(1.0) as usize
    };
    // Bound CPU work and keep segment-index arithmetic within u32.
    anyhow::ensure!(steps <= 65_536, "надто складна або некоректна дуга G-code");
    let radial = start - center;
    let ccw = radial.x * tangent.y - radial.y * tangent.x > 0.0;
    let theta = angle * if ccw { 1.0 } else { -1.0 } / steps as f32;
    let mut prev = start;
    let theta = f64::from(theta);
    let mut radial64 = radial.truncate().as_dvec2();
    let mut z64 = f64::from(start.z);
    let (sin_step, cos_step) = if marlin2 {
        let (sin, cos) = (theta as f32).sin_cos();
        (f64::from(sin), f64::from(cos))
    } else {
        (
            theta - theta * theta * theta / 6.0,
            1.0 - theta * theta * 0.5,
        )
    };
    let mut correction = 25usize;
    for i in 1..=steps {
        let correct = if marlin2 {
            correction -= 1;
            correction == 0
        } else if correction == 0 {
            true
        } else {
            correction -= 1;
            false
        };
        if correct {
            let (sin, cos) = (theta * i as f64).sin_cos();
            radial64 = glam::DVec2::new(
                f64::from(radial.x) * cos - f64::from(radial.y) * sin,
                f64::from(radial.x) * sin + f64::from(radial.y) * cos,
            );
            correction = 25;
        } else {
            let y = radial64.x * sin_step + radial64.y * cos_step;
            radial64.x = radial64.x * cos_step - radial64.y * sin_step;
            radial64.y = f64::from(y as f32);
        }
        z64 += f64::from(end.z - start.z) / steps as f64;
        let next = if i == steps {
            end
        } else {
            Vec3::new(
                (f64::from(center.x) + radial64.x) as f32,
                (f64::from(center.y) + radial64.y) as f32,
                z64 as f32,
            )
        };
        let d = next - prev;
        // Видимі сегменти ділять загальний час, не дублюючи його для кожного внутрішнього G1.
        let count = meta.segments.1 - meta.segments.0;
        let from = meta.segments.0 + count * (i - 1) as u32 / steps as u32;
        let to = if count == 0 {
            from
        } else {
            (meta.segments.0 + count * i as u32 / steps as u32).max(from + 1)
        };
        time.push(
            &Motion {
                delta: [d.x, d.y, d.z, extrusion / steps as f32],
                arc: None,
                feedrate,
            },
            MoveMeta {
                segments: (from, to),
                ..meta
            },
        );
        prev = next;
    }
    Ok(())
}

/// Параметри найчастіших рухів читаються одним проходом замість п’яти пошуків рядка.
fn motion_params(line: &str) -> [Option<f32>; 5] {
    let mut result = [None; 5];
    for word in line.split_ascii_whitespace().skip(1) {
        let index = match word.as_bytes()[0].to_ascii_uppercase() {
            b'X' => 0,
            b'Y' => 1,
            b'Z' => 2,
            b'E' => 3,
            b'F' => 4,
            _ => continue,
        };
        if result[index].is_none() {
            result[index] = word[1..].parse().ok();
        }
    }
    result
}

#[cfg(test)]
mod regression_tests {
    use super::*;
    #[test]
    fn full_circle_p_and_radius_have_time_and_filament() {
        for cmd in [
            "G2 I10 E1",
            "G3 I10 E1 P1",
            "G2 X20 R10 E1",
            "G3 X20 R-15 E1",
        ] {
            let t = parse(&format!(
                "M83\n; CHANGE_LAYER\n; FEATURE: Outer wall\n{cmd} F600\nG1 E-1\n"
            ))
            .unwrap();
            assert!(t.estimate.role(Role::ExternalPerimeter) > 3.0, "{cmd}");
            assert_eq!(t.filament[0].mm, 1.0);
            assert!(
                (t.segments.iter().map(|s| s.time).sum::<f32>()
                    - t.estimate.moves(MoveKind::Extrude))
                .abs()
                    < 0.001
            );
            if !cmd.contains('R') {
                assert!(t.markers.iter().any(|m| m.kind == Role::Seam));
            }
        }
    }
    #[test]
    fn orca_arc_equals_internal_linear_moves() {
        // Повне коло R10 з допуском 0,0125 мм дає 63 внутрішні G1.
        let mut linear = String::from("M83\n; CHANGE_LAYER\n; FEATURE: Outer wall\n");
        for i in 1..=63 {
            use std::fmt::Write;
            let theta = std::f64::consts::TAU * i as f64 / 63.0;
            writeln!(
                linear,
                "G1 X{} Y{} E{} F600",
                10.0 - 10.0 * theta.cos(),
                -10.0 * theta.sin(),
                1.0 / 63.0
            )
            .unwrap();
        }
        let expected = parse(&linear).unwrap().estimate.total;
        let t = parse("; generated by OrcaSlicer\nM83\n; CHANGE_LAYER\n; FEATURE: Outer wall\nG3 I10 E1 F600\n").unwrap();
        assert!((t.estimate.total - expected).abs() < 0.01);
        assert!((t.segments.iter().map(|s| s.time).sum::<f32>() - expected).abs() < 0.01);
        let b = parse("M83\n; CHANGE_LAYER\n; FEATURE: Outer wall\nG3 I10 E1 F600\n").unwrap();
        assert!((t.estimate.total - b.estimate.total).abs() > 0.001);
    }
    #[test]
    fn g29_conditional_and_loading_delays() {
        let prefix = "; generated by BambuStudio\n; CONFIG_BLOCK_START\n; machine_prepare_compensation_time = 260\n; machine_load_filament_time = 25\n; CONFIG_BLOCK_END\n; FEATURE: Custom\nG1 X10 F600\n";
        let base = parse(prefix).unwrap().estimate.total;
        let t = parse(&format!(
            "{prefix}G29\nM622 J0\nG29\nM622 J1\nG29\nM623\nG29\nT0\nT0\nG1 X20\n"
        ))
        .unwrap();
        assert!((t.estimate.total - (base * 2.0 + 285.0)).abs() < 0.01);
        assert!((t.estimate.prepare - t.estimate.total).abs() < 0.001);
    }
    #[test]
    fn shares_sum_to_one_and_layers_match() {
        let t = parse("; HEADER_BLOCK_START\n; total estimated time: 1h\n; HEADER_BLOCK_END\nM83\n; CHANGE_LAYER\n; FEATURE: Outer wall\nG1 X10 E1 F600\nG1 X20\n; CHANGE_LAYER\n; CHANGE_LAYER\n").unwrap();
        let r = t.report(None);
        assert!(
            (r.rows.iter().map(|r| r.share).sum::<f32>() + r.travel.unwrap().1 - 1.0).abs() < 1e-6
        );
        assert_eq!(t.layers.len(), t.estimate.layers.len());
    }
    #[test]
    fn spiral_and_scarf_update_seam_z() {
        for key in ["spiral_mode", "has_scarf_joint_seam"] {
            let src = format!(
                "; CONFIG_BLOCK_START\n; {key} = 1\n; CONFIG_BLOCK_END\nM83\n; CHANGE_LAYER\n; FEATURE: Outer wall\nG1 X10 E1\nG1 X0 Z0.2 E1\nG1 X10 E1\nG1 X0 Z0.4 E1\nG1 X10 E1\nG1 X0 E1\nG1 E-1\n"
            );
            let t = parse(&src).unwrap();
            let seam = t.markers.iter().find(|m| m.kind == Role::Seam).unwrap();
            assert!((seam.pos.z - 0.4).abs() < 1e-6);
        }
    }
    #[test]
    fn klipper_command_matches_m_commands_and_reprap_units() {
        let tail =
            "M83\n; CHANGE_LAYER\n; FEATURE: Outer wall\nG1 X100 Y100 E1 F60000\nG1 X0 Y0 E1\n";
        let a = parse(&format!(
            "SET_VELOCITY_LIMIT ACCEL = 500 SQUARE_CORNER_VELOCITY=5 VELOCITY =50\n{tail}"
        ))
        .unwrap();
        let b = parse(&format!("M204 P500 T500\nM205 X5 Y5\nM203 X50 Y50\n{tail}")).unwrap();
        assert_eq!(a.estimate.total, b.estimate.total);
        let r = parse(&format!("; CONFIG_BLOCK_START\n; gcode_flavor = reprapfirmware\n; CONFIG_BLOCK_END\nM204 P500 T500\nM205 X5 Y5\nM203 X3000 Y3000\n{tail}")).unwrap();
        assert_eq!(r.estimate.total, b.estimate.total);
    }
}

#[cfg(test)]
mod input_safety_tests {
    use super::*;

    #[test]
    fn material_commands_use_zero_based_tools_and_ignore_sentinels() {
        let src = "M83\n; CHANGE_LAYER\nT1\nM621 S1A\nG1 X10 E1\nM620 S0A\nG1 X20 E2\nM621 S2A\nG1 X30 E3\nM621 S255\nM620 S65535\nM621 S65279\nG1 X40 E4\n";
        let t = parse(src).unwrap();
        assert_eq!(t.segments.iter().map(|s| s.tool).collect::<Vec<_>>(), [1, 0, 2, 2]);
        assert_eq!(t.filament.iter().find(|f| f.tool == 2).unwrap().mm, 7.0);
    }

    #[test]
    fn untagged_gcode_retains_moves_and_a_fallback_layer() {
        let t = parse("G1 Z0.2 F600\nG1 X10 E1\nG1 Y10 E2\n").unwrap();
        assert_eq!(t.segments.len(), 3);
        assert_eq!(t.layers.len(), 1);
        assert_eq!(t.layers[0].segment_range, 0..3);
        assert_eq!(t.layers[0].z, 0.2);
        assert!(t.layers[0].time_estimate.unwrap().as_secs_f32() > 0.0);
        assert!(parse("").unwrap().layers.is_empty());
        // Preparation moves must stay hidden when explicit layer tags exist.
        let tagged = parse("G1 X5\n; CHANGE_LAYER\nG1 X10\n").unwrap();
        assert_eq!(tagged.segments.len(), 1);
        assert_eq!(tagged.segments[0].from.x, 5.0);
    }

    #[test]
    fn orca_arc_subdivision_has_a_work_limit() {
        for flavor in ["marlin", "marlin2"] {
            for radius in ["1e9", "1e30"] {
                let src = format!("; generated by OrcaSlicer\n; CONFIG_BLOCK_START\n; gcode_flavor = {flavor}\n; CONFIG_BLOCK_END\n; CHANGE_LAYER\nG3 I{radius} E1 F600\n");
                assert!(parse(&src).is_err());
            }
        }
    }

    #[test]
    fn invalid_layer_durations_return_errors() {
        for dwell in ["inf", "1e30"] {
            assert!(parse(&format!("; CHANGE_LAYER\nG1 X10 F600\nG4 S{dwell}\n")).is_err());
        }
    }

    #[test]
    fn oversized_integer_fields_are_ignored() {
        let huge = "9".repeat(100);
        for value in [huge.as_str(), "18446744073709551615h", "18446744073709551615s 1s"] {
            assert!(parse_duration(value).is_none());
        }
        assert_eq!(parse_duration("1d 2h 3m 4s"), Some(Duration::from_secs(93784)));
        for command in ["M620", "M621"] {
            let t = parse(&format!("{command} S{huge}A\n; CHANGE_LAYER\nG1 X10 E1\n")).unwrap();
            assert_eq!(t.segments[0].tool, 0);
        }
    }
}
