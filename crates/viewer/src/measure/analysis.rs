//! Аналіз сітки як у Orca `Measure.cpp` (`MeasuringImpl`): зварювання вершин, сусідство
//! граней, кластери компланарних граней, межові контури й виявлення кіл/дуг/ребер.
//!
//! Усе в локальних координатах моделі (f64). Контури й ознаки площини рахуються ліниво,
//! при першому зверненні (`Topology::borders`, `extract_features`).

use std::{
    collections::HashMap,
    hash::{BuildHasherDefault, Hasher},
};

use glam::DVec3;

use super::{
    Circle, Edge,
    geom::{approx_eq, orthonormal_basis},
};

/// Допуск збігу нормалей по кожній компоненті (Orca `is_same_normal`).
pub const NORMAL_TOLERANCE: f64 = 0.001;
/// Допуск «однакових» кутів і довжин при пошуку дуг (Orca `are_angles_same`/`are_lengths_same`).
const SAME_TOLERANCE: f64 = 0.01;
/// Макс. відхилення точок контуру від прилеглого кола, мм.
const CIRCLE_FIT_ERROR: f64 = 0.05;

// ───────────────────────── топологія ─────────────────────────

#[derive(Default)]
struct Fx(u64);

impl Hasher for Fx {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u32(b as u32);
        }
    }
    fn write_u32(&mut self, i: u32) {
        self.0 = (self.0.rotate_left(5) ^ i as u64).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

pub struct Topology {
    pub verts: Vec<DVec3>,
    pub faces: Vec<[u32; 3]>,
    /// `neighbors[f][k]` — грань за ребром `faces[f][k] → faces[f][(k+1)%3]`, або -1
    /// (межа, неманіфолдне ребро чи виродження).
    pub neighbors: Vec<[i32; 3]>,
    pub face_plane: Vec<u32>,
    pub plane_normal: Vec<DVec3>,
    pub plane_area: Vec<f64>,
    plane_offsets: Vec<u32>,
    plane_facets: Vec<u32>,
}

/// Результат обходу межі площини.
#[derive(Default)]
pub struct Borders {
    /// Замкнені контури вершин (без повторення першої в кінці), проти годинникової
    /// відносно нормалі для зовнішнього контуру.
    pub loops: Vec<Vec<DVec3>>,
    /// Відрізки меж, що не склалися в замкнений контур (відкрита/биту сітка).
    pub open_edges: Vec<(DVec3, DVec3)>,
}

pub struct PlaneFeatures {
    /// Кола й ребра в порядку обходу контурів (як `surface_features` в Orca, без площини).
    pub edges: Vec<Edge>,
    pub circles: Vec<Circle>,
    /// Центр ваги меж (або центроїд граней, якщо меж нема).
    pub origin: DVec3,
}

impl Topology {
    /// `indices` — трикутники в `positions`; для «супу» без індексів передайте 0,1,2,3,…
    pub fn new(positions: &[[f32; 3]], indices: &[[u32; 3]]) -> Self {
        // Зварювання вершин за точним збігом бітів f32 (−0.0 зводимо до +0.0).
        let mut map: HashMap<(u32, u32, u32), u32, BuildHasherDefault<Fx>> =
            HashMap::with_capacity_and_hasher(positions.len() / 2 + 1, Default::default());
        let mut verts: Vec<DVec3> = Vec::with_capacity(positions.len() / 2 + 1);
        let remap: Vec<u32> = positions
            .iter()
            .map(|p| {
                let key = ((p[0] + 0.0).to_bits(), (p[1] + 0.0).to_bits(), (p[2] + 0.0).to_bits());
                *map.entry(key).or_insert_with(|| {
                    verts.push(DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64));
                    verts.len() as u32 - 1
                })
            })
            .collect();
        let faces: Vec<[u32; 3]> = indices
            .iter()
            .map(|t| [remap[t[0] as usize], remap[t[1] as usize], remap[t[2] as usize]])
            .collect();

        let nf = faces.len();
        let neighbors = build_neighbors(&faces);

        // Нормалі граней.
        let mut normals = Vec::with_capacity(nf);
        let mut areas = Vec::with_capacity(nf);
        for f in &faces {
            let (a, b, c) = (verts[f[0] as usize], verts[f[1] as usize], verts[f[2] as usize]);
            let n = (b - a).cross(c - a);
            let len = n.length();
            if len > 1e-12 {
                normals.push(n / len);
                areas.push(len * 0.5);
            } else {
                normals.push(DVec3::ZERO);
                areas.push(0.0);
            }
        }

        // Кластеризація: зв'язні множини граней із нормаллю, що збігається з нормаллю зерна.
        let mut face_plane = vec![u32::MAX; nf];
        let mut plane_normal = Vec::new();
        let mut plane_area = Vec::new();
        let mut plane_offsets = vec![0u32];
        let mut plane_facets: Vec<u32> = Vec::with_capacity(nf);
        let mut stack: Vec<u32> = Vec::new();
        for seed in 0..nf {
            if face_plane[seed] != u32::MAX {
                continue;
            }
            let pid = plane_normal.len() as u32;
            let seed_n = normals[seed];
            let mut area = areas[seed];
            face_plane[seed] = pid;
            plane_facets.push(seed as u32);
            stack.push(seed as u32);
            while let Some(f) = stack.pop() {
                for &nb in &neighbors[f as usize] {
                    if nb < 0 || face_plane[nb as usize] != u32::MAX {
                        continue;
                    }
                    let nn = normals[nb as usize];
                    if (nn.x - seed_n.x).abs() < NORMAL_TOLERANCE
                        && (nn.y - seed_n.y).abs() < NORMAL_TOLERANCE
                        && (nn.z - seed_n.z).abs() < NORMAL_TOLERANCE
                    {
                        face_plane[nb as usize] = pid;
                        plane_facets.push(nb as u32);
                        area += areas[nb as usize];
                        stack.push(nb as u32);
                    }
                }
            }
            plane_normal.push(seed_n);
            plane_area.push(area);
            plane_offsets.push(plane_facets.len() as u32);
        }

        Self { verts, faces, neighbors, face_plane, plane_normal, plane_area, plane_offsets, plane_facets }
    }

    pub fn plane_count(&self) -> usize {
        self.plane_normal.len()
    }

    pub fn plane_facets(&self, plane: usize) -> &[u32] {
        &self.plane_facets[self.plane_offsets[plane] as usize..self.plane_offsets[plane + 1] as usize]
    }

    /// Обхід меж площини через напівребра (аналог циклу з `next_around_target` в Orca).
    pub fn borders(&self, plane: usize) -> Borders {
        let pid = plane as u32;
        let facets = self.plane_facets(plane);
        let is_border = |f: u32, k: usize| {
            let nb = self.neighbors[f as usize][k];
            nb < 0 || self.face_plane[nb as usize] != pid
        };
        let mut border: Vec<u32> = Vec::new(); // f*3+k
        for &f in facets {
            for k in 0..3 {
                let fv = self.faces[f as usize];
                if fv[k] != fv[(k + 1) % 3] && is_border(f, k) {
                    border.push(f * 3 + k as u32);
                }
            }
        }
        border.sort_unstable();
        let mut visited = vec![false; border.len()];
        let mut out = Borders::default();
        let guard = 3 * facets.len() + 8;

        // Наступне межове напівребро за обходом навколо кінцевої вершини.
        let next_border = |he: u32| -> Option<u32> {
            let (f, k) = ((he / 3) as usize, (he % 3) as usize);
            let v = self.faces[f][(k + 1) % 3];
            let (mut cf, mut cs) = (f, (k + 1) % 3);
            for _ in 0..guard {
                let nb = self.neighbors[cf][cs];
                if nb < 0 || self.face_plane[nb as usize] != pid {
                    return Some(cf as u32 * 3 + cs as u32);
                }
                let c = self.faces[cf][(cs + 1) % 3];
                let nf = self.faces[nb as usize];
                let t = (0..3).find(|&t| nf[t] == c && nf[(t + 1) % 3] == v)?;
                cf = nb as usize;
                cs = (t + 1) % 3;
            }
            None
        };

        for start in 0..border.len() {
            if visited[start] {
                continue;
            }
            let mut chain: Vec<u32> = Vec::new();
            let mut idx = start;
            let closed = loop {
                visited[idx] = true;
                chain.push(border[idx]);
                let Some(next) = next_border(border[idx]) else { break false };
                if next == border[start] {
                    break true;
                }
                match border.binary_search(&next) {
                    Ok(j) if !visited[j] => idx = j,
                    _ => break false,
                }
            };
            let src = |he: u32| self.faces[(he / 3) as usize][(he % 3) as usize] as usize;
            let dst = |he: u32| self.faces[(he / 3) as usize][((he % 3) as usize + 1) % 3] as usize;
            if closed && chain.len() >= 3 {
                out.loops.push(chain.iter().map(|&he| self.verts[src(he)]).collect());
            } else {
                out.open_edges
                    .extend(chain.iter().map(|&he| (self.verts[src(he)], self.verts[dst(he)])));
            }
        }
        out
    }

    /// Ознаки площини: кола, дуги, ребра межі (порт `MeasuringImpl::extract_features`).
    pub fn extract_features(&self, plane: usize) -> PlaneFeatures {
        let normal = self.plane_normal[plane];
        let borders = self.borders(plane);
        let mut edges = Vec::new();
        let mut circles = Vec::new();
        for border in &borders.loops {
            extract_loop(border, normal, &mut edges, &mut circles);
        }
        for &(a, b) in &borders.open_edges {
            edges.push(Edge { a, b, center: None });
        }

        let (mut cog, mut cnt) = (DVec3::ZERO, 0usize);
        for p in borders.loops.iter().flatten() {
            cog += *p;
            cnt += 1;
        }
        for (a, b) in &borders.open_edges {
            cog += *a + *b;
            cnt += 2;
        }
        let origin = if cnt > 0 {
            cog / cnt as f64
        } else {
            // Без меж (замкнена поверхня з однією площиною — вироджений випадок): центроїд.
            let facets = self.plane_facets(plane);
            let mut c = DVec3::ZERO;
            for &f in facets {
                let v = self.faces[f as usize];
                c += (self.verts[v[0] as usize] + self.verts[v[1] as usize] + self.verts[v[2] as usize]) / 3.0;
            }
            c / facets.len().max(1) as f64
        };
        PlaneFeatures { edges, circles, origin }
    }
}

