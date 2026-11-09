//! Fast, deterministic print orientation, evaluated in the mesh's current coordinates.
use crate::{DQuat, DVec3, IndexedMesh, InstanceRef, Project, VolumeKind};

#[derive(Clone, Debug)]
pub struct OrientParams {
    /// Angle of a printable slope from the horizontal, in degrees.
    pub overhang_angle: f64,
    pub bottom_bonus: f64,
    /// Dimensionless penalty, scaled by surface area and relative height.
    pub height_penalty: f64,
    pub first_layer_height: f64,
}
impl Default for OrientParams {
    fn default() -> Self {
        Self {
            overhang_angle: 45.,
            bottom_bonus: 0.5,
            height_penalty: 0.02,
            first_layer_height: 0.2,
        }
    }
}

/// An incremental shortest-arc tilt; no additional rotation about the bed's Z axis.
/// Inspired by OrcaSlicer's AutoOrienter / OrientMesh (candidate normals, supplements,
/// bottom exclusion and overhang/contact scoring), rather than its full target function:
/// https://github.com/SoftFever/OrcaSlicer/blob/main/src/libslic3r/Orient.cpp
/// Sampling bounds the hull and scoring work independently of the input triangle count.
pub fn best_rotation(mesh: &IndexedMesh, params: &OrientParams) -> DQuat {
    let count = mesh.triangles.len().min(8192);
    if count == 0 {
        return DQuat::IDENTITY;
    }
    let mut faces = Vec::with_capacity(count);
    let weight = mesh.triangles.len() as f64 / count as f64;
    for i in 0..count {
        let t = mesh.triangles[i * mesh.triangles.len() / count];
        let Some(a) = mesh.positions.get(t[0] as usize) else {
            continue;
        };
        let Some(b) = mesh.positions.get(t[1] as usize) else {
            continue;
        };
        let Some(c) = mesh.positions.get(t[2] as usize) else {
            continue;
        };
        let vertices = [*a, *b, *c].map(DVec3::from);
        if !vertices.iter().all(|v| v.is_finite()) {
            continue;
        }
        let cross = (vertices[1] - vertices[0]).cross(vertices[2] - vertices[0]);
        let area = cross.length() * 0.5;
        if area > 1e-12 {
            faces.push((vertices, cross / (2. * area), area * weight));
        }
    }
    if faces.is_empty() {
        return DQuat::IDENTITY;
    }
    let points: Vec<_> = faces.iter().flat_map(|f| f.0).collect();
    let hull_points: Vec<_> = (0..points.len().min(2048))
        .map(|i| points[i * points.len() / points.len().min(2048)])
        .collect();
    let mut normals: Vec<(DVec3, f64)> = Vec::new();
    let mut add = |normal: DVec3, area: f64| {
        if let Some(n) = normals.iter_mut().find(|n| n.0.dot(normal) > 0.9999) {
            n.1 += area;
        } else {
            normals.push((normal, area));
        }
    };
    for t in crate::convex_hull(&hull_points) {
        let [a, b, c] = t.map(|i| hull_points[i]);
        let cross = (b - a).cross(c - a);
        if cross.length_squared() > 1e-20 {
            add(cross.normalize(), cross.length() * 0.5);
        }
    }
    normals.sort_by(|a, b| b.1.total_cmp(&a.1));
    normals.truncate(32);
    // Large actual faces are useful when the sampled hull approximates a curved surface.
    let mut largest: Vec<_> = faces.iter().map(|f| (f.1, f.2)).collect();
    largest.sort_by(|a, b| b.1.total_cmp(&a.1));
    normals.extend(largest.into_iter().take(16));
    let mut directions = vec![DVec3::Z]; // ties retain the current orientation
    directions.extend(normals.into_iter().map(|n| -n.0));
    directions.extend([DVec3::X, -DVec3::X, DVec3::Y, -DVec3::Y, -DVec3::Z]);
    for i in 0..48 {
        let z = 1. - 2. * (i as f64 + 0.5) / 48.;
        let angle = i as f64 * std::f64::consts::PI * (3. - 5f64.sqrt());
        let r = (1. - z * z).sqrt();
        directions.push(DVec3::new(r * angle.cos(), r * angle.sin(), z));
    }
    let total: f64 = faces.iter().map(|f| f.2).sum();
    let lo = points.iter().fold(DVec3::MAX, |a, b| a.min(*b));
    let hi = points.iter().fold(DVec3::MIN, |a, b| a.max(*b));
    let size = (hi - lo).length().max(1e-9);
    let threshold = -params.overhang_angle.to_radians().cos();
    let score = |up: DVec3| {
        let (mut min, mut max) = (f64::INFINITY, f64::NEG_INFINITY);
        for p in &points {
            let z = p.dot(up);
            min = min.min(z);
            max = max.max(z);
        }
        let (mut bottom, mut overhang) = (0., 0.);
        for (v, n, area) in &faces {
            let top = v
                .iter()
                .map(|p| p.dot(up))
                .fold(f64::NEG_INFINITY, f64::max);
            if top <= min + params.first_layer_height.max(1e-6) {
                bottom += area;
            } else if n.dot(up) < threshold {
                overhang += area;
            }
        }
        overhang - params.bottom_bonus * bottom + params.height_penalty * total * (max - min) / size
    };
    let mut best = DVec3::Z;
    let mut cost = score(best);
    for up in directions.into_iter().skip(1) {
        let next = score(up);
        if next < cost - total * 1e-8 {
            best = up;
            cost = next;
        }
    }
    DQuat::from_rotation_arc(best, DVec3::Z)
}

