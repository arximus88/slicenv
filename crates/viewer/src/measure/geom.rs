//! Чиста геометрія на `DVec3`: допуски паралельності, найближчі точки, 1D-мінімізація.

use glam::DVec3;

/// Допуск паралельності/перпендикулярності, рад (~0.057°). Вужчий за EPSILON Orca (1e-4 на
/// косинусі ≈ 0.8°), тож «майже паралельні» грані з нахилом >0.06° паралельними не вважаються.
pub const ANGLE_EPS: f64 = 1e-3;

/// Допуск збігу точок, мм.
pub const POINT_EPS: f64 = 1e-6;

pub fn are_parallel(a: DVec3, b: DVec3) -> bool {
    a.normalize_or_zero().cross(b.normalize_or_zero()).length() < ANGLE_EPS
}

pub fn are_perpendicular(a: DVec3, b: DVec3) -> bool {
    a.normalize_or_zero().dot(b.normalize_or_zero()).abs() < ANGLE_EPS
}

pub fn approx_eq(a: DVec3, b: DVec3) -> bool {
    (a - b).length() < POINT_EPS
}

/// Одиничний вектор, ортогональний до `v` (як `get_orthogonal` у MeasureUtils.hpp).
pub fn orthogonal(v: DVec3) -> DVec3 {
    let abs = v.abs();
    let (imax, inext) = if abs.x >= abs.y && abs.x >= abs.z {
        (0, 1)
    } else if abs.y >= abs.z {
        (1, 2)
    } else {
        (2, 0)
    };
    let mut r = DVec3::ZERO;
    r[imax] = v[inext];
    r[inext] = -v[imax];
    r.normalize_or_zero()
}

/// Ортонормований базис `[u, v, n]` для нормалі `n` (як `orthonormal_basis` в Orca).
pub fn orthonormal_basis(n: DVec3) -> [DVec3; 3] {
    let z = n.normalize_or_zero();
    let a = z.abs();
    let x = if a.x >= a.y && a.x >= a.z {
        DVec3::new(z.y, -z.x, 0.0)
    } else if a.y >= a.z {
        DVec3::new(0.0, z.z, -z.y)
    } else {
        DVec3::new(-z.z, 0.0, z.x)
    }
    .normalize_or_zero();
    let y = z.cross(x).normalize_or_zero();
    [x, y, z]
}

/// Найближча до `p` точка відрізка `a..b`.
pub fn closest_on_segment(p: DVec3, a: DVec3, b: DVec3) -> DVec3 {
    let d = b - a;
    let len2 = d.length_squared();
    if len2 < 1e-24 {
        return a;
    }
    a + d * ((p - a).dot(d) / len2).clamp(0.0, 1.0)
}

/// Найближчі точки двох відрізків (Ericson, Real-Time Collision Detection 5.1.9).
pub fn segment_segment(p1: DVec3, q1: DVec3, p2: DVec3, q2: DVec3) -> (DVec3, DVec3) {
    const EPS: f64 = 1e-24;
    let d1 = q1 - p1;
    let d2 = q2 - p2;
    let r = p1 - p2;
    let a = d1.length_squared();
    let e = d2.length_squared();
    let f = d2.dot(r);
    if a <= EPS && e <= EPS {
        return (p1, p2);
    }
    let (s, t);
    if a <= EPS {
        s = 0.0;
        t = (f / e).clamp(0.0, 1.0);
    } else {
        let c = d1.dot(r);
        if e <= EPS {
            t = 0.0;
            s = (-c / a).clamp(0.0, 1.0);
        } else {
            let b = d1.dot(d2);
            let denom = a * e - b * b;
            let mut s_ = if denom > 1e-12 * a * e {
                ((b * f - c * e) / denom).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let mut t_ = (b * s_ + f) / e;
            if t_ < 0.0 {
                t_ = 0.0;
                s_ = (-c / a).clamp(0.0, 1.0);
            } else if t_ > 1.0 {
                t_ = 1.0;
                s_ = ((b - c) / a).clamp(0.0, 1.0);
            }
            s = s_;
            t = t_;
        }
    }
    (p1 + d1 * s, p2 + d2 * t)
}

/// Найближчі точки двох нескінченних прямих; `None`, якщо вони паралельні.
pub fn line_line(p1: DVec3, d1: DVec3, p2: DVec3, d2: DVec3) -> Option<(DVec3, DVec3)> {
    let n = d1.cross(d2);
    let n2 = n.length_squared();
    if n2 < ANGLE_EPS * ANGLE_EPS * d1.length_squared() * d2.length_squared() {
        return None;
    }
    let r = p2 - p1;
    let t1 = r.cross(d2).dot(n) / n2;
    let t2 = r.cross(d1).dot(n) / n2;
    Some((p1 + d1 * t1, p2 + d2 * t2))
}

/// Перетин прямої `a→b` з площиною `(n, o)`; `None`, якщо пряма паралельна площині.
pub fn line_plane_intersection(a: DVec3, b: DVec3, n: DVec3, o: DVec3) -> Option<DVec3> {
    let d = b - a;
    let denom = d.dot(n);
    if denom.abs() < 1e-12 * d.length().max(1e-12) {
        return None;
    }
    Some(a + d * ((o - a).dot(n) / denom))
}

/// Мінімум `f` на `[lo, hi]`: груба вибірка + золотий переріз навколо кращих локальних
/// мінімумів. `cyclic` — `f` періодична з періодом `hi - lo` (кут кола). Повертає `(x, f(x))`.
pub fn minimize(f: &dyn Fn(f64) -> f64, lo: f64, hi: f64, samples: usize, cyclic: bool) -> (f64, f64) {
    let n = samples.max(3);
    let step = (hi - lo) / n as f64;
    let count = if cyclic { n } else { n + 1 };
    let xs: Vec<f64> = (0..count).map(|i| lo + step * i as f64).collect();
    let fs: Vec<f64> = xs.iter().map(|&x| f(x)).collect();
    let mut best = (xs[0], fs[0]);
    let mut minima: Vec<usize> = Vec::new();
    for i in 0..count {
        if fs[i] < best.1 {
            best = (xs[i], fs[i]);
        }
        let prev = if i > 0 {
            Some(i - 1)
        } else if cyclic {
            Some(count - 1)
        } else {
            None
        };
        let next = if i + 1 < count {
            Some(i + 1)
        } else if cyclic {
            Some(0)
        } else {
            None
        };
        if prev.is_none_or(|p| fs[i] <= fs[p]) && next.is_none_or(|q| fs[i] <= fs[q]) {
            minima.push(i);
        }
    }
    minima.sort_by(|&a, &b| fs[a].total_cmp(&fs[b]));
    minima.truncate(6);
    const INV_PHI: f64 = 0.618_033_988_749_894_9;
    for i in minima {
        let mut a = xs[i] - step;
        let mut b = xs[i] + step;
        if !cyclic {
            a = a.max(lo);
            b = b.min(hi);
        }
        let mut c = b - (b - a) * INV_PHI;
        let mut d = a + (b - a) * INV_PHI;
        let (mut fc, mut fd) = (f(c), f(d));
        for _ in 0..60 {
            if fc < fd {
                b = d;
                d = c;
                fd = fc;
                c = b - (b - a) * INV_PHI;
                fc = f(c);
            } else {
                a = c;
                c = d;
                fc = fd;
                d = a + (b - a) * INV_PHI;
                fd = f(d);
            }
        }
        let (x, fx) = if fc < fd { (c, fc) } else { (d, fd) };
        if fx < best.1 {
            best = (x, fx);
        }
    }
    best
}
