use std::{
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::Result;
use glam::Vec3;

use crate::BED_SIZE;

/// Трикутна сітка без індексів: по три вершини на грань, нормалі пласкі.
#[derive(Clone, Debug, Default)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
}

impl Mesh {
    pub fn triangle_count(&self) -> usize {
        self.positions.len() / 3
    }

    pub(crate) fn push_triangle(&mut self, a: Vec3, b: Vec3, c: Vec3) {
        let n = (b - a).cross(c - a).normalize_or_zero();
        for v in [a, b, c] {
            self.positions.push(v.to_array());
            self.normals.push(n.to_array());
        }
    }

    /// Кладе модель на стіл: низ на Z=0, центр по XY — у центр столу.
    pub fn place_on_bed(&mut self) {
        let Some((min, max)) = self.bounds() else {
            return;
        };
        let offset = Vec3::new(
            BED_SIZE / 2.0 - (min.x + max.x) / 2.0,
            BED_SIZE / 2.0 - (min.y + max.y) / 2.0,
            -min.z,
        );
        for p in &mut self.positions {
            *p = (Vec3::from(*p) + offset).to_array();
        }
    }

    /// Для 3MF: розміщення зі слайсера зберігаємо, якщо модель уміщається на стіл;
    /// інакше (кілька пластин, чужий слайсер) — центруємо. Низ завжди на Z=0.
    pub fn settle_on_bed(&mut self) {
        let Some((min, max)) = self.bounds() else {
            return;
        };
        let fits =
            min.x >= -1.0 && min.y >= -1.0 && max.x <= BED_SIZE + 1.0 && max.y <= BED_SIZE + 1.0;
        if !fits {
            return self.place_on_bed();
        }
        for p in &mut self.positions {
            p[2] -= min.z;
        }
    }

    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        let mut it = self.positions.iter().map(|p| Vec3::from(*p));
        let first = it.next()?;
        Some(it.fold((first, first), |(lo, hi), p| (lo.min(p), hi.max(p))))
    }

    /// Калібрувальний кубик 20 мм — показуємо, поки не відкрито файл.
    pub fn calibration_cube() -> Self {
        let s = 20.0;
        let v = |x: f32, y: f32, z: f32| Vec3::new(x * s, y * s, z * s);
        let quads = [
            [v(0., 0., 0.), v(0., 1., 0.), v(1., 1., 0.), v(1., 0., 0.)], // низ
            [v(0., 0., 1.), v(1., 0., 1.), v(1., 1., 1.), v(0., 1., 1.)], // верх
            [v(0., 0., 0.), v(1., 0., 0.), v(1., 0., 1.), v(0., 0., 1.)],
            [v(1., 0., 0.), v(1., 1., 0.), v(1., 1., 1.), v(1., 0., 1.)],
            [v(1., 1., 0.), v(0., 1., 0.), v(0., 1., 1.), v(1., 1., 1.)],
            [v(0., 1., 0.), v(0., 0., 0.), v(0., 0., 1.), v(0., 1., 1.)],
        ];
        let mut mesh = Self::default();
        for [a, b, c, d] in quads {
            mesh.push_triangle(a, b, c);
            mesh.push_triangle(a, c, d);
        }
        mesh.place_on_bed();
        mesh
    }
}

/// Читає бінарний або ASCII STL. Нормалі з файлу ігноруються — їх часто пишуть нулями.
pub fn load_stl(path: &Path) -> Result<Mesh> {
    let stl = project::read_stl(path)?;
    let vertex = |i: u32| Vec3::from_array(stl.positions[i as usize].map(|v| v as f32));

    let mut mesh = Mesh::default();
    for &[a, b, c] in &stl.triangles {
        mesh.push_triangle(vertex(a), vertex(b), vertex(c));
    }
    mesh.place_on_bed();
    Ok(mesh)
}

