use project::*;
use std::collections::BTreeMap;

fn quad(m: &mut IndexedMesh, a: u32, b: u32, c: u32, d: u32) {
    m.triangles.extend([[a, b, c], [a, c, d]]);
}
fn cube(layers: &[f64]) -> IndexedMesh {
    let mut m = IndexedMesh::default();
    for &z in layers {
        for [x, y] in [[-1., -1.], [1., -1.], [1., 1.], [-1., 1.]] {
            m.positions.push([x, y, z]);
        }
    }
    quad(&mut m, 3, 2, 1, 0);
    let top = (m.positions.len() - 4) as u32;
    quad(&mut m, top, top + 1, top + 2, top + 3);
    for l in 0..layers.len() - 1 {
        for i in 0..4 {
            let a = (l * 4 + i) as u32;
            let b = (l * 4 + (i + 1) % 4) as u32;
            quad(&mut m, a, b, b + 4, a + 4);
        }
    }
    m
}
fn tube() -> IndexedMesh {
    let mut m = IndexedMesh::default();
    for z in [-1., 1.] {
        for r in [2., 1.] {
            for [x, y] in [[-r, -r], [r, -r], [r, r], [-r, r]] {
                m.positions.push([x, y, z]);
            }
        }
    }
    for i in 0..4u32 {
        let j = (i + 1) % 4;
        quad(&mut m, i, j, j + 8, i + 8); // outside
        quad(&mut m, i + 4, i + 12, j + 12, j + 4); // bore
        quad(&mut m, i + 8, j + 8, j + 12, i + 12); // top ring
        quad(&mut m, i, i + 4, j + 4, j); // bottom ring
    }
    m
}
fn fixture(mesh: IndexedMesh) -> (Project, InstanceRef) {
    let mut p = Project::default();
    p.add_plate("bed").unwrap();
    p.objects.push(Object {
        id: 1,
        name: "solid".into(),
        volumes: vec![Volume {
            mesh: mesh.into(),
            ..Default::default()
        }],
        instances: vec![Instance::default()],
        config: BTreeMap::from([("test".into(), "value".into())]),
        ..Default::default()
    });
    p.sync_plates();
    (
        p,
        InstanceRef {
            object: 1,
            instance: 0,
        },
    )
}
fn options() -> CutOptions {
    CutOptions {
        keep_upper: true,
        keep_lower: true,
        ..Default::default()
    }
}
fn plane() -> CutPlane {
    CutPlane {
        point: DVec3::ZERO,
        normal: DVec3::Z,
    }
}
fn mesh(p: &Project, k: InstanceRef) -> &IndexedMesh {
    &p.object(k.object).unwrap().volumes[0].mesh
}
fn check_mesh(m: &IndexedMesh) -> f64 {
    let mut edges: BTreeMap<(u32, u32), (usize, i32)> = BTreeMap::new();
    let mut volume = 0.;
    for t in &m.triangles {
        let [a, b, c] = t.map(|i| DVec3::from(m.positions[i as usize]));
        assert!((b - a).cross(c - a).length() > 1e-12, "degenerate {t:?}");
        volume += a.dot(b.cross(c)) / 6.;
        for i in 0..3 {
            let (a, b) = (t[i], t[(i + 1) % 3]);
            let e = edges.entry((a.min(b), a.max(b))).or_default();
            e.0 += 1;
            e.1 += if a < b { 1 } else { -1 };
        }
    }
    for (edge, usage) in edges {
        assert_eq!(
            usage,
            (2, 0),
            "edge {edge:?}, positions {:?} {:?}",
            m.positions[edge.0 as usize],
            m.positions[edge.1 as usize]
        );
    }
    volume
}
fn close(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-7, "{a} != {b}");
}
fn cap_area(m: &IndexedMesh, n: DVec3) -> f64 {
    m.triangles
        .iter()
        .filter_map(|t| {
            let [a, b, c] = t.map(|i| DVec3::from(m.positions[i as usize]));
            if [a, b, c].iter().all(|v| v.z.abs() < 1e-8) {
                let cross = (b - a).cross(c - a);
                assert!(cross.normalize().dot(n) > 1. - 1e-8);
                Some(cross.length() * 0.5)
            } else {
                None
            }
        })
        .sum()
}
#[test]
fn cube_halves_are_closed_oriented_and_half_volume() {
    let (mut p, k) = fixture(cube(&[-1., 1.]));
    let r = p.cut_instance(k, plane(), options()).unwrap();
    for (key, lo, hi, n) in [
        (r.upper.unwrap(), 0., 1., -DVec3::Z),
        (r.lower.unwrap(), -1., 0., DVec3::Z),
    ] {
        close(check_mesh(mesh(&p, key)), 4.);
        close(cap_area(mesh(&p, key), n), 4.);
        let b = p.instance_bounds(key).unwrap().unwrap();
        assert_eq!(b.min, DVec3::new(-1., -1., lo));
        assert_eq!(b.max, DVec3::new(1., 1., hi));
        assert_eq!(p.object(key.object).unwrap().config["test"], "value");
    }
    assert_eq!(p.objects.len(), 2);
    assert_eq!(p.plates[0].objects.len(), 2);
    assert_eq!(p.object(r.upper.unwrap().object).unwrap().name, "solid A");
    assert_eq!(p.object(r.lower.unwrap().object).unwrap().name, "solid B");
}
#[test]
fn through_hole_produces_ring_caps() {
    let (mut p, k) = fixture(tube());
    close(check_mesh(mesh(&p, k)), 24.);
    let r = p.cut_instance(k, plane(), options()).unwrap();
    for (key, n) in [(r.upper.unwrap(), -DVec3::Z), (r.lower.unwrap(), DVec3::Z)] {
        close(check_mesh(mesh(&p, key)), 12.);
        close(cap_area(mesh(&p, key), n), 12.);
    }
}
#[test]
fn non_intersecting_and_tangent_planes_fail_atomically() {
    let (mut p, k) = fixture(cube(&[-1., 1.]));
    let original = p.clone();
    for z in [2., 1., -1.] {
        assert!(
            p.cut_instance(
                k,
                CutPlane {
                    point: DVec3::new(0., 0., z),
                    ..plane()
                },
                options()
            )
            .is_err()
        );
        assert_eq!(p, original);
    }
}
#[test]
fn world_plane_respects_instance_and_volume_transforms() {
    let (mut p, k) = fixture(cube(&[-1., 1.]));
    let transform = DAffine3::from_translation(DVec3::new(7., 11., 13.))
        * DAffine3::from_quat(DQuat::from_rotation_y(0.7));
    let vt = DAffine3::from_scale_rotation_translation(
        DVec3::new(2., 1., 3.),
        DQuat::from_rotation_x(0.4),
        DVec3::new(1., 2., 3.),
    );
    p.instance_mut(k).unwrap().transform = transform;
    p.objects[0].volumes[0].transform = vt;
    let world = transform * vt;
    let point = world.transform_point3(DVec3::ZERO);
    let normal = world.matrix3.inverse().transpose() * DVec3::Z;
    let r = p
        .cut_instance(k, CutPlane { point, normal }, options())
        .unwrap();
    for (key, sign) in [(r.upper.unwrap(), 1.), (r.lower.unwrap(), -1.)] {
        let m = mesh(&p, key);
        close(check_mesh(m), 24.);
        for v in &m.positions {
            assert!((DVec3::from(*v) - point).dot(normal) * sign >= -1e-8);
        }
        let inverse = world.inverse();
        let local: Vec<_> = m
            .positions
            .iter()
            .map(|v| inverse.transform_point3(DVec3::from(*v)))
            .collect();
        close(
            local.iter().map(|v| v.z).fold(f64::INFINITY, f64::min),
            if sign > 0. { 0. } else { -1. },
        );
        close(
            local.iter().map(|v| v.z).fold(f64::NEG_INFINITY, f64::max),
            if sign > 0. { 1. } else { 0. },
        );
    }
}
#[test]
fn place_on_cut_points_cap_down_and_drops_to_bed() {
    for side in [0, 1] {
        let (mut p, k) = fixture(cube(&[-1., 1.]));
        let normal = DVec3::new(1., 2., 3.).normalize();
        let r = p
            .cut_instance(
                k,
                CutPlane {
                    point: DVec3::ZERO,
                    normal,
                },
                CutOptions {
                    upper_place_on_cut: side == 0,
                    lower_place_on_cut: side == 1,
                    ..options()
                },
            )
            .unwrap();
        let key = if side == 0 {
            r.upper.unwrap()
        } else {
            r.lower.unwrap()
        };
        close(p.instance_bounds(key).unwrap().unwrap().min.z, 0.);
        let v = &p.object(key.object).unwrap().volumes[0];
        let mut found = false;
        for t in &v.mesh.triangles {
            let original = t.map(|i| DVec3::from(v.mesh.positions[i as usize]));
            if original.iter().all(|p| p.dot(normal).abs() < 1e-8) {
                let [a, b, c] = original.map(|q| v.transform.transform_point3(q));
                assert!((b - a).cross(c - a).normalize().dot(-DVec3::Z) > 1. - 1e-8);
                close(a.z, 0.);
                found = true;
            }
        }
        assert!(found);
    }
}
#[test]
fn cut_through_existing_vertex_layer_and_edges() {
    for normal in [
        DVec3::Z,
        DVec3::new(1., 1., 0.).normalize(),
        DVec3::new(1., 1., 1.).normalize(),
    ] {
        let (mut p, k) = fixture(cube(&[-1., 0., 1.]));
        let r = p
            .cut_instance(k, CutPlane { normal, ..plane() }, options())
            .unwrap();
        close(check_mesh(mesh(&p, r.upper.unwrap())), 4.);
        close(check_mesh(mesh(&p, r.lower.unwrap())), 4.);
    }
}
#[test]
fn parts_and_lower_only_preserve_other_instances_and_modifiers() {
    let (mut p, k) = fixture(cube(&[-1., 1.]));
    p.duplicate_instance(k).unwrap();
    for (z, kind) in [
        (0., VolumeKind::NegativeVolume),
        (5., VolumeKind::Modifier),
        (-5., VolumeKind::SupportBlocker),
    ] {
        p.objects[0].volumes.push(Volume {
            mesh: cube(&[-1., 1.]).into(),
            kind,
            transform: DAffine3::from_translation(DVec3::new(0., 0., z)),
            ..Default::default()
        });
    }
    let mut parts = p.clone();
    let r = parts
        .cut_instance(
            k,
            plane(),
            CutOptions {
                cut_to_parts: true,
                ..options()
            },
        )
        .unwrap();
    assert_eq!(r.upper, r.lower);
    assert_eq!(r.parts, vec![r.upper.unwrap()]);
    let obj = parts.object(r.parts[0].object).unwrap();
    assert_eq!(
        obj.volumes
            .iter()
            .filter(|v| v.kind == VolumeKind::Part)
            .count(),
        2
    );
    assert_eq!(obj.volumes.len(), 5);
    assert_eq!(parts.object(1).unwrap().instances.len(), 1);
    let r = p
        .cut_instance(
            k,
            plane(),
            CutOptions {
                keep_upper: false,
                ..options()
            },
        )
        .unwrap();
    assert_eq!(r.upper, None);
    let lower = r.lower.unwrap();
    let obj = p.object(lower.object).unwrap();
    assert_eq!(obj.name, "solid B");
    assert_eq!(obj.volumes.len(), 3);
    assert!(!obj.volumes.iter().any(|v| v.kind == VolumeKind::Modifier));
    close(
        check_mesh(
            &obj.volumes
                .iter()
                .find(|v| v.kind == VolumeKind::Part)
                .unwrap()
                .mesh,
        ),
        4.,
    );
}
#[test]
fn reflected_mesh_keeps_outward_winding_and_attributes() {
    let (mut p, k) = fixture(cube(&[-1., 1.]));
    std::sync::Arc::make_mut(&mut p.objects[0].volumes[0].mesh)
        .triangle_attributes
        .insert(4, BTreeMap::from([("paint".into(), "1".into())]));
    p.instance_mut(k).unwrap().transform = DAffine3::from_scale(DVec3::new(-1., 1., 1.));
    let r = p.cut_instance(k, plane(), options()).unwrap();
    for key in [r.upper.unwrap(), r.lower.unwrap()] {
        let m = mesh(&p, key);
        close(check_mesh(m), 4.);
        assert!(!m.triangle_attributes.is_empty());
        assert!(m.triangle_attributes.keys().all(|i| *i < m.triangles.len()));
    }
}

