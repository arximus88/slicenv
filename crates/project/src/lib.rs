//! Редагований проєкт Bambu: локальні координати пластин, сітки й метадані.

mod add;
mod cut;
mod edit;
pub mod bounds_cache;
pub mod history;
mod hull;
pub mod orient;
mod object_ops;
mod primitives;
mod threemf;
mod xml;

pub use add::{
    read_stl,
    IMPORT_EXTENSIONS, ImportedModel, MESH_EXTENSIONS, ReplaceReport, SOURCE_FILE, parse_obj,
    read_mesh_file, read_model,
};
pub use hull::{HullPlane, convex_hull, hull_planes, pick_plane};
pub use object_ops::{INCH, connected_components};
pub use primitives::{Handy, Primitive};
pub use threemf::MeshSource;
pub use xml::{XmlContent, XmlNode};

use std::collections::BTreeMap;

pub use cut::{CutOptions, CutPlane, CutResult};
pub use glam::{DAffine3, DMat3, DQuat, DVec3};

pub const BED_SIZE: f64 = 256.0;
pub const PLATE_STRIDE: f64 = BED_SIZE * 1.2;
pub type Config = BTreeMap<String, String>;
pub type ObjectId = u32;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Project {
    pub objects: Vec<Object>,
    pub plates: Vec<Plate>,
    pub config: serde_json::Map<String, serde_json::Value>,
    pub metadata: Config,
    /// Layer ranges use resolved root object IDs in memory. On disk Bambu uses
    /// 1-based model-object positions, remapped when loading and saving.
    pub(crate) layer_ranges: Vec<LayerRange>,
    pub(crate) unmatched_layer_ranges: Vec<LayerRange>,
    /// Усі вихідні ZIP-файли, включно з невідомими розширеннями.
    pub archive: BTreeMap<String, std::sync::Arc<[u8]>>,
    pub root: Option<XmlNode>,
    pub settings: Option<XmlNode>,
    #[doc(hidden)]
    pub source_fingerprint: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayerRange {
    /// Resolved root object ID in memory; Bambu's on-disk IDs are positional.
    pub source_object_id: u32,
    /// Original range node, retaining extension metadata separately from options.
    pub metadata: Option<XmlNode>,
    pub min_z: f64,
    pub max_z: f64,
    pub config: Config,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Object {
    pub model: Option<XmlNode>,
    pub id: ObjectId,
    pub name: String,
    pub volumes: Vec<Volume>,
    pub instances: Vec<Instance>,
    pub config: Config,
    pub metadata: Option<XmlNode>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Volume {
    pub name: String,
    /// Shared by snapshots; geometry edits use `Arc::make_mut`.
    pub mesh: std::sync::Arc<IndexedMesh>,
    pub transform: DAffine3,
    pub kind: VolumeKind,
    pub config: Config,
    pub metadata: Option<XmlNode>,
    pub source: Option<threemf::MeshSource>,
}

impl Default for Volume {
    fn default() -> Self {
        Self {
            name: String::new(),
            mesh: Default::default(),
            transform: DAffine3::IDENTITY,
            kind: VolumeKind::Part,
            config: Config::new(),
            metadata: None,
            source: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct IndexedMesh {
    pub positions: Vec<[f64; 3]>,
    pub triangles: Vec<[u32; 3]>,
    /// Розфарбування, підтримки й інші атрибути окремих граней.
    pub triangle_attributes: BTreeMap<usize, Config>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VolumeKind {
    #[default]
    Part,
    NegativeVolume,
    Modifier,
    SupportBlocker,
    SupportEnforcer,
}

/// Трансформа — у координатах пластини; `None` означає поза пластинами.
#[derive(Clone, Debug, PartialEq)]
pub struct Instance {
    pub transform: DAffine3,
    pub plate: Option<u32>,
    pub printable: bool,
    pub metadata: Option<XmlNode>,
}

impl Default for Instance {
    fn default() -> Self {
        Self {
            transform: DAffine3::IDENTITY,
            plate: Some(1),
            printable: true,
            metadata: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct InstanceRef {
    pub object: ObjectId,
    pub instance: usize,
}

/// Індекси пластин послідовні з 1; видалення перенумеровує наступні.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plate {
    pub index: u32,
    pub name: String,
    pub bed_type: Option<String>,
    pub locked: bool,
    pub objects: Vec<InstanceRef>,
    pub metadata: Option<XmlNode>,
}

/// Той самий формат, що й `viewer::Mesh`, без залежності від GPU.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TriangleSoup {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min: DVec3,
    pub max: DVec3,
}

impl Bounds {
    pub fn size(self) -> DVec3 {
        self.max - self.min
    }
    pub fn center(self) -> DVec3 {
        (self.max + self.min) * 0.5
    }
}

impl Project {
    pub fn object(&self, id: ObjectId) -> anyhow::Result<&Object> {
        self.objects
            .iter()
            .find(|o| o.id == id)
            .ok_or_else(|| anyhow::anyhow!("немає обʼєкта {id}"))
    }

    pub fn instance(&self, key: InstanceRef) -> anyhow::Result<&Instance> {
        self.object(key.object)?
            .instances
            .get(key.instance)
            .ok_or_else(|| anyhow::anyhow!("немає інстанса {key:?}"))
    }

    pub fn instance_mut(&mut self, key: InstanceRef) -> anyhow::Result<&mut Instance> {
        self.objects
            .iter_mut()
            .find(|o| o.id == key.object)
            .and_then(|o| o.instances.get_mut(key.instance))
            .ok_or_else(|| anyhow::anyhow!("немає інстанса {key:?}"))
    }

    /// Відновлює список інстансів після прямого редагування публічних полів.
    /// Призначає `identify_id` інстансам на пластинах, у яких його ще немає, — у тому самому
    /// порядку, що й `save_3mf` (пластини, об’єкти, копії; від найбільшого наявного + 1).
    /// Тоді копія проєкту лише з частиною об’єктів (вхід рушія для однієї пластини)
    /// зберігається з тими самими ID, що й повний проєкт: це мітки об’єктів у G-code
    /// (`unique label id`) і `slice_info.config`, за якими принтер пропускає об’єкти.
    pub fn ensure_identify_ids(&mut self) {
        let read = |instance: &Instance| {
            instance
                .metadata
                .as_ref()
                .and_then(|m| m.metadata().get("identify_id")?.parse::<u32>().ok())
        };
        let mut next = self
            .objects
            .iter()
            .flat_map(|o| &o.instances)
            .filter_map(read)
            .max()
            .map_or(1, |n| n.saturating_add(1));
        let mut used = std::collections::HashSet::new();
        let plates: Vec<u32> = self.plates.iter().map(|p| p.index).collect();
        for plate in plates {
            for object in &mut self.objects {
                for instance in object.instances.iter_mut().filter(|i| i.plate == Some(plate)) {
                    if read(instance).is_some_and(|id| used.insert(id)) || next == u32::MAX {
                        continue;
                    }
                    instance
                        .metadata
                        .get_or_insert_with(|| XmlNode::new("model_instance"))
                        .set_metadata("identify_id", next);
                    used.insert(next);
                    next += 1;
                }
            }
        }
    }

    pub fn sync_plates(&mut self) {
        for p in &mut self.plates {
            let index = p.index;
            p.objects = self
                .objects
                .iter()
                .flat_map(|o| {
                    o.instances
                        .iter()
                        .enumerate()
                        .filter(move |(_, i)| i.plate == Some(index))
                        .map(move |(instance, _)| InstanceRef {
                            object: o.id,
                            instance,
                        })
                })
                .collect();
        }
    }

    pub fn instance_bounds(&self, key: InstanceRef) -> anyhow::Result<Option<Bounds>> {
        self.instance_bounds_with_kinds(key, &[VolumeKind::Part])
    }

    pub fn instance_bounds_with_kinds(
        &self,
        key: InstanceRef,
        kinds: &[VolumeKind],
    ) -> anyhow::Result<Option<Bounds>> {
        let object = self.object(key.object)?;
        let instance = self.instance(key)?;
        let mut bounds: Option<Bounds> = None;
        for volume in &object.volumes {
            if !kinds.contains(&volume.kind) {
                continue;
            }
            let transform = instance.transform * volume.transform;
            for point in &volume.mesh.positions {
                let point = transform.transform_point3(DVec3::from(*point));
                match &mut bounds {
                    Some(b) => {
                        b.min = b.min.min(point);
                        b.max = b.max.max(point);
                    }
                    None => {
                        bounds = Some(Bounds {
                            min: point,
                            max: point,
                        })
                    }
                }
            }
        }
        Ok(bounds)
    }

    /// Трикутники деталей інстансу у світових координатах: (частина, трикутник, вершини).
    pub fn world_triangles(&self, key: InstanceRef) -> anyhow::Result<Vec<(usize, usize, [DVec3; 3])>> {
        let object = self.object(key.object)?;
        let instance = self.instance(key)?;
        let mut out = Vec::new();
        for (vi, volume) in object.volumes.iter().enumerate() {
            if volume.kind != VolumeKind::Part {
                continue;
            }
            let transform = instance.transform * volume.transform;
            let points: Vec<DVec3> = volume
                .mesh
                .positions
                .iter()
                .map(|p| transform.transform_point3(DVec3::from(*p)))
                .collect();
            for (ti, t) in volume.mesh.triangles.iter().enumerate() {
                let (Some(a), Some(b), Some(c)) = (
                    points.get(t[0] as usize),
                    points.get(t[1] as usize),
                    points.get(t[2] as usize),
                ) else {
                    continue;
                };
                out.push((vi, ti, [*a, *b, *c]));
            }
        }
        Ok(out)
    }

    /// Найближчий перетин променя з деталями інстансу: (відстань, частина, трикутник).
    pub fn raycast(
        &self,
        key: InstanceRef,
        origin: DVec3,
        dir: DVec3,
    ) -> anyhow::Result<Option<(f64, usize, usize)>> {
        let mut best: Option<(f64, usize, usize)> = None;
        for (vi, ti, [a, b, c]) in self.world_triangles(key)? {
            // Möller–Trumbore.
            let (e1, e2) = (b - a, c - a);
            let p = dir.cross(e2);
            let det = e1.dot(p);
            if det.abs() < 1e-12 {
                continue;
            }
            let inv = 1.0 / det;
            let s = origin - a;
            let u = s.dot(p) * inv;
            if !(0.0..=1.0).contains(&u) {
                continue;
            }
            let q = s.cross(e1);
            let v = dir.dot(q) * inv;
            if v < 0.0 || u + v > 1.0 {
                continue;
            }
            let t = e2.dot(q) * inv;
            if t > 1e-9 && best.is_none_or(|(bt, _, _)| t < bt) {
                best = Some((t, vi, ti));
            }
        }
        Ok(best)
    }

    /// Пласка грань навколо трикутника: усі трикутники інстансу в тій самій площині
    /// (кут нормалей до ~2°, відстань до 0.05 мм) — що саме ляже на стіл.
    pub fn facet(
        &self,
        key: InstanceRef,
        volume: usize,
        triangle: usize,
    ) -> anyhow::Result<Vec<[DVec3; 3]>> {
        let all = self.world_triangles(key)?;
        let seed = all
            .iter()
            .find(|(v, t, _)| *v == volume && *t == triangle)
            .ok_or_else(|| anyhow::anyhow!("немає грані"))?
            .2;
        let normal = (seed[1] - seed[0])
            .cross(seed[2] - seed[0])
            .normalize_or_zero();
        let offset = normal.dot(seed[0]);
        Ok(all
            .into_iter()
            .map(|(_, _, p)| p)
            .filter(|p| {
                let n = (p[1] - p[0]).cross(p[2] - p[0]).normalize_or_zero();
                n.dot(normal) > 0.9994 && p.iter().all(|v| (normal.dot(*v) - offset).abs() < 0.05)
            })
            .collect())
    }

    pub fn plate_bounds(&self, plate: Option<u32>) -> anyhow::Result<Vec<(InstanceRef, Bounds)>> {
        let mut out = Vec::new();
        for o in &self.objects {
            for (instance, i) in o.instances.iter().enumerate() {
                if i.plate == plate {
                    let key = InstanceRef {
                        object: o.id,
                        instance,
                    };
                    if let Some(b) = self.instance_bounds(key)? {
                        out.push((key, b));
                    }
                }
            }
        }
        Ok(out)
    }

    /// Світові координати відносно початку обраної пластини.
    pub fn plate_mesh(&self, plate: Option<u32>) -> anyhow::Result<TriangleSoup> {
        self.plate_mesh_with_kinds(plate, &[VolumeKind::Part])
    }

    /// Для окремого шару модифікаторів чи підтримок у viewport.
    pub fn plate_mesh_with_kinds(
        &self,
        plate: Option<u32>,
        kinds: &[VolumeKind],
    ) -> anyhow::Result<TriangleSoup> {
        self.plate_mesh_where(plate, kinds, |_, _| true)
    }

    /// Сітка пластини без одного інстансу й окремо сам інстанс — для перетягування,
    /// коли рухомий об’єкт малюється з матрицею на GPU, а решта не перебудовується.
    pub fn plate_mesh_split(
        &self,
        plate: Option<u32>,
        key: InstanceRef,
    ) -> anyhow::Result<(TriangleSoup, TriangleSoup)> {
        self.plate_mesh_split_volume(plate, key, None)
    }

    /// Окрема частина використовує той самий GPU-шлях, що й інстанс.
    pub fn plate_mesh_split_volume(
        &self,
        plate: Option<u32>,
        key: InstanceRef,
        volume: Option<usize>,
    ) -> anyhow::Result<(TriangleSoup, TriangleSoup)> {
        let selected = |k, i| k == key && volume.is_none_or(|v| v == i);
        Ok((
            self.plate_mesh_where(plate, &[VolumeKind::Part], |k, i| !selected(k, i))?,
            self.plate_mesh_where(plate, &[VolumeKind::Part], selected)?,
        ))
    }

    fn plate_mesh_where(
        &self,
        plate: Option<u32>,
        kinds: &[VolumeKind],
        keep: impl Fn(InstanceRef, usize) -> bool,
    ) -> anyhow::Result<TriangleSoup> {
        let mut out = TriangleSoup::default();
        for object in &self.objects {
            for (index, instance) in object.instances.iter().enumerate() {
                if instance.plate != plate {
                    continue;
                }
                for (volume_index, volume) in object.volumes.iter().enumerate() {
                    if !kinds.contains(&volume.kind)
                        || !keep(
                            InstanceRef {
                                object: object.id,
                                instance: index,
                            },
                            volume_index,
                        )
                    {
                        continue;
                    }
                    let transform = instance.transform * volume.transform;
                    for triangle in &volume.mesh.triangles {
                        let mut p = [DVec3::ZERO; 3];
                        for (j, &index) in triangle.iter().enumerate() {
                            let v = volume.mesh.positions.get(index as usize).ok_or_else(|| {
                                anyhow::anyhow!("індекс вершини {index} поза сіткою")
                            })?;
                            p[j] = transform.transform_point3(DVec3::from(*v));
                        }
                        let n = (p[1] - p[0])
                            .cross(p[2] - p[0])
                            .normalize_or_zero()
                            .as_vec3()
                            .to_array();
                        for v in p {
                            out.positions.push(v.as_vec3().to_array());
                            out.normals.push(n);
                        }
                    }
                }
            }
        }
        Ok(out)
    }
}
