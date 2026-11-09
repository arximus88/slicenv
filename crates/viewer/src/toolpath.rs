use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use wgpu::util::DeviceExt as _;

use crate::renderer::FlatVertex;

/// Інстанс сегмента траєкторії для шейдера.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct ToolpathInstance {
    pub from: [f32; 3],
    pub to: [f32; 3],
    pub color: [f32; 4],
    pub width: f32,
    pub height: f32,
    /// Бічний напрямок перерізу на початку й у кінці (XY). На стику з сусідньою лінією —
    /// бісектриса, подовжена на 1/cos половини кута, тож валик згинається без шва.
    pub side_from: [f32; 2],
    pub side_to: [f32; 2],
    /// Біт 0 — заокруглити початок, біт 1 — кінець; біт 2 (`WALL`) — лінія стінки: такі
    /// лінії лежать стосом по висоті, тож здалеку поверхня — бік валика, а не верх.
    pub caps: u32,
}

impl ToolpathInstance {
    pub const CAP_START: u32 = 1;
    pub const CAP_END: u32 = 2;
    pub const WALL: u32 = 4;
    /// Маркер (шов, втягування): куля радіуса `width / 2` у точці `from`, завжди кругла
    /// до камери — вертикальна «пігулка» згори сплющувалась у риску.
    pub const DOT: u32 = 8;

    /// Маркер-куля діаметра `diameter` з центром у `center`.
    pub fn dot(center: Vec3, color: [f32; 4], diameter: f32) -> Self {
        Self {
            caps: Self::DOT,
            ..Self::new(center, center, color, diameter, diameter)
        }
    }

    /// Позначає лінію як стінку (периметр, спідниця): див. `WALL`.
    pub fn wall(mut self, wall: bool) -> Self {
        if wall {
            self.caps |= Self::WALL;
        }
        self
    }

    pub fn new(from: Vec3, to: Vec3, color: [f32; 4], width: f32, height: f32) -> Self {
        let side = side_of(from, to);
        Self {
            from: from.to_array(),
            to: to.to_array(),
            color,
            width,
            height,
            side_from: side,
            side_to: side,
            caps: Self::CAP_START | Self::CAP_END,
        }
    }

    /// Зшиває послідовні лінії: якщо наступна починається там, де закінчилась попередня,
    /// з тим самим перерізом і поворотом до 60°, стик стає спільним косим перерізом;
    /// різкіші повороти й обриви отримують заокруглені торці.
    pub fn join(instances: &mut [Self]) {
        for i in 1..instances.len() {
            let (a, b) = (instances[i - 1], instances[i]);
            let contiguous = Vec3::from(a.to).distance_squared(Vec3::from(b.from)) < 1e-6
                && (a.width - b.width).abs() < 0.02
                && (a.height - b.height).abs() < 0.02;
            let (sa, sb) = (glam::Vec2::from(a.side_to), glam::Vec2::from(b.side_from));
            let cos = sa.dot(sb);
            if !contiguous || cos < 0.5 || sa == glam::Vec2::ZERO || sb == glam::Vec2::ZERO {
                continue;
            }
            let bisector = (sa + sb).normalize_or_zero();
            let miter = bisector / bisector.dot(sa).max(0.5);
            instances[i - 1].side_to = miter.to_array();
            instances[i - 1].caps &= !Self::CAP_END;
            instances[i].side_from = miter.to_array();
            instances[i].caps &= !Self::CAP_START;
        }
    }
}

/// Упакований для GPU інстанс (11 слів = 44 байти замість 68): позиції — f32, колір —
/// unorm8×4, ширина й висота — f16, бічні вектори — snorm16 з масштабом
/// [`GpuInstance::SIDE_SCALE`] (косий стик подовжує їх до 2). Розпаковує `vs_toolpath`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable, PartialEq)]
pub struct GpuInstance {
    pub from: [f32; 3],
    pub to: [f32; 3],
    pub color: u32,
    pub size: u32,
    pub side_from: u32,
    pub side_to: u32,
    pub caps: u32,
}

