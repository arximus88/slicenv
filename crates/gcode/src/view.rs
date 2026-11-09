//! Схеми кольорів «Перегляду», як в Orca: тип лінії, висота, ширина, швидкість, вентилятор,
//! температура, об'ємна витрата, час шару, філамент.

use crate::model::{Role, Segment, Toolpath};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorScheme {
    #[default]
    LineType,
    LayerHeight,
    LineWidth,
    Speed,
    Fan,
    Temperature,
    Flow,
    LayerTime,
    Filament,
}

impl ColorScheme {
    pub const ALL: [ColorScheme; 9] = [
        ColorScheme::LineType,
        ColorScheme::LayerHeight,
        ColorScheme::LineWidth,
        ColorScheme::Speed,
        ColorScheme::Fan,
        ColorScheme::Temperature,
        ColorScheme::Flow,
        ColorScheme::LayerTime,
        ColorScheme::Filament,
    ];

    pub fn name_uk(self) -> &'static str {
        match self {
            ColorScheme::LineType => "Тип лінії",
            ColorScheme::LayerHeight => "Висота шару",
            ColorScheme::LineWidth => "Ширина лінії",
            ColorScheme::Speed => "Швидкість",
            ColorScheme::Fan => "Вентилятор",
            ColorScheme::Temperature => "Температура",
            ColorScheme::Flow => "Об’ємна витрата",
            ColorScheme::LayerTime => "Час шару",
            ColorScheme::Filament => "Філамент",
        }
    }

    /// Одиниця для легенди градієнта.
    pub fn unit(self) -> &'static str {
        match self {
            ColorScheme::LayerHeight | ColorScheme::LineWidth => "мм",
            ColorScheme::Speed => "мм/с",
            ColorScheme::Fan => "%",
            ColorScheme::Temperature => "°C",
            ColorScheme::Flow => "мм³/с",
            ColorScheme::LayerTime => "с",
            ColorScheme::LineType | ColorScheme::Filament => "",
        }
    }

    /// Чи це неперервна шкала (градієнт), а не набір категорій.
    pub fn is_gradient(self) -> bool {
        !matches!(self, ColorScheme::LineType | ColorScheme::Filament)
    }

    /// Значення сегмента для градієнтної схеми; `layer_time` — час його шару, с.
    pub fn value(self, seg: &Segment, layer_time: f32) -> f32 {
        match self {
            ColorScheme::LayerHeight => seg.height,
            ColorScheme::LineWidth => seg.width,
            ColorScheme::Speed => seg.speed(),
            ColorScheme::Fan => f32::from(seg.fan),
            ColorScheme::Temperature => f32::from(seg.temp),
            ColorScheme::Flow => seg.volumetric_flow(),
            ColorScheme::LayerTime => layer_time,
            ColorScheme::LineType | ColorScheme::Filament => 0.0,
        }
    }
}

/// Шкала Orca/PrusaSlicer від синього до червоного (sRGB).
pub const RANGE_COLORS: [[f32; 3]; 11] = [
    [0.043, 0.173, 0.478],
    [0.075, 0.349, 0.522],
    [0.110, 0.533, 0.569],
    [0.016, 0.839, 0.059],
    [0.667, 0.949, 0.000],
    [0.988, 0.975, 0.012],
    [0.961, 0.808, 0.039],
    [0.890, 0.533, 0.125],
    [0.820, 0.408, 0.188],
    [0.761, 0.322, 0.235],
    [0.581, 0.149, 0.087],
];

/// Колір шкали для `t` у [0, 1] з лінійною інтерполяцією між сусідніми кольорами.
pub fn gradient(t: f32) -> [f32; 3] {
    let t = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let pos = t * (RANGE_COLORS.len() - 1) as f32;
    let i = (pos.floor() as usize).min(RANGE_COLORS.len() - 2);
    let f = pos - i as f32;
    let (a, b) = (RANGE_COLORS[i], RANGE_COLORS[i + 1]);
    [
        a[0] + (b[0] - a[0]) * f,
        a[1] + (b[1] - a[1]) * f,
        a[2] + (b[2] - a[2]) * f,
    ]
}

