//! Оцінка часу друку — порт `GCodeProcessor::TimeMachine` з Bambu Studio (нормальний режим):
//! трапецієподібні профілі швидкості з прискореннями M204, межі осей M201/M203/M205,
//! швидкість з’єднань за jerk, прямий і зворотний прохід планувальника, черга 64/256 блоків
//! і синхронізації (M400 S/P, G4) з додатковим часом. Повторює і дивацтва оригіналу
//! (наприклад, jerk рахується по різниці одиничних напрямків), бо мета — ті самі числа.

use std::collections::HashMap;

use glam::Vec3;

use crate::model::Role;

/// Тип руху, як `EMoveType` у Bambu (лише ті, що мають час).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MoveKind {
    Retract,
    Unretract,
    Travel,
    Wipe,
    Extrude,
}

/// Межі принтера (значення нормального режиму, мм/с і мм/с²) з конфігу G-code.
#[derive(Clone, Debug)]
pub(crate) struct Limits {
    max_speed: [f32; 4],
    max_acc: [f32; 4],
    max_jerk: [f32; 4],
    min_extruding_rate: f32,
    min_travel_rate: f32,
    max_acceleration: f32,
    max_retract_acceleration: f32,
    max_travel_acceleration: f32,
}

const DEFAULT_ACCELERATION: f32 = 1500.0;
const DEFAULT_RETRACT_ACCELERATION: f32 = 1500.0;
const DEFAULT_TRAVEL_ACCELERATION: f32 = 1250.0;
const QUEUE_SIZE: usize = 64;
const REFRESH_THRESHOLD: usize = QUEUE_SIZE * 4;

impl Limits {
    /// Перше значення списку `a,b` (нормальний режим); за відсутності — дефолти Bambu.
    pub(crate) fn from_config(config: &HashMap<String, String>) -> Self {
        let use_config = matches!(
            config.get("gcode_flavor").map(|s| s.trim()),
            Some("marlin" | "marlin2" | "klipper")
        );
        let get = |key: &str| -> f32 {
            config
                .get(key)
                .and_then(|v| v.split([',', ';']).next())
                .and_then(|v| v.trim().parse().ok())
                .filter(|_| use_config)
                .unwrap_or(match key {
                    "machine_max_speed_x" | "machine_max_speed_y" => 500.0,
                    "machine_max_speed_z" => 12.0,
                    "machine_max_speed_e" => 120.0,
                    "machine_max_acceleration_x" | "machine_max_acceleration_y" => 1000.0,
                    "machine_max_acceleration_z" => 500.0,
                    "machine_max_acceleration_e" => 5000.0,
                    "machine_max_jerk_x" | "machine_max_jerk_y" => 10.0,
                    "machine_max_jerk_z" => 0.2,
                    "machine_max_jerk_e" => 2.5,
                    _ => 0.0,
                })
        };
        let axes = |prefix: &str| ["x", "y", "z", "e"].map(|a| get(&format!("{prefix}_{a}")));
        let extruding = get("machine_max_acceleration_extruding");
        // У «marlin» (legacy) окремого прискорення переміщень немає — береться extruding.
        let legacy = config
            .get("gcode_flavor")
            .is_none_or(|f| f.trim() == "marlin");
        Self {
            max_speed: axes("machine_max_speed"),
            max_acc: axes("machine_max_acceleration"),
            max_jerk: axes("machine_max_jerk"),
            min_extruding_rate: get("machine_min_extruding_rate"),
            min_travel_rate: get("machine_min_travel_rate"),
            max_acceleration: extruding,
            max_retract_acceleration: get("machine_max_acceleration_retracting"),
            max_travel_acceleration: if legacy {
                extruding
            } else {
                get("machine_max_acceleration_travel")
            },
        }
    }
}

#[derive(Clone, Copy, Default)]
struct State {
    feedrate: f32,
    safe_feedrate: f32,
    axis_feedrate: [f32; 4],
    abs_axis_feedrate: [f32; 4],
    enter_direction: Vec3,
    exit_direction: Vec3,
}

/// Що відомо про рух для обліку часу.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MoveMeta {
    pub kind: MoveKind,
    /// Роль для `roles_times`: переміщення поза Custom — `Role::None`.
    pub role: Role,
    pub layer: u32,
    pub prepare: bool,
    pub timelapse: bool,
    /// Сегменти траєкторії, що належать цьому руху (порожньо для E-only).
    pub segments: (u32, u32),
}

