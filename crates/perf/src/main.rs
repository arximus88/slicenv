//! Регресійні бюджети продуктивності без вікна й без GPUI.
//!
//! `cargo run --release -p perf` міряє гарячі шляхи на процедурній сфері 1 М △ і порівнює
//! медіани з `bench/baselines.json`: вихід 1, якщо метрика повільніша за
//! `max(tolerance × baseline, baseline + floor_ms)` або порушено інваріант (наприклад,
//! рамка після перенесення має братися з кешу). `--update-baseline` переписує розділ
//! `headless` поточними числами, решту файлу (бюджети GUI-бенча) лишає як є.
//!
//! GPU-сценарії пропускаються, якщо wgpu-адаптера немає (`SLICENV_REQUIRE_GPU=1` — провал).
//! `--invariants-only` (CI) друкує числа, але валить лише інваріанти: baseline записано на
//! конкретній машині й на іншому залізі не порівнюваний.

use std::{
    path::PathBuf,
    process::ExitCode,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result};
use glam::{Mat4, Vec3};
use project::{DVec3, IndexedMesh, InstanceRef, Project, bounds_cache, history};
use serde_json::{Map, Value, json};
use viewer::{Camera, Renderer};

/// Розмір кадру як у вікні 1600×1000 без бічної панелі.
const FRAME: (u32, u32) = (1300, 900);

struct Args {
    update: bool,
    /// Лише машинонезалежні інваріанти (CI): baseline цієї машини на чужому залізі не діє.
    invariants_only: bool,
    gpu: bool,
    baseline: PathBuf,
    only: Option<String>,
}

fn args() -> Result<Args> {
    let mut out = Args {
        update: false,
        invariants_only: false,
        gpu: true,
        baseline: PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../bench/baselines.json")),
        only: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--update-baseline" => out.update = true,
            "--no-gpu" => out.gpu = false,
            "--invariants-only" => out.invariants_only = true,
            "--baseline" => out.baseline = it.next().context("--baseline PATH")?.into(),
            "--only" => out.only = Some(it.next().context("--only SUBSTRING")?),
            // `cargo bench` передає власний прапорець.
            "--bench" => {}
            "-h" | "--help" => {
                println!(
                    "perf [--update-baseline] [--no-gpu] [--invariants-only] [--only SUBSTRING] [--baseline PATH]"
                );
                std::process::exit(0);
            }
            other => anyhow::bail!("невідомий аргумент {other}"),
        }
    }
    // Справжній G-code пише в ті самі метрики, що й синтетика, — baseline не переписувати.
    if out.update && std::env::var_os("SLICENV_PERF_GCODE").is_some() {
        anyhow::bail!("--update-baseline не можна разом із SLICENV_PERF_GCODE");
    }
    Ok(out)
}

struct Metric {
    name: &'static str,
    median_ms: f64,
    p95_ms: f64,
    samples: usize,
}

#[derive(Default)]
struct Report {
    metrics: Vec<Metric>,
    /// Абсолютні інваріанти, що не залежать від машини: (опис, пройдено).
    invariants: Vec<(String, bool)>,
    skipped: Vec<String>,
    only: Option<String>,
}

impl Report {
    fn wants(&self, name: &str) -> bool {
        self.only.as_deref().is_none_or(|s| name.contains(s))
    }

    /// `f` повертає тривалість однієї ітерації: так підготовка (клон, скидання) не
    /// потрапляє в замір.
    fn time(&mut self, name: &'static str, warmup: usize, n: usize, mut f: impl FnMut() -> Duration) -> Option<f64> {
        if !self.wants(name) {
            return None;
        }
        for _ in 0..warmup {
            f();
        }
        let mut samples: Vec<f64> = (0..n).map(|_| f().as_secs_f64() * 1e3).collect();
        samples.sort_by(f64::total_cmp);
        let median_ms = samples[samples.len() / 2];
        let p95_ms = samples[((samples.len() as f64 * 0.95) as usize).min(samples.len() - 1)];
        self.metrics.push(Metric {
            name,
            median_ms,
            p95_ms,
            samples: n,
        });
        Some(median_ms)
    }

