//! Вимірювання між двома ознаками (порт `get_measurement` з Orca `Measure.cpp`).
//!
//! Відмінності від Orca (свідомі):
//! * мінімальна відстань коло–коло, ребро–коло та точка–коло рахується точно: замкнено там,
//!   де можна, інакше чисельно (вибірка + золотий переріз), а не поліномом 8-го степеня
//!   Еберлі; похибка ≲1e-9 мм;
//! * для паралельних кіл відстань враховує і зсув уздовж осі (Orca його ігнорує);
//! * `distance_strict` для пар із площиною — це відстань до *контуру* площини (як у Orca
//!   для ребро–площина/коло–площина); внутрішність площини не перевіряється.

use glam::DVec3;

use super::{
    AngleInfo, Circle, DistAndPoints, Edge, MeasurementResult, Plane, SurfaceFeature,
    geom::{
        are_parallel, are_perpendicular, closest_on_segment, line_line, line_plane_intersection,
        minimize, orthogonal, orthonormal_basis, segment_segment,
    },
};

fn dp(from: DVec3, to: DVec3) -> DistAndPoints {
    DistAndPoints { dist: (to - from).length(), from, to }
}

fn swapped(d: DistAndPoints) -> DistAndPoints {
    DistAndPoints { dist: d.dist, from: d.to, to: d.from }
}

fn rank(f: &SurfaceFeature) -> u8 {
    match f {
        SurfaceFeature::Point(_) => 0,
        SurfaceFeature::Edge(_) => 1,
        SurfaceFeature::Circle(_) => 2,
        SurfaceFeature::Plane(_) => 3,
    }
}

/// Коефіцієнт рівномірного масштабу для «редагування відстані»: `new / old`.
/// `None`, якщо стара відстань нульова/не скінченна чи нова не додатна.
pub fn scale_factor_for(old_distance: f64, new_distance: f64) -> Option<f64> {
    let ok = old_distance.is_finite() && new_distance.is_finite() && old_distance > 1e-9 && new_distance > 0.0;
    ok.then(|| new_distance / old_distance)
}

/// Повне вимірювання пари ознак `a`, `b`. `from` у відстанях лежить на `a`, `to` — на `b`,
/// `dir_a`/`dir_b` кута — вздовж `a`/`b`.
pub fn measure(a: &SurfaceFeature, b: &SurfaceFeature) -> MeasurementResult {
    if rank(a) > rank(b) {
        let mut r = ordered(b, a);
        r.distance_infinite = r.distance_infinite.map(swapped);
        r.distance_strict = r.distance_strict.map(swapped);
        r.center_distance = r.center_distance.map(swapped);
        if let Some(angle) = &mut r.angle {
            std::mem::swap(&mut angle.dir_a, &mut angle.dir_b);
        }
        finish(a, b, r)
    } else {
        finish(a, b, ordered(a, b))
    }
}

fn finish(a: &SurfaceFeature, b: &SurfaceFeature, mut r: MeasurementResult) -> MeasurementResult {
    r.distance_xyz = match (a, b) {
        (SurfaceFeature::Point(p), SurfaceFeature::Point(q)) => Some(*q - *p),
        (SurfaceFeature::Circle(c), SurfaceFeature::Circle(d)) => Some(d.center - c.center),
        _ => r.primary_distance().map(|d| d.to - d.from),
    };
    r
}

/// Обчислення для `rank(a) <= rank(b)`.
fn ordered(a: &SurfaceFeature, b: &SurfaceFeature) -> MeasurementResult {
    use SurfaceFeature as F;
    match (a, b) {
        (F::Point(p), F::Point(q)) => MeasurementResult { distance_strict: Some(dp(*p, *q)), ..Default::default() },
        (F::Point(p), F::Edge(e)) => point_edge(*p, e),
        (F::Point(p), F::Circle(c)) => MeasurementResult {
            distance_strict: Some(point_circle_distance(*p, c)),
            center_distance: Some(dp(*p, c.center)),
            ..Default::default()
        },
        (F::Point(p), F::Plane(pl)) => MeasurementResult {
            distance_infinite: Some(point_plane(*p, pl.normal, pl.origin)),
            ..Default::default()
        },
        (F::Edge(e), F::Edge(f)) => edge_edge(e, f),
        (F::Edge(e), F::Circle(c)) => edge_circle(e, c),
        (F::Edge(e), F::Plane(pl)) => edge_plane(e, pl),
        (F::Circle(c), F::Circle(d)) => circle_circle(c, d),
        (F::Circle(c), F::Plane(pl)) => circle_plane(c, pl),
        (F::Plane(p), F::Plane(q)) => plane_plane(p, q),
        _ => unreachable!("пари впорядковані за рангом"),
    }
}

