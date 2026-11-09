use std::collections::HashMap;
use std::ops::Range;
use std::time::Duration;

use glam::Vec3;

/// Типи ліній траєкторії, що відповідають ExtrusionRole в OrcaSlicer плюс Travel/Retract/Wipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Role {
    #[default]
    None,
    Perimeter,
    ExternalPerimeter,
    OverhangPerimeter,
    InternalInfill,
    SolidInfill,
    TopSolidInfill,
    BottomSurface,
    Ironing,
    BridgeInfill,
    InternalBridgeInfill,
    GapFill,
    Skirt,
    Brim,
    SupportMaterial,
    SupportMaterialInterface,
    SupportTransition,
    WipeTower,
    Custom,
    Mixed,
    Travel,
    Retract,
    Wipe,
    /// Маркер: повернення філаменту без руху (E-only).
    Unretract,
    /// Маркер: шов зовнішньої стінки.
    Seam,
}

impl Role {
    pub(crate) const COUNT: usize = 25;
    pub(crate) const ALL: [Self; Self::COUNT] = [
        Self::None,
        Self::Perimeter,
        Self::ExternalPerimeter,
        Self::OverhangPerimeter,
        Self::InternalInfill,
        Self::SolidInfill,
        Self::TopSolidInfill,
        Self::BottomSurface,
        Self::Ironing,
        Self::BridgeInfill,
        Self::InternalBridgeInfill,
        Self::GapFill,
        Self::Skirt,
        Self::Brim,
        Self::SupportMaterial,
        Self::SupportMaterialInterface,
        Self::SupportTransition,
        Self::WipeTower,
        Self::Custom,
        Self::Mixed,
        Self::Travel,
        Self::Retract,
        Self::Wipe,
        Self::Unretract,
        Self::Seam,
    ];
    /// Кольори OrcaSlicer у просторі sRGB [0..1].
    pub fn color_srgb(&self) -> [f32; 4] {
        let rgb = match self {
            Role::None => [230.0, 179.0, 179.0],
            Role::Perimeter => [255.0, 230.0, 77.0],
            Role::ExternalPerimeter => [255.0, 125.0, 56.0],
            Role::OverhangPerimeter => [31.0, 31.0, 255.0],
            Role::InternalInfill => [176.0, 48.0, 41.0],
            Role::SolidInfill => [150.0, 84.0, 204.0],
            Role::TopSolidInfill => [240.0, 64.0, 64.0],
            Role::BottomSurface => [102.0, 92.0, 199.0],
            Role::Ironing => [255.0, 140.0, 105.0],
            Role::BridgeInfill | Role::InternalBridgeInfill => [77.0, 128.0, 186.0],
            Role::GapFill => [255.0, 255.0, 255.0],
            Role::Skirt => [0.0, 135.0, 110.0],
            Role::Brim => [0.0, 59.0, 110.0],
            Role::SupportMaterial => [0.0, 255.0, 0.0],
            Role::SupportMaterialInterface => [0.0, 128.0, 0.0],
            Role::SupportTransition => [0.0, 64.0, 0.0],
            Role::WipeTower => [179.0, 227.0, 171.0],
            Role::Custom => [94.0, 209.0, 148.0],
            Role::Mixed => [128.0, 128.0, 128.0],
            Role::Travel => [56.0, 72.0, 155.0],
            Role::Wipe => [255.0, 255.0, 0.0],
            // Кольори опцій Bambu Studio (Options_Colors).
            Role::Retract => [205.0, 34.0, 214.0],
            Role::Unretract => [73.0, 173.0, 207.0],
            Role::Seam => [230.0, 230.0, 230.0],
        };
        let a = if *self == Role::Travel { 0.4 } else { 1.0 };
        [rgb[0] / 255.0, rgb[1] / 255.0, rgb[2] / 255.0, a]
    }