    fn invariant(&mut self, what: String, ok: bool) {
        self.invariants.push((what, ok));
    }
}

fn timed(f: impl FnOnce()) -> Duration {
    let started = Instant::now();
    f();
    started.elapsed()
}

/// UV-сфера: `2 × slices × (stacks − 1)` трикутників, полюси — по одній вершині.
fn uv_sphere(slices: u32, stacks: u32, radius: f64) -> IndexedMesh {
    let mut positions = vec![[0., 0., radius]];
    for i in 1..stacks {
        let phi = std::f64::consts::PI * i as f64 / stacks as f64;
        for j in 0..slices {
            let theta = std::f64::consts::TAU * j as f64 / slices as f64;
            positions.push([
                radius * phi.sin() * theta.cos(),
                radius * phi.sin() * theta.sin(),
                radius * phi.cos(),
            ]);
        }
    }
    positions.push([0., 0., -radius]);
    let bottom = positions.len() as u32 - 1;
    let ring = |i: u32, j: u32| 1 + (i - 1) * slices + j % slices;
    let mut triangles = Vec::with_capacity((2 * slices * (stacks - 1)) as usize);
    for j in 0..slices {
        triangles.push([0, ring(1, j), ring(1, j + 1)]);
        triangles.push([bottom, ring(stacks - 1, j + 1), ring(stacks - 1, j)]);
    }
    for i in 1..stacks - 1 {
        for j in 0..slices {
            let (a, b) = (ring(i, j), ring(i, j + 1));
            let (c, d) = (ring(i + 1, j), ring(i + 1, j + 1));
            triangles.push([a, c, b]);
            triangles.push([b, c, d]);
        }
    }
    IndexedMesh {
        positions,
        triangles,
        ..Default::default()
    }
}

fn sphere_project() -> Result<(Project, InstanceRef)> {
    let mesh = uv_sphere(1000, 501, 40.);
    assert_eq!(mesh.triangles.len(), 1_000_000);
    let mut project = Project::default();
    project.add_plate("1")?;
    let key = project.add_mesh_object("sphere", mesh, 1, [128., 128.], [0., 0.], [256., 256.])?;
    Ok((project, key))
}