impl Project {
    /// Includes only printable parts, with instance/volume transforms (including scale).
    pub fn orient_instance(&mut self, key: InstanceRef) -> anyhow::Result<()> {
        let object = self.object(key.object)?;
        let instance = self.instance(key)?;
        let parts: Vec<_> = object
            .volumes
            .iter()
            .filter(|v| v.kind == VolumeKind::Part)
            .collect();
        let total: usize = parts.iter().map(|v| v.mesh.triangles.len()).sum();
        let count = total.min(8192);
        let mut mesh = IndexedMesh::default();
        let (mut start, mut sample) = (0, 0);
        // Sample across the combined mesh before transforming: avoid allocating and
        // transforming millions of vertices that scoring would immediately discard.
        for volume in parts {
            let transform = instance.transform * volume.transform;
            let end = start + volume.mesh.triangles.len();
            while sample < count && sample * total / count < end {
                let t = volume.mesh.triangles[sample * total / count - start];
                let offset = mesh.positions.len() as u32;
                for i in t {
                    let p = volume
                        .mesh
                        .positions
                        .get(i as usize)
                        .ok_or_else(|| anyhow::anyhow!("некоректна грань"))?;
                    mesh.positions
                        .push(transform.transform_point3(DVec3::from(*p)).to_array());
                }
                let mut t = [offset, offset + 1, offset + 2];
                // A reflected transform reverses winding.
                if transform.matrix3.determinant() < 0. {
                    t.swap(1, 2);
                }
                mesh.triangles.push(t);
                sample += 1;
            }
            start = end;
        }
        let rotation = best_rotation(&mesh, &OrientParams::default());
        self.rotate(key, rotation)?;
        self.drop_to_bed(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn triangle(mesh: &mut IndexedMesh, v: [DVec3; 3]) {
        let offset = mesh.positions.len() as u32;
        mesh.positions.extend(v.map(|p| p.to_array()));
        mesh.triangles.push([offset, offset + 1, offset + 2]);
    }
    fn boxes(cells: &[[i32; 3]]) -> IndexedMesh {
        let mut mesh = IndexedMesh::default();
        for cell in cells {
            let origin = DVec3::new(cell[0] as f64, cell[1] as f64, cell[2] as f64) * 10.;
            for axis in 0..3 {
                let u = DVec3::AXES[(axis + 1) % 3] * 10.;
                let v = DVec3::AXES[(axis + 2) % 3] * 10.;
                for side in [-1, 1] {
                    let mut neighbour = *cell;
                    neighbour[axis] += side;
                    if cells.contains(&neighbour) {
                        continue;
                    }
                    let p = origin
                        + if side == 1 {
                            DVec3::AXES[axis] * 10.
                        } else {
                            DVec3::ZERO
                        };
                    let mut a = [p, p + u, p + u + v];
                    let mut b = [p, p + u + v, p + v];
                    if side < 0 {
                        a.swap(1, 2);
                        b.swap(1, 2);
                    }
                    triangle(&mut mesh, a);
                    triangle(&mut mesh, b);
                }
            }
        }
        mesh
    }
    fn rotated(mut mesh: IndexedMesh, q: DQuat) -> IndexedMesh {
        for p in &mut mesh.positions {
            *p = (q * DVec3::from(*p)).to_array();
        }
        mesh
    }
    fn bottom_area(mesh: &IndexedMesh) -> f64 {
        let min = mesh
            .positions
            .iter()
            .map(|p| p[2])
            .fold(f64::INFINITY, f64::min);
        mesh.triangles
            .iter()
            .map(|t| t.map(|i| DVec3::from(mesh.positions[i as usize])))
            .filter(|v| v.iter().all(|p| p.z < min + 1e-5))
            .map(|[a, b, c]| (b - a).cross(c - a).length() * 0.5)
            .sum()
    }
    #[test]
    fn edge_cube_returns_to_flat_and_is_idempotent() {
        let mesh = rotated(
            boxes(&[[0, 0, 0]]),
            DQuat::from_rotation_x(std::f64::consts::FRAC_PI_4),
        );
        let result = rotated(mesh.clone(), best_rotation(&mesh, &OrientParams::default()));
        assert!((bottom_area(&result) - 100.).abs() < 1e-6);
        assert!(
            best_rotation(&result, &OrientParams::default()).angle_between(DQuat::IDENTITY) < 1e-7
        );
    }
    #[test]
    fn already_optimal_preserves_yaw() {
        let mesh = rotated(boxes(&[[0, 0, 0]]), DQuat::from_rotation_z(0.731));
        assert_eq!(
            best_rotation(&mesh, &OrientParams::default()),
            DQuat::IDENTITY
        );
    }
    #[test]
    fn tilted_cylinder_stands_on_base() {
        let mut mesh = IndexedMesh::default();
        for i in 0..96 {
            let a = i as f64 * std::f64::consts::TAU / 96.;
            let b = (i + 1) as f64 * std::f64::consts::TAU / 96.;
            let p = DVec3::new(a.cos() * 10., a.sin() * 10., 0.);
            let q = DVec3::new(b.cos() * 10., b.sin() * 10., 0.);
            let z = DVec3::Z * 40.;
            for t in [
                [DVec3::ZERO, q, p],
                [z, p + z, q + z],
                [p, q, q + z],
                [p, q + z, p + z],
            ] {
                triangle(&mut mesh, t);
            }
        }
        let tilt = DQuat::from_rotation_y(0.94) * DQuat::from_rotation_x(0.37);
        let mesh = rotated(mesh, tilt);
        let rotation = best_rotation(&mesh, &OrientParams::default());
        assert!((rotation * tilt * DVec3::Z).z.abs() > 0.999999);
        assert!(bottom_area(&rotated(mesh, rotation)) > 310.);
    }
    #[test]
    fn l_bracket_has_no_unsupported_downward_faces() {
        let mesh = rotated(
            boxes(&[[0, 0, 0], [1, 0, 0], [0, 0, 1]]),
            DQuat::from_rotation_x(0.8) * DQuat::from_rotation_y(0.4),
        );
        let mesh = rotated(mesh.clone(), best_rotation(&mesh, &OrientParams::default()));
        assert!(bottom_area(&mesh) >= 300. - 1e-6);
        let min = mesh
            .positions
            .iter()
            .map(|p| p[2])
            .fold(f64::INFINITY, f64::min);
        for t in &mesh.triangles {
            let [a, b, c] = t.map(|i| DVec3::from(mesh.positions[i as usize]));
            if (b - a).cross(c - a).normalize().z < -0.7072 {
                assert!(a.z.max(b.z).max(c.z) < min + 1e-5);
            }
        }
    }
    #[test]
    fn instance_orientation_drops_to_bed_and_preserves_other_instances() {
        use crate::{DAffine3, Instance, Object, Volume};
        let key = InstanceRef {
            object: 1,
            instance: 0,
        };
        let mut p = Project::default();
        p.objects.push(Object {
            id: 1,
            volumes: vec![Volume {
                mesh: std::sync::Arc::new(boxes(&[[0, 0, 0]])),
                ..Default::default()
            }],
            instances: vec![
                Instance {
                    transform: DAffine3::from_rotation_translation(
                        DQuat::from_rotation_x(0.7),
                        DVec3::new(90., 80., 50.),
                    ),
                    ..Default::default()
                },
                Instance::default(),
            ],
            ..Default::default()
        });
        p.orient_instance(key).unwrap();
        let bounds = p.instance_bounds(key).unwrap().unwrap();
        assert!(bounds.min.z.abs() < 1e-7);
        assert!((bounds.max.z - 10.).abs() < 1e-7);
        assert_eq!(p.objects[0].instances[1], Instance::default());
    }
    #[test]
    fn empty_and_degenerate_are_safe() {
        assert_eq!(
            best_rotation(&IndexedMesh::default(), &OrientParams::default()),
            DQuat::IDENTITY
        );
    }
    #[test]
    #[ignore = "release performance measurement"]
    fn half_million_triangles_timing() {
        let mut mesh = IndexedMesh::default();
        for axis in 0..3 {
            let u = DVec3::AXES[(axis + 1) % 3] * 40. / 205.;
            let v = DVec3::AXES[(axis + 2) % 3] * 40. / 205.;
            for side in [-1, 1] {
                for i in 0..205 {
                    for j in 0..205 {
                        let p = u * i as f64
                            + v * j as f64
                            + if side == 1 {
                                DVec3::AXES[axis] * 40.
                            } else {
                                DVec3::ZERO
                            };
                        let mut a = [p, p + u, p + u + v];
                        let mut b = [p, p + u + v, p + v];
                        if side < 0 {
                            a.swap(1, 2);
                            b.swap(1, 2);
                        }
                        triangle(&mut mesh, a);
                        triangle(&mut mesh, b);
                    }
                }
            }
        }
        let mesh = rotated(mesh, DQuat::from_rotation_x(0.8));
        let start = std::time::Instant::now();
        let rotation = best_rotation(&mesh, &OrientParams::default());
        eprintln!("{} triangles: {:?}", mesh.triangles.len(), start.elapsed());
        assert!(rotation.angle_between(DQuat::IDENTITY) > 0.1);
        let mut project = Project::default();
        project.objects.push(crate::Object {
            id: 1,
            volumes: vec![crate::Volume {
                mesh: std::sync::Arc::new(mesh),
                ..Default::default()
            }],
            instances: vec![crate::Instance::default()],
            ..Default::default()
        });
        let start = std::time::Instant::now();
        let key = InstanceRef {
            object: 1,
            instance: 0,
        };
        project.orient_instance(key).unwrap();
        eprintln!(
            "orient_instance including transforms and drop: {:?}",
            start.elapsed()
        );
        let bounds = project.instance_bounds(key).unwrap().unwrap();
        assert!(bounds.min.z.abs() < 1e-6);
        assert!((bounds.max.z - 40.).abs() < 1e-6);
    }
}
