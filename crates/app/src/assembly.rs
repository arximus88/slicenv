//! Assembly selection, non-destructive inspection, and scoped process settings.
use crate::{
    jobs::JobKind,
    theme::zed,
    ui::{self, IconName, Toggle as _},
    widgets::ToastKind,
    workspace::{Page, Workspace},
};
use gpui::{Context, Window, div, prelude::*, px};
use gpui_component::input::{Input, InputState};
use project::{Bounds, InstanceRef, VolumeKind};
use std::rc::Rc;

pub fn role_name(kind: VolumeKind) -> &'static str {
    match kind {
        VolumeKind::Part => "Деталь",
        VolumeKind::NegativeVolume => "Віднімання",
        VolumeKind::Modifier => "Модифікатор",
        VolumeKind::SupportBlocker => "Блокування підтримки",
        VolumeKind::SupportEnforcer => "Примусова підтримка",
    }
}

impl Workspace {
    pub fn selected_bounds(&self) -> Option<Bounds> {
        let _profile = paths::profile::Span::new("selected_bounds");
        let key = self.selected?;
        let project = self.project.as_ref()?;
        if let Some(drag) = self.live_drag.as_ref().filter(|d| d.key == key) {
            let local = match drag.volume {
                Some(index) => {
                    project
                        .object(key.object)
                        .ok()?
                        .volumes
                        .get(index)?
                        .transform
                }
                None => project::DAffine3::IDENTITY,
            };
            let delta = project.instance(key).ok()?.transform * local * drag.start.inverse();
            let mut min = project::DVec3::MAX;
            let mut max = project::DVec3::MIN;
            for i in 0..8 {
                let lo = drag.bounds.min;
                let hi = drag.bounds.max;
                let p = delta.transform_point3(project::DVec3::new(
                    if i & 1 == 0 { lo.x } else { hi.x },
                    if i & 2 == 0 { lo.y } else { hi.y },
                    if i & 4 == 0 { lo.z } else { hi.z },
                ));
                min = min.min(p);
                max = max.max(p);
            }
            return Some(Bounds { min, max });
        }
        match self.selected_volume {
            None => self.cached_instance_bounds(key),
            Some(index) => self.cached_volume_bounds(key, index),
        }
    }

    pub fn choose_volume(
        &mut self,
        key: InstanceRef,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected = Some(key);
        self.selected_instances = vec![key];
        self.selected_volume = Some(index);
        self.refresh_modifiers();
        self.sync_selection(window, cx);
        cx.notify();
    }

    pub fn toggle_instance(
        &mut self,
        key: InstanceRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_volume = None;
        if let Some(i) = self.selected_instances.iter().position(|k| *k == key) {
            self.selected_instances.remove(i);
        } else {
            self.selected_instances.push(key);
        }
        self.selected = self.selected_instances.last().copied();
        self.refresh_modifiers();
        self.dirty = true;
        self.sync_selection(window, cx);
        cx.notify();
    }

