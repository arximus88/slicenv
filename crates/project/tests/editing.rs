use project::*;

fn fixture() -> (Project, InstanceRef) {
    let mesh = IndexedMesh {
        positions: vec![[0., 0., 0.], [20., 0., 0.], [0., 10., 0.], [0., 0., 5.]],
        triangles: vec![[0, 2, 1], [0, 1, 3], [1, 2, 3], [2, 0, 3]],
        ..IndexedMesh::default()
    };
    let mut p = Project::default();
    p.add_plate("first").unwrap();
    p.objects.push(Object {
        id: 1,
        name: "tetra".into(),
        volumes: vec![Volume {
            mesh: mesh.into(),
            ..Volume::default()
        }],
        instances: vec![Instance::default()],
        ..Object::default()
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

fn close(a: DVec3, b: DVec3) {
    assert!((a - b).abs().max_element() < 1e-8, "{a:?} != {b:?}");
}

#[test]
fn translate() {
    let (mut p, k) = fixture();
    p.translate(k, DVec3::new(3., 4., 5.)).unwrap();
    close(
        p.instance_bounds(k).unwrap().unwrap().min,
        DVec3::new(3., 4., 5.),
    );
}

#[test]
fn rotate() {
    let (mut p, k) = fixture();
    let center = p.instance_bounds(k).unwrap().unwrap().center();
    p.rotate(k, DQuat::from_rotation_z(std::f64::consts::FRAC_PI_2))
        .unwrap();
    let b = p.instance_bounds(k).unwrap().unwrap();
    close(b.center(), center);
    close(b.size(), DVec3::new(10., 20., 5.));
}

#[test]
fn scale() {
    let (mut p, k) = fixture();
    let center = p.instance_bounds(k).unwrap().unwrap().center();
    p.scale(k, DVec3::new(2., 3., 4.)).unwrap();
    let b = p.instance_bounds(k).unwrap().unwrap();
    close(b.center(), center);
    close(b.size(), DVec3::new(40., 30., 20.));
    assert!(p.scale(k, DVec3::ZERO).is_err());
}

#[test]
fn scale_along_object_axes_after_rotation() {
    let (mut p, k) = fixture();
    p.rotate(k, DQuat::from_rotation_z(std::f64::consts::FRAC_PI_2))
        .unwrap();
    let center = p.instance_bounds(k).unwrap().unwrap().center();
    // Власна вісь X після повороту на 90° дивиться вздовж світової Y.
    p.scale_along_axes(k, DVec3::new(2., 1., 1.)).unwrap();
    let b = p.instance_bounds(k).unwrap().unwrap();
    close(b.size(), DVec3::new(10., 40., 5.));
    close(b.center(), center);
    assert!(p.scale_along_axes(k, DVec3::ZERO).is_err());
}

#[test]
fn lay_on_normal_puts_that_side_down() {
    let (mut p, k) = fixture();
    p.lay_on_normal(k, DVec3::X).unwrap();
    let b = p.instance_bounds(k).unwrap().unwrap();
    close(b.size(), DVec3::new(5., 10., 20.));
    assert!(b.min.z.abs() < 1e-8);
    assert!(p.lay_on_normal(k, DVec3::ZERO).is_err());
}

#[test]
fn drop_to_bed() {
    let (mut p, k) = fixture();
    p.translate(k, DVec3::new(0., 0., 50.)).unwrap();
    p.drop_to_bed(k).unwrap();
    assert!(p.instance_bounds(k).unwrap().unwrap().min.z.abs() < 1e-10);
}

#[test]
fn lay_on_face() {
    let (mut p, k) = fixture();
    p.scale(k, DVec3::new(2., 3., 1.)).unwrap();
    p.lay_on_face(k, 0, 1).unwrap();
    let soup = p.plate_mesh(Some(1)).unwrap();
    close(DVec3::from(soup.normals[3].map(f64::from)), -DVec3::Z);
    assert!(p.instance_bounds(k).unwrap().unwrap().min.z.abs() < 1e-8);
    assert!(p.lay_on_face(k, 0, 99).is_err());
}

#[test]
fn center_on_plate() {
    let (mut p, k) = fixture();
    p.center_on_plate(k).unwrap();
    let c = p.instance_bounds(k).unwrap().unwrap().center();
    close(DVec3::new(c.x, c.y, 0.), DVec3::new(128., 128., 0.));
}

#[test]
fn duplicate_delete_instance() {
    let (mut p, k) = fixture();
    let k2 = p.duplicate_instance(k).unwrap();
    assert_eq!(k2.instance, 1);
    assert_eq!(p.plates[0].objects.len(), 2);
    p.delete_instance(k).unwrap();
    assert_eq!(p.plates[0].objects, vec![k]);
    assert!(p.instance(k2).is_err());
}

#[test]
fn duplicate_delete_object() {
    let (mut p, k) = fixture();
    let id = p.duplicate_object(k.object).unwrap();
    assert_ne!(id, k.object);
    p.delete_object(k.object).unwrap();
    assert_eq!(p.objects.len(), 1);
    assert_eq!(p.plates[0].objects[0].object, id);
}

#[test]
fn move_instance() {
    let (mut p, k) = fixture();
    let second = p.add_plate("second").unwrap();
    p.move_to_plate(k, Some(second)).unwrap();
    assert!(p.plates[0].objects.is_empty());
    assert_eq!(p.plates[1].objects, vec![k]);
    p.move_to_plate(k, None).unwrap();
    assert_eq!(p.plate_bounds(None).unwrap().len(), 1);
    assert!(p.move_to_plate(k, Some(100)).is_err());
}

#[test]
fn add_delete_rename_plate() {
    let (mut p, k) = fixture();
    p.rename_plate(1, "new").unwrap();
    assert_eq!(p.plates[0].name, "new");
    let second = p.add_plate("two").unwrap();
    p.delete_plate(1).unwrap();
    assert_eq!(p.instance(k).unwrap().plate, None);
    assert_eq!(p.plates[0].index, second - 1);
}

#[test]
fn arrange_inside_bed_with_spacing() {
    let (mut p, k) = fixture();
    for _ in 0..29 {
        p.duplicate_instance(k).unwrap();
    }
    p.arrange(1, 3.).unwrap();
    let boxes = p.plate_bounds(Some(1)).unwrap();
    for (j, (_, b)) in boxes.iter().enumerate() {
        assert!(b.min.min_element() >= -1e-8);
        assert!(b.max.x <= BED_SIZE && b.max.y <= BED_SIZE);
        for (_, a) in &boxes[..j] {
            assert!(
                b.min.x >= a.max.x + 3. - 1e-8
                    || a.min.x >= b.max.x + 3. - 1e-8
                    || b.min.y >= a.max.y + 3. - 1e-8
                    || a.min.y >= b.max.y + 3. - 1e-8
            );
        }
    }
}

#[test]
fn arrange_failure_is_atomic() {
    let (mut p, k) = fixture();
    p.scale(k, DVec3::splat(100.)).unwrap();
    let before = p.clone();
    assert!(p.arrange(1, 2.).is_err());
    assert_eq!(p, before);
    assert!(p.arrange(1, -1.).is_err());
}

#[test]
fn render_and_pick_bounds_include_volume_transform() {
    let (mut p, k) = fixture();
    p.objects[0].volumes[0].transform = DAffine3::from_translation(DVec3::new(2., 3., 4.));
    let soup = p.plate_mesh(Some(1)).unwrap();
    assert_eq!(soup.positions.len(), 12);
    assert_eq!(soup.positions[0], [2., 3., 4.]);
    assert_eq!(soup.normals[0], [0., 0., -1.]);
    close(
        p.instance_bounds(k).unwrap().unwrap().min,
        DVec3::new(2., 3., 4.),
    );
    assert_eq!(p.plate_bounds(Some(1)).unwrap().len(), 1);
}

#[test]
fn arrange_out_of_space_and_locked_plate_leave_project_unchanged() {
    let (mut p, k) = fixture();
    for _ in 0..249 {
        p.duplicate_instance(k).unwrap();
    }
    let before = p.clone();
    assert!(p.arrange(1, 3.).is_err());
    assert_eq!(p, before);
    p.plates[0].locked = true;
    let before = p.clone();
    assert!(p.arrange(1, 0.).is_err());
    assert_eq!(p, before);
}

#[test]
fn render_and_pick_modifier_overlay_separately() {
    let (mut p, k) = fixture();
    let mut modifier = p.objects[0].volumes[0].clone();
    modifier.kind = VolumeKind::Modifier;
    modifier.transform = DAffine3::from_translation(DVec3::new(50., 0., 0.));
    p.objects[0].volumes.push(modifier);
    assert_eq!(p.plate_mesh(Some(1)).unwrap().positions.len(), 12);
    let overlay = p
        .plate_mesh_with_kinds(Some(1), &[VolumeKind::Modifier])
        .unwrap();
    assert_eq!(overlay.positions.len(), 12);
    assert_eq!(overlay.positions[0][0], 50.);
    assert_eq!(
        p.instance_bounds_with_kinds(k, &[VolumeKind::Modifier])
            .unwrap()
            .unwrap()
            .min
            .x,
        50.
    );
}

#[test]
fn printer_bed_dimensions_control_arrange_and_center() {
    let (mut p, k) = fixture();
    let lo = [-10.0, 5.0];
    let hi = [170.0, 185.0];
    p.center_on_bed(k, lo, hi).unwrap();
    let bounds = p.instance_bounds(k).unwrap().unwrap();
    assert_eq!(bounds.center().truncate(), glam::DVec2::new(80.0, 95.0));
    p.arrange_on_bed(1, 6.0, lo, hi).unwrap();
    close(
        p.instance_bounds(k).unwrap().unwrap().min,
        DVec3::new(-10.0, 5.0, 0.0),
    );
    let before = p.clone();
    assert!(p.arrange_on_bed(1, 6.0, [0.0; 2], [10.0; 2]).is_err());
    assert_eq!(p, before, "failed packing must not change the project");
    assert!(p.center_on_bed(k, [f64::NAN; 2], hi).is_err());
}

#[test]
fn merge_and_split_preserve_world_geometry_and_volume_roles() {
    let (mut p, first) = fixture();
    let mut second_object = p.objects[0].clone();
    second_object.id = 2;
    second_object.name = "modifier".into();
    second_object.volumes[0].name = "cutout".into();
    second_object.volumes[0].kind = VolumeKind::NegativeVolume;
    second_object.volumes[0]
        .config
        .insert("extruder".into(), "2".into());
    second_object.instances[0].transform = DAffine3::from_translation(DVec3::new(30., 20., 5.));
    p.objects.push(second_object);
    p.sync_plates();
    let second = InstanceRef {
        object: 2,
        instance: 0,
    };
    let before_a = p.instance_bounds(first).unwrap().unwrap();
    let before_b = p
        .instance_bounds_with_kinds(second, &[VolumeKind::NegativeVolume])
        .unwrap()
        .unwrap();
    let merged = p.merge_instances(&[first, second]).unwrap();
    assert_eq!(p.object(merged.object).unwrap().volumes.len(), 2);
    assert_eq!(
        p.object(merged.object).unwrap().volumes[1].kind,
        VolumeKind::NegativeVolume
    );
    assert_eq!(
        p.object(merged.object).unwrap().volumes[1].config["extruder"],
        "2"
    );
    assert_eq!(p.instance_bounds(merged).unwrap().unwrap(), before_a);
    assert_eq!(
        p.instance_bounds_with_kinds(merged, &[VolumeKind::NegativeVolume])
            .unwrap()
            .unwrap(),
        before_b
    );
    let split = p.split_instance(merged).unwrap();
    assert_eq!(split.len(), 2);
    assert_eq!(p.instance_bounds(split[0]).unwrap().unwrap(), before_a);
    assert_eq!(
        p.instance_bounds_with_kinds(split[1], &[VolumeKind::NegativeVolume])
            .unwrap()
            .unwrap(),
        before_b
    );
}

#[test]
fn merge_rejects_invalid_selection_atomically() {
    let (mut p, key) = fixture();
    let before = p.clone();
    assert!(p.merge_instances(&[key, key]).is_err());
    assert_eq!(p, before);
}

#[test]
fn merge_rejects_conflicting_object_config_atomically() {
    let (mut p, first) = fixture();
    let mut second_object = p.objects[0].clone();
    second_object.id = 2;
    second_object
        .config
        .insert("support_type".into(), "tree".into());
    p.objects.push(second_object);
    p.sync_plates();
    let before = p.clone();
    assert!(
        p.merge_instances(&[
            first,
            InstanceRef {
                object: 2,
                instance: 0
            }
        ])
        .is_err()
    );
    assert_eq!(p, before);
}

#[test]
fn changing_volume_kind_cannot_remove_last_positive_part() {
    let (mut project, key) = fixture();
    let before = project.clone();
    assert!(
        project
            .set_volume_kind(key.object, 0, VolumeKind::NegativeVolume)
            .is_err()
    );
    assert_eq!(project, before);
    let extra = project.objects[0].volumes[0].clone();
    project.objects[0].volumes.push(extra);
    project
        .set_volume_kind(key.object, 1, VolumeKind::NegativeVolume)
        .unwrap();
    assert_eq!(
        project.objects[0].volumes[1].kind,
        VolumeKind::NegativeVolume
    );
}

#[test]
fn live_drag_splits_only_the_selected_volume() {
    let (mut project, key) = fixture();
    let mut second = project.objects[0].volumes[0].clone();
    second.transform = DAffine3::from_translation(DVec3::X * 30.);
    project.objects[0].volumes.push(second);
    let plate = project.instance(key).unwrap().plate;
    let (rest, only) = project
        .plate_mesh_split_volume(plate, key, Some(1))
        .unwrap();
    assert_eq!(rest.positions.len(), 12);
    assert_eq!(only.positions.len(), 12);
    assert!(rest.positions.iter().all(|p| p[0] <= 20.));
    assert!(only.positions.iter().all(|p| p[0] >= 30.));
    let (rest, whole) = project.plate_mesh_split(plate, key).unwrap();
    assert!(rest.positions.is_empty());
    assert_eq!(whole.positions.len(), 24);
}

#[test]
fn overflowing_transforms_leave_project_unchanged() {
    for operation in 0..4 {
        let (mut p, key) = fixture();
        let before = p.clone();
        let result = match operation {
            0 => p.scale(key, DVec3::splat(f64::MAX)),
            1 => p.scale_along_axes(key, DVec3::splat(f64::MAX)),
            2 => p.scale(key, DVec3::splat(1e-10)),
            _ => {
                p.objects[0].instances[0].transform.translation = DVec3::splat(f64::MAX);
                let before = p.clone();
                assert!(p.translate(key, DVec3::splat(f64::MAX)).is_err());
                assert_eq!(p, before);
                continue;
            }
        };
        assert!(result.is_err());
        assert_eq!(p, before);
    }
}
