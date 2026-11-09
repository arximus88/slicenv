//! Опуклий корпус інстансу: плоскі грані, на які модель може лягти (режим «Опуклий корпус»
//! інструмента «покласти на грань», як у Bambu Studio).

use anyhow::Result;

use crate::{DVec3, InstanceRef, Project, VolumeKind};

/// Найменша площа грані корпуса (мм²), на яку пропонуємо покласти модель.
const MIN_FACE_AREA: f64 = 4.0;

/// Плоска грань корпуса: зовнішня нормаль і трикутники у світових координатах.
#[derive(Clone, Debug)]
pub struct HullPlane {
    pub normal: DVec3,
    pub triangles: Vec<[DVec3; 3]>,
    pub area: f64,
}

struct Face {
    v: [usize; 3],
    /// Сусід через ребро `v[e] → v[e + 1]`.
    nbr: [usize; 3],
    normal: DVec3,
    offset: f64,
    outside: Vec<usize>,
    alive: bool,
    /// Номер обходу, у якому грань уже визнано видимою.
    mark: u32,
}

impl Face {
    fn new(points: &[DVec3], v: [usize; 3], nbr: [usize; 3]) -> Self {
        let [a, b, c] = v.map(|i| points[i]);
        let normal = (b - a).cross(c - a).normalize_or_zero();
        Self {
            v,
            nbr,
            normal,
            offset: normal.dot(a),
            outside: Vec::new(),
            alive: true,
            mark: 0,
        }
    }

    fn distance(&self, p: DVec3) -> f64 {
        self.normal.dot(p) - self.offset
    }

    /// Індекс ребра `from → to`, якщо грань його має.
    fn edge(&self, from: usize, to: usize) -> Option<usize> {
        (0..3).find(|&e| self.v[e] == from && self.v[(e + 1) % 3] == to)
    }
}

