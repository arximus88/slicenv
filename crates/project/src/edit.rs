use crate::{
    BED_SIZE, DAffine3, DQuat, DVec3, InstanceRef, Object, ObjectId, Plate, Project, VolumeKind,
};
use anyhow::{Result, bail, ensure};

impl Project {
    /// Merge placed instances into one multi-volume object. Volume transforms are
    /// rebased to plate-local coordinates so every source volume stays in place.
    /// The operation is atomic and consumes only the selected instances.
    pub fn merge_instances(&mut self, selected: &[InstanceRef]) -> Result<InstanceRef> {
        let mut staged = self.clone();
        let merged = staged.merge_instances_inner(selected)?;
        *self = staged;
        Ok(merged)
    }

    fn merge_instances_inner(&mut self, selected: &[InstanceRef]) -> Result<InstanceRef> {
        ensure!(
            selected.len() >= 2,
            "потрібно вибрати щонайменше два інстанси"
        );
        let mut refs = selected.to_vec();
        refs.sort();
        refs.dedup();
        ensure!(refs.len() == selected.len(), "інстанс вибрано повторно");
        let first = self.instance(refs[0])?.clone();
        ensure!(first.plate.is_some(), "інстанс поза пластиною");
        let plate = first.plate;
        let mut volumes = Vec::new();
        let mut members = Vec::new();
        let first_config = self.object(refs[0].object)?.config.clone();
        for key in &refs {
            let object = self.object(key.object)?;
            ensure!(
                object.config == first_config,
                "не можна злити обʼєкти з різними налаштуваннями обʼєкта"
            );
            ensure!(
                self.object_layer_ranges(key.object)
                    .is_none_or(|r| r.is_empty()),
                "не можна злити обʼєкт із діапазонами шарів"
            );
            let instance = self.instance(*key)?;
            ensure!(
                instance.plate == plate,
                "усі інстанси мають бути на одній пластині"
            );
            ensure!(
                instance.printable == first.printable,
                "усі інстанси мають мати однаковий стан друку"
            );
            for volume in &object.volumes {
                let mut volume = volume.clone();
                volume.transform = instance.transform * volume.transform;
                // The baked transform no longer corresponds to the external source mesh.
                volume.source = None;
                volumes.push(volume);
            }
            members.push((*key, instance.clone()));
        }
        let id = self
            .objects
            .iter()
            .map(|o| o.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("вичерпано ID"))?;
        let name = members
            .iter()
            .map(|(k, _)| self.object(k.object).map(|o| o.name.as_str()))
            .collect::<Result<Vec<_>>>()?
            .join(" + ");
        self.objects.push(Object {
            id,
            name,
            volumes,
            config: first_config,
            instances: vec![crate::Instance {
                transform: DAffine3::IDENTITY,
                plate,
                printable: first.printable,
                metadata: None,
            }],
            ..Object::default()
        });
        let new_key = InstanceRef {
            object: id,
            instance: 0,
        };
        // Remove in descending order per object so indexes remain valid.
        let mut removals: Vec<_> = refs.into_iter().collect();
        removals.sort_by(|a, b| a.object.cmp(&b.object).then(b.instance.cmp(&a.instance)));
        for key in removals {
            self.objects
                .iter_mut()
                .find(|o| o.id == key.object)
                .unwrap()
                .instances
                .remove(key.instance);
        }
        self.objects
            .retain(|o| !o.instances.is_empty() || o.id == id);
        self.sync_plates();
        Ok(new_key)
    }

    /// Split an object instance into one object per volume, retaining each volume's
    /// kind, config, mesh, and transform. Returns the new instance references.
    pub fn split_instance(&mut self, key: InstanceRef) -> Result<Vec<InstanceRef>> {
        let mut staged = self.clone();
        let result = staged.split_instance_inner(key)?;
        *self = staged;
        Ok(result)
    }