#[derive(Clone, Copy)]
struct Block {
    meta: MoveMeta,
    distance: f32,
    acceleration: f32,
    entry: f32,
    cruise: f32,
    exit: f32,
    max_entry_speed: f32,
    safe_feedrate: f32,
    nominal_length: bool,
    recalculate: bool,
    accelerate_until: f32,
    decelerate_after: f32,
    cruise_feedrate: f32,
}

fn sqr(v: f32) -> f32 {
    v * v
}

fn estimated_acceleration_distance(initial: f32, target: f32, acc: f32) -> f32 {
    if acc == 0.0 {
        0.0
    } else {
        (sqr(target) - sqr(initial)) / (2.0 * acc)
    }
}

fn intersection_distance(initial: f32, fin: f32, acc: f32, distance: f32) -> f32 {
    if acc == 0.0 {
        0.0
    } else {
        (2.0 * acc * distance - sqr(initial) + sqr(fin)) / (4.0 * acc)
    }
}

fn speed_from_distance(initial: f32, distance: f32, acc: f32) -> f32 {
    (sqr(initial) + 2.0 * acc * distance).max(0.0).sqrt()
}

fn max_allowable_speed(acc: f32, target: f32, distance: f32) -> f32 {
    (sqr(target) - 2.0 * acc * distance).max(0.0).sqrt()
}

impl Block {
    fn calculate_trapezoid(&mut self) {
        self.cruise_feedrate = self.cruise;
        let mut accelerate =
            estimated_acceleration_distance(self.entry, self.cruise, self.acceleration).max(0.0);
        let decelerate =
            estimated_acceleration_distance(self.cruise, self.exit, -self.acceleration).max(0.0);
        let mut cruise = self.distance - accelerate - decelerate;
        if cruise < 0.0 {
            accelerate =
                intersection_distance(self.entry, self.exit, self.acceleration, self.distance)
                    .clamp(0.0, self.distance);
            cruise = 0.0;
            self.cruise_feedrate = speed_from_distance(self.entry, accelerate, self.acceleration);
        }
        self.accelerate_until = accelerate;
        self.decelerate_after = accelerate + cruise;
    }

    fn time(&self) -> f32 {
        // Кінцева швидкість розгону вже відома з трапеції — повторний sqrt зайвий.
        let accel = if self.acceleration > 0.0 {
            (self.cruise_feedrate - self.entry).max(0.0) / self.acceleration
        } else {
            0.0
        };
        let cruise_distance = self.decelerate_after - self.accelerate_until;
        let cruise = if self.cruise_feedrate != 0.0 {
            cruise_distance / self.cruise_feedrate
        } else {
            0.0
        };
        // Для гальмування лишаємо формулу Bambu: вихідна швидкість буває недосяжною.
        let decel = if self.acceleration != 0.0 {
            (speed_from_distance(
                self.cruise_feedrate,
                self.distance - self.decelerate_after,
                -self.acceleration,
            ) - self.cruise_feedrate)
                / -self.acceleration
        } else {
            0.0
        };
        accel + cruise + decel
    }
}

/// Накопичені результати оцінки.
#[derive(Clone, Debug, Default)]
pub struct TimeTotals {
    /// Повний час, с (`time_mode.time`).
    pub total: f32,
    /// Підготовка (стартовий G-code), с.
    pub prepare: f32,
    /// Рухи у блоках `SKIPTYPE: timelapse`, с (`skippable_part_time[stTimelapse]`).
    pub timelapse: f32,
    /// Час за ролями (`roles_times`), включно з `Role::None` для переміщень.
    pub roles: Vec<(Role, f32)>,
    /// Час за типами рухів (`moves_times`); переміщення стартового G-code не входять.
    pub moves: Vec<(MoveKind, f32)>,
    /// Час шарів, с; перший шар — без підготовки.
    pub layers: Vec<f32>,
}

impl TimeTotals {
    pub fn role(&self, role: Role) -> f32 {
        self.roles
            .iter()
            .find(|(r, _)| *r == role)
            .map_or(0.0, |(_, t)| *t)
    }

    pub fn moves(&self, kind: MoveKind) -> f32 {
        self.moves
            .iter()
            .find(|(k, _)| *k == kind)
            .map_or(0.0, |(_, t)| *t)
    }
}

