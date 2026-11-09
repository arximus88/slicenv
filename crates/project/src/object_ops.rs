//! Дії контекстного меню об’єкта, яких немає деінде: позначка «друкувати», перетворення
//! одиниць, додавання частин-примітивів, розділення сітки на частини, заповнення пластини
//! копіями й новий діапазон висот шарів. Кожна дія атомарна: при помилці проєкт не змінюється.

use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result, bail, ensure};

use crate::{
    DAffine3, DVec3, IndexedMesh, InstanceRef, LayerRange, ObjectId, Project, Volume, VolumeKind,
    add::SOURCE_FILE,
};

/// Міліметрів у дюймі.
pub const INCH: f64 = 25.4;

impl Project {
    pub fn set_printable(&mut self, key: InstanceRef, printable: bool) -> Result<()> {
        self.instance_mut(key)?.printable = printable;
        Ok(())
    }

    /// Рівномірний масштаб `factor` навколо центру рамки, потім — на стіл. Для «з дюймів»
    /// це [`INCH`], для «з метричної» — `1 / INCH`.
    pub fn convert_units(&mut self, key: InstanceRef, factor: f64) -> Result<()> {
        ensure!(factor.is_finite() && factor > 0.0, "некоректний множник");
        let mut staged = self.clone();
        staged.scale(key, DVec3::splat(factor))?;
        staged.drop_to_bed(key)?;
        *self = staged;
        Ok(())
    }

    /// Додає до об’єкта частину-примітив ролі `kind`. Сітка в світі має той самий розмір,
    /// що й `mesh` (масштаб інстансу компенсується), а центр її рамки — у центрі рамки
    /// інстансу `key`. Повертає індекс нової частини.
    pub fn add_primitive_volume(
        &mut self,
        key: InstanceRef,
        kind: VolumeKind,
        name: &str,
        mesh: IndexedMesh,
    ) -> Result<usize> {
        ensure!(!mesh.triangles.is_empty(), "порожня сітка");
        let target = self
            .instance_bounds(key)?
            .context("порожня сітка")?
            .center();
        let own = mesh.bounds().context("порожня сітка")?.center();
        let instance = self.instance(key)?.transform;
        ensure!(
            instance.matrix3.determinant().abs() > 1e-12,
            "вироджена трансформація"
        );
        let transform = instance.inverse() * DAffine3::from_translation(target - own);
        let object = self.object_mut(key.object)?;
        object.volumes.push(Volume {
            name: name.to_owned(),
            mesh: mesh.into(),
            transform,
            kind,
            ..Volume::default()
        });
        Ok(object.volumes.len() - 1)
    }

    /// Розбиває кожну деталь об’єкта на зв’язні шматки сітки (як «Розділити на частини» у
    /// Bambu Studio). Модифікатори й інші ролі лишаються як є. Повертає кількість деталей після.
    pub fn split_into_parts(&mut self, object: ObjectId) -> Result<usize> {
        let target = self.object_mut(object)?;
        let mut volumes = Vec::with_capacity(target.volumes.len());
        let mut split_any = false;
        for volume in std::mem::take(&mut target.volumes) {
            if volume.kind != VolumeKind::Part {
                volumes.push(volume);
                continue;
            }
            let pieces = connected_components(&volume.mesh);
            if pieces.len() < 2 {
                volumes.push(volume);
                continue;
            }
            split_any = true;
            let base = volume.name.clone();
            for (i, mesh) in pieces.into_iter().enumerate() {
                let mut config = volume.config.clone();
                // Шматок уже не відповідає файлу: перезавантаження замінило б його цілим.
                config.remove(SOURCE_FILE);
                volumes.push(Volume {
                    name: if base.is_empty() {
                        format!("{}", i + 1)
                    } else {
                        format!("{base} {}", i + 1)
                    },
                    mesh: mesh.into(),
                    transform: volume.transform,
                    kind: VolumeKind::Part,
                    config,
                    metadata: None,
                    source: None,
                });
            }
        }
        let parts = volumes
            .iter()
            .filter(|v| v.kind == VolumeKind::Part)
            .count();
        target.volumes = volumes;
        if !split_any {
            bail!("модель суцільна — немає окремих шматків");
        }
        Ok(parts)
    }

    /// Додає копії інстансу на його пластину, доки вони вміщаються (не більше `limit`).
    /// Оригінал і копії розкладаються сіткою, інші об’єкти пластини не рухаються.
    /// Повертає оригінал і всі копії.
    pub fn fill_plate_with_copies(
        &mut self,
        key: InstanceRef,
        spacing: f64,
        lo: [f64; 2],
        hi: [f64; 2],
        limit: usize,
    ) -> Result<Vec<InstanceRef>> {
        let plate = self.instance(key)?.plate.context("інстанс поза пластиною")?;
        ensure!(!self.plate_locked(plate), "пластина заблокована");
        let mut staged = self.clone();
        let mut keys = vec![key];
        for _ in 0..limit {
            let mut attempt = staged.clone();
            let copy = attempt.duplicate_instance(key)?;
            let mut next = keys.clone();
            next.push(copy);
            if attempt
                .arrange_instances_on_bed(plate, &next, spacing, lo, hi)
                .is_err()
            {
                break;
            }
            staged = attempt;
            keys = next;
        }
        ensure!(keys.len() > 1, "на пластині немає місця для копії");
        *self = staged;
        Ok(keys)
    }