fn build_neighbors(faces: &[[u32; 3]]) -> Vec<[i32; 3]> {
    let mut he: Vec<(u64, u32)> = Vec::with_capacity(faces.len() * 3);
    for (f, v) in faces.iter().enumerate() {
        for k in 0..3 {
            let (a, b) = (v[k], v[(k + 1) % 3]);
            if a != b {
                let (lo, hi) = if a < b { (a, b) } else { (b, a) };
                he.push((((lo as u64) << 32) | hi as u64, (f * 3 + k) as u32));
            }
        }
    }
    he.sort_unstable();
    let mut neighbors = vec![[-1i32; 3]; faces.len()];
    let mut i = 0;
    while i < he.len() {
        let mut j = i + 1;
        while j < he.len() && he[j].0 == he[i].0 {
            j += 1;
        }
        // Лише рівно два напівребра => маніфолдне ребро. Решта — межа.
        if j - i == 2 {
            let (fa, ka) = ((he[i].1 / 3) as usize, (he[i].1 % 3) as usize);
            let (fb, kb) = ((he[i + 1].1 / 3) as usize, (he[i + 1].1 % 3) as usize);
            if fa != fb {
                neighbors[fa][ka] = fb as i32;
                neighbors[fb][kb] = fa as i32;
            }
        }
        i = j;
    }
    neighbors
}