/// Quickhull із суміжністю граней: трикутники корпусу зовнішньою орієнтацією (проти годинникової,
/// якщо дивитись ззовні). Порожній результат — точки вироджені (на прямій чи в площині).
pub fn convex_hull(points: &[DVec3]) -> Vec<[usize; 3]> {
    if points.len() < 4 {
        return Vec::new();
    }
    let (mut lo, mut hi) = (DVec3::MAX, DVec3::MIN);
    for p in points {
        lo = lo.min(*p);
        hi = hi.max(*p);
    }
    // Допуск ~1e-4 мм на столі: достатньо, щоб не плодити грані з майже копланарних точок.
    let eps = 1e-6 * (hi - lo).max_element().max(1.0);

    // Початковий тетраедр із найвіддаленіших точок.
    let mut extremes = Vec::with_capacity(6);
    for axis in 0..3 {
        let min = (0..points.len()).min_by(|&a, &b| points[a][axis].total_cmp(&points[b][axis]));
        let max = (0..points.len()).max_by(|&a, &b| points[a][axis].total_cmp(&points[b][axis]));
        extremes.extend(min.into_iter().chain(max));
    }
    let mut pair = (extremes[0], extremes[1], 0.0_f64);
    for &a in &extremes {
        for &b in &extremes {
            let d = points[a].distance_squared(points[b]);
            if d > pair.2 {
                pair = (a, b, d);
            }
        }
    }
    let (i, j, d2) = pair;
    if d2.sqrt() < eps {
        return Vec::new();
    }
    let line = (points[j] - points[i]).normalize();
    let Some((k, dk)) = (0..points.len())
        .map(|n| (n, (points[n] - points[i]).cross(line).length()))
        .max_by(|a, b| a.1.total_cmp(&b.1))
    else {
        return Vec::new();
    };
    if dk < eps {
        return Vec::new();
    }
    let base = (points[j] - points[i]).cross(points[k] - points[i]).normalize();
    let Some((l, dl)) = (0..points.len())
        .map(|n| (n, base.dot(points[n] - points[i]).abs()))
        .max_by(|a, b| a.1.total_cmp(&b.1))
    else {
        return Vec::new();
    };
    if dl < eps {
        return Vec::new();
    }

    let centroid = (points[i] + points[j] + points[k] + points[l]) / 4.0;
    let mut faces: Vec<Face> = Vec::new();
    for v in [[i, j, k], [i, j, l], [j, k, l], [k, i, l]] {
        let mut face = Face::new(points, v, [0; 3]);
        if face.distance(centroid) > 0.0 {
            face = Face::new(points, [v[0], v[2], v[1]], [0; 3]);
        }
        faces.push(face);
    }
    for a in 0..4 {
        for e in 0..3 {
            let (from, to) = (faces[a].v[e], faces[a].v[(e + 1) % 3]);
            faces[a].nbr[e] = (0..4)
                .find(|&b| b != a && faces[b].edge(to, from).is_some())
                .expect("тетраедр замкнений");
        }
    }
    let simplex = [i, j, k, l];
    for n in (0..points.len()).filter(|n| !simplex.contains(n)) {
        if let Some(face) = faces.iter_mut().find(|f| f.distance(points[n]) > eps) {
            face.outside.push(n);
        }
    }

    let mut stack: Vec<usize> = (0..faces.len())
        .filter(|&f| !faces[f].outside.is_empty())
        .collect();
    let mut epoch = 0u32;
    while let Some(seed) = stack.pop() {
        if !faces[seed].alive || faces[seed].outside.is_empty() {
            continue;
        }
        let apex = *faces[seed]
            .outside
            .iter()
            .max_by(|&&a, &&b| {
                faces[seed]
                    .distance(points[a])
                    .total_cmp(&faces[seed].distance(points[b]))
            })
            .expect("непорожній");
        let p = points[apex];

        // Видимі з вершини грані — зв’язна область навколо `seed`; решта ребер — горизонт.
        epoch += 1;
        faces[seed].mark = epoch;
        let mut visible = vec![seed];
        let mut horizon: Vec<(usize, usize, usize)> = Vec::new();
        let mut at = 0;
        while at < visible.len() {
            let current = visible[at];
            at += 1;
            for e in 0..3 {
                let neighbour = faces[current].nbr[e];
                if faces[neighbour].mark == epoch {
                    continue;
                }
                if faces[neighbour].distance(p) > eps {
                    faces[neighbour].mark = epoch;
                    visible.push(neighbour);
                } else {
                    horizon.push((faces[current].v[e], faces[current].v[(e + 1) % 3], neighbour));
                }
            }
        }

        let mut orphans = Vec::new();
        for &f in &visible {
            faces[f].alive = false;
            orphans.extend(std::mem::take(&mut faces[f].outside).into_iter().filter(|&n| n != apex));
        }

        let first = faces.len();
        let mut starts = std::collections::HashMap::with_capacity(horizon.len());
        let mut ends = std::collections::HashMap::with_capacity(horizon.len());
        for (u, w, outer) in horizon {
            let id = faces.len();
            faces.push(Face::new(points, [u, w, apex], [outer, id, id]));
            if let Some(e) = faces[outer].edge(w, u) {
                faces[outer].nbr[e] = id;
            }
            starts.insert(u, id);
            ends.insert(w, id);
        }
        for id in first..faces.len() {
            let (u, w) = (faces[id].v[0], faces[id].v[1]);
            faces[id].nbr[1] = starts.get(&w).copied().unwrap_or(id);
            faces[id].nbr[2] = ends.get(&u).copied().unwrap_or(id);
        }
        for n in orphans {
            if let Some(face) = faces[first..]
                .iter_mut()
                .find(|f| f.distance(points[n]) > eps)
            {
                face.outside.push(n);
            }
        }
        stack.extend((first..faces.len()).filter(|&f| !faces[f].outside.is_empty()));
    }
    faces
        .into_iter()
        .filter(|f| f.alive && f.normal != DVec3::ZERO)
        .map(|f| f.v)
        .collect()
}

/// Корпус, згрупований у площини (сусідні копланарні трикутники — одна грань).
pub fn hull_planes(points: &[DVec3]) -> Vec<HullPlane> {
    let mut planes: Vec<(HullPlane, f64)> = Vec::new();
    for [a, b, c] in convex_hull(points)
        .into_iter()
        .map(|t| t.map(|i| points[i]))
    {
        let cross = (b - a).cross(c - a);
        let area = cross.length() / 2.0;
        let normal = cross.normalize_or_zero();
        if normal == DVec3::ZERO {
            continue;
        }
        let offset = normal.dot(a);
        match planes
            .iter_mut()
            .find(|(p, o)| p.normal.dot(normal) > 0.99995 && (o - offset).abs() < 0.02)
        {
            Some((plane, _)) => {
                plane.triangles.push([a, b, c]);
                plane.area += area;
            }
            None => planes.push((
                HullPlane {
                    normal,
                    triangles: vec![[a, b, c]],
                    area,
                },
                offset,
            )),
        }
    }
    planes
        .into_iter()
        .map(|(p, _)| p)
        .filter(|p| p.area > 1e-6)
        .collect()
}

/// Найближча до променя грань корпуса.
pub fn pick_plane(planes: &[HullPlane], origin: DVec3, dir: DVec3) -> Option<usize> {
    let mut best: Option<(f64, usize)> = None;
    for (index, plane) in planes.iter().enumerate() {
        for &[a, b, c] in &plane.triangles {
            // Möller–Trumbore.
            let (e1, e2) = (b - a, c - a);
            let p = dir.cross(e2);
            let det = e1.dot(p);
            if det.abs() < 1e-12 {
                continue;
            }
            let inv = 1.0 / det;
            let s = origin - a;
            let u = s.dot(p) * inv;
            if !(0.0..=1.0).contains(&u) {
                continue;
            }
            let q = s.cross(e1);
            let v = dir.dot(q) * inv;
            if v < 0.0 || u + v > 1.0 {
                continue;
            }
            let t = e2.dot(q) * inv;
            if t > 1e-9 && best.is_none_or(|(bt, _)| t < bt) {
                best = Some((t, index));
            }
        }
    }
    best.map(|(_, index)| index)
}

