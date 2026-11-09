use crate::{
    BED_SIZE, DAffine3, DVec3, IndexedMesh, Instance, LayerRange, Object, PLATE_STRIDE, Plate, Project,
    Volume, VolumeKind, XmlContent, XmlNode, xml::attrs,
};
use anyhow::{Context, Result, bail, ensure};
use glam::DMat3;
use quick_xml::{Reader, events::Event};
use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    hash::{Hash, Hasher},
    io::{BufReader, Read, Write},
    path::Path,
    sync::Arc,
};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

const ROOT: &str = "3D/3dmodel.model";
const SETTINGS: &str = "Metadata/model_settings.config";
const CONFIG: &str = "Metadata/project_settings.config";
const LAYER_RANGES: &str = "Metadata/layer_config_ranges.xml";
const CORE: &str = "http://schemas.microsoft.com/3dmanufacturing/core/2015/02";
const PRODUCTION: &str = "http://schemas.microsoft.com/3dmanufacturing/production/2015/06";

fn parse_layer_ranges(bytes: &[u8]) -> Result<Vec<LayerRange>> {
    let xml = XmlNode::parse(std::str::from_utf8(bytes)?.into())?;
    let mut out = Vec::new();
    for object in xml.elements().filter(|n| n.local_name() == "object") {
        let source_object_id = object
            .attr("id")
            .context("layer range object without id")?
            .parse()?;
        for range in object.elements().filter(|n| n.local_name() == "range") {
            let min_z: f64 = range
                .attr("min_z")
                .context("layer range without min_z")?
                .parse()?;
            let max_z: f64 = range
                .attr("max_z")
                .context("layer range without max_z")?
                .parse()?;
            ensure!(
                min_z.is_finite() && max_z.is_finite() && min_z < max_z,
                "invalid layer range"
            );
            let mut config = crate::Config::new();
            for option in range.elements().filter(|n| n.local_name() == "option") {
                let key = option
                    .attr("opt_key")
                    .context("layer option without opt_key")?;
                let text = option
                    .children
                    .iter()
                    .filter_map(|child| match child {
                        XmlContent::Raw { source, start, end } => Some(&source[*start..*end]),
                        _ => None,
                    })
                    .collect::<String>();
                config.insert(key.into(), quick_xml::escape::unescape(&text)?.into_owned());
            }
            out.push(LayerRange {
                source_object_id,
                min_z,
                max_z,
                config,
                metadata: Some(range.clone()),
            });
        }
    }
    Ok(out)
}

fn layer_range_node(range: &LayerRange) -> XmlNode {
    let mut node = range
        .metadata
        .clone()
        .unwrap_or_else(|| XmlNode::new("range"));
    node.set("min_z", range.min_z);
    node.set("max_z", range.max_z);
    node.remove_elements("option");
    for (key, value) in &range.config {
        let mut option = XmlNode::new("option");
        option.set("opt_key", key);
        let text: std::sync::Arc<str> = quick_xml::escape::escape(value).into_owned().into();
        option.children.push(XmlContent::Raw {
            start: 0,
            end: text.len(),
            source: text,
        });
        node.push(option);
    }
    node
}

/// Посилання на незмінену геометрію; зберігає XML розширень без переписування.
#[derive(Clone, Debug, PartialEq)]
pub struct MeshSource {
    path: String,
    id: u32,
    fingerprint: u64,
}

struct ModelFile {
    tree: XmlNode,
    meshes: HashMap<u32, IndexedMesh>,
}