pub(crate) struct TimeMachine {
    limits: Limits,
    acceleration: f32,
    retract_acceleration: f32,
    travel_acceleration: f32,
    extrude_factor: f32,
    curr: State,
    prev: State,
    blocks: Vec<Block>,
    additional: Vec<(Option<Role>, f32)>,
    roles: [f32; Role::COUNT],
    moves: [f32; 5],
    layers: Vec<f32>,
    total: f32,
    prepare: f32,
    timelapse: f32,
    /// Час кожного сегмента траєкторії, с.
    pub segment_times: Vec<f32>,
}

/// Рух у термінах планувальника.
pub(crate) struct Motion {
    pub delta: [f32; 4],
    /// Для дуг: довжина дуги в площині XY і дотичні на початку/в кінці.
    pub arc: Option<(f32, f32, Vec3, Vec3)>,
    /// Подача з G-code, мм/с.
    pub feedrate: f32,
}

impl TimeMachine {
    pub(crate) fn new(limits: Limits) -> Self {
        let pick = |max: f32, default: f32| if max > 0.0 { max } else { default };
        Self {
            acceleration: pick(limits.max_acceleration, DEFAULT_ACCELERATION),
            retract_acceleration: pick(
                limits.max_retract_acceleration,
                DEFAULT_RETRACT_ACCELERATION,
            ),
            travel_acceleration: pick(limits.max_travel_acceleration, DEFAULT_TRAVEL_ACCELERATION),
            limits,
            extrude_factor: 1.0,
            curr: State::default(),
            prev: State::default(),
            blocks: Vec::new(),
            additional: Vec::new(),
            roles: [0.0; Role::COUNT],
            moves: [0.0; 5],
            layers: Vec::new(),
            total: 0.0,
            prepare: 0.0,
            timelapse: 0.0,
            segment_times: Vec::new(),
        }
    }

    fn clamp(value: f32, max: f32) -> f32 {
        if max == 0.0 { value } else { value.min(max) }
    }

    /// M204: `S` — legacy (друк і переміщення), інакше `P`/`R`/`T`.
    pub(crate) fn set_m204(
        &mut self,
        s: Option<f32>,
        p: Option<f32>,
        r: Option<f32>,
        t: Option<f32>,
    ) {
        if let Some(s) = s {
            self.acceleration = Self::clamp(s, self.limits.max_acceleration);
            self.travel_acceleration = Self::clamp(s, self.limits.max_travel_acceleration);
            if let Some(t) = t {
                self.retract_acceleration = Self::clamp(t, self.limits.max_retract_acceleration);
            }
        } else {
            if let Some(p) = p {
                self.acceleration = Self::clamp(p, self.limits.max_acceleration);
            }
            if let Some(r) = r {
                self.retract_acceleration = Self::clamp(r, self.limits.max_retract_acceleration);
            }
            if let Some(t) = t {
                self.travel_acceleration = Self::clamp(t, self.limits.max_travel_acceleration);
            }
        }
    }

    /// M201 (прискорення осей), M203 (швидкості осей), M205 (jerk; X задає і Y).
    pub(crate) fn set_axes(&mut self, code: u16, values: [Option<f32>; 4]) {
        let target = match code {
            201 => &mut self.limits.max_acc,
            203 => &mut self.limits.max_speed,
            _ => &mut self.limits.max_jerk,
        };
        for (slot, value) in target.iter_mut().zip(values) {
            if let Some(v) = value {
                *slot = v;
            }
        }
        if code == 205
            && values[1].is_none()
            && let Some(x) = values[0]
        {
            self.limits.max_jerk[1] = x;
        }
    }

    /// M205 S/T: мінімальні подачі друку й переміщень.
    pub(crate) fn set_min_rates(&mut self, s: Option<f32>, t: Option<f32>) {
        if let Some(s) = s {
            self.limits.min_extruding_rate = s;
        }
        if let Some(t) = t {
            self.limits.min_travel_rate = t;
        }
    }

    pub(crate) fn set_extrude_factor(&mut self, factor: f32) {
        self.extrude_factor = factor;
    }

