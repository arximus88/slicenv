//! Planar cuts in plate-local world coordinates. Input parts must be closed,
//! consistently oriented manifold surfaces; ambiguous/open contours return errors.
use crate::{
    DAffine3, DQuat, DVec3, IndexedMesh, InstanceRef, Object, Project, Volume, VolumeKind,
};
use anyhow::{Result, ensure};
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CutPlane {
    pub point: DVec3,
    /// Need not be normalized. Positive signed distance is the upper (A) side.
    pub normal: DVec3,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CutOptions {
    pub keep_upper: bool,
    pub keep_lower: bool,
    pub upper_place_on_cut: bool,
    pub upper_flip: bool,
    pub lower_place_on_cut: bool,
    pub lower_flip: bool,
    pub cut_to_parts: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CutResult {
    pub upper: Option<InstanceRef>,
    pub lower: Option<InstanceRef>,
    /// With cut_to_parts, contains the single resulting multi-volume instance.
    /// upper/lower refer to that same instance for each retained side.
    pub parts: Vec<InstanceRef>,
}

impl Project {
    /// Atomically replace only the selected instance with A/B objects (or volumes
    /// of one object). Other instances retain their original geometry.
    /// Non-part volumes are kept whole on each side their projected vertex bounds
    /// touch, like Orca modifiers. In parts mode identical copies are deduplicated;
    /// independently placed sides carry their own transformed modifier copies.
    /// Placement maps the side's outward cap normal to -Z, then applies the
    /// optional world-X flip, then drops that side to the bed. Without either
    /// placement or flip flag geometry stays in its original world position.
    /// Layer-range objects are rejected: their height-scoped settings cannot be
    /// preserved unambiguously after arbitrary rotation and cutting.
    pub fn cut_instance(
        &mut self,
        key: InstanceRef,
        plane: CutPlane,
        opts: CutOptions,
    ) -> Result<CutResult> {
        ensure!(
            plane.point.is_finite()
                && plane.normal.is_finite()
                && plane.normal.length().is_finite()
                && plane.normal.length() > 1e-12,
            "некоректна площина різання"
        );
        ensure!(
            opts.keep_upper || opts.keep_lower,
            "не вибрано сторону різання"
        );
        ensure!(
            self.object_layer_ranges(key.object)
                .is_none_or(|r| r.is_empty()),
            "не можна різати обʼєкт із діапазонами шарів"
        );
        let source = self.object(key.object)?.clone();
        let inst = self.instance(key)?.clone();
        ensure!(inst.plate.is_some(), "інстанс поза пластиною");
        let n = plane.normal.normalize();
        let mut halves: [Vec<Volume>; 2] = Default::default();
        let mut intersected = false;
        for v in &source.volumes {
            let tr = inst.transform * v.transform;
            ensure!(tr.is_finite(), "некоректна трансформація");
            if v.kind != VolumeKind::Part {
                let mut v = v.clone();
                v.transform = tr;
                v.source = None;
                let mut lo = f64::INFINITY;
                let mut hi = f64::NEG_INFINITY;
                for p in &v.mesh.positions {
                    let point = tr.transform_point3(DVec3::from(*p));
                    ensure!(point.is_finite(), "некоректна вершина модифікатора");
                    let d = (point - plane.point).dot(n);
                    lo = lo.min(d);
                    hi = hi.max(d);
                }
                if hi >= 0.0 {
                    halves[0].push(v.clone());
                }
                if lo <= 0.0 {
                    halves[1].push(v.clone());
                }
                continue;
            }
            let (meshes, hit) = world_clip(&v.mesh, tr, plane.point, n)?;
            intersected |= hit;
            for (side, mesh) in meshes.into_iter().enumerate() {
                if !mesh.triangles.is_empty() {
                    let mut volume = v.clone();
                    volume.mesh = std::sync::Arc::new(mesh);
                    volume.transform = DAffine3::IDENTITY;
                    volume.source = None;
                    volume.name = format!("{} {}", v.name, if side == 0 { "A" } else { "B" });
                    halves[side].push(volume);
                }
            }
        }
        ensure!(intersected, "площина не перетинає обʼєкт");
        let mut staged = self.clone();
        let mut result = CutResult::default();
        let mut next = staged.objects.iter().map(|o| o.id).max().unwrap_or(0);
        for (side, volumes) in halves.iter_mut().enumerate() {
            let keep = if side == 0 {
                opts.keep_upper
            } else {
                opts.keep_lower
            };
            if !keep || !volumes.iter().any(|v| v.kind == VolumeKind::Part) {
                volumes.clear();
                continue;
            }
            let place = if side == 0 {
                opts.upper_place_on_cut
            } else {
                opts.lower_place_on_cut
            };
            let flip = if side == 0 {
                opts.upper_flip
            } else {
                opts.lower_flip
            };
            let mut rotation = if place {
                DQuat::from_rotation_arc(if side == 0 { -n } else { n }, -DVec3::Z)
            } else {
                DQuat::IDENTITY
            };
            if flip {
                rotation = DQuat::from_rotation_x(std::f64::consts::PI) * rotation;
            }
            let mut change = DAffine3::from_translation(plane.point)
                * DAffine3::from_quat(rotation)
                * DAffine3::from_translation(-plane.point);
            if place || flip {
                let min_z = volumes
                    .iter()
                    .filter(|v| v.kind == VolumeKind::Part)
                    .flat_map(|v| {
                        v.mesh.positions.iter().map(move |p| {
                            (change * v.transform).transform_point3(DVec3::from(*p)).z
                        })
                    })
                    .fold(f64::INFINITY, f64::min);
                change = DAffine3::from_translation(DVec3::new(0., 0., -min_z)) * change;
            }
            for v in volumes.iter_mut() {
                v.transform = change * v.transform;
            }
            if opts.cut_to_parts {
                continue;
            }
            next = next
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("вичерпано ID"))?;
            let mut instance = inst.clone();
            instance.transform = DAffine3::IDENTITY;
            staged.objects.push(Object {
                id: next,
                name: format!("{} {}", source.name, if side == 0 { "A" } else { "B" }),
                volumes: std::mem::take(volumes),
                instances: vec![instance],
                config: source.config.clone(),
                model: source.model.clone(),
                metadata: source.metadata.clone(),
            });
            let new_key = InstanceRef {
                object: next,
                instance: 0,
            };
            if side == 0 {
                result.upper = Some(new_key);
            } else {
                result.lower = Some(new_key);
            }
        }
        if opts.cut_to_parts {
            next = next
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("вичерпано ID"))?;
            let new_key = InstanceRef {
                object: next,
                instance: 0,
            };
            if !halves[0].is_empty() {
                result.upper = Some(new_key);
            }
            if !halves[1].is_empty() {
                result.lower = Some(new_key);
            }
            let mut volumes = Vec::new();
            for volume in halves.into_iter().flatten() {
                if volume.kind == VolumeKind::Part || !volumes.contains(&volume) {
                    volumes.push(volume);
                }
            }
            let mut instance = inst;
            instance.transform = DAffine3::IDENTITY;
            staged.objects.push(Object {
                id: next,
                name: source.name,
                volumes,
                instances: vec![instance],
                config: source.config,
                model: source.model,
                metadata: source.metadata,
            });
            result.parts.push(new_key);
        }
        staged.delete_instance(key)?;
        *self = staged;
        Ok(result)
    }
}

