//! Гізмо сцени (як у Bambu Studio): стрілки переміщення, кільця повороту, ручки масштабу,
//! маркер сопла «Перегляду». Геометрія будується на CPU щокадру й малюється поверх сцени;
//! розмір стрілок — сталий на екрані, тож гізмо не зникає при віддаленні.

use glam::{Vec2, Vec3};

use crate::{Camera, FlatVertex};

/// Кольори осей Zed (X червона, Y зелена, Z синя), sRGB.
pub const AXIS_COLORS: [[f32; 3]; 3] = [
    [0.816, 0.447, 0.467],
    [0.631, 0.757, 0.506],
    [0.455, 0.678, 0.910],
];
const HOVER: [f32; 3] = [0.871, 0.757, 0.518];
/// Стрілка зсуву площини різання — акцент Zed.
const CUT_ARROW: [f32; 3] = [0.455, 0.678, 0.910];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GizmoKind {
    Move,
    Rotate,
    Scale,
    /// Площина різання: кільця повороту навколо осей світу й стрілка вздовж її нормалі.
    Cut,
}

/// Ручка гізмо під курсором.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    /// Переміщення вздовж осі 0/1/2.
    Move(usize),
    /// Поворот навколо осі.
    Rotate(usize),
    /// Масштаб уздовж осі; `bool` — додатний бік.
    Scale(usize, bool),
    /// Пропорційний масштаб (кутова ручка).
    ScaleUniform(usize),
    /// Зсув площини різання вздовж її нормалі.
    CutMove,
}

/// Гізмо для рамки обʼєкта `lo..hi`.
#[derive(Clone, Copy, Debug)]
pub struct Gizmo {
    pub kind: GizmoKind,
    pub lo: Vec3,
    pub hi: Vec3,
    /// Світовий розмір, що на екрані виглядає як ~90 px.
    pub size: f32,
    /// Scale handles follow the target's local axes, center, and half extents.
    pub scale_frame: Option<([Vec3; 3], Vec3, Vec3)>,
    /// Нормаль площини різання (одинична) для `GizmoKind::Cut`.
    pub cut_normal: Vec3,
}

/// Перетворення світ → пікселі кадру (Y донизу).
pub struct Screen<'a> {
    pub camera: &'a Camera,
    pub width: f32,
    pub height: f32,
}

impl Screen<'_> {
    pub fn project(&self, p: Vec3) -> Option<Vec2> {
        let clip = self.camera.view_proj(self.width / self.height) * p.extend(1.0);
        if clip.w <= 1e-4 {
            return None;
        }
        let ndc = clip.truncate() / clip.w;
        Some(Vec2::new(
            (ndc.x + 1.0) * 0.5 * self.width,
            (1.0 - ndc.y) * 0.5 * self.height,
        ))
    }

    /// Світова довжина, що біля точки `p` займає `pixels` на екрані.
    pub fn world_size(&self, p: Vec3, pixels: f32) -> f32 {
        let distance = (self.camera.eye() - p).length();
        let fov = 35f32.to_radians();
        distance * (fov / 2.0).tan() * 2.0 * pixels / self.height
    }
}

impl Gizmo {
    pub fn new(kind: GizmoKind, lo: Vec3, hi: Vec3, screen: &Screen) -> Self {
        let size = screen.world_size((lo + hi) / 2.0, 90.0);
        Self {
            kind,
            lo,
            hi,
            size,
            scale_frame: None,
            cut_normal: Vec3::Z,
        }
    }

    /// Кінці стрілки зсуву площини різання: від центру вздовж нормалі.
    fn cut_arrow(&self) -> (Vec3, Vec3) {
        let n = self.cut_normal.normalize_or(Vec3::Z);
        (self.center(), self.center() + n * self.size)
    }

    pub fn center(&self) -> Vec3 {
        (self.lo + self.hi) / 2.0
    }

    /// Вісь `i` як одиничний вектор.
    pub fn axis(i: usize) -> Vec3 {
        [Vec3::X, Vec3::Y, Vec3::Z][i]
    }

