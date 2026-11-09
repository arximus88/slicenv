//! Валики режиму «Фото»: інстанси з номером слота філаменту замість кольору і
//! CPU-дзеркало профілю валика з `bead.wgsl` (для тестів і рамок).
//!
//! Інстанс той самий, що у звичайному Preview (`ToolpathInstance`, 44 байти на GPU), тож
//! працюють той самий буфер шматками, LOD і повзунки. Відрізняється лише вміст кольору:
//! байт R — номер слота філаменту, байт G — прапорці [`FLAG_SUPPORT`], [`FLAG_SEAM`],
//! байти B і A — довжина нитки до початку сегмента ([`ALONG_PERIOD`]).

use glam::Vec3;

use crate::{FlatVertex, ToolpathInstance};

/// Підтримка або її інтерфейс: приглушується чи ховається перемикачем.
pub const FLAG_SUPPORT: u32 = 1;
/// Шов зовнішньої стінки: короткий валик-«горбик» тим самим матеріалом.
pub const FLAG_SEAM: u32 = 2;

/// Довжина нитки (сума довжин ліній екструзії від початку друку) зберігається за модулем
/// цього періоду, мм, з кроком `ALONG_PERIOD / 65536` (≈ 0.001 мм): візерунок, що сидить у
/// нитці (вкраплення Marble), неперервний уздовж лінії через стики сегментів. Розрив — раз
/// на 64 мм нитки.
pub const ALONG_PERIOD: f32 = 64.0;

/// Колір інстансу «Фото»: слот і прапорці в байтах unorm8 (див. `GpuInstance::pack`), у
/// байтах B і A — довжина нитки `along` до початку сегмента (16 біт, [`ALONG_PERIOD`]).
pub fn encode(slot: u8, flags: u32, along: f32) -> [f32; 4] {
    let q = ((along.rem_euclid(ALONG_PERIOD) / ALONG_PERIOD * 65536.0) as u32).min(65535);
    [
        f32::from(slot) / 255.0,
        flags as f32 / 255.0,
        (q & 0xff) as f32 / 255.0,
        (q >> 8) as f32 / 255.0,
    ]
}

/// Зворотне до [`encode`]: (слот, прапорці).
pub fn decode(color: [f32; 4]) -> (u8, u32) {
    (
        (color[0] * 255.0).round() as u8,
        (color[1] * 255.0).round() as u32,
    )
}

/// CPU-частина побудови: інстанси й відповідність сегментам, як у `ToolpathGpuExt::build`.
#[derive(Default)]
pub struct PhotoInstances {
    pub instances: Vec<ToolpathInstance>,
    pub instance_segment: Vec<u32>,
    pub layer_ranges: Vec<std::ops::Range<u32>>,
    pub travel_vertices: Vec<FlatVertex>,
    pub travel_segment: Vec<u32>,
    pub travel_layer_ranges: Vec<std::ops::Range<u32>>,
}

fn is_support(role: gcode::Role) -> bool {
    matches!(
        role,
        gcode::Role::SupportMaterial
            | gcode::Role::SupportMaterialInterface
            | gcode::Role::SupportTransition
    )
}

fn is_wall(role: gcode::Role) -> bool {
    matches!(
        role,
        gcode::Role::Perimeter
            | gcode::Role::ExternalPerimeter
            | gcode::Role::OverhangPerimeter
            | gcode::Role::Skirt
    )
}