/// STL у власних координатах файлу (модель столу з профілю принтера).
pub fn load_stl_raw(path: &Path) -> Result<Mesh> {
    let stl = project::read_stl(path)?;
    let vertex = |i: u32| Vec3::from_array(stl.positions[i as usize].map(|v| v as f32));
    let mut mesh = Mesh::default();
    for &[a, b, c] in &stl.triangles {
        mesh.push_triangle(vertex(a), vertex(b), vertex(c));
    }
    Ok(mesh)
}

/// Одна пластина проєкту. STL — завжди одна пластина.
#[derive(Clone, Debug)]
pub struct Plate {
    /// Номер як у слайсері, з 1; 0 — обʼєкти поза пластинами.
    pub number: u32,
    pub name: Option<String>,
    pub mesh: Mesh,
}

impl Plate {
    pub fn title(&self) -> String {
        if self.number == 0 {
            return "Поза пластинами".into();
        }
        match &self.name {
            Some(name) => format!("{} · {name}", self.number),
            None => format!("Пластина {}", self.number),
        }
    }
}

/// Формати, які вміє [`load_model`].
pub const SUPPORTED_EXTENSIONS: &[&str] = &["stl", "3mf"];

fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

pub fn load_model(path: &Path) -> Result<Vec<Plate>> {
    match extension(path).as_str() {
        "stl" => Ok(vec![Plate {
            number: 1,
            name: None,
            mesh: load_stl(path)?,
        }]),
        "3mf" => crate::threemf::load_3mf(path),
        other => anyhow::bail!("формат .{other} поки не підтримується"),
    }
}

/// PNG-превʼю моделі: вбудоване в 3MF або сусідній `<імʼя>.png` біля STL.
/// Шлях до теки кешу прев'ю: `~/.cache/slicenv/thumbnails`.
pub fn thumbnail_cache_dir() -> PathBuf {
    paths::cache_dir().join("thumbnails")
}

/// Шлях до файлу кешу для моделі (хеш від шляху, розміру та часу модифікації).
pub fn thumbnail_cache_path(path: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let size = meta.len();
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    canonical.hash(&mut hasher);
    size.hash(&mut hasher);
    modified.hash(&mut hasher);
    let hash = hasher.finish();

    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let clean_stem: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .take(24)
        .collect();

    Some(thumbnail_cache_dir().join(format!("{clean_stem}_{hash:016x}.png")))
}

/// Швидке читання прев'ю без 3D-рендеру: сусідній `<імʼя>.png`, вбудоване в 3MF або збережений диск-кеш.
pub fn thumbnail_cached(path: &Path) -> Option<Vec<u8>> {
    let neighbor = path.with_extension("png");
    if neighbor.is_file()
        && let Ok(bytes) = std::fs::read(&neighbor)
    {
        return Some(bytes);
    }
    if extension(path).as_str() == "3mf"
        && let Some(bytes) = crate::threemf::thumbnail_3mf(path)
    {
        return Some(bytes);
    }
    if let Some(cache_path) = thumbnail_cache_path(path)
        && cache_path.is_file()
        && let Ok(bytes) = std::fs::read(&cache_path)
    {
        return Some(bytes);
    }
    None
}

static THUMBNAIL_RENDERER: Mutex<Option<crate::Renderer>> = Mutex::new(None);

/// Повне отримання або генерація превʼю: якщо немає кешованого, завантажує модель,
/// рендерить мініатюру у wgpu, зберігає в кеш і повертає PNG.
pub fn thumbnail(path: &Path) -> Option<Vec<u8>> {
    if let Some(bytes) = thumbnail_cached(path) {
        return Some(bytes);
    }

    let plates = load_model(path).ok()?;
    let plate = plates.first()?;

    let mut lock = THUMBNAIL_RENDERER.lock().ok()?;
    let renderer = match lock.as_mut() {
        Some(r) => r,
        None => {
            let r = crate::Renderer::new().ok()?;
            *lock = Some(r);
            lock.as_mut().unwrap()
        }
    };

    let png = renderer.render_mesh_thumbnail(&plate.mesh, 256)?;

    if let Some(cache_path) = thumbnail_cache_path(path) {
        if let Some(parent) = cache_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&cache_path, &png);
    }

    Some(png)
}

