//! Редагування проєкту на «Підготовці»: список об’єктів, вибір (кліком у сцені або в
//! списку), переміщення / поворот / масштаб, пластини, розкладання й збереження 3MF.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use gpui::{Context, Entity, SharedString, Window, div, prelude::*, px};
use gpui_component::{
    input::{Input, InputEvent, InputState},
    tooltip::Tooltip,
};
use project::{DQuat, DVec3, InstanceRef, Project};
use viewer::Plate;

use crate::{
    theme::zed,
    ui::{self, IconName, Toggle as _},
    widgets::ToastKind,
    workspace::Workspace,
};

/// Поля трансформації вибраного об’єкта; живуть увесь час, значення оновлюються з вибору.
pub struct TransformInputs {
    x: Entity<InputState>,
    y: Entity<InputState>,
    z_rot: Entity<InputState>,
    scale: Entity<InputState>,
}

#[derive(Clone, Copy)]
enum Field {
    X,
    Y,
    Rotate,
    Scale,
}

/// Пластини й GPU-сітки частин будуються у `viewer` — там їх міряють бенчмарки без вікна.
pub use viewer::{local_mesh, plates_of};

pub fn load_project(path: &Path) -> anyhow::Result<(Project, Vec<Plate>)> {
    let project = Project::load(path)?;
    let plates = plates_of(&project)?;
    Ok((project, plates))
}

fn plate_key(number: u32) -> Option<u32> {
    (number > 0).then_some(number)
}

impl TransformInputs {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> (Self, Vec<gpui::Subscription>) {
        let mut subs = Vec::new();
        let mut make = |field: Field, window: &mut Window, cx: &mut Context<Workspace>| {
            let input = cx.new(|cx| InputState::new(window, cx));
            subs.push(cx.subscribe_in(
                &input,
                window,
                move |this, input, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        let text = input.read(cx).value().trim().replace(',', ".");
                        if let Ok(value) = text.trim_end_matches(['%', '°']).parse::<f64>() {
                            this.apply_field(field, value, window, cx);
                        }
                    }
                },
            ));
            input
        };
        let inputs = Self {
            x: make(Field::X, window, cx),
            y: make(Field::Y, window, cx),
            z_rot: make(Field::Rotate, window, cx),
            scale: make(Field::Scale, window, cx),
        };
        (inputs, subs)
    }
}

