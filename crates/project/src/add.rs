//! Додавання моделей у відкритий проєкт (файли, примітиви), заміна сіток, перезавантаження
//! з диска, дзеркало й експорт у STL. Читання файлів відокремлене від зміни проєкту, щоб
//! його можна було виконувати у фоні.

use crate::{
    DAffine3, DMat3, DVec3, IndexedMesh, Instance, InstanceRef, Object, Project, Volume, VolumeKind,
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

/// Ключ метаданих частини, який Bambu Studio використовує для шляху до вихідного файлу.
pub const SOURCE_FILE: &str = "source_file";

/// Розширення, які вміє читати `read_model`.
pub const IMPORT_EXTENSIONS: &[&str] = &["stl", "3mf", "obj"];
/// Розширення з однією сіткою — для «Перезавантажити» й «Замінити».
pub const MESH_EXTENSIONS: &[&str] = &["stl", "obj"];

impl Volume {
    /// Шлях до вихідного файлу сітки, якщо відомий.
    pub fn source_file(&self) -> Option<PathBuf> {
        self.config
            .get(SOURCE_FILE)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    }

    pub fn set_source_file(&mut self, path: &Path) {
        self.config
            .insert(SOURCE_FILE.into(), path.display().to_string());
    }
}

/// Прочитана з диска модель: об’єкти з тимчасовими ID, ще не прив’язані до пластини.
#[derive(Clone, Debug)]
pub struct ImportedModel {
    pub name: String,
    pub objects: Vec<Object>,
    pub layer_ranges: Vec<crate::LayerRange>,
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

pub fn read_stl(path: &Path) -> Result<IndexedMesh> {
    let file = std::fs::File::open(path).with_context(|| format!("{}", path.display()))?;
    let stl = stl_io::read_stl(&mut std::io::BufReader::new(file))
        .with_context(|| format!("{}", path.display()))?;
    Ok(IndexedMesh {
        positions: stl
            .vertices
            .iter()
            .map(|v| <[f32; 3]>::from(*v).map(f64::from))
            .collect(),
        triangles: stl
            .faces
            .iter()
            .map(|f| f.vertices.map(|i| i as u32))
            .collect(),
        ..IndexedMesh::default()
    })
}

/// Мінімальний Wavefront OBJ: `v` і `f` (багатокутники розбиваються віялом).
pub fn parse_obj(text: &str) -> Result<IndexedMesh> {
    let mut mesh = IndexedMesh::default();
    for (number, line) in text.lines().enumerate() {
        let mut words = line.split_whitespace();
        match words.next() {
            Some("v") => {
                let coords = words
                    .take(3)
                    .map(|w| w.parse::<f64>())
                    .collect::<Result<Vec<_>, _>>()
                    .with_context(|| format!("рядок {}: некоректна вершина", number + 1))?;
                ensure!(
                    coords.len() == 3,
                    "рядок {}: вершина без 3 координат",
                    number + 1
                );
                ensure!(coords.iter().all(|c| c.is_finite()), "нескінченна вершина");
                mesh.positions.push([coords[0], coords[1], coords[2]]);
            }
            Some("f") => {
                let count = mesh.positions.len() as i64;
                let face = words
                    .map(|w| {
                        let raw: i64 = w.split('/').next().unwrap_or("").parse()?;
                        let index = if raw < 0 { count + raw } else { raw - 1 };
                        ensure!((0..count).contains(&index), "індекс вершини поза межами");
                        Ok(index as u32)
                    })
                    .collect::<Result<Vec<_>>>()
                    .with_context(|| format!("рядок {}: некоректна грань", number + 1))?;
                ensure!(
                    face.len() >= 3,
                    "рядок {}: грань має < 3 вершин",
                    number + 1
                );
                for i in 1..face.len() - 1 {
                    mesh.triangles.push([face[0], face[i], face[i + 1]]);
                }
            }
            _ => {}
        }
    }
    ensure!(!mesh.triangles.is_empty(), "OBJ без граней");
    Ok(mesh)
}

/// Сітка з STL або OBJ як є (без нормалізації положення).
pub fn read_mesh_file(path: &Path) -> Result<IndexedMesh> {
    match extension(path).as_str() {
        "stl" => read_stl(path),
        "obj" => parse_obj(
            &std::fs::read_to_string(path).with_context(|| format!("{}", path.display()))?,
        ),
        _ => bail!("потрібен STL або OBJ"),
    }
}

impl ImportedModel {
    /// Сітка з файлу: центр основи в (0, 0), низ на z = 0, шлях запам’ятовується в частині.
    pub fn from_mesh(name: String, mut mesh: IndexedMesh, source: Option<&Path>) -> Self {
        if let Some(b) = mesh.bounds() {
            let shift = DVec3::new(-b.center().x, -b.center().y, -b.min.z);
            for p in &mut mesh.positions {
                *p = (DVec3::from(*p) + shift).to_array();
            }
        }
        let mut volume = Volume {
            name: name.clone(),
            mesh: mesh.into(),
            ..Volume::default()
        };
        if let Some(path) = source {
            volume.set_source_file(path);
        }
        Self {
            name: name.clone(),
            layer_ranges: Vec::new(),
            objects: vec![Object {
                name,
                volumes: vec![volume],
                instances: vec![Instance::default()],
                ..Object::default()
            }],
        }
    }
}

/// STL / OBJ / 3MF з диска. 3MF додає всі свої об’єкти зі збереженим взаємним розташуванням.
pub fn read_model(path: &Path) -> Result<ImportedModel> {
    match extension(path).as_str() {
        "stl" | "obj" => Ok(ImportedModel::from_mesh(
            stem(path),
            read_mesh_file(path)?,
            std::fs::canonicalize(path).ok().as_deref().or(Some(path)),
        )),
        "3mf" => {
            let source = Project::load_3mf(path)?;
            let offsets = source
                .plates
                .iter()
                .map(|p| Ok((p.index, source.plate_offset(p.index)?)))
                .collect::<Result<std::collections::BTreeMap<_, _>>>()?;
            let mut objects = source.objects;
            for object in &mut objects {
                for instance in &mut object.instances {
                    if let Some(offset) = instance.plate.and_then(|p| offsets.get(&p)) {
                        instance.transform =
                            DAffine3::from_translation(*offset) * instance.transform;
                    }
                }
                object.model = None;
                object.metadata = None;
                for volume in &mut object.volumes {
                    volume.source = None;
                }
                object.instances.iter_mut().for_each(|i| i.metadata = None);
            }
            objects.retain(|o| !o.instances.is_empty());
            ensure!(!objects.is_empty(), "у файлі немає розміщених об’єктів");
            Ok(ImportedModel {
                name: stem(path),
                objects,
                layer_ranges: source.layer_ranges,
            })
        }
        other => bail!("формат .{other} не підтримується (потрібен STL, 3MF або OBJ)"),
    }
}

/// Результат масового перезавантаження чи заміни.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReplaceReport {
    pub replaced: usize,
    pub skipped: usize,
}

impl Project {
    fn next_object_id(&self) -> Result<u32> {
        self.objects
            .iter()
            .map(|o| o.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("вичерпано ID"))
    }

    /// Додає модель на пластину: група центрується в `center` (у межах столу, якщо вміщується)
    /// й опускається на стіл. Повертає нові інстанси.
    pub fn add_model(
        &mut self,
        model: ImportedModel,
        plate: u32,
        center: [f64; 2],
        lo: [f64; 2],
        hi: [f64; 2],
    ) -> Result<Vec<InstanceRef>> {
        let mut staged = self.clone();
        let keys = staged.add_model_inner(model, plate, center, lo, hi)?;
        *self = staged;
        Ok(keys)
    }

    fn add_model_inner(
        &mut self,
        model: ImportedModel,
        plate: u32,
        center: [f64; 2],
        lo: [f64; 2],
        hi: [f64; 2],
    ) -> Result<Vec<InstanceRef>> {
        ensure!(
            self.plates.iter().any(|p| p.index == plate),
            "немає пластини"
        );
        ensure!(!self.plate_locked(plate), "пластина заблокована");
        ensure!(!model.objects.is_empty(), "порожня модель");
        if !model.layer_ranges.is_empty() {
            self.archive
                .entry("Metadata/layer_config_ranges.xml".into())
                .or_insert_with(|| std::sync::Arc::from(b"<objects/>".as_slice()));
        }
        let mut keys = Vec::new();
        for mut object in model.objects {
            let old_id = object.id;
            object.id = self.next_object_id()?;
            self.layer_ranges.extend(
                model
                    .layer_ranges
                    .iter()
                    .filter(|r| r.source_object_id == old_id)
                    .cloned()
                    .map(|mut r| {
                        r.source_object_id = object.id;
                        r
                    }),
            );
            ensure!(
                object
                    .volumes
                    .iter()
                    .any(|v| v.kind == VolumeKind::Part && !v.mesh.triangles.is_empty()),
                "модель без геометрії"
            );
            for (index, instance) in object.instances.iter_mut().enumerate() {
                instance.plate = Some(plate);
                keys.push(InstanceRef {
                    object: object.id,
                    instance: index,
                });
            }
            self.objects.push(object);
        }
        let (mut lo_b, mut hi_b) = (DVec3::MAX, DVec3::MIN);
        for key in &keys {
            if let Some(b) = self.instance_bounds(*key)? {
                lo_b = lo_b.min(b.min);
                hi_b = hi_b.max(b.max);
            }
        }
        ensure!(lo_b.is_finite() && hi_b.is_finite(), "порожня сітка");
        let size = hi_b - lo_b;
        let fit = |c: f64, half: f64, lo: f64, hi: f64| {
            if half * 2.0 <= hi - lo {
                c.clamp(lo + half, hi - half)
            } else {
                (lo + hi) / 2.0
            }
        };
        let target = DVec3::new(
            fit(center[0], size.x / 2.0, lo[0], hi[0]),
            fit(center[1], size.y / 2.0, lo[1], hi[1]),
            0.0,
        );
        let delta = DVec3::new(
            target.x - (lo_b.x + hi_b.x) / 2.0,
            target.y - (lo_b.y + hi_b.y) / 2.0,
            -lo_b.z,
        );
        for key in &keys {
            self.translate(*key, delta)?;
        }
        self.sync_plates();
        Ok(keys)
    }

    /// Додає одну сітку (примітив, «зручна» модель) як новий об’єкт.
    pub fn add_mesh_object(
        &mut self,
        name: &str,
        mesh: IndexedMesh,
        plate: u32,
        center: [f64; 2],
        lo: [f64; 2],
        hi: [f64; 2],
    ) -> Result<InstanceRef> {
        let model = ImportedModel::from_mesh(name.to_owned(), mesh, None);
        Ok(self.add_model(model, plate, center, lo, hi)?[0])
    }

    /// Видаляє кілька інстансів; індекси в межах об’єкта не зсуваються між видаленнями.
    pub fn delete_instances(&mut self, keys: &[InstanceRef]) -> Result<()> {
        let mut keys = keys.to_vec();
        keys.sort();
        keys.dedup();
        for key in &keys {
            self.instance(*key)?;
        }
        for key in keys.into_iter().rev() {
            self.delete_instance(key)?;
        }
        Ok(())
    }

    /// Прибирає всі об’єкти (пластини лишаються).
    pub fn delete_all_objects(&mut self) {
        self.objects.clear();
        self.layer_ranges.clear();
        self.sync_plates();
    }

    /// Нова сітка частини: зберігає трансформації інстансів і частини; центр у XY і низ
    /// сітки лишаються на місці старої, тож перезавантаження незміненого файлу нічого не зсуває.
    pub fn replace_volume_mesh(
        &mut self,
        object: u32,
        volume: usize,
        mut mesh: IndexedMesh,
        source: Option<&Path>,
    ) -> Result<()> {
        ensure!(!mesh.triangles.is_empty(), "порожня сітка");
        let target = self.object_mut(object)?;
        let part = target
            .volumes
            .get_mut(volume)
            .ok_or_else(|| anyhow::anyhow!("немає частини {volume}"))?;
        if let (Some(old), Some(new)) = (part.mesh.bounds(), mesh.bounds()) {
            let shift = DVec3::new(
                old.center().x - new.center().x,
                old.center().y - new.center().y,
                old.min.z - new.min.z,
            );
            for p in &mut mesh.positions {
                *p = (DVec3::from(*p) + shift).to_array();
            }
        }
        part.mesh = mesh.into();
        part.source = None;
        if let Some(path) = source {
            part.set_source_file(path);
        }
        Ok(())
    }

    pub(crate) fn object_mut(&mut self, id: u32) -> Result<&mut Object> {
        self.objects
            .iter_mut()
            .find(|o| o.id == id)
            .ok_or_else(|| anyhow::anyhow!("немає обʼєкта {id}"))
    }

    /// Частини об’єкта з відомим існуючим файлом STL/OBJ: (індекс частини, шлях).
    pub fn reloadable_sources(&self, object: u32) -> Vec<(usize, PathBuf)> {
        let Ok(object) = self.object(object) else {
            return Vec::new();
        };
        object
            .volumes
            .iter()
            .enumerate()
            .filter_map(|(i, v)| {
                let path = v.source_file()?;
                (MESH_EXTENSIONS.contains(&extension(&path).as_str()) && path.is_file())
                    .then_some((i, path))
            })
            .collect()
    }

    /// Читає ті самі шляхи з диска без зміни проєкту (для фонового потоку).
    pub fn read_reload_sources(
        &self,
        objects: &[u32],
    ) -> Vec<(u32, usize, Result<IndexedMesh>, PathBuf)> {
        objects
            .iter()
            .flat_map(|&id| {
                self.reloadable_sources(id)
                    .into_iter()
                    .map(move |(volume, path)| (id, volume, path))
            })
            .map(|(id, volume, path)| (id, volume, read_mesh_file(&path), path))
            .collect()
    }

    /// Застосовує прочитані сітки; помилки читання рахуються як пропущені.
    pub fn apply_reloaded(
        &mut self,
        loaded: Vec<(u32, usize, Result<IndexedMesh>, PathBuf)>,
    ) -> Result<ReplaceReport> {
        let mut staged = self.clone();
        let mut report = ReplaceReport::default();
        for (object, volume, mesh, path) in loaded {
            match mesh {
                Ok(mesh) => {
                    staged.replace_volume_mesh(object, volume, mesh, Some(&path))?;
                    report.replaced += 1;
                }
                Err(_) => report.skipped += 1,
            }
        }
        *self = staged;
        Ok(report)
    }

    /// Файли STL/OBJ теки (без підтек), що збігаються з іменами об’єктів, у нижньому регістрі.
    pub fn scan_replacement_folder(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(dir).with_context(|| format!("{}", dir.display()))? {
            let path = entry?.path();
            if path.is_file() && MESH_EXTENSIONS.contains(&extension(&path).as_str()) {
                found.push((stem(&path).to_lowercase(), path));
            }
        }
        found.sort();
        Ok(found)
    }

    /// Об’єкти з одною деталлю, назва яких збігається з файлом теки: (об’єкт, деталь, файл).
    pub fn replacement_matches(
        &self,
        files: &[(String, PathBuf)],
    ) -> (Vec<(u32, usize, PathBuf)>, usize) {
        let mut matches = Vec::new();
        let mut skipped = 0;
        for object in &self.objects {
            let parts: Vec<usize> = object
                .volumes
                .iter()
                .enumerate()
                .filter(|(_, v)| v.kind == VolumeKind::Part)
                .map(|(i, _)| i)
                .collect();
            let name = object.name.to_lowercase();
            let name = name
                .rsplit_once('.')
                .filter(|(_, ext)| MESH_EXTENSIONS.contains(ext))
                .map_or(name.as_str(), |(base, _)| base);
            let file = files.iter().find(|(stem, _)| stem == name);
            match (parts.as_slice(), file) {
                ([part], Some((_, path))) => matches.push((object.id, *part, path.clone())),
                _ => skipped += 1,
            }
        }
        (matches, skipped)
    }

    /// Дзеркало інстансу відносно площини через центр його рамки, перпендикулярної осі
    /// (0 = X, 1 = Y, 2 = Z) у світових координатах. Відбиття вписується в сітки (з
    /// перевертанням обходу граней), тож нормалі лишаються зовнішніми. Якщо об’єкт має кілька
    /// інстансів, дзеркальний стає окремим об’єктом. Повертає новий ключ.
    pub fn mirror_instance(&mut self, key: InstanceRef, axis: usize) -> Result<InstanceRef> {
        ensure!(axis < 3, "вісь має бути 0, 1 чи 2");
        let mut staged = self.clone();
        let mirrored = staged.mirror_instance_inner(key, axis)?;
        *self = staged;
        Ok(mirrored)
    }

    fn mirror_instance_inner(&mut self, key: InstanceRef, axis: usize) -> Result<InstanceRef> {
        let center = self
            .instance_bounds(key)?
            .context("порожня сітка")?
            .center();
        let mut key = key;
        if self.object(key.object)?.instances.len() > 1 {
            let copy = self.duplicate_object(key.object)?;
            let instance = self.instance(key)?.clone();
            self.object_mut(copy)?.instances = vec![instance];
            self.object_mut(key.object)?.instances.remove(key.instance);
            key = InstanceRef {
                object: copy,
                instance: 0,
            };
        }
        let transform = self.instance(key)?.transform;
        let mut normal = DVec3::ZERO;
        normal[axis] = 1.0;
        let reflection = DAffine3::from_translation(center)
            * DAffine3::from_mat3(DMat3::IDENTITY - 2.0 * outer(normal, normal))
            * DAffine3::from_translation(-center);
        // Те саме відбиття в системі координат об’єкта: T⁻¹ · M · T.
        let local = transform.inverse() * reflection * transform;
        for volume in &mut self.object_mut(key.object)?.volumes {
            let full = local * volume.transform;
            let mut mesh = (*volume.mesh).clone();
            for p in &mut mesh.positions {
                *p = full.transform_point3(DVec3::from(*p)).to_array();
            }
            mesh.flip_winding();
            volume.mesh = mesh.into();
            volume.transform = DAffine3::IDENTITY;
            volume.source = None;
        }
        self.sync_plates();
        Ok(key)
    }

    /// Бінарний STL із деталей інстансів у координатах пластини.
    pub fn export_stl(&self, keys: &[InstanceRef], path: &Path) -> Result<usize> {
        let mut triangles = Vec::new();
        for key in keys {
            for (_, _, [a, b, c]) in self.world_triangles(*key)? {
                let normal = (b - a).cross(c - a).normalize_or_zero();
                let vertex = |v: DVec3| stl_io::Vertex::new([v.x as f32, v.y as f32, v.z as f32]);
                triangles.push(stl_io::Triangle {
                    normal: stl_io::Normal::new([
                        normal.x as f32,
                        normal.y as f32,
                        normal.z as f32,
                    ]),
                    vertices: [vertex(a), vertex(b), vertex(c)],
                });
            }
        }
        ensure!(!triangles.is_empty(), "немає геометрії для експорту");
        let mut file = std::io::BufWriter::new(
            std::fs::File::create(path).with_context(|| format!("{}", path.display()))?,
        );
        stl_io::write_stl(&mut file, triangles.iter())?;
        Ok(triangles.len())
    }

    /// Розкладає лише вибрані інстанси: решта пластини лишається на місці й обходиться.
    pub fn arrange_instances_on_bed(
        &mut self,
        plate: u32,
        keys: &[InstanceRef],
        spacing: f64,
        lo: [f64; 2],
        hi: [f64; 2],
    ) -> Result<()> {
        ensure!(
            lo.iter().chain(&hi).all(|v| v.is_finite()) && hi[0] > lo[0] && hi[1] > lo[1],
            "некоректний стіл"
        );
        ensure!(
            spacing.is_finite() && spacing >= 0.0,
            "некоректний проміжок"
        );
        ensure!(
            self.plates.iter().any(|p| p.index == plate),
            "немає пластини"
        );
        ensure!(!self.plate_locked(plate), "пластина заблокована");
        let chosen: BTreeSet<_> = keys.iter().copied().collect();
        let (mut items, obstacles): (Vec<_>, Vec<_>) = self
            .plate_bounds(Some(plate))?
            .into_iter()
            .partition(|(key, _)| chosen.contains(key));
        ensure!(items.len() == chosen.len(), "інстанс поза пластиною");
        items.sort_by(|a, b| b.1.size().y.total_cmp(&a.1.size().y));
        let (mut x, mut y, mut row) = (lo[0], lo[1], 0.0_f64);
        let mut moves = Vec::new();
        for (key, bounds) in items {
            let size = bounds.size();
            if size.x > hi[0] - lo[0] || size.y > hi[1] - lo[1] {
                bail!("обʼєкт не вміщається на стіл");
            }
            loop {
                if x + size.x > hi[0] {
                    x = lo[0];
                    y += (row + spacing).max(1.0);
                    row = 0.0;
                }
                ensure!(y + size.y <= hi[1], "на пластині недостатньо місця");
                let blocker = obstacles.iter().find(|(_, o)| {
                    x < o.max.x + spacing
                        && x + size.x + spacing > o.min.x
                        && y < o.max.y + spacing
                        && y + size.y + spacing > o.min.y
                });
                match blocker {
                    Some((_, o)) => x = o.max.x + spacing,
                    None => break,
                }
            }
            moves.push((
                key,
                DVec3::new(x - bounds.min.x, y - bounds.min.y, -bounds.min.z),
            ));
            x += size.x + spacing;
            row = row.max(size.y);
        }
        for (key, delta) in moves {
            self.translate(key, delta)?;
        }
        Ok(())
    }
}

fn outer(a: DVec3, b: DVec3) -> DMat3 {
    DMat3::from_cols(a * b.x, a * b.y, a * b.z)
}

#[cfg(test)]
mod audit_tests {
    use super::*;
    #[test]
    fn imported_3mf_preserves_ranges_and_plate_separation() {
        let mut source = Project::default();
        source.add_plate("first").unwrap();
        source
            .add_mesh_object(
                "test",
                crate::IndexedMesh {
                    positions: vec![[0., 0., 0.], [20., 0., 0.], [0., 10., 5.]],
                    triangles: vec![[0, 1, 2]],
                    ..Default::default()
                },
                1,
                [128., 128.],
                [0., 0.],
                [256., 256.],
            )
            .unwrap();
        let key = crate::InstanceRef {
            object: 1,
            instance: 0,
        };
        source.objects[0].instances[0].plate = Some(1);
        let second = source.add_plate("second").unwrap();
        let mut second_instance = source.instance(key).unwrap().clone();
        second_instance.plate = Some(second);
        source.objects[0].instances.push(second_instance);
        source.archive.insert(
            "Metadata/layer_config_ranges.xml".into(),
            std::sync::Arc::from(b"<objects/>".as_slice()),
        );
        source.layer_ranges.push(crate::LayerRange {
            source_object_id: 1,
            min_z: 0.2,
            max_z: 1.,
            ..Default::default()
        });
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("source.3mf");
        source.save_3mf(&file).unwrap();
        let model = read_model(&file).unwrap();
        let mut target = source.clone();
        target.archive.clear();
        target.layer_ranges.clear();
        let keys = target
            .add_model(model, 1, [128., 128.], [0., 0.], [256., 256.])
            .unwrap();
        assert_ne!(keys[0].object, 1);
        assert_eq!(target.object_layer_ranges(keys[0].object).unwrap().len(), 1);
        let a = target.instance_bounds(keys[0]).unwrap().unwrap();
        let b = target.instance_bounds(keys[1]).unwrap().unwrap();
        assert!((a.center() - b.center()).length() > 256.);
        let saved = dir.path().join("target.3mf");
        target.save_3mf(&saved).unwrap();
        let loaded = Project::load_3mf(&saved).unwrap();
        assert_eq!(loaded.object_layer_ranges(keys[0].object).unwrap().len(), 1);
    }
}