impl Project {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        match path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "3mf" => Self::load_3mf(path),
            "stl" => Self::load_stl(path),
            "obj" => Self::load_mesh(path, crate::read_mesh_file(path)?),
            _ => bail!("потрібен STL, 3MF або OBJ"),
        }
    }

    pub fn load_stl(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        Self::load_mesh(path, crate::read_stl(path)?)
    }

    /// Проєкт з однією сіткою з файлу `path` (STL чи OBJ) на першій пластині.
    fn load_mesh(path: &Path, mesh: IndexedMesh) -> Result<Self> {
        let name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let mut project = Self::default();
        project.add_plate("")?;
        let mut volume = Volume {
            name: name.clone(),
            mesh: mesh.into(),
            ..Volume::default()
        };
        volume.set_source_file(&std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned()));
        project.objects.push(Object {
            id: 1,
            name,
            volumes: vec![volume],
            instances: vec![Instance::default()],
            ..Object::default()
        });
        project.sync_plates();
        let key = project.plates[0].objects[0];
        project.drop_to_bed(key)?;
        project.center_on_plate(key)?;
        Ok(project)
    }

    pub fn load_3mf(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let mut zip = ZipArchive::new(BufReader::new(File::open(path)?))?;
        let mut project = Self::default();
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            let name = entry.name()?.into_owned();
            ensure!(
                !project.archive.contains_key(&name),
                "повторений ZIP-файл {name}"
            );
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            project.archive.insert(name, bytes.into());
        }
        if let Some(bytes) = project.archive.get(CONFIG) {
            project.config =
                serde_json::from_slice(bytes).context("project_settings.config не JSON-обʼєкт")?;
        }
        if let Some(bytes) = project.archive.get(LAYER_RANGES) {
            project.layer_ranges = parse_layer_ranges(bytes)?;
        }
        if let Some(bytes) = project.archive.get(SETTINGS) {
            project.settings = Some(XmlNode::parse(std::str::from_utf8(bytes)?.into())?);
        }
        if let Some(settings) = &project.settings {
            for n in settings.elements().filter(|n| n.local_name() == "plate") {
                let m = n.metadata();
                let index = m
                    .get("plater_id")
                    .context("пластина без plater_id")?
                    .parse()?;
                ensure!(
                    index > 0 && !project.plates.iter().any(|p| p.index == index),
                    "некоректний індекс пластини"
                );
                project.plates.push(Plate {
                    index,
                    name: m.get("plater_name").cloned().unwrap_or_default(),
                    locked: m.get("locked").is_some_and(|v| v == "true" || v == "1"),
                    bed_type: m.get("bed_type").cloned(),
                    metadata: Some(n.clone()),
                    ..Plate::default()
                });
            }
        }
        project.plates.sort_by_key(|p| p.index);
        let explicit_plates = !project.plates.is_empty();
        if !explicit_plates {
            project.add_plate("")?;
        }
        let root = read_model(
            project
                .archive
                .get(ROOT)
                .context("немає 3D/3dmodel.model")?,
        )?;
        ensure!(
            root.tree.attr("unit").is_none_or(|u| u == "millimeter"),
            "підтримуються одиниці millimeter"
        );
        for n in root
            .tree
            .elements()
            .filter(|n| n.local_name() == "metadata")
        {
            if let Some(name) = n.attr("name") {
                let xml = n.to_xml();
                let content = if n.children.is_empty() {
                    String::new()
                } else {
                    let start = xml.find('>').context("metadata")? + 1;
                    quick_xml::escape::unescape(&xml[start..xml.rfind("</").context("metadata")?])?
                        .into_owned()
                };
                project.metadata.insert(name.into(), content);
            }
        }
        let build = root.tree.element("build").context("немає build")?.clone();
        project.root = Some(root.tree.clone());
        let mut files = HashMap::from([(ROOT.to_owned(), root)]);
        for item in build.elements().filter(|n| n.local_name() == "item") {
            let id = parse_id(item, "objectid")?;
            if !project.objects.iter().any(|o| o.id == id) {
                let model = files[ROOT]
                    .tree
                    .element("resources")
                    .context("немає resources")?
                    .elements()
                    .find(|n| {
                        n.local_name() == "object"
                            && n.attr("id").and_then(|s| s.parse::<u32>().ok()) == Some(id)
                    })
                    .context("build посилається на відсутній обʼєкт")?
                    .clone();
                let metadata = project
                    .settings
                    .as_ref()
                    .and_then(|s| {
                        s.elements().find(|n| {
                            n.local_name() == "object"
                                && n.attr("id").and_then(|s| s.parse::<u32>().ok()) == Some(id)
                        })
                    })
                    .cloned();
                let mut config = metadata.as_ref().map(XmlNode::metadata).unwrap_or_default();
                let name = config
                    .remove("name")
                    .or_else(|| model.attr("name").map(str::to_owned))
                    .unwrap_or_default();
                let mut volumes = Vec::new();
                collect_volumes(
                    &project.archive,
                    &mut files,
                    ROOT,
                    id,
                    DAffine3::IDENTITY,
                    &mut volumes,
                    0,
                )?;
                let parts: Vec<_> = metadata
                    .as_ref()
                    .into_iter()
                    .flat_map(|n| n.elements())
                    .filter(|n| n.local_name() == "part" || n.local_name() == "volume")
                    .collect();
                if parts
                    .iter()
                    .any(|p| p.attr("firstid").is_some() || p.attr("lastid").is_some())
                {
                    ensure!(
                        volumes.len() == 1,
                        "діапазони граней потребують однієї сітки"
                    );
                    let base = volumes.remove(0);
                    for part in &parts {
                        let first: usize = part.attr("firstid").unwrap_or("0").parse()?;
                        let last: usize =
                            part.attr("lastid").context("частина без lastid")?.parse()?;
                        ensure!(
                            first <= last && last < base.mesh.triangles.len(),
                            "некоректний діапазон граней"
                        );
                        let mut mesh = IndexedMesh::default();
                        let mut remap = BTreeMap::new();
                        for index in first..=last {
                            let triangle = base.mesh.triangles[index].map(|old| {
                                *remap.entry(old).or_insert_with(|| {
                                    let new = mesh.positions.len() as u32;
                                    mesh.positions.push(base.mesh.positions[old as usize]);
                                    new
                                })
                            });
                            if let Some(attrs) = base.mesh.triangle_attributes.get(&index) {
                                mesh.triangle_attributes
                                    .insert(mesh.triangles.len(), attrs.clone());
                            }
                            mesh.triangles.push(triangle);
                        }
                        volumes.push(Volume {
                            mesh: mesh.into(),
                            transform: base.transform,
                            ..Volume::default()
                        });
                    }
                }
                for (i, v) in volumes.iter_mut().enumerate() {
                    let part = parts
                        .iter()
                        .find(|p| {
                            v.source.as_ref().is_some_and(|source| {
                                p.attr("id").and_then(|v| v.parse::<u32>().ok()) == Some(source.id)
                            })
                        })
                        .copied()
                        .or_else(|| parts.get(i).copied());
                    if let Some(part) = part {
                        let mut config = part.metadata();
                        v.name = config.remove("name").unwrap_or_default();
                        v.kind = VolumeKind::parse(
                            config
                                .get("volume_type")
                                .or_else(|| config.get("part_type"))
                                .map(String::as_str)
                                .or_else(|| part.attr("subtype"))
                                .unwrap_or("normal_part"),
                        )?;
                        v.config = config;
                        v.metadata = Some(part.clone());
                    }
                }
                project.objects.push(Object {
                    id,
                    name,
                    volumes,
                    config,
                    metadata,
                    model: Some(model),
                    instances: Vec::new(),
                });
            }
            let object_index = project
                .objects
                .iter()
                .position(|o| o.id == id)
                .context("немає обʼєкта")?;
            let instance_index = project.objects[object_index].instances.len();
            let membership = project.plates.iter().find_map(|p| {
                p.metadata
                    .as_ref()?
                    .elements()
                    .filter(|n| n.local_name() == "model_instance")
                    .find_map(|n| {
                        let m = n.metadata();
                        (m.get("object_id").and_then(|s| s.parse::<u32>().ok()) == Some(id)
                            && m.get("instance_id").and_then(|s| s.parse::<usize>().ok())
                                == Some(instance_index))
                        .then_some((p.index, n.clone()))
                    })
            });
            let plate = if explicit_plates {
                membership.as_ref().map(|(i, _)| *i)
            } else {
                Some(1)
            };
            let global = parse_transform(item.attr("transform"))?;
            let offset = plate
                .map(|p| project.plate_offset(p))
                .transpose()?
                .unwrap_or(DVec3::ZERO);
            let transform = DAffine3::from_translation(-offset) * global;
            let mut metadata = membership
                .map(|(_, n)| n)
                .unwrap_or_else(|| XmlNode::new("model_instance"));
            // Build-атрибути тримаємо окремим дочірнім вузлом.
            if let Some(assemble) = project
                .settings
                .as_ref()
                .and_then(|s| s.element("assemble"))
            {
                for n in assemble.elements().filter(|n| {
                    n.local_name() == "assemble_item"
                        && n.attr("object_id").and_then(|v| v.parse::<u32>().ok()) == Some(id)
                        && n.attr("instance_id").and_then(|v| v.parse::<usize>().ok())
                            == Some(instance_index)
                }) {
                    let mut n = n.clone();
                    n.name = "project:assemble_item".into();
                    metadata.push(n);
                }
            }
            let mut original = item.clone();
            original.name = "project:build_item".into();
            metadata.push(original);
            project.objects[object_index].instances.push(Instance {
                transform,
                plate,
                printable: match item.attr("printable").map(str::trim) {
                    None | Some("1" | "true") => true,
                    Some("0" | "false") => false,
                    _ => bail!("некоректний атрибут printable у 3MF"),
                },
                metadata: Some(metadata),
            });
        }
        project.sync_plates();
        // Bambu's range IDs are 1-based positions in model-object creation order.
        // `objects` is appended in that same order while consuming build items.
        let mut resolved = Vec::new();
        for mut range in std::mem::take(&mut project.layer_ranges) {
            if let Some(object) = range
                .source_object_id
                .checked_sub(1)
                .and_then(|i| project.objects.get(i as usize))
            {
                range.source_object_id = object.id;
                resolved.push(range);
            } else {
                project.unmatched_layer_ranges.push(range);
            }
        }
        project.layer_ranges = resolved;
        project.source_fingerprint = Some(project.fingerprint());
        Ok(project)
    }

    /// Позиція пластини в глобальній сітці Bambu.
    pub fn plate_offset(&self, index: u32) -> Result<DVec3> {
        let slot = self
            .plates
            .iter()
            .position(|p| p.index == index)
            .context("немає пластини")?;
        let columns = (self.plates.len() as f64).sqrt().ceil().max(1.0) as usize;
        let [sx, sy] = self.plate_stride();
        Ok(DVec3::new(
            (slot % columns) as f64 * sx,
            -((slot / columns) as f64) * sy,
            0.0,
        ))
    }

    /// Крок сітки пластин, як у Bambu (`PartPlateList`): розмір столу × 1.2 по кожній
    /// осі. Стіл — з `printable_area` конфігурації проєкту; без неї — 256 мм.
    /// Для A1 mini (180 мм) крок 216, а не 307: інакше об’єкти другої пластини
    /// у файлі опиняються за межею столу.
    pub fn plate_stride(&self) -> [f64; 2] {
        let mut lo = [f64::INFINITY; 2];
        let mut hi = [f64::NEG_INFINITY; 2];
        let points = self
            .config
            .get("printable_area")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| {
                let (x, y) = v.as_str()?.split_once('x')?;
                Some([x.trim().parse::<f64>().ok()?, y.trim().parse::<f64>().ok()?])
            });
        for point in points {
            for axis in 0..2 {
                lo[axis] = lo[axis].min(point[axis]);
                hi[axis] = hi[axis].max(point[axis]);
            }
        }
        std::array::from_fn(|axis| {
            let size = hi[axis] - lo[axis];
            if size.is_finite() && size > 1.0 {
                size * (PLATE_STRIDE / BED_SIZE)
            } else {
                PLATE_STRIDE
            }
        })
    }
}

