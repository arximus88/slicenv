//! Read-only, manifold boolean previews for Prepare. All CSG is evaluated in
//! object-local f64 coordinates before placing copies on the selected plate.
//! This synchronous CPU API is intended to run on a background worker.

use anyhow::{Context, Result, ensure};
use manifold_csg::Manifold;
use project::TriangleSoup as Mesh;
use project::{DAffine3, DVec3, IndexedMesh, Object, Project, VolumeKind};

/// Union each object's parts, subtract its negative volumes, and place every
/// instance whose plate equals `plate` (`None` means outside all plates).
/// Modifiers/support volumes do not contribute solid geometry. Non-printable
/// instances remain visible, matching `Project::plate_mesh`.
///
/// Fails atomically for invalid/nonmanifold input; callers should retain their
/// previous/raw preview and show the contextual error. Never changes project,
/// archive, mesh attributes, or painter data.
pub fn plate_preview(project: &Project, plate: Option<u32>) -> Result<Mesh> {
    let mut out = Mesh::default();
    for object in &project.objects {
        if !object.instances.iter().any(|i| i.plate == plate) {
            continue;
        }
        let solid = object_solid(object)?;
        let local = export(&solid)?;
        for (index, instance) in object.instances.iter().enumerate() {
            if instance.plate == plate {
                append(&mut out, &local, instance.transform)
                    .with_context(|| format!("object {} instance {index}", object.id))?;
            }
        }
    }
    Ok(out)
}

/// Evaluate a single object without applying its instance transforms.
/// Useful for caching a local result across placements.
pub fn object_preview(object: &Object) -> Result<Mesh> {
    let local = export(&object_solid(object)?)?;
    let mut out = Mesh::default();
    append(&mut out, &local, DAffine3::IDENTITY)?;
    Ok(out)
}

/// Capped halves of a boolean object in object-local coordinates.
/// Plane equation: `normal.dot(point) = offset`; returns (positive, negative).
/// The normal need not be unit length. Input and project remain untouched.
pub fn split_object_by_plane(
    object: &Object,
    normal: [f64; 3],
    offset: f64,
) -> Result<(Mesh, Mesh)> {
    let n = DVec3::from(normal);
    let length = n.length();
    ensure!(
        n.is_finite() && length.is_finite() && length > 0.0 && offset.is_finite(),
        "cut plane must have a finite nonzero normal and finite offset"
    );
    let (positive, negative) =
        object_solid(object)?.split_by_plane((n / length).to_array(), offset / length);
    let convert = |solid: Manifold| -> Result<Mesh> {
        let mut mesh = Mesh::default();
        append(&mut mesh, &export(&solid)?, DAffine3::IDENTITY)?;
        Ok(mesh)
    };
    Ok((convert(positive)?, convert(negative)?))
}

fn object_solid(object: &Object) -> Result<Manifold> {
    let mut positives = Vec::new();
    let mut negatives = Vec::new();
    for (index, volume) in object.volumes.iter().enumerate() {
        let target = match volume.kind {
            VolumeKind::Part => &mut positives,
            VolumeKind::NegativeVolume => &mut negatives,
            _ => continue,
        };
        let solid = import(&volume.mesh, volume.transform).with_context(|| {
            format!("object {} ({:?}), volume {index} ({:?}, {:?}): boolean preview requires a closed, consistently oriented manifold mesh; repair the source mesh", object.id, object.name, volume.name, volume.kind)
        })?;
        target.push(solid);
    }
    let positive = Manifold::batch_union(&positives);
    positive.status().context("positive-volume union failed")?;
    if negatives.is_empty() {
        return Ok(positive);
    }
    // Batch difference subtracts every subsequent solid from the first.
    negatives.insert(0, positive);
    let result = Manifold::batch_difference(&negatives);
    result
        .status()
        .with_context(|| format!("object {} negative-volume subtraction failed", object.id))?;
    Ok(result)
}

fn validate_transform(transform: DAffine3) -> Result<bool> {
    let det = transform.matrix3.determinant();
    ensure!(
        transform.is_finite() && det.is_finite() && det != 0.0,
        "nonfinite or singular transform"
    );
    Ok(det < 0.0)
}

fn import(mesh: &IndexedMesh, transform: DAffine3) -> Result<Manifold> {
    let mirrored = validate_transform(transform)?;
    let positions: Vec<_> = mesh
        .positions
        .iter()
        .map(|p| transform.transform_point3(DVec3::from(*p)))
        .collect();
    ensure!(positions.iter().all(|p| p.is_finite()), "nonfinite vertex");
    let mut triangles = Vec::with_capacity(mesh.triangles.len() * 3);
    for t in &mesh.triangles {
        ensure!(
            t.iter().all(|&i| (i as usize) < positions.len()),
            "vertex index outside mesh"
        );
        let [a, b, c] = *t;
        triangles.extend(if mirrored {
            [a as u64, c as u64, b as u64]
        } else {
            [a as u64, b as u64, c as u64]
        });
    }
    let vertices: Vec<_> = positions.iter().flat_map(|p| p.to_array()).collect();
    Ok(Manifold::from_mesh_f64(&vertices, 3, &triangles)?)
}

struct LocalMesh {
    vertices: Vec<f64>,
    stride: usize,
    indices: Vec<u64>,
}

fn export(solid: &Manifold) -> Result<LocalMesh> {
    solid.status().context("boolean evaluation failed")?;
    let (vertices, stride, indices) = solid.to_mesh_f64();
    Ok(LocalMesh {
        vertices,
        stride,
        indices,
    })
}

fn append(out: &mut Mesh, local: &LocalMesh, transform: DAffine3) -> Result<()> {
    let mirrored = validate_transform(transform)?;
    for indices in local.indices.as_chunks::<3>().0 {
        let mut p = [DVec3::ZERO; 3];
        for (j, &index) in indices.iter().enumerate() {
            let start = index as usize * local.stride;
            p[j] = transform.transform_point3(DVec3::from_slice(&local.vertices[start..start + 3]));
        }
        if mirrored {
            p.swap(1, 2);
        }
        ensure!(
            p.iter().all(|v| v.is_finite() && v.as_vec3().is_finite()),
            "transformed vertex exceeds preview range"
        );
        let normal = (p[1] - p[0])
            .cross(p[2] - p[0])
            .normalize_or_zero()
            .as_vec3()
            .to_array();
        for point in p {
            out.positions.push(point.as_vec3().to_array());
            out.normals.push(normal);
        }
    }
    Ok(())
}
