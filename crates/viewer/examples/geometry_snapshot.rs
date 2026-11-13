//! cargo run -p viewer --example geometry_snapshot -- angel_medium.3mf /tmp/angel-boolean.png [plate]
//! Writes the actual boolean result, plus a raw parts comparison next to it.
use std::{path::PathBuf, time::Instant};
use viewer::{Camera, Mesh, Renderer};
fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let input = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("usage: snapshot model.3mf output.png [plate]"))?;
    let output = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| "boolean.png".into());
    let plate = args
        .next()
        .map(|p| p.to_string_lossy().parse::<u32>())
        .transpose()?
        .unwrap_or(1);
    let started = Instant::now();
    let project = project::Project::load(input)?;
    eprintln!("load: {:?}", started.elapsed());
    for object in &project.objects {
        for volume in &object.volumes {
            if volume.kind == project::VolumeKind::NegativeVolume {
                eprintln!(
                    "negative volume {:?}: {} triangles, transform {:?}",
                    volume.name,
                    volume.mesh.triangles.len(),
                    volume.transform
                );
            }
        }
    }
    let started = Instant::now();
    let mesh = geometry::plate_preview(&project, if plate == 0 { None } else { Some(plate) })?;
    let mesh = Mesh { positions: mesh.positions, normals: mesh.normals };
    eprintln!(
        "boolean + export: {:?}; {} triangles",
        started.elapsed(),
        mesh.triangle_count()
    );
    let raw = project.plate_mesh(if plate == 0 { None } else { Some(plate) })?;
    let raw = Mesh {
        positions: raw.positions,
        normals: raw.normals,
    };
    let selected_plate = if plate == 0 { None } else { Some(plate) };
    let mut focus = None;
    for object in &project.objects {
        for (instance, placement) in object.instances.iter().enumerate() {
            if placement.plate != selected_plate {
                continue;
            }
            if let Some(bounds) = project.instance_bounds_with_kinds(
                project::InstanceRef {
                    object: object.id,
                    instance,
                },
                &[project::VolumeKind::NegativeVolume],
            )? {
                focus = Some(match focus {
                    Some((lo, hi)) => (
                        project::DVec3::min(lo, bounds.min),
                        project::DVec3::max(hi, bounds.max),
                    ),
                    None => (bounds.min, bounds.max),
                });
            }
        }
    }
    let mut renderer = Renderer::new()?;
    let mut camera = Camera::default();
    if let Some((lo, hi)) = raw.bounds() {
        camera.frame_tight(lo, hi);
    }
    for (path, mesh) in [
        (output.clone(), mesh),
        (output.with_extension("raw.png"), raw),
    ] {
        renderer.set_mesh(&mesh);
        let mut pixels = renderer.render(1280, 1000, &camera);
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
        image::RgbaImage::from_raw(1280, 1000, pixels)
            .unwrap()
            .save(&path)?;
        eprintln!("wrote {}", path.display());
        // Negative cutters often enter from the underside (including angel).
        // Hide the bed and use the same underside camera for both versions.
        renderer.set_show_bed(false);
        let mut underside = camera.clone();
        underside.set_angles(-100., -65.);
        let mut pixels = renderer.render(1280, 1000, &underside);
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
        let underside_path = path.with_extension("underside.png");
        image::RgbaImage::from_raw(1280, 1000, pixels)
            .unwrap()
            .save(&underside_path)?;
        if let Some((lo, hi)) = focus {
            let mut detail = camera.clone();
            detail.frame_tight(lo.as_vec3(), hi.as_vec3());
            let mut pixels = renderer.render(1280, 1000, &detail);
            for pixel in pixels.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
            }
            let detail_path = path.with_extension("cut-detail.png");
            image::RgbaImage::from_raw(1280, 1000, pixels)
                .unwrap()
                .save(&detail_path)?;
            eprintln!("wrote {}", detail_path.display());
        }
        renderer.set_show_bed(true);
        eprintln!("wrote {}", underside_path.display());
    }
    Ok(())
}
