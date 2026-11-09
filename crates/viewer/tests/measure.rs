use std::time::Instant;

use glam::{DVec3, Mat4, Quat, Vec3};
use viewer::{
    Mesh,
    measure::{HoverOptions, MeasuringMesh, SingleInfo, SurfaceFeature, measure, scale_factor_for},
};

// ───────────── генератори сіток ─────────────

fn mesh_from(tris: &[[Vec3; 3]]) -> Mesh {
    let mut m = Mesh::default();
    for t in tris {
        for v in t {
            m.positions.push(v.to_array());
            m.normals.push([0.0; 3]);
        }
    }
    m
}

fn quad(out: &mut Vec<[Vec3; 3]>, a: Vec3, b: Vec3, c: Vec3, d: Vec3) {
    out.push([a, b, c]);
    out.push([a, c, d]);
}

/// Куб з ребром `s` на початку координат; грані зовні проти годинникової.
fn cube(s: f32) -> Mesh {
    let v = |x: f32, y: f32, z: f32| Vec3::new(x, y, z) * s;
    let mut t = Vec::new();
    quad(&mut t, v(0., 0., 0.), v(0., 1., 0.), v(1., 1., 0.), v(1., 0., 0.)); // -Z
    quad(&mut t, v(0., 0., 1.), v(1., 0., 1.), v(1., 1., 1.), v(0., 1., 1.)); // +Z
    quad(&mut t, v(0., 0., 0.), v(1., 0., 0.), v(1., 0., 1.), v(0., 0., 1.)); // -Y
    quad(&mut t, v(1., 0., 0.), v(1., 1., 0.), v(1., 1., 1.), v(1., 0., 1.)); // +X
    quad(&mut t, v(1., 1., 0.), v(0., 1., 0.), v(0., 1., 1.), v(1., 1., 1.)); // +Y
    quad(&mut t, v(0., 0., 0.), v(0., 0., 1.), v(0., 1., 1.), v(0., 1., 0.)); // -X
    mesh_from(&t)
}

/// Випукла призма: контур (CCW) вивернуто на висоту `h`; кришки — віялом із центру.
fn prism(outline: &[Vec3], h: f32) -> Mesh {
    let n = outline.len();
    let center = outline.iter().fold(Vec3::ZERO, |a, p| a + *p) / n as f32;
    let up = Vec3::Z * h;
    let mut t = Vec::new();
    for i in 0..n {
        let (a, b) = (outline[i], outline[(i + 1) % n]);
        t.push([center, a, b].map(|p| p + up)); // +Z
        t.push([center, b, a]); // -Z
        quad(&mut t, a, b, b + up, a + up);
    }
    mesh_from(&t)
}

fn circle_outline(n: usize, r: f32) -> Vec<Vec3> {
    (0..n)
        .map(|i| {
            let a = std::f32::consts::TAU * i as f32 / n as f32;
            Vec3::new(r * a.cos(), r * a.sin(), 0.0)
        })
        .collect()
}

fn cylinder(segments: usize, r: f32, h: f32) -> Mesh {
    prism(&circle_outline(segments, r), h)
}

/// Труба: зовнішній радіус `ro`, внутрішній `ri`, висота `h`, `n` сегментів.
fn tube(n: usize, ro: f32, ri: f32, h: f32) -> Mesh {
    let (outer, inner) = (circle_outline(n, ro), circle_outline(n, ri));
    let up = Vec3::Z * h;
    let mut t = Vec::new();
    for i in 0..n {
        let j = (i + 1) % n;
        quad(&mut t, outer[i] + up, outer[j] + up, inner[j] + up, inner[i] + up); // верх
        quad(&mut t, outer[i], inner[i], inner[j], outer[j]); // низ
        quad(&mut t, outer[i], outer[j], outer[j] + up, outer[i] + up); // зовнішня стінка
        quad(&mut t, inner[i], inner[i] + up, inner[j] + up, inner[j]); // внутрішня стінка
    }
    mesh_from(&t)
}