/// A spatial weld (neighboring cells included) avoids rounding-boundary cracks.
struct Builder {
    mesh: IndexedMesh,
    cells: HashMap<[i64; 3], Vec<u32>>,
    origin: DVec3,
    eps: f64,
}
impl Builder {
    fn new(origin: DVec3, eps: f64) -> Self {
        Self {
            mesh: IndexedMesh::default(),
            cells: HashMap::new(),
            origin,
            eps,
        }
    }
    fn vertex(&mut self, p: DVec3) -> u32 {
        let q = ((p - self.origin) / self.eps).floor();
        let cell = [q.x as i64, q.y as i64, q.z as i64];
        for x in -1..=1 {
            for y in -1..=1 {
                for z in -1..=1 {
                    if let Some(ids) = self.cells.get(&[cell[0] + x, cell[1] + y, cell[2] + z]) {
                        for &id in ids {
                            if DVec3::from(self.mesh.positions[id as usize]).distance(p) <= self.eps
                            {
                                return id;
                            }
                        }
                    }
                }
            }
        }
        let id = self.mesh.positions.len() as u32;
        self.mesh.positions.push(p.to_array());
        self.cells.entry(cell).or_default().push(id);
        id
    }
    fn triangle(&mut self, p: [DVec3; 3], attr: Option<&crate::Config>) {
        let t = p.map(|v| self.vertex(v));
        if t[0] == t[1] || t[1] == t[2] || t[0] == t[2] {
            return;
        }
        if (p[1] - p[0]).cross(p[2] - p[0]).length() <= self.eps * self.eps {
            return;
        }
        if let Some(attr) = attr {
            self.mesh
                .triangle_attributes
                .insert(self.mesh.triangles.len(), attr.clone());
        }
        self.mesh.triangles.push(t);
    }
}