impl Workspace {
    /// Після будь-якої зміни проєкту: перебудувати сітки, рамку вибору й скинути нарізку.
    pub(crate) fn project_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let _profile = settings::profile::Span::new("project_changed");
        self.finish_checkpoint();
        self.edited = true;
        // Зупиняє нарізку; застарілою стає лише пластина, чий вміст змінився (`plate_strip`).
        self.invalidate_geometry();
        self.preview = None;
        if let Some(project) = &self.project {
            match plates_of(project) {
                Ok(plates) => {
                    let number = self.plates.get(self.active_plate).map(|p| p.number);
                    self.plates = plates;
                    self.invalidate_plate_bounds();
                    self.active_plate = number
                        .and_then(|n| self.plates.iter().position(|p| p.number == n))
                        .unwrap_or(0)
                        .min(self.plates.len().saturating_sub(1));
                    self.sync_plate_scene();
                }
                Err(err) => self.error = Some(format!("{err:#}").into()),
            }
        }
        if self.selected.is_some_and(|key| {
            self.project
                .as_ref()
                .is_none_or(|p| p.instance(key).is_err())
        }) {
            self.selected = None;
        }
        self.selected_volume = self.selected_volume.filter(|&i| {
            self.selected
                .and_then(|k| self.project.as_ref()?.object(k.object).ok())
                .is_some_and(|o| i < o.volumes.len())
        });
        self.selected_instances.retain(|k| {
            self.project
                .as_ref()
                .is_some_and(|p| p.instance(*k).is_ok())
        });
        self.refresh_modifiers();
        self.sync_selection(window, cx);
        cx.notify();
    }

    /// Рамки вибраних об’єктів показаної пластини (мультивибір) на «Підготовці».
    pub fn update_highlight(&mut self) {
        self.sync_tool();
        let number = self.plates.get(self.active_plate).map(|p| p.number);
        let mut boxes = Vec::new();
        if self.page == crate::workspace::Page::Prepare
            && let Some(project) = self.project.as_ref()
        {
            // Первинний — з урахуванням живого перетягування й вибраної частини.
            if let Some(key) = self.selected
                && project
                    .instance(key)
                    .is_ok_and(|i| Some(i.plate.unwrap_or(0)) == number)
                && let Some(b) = self.scene_selected_bounds()
            {
                boxes.push((b.min.as_vec3(), b.max.as_vec3()));
            }
            for key in &self.selected_instances {
                if Some(*key) == self.selected {
                    continue;
                }
                let Ok(instance) = project.instance(*key) else {
                    continue;
                };
                if Some(instance.plate.unwrap_or(0)) != number {
                    continue;
                }
                if let Some(mut b) = self.cached_instance_bounds(*key) {
                    let offset = self.plate_offset(instance.plate.unwrap_or(0)).as_dvec3();
                    b.min += offset;
                    b.max += offset;
                    boxes.push((b.min.as_vec3(), b.max.as_vec3()));
                }
            }
        }
        self.renderer.set_highlights(boxes);
        // Наведення перерахується з наступним рухом миші.
        self.hovered = None;
        self.renderer.set_hover(None);
        self.dirty = true;
    }

    /// Бліда рамка під курсором; кадр перерендерюється лише коли обʼєкт під курсором змінився.
    pub fn update_hover(&mut self, position: Option<gpui::Point<gpui::Pixels>>) -> bool {
        let hit = if self.page == crate::workspace::Page::Prepare {
            position.and_then(|p| self.hit_test(p))
        } else {
            None
        };
        self.set_hovered(hit)
    }

    /// Наведення з будь-якого боку — зі сцени чи з дерева; вибраний обʼєкт уже має свою рамку.
    pub fn set_hovered(&mut self, key: Option<InstanceRef>) -> bool {
        let key =
            key.filter(|key| !self.selected_instances.contains(key) && Some(*key) != self.selected);
        if key == self.hovered {
            return false;
        }
        self.hovered = key;
        let bounds = key.and_then(|key| {
            let mut b = self.cached_instance_bounds(key)?;
            let offset = self.instance_offset(key).as_dvec3();
            b.min += offset;
            b.max += offset;
            Some(b)
        });
        self.renderer
            .set_hover(bounds.map(|b| (b.min.as_vec3(), b.max.as_vec3())));
        self.dirty = true;
        true
    }

    /// Рамка в сцені й значення в полях — з поточного вибору.
    pub fn sync_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.update_highlight();
        self.refresh_tool_panel(window, cx);
        let bounds = self.selected_bounds();
        if let (Some(b), Some(inputs)) = (bounds, &self.transform_inputs) {
            let center = b.center();
            let scale = self
                .selected
                .and_then(|key| self.project.as_ref()?.instance(key).ok())
                .map(|i| i.transform.matrix3.x_axis.length() * 100.0)
                .unwrap_or(100.0);
            let values = [
                (&inputs.x, format!("{:.2}", center.x)),
                (&inputs.y, format!("{:.2}", center.y)),
                (&inputs.z_rot, "0".to_string()),
                (&inputs.scale, format!("{scale:.1}")),
            ];
            for (input, value) in values {
                input.update(cx, |state, cx| state.set_value(value, window, cx));
            }
        }
    }

    pub fn select_instance(
        &mut self,
        key: Option<InstanceRef>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(key) = key {
            let number = self
                .project
                .as_ref()
                .and_then(|p| p.instance(key).ok())
                .map(|i| i.plate.unwrap_or(0));
            if let Some(index) = self.plates.iter().position(|p| Some(p.number) == number) {
                self.show_plate(index);
            }
        }
        self.selected = key;
        self.selected_volume = None;
        self.selected_instances = key.into_iter().collect();
        self.refresh_modifiers();
        self.sync_selection(window, cx);
        cx.notify();
    }

    /// Виконує редагування вибраного інстансу й показує помилку сповіщенням.
    pub(crate) fn edit_selected(
        &mut self,
        label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Project, InstanceRef) -> anyhow::Result<Option<InstanceRef>>,
    ) {
        self.checkpoint(label);
        let (Some(project), Some(key)) = (self.project.as_mut(), self.selected) else {
            return;
        };
        match edit(project, key) {
            Ok(next) => {
                if self.selected != next {
                    self.selected_volume = None;
                    self.selected_instances = next.into_iter().collect();
                }
                self.selected = next;
                self.project_changed(window, cx);
            }
            Err(err) => self.toast(ToastKind::Error, format!("{err:#}"), cx),
        }
    }

    pub(crate) fn transform_scrub_binding(
        &self,
        id: gpui::EntityId,
    ) -> Option<crate::scrub::Binding> {
        use crate::scrub::*;
        let inputs = self.transform_inputs.as_ref()?;
        let (target, step) = if id == inputs.x.entity_id() {
            (Transform::Position(0), 0.1)
        } else if id == inputs.y.entity_id() {
            (Transform::Position(1), 0.1)
        } else if id == inputs.z_rot.entity_id() {
            (Transform::Rotate(2), 1.0)
        } else if id == inputs.scale.entity_id() {
            (Transform::UniformPercent, 1.0)
        } else {
            return None;
        };
        let mut spec = Spec::unit(step);
        if matches!(target, Transform::UniformPercent) {
            spec.min = 0.1;
            spec.max = 10000.;
        }
        Some(Binding {
            spec,
            target: Target::Transform(target),
        })
    }

    fn apply_field(
        &mut self,
        field: Field,
        value: f64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !value.is_finite() {
            return;
        }
        // Values shown with two/one decimals must not mutate on an unchanged blur.
        if let Some(bounds) = self.selected_bounds() {
            let unchanged = match field {
                Field::X => (value - bounds.center().x).abs() < 0.0051,
                Field::Y => (value - bounds.center().y).abs() < 0.0051,
                Field::Rotate => value.abs() < 1e-10,
                Field::Scale => self
                    .selected
                    .and_then(|k| self.project.as_ref()?.instance(k).ok())
                    .is_some_and(|i| {
                        (value - i.transform.matrix3.x_axis.length() * 100.0).abs() < 0.051
                    }),
            };
            if unchanged {
                return;
            }
        }
        let label = match field {
            Field::X | Field::Y => "Переміщення",
            Field::Rotate => "Поворот",
            Field::Scale => "Масштабування",
        };
        self.edit_selected(label, window, cx, |project, key| {
            let bounds = project.instance_bounds(key)?.context("порожній об’єкт")?;
            match field {
                Field::X => {
                    project.translate(key, DVec3::new(value - bounds.center().x, 0.0, 0.0))?
                }
                Field::Y => {
                    project.translate(key, DVec3::new(0.0, value - bounds.center().y, 0.0))?
                }
                Field::Rotate => {
                    project.rotate(key, DQuat::from_rotation_z(value.to_radians()))?;
                    project.drop_to_bed(key)?;
                }
                Field::Scale => {
                    anyhow::ensure!(
                        value > 0.1 && value < 10_000.0,
                        "масштаб має бути від 0.1 до 10000 %"
                    );
                    let current = project.instance(key)?.transform.matrix3.x_axis.length();
                    let factor = value / 100.0 / current;
                    project.scale(key, DVec3::splat(factor))?;
                    project.drop_to_bed(key)?;
                }
            }
            Ok(Some(key))
        });
    }

    fn rotate_selected(&mut self, axis: DVec3, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_selected("Поворот", window, cx, |project, key| {
            project.rotate(
                key,
                DQuat::from_axis_angle(axis, std::f64::consts::FRAC_PI_2),
            )?;
            project.drop_to_bed(key)?;
            Ok(Some(key))
        });
    }

    pub(crate) fn arrange_plate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(number) = self
            .plates
            .get(self.active_plate)
            .and_then(|p| plate_key(p.number))
        else {
            return;
        };
        let (bed_lo, bed_hi) = self.bed_bounds();
        self.checkpoint("Розкладання");
        let Some(project) = self.project.as_mut() else {
            return;
        };
        // Пакування йде від кута столу; як у Studio, зсуваємо групу в центр.
        let arranged = project
            .arrange_on_bed(number, 6.0, bed_lo, bed_hi)
            .and_then(|()| {
                let items = project.plate_bounds(Some(number))?;
                let (lo, hi) = items
                    .iter()
                    .fold((DVec3::MAX, DVec3::MIN), |(lo, hi), (_, b)| {
                        (lo.min(b.min), hi.max(b.max))
                    });
                let shift = DVec3::new(
                    (bed_lo[0] + bed_hi[0] - lo.x - hi.x) / 2.0,
                    (bed_lo[1] + bed_hi[1] - lo.y - hi.y) / 2.0,
                    0.0,
                );
                for (key, _) in items {
                    project.translate(key, shift)?;
                }
                Ok(())
            });
        match arranged {
            Ok(()) => self.project_changed(window, cx),
            Err(err) => self.toast(
                ToastKind::Error,
                crate::tr_format!("Розкласти не вдалося: {err:#}"),
                cx,
            ),
        }
    }

    pub fn add_plate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.checkpoint("Додавання пластини");
        let Some(project) = self.project.as_mut() else {
            return;
        };
        match project.add_plate("") {
            Ok(index) => {
                self.project_changed(window, cx);
                if let Some(i) = self.plates.iter().position(|p| p.number == index) {
                    self.show_plate(i);
                }
            }
            Err(err) => self.toast(ToastKind::Error, format!("{err:#}"), cx),
        }
    }

    pub(crate) fn delete_plate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(number) = self
            .plates
            .get(self.active_plate)
            .and_then(|p| plate_key(p.number))
        else {
            return;
        };
        // Як у Bambu Studio й Orca: пластина зникає разом зі своїми об’єктами. Якщо лишити
        // їх «поза пластиною», вони стоять на місці видаленої, а наступні пластини зсуваються
        // на її слот поверх них — і здається, що видалено останню пластину.
        self.mutate("Видалення пластини", window, cx, move |p| {
            let members: Vec<_> = p
                .objects
                .iter()
                .flat_map(|o| {
                    o.instances.iter().enumerate().filter_map(move |(instance, i)| {
                        (i.plate == Some(number)).then_some(InstanceRef { object: o.id, instance })
                    })
                })
                .collect();
            p.delete_instances(&members)?;
            p.delete_plate(number)?;
            Ok(Some(Vec::new()))
        });
    }

    /// 3MF збережено в Bambu Studio: Orca порівнює його версію зі своєю, хоча це різні програми.
    pub fn project_from_bambu_studio(&self) -> bool {
        self.project.as_ref().is_some_and(project::Project::from_bambu_studio)
    }

    /// «Зберегти проєкт» (Ctrl+S): перший раз — діалог, після підтвердження — мовчки туди ж.
    pub fn prompt_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_with(false, None, window, cx);
    }

    /// «Зберегти як…» (Ctrl+Shift+S): завжди діалог; новий шлях стає підтвердженим.
    pub fn prompt_save_as(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_with(true, None, window, cx);
    }

    /// Зберегти (діалог чи мовчки — `save_flow::save_action`) і, якщо збережено, виконати `then`.
    pub fn prompt_save_then(
        &mut self,
        then: Option<crate::file_menu::AfterSave>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.save_with(false, then, window, cx);
    }

    fn save_with(
        &mut self,
        force_dialog: bool,
        then: Option<crate::file_menu::AfterSave>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.project.is_none() {
            return;
        }
        let action = crate::save_flow::save_action(
            self.path.as_deref(),
            self.save_target.as_deref(),
            &self.library_dir,
            force_dialog,
        );
        let (dir, name) = match action {
            None => return,
            Some(crate::save_flow::SaveAction::Silent(target)) => {
                self.write_project(target, then, window, cx);
                return;
            }
            Some(crate::save_flow::SaveAction::Dialog { dir, name }) => (dir, name),
        };
        let target = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(target))) = target.await else {
                return;
            };
            let target = crate::save_flow::normalize_target(target);
            this.update_in(cx, |this, window, cx| this.write_project(target, then, window, cx))
                .ok();
        })
        .detach();
    }

    /// Записати проєкт у `target`. Успіх — шлях підтверджено, крапка змін знімається;
    /// помилка — повідомлення, і наступний Ctrl+S знову спитає шлях.
    fn write_project(
        &mut self,
        target: PathBuf,
        then: Option<crate::file_menu::AfterSave>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mut project) = self.project.clone() else {
            return;
        };
        project.config = self.effective_project_config();
        match project.save_3mf(&target) {
            Ok(()) => {
                self.model_name = target
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
                    .into();
                self.project = Some(project);
                self.path = Some(target.clone());
                self.save_target = Some(target.clone());
                self.edited = false;
                self.untitled = false;
                self.toast(
                    ToastKind::Success,
                    crate::tr_format!("Збережено {}", crate::shorten_home(&target)),
                    cx,
                );
                if let Some(then) = then {
                    then(self, window, cx);
                }
            }
            Err(err) => {
                self.save_target = None;
                self.toast(
                    ToastKind::Error,
                    crate::tr_format!(
                        "Не вдалося зберегти {path}: {err:#}",
                        path = crate::shorten_home(&target)
                    ),
                    cx,
                );
            }
        }
        cx.notify();
    }

    pub fn ray_at(&self, position: gpui::Point<gpui::Pixels>) -> Option<(glam::Vec3, glam::Vec3)> {
        let bounds = self.viewport_bounds?;
        let local = position - bounds.origin;
        let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        if w <= 0.0 || h <= 0.0 {
            return None;
        }
        let ndc = glam::Vec2::new(
            f32::from(local.x) / w * 2.0 - 1.0,
            1.0 - f32::from(local.y) / h * 2.0,
        );
        Some(self.camera.ray(ndc, w / h))
    }

    /// Найближча рамка об’єкта на промені камери.
    pub fn hit_test(&self, position: gpui::Point<gpui::Pixels>) -> Option<InstanceRef> {
        let (origin, dir) = self.ray_at(position)?;
        // Рамки з кешу: hover викликає це на кожен рух миші, обхід вершин тут неприйнятний.
        let project = self.project.as_ref()?;
        project
            .objects
            .iter()
            .flat_map(|o| {
                (0..o.instances.len()).map(|instance| InstanceRef {
                    object: o.id,
                    instance,
                })
            })
            .filter_map(|key| Some((key, self.cached_instance_bounds(key)?)))
            .filter_map(|(key, b)| {
                let offset = self.instance_offset(key);
                ray_box(origin - offset, dir, b.min.as_vec3(), b.max.as_vec3()).map(|t| (t, key))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, key)| key)
    }

    /// Точка на площині столу (Z=0) під курсором.
    pub fn bed_point(&self, position: gpui::Point<gpui::Pixels>) -> Option<glam::Vec3> {
        let (origin, dir) = self.ray_at(position)?;
        if dir.z.abs() < 1e-4 {
            return None;
        }
        let t = -origin.z / dir.z;
        (t > 0.0).then(|| origin + dir * t)
    }

    /// Натискання лівої кнопки в сцені «Підготовки»: на об’єкті — вибір і початок перетягування.
    pub fn begin_pointer(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        modifiers: gpui::Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(key) = self.hit_test(position) else {
            if let Some(index) = self.plate_at(position) {
                self.show_plate(index);
                self.drag_from = Some(position);
                cx.notify();
            }
            return false;
        };
        // Ctrl/Shift+клік додає чи прибирає об’єкт із вибору, не починаючи перетягування.
        if modifiers.control || modifiers.shift {
            self.toggle_instance(key, window, cx);
            self.drag_from = Some(position);
            self.moving = None;
            return false;
        }
        if self.selected != Some(key) {
            self.select_instance(Some(key), window, cx);
        }
        self.drag_from = Some(position);
        // Площина різання рухається незалежно від об’єкта.
        // Об’єкти заблокованої пластини лише вибираються, як у Bambu Studio.
        let locked = self.project.as_ref().is_some_and(|p| {
            p.instance(key)
                .ok()
                .and_then(|i| i.plate)
                .is_some_and(|n| p.plate_locked(n))
        });
        self.moving = if self.preferences.left_drag_moves
            && !locked
            && self.tool != Some(crate::tools::Tool::Cut)
        {
            self.bed_point(position).map(|p| (key, p))
        } else {
            None
        };
        if self.moving.is_some() {
            self.checkpoint("Переміщення");
        }
        self.moving.is_some()
    }

    /// Рух перетягуваного об’єкта: зсуваємо по площині столу й оновлюємо лише сітку пластини.
    pub fn drag_selected(&mut self, position: gpui::Point<gpui::Pixels>) {
        let (Some((key, from)), Some(to)) = (self.moving, self.bed_point(position)) else {
            return;
        };
        self.ensure_live_drag(key);
        let Some(project) = self.project.as_mut() else {
            return;
        };
        let delta = to - from;
        let shift =
            project::DAffine3::from_translation(DVec3::new(delta.x as f64, delta.y as f64, 0.0));
        if let Some(index) = self.selected_volume {
            let Ok(instance) = project.instance(key) else {
                return;
            };
            let transform = instance.transform;
            let Some(volume) = project
                .objects
                .iter_mut()
                .find(|o| o.id == key.object)
                .and_then(|o| o.volumes.get_mut(index))
            else {
                return;
            };
            volume.transform = transform.inverse() * shift * transform * volume.transform;
        } else if project.translate(key, shift.translation).is_err() {
            return;
        }
        self.moving = Some((key, to));
        self.edited = true;
        self.live_drag_moved(key);
    }

    /// Перед першою зміною перетягування: рухомий інстанс — окрема GPU-сітка,
    /// решта пластини — без нього. Далі кожен рух міняє лише матрицю.
    pub(crate) fn ensure_live_drag(&mut self, key: project::InstanceRef) {
        if self.live_drag.as_ref().is_some_and(|d| d.key == key) {
            return;
        }
        let _profile = settings::profile::Span::new("live_drag_begin");
        // Булеве прев’ю — одна сітка на пластину; під час жесту частини рухаються окремо,
        // тож переходимо на них одразу, інакше стара булева сітка лишається «привидом».
        if self.boolean_mesh.take().is_some() {
            self.sync_plate_scene();
        }
        let Some(project) = &self.project else { return };
        let Ok(instance) = project.instance(key) else {
            return;
        };
        if self
            .selected_volume
            .and_then(|index| project.object(key.object).ok()?.volumes.get(index))
            .is_some_and(|volume| volume.kind != project::VolumeKind::Part)
        {
            return;
        }
        let number = instance.plate.unwrap_or(0);
        let Some(bounds) = self.selected_bounds() else {
            return;
        };
        let start = instance.transform
            * self
                .selected_volume
                .and_then(|i| {
                    project
                        .object(key.object)
                        .ok()?
                        .volumes
                        .get(i)
                        .map(|v| v.transform)
                })
                .unwrap_or(project::DAffine3::IDENTITY);
        let offset = self.plate_offset(number);
        self.live_drag = Some(LiveDrag {
            key,
            volume: self.selected_volume,
            start,
            offset,
            bounds,
        });
    }

    /// Після зміни трансформації під час перетягування: лише матриця й рамка.
    pub(crate) fn live_drag_moved(&mut self, key: project::InstanceRef) {
        let Some(drag) = self.live_drag.as_ref().filter(|d| d.key == key) else {
            self.refresh_plate_mesh();
            return;
        };
        let Some(current) = self
            .project
            .as_ref()
            .and_then(|p| p.instance(key).ok())
            .and_then(|i| {
                Some(
                    i.transform
                        * match drag.volume {
                            Some(index) => {
                                self.project
                                    .as_ref()?
                                    .object(key.object)
                                    .ok()?
                                    .volumes
                                    .get(index)?
                                    .transform
                            }
                            None => project::DAffine3::IDENTITY,
                        },
                )
            })
        else {
            return;
        };
        let delta = current * drag.start.inverse();
        let matrix = glam::Mat4::from_translation(drag.offset) * glam::DMat4::from(delta).as_mat4();
        // Рамка — з кутів стартової, без обходу вершин.
        let (lo, hi) = (drag.bounds.min, drag.bounds.max);
        let (mut min, mut max) = (DVec3::MAX, DVec3::MIN);
        for i in 0..8 {
            let corner = DVec3::new(
                if i & 1 == 0 { lo.x } else { hi.x },
                if i & 2 == 0 { lo.y } else { hi.y },
                if i & 4 == 0 { lo.z } else { hi.z },
            );
            let p = delta.transform_point3(corner);
            min = min.min(p);
            max = max.max(p);
        }
        let offset = drag.offset;
        let _ = matrix;
        // Частини малюються окремими сітками: рух — лише нові матриці елементів сцени.
        self.sync_scene_items();
        self.renderer
            .set_highlight(Some((min.as_vec3() + offset, max.as_vec3() + offset)));
        self.cancel_slice();
        self.preview = None;
        self.invalidate_geometry_live();
        self.dirty = true;
    }

    /// Кінець або переривання перетягування: одна повна перебудова сітки.
    pub(crate) fn finish_live_drag(&mut self) {
        if self.live_drag.is_some() {
            self.refresh_plate_mesh();
        }
    }

    /// Після живої зміни трансформації: нова сітка показаної пластини й рамка вибору.
    pub fn refresh_plate_mesh(&mut self) {
        let _profile = settings::profile::Span::new("refresh_plate_mesh");
        if self.live_drag.take().is_some() {
            self.renderer.remove_scene_mesh(viewer::DRAG_MESH);
        }

        // Invalidate immediately: releasing outside the window or changing pages
        // must never leave printable G-code from before a live geometry edit.
        self.cancel_slice();
        self.preview = None;
        self.invalidate_geometry();
        self.sync_plate_scene();
        self.refresh_modifiers();
        self.update_highlight();
    }

    pub fn end_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_live_drag();
        let Some((key, _)) = self.moving.take() else {
            return;
        };
        if !self.drag_moved {
            self.finish_checkpoint();
            return;
        }
        {
            // Preserve the world position when changing the local plate coordinate system.
            if self.selected_volume.is_none() {
                let source = self.instance_offset(key);
                let center = self
                    .project
                    .as_ref()
                    .and_then(|p| p.instance_bounds(key).ok().flatten())
                    .map(|b| b.center().as_vec3() + source);
                if let Some(center) = center {
                    let (lo, hi) = self.bed_bounds();
                    let target = self.plates.iter().filter(|p| p.number > 0).find_map(|p| {
                        let q = center - self.plate_offset(p.number);
                        (q.x >= lo[0] as f32
                            && q.x <= hi[0] as f32
                            && q.y >= lo[1] as f32
                            && q.y <= hi[1] as f32)
                            .then_some(p.number)
                    });
                    let offset = target.map_or(glam::Vec3::ZERO, |n| self.plate_offset(n));
                    if let Some(project) = &mut self.project {
                        let current = project.instance(key).ok().and_then(|i| i.plate);
                        let locked = target.is_some_and(|n| project.plate_locked(n));
                        if current != target
                            && !locked
                            && project.move_to_plate(key, target).is_ok()
                        {
                            let _ = project.translate(key, (source - offset).as_dvec3());
                        }
                    }
                }
            }
            self.project_changed(window, cx);
            self.select_instance(Some(key), window, cx);
        }
    }
}