    /// Кольори перетворені в лінійний простір для коректного шейдингу на sRGB цілі.
    pub fn color_linear(&self) -> [f32; 4] {
        let [r, g, b, a] = self.color_srgb();
        [srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b), a]
    }

    /// Розпізнавання тегу `; FEATURE: <назва>` з Bambu Studio / OrcaSlicer.
    pub fn from_feature_tag(tag: &str) -> Self {
        let trimmed = tag.trim();
        match trimmed {
            "Inner wall" => Role::Perimeter,
            "Outer wall" => Role::ExternalPerimeter,
            "Overhang wall" => Role::OverhangPerimeter,
            "Sparse infill" => Role::InternalInfill,
            // GCodeProcessor Bambu рахує плаваючу оболонку як суцільне заповнення.
            "Internal solid infill" | "Floating vertical shell" => Role::SolidInfill,
            "Top surface" => Role::TopSolidInfill,
            "Bottom surface" => Role::BottomSurface,
            "Ironing" => Role::Ironing,
            "Bridge" => Role::BridgeInfill,
            "Internal Bridge" => Role::InternalBridgeInfill,
            "Gap infill" => Role::GapFill,
            "Skirt" => Role::Skirt,
            "Brim" => Role::Brim,
            "Support" => Role::SupportMaterial,
            "Support interface" => Role::SupportMaterialInterface,
            "Support transition" => Role::SupportTransition,
            "Prime tower" | "Wipe tower" => Role::WipeTower,
            "Custom" => Role::Custom,
            "Multiple" => Role::Mixed,
            // Невідомий тег — erNone, як `string_to_role` у Bambu.
            _ => Role::None,
        }
    }

    /// Назва ролі для UI та легенди (українською).
    pub fn name_uk(&self) -> &'static str {
        match self {
            Role::None => "Не визначено",
            Role::Perimeter => "Внутрішня стінка",
            Role::ExternalPerimeter => "Зовнішня стінка",
            Role::OverhangPerimeter => "Звисаюча стінка",
            Role::InternalInfill => "Розріджене заповнення",
            Role::SolidInfill => "Суцільне заповнення",
            Role::TopSolidInfill => "Верхня поверхня",
            Role::BottomSurface => "Нижня поверхня",
            Role::Ironing => "Прасування",
            Role::BridgeInfill => "Міст",
            Role::InternalBridgeInfill => "Внутрішній міст",
            Role::GapFill => "Заповнення проміжків",
            Role::Skirt => "Спідниця",
            Role::Brim => "Кайма",
            Role::SupportMaterial => "Підтримка",
            Role::SupportMaterialInterface => "Інтерфейс підтримки",
            Role::SupportTransition => "Перехід підтримки",
            Role::WipeTower => "Башта очищення",
            Role::Custom => "Користувацький G-code",
            Role::Mixed => "Змішаний",
            Role::Travel => "Холостий хід",
            Role::Retract => "Ретракція",
            Role::Wipe => "Витирання сопла",
            Role::Unretract => "Повернення філаменту",
            Role::Seam => "Шви",
        }
    }
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Один сегмент руху екструдера.
#[derive(Debug, Clone, Copy)]
pub struct Segment {
    pub from: Vec3,
    pub to: Vec3,
    pub role: Role,
    pub width: f32,
    pub height: f32,
    pub feedrate: f32,
    pub tool: u8,
    pub is_extrusion: bool,
    /// Вентилятор обдуву деталі, 0–100 %.
    pub fan: u8,
    /// Задана температура сопла, °C.
    pub temp: u16,
    /// Подача філаменту на цьому русі, мм (від'ємна — ретракція).
    pub extruded: f32,
    /// Оцінка часу руху з урахуванням прискорень, с.
    pub time: f32,
}

impl Segment {
    pub fn length(&self) -> f32 {
        (self.to - self.from).length()
    }

    /// Швидкість руху, мм/с.
    pub fn speed(&self) -> f32 {
        self.feedrate / 60.0
    }

    /// Об'ємна витрата, мм³/с: філамент Ø1.75 мм на час руху.
    pub fn volumetric_flow(&self) -> f32 {
        const FILAMENT_AREA: f32 = std::f32::consts::PI * 0.875 * 0.875;
        let time = self.length() / self.speed().max(1e-3);
        if time <= 1e-6 {
            0.0
        } else {
            self.extruded.max(0.0) * FILAMENT_AREA / time
        }
    }
}

/// Шар моделі з діапазоном сегментів та часовою оцінкою.
#[derive(Debug, Clone)]
pub struct Layer {
    pub z: f32,
    pub height: f32,
    pub segment_range: Range<usize>,
    pub time_estimate: Option<Duration>,
}

/// Статистика та заголовки з G-code.
#[derive(Debug, Clone, Default)]
pub struct GcodeStats {
    pub total_estimated_time: Option<Duration>,
    pub model_printing_time: Option<Duration>,
    pub total_layer_number: Option<u32>,
    pub total_filament_length_mm: Option<f64>,
    pub total_filament_volume_cm3: Option<f64>,
    pub total_filament_weight_g: Option<f64>,
    pub max_z_height: Option<f32>,
    pub filament_density: Vec<f32>,
    pub filament_diameter: Vec<f32>,
    pub config: HashMap<String, String>,
}

/// Точкова позначка в траєкторії: шов, втягування чи повернення філаменту.
#[derive(Debug, Clone, Copy)]
pub struct Marker {
    pub pos: Vec3,
    /// `Role::Seam`, `Role::Retract` або `Role::Unretract`.
    pub kind: Role,
    /// Останній сегмент перед позначкою — для повзунка рухів.
    pub segment: u32,
    pub width: f32,
    pub height: f32,
}

/// Філамент, витиснутий рухами екструзії однієї ролі одним інструментом, мм.
#[derive(Debug, Clone, Copy)]
pub struct FilamentUse {
    pub role: Role,
    pub tool: u8,
    pub mm: f64,
}

/// Повна розпарсена траєкторія друку.
#[derive(Debug, Clone)]
pub struct Toolpath {
    pub layers: Vec<Layer>,
    pub segments: Vec<Segment>,
    pub stats: GcodeStats,
    /// Позначки в порядку `segment`.
    pub markers: Vec<Marker>,
    /// Власна оцінка часу (порт планувальника Bambu).
    pub estimate: crate::time::TimeTotals,
    pub filament: Vec<FilamentUse>,
}

impl Toolpath {
    /// Обчислює обмежувальний паралелепіпед (AABB) для всіх ліній екструзії.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        let mut has_points = false;

        for seg in &self.segments {
            if seg.is_extrusion {
                min = min.min(seg.from).min(seg.to);
                max = max.max(seg.from).max(seg.to);
                has_points = true;
            }
        }

        if has_points { Some((min, max)) } else { None }
    }

    /// Кількість шарів.
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    /// Загальна кількість сегментів.
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }
}