/// Прямокутник 40×30 із заокругленими кутами r=5 (по 12 сегментів на кут 90°).
fn rounded_slab() -> Mesh {
    let (w, d, r, seg) = (40.0f32, 30.0f32, 5.0f32, 12);
    let mut o = Vec::new();
    let corners = [(w - r, r, -90.0f32), (w - r, d - r, 0.0), (r, d - r, 90.0), (r, r, 180.0)];
    for (cx, cy, start) in corners {
        for i in 0..=seg {
            let a = (start + 90.0 * i as f32 / seg as f32).to_radians();
            o.push(Vec3::new(cx + r * a.cos(), cy + r * a.sin(), 0.0));
        }
    }
    prism(&o, 3.0)
}

/// UV-сфера: ≈ 2·rings·segs трикутників, кожна грань — своя площина.
fn sphere(rings: usize, segs: usize, r: f32) -> Mesh {
    let p = |i: usize, j: usize| {
        let th = std::f32::consts::PI * i as f32 / rings as f32;
        let ph = std::f32::consts::TAU * (j % segs) as f32 / segs as f32;
        Vec3::new(th.sin() * ph.cos(), th.sin() * ph.sin(), th.cos()) * r
    };
    let mut t = Vec::new();
    for i in 0..rings {
        for j in 0..segs {
            let (a, b, c, d) = (p(i, j), p(i + 1, j), p(i + 1, j + 1), p(i, j + 1));
            if i > 0 {
                t.push([a, b, d]);
            }
            if i + 1 < rings {
                t.push([b, c, d]);
            }
        }
    }
    mesh_from(&t)
}

/// Плоска сітка n×n клітин => 2n² трикутників в одній площині.
fn flat_grid(n: usize) -> Mesh {
    let p = |i: usize, j: usize| Vec3::new(i as f32, j as f32, 0.0);
    let mut t = Vec::new();
    for i in 0..n {
        for j in 0..n {
            quad(&mut t, p(i, j), p(i + 1, j), p(i + 1, j + 1), p(i, j + 1));
        }
    }
    mesh_from(&t)
}

fn plane_with_normal(m: &MeasuringMesh, n: DVec3) -> SurfaceFeature {
    (0..m.plane_count())
        .map(|i| m.plane_feature(i))
        .find(|f| matches!(f, SurfaceFeature::Plane(p) if p.normal.dot(n) > 0.999))
        .unwrap_or_else(|| panic!("нема площини з нормаллю {n}"))
}

fn assert_close(a: f64, b: f64, eps: f64) {
    assert!((a - b).abs() < eps, "{a} != {b} (±{eps})");
}

fn pt(x: f64, y: f64, z: f64) -> SurfaceFeature {
    SurfaceFeature::Point(DVec3::new(x, y, z))
}

fn edge(a: [f64; 3], b: [f64; 3]) -> SurfaceFeature {
    SurfaceFeature::Edge(viewer::measure::Edge { a: a.into(), b: b.into(), center: None })
}

// ───────────── куб ─────────────

#[test]
fn cube_has_6_planes_and_12_edges() {
    let m = MeasuringMesh::new(&cube(20.0), Mat4::IDENTITY);
    assert_eq!(m.triangle_count(), 12);
    assert_eq!(m.plane_count(), 6);
    let edges = m.edges();
    assert_eq!(edges.len(), 12);
    for e in &edges {
        assert_close(e.length(), 20.0, 1e-4);
    }
    assert!(m.circles().is_empty());
    for i in 0..6 {
        assert_eq!(m.plane_triangles(i).len(), 2);
        assert_eq!(m.plane_features(i).len(), 4, "у грані куба 4 ребра");
    }
}

#[test]
fn cube_parallel_planes_distance_equals_edge_length() {
    let m = MeasuringMesh::new(&cube(20.0), Mat4::IDENTITY);
    let top = plane_with_normal(&m, DVec3::Z);
    let bottom = plane_with_normal(&m, -DVec3::Z);
    let r = measure(&top, &bottom);
    assert!(r.infinite_is_primary);
    assert!(r.angle.is_none());
    assert_close(r.primary_distance().unwrap().dist, 20.0, 1e-4);
    assert_close(r.distance_infinite.unwrap().dist, 20.0, 1e-4);
    // Різниці по осях: лише Z.
    let xyz = r.distance_xyz.unwrap();
    assert_close(xyz.z.abs(), 20.0, 1e-4);
    assert_close(xyz.x.abs() + xyz.y.abs(), 0.0, 1e-4);
    // Порядок аргументів не міняє модуль відстані.
    assert_close(measure(&bottom, &top).primary_distance().unwrap().dist, 20.0, 1e-4);
}