    /// Чи можна додати об’єкту діапазон висот шарів.
    pub fn can_add_layer_range(&self, object: ObjectId) -> bool {
        self.layer_ranges_for_object(object).is_some()
    }

    /// Новий діапазон висот 2 мм над останнім наявним (або від столу) з висотою шару
    /// об’єкта / проєкту. Повертає індекс діапазону серед діапазонів об’єкта.
    pub fn add_layer_range(&mut self, object: ObjectId) -> Result<usize> {
        let mut ranges: Vec<LayerRange> = self
            .layer_ranges_for_object(object)
            .context("діапазони шарів недоступні для цього об’єкта")?
            .into_iter()
            .cloned()
            .collect();
        let start = ranges.iter().map(|r| r.max_z).fold(0.0, f64::max);
        let project_height = match self.config.get("layer_height") {
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(serde_json::Value::Array(a)) => a.first().and_then(|v| v.as_str()).map(str::to_owned),
            Some(serde_json::Value::Number(n)) => Some(n.to_string()),
            _ => None,
        };
        let height = self
            .object(object)?
            .config
            .get("layer_height")
            .cloned()
            .or(project_height)
            .unwrap_or_else(|| "0.2".to_owned());
        let mut config = crate::Config::new();
        config.insert("layer_height".into(), height);
        ranges.push(LayerRange {
            source_object_id: object,
            metadata: None,
            min_z: start,
            max_z: start + 2.0,
            config,
        });
        let index = ranges.len() - 1;
        let mut staged = self.clone();
        staged.ensure_layer_range_source();
        staged.set_layer_ranges_for_object(object, ranges)?;
        *self = staged;
        Ok(index)
    }
}