impl GpuInstance {
    /// |side| ≤ 2 (див. `ToolpathInstance::join`): у snorm16 кладемо side / 2.
    pub const SIDE_SCALE: f32 = 2.0;
    /// Слів u32 на інстанс — те саме число в `shader.wgsl`.
    pub const WORDS: u32 = (size_of::<Self>() / 4) as u32;

    pub fn pack(i: &ToolpathInstance) -> Self {
        let unorm8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
        let snorm16 = |v: f32| {
            (((v / Self::SIDE_SCALE).clamp(-1.0, 1.0) * 32767.0).round() as i32 as u32) & 0xffff
        };
        let side = |s: [f32; 2]| snorm16(s[0]) | (snorm16(s[1]) << 16);
        Self {
            from: i.from,
            to: i.to,
            color: unorm8(i.color[0])
                | (unorm8(i.color[1]) << 8)
                | (unorm8(i.color[2]) << 16)
                | (unorm8(i.color[3]) << 24),
            size: u32::from(f16_bits(i.width)) | (u32::from(f16_bits(i.height)) << 16),
            side_from: side(i.side_from),
            side_to: side(i.side_to),
            caps: i.caps,
        }
    }
}

/// f32 → біти IEEE half (до найближчого; піднормальні → 0, переповнення → max). Ширини й
/// висоти валиків 0.05–3 мм: відносна похибка ≤ 2⁻¹¹.
fn f16_bits(v: f32) -> u16 {
    let bits = v.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    if v.is_nan() {
        return sign | 0x7e00;
    }
    if exp <= 0 {
        return sign;
    }
    if exp >= 31 {
        return sign | 0x7bff;
    }
    let mant = bits & 0x7f_ffff;
    let mut half = ((exp as u32) << 10) | (mant >> 13);
    // Округлення до найближчого (до парного на половині).
    let rest = mant & 0x1fff;
    if rest > 0x1000 || (rest == 0x1000 && half & 1 == 1) {
        half += 1;
    }
    sign | (half.min(0x7bff) as u16)
}

/// Одиничний бічний вектор лінії в площині XY (праворуч від руху); для вертикальних рухів — нуль.
fn side_of(from: Vec3, to: Vec3) -> [f32; 2] {
    let d = (to - from).truncate();
    let len = d.length();
    if len < 1e-5 {
        [0.0, 0.0]
    } else {
        [d.y / len, -d.x / len]
    }
}

/// Спрощена копія траєкторії для загального плану: послідовні лінії одного валика,
/// що відхиляються від спільної хорди не більше ніж на `tolerance` мм, злиті в одну.
/// Шари не змішуються; інстанс `i` замінює повні інстанси `cover[i]..cover[i + 1]`.
#[derive(Clone)]
pub struct ToolpathLod {
    pub buffer: wgpu::Buffer,
    pub count: u32,
    pub cover: std::sync::Arc<[u32]>,
    pub tolerance: f32,
}

impl ToolpathLod {
    /// Видимий діапазон повних інстансів `range` → (голова з повного буфера, середина зі
    /// спрощеного, хвіст з повного). Інстанс LOD i замінює повні cover[i]..cover[i + 1];
    /// беремо лише ті, що строго всередині, тож голова й хвіст непорожні й містять межі
    /// повзунків (заокруглені торці, точна позиція сопла). `None` — малювати лише повний.
    pub fn split(&self, range: std::ops::Range<u32>) -> Option<[std::ops::Range<u32>; 3]> {
        lod_split(&self.cover, range)
    }
}

fn lod_split(cover: &[u32], range: std::ops::Range<u32>) -> Option<[std::ops::Range<u32>; 3]> {
    let count = cover.len() - 1;
    let la = cover[..count].partition_point(|&c| c <= range.start);
    let lb = cover[1..].partition_point(|&c| c < range.end);
    (la < lb).then(|| [range.start..cover[la], la as u32..lb as u32, cover[lb]..range.end])
}

