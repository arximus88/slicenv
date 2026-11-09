//! Процедурні примітиви й «зручні» калібрувальні моделі. Усі сітки замкнені, зовнішня
//! нормаль — за правилом правої руки; основа лежить на z = 0, центр основи — в (0, 0).

use crate::{Bounds, DVec3, IndexedMesh};
use std::f64::consts::{FRAC_PI_2, TAU};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Primitive {
    Cube,
    Cylinder,
    Sphere,
    Cone,
    Disc,
    Torus,
}

impl Primitive {
    pub const ALL: [Primitive; 6] = [
        Primitive::Cube,
        Primitive::Cylinder,
        Primitive::Sphere,
        Primitive::Cone,
        Primitive::Disc,
        Primitive::Torus,
    ];

    /// Типові розміри: куб 20 мм, Ø20 мм для круглих тіл, диск Ø30 × 2 мм, тор 20/6 мм.
    pub fn mesh(self) -> IndexedMesh {
        match self {
            Primitive::Cube => IndexedMesh::cuboid(DVec3::splat(20.0)),
            Primitive::Cylinder => IndexedMesh::cylinder(10.0, 20.0, 64),
            Primitive::Sphere => IndexedMesh::sphere(10.0, 48, 24),
            Primitive::Cone => IndexedMesh::cone(10.0, 20.0, 64),
            Primitive::Disc => IndexedMesh::cylinder(15.0, 2.0, 64),
            Primitive::Torus => IndexedMesh::torus(10.0, 3.0, 64, 24),
        }
    }
}

/// Прості калібрувальні моделі, згенеровані кодом (без чужих файлів).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handy {
    CalibrationCube,
    CalibrationCylinder,
    StepTower,
}

impl Handy {
    pub const ALL: [Handy; 3] = [
        Handy::CalibrationCube,
        Handy::CalibrationCylinder,
        Handy::StepTower,
    ];

    pub fn mesh(self) -> IndexedMesh {
        match self {
            Handy::CalibrationCube => IndexedMesh::cuboid(DVec3::splat(20.0)),
            Handy::CalibrationCylinder => IndexedMesh::cylinder(10.0, 20.0, 96),
            // П’ять сходинок по 4 мм: Ø30, Ø25, Ø20, Ø15, Ø10 — перевірка розмірів і звисів.
            Handy::StepTower => {
                let mut profile = vec![(0.0, 0.0)];
                for (i, r) in [15.0, 12.5, 10.0, 7.5, 5.0].into_iter().enumerate() {
                    profile.push((r, i as f64 * 4.0));
                    profile.push((r, (i + 1) as f64 * 4.0));
                }
                profile.push((0.0, 20.0));
                IndexedMesh::revolve(&profile, 96, false)
            }
        }
    }
}

impl IndexedMesh {
    pub fn bounds(&self) -> Option<Bounds> {
        let mut it = self.positions.iter().map(|p| DVec3::from(*p));
        let first = it.next()?;
        Some(it.fold(
            Bounds {
                min: first,
                max: first,
            },
            |b, p| Bounds {
                min: b.min.min(p),
                max: b.max.max(p),
            },
        ))
    }

    /// Об’єм із знаком: додатний для замкненої сітки з зовнішніми нормалями.
    pub fn signed_volume(&self) -> f64 {
        self.triangles
            .iter()
            .filter_map(|t| {
                let p = |i: u32| self.positions.get(i as usize).map(|v| DVec3::from(*v));
                Some(p(t[0])?.dot(p(t[1])?.cross(p(t[2])?)) / 6.0)
            })
            .sum()
    }

    pub fn flip_winding(&mut self) {
        for t in &mut self.triangles {
            t.swap(1, 2);
        }
    }

    /// Паралелепіпед з основою на z = 0, центрованою в (0, 0).
    pub fn cuboid(size: DVec3) -> Self {
        let (x, y, z) = (size.x / 2.0, size.y / 2.0, size.z);
        Self {
            positions: vec![
                [-x, -y, 0.0],
                [x, -y, 0.0],
                [x, y, 0.0],
                [-x, y, 0.0],
                [-x, -y, z],
                [x, -y, z],
                [x, y, z],
                [-x, y, z],
            ],
            triangles: vec![
                [0, 2, 1],
                [0, 3, 2],
                [4, 5, 6],
                [4, 6, 7],
                [0, 1, 5],
                [0, 5, 4],
                [2, 3, 7],
                [2, 7, 6],
                [3, 0, 4],
                [3, 4, 7],
                [1, 2, 6],
                [1, 6, 5],
            ],
            ..Self::default()
        }
    }

    pub fn cylinder(radius: f64, height: f64, segments: usize) -> Self {
        Self::revolve(
            &[(0.0, 0.0), (radius, 0.0), (radius, height), (0.0, height)],
            segments,
            false,
        )
    }

    pub fn cone(radius: f64, height: f64, segments: usize) -> Self {
        Self::revolve(&[(0.0, 0.0), (radius, 0.0), (0.0, height)], segments, false)
    }