#[test]
fn cube_adjacent_planes_angle_is_90_degrees() {
    let m = MeasuringMesh::new(&cube(20.0), Mat4::IDENTITY);
    let top = plane_with_normal(&m, DVec3::Z);
    let side = plane_with_normal(&m, DVec3::X);
    let r = measure(&top, &side);
    let a = r.angle.unwrap();
    assert_close(a.degrees(), 90.0, 1e-3);
    assert!(a.dir_a.dot(a.dir_b).abs() < 1e-6);
    // Вершина на ребрі x=20,z=20.
    assert_close(a.apex.x, 20.0, 1e-4);
    assert_close(a.apex.z, 20.0, 1e-4);
    assert!(r.primary_distance().is_none());
    // Зворотний порядок міняє місцями напрямки.
    let back = measure(&side, &top).angle.unwrap();
    assert_close(back.degrees(), 90.0, 1e-3);
    assert!((back.dir_a - a.dir_b).length() < 1e-9);
}

#[test]
fn cube_plane_info_edge_plane_and_point_plane() {
    let m = MeasuringMesh::new(&cube(20.0), Mat4::IDENTITY);
    let top = plane_with_normal(&m, DVec3::Z);
    let SingleInfo::Plane { normal, area, .. } = top.describe() else { panic!() };
    assert_close(normal.z, 1.0, 1e-9);
    assert_close(area, 400.0, 1e-3);

    // Вертикальне ребро x=0,y=0 проти верхньої грані: ребро торкається площини, кут 90°.
    let vertical = edge([0., 0., 0.], [0., 0., 20.]);
    let r = measure(&vertical, &top);
    assert_close(r.angle.unwrap().degrees(), 90.0, 1e-3);
    assert_close(r.distance_strict.unwrap().dist, 0.0, 1e-6);
    // Горизонтальне ребро на нижній грані проти верхньої: паралельне, відстань 20.
    let horiz = edge([0., 0., 0.], [20., 0., 0.]);
    let r = measure(&horiz, &top);
    assert!(r.infinite_is_primary && r.angle.is_none());
    assert_close(r.primary_distance().unwrap().dist, 20.0, 1e-6);
    // Точка–площина.
    let r = measure(&pt(3., 4., 5.), &top);
    assert_close(r.primary_distance().unwrap().dist, 15.0, 1e-6);
    let to = r.primary_distance().unwrap().to;
    assert_close(to.z, 20.0, 1e-6);
}

// ───────────── циліндр, кола, дуги ─────────────

#[test]
fn cylinder_has_two_circles_with_correct_radius() {
    let m = MeasuringMesh::new(&cylinder(64, 10.0, 30.0), Mat4::IDENTITY);
    assert_eq!(m.plane_count(), 2 + 64);
    let circles = m.circles();
    assert_eq!(circles.len(), 2, "верх і низ");
    for c in &circles {
        assert_close(c.radius, 10.0, 1e-4);
        assert_close(c.normal.z.abs(), 1.0, 1e-9);
        assert_close(c.center.x, 0.0, 1e-4);
        assert_close(c.center.y, 0.0, 1e-4);
    }
    let mut zs: Vec<f64> = circles.iter().map(|c| c.center.z).collect();
    zs.sort_by(f64::total_cmp);
    assert_close(zs[0], 0.0, 1e-4);
    assert_close(zs[1], 30.0, 1e-4);
    assert_eq!(m.edges().len(), 64 * 3, "64 вертикальних + по 64 відрізки обідків");
    let info = SurfaceFeature::Circle(circles[0]).describe();
    let SingleInfo::Circle { diameter, .. } = info else { panic!() };
    assert_close(diameter, 20.0, 1e-3);
}