    fn split_instance_inner(&mut self, key: InstanceRef) -> Result<Vec<InstanceRef>> {
        let source = self.object(key.object)?.clone();
        ensure!(
            self.object_layer_ranges(key.object)
                .is_none_or(|r| r.is_empty()),
            "не можна розділити обʼєкт із діапазонами шарів"
        );
        ensure!(source.volumes.len() >= 2, "обʼєкт уже має одну частину");
        let instance = self.instance(key)?.clone();
        let mut ids: Vec<ObjectId> = Vec::new();
        let mut next = self.objects.iter().map(|o| o.id).max().unwrap_or(0);
        for volume in source.volumes {
            next = next
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("вичерпано ID"))?;
            ids.push(next);
            let name = if volume.name.is_empty() {
                source.name.clone()
            } else {
                volume.name.clone()
            };
            self.objects.push(Object {
                id: next,
                name,
                volumes: vec![volume],
                instances: vec![instance.clone()],
                config: source.config.clone(),
                ..Object::default()
            });
        }
        self.objects
            .iter_mut()
            .find(|o| o.id == key.object)
            .unwrap()
            .instances
            .remove(key.instance);
        self.objects.retain(|o| !o.instances.is_empty());
        self.sync_plates();
        Ok(ids
            .into_iter()
            .map(|object| InstanceRef {
                object,
                instance: 0,
            })
            .collect())
    }

    /// Set the role of a single volume, preserving its mesh and scoped config.
    pub fn set_volume_kind(
        &mut self,
        object: ObjectId,
        volume: usize,
        kind: VolumeKind,
    ) -> Result<()> {
        let target = self
            .objects
            .iter_mut()
            .find(|o| o.id == object)
            .ok_or_else(|| anyhow::anyhow!("немає обʼєкта {object}"))?;
        ensure!(volume < target.volumes.len(), "немає частини {volume}");
        ensure!(
            kind == VolumeKind::Part
                || target
                    .volumes
                    .iter()
                    .enumerate()
                    .any(|(i, v)| i != volume && v.kind == VolumeKind::Part),
            "об’єкт повинен мати щонайменше одну позитивну деталь"
        );
        target.volumes[volume].kind = kind;
        Ok(())
    }

    /// Replace configuration scoped to an object or volume.
    pub fn set_object_config(&mut self, object: ObjectId, config: crate::Config) -> Result<()> {
        self.objects
            .iter_mut()
            .find(|o| o.id == object)
            .ok_or_else(|| anyhow::anyhow!("немає обʼєкта {object}"))?
            .config = config;
        Ok(())
    }

    pub fn set_volume_config(
        &mut self,
        object: ObjectId,
        volume: usize,
        config: crate::Config,
    ) -> Result<()> {
        let target = self
            .objects
            .iter_mut()
            .find(|o| o.id == object)
            .ok_or_else(|| anyhow::anyhow!("немає обʼєкта {object}"))?;
        target
            .volumes
            .get_mut(volume)
            .ok_or_else(|| anyhow::anyhow!("немає частини {volume}"))?
            .config = config;
        Ok(())
    }

    pub fn translate(&mut self, key: InstanceRef, delta: DVec3) -> Result<()> {
        ensure!(delta.is_finite(), "зсув має бути скінченним");
        let instance = self.instance_mut(key)?;
        let transform = DAffine3::from_translation(delta) * instance.transform;
        validate_transform(transform)?;
        instance.transform = transform;
        Ok(())
    }

    /// Поворот навколо центру світового bounding box; кут задає quaternion.
    pub fn rotate(&mut self, key: InstanceRef, rotation: DQuat) -> Result<()> {
        ensure!(
            rotation.is_finite() && rotation.length().is_finite() && rotation.length() > 1e-12,
            "некоректний поворот"
        );
        self.around_center(key, DAffine3::from_quat(rotation.normalize()))
    }

    /// Масштаб уздовж світових осей навколо центру bounding box.
    pub fn scale(&mut self, key: InstanceRef, factors: DVec3) -> Result<()> {
        ensure!(
            factors.is_finite() && factors.abs().min_element() > 1e-12,
            "нульовий масштаб"
        );
        self.around_center(key, DAffine3::from_scale(factors))
    }

    /// Масштаб уздовж власних осей інстансу (не світових); центр bounding box лишається на місці.
    pub fn scale_along_axes(&mut self, key: InstanceRef, factors: DVec3) -> Result<()> {
        ensure!(
            factors.is_finite() && factors.abs().min_element() > 1e-12,
            "нульовий масштаб"
        );
        let center = self
            .instance_bounds(key)?
            .ok_or_else(|| anyhow::anyhow!("порожня сітка"))?
            .center();
        let mut staged = self.clone();
        let instance = staged.instance_mut(key)?;
        let transform = instance.transform * DAffine3::from_scale(factors);
        validate_transform(transform)?;
        instance.transform = transform;
        let moved = staged
            .instance_bounds(key)?
            .ok_or_else(|| anyhow::anyhow!("порожня сітка"))?
            .center();
        staged.translate(key, center - moved)?;
        *self = staged;
        Ok(())
    }

    fn around_center(&mut self, key: InstanceRef, change: DAffine3) -> Result<()> {
        let center = self
            .instance_bounds(key)?
            .ok_or_else(|| anyhow::anyhow!("порожня сітка"))?
            .center();
        let instance = self.instance_mut(key)?;
        let transform = DAffine3::from_translation(center)
            * change
            * DAffine3::from_translation(-center)
            * instance.transform;
        validate_transform(transform)?;
        instance.transform = transform;
        Ok(())
    }

    pub fn drop_to_bed(&mut self, key: InstanceRef) -> Result<()> {
        let min = self
            .instance_bounds(key)?
            .ok_or_else(|| anyhow::anyhow!("порожня сітка"))?
            .min;
        self.translate(key, DVec3::new(0.0, 0.0, -min.z))
    }

    pub fn center_on_plate(&mut self, key: InstanceRef) -> Result<()> {
        self.center_on_bed(key, [0.0; 2], [BED_SIZE; 2])
    }

    pub fn center_on_bed(&mut self, key: InstanceRef, lo: [f64; 2], hi: [f64; 2]) -> Result<()> {
        ensure!(
            lo.iter().chain(&hi).all(|v| v.is_finite()) && hi[0] > lo[0] && hi[1] > lo[1],
            "некоректний стіл"
        );
        ensure!(
            self.instance(key)?.plate.is_some(),
            "інстанс поза пластиною"
        );
        let center = self
            .instance_bounds(key)?
            .ok_or_else(|| anyhow::anyhow!("порожня сітка"))?
            .center();
        self.translate(
            key,
            DVec3::new(
                (lo[0] + hi[0]) * 0.5 - center.x,
                (lo[1] + hi[1]) * 0.5 - center.y,
                0.0,
            ),
        )
    }

    pub fn lay_on_face(&mut self, key: InstanceRef, volume: usize, triangle: usize) -> Result<()> {
        let object = self.object(key.object)?;
        let volume = object
            .volumes
            .get(volume)
            .ok_or_else(|| anyhow::anyhow!("немає частини"))?;
        let triangle = volume
            .mesh
            .triangles
            .get(triangle)
            .ok_or_else(|| anyhow::anyhow!("немає грані"))?;
        let transform = self.instance(key)?.transform * volume.transform;
        let mut p = [DVec3::ZERO; 3];
        for (j, &i) in triangle.iter().enumerate() {
            let v = volume
                .mesh
                .positions
                .get(i as usize)
                .ok_or_else(|| anyhow::anyhow!("некоректна грань"))?;
            p[j] = transform.transform_point3(DVec3::from(*v));
        }
        let normal = (p[1] - p[0]).cross(p[2] - p[0]);
        ensure!(normal.length_squared() > 1e-24, "вироджена грань");
        self.lay_on_normal(key, normal)
    }

    /// Повертає інстанс так, щоб грань із зовнішньою світовою нормаллю `normal` лягла на стіл.
    pub fn lay_on_normal(&mut self, key: InstanceRef, normal: DVec3) -> Result<()> {
        ensure!(
            normal.is_finite() && normal.length_squared() > 1e-24,
            "вироджена грань"
        );
        self.rotate(key, DQuat::from_rotation_arc(normal.normalize(), -DVec3::Z))?;
        self.drop_to_bed(key)
    }

    pub fn duplicate_instance(&mut self, key: InstanceRef) -> Result<InstanceRef> {
        let instance = self.instance(key)?.clone();
        let object = self
            .objects
            .iter_mut()
            .find(|o| o.id == key.object)
            .expect("перевірено вище");
        let key = InstanceRef {
            object: key.object,
            instance: object.instances.len(),
        };
        object.instances.push(instance);
        self.sync_plates();
        Ok(key)
    }

    pub fn delete_instance(&mut self, key: InstanceRef) -> Result<()> {
        self.instance(key)?;
        self.objects
            .iter_mut()
            .find(|o| o.id == key.object)
            .expect("перевірено вище")
            .instances
            .remove(key.instance);
        self.objects.retain(|o| !o.instances.is_empty());
        self.layer_ranges
            .retain(|r| self.objects.iter().any(|o| o.id == r.source_object_id));
        self.sync_plates();
        Ok(())
    }

    pub fn duplicate_object(&mut self, id: ObjectId) -> Result<ObjectId> {
        let mut object = self.object(id)?.clone();
        let ranges = self
            .layer_ranges
            .iter()
            .filter(|r| r.source_object_id == id)
            .cloned()
            .collect::<Vec<_>>();
        object.id = self
            .objects
            .iter()
            .map(|o| o.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("вичерпано ID"))?;
        let id = object.id;
        self.objects.push(object);
        self.layer_ranges.extend(ranges.into_iter().map(|mut r| {
            r.source_object_id = id;
            r
        }));
        self.sync_plates();
        Ok(id)
    }

    pub fn delete_object(&mut self, id: ObjectId) -> Result<()> {
        self.object(id)?;
        self.objects.retain(|o| o.id != id);
        self.layer_ranges.retain(|r| r.source_object_id != id);
        self.sync_plates();
        Ok(())
    }

    pub fn move_to_plate(&mut self, key: InstanceRef, plate: Option<u32>) -> Result<()> {
        ensure!(
            plate.is_none() || self.plates.iter().any(|p| Some(p.index) == plate),
            "немає пластини"
        );
        self.instance_mut(key)?.plate = plate;
        self.sync_plates();
        Ok(())
    }

    pub fn add_plate(&mut self, name: impl Into<String>) -> Result<u32> {
        let index = self
            .plates
            .iter()
            .map(|p| p.index)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("вичерпано індекси пластин"))?;
        self.plates.push(Plate {
            index,
            name: name.into(),
            ..Plate::default()
        });
        Ok(index)
    }

    /// Інстанси лишаються поза пластинами; останню пластину замінює порожня.
    pub fn delete_plate(&mut self, index: u32) -> Result<()> {
        ensure!(
            self.plates.iter().any(|p| p.index == index),
            "немає пластини"
        );
        self.plates.retain(|p| p.index != index);
        for p in &mut self.plates {
            if p.index > index {
                p.index -= 1;
            }
        }
        for o in &mut self.objects {
            for i in &mut o.instances {
                if i.plate == Some(index) {
                    i.plate = None;
                } else if i.plate.is_some_and(|p| p > index) {
                    i.plate = i.plate.map(|p| p - 1);
                }
            }
        }
        if self.plates.is_empty() {
            self.add_plate("")?;
        }
        self.sync_plates();
        Ok(())
    }

    pub fn rename_plate(&mut self, index: u32, name: impl Into<String>) -> Result<()> {
        let plate = self
            .plates
            .iter_mut()
            .find(|p| p.index == index)
            .ok_or_else(|| anyhow::anyhow!("немає пластини"))?;
        plate.name = name.into();
        Ok(())
    }

    fn plate_mut(&mut self, index: u32) -> Result<&mut Plate> {
        self.plates
            .iter_mut()
            .find(|p| p.index == index)
            .ok_or_else(|| anyhow::anyhow!("немає пластини"))
    }

    /// Заблоковану пластину пропускає розкладання й вона не приймає перенесені об’єкти.
    pub fn set_plate_locked(&mut self, index: u32, locked: bool) -> Result<()> {
        self.plate_mut(index)?.locked = locked;
        Ok(())
    }

    pub fn plate_locked(&self, index: u32) -> bool {
        self.plates.iter().any(|p| p.index == index && p.locked)
    }

    /// `None` — тип столу з налаштувань принтера.
    pub fn set_plate_bed_type(&mut self, index: u32, bed: Option<String>) -> Result<()> {
        self.plate_mut(index)?.bed_type = bed.filter(|b| !b.is_empty());
        Ok(())
    }

    /// Shelf packing: спочатку плануємо все, потім застосовуємо зсуви.
    pub fn arrange(&mut self, plate: u32, spacing: f64) -> Result<()> {
        self.arrange_on_bed(plate, spacing, [0.0; 2], [BED_SIZE; 2])
    }

    pub fn arrange_on_bed(
        &mut self,
        plate: u32,
        spacing: f64,
        lo: [f64; 2],
        hi: [f64; 2],
    ) -> Result<()> {
        ensure!(
            lo.iter().chain(&hi).all(|v| v.is_finite()) && hi[0] > lo[0] && hi[1] > lo[1],
            "некоректний стіл"
        );
        let p = self
            .plates
            .iter()
            .find(|p| p.index == plate)
            .ok_or_else(|| anyhow::anyhow!("немає пластини"))?;
        ensure!(!p.locked, "пластина заблокована");
        ensure!(
            spacing.is_finite() && spacing >= 0.0,
            "некоректний проміжок"
        );
        let mut items = self.plate_bounds(Some(plate))?;
        items.sort_by(|a, b| b.1.size().y.total_cmp(&a.1.size().y));
        let (mut x, mut y, mut row) = (lo[0], lo[1], 0.0_f64);
        let mut moves = Vec::new();
        for (key, bounds) in items {
            let size = bounds.size();
            if size.x > hi[0] - lo[0] || size.y > hi[1] - lo[1] {
                bail!("обʼєкт не вміщається на стіл");
            }
            if x + size.x > hi[0] {
                x = lo[0];
                y += row + spacing;
                row = 0.0;
            }
            ensure!(y + size.y <= hi[1], "на пластині недостатньо місця");
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

fn validate_transform(transform: DAffine3) -> Result<()> {
    let determinant = transform.matrix3.determinant();
    ensure!(
        transform.is_finite() && determinant.is_finite() && determinant.abs() > 1e-18,
        "некоректна трансформація"
    );
    Ok(())
}