/// Стан живого перетягування: стартова трансформація, зсув пластини й рамка (у координатах пластини).
pub struct LiveDrag {
    pub(crate) volume: Option<usize>,
    pub key: project::InstanceRef,
    pub(crate) start: project::DAffine3,
    offset: glam::Vec3,
    pub(crate) bounds: project::Bounds,
}

/// Відстань уздовж променя до перетину з рамкою (slab test).
pub fn ray_box(origin: glam::Vec3, dir: glam::Vec3, lo: glam::Vec3, hi: glam::Vec3) -> Option<f32> {
    let inv = dir.recip();
    let t1 = (lo - origin) * inv;
    let t2 = (hi - origin) * inv;
    let near = t1.min(t2).max_element();
    let far = t1.max(t2).min_element();
    (far >= near.max(0.0)).then_some(near.max(0.0))
}

/// Секція «Об’єкти» лівої панелі.
/// Лівий док «Об’єкти»: дерево пластин і об’єктів (як панель проєкту Zed) і трансформація вибраного.
pub fn panel(
    ws: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui::AnyElement {
    let _profile = settings::profile::Span::new("objects::panel");
    let z = zed();
    let number = ws.plates.get(ws.active_plate).map_or(0, |p| p.number);
    let on_plate = number > 0 && ws.project.is_some();
    let header = ui::panel_header(
        "Об’єкти",
        vec![
            ui::icon_button("plate-add", IconName::Plus, "Додати пластину")
            .inert(ws.project.is_none())
            .when(ws.project.is_some(), |d| {
                d.on_click(cx.listener(|this, _, window, cx| this.add_plate(window, cx)))
            })
            .into_any_element(),
        ],
    );
    let Some(project) = &ws.project else {
        let hint = if ws.path.is_some() {
            "Цей файл відкрито лише для перегляду — редагування розміщення недоступне."
        } else {
            "Відкрий STL або 3MF — тут з’являться пластини й об’єкти."
        };
        return ui::dock()
            .child(header)
            .child(
                div()
                    .p(px(10.))
                    .text_sm()
                    .text_color(z.text_muted)
                    .child(hint),
            )
            .into_any_element();
    };

    let mut rows: Vec<gpui::AnyElement> = Vec::new();
    for (plate_index, plate) in ws.plates.iter().enumerate() {
        let active = plate_index == ws.active_plate;
        let members: Vec<(InstanceRef, String, bool)> = project
            .objects
            .iter()
            .flat_map(|object| {
                object
                    .instances
                    .iter()
                    .enumerate()
                    .filter(move |(_, instance)| instance.plate.unwrap_or(0) == plate.number)
                    .map(move |(index, instance)| {
                        let name = if object.instances.len() > 1 {
                            format!("{} #{}", object.name, index + 1)
                        } else {
                            object.name.clone()
                        };
                        (
                            InstanceRef {
                                object: object.id,
                                instance: index,
                            },
                            name,
                            instance.printable,
                        )
                    })
            })
            .collect();
        rows.push(
            ui::list_item(("plate-row", plate_index), 0, false)
                .text_color(if active { z.text } else { z.text_muted })
                .child(ui::icon(IconName::ChevronDown).text_color(z.text_placeholder))
                .child(
                    ui::icon(if plate.number > 0 {
                        IconName::Layers2
                    } else {
                        IconName::Boxes
                    })
                    .text_color(if active { z.accent } else { z.text_muted }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(crate::i18n::plate_title(plate))),
                )
                // Дії з пластиною — лише під курсором, щоб не захаращувати дерево.
                .when(plate.number > 0, |d| {
                    let group = SharedString::from(format!("plate-row-{plate_index}"));
                    d.group(group.clone()).child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .opacity(0.)
                            .group_hover(group, |s| s.opacity(1.))
                            .child(
                                ui::icon_button(("plate-arrange", plate_index), IconName::LayoutGrid, "Розкласти пластину")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.show_plate(plate_index);
                                        this.arrange_plate(window, cx);
                                    })),
                            )
                            .child(
                                ui::icon_button(("plate-rename", plate_index), IconName::Pencil, "Перейменувати пластину")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.show_plate(plate_index);
                                        this.prompt_rename_plate(window, cx);
                                    })),
                            )
                            .child(
                                ui::icon_button(("plate-del", plate_index), IconName::Minus, "Видалити пластину")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.show_plate(plate_index);
                                        this.delete_plate(window, cx);
                                    })),
                            ),
                    )
                })
                .child(
                    div()
                        .text_xs()
                        .text_color(z.text_placeholder)
                        .child(members.len().to_string()),
                )
                .when(plate.number > 0, |d| {
                    d.on_mouse_up(
                        gpui::MouseButton::Right,
                        cx.listener(move |this, e: &gpui::MouseUpEvent, window, cx| {
                            this.open_plate_menu(plate_index, e.position, window, cx);
                            cx.stop_propagation();
                        }),
                    )
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.show_plate(plate_index);
                    cx.notify();
                }))
                .into_any_element(),
        );
        for (key, name, printable) in members {
            let size = if ws.selected == Some(key) && ws.live_drag.is_some() {
                ws.selected_bounds()
            } else {
                ws.cached_instance_bounds(key)
            }
            .map(|b| b.size())
            .unwrap_or_default();
            let selected = ws.selected_instances.contains(&key) && ws.selected_volume.is_none();
            let object = project.object(key.object).ok();
            let ranges = project.object_layer_ranges(key.object).unwrap_or_default();
            let has_children = object.is_some_and(|o| o.volumes.len() > 1) || !ranges.is_empty();
            let collapsed = ws.collapsed_objects.contains(&key);
            let hovered = ws.hovered == Some(key);
            let on_active = plate_index == ws.active_plate;
            rows.push(
                ui::list_item(
                    SharedString::from(format!("obj-{}-{}", key.object, key.instance)),
                    1,
                    selected,
                )
                .when(hovered, |d| d.bg(z.element_hover))
                .on_hover(cx.listener(move |this, inside: &bool, _, cx| {
                    let changed = if *inside && on_active {
                        this.set_hovered(Some(key))
                    } else if !*inside && this.hovered == Some(key) {
                        this.set_hovered(None)
                    } else {
                        false
                    };
                    if changed {
                        cx.notify();
                    }
                }))
                .when(has_children, |d| {
                    d.child(
                        ui::icon_button(
                            SharedString::from(format!("expand-{}-{}", key.object, key.instance)),
                            if collapsed {
                                IconName::ChevronRight
                            } else {
                                IconName::ChevronDown
                            },
                            "Частини та діапазони",
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.collapsed_objects.remove(&key) {
                                this.collapsed_objects.insert(key);
                            }
                            cx.stop_propagation();
                            cx.notify();
                        })),
                    )
                })
                .child(
                    ui::icon(if has_children {
                        IconName::Boxes
                    } else {
                        IconName::Box
                    })
                    .text_color(if selected {
                        z.accent
                    } else {
                        z.text_muted
                    }),
                )
                .child(div().flex_1().min_w_0().truncate().child(name))
                .when(!printable, |d| {
                    d.child(ui::icon(IconName::EyeOff).text_color(z.warning))
                        .tooltip(|window, cx| Tooltip::new("Не друкується").build(window, cx))
                })
                .child(
                    div()
                        .font_family(crate::theme::MONO_FONT)
                        .text_xs()
                        .text_color(z.text_placeholder)
                        .child(format!("{:.0}×{:.0}×{:.0}", size.x, size.y, size.z)),
                )
                .on_mouse_up(
                    gpui::MouseButton::Right,
                    cx.listener(move |this, e: &gpui::MouseUpEvent, window, cx| {
                        if this.active_plate != plate_index {
                            this.show_plate(plate_index);
                        }
                        this.open_object_menu(key, e.position, window, cx);
                        cx.stop_propagation();
                    }),
                )
                .on_click(
                    cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                        if this.active_plate != plate_index {
                            this.show_plate(plate_index);
                        }
                        if event.modifiers().control || event.modifiers().shift {
                            this.toggle_instance(key, window, cx);
                        } else {
                            this.select_instance(Some(key), window, cx);
                        }
                    }),
                )
                .into_any_element(),
            );
            if has_children && !collapsed {
                if let Some(object) = object {
                    for (index, volume) in object.volumes.iter().enumerate() {
                        let part_selected =
                            ws.selected == Some(key) && ws.selected_volume == Some(index);
                        let hint = format!(
                            "{} · {}",
                            volume.name,
                            crate::assembly::role_name(volume.kind)
                        );
                        rows.push(
                            ui::list_item(
                                SharedString::from(format!(
                                    "part-{}-{}-{index}",
                                    key.object, key.instance
                                )),
                                2,
                                part_selected,
                            )
                            .child(
                                ui::icon(if volume.kind == project::VolumeKind::Part {
                                    IconName::Box
                                } else {
                                    IconName::SquareDashed
                                })
                                .text_color(
                                    if volume.kind == project::VolumeKind::NegativeVolume {
                                        z.warning
                                    } else {
                                        z.accent
                                    },
                                ),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(volume.name.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(z.text_muted)
                                    .child(crate::i18n::tr(crate::assembly::role_name(
                                        volume.kind,
                                    ))),
                            )
                            .tooltip(move |window, cx| Tooltip::new(hint.clone()).build(window, cx))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if this.active_plate != plate_index {
                                    this.show_plate(plate_index);
                                }
                                this.choose_volume(key, index, window, cx);
                            }))
                            .into_any_element(),
                        );
                    }
                }
                for (index, range) in ranges.iter().enumerate() {
                    let label = crate::tr_format!("{:.2}–{:.2} мм", range.min_z, range.max_z);
                    let height = range
                        .config
                        .get("layer_height")
                        .map(|s| format!("h {s}"))
                        .unwrap_or_default();
                    rows.push(
                        ui::list_item(
                            SharedString::from(format!("range-{}-{index}", key.object)),
                            2,
                            false,
                        )
                        .child(ui::icon(IconName::Layers).text_color(z.accent))
                        .child(div().flex_1().child(label))
                        .child(div().text_xs().text_color(z.text_muted).child(height))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if this.active_plate != plate_index {
                                this.show_plate(plate_index);
                            }
                            this.select_instance(Some(key), window, cx);
                            this.open_scoped_settings(Some(index), window, cx);
                        }))
                        .into_any_element(),
                    );
                }
            }
        }
    }

    let mut dock = ui::dock().child(header).child(
        div()
            .id("objects-tree")
            .flex_1()
            .min_h_0()
            .py(px(4.))
            .overflow_y_scroll()
            .children(rows),
    );

    if ws.selected.is_some()
        && ws.selected_volume.is_none()
        && ws.selected_instances.len() <= 1
        && let Some(inputs) = &ws.transform_inputs
    {
        let field = |label: &'static str, input: &Entity<InputState>, suffix: &'static str| {
            crate::scrub::zone(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .w(px(14.))
                            .text_xs()
                            .text_color(z.text_muted)
                            .child(label),
                    )
                    .child(crate::scrub::field(
                        input,
                        Input::new(input).suffix(
                            div()
                                .text_xs()
                                .text_color(z.text_placeholder)
                                .child(crate::i18n::tr(suffix)),
                        ),
                        window,
                        cx,
                    )),
                input,
                window,
                cx,
            )
        };
        let rotate =
            |id: &'static str, axis: DVec3, label: &'static str, cx: &mut Context<Workspace>| {
                ui::ghost_button(id, Some(IconName::RotateCw), label)
                    .text_xs()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.rotate_selected(axis, window, cx)
                    }))
            };
        dock = dock.child(
            div()
                .flex()
                .flex_col()
                .flex_none()
                .pb(px(8.))
                .border_t_1()
                .border_color(z.border_variant)
                .child(
                    ui::group_title("Трансформація")
                        .child(div().flex_1())
                        .child(
                            ui::icon_button("drop", IconName::ArrowDownToLine, "Покласти на стіл")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.edit_selected(
                                        "Покласти на стіл",
                                        window,
                                        cx,
                                        |p, k| p.drop_to_bed(k).map(|_| Some(k)),
                                    )
                                })),
                        )
                        .child(
                            ui::icon_button("center", IconName::Crosshair, "По центру пластини")
                                .inert(!on_plate)
                                .when(on_plate, |d| {
                                    d.on_click(cx.listener(|this, _, window, cx| {
                                        let (lo, hi) = this.bed_bounds();
                                        this.edit_selected(
                                            "Центрування",
                                            window,
                                            cx,
                                            |p, k| p.center_on_bed(k, lo, hi).map(|_| Some(k)),
                                        )
                                    }))
                                }),
                        )
                        .child(
                            ui::icon_button("dup", IconName::Copy, "Дублювати (Ctrl+D)").on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.duplicate_selected(window, cx)
                                }),
                            ),
                        )
                        .child(
                            ui::danger_icon_button("delete", IconName::Trash, "Видалити (Delete)")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.delete_selected(window, cx)
                                })),
                        ),
                )
                .child(
                    div()
                        .grid()
                        .grid_cols(2)
                        .gap(px(8.))
                        .px(px(10.))
                        .child(field("X", &inputs.x, "мм"))
                        .child(field("Y", &inputs.y, "мм"))
                        .child(field("↻", &inputs.z_rot, "°"))
                        .child(field("⤢", &inputs.scale, "%")),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(2.))
                        .px(px(6.))
                        .pt(px(6.))
                        .child(rotate("rot-x", DVec3::X, "X 90°", cx))
                        .child(rotate("rot-y", DVec3::Y, "Y 90°", cx))
                        .child(rotate("rot-z", DVec3::Z, "Z 90°", cx)),
                )
        );
    }
    dock.into_any_element()
}

