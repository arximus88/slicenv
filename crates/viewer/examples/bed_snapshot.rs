//! Знімок порожнього столу згори з ресурсами встановленого рушія — щоб побачити, як лягають
//! логотип і написи типу пластини:
//!
//! `cargo run -p viewer --example bed_snapshot -- <resources> <модель.stl> <розмір> <тип> <режим> [out.png] [top|orca]`
//!
//! `<resources>` — тека `resources` рушія (у ній `profiles/BBL` і `images`). Режими:
//! `decals` — SVG-написи Bambu, як у Bambu Studio й OrcaSlicer; `fallback` — SVG не знайшлись,
//! один рядок «Bambu <тип>» (так робить застосунок); `logo` — логотип профілю й підпис типу
//! пластини поверх нього (так застосунок робить для логотипів інших виробників).
//! Ракурс: `top` — згори, X праворуч; `orca` — як на типовому знімку OrcaSlicer, X догори.
use std::path::PathBuf;

use anyhow::Context as _;
use glam::Vec2;
use viewer::{BedAssets, BedDecal, BedImage, Camera, Mesh, Renderer};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [res, model, size, bed_type, mode, rest @ ..] = args.as_slice() else {
        anyhow::bail!("аргументи: <resources> <модель.stl> <розмір> <тип> <decals|fallback|logo> [out.png] [top|orca]");
    };
    let res = PathBuf::from(res);
    let size: f32 = size.parse().context("розмір столу, мм")?;
    let out = rest.first().map_or_else(|| PathBuf::from("bed.png"), PathBuf::from);
    let orca_view = rest.get(1).is_some_and(|v| v == "orca");
    let (lo, hi) = (Vec2::ZERO, Vec2::splat(size));

    let mut mesh = viewer::load_stl_raw(&res.join("profiles/BBL").join(model))?;
    if let Some((a, b)) = mesh.bounds() {
        let covers = a.x <= lo.x + 1. && a.y <= lo.y + 1. && b.x >= hi.x - 1. && b.y >= hi.y - 1.;
        if !covers {
            let shift = ((lo + hi) * 0.5 - (a.truncate() + b.truncate()) * 0.5).extend(0.);
            for p in &mut mesh.positions {
                *p = (glam::Vec3::from(*p) + shift).to_array();
            }
        }
    }
    let mut assets = BedAssets {
        model: Some(mesh),
        type_label: Some(bed_type.clone()),
        key: format!("{model}|{bed_type}|{mode}|{size}"),
        ..Default::default()
    };
    if mode == "decals" {
        let t = bed_type.to_ascii_lowercase();
        let prefix = if t.contains("textured pei") {
            "pte"
        } else if t.contains("high temp") || t.contains("smooth pei") {
            "pei"
        } else if t.contains("engineering") {
            "ep"
        } else {
            "pc"
        };
        let [left, bottom] = viewer::bambu_decal_rects(hi - lo);
        for (name, (min, max)) in [("left", left), ("bottom", bottom)] {
            let path = res.join("images").join(format!("bbl_bed_{prefix}_{name}.svg"));
            assets.decals.push(BedDecal {
                min: lo + min,
                max: lo + max,
                image: BedImage::Svg(std::fs::read(path)?),
            });
        }
    } else if mode == "fallback" {
        assets.type_label = Some(format!("Bambu {bed_type}"));
    } else {
        assets.logo = Some(BedImage::Svg(std::fs::read(
            res.join("profiles/BBL/bbl-3dp-logo.svg"),
        )?));
    }

    let mut renderer = Renderer::new()?;
    renderer.set_mesh(&Mesh::default());
    renderer.set_bed_bounds(lo.to_array(), hi.to_array());
    renderer.set_bed_assets(Some(assets));
    let mut camera = Camera {
        target: ((lo + hi) * 0.5).extend(0.),
        ..Default::default()
    };
    if orca_view {
        // Око з боку −X: X іде догори кадру, Y — ліворуч, як на знімку OrcaSlicer.
        camera.set_angles(180., 62.);
        camera.distance = size * 2.1;
    } else {
        camera.set_angles(-90., 89.);
        camera.distance = size * 1.9;
    }
    let (w, h) = (1000, 1000);
    let mut pixels = renderer.render(w, h, &camera);
    for px in pixels.chunks_exact_mut(4) {
        px.swap(0, 2); // BGRA → RGBA
    }
    image::RgbaImage::from_raw(w, h, pixels).unwrap().save(&out)?;
    println!("{}", out.display());
    Ok(())
}