/// Мініатюри пластини для `.gcode.3mf`, який Slicenv зібрав сам (рушій їх не дав).
pub struct PlateImages {
    /// Ізометрія 512×512 (`plate_N.png`, `plate_no_light_N.png`).
    pub plate: Vec<u8>,
    /// 128×128 (`plate_N_small.png`).
    pub small: Vec<u8>,
    /// Згори 512×512 (`top_N.png`, `pick_N.png`).
    pub top: Vec<u8>,
    /// Рамка моделі на столі, мм.
    pub bounds: (glam::Vec3, glam::Vec3),
}

/// Рендерить мініатюри пластини `plate` з 3MF/STL `path` тим самим рендерером, що й превʼю.
pub fn plate_images(path: &Path, plate: u32, color: Option<[f32; 3]>) -> Option<PlateImages> {
    let plates = load_model(path).ok()?;
    let mesh = &plates
        .iter()
        .find(|p| p.number == plate)
        .or_else(|| plates.first())?
        .mesh;
    let bounds = mesh.bounds()?;
    let mut lock = THUMBNAIL_RENDERER.lock().ok()?;
    if lock.is_none() {
        *lock = Some(crate::Renderer::new().ok()?);
    }
    let renderer = lock.as_mut()?;
    // Колір філаменту з проєкту; рендерер спільний з мініатюрами «Головної», тож повертаємо.
    renderer.set_model_color(color.unwrap_or([0.0, 0.682, 0.259]));
    let images = Some(PlateImages {
        plate: renderer.render_mesh_view(mesh, 512, None)?,
        small: renderer.render_mesh_view(mesh, 128, None)?,
        top: renderer.render_mesh_view(mesh, 512, Some(89f32.to_radians()))?,
        bounds,
    });
    renderer.set_model_color([0.0, 0.682, 0.259]);
    images
}

/// PNG-мініатюра сітки `size`×`size` на прозорому тлі (колонка пластин «Перегляду»).
/// Той самий фоновий рендерер, що й для мініатюр «Головної» та пакетів нарізки.
pub fn mesh_image(mesh: &Mesh, size: u32, color: Option<[f32; 3]>) -> Option<Vec<u8>> {
    mesh.bounds()?;
    let mut lock = THUMBNAIL_RENDERER.lock().ok()?;
    if lock.is_none() {
        *lock = Some(crate::Renderer::new().ok()?);
    }
    let renderer = lock.as_mut()?;
    renderer.set_model_color(color.unwrap_or([0.0, 0.682, 0.259]));
    let png = renderer.render_mesh_view(mesh, size, None);
    renderer.set_model_color([0.0, 0.682, 0.259]);
    png
}

/// Проєкт → пластини для показу: по одній на пластину, плюс «Поза пластинами», якщо є такі
/// об’єкти. Геометрію тут не будуємо: сцена малює кожну частину окремою GPU-сіткою
/// (`app::Workspace::sync_scene_items`), тож пластина — лише номер і назва.
pub fn plates_of(project: &project::Project) -> anyhow::Result<Vec<Plate>> {
    let mut plates: Vec<Plate> = project
        .plates
        .iter()
        .map(|plate| Plate {
            number: plate.index,
            name: (!plate.name.is_empty()).then(|| plate.name.clone()),
            mesh: Mesh::default(),
        })
        .collect();
    let loose = project.objects.iter().any(|o| {
        o.volumes.iter().any(|v| v.kind == project::VolumeKind::Part)
            && o.instances.iter().any(|i| i.plate.is_none())
    });
    if loose {
        plates.push(Plate {
            number: 0,
            name: None,
            mesh: Mesh::default(),
        });
    }
    Ok(plates)
}