/// Усі лінії екструзії (без витирань), шви як короткі валики, переміщення — лінії.
/// Видимість ролей не застосовується: підтримки ховає шейдер за прапорцем, без перебудови.
pub fn instances(toolpath: &gcode::Toolpath) -> PhotoInstances {
    let mut out = PhotoInstances {
        instances: Vec::with_capacity(toolpath.segments.len()),
        instance_segment: Vec::with_capacity(toolpath.segments.len()),
        ..Default::default()
    };
    let mut markers = toolpath
        .markers
        .iter()
        .filter(|m| m.kind == gcode::Role::Seam)
        .peekable();
    // Слот останньої лінії екструзії: шов належить філаменту стінки, яку він замикає.
    let mut last_slot = 0u8;
    // Довжина нитки від початку друку (f64: на великих друках — кілометри).
    let mut along = 0f64;
    for layer in &toolpath.layers {
        let start_inst = out.instances.len() as u32;
        let start_travel = out.travel_vertices.len() as u32;
        for (offset, seg) in toolpath.segments[layer.segment_range.clone()].iter().enumerate() {
            let index = (layer.segment_range.start + offset) as u32;
            if seg.role == gcode::Role::Wipe {
                // Витирання не лишає пластику — у фото його немає.
            } else if seg.is_extrusion {
                let flags = if is_support(seg.role) { FLAG_SUPPORT } else { 0 };
                last_slot = seg.tool;
                let start = (along % f64::from(ALONG_PERIOD)) as f32;
                along += f64::from(seg.length());
                out.instances.push(
                    ToolpathInstance::new(
                        seg.from,
                        seg.to,
                        encode(seg.tool, flags, start),
                        seg.width,
                        seg.height,
                    )
                    .wall(is_wall(seg.role)),
                );
                out.instance_segment.push(index);
            } else {
                let color = seg.role.color_linear();
                out.travel_vertices.push(FlatVertex {
                    pos: seg.from.to_array(),
                    color,
                });
                out.travel_vertices.push(FlatVertex {
                    pos: seg.to.to_array(),
                    color,
                });
                out.travel_segment.push(index);
            }
            while let Some(m) = markers.next_if(|m| m.segment <= index) {
                // Горбик шва: валик нульової довжини (еліпсоїд), трохи ширший і вищий за
                // стінку, тож виступає з неї, як справжній шов.
                let center = m.pos;
                let mut seam = ToolpathInstance::new(
                    center,
                    center,
                    encode(last_slot, FLAG_SEAM, 0.0),
                    m.width.max(0.2) * 1.12,
                    m.height.max(0.08) * 1.08,
                );
                seam.caps = ToolpathInstance::CAP_START | ToolpathInstance::CAP_END;
                out.instances.push(seam);
                out.instance_segment.push(index);
            }
        }
        out.layer_ranges.push(start_inst..out.instances.len() as u32);
        out.travel_layer_ranges
            .push(start_travel..out.travel_vertices.len() as u32);
    }
    // Спільний косий стик сусідніх сегментів: шейдер обрізає ним циліндр валика (без
    // перекриття й щілини); вільні кінці — заокруглені.
    ToolpathInstance::join(&mut out.instances);
    out
}

/// У скільки разів переріз валика вищий за шар (`BEAD_OVERLAP` у bead.wgsl): сусідні шари
/// перекриваються, і між ними лишається мілка борозенка замість щілини.
pub const BEAD_OVERLAP: f32 = 1.35;

/// Точка й одинична зовнішня нормаль поверхні валика: `t` — 0…1 уздовж осі, `phi` — кут
/// у перерізі від бічного напрямку (0 — праворуч, π/2 — верх). Те саме, що в `bead.wgsl`:
/// переріз — еліпс ширина × (висота·[`BEAD_OVERLAP`]), верх на висоті сопла.
pub fn surface(inst: &ToolpathInstance, t: f32, phi: f32) -> (Vec3, Vec3) {
    let (from, to) = (Vec3::from(inst.from), Vec3::from(inst.to));
    let (side, up) = frame(inst);
    let (hw, hh) = (inst.width.max(0.02) * 0.5, inst.height.max(0.02) * 0.5 * BEAD_OVERLAP);
    let axis = from.lerp(to, t) - Vec3::Z * hh;
    let (s, c) = phi.sin_cos();
    let pos = axis + side * (c * hw) + up * (s * hh);
    let normal = (side * (c / hw) + up * (s / hh)).normalize();
    (pos, normal)
}