impl VolumeKind {
    fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "normal_part" | "0" => Self::Part,
            "negative_part" | "negative_volume" | "4" => Self::NegativeVolume,
            "modifier_part" | "parameter_modifier" | "1" => Self::Modifier,
            "support_blocker" | "2" => Self::SupportBlocker,
            "support_enforcer" | "3" => Self::SupportEnforcer,
            _ => bail!("невідомий тип частини {text}"),
        })
    }
    fn as_str(self) -> &'static str {
        match self {
            Self::Part => "normal_part",
            Self::NegativeVolume => "negative_part",
            Self::Modifier => "modifier_part",
            Self::SupportBlocker => "support_blocker",
            Self::SupportEnforcer => "support_enforcer",
        }
    }
}

fn parse_id(n: &XmlNode, key: &str) -> Result<u32> {
    n.attr(key)
        .with_context(|| format!("немає {key}"))?
        .parse()
        .context("некоректний ID")
}

pub(crate) fn parse_transform(raw: Option<&str>) -> Result<DAffine3> {
    let Some(raw) = raw else {
        return Ok(DAffine3::IDENTITY);
    };
    let m: Vec<f64> = raw
        .split_ascii_whitespace()
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()?;
    ensure!(
        m.len() == 12 && m.iter().all(|n| n.is_finite()),
        "некоректна матриця 3MF"
    );
    Ok(DAffine3::from_cols(
        DVec3::new(m[0], m[1], m[2]),
        DVec3::new(m[3], m[4], m[5]),
        DVec3::new(m[6], m[7], m[8]),
        DVec3::new(m[9], m[10], m[11]),
    ))
}
fn transform_string(t: DAffine3) -> String {
    t.to_cols_array()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}
fn mesh_fingerprint(mesh: &IndexedMesh) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    mesh.positions.len().hash(&mut h);
    for p in &mesh.positions {
        for v in p {
            v.to_bits().hash(&mut h);
        }
    }
    mesh.triangles.hash(&mut h);
    mesh.triangle_attributes.hash(&mut h);
    h.finish()
}