/// Підтвердження перед відкриттям іншого файлу, якщо є незбережені зміни.
pub fn confirm_discard(
    ws: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
    then: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) -> bool {
    if !ws.edited {
        return true;
    }
    ws.confirm(
        "Незбережені зміни",
        "Незбережені зміни проєкту буде втрачено.",
        "Відкинути",
        move |ws, window, cx| {
            ws.edited = false;
            then(ws, window, cx)
        },
        window,
        cx,
    );
    false
}

impl Workspace {
    /// Рамка інстанса з кешу. Кеш зберігає рамку без зсуву інстанса, тож перенесення
    /// (найчастіша правка й живе перетягування) нічого не перераховує.
    pub(crate) fn cached_instance_bounds(&self, key: InstanceRef) -> Option<project::Bounds> {
        let project = self.project.as_ref()?;
        project::bounds_cache::cached_instance(project, &self.instance_bounds_cache, key)
    }

    pub(crate) fn cached_volume_bounds(&self, key: InstanceRef, index: usize) -> Option<project::Bounds> {
        let project = self.project.as_ref()?;
        project::bounds_cache::cached_volume(project, &self.volume_bounds_cache, key, index)
    }

    /// Оновлює лише записи, чия форма змінилась (меш, трансформації частин, поворот чи
    /// масштаб інстанса); зниклі інстанси й частини прибирає.
    pub(crate) fn rebuild_instance_bounds(&mut self) {
        let Some(project) = &self.project else {
            self.instance_bounds_cache.clear();
            self.volume_bounds_cache.clear();
            return;
        };
        let (instances, volumes) = project::bounds_cache::refresh(
            project,
            std::mem::take(&mut self.instance_bounds_cache),
            std::mem::take(&mut self.volume_bounds_cache),
        );
        self.instance_bounds_cache = instances;
        self.volume_bounds_cache = volumes;
    }