    /// Стрілка переміщення: від грані рамки назовні.
    fn arrow(&self, i: usize) -> (Vec3, Vec3) {
        let c = self.center();
        let half = (self.hi - self.lo)[i] / 2.0;
        let start = c + Self::axis(i) * (half + self.size * 0.08);
        (start, start + Self::axis(i) * self.size)
    }

    pub fn ring_radius(&self) -> f32 {
        (self.hi - self.lo).length() / 2.0 + self.size * 0.2
    }

    /// Точки кільця повороту навколо осі `i`.
    fn ring(&self, i: usize, steps: usize) -> Vec<Vec3> {
        let (u, v) = perpendicular(Self::axis(i));
        let r = self.ring_radius();
        (0..steps)
            .map(|k| {
                let a = k as f32 / steps as f32 * std::f32::consts::TAU;
                self.center() + (u * a.cos() + v * a.sin()) * r
            })
            .collect()
    }

    pub fn orient_scale(&mut self, transform: glam::Mat4, lo: Vec3, hi: Vec3) {
        let columns = [
            transform.x_axis.truncate(),
            transform.y_axis.truncate(),
            transform.z_axis.truncate(),
        ];
        let axes = columns.map(|v| v.normalize_or_zero());
        let size = Vec3::new(
            columns[0].length(),
            columns[1].length(),
            columns[2].length(),
        );
        self.scale_frame = Some((
            axes,
            transform.transform_point3((lo + hi) * 0.5),
            (hi - lo) * 0.5 * size,
        ));
    }

    pub fn scale_axes(&self) -> [Vec3; 3] {
        self.scale_frame
            .map_or([Vec3::X, Vec3::Y, Vec3::Z], |f| f.0)
    }
    pub fn scale_center(&self) -> Vec3 {
        self.scale_frame.map_or(self.center(), |f| f.1)
    }
    fn scale_half(&self) -> Vec3 {
        self.scale_frame.map_or((self.hi - self.lo) * 0.5, |f| f.2)
    }
    pub fn scale_base(&self) -> Vec3 {
        self.scale_center() - self.scale_axes()[2] * self.scale_half().z
    }
    pub fn scale_handle(&self, axis: usize, positive: bool) -> Vec3 {
        self.scale_center()
            + self.scale_axes()[axis] * self.scale_half()[axis] * if positive { 1.0 } else { -1.0 }
    }
    pub fn uniform_corner(&self, index: usize) -> Vec3 {
        let [x, y, z] = self.scale_axes();
        let h = self.scale_half();
        let [sx, sy] = [[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]][index.min(3)];
        self.scale_center() + x * h.x * sx + y * h.y * sy + z * h.z
    }

    fn scale_handles(&self) -> Vec<(Handle, Vec3)> {
        let mut out = Vec::new();
        for (axis, positive) in [(0, true), (0, false), (1, true), (1, false), (2, true)] {
            out.push((
                Handle::Scale(axis, positive),
                self.scale_handle(axis, positive),
            ));
        }
        for index in 0..4 {
            out.push((Handle::ScaleUniform(index), self.uniform_corner(index)));
        }
        out
    }

