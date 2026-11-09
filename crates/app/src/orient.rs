//! Auto-orientation is an asynchronous action, not a persistent scene tool.
use crate::{
    jobs::JobKind,
    workspace::{Page, Workspace},
};
use gpui::Context;
use project::InstanceRef;

impl Workspace {
    pub(crate) fn orient_selection(&mut self, cx: &mut Context<Self>) {
        let mut keys = self.selected_instances.clone();
        if keys.is_empty() {
            keys.extend(self.selected);
        }
        if keys.is_empty() {
            self.orient_plate(self.active_plate, cx);
        } else {
            self.start_orientation(keys, cx);
        }
    }

    /// `index` is the zero-based Workspace plate index (as for show_plate).
    pub fn orient_plate(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(plate) = self.plates.get(index) else {
            return;
        };
        let number = (plate.number > 0).then_some(plate.number);
        let Some(project) = &self.project else { return };
        let keys = project
            .objects
            .iter()
            .flat_map(|o| {
                o.instances
                    .iter()
                    .enumerate()
                    .filter(move |(_, i)| i.plate == number)
                    .map(move |(instance, _)| InstanceRef {
                        object: o.id,
                        instance,
                    })
            })
            .collect();
        self.start_orientation(keys, cx);
    }

    fn start_orientation(&mut self, keys: Vec<InstanceRef>, cx: &mut Context<Self>) {
        if self.page != Page::Prepare
            || self.modal.is_some()
            || self.jobs.is_running(JobKind::Open)
            || self.jobs.is_running(JobKind::Orient)
            || keys.is_empty()
        {
            return;
        }
        self.finish_scene_gesture();
        let Some(mut result) = self.project.clone() else {
            return;
        };
        // Завантаження, правка, undo або живий жест можуть змінити ціль, поки йде робота.
        let target = self
            .job_target()
            .geometry()
            .idle_scene()
            .instances(&result, &keys);
        let targets = keys.clone();
        self.spawn_job(
            JobKind::Orient,
            target,
            move || {
                for key in keys {
                    result.orient_instance(key)?;
                }
                anyhow::Ok(result)
            },
            move |this, result, window, cx| match result {
                Ok(project) => {
                    this.checkpoint("Автоорієнтація");
                    // Apply transforms only: preserve unrelated settings edited while working.
                    if let Some(current) = this.project.as_mut() {
                        for key in &targets {
                            if let (Ok(to), Ok(from)) =
                                (current.instance_mut(*key), project.instance(*key))
                            {
                                to.transform = from.transform;
                            }
                        }
                    }
                    this.project_changed(window, cx);
                }
                Err(err) => {
                    this.error = Some(crate::tr_format!("Автоорієнтація: {err:#}").into());
                }
            },
            cx,
        );
    }
}