    pub fn merge_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let keys = self.selected_instances.clone();
        self.checkpoint("Об’єднання");
        let Some(project) = &mut self.project else {
            return;
        };
        match project.merge_instances(&keys) {
            Ok(key) => {
                self.selected = Some(key);
                self.selected_volume = None;
                self.selected_instances = vec![key];
                self.project_changed(window, cx);
            }
            Err(err) => self.toast(
                ToastKind::Error,
                crate::tr_format!("Об’єднання: {err:#}"),
                cx,
            ),
        }
    }

    pub fn can_split_selection(&self) -> bool {
        self.selected_volume.is_none()
            && self
                .selected
                .and_then(|key| self.project.as_ref()?.object(key.object).ok())
                .is_some_and(|o| {
                    o.volumes.len() > 1 && o.volumes.iter().all(|v| v.kind == VolumeKind::Part)
                })
    }

    pub fn split_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_split_selection() {
            return;
        }
        let Some(key) = self.selected else { return };
        self.checkpoint("Розділення");
        let Some(project) = &mut self.project else {
            return;
        };
        match project.split_instance(key) {
            Ok(keys) => {
                self.selected = keys.first().copied();
                self.selected_volume = None;
                self.selected_instances = keys;
                self.project_changed(window, cx);
            }
            Err(err) => self.toast(
                ToastKind::Error,
                crate::tr_format!("Розділення: {err:#}"),
                cx,
            ),
        }
    }

    pub fn refresh_modifiers(&mut self) {
        let mut mesh = if self.page == Page::Prepare && self.show_modifiers {
            self.project
                .as_ref()
                .zip(self.plates.get(self.active_plate))
                .and_then(|(p, plate)| {
                    p.plate_mesh_with_kinds(
                        (plate.number > 0).then_some(plate.number),
                        &[
                            VolumeKind::NegativeVolume,
                            VolumeKind::Modifier,
                            VolumeKind::SupportBlocker,
                            VolumeKind::SupportEnforcer,
                        ],
                    )
                    .ok()
                })
                .map(|s| viewer::Mesh {
                    positions: s.positions,
                    normals: s.normals,
                })
                .unwrap_or_default()
        } else if self.page == Page::Prepare {
            self.selected_volume_mesh().unwrap_or_default()
        } else {
            viewer::Mesh::default()
        };
        if self.page == Page::Prepare {
            let offset = self
                .plates
                .get(self.active_plate)
                .map_or(glam::Vec3::ZERO, |p| self.plate_offset(p.number));
            for p in &mut mesh.positions {
                *p = (glam::Vec3::from(*p) + offset).to_array();
            }
        }
        self.renderer.set_modifiers(&mesh);
    }

    fn selected_volume_mesh(&self) -> Option<viewer::Mesh> {
        let key = self.selected?;
        let index = self.selected_volume?;
        let project = self.project.as_ref()?;
        let volume = project.object(key.object).ok()?.volumes.get(index)?;
        if volume.kind == VolumeKind::Part {
            return None;
        }
        let instance = project.instance(key).ok()?;
        if instance.plate.unwrap_or(0) != self.plates.get(self.active_plate)?.number {
            return None;
        }
        let transform = instance.transform * volume.transform;
        let mut mesh = viewer::Mesh::default();
        for triangle in &volume.mesh.triangles {
            let points = triangle.map(|i| {
                transform.transform_point3(project::DVec3::from(volume.mesh.positions[i as usize]))
            });
            let normal = (points[1] - points[0])
                .cross(points[2] - points[0])
                .normalize_or_zero()
                .as_vec3()
                .to_array();
            for point in points {
                mesh.positions.push(point.as_vec3().to_array());
                mesh.normals.push(normal);
            }
        }
        Some(mesh)
    }

    pub fn apply_prepare_clip(&mut self, cx: &gpui::App) {
        // Інструмент «Різання»: напівпрозора площина й блакитно-пурпурне прев’ю частин замість перерізу.
        if let Some((plane, bounds)) = self.cut_preview() {
            self.renderer.set_clip_plane(None);
            self.renderer.set_section_plane(Some((plane, bounds)));
            self.renderer.set_cut_tint(Some((plane, bounds)));
            return;
        }
        self.renderer.set_cut_tint(None);
        let plane = if self.cut_enabled {
            self.scene_bounds().map(|(lo, hi)| {
                let t = self.cut_position.read(cx).values().1 as f32 / 1000.0;
                let height = lo[self.cut_axis] + (hi[self.cut_axis] - lo[self.cut_axis]) * t;
                let mut p = [0.0; 4];
                p[self.cut_axis] = 1.0;
                p[3] = -height;
                p
            })
        } else {
            None
        };
        self.renderer.set_clip_plane(plane);
        // Рамку рахуємо лише для ввімкненого перерізу: `zip` обчислює аргумент завжди.
        self.renderer.set_section_plane(if self.cut_ghost {
            plane.and_then(|p| Some((p, self.scene_bounds()?)))
        } else {
            None
        });
    }

    pub fn open_part_kind(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(key), Some(index)) = (self.selected, self.selected_volume) else {
            return;
        };
        self.open_modal(
            Rc::new(move |ws, _, cx| {
                let body = div().flex().flex_col().p(px(12.)).gap(px(6.)).children(
                    [
                        VolumeKind::Part,
                        VolumeKind::NegativeVolume,
                        VolumeKind::Modifier,
                        VolumeKind::SupportBlocker,
                        VolumeKind::SupportEnforcer,
                    ]
                    .into_iter()
                    .map(|kind| {
                        ui::ghost_button(
                            gpui::SharedString::from(format!("part-kind-{kind:?}")),
                            Some(IconName::Box),
                            role_name(kind),
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.checkpoint("Роль частини");
                                let result = this
                                    .project
                                    .as_mut()
                                    .map(|p| p.set_volume_kind(key.object, index, kind));
                                match result {
                                    Some(Ok(())) => {
                                        this.close_modal(cx);
                                        this.project_changed(window, cx);
                                    }
                                    Some(Err(err)) => {
                                        this.toast(ToastKind::Error, err.to_string(), cx)
                                    }
                                    None => {}
                                }
                            },
                        ))
                    }),
                );
                let _ = ws;
                crate::widgets::modal("Роль частини", 360., body, None)
            }),
            window,
            cx,
        );
    }

    /// `range` targets one height-range override; None targets the selected object/part.
    pub fn open_scoped_settings(
        &mut self,
        range: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = self.selected else { return };
        let volume = self.selected_volume.filter(|_| range.is_none());
        let Some(project) = &self.project else { return };
        let Ok(object) = project.object(key.object) else {
            return;
        };
        let ranges = project.object_layer_ranges(key.object).unwrap_or_default();
        let config = if let Some(i) = range {
            ranges.get(i).map(|r| &r.config)
        } else if let Some(i) = volume {
            object.volumes.get(i).map(|v| &v.config)
        } else {
            Some(&object.config)
        };
        let Some(config) = config else { return };
        let keys = [
            ("layer_height", "Висота шару, мм"),
            ("sparse_infill_density", "Заповнення, %"),
            ("wall_loops", "Стінок"),
        ];
        let fields: Vec<_> = keys
            .into_iter()
            .map(|(key, label)| {
                let initial = config.get(key).cloned().unwrap_or_default();
                let input = cx.new(|cx| {
                    InputState::new(window, cx).placeholder(crate::i18n::tr("Успадковано"))
                });
                input.update(cx, |s, cx| s.set_value(initial, window, cx));
                (key, label, input)
            })
            .collect();
        for (key, _, input) in &fields {
            if let Some(def) = crate::preset_editor::schema().options.get(*key)
                && let Some(spec) = crate::scrub::Spec::option(
                    def,
                    input
                        .read(cx)
                        .value()
                        .trim_end_matches('%')
                        .parse()
                        .unwrap_or(0.0),
                )
            {
                self.bind_scrub(input, spec, crate::scrub::Target::Text);
            }
        }
        let title = if range.is_some() {
            "Налаштування діапазону"
        } else if volume.is_some() {
            "Налаштування частини"
        } else {
            "Налаштування об’єкта"
        };
        self.open_modal(
            Rc::new(move |_, window, cx| {
                let mut body = div()
                    .flex()
                    .flex_col()
                    .p(px(12.))
                    .gap(px(8.))
                    .child(div().text_sm().text_color(zed().text_muted).child(crate::i18n::tr(
                    "Порожнє поле успадковує значення. Зміни стосуються всіх копій цього об’єкта.",
                )));
                for (_, label, input) in &fields {
                    body = body.child(ui::prop_row(
                        *label,
                        crate::scrub::field(input, Input::new(input), window, cx),
                    ));
                }
                let save_fields = fields.clone();
                let footer = ui::accent_button("scoped-save", IconName::Save, "Застосувати", true)
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        let Some(p) = &this.project else { return };
                        let Ok(o) = p.object(key.object) else { return };
                        let mut ranges = p
                            .object_layer_ranges(key.object)
                            .unwrap_or_default()
                            .into_iter()
                            .cloned()
                            .collect::<Vec<_>>();
                        let mut config = if let Some(i) = range {
                            ranges.get(i).map(|r| r.config.clone())
                        } else if let Some(i) = volume {
                            o.volumes.get(i).map(|v| v.config.clone())
                        } else {
                            Some(o.config.clone())
                        };
                        let Some(config) = &mut config else { return };
                        for (key, _, input) in &save_fields {
                            let text = input.read(cx).value().trim().replace(',', ".");
                            if text.is_empty() {
                                config.remove(*key);
                                continue;
                            }
                            let json = serde_json::Value::String(text.clone());
                            if let Err(err) = this.settings_schema.validate(key, &json) {
                                this.toast(ToastKind::Error, err.to_string(), cx);
                                return;
                            }
                            config.insert(key.to_string(), text);
                        }
                        let config = config.clone();
                        this.checkpoint("Налаштування об’єкта / частини");
                        let Some(p) = &mut this.project else { return };
                        let result = if let Some(i) = range {
                            ranges[i].config = config;
                            p.set_layer_ranges(key.object, ranges)
                        } else if let Some(i) = volume {
                            p.set_volume_config(key.object, i, config)
                        } else {
                            p.set_object_config(key.object, config)
                        };
                        match result {
                            Ok(()) => {
                                this.close_modal(cx);
                                this.finish_checkpoint();
                                // Config overrides do not change triangles, transforms,
                                // selection, or the boolean preview. Invalidate slicing
                                // without regenerating every plate and uploading meshes.
                                this.edited = true;
                                this.cancel_slice();
                                this.preview = None;
                                cx.notify();
                            }
                            Err(err) => this.toast(ToastKind::Error, err.to_string(), cx),
                        }
                    }))
                    .into_any_element();
                crate::widgets::modal(title, 540., body, Some(footer))
            }),
            window,
            cx,
        );
    }
}