    pub fn sphere(radius: f64, segments: usize, rings: usize) -> Self {
        let rings = rings.max(2);
        let profile: Vec<_> = (0..=rings)
            .map(|i| {
                let phi = -FRAC_PI_2 + std::f64::consts::PI * i as f64 / rings as f64;
                let r = if i == 0 || i == rings {
                    0.0
                } else {
                    radius * phi.cos()
                };
                (r, radius + radius * phi.sin())
            })
            .collect();
        Self::revolve(&profile, segments, false)
    }

    pub fn torus(major: f64, minor: f64, major_segments: usize, minor_segments: usize) -> Self {
        let profile: Vec<_> = (0..minor_segments.max(3))
            .map(|i| {
                let t = TAU * i as f64 / minor_segments.max(3) as f64;
                (major + minor * t.cos(), minor + minor * t.sin())
            })
            .collect();
        Self::revolve(&profile, major_segments, true)
    }

    /// Тіло обертання профілю `(r, z)` навколо осі Z. Точки з r = 0 стають полюсами;
    /// `closed` замикає профіль у кільце (тор). Орієнтація виправляється за об’ємом.
    pub fn revolve(profile: &[(f64, f64)], segments: usize, closed: bool) -> Self {
        let n = segments.max(3);
        let mut mesh = Self::default();
        // Для кожної точки профілю: вершини-кільце (або один полюс).
        let rings: Vec<Vec<u32>> = profile
            .iter()
            .map(|&(r, z)| {
                if r.abs() < 1e-12 {
                    mesh.positions.push([0.0, 0.0, z]);
                    vec![mesh.positions.len() as u32 - 1]
                } else {
                    (0..n)
                        .map(|s| {
                            let a = TAU * s as f64 / n as f64;
                            mesh.positions.push([r * a.cos(), r * a.sin(), z]);
                            mesh.positions.len() as u32 - 1
                        })
                        .collect()
                }
            })
            .collect();
        let pairs = if closed {
            profile.len()
        } else {
            profile.len().saturating_sub(1)
        };
        for k in 0..pairs {
            let (lower, upper) = (&rings[k], &rings[(k + 1) % profile.len()]);
            for s in 0..n {
                let pick = |ring: &Vec<u32>, i: usize| ring[i % ring.len()];
                let (a, b) = (pick(lower, s), pick(lower, s + 1));
                let (c, d) = (pick(upper, s + 1), pick(upper, s));
                for t in [[a, b, c], [a, c, d]] {
                    if t[0] != t[1] && t[1] != t[2] && t[0] != t[2] {
                        mesh.triangles.push(t);
                    }
                }
            }
        }
        if mesh.signed_volume() < 0.0 {
            mesh.flip_winding();
        }
        mesh
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Кожне орієнтоване ребро трапляється один раз і має протилежне — сітка замкнена.
    fn assert_closed(mesh: &IndexedMesh) {
        let mut edges: HashMap<(u32, u32), usize> = HashMap::new();
        for t in &mesh.triangles {
            for i in 0..3 {
                *edges.entry((t[i], t[(i + 1) % 3])).or_default() += 1;
            }
        }
        for (&(a, b), &count) in &edges {
            assert_eq!(count, 1, "ребро {a}->{b} повторюється");
            assert_eq!(edges.get(&(b, a)), Some(&1), "ребро {a}->{b} без пари");
        }
    }

    #[test]
    fn every_primitive_is_closed_outward_and_sits_on_the_bed() {
        for p in Primitive::ALL {
            let mesh = p.mesh();
            assert_closed(&mesh);
            assert!(mesh.signed_volume() > 0.0, "{p:?}");
            let b = mesh.bounds().unwrap();
            assert!(b.min.z.abs() < 1e-9, "{p:?}: основа не на z = 0");
            assert!((b.center().x).abs() < 0.5 && (b.center().y).abs() < 0.5, "{p:?}");
        }
        for h in Handy::ALL {
            let mesh = h.mesh();
            assert_closed(&mesh);
            assert!(mesh.signed_volume() > 0.0, "{h:?}");
        }
    }

    #[test]
    fn default_sizes_are_sane() {
        let cube = Primitive::Cube.mesh();
        assert!((cube.signed_volume() - 8000.0).abs() < 1e-6);
        assert_eq!(cube.bounds().unwrap().size(), DVec3::splat(20.0));
        let sphere = Primitive::Sphere.mesh();
        let size = sphere.bounds().unwrap().size();
        assert!((size.z - 20.0).abs() < 1e-9 && (size.x - 20.0).abs() < 0.2);
        // Об’єм сфери 4/3 π r³ ≈ 4189; дискретизація трохи менша.
        assert!((sphere.signed_volume() - 4188.8).abs() < 60.0);
        let torus = Primitive::Torus.mesh();
        let size = torus.bounds().unwrap().size();
        assert!((size.x - 26.0).abs() < 0.1 && (size.z - 6.0).abs() < 0.1);
        let cylinder = Primitive::Cylinder.mesh();
        assert!((cylinder.signed_volume() - std::f64::consts::PI * 100.0 * 20.0).abs() < 40.0);
    }
}