#[test]
fn cylinder_circle_pairs() {
    let m = MeasuringMesh::new(&cylinder(64, 10.0, 30.0), Mat4::IDENTITY);
    let c = m.circles();
    let (top, bottom) = if c[0].center.z > c[1].center.z { (c[0], c[1]) } else { (c[1], c[0]) };
    let r = measure(&SurfaceFeature::Circle(top), &SurfaceFeature::Circle(bottom));
    assert_close(r.center_distance.unwrap().dist, 30.0, 1e-4);
    // Співвісні рівні кола: відстань між обідками = висота.
    assert_close(r.distance_strict.unwrap().dist, 30.0, 1e-4);
    assert!(r.angle.is_none());
    assert_close(r.distance_xyz.unwrap().z.abs(), 30.0, 1e-4);
    // Коло проти площини протилежної кришки.
    let plane = plane_with_normal(&m, -DVec3::Z);
    let r = measure(&SurfaceFeature::Circle(top), &plane);
    assert!(r.infinite_is_primary);
    assert_close(r.primary_distance().unwrap().dist, 30.0, 1e-4);
}

#[test]
fn partial_arcs_of_rounded_slab_are_circles_and_hexagon_is_polygon() {
    let m = MeasuringMesh::new(&rounded_slab(), Mat4::IDENTITY);
    let circles = m.circles();
    // 4 дуги на верхній і 4 на нижній кришках.
    assert_eq!(circles.len(), 8, "{circles:?}");
    for c in &circles {
        assert_close(c.radius, 5.0, 1e-3);
    }

    let hex = MeasuringMesh::new(&prism(&circle_outline(6, 10.0), 5.0), Mat4::IDENTITY);
    assert!(hex.circles().is_empty(), "шестикутник — багатокутник, не коло");
    assert!(
        (0..hex.plane_count()).any(|i| hex
            .plane_features(i)
            .iter()
            .any(|f| matches!(f, SurfaceFeature::Edge(e) if e.center.is_some()))),
        "кришки шестикутника мають ребра з центром багатокутника"
    );
}

#[test]
fn tube_has_outer_and_inner_circles_on_each_cap() {
    let m = MeasuringMesh::new(&tube(64, 10.0, 5.0, 8.0), Mat4::IDENTITY);
    let mut radii: Vec<f64> = m.circles().iter().map(|c| c.radius).collect();
    radii.sort_by(f64::total_cmp);
    assert_eq!(radii.len(), 4, "{radii:?}");
    assert_close(radii[0], 5.0, 1e-4);
    assert_close(radii[1], 5.0, 1e-4);
    assert_close(radii[2], 10.0, 1e-4);
    assert_close(radii[3], 10.0, 1e-4);
    // Наведення на внутрішній обідок верхньої кришки дає коло r=5.
    let a = std::f32::consts::TAU / 128.0;
    let f = m.feature_at(Vec3::new(5.2 * a.cos(), 5.2 * a.sin(), 50.0), -Vec3::Z).unwrap();
    let SurfaceFeature::Circle(c) = f else { panic!("{f:?}") };
    assert_close(c.radius, 5.0, 1e-4);
}

#[test]
fn degenerate_and_open_meshes_do_not_panic() {
    // Порожня сітка.
    let m = MeasuringMesh::new(&Mesh::default(), Mat4::IDENTITY);
    assert_eq!(m.plane_count(), 0);
    assert!(m.feature_at(Vec3::Z, -Vec3::Z).is_none());
    assert!(m.edges().is_empty());

    // Відкритий квад: усе — межа, одна площина з 4 ребрами.
    let mut t = Vec::new();
    quad(&mut t, Vec3::ZERO, Vec3::X * 10.0, Vec3::new(10., 10., 0.), Vec3::Y * 10.0);
    let m = MeasuringMesh::new(&mesh_from(&t), Mat4::IDENTITY);
    assert_eq!(m.plane_count(), 1);
    assert_eq!(m.edges().len(), 4);

    // Вироджений трикутник + перевернутий сусід + дубль: просто не падаємо.
    t.push([Vec3::ZERO, Vec3::ZERO, Vec3::X]);
    t.push([Vec3::ZERO, Vec3::new(10., 10., 0.), Vec3::X * 10.0]);
    t.push([Vec3::ZERO, Vec3::X * 10.0, Vec3::Y * 10.0]);
    let m = MeasuringMesh::new(&mesh_from(&t), Mat4::IDENTITY);
    m.extract_all();
    let _ = m.feature_at(Vec3::new(5., 5., 10.), -Vec3::Z);
    let _ = m.edges();
}