// ───────────────────────── кола, дуги, ребра ─────────────────────────

/// Кола за найменшими квадратами (Каса) у площині нормалі. Повертає (центр, радіус, похибка):
/// радіус — середня відстань, похибка — макс. відхилення від нього (як у Orca `circle_ransac`,
/// але детерміновано й без випадкових вибірок).
fn fit_circle(points: &[DVec3], normal: DVec3) -> Option<(DVec3, f64, f64)> {
    let n = points.len();
    if n < 3 {
        return None;
    }
    let [bx, by, bn] = orthonormal_basis(normal);
    let xy: Vec<(f64, f64)> = points.iter().map(|p| (p.dot(bx), p.dot(by))).collect();
    let z = points.iter().map(|p| p.dot(bn)).sum::<f64>() / n as f64;
    let (mx, my) = (
        xy.iter().map(|p| p.0).sum::<f64>() / n as f64,
        xy.iter().map(|p| p.1).sum::<f64>() / n as f64,
    );
    let (mut suu, mut suv, mut svv, mut suuu, mut svvv, mut suvv, mut svuu) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for &(x, y) in &xy {
        let (u, v) = (x - mx, y - my);
        suu += u * u;
        suv += u * v;
        svv += v * v;
        suuu += u * u * u;
        svvv += v * v * v;
        suvv += u * v * v;
        svuu += v * u * u;
    }
    let det = suu * svv - suv * suv;
    if det.abs() < 1e-12 * (suu * svv).abs().max(1e-30) {
        return None;
    }
    let (r1, r2) = (0.5 * (suuu + suvv), 0.5 * (svvv + svuu));
    let uc = (r1 * svv - r2 * suv) / det;
    let vc = (suu * r2 - suv * r1) / det;
    let (cx, cy) = (mx + uc, my + vc);
    let dists: Vec<f64> = xy.iter().map(|&(x, y)| ((x - cx).powi(2) + (y - cy).powi(2)).sqrt()).collect();
    let radius = dists.iter().sum::<f64>() / n as f64;
    let err = dists.iter().fold(0.0f64, |m, d| m.max((d - radius).abs()));
    Some((bx * cx + by * cy + bn * z, radius, err))
}