    /// `printable_area` вибраного принтера (з правками в редакторі). Bambu розставляє
    /// пластини у файлі з кроком «стіл × 1.2» саме цього принтера, тож проєкт перед
    /// записом має знати його стіл, а не стіл, з яким файл колись відкрили.
    fn machine_printable_area(&self) -> Option<serde_json::Value> {
        let mut values = self
            .presets
            .as_ref()
            .and_then(|s| s.flatten(slicer::Kind::Machine, &self.selection.machine).ok())
            .unwrap_or_default();
        if let Some(editor) = &self.machine_editor {
            values.extend(editor.overrides());
        }
        values.remove("printable_area")
    }

    /// Копія проєкту для запису у файл: стіл — поточного принтера.
    pub(crate) fn project_for_write(&self, mut project: project::Project) -> project::Project {
        if let Some(area) = self.machine_printable_area() {
            project.config.insert("printable_area".into(), area);
        }
        project
    }

    pub(crate) fn effective_project_config(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut config = self.effective_project_config_inner();
        if let Some(area) = self.machine_printable_area() {
            config.insert("printable_area".into(), area);
        }
        config
    }

    fn effective_project_config_inner(&self) -> serde_json::Map<String, serde_json::Value> {
        effective_project_config(
            self.project
                .as_ref()
                .map(|p| p.config.clone())
                .unwrap_or_default(),
            &self.selection,
            [
                &self.machine_editor,
                &self.process_editor,
                &self.filament_editor,
            ]
            .into_iter()
            .flatten()
            .map(|editor| (&editor.preset, editor.overrides())),
        )
    }
}

