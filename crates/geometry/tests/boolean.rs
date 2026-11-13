use geometry::{object_preview, plate_preview, split_object_by_plane};
use project::TriangleSoup as Mesh;
use project::{DAffine3, DQuat, DVec3, IndexedMesh, Instance, Object, Project, Volume, VolumeKind};
use std::collections::BTreeMap;
trait MeshInspect {
    fn triangle_count(&self) -> usize;
    fn bounds(&self) -> Option<(project::DVec3, project::DVec3)>;
}
impl MeshInspect for Mesh {
    fn triangle_count(&self) -> usize {
        self.positions.len() / 3
    }
    fn bounds(&self) -> Option<(project::DVec3, project::DVec3)> {
        let first = project::DVec3::from(self.positions.first()?.map(f64::from));
        Some(self.positions.iter().fold((first, first), |(lo, hi), p| {
            let p = project::DVec3::from(p.map(f64::from));
            (lo.min(p), hi.max(p))
        }))
    }
}

fn cube(size: f64) -> IndexedMesh {
    IndexedMesh {
        positions: [
            [0., 0., 0.],
            [1., 0., 0.],
            [1., 1., 0.],
            [0., 1., 0.],
            [0., 0., 1.],
            [1., 0., 1.],
            [1., 1., 1.],
            [0., 1., 1.],
        ]
        .map(|p| p.map(|v| v * size))
        .to_vec(),
        triangles: vec![
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [1, 2, 6],
            [1, 6, 5],
            [2, 3, 7],
            [2, 7, 6],
            [3, 0, 4],
            [3, 4, 7],
        ],
        ..Default::default()
    }
}
fn sample() -> Project {
    let mut p = Project::default();
    p.objects.push(Object {
        id: 17,
        name: "cut cube".into(),
        volumes: vec![
            Volume {
                mesh: cube(10.).into(),
                ..Default::default()
            },
            Volume {
                mesh: cube(4.).into(),
                transform: DAffine3::from_translation(DVec3::splat(3.)),
                kind: VolumeKind::NegativeVolume,
                ..Default::default()
            },
            // Open modifier geometry must never enter CSG.
            Volume {
                mesh: IndexedMesh {
                    positions: vec![[0.; 3]],
                    triangles: vec![[0, 0, 0]],
                    ..Default::default()
                }
                .into(),
                kind: VolumeKind::Modifier,
                ..Default::default()
            },
        ],
        instances: vec![Instance::default()],
        ..Default::default()
    });
    p.archive
        .insert("opaque".into(), std::sync::Arc::from(&b"preserve"[..]));
    std::sync::Arc::make_mut(&mut p.objects[0].volumes[0].mesh)
        .triangle_attributes
        .insert(0, BTreeMap::from([("paint".into(), "red".into())]));
    p
}
fn volume(mesh: &Mesh) -> f64 {
    mesh.positions
        .as_chunks::<3>()
        .0
        .iter()
        .map(|p| {
            DVec3::from(p[0].map(f64::from))
                .dot(DVec3::from(p[1].map(f64::from)).cross(DVec3::from(p[2].map(f64::from))))
                / 6.
        })
        .sum()
}
fn closed(mesh: &Mesh) {
    // Every geometric edge occurs exactly twice, with opposite directions.
    let key = |p: [f32; 3]| p.map(|v| (v as f64 * 1e6).round() as i64);
    let mut edges = BTreeMap::<_, (usize, i32)>::new();
    for tri in mesh.positions.as_chunks::<3>().0 {
        for (a, b) in [(0, 1), (1, 2), (2, 0)] {
            let (a, b) = (key(tri[a]), key(tri[b]));
            assert_ne!(a, b);
            let (edge, direction) = if a < b { ((a, b), 1) } else { ((b, a), -1) };
            let value = edges.entry(edge).or_default();
            value.0 += 1;
            value.1 += direction;
        }
    }
    assert!(
        edges.values().all(|&v| v == (2, 0)),
        "open or inconsistently oriented edge"
    );
}
#[test]
fn difference_is_closed_and_preserves_project() {
    let p = sample();
    let before = p.clone();
    let mesh = plate_preview(&p, Some(1)).unwrap();
    assert!((volume(&mesh) - 936.).abs() < 1e-5);
    closed(&mesh);
    assert_eq!(p, before);
}
#[test]
fn union_before_subtraction_and_cut_through_surface() {
    let mut p = sample();
    let object = &mut p.objects[0];
    object.volumes.truncate(2);
    object.volumes[1].transform = DAffine3::from_translation(DVec3::new(8., 3., 3.));
    object.volumes.push(Volume {
        mesh: cube(10.).into(),
        transform: DAffine3::from_translation(DVec3::new(5., 0., 0.)),
        ..Default::default()
    });
    let mesh = plate_preview(&p, Some(1)).unwrap();
    assert!((volume(&mesh) - 1436.).abs() < 1e-5);
    closed(&mesh);
}
#[test]
fn transformed_instances_and_plate_filtering() {
    let mut p = sample();
    let transform = DAffine3::from_scale_rotation_translation(
        DVec3::new(-2., 3., 1.),
        DQuat::from_rotation_z(std::f64::consts::FRAC_PI_2),
        DVec3::new(70., 20., 5.),
    );
    p.objects[0].instances = vec![
        Instance {
            transform,
            ..Default::default()
        },
        Instance {
            plate: None,
            transform: DAffine3::from_translation(DVec3::splat(100.)),
            ..Default::default()
        },
    ];
    // Mirror a volume too: local solid still occupies [0,10]^3.
    p.objects[0].volumes[0].transform = DAffine3::from_scale_rotation_translation(
        DVec3::new(-1., 1., 1.),
        DQuat::IDENTITY,
        DVec3::new(10., 0., 0.),
    );
    let mesh = plate_preview(&p, Some(1)).unwrap();
    let (min, max) = mesh.bounds().unwrap();
    // manifold на MSVC дає похибку ~1e-14 — порівнюємо з допуском.
    assert!(min.abs_diff_eq(DVec3::new(40., 0., 5.), 1e-9), "{min}");
    assert!(max.abs_diff_eq(DVec3::new(70., 20., 15.), 1e-9), "{max}");
    assert!((volume(&mesh) - 936. * 6.).abs() < 1e-4);
    closed(&mesh);
    let outside = plate_preview(&p, None).unwrap();
    assert!(outside.bounds().unwrap().0.abs_diff_eq(DVec3::splat(100.), 1e-9));
    assert!(plate_preview(&p, Some(2)).unwrap().positions.is_empty());
    p.objects[0].instances.push(Instance::default());
    let two = plate_preview(&p, Some(1)).unwrap();
    assert_eq!(two.triangle_count(), mesh.triangle_count() * 2);
    assert!((volume(&two) - 936. * 7.).abs() < 1e-4);
    closed(&two);
}
#[test]
fn capped_split_conserves_volume() {
    let p = sample();
    let (a, b) = split_object_by_plane(&p.objects[0], [0., 0., 2.], 10.).unwrap();
    closed(&a);
    closed(&b);
    assert!((volume(&a) - 468.).abs() < 1e-5);
    assert!((volume(&b) - 468.).abs() < 1e-5);
    assert!(split_object_by_plane(&p.objects[0], [0.; 3], 0.).is_err());
}
#[test]
fn invalid_inputs_are_actionable_errors() {
    let mut p = sample();
    std::sync::Arc::make_mut(&mut p.objects[0].volumes[0].mesh)
        .triangles
        .pop();
    let error = format!("{:#}", plate_preview(&p, Some(1)).unwrap_err());
    assert!(
        error.contains("object 17") && error.contains("volume 0") && error.contains("manifold"),
        "{error}"
    );
    let mut p = sample();
    std::sync::Arc::make_mut(&mut p.objects[0].volumes[1].mesh).triangles[0][0] = 1000;
    assert!(plate_preview(&p, Some(1)).is_err());
    let mut p = sample();
    p.objects[0].instances[0].transform = DAffine3::from_scale(DVec3::ZERO);
    assert!(plate_preview(&p, Some(1)).is_err());
}
#[test]
fn empty_difference_is_valid() {
    let mut p = sample();
    p.objects[0].volumes[1].mesh = cube(20.).into();
    p.objects[0].volumes[1].transform = DAffine3::IDENTITY;
    assert!(object_preview(&p.objects[0]).unwrap().positions.is_empty());
}

