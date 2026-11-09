use glam::Vec3;
use viewer::{Camera, ToolpathGpuData, ToolpathInstance};

#[test]
fn storage_toolpath_ranges_rebind_and_orbit() {
    let Some(mut renderer) = viewer::test_renderer("storage_toolpath_ranges_rebind_and_orbit")
    else {
        return;
    };
    // Шейдер читає упаковані 17 слів; зміна ABI не має мовчки псувати координати.
    assert_eq!(std::mem::size_of::<ToolpathInstance>(), 17 * 4);
    assert_eq!(std::mem::offset_of!(ToolpathInstance, caps), 16 * 4);
    let mut camera = Camera::default();
    camera.frame_tight(Vec3::new(0., 0., 0.), Vec3::new(20., 20., 2.));
    renderer.set_show_bed(false);
    let empty = renderer.render(192, 128, &camera);
    let instances: Vec<_> = (0..32)
        .map(|i| {
            ToolpathInstance::new(
                Vec3::new(i as f32 * 0.4, 4., 1.),
                Vec3::new(i as f32 * 0.4 + 0.4, 4., 1.),
                [1., 0.3, 0.1, 1.],
                0.8,
                0.4,
            )
        })
        .collect();
    let data = ToolpathGpuData::new(
        renderer.device(),
        &instances,
        vec![0..16, 16..32],
        None,
        vec![],
        Some((Vec3::ZERO, Vec3::splat(20.))),
    );
    assert!(data.lod.is_some(), "прямі сегменти мають злитись");
    renderer.set_toolpath_lod(false);
    renderer.set_toolpath(&data);
    renderer.set_visible_ranges(17..25, 0..0);
    let partial = renderer.render(192, 128, &camera);
    assert_ne!(partial, empty);
    // Ненульовий початок draw має читати саме вибрані сегменти зі storage-буфера.
    let slice = ToolpathGpuData::new(
        renderer.device(),
        &instances[17..25],
        vec![0..8],
        None,
        vec![],
        data.bounds,
    );
    renderer.set_toolpath(&slice);
    renderer.set_all_layers_visible();
    assert_eq!(renderer.render(192, 128, &camera), partial);
    renderer.set_toolpath(&data);
    renderer.set_toolpath_lod(true);
    renderer.set_visible_layers(1..2);
    assert_ne!(renderer.render(192, 128, &camera), empty);
    renderer.set_visible_layers(2..2);
    assert_eq!(renderer.render(192, 128, &camera), empty);
    renderer.set_all_layers_visible();
    let first = renderer.render(192, 128, &camera);
    camera.orbit(20., 5.);
    assert_ne!(renderer.render(192, 128, &camera), first);
    renderer.clear_toolpath();
    assert_eq!(renderer.render(192, 128, &camera), empty);
}