fn world_clip(
    mesh: &IndexedMesh,
    tr: DAffine3,
    p: DVec3,
    n: DVec3,
) -> Result<([IndexedMesh; 2], bool)> {
    ensure!(
        tr.is_finite()
            && tr.matrix3.determinant().is_finite()
            && tr.matrix3.determinant().abs() > 1e-18,
        "некоректна трансформація"
    );
    let mut points = Vec::new();
    let (mut lo, mut hi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
    for v in &mesh.positions {
        let v = tr.transform_point3(DVec3::from(*v));
        ensure!(v.is_finite(), "некоректна вершина");
        lo = lo.min(v);
        hi = hi.max(v);
        points.push(v);
    }
    if points.is_empty() {
        return Ok((Default::default(), false));
    }
    let eps = (hi - lo).max_element().max(1.0) * 1e-9;
    let distances: Vec<_> = points
        .iter_mut()
        .map(|v| {
            let d = (*v - p).dot(n);
            if d.abs() <= eps {
                *v -= d * n;
                0.0
            } else {
                d
            }
        })
        .collect();
    let both = distances.iter().any(|d| *d > 0.) && distances.iter().any(|d| *d < 0.);
    let mut builders = [Builder::new(lo, eps), Builder::new(lo, eps)];
    for (ti, tri) in mesh.triangles.iter().enumerate() {
        ensure!(
            tri.iter().all(|i| (*i as usize) < points.len()),
            "індекс вершини поза сіткою"
        );
        let mut tri = *tri;
        if tr.matrix3.determinant() < 0. {
            tri.swap(1, 2);
        }
        let vertices = tri.map(|i| points[i as usize]);
        let ds = tri.map(|i| distances[i as usize]);
        for (side, b) in builders.iter_mut().enumerate() {
            if ds == [0.; 3] {
                let faces_upper = (vertices[1] - vertices[0])
                    .cross(vertices[2] - vertices[0])
                    .dot(n)
                    < 0.;
                if faces_upper == (side == 0) {
                    b.triangle(vertices, mesh.triangle_attributes.get(&ti));
                }
                continue;
            }
            let sign = if side == 0 { 1. } else { -1. };
            let mut poly = Vec::new();
            for i in 0..3 {
                let j = (i + 1) % 3;
                if ds[i] * sign >= 0. {
                    poly.push(vertices[i]);
                }
                if ds[i] * ds[j] < 0. {
                    let x = vertices[i] + (vertices[j] - vertices[i]) * (ds[i] / (ds[i] - ds[j]));
                    poly.push(x - n * (x - p).dot(n));
                }
            }
            for i in 1..poly.len().saturating_sub(1) {
                b.triangle(
                    [poly[0], poly[i], poly[i + 1]],
                    mesh.triangle_attributes.get(&ti),
                );
            }
        }
    }
    let mut hit = false;
    if both {
        for (s, b) in builders.iter_mut().enumerate() {
            hit |= cap(&mut b.mesh, p, n, s == 0, eps)?;
        }
    }
    Ok((builders.map(|b| b.mesh), hit))
}

fn cap(mesh: &mut IndexedMesh, p: DVec3, n: DVec3, upper: bool, eps: f64) -> Result<bool> {
    let mut edges: BTreeMap<(u32, u32), Vec<(u32, u32)>> = BTreeMap::new();
    for t in &mesh.triangles {
        for i in 0..3 {
            let (a, b) = (t[i], t[(i + 1) % 3]);
            edges.entry((a.min(b), a.max(b))).or_default().push((a, b));
        }
    }
    let mut adjacent: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for uses in edges.values() {
        ensure!(uses.len() <= 2, "неманіфолдна сітка");
        if uses.len() == 2 {
            ensure!(uses[0].0 == uses[1].1, "неузгоджена орієнтація сітки");
            continue;
        }
        let (a, b) = uses[0];
        ensure!(
            [a, b]
                .iter()
                .all(|i| (DVec3::from(mesh.positions[*i as usize]) - p).dot(n).abs() <= eps * 2.),
            "відкрита вихідна сітка"
        );
        adjacent.entry(a).or_default().push(b);
        adjacent.entry(b).or_default().push(a);
    }
    if adjacent.is_empty() {
        return Ok(false);
    }
    ensure!(
        adjacent.values().all(|v| v.len() == 2),
        "незамкнений або неоднозначний контур різання"
    );
    let mut unused: BTreeSet<u32> = adjacent.keys().copied().collect();
    let mut loops = Vec::new();
    while let Some(&start) = unused.first() {
        let mut ring = vec![start];
        unused.remove(&start);
        let (mut prev, mut cur) = (start, adjacent[&start][0]);
        while cur != start {
            ensure!(unused.remove(&cur), "самоперетин контуру різання");
            ring.push(cur);
            let next = adjacent[&cur].iter().copied().find(|v| *v != prev).unwrap();
            prev = cur;
            cur = next;
        }
        ensure!(ring.len() >= 3, "вироджений контур");
        loops.push(ring);
    }
    let axis = if n.x.abs() < 0.8 { DVec3::X } else { DVec3::Y };
    let u = n.cross(axis).normalize();
    let v = n.cross(u);
    let xy: Vec<_> = mesh
        .positions
        .iter()
        .map(|q| {
            let q = DVec3::from(*q) - p;
            [q.dot(u), q.dot(v)]
        })
        .collect();
    let areas: Vec<f64> = loops
        .iter()
        .map(|l| {
            (0..l.len())
                .map(|i| {
                    let a = xy[l[i] as usize];
                    let b = xy[l[(i + 1) % l.len()] as usize];
                    a[0] * b[1] - a[1] * b[0]
                })
                .sum::<f64>()
                * 0.5
        })
        .collect();
    // The smallest containing loop is the parent; even nesting depth is solid.
    let parents: Vec<_> = loops
        .iter()
        .enumerate()
        .map(|(i, l)| {
            (0..loops.len())
                .filter(|&j| {
                    areas[j].abs() > areas[i].abs() && contains(&loops[j], xy[l[0] as usize], &xy)
                })
                .min_by(|&a, &b| areas[a].abs().total_cmp(&areas[b].abs()))
        })
        .collect();
    let depth = |mut i: usize| {
        let mut d = 0;
        while let Some(j) = parents[i] {
            d += 1;
            i = j;
        }
        d
    };
    for outer in 0..loops.len() {
        if depth(outer) % 2 != 0 {
            continue;
        }
        let mut ids = Vec::new();
        let mut holes = Vec::new();
        let mut boundary = Vec::new();
        for j in
            std::iter::once(outer).chain((0..loops.len()).filter(|&j| parents[j] == Some(outer)))
        {
            if j != outer {
                holes.push(ids.len());
            }
            // Normalize outer CCW, holes CW in the plane basis.
            let mut ring = loops[j].clone();
            if (areas[j] > 0.) != (j == outer) {
                ring.reverse();
            }
            boundary.extend_from_slice(&ring);
            // Floating point projection can make straight section edges appear
            // very slightly bent. Remove these junctions before earcut, avoiding
            // sliver triangles; restore their indices in the cap afterwards.
            loop {
                let removable = (0..ring.len()).find(|&i| {
                    let a = DVec3::from(
                        mesh.positions[ring[(i + ring.len() - 1) % ring.len()] as usize],
                    );
                    let q = DVec3::from(mesh.positions[ring[i] as usize]);
                    let b = DVec3::from(mesh.positions[ring[(i + 1) % ring.len()] as usize]);
                    let ab = b - a;
                    let f = (q - a).dot(ab) / ab.length_squared();
                    f > 0. && f < 1. && (a + ab * f).distance(q) <= eps
                });
                if ring.len() <= 3 {
                    break;
                }
                if let Some(i) = removable {
                    ring.remove(i);
                } else {
                    break;
                }
            }
            ids.extend(ring);
        }
        let coords: Vec<_> = ids.iter().flat_map(|&i| xy[i as usize]).collect();
        let indices = earcutr::earcut(&coords, &holes, 2)
            .map_err(|e| anyhow::anyhow!("тріангуляція кришки: {e:?}"))?;
        ensure!(!indices.is_empty(), "порожня кришка");
        let mut triangles: Vec<[u32; 3]> = indices
            .chunks_exact(3)
            .map(|t| [ids[t[0]], ids[t[1]], ids[t[2]]])
            .collect();
        // Earcut may omit collinear boundary vertices. Reinsert them by splitting
        // incident cap triangles, so every side edge has an exact cap counterpart.
        for &id in &boundary {
            let q = DVec3::from(mesh.positions[id as usize]);
            let mut refined = Vec::new();
            for t in triangles {
                let mut split = false;
                if !t.contains(&id) {
                    for e in 0..3 {
                        let (a, b, c) = (t[e], t[(e + 1) % 3], t[(e + 2) % 3]);
                        let pa = DVec3::from(mesh.positions[a as usize]);
                        let pb = DVec3::from(mesh.positions[b as usize]);
                        let ab = pb - pa;
                        let f = (q - pa).dot(ab) / ab.length_squared();
                        if f > 0. && f < 1. && (pa + ab * f).distance(q) <= eps {
                            refined.extend([[a, id, c], [id, b, c]]);
                            split = true;
                            break;
                        }
                    }
                }
                if !split {
                    refined.push(t);
                }
            }
            triangles = refined;
        }
        for mut t in triangles {
            let [a, b, c] = t.map(|i| DVec3::from(mesh.positions[i as usize]));
            if ((b - a).cross(c - a).dot(n) > 0.) == upper {
                t.swap(1, 2);
            }
            mesh.triangles.push(t);
        }
    }
    let mut usage: BTreeMap<(u32, u32), (usize, i32)> = BTreeMap::new();
    for t in &mesh.triangles {
        for i in 0..3 {
            let (a, b) = (t[i], t[(i + 1) % 3]);
            let e = usage.entry((a.min(b), a.max(b))).or_default();
            e.0 += 1;
            e.1 += if a < b { 1 } else { -1 };
        }
    }
    ensure!(
        usage.values().all(|&v| v == (2, 0)),
        "не вдалося замкнути кришку різання"
    );
    Ok(true)
}
fn contains(ring: &[u32], p: [f64; 2], xy: &[[f64; 2]]) -> bool {
    let mut inside = false;
    for i in 0..ring.len() {
        let a = xy[ring[i] as usize];
        let b = xy[ring[(i + 1) % ring.len()] as usize];
        if (a[1] > p[1]) != (b[1] > p[1])
            && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0]
        {
            inside = !inside;
        }
    }
    inside
}
