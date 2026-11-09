//! Кеш рамок інстансів і частин без зсуву інстанса. Перенесення — найчастіша правка й
//! живе перетягування — лишає сигнатури незмінними, тож рамка після нього коштує
//! O(1): `shifted(local, translation)`. Перераховуються лише записи, чия форма змінилась
//! (меш, трансформації частин, поворот чи масштаб інстанса).
use crate::{Bounds, DAffine3, DVec3, IndexedMesh, InstanceRef, Project, VolumeKind};

/// Рамка інстанса з кешу або `None`, якщо запис застарів чи відсутній.
pub fn cached_instance(
    project: &Project,
    cache: &InstanceBoundsCache,
    key: InstanceRef,
) -> Option<Bounds> {
    let (signature, local) = cache.get(&key)?;
    (*signature == instance_signature(project, key)?)
        .then_some(*local)
        .zip(project.instance(key).ok())
        .map(|(local, i)| shifted(local, i.transform.translation))
}

/// Рамка частини `index` інстанса з кешу або `None`, якщо запис застарів чи відсутній.
pub fn cached_volume(
    project: &Project,
    cache: &VolumeBoundsCache,
    key: InstanceRef,
    index: usize,
) -> Option<Bounds> {
    let (signature, local) = cache.get(&(key, index))?;
    (*signature == volume_signature(project, key, index)?)
        .then_some(*local)
        .zip(project.instance(key).ok())
        .map(|(local, i)| shifted(local, i.transform.translation))
}

pub type InstanceBoundsCache =
    std::collections::BTreeMap<InstanceRef, (u64, Bounds)>;
pub type VolumeBoundsCache =
    std::collections::BTreeMap<(InstanceRef, usize), (u64, Bounds)>;

pub fn shifted(bounds: Bounds, by: DVec3) -> Bounds {
    Bounds {
        min: bounds.min + by,
        max: bounds.max + by,
    }
}

/// Трансформація інстанса без зсуву: рамка під нею плюс зсув — точна рамка інстанса.
fn linear(transform: DAffine3) -> DAffine3 {
    DAffine3 {
        matrix3: transform.matrix3,
        translation: DVec3::ZERO,
    }
}

fn mesh_bounds(mesh: &IndexedMesh, transform: DAffine3) -> Option<Bounds> {
    let mut points = mesh
        .positions
        .iter()
        .map(|p| transform.transform_point3(DVec3::from(*p)));
    let first = points.next()?;
    let (min, max) = points.fold((first, first), |(lo, hi), p| (lo.min(p), hi.max(p)));
    Some(Bounds { min, max })
}

fn union(a: Option<Bounds>, b: Option<Bounds>) -> Option<Bounds> {
    match (a, b) {
        (Some(a), Some(b)) => Some(Bounds {
            min: a.min.min(b.min),
            max: a.max.max(b.max),
        }),
        (a, b) => a.or(b),
    }
}