/// Глибина канавки між шарами стінки, частка піввисоти шару (`WALL_GROOVE` у photo.wgsl).
pub const WALL_GROOVE: f32 = 0.22;
/// Глибина канавки між лініями верхньої поверхні, частка півширини (`TOP_GROOVE`).
pub const TOP_GROOVE: f32 = 0.12;
/// Звідки (|u|) дно канавки заокруглюється (`PROFILE_FILLET`).
pub const PROFILE_FILLET: f32 = 0.78;

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Кут нормалі (рад) від нормалі поверхні в площині перерізу злитих валиків, як
/// `profile_angle` у photo.wgsl: `u` — положення поперек валика, −1…1 від стику до стику,
/// `depth` — глибина канавки в частках піввисоти. Дуга кола з такою глибиною, біля стику
/// кут плавно повертається до 0 (заокруглене дно канавки, однакова нормаль з обох боків).
pub fn profile_angle(u: f32, depth: f32) -> f32 {
    let r = (1.0 + depth * depth) / (2.0 * depth);
    let a = u.clamp(-1.0, 1.0).abs();
    let arc = (a.min(PROFILE_FILLET) / r).asin();
    let fillet = 1.0 - smoothstep(PROFILE_FILLET, 1.0, a);
    u.signum() * arc * fillet
}

/// Частка рельєфу за розміром валика на екрані в пікселях (`relief_lod`): дрібніше за
/// ~1.6 px — без рельєфу, від 7 px — повний.
pub fn relief_lod(bead_px: f32) -> f32 {
    smoothstep(1.6, 7.0, bead_px)
}

/// Бічний і верхній напрямки перерізу (як `bead_vertex`): бік — горизонтальний
/// перпендикуляр до осі (вертикальний рух — −Y), верх — z ≥ 0.
fn frame(inst: &ToolpathInstance) -> (Vec3, Vec3) {
    let (from, to) = (Vec3::from(inst.from), Vec3::from(inst.to));
    let dir = to - from;
    let fwd = if dir.length() > 1e-5 {
        dir.normalize()
    } else {
        Vec3::X
    };
    let side = fwd.cross(Vec3::Z);
    let side = if side.length_squared() < 1e-8 {
        Vec3::NEG_Y
    } else {
        side.normalize()
    };
    let up = side.cross(fwd).normalize();
    (side, if up.z < 0.0 { -up } else { up })
}