    /// Рух G0/G1/G2/G3.
    #[inline(always)]
    pub(crate) fn push(&mut self, motion: &Motion, meta: MoveMeta) {
        let d = motion.delta;
        let extrusion_only = d[0] == 0.0 && d[1] == 0.0 && d[2] == 0.0 && d[3] != 0.0;
        let distance = match motion.arc {
            Some((arc_length, ..)) => (sqr(arc_length) + sqr(d[2])).sqrt(),
            None => {
                let xyz = sqr(d[0]) + sqr(d[1]) + sqr(d[2]);
                if xyz > 0.0 { xyz.sqrt() } else { d[3].abs() }
            }
        };
        if distance == 0.0 {
            return;
        }
        let inv_distance = 1.0 / distance;
        let curr = &mut self.curr;

        let is_travel = meta.kind == MoveKind::Travel;
        let travel_rate = if motion.arc.is_some() {
            is_travel
        } else {
            d[3] == 0.0
        };
        curr.feedrate = if travel_rate {
            motion.feedrate.max(self.limits.min_travel_rate)
        } else {
            motion.feedrate.max(self.limits.min_extruding_rate)
        };

        let mut acceleration;
        if let Some((arc_length, radius, start_dir, end_dir)) = motion.arc {
            curr.enter_direction = start_dir;
            curr.exit_direction = end_dir;
            let max_by_centripetal =
                (self.acceleration * radius).sqrt() / (arc_length * inv_distance);
            curr.feedrate = curr.feedrate.min(max_by_centripetal);
            let mut factor = 1.0f32;
            for (a, delta) in d.iter().enumerate() {
                if a < 2 {
                    curr.axis_feedrate[a] = curr.feedrate * arc_length * inv_distance;
                } else if a == 2 {
                    curr.axis_feedrate[a] = curr.feedrate * delta * inv_distance;
                } else {
                    curr.axis_feedrate[a] *= self.extrude_factor;
                }
                curr.abs_axis_feedrate[a] = curr.axis_feedrate[a].abs();
                if curr.abs_axis_feedrate[a] != 0.0 && self.limits.max_speed[a] != 0.0 {
                    factor = factor.min(self.limits.max_speed[a] / curr.abs_axis_feedrate[a]);
                }
            }
            curr.feedrate *= factor;
            if factor < 1.0 {
                for a in 0..4 {
                    curr.axis_feedrate[a] *= factor;
                    curr.abs_axis_feedrate[a] *= factor;
                }
            }
            acceleration = if is_travel {
                self.travel_acceleration
            } else {
                self.acceleration
            };
            let mut acc_factor = 1.0f32;
            for (a, delta) in d.iter().enumerate().take(3) {
                let axis_acc = if a < 2 {
                    acceleration * arc_length * inv_distance
                } else {
                    acceleration * delta.abs() * inv_distance
                };
                let max = self.limits.max_acc[a];
                if axis_acc != 0.0 && max != 0.0 && axis_acc > max {
                    acc_factor = acc_factor.min(max / axis_acc);
                }
            }
            acceleration *= acc_factor;
        } else {
            let dir = Vec3::new(d[0], d[1], d[2]);
            curr.enter_direction = if extrusion_only {
                dir
            } else {
                dir * inv_distance
            };
            curr.exit_direction = curr.enter_direction;
            acceleration = if is_travel {
                self.travel_acceleration
            } else if extrusion_only {
                self.retract_acceleration
            } else {
                self.acceleration
            };
            let prev_exit = self.prev.exit_direction;
            if (prev_exit.x != 0.0 || prev_exit.y != 0.0)
                && (curr.enter_direction.x != 0.0 || curr.enter_direction.y != 0.0)
                && !extrusion_only
            {
                let v1 = Vec3::new(prev_exit.x, prev_exit.y, 0.0).normalize();
                let v2 = Vec3::new(curr.enter_direction.x, curr.enter_direction.y, 0.0).normalize();
                let norm_diff = (v2 - v1).length_squared();
                if norm_diff < 0.25 && norm_diff > 0.0000000001 {
                    let dot = v1.x * v2.x + v1.y * v2.y;
                    let sin_theta_2 = ((1.0 - dot.clamp(-1.0, 1.0)) * 0.5).sqrt();
                    let r = (sqr(d[0]) + sqr(d[1])).sqrt() * 0.5 / sin_theta_2;
                    curr.feedrate = curr.feedrate.min((acceleration * r).sqrt());
                }
            }
            let mut factor = 1.0f32;
            for (a, delta) in d.iter().enumerate() {
                curr.axis_feedrate[a] = curr.feedrate * delta * inv_distance;
                if a == 3 {
                    curr.axis_feedrate[a] *= self.extrude_factor;
                }
                curr.abs_axis_feedrate[a] = curr.axis_feedrate[a].abs();
                if curr.abs_axis_feedrate[a] != 0.0 && self.limits.max_speed[a] != 0.0 {
                    factor = factor.min(self.limits.max_speed[a] / curr.abs_axis_feedrate[a]);
                }
            }
            curr.feedrate *= factor;
            if factor < 1.0 {
                for a in 0..4 {
                    curr.axis_feedrate[a] *= factor;
                    curr.abs_axis_feedrate[a] *= factor;
                }
            }
            for (a, delta) in d.iter().enumerate() {
                let max = self.limits.max_acc[a];
                if max > 0.0 && acceleration * delta.abs() * inv_distance > max {
                    acceleration = max / (delta.abs() * inv_distance);
                }
            }
            // Дуги в Bambu не скидають safe_feedrate — лише прямі рухи.
            curr.safe_feedrate = curr.feedrate;
        }
        let cruise = curr.feedrate;

        for a in 0..4 {
            if curr.abs_axis_feedrate[a] > self.limits.max_jerk[a] {
                curr.safe_feedrate = curr.safe_feedrate.min(self.limits.max_jerk[a]);
            }
        }

        let mut vmax_junction = curr.safe_feedrate;
        if !self.blocks.is_empty() && self.prev.feedrate > 0.0001 {
            vmax_junction = self.prev.feedrate.min(cruise);
            let exit = self.prev.exit_direction;
            let enter = curr.enter_direction;
            let jerk_v = (enter - exit).abs();
            let mut limited = false;
            let mut k_min = 10000.0f32;
            for i in 0..3 {
                if jerk_v[i] > 0.0 {
                    limited = true;
                    k_min = k_min.min(self.limits.max_jerk[i] / jerk_v[i]);
                }
            }
            if limited {
                vmax_junction = k_min;
            }
            let threshold = vmax_junction * 0.99;
            if self.prev.safe_feedrate > threshold && curr.safe_feedrate > threshold {
                vmax_junction = curr.safe_feedrate;
            }
        }

        let v_allowable = max_allowable_speed(-acceleration, curr.safe_feedrate, distance);
        let block = Block {
            meta,
            distance,
            acceleration,
            entry: vmax_junction.min(v_allowable),
            cruise,
            exit: curr.safe_feedrate,
            max_entry_speed: vmax_junction,
            safe_feedrate: curr.safe_feedrate,
            nominal_length: cruise <= v_allowable,
            recalculate: true,
            accelerate_until: 0.0,
            decelerate_after: 0.0,
            cruise_feedrate: 0.0,
        };
        self.prev = self.curr;
        self.blocks.push(block);
        if self.blocks.len() > REFRESH_THRESHOLD {
            self.calculate_time(QUEUE_SIZE, 0.0, None);
        }
    }