/// Нові кеші з повторним використанням незмінених записів старих.
pub fn refresh(
    project: &Project,
    mut old_instances: InstanceBoundsCache,
    mut old_volumes: VolumeBoundsCache,
) -> (InstanceBoundsCache, VolumeBoundsCache) {
    let mut instances = InstanceBoundsCache::new();
    let mut volumes = VolumeBoundsCache::new();
    for object in &project.objects {
        for (instance, placement) in object.instances.iter().enumerate() {
            let key = InstanceRef {
                object: object.id,
                instance,
            };
            let base = linear(placement.transform);
            let mut whole = None;
            for (index, volume) in object.volumes.iter().enumerate() {
                let Some(signature) = volume_signature(project, key, index) else {
                    continue;
                };
                let local = match old_volumes.remove(&(key, index)) {
                    Some((old, bounds)) if old == signature => Some(bounds),
                    _ => mesh_bounds(&volume.mesh, base * volume.transform),
                };
                if let Some(bounds) = local {
                    volumes.insert((key, index), (signature, bounds));
                    // Рамка інстанса — лише друковані частини.
                    if volume.kind == VolumeKind::Part {
                        whole = union(whole, Some(bounds));
                    }
                }
            }
            let Some(signature) = instance_signature(project, key) else {
                continue;
            };
            let reused = old_instances
                .remove(&key)
                .filter(|(old, _)| *old == signature)
                .map(|(_, b)| b);
            if let Some(bounds) = reused.or(whole) {
                instances.insert(key, (signature, bounds));
            }
        }
    }
    (instances, volumes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DQuat;

    fn project_with_object() -> (Project, InstanceRef) {
        let mut p = Project::default();
        p.add_plate("plate").unwrap();
        let key = p
            .add_mesh_object(
                "test",
                IndexedMesh {
                    positions: vec![[0., 0., 0.], [10., 0., 0.], [0., 10., 5.]],
                    triangles: vec![[0, 1, 2]],
                    ..Default::default()
                },
                1,
                [128., 128.],
                [0., 0.],
                [256., 256.],
            )
            .unwrap();
        (p, key)
    }

    fn cached(p: &Project, cache: &InstanceBoundsCache, key: InstanceRef) -> Bounds {
        let (signature, local) = cache[&key];
        assert_eq!(Some(signature), instance_signature(p, key));
        shifted(local, p.instance(key).unwrap().transform.translation)
    }

    #[test]
    fn cached_bounds_match_project_after_translate_rotate_and_removal() {
        let (mut p, key) = project_with_object();
        let (cache, volumes) = refresh(&p, Default::default(), Default::default());
        assert_eq!(cached(&p, &cache, key), p.instance_bounds(key).unwrap().unwrap());

        // Перенесення не змінює сигнатуру: старий запис дає правильну нову рамку.
        p.translate(key, DVec3::new(10., -3., 0.)).unwrap();
        assert_eq!(cached(&p, &cache, key), p.instance_bounds(key).unwrap().unwrap());

        p.rotate(key, DQuat::from_rotation_z(0.7)).unwrap();
        assert_ne!(Some(cache[&key].0), instance_signature(&p, key));
        let (cache, volumes) = refresh(&p, cache, volumes);
        let expected = p.instance_bounds(key).unwrap().unwrap();
        let got = cached(&p, &cache, key);
        assert!((got.min - expected.min).abs().max_element() < 1e-9);
        assert!((got.max - expected.max).abs().max_element() < 1e-9);
        assert_eq!(volumes.len(), 1);

        p.delete_all_objects();
        let (cache, volumes) = refresh(&p, cache, volumes);
        assert!(cache.is_empty() && volumes.is_empty());
    }
}

// Mesh identity, part transforms and the instance's linear part (not its translation).
pub fn instance_signature(project: &Project, key: InstanceRef) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    project
        .instance(key)
        .ok()?
        .transform
        .matrix3
        .to_cols_array()
        .map(f64::to_bits)
        .hash(&mut hash);
    for volume in &project.object(key.object).ok()?.volumes {
        (std::sync::Arc::as_ptr(&volume.mesh) as usize).hash(&mut hash);
        volume
            .transform
            .to_cols_array()
            .map(f64::to_bits)
            .hash(&mut hash);
        // Bounds ignore non-part volumes.
        (volume.kind == VolumeKind::Part).hash(&mut hash);
    }
    Some(hash.finish())
}

pub fn volume_signature(project: &Project, key: InstanceRef, index: usize) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    let volume = project.object(key.object).ok()?.volumes.get(index)?;
    project
        .instance(key)
        .ok()?
        .transform
        .matrix3
        .to_cols_array()
        .map(f64::to_bits)
        .hash(&mut hash);
    (std::sync::Arc::as_ptr(&volume.mesh) as usize).hash(&mut hash);
    volume
        .transform
        .to_cols_array()
        .map(f64::to_bits)
        .hash(&mut hash);
    Some(hash.finish())
}