fn effective_project_config<'a>(
    mut config: serde_json::Map<String, serde_json::Value>,
    selection: &slicer::Selection,
    editors: impl IntoIterator<
        Item = (
            &'a settings::EditablePreset<'static>,
            serde_json::Map<String, serde_json::Value>,
        ),
    >,
) -> serde_json::Map<String, serde_json::Value> {
    for (preset, overrides) in editors {
        config.extend(preset.base().clone());
        config.extend(overrides);
    }
    config.insert(
        "printer_settings_id".into(),
        serde_json::json!(selection.machine),
    );
    config.insert(
        "print_settings_id".into(),
        serde_json::json!(selection.process),
    );
    config.insert(
        "filament_settings_id".into(),
        serde_json::json!([selection.filament]),
    );
    config.insert(
        "curr_bed_type".into(),
        serde_json::json!(selection.bed_type),
    );
    config
}

#[cfg(test)]
mod slice_input_tests {
    use super::*;

    #[test]
    fn slice_snapshot_keeps_only_the_sliced_plate() {
        const LO: [f64; 2] = [0.0, 0.0];
        const HI: [f64; 2] = [256.0, 256.0];
        let mut project = Project::default();
        project.add_plate("1").unwrap();
        project.add_plate("2").unwrap();
        let cube = || project::IndexedMesh::cylinder(10.0, 10.0, 8);
        let a = project.add_mesh_object("a", cube(), 1, [50.0; 2], LO, HI).unwrap();
        project.add_mesh_object("b", cube(), 2, [50.0; 2], LO, HI).unwrap();
        // Об’єкт із копіями на обох пластинах: лишається копія пластини 2.
        let copy = project.duplicate_instance(a).unwrap();
        project.move_to_plate(copy, Some(2)).unwrap();
        let schema = crate::preset_editor::schema();
        let mut input = SliceInput {
            project: Some(project.clone()),
            path: Some("p.3mf".into()),
            settings_schema: schema,
        };
        input.keep_only_plate(2);
        let kept = input.project.as_ref().unwrap();
        assert_eq!(kept.plates.len(), 2, "пластини й нумерація ті самі");
        let names: Vec<_> = kept.objects.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
        assert!(kept.objects.iter().all(|o| o.instances.iter().all(|i| i.plate == Some(2))));
        assert_eq!(kept.objects[0].instances.len(), 1);
        // Збережений вхід рушія читається назад з тими самими пластинами.
        let dir = tempfile::tempdir().unwrap();
        let path = input.slice_input(dir.path()).unwrap();
        let loaded = Project::load_3mf(&path).unwrap();
        assert_eq!(loaded.plates.len(), 2);
        assert_eq!(loaded.objects.len(), 2);
        // Оригінал не зачеплено.
        assert_eq!(project.objects.iter().map(|o| o.instances.len()).sum::<usize>(), 3);
        // ID для skip-object у копії пластини ті самі, що дав би повний проєкт.
        let identify = |p: &Project| -> Vec<(String, Option<u32>, String)> {
            p.objects
                .iter()
                .flat_map(|o| {
                    o.instances.iter().filter(|i| i.plate == Some(2)).map(|i| {
                        let id = i.metadata.as_ref().and_then(|m| m.metadata().get("identify_id")?.parse().ok());
                        (o.name.clone(), id, format!("{:?}", i.transform.translation))
                    })
                })
                .collect()
        };
        let full_path = dir.path().join("full.3mf");
        project.save_3mf(&full_path).unwrap();
        let full = identify(&Project::load_3mf(&full_path).unwrap());
        assert!(full.iter().all(|(_, id, _)| id.is_some()), "{full:?}");
        assert_eq!(identify(&loaded), full);
    }
}

