//! Статистика нарізки для панелі «Перегляду», як легенда Bambu Studio: час, частка й філамент
//! за типами ліній, переміщення, підсумки часу, філаменту й вартості.
//!
//! Джерела, від точнішого: `result.json` рушія (час за типами з його `GCodeProcessor`),
//! шапка G-code (повний час, час моделі, філамент), власна оцінка (порт планувальника Bambu,
//! `time.rs`) і власні суми подачі E. Рядок без даних не показується.

use std::path::Path;

use crate::model::{Role, Toolpath};
use crate::time::MoveKind;

/// Оцінки рушія з `result.json` (Bambu Studio CLI) для однієї пластини.
#[derive(Clone, Debug, Default)]
pub struct EngineReport {
    /// `total_predication`, с.
    pub total: Option<f32>,
    /// Час за типами (`feature_type_times`): назви ролей Bambu (`Outer wall`, …).
    pub feature_times: Vec<(String, f32)>,
}

impl EngineReport {
    /// Читає `result.json`; `plate` — номер пластини (якщо пластина одна, номер не потрібен).
    pub fn read(path: &Path, plate: Option<u32>) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        Self::parse(&text, plate)
    }

    pub fn parse(text: &str, plate: Option<u32>) -> Option<Self> {
        let json: serde_json::Value = serde_json::from_str(text).ok()?;
        let plates = json.get("sliced_plates")?.as_array()?;
        let entry = plates
            .iter()
            .find(|p| {
                plate.is_some_and(|n| p.get("id").and_then(|v| v.as_u64()) == Some(u64::from(n)))
            })
            .or_else(|| (plates.len() == 1).then(|| &plates[0]))?;
        let finite = |v: &serde_json::Value| v.as_f64().filter(|x| x.is_finite() && *x >= 0.0);
        let feature_times = entry
            .get("feature_type_times")
            .and_then(|v| v.as_object())
            .map(|map| {
                map.iter()
                    .filter_map(|(k, v)| Some((k.clone(), finite(v)? as f32)))
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            total: entry
                .get("total_predication")
                .and_then(finite)
                .map(|v| v as f32),
            feature_times,
        })
    }

    fn feature(&self, name: &str) -> Option<f32> {
        self.feature_times
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, t)| *t)
    }
}

/// Звідки взято значення — показується в підказці панелі.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Рушій нарізки (`result.json` або шапка G-code).
    Engine,
    /// Власна оцінка Slicenv з G-code.
    Estimate,
}

/// Рядок таблиці типів ліній.
#[derive(Clone, Debug)]
pub struct LineRow {
    pub role: Role,
    pub time: f32,
    /// Частка від суми показаних рядків і переміщень, 0..1.
    pub share: f32,
    pub filament_m: f64,
    pub filament_g: Option<f64>,
}

/// Філамент одного інструмента.
#[derive(Clone, Debug)]
pub struct ToolRow {
    pub tool: u8,
    pub m: f64,
    pub g: Option<f64>,
    pub cost: Option<f64>,
}

/// Зведена статистика.
#[derive(Clone, Debug)]
pub struct Report {
    pub rows: Vec<LineRow>,
    pub rows_source: Source,
    /// Переміщення: час і частка.
    pub travel: Option<(f32, f32)>,
    pub retracts: usize,
    pub unretracts: usize,
    pub seams: usize,
    pub wipes: usize,
    pub total_time: f32,
    pub total_source: Source,
    /// Підготовка (стартовий G-code), с.
    pub prepare: Option<f32>,
    /// Timelapse — рухи блоків `SKIPTYPE: timelapse`, с (власна оцінка).
    pub timelapse: Option<f32>,
    /// Друк моделі: повний час без підготовки й timelapse.
    pub model_time: f32,
    /// Увесь філамент, м і г.
    pub total_filament: Option<(f64, f64)>,
    /// Без підтримки й башти очищення; `None`, коли філаментів кілька (промивку не видно).
    pub model_filament: Option<(f64, f64)>,
    pub cost: Option<f64>,
    pub tools: Vec<ToolRow>,
}

