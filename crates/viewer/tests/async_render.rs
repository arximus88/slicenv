use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use viewer::{Camera, Mesh, RenderedFrame, Renderer};

fn complete(renderer: &mut Renderer) -> Arc<RenderedFrame> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(frame) = renderer.poll_frame().expect("poll frame") {
            assert!(!renderer.has_pending_frame());
            return frame;
        }
        assert!(Instant::now() < deadline, "GPU frame did not complete");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn async_readback_resize_cutaway_and_device_loss() {
    let Some(mut renderer) = viewer::test_renderer("async_readback_resize_cutaway_and_device_loss") else {
        return;
    };
    let mesh = Mesh::calibration_cube();
    let (lo, hi) = mesh.bounds().unwrap();
    let mut camera = Camera::default();
    camera.frame_tight(lo, hi);
    renderer.set_show_bed(false);
    renderer.set_mesh(&mesh);
    assert!(renderer.poll_frame().unwrap().is_none());
    assert!(renderer.ready_frame().is_none());
    assert!(renderer.submit_frame(67, 53, &camera).unwrap());
    assert!(renderer.has_pending_frame());
    // No implicit poll/wait or queued resize; even if the GPU has finished, the
    // mapping must be consumed before this buffer can be reused.
    assert!(!renderer.submit_frame(91, 39, &camera).unwrap());
    let first = complete(&mut renderer);
    assert_eq!(
        (first.width, first.height, first.pixels.len()),
        (67, 53, 67 * 53 * 4)
    );
    assert_eq!(first.pixels, renderer.render(67, 53, &camera));

    assert!(renderer.submit_frame(91, 39, &camera).unwrap());
    assert!(Arc::ptr_eq(renderer.ready_frame().unwrap(), &first));
    // Synchronous headless snapshots also cannot reuse the in-flight buffer.
    assert_eq!(renderer.render(67, 53, &camera), first.pixels);
    let resized = complete(&mut renderer);
    assert_eq!(
        (resized.width, resized.height, resized.pixels.len()),
        (91, 39, 91 * 39 * 4)
    );
    assert!(renderer.poll_frame().unwrap().is_none());
    assert!(Arc::ptr_eq(renderer.ready_frame().unwrap(), &resized));

    // A partial cut must change visible pixels without mutating the source mesh.
    renderer.set_clip_plane(Some([0., 0., 1., -(lo.z + hi.z) * 0.5]));
    assert!(renderer.submit_frame(67, 53, &camera).unwrap());
    let cut = complete(&mut renderer);
    assert_ne!(cut.pixels, first.pixels);
    renderer.set_clip_plane(Some([0., 0., 1., 1.]));
    let all_cut = renderer.render(67, 53, &camera);
    renderer.set_mesh(&Mesh::default());
    let background = renderer.render(67, 53, &camera);
    assert!(
        all_cut == background,
        "all-cut differs at {:?}; total {}",
        all_cut.iter().zip(&background).position(|(a, b)| a != b),
        all_cut
            .iter()
            .zip(&background)
            .filter(|(a, b)| a != b)
            .count()
    );
    assert_ne!(cut.pixels, background);
    renderer.set_mesh(&mesh);
    renderer.set_clip_plane(None);
    assert_eq!(renderer.render(67, 53, &camera), first.pixels);

    renderer.set_mesh(&Mesh::default());
    renderer.set_modifiers(&mesh);
    renderer.set_clip_plane(Some([0., 0., 1., 1.]));
    assert_ne!(renderer.render(67, 53, &camera), background);
    renderer.set_modifiers(&Mesh::default());
    assert_eq!(renderer.render(67, 53, &camera), background);

    // Hiding the model/setting a Prepare cut plane must not hide toolpaths.
    let instances = [viewer::ToolpathInstance::new(
        glam::Vec3::new(lo.x, lo.y, 10.),
        glam::Vec3::new(hi.x, hi.y, 10.),
        [1., 0.2, 0.1, 1.],
        2.,
        1.,
    )];
    let toolpath = viewer::ToolpathGpuData::new(
        renderer.device(),
        &instances,
        vec![0..1],
        None,
        vec![],
        Some((lo, hi)),
    );
    renderer.set_toolpath(&toolpath);
    let toolpath_clipped = renderer.render(67, 53, &camera);
    assert!(
        toolpath_clipped != background,
        "toolpath hidden with empty mesh"
    );
    renderer.set_clip_plane(None);
    assert!(
        renderer.render(67, 53, &camera) == toolpath_clipped,
        "mesh clip affected toolpath"
    );
    renderer.clear_toolpath();

    assert!(renderer.submit_frame(u32::MAX, 1, &camera).is_err());
    assert!(!renderer.has_pending_frame());
    assert!(Arc::ptr_eq(renderer.ready_frame().unwrap(), &cut));
    assert!(renderer.submit_frame(0, 0, &camera).unwrap());
    let tiny = complete(&mut renderer);
    assert_eq!((tiny.width, tiny.height, tiny.pixels.len()), (1, 1, 4));

    assert!(renderer.submit_frame(67, 53, &camera).unwrap());
    renderer.device().destroy();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if renderer.poll_frame().is_err() {
            break;
        }
        assert!(Instant::now() < deadline, "device loss was not reported");
        thread::sleep(Duration::from_millis(1));
    }
    assert!(!renderer.has_pending_frame());
    assert!(renderer.ready_frame().is_some());
    assert!(renderer.submit_frame(67, 53, &camera).is_err());
}

/// Preview кличе `set_mesh` (одиночна сітка); елементи «Підготовки» не мають лишатися
/// поверх траєкторій (знахідка P1 рев’ю 2026-11-04).
#[test]
fn single_mesh_mode_hides_prepare_scene_items() {
    let mut renderer = match Renderer::new() {
        Ok(renderer) => renderer,
        Err(error) => {
            eprintln!("skipping GPU test: {error:#}");
            return;
        }
    };
    let cube = Mesh::calibration_cube();
    let (lo, hi) = cube.bounds().unwrap();
    let mut camera = Camera::default();
    camera.frame_tight(lo, hi);
    renderer.set_show_bed(false);
    let render = |renderer: &mut Renderer| {
        assert!(renderer.submit_frame(64, 64, &camera).unwrap());
        complete(renderer).pixels.clone()
    };
    renderer.set_mesh(&Mesh::default());
    let empty = render(&mut renderer);
    renderer.set_volume_mesh(7, &cube);
    renderer.set_scene_items(&[(7, glam::Mat4::IDENTITY)]);
    let with_items = render(&mut renderer);
    assert_ne!(with_items, empty, "the scene item must be visible in Prepare mode");
    renderer.set_mesh(&Mesh::default());
    assert_eq!(render(&mut renderer), empty);
    // Повернення на «Підготовку»: сітка частини лишилась завантаженою.
    assert!(renderer.has_volume_mesh(7));
    renderer.set_scene_items(&[(7, glam::Mat4::IDENTITY)]);
    assert_eq!(render(&mut renderer), with_items);
}