fn read_model(bytes: &[u8]) -> Result<ModelFile> {
    let source: Arc<str> = std::str::from_utf8(bytes)?.into();
    let tree = XmlNode::parse(source.clone())?;
    let mut reader = Reader::from_str(&source);
    let mut current = None;
    let mut meshes = HashMap::new();
    let mut mesh = IndexedMesh::default();
    loop {
        match reader.read_event()? {
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                "object" => {
                    current = Some(
                        attrs(&e)?
                            .get("id")
                            .context("обʼєкт без id")?
                            .parse::<u32>()?,
                    );
                    mesh = IndexedMesh::default();
                }
                "vertex" => {
                    let a = attrs(&e)?;
                    let mut p = [0.; 3];
                    for (i, key) in ["x", "y", "z"].iter().enumerate() {
                        p[i] = a
                            .get(*key)
                            .context("вершина без координат")?
                            .parse::<f64>()?;
                    }
                    ensure!(p.iter().all(|n| n.is_finite()), "нескінченна вершина");
                    mesh.positions.push(p);
                }
                "triangle" => {
                    let mut a = attrs(&e)?;
                    let mut t = [0; 3];
                    for (i, key) in ["v1", "v2", "v3"].iter().enumerate() {
                        t[i] = a.remove(*key).context("грань без індексів")?.parse()?;
                    }
                    if !a.is_empty() {
                        mesh.triangle_attributes.insert(mesh.triangles.len(), a);
                    }
                    mesh.triangles.push(t);
                }
                _ => {}
            },
            Event::End(e) if e.local_name().as_ref() == "object" => {
                if let Some(id) = current.take() {
                    ensure!(
                        mesh.triangles
                            .iter()
                            .flatten()
                            .all(|&i| (i as usize) < mesh.positions.len()),
                        "індекс вершини поза сіткою"
                    );
                    ensure!(!meshes.contains_key(&id), "повторений ID обʼєкта");
                    meshes.insert(id, std::mem::take(&mut mesh));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(ModelFile { tree, meshes })
}

fn resolve_path(parent: &str, path: &str) -> Result<String> {
    let joined = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("{}/{path}", parent.rsplit_once('/').map_or("", |(p, _)| p))
    };
    let mut parts = Vec::new();
    for p in joined.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                ensure!(parts.pop().is_some(), "шлях поза архівом");
            }
            _ => parts.push(p),
        }
    }
    Ok(parts.join("/"))
}

fn collect_volumes(
    archive: &BTreeMap<String, Arc<[u8]>>,
    files: &mut HashMap<String, ModelFile>,
    path: &str,
    id: u32,
    transform: DAffine3,
    out: &mut Vec<Volume>,
    depth: usize,
) -> Result<()> {
    ensure!(depth < 32, "цикл або надто глибокі компоненти");
    if !files.contains_key(path) {
        files.insert(
            path.into(),
            read_model(archive.get(path).with_context(|| format!("немає {path}"))?)?,
        );
    }
    let file = &files[path];
    let node = file
        .tree
        .element("resources")
        .context("немає resources")?
        .elements()
        .find(|n| {
            n.local_name() == "object"
                && n.attr("id").and_then(|s| s.parse::<u32>().ok()) == Some(id)
        })
        .with_context(|| format!("немає object {id} у {path}"))?
        .clone();
    let mesh = file.meshes.get(&id).context("немає сітки")?;
    if !mesh.triangles.is_empty() {
        out.push(Volume {
            mesh: mesh.clone().into(),
            transform,
            source: Some(MeshSource {
                path: path.into(),
                id,
                fingerprint: mesh_fingerprint(mesh),
            }),
            ..Volume::default()
        });
    }
    if let Some(components) = node.element("components") {
        for c in components
            .elements()
            .filter(|n| n.local_name() == "component")
        {
            let target = c
                .attributes
                .iter()
                .find(|(k, _)| k.rsplit(':').next() == Some("path"))
                .map(|(_, v)| resolve_path(path, v))
                .transpose()?
                .unwrap_or_else(|| path.into());
            collect_volumes(
                archive,
                files,
                &target,
                parse_id(c, "objectid")?,
                transform * parse_transform(c.attr("transform"))?,
                out,
                depth + 1,
            )?;
        }
    }
    Ok(())
}

impl Project {
    /// 3MF збережено в Bambu Studio (`Application` = «BambuStudio-02.07.01.62»). OrcaSlicer
    /// звіряє цю версію зі своєю внутрішньою `SLIC3R_VERSION` і без `--allow-newer-file`
    /// відмовляється нарізати (−24), хоч формат той самий.
    pub fn from_bambu_studio(&self) -> bool {
        self.metadata
            .get("Application")
            .is_some_and(|a| a.starts_with("BambuStudio"))
    }
}

impl Project {
    /// Bambu range IDs resolve by 1-based model-object order, not root object ID.
    pub fn layer_ranges_for_object(&self, object: crate::ObjectId) -> Option<Vec<&LayerRange>> {
        (self.objects.iter().filter(|o| o.id == object).count() == 1).then(|| {
            self.layer_ranges
                .iter()
                .filter(|r| r.source_object_id == object)
                .collect()
        })
    }

    pub fn object_layer_ranges(&self, object: crate::ObjectId) -> Option<Vec<&LayerRange>> {
        self.layer_ranges_for_object(object)
    }

    pub fn layer_ranges(&self) -> &[LayerRange] {
        &self.layer_ranges
    }

    pub fn set_layer_ranges(
        &mut self,
        object: crate::ObjectId,
        ranges: Vec<LayerRange>,
    ) -> Result<()> {
        self.set_layer_ranges_for_object(object, ranges)
    }

    /// Порожній `layer_config_ranges.xml`, якщо проєкт його ще не має: інакше нові
    /// діапазони не потраплять у збережений 3MF.
    pub(crate) fn ensure_layer_range_source(&mut self) {
        if !self.archive.contains_key(LAYER_RANGES) {
            self.archive.insert(
                LAYER_RANGES.into(),
                std::sync::Arc::from(b"<objects/>".as_slice()),
            );
        }
    }

    /// Replace ranges only when the source ID maps exactly to a project object.
    pub fn set_layer_ranges_for_object(
        &mut self,
        object: crate::ObjectId,
        ranges: Vec<LayerRange>,
    ) -> Result<()> {
        ensure!(
            self.layer_ranges_for_object(object).is_some(),
            "object ID does not map unambiguously to layer range source IDs"
        );
        ensure!(
            self.archive
                .get(LAYER_RANGES)
                .is_some_and(|bytes| parse_layer_ranges(bytes).is_ok()),
            "project has no editable layer range source"
        );
        ensure!(
            ranges
                .iter()
                .all(|r| r.min_z.is_finite() && r.max_z.is_finite() && r.min_z < r.max_z),
            "invalid layer range"
        );
        self.layer_ranges.retain(|r| r.source_object_id != object);
        self.layer_ranges.extend(ranges.into_iter().map(|mut r| {
            r.source_object_id = object;
            r
        }));
        Ok(())
    }