impl Project {
    /// Грані опуклого корпусу деталей інстансу у світових координатах.
    pub fn convex_hull_planes(&self, key: InstanceRef) -> Result<Vec<HullPlane>> {
        let object = self.object(key.object)?;
        let instance = self.instance(key)?;
        let mut points = Vec::new();
        for volume in object
            .volumes
            .iter()
            .filter(|v| v.kind == VolumeKind::Part)
        {
            let transform = instance.transform * volume.transform;
            points.extend(
                volume
                    .mesh
                    .positions
                    .iter()
                    .map(|p| transform.transform_point3(DVec3::from(*p))),
            );
        }
        let mut planes = hull_planes(&points);
        // Дрібні грані на заокругленнях не стійкі для друку й лише заважають навести на велику.
        let total: f64 = planes.iter().map(|p| p.area).sum();
        planes.retain(|p| p.area >= MIN_FACE_AREA.max(total * 0.002));
        Ok(planes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube_points() -> Vec<DVec3> {
        let mut points = Vec::new();
        for x in [0.0, 10.0] {
            for y in [0.0, 20.0] {
                for z in [0.0, 30.0] {
                    points.push(DVec3::new(x, y, z));
                }
            }
        }
        // Внутрішні точки й точки на гранях не повинні змінити корпус.
        points.push(DVec3::new(5.0, 10.0, 15.0));
        points.push(DVec3::new(5.0, 10.0, 0.0));
        points.push(DVec3::new(2.0, 3.0, 30.0));
        points
    }

    #[test]
    fn cube_has_six_planes_with_outward_normals() {
        let points = cube_points();
        let planes = hull_planes(&points);
        assert_eq!(planes.len(), 6);
        let centroid = DVec3::new(5.0, 10.0, 15.0);
        for plane in &planes {
            let [a, ..] = plane.triangles[0];
            assert!(plane.normal.dot(a - centroid) > 0.0, "нормаль усередину");
        }
        let total: f64 = planes.iter().map(|p| p.area).sum();
        assert!((total - 2.0 * (200.0 + 300.0 + 600.0)).abs() < 1e-6);
    }

    #[test]
    fn sphere_hull_contains_all_points() {
        let mut points = Vec::new();
        let n = 400;
        for k in 0..n {
            // Спіраль Фібоначчі.
            let t = (k as f64 + 0.5) / n as f64;
            let phi = (1.0 - 2.0 * t).acos();
            let theta = std::f64::consts::PI * (1.0 + 5f64.sqrt()) * k as f64;
            points.push(
                DVec3::new(
                    phi.sin() * theta.cos(),
                    phi.sin() * theta.sin(),
                    phi.cos(),
                ) * 25.0,
            );
        }
        let tris = convex_hull(&points);
        assert!(tris.len() > 100);
        for t in &tris {
            let [a, b, c] = t.map(|i| points[i]);
            let n = (b - a).cross(c - a).normalize();
            for p in &points {
                assert!(n.dot(*p - a) < 1e-6, "точка поза корпусом");
            }
        }
    }

    #[test]
    fn dense_sphere_hull_is_fast() {
        let mut points = Vec::new();
        let n = 20_000;
        for k in 0..n {
            let t = (k as f64 + 0.5) / n as f64;
            let phi = (1.0 - 2.0 * t).acos();
            let theta = std::f64::consts::PI * (1.0 + 5f64.sqrt()) * k as f64;
            points.push(
                DVec3::new(phi.sin() * theta.cos(), phi.sin() * theta.sin(), phi.cos()) * 25.0,
            );
        }
        let started = std::time::Instant::now();
        let tris = convex_hull(&points);
        eprintln!("hull {} tris in {:?}", tris.len(), started.elapsed());
        assert!(tris.len() > 1000);
    }

    #[test]
    fn flat_points_have_no_hull() {
        let points = [
            DVec3::ZERO,
            DVec3::X,
            DVec3::Y,
            DVec3::new(1.0, 1.0, 0.0),
        ];
        assert!(convex_hull(&points).is_empty());
    }

    #[test]
    fn picking_hits_the_nearest_plane() {
        let planes = hull_planes(&cube_points());
        let hit = pick_plane(&planes, DVec3::new(5.0, 10.0, 100.0), -DVec3::Z).unwrap();
        assert!(planes[hit].normal.dot(DVec3::Z) > 0.99);
    }
}