#[cfg(test)]
mod settings_save_tests {
    use super::*;
    #[test]
    fn effective_editor_value_survives_save_reload_and_changes_slice_snapshot() {
        let schema = crate::preset_editor::schema();
        let mut editor = settings::EditablePreset::new(
            schema,
            settings::PresetType::Process,
            "process",
            serde_json::from_value(serde_json::json!({"wall_loops":"2"})).unwrap(),
        );
        let selection = slicer::Selection {
            machine: "printer".into(),
            process: "process".into(),
            filament: "filament".into(),
            bed_type: "Textured PEI Plate".into(),
        };
        let build = |editor: &settings::EditablePreset<'static>| {
            let overrides = editor
                .dirty_keys()
                .map(|k| (k.to_owned(), editor.value(k).unwrap().clone()))
                .collect();
            effective_project_config(Default::default(), &selection, [(editor, overrides)])
        };
        let before = build(&editor);
        editor.set("wall_loops", serde_json::json!("5")).unwrap();
        let after = build(&editor);
        assert_ne!(before, after);
        let mut project = Project::default();
        project.add_plate("plate").unwrap();
        project.config = after;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.3mf");
        project.save_3mf(&file).unwrap();
        let loaded = Project::load_3mf(&file).unwrap();
        assert_eq!(loaded.config["wall_loops"], "5");
        assert_eq!(loaded.config["print_settings_id"], "process");
        assert_eq!(slicer::project_selection(&file).unwrap(), selection);
        editor.reset("wall_loops");
        assert_eq!(build(&editor), before);
    }
}