pub fn cut_card(ws: &Workspace, cx: &mut Context<Workspace>) -> Option<gpui::AnyElement> {
    if !ws.cut_enabled {
        return None;
    }
    let mut row = ui::scene_control("prepare-section-card")
        .absolute()
        .top(px(174.))
        .right(px(12.))
        .w(px(260.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .p(px(10.))
        .rounded(px(8.))
        .bg(zed().panel.opacity(0.96))
        .border_1()
        .border_color(zed().border);
    let mut axes = div()
        .flex()
        .items_center()
        .gap(px(6.))
        .child(div().flex_1().text_sm().child(crate::i18n::tr("Переріз")));
    for (axis, label) in ["X", "Y", "Z"].into_iter().enumerate() {
        axes = axes.child(
            ui::ghost_button(("section-axis", axis), None, label)
                .toggled(axis == ws.cut_axis)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.cut_axis = axis;
                    this.dirty = true;
                    cx.notify();
                })),
        );
    }
    row = row
        .child(axes)
        .child(ws.cut_position.clone())
        .child(
            ui::ghost_button("section-ghost", None, "Показувати площину")
                .toggled(ws.cut_ghost)
            .on_click(cx.listener(|this, _, _, cx| {
                this.cut_ghost = !this.cut_ghost;
                this.preferences.section_plane = this.cut_ghost;
                this.save_preferences(cx);
                this.dirty = true;
                cx.notify();
            })),
        )
        .child(div().text_xs().text_color(zed().text_muted).child({
            let t = ws.cut_position.read(cx).values().1 as f32 / 1000.0;
            let height = ws.scene_bounds().map_or(0.0, |(lo, hi)| {
                lo[ws.cut_axis] + (hi[ws.cut_axis] - lo[ws.cut_axis]) * t
            });
            crate::tr_format!(
                "{} = {height:.2} мм · геометрія зберігається",
                ["X", "Y", "Z"][ws.cut_axis]
            )
        }));
    Some(row.into_any_element())
}