fn append(target: &mut IndexedMesh, source: IndexedMesh, transform: DAffine3) {
    let base = target.positions.len() as u32;
    target.positions.extend(
        source
            .positions
            .into_iter()
            .map(|v| transform.transform_point3(DVec3::from(v)).to_array()),
    );
    target
        .triangles
        .extend(source.triangles.into_iter().map(|t| t.map(|i| i + base)));
}
#[test]
fn nested_island_and_disconnected_contours() {
    let mut m = tube();
    append(
        &mut m,
        cube(&[-1., 1.]),
        DAffine3::from_scale(DVec3::new(0.5, 0.5, 1.)),
    );
    append(
        &mut m,
        tube(),
        DAffine3::from_translation(DVec3::new(8., 0., 0.)),
    );
    let (mut p, k) = fixture(m);
    let r = p.cut_instance(k, plane(), options()).unwrap();
    for (key, n) in [(r.upper.unwrap(), -DVec3::Z), (r.lower.unwrap(), DVec3::Z)] {
        close(check_mesh(mesh(&p, key)), 25.);
        close(cap_area(mesh(&p, key), n), 25.);
    }
}
#[test]
fn oblique_cuts_stay_closed_and_conserve_volume() {
    for i in 0..32 {
        let (mut p, k) = fixture(cube(&[-1., 0., 1.]));
        let normal = DVec3::new((i as f64 * 0.71).sin(), (i as f64 * 1.13).cos(), 0.37).normalize();
        let r = p
            .cut_instance(
                k,
                CutPlane {
                    point: normal * 0.23,
                    normal,
                },
                options(),
            )
            .unwrap_or_else(|e| panic!("normal {i}: {normal:?}: {e}"));
        close(
            check_mesh(mesh(&p, r.upper.unwrap())) + check_mesh(mesh(&p, r.lower.unwrap())),
            8.,
        );
    }
}
#[test]
fn flip_follows_place_on_cut_then_drops_to_bed() {
    let (mut p, k) = fixture(cube(&[-1., 1.]));
    let normal = DVec3::new(1., 2., 3.).normalize();
    let r = p
        .cut_instance(
            k,
            CutPlane {
                point: DVec3::ZERO,
                normal,
            },
            CutOptions {
                upper_place_on_cut: true,
                upper_flip: true,
                ..options()
            },
        )
        .unwrap();
    let key = r.upper.unwrap();
    close(p.instance_bounds(key).unwrap().unwrap().min.z, 0.);
    let v = &p.object(key.object).unwrap().volumes[0];
    let expected =
        DQuat::from_rotation_x(std::f64::consts::PI) * DQuat::from_rotation_arc(-normal, -DVec3::Z);
    for axis in [DVec3::X, DVec3::Y, DVec3::Z] {
        assert!((v.transform.transform_vector3(axis) - expected * axis).length() < 1e-8);
    }
}
#[test]
fn malformed_mesh_and_invalid_options_fail_atomically() {
    for invalid in 0..3 {
        let (mut p, k) = fixture(cube(&[-1., 1.]));
        let m = std::sync::Arc::make_mut(&mut p.objects[0].volumes[0].mesh);
        match invalid {
            0 => {
                m.triangles.remove(4);
            }
            1 => {
                m.triangles[4].swap(0, 1);
            }
            _ => {
                m.triangles[0][0] = 9999;
            }
        }
        let original = p.clone();
        assert!(p.cut_instance(k, plane(), options()).is_err());
        assert_eq!(p, original);
    }
    let (mut p, k) = fixture(cube(&[-1., 1.]));
    let original = p.clone();
    assert!(p.cut_instance(k, plane(), CutOptions::default()).is_err());
    assert!(
        p.cut_instance(
            k,
            CutPlane {
                normal: DVec3::ZERO,
                ..plane()
            },
            options()
        )
        .is_err()
    );
    assert_eq!(p, original);
}

#[test]
fn concave_cap_is_not_a_centroid_fan() {
    let mut m = IndexedMesh::default();
    for z in [-1., 1.] {
        for [x, y] in [[0., 0.], [2., 0.], [2., 1.], [1., 1.], [1., 2.], [0., 2.]] {
            m.positions.push([x, y, z]);
        }
    }
    for t in [[0, 1, 3], [1, 2, 3], [0, 3, 5], [3, 4, 5]] {
        m.triangles.push([t[2], t[1], t[0]]);
        m.triangles.push(t.map(|i| i + 6));
    }
    for i in 0..6 {
        let j = (i + 1) % 6;
        quad(&mut m, i, j, j + 6, i + 6);
    }
    let (mut p, k) = fixture(m);
    close(check_mesh(mesh(&p, k)), 6.);
    let r = p.cut_instance(k, plane(), options()).unwrap();
    for (key, n) in [(r.upper.unwrap(), -DVec3::Z), (r.lower.unwrap(), DVec3::Z)] {
        close(check_mesh(mesh(&p, key)), 3.);
        close(cap_area(mesh(&p, key), n), 3.);
    }
}