/// Сітка частини для GPU відносно центру її рамки (f64), щоб великі локальні координати
/// не злипались при переході в f32; центр компенсується в матриці елемента сцени.
/// По три вершини на грань, пласкі нормалі; трикутники з індексами поза сіткою пропускаються.
pub fn local_mesh(mesh: &project::IndexedMesh) -> (Mesh, glam::DVec3) {
    let origin = {
        let mut points = mesh.positions.iter().map(|p| glam::DVec3::from(*p));
        points.next().map_or(glam::DVec3::ZERO, |first| {
            let (lo, hi) = points.fold((first, first), |(lo, hi), p| (lo.min(p), hi.max(p)));
            (lo + hi) * 0.5
        })
    };
    let local = |i: u32| {
        mesh.positions
            .get(i as usize)
            .map(|v| (glam::DVec3::from(*v) - origin).as_vec3())
    };
    let mut out = Mesh::default();
    out.positions.reserve(mesh.triangles.len() * 3);
    out.normals.reserve(mesh.triangles.len() * 3);
    for &[a, b, c] in &mesh.triangles {
        let (Some(a), Some(b), Some(c)) = (local(a), local(b), local(c)) else {
            continue;
        };
        let n = (b - a).cross(c - a).normalize_or_zero().to_array();
        out.positions.extend([a.to_array(), b.to_array(), c.to_array()]);
        out.normals.extend([n, n, n]);
    }
    (out, origin)
}

#[cfg(test)]
mod scene_tests {
    use super::*;
    use project::Project;

    #[test]
    fn plates_carry_no_geometry_and_loose_plate_only_when_needed() {
        let mut project = Project::default();
        project.add_plate("A").unwrap();
        project.add_plate("").unwrap();
        let key = project
            .add_mesh_object(
                "t",
                project::IndexedMesh {
                    positions: vec![[0., 0., 0.], [10., 0., 0.], [0., 10., 0.]],
                    triangles: vec![[0, 1, 2], [0, 1, 9]],
                    ..Default::default()
                },
                1,
                [128., 128.],
                [0., 0.],
                [256., 256.],
            )
            .unwrap();
        let plates = plates_of(&project).unwrap();
        assert_eq!(plates.iter().map(|p| p.number).collect::<Vec<_>>(), [1, 2]);
        assert!(plates.iter().all(|p| p.mesh.positions.is_empty()));
        assert_eq!(plates[0].name.as_deref(), Some("A"));
        project.instance_mut(key).unwrap().plate = None;
        assert_eq!(plates_of(&project).unwrap().last().unwrap().number, 0);
        // Трикутник з індексом поза сіткою пропускається, а не панікує.
        let (mesh, origin) = local_mesh(&project.objects[0].volumes[0].mesh);
        let source = &project.objects[0].volumes[0].mesh.positions;
        let (lo, hi) = source.iter().map(|p| glam::DVec3::from(*p)).fold(
            (glam::DVec3::MAX, glam::DVec3::MIN),
            |(lo, hi), p| (lo.min(p), hi.max(p)),
        );
        assert_eq!(origin, (lo + hi) * 0.5);
        assert_eq!(mesh.positions.len(), 3);
        assert_eq!(mesh.normals[0], [0., 0., 1.]);
    }

    #[test]
    fn far_local_coordinates_survive_f32_after_rebasing() {
        // Вершини 1e8 і 1e8+1 у f32 злипаються; відносно центру рамки — ні.
        let mesh = project::IndexedMesh {
            positions: vec![[1e8, 0., 0.], [1e8 + 1., 0., 0.], [1e8, 1., 0.]],
            triangles: vec![[0, 1, 2]],
            ..Default::default()
        };
        let (gpu, origin) = local_mesh(&mesh);
        let xs: Vec<f32> = gpu.positions.iter().map(|p| p[0]).collect();
        assert!((xs[1] - xs[0] - 1.).abs() < 1e-6, "{xs:?}");
        assert!((origin.x - (1e8 + 0.5)).abs() < 1e-9);
        assert_ne!(gpu.normals[0], [0., 0., 0.]);
    }
}