// ───────────────────────── відстані ─────────────────────────

fn point_plane(p: DVec3, n: DVec3, o: DVec3) -> DistAndPoints {
    let signed = (p - o).dot(n);
    DistAndPoints { dist: signed.abs(), from: p, to: p - n * signed }
}

/// Відстань від точки до обідка кола (до найближчої точки на колі).
pub(super) fn point_circle_distance(p: DVec3, c: &Circle) -> DistAndPoints {
    let d = p - c.center;
    let in_plane = d - c.normal * d.dot(c.normal);
    let rho = in_plane.length();
    let dir = if rho < 1e-12 { orthogonal(c.normal) } else { in_plane / rho };
    dp(p, c.center + dir * c.radius)
}

fn point_edge(p: DVec3, e: &Edge) -> MeasurementResult {
    let dir = e.b - e.a;
    let len2 = dir.length_squared();
    if len2 < 1e-24 {
        return MeasurementResult { distance_strict: Some(dp(p, e.a)), ..Default::default() };
    }
    let t = (p - e.a).dot(dir) / len2;
    let on_line = e.a + dir * t;
    MeasurementResult {
        distance_infinite: Some(dp(p, on_line)),
        distance_strict: Some(dp(p, e.a + dir * t.clamp(0.0, 1.0))),
        ..Default::default()
    }
}

fn edge_edge(e: &Edge, f: &Edge) -> MeasurementResult {
    let (d1, d2) = (e.direction(), f.direction());
    let (c1, c2) = segment_segment(e.a, e.b, f.a, f.b);
    let strict = Some(dp(c1, c2));
    if are_parallel(d1, d2) {
        let mid = e.midpoint();
        let on_line = f.a + d2 * (mid - f.a).dot(d2);
        return MeasurementResult {
            distance_infinite: Some(dp(mid, on_line)),
            distance_strict: strict,
            infinite_is_primary: true,
            ..Default::default()
        };
    }
    let (l1, l2) = line_line(e.a, d1, f.a, d2).expect("непаралельні прямі перетинаються/мимобіжні");
    MeasurementResult {
        distance_infinite: Some(dp(l1, l2)),
        distance_strict: strict,
        angle: Some(oriented_angle(l1, l2, e, f)),
        ..Default::default()
    }
}

/// Кут між прямими ребер; напрямки — від вершини до дальшого кінця кожного ребра.
fn oriented_angle(on_first: DVec3, on_second: DVec3, e: &Edge, f: &Edge) -> AngleInfo {
    let away = |apex: DVec3, edge: &Edge| {
        let far = if (edge.a - apex).length_squared() > (edge.b - apex).length_squared() { edge.a } else { edge.b };
        let d = (far - apex).normalize_or_zero();
        if d == DVec3::ZERO { edge.direction() } else { d }
    };
    let (dir_a, dir_b) = (away(on_first, e), away(on_second, f));
    AngleInfo {
        radians: dir_a.dot(dir_b).clamp(-1.0, 1.0).acos(),
        apex: (on_first + on_second) * 0.5,
        dir_a,
        dir_b,
    }
}

/// Скінченний відрізок проти кола: мінімум відстані від точок відрізка до обідка.
/// `from` на відрізку, `to` на колі.
fn segment_circle(a: DVec3, b: DVec3, c: &Circle) -> DistAndPoints {
    let at = |t: f64| a + (b - a) * t;
    let f = |t: f64| point_circle_distance(at(t), c).dist;
    let (t, _) = minimize(&f, 0.0, 1.0, 64, false);
    point_circle_distance(at(t), c)
}

