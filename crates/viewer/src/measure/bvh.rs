//! Компактний BVH над трикутниками (локальні координати) для променя наведення.

use glam::DVec3;

const LEAF_SIZE: usize = 4;

#[derive(Clone, Copy)]
struct Node {
    min: DVec3,
    max: DVec3,
    /// Лист: перший індекс у `ids`; внутрішній вузол: індекс правого нащадка (лівий — наступний).
    index: u32,
    /// 0 — внутрішній вузол, >0 — кількість трикутників у листі.
    count: u32,
}

pub struct Bvh {
    nodes: Vec<Node>,
    ids: Vec<u32>,
}

impl Bvh {
    pub fn build(verts: &[DVec3], faces: &[[u32; 3]]) -> Self {
        let n = faces.len();
        let mut boxes: Vec<(DVec3, DVec3)> = Vec::with_capacity(n);
        let mut centroids: Vec<DVec3> = Vec::with_capacity(n);
        for f in faces {
            let (a, b, c) = (verts[f[0] as usize], verts[f[1] as usize], verts[f[2] as usize]);
            boxes.push((a.min(b).min(c), a.max(b).max(c)));
            centroids.push((a + b + c) / 3.0);
        }
        let mut ids: Vec<u32> = (0..n as u32).collect();
        let mut nodes = Vec::with_capacity(n / 2 + 1);
        if n > 0 {
            build_node(&mut nodes, &mut ids, 0, &boxes, &centroids);
        }
        Self { nodes, ids }
    }

    /// Найближче перетинання променя `origin + t·dir` (t ≥ 0) з трикутниками; двобічне.
    pub fn raycast(
        &self,
        verts: &[DVec3],
        faces: &[[u32; 3]],
        origin: DVec3,
        dir: DVec3,
    ) -> Option<(u32, f64)> {
        if self.nodes.is_empty() {
            return None;
        }
        let inv = DVec3::new(1.0 / dir.x, 1.0 / dir.y, 1.0 / dir.z);
        let mut best: Option<(u32, f64)> = None;
        let mut t_max = f64::INFINITY;
        let mut stack = [0u32; 64];
        let mut sp = 1usize;
        while sp > 0 {
            sp -= 1;
            let node = &self.nodes[stack[sp] as usize];
            if !slab(node.min, node.max, origin, inv, t_max) {
                continue;
            }
            if node.count == 0 {
                let ni = stack[sp] as usize;
                if sp + 2 <= stack.len() {
                    stack[sp] = node.index;
                    stack[sp + 1] = ni as u32 + 1;
                    sp += 2;
                }
            } else {
                let first = node.index as usize;
                for &id in &self.ids[first..first + node.count as usize] {
                    let f = faces[id as usize];
                    let (a, b, c) = (verts[f[0] as usize], verts[f[1] as usize], verts[f[2] as usize]);
                    if let Some(t) = moller_trumbore(origin, dir, a, b, c)
                        && t < t_max
                    {
                        t_max = t;
                        best = Some((id, t));
                    }
                }
            }
        }
        best
    }
}

fn build_node(
    nodes: &mut Vec<Node>,
    ids: &mut [u32],
    offset: usize,
    boxes: &[(DVec3, DVec3)],
    centroids: &[DVec3],
) -> usize {
    let (mut lo, mut hi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
    let (mut clo, mut chi) = (lo, hi);
    for &id in ids.iter() {
        let (bl, bh) = boxes[id as usize];
        lo = lo.min(bl);
        hi = hi.max(bh);
        clo = clo.min(centroids[id as usize]);
        chi = chi.max(centroids[id as usize]);
    }
    let me = nodes.len();
    if ids.len() <= LEAF_SIZE {
        nodes.push(Node { min: lo, max: hi, index: offset as u32, count: ids.len() as u32 });
        return me;
    }
    nodes.push(Node { min: lo, max: hi, index: 0, count: 0 });
    let ext = chi - clo;
    let axis = if ext.x >= ext.y && ext.x >= ext.z {
        0
    } else if ext.y >= ext.z {
        1
    } else {
        2
    };
    let mid = ids.len() / 2;
    ids.select_nth_unstable_by(mid, |&a, &b| {
        centroids[a as usize][axis].total_cmp(&centroids[b as usize][axis])
    });
    let (left, right) = ids.split_at_mut(mid);
    build_node(nodes, left, offset, boxes, centroids);
    let right_idx = build_node(nodes, right, offset + mid, boxes, centroids);
    nodes[me].index = right_idx as u32;
    me
}

fn slab(min: DVec3, max: DVec3, o: DVec3, inv: DVec3, t_max: f64) -> bool {
    let t1 = (min - o) * inv;
    let t2 = (max - o) * inv;
    // `f64::min/max` ігнорують NaN (0·∞ на межі плити).
    let tn = t1.x.min(t2.x).max(t1.y.min(t2.y)).max(t1.z.min(t2.z));
    let tf = t1.x.max(t2.x).min(t1.y.max(t2.y)).min(t1.z.max(t2.z));
    tf >= tn.max(0.0) && tn <= t_max
}

fn moller_trumbore(o: DVec3, d: DVec3, a: DVec3, b: DVec3, c: DVec3) -> Option<f64> {
    let e1 = b - a;
    let e2 = c - a;
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-18 {
        return None;
    }
    let inv = 1.0 / det;
    let s = o - a;
    let u = s.dot(p) * inv;
    if !(-1e-9..=1.0 + 1e-9).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = d.dot(q) * inv;
    if v < -1e-9 || u + v > 1.0 + 1e-9 {
        return None;
    }
    let t = e2.dot(q) * inv;
    (t >= 0.0).then_some(t)
}