/// Розбиває сітку на зв’язні шматки. Вершини з однаковими координатами вважаються однією
/// (STL часто дублює вершини). Атрибути граней переносяться з новими індексами.
pub fn connected_components(mesh: &IndexedMesh) -> Vec<IndexedMesh> {
    let n = mesh.positions.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let union = |parent: &mut Vec<usize>, a: usize, b: usize| {
        let (ra, rb) = (find(parent, a), find(parent, b));
        if ra != rb {
            parent[ra] = rb;
        }
    };
    let mut seen: HashMap<[u64; 3], usize> = HashMap::new();
    for (i, p) in mesh.positions.iter().enumerate() {
        let bits = p.map(|v| (v + 0.0).to_bits());
        let first = *seen.entry(bits).or_insert(i);
        union(&mut parent, i, first);
    }
    for t in &mesh.triangles {
        let [a, b, c] = t.map(|i| i as usize);
        if a < n && b < n && c < n {
            union(&mut parent, a, b);
            union(&mut parent, b, c);
        }
    }
    // Шматки в порядку першої появи трикутника.
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut order: HashMap<usize, usize> = HashMap::new();
    for (ti, t) in mesh.triangles.iter().enumerate() {
        let Some(&v) = t.first().filter(|v| (**v as usize) < n) else {
            continue;
        };
        let root = find(&mut parent, v as usize);
        let next = order.len();
        let slot = *order.entry(root).or_insert(next);
        groups.entry(slot).or_default().push(ti);
    }
    groups
        .into_values()
        .map(|triangles| {
            let mut remap: HashMap<u32, u32> = HashMap::new();
            let mut out = IndexedMesh::default();
            for (new_index, &ti) in triangles.iter().enumerate() {
                let t = mesh.triangles[ti].map(|v| {
                    *remap.entry(v).or_insert_with(|| {
                        out.positions.push(mesh.positions[v as usize]);
                        (out.positions.len() - 1) as u32
                    })
                });
                out.triangles.push(t);
                if let Some(attr) = mesh.triangle_attributes.get(&ti) {
                    out.triangle_attributes.insert(new_index, attr.clone());
                }
            }
            out
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project_with(mesh: IndexedMesh) -> (Project, InstanceRef) {
        let mut p = Project::default();
        p.add_plate("").unwrap();
        let key = p
            .add_mesh_object("obj", mesh, 1, [128., 128.], [0., 0.], [256., 256.])
            .unwrap();
        (p, key)
    }

    fn two_cubes() -> IndexedMesh {
        let mut a = IndexedMesh::cuboid(DVec3::splat(10.0));
        let b = IndexedMesh::cuboid(DVec3::splat(10.0));
        let offset = a.positions.len() as u32;
        a.positions
            .extend(b.positions.iter().map(|p| [p[0] + 30.0, p[1], p[2]]));
        a.triangles
            .extend(b.triangles.iter().map(|t| t.map(|i| i + offset)));
        a
    }

    #[test]
    fn inch_conversion_scales_by_25_4_and_stays_on_bed() {
        let (mut p, key) = project_with(IndexedMesh::cuboid(DVec3::splat(1.0)));
        p.convert_units(key, INCH).unwrap();
        let b = p.instance_bounds(key).unwrap().unwrap();
        assert!((b.size() - DVec3::splat(25.4)).abs().max_element() < 1e-9);
        assert!(b.min.z.abs() < 1e-9);
        p.convert_units(key, 1.0 / INCH).unwrap();
        let b = p.instance_bounds(key).unwrap().unwrap();
        assert!((b.size() - DVec3::splat(1.0)).abs().max_element() < 1e-9);
        assert!(p.convert_units(key, 0.0).is_err());
    }

    #[test]
    fn printable_flag_toggles() {
        let (mut p, key) = project_with(IndexedMesh::cuboid(DVec3::splat(5.0)));
        p.set_printable(key, false).unwrap();
        assert!(!p.instance(key).unwrap().printable);
        p.set_printable(key, true).unwrap();
        assert!(p.instance(key).unwrap().printable);
    }

    #[test]
    fn primitive_volume_is_centered_and_keeps_world_size_under_instance_scale() {
        let (mut p, key) = project_with(IndexedMesh::cuboid(DVec3::splat(40.0)));
        p.scale(key, DVec3::splat(2.0)).unwrap();
        p.drop_to_bed(key).unwrap();
        let host = p.instance_bounds(key).unwrap().unwrap();
        let index = p
            .add_primitive_volume(
                key,
                VolumeKind::Modifier,
                "m",
                crate::Primitive::Cube.mesh(),
            )
            .unwrap();
        let object = p.object(key.object).unwrap();
        assert_eq!(object.volumes[index].kind, VolumeKind::Modifier);
        let world = p.instance(key).unwrap().transform * object.volumes[index].transform;
        let mesh = &object.volumes[index].mesh;
        let (mut lo, mut hi) = (DVec3::MAX, DVec3::MIN);
        for q in &mesh.positions {
            let w = world.transform_point3(DVec3::from(*q));
            lo = lo.min(w);
            hi = hi.max(w);
        }
        assert!(((hi - lo) - DVec3::splat(20.0)).abs().max_element() < 1e-9);
        assert!((((hi + lo) / 2.0) - host.center()).abs().max_element() < 1e-9);
        // Модифікатор не змінює рамку деталей.
        assert_eq!(p.instance_bounds(key).unwrap().unwrap(), host);
    }

    #[test]
    fn split_into_parts_finds_disjoint_pieces() {
        let (mut p, key) = project_with(two_cubes());
        assert_eq!(p.split_into_parts(key.object).unwrap(), 2);
        let object = p.object(key.object).unwrap();
        assert_eq!(object.volumes.len(), 2);
        assert!(object.volumes.iter().all(|v| v.mesh.triangles.len() == 12));
        // Суцільна модель — помилка, проєкт не змінюється.
        let (mut solid, key) = project_with(IndexedMesh::cuboid(DVec3::splat(5.0)));
        let before = solid.clone();
        assert!(solid.split_into_parts(key.object).is_err());
        assert_eq!(solid, before);
    }

    #[test]
    fn components_weld_duplicated_vertices() {
        // Два трикутники зі спільним ребром, але окремими (дубльованими) вершинами.
        let mesh = IndexedMesh {
            positions: vec![
                [0., 0., 0.],
                [1., 0., 0.],
                [0., 1., 0.],
                [1., 0., 0.],
                [0., 1., 0.],
                [1., 1., 0.],
            ],
            triangles: vec![[0, 1, 2], [3, 5, 4]],
            ..Default::default()
        };
        assert_eq!(connected_components(&mesh).len(), 1);
    }

    #[test]
    fn fill_plate_adds_copies_until_full() {
        let (mut p, key) = project_with(IndexedMesh::cuboid(DVec3::splat(50.0)));
        let keys = p
            .fill_plate_with_copies(key, 6.0, [0., 0.], [256., 256.], 100)
            .unwrap();
        // 50 мм + 6 мм проміжку: 4 × 4 на столі 256 мм.
        assert_eq!(keys.len(), 16);
        assert_eq!(p.object(key.object).unwrap().instances.len(), 16);
        let full = p.clone();
        assert!(
            p.fill_plate_with_copies(key, 6.0, [0., 0.], [256., 256.], 100)
                .is_err()
        );
        assert_eq!(p, full);
    }

    #[test]
    fn layer_range_is_added_above_existing() {
        let (mut p, key) = project_with(IndexedMesh::cuboid(DVec3::splat(10.0)));
        assert!(p.can_add_layer_range(key.object));
        assert_eq!(p.add_layer_range(key.object).unwrap(), 0);
        assert_eq!(p.add_layer_range(key.object).unwrap(), 1);
        let ranges = p.object_layer_ranges(key.object).unwrap();
        assert_eq!((ranges[1].min_z, ranges[1].max_z), (2.0, 4.0));
        assert_eq!(ranges[0].config.get("layer_height").unwrap(), "0.2");
    }
}
