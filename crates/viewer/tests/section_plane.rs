use glam::Vec3;
use viewer::{Camera, Mesh, ToolpathGpuData, ToolpathInstance};

const SIZE: u32 = 160;

fn cyan(pixel: &[u8]) -> bool {
    // Readback is BGRA: both blue and green should dominate red.
    pixel[0] > pixel[2].saturating_add(35) && pixel[1] > pixel[2].saturating_add(15)
}

#[test]
fn ghost_is_opt_in_visible_on_all_axes_and_both_sides_and_independent_of_clipping() {
    let Some(mut renderer) = viewer::test_renderer("section_ghost_visibility") else {
        return;
    };
    renderer.set_show_bed(false);
    let bounds = (Vec3::splat(-10.), Vec3::splat(10.));
    for (normal, yaw, pitch) in [(Vec3::X, 0., 0.), (Vec3::Y, 90., 0.), (Vec3::Z, 0., 75.)] {
        for side in [-1., 1.] {
            let mut camera = Camera {
                target: Vec3::ZERO,
                distance: 70.,
                ..Camera::default()
            };
            camera.set_angles(yaw + if side < 0. { 180. } else { 0. }, pitch * side);
            renderer.set_section_plane(None);
            let background = renderer.render(SIZE, SIZE, &camera);
            let plane = [normal.x, normal.y, normal.z, 0.];
            renderer.set_clip_plane(Some(plane));
            assert_eq!(renderer.render(SIZE, SIZE, &camera), background);
            renderer.set_section_plane(Some((plane, bounds)));
            let ghost = renderer.render(SIZE, SIZE, &camera);
            assert!(ghost.as_chunks::<4>().0.iter().filter(|p| cyan(*p)).count() > 1000);
            let center = ((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize;
            assert!(
                cyan(&ghost[center..center + 4]),
                "fill missing for {normal:?}"
            );
            let fill = ghost[center];
            assert!(
                ghost
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .filter(|p| cyan(*p) && p[0] > fill + 35)
                    .count()
                    > 100,
                "bright border missing for {normal:?}"
            );
            // The guide must survive even a mesh clip equation that discards everything.
            renderer.set_clip_plane(Some([0., 0., 0., 1.]));
            assert_eq!(renderer.render(SIZE, SIZE, &camera), ghost);
            renderer.set_section_plane(None);
            assert_eq!(renderer.render(SIZE, SIZE, &camera), background);
        }
    }

    let camera = Camera {
        target: Vec3::ZERO,
        distance: 70.,
        ..Camera::default()
    };
    let instances = [ToolpathInstance::new(
        Vec3::new(-8., 0., 0.),
        Vec3::new(8., 0., 0.),
        [1., 0.2, 0.1, 1.],
        2.,
        1.,
    )];
    let toolpath = ToolpathGpuData::new(
        renderer.device(),
        &instances,
        vec![0..1],
        None,
        vec![],
        Some(bounds),
    );
    renderer.set_toolpath(&toolpath);
    renderer.set_clip_plane(None);
    let preview = renderer.render(SIZE, SIZE, &camera);
    renderer.set_clip_plane(Some([0., 0., 0., 1.]));
    assert_eq!(renderer.render(SIZE, SIZE, &camera), preview);
    renderer.set_section_plane(Some(([0., 0., 1., 0.], bounds)));
    assert_ne!(renderer.render(SIZE, SIZE, &camera), preview);
    renderer.set_section_plane(None);
    assert_eq!(renderer.render(SIZE, SIZE, &camera), preview);
}

#[test]
fn ghost_is_occluded_by_mesh_and_does_not_write_depth() {
    let Some(mut renderer) = viewer::test_renderer("section_ghost_depth") else {
        return;
    };
    renderer.set_show_bed(false);
    let mut camera = Camera {
        target: Vec3::ZERO,
        distance: 70.,
        ..Camera::default()
    };
    camera.set_angles(0., 0.);
    let bounds = (Vec3::splat(-10.), Vec3::splat(10.));
    // Opaque wall in front of the whole guide, facing the camera on +X.
    let wall = Mesh {
        positions: vec![
            [5., -20., -20.],
            [5., 20., -20.],
            [5., 20., 20.],
            [5., -20., -20.],
            [5., 20., 20.],
            [5., -20., 20.],
        ],
        normals: vec![[1., 0., 0.]; 6],
    };
    renderer.set_mesh(&wall);
    let opaque = renderer.render(SIZE, SIZE, &camera);
    renderer.set_section_plane(Some(([1., 0., 0., 0.], bounds)));
    assert_eq!(
        renderer.render(SIZE, SIZE, &camera),
        opaque,
        "guide ignored mesh depth"
    );
    renderer.set_section_plane(Some(([1., 0., 0., -10.], bounds)));
    let in_front = renderer.render(SIZE, SIZE, &camera);
    assert_ne!(in_front, opaque, "front guide hidden");
    let center = ((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize;
    assert_ne!(&in_front[center..center + 4], &opaque[center..center + 4]);

    renderer.set_mesh(&Mesh::default());
    renderer.set_section_plane(None);
    let background = renderer.render(SIZE, SIZE, &camera);
    // Highlight lines draw after the guide and use depth testing. They remain visible
    // behind it only if the guide leaves the depth buffer untouched.
    renderer.set_highlight(Some((Vec3::new(-3., -8., -8.), Vec3::new(-1., 8., 8.))));
    let lines = renderer.render(SIZE, SIZE, &camera);
    renderer.set_section_plane(Some(([1., 0., 0., 0.], bounds)));
    let with_guide = renderer.render(SIZE, SIZE, &camera);
    let preserved = lines
        .as_chunks::<4>()
        .0
        .iter()
        .zip(background.as_chunks::<4>().0)
        .zip(with_guide.as_chunks::<4>().0)
        .filter(|((line, bg), guide)| line != bg && line == guide)
        .count();
    assert!(
        preserved > 100,
        "guide wrote depth: only {preserved} line pixels survived"
    );
}