    /// Трикутники гізмо; ручка під курсором або та, що тягнеться, — підсвічена.
    pub fn geometry(&self, camera: &Camera, hot: Option<Handle>) -> Vec<FlatVertex> {
        let mut b = Builder::new(camera);
        let color = |h: Handle, axis: usize| {
            if hot == Some(h) {
                HOVER
            } else {
                AXIS_COLORS[axis]
            }
        };
        match self.kind {
            GizmoKind::Move => {
                for i in 0..3 {
                    let (from, to) = self.arrow(i);
                    let tip_base = to - Self::axis(i) * self.size * 0.22;
                    let c = color(Handle::Move(i), i);
                    b.cylinder(from, tip_base, self.size * 0.018, c, 1.0);
                    b.cone(tip_base, to, self.size * 0.065, c, 1.0);
                }
            }
            GizmoKind::Rotate | GizmoKind::Cut => {
                for i in 0..3 {
                    let points = self.ring(i, 96);
                    let c = color(Handle::Rotate(i), i);
                    let thickness = if hot == Some(Handle::Rotate(i)) {
                        0.022
                    } else {
                        0.014
                    };
                    for k in 0..points.len() {
                        b.cylinder(
                            points[k],
                            points[(k + 1) % points.len()],
                            self.size * thickness,
                            c,
                            1.0,
                        );
                    }
                }
                if self.kind == GizmoKind::Cut {
                    let (from, to) = self.cut_arrow();
                    let n = (to - from).normalize_or_zero();
                    let tip_base = to - n * self.size * 0.22;
                    let c = if hot == Some(Handle::CutMove) {
                        HOVER
                    } else {
                        CUT_ARROW
                    };
                    b.cylinder(from, tip_base, self.size * 0.018, c, 1.0);
                    b.cone(tip_base, to, self.size * 0.065, c, 1.0);
                    b.cube(from, self.size * 0.04, c, 1.0);
                }
            }
            GizmoKind::Scale => {
                for (handle, p) in self.scale_handles() {
                    let c = match handle {
                        Handle::Scale(i, _) => color(handle, i),
                        _ => {
                            if hot == Some(handle) {
                                HOVER
                            } else {
                                [0.863, 0.878, 0.898]
                            }
                        }
                    };
                    b.cube(p, self.size * 0.045, c, 1.0);
                }
            }
        }
        b.triangles
    }

    /// Ручка під точкою кадру `cursor` (пікселі), якщо вона ближче ~9 px.
    pub fn pick(&self, screen: &Screen, cursor: Vec2) -> Option<Handle> {
        const RADIUS: f32 = 9.0;
        let mut best: Option<(f32, Handle)> = None;
        let mut consider = |d: f32, h: Handle| {
            if d < RADIUS && best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, h));
            }
        };
        match self.kind {
            GizmoKind::Move => {
                for i in 0..3 {
                    let (from, to) = self.arrow(i);
                    if let (Some(a), Some(b)) = (screen.project(from), screen.project(to)) {
                        consider(segment_distance(cursor, a, b), Handle::Move(i));
                    }
                }
            }
            GizmoKind::Rotate | GizmoKind::Cut => {
                if self.kind == GizmoKind::Cut {
                    let (from, to) = self.cut_arrow();
                    if let (Some(a), Some(b)) = (screen.project(from), screen.project(to)) {
                        // Стрілка пріоритетніша за кільця, що перетинаються з нею на екрані.
                        consider(segment_distance(cursor, a, b) - 3.0, Handle::CutMove);
                    }
                }
                for i in 0..3 {
                    let points: Vec<Vec2> = self
                        .ring(i, 96)
                        .into_iter()
                        .filter_map(|p| screen.project(p))
                        .collect();
                    for k in 0..points.len() {
                        consider(
                            segment_distance(cursor, points[k], points[(k + 1) % points.len()]),
                            Handle::Rotate(i),
                        );
                    }
                }
            }
            GizmoKind::Scale => {
                for (handle, p) in self.scale_handles() {
                    if let Some(s) = screen.project(p) {
                        consider(s.distance(cursor) * 0.8, handle);
                    }
                }
            }
        }
        best.map(|(_, h)| h)
    }
}

/// Маркер сопла на «Перегляді»: напівпрозорий конус вістрям у точку друку.
pub fn nozzle_marker(camera: &Camera, at: Vec3) -> Vec<FlatVertex> {
    let mut b = Builder::new(camera);
    let color = [0.863, 0.878, 0.898];
    b.cone(at + Vec3::Z * 3.0, at, 1.4, color, 0.75);
    b.cylinder(at + Vec3::Z * 3.0, at + Vec3::Z * 8.0, 1.4, color, 0.75);
    b.triangles
}