/// Кольори філаментів з конфігу G-code (`filament_colour = #RRGGBB;#…`), sRGB.
pub fn filament_colors(toolpath: &Toolpath) -> Vec<[f32; 3]> {
    toolpath
        .stats
        .config
        .get("filament_colour")
        .map(|v| v.split([';', ',']).filter_map(parse_hex).collect())
        .unwrap_or_default()
}

/// Філамент слота з конфігу G-code: колір (`filament_colour`), тип (`filament_type`) і
/// назва профілю (`filament_settings_id`). Номер слота — індекс у векторі (`Segment::tool`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FilamentSlot {
    /// sRGB; `None`, якщо колір не записано.
    pub colour: Option<[f32; 3]>,
    pub kind: String,
    pub name: String,
}

/// Слоти філаменту з конфігу G-code. Довжина — найбільша з трьох списків: у старих
/// файлах `filament_settings_id` буває коротшим за `filament_colour`.
pub fn filament_slots(toolpath: &Toolpath) -> Vec<FilamentSlot> {
    let config = &toolpath.stats.config;
    let list = |key: &str| -> Vec<String> {
        config
            .get(key)
            .map(|v| {
                v.split(';')
                    .map(|s| s.trim().trim_matches('"').trim().to_owned())
                    .collect()
            })
            .unwrap_or_default()
    };
    let colours = list("filament_colour");
    let kinds = list("filament_type");
    let names = list("filament_settings_id");
    let count = colours.len().max(kinds.len()).max(names.len());
    (0..count)
        .map(|i| FilamentSlot {
            colour: colours.get(i).and_then(|c| parse_hex(c)),
            kind: kinds.get(i).cloned().unwrap_or_default(),
            name: names.get(i).cloned().unwrap_or_default(),
        })
        .collect()
}

fn parse_hex(s: &str) -> Option<[f32; 3]> {
    let hex = s.trim().trim_matches('"').trim_start_matches('#');
    let v = u32::from_str_radix(hex.get(..6)?, 16).ok()?;
    Some([
        ((v >> 16) & 0xff) as f32 / 255.0,
        ((v >> 8) & 0xff) as f32 / 255.0,
        (v & 0xff) as f32 / 255.0,
    ])
}

/// Що й як показувати в траєкторії.
#[derive(Clone, Debug)]
pub struct ViewOptions {
    pub scheme: ColorScheme,
    /// Приховані типи ліній і позначок (клік по легенді).
    pub hidden: Vec<Role>,
}

impl ViewOptions {
    /// Позначки, що за замовчуванням сховані, як у Bambu Studio (шви видно).
    pub const HIDDEN_BY_DEFAULT: [Role; 3] = [Role::Retract, Role::Unretract, Role::Wipe];
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            scheme: ColorScheme::default(),
            hidden: Self::HIDDEN_BY_DEFAULT.to_vec(),
        }
    }
}

impl ViewOptions {
    pub fn is_visible(&self, role: Role) -> bool {
        !self.hidden.contains(&role)
    }
}

impl Toolpath {
    /// Час кожного шару, с (оцінка з довжин і подач).
    pub fn layer_times(&self) -> Vec<f32> {
        self.layers
            .iter()
            .map(|l| l.time_estimate.map_or(0.0, |d| d.as_secs_f32()))
            .collect()
    }

    /// Межі значень градієнтної схеми серед видимих ліній екструзії.
    pub fn value_range(&self, options: &ViewOptions) -> Option<(f32, f32)> {
        if !options.scheme.is_gradient() {
            return None;
        }
        self.value_range_with_times(options, &self.layer_times())
    }

