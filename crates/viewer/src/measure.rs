//! Ядро інструмента «Вимірювання» (аналог `GLGizmoMeasure` + `Measure.cpp` в Orca/Bambu Studio).
//!
//! Модуль чистий: без GPUI/wgpu. UI підключається окремо — він лише:
//! 1. будує [`MeasuringMesh`] для моделі й тримає її (перебудова потрібна тільки при зміні
//!    геометрії; рух/поворот/масштаб — [`MeasuringMesh::set_transform`]);
//! 2. на рух миші викликає [`MeasuringMesh::feature_at`] з променем у світових координатах;
//! 3. після двох кліків викликає [`measure`] і малює результат.
//!
//! Усі виходи — світові координати в f64 (`glam::DVec3`); для малювання `.as_vec3()`.
//!
//! # Що рахується заздалегідь, що ліниво
//! `new` зварює вершини, будує сусідство граней, кластери компланарних граней (площини) і BVH
//! для променів. Межові контури, кола, дуги та ребра площини рахуються при першому доторку до
//! площини (`OnceLock`), тож побудова лишається швидкою навіть на 100k трикутників.
//! [`MeasuringMesh::extract_all`] примусово рахує все.

mod analysis;
mod bvh;
mod geom;
mod pairs;

use std::sync::{Arc, OnceLock};

use glam::{DMat3, DMat4, DVec3, Mat4, Vec3};

use crate::Mesh;
use analysis::{PlaneFeatures, Topology};
pub use pairs::{measure, scale_factor_for};

/// Скільки мм від ознаки (у світових координатах) курсор «чіпляє» її (`feature_hover_limit`).
pub const FEATURE_HOVER_LIMIT: f64 = 0.5;
/// Межі зони прилипання до кінців ребра: `clamp(0.1·len, MIN, MAX)` мм.
pub const POINT_SNAP_MIN: f64 = 0.025;
pub const POINT_SNAP_MAX: f64 = 0.5;

// ───────────────────────── ознаки ─────────────────────────

/// Відрізок. `center` — центр багатокутника, якщо ребро належить правильному 5–8-кутнику
/// (як `extra_point` в Orca).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Edge {
    pub a: DVec3,
    pub b: DVec3,
    pub center: Option<DVec3>,
}

impl Edge {
    pub fn length(&self) -> f64 {
        (self.b - self.a).length()
    }
    pub fn direction(&self) -> DVec3 {
        (self.b - self.a).normalize_or_zero()
    }
    pub fn midpoint(&self) -> DVec3 {
        (self.a + self.b) * 0.5
    }
}

/// Коло або дуга ≥ ~81° (дуга вважається колом, як в Orca).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Circle {
    pub center: DVec3,
    pub normal: DVec3,
    pub radius: f64,
}

impl Circle {
    pub fn diameter(&self) -> f64 {
        self.radius * 2.0
    }
}

/// Контур площини у світових координатах (для відстаней «до межі площини»).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlaneOutline {
    pub edges: Vec<Edge>,
    pub circles: Vec<Circle>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Plane {
    /// Індекс площини в [`MeasuringMesh`].
    pub index: usize,
    /// Одинична нормаль у світових координатах.
    pub normal: DVec3,
    /// Центр ваги межі.
    pub origin: DVec3,
    /// Площа поверхні, мм² (з урахуванням масштабу трансформації).
    pub area: f64,
    pub outline: Arc<PlaneOutline>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SurfaceFeature {
    Point(DVec3),
    Edge(Edge),
    Circle(Circle),
    Plane(Plane),
}

/// Інформація про одну ознаку (панель «Вимірювання» для єдиного вибору).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SingleInfo {
    Point { position: DVec3 },
    Edge { length: f64, direction: DVec3 },
    Circle { radius: f64, diameter: f64, center: DVec3, normal: DVec3 },
    Plane { normal: DVec3, area: f64, origin: DVec3 },
}

impl SurfaceFeature {
    pub fn describe(&self) -> SingleInfo {
        match self {
            Self::Point(p) => SingleInfo::Point { position: *p },
            Self::Edge(e) => SingleInfo::Edge { length: e.length(), direction: e.direction() },
            Self::Circle(c) => SingleInfo::Circle {
                radius: c.radius,
                diameter: c.diameter(),
                center: c.center,
                normal: c.normal,
            },
            Self::Plane(p) => SingleInfo::Plane { normal: p.normal, area: p.area, origin: p.origin },
        }
    }