/// Допуск LOD, мм: на загальному плані (піксель ≥ 2 допуски) похибка менша за пів пікселя.
pub const LOD_TOLERANCE: f32 = 0.05;
/// Довша хорда помітно зрізала б кут на стику навіть у межах допуску.
const LOD_MAX_LEN: f32 = 6.0;
const LOD_MAX_RUN: usize = 48;

/// Жадібне злиття сусідніх інстансів у хорди; повертає інстанси й `cover` (довжина n + 1).
pub fn simplify(
    instances: &[ToolpathInstance],
    layer_ranges: &[std::ops::Range<u32>],
    tolerance: f32,
) -> (Vec<ToolpathInstance>, Vec<u32>) {
    let n = instances.len();
    let mut layer_start = vec![false; n + 1];
    for r in layer_ranges {
        if let Some(flag) = layer_start.get_mut(r.start as usize) {
            *flag = true;
        }
    }
    let compatible = |a: &ToolpathInstance, b: &ToolpathInstance| {
        Vec3::from(a.to).distance_squared(Vec3::from(b.from)) < 1e-6
            && (a.width - b.width).abs() < 0.02
            && (a.height - b.height).abs() < 0.02
            && a.color == b.color
            && (a.caps & ToolpathInstance::WALL) == (b.caps & ToolpathInstance::WALL)
            && (a.caps | b.caps) & ToolpathInstance::DOT == 0
    };
    let tol2 = tolerance * tolerance;
    let mut out = Vec::with_capacity(n / 2);
    let mut cover = Vec::with_capacity(n / 2 + 1);
    let mut i = 0;
    while i < n {
        let first = &instances[i];
        let a = Vec3::from(first.from);
        let mut j = i;
        while j + 1 < n
            && j + 1 - i < LOD_MAX_RUN
            && !layer_start[j + 1]
            && compatible(&instances[j], &instances[j + 1])
        {
            let end = Vec3::from(instances[j + 1].to);
            if a.distance(end) > LOD_MAX_LEN
                || !(i..=j)
                    .all(|k| distance_to_segment_sq(Vec3::from(instances[k].to), a, end) <= tol2)
            {
                break;
            }
            j += 1;
        }
        if first.caps & ToolpathInstance::DOT != 0 {
            out.push(*first);
            cover.push(i as u32);
            i += 1;
            continue;
        }
        let wall = first.caps & ToolpathInstance::WALL != 0;
        let to = Vec3::from(instances[j].to);
        out.push(ToolpathInstance::new(a, to, first.color, first.width, first.height).wall(wall));
        cover.push(i as u32);
        i = j + 1;
    }
    cover.push(n as u32);
    ToolpathInstance::join(&mut out);
    (out, cover)
}

fn distance_to_segment_sq(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_squared().max(1e-12)).clamp(0.0, 1.0);
    (a + ab * t).distance_squared(p)
}

/// GPU-буфери та метадані готової траєкторії для рендерера.
#[derive(Clone)]
pub struct ToolpathGpuData {
    pub instance_buffer: wgpu::Buffer,
    pub total_instances: u32,
    /// Спрощена копія для загального плану; `None`, якщо злиття майже нічого не дає.
    pub lod: Option<ToolpathLod>,
    /// Діапазон інстансів для кожного шару (0..layers_count).
    pub layer_ranges: Vec<std::ops::Range<u32>>,
    /// Висота кожного шару (Z першого інстансу; порожній шар — як попередній): порядок
    /// малювання від шарів біля ока, див. `Renderer::toolpath_order`.
    pub layer_z: Vec<f32>,
    pub travel_buffer: Option<wgpu::Buffer>,
    pub travel_count: u32,
    pub travel_layer_ranges: Vec<std::ops::Range<u32>>,
    pub bounds: Option<(Vec3, Vec3)>,
}