    /// Частка часу видимої екструзії в кожному з `buckets` рівних інтервалів шкали
    /// `range` градієнтної схеми (від мінімуму до максимуму).
    pub fn time_shares(
        &self,
        options: &ViewOptions,
        range: (f32, f32),
        buckets: usize,
    ) -> Vec<f32> {
        let mut out = vec![0.0f32; buckets];
        if buckets == 0 || !options.scheme.is_gradient() {
            return out;
        }
        let (lo, hi) = range;
        let times = self.layer_times();
        let mut total = 0.0f32;
        for (layer, time) in self.layers.iter().zip(&times) {
            for seg in &self.segments[layer.segment_range.clone()] {
                if !seg.is_extrusion || !options.is_visible(seg.role) {
                    continue;
                }
                let v = options.scheme.value(seg, *time);
                let t = if hi - lo > 1e-6 {
                    (v - lo) / (hi - lo)
                } else {
                    0.5
                };
                if !t.is_finite() {
                    continue;
                }
                // Як кольори шкали: значення відходить до найближчого з `buckets` кольорів.
                let i = (t.clamp(0.0, 1.0) * (buckets - 1) as f32).round() as usize;
                out[i] += seg.time;
                total += seg.time;
            }
        }
        if total > 0.0 {
            for v in &mut out {
                *v /= total;
            }
        }
        out
    }

    /// Reuse already computed layer times when preparing colored toolpaths.
    pub fn value_range_with_times(
        &self,
        options: &ViewOptions,
        times: &[f32],
    ) -> Option<(f32, f32)> {
        if !options.scheme.is_gradient() {
            return None;
        }
        let mut range: Option<(f32, f32)> = None;
        for (layer, time) in self.layers.iter().zip(times) {
            for seg in &self.segments[layer.segment_range.clone()] {
                if !seg.is_extrusion || !options.is_visible(seg.role) {
                    continue;
                }
                let v = options.scheme.value(seg, *time);
                if v.is_finite() {
                    range = Some(range.map_or((v, v), |(lo, hi)| (lo.min(v), hi.max(v))));
                }
            }
        }
        range
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_ranges_respect_role_filters_and_empty_views() {
        let toolpath = crate::parse("; CHANGE_LAYER\n; Z_HEIGHT: 0.2\n; LAYER_HEIGHT: 0.2\n; FEATURE: Outer wall\nG1 X10 Y0 E1 F600\n; FEATURE: Inner wall\nG1 X20 Y0 E2 F1200\n").unwrap();
        let mut options = ViewOptions {
            scheme: ColorScheme::Speed,
            hidden: vec![],
        };
        assert_eq!(toolpath.value_range(&options), Some((10.0, 20.0)));
        options.hidden.push(Role::ExternalPerimeter);
        assert_eq!(toolpath.value_range(&options), Some((20.0, 20.0)));
        options.hidden.push(Role::Perimeter);
        assert_eq!(toolpath.value_range(&options), None);
        for scheme in ColorScheme::ALL {
            assert!(!scheme.name_uk().is_empty());
            options.scheme = scheme;
            assert_eq!(toolpath.value_range(&options), None);
        }
        assert_eq!(gradient(f32::NAN), RANGE_COLORS[0]);
    }

    #[test]
    fn filament_slots_read_colour_type_and_profile_name() {
        let toolpath = crate::parse(
            "; CONFIG_BLOCK_START\n; filament_colour = #000000;#FFFFFF\n; filament_settings_id = \"Bambu PLA Silk+ @BBL A1\";\"Bambu PETG HF @BBL A1\"\n; filament_type = PLA;PETG\n; CONFIG_BLOCK_END\nG1 X1 E1\n",
        )
        .unwrap();
        let slots = filament_slots(&toolpath);
        assert_eq!(slots.len(), 2);
        assert_eq!(slots[0].colour, Some([0.0, 0.0, 0.0]));
        assert_eq!(slots[1].colour, Some([1.0, 1.0, 1.0]));
        assert_eq!(slots[0].kind, "PLA");
        assert_eq!(slots[0].name, "Bambu PLA Silk+ @BBL A1");
        assert_eq!(slots[1].name, "Bambu PETG HF @BBL A1");
        assert!(filament_slots(&crate::parse("G1 X1 E1\n").unwrap()).is_empty());
    }
}