impl Role {
    /// Назва ролі в Bambu Studio (`ExtrusionEntity::role_to_string`) — ключ `feature_type_times`.
    pub fn bambu_name(self) -> Option<&'static str> {
        Some(match self {
            Role::None => "Undefined",
            Role::Perimeter => "Inner wall",
            Role::ExternalPerimeter => "Outer wall",
            Role::OverhangPerimeter => "Overhang wall",
            Role::InternalInfill => "Sparse infill",
            Role::SolidInfill => "Internal solid infill",
            Role::TopSolidInfill => "Top surface",
            Role::BottomSurface => "Bottom surface",
            Role::Ironing => "Ironing",
            Role::BridgeInfill => "Bridge",
            Role::InternalBridgeInfill => "Internal Bridge",
            Role::GapFill => "Gap infill",
            Role::Skirt => "Skirt",
            Role::Brim => "Brim",
            Role::SupportMaterial => "Support",
            Role::SupportMaterialInterface => "Support interface",
            Role::SupportTransition => "Support transition",
            Role::WipeTower => "Prime tower",
            Role::Custom => "Custom",
            Role::Mixed => "Multiple",
            _ => return None,
        })
    }
}

/// Список `a,b;c` з конфігу як числа.
fn config_list(toolpath: &Toolpath, key: &str) -> Vec<f64> {
    toolpath
        .stats
        .config
        .get(key)
        .map(|v| {
            v.split([',', ';'])
                .filter_map(|x| x.trim().trim_matches('"').parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

impl Toolpath {
    /// Зведення для панелі; `engine` — `result.json` рушія, якщо є.
    pub fn report(&self, engine: Option<&EngineReport>) -> Report {
        let est = &self.estimate;
        let header_total = self.stats.total_estimated_time.map(|d| d.as_secs_f32());
        let header_model = self.stats.model_printing_time.map(|d| d.as_secs_f32());
        let (total_time, total_source) = match engine.and_then(|e| e.total).or(header_total) {
            Some(t) => (t, Source::Engine),
            None => (est.total, Source::Estimate),
        };
        let share = |t: f32| {
            if total_time > 0.0 {
                t / total_time
            } else {
                0.0
            }
        };

        // Шапка Bambu: «model printing time» = повний час − підготовка.
        let prepare = match (header_total, header_model) {
            (Some(t), Some(m)) if t >= m => Some(t - m),
            _ => (est.prepare > 0.0).then_some(est.prepare),
        }
        .filter(|p| *p > 0.0);
        let timelapse = (est.timelapse > 0.0).then_some(est.timelapse);
        let model_time = (total_time - prepare.unwrap_or(0.0) - timelapse.unwrap_or(0.0)).max(0.0);

        let diameters = config_list(self, "filament_diameter");
        let densities = config_list(self, "filament_density");
        let costs = config_list(self, "filament_cost");
        let pick = |v: &[f64], tool: u8, default: f64| {
            v.get(usize::from(tool))
                .or(v.last())
                .copied()
                .unwrap_or(default)
        };
        let grams = |mm: f64, tool: u8| -> Option<f64> {
            let d = *diameters.get(usize::from(tool)).or(diameters.last())?;
            let density = *densities.get(usize::from(tool)).or(densities.last())?;
            (d > 0.0 && density > 0.0)
                .then_some(mm * std::f64::consts::PI * d * d / 4.0 * density / 1000.0)
        };

        // Рядки — ролі, що мають лінії екструзії, у порядку Bambu (порядок переліку Role).
        let engine_rows = engine.filter(|e| !e.feature_times.is_empty());
        let mut roles: Vec<Role> = self
            .filament
            .iter()
            .filter(|f| f.mm > 0.0)
            .map(|f| f.role)
            .collect();
        roles.sort_by_key(|r| *r as u8);
        roles.dedup();
        let mut rows: Vec<LineRow> = roles
            .into_iter()
            .map(|role| {
                let time = match engine_rows {
                    Some(e) => role.bambu_name().and_then(|n| e.feature(n)).unwrap_or(0.0),
                    None => est.role(role),
                };
                let (mm, g) = self
                    .filament
                    .iter()
                    .filter(|f| f.role == role)
                    .fold((0.0, Some(0.0)), |(mm, g), f| {
                        (mm + f.mm, g.zip(grams(f.mm, f.tool)).map(|(a, b)| a + b))
                    });
                LineRow {
                    role,
                    time,
                    share: share(time),
                    filament_m: mm / 1000.0,
                    filament_g: g,
                }
            })
            .collect();
        let mut travel = match engine_rows {
            Some(e) => e.feature("Travel"),
            None => Some(est.moves(MoveKind::Travel)),
        }
        .filter(|t| *t > 0.0)
        .map(|t| (t, share(t)));

        // Частки описують розподіл видимих рядків незалежно від джерела повного часу.
        let denominator = rows.iter().map(|r| r.time).sum::<f32>() + travel.map_or(0.0, |(t, _)| t);
        if denominator > 0.0 {
            for row in &mut rows {
                row.share = row.time / denominator;
            }
            if let Some((t, share)) = &mut travel {
                *share = *t / denominator;
            }
        }

        let count = |role: Role| self.markers.iter().filter(|m| m.kind == role).count();
        let wipes = {
            let mut n = 0;
            let mut prev = false;
            for s in &self.segments {
                let wipe = s.role == Role::Wipe;
                n += usize::from(wipe && !prev);
                prev = wipe;
            }
            n
        };

        // Філамент за інструментами — з власних сум E.
        let mut by_tool: Vec<(u8, f64)> = Vec::new();
        for f in &self.filament {
            match by_tool.iter_mut().find(|(t, _)| *t == f.tool) {
                Some(slot) => slot.1 += f.mm,
                None => by_tool.push((f.tool, f.mm)),
            }
        }
        by_tool.sort_by_key(|(t, _)| *t);
        let single = by_tool.len() == 1;
        let header_filament = self
            .stats
            .total_filament_length_mm
            .zip(self.stats.total_filament_weight_g)
            .map(|(mm, g)| (mm / 1000.0, g));
        let tools: Vec<ToolRow> = by_tool
            .iter()
            .map(|&(tool, mm)| {
                // Один філамент: шапка рушія точніша за наші суми (вона ж і в Bambu).
                let (m, g) = match header_filament {
                    Some((m, g)) if single => (m, Some(g)),
                    _ => (mm / 1000.0, grams(mm, tool)),
                };
                let cost = g
                    .filter(|_| !costs.is_empty())
                    .map(|g| g * pick(&costs, tool, 0.0) / 1000.0);
                ToolRow { tool, m, g, cost }
            })
            .collect();
        let total_filament = header_filament.or_else(|| {
            if tools.is_empty() {
                return None;
            }
            tools
                .iter()
                .try_fold((0.0, 0.0), |(m, g), t| Some((m + t.m, g + t.g?)))
        });
        let excluded = self
            .filament
            .iter()
            .filter(|f| {
                matches!(
                    f.role,
                    Role::SupportMaterial
                        | Role::SupportMaterialInterface
                        | Role::SupportTransition
                        | Role::WipeTower
                )
            })
            .try_fold((0.0, 0.0), |(m, g), f| {
                Some((m + f.mm / 1000.0, g + grams(f.mm, f.tool)?))
            });
        let model_filament = total_filament
            .filter(|_| single)
            .zip(excluded)
            .map(|((m, g), (em, eg))| ((m - em).max(0.0), (g - eg).max(0.0)));
        let cost = tools
            .iter()
            .map(|t| t.cost)
            .sum::<Option<f64>>()
            .filter(|_| !tools.is_empty());

        Report {
            rows,
            rows_source: if engine_rows.is_some() {
                Source::Engine
            } else {
                Source::Estimate
            },
            travel,
            retracts: count(Role::Retract),
            unretracts: count(Role::Unretract),
            seams: count(Role::Seam),
            wipes,
            total_time,
            total_source,
            prepare,
            timelapse,
            model_time,
            total_filament,
            model_filament,
            cost,
            tools,
        }
    }
}