// ───────────── числові пари ─────────────

#[test]
fn point_edge_distances() {
    // Проєкція всередині відрізка.
    let r = measure(&pt(2., 3., 0.), &edge([-1., 0., 0.], [5., 0., 0.]));
    assert_close(r.distance_strict.unwrap().dist, 3.0, 1e-9);
    assert_close(r.distance_infinite.unwrap().dist, 3.0, 1e-9);
    // За кінцем: строга — до кінця, нескінченна — до прямої.
    let r = measure(&pt(8., 4., 0.), &edge([-1., 0., 0.], [5., 0., 0.]));
    assert_close(r.distance_strict.unwrap().dist, 5.0, 1e-9); // (3,4)
    assert_close(r.distance_infinite.unwrap().dist, 4.0, 1e-9);
    let s = r.distance_strict.unwrap();
    assert_eq!((s.from, s.to), (DVec3::new(8., 4., 0.), DVec3::new(5., 0., 0.)));
    // Зворотний порядок: from/to міняються місцями.
    let back = measure(&edge([-1., 0., 0.], [5., 0., 0.]), &pt(8., 4., 0.));
    assert_eq!(back.distance_strict.unwrap().from, DVec3::new(5., 0., 0.));
    assert_close(back.distance_strict.unwrap().dist, 5.0, 1e-9);
}

#[test]
fn point_point_xyz() {
    let r = measure(&pt(1., 2., 3.), &pt(4., -2., 15.));
    assert_close(r.distance_strict.unwrap().dist, (9.0f64 + 16.0 + 144.0).sqrt(), 1e-9);
    assert_eq!(r.distance_xyz.unwrap(), DVec3::new(3., -4., 12.));
}

#[test]
fn edge_edge_skew_parallel_and_intersecting() {
    // Мимобіжні, хрест: прямі розділені 3 мм по Z, кут 90°.
    let r = measure(&edge([0., 0., 0.], [10., 0., 0.]), &edge([5., -5., 3.], [5., 5., 3.]));
    assert_close(r.distance_infinite.unwrap().dist, 3.0, 1e-9);
    assert_close(r.distance_strict.unwrap().dist, 3.0, 1e-9);
    assert_close(r.angle.unwrap().degrees(), 90.0, 1e-9);
    assert!(!r.infinite_is_primary);

    // Те саме, але друге ребро відсунуте за кінець першого: прямі 3 мм, відрізки √34.
    let r = measure(&edge([0., 0., 0.], [10., 0., 0.]), &edge([15., -5., 3.], [15., 5., 3.]));
    assert_close(r.distance_infinite.unwrap().dist, 3.0, 1e-9);
    assert_close(r.distance_strict.unwrap().dist, 34.0f64.sqrt(), 1e-9);

    // Паралельні зі зсувом по довжині: нескінченна — перпендикуляр 4, строга — діагональ.
    let r = measure(&edge([0., 0., 0.], [10., 0., 0.]), &edge([13., 4., 0.], [20., 4., 0.]));
    assert!(r.infinite_is_primary && r.angle.is_none());
    assert_close(r.distance_infinite.unwrap().dist, 4.0, 1e-9);
    assert_close(r.distance_strict.unwrap().dist, 5.0, 1e-9); // (3,4)

    // Перетин під 60°: кут між напрямками від точки перетину до дальших кінців.
    let (c, s) = (60f64.to_radians().cos(), 60f64.to_radians().sin());
    let r = measure(&edge([0., 0., 0.], [10., 0., 0.]), &edge([0., 0., 0.], [10. * c, 10. * s, 0.]));
    assert_close(r.angle.unwrap().degrees(), 60.0, 1e-6);
    assert_close(r.distance_strict.unwrap().dist, 0.0, 1e-9);
}

