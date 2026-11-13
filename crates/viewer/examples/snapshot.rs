//! Headless-знімок вʼюпорту в PNG: `cargo run -p viewer --example snapshot -- [model.stl] [out.png]`.

use std::path::PathBuf;

use viewer::{Camera, Mesh, Plate, Renderer, load_model};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    let started = std::time::Instant::now();
    let plates = match args.next() {
        Some(path) if path.as_os_str() != "-" => load_model(&path)?,
        _ => vec![Plate {
            number: 1,
            name: None,
            mesh: Mesh::calibration_cube(),
        }],
    };
    let load_time = started.elapsed();
    let out = args.next().unwrap_or_else(|| "snapshot.png".into());

    let mut renderer = Renderer::new()?;
    let (w, h) = (1280, 800);
    // Кілька пластин → out.png, out.p2.png, out.p3.png …
    for (i, plate) in plates.iter().enumerate() {
        let path = if i == 0 {
            out.clone()
        } else {
            out.with_extension(format!("p{}.png", plate.number))
        };
        renderer.set_mesh(&plate.mesh);
        let mut camera = Camera::default();
        camera.frame(plate.mesh.bounds());
        let mut pixels = renderer.render(w, h, &camera);
        for px in pixels.as_chunks_mut::<4>().0 {
            px.swap(0, 2); // BGRA → RGBA
        }
        image::RgbaImage::from_raw(w, h, pixels)
            .unwrap()
            .save(&path)?;
        println!(
            "{}: {} трикутн. (завантаження {:.0} мс) → {} (кадр {:.1} мс)",
            plate.title(),
            plate.mesh.triangle_count(),
            load_time.as_secs_f64() * 1000.0,
            path.display(),
            renderer.last_frame_time.as_secs_f64() * 1000.0,
        );
    }
    Ok(())
}