/// Рамка валика з урахуванням заокруглених торців (півширини за кінцем).
pub fn bounds(inst: &ToolpathInstance) -> (Vec3, Vec3) {
    let (from, to) = (Vec3::from(inst.from), Vec3::from(inst.to));
    let (hw, h) = (inst.width.max(0.02) * 0.5, inst.height.max(0.02) * BEAD_OVERLAP);
    let reach = Vec3::new(hw, hw, 0.0);
    let (lo, hi) = (from.min(to) - reach, from.max(to) + reach);
    (Vec3::new(lo.x, lo.y, lo.z - h), Vec3::new(hi.x, hi.y, hi.z))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> gcode::Toolpath {
        gcode::parse(text).unwrap()
    }

    #[test]
    fn bead_surface_lies_in_bounds_with_outward_unit_normals() {
        let inst = ToolpathInstance::new(
            Vec3::new(10.0, 5.0, 0.4),
            Vec3::new(20.0, 5.0, 0.4),
            encode(0, 0, 0.0),
            0.42,
            0.2,
        );
        let (lo, hi) = bounds(&inst);
        let (mut min, mut max) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
        for ti in 0..=8 {
            for pi in 0..64 {
                let (t, phi) = (ti as f32 / 8.0, pi as f32 / 64.0 * std::f32::consts::TAU);
                let (p, n) = surface(&inst, t, phi);
                assert!((n.length() - 1.0).abs() < 1e-5);
                let axis = Vec3::new(10.0 + 10.0 * t, 5.0, 0.4 - 0.1 * BEAD_OVERLAP);
                assert!(n.dot(p - axis) > 0.0, "нормаль усередину при φ={phi}");
                // Уздовж руху нормаль перпендикулярна осі.
                assert!(n.x.abs() < 1e-5);
                assert!(p.cmpge(lo - 1e-4).all() && p.cmple(hi + 1e-4).all(), "{p} поза {lo}…{hi}");
                min = min.min(p);
                max = max.max(p);
            }
        }
        // Переріз — еліпс 0.42 × 0.2·k мм, верх на висоті сопла, низ заходить у шар нижче.
        assert!((max.z - 0.4).abs() < 1e-4 && (min.z - (0.4 - 0.2 * BEAD_OVERLAP)).abs() < 1e-4);
        assert!((max.y - min.y - 0.42).abs() < 1e-3);
        // Верх валика дивиться вгору, бік — убік.
        let (_, top) = surface(&inst, 0.5, std::f32::consts::FRAC_PI_2);
        assert!(top.z > 0.999);
        let (_, side) = surface(&inst, 0.5, 0.0);
        assert!(side.z.abs() < 1e-5 && side.y.abs() > 0.999);
        // Рамка включає заокруглені торці.
        assert!((lo.x - 9.79).abs() < 1e-4 && (hi.x - 20.21).abs() < 1e-4);
    }

    #[test]
    fn bead_profile_is_a_shallow_groove_not_a_tube() {
        let deg = |x: f32| x.to_degrees();
        let samples: Vec<f32> = (0..=400).map(|i| -1.0 + i as f32 / 200.0).collect();
        for depth in [WALL_GROOVE, TOP_GROOVE] {
            let angles: Vec<f32> = samples.iter().map(|&u| profile_angle(u, depth)).collect();
            // Непарна: верх валика дивиться вгору, низ — униз, середина — прямо.
            for (&u, &a) in samples.iter().zip(&angles) {
                assert!((a + profile_angle(-u, depth)).abs() < 1e-6);
            }
            assert!(profile_angle(0.0, depth).abs() < 1e-6);
            // На стику нормаль з обох боків та сама (0) — без розриву між шарами.
            assert!(profile_angle(1.0, depth).abs() < 1e-6 && profile_angle(-1.0, depth).abs() < 1e-6);
            // Пологий схил, а не трубка: «трубка» дала б 90° на стику.
            let max = angles.iter().fold(0.0f32, |m, a| m.max(a.abs()));
            assert!(deg(max) < 20.0 && deg(max) > 5.0, "{depth}: {}", deg(max));
            // Без стрибків: сусідні вибірки (крок 0.005) відрізняються менше ніж на 1.5°.
            assert!(angles.windows(2).all(|w| deg((w[1] - w[0]).abs()) < 1.5));
        }
        // Стінка глибша за верхню поверхню (лінії зверху розплющені соплом).
        assert!(profile_angle(0.7, WALL_GROOVE) > profile_angle(0.7, TOP_GROOVE));
        // LOD: шар на 1–1.5 px — без рельєфу (інакше муар), від 7 px — повний, монотонно.
        assert_eq!(relief_lod(1.0), 0.0);
        assert_eq!(relief_lod(1.5), 0.0);
        assert_eq!(relief_lod(8.0), 1.0);
        let lods: Vec<f32> = (0..100).map(|i| relief_lod(i as f32 * 0.1)).collect();
        assert!(lods.windows(2).all(|w| w[1] >= w[0]));
    }

    #[test]
    fn photo_instances_carry_slot_support_and_seam_and_skip_wipes() {
        let t = parse(concat!(
            "; CHANGE_LAYER\n; Z_HEIGHT: 0.2\n; LAYER_HEIGHT: 0.2\nG1 Z0.2\n",
            "; FEATURE: Outer wall\n; LINE_WIDTH: 0.42\n",
            "G1 X10 E1\nG1 Y10 E2\nG1 X0 E3\nG1 Y0 E4\n",
            "; WIPE_START\nG1 X5 E3.5\n; WIPE_END\n",
            "T1\n; FEATURE: Support\nG1 X20 Y20 E5\n",
            "G0 X30\n",
        ));
        let built = instances(&t);
        assert_eq!(built.instances.len(), built.instance_segment.len());
        assert!(built.instance_segment.windows(2).all(|w| w[0] <= w[1]));
        let decoded: Vec<_> = built.instances.iter().map(|i| decode(i.color)).collect();
        // Чотири стінки слота 0, підтримка слота 1 з прапорцем.
        assert_eq!(decoded.iter().filter(|d| **d == (0, 0)).count(), 4);
        assert!(decoded.contains(&(1, FLAG_SUPPORT)));
        // Шов є (якщо парсер його позначив) — і він того ж слота, що й стінка.
        let seams = decoded.iter().filter(|d| d.1 == FLAG_SEAM).count();
        let marked = t.markers.iter().filter(|m| m.kind == gcode::Role::Seam).count();
        assert_eq!(seams, marked);
        assert!(decoded.iter().filter(|d| d.1 == FLAG_SEAM).all(|d| d.0 == 0));
        // Витирання немає: жодного інстансу ширини 0.12.
        assert!(built.instances.iter().all(|i| (i.width - 0.12).abs() > 1e-3));
        // Переміщення — пари вершин.
        assert_eq!(built.travel_vertices.len(), built.travel_segment.len() * 2);
        assert_eq!(built.layer_ranges.len(), t.layers.len());
        // Рамка всіх валиків — від низу першого шару до верху.
        let (lo, hi) = built.instances.iter().map(bounds).fold(
            (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
            |(a, b), (l, h)| (a.min(l), b.max(h)),
        );
        // Низ — шар нижче з перекриттям (горбик шва на 8 % вищий — ще трохи нижче).
        let (bottom, seam_bottom) = (0.2 - 0.2 * BEAD_OVERLAP, 0.2 - 0.2 * 1.08 * BEAD_OVERLAP);
        assert!(lo.z <= bottom + 1e-3 && lo.z >= seam_bottom - 1e-3, "{lo} {hi}");
        assert!((hi.z - 0.2).abs() < 0.03, "{lo} {hi}");
        assert!(lo.x < 0.0 && hi.x > 20.0);
    }

    #[test]
    fn encode_roundtrips_through_gpu_packing() {
        for (slot, flags) in [(0u8, 0u32), (3, FLAG_SUPPORT), (31, FLAG_SEAM | FLAG_SUPPORT)] {
            let i = ToolpathInstance::new(Vec3::ZERO, Vec3::X, encode(slot, flags, 0.0), 0.4, 0.2);
            let packed = crate::toolpath::GpuInstance::pack(&i);
            assert_eq!(packed.color & 0xff, u32::from(slot));
            assert_eq!((packed.color >> 8) & 0xff, flags);
            assert_eq!(decode(i.color), (slot, flags));
        }
    }

    #[test]
    fn along_is_filament_length_continuous_across_segments() {
        // Байти B і A — довжина нитки до початку сегмента (за модулем ALONG_PERIOD).
        let along = |i: &ToolpathInstance| {
            let p = crate::toolpath::GpuInstance::pack(i);
            (p.color >> 16) as f32 * ALONG_PERIOD / 65536.0
        };
        let i = ToolpathInstance::new(Vec3::ZERO, Vec3::X, encode(2, FLAG_SUPPORT, 12.345), 0.4, 0.2);
        assert!((along(&i) - 12.345).abs() < 2e-3, "{}", along(&i));
        assert_eq!(decode(i.color), (2, FLAG_SUPPORT));
        assert!((along(&ToolpathInstance::new(Vec3::ZERO, Vec3::X, encode(0, 0, ALONG_PERIOD + 1.5), 0.4, 0.2)) - 1.5).abs() < 2e-3);

        let t = parse(concat!(
            "; CHANGE_LAYER\n; Z_HEIGHT: 0.2\n; LAYER_HEIGHT: 0.2\nG1 Z0.2\n",
            "; FEATURE: Outer wall\n; LINE_WIDTH: 0.42\n",
            "G1 X10 E1\nG1 Y10 E2\nG1 X0 E3\n",
        ));
        let walls: Vec<_> = instances(&t).instances.into_iter().filter(|i| decode(i.color).1 == 0).collect();
        assert_eq!(walls.len(), 3);
        // Кожен наступний сегмент починається там, де закінчилась нитка попереднього.
        for w in walls.windows(2) {
            let len = (Vec3::from(w[0].to) - Vec3::from(w[0].from)).length();
            assert!((along(&w[1]) - (along(&w[0]) + len).rem_euclid(ALONG_PERIOD)).abs() < 2e-3);
        }
    }
}