fn headless(report: &mut Report, project: &Project, key: InstanceRef) -> Result<()> {
    let mesh = project.object(key.object)?.volumes[0].mesh.clone();

    report.time("viewer::local_mesh 1M", 1, 7, || timed(|| drop(std::hint::black_box(viewer::local_mesh(&mesh)))));
    report.time("viewer::plates_of 1M", 3, 200, || timed(|| drop(std::hint::black_box(viewer::plates_of(project)))));

    let cold = report.time("bounds_cache::refresh cold 1M", 2, 15, || {
        timed(|| drop(std::hint::black_box(bounds_cache::refresh(project, Default::default(), Default::default()))))
    });
    // Перенесення лишає сигнатури незмінними: оновлення кешу не торкається вершин.
    let (mut instances, mut volumes) = bounds_cache::refresh(project, Default::default(), Default::default());
    let mut moved = project.clone();
    let warm = report.time("bounds_cache::refresh after translate 1M", 5, 200, || {
        moved.translate(key, DVec3::new(0.5, 0., 0.)).unwrap();
        let (i, v) = (std::mem::take(&mut instances), std::mem::take(&mut volumes));
        let started = Instant::now();
        let (i, v) = bounds_cache::refresh(&moved, i, v);
        let elapsed = started.elapsed();
        (instances, volumes) = (i, v);
        elapsed
    });
    if report.wants("bounds_cache::refresh after translate 1M") {
        let cached = bounds_cache::cached_instance(&moved, &instances, key);
        let exact = moved.instance_bounds(key)?;
        report.invariant(
            "кеш рамок після перенесення дає точну рамку".into(),
            cached.is_some() && cached == exact,
        );
    }
    if let (Some(cold), Some(warm)) = (cold, warm) {
        let limit = (cold / 100.).max(0.05);
        report.invariant(
            format!("рамки після перенесення з кешу: {warm:.4} мс ≤ {limit:.3} мс (1 % холодного або 0.05 мс)"),
            warm <= limit,
        );
    }

    let mut edited = project.clone();
    let translate = report.time("Project::translate 1M", 10, 1000, || {
        timed(|| edited.translate(key, DVec3::new(0.1, 0., 0.)).unwrap())
    });
    if let Some(ms) = translate {
        report.invariant(format!("Project::translate не залежить від розміру меша: {ms:.4} мс ≤ 0.05 мс"), ms <= 0.05);
    }

    report.time("Project::plate_bounds 1M (hit-test)", 2, 30, || {
        timed(|| drop(std::hint::black_box(project.plate_bounds(Some(1)).unwrap())))
    });

    // Одна правка з точки зору Workspace: checkpoint (клон) → правка → History::record
    // (порівняння проєктів) і окремо undo.
    let mut current = project.clone();
    let mut log = history::History::default();
    report.time("edit + undo snapshot (checkpoint+translate+record) 1M", 3, 50, || {
        timed(|| {
            let before = history::Entry {
                label: "Переміщення".into(),
                project: current.clone(),
                selection: history::Selection::default(),
            };
            current.translate(key, DVec3::new(1., 0., 0.)).unwrap();
            assert!(log.record(before, &current));
        })
    });
    report.time("History::undo 1M", 0, 40, || {
        timed(|| {
            log.undo(&mut current, history::Selection::default()).unwrap();
        })
    });
    Ok(())
}

fn wait(renderer: &mut Renderer) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if renderer.wait_frame(Duration::from_millis(100))?.is_some() {
            renderer.release_ready();
            return Ok(());
        }
        anyhow::ensure!(Instant::now() < deadline, "GPU-кадр не завершився за 20 с");
    }
}

fn gpu(report: &mut Report, project: &Project, key: InstanceRef) -> Result<()> {
    let Some(mut renderer) = viewer::test_renderer("perf GPU budgets") else {
        report.skipped.push("GPU-сценарії: немає wgpu-адаптера".into());
        return Ok(());
    };
    let (local, _origin) = viewer::local_mesh(&project.object(key.object)?.volumes[0].mesh);
    let transform = Mat4::from_cols_array(
        &glam::DMat4::from(project.instance(key)?.transform)
            .as_mat4()
            .to_cols_array(),
    );
    report.time("Renderer::set_volume_mesh upload 1M", 1, 5, || timed(|| renderer.set_volume_mesh(1, &local)));
    renderer.set_volume_mesh(1, &local);
    renderer.set_scene_items(&[(1, transform)]);
    renderer.set_show_bed(true);

    let bounds = project.instance_bounds(key)?.context("сфера без рамки")?;
    let mut camera = Camera::default();
    camera.frame_tight(bounds.min.as_vec3(), bounds.max.as_vec3());
    let (w, h) = FRAME;
    // Перший кадр створює цілі рендерингу — його не міряємо.
    renderer.submit_frame(w, h, &camera)?;
    wait(&mut renderer)?;

    report.time("GPU frame 1M: encode+submit (CPU)", 3, 60, || {
        camera.orbit(0.01, 0.);
        let elapsed = timed(|| assert!(renderer.submit_frame(w, h, &camera).unwrap()));
        wait(&mut renderer).unwrap();
        elapsed
    });
    report.time("GPU frame 1M: encode+submit+readback", 3, 60, || {
        camera.orbit(0.01, 0.);
        timed(|| {
            assert!(renderer.submit_frame(w, h, &camera).unwrap());
            wait(&mut renderer).unwrap();
        })
    });

    // Перетягування змінює лише матриці: кадр перетягування = set_scene_items + кадр.
    let mut dx = 0.;
    report.time("drag frame 1M: set_scene_items(1)+frame", 3, 60, || {
        dx += 0.5;
        let moved = Mat4::from_translation(Vec3::new(dx, 0., 0.)) * transform;
        timed(|| {
            renderer.set_scene_items(&[(1, moved)]);
            assert!(renderer.submit_frame(w, h, &camera).unwrap());
            wait(&mut renderer).unwrap();
        })
    });
    let many: Vec<(u64, Mat4)> = (0..1000)
        .map(|i| (1 + i % 4, Mat4::from_translation(Vec3::new(i as f32, 0., 0.))))
        .collect();
    report.time("Renderer::set_scene_items(1000) CPU", 5, 200, || timed(|| renderer.set_scene_items(&many)));
    report.time("Renderer::set_scene_items(1) CPU", 5, 500, || timed(|| renderer.set_scene_items(&[(1, transform)])));
    Ok(())
}