#[test]
fn point_circle_edge_circle_circle_circle_values() {
    use viewer::measure::Circle;
    let c0 = Circle { center: DVec3::ZERO, normal: DVec3::Z, radius: 5.0 };
    // Точка над центром на висоті 12: до обідка √(25+144)=13.
    let r = measure(&pt(0., 0., 12.), &SurfaceFeature::Circle(c0));
    assert_close(r.distance_strict.unwrap().dist, 13.0, 1e-9);
    assert_close(r.center_distance.unwrap().dist, 12.0, 1e-9);
    // Точка поза колом в його площині: до обідка 8-5.
    let r = measure(&pt(8., 0., 0.), &SurfaceFeature::Circle(c0));
    assert_close(r.distance_strict.unwrap().dist, 3.0, 1e-9);

    // Ребро над колом паралельно площині кола, проходить над центром на висоті 4: до обідка 4.
    let r = measure(&edge([-20., 0., 4.], [20., 0., 4.]), &SurfaceFeature::Circle(c0));
    assert_close(r.distance_strict.unwrap().dist, 4.0, 1e-6);
    assert_close(r.center_distance.unwrap().dist, 4.0, 1e-9);
    // Ребро поза колом: від (8,-3,0)→(8,3,0): найближче до обідка в (8,0,0) => 3.
    let r = measure(&edge([8., -3., 0.], [8., 3., 0.]), &SurfaceFeature::Circle(c0));
    assert_close(r.distance_strict.unwrap().dist, 3.0, 1e-6);

    // Співвісні r=5 і r=3 на висоті 4: g=2, h=4 → √20.
    let c1 = Circle { center: DVec3::new(0., 0., 4.), normal: DVec3::Z, radius: 3.0 };
    let r = measure(&SurfaceFeature::Circle(c0), &SurfaceFeature::Circle(c1));
    assert_close(r.distance_strict.unwrap().dist, 20.0f64.sqrt(), 1e-9);
    // Скошені кола: c2 у площині x=8 (нормаль X), r=2: мінімум √68 − 5.
    let c2 = Circle { center: DVec3::new(8., 0., 0.), normal: DVec3::X, radius: 2.0 };
    let r = measure(&SurfaceFeature::Circle(c0), &SurfaceFeature::Circle(c2));
    assert_close(r.distance_strict.unwrap().dist, 68.0f64.sqrt() - 5.0, 1e-7);
    assert_close(r.center_distance.unwrap().dist, 8.0, 1e-9);
    assert_close(r.angle.unwrap().degrees(), 90.0, 1e-6);
    let s = r.distance_strict.unwrap();
    assert_close((s.from - c0.center).length(), 5.0, 1e-6); // from — на першому колі
    assert_close((s.to - c2.center).length(), 2.0, 1e-6); // to — на другому
}

#[test]
fn scale_factor_helper() {
    assert_eq!(scale_factor_for(20.0, 30.0), Some(1.5));
    assert_eq!(scale_factor_for(20.0, 10.0), Some(0.5));
    assert_eq!(scale_factor_for(0.0, 10.0), None);
    assert_eq!(scale_factor_for(20.0, 0.0), None);
    assert_eq!(scale_factor_for(20.0, -3.0), None);
    assert_eq!(scale_factor_for(f64::NAN, 3.0), None);
}

// ───────────── трансформація й наведення ─────────────