impl ToolpathGpuData {
    pub fn new(
        device: &wgpu::Device,
        instances: &[ToolpathInstance],
        layer_ranges: Vec<std::ops::Range<u32>>,
        travel_vertices: Option<&[FlatVertex]>,
        travel_layer_ranges: Vec<std::ops::Range<u32>>,
        bounds: Option<(Vec3, Vec3)>,
    ) -> Self {
        let storage = |label, data: &[ToolpathInstance]| {
            let packed: Vec<GpuInstance> = data.iter().map(GpuInstance::pack).collect();
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(&packed),
                // Читається шейдером за vertex_index (див. `Renderer::set_toolpath`).
                usage: wgpu::BufferUsages::STORAGE,
            })
        };
        let instance_buffer = storage("toolpath_instances", instances);
        let (simple, cover) = simplify(instances, &layer_ranges, LOD_TOLERANCE);
        let lod = (simple.len() * 5 < instances.len() * 4).then(|| ToolpathLod {
            buffer: storage("toolpath_lod", &simple),
            count: simple.len() as u32,
            cover: cover.into(),
            tolerance: LOD_TOLERANCE,
        });

        let (travel_buffer, travel_count) = match travel_vertices {
            Some(verts) if !verts.is_empty() => {
                let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("toolpath_travel"),
                    contents: bytemuck::cast_slice(verts),
                    usage: wgpu::BufferUsages::VERTEX,
                });
                (Some(buf), verts.len() as u32)
            }
            _ => (None, 0),
        };

        let mut z = f32::NEG_INFINITY;
        let layer_z = layer_ranges
            .iter()
            .map(|r| {
                if let Some(first) = instances.get(r.start as usize).filter(|_| !r.is_empty()) {
                    z = first.to[2];
                }
                z
            })
            .collect();

        Self {
            instance_buffer,
            total_instances: instances.len() as u32,
            lod,
            layer_ranges,
            layer_z,
            travel_buffer,
            travel_count,
            travel_layer_ranges,
            bounds,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(points: &[[f32; 3]]) -> Vec<ToolpathInstance> {
        let mut out: Vec<_> = points
            .windows(2)
            .map(|w| {
                ToolpathInstance::new(w[0].into(), w[1].into(), [1.0; 4], 0.42, 0.2).wall(true)
            })
            .collect();
        ToolpathInstance::join(&mut out);
        out
    }

    #[test]
    fn simplify_merges_collinear_runs_and_keeps_layers_and_corners() {
        // Пряма з 10 відрізків, потім поворот на 90° і ще 5.
        let mut points: Vec<[f32; 3]> = (0..=10).map(|i| [i as f32 * 0.4, 0.0, 0.2]).collect();
        points.extend((1..=5).map(|i| [4.0, i as f32 * 0.4, 0.2]));
        let instances = line(&points);
        let (lod, cover) = simplify(&instances, &[0..instances.len() as u32], LOD_TOLERANCE);
        assert_eq!(lod.len(), 2, "дві хорди: до кута й після");
        assert_eq!(cover, vec![0, 10, 15]);
        assert_eq!(lod[0].to, [4.0, 0.0, 0.2]);
        assert!(lod[0].caps & ToolpathInstance::WALL != 0);

        // Межа шару посередині прямої не зливається.
        let (lod, cover) = simplify(&instances[..10], &[0..4, 4..10], LOD_TOLERANCE);
        assert_eq!(lod.len(), 2);
        assert_eq!(cover, vec![0, 4, 10]);
    }

    #[test]
    fn simplify_respects_tolerance_and_color() {
        // Коло r = 30 мм із хорд 0.4 мм: прогин хорди з k відрізків ≈ (0.4k)² / 240.
        let points: Vec<[f32; 3]> = (0..=40)
            .map(|i| {
                let a = i as f32 * 0.4 / 30.0;
                [30.0 * a.cos(), 30.0 * a.sin(), 0.2]
            })
            .collect();
        let instances = line(&points);
        let (lod, cover) = simplify(&instances, &[0..40], LOD_TOLERANCE);
        assert!(lod.len() < 20 && lod.len() > 4, "{} хорд", lod.len());
        for (i, chord) in lod.iter().enumerate() {
            for k in cover[i] as usize..cover[i + 1] as usize {
                let p = Vec3::from(instances[k].to);
                let d = distance_to_segment_sq(p, chord.from.into(), chord.to.into()).sqrt();
                assert!(d <= LOD_TOLERANCE + 1e-4, "відхилення {d}");
            }
        }

        let mut colored = line(&[[0.0, 0.0, 0.2], [0.4, 0.0, 0.2], [0.8, 0.0, 0.2]]);
        colored[1].color = [0.5, 0.5, 0.5, 1.0];
        assert_eq!(simplify(&colored, &[0..2], LOD_TOLERANCE).0.len(), 2);
    }

    #[test]
    fn gpu_instance_packs_into_44_bytes_without_visible_loss() {
        assert_eq!(size_of::<GpuInstance>(), 44);
        assert_eq!(GpuInstance::WORDS, 11);
        // 64 інстанси — ціле число вирівнювань 256 байт для зсуву прив’язки шматка.
        assert_eq!(64 * size_of::<GpuInstance>() % 256, 0);
        let mut i = ToolpathInstance::new(
            Vec3::new(1.5, 2.0, 0.2),
            Vec3::new(3.0, 2.0, 0.2),
            [0.2, 0.4, 0.6, 1.0],
            0.42,
            0.2,
        );
        i.side_to = [1.9, -1.2];
        let p = GpuInstance::pack(&i);
        assert_eq!(p.from, i.from);
        assert_eq!(p.color & 0xff, 51);
        assert_eq!(p.color >> 24, 255);
        let half = |b: u32| {
            let (e, m) = ((b >> 10) & 0x1f, b & 0x3ff);
            (1.0 + m as f32 / 1024.0) * 2f32.powi(e as i32 - 15)
        };
        assert!((half(p.size & 0xffff) - 0.42).abs() < 0.42 / 1000.0);
        assert!((half(p.size >> 16) - 0.2).abs() < 0.2 / 1000.0);
        let snorm = |b: u32| (b as u16 as i16) as f32 / 32767.0 * GpuInstance::SIDE_SCALE;
        assert!((snorm(p.side_to & 0xffff) - 1.9).abs() < 1e-4);
        assert!((snorm(p.side_to >> 16) + 1.2).abs() < 1e-4);
        assert!((snorm(p.side_from >> 16) - i.side_from[1]).abs() < 1e-4);
        assert_eq!(f16_bits(1.0), 0x3c00);
        assert_eq!(f16_bits(-2.0), 0xc000);
    }

    #[test]
    fn lod_split_keeps_exact_boundaries_in_full_detail() {
        let cover = [0, 10, 20, 30, 40];
        // Усе: голова — перший інстанс LOD, хвіст — останній.
        assert_eq!(lod_split(&cover, 0..40), Some([0..10, 1..3, 30..40]));
        // Повзунок рухів посеред інстансу LOD: хвіст із повного буфера закінчується точно там.
        assert_eq!(lod_split(&cover, 0..35), Some([0..10, 1..3, 30..35]));
        assert_eq!(lod_split(&cover, 5..25), Some([5..10, 1..2, 20..25]));
        assert_eq!(lod_split(&cover, 12..38), Some([12..20, 2..3, 30..38]));
        // Менше за один повний інстанс LOD усередині — лише повний буфер.
        assert_eq!(lod_split(&cover, 3..7), None);
        assert_eq!(lod_split(&cover, 10..20), None);
    }
}