    /// Positional IDs that do not resolve to a loaded model object.
    pub fn unmatched_layer_range_ids(&self) -> Vec<u32> {
        let mut ids: Vec<_> = self
            .unmatched_layer_ranges
            .iter()
            .map(|r| r.source_object_id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Записує через сусідній тимчасовий файл; помилка не псує попереднє збереження.
    pub fn save_3mf(&self, path: impl AsRef<Path>) -> Result<()> {
        self.save_3mf_with(path.as_ref(), ArchiveMode::Durable)
    }

    /// Одноразовий вхід рушія нарізки: той самий вміст, що й [`Self::save_3mf`], але без
    /// стиснення й без `fsync` — рушій прочитає файл одразу й тека видаляється після нарізки.
    /// На пластині 250 тис. △ (20 МБ XML) запис 0,28 → 0,15 с.
    pub fn save_3mf_for_engine(&self, path: impl AsRef<Path>) -> Result<()> {
        self.save_3mf_with(path.as_ref(), ArchiveMode::Scratch)
    }

    fn save_3mf_with(&self, path: &Path, mode: ArchiveMode) -> Result<()> {
        self.validate()?;
        if self.source_fingerprint == Some(self.fingerprint()) {
            return write_archive(path, self.archive.clone(), mode);
        }
        let mut entries = self.archive.clone();
        if let Some(original) = entries.get(LAYER_RANGES) {
            let original_ranges = parse_layer_ranges(original)?;
            // Keep unresolved IDs verbatim. Resolved ranges are serialized against
            // the current exported model-object order, so reorder/edit is safe.
            let mut encoded_ranges: Vec<LayerRange> = self
                .layer_ranges
                .iter()
                .map(|r| {
                    let object_id = self
                        .objects
                        .iter()
                        .position(|o| o.id == r.source_object_id)
                        .map(|i| i as u32 + 1)
                        .unwrap_or(r.source_object_id);
                    let mut r = r.clone();
                    r.source_object_id = object_id;
                    r
                })
                .collect();
            encoded_ranges.extend(self.unmatched_layer_ranges.iter().cloned());
            if original_ranges != encoded_ranges {
                let mut xml = XmlNode::parse(std::str::from_utf8(original)?.into())?;
                for object in xml.children.iter_mut().filter_map(|c| match c {
                    XmlContent::Element(n) if n.local_name() == "object" => Some(n),
                    _ => None,
                }) {
                    let id = object.attr("id").and_then(|s| s.parse::<u32>().ok());
                    let Some(id) = id else { continue };
                    let replacements: Vec<_> = encoded_ranges
                        .iter()
                        .filter(|r| r.source_object_id == id)
                        .collect();
                    if original_ranges
                        .iter()
                        .filter(|r| r.source_object_id == id)
                        .cloned()
                        .collect::<Vec<_>>()
                        == replacements
                            .iter()
                            .map(|r| (*r).clone())
                            .collect::<Vec<_>>()
                    {
                        continue;
                    }
                    object.remove_elements("range");
                    for range in replacements {
                        object.push(layer_range_node(range));
                    }
                }
                let present: std::collections::HashSet<u32> = xml
                    .elements()
                    .filter(|n| n.local_name() == "object")
                    .filter_map(|n| n.attr("id")?.parse().ok())
                    .collect();
                for id in encoded_ranges
                    .iter()
                    .map(|r| r.source_object_id)
                    .collect::<std::collections::HashSet<_>>()
                {
                    if present.contains(&id) {
                        continue;
                    }
                    let mut object = XmlNode::new("object");
                    object.set("id", id);
                    for range in encoded_ranges.iter().filter(|r| r.source_object_id == id) {
                        object.push(layer_range_node(range));
                    }
                    xml.push(object);
                }
                entries.insert(LAYER_RANGES.into(), xml.to_xml().into_bytes().into());
            }
        }
        let mut root = self.root.clone().unwrap_or_else(new_model);
        root.set("xmlns:p", PRODUCTION);
        root.set(
            "xmlns:BambuStudio",
            "http://schemas.bambulab.com/package/2021",
        );
        for (name, value) in &self.metadata {
            let existing = root.children.iter_mut().find_map(|c| match c {
                XmlContent::Element(n)
                    if n.local_name() == "metadata" && n.attr("name") == Some(name) =>
                {
                    Some(n)
                }
                _ => None,
            });
            let escaped: Arc<str> = quick_xml::escape::escape(value).into_owned().into();
            let content = XmlContent::Raw {
                start: 0,
                end: escaped.len(),
                source: escaped,
            };
            if let Some(n) = existing {
                n.children = vec![content];
            } else {
                let mut n = XmlNode::new("metadata");
                n.set("name", name);
                n.children.push(content);
                root.push(n);
            }
        }
        root.children.retain(|c| {
            !matches!(c, XmlContent::Element(n) if n.local_name() == "metadata"
            && n.attr("name").is_some_and(|key| !self.metadata.contains_key(key)))
        });
        if self.root.is_none() && !self.metadata.contains_key("BambuStudio:3mfVersion") {
            let mut version = XmlNode::new("metadata");
            version.set("name", "BambuStudio:3mfVersion");
            version.children.push(XmlContent::Raw {
                source: "1".into(),
                start: 0,
                end: 1,
            });
            root.push(version);
        }
        if root.element("resources").is_none() {
            root.push(XmlNode::new("resources"));
        }
        let original_ids: Vec<_> = root
            .element("build")
            .into_iter()
            .flat_map(|b| b.elements())
            .filter_map(|n| n.attr("objectid").and_then(|s| s.parse::<u32>().ok()))
            .collect();
        let resources = root.element_mut("resources").context("немає resources")?;
        resources.children.retain(|c| !matches!(c, XmlContent::Element(n) if n.local_name() == "object"
            && n.attr("id").and_then(|s| s.parse::<u32>().ok()).is_some_and(|id| original_ids.contains(&id))));
        let mut settings = self
            .settings
            .clone()
            .unwrap_or_else(|| XmlNode::new("config"));
        settings.remove_elements("object");
        settings.remove_elements("plate");
        let mut geometry_paths = Vec::new();
        let mut next_identify_id = 1_u32;
        // Id сіток у 3D/Objects/*.model мають бути унікальні на весь архів, як пише Bambu
        // Studio: рушії зіставляють частини (`part id` у model_settings.config) з сітками лише
        // за id, і однакові id в різних файлах давали всім об’єктам сітку першого.
        let mut used_mesh_ids: std::collections::HashSet<u32> =
            self.objects.iter().map(|o| o.id).collect();
        for object in &self.objects {
            for volume in &object.volumes {
                if let Some(source) = volume.source.as_ref().filter(|s| {
                    s.path != ROOT && mesh_fingerprint(&volume.mesh) == s.fingerprint
                }) {
                    used_mesh_ids.insert(source.id);
                }
            }
        }
        let mut next_mesh_id = 1_u32;
        for object in &self.objects {
            let mut model = object
                .model
                .clone()
                .unwrap_or_else(|| XmlNode::new("object"));
            model.set("id", object.id);
            model.set("type", "model");
            model.set("p:UUID", uuid(object.id, 0, 1));
            let old_components = model.element("components").cloned();
            model.remove_elements("components");
            model.children.retain(|c| {
                !matches!(c, XmlContent::Raw {source, start, end}
                if source[*start..*end].trim_start().starts_with("<mesh"))
            });
            let mut components = XmlNode::new("components");
            let mut meta = object
                .metadata
                .clone()
                .unwrap_or_else(|| XmlNode::new("object"));
            meta.set("id", object.id);
            meta.replace_metadata(&object.config, &["name"]);
            meta.set_metadata("name", &object.name);
            meta.remove_elements("part");
            meta.remove_elements("volume");
            for (v_index, volume) in object.volumes.iter().enumerate() {
                let unchanged = volume
                    .source
                    .as_ref()
                    .filter(|s| s.path != ROOT && mesh_fingerprint(&volume.mesh) == s.fingerprint);
                let (mesh_path, mesh_id) = if let Some(source) = unchanged {
                    (source.path.clone(), source.id)
                } else {
                    let mut serial = 0;
                    let target = loop {
                        let name = format!(
                            "3D/Objects/project_{}_{}_{}.model",
                            object.id, v_index, serial
                        );
                        if !entries.contains_key(&name) {
                            break name;
                        }
                        serial += 1;
                    };
                    while used_mesh_ids.contains(&next_mesh_id) {
                        next_mesh_id += 1;
                    }
                    let id = next_mesh_id;
                    used_mesh_ids.insert(id);
                    entries.insert(
                        target.clone(),
                        mesh_xml(volume, id, &entries)?.into_bytes().into(),
                    );
                    (target, id)
                };
                geometry_paths.push(mesh_path.clone());
                let mut component = old_components
                    .as_ref()
                    .and_then(|c| {
                        c.elements()
                            .filter(|n| n.local_name() == "component")
                            .nth(v_index)
                    })
                    .cloned()
                    .unwrap_or_else(|| XmlNode::new("component"));
                component
                    .attributes
                    .retain(|k, _| k.rsplit(':').next() != Some("path"));
                component.set("p:path", format!("/{mesh_path}"));
                component.set("objectid", mesh_id);
                component.set("p:UUID", uuid(object.id, v_index as u32, 2));
                if component
                    .attr("transform")
                    .map(|s| parse_transform(Some(s)))
                    .transpose()?
                    .unwrap_or(DAffine3::IDENTITY)
                    != volume.transform
                {
                    component.set("transform", transform_string(volume.transform));
                }
                components.push(component);
                let mut part = volume
                    .metadata
                    .clone()
                    .unwrap_or_else(|| XmlNode::new("part"));
                part.name = "part".into();
                part.set("id", mesh_id);
                part.set("subtype", volume.kind.as_str());
                part.attributes.remove("firstid");
                part.attributes.remove("lastid");
                part.replace_metadata(&volume.config, &["name"]);
                part.set_metadata("name", &volume.name);
                for key in ["part_type", "volume_type"] {
                    if volume.config.contains_key(key) {
                        part.set_metadata(key, volume.kind.as_str());
                    }
                }
                meta.push(part);
            }
            model.push(components);
            resources.push(model);
            settings.push(meta);
            for instance in &object.instances {
                if let Some(n) = instance.metadata.as_ref().and_then(|n| {
                    n.metadata()
                        .get("identify_id")
                        .and_then(|v| v.parse::<u32>().ok())
                }) {
                    next_identify_id = next_identify_id.max(n.saturating_add(1));
                }
            }
        }
        let mut build = root
            .element("build")
            .cloned()
            .unwrap_or_else(|| XmlNode::new("build"));
        build.remove_elements("item");
        for object in &self.objects {
            for (i, instance) in object.instances.iter().enumerate() {
                let mut item = instance
                    .metadata
                    .as_ref()
                    .and_then(|n| n.element("build_item"))
                    .cloned()
                    .unwrap_or_else(|| XmlNode::new("item"));
                item.name = "item".into();
                item.set("objectid", object.id);
                item.set("printable", u8::from(instance.printable));
                item.set("p:UUID", uuid(object.id, i as u32, 3));
                let offset = instance
                    .plate
                    .map(|p| self.plate_offset(p))
                    .transpose()?
                    .unwrap_or(DVec3::ZERO);
                let global = DAffine3::from_translation(offset) * instance.transform;
                let original = parse_transform(item.attr("transform"))?;
                let same = original
                    .to_cols_array()
                    .iter()
                    .zip(global.to_cols_array())
                    .all(|(a, b)| (a - b).abs() < 1e-12);
                if !same || item.attr("transform").is_none() {
                    item.set("transform", transform_string(global));
                }
                build.push(item);
            }
        }
        root.remove_elements("build");
        root.push(build);
        let mut used_identify_ids = std::collections::HashSet::new();
        for plate in &self.plates {
            let mut meta = plate
                .metadata
                .clone()
                .unwrap_or_else(|| XmlNode::new("plate"));
            meta.set_metadata("plater_id", plate.index);
            meta.set_metadata("plater_name", &plate.name);
            meta.set_metadata("locked", plate.locked);
            if let Some(bed) = &plate.bed_type {
                meta.set_metadata("bed_type", bed);
            } else {
                meta.children.retain(|c| !matches!(c, XmlContent::Element(n) if n.local_name() == "metadata" && n.attr("key") == Some("bed_type")));
            }
            meta.remove_elements("model_instance");
            for object in &self.objects {
                for (i, instance) in object
                    .instances
                    .iter()
                    .enumerate()
                    .filter(|(_, i)| i.plate == Some(plate.index))
                {
                    let mut m = instance
                        .metadata
                        .clone()
                        .unwrap_or_else(|| XmlNode::new("model_instance"));
                    m.remove_elements("build_item");
                    m.remove_elements("assemble_item");
                    m.set_metadata("object_id", object.id);
                    m.set_metadata("instance_id", i);
                    let identify_id = m
                        .metadata()
                        .get("identify_id")
                        .and_then(|s| s.parse::<u32>().ok())
                        .filter(|id| used_identify_ids.insert(*id));
                    if identify_id.is_none() {
                        ensure!(next_identify_id < u32::MAX, "вичерпано identify_id");
                        m.set_metadata("identify_id", next_identify_id);
                        used_identify_ids.insert(next_identify_id);
                        next_identify_id += 1;
                    }
                    meta.push(m);
                }
            }
            settings.push(meta);
        }
        let mut assemble = settings
            .element("assemble")
            .cloned()
            .unwrap_or_else(|| XmlNode::new("assemble"));
        assemble.remove_elements("assemble_item");
        for object in &self.objects {
            for (index, instance) in object.instances.iter().enumerate() {
                for n in instance
                    .metadata
                    .as_ref()
                    .into_iter()
                    .flat_map(|n| n.elements())
                    .filter(|n| n.local_name() == "assemble_item")
                {
                    let mut n = n.clone();
                    n.name = "assemble_item".into();
                    n.set("object_id", object.id);
                    n.set("instance_id", index);
                    assemble.push(n);
                }
            }
        }
        settings.remove_elements("assemble");
        if !assemble.children.is_empty() {
            settings.push(assemble);
        }
        entries.insert(ROOT.into(), root.to_xml().into_bytes().into());
        entries.insert(SETTINGS.into(), settings.to_xml().into_bytes().into());
        let original_config = entries.get(CONFIG).and_then(|v| {
            serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(v).ok()
        });
        if original_config.as_ref() != Some(&self.config) {
            entries.insert(
                CONFIG.into(),
                serde_json::to_vec_pretty(&self.config)?.into(),
            );
        }
        let mut relationships = entries
            .get("3D/_rels/3dmodel.model.rels")
            .map(|b| -> Result<XmlNode> { XmlNode::parse(std::str::from_utf8(b)?.into()) })
            .transpose()?
            .unwrap_or_else(new_relationships);
        geometry_paths.sort();
        geometry_paths.dedup();
        for (i, path) in geometry_paths.iter().enumerate() {
            if !relationships
                .elements()
                .any(|r| r.attr("Target") == Some(&format!("/{path}")))
            {
                let mut r = XmlNode::new("Relationship");
                r.set("Id", format!("project-{i}"));
                r.set("Target", format!("/{path}"));
                r.set(
                    "Type",
                    "http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel",
                );
                relationships.push(r);
            }
        }
        entries.insert(
            "3D/_rels/3dmodel.model.rels".into(),
            relationships.to_xml().into_bytes().into(),
        );
        entries.entry("_rels/.rels".into()).or_insert_with(|| {
            let mut rel = new_relationships();
            let mut r = XmlNode::new("Relationship");
            r.set("Id", "rel0");
            r.set("Target", format!("/{ROOT}"));
            r.set(
                "Type",
                "http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel",
            );
            rel.push(r);
            rel.to_xml().into_bytes().into()
        });
        entries.entry("[Content_Types].xml".into()).or_insert_with(|| Arc::from(br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="config" ContentType="application/octet-stream"/></Types>"#.as_slice()));
        write_archive(path, entries, mode)
    }

    pub fn validate(&self) -> Result<()> {
        let mut ids = std::collections::HashSet::new();
        for plate in &self.plates {
            ensure!(
                plate.index > 0 && ids.insert(plate.index),
                "повторений індекс пластини"
            );
        }
        ensure!(
            self.plates
                .iter()
                .enumerate()
                .all(|(i, p)| p.index == i as u32 + 1),
            "індекси пластин мають бути послідовними з 1"
        );
        let plate_ids = ids;
        let mut ids = std::collections::HashSet::new();
        for object in &self.objects {
            ensure!(
                object.id > 0 && ids.insert(object.id),
                "повторений ID обʼєкта"
            );
            ensure!(!object.volumes.is_empty(), "обʼєкт без частин");
            ensure!(!object.instances.is_empty(), "обʼєкт без інстансів");
            for volume in &object.volumes {
                valid_transform(volume.transform)?;
                ensure!(
                    volume
                        .mesh
                        .positions
                        .iter()
                        .flatten()
                        .all(|n| n.is_finite()),
                    "нескінченна вершина"
                );
                ensure!(
                    volume
                        .mesh
                        .triangles
                        .iter()
                        .flatten()
                        .all(|&i| (i as usize) < volume.mesh.positions.len()),
                    "індекс вершини поза сіткою"
                );
            }
            for instance in &object.instances {
                valid_transform(instance.transform)?;
                ensure!(
                    instance.plate.is_none_or(|p| plate_ids.contains(&p)),
                    "інстанс на відсутній пластині"
                );
            }
        }
        Ok(())
    }
}

fn valid_transform(t: DAffine3) -> Result<()> {
    ensure!(
        t.is_finite()
            && DMat3::from_cols(t.matrix3.x_axis, t.matrix3.y_axis, t.matrix3.z_axis)
                .determinant()
                .abs()
                > 1e-18,
        "вироджена трансформа"
    );
    Ok(())
}
fn uuid(object: u32, index: u32, kind: u32) -> String {
    format!("{object:08x}-{kind:04x}-4000-8000-{index:012x}")
}
fn new_model() -> XmlNode {
    let mut model = XmlNode::new("model");
    model.set("unit", "millimeter");
    model.set("xmlns", CORE);
    model.set("xmlns:p", PRODUCTION);
    model.set("requiredextensions", "p");
    let mut m = XmlNode::new("metadata");
    m.set("name", "BambuStudio:3mfVersion");
    m.children.push(XmlContent::Raw {
        source: "1".into(),
        start: 0,
        end: 1,
    });
    model.push(m);
    model
}
fn new_relationships() -> XmlNode {
    let mut r = XmlNode::new("Relationships");
    r.set(
        "xmlns",
        "http://schemas.openxmlformats.org/package/2006/relationships",
    );
    r
}
fn mesh_xml(volume: &Volume, id: u32, archive: &BTreeMap<String, Arc<[u8]>>) -> Result<String> {
    use std::fmt::Write as _;
    let mut model = new_model();
    if let Some(source) = &volume.source
        && let Some(bytes) = archive.get(&source.path)
    {
        let original = XmlNode::parse(std::str::from_utf8(bytes)?.into())?;
        for (key, value) in original.attributes {
            if key.starts_with("xmlns") {
                model.set(&key, value);
            }
        }
    }
    let mut out = model.to_xml();
    out.truncate(out.len() - "</model>".len());
    write!(
        out,
        "<resources><object id=\"{id}\" type=\"model\"><mesh><vertices>"
    )?;
    for [x, y, z] in &volume.mesh.positions {
        write!(out, "<vertex x=\"{x}\" y=\"{y}\" z=\"{z}\"/>")?;
    }
    out.push_str("</vertices><triangles>");
    for (i, [a, b, c]) in volume.mesh.triangles.iter().enumerate() {
        write!(out, "<triangle v1=\"{a}\" v2=\"{b}\" v3=\"{c}\"")?;
        if let Some(attrs) = volume.mesh.triangle_attributes.get(&i) {
            for (key, value) in attrs {
                write!(out, " {key}=\"{}\"", quick_xml::escape::escape(value))?;
            }
        }
        out.push_str("/>");
    }
    out.push_str("</triangles></mesh></object></resources></model>");
    Ok(out)
}

/// Як писати архів: збереження користувача — стиснуте й з `fsync`; вхід рушія — як є.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ArchiveMode {
    Durable,
    Scratch,
}

fn write_archive(path: &Path, entries: BTreeMap<String, Arc<[u8]>>, mode: ArchiveMode) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temp = None;
    for serial in 0..1000 {
        let candidate = parent.join(format!(".project-{}-{serial}.3mf.tmp", std::process::id()));
        match std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&candidate)
        {
            Ok(file) => {
                temp = Some((candidate, file));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    let (temp_path, file) = temp.context("не вдалося створити тимчасовий файл")?;
    let result = (|| -> Result<()> {
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(match mode {
            ArchiveMode::Durable => zip::CompressionMethod::Deflated,
            ArchiveMode::Scratch => zip::CompressionMethod::Stored,
        });
        for (name, bytes) in entries {
            zip.start_file(name, options)?;
            zip.write_all(&bytes)?;
        }
        let file = zip.finish()?;
        if mode == ArchiveMode::Durable {
            file.sync_all()?;
        }
        std::fs::rename(&temp_path, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

impl Project {
    fn fingerprint(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.metadata.hash(&mut h);
        for range in &self.layer_ranges {
            range.source_object_id.hash(&mut h);
            range.min_z.to_bits().hash(&mut h);
            range.max_z.to_bits().hash(&mut h);
            range.config.hash(&mut h);
            if let Some(node) = &range.metadata {
                hash_xml(node, &mut h);
            }
        }
        serde_json::to_string(&self.config)
            .expect("JSON map")
            .hash(&mut h);
        for node in [&self.root, &self.settings].into_iter().flatten() {
            hash_xml(node, &mut h);
        }
        self.objects.len().hash(&mut h);
        for o in &self.objects {
            o.id.hash(&mut h);
            o.name.hash(&mut h);
            o.config.hash(&mut h);
            for n in [&o.model, &o.metadata].into_iter().flatten() {
                hash_xml(n, &mut h);
            }
            o.volumes.len().hash(&mut h);
            for v in &o.volumes {
                v.name.hash(&mut h);
                v.config.hash(&mut h);
                v.kind.as_str().hash(&mut h);
                mesh_fingerprint(&v.mesh).hash(&mut h);
                for x in v.transform.to_cols_array() {
                    x.to_bits().hash(&mut h);
                }
                if let Some(n) = &v.metadata {
                    hash_xml(n, &mut h);
                }
            }
            o.instances.len().hash(&mut h);
            for i in &o.instances {
                i.plate.hash(&mut h);
                i.printable.hash(&mut h);
                for x in i.transform.to_cols_array() {
                    x.to_bits().hash(&mut h);
                }
                if let Some(n) = &i.metadata {
                    hash_xml(n, &mut h);
                }
            }
        }
        self.plates.len().hash(&mut h);
        for p in &self.plates {
            p.index.hash(&mut h);
            p.name.hash(&mut h);
            p.bed_type.hash(&mut h);
            p.locked.hash(&mut h);
            if let Some(n) = &p.metadata {
                hash_xml(n, &mut h);
            }
        }
        h.finish()
    }
}
fn hash_xml(n: &XmlNode, h: &mut impl Hasher) {
    n.name.hash(h);
    n.attributes.hash(h);
    n.children.len().hash(h);
    for c in &n.children {
        match c {
            XmlContent::Element(n) => {
                0_u8.hash(h);
                hash_xml(n, h);
            }
            XmlContent::Raw { source, start, end } => {
                1_u8.hash(h);
                source[*start..*end].hash(h);
            }
        }
    }
}
