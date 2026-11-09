use std::f32::consts::PI;

use glam::Vec3;

use crate::model::Segment;

/// Теселює дугу G2/G3 у ламану лінію із сегментів. `proto` дає властивості руху;
/// його `extruded` (подача на всю дугу) ділиться між відрізками пропорційно довжині.
pub fn tessellate_arc(
    start: Vec3,
    end: Vec3,
    center_offset: Option<(f32, f32)>,
    radius_arg: Option<f32>,
    clockwise: bool,
    proto: Segment,
    output: &mut Vec<Segment>,
) {
    let (center_x, center_y, radius) = if let Some((i, j)) = center_offset {
        let r = (i * i + j * j).sqrt();
        if r < 1e-6 {
            return;
        }
        (start.x + i, start.y + j, r)
    } else if let Some(r) = radius_arg {
        let r_abs = r.abs();
        let dx = end.x - start.x;
        let dy = end.y - start.y;
        let d = (dx * dx + dy * dy).sqrt();
        if d < 1e-6 || d > 2.0 * r_abs {
            return;
        }
        let h = (r_abs * r_abs - (d * 0.5) * (d * 0.5)).max(0.0).sqrt();
        let mx = (start.x + end.x) * 0.5;
        let my = (start.y + end.y) * 0.5;
        let ux = dx / d;
        let uy = dy / d;
        // Нормаль: ліворуч чи праворуч залежно від знаку радіуса та напрямку дуги
        let factor = if (!clockwise && r > 0.0) || (clockwise && r < 0.0) {
            1.0
        } else {
            -1.0
        };
        (mx + factor * h * (-uy), my + factor * h * ux, r_abs)
    } else {
        return;
    };

    let v0_x = start.x - center_x;
    let v0_y = start.y - center_y;
    let v1_x = end.x - center_x;
    let v1_y = end.y - center_y;

    let theta0 = v0_y.atan2(v0_x);
    let theta1 = v1_y.atan2(v1_x);

    let is_full_circle = (start.x - end.x).abs() < 1e-4 && (start.y - end.y).abs() < 1e-4;

    let delta_theta = if is_full_circle {
        if clockwise { -2.0 * PI } else { 2.0 * PI }
    } else {
        let diff = theta1 - theta0;
        if clockwise {
            if diff > 0.0 { diff - 2.0 * PI } else { diff }
        } else {
            if diff < 0.0 { diff + 2.0 * PI } else { diff }
        }
    };

    let arc_length_xy = radius * delta_theta.abs();
    if arc_length_xy < 1e-4 && (start.z - end.z).abs() < 1e-4 {
        return;
    }

    // Крок розбиття: приблизно 0.5 мм або до 6 градусів на сегмент
    let steps_by_len = (arc_length_xy / 0.5).ceil();
    let steps_by_ang = (delta_theta.abs() / 0.1).ceil();
    let steps = (steps_by_len.max(steps_by_ang) as usize).clamp(1, 128);

    let mut prev = start;
    for step in 1..=steps {
        let t = step as f32 / steps as f32;
        let curr = if step == steps {
            end
        } else {
            let theta = theta0 + t * delta_theta;
            Vec3::new(
                center_x + radius * theta.cos(),
                center_y + radius * theta.sin(),
                start.z + t * (end.z - start.z),
            )
        };

        if (curr - prev).length_squared() > 1e-8 {
            output.push(Segment {
                from: prev,
                to: curr,
                extruded: proto.extruded / steps as f32,
                ..proto
            });
            prev = curr;
        }
    }
}