/// Траєкторія розміру angel_medium: `SLICENV_PERF_GCODE` (справжній G-code) або синтетична
/// ажурна оболонка — 636 шарів коротких сегментів, щоб сценарій працював і без фікстури.
fn preview_toolpath() -> Result<(gcode::Toolpath, String)> {
    if let Some(path) = std::env::var_os("SLICENV_PERF_GCODE") {
        let path = PathBuf::from(path);
        let toolpath =
            gcode::parse_file(&path).with_context(|| format!("розбір {}", path.display()))?;
        return Ok((toolpath, path.display().to_string()));
    }
    Ok((
        gcode::parse(&lattice_gcode(636))?,
        "синтетична ажурна оболонка".into(),
    ))
}

/// G-code ажурної оболонки, схожої на angel_medium: `layers` шарів по 0.2 мм, у кожному
/// зовнішня й внутрішня стінки — кола r ≈ 30 мм з хорд ~0.4 мм; ромбові отвори зсуваються
/// від шару до шару. Приблизно 950 сегментів на шар.
fn lattice_gcode(layers: usize) -> String {
    use std::fmt::Write as _;
    const CHORDS: usize = 470;
    const CELL: usize = 24;
    let mut out = String::with_capacity(layers * 2 * CHORDS * 40);
    out.push_str("G90\nM83\n");
    let (cx, cy) = (128.0f32, 128.0f32);
    for layer in 0..layers {
        let z = 0.2 * (layer + 1) as f32;
        let radius = 30.0 + 8.0 * (z * 0.04).sin();
        let _ = writeln!(
            out,
            ";LAYER_CHANGE\n;Z:{z:.2}\n;HEIGHT:0.2\nG1 Z{z:.2} F600"
        );
        out.push_str("; FEATURE: Outer wall\n; LINE_WIDTH: 0.42\n");
        // Ромби: ширина отвору змінюється трикутно з висотою, фаза — через ряд.
        let row = layer / 12;
        let open = (layer % 12).min(12 - layer % 12);
        for ring in [radius, radius - 0.42] {
            let mut travel = true;
            for k in 0..=CHORDS {
                let a = std::f32::consts::TAU * k as f32 / CHORDS as f32;
                let (x, y) = (cx + ring * a.cos(), cy + ring * a.sin());
                let phase = (k + if row % 2 == 0 { 0 } else { CELL / 2 }) % CELL;
                let hole = phase < open;
                if travel || hole {
                    let _ = writeln!(out, "G0 X{x:.3} Y{y:.3} F12000");
                } else {
                    let _ = writeln!(out, "G1 X{x:.3} Y{y:.3} E0.02 F6000");
                }
                travel = hole;
            }
        }
    }
    out
}