/// Два одиничні вектори, перпендикулярні `n` і один одному.
fn perpendicular(n: Vec3) -> (Vec3, Vec3) {
    let helper = if n.z.abs() < 0.9 { Vec3::Z } else { Vec3::X };
    let u = n.cross(helper).normalize();
    (u, n.cross(u))
}

fn segment_distance(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let t = if ab.length_squared() < 1e-6 {
        0.0
    } else {
        ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0)
    };
    p.distance(a + ab * t)
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Збирає освітлені трикутники; світло — від камери трохи згори, як у сцені.
struct Builder {
    triangles: Vec<FlatVertex>,
    light: Vec3,
}

impl Builder {
    fn new(camera: &Camera) -> Self {
        let light = ((camera.eye() - camera.target).normalize_or_zero() + Vec3::Z * 0.5)
            .normalize_or_zero();
        Self {
            triangles: Vec::new(),
            light,
        }
    }

    fn tri(&mut self, a: Vec3, b: Vec3, c: Vec3, normal: Vec3, rgb: [f32; 3], alpha: f32) {
        let shade = 0.55 + 0.45 * normal.normalize_or_zero().dot(self.light).abs();
        let color = [
            srgb_to_linear((rgb[0] * shade).min(1.0)),
            srgb_to_linear((rgb[1] * shade).min(1.0)),
            srgb_to_linear((rgb[2] * shade).min(1.0)),
            alpha,
        ];
        for p in [a, b, c] {
            self.triangles.push(FlatVertex {
                pos: p.to_array(),
                color,
            });
        }
    }

    fn cylinder(&mut self, a: Vec3, b: Vec3, radius: f32, rgb: [f32; 3], alpha: f32) {
        let axis = (b - a).normalize_or_zero();
        if axis == Vec3::ZERO {
            return;
        }
        let (u, v) = perpendicular(axis);
        const N: usize = 10;
        for k in 0..N {
            let (a0, a1) = (
                k as f32 / N as f32 * std::f32::consts::TAU,
                (k + 1) as f32 / N as f32 * std::f32::consts::TAU,
            );
            let (d0, d1) = (u * a0.cos() + v * a0.sin(), u * a1.cos() + v * a1.sin());
            let n = d0 + d1;
            self.tri(
                a + d0 * radius,
                b + d0 * radius,
                b + d1 * radius,
                n,
                rgb,
                alpha,
            );
            self.tri(
                a + d0 * radius,
                b + d1 * radius,
                a + d1 * radius,
                n,
                rgb,
                alpha,
            );
        }
    }

    fn cone(&mut self, base: Vec3, tip: Vec3, radius: f32, rgb: [f32; 3], alpha: f32) {
        let axis = (tip - base).normalize_or_zero();
        if axis == Vec3::ZERO {
            return;
        }
        let (u, v) = perpendicular(axis);
        const N: usize = 16;
        for k in 0..N {
            let (a0, a1) = (
                k as f32 / N as f32 * std::f32::consts::TAU,
                (k + 1) as f32 / N as f32 * std::f32::consts::TAU,
            );
            let (d0, d1) = (u * a0.cos() + v * a0.sin(), u * a1.cos() + v * a1.sin());
            let n = (d0 + d1).normalize() + axis * 0.5;
            self.tri(base + d0 * radius, tip, base + d1 * radius, n, rgb, alpha);
            self.tri(
                base + d0 * radius,
                base + d1 * radius,
                base,
                -axis,
                rgb,
                alpha,
            );
        }
    }

    fn cube(&mut self, center: Vec3, half: f32, rgb: [f32; 3], alpha: f32) {
        for axis in 0..3 {
            for sign in [-1.0f32, 1.0] {
                let n = Gizmo::axis(axis) * sign;
                let (u, v) = perpendicular(n);
                let c = center + n * half;
                let corners = [
                    c + (u + v) * half,
                    c + (u - v) * half,
                    c + (-u - v) * half,
                    c + (-u + v) * half,
                ];
                self.tri(corners[0], corners[1], corners[2], n, rgb, alpha);
                self.tri(corners[0], corners[2], corners[3], n, rgb, alpha);
            }
        }
    }
}