    /// «Центральна» точка ознаки (режим Shift в Orca): центр кола/багатокутника, центр
    /// площини, середина ребра.
    pub fn center_point(&self) -> DVec3 {
        match self {
            Self::Point(p) => *p,
            Self::Edge(e) => e.center.unwrap_or_else(|| e.midpoint()),
            Self::Circle(c) => c.center,
            Self::Plane(p) => p.origin,
        }
    }
}

// ───────────────────────── результат вимірювання ─────────────────────────

/// Відстань між двома точками: `from` лежить на першій ознаці пари, `to` — на другій.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DistAndPoints {
    pub dist: f64,
    pub from: DVec3,
    pub to: DVec3,
}

/// Кут між ознаками й обидва напрямки (для малювання дуги кута).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AngleInfo {
    pub radians: f64,
    /// Вершина кута.
    pub apex: DVec3,
    /// Напрямок від вершини вздовж першої ознаки пари (одиничний).
    pub dir_a: DVec3,
    /// Те саме для другої.
    pub dir_b: DVec3,
}

impl AngleInfo {
    pub fn degrees(&self) -> f64 {
        self.radians.to_degrees()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeasurementResult {
    pub angle: Option<AngleInfo>,
    /// Відстань між нескінченними продовженнями (пряма/площина), де це має сенс.
    pub distance_infinite: Option<DistAndPoints>,
    /// Найменша відстань між скінченними ознаками.
    pub distance_strict: Option<DistAndPoints>,
    /// Відстань до центру кола (точка/ребро/площина/коло — центр).
    pub center_distance: Option<DistAndPoints>,
    /// Різниці по осях X/Y/Z зі знаком (`to − from` головної відстані; для пар точка–точка
    /// і коло–коло — між центрами). UI показує модулі.
    pub distance_xyz: Option<DVec3>,
    /// Ознаки паралельні, і головна відстань — `distance_infinite`.
    pub infinite_is_primary: bool,
}

impl MeasurementResult {
    /// Відстань, яку Orca виносить у велику мітку: нескінченна для паралельних пар,
    /// інакше строга, інакше (центр/нескінченна).
    pub fn primary_distance(&self) -> Option<DistAndPoints> {
        if self.infinite_is_primary {
            self.distance_infinite.or(self.distance_strict)
        } else {
            self.distance_strict.or(self.distance_infinite).or(self.center_distance)
        }
    }
}

// ───────────────────────── MeasuringMesh ─────────────────────────

/// Результат променевого тесту.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub face: u32,
    /// Параметр променя (у одиницях `ray_dir`).
    pub t: f64,
    /// Точка влучання у світових координатах.
    pub point: DVec3,
    local_point: DVec3,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HoverOptions {
    /// Максимальна відстань від ознаки до точки влучання, мм (світові).
    pub limit: f64,
    /// Повертати лише площину (режим вибору площин).
    pub only_plane: bool,
}

impl Default for HoverOptions {
    fn default() -> Self {
        Self { limit: FEATURE_HOVER_LIMIT, only_plane: false }
    }
}

#[derive(Clone, Copy)]
struct Placement {
    world: DMat4,
    inverse: DMat4,
    /// (M⁻¹)ᵀ — для нормалей.
    normal: DMat3,
    det: f64,
    /// Середній масштаб ∛|det|: переводить допуски зі світових мм у локальні.
    scale: f64,
}

impl Placement {
    fn new(m: Mat4) -> Self {
        let world = m.as_dmat4();
        let linear = DMat3::from_mat4(world);
        let det = linear.determinant();
        let (inverse, normal) = if det.abs() > 1e-18 {
            (world.inverse(), linear.inverse().transpose())
        } else {
            (DMat4::IDENTITY, DMat3::IDENTITY)
        };
        Self { world, inverse, normal, det, scale: det.abs().cbrt().max(1e-12) }
    }
    fn point(&self, p: DVec3) -> DVec3 {
        self.world.transform_point3(p)
    }
    fn unit_normal(&self, n: DVec3) -> DVec3 {
        (self.normal * n).normalize_or_zero()
    }
}

pub struct MeasuringMesh {
    topo: Topology,
    bvh: bvh::Bvh,
    features: Vec<OnceLock<Box<PlaneFeatures>>>,
    place: Placement,
}

impl MeasuringMesh {
    /// Сітка «супом» трикутників ([`Mesh`]) і її трансформація локальне → світове.
    pub fn new(mesh: &Mesh, transform: Mat4) -> Self {
        let n = mesh.positions.len() / 3;
        let indices: Vec<[u32; 3]> = (0..n as u32).map(|i| [i * 3, i * 3 + 1, i * 3 + 2]).collect();
        Self::from_indexed(&mesh.positions[..n * 3], &indices, transform)
    }

    /// Індексована сітка. Вершини все одно зварюються за точним збігом координат.
    pub fn from_indexed(positions: &[[f32; 3]], indices: &[[u32; 3]], transform: Mat4) -> Self {
        let topo = Topology::new(positions, indices);
        let bvh = bvh::Bvh::build(&topo.verts, &topo.faces);
        let features = (0..topo.plane_count()).map(|_| OnceLock::new()).collect();
        Self { topo, bvh, features, place: Placement::new(transform) }
    }

    /// Нова трансформація модель→світ без перерахунку ознак.
    pub fn set_transform(&mut self, transform: Mat4) {
        self.place = Placement::new(transform);
    }

    pub fn triangle_count(&self) -> usize {
        self.topo.faces.len()
    }

    pub fn plane_count(&self) -> usize {
        self.topo.plane_count()
    }

    /// Площина, якій належить трикутник.
    pub fn plane_of_triangle(&self, face: u32) -> usize {
        self.topo.face_plane[face as usize] as usize
    }

    /// Трикутники площини (індекси в порядку вхідної сітки).
    pub fn plane_triangles(&self, plane: usize) -> &[u32] {
        self.topo.plane_facets(plane)
    }

    /// Примусово рахує ознаки всіх площин (для тестів/профілювання).
    pub fn extract_all(&self) {
        for i in 0..self.plane_count() {
            self.local_features(i);
        }
    }

    fn local_features(&self, plane: usize) -> &PlaneFeatures {
        self.features[plane].get_or_init(|| Box::new(self.topo.extract_features(plane)))
    }

    // ── світові ознаки ──

    fn world_edge(&self, e: &Edge) -> Edge {
        Edge { a: self.place.point(e.a), b: self.place.point(e.b), center: e.center.map(|c| self.place.point(c)) }
    }

    fn world_circle(&self, c: &Circle) -> Circle {
        // Радіус при неоднорідному масштабі не визначений: беремо середнє по двох осях площини.
        let [u, v, _] = geom::orthonormal_basis(c.normal);
        let lin = DMat3::from_mat4(self.place.world);
        let radius = c.radius * 0.5 * ((lin * u).length() + (lin * v).length());
        Circle { center: self.place.point(c.center), normal: self.place.unit_normal(c.normal), radius }
    }

    fn world_plane(&self, plane: usize) -> Plane {
        let f = self.local_features(plane);
        let n = self.topo.plane_normal[plane];
        let area = self.topo.plane_area[plane] * self.place.det.abs() * (self.place.normal * n).length();
        Plane {
            index: plane,
            normal: self.place.unit_normal(n),
            origin: self.place.point(f.origin),
            area,
            outline: Arc::new(PlaneOutline {
                edges: f.edges.iter().map(|e| self.world_edge(e)).collect(),
                circles: f.circles.iter().map(|c| self.world_circle(c)).collect(),
            }),
        }
    }

    /// Ознака «площина» (з контуром) за індексом.
    pub fn plane_feature(&self, plane: usize) -> SurfaceFeature {
        SurfaceFeature::Plane(self.world_plane(plane))
    }

    /// Кола й ребра однієї площини у світових координатах (кола першими, як в Orca).
    pub fn plane_features(&self, plane: usize) -> Vec<SurfaceFeature> {
        let f = self.local_features(plane);
        f.circles
            .iter()
            .map(|c| SurfaceFeature::Circle(self.world_circle(c)))
            .chain(f.edges.iter().map(|e| SurfaceFeature::Edge(self.world_edge(e))))
            .collect()
    }

    /// Унікальні ребра моделі (спільні ребра суміжних площин зливаються).
    pub fn edges(&self) -> Vec<Edge> {
        let q = |p: DVec3| ((p.x * 1e4).round() as i64, (p.y * 1e4).round() as i64, (p.z * 1e4).round() as i64);
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for i in 0..self.plane_count() {
            for e in &self.local_features(i).edges {
                let (ka, kb) = (q(e.a), q(e.b));
                if seen.insert(if ka <= kb { (ka, kb) } else { (kb, ka) }) {
                    out.push(self.world_edge(e));
                }
            }
        }
        out
    }

    /// Усі кола/дуги моделі.
    pub fn circles(&self) -> Vec<Circle> {
        (0..self.plane_count())
            .flat_map(|i| self.local_features(i).circles.iter().map(|c| self.world_circle(c)).collect::<Vec<_>>())
            .collect()
    }

    // ── вибір під курсором ──

    /// Найближче влучання променя (світові координати) в модель.
    pub fn raycast(&self, ray_origin: Vec3, ray_dir: Vec3) -> Option<Hit> {
        let o = self.place.inverse.transform_point3(ray_origin.as_dvec3());
        let d = self.place.inverse.transform_vector3(ray_dir.as_dvec3());
        let (face, t) = self.bvh.raycast(&self.topo.verts, &self.topo.faces, o, d)?;
        let local_point = o + d * t;
        Some(Hit { face, t, point: self.place.point(local_point), local_point })
    }

    /// Ознака під курсором: Point / Edge / Circle / Plane. `None`, якщо промінь поза моделлю.
    pub fn feature_at(&self, ray_origin: Vec3, ray_dir: Vec3) -> Option<SurfaceFeature> {
        self.feature_at_with(ray_origin, ray_dir, HoverOptions::default())
    }

    pub fn feature_at_with(&self, ray_origin: Vec3, ray_dir: Vec3, opts: HoverOptions) -> Option<SurfaceFeature> {
        let hit = self.raycast(ray_origin, ray_dir)?;
        Some(self.feature_at_hit(&hit, opts))
    }

    /// Те саме, коли влучання вже відоме (наприклад, для підсвічування без повторного променя).
    pub fn feature_at_hit(&self, hit: &Hit, opts: HoverOptions) -> SurfaceFeature {
        let plane = self.plane_of_triangle(hit.face);
        if !opts.only_plane {
            let s = self.place.scale;
            let limit = opts.limit / s;
            let p = hit.local_point;
            let f = self.local_features(plane);
            // Кола першими: при рівній відстані перемагає раніша ознака (як в Orca).
            let mut best: Option<(f64, Pick)> = None;
            for (i, c) in f.circles.iter().enumerate() {
                let d = pairs::point_circle_distance(p, c).dist;
                if d < limit && best.is_none_or(|(b, _)| d < b) {
                    best = Some((d, Pick::Circle(i)));
                }
            }
            for (i, e) in f.edges.iter().enumerate() {
                let d = (p - geom::closest_on_segment(p, e.a, e.b)).length();
                if d < limit && best.is_none_or(|(b, _)| d < b) {
                    best = Some((d, Pick::Edge(i)));
                }
            }
            match best {
                Some((_, Pick::Circle(i))) => return SurfaceFeature::Circle(self.world_circle(&f.circles[i])),
                Some((_, Pick::Edge(i))) => {
                    let e = &f.edges[i];
                    // Біля кінця ребра — прилипаємо до точки: 10% довжини в межах [0.025; 0.5] мм.
                    let snap = (0.1 * (e.b - e.a).length()).clamp(POINT_SNAP_MIN / s, POINT_SNAP_MAX / s);
                    for end in [e.a, e.b] {
                        if (p - end).length() < snap {
                            return SurfaceFeature::Point(self.place.point(end));
                        }
                    }
                    return SurfaceFeature::Edge(self.world_edge(e));
                }
                None => {}
            }
        }
        self.plane_feature(plane)
    }
}

#[derive(Clone, Copy)]
enum Pick {
    Circle(usize),
    Edge(usize),
}