/// Opt in with GEOMETRY_ANGEL_FIXTURE=/path/to/angel_medium.3mf.
#[test]
#[ignore = "requires external angel fixture"]
fn real_angel() {
    let path = std::env::var_os("GEOMETRY_ANGEL_FIXTURE").expect("set GEOMETRY_ANGEL_FIXTURE");
    let p = Project::load(path).unwrap();
    let before = p.clone();
    assert!(p.objects.iter().any(|o| {
        o.volumes
            .iter()
            .any(|v| v.kind == VolumeKind::NegativeVolume)
    }));
    let started = std::time::Instant::now();
    let mesh = plate_preview(&p, Some(1)).unwrap();
    eprintln!(
        "angel boolean: {:?}, {} triangles",
        started.elapsed(),
        mesh.triangle_count()
    );
    assert!(!mesh.positions.is_empty());
    // TriangleSoup is f32: microscopic kernel edges can coincide on export.
    // Strict topology assertions above use exact synthetic coordinates.
    let raw = p.plate_mesh(Some(1)).unwrap();
    let raw = Mesh {
        positions: raw.positions,
        normals: raw.normals,
    };
    eprintln!(
        "angel raw volume {}, boolean volume {}",
        volume(&raw),
        volume(&mesh)
    );
    assert!(volume(&mesh) > 0. && volume(&mesh) < volume(&raw) - 1.);
    assert!(mesh.normals.iter().flatten().all(|v| v.is_finite()));
    assert_eq!(p, before);
}