fn preview_gpu(report: &mut Report) -> Result<()> {
    if !report.wants("Preview") {
        return Ok(());
    }
    let Some(mut renderer) = viewer::test_renderer("perf Preview budgets") else {
        report
            .skipped
            .push("Preview GPU: немає wgpu-адаптера".into());
        return Ok(());
    };
    use viewer::ToolpathGpuExt as _;
    let started = Instant::now();
    let (toolpath, source) = preview_toolpath()?;
    let parsed = started.elapsed();
    let built = toolpath.build(renderer.device(), &gcode::ViewOptions::default());
    eprintln!(
        "perf: Preview — {source}: {} шарів, {} сегментів, {} інстансів екструзії (розбір {:.0} мс)",
        toolpath.layer_count(),
        toolpath.segment_count(),
        built.gpu.total_instances,
        parsed.as_secs_f64() * 1e3
    );
    let lod_count = built
        .gpu
        .lod
        .as_ref()
        .map_or(built.gpu.total_instances, |l| l.count);
    // Довільний файл може вже містити довгі прямі: злиття гарантує лише синтетична сцена.
    if std::env::var_os("SLICENV_PERF_GCODE").is_none() {
        report.invariant(
            format!(
                "LOD траєкторії: {lod_count} з {} інстансів ≤ 80 % (злиття коротких сегментів працює)",
                built.gpu.total_instances
            ),
            lod_count * 5 <= built.gpu.total_instances * 4,
        );
    }
    report.time("Preview toolpath build (CPU+upload)", 0, 3, || {
        timed(|| {
            drop(std::hint::black_box(
                toolpath.build(renderer.device(), &gcode::ViewOptions::default()),
            ))
        })
    });
    // Повне завантаження Preview після нарізки, як у `Workspace::load_preview` (spans
    // `preview::parse_gcode` / `build_gpu` / `report`): файл з диска → розбір → GPU-буфери →
    // звіт. Окремо від «gcode::parse 800k» (рядок у пам’яті) і від побудови буферів.
    let gcode_file = match std::env::var_os("SLICENV_PERF_GCODE") {
        Some(path) => PathBuf::from(path),
        None => {
            let path = std::env::temp_dir().join(format!("slicenv-perf-{}.gcode", std::process::id()));
            std::fs::write(&path, lattice_gcode(636))?;
            path
        }
    };
    let load = |stage: u8| {
        timed(|| {
            let toolpath = gcode::parse_file(&gcode_file).unwrap();
            if stage == 0 {
                drop(std::hint::black_box(toolpath));
                return;
            }
            let built = toolpath.build(renderer.device(), &gcode::ViewOptions::default());
            let report = toolpath.report(None);
            drop(std::hint::black_box((toolpath, built, report)));
        })
    };
    report.time("Preview load: gcode::parse_file", 1, 5, || load(0));
    report.time("Preview load: parse_file+build+report", 1, 5, || load(1));
    if std::env::var_os("SLICENV_PERF_GCODE").is_none() {
        let _ = std::fs::remove_file(&gcode_file);
    }
    renderer.set_toolpath(&built.gpu);
    renderer.set_all_layers_visible();
    renderer.set_show_bed(true);
    let mut camera = Camera::default();
    let (lo, hi) = toolpath.bounds().context("траєкторія без екструзії")?;
    let (w, h) = FRAME;
    camera.frame_scene(lo, hi, w as f32 / h as f32);
    renderer.submit_frame(w, h, &camera)?;
    wait(&mut renderer)?;
    // Кадри поспіль, а також після 25 мс простою, як між подіями миші: iGPU за цей час
    // скидає частоту (780M: 2700 → 800 МГц), і саме такий кадр відчуває рука.
    for (name, lod, idle) in [
        ("Preview orbit frame: encode+submit+readback", true, 0),
        (
            "Preview orbit frame, no LOD: encode+submit+readback",
            false,
            0,
        ),
        ("Preview orbit frame after 25 ms idle", true, 25),
    ] {
        renderer.set_toolpath_lod(lod);
        report.time(name, 2, 30, || {
            std::thread::sleep(Duration::from_millis(idle));
            camera.orbit(4., 1.);
            timed(|| {
                assert!(renderer.submit_frame(w, h, &camera).unwrap());
                wait(&mut renderer).unwrap();
            })
        });
    }
    renderer.set_toolpath_lod(true);
    // Крупний план згори: ближні верхні шари мають малюватись першими, інакше рання
    // перевірка глибини нічого не відкидає (у порядку друку кадр був у 10+ разів дорожчий).
    let size = hi - lo;
    camera.target = lo + size * glam::Vec3::new(0.5, 0.5, 1.0);
    camera.pitch = 75f32.to_radians();
    camera.distance = size.length() * 0.25;
    report.time("Preview close-up frame from above", 2, 20, || {
        camera.orbit(2., 0.);
        timed(|| {
            assert!(renderer.submit_frame(w, h, &camera).unwrap());
            wait(&mut renderer).unwrap();
        })
    });
    Ok(())
}