fn edge_circle(e: &Edge, c: &Circle) -> MeasurementResult {
    let center_on_edge = closest_on_segment(c.center, e.a, e.b);
    MeasurementResult {
        distance_strict: Some(segment_circle(e.a, e.b, c)),
        center_distance: Some(dp(center_on_edge, c.center)),
        angle: angle_line_plane(e.a, e.b, c.normal, c.center),
        ..Default::default()
    }
}

/// Мінімальна відстань обідок–обідок: `from` на `c0`, `to` на `c1`.
fn rim_rim(c0: &Circle, c1: &Circle) -> DistAndPoints {
    if are_parallel(c0.normal, c1.normal) {
        // Замкнена форма: радіальний зазор g у площині й осьовий зсув h дають √(g²+h²).
        let d = c1.center - c0.center;
        let h = c0.normal.dot(d);
        let comp = d - c0.normal * h;
        let dist = comp.length();
        let u = if dist > 1e-12 { comp / dist } else { orthogonal(c0.normal) };
        let (p0, p1);
        if dist - c1.radius >= c0.radius {
            p0 = c0.center + u * c0.radius;
            p1 = c1.center - u * c1.radius;
        } else if dist + c1.radius <= c0.radius {
            p0 = c0.center + u * c0.radius;
            p1 = c1.center + u * c1.radius;
        } else if dist - c1.radius <= -c0.radius {
            p0 = c0.center - u * c0.radius;
            p1 = c1.center - u * c1.radius;
        } else {
            // Кола перетинаються в проєкції: точки перетину (g = 0).
            let x = (dist * dist + c0.radius * c0.radius - c1.radius * c1.radius) / (2.0 * dist.max(1e-12));
            let y = (c0.radius * c0.radius - x * x).max(0.0).sqrt();
            let w = c0.normal.cross(u);
            p0 = c0.center + u * x + w * y;
            p1 = p0 + c0.normal * h;
        }
        return dp(p0, p1);
    }
    // Загальний випадок: параметризуємо c1 кутом, відстань точка–коло c0 — точна.
    let [u1, v1, _] = orthonormal_basis(c1.normal);
    let at = |th: f64| c1.center + (u1 * th.cos() + v1 * th.sin()) * c1.radius;
    let f = |th: f64| point_circle_distance(at(th), c0).dist;
    let (th, _) = minimize(&f, 0.0, std::f64::consts::TAU, 720, true);
    swapped(point_circle_distance(at(th), c0))
}

fn circle_circle(c0: &Circle, c1: &Circle) -> MeasurementResult {
    MeasurementResult {
        distance_strict: Some(rim_rim(c0, c1)),
        center_distance: Some(dp(c0.center, c1.center)),
        angle: angle_plane_plane(c0.normal, c0.center, c1.normal, c1.center),
        ..Default::default()
    }
}

/// Мінімальна відстань від скінченного відрізка до контуру площини: `from` на відрізку.
fn segment_to_outline(a: DVec3, b: DVec3, pl: &Plane) -> Option<DistAndPoints> {
    let edges = pl.outline.edges.iter().map(|o| {
        let (p, q) = segment_segment(a, b, o.a, o.b);
        dp(p, q)
    });
    let circles = pl.outline.circles.iter().map(|c| segment_circle(a, b, c));
    edges.chain(circles).min_by(|x, y| x.dist.total_cmp(&y.dist))
}

fn edge_plane(e: &Edge, pl: &Plane) -> MeasurementResult {
    let dir = e.direction();
    let angle = angle_line_plane(e.a, e.b, pl.normal, pl.origin);
    if are_perpendicular(dir, pl.normal) {
        // Ребро паралельне площині: відстань однакова скрізь, малюємо від середини.
        return MeasurementResult {
            distance_infinite: Some(point_plane(e.midpoint(), pl.normal, pl.origin)),
            infinite_is_primary: true,
            angle,
            ..Default::default()
        };
    }
    let inter = line_plane_intersection(e.a, e.b, pl.normal, pl.origin);
    MeasurementResult {
        distance_infinite: inter.map(|i| dp(i, i)),
        distance_strict: segment_to_outline(e.a, e.b, pl),
        angle,
        ..Default::default()
    }
}