impl Workspace {
    /// Add STL geometry to the current project; opening a 3MF still preserves its full archive.
    pub fn prompt_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.project.is_none() {
            self.prompt_open(window, cx);
            return;
        }
        // Знімок до діалогу: результат належить цьому проєкту, геометрії й пластині.
        let mut origin = self.job_target().geometry().active_plate();
        if let Some(plate) = self.plates.get(self.active_plate).map(|p| p.number) {
            origin = origin.plate_exists(plate);
        }
        let prompt = crate::file_menu::prompt_open(
            window,
            crate::file_menu::OpenKind::Stl,
            "Додати STL",
            true,
        );
        cx.spawn_in(window, async move |this, cx| {
            let paths = prompt.await;
            if paths.is_empty() {
                return;
            }
            this.update(cx, |this, cx| {
                this.spawn_job(
                    JobKind::Import,
                    origin,
                    move || {
                        paths
                            .into_iter()
                            .map(|path| {
                                anyhow::ensure!(
                                    path.extension()
                                        .is_some_and(|e| e.eq_ignore_ascii_case("stl")),
                                    "Для додавання обери STL; проєкти 3MF відкриваються через Головну."
                                );
                                project::Project::load_stl(&path)
                            })
                            .collect::<anyhow::Result<Vec<_>>>()
                    },
                    |this, loaded, window, cx| this.add_imported_stl(loaded, window, cx),
                    cx,
                );
            })
            .ok();
        })
        .detach();
    }

    fn add_imported_stl(
        &mut self,
        loaded: anyhow::Result<Vec<project::Project>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let imported = match loaded {
            Ok(v) => v,
            Err(err) => {
                self.toast(ToastKind::Error, err.to_string(), cx);
                return;
            }
        };
        let Some(number) = self
            .plates
            .get(self.active_plate)
            .and_then(|p| (p.number > 0).then_some(p.number))
        else {
            return;
        };
        let Some(project) = &self.project else {
            return;
        };
        let mut staged = project.clone();
        let mut next = staged.objects.iter().map(|o| o.id).max().unwrap_or(0);
        for p in imported {
            for mut object in p.objects {
                let Some(id) = next.checked_add(1) else {
                    self.toast(ToastKind::Error, "Вичерпано ID об’єктів", cx);
                    return;
                };
                next = id;
                object.id = id;
                for instance in &mut object.instances {
                    instance.plate = Some(number);
                }
                staged.objects.push(object);
            }
        }
        staged.sync_plates();
        self.checkpoint("Імпорт моделей");
        self.project = Some(staged);
        self.selected_volume = None;
        self.selected = Some(InstanceRef {
            object: next,
            instance: 0,
        });
        self.selected_instances = self.selected.into_iter().collect();
        self.project_changed(window, cx);
    }
}

