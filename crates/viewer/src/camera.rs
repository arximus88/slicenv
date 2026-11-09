use glam::{Mat4, Vec3};

use crate::BED_SIZE;

const FOV_Y_DEG: f32 = 35.0;
/// Майже вертикально: точно 90° зробило б напрямок «вгору» камери невизначеним.
const PITCH_LIMIT: f32 = 1.5603; // 89.4°

/// Орбітальна камера навколо точки на столі. Вісь Z — вгору, як у слайсерах.
#[derive(Clone, Debug)]
pub struct Camera {
    pub target: Vec3,
    /// Кут навколо Z, радіани.
    pub yaw: f32,
    /// Кут над площиною столу, радіани.
    pub pitch: f32,
    pub distance: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            target: Vec3::new(BED_SIZE / 2.0, BED_SIZE / 2.0, 0.0),
            yaw: -100f32.to_radians(),
            pitch: 35f32.to_radians(),
            distance: 420.0,
        }
    }
}

impl Camera {
    /// Наводить камеру так, щоб у кадр влізли і стіл, і модель; кут огляду не змінює.
    pub fn frame(&mut self, model: Option<(Vec3, Vec3)>) {
        let (mut lo, mut hi) = (Vec3::ZERO, Vec3::new(BED_SIZE, BED_SIZE, 0.0));
        if let Some((min, max)) = model {
            lo = lo.min(min);
            hi = hi.max(max);
        }
        self.target = (lo + hi) / 2.0;
        // Сфера навколо бокса — завищена оцінка; 0.73 підібрано так, щоб порожній стіл
        // займав кадр так само, як у Bambu Studio.
        let radius = (hi - lo).length() / 2.0;
        self.distance = radius / (FOV_Y_DEG.to_radians() / 2.0).tan() * 0.73;
    }

    /// Fit every corner with the actual camera orientation and viewport aspect.
    pub fn frame_scene(&mut self, lo: Vec3, hi: Vec3, aspect: f32) {
        self.target = (lo + hi) * 0.5;
        let forward = (self.target - self.eye()).normalize_or_zero();
        let right = forward.cross(Vec3::Z).normalize_or_zero();
        let up = right.cross(forward);
        let tangent = (FOV_Y_DEG.to_radians() * 0.5).tan();
        let mut distance = 5.0f32;
        for i in 0..8 {
            let point = Vec3::new(
                if i & 1 == 0 { lo.x } else { hi.x },
                if i & 2 == 0 { lo.y } else { hi.y },
                if i & 4 == 0 { lo.z } else { hi.z },
            ) - self.target;
            distance = distance
                .max(point.dot(right).abs() / (tangent * aspect.max(0.1)) - point.dot(forward));
            distance = distance.max(point.dot(up).abs() / tangent - point.dot(forward));
        }
        self.distance = distance * 1.12;
    }

    /// Наводить камеру лише на рамку (без столу) — «вписати вибране».
    pub fn frame_tight(&mut self, lo: Vec3, hi: Vec3) {
        self.target = (lo + hi) / 2.0;
        let radius = ((hi - lo).length() / 2.0).max(5.0);
        self.distance = radius / (FOV_Y_DEG.to_radians() / 2.0).tan() * 1.15;
    }

    /// Обертання навколо точки огляду; можна заглянути й під стіл.
    pub fn orbit(&mut self, dx: f32, dy: f32) {
        self.yaw -= dx * 0.008;
        self.pitch = (self.pitch + dy * 0.008).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }

    /// Готовий ракурс (градуси); кут над столом обмежено, щоб «згори» не вироджувалось.
    pub fn set_angles(&mut self, yaw_deg: f32, pitch_deg: f32) {
        self.yaw = yaw_deg.to_radians();
        self.pitch = pitch_deg.to_radians().clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }

    /// Зум до точки під курсором: вона лишається на місці, а камера наближається до неї.
    pub fn zoom_toward(&mut self, point: Vec3, amount: f32) {
        let before = self.distance;
        self.zoom(amount);
        let k = 1.0 - self.distance / before;
        self.target += (point - self.target) * k;
    }

    /// Зсув точки огляду в площині екрана (права/середня кнопка миші).
    pub fn pan(&mut self, dx: f32, dy: f32) {
        let forward = (self.target - self.eye()).normalize_or_zero();
        let right = forward.cross(Vec3::Z).normalize_or_zero();
        let up = right.cross(forward);
        let scale = self.distance * 0.0012;
        self.target += (-right * dx + up * dy) * scale;
    }

    /// `amount` > 0 — наблизити.
    pub fn zoom(&mut self, amount: f32) {
        self.distance = (self.distance * (-amount * 0.0015).exp()).clamp(5.0, 2000.0);
    }

    /// Розмір пікселя (мм) на відстані `distance` від ока для кадру висотою `height_px`.
    pub fn pixel_size(&self, distance: f32, height_px: u32) -> f32 {
        2.0 * distance * (FOV_Y_DEG.to_radians() * 0.5).tan() / height_px.max(1) as f32
    }

    pub fn eye(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        self.target + self.distance * Vec3::new(cp * cy, cp * sy, sp)
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        let view = Mat4::look_at_rh(self.eye(), self.target, Vec3::Z);
        // Ближня площина росте з відстанню: зблизька видно окремі лінії, здалеку глибина точна.
        let near = (self.distance * 0.01).clamp(0.05, 2.0);
        let proj = Mat4::perspective_rh(FOV_Y_DEG.to_radians(), aspect, 5000.0, near);
        proj * view
    }

    /// Промінь зі світу камери через точку кадру: `ndc` у [-1, 1], Y догори.
    pub fn ray(&self, ndc: glam::Vec2, aspect: f32) -> (Vec3, Vec3) {
        let inv = self.view_proj(aspect).inverse();
        let near = inv.project_point3(Vec3::new(ndc.x, ndc.y, 1.0));
        let far = inv.project_point3(Vec3::new(ndc.x, ndc.y, 0.0));
        (near, (far - near).normalize_or_zero())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_fit_contains_every_plate_corner_in_narrow_and_wide_viewports() {
        let lo = Vec3::new(0., -307.2, 0.);
        let hi = Vec3::new(563.2, 256., 100.);
        for aspect in [0.65, 1., 1.6] {
            let mut camera = Camera::default();
            camera.frame_scene(lo, hi, aspect);
            for i in 0..8 {
                let p = Vec3::new(
                    if i & 1 == 0 { lo.x } else { hi.x },
                    if i & 2 == 0 { lo.y } else { hi.y },
                    if i & 4 == 0 { lo.z } else { hi.z },
                );
                let projected = camera.view_proj(aspect).project_point3(p);
                assert!(projected.x.abs() < 1. && projected.y.abs() < 1.);
            }
        }
    }

    #[test]
    fn reversed_depth_and_picking_agree() {
        for distance in [5.0, 100.0, 500.0, 2000.0] {
            let camera = Camera {
                distance,
                ..Camera::default()
            };
            let matrix = camera.view_proj(1.6);
            let direction = (camera.target - camera.eye()).normalize();
            let near = matrix.project_point3(camera.eye() + direction * 3.0);
            let far = matrix.project_point3(camera.eye() + direction * 3000.0);
            assert!(near.z > far.z && near.z <= 1.0 && far.z >= 0.0);
            let (origin, ray) = camera.ray(glam::Vec2::ZERO, 1.6);
            assert!(ray.dot(direction) > 0.9999);
            assert!((camera.target - origin).cross(ray).length() < 0.1);
        }
    }
}