#[test]
fn transform_is_respected() {
    let tr = Mat4::from_scale_rotation_translation(
        Vec3::splat(2.0),
        Quat::from_rotation_z(30f32.to_radians()),
        Vec3::new(100.0, 50.0, 10.0),
    );
    let mut m = MeasuringMesh::new(&cube(20.0), tr);
    // Ребра ×2 = 40, відстань між паралельними гранями 40, площа 1600.
    for e in m.edges() {
        assert_close(e.length(), 40.0, 1e-3);
    }
    let top = plane_with_normal(&m, DVec3::Z);
    let bottom = plane_with_normal(&m, -DVec3::Z);
    assert_close(measure(&top, &bottom).primary_distance().unwrap().dist, 40.0, 1e-3);
    let SingleInfo::Plane { area, origin, .. } = top.describe() else { panic!() };
    assert_close(area, 1600.0, 1e-2);
    // Центр верхньої грані: локальний (10,10,20) → світовий.
    let want = tr.transform_point3(Vec3::new(10.0, 10.0, 20.0)).as_dvec3();
    assert!((origin - want).length() < 1e-3, "{origin} vs {want}");
    // Бічна грань повернута на 30°.
    let side = (0..m.plane_count())
        .map(|i| m.plane_feature(i))
        .find(|f| matches!(f, SurfaceFeature::Plane(p) if p.normal.z.abs() < 0.1 && p.normal.x > 0.8))
        .unwrap();
    let SurfaceFeature::Plane(sp) = &side else { unreachable!() };
    assert_close(sp.normal.x, 30f64.to_radians().cos(), 1e-6);

    // Промінь вертикально вниз у центр верхньої грані влучає саме в неї.
    let from = want.as_vec3() + Vec3::Z * 50.0;
    let hit = m.raycast(from, -Vec3::Z).unwrap();
    assert_close(hit.point.z, want.z, 1e-3);
    assert_close(hit.t, 50.0, 1e-3);
    let SurfaceFeature::Plane(p) = m.feature_at(from, -Vec3::Z).unwrap() else { panic!("очікували площину") };
    assert_close(p.normal.z, 1.0, 1e-6);
    // Мимо моделі — нічого.
    assert!(m.feature_at(Vec3::new(500.0, 500.0, 100.0), -Vec3::Z).is_none());

    // set_transform без перерахунку: тепер зсув (−100,−50,−10) і масштаб 1 → куб 20.
    m.set_transform(Mat4::IDENTITY);
    assert_close(m.edges()[0].length(), 20.0, 1e-4);
    assert!(m.feature_at(from, -Vec3::Z).is_none());
    let SurfaceFeature::Plane(p) = m.feature_at(Vec3::new(10., 10., 50.), -Vec3::Z).unwrap() else { panic!() };
    assert_close(p.origin.z, 20.0, 1e-4);
}

#[test]
fn hover_picks_point_edge_plane_with_limits() {
    let m = MeasuringMesh::new(&cube(20.0), Mat4::IDENTITY);
    let down = -Vec3::Z;
    let at = |x: f32, y: f32| m.feature_at(Vec3::new(x, y, 50.0), down).unwrap();
    // Центр верхньої грані — площина.
    assert!(matches!(at(10.0, 10.0), SurfaceFeature::Plane(_)));
    // Далеко від межі (2 мм) — усе ще площина.
    assert!(matches!(at(10.0, 2.0), SurfaceFeature::Plane(_)));
    // 0.3 мм від ребра y=0 посередині — ребро.
    let SurfaceFeature::Edge(e) = at(10.0, 0.3) else { panic!("очікували ребро") };
    assert_close(e.length(), 20.0, 1e-4);
    // 0.6 мм — за межею наведення 0.5.
    assert!(matches!(at(10.0, 0.6), SurfaceFeature::Plane(_)));
    // Біля вершини (0.05, 0.05): прилипання до кінця ребра (поріг 0.5 мм для ребра 20 мм).
    let SurfaceFeature::Point(p) = at(0.05, 0.05) else { panic!("очікували точку") };
    assert!((p - DVec3::new(0., 0., 20.)).length() < 1e-6);
    // Поза порогом прилипання, але в зоні ребра (0.3 від ребра, 1.5 від вершини).
    assert!(matches!(at(1.5, 0.3), SurfaceFeature::Edge(_)));
    // only_plane ігнорує ребра.
    let f = m
        .feature_at_with(Vec3::new(10., 0.1, 50.), down, HoverOptions { only_plane: true, ..Default::default() })
        .unwrap();
    assert!(matches!(f, SurfaceFeature::Plane(_)));
    // Власний поріг (під піксельний допуск UI).
    let f = m
        .feature_at_with(Vec3::new(10., 1.5, 50.), down, HoverOptions { limit: 2.0, ..Default::default() })
        .unwrap();
    assert!(matches!(f, SurfaceFeature::Edge(_)));
}

