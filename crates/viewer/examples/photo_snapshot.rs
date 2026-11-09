//! Знімок режиму «Фото» поза екраном, без застосунку:
//!
//! ```sh
//! cargo run --release -p viewer --example photo_snapshot -- plate_1.gcode /tmp/photo.png \
//!     [--preset silk] [--color #C0392B] [--floor] [--size 1600x1000] [--yaw -60 --pitch 25]
//!     [--zoom 1.0] [--layers 0.5] [--supports shown|dimmed|hidden]
//! ```
//! Матеріал — з конфігу G-code (`filament_type`, `filament_settings_id`, `filament_colour`),
//! `--preset`/`--color` замінюють його для всіх слотів.

use anyhow::{Context as _, Result};
use viewer::{Camera, MaterialTable, PhotoSettings, Stage, SupportView, ToolpathGpuExt as _};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let gcode = args.next().context("шлях до G-code")?;
    let out = args.next().context("шлях до PNG")?;
    let (mut preset, mut color, mut stage) = (None, None, Stage::Bed);
    let (mut w, mut h) = (1600u32, 1000u32);
    let (mut yaw, mut pitch, mut zoom, mut layers) = (None, None, 1.0f32, 1.0f32);
    let mut supports = SupportView::Dimmed;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--preset" => preset = args.next(),
            "--color" => color = args.next().and_then(|c| viewer::photo::material::parse_hex(&c)),
            "--floor" => stage = Stage::Floor,
            "--size" => {
                let s = args.next().context("--size WxH")?;
                let (a, b) = s.split_once('x').context("--size WxH")?;
                (w, h) = (a.parse()?, b.parse()?);
            }
            "--yaw" => yaw = Some(args.next().context("--yaw")?.parse::<f32>()?),
            "--pitch" => pitch = Some(args.next().context("--pitch")?.parse::<f32>()?),
            "--zoom" => zoom = args.next().context("--zoom")?.parse()?,
            "--layers" => layers = args.next().context("--layers")?.parse()?,
            "--supports" => {
                supports = match args.next().as_deref() {
                    Some("shown") => SupportView::Shown,
                    Some("hidden") => SupportView::Hidden,
                    _ => SupportView::Dimmed,
                }
            }
            other => anyhow::bail!("невідомий аргумент {other}"),
        }
    }
    let toolpath = gcode::parse_file(std::path::Path::new(&gcode))?;
    let mut renderer = viewer::Renderer::new()?;
    let built = toolpath.build_photo(renderer.device());
    renderer.set_toolpath(&built.gpu);
    let top = ((toolpath.layer_count() as f32 * layers).round() as usize).clamp(1, toolpath.layer_count().max(1));
    renderer.set_visible_layers(0..top);

    let table = MaterialTable::builtin();
    let slots = gcode::filament_slots(&toolpath);
    let materials = (0..slots.len().max(1))
        .map(|i| {
            let slot = slots.get(i).cloned().unwrap_or_default();
            let index = preset
                .as_deref()
                .and_then(|p| table.index_of(p))
                .unwrap_or_else(|| table.select(&slot.kind, &slot.name));
            eprintln!("слот {i}: {} «{}» → {}", slot.kind, slot.name, table.presets[index].id);
            table.resolve(index, color.or(slot.colour).unwrap_or([0.8; 3]))
        })
        .collect();
    let started = std::time::Instant::now();
    renderer.set_photo(Some(PhotoSettings {
        materials,
        supports,
        stage,
        ..Default::default()
    }));
    eprintln!(
        "вхід у «Фото»: {:.1} мс (запікання {:.1} мс), ресурси ≈ {:.1} МБ",
        started.elapsed().as_secs_f64() * 1e3,
        renderer.photo_bake_time().unwrap_or_default().as_secs_f64() * 1e3,
        renderer.photo_bytes().unwrap_or(0) as f64 / 1048576.0
    );
    let mut camera = Camera::default();
    if let Some(y) = yaw {
        camera.yaw = y.to_radians();
    }
    if let Some(p) = pitch {
        camera.pitch = p.to_radians();
    }
    let (lo, hi) = toolpath.bounds().context("траєкторія без екструзії")?;
    camera.frame_tight(lo, hi);
    camera.distance /= zoom;
    let png = renderer.render_photo_png(w, h, &camera)?;
    std::fs::write(&out, png)?;
    eprintln!("{out}: {w}×{h}, кадр {:.1} мс", renderer.last_frame_time.as_secs_f64() * 1e3);
    Ok(())
}