/// Immutable UI snapshot. Migration and serialization only run on the worker.
pub(crate) struct SliceInput {
    pub project: Option<Project>,
    pub path: Option<PathBuf>,
    pub settings_schema: &'static settings::Schema,
}
impl SliceInput {
    /// Лишає в знімку для рушія лише інстанси пластини `plate`; інші пластини лишаються
    /// порожніми (нумерація й крок пластин у файлі ті самі). Рушій ріже одну пластину, але
    /// читав і готував сітки всіх: 4 пластини по 250 тис. △ — `slice::background` 4,6 с,
    /// лише пластина — 3,3 с. G-code той самий. Відкритий проєкт не змінюється.
    pub fn keep_only_plate(&mut self, plate: u32) {
        let Some(project) = &mut self.project else {
            return;
        };
        let others: Vec<project::InstanceRef> = project
            .objects
            .iter()
            .flat_map(|object| {
                object
                    .instances
                    .iter()
                    .enumerate()
                    .filter(|(_, placement)| placement.plate != Some(plate))
                    .map(|(instance, _)| project::InstanceRef {
                        object: object.id,
                        instance,
                    })
            })
            .collect();
        if others.is_empty() {
            return;
        }
        // ID об’єктів (мітки skip-object) — як у повного проєкту, а не перенумеровані.
        project.ensure_identify_ids();
        if let Err(e) = project.delete_instances(&others) {
            eprintln!("[slice] plate-only engine input (plate {plate}): {e:#}");
        }
    }

    /// Serialize the captured project into the request's private directory.
    pub fn slice_input(&self, dir: &Path) -> anyhow::Result<PathBuf> {
        let path = self.path.clone().context("файл не відкрито")?;
        let Some(project) = self.project.as_ref() else {
            return Ok(path);
        };
        std::fs::create_dir_all(dir)?;
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .replace(".gcode", "");
        let out = dir.join(format!("{stem}.3mf"));
        project.save_3mf_for_engine(&out)?;
        Ok(out)
    }

    /// Вхід для нарізки поточним рушієм. Значення, які рушій відхилить (наприклад `-1`
    /// з Bambu у полях Orca з межею від 0), у копії для нарізки замінюються його
    /// типовими; відкритий проєкт не змінюється, а заміни повертаються для показу.
    pub fn prepare(&self, dir: &Path) -> anyhow::Result<(PathBuf, Vec<String>)> {
        let Some(project) = &self.project else {
            return Ok((self.slice_input(dir)?, Vec::new()));
        };
        let (configs, report) = crate::migration::report(project, self.settings_schema);
        let selected: std::collections::HashSet<usize> = report
            .entries
            .iter()
            .enumerate()
            // Лише числові поля поза межами: їх рушій відхиляє. Переліки й прапорці
            // Orca перекладає сама за змістом (`1` → `enabled`, `enabled` → `ensure_all`),
            // а типове значення замість них змінило б поведінку (дуги → вимкнено).
            .filter(|(_, e)| {
                use settings::OptionType::*;
                e.status == settings::MigrationStatus::Invalid
                    && e.proposed_value.is_some()
                    && self.settings_schema.options.get(&e.key).is_some_and(|d| {
                        matches!(
                            d.kind,
                            Float
                                | Floats
                                | Int
                                | Ints
                                | Percent
                                | Percents
                                | FloatOrPercent
                                | FloatsOrPercents
                        )
                    })
            })
            .map(|(i, _)| i)
            .collect();
        if selected.is_empty() {
            return Ok((self.slice_input(dir)?, Vec::new()));
        }
        let migrated = crate::migration::apply(project, &configs, &report, &selected)?;
        let path = self.path.clone().context("файл не відкрито")?;
        std::fs::create_dir_all(dir)?;
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .replace(".gcode", "");
        let out = dir.join(format!("{stem}-compat.3mf"));
        migrated.save_3mf_for_engine(&out)?;
        let mut changes: Vec<String> = selected
            .iter()
            .filter_map(|&i| report.entries.get(i))
            .map(|e| {
                let show = |v: &serde_json::Value| match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                format!(
                    "{}: {} → {}",
                    e.key,
                    show(&e.old_value),
                    e.proposed_value.as_ref().map(show).unwrap_or_default()
                )
            })
            .collect();
        changes.sort();
        changes.dedup();
        Ok((out, changes))
    }
}
