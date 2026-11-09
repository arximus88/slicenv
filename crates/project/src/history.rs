//! Bounded Prepare snapshots. Meshes and original archive bytes are Arc-shared.
use crate::{InstanceRef, Project};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub primary: Option<InstanceRef>,
    pub instances: Vec<InstanceRef>,
    pub volume: Option<usize>,
    /// Display plate number (0 is the loose-objects view).
    pub plate: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct Entry {
    /// Ukrainian source key, translated by the UI when displayed.
    pub label: String,
    pub project: Project,
    pub selection: Selection,
}

#[derive(Debug)]
pub struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    limit: usize,
}

impl Default for History {
    fn default() -> Self {
        Self::new(50)
    }
}

impl History {
    pub fn new(limit: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            limit,
        }
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|e| e.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|e| e.label.as_str())
    }

    /// Commit a checkpoint only after a successful change. A no-op preserves redo.
    pub fn record(&mut self, before: Entry, current: &Project) -> bool {
        if before.project == *current {
            return false;
        }
        self.redo.clear();
        if self.limit > 0 {
            self.undo.push(before);
            if self.undo.len() > self.limit {
                self.undo.remove(0);
            }
        }
        true
    }

    pub fn undo(&mut self, project: &mut Project, selection: Selection) -> Option<Selection> {
        let entry = self.undo.pop()?;
        let previous = std::mem::replace(project, entry.project);
        self.redo.push(Entry {
            label: entry.label,
            project: previous,
            selection,
        });
        Some(entry.selection)
    }

    pub fn redo(&mut self, project: &mut Project, selection: Selection) -> Option<Selection> {
        let entry = self.redo.pop()?;
        let previous = std::mem::replace(project, entry.project);
        self.undo.push(Entry {
            label: entry.label,
            project: previous,
            selection,
        });
        Some(entry.selection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DVec3, IndexedMesh, Instance, Object, Volume};
    use std::sync::Arc;

    fn fixture() -> (Project, Selection) {
        let key = InstanceRef {
            object: 1,
            instance: 0,
        };
        let mut p = Project::default();
        p.add_plate("plate").unwrap();
        p.objects.push(Object {
            id: 1,
            volumes: vec![Volume {
                mesh: Arc::new(IndexedMesh {
                    positions: vec![[0.; 3]; 100_000],
                    triangles: vec![[0, 0, 0]; 100_000],
                    ..Default::default()
                }),
                ..Default::default()
            }],
            instances: vec![Instance::default()],
            ..Default::default()
        });
        p.sync_plates();
        (
            p,
            Selection {
                primary: Some(key),
                instances: vec![key],
                plate: Some(1),
                ..Default::default()
            },
        )
    }

    fn checkpoint(p: &Project, s: &Selection, label: &str) -> Entry {
        Entry {
            label: label.into(),
            project: p.clone(),
            selection: s.clone(),
        }
    }

    #[test]
    fn drag_is_one_entry_and_delete_restores_selection() {
        let (mut p, selection) = fixture();
        let mut history = History::default();
        let key = selection.primary.unwrap();
        let start = p.clone();
        let before = checkpoint(&p, &selection, "move");
        for _ in 0..20 {
            p.translate(key, DVec3::X).unwrap();
        }
        assert!(history.record(before, &p));
        let moved = p.clone();
        let before = checkpoint(&p, &selection, "delete");
        p.delete_instance(key).unwrap();
        history.record(before, &p);
        let deleted = p.clone();
        assert_eq!(
            history.undo(&mut p, Selection::default()),
            Some(selection.clone())
        );
        assert_eq!(p, moved);
        assert_eq!(
            history.undo(&mut p, selection.clone()),
            Some(selection.clone())
        );
        assert_eq!(p, start);
        assert!(history.undo(&mut p, selection.clone()).is_none());
        history.redo(&mut p, selection.clone());
        assert_eq!(p, moved);
        assert_eq!(history.redo(&mut p, selection), Some(Selection::default()));
        assert_eq!(p, deleted);
    }

    #[test]
    fn noops_preserve_redo_and_branching_clears_it() {
        let (mut p, s) = fixture();
        let mut h = History::default();
        let before = checkpoint(&p, &s, "rename");
        p.rename_plate(1, "new").unwrap();
        h.record(before, &p);
        h.undo(&mut p, s.clone());
        assert!(!h.record(checkpoint(&p, &s, "no-op"), &p));
        assert_eq!(h.redo_label(), Some("rename"));
        let before = checkpoint(&p, &s, "config");
        p.set_object_config(1, crate::Config::from([("wall_loops".into(), "3".into())]))
            .unwrap();
        h.record(before, &p);
        assert!(h.redo_label().is_none());
        h.clear();
        assert!(h.undo_label().is_none());
    }

    #[test]
    fn prepare_operations_round_trip_complete_snapshots() {
        let (mut p, mut selection) = fixture();
        let mut history = History::default();
        let key = selection.primary.unwrap();
        let mut states = vec![(p.clone(), selection.clone())];
        // Exercise the same transaction boundary used by Prepare, including
        // structural changes and settings that are invisible to mesh bounds.
        for step in 0..13 {
            let before = checkpoint(&p, &selection, "edit");
            match step {
                0 => p.rotate(key, crate::DQuat::from_rotation_z(0.5)).unwrap(),
                1 => p.scale(key, DVec3::splat(2.)).unwrap(),
                2 => {
                    p.duplicate_instance(key).unwrap();
                }
                3 => {
                    p.duplicate_object(1).unwrap();
                }
                4 => {
                    p.add_plate("second").unwrap();
                }
                5 => p.move_to_plate(key, Some(2)).unwrap(),
                6 => p
                    .set_volume_config(1, 0, crate::Config::from([("extruder".into(), "2".into())]))
                    .unwrap(),
                7 => {
                    let source = p.objects[0].volumes[0].clone();
                    p.objects[0].volumes.push(Volume {
                        kind: crate::VolumeKind::Modifier,
                        ..source
                    });
                    p.set_volume_kind(1, 1, crate::VolumeKind::NegativeVolume)
                        .unwrap();
                }
                8 => p.delete_plate(2).unwrap(),
                9 => {
                    p.move_to_plate(key, Some(1)).unwrap();
                    selection.primary = Some(
                        p.merge_instances(&[
                            key,
                            InstanceRef {
                                object: 2,
                                instance: 0,
                            },
                        ])
                        .unwrap(),
                    );
                    selection.instances = selection.primary.into_iter().collect();
                }
                10 => {
                    selection.instances = p.split_instance(selection.primary.unwrap()).unwrap();
                    selection.primary = selection.instances.first().copied();
                }
                11 => p.delete_object(1).unwrap(),
                12 => {
                    p.objects.push(Object {
                        id: 99,
                        volumes: vec![Volume::default()],
                        instances: vec![Instance::default()],
                        ..Default::default()
                    });
                    p.sync_plates();
                }
                _ => unreachable!(),
            }
            assert!(history.record(before, &p), "step {step}");
            states.push((p.clone(), selection.clone()));
        }
        for (expected, expected_selection) in states[..states.len() - 1].iter().rev() {
            selection = history.undo(&mut p, selection).unwrap();
            assert_eq!(&p, expected);
            assert_eq!(&selection, expected_selection);
        }
        for (expected, expected_selection) in &states[1..] {
            selection = history.redo(&mut p, selection).unwrap();
            assert_eq!(&p, expected);
            assert_eq!(&selection, expected_selection);
        }
    }

    #[test]
    fn limit_and_copy_on_write() {
        let (mut p, s) = fixture();
        p.archive.insert("large".into(), vec![0; 1_000_000].into());
        let mut h = History::new(2);
        for i in 0..3 {
            let before = checkpoint(&p, &s, "rename");
            assert!(Arc::ptr_eq(
                &before.project.objects[0].volumes[0].mesh,
                &p.objects[0].volumes[0].mesh
            ));
            assert!(Arc::ptr_eq(
                &before.project.archive["large"],
                &p.archive["large"]
            ));
            p.rename_plate(1, i.to_string()).unwrap();
            h.record(before, &p);
        }
        h.undo(&mut p, s.clone()).unwrap();
        h.undo(&mut p, s.clone()).unwrap();
        assert_eq!(p.plates[0].name, "0");
        assert!(h.undo(&mut p, s.clone()).is_none());
        let snapshot = p.clone();
        Arc::make_mut(&mut p.objects[0].volumes[0].mesh).positions[0][0] = 42.;
        assert_eq!(snapshot.objects[0].volumes[0].mesh.positions[0][0], 0.);
        let mut disabled = History::new(0);
        disabled.record(checkpoint(&snapshot, &s, "mesh"), &p);
        assert!(disabled.undo_label().is_none());
    }
}
