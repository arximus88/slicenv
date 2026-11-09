//! Headless-рендер траєкторії G-code у PNG:
//! `cargo run -p viewer --example gcode_preview -- path/to/plate_1.gcode out.png [max_layer]`

use std::path::PathBuf;
use std::time::Instant;

use gcode::parse_file;
use viewer::{Camera, Renderer, ToolpathGpuExt};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    let gcode_path = match args.next() {
        Some(p) => p,
        None => {
            eprintln!("Використання: preview <path/to/plate.gcode> <out.png> [max_layer]");
            std::process::exit(1);
        }
    };
    let out_path = args.next().unwrap_or_else(|| "out.png".into());
    let max_layer = args
        .next()
        .and_then(|s| s.to_string_lossy().parse::<usize>().ok());

    let parse_start = Instant::now();
    let toolpath = parse_file(&gcode_path)?;
    let parse_time = parse_start.elapsed();

    let mut renderer = Renderer::new()?;
    // Схема кольорів: SCHEME=0..8 (тип лінії, висота, ширина, швидкість, вентилятор, …).
    let scheme = std::env::var("SCHEME")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .and_then(|i| gcode::ColorScheme::ALL.get(i).copied())
        .unwrap_or_default();
    let built = toolpath.build(
        renderer.device(),
        &gcode::ViewOptions {
            scheme,
            // HIDDEN=default — як у застосунку (без ретрактів і витирань); HIDDEN=markers —
            // ще й без швів. Без змінної — усе видно.
            hidden: match std::env::var("HIDDEN").as_deref() {
                Ok("default") => gcode::ViewOptions::HIDDEN_BY_DEFAULT.to_vec(),
                Ok("markers") => vec![
                    gcode::Role::Retract,
                    gcode::Role::Unretract,
                    gcode::Role::Wipe,
                    gcode::Role::Seam,
                ],
                _ => Vec::new(),
            },
        },
    );
    println!("{}: {:?}", scheme.name_uk(), built.range);
    renderer.set_flat_toolpath(std::env::var_os("FLAT_LINES").is_some());
    // NO_LOD=1 — завжди повна траєкторія (порівняти зі спрощеною на загальному плані).
    renderer.set_toolpath_lod(std::env::var_os("NO_LOD").is_none());
    let gpu_data = built.gpu;
    renderer.set_toolpath(&gpu_data);

    let total_layers = toolpath.layer_count();
    let visible_end = match max_layer {
        Some(m) => {
            let end = m.min(total_layers);
            renderer.set_visible_layers(0..end);
            end
        }
        None => total_layers,
    };

    let mut camera = Camera::default();
    camera.frame(toolpath.bounds());
    // Крупний план для звірки вигляду ліній: CAM="yaw,pitch,dist[,x,y,z]" (градуси, мм).
    if let Ok(spec) = std::env::var("CAM") {
        let v: Vec<f32> = spec
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect();
        if let [yaw, pitch, dist, rest @ ..] = v.as_slice() {
            camera.yaw = yaw.to_radians();
            camera.pitch = pitch.to_radians();
            camera.distance = *dist;
            if let [x, y, z] = rest {
                camera.target = glam::Vec3::new(*x, *y, *z);
            }
        }
    }

    // SIZE="2560x1440" — роздільність кадру; FRAMES=N — серія кадрів, друкується медіана.
    let (w, h) = std::env::var("SIZE")
        .ok()
        .and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((1280, 800));
    let frames: usize = std::env::var("FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let mut times = Vec::with_capacity(frames);
    let mut pixels = Vec::new();
    // IDLE_MS=N — пауза між кадрами, як у застосунку між поданнями (частоти iGPU).
    let idle = std::env::var("IDLE_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(std::time::Duration::from_millis);
    for _ in 0..frames.max(1) {
        if let Some(idle) = idle {
            std::thread::sleep(idle);
        }
        pixels = renderer.render(w, h, &camera);
        times.push(renderer.last_frame_time.as_secs_f64() * 1000.0);
    }
    if frames > 1 {
        times.sort_by(f64::total_cmp);
        println!(
            "{frames} кадрів {w}×{h}: медіана {:.1} мс, мін {:.1} мс",
            times[times.len() / 2],
            times[0]
        );
    }
    for px in pixels.chunks_exact_mut(4) {
        px.swap(0, 2); // BGRA -> RGBA
    }

    image::RgbaImage::from_raw(w, h, pixels)
        .expect("не вдалося створити буфер зображення")
        .save(&out_path)?;

    println!(
        "Рендер {} шарів (із {}) у {} ({:.1} мс парсинг, {:.1} мс рендер GPU)",
        visible_end,
        total_layers,
        out_path.display(),
        parse_time.as_secs_f64() * 1000.0,
        renderer.last_frame_time.as_secs_f64() * 1000.0
    );

    Ok(())
}