impl Workspace {
    pub fn invalidate_geometry(&mut self) {
        self.invalidate_geometry_live();
        self.rebuild_instance_bounds();
    }

    /// Як `invalidate_geometry`, але без точних рамок: під час живого жесту (Move/Rotate/
    /// Scale) рамка вибраного береться з кутів стартової, точні — після відпускання.
    pub fn invalidate_geometry_live(&mut self) {
        self.cancel_slice();
        self.geometry_revision = self.geometry_revision.wrapping_add(1);
        self.invalidate_plate_bounds();
        self.jobs.cancel(JobKind::BooleanPreview);
        self.boolean_mesh = None;
        self.geometry_error = None;
    }

    pub fn refresh_prepare_mesh(&mut self) {
        let _profile = paths::profile::Span::new("refresh_prepare_mesh");
        self.sync_plate_scene();
    }

    pub fn ensure_geometry_preview(&mut self, cx: &mut Context<Self>) {
        if !self.boolean_preview
            || self.jobs.is_running(JobKind::BooleanPreview)
            || self.boolean_mesh.is_some()
            || self.geometry_error.is_some()
            || self.moving.is_some()
            || self.gizmo_drag.is_some()
        {
            return;
        }
        let Some(project) = &self.project else { return };
        let Some(plate) = self.plates.get(self.active_plate) else {
            return;
        };
        let number = (plate.number > 0).then_some(plate.number);
        if !project.objects.iter().any(|o| {
            o.instances.iter().any(|i| i.plate == number)
                && o.volumes
                    .iter()
                    .any(|v| v.kind == VolumeKind::NegativeVolume)
        }) {
            return;
        }
        let project = project.clone();
        let revision = self.geometry_revision;
        self.spawn_job(
            JobKind::BooleanPreview,
            self.job_target().geometry().active_plate(),
            move || geometry::plate_preview(&project, number),
            move |this, result, _, cx| match result {
                Ok(mesh) => {
                    this.boolean_mesh = Some((
                        revision,
                        viewer::Mesh {
                            positions: mesh.positions,
                            normals: mesh.normals,
                        },
                    ));
                    if this.page == Page::Prepare {
                        this.refresh_prepare_mesh();
                    }
                }
                Err(err) => {
                    let error = crate::tr_format!("Попередній вигляд віднімання: {err:#}");
                    this.geometry_error = Some(error.clone().into());
                    this.toast(ToastKind::Error, error, cx);
                }
            },
            cx,
        );
    }
}

impl Workspace {
    /// Flush live edits before navigation, even if a platform mouse-up was lost.
    pub fn finish_scene_gesture(&mut self) {
        // A navigation/tool change can end a gesture without a mouse-up event.
        self.finish_live_drag();
        if let Some(drag) = &self.gizmo_drag
            && drag.readout.is_some()
            && self.selected_volume.is_none()
            && !matches!(drag.handle, viewer::Handle::Move(_))
            && let Some(project) = self.project.as_mut()
        {
            let _ = project.drop_to_bed(drag.key);
        }
        self.finish_checkpoint();
        if self.edited && (self.moving.is_some() || self.gizmo_drag.is_some()) {
            let number = self.plates.get(self.active_plate).map(|p| p.number);
            if let Some(project) = &self.project {
                match crate::objects::plates_of(project) {
                    Ok(plates) => {
                        self.plates = plates;
                        self.invalidate_plate_bounds();
                        self.active_plate = number
                            .and_then(|n| self.plates.iter().position(|p| p.number == n))
                            .unwrap_or(0);
                    }
                    Err(err) => self.error = Some(format!("{err:#}").into()),
                }
            }
        }
        self.moving = None;
        self.gizmo_drag = None;
        self.drag_from = None;
        self.drag_moved = false;
        self.panning = None;
    }
}