fn same(a: f64, b: f64) -> bool {
    (a - b).abs() < SAME_TOLERANCE
}

/// Порт гілки обробки одного контуру з `MeasuringImpl::extract_features`.
fn extract_loop(border: &[DVec3], normal: DVec3, edges_out: &mut Vec<Edge>, circles_out: &mut Vec<Circle>) {
    let n = border.len() as i32;
    if n <= 1 {
        return;
    }
    let mut done = false;

    // Весь контур — одне коло (>8 вершин) або правильний багатокутник (5..=8).
    if n > 4
        && let Some((center, radius, err)) = fit_circle(border, normal)
        && err < CIRCLE_FIT_ERROR
    {
        let is_polygon = n <= 8;
        let lengths_match = (2..border.len()).all(|i| {
            let l1 = (border[i] - border[i - 1]).length_squared();
            let l0 = (border[i - 1] - border[i - 2]).length_squared();
            (l1 - l0).abs() < SAME_TOLERANCE
        });
        if lengths_match {
            if is_polygon {
                for j in 0..border.len() {
                    let prev = if j == 0 { border.len() - 1 } else { j - 1 };
                    edges_out.push(Edge { a: border[prev], b: border[j], center: Some(center) });
                }
            } else {
                circles_out.push(Circle { center, normal, radius });
            }
            done = true;
        }
    }
    if done {
        return;
    }

    // Контур не коло цілком: шукаємо кругові сегменти (послідовності вершин з однаковим кутом).
    let off = |idx: i32, o: i32| -> i32 {
        let out = idx + o;
        if out >= n {
            out - n
        } else if out < 0 {
            n + out
        } else {
            out
        }
    };
    let at = |i: i32| border[i as usize];

    let mut angles: Vec<f64> = Vec::with_capacity(border.len());
    let mut lengths: Vec<f64> = Vec::with_capacity(border.len());
    let mut first_different = 0usize;
    for i in 0..n {
        let v2 = at(i) - at(off(i, -1));
        let v1 = at(off(i, 1)) - at(i);
        let mut angle = (-normal.dot(v1.cross(v2))).atan2(-v1.dot(v2)) + std::f64::consts::PI;
        if angle > std::f64::consts::PI {
            angle = 2.0 * std::f64::consts::PI - angle;
        }
        angles.push(angle);
        lengths.push(v2.length());
        if first_different == 0 && angles.len() > 1 && !same(angle, angles[angles.len() - 2]) {
            first_different = angles.len() - 1;
        }
    }

    let mut circles: Vec<Circle> = Vec::new();
    let mut circle_idxs: Vec<(i32, i32)> = Vec::new();
    let mut start_idx = -1i32;
    let mut in_circle = false;
    let mut single: Vec<DVec3> = Vec::new();
    let mut single_len = 0.0;
    let first_pt = off(first_different as i32, 1);
    let mut i = first_pt;
    let mut first_iter = true;
    while i != first_pt || first_iter {
        if same(angles[i as usize], angles[off(i, -1) as usize])
            && i != off(first_pt, -1)
            && i != start_idx
        {
            if !in_circle {
                in_circle = true;
                single.clear();
                single_len = 0.0;
                start_idx = off(i, -2);
                single.push(at(start_idx));
                single.push(at(off(start_idx, 1)));
                single_len += lengths[off(i, -1) as usize];
            }
            single.push(at(i));
            single_len += lengths[i as usize];
        } else {
            if in_circle && single.len() >= 5 {
                single.push(at(i));
                single_len += lengths[i as usize];
                // Довжини внутрішніх ребер мають збігатися.
                let mut accept = true;
                let mut j = off(start_idx, 3);
                while j != i {
                    if !same(lengths[off(j, -1) as usize], lengths[j as usize]) {
                        accept = false;
                        break;
                    }
                    j = off(j, 1);
                }
                if accept && let Some((center, radius, err)) = fit_circle(&single, normal) {
                    // Дуга менше ~81° (0.9·π/2) відкидається.
                    if err < CIRCLE_FIT_ERROR && single_len / radius > 0.9 * std::f64::consts::FRAC_PI_2 {
                        circle_idxs.push((start_idx, i));
                        circles.push(Circle { center, normal, radius });
                    }
                }
            }
            in_circle = false;
        }
        first_iter = false;
        i = off(i, 1);
    }

    // Решта — ребра, обходячи кола.
    let mut edges: Vec<Edge> = Vec::new();
    let edge = |a: DVec3, b: DVec3| Edge { a, b, center: None };
    if circle_idxs.is_empty() {
        for i in 1..border.len() {
            edges.push(edge(border[i - 1], border[i]));
        }
        edges.push(edge(border[border.len() - 1], border[0]));
    } else if circle_idxs.len() > 1 || circle_idxs[0].0 != circle_idxs[0].1 {
        let mut i = circle_idxs[0].1;
        let mut circle_idx = 1usize;
        let mut budget = 2 * border.len() + 4;
        loop {
            i = off(i, 1);
            edges.push(edge(at(off(i, -1)), at(i)));
            if circle_idx < circle_idxs.len() && i == circle_idxs[circle_idx].0 {
                i = circle_idxs[circle_idx].1;
                circle_idx += 1;
            }
            budget -= 1;
            if i == circle_idxs[0].0 || budget == 0 {
                break;
            }
        }
    }

    // Зливаємо сусідні співнаправлені ребра.
    let mut i = edges.len() as isize - 1;
    while i >= 0 && edges.len() >= 2 {
        let iu = i as usize;
        let pi = if iu == 0 { edges.len() - 1 } else { iu - 1 };
        let (first, second) = (edges[pi], edges[iu]);
        if approx_eq(first.b, second.a)
            && ((first.b - first.a).normalize_or_zero().dot((second.b - second.a).normalize_or_zero()) - 1.0).abs()
                < 1e-6
        {
            edges[pi] = edge(first.a, second.b);
            edges.remove(iu);
        }
        i -= 1;
    }

    circles_out.extend(circles);
    edges_out.extend(edges);
}