    /// `simulate_st_synchronize`: обробити всю чергу, додавши паузу `additional` с.
    /// `role` — кому належить пауза (`None` — першому блоку будь-якої ролі).
    pub(crate) fn synchronize(&mut self, additional: f32, role: Option<Role>) {
        self.calculate_time(0, additional, role);
    }

    fn calculate_time(&mut self, keep_last: usize, additional: f32, role: Option<Role>) {
        if self.blocks.is_empty() {
            if additional > 0.0 {
                self.additional.push((role, additional));
            }
            return;
        }
        let mut buffer = std::mem::take(&mut self.additional);
        if additional > 0.0 {
            buffer.push((role, additional));
        }
        // Сусідні паузи однієї ролі зливаються.
        let mut merged: Vec<(Option<Role>, f32)> = Vec::with_capacity(buffer.len());
        for item in buffer {
            match merged.last_mut() {
                Some(last) if last.0 == item.0 => last.1 += item.1,
                _ => merged.push(item),
            }
        }

        let blocks = &mut self.blocks;
        for i in 0..blocks.len() - 1 {
            let (prev, curr) = (blocks[i], &mut blocks[i + 1]);
            if !prev.nominal_length && prev.entry < curr.entry {
                let entry = curr.entry.min(max_allowable_speed(
                    -prev.acceleration,
                    prev.entry,
                    prev.distance,
                ));
                if curr.entry != entry {
                    curr.entry = entry;
                    curr.recalculate = true;
                }
            }
        }
        for i in (1..blocks.len()).rev() {
            let next_entry = blocks[i].entry;
            let curr = &mut blocks[i - 1];
            if curr.entry != curr.max_entry_speed {
                curr.entry = if !curr.nominal_length && curr.max_entry_speed > next_entry {
                    curr.max_entry_speed.min(max_allowable_speed(
                        -curr.acceleration,
                        next_entry,
                        curr.distance,
                    ))
                } else {
                    curr.max_entry_speed
                };
                curr.recalculate = true;
            }
        }
        for i in 0..blocks.len() {
            if i + 1 < blocks.len() {
                if blocks[i].recalculate || blocks[i + 1].recalculate {
                    blocks[i].exit = blocks[i + 1].entry;
                    blocks[i].calculate_trapezoid();
                    blocks[i].recalculate = false;
                }
            } else {
                let b = &mut blocks[i];
                b.exit = b.safe_feedrate;
                b.calculate_trapezoid();
                b.recalculate = false;
            }
        }

        let process = blocks.len() - keep_last;
        let mut extra = 0usize;
        for i in 0..process {
            let block = self.blocks[i];
            let move_time = block.time();
            let mut time = move_time;
            if let Some(&(role, t)) = merged.get(extra)
                && (role.is_none() || role == Some(block.meta.role))
            {
                time += t;
                extra += 1;
            }
            self.total += time;
            if block.meta.timelapse {
                self.timelapse += move_time;
            }
            if !block.meta.prepare || block.meta.kind != MoveKind::Travel {
                self.moves[block.meta.kind as usize] += time;
            }
            self.roles[block.meta.role as usize] += time;
            let layer = block.meta.layer.max(1) as usize;
            if layer > self.layers.len() {
                self.layers.resize(layer, 0.0);
            }
            self.layers[layer - 1] += time;
            if block.meta.prepare {
                self.prepare += time;
            }
            let (from, to) = block.meta.segments;
            if to > from {
                let to = to as usize;
                if self.segment_times.len() < to {
                    self.segment_times.resize(to, 0.0);
                }
                // Дуга розбита на кілька сегментів — час ділиться пропорційно до кількості.
                let share = time / (to - from as usize) as f32;
                for t in &mut self.segment_times[from as usize..to] {
                    *t += share;
                }
            }
        }
        self.additional = merged.split_off(extra.min(merged.len()));
        if keep_last > 0 {
            self.blocks.drain(..process);
        } else {
            self.blocks.clear();
        }
    }