/// Виділено GPU-пам’яті (gpu-allocator wgpu), МБ; `None` — бекенд звіту не дає.
fn allocated_mb(renderer: &Renderer) -> Option<f64> {
    renderer
        .device()
        .generate_allocator_report()
        .map(|r| r.total_allocated_bytes as f64 / 1048576.0)
}

/// Режим Preview «Фото» (docs/PHOTO-MODE.md): вхід із запіканням IBL, буфер валиків, кадри
/// орбіти, кадр зі зміною шарів (перемальовуються тіньові карти) і пам’ять.
fn photo_gpu(report: &mut Report) -> Result<()> {
    if !report.wants("Photo") {
        return Ok(());
    }
    let Some(mut renderer) = viewer::test_renderer("perf Photo budgets") else {
        report.skipped.push("Photo GPU: немає wgpu-адаптера".into());
        return Ok(());
    };
    use viewer::ToolpathGpuExt as _;
    let (toolpath, source) = preview_toolpath()?;
    let (w, h) = FRAME;
    let (lo, hi) = toolpath.bounds().context("траєкторія без екструзії")?;
    let mut camera = Camera::default();
    camera.frame_scene(lo, hi, w as f32 / h as f32);

    // Звичайний Preview як точка відліку пам’яті.
    let built = toolpath.build(renderer.device(), &gcode::ViewOptions::default());
    renderer.set_toolpath(&built.gpu);
    renderer.set_all_layers_visible();
    renderer.submit_frame(w, h, &camera)?;
    wait(&mut renderer)?;
    let preview_mb = allocated_mb(&renderer);

    report.time("Photo toolpath build (CPU+upload)", 0, 3, || {
        timed(|| drop(std::hint::black_box(toolpath.build_photo(renderer.device()))))
    });
    let photo = toolpath.build_photo(renderer.device());
    let table = viewer::MaterialTable::builtin();
    let slots = gcode::filament_slots(&toolpath);
    let material = table.resolve(
        slots.first().map_or(table.default, |s| table.select(&s.kind, &s.name)),
        slots.first().and_then(|s| s.colour).unwrap_or([0.85, 0.85, 0.85]),
    );
    let settings = viewer::PhotoSettings {
        materials: vec![material],
        ..Default::default()
    };
    renderer.set_toolpath(&photo.gpu);
    report.time("Photo enter: IBL bake + pipelines (GPU wait)", 1, 5, || {
        renderer.set_photo(None);
        timed(|| renderer.set_photo(Some(settings.clone())))
    });
    renderer.submit_frame(w, h, &camera)?;
    wait(&mut renderer)?;
    let photo_mb = allocated_mb(&renderer);
    eprintln!(
        "perf: Photo — {source}: {} інстансів; ресурси режиму ≈ {:.1} МБ; виділено GPU: Preview {} → Photo {}",
        photo.gpu.total_instances,
        renderer.photo_bytes().unwrap_or(0) as f64 / 1048576.0,
        preview_mb.map_or("—".into(), |v| format!("{v:.1} МБ")),
        photo_mb.map_or("—".into(), |v| format!("{v:.1} МБ (з буфером валиків «Фото»)")),
    );
    for (name, idle) in [
        ("Photo orbit frame: encode+submit+readback", 0),
        ("Photo orbit frame after 25 ms idle", 25),
    ] {
        report.time(name, 2, 30, || {
            std::thread::sleep(Duration::from_millis(idle));
            camera.orbit(4., 1.);
            timed(|| {
                assert!(renderer.submit_frame(w, h, &camera).unwrap());
                wait(&mut renderer).unwrap();
            })
        });
    }
    // Повзунок шарів: кожен кадр — нові межі видимих шарів, тож і тіньові карти.
    let layers = toolpath.layer_count();
    let mut top = layers;
    report.time("Photo layer-slider frame (shadow maps redrawn)", 2, 20, || {
        top = if top <= layers / 2 { layers } else { top - 7 };
        renderer.set_visible_layers(0..top);
        timed(|| {
            assert!(renderer.submit_frame(w, h, &camera).unwrap());
            wait(&mut renderer).unwrap();
        })
    });
    renderer.set_all_layers_visible();
    let size = hi - lo;
    camera.target = lo + size * Vec3::new(0.5, 0.5, 1.0);
    camera.pitch = 75f32.to_radians();
    camera.distance = size.length() * 0.25;
    report.time("Photo close-up frame from above", 2, 20, || {
        camera.orbit(2., 0.);
        timed(|| {
            assert!(renderer.submit_frame(w, h, &camera).unwrap());
            wait(&mut renderer).unwrap();
        })
    });
    report.time("Photo save image 3840x2160 (offscreen PNG)", 0, 3, || {
        timed(|| drop(std::hint::black_box(renderer.render_photo_png(3840, 2160, &camera).unwrap())))
    });

    // Вихід із режиму: ресурси «Фото» звільняються, Preview повертається до своїх чисел.
    renderer.set_photo(None);
    renderer.set_toolpath(&built.gpu);
    drop(photo);
    renderer.submit_frame(w, h, &camera)?;
    wait(&mut renderer)?;
    let after_mb = allocated_mb(&renderer);
    eprintln!(
        "perf: Photo — після виходу виділено GPU: {}",
        after_mb.map_or("—".into(), |v| format!("{v:.1} МБ"))
    );
    if let (Some(before), Some(after)) = (preview_mb, after_mb) {
        report.invariant(
            format!("вихід із «Фото» звільняє пам’ять: {after:.1} МБ ≤ Preview {before:.1} МБ + 2"),
            after <= before + 2.0,
        );
    }
    report.invariant(
        "«Фото» вимкнено: рендерер не тримає його ресурсів".into(),
        !renderer.photo_active() && renderer.photo_bytes().is_none(),
    );
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("perf: {error:#}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<bool> {
    let args = args()?;
    if cfg!(debug_assertions) {
        eprintln!("perf: debug-збірка — числа непорівнянні з baseline; запускай з --release");
    }
    let started = Instant::now();
    let (project, key) = sphere_project()?;
    eprintln!("perf: сфера 1 М △ за {:.0} мс", started.elapsed().as_secs_f64() * 1e3);
    let mut report = Report {
        only: args.only.clone(),
        ..Default::default()
    };
    // Синтетичний файл доступний і в CI; референс можна підставити без копіювання у git.
    let source = match std::env::var("GCODE_PERF_REFERENCE") {
        Ok(path) => std::fs::read_to_string(path)?,
        Err(_) => {
            let mut source = String::from("M83\n; CHANGE_LAYER\n; FEATURE: Outer wall\n");
            for i in 0..800_000 {
                use std::fmt::Write;
                writeln!(&mut source, "G1 X{} Y{} E0.02 F6000", i % 200, i / 200 % 200)?;
            }
            source
        }
    };
    let metric = if std::env::var_os("GCODE_PERF_REFERENCE").is_some() { "gcode::parse reference" } else { "gcode::parse 800k" };
    report.time(metric, 2, 9, || {
        let started = Instant::now();
        let toolpath = std::hint::black_box(gcode::parse(&source).unwrap());
        let elapsed = started.elapsed();
        drop(toolpath);
        elapsed
    });
    headless(&mut report, &project, key)?;
    if args.gpu {
        gpu(&mut report, &project, key)?;
        preview_gpu(&mut report)?;
        photo_gpu(&mut report)?;
    } else {
        report.skipped.push("GPU-сценарії: --no-gpu".into());
    }

    let text = std::fs::read_to_string(&args.baseline).unwrap_or_else(|_| "{}".into());
    let mut file: Map<String, Value> =
        serde_json::from_str(&text).with_context(|| format!("розбір {}", args.baseline.display()))?;
    let tolerance = file.get("tolerance").and_then(Value::as_f64).unwrap_or(2.);
    let floor = file.get("floor_ms").and_then(Value::as_f64).unwrap_or(0.05);
    let baseline = file
        .get("headless")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let mut ok = true;
    println!(
        "{:<56} {:>10} {:>10} {:>10} {:>10}  статус",
        "метрика", "p50 мс", "p95 мс", "база мс", "межа мс"
    );
    for m in &report.metrics {
        let base = baseline
            .get(m.name)
            .and_then(|b| b.get("median_ms"))
            .and_then(Value::as_f64);
        let (base_s, limit_s, status) = match base {
            Some(base) => {
                // `max_ms` замінює відносну межу (парсинг G-code), `budget_ms` її обмежує
                // зверху (кадр Preview ≤ 16.7 мс).
                let entry = baseline.get(m.name);
                let limit = entry
                    .and_then(|b| b.get("max_ms"))
                    .and_then(Value::as_f64)
                    .unwrap_or_else(|| (base * tolerance).max(base + floor));
                let limit = entry
                    .and_then(|b| b.get("budget_ms"))
                    .and_then(Value::as_f64)
                    .map_or(limit, |budget| limit.min(budget));
                let pass = m.median_ms <= limit;
                ok &= pass || args.update || args.invariants_only;
                let status = match (pass, args.invariants_only) {
                    (true, _) => "ok",
                    (false, false) => "РЕГРЕСІЯ",
                    (false, true) => "повільніше за baseline (не перевіряється)",
                };
                (format!("{base:.4}"), format!("{limit:.4}"), status)
            }
            None => ("—".into(), "—".into(), "нова (немає baseline)"),
        };
        println!(
            "{:<56} {:>10.4} {:>10.4} {:>10} {:>10}  {status}",
            m.name, m.median_ms, m.p95_ms, base_s, limit_s
        );
    }
    for (what, pass) in &report.invariants {
        println!("інваріант: {what} — {}", if *pass { "ok" } else { "ПОРУШЕНО" });
        ok &= *pass;
    }
    for what in &report.skipped {
        println!("пропущено: {what}");
    }

    if args.update {
        let mut headless = baseline;
        for m in &report.metrics {
            let mut entry = headless.get(m.name).cloned().unwrap_or_else(|| json!({}));
            entry["median_ms"] = json!(round(m.median_ms));
            entry["p95_ms"] = json!(round(m.p95_ms));
            entry["samples"] = json!(m.samples);
            headless.insert(m.name.into(), entry);
        }
        file.entry("tolerance").or_insert(json!(tolerance));
        file.entry("floor_ms").or_insert(json!(floor));
        file.insert("headless".into(), Value::Object(headless));
        std::fs::write(&args.baseline, serde_json::to_string_pretty(&file)? + "\n")?;
        println!("baseline оновлено: {}", args.baseline.display());
    }
    println!("{}", if ok { "perf: усі бюджети дотримано" } else { "perf: БЮДЖЕТ ПЕРЕВИЩЕНО" });
    Ok(ok)
}

fn round(ms: f64) -> f64 {
    (ms * 1e4).round() / 1e4
}