fn circle_plane(c: &Circle, pl: &Plane) -> MeasurementResult {
    let to_plane = point_plane(c.center, pl.normal, pl.origin);
    let parallel = are_parallel(c.normal, pl.normal);
    let mut r = MeasurementResult {
        distance_infinite: Some(to_plane),
        center_distance: Some(to_plane),
        angle: angle_plane_plane(c.normal, c.center, pl.normal, pl.origin),
        infinite_is_primary: parallel,
        ..Default::default()
    };
    if parallel && to_plane.dist < 1e-6 {
        // Коло лежить у площині (Orca: strict = 0).
        r.distance_strict = Some(dp(c.center, to_plane.to));
    } else if !parallel {
        let edges = pl.outline.edges.iter().map(|o| swapped(segment_circle(o.a, o.b, c)));
        let circles = pl.outline.circles.iter().map(|o| rim_rim(c, o));
        r.distance_strict = edges.chain(circles).min_by(|x, y| x.dist.total_cmp(&y.dist));
    }
    r
}

fn plane_plane(p: &Plane, q: &Plane) -> MeasurementResult {
    if are_parallel(p.normal, q.normal) {
        return MeasurementResult {
            distance_infinite: Some(point_plane(p.origin, q.normal, q.origin)),
            infinite_is_primary: true,
            ..Default::default()
        };
    }
    MeasurementResult {
        angle: angle_plane_plane(p.normal, p.origin, q.normal, q.origin),
        ..Default::default()
    }
}

// ───────────────────────── кути ─────────────────────────

/// Кут між прямою `a→b` і площиною `(n, o)` — між ребром і його проєкцією на площину.
/// Напрямок ребра — від точки перетину до дальшого кінця. `None`, якщо пряма ∥ площині.
fn angle_line_plane(a: DVec3, b: DVec3, n: DVec3, o: DVec3) -> Option<AngleInfo> {
    let dir = (b - a).normalize_or_zero();
    if are_perpendicular(dir, n) {
        return None;
    }
    let inter = line_plane_intersection(a, b, n, o)?;
    let far = if (a - inter).length_squared() > (b - inter).length_squared() { a } else { b };
    let mut d = (far - inter).normalize_or_zero();
    if d == DVec3::ZERO {
        d = dir;
    }
    let (on_plane, radians) = if are_parallel(d, n) {
        (orthogonal(n), std::f64::consts::FRAC_PI_2)
    } else {
        let proj = (d - n * d.dot(n)).normalize_or_zero();
        (proj, d.dot(proj).clamp(-1.0, 1.0).acos())
    };
    Some(AngleInfo { radians, apex: inter, dir_a: d, dir_b: on_plane })
}

/// Двогранний кут між півплощинами, що несуть точки `o1` і `o2` (як `angle_plane_plane`
/// в Orca). `None` для паралельних площин.
fn angle_plane_plane(n1: DVec3, o1: DVec3, n2: DVec3, o2: DVec3) -> Option<AngleInfo> {
    if are_parallel(n1, n2) {
        return None;
    }
    let l = n1.cross(n2);
    let l2 = l.length_squared();
    let (d1, d2) = (n1.dot(o1), n2.dot(o2));
    // Точка на лінії перетину площин.
    let q = (n2.cross(l) * d1 + l.cross(n1) * d2) / l2;
    let ldir = l / l2.sqrt();
    let toward = |o: DVec3, n: DVec3| {
        let v = o - q;
        let perp = v - ldir * v.dot(ldir);
        if perp.length() > 1e-9 { perp.normalize() } else { n.cross(ldir).normalize_or_zero() }
    };
    let (u1, u2) = (toward(o1, n1), toward(o2, n2));
    // Вершина — проєкція середини між центрами на лінію перетину.
    let mid = (o1 + o2) * 0.5;
    let apex = q + ldir * (mid - q).dot(ldir);
    Some(AngleInfo { radians: u1.dot(u2).clamp(-1.0, 1.0).acos(), apex, dir_a: u1, dir_b: u2 })
}