    pub(crate) fn finish(mut self, segments: usize) -> (TimeTotals, Vec<f32>) {
        self.calculate_time(0, 0.0, None);
        let mut layers = self.layers;
        if let Some(first) = layers.first_mut() {
            *first = (*first - self.prepare).max(0.0);
        }
        let mut roles: Vec<_> = Role::ALL
            .into_iter()
            .zip(self.roles)
            .filter(|(_, t)| *t > 0.0)
            .collect();
        roles.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mut moves: Vec<_> = [
            MoveKind::Retract,
            MoveKind::Unretract,
            MoveKind::Travel,
            MoveKind::Wipe,
            MoveKind::Extrude,
        ]
        .into_iter()
        .zip(self.moves)
        .filter(|(_, t)| *t > 0.0)
        .collect();
        moves.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mut segment_times = self.segment_times;
        segment_times.resize(segments, 0.0);
        (
            TimeTotals {
                total: self.total,
                prepare: self.prepare,
                timelapse: self.timelapse,
                roles,
                moves,
                layers,
            },
            segment_times,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn axis_commands_preserve_explicit_y_and_defaults() {
        let mut t = TimeMachine::new(Limits::from_config(&HashMap::new()));
        assert_eq!(t.limits.max_jerk, [10.0, 10.0, 0.2, 2.5]);
        t.set_axes(201, [Some(2000.0), Some(3000.0), None, None]);
        assert_eq!(t.limits.max_acc, [2000.0, 3000.0, 500.0, 5000.0]);
        t.set_axes(203, [Some(200.0), Some(100.0), None, None]);
        assert_eq!(t.limits.max_speed, [200.0, 100.0, 12.0, 120.0]);
        t.set_axes(205, [Some(9.0), Some(7.0), None, None]);
        assert_eq!(t.limits.max_jerk[1], 7.0);
        t.set_axes(205, [Some(8.0), None, None, None]);
        assert_eq!(t.limits.max_jerk[1], 8.0);
        let config = HashMap::from([
            ("gcode_flavor".into(), "reprapfirmware".into()),
            ("machine_max_jerk_x".into(), "0".into()),
        ]);
        assert_eq!(Limits::from_config(&config).max_jerk[0], 10.0);
    }
}