#[test]
fn hover_on_cylinder_rim_gives_circle() {
    let m = MeasuringMesh::new(&cylinder(64, 10.0, 30.0), Mat4::IDENTITY);
    // Над обідком верхньої кришки, на 0.2 мм всередину.
    let r = 10.0 - 0.2;
    let f = m.feature_at(Vec3::new(r, 0.0, 100.0), -Vec3::Z).unwrap();
    // Вершина (10,0,30) лежить на осі X, тож перевіримо трохи збоку, між вершинами.
    let a = std::f32::consts::TAU / 128.0;
    let f2 = m
        .feature_at(Vec3::new(r * a.cos(), r * a.sin(), 100.0), -Vec3::Z)
        .unwrap();
    let SurfaceFeature::Circle(c) = f2 else { panic!("очікували коло, а не {f2:?} (біля вершини: {f:?})") };
    assert_close(c.radius, 10.0, 1e-4);
    assert_close(c.center.z, 30.0, 1e-4);
    // Центр кришки — площина.
    assert!(matches!(m.feature_at(Vec3::new(0., 0., 100.), -Vec3::Z).unwrap(), SurfaceFeature::Plane(_)));
}

// ───────────── продуктивність ─────────────

fn timed(name: &str, mesh: &Mesh) {
    let tris = mesh.triangle_count();
    let t0 = Instant::now();
    let m = MeasuringMesh::new(mesh, Mat4::IDENTITY);
    let build = t0.elapsed();
    let t1 = Instant::now();
    m.extract_all();
    let extract = t1.elapsed();
    let t2 = Instant::now();
    let rays = 1000;
    let mut hits = 0;
    let b = mesh.bounds().unwrap();
    let c = (b.0 + b.1) * 0.5;
    let s = (b.1 - b.0) * 0.5;
    for i in 0..rays {
        let (u, v) = ((i % 31) as f32 / 31.0 - 0.5, (i / 31 % 31) as f32 / 31.0 - 0.5);
        let o = Vec3::new(c.x + u * s.x * 1.6, c.y + v * s.y * 1.6, b.1.z + 10.0);
        if m.feature_at(o, -Vec3::Z).is_some() {
            hits += 1;
        }
    }
    let per_ray = t2.elapsed().as_secs_f64() * 1e6 / rays as f64;
    println!(
        "{name}: {tris} трикутників, {} площин | побудова {:.1} мс, усі ознаки {:.1} мс, hover {:.1} мкс/промінь ({hits} влучань)",
        m.plane_count(),
        build.as_secs_f64() * 1e3,
        extract.as_secs_f64() * 1e3,
        per_ray
    );
    let limit = if cfg!(debug_assertions) { 2000.0 } else { 200.0 };
    assert!(build.as_secs_f64() * 1e3 < limit, "{name}: побудова {build:?} > {limit} мс");
    assert!(
        (build + extract).as_secs_f64() * 1e3 < limit * 2.0,
        "{name}: побудова+ознаки {:?}",
        build + extract
    );
}

#[test]
fn perf_100k_triangles() {
    // 2·rings·segs − 2·segs ≈ 100k.
    let s = sphere(224, 224, 50.0);
    assert!(s.triangle_count() > 99_000, "{}", s.triangle_count());
    timed("сфера (кожна грань — своя площина)", &s);

    let g = flat_grid(224);
    assert!(g.triangle_count() > 99_000);
    let m = MeasuringMesh::new(&g, Mat4::IDENTITY);
    assert_eq!(m.plane_count(), 1, "уся сітка — одна площина");
    timed("плоска сітка (одна площина зі 100k граней)", &g);

    // Циліндр на 25k сегментів: 100k трикутників, 2 кола з 25k вершин у контурі.
    let c = cylinder(25_000, 10.0, 20.0);
    timed("циліндр 25000 сегментів", &c);
    let m = MeasuringMesh::new(&c, Mat4::IDENTITY);
    assert_eq!(m.circles().len(), 2);
}

#[test]
fn measuring_mesh_is_send_and_sync() {
    fn check<T: Send + Sync>() {}
    check::<MeasuringMesh>();
}
