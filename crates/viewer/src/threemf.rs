//! Project-backed model loading; embedded thumbnails only read ZIP entries.
use crate::{Mesh, Plate};
use anyhow::Result;
use std::{
    fs::File,
    io::{BufReader, Read},
    path::Path,
};
use zip::ZipArchive;

const THUMBNAILS: &[&str] = &[
    "Metadata/plate_1.png",
    "Auxiliaries/.thumbnails/thumbnail_middle.png",
    "Auxiliaries/.thumbnails/thumbnail_3mf.png",
    "Metadata/thumbnail.png",
    "Metadata/plate_1_small.png",
];

pub fn load_3mf(path: &Path) -> Result<Vec<Plate>> {
    let project = project::Project::load(path)?;
    let mut plates = Vec::new();
    // View-only loading historically displayed every volume, including modifiers.
    let kinds = [
        project::VolumeKind::Part,
        project::VolumeKind::NegativeVolume,
        project::VolumeKind::Modifier,
        project::VolumeKind::SupportBlocker,
        project::VolumeKind::SupportEnforcer,
    ];
    for plate in &project.plates {
        let soup = project.plate_mesh_with_kinds(Some(plate.index), &kinds)?;
        let mut mesh = Mesh {
            positions: soup.positions,
            normals: soup.normals,
        };
        mesh.settle_on_bed();
        plates.push(Plate {
            number: plate.index,
            name: (!plate.name.is_empty()).then(|| plate.name.clone()),
            mesh,
        });
    }
    let soup = project.plate_mesh_with_kinds(None, &kinds)?;
    if !soup.positions.is_empty() {
        plates.push(Plate {
            number: 0,
            name: Some("поза пластинами".into()),
            mesh: Mesh {
                positions: soup.positions,
                normals: soup.normals,
            },
        });
    }
    anyhow::ensure!(!plates.is_empty(), "3MF без геометрії");
    Ok(plates)
}

pub fn thumbnail_3mf(path: &Path) -> Option<Vec<u8>> {
    let mut archive = ZipArchive::new(BufReader::new(File::open(path).ok()?)).ok()?;
    THUMBNAILS.iter().find_map(|name| {
        let mut entry = archive.by_name(name).ok()?;
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut bytes).ok()?;
        Some(bytes)
    })
}
