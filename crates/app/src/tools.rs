//! Інструменти сцени «Підготовки», як у Bambu Studio: переміщення (M), поворот (R),
//! масштаб (S) гізмо поверх вибраного об’єкта, «покласти на грань» (F) і різання площиною (C).

use gpui::{Context, Pixels, Point, SharedString, Window, div, prelude::*, px};
use project::{DAffine3, DQuat, InstanceRef};
use viewer::{Gizmo, GizmoKind, Handle, Screen};

use crate::{
    theme::zed,
    ui::{self, IconName, Toggle as _},
    workspace::{Page, Workspace},
};

/// Активний інструмент — `Option<Tool>` у `Workspace`: `None` — звичайний режим вибору.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Move,
    Rotate,
    Scale,
    LayOnFace,
    Cut,
    Measure,
}

impl Tool {
    pub const ALL: [Tool; 6] = [
        Tool::Move,
        Tool::Rotate,
        Tool::Scale,
        Tool::LayOnFace,
        Tool::Cut,
        Tool::Measure,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Tool::Move => "Перемістити",
            Tool::Rotate => "Повернути",
            Tool::Scale => "Масштаб",
            Tool::LayOnFace => "Покласти на грань",
            Tool::Cut => "Розрізати",
            Tool::Measure => "Вимірювання",
        }
    }

    pub fn icon(self) -> IconName {
        match self {
            Tool::Move => IconName::Move3d,
            Tool::Rotate => IconName::Rotate3d,
            Tool::Scale => IconName::Scale3d,
            Tool::LayOnFace => IconName::ArrowDownToLine,
            // Відмінна від «Переріз перед нарізкою» (Scissors) в глобальній групі.
            Tool::Cut => IconName::SquareSplitVertical,
            Tool::Measure => IconName::Ruler,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Tool::Move => "Перемістити (M)",
            Tool::Rotate => "Повернути (R)",
            Tool::Scale => "Масштаб (S)",
            Tool::LayOnFace => "Покласти на грань (F)",
            Tool::Cut => "Розрізати (C)",
            Tool::Measure => "Вимірювання (U)",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Tool::Move => "m",
            Tool::Rotate => "r",
            Tool::Scale => "s",
            Tool::LayOnFace => "f",
            Tool::Cut => "c",
            Tool::Measure => "u",
        }
    }

    fn gizmo(self) -> Option<GizmoKind> {
        match self {
            Tool::Move => Some(GizmoKind::Move),
            Tool::Rotate => Some(GizmoKind::Rotate),
            Tool::Scale => Some(GizmoKind::Scale),
            Tool::LayOnFace | Tool::Measure => None,
            Tool::Cut => Some(GizmoKind::Cut),
        }
    }
}

/// Перетягування ручки гізмо: усе рахується від стану на початку, тож похибка не накопичується.
pub struct GizmoDrag {
    pub(crate) handle: Handle,
    pub(crate) key: InstanceRef,
    start: DAffine3,
    gizmo: Gizmo,
    /// Курсор на початку, пікселі в’юпорту.
    from: glam::Vec2,
    /// Параметр на осі для переміщення.
    axis_t0: f32,
    /// Поточне значення для підказки: мм, градуси чи відсотки.
    pub readout: Option<String>,
}

/// Найближча до променя точка на прямій `origin + axis * t` — параметр `t`.
pub(crate) fn closest_on_axis(
    origin: glam::Vec3,
    axis: glam::Vec3,
    ray_o: glam::Vec3,
    ray_d: glam::Vec3,
) -> Option<f32> {
    let w = origin - ray_o;
    let (a, b, c) = (axis.dot(axis), axis.dot(ray_d), ray_d.dot(ray_d));
    let (d, e) = (axis.dot(w), ray_d.dot(w));
    let denom = a * c - b * b;
    (denom.abs() > 1e-6).then(|| (b * e - c * d) / denom)
}

impl Workspace {
    pub(crate) fn screen(&self) -> Option<Screen<'_>> {
        let b = self.viewport_bounds?;
        Some(Screen {
            camera: &self.camera,
            width: f32::from(b.size.width),
            height: f32::from(b.size.height),
        })
    }

    pub(crate) fn local(&self, position: Point<Pixels>) -> Option<glam::Vec2> {
        let b = self.viewport_bounds?;
        Some(glam::Vec2::new(
            f32::from(position.x - b.origin.x),
            f32::from(position.y - b.origin.y),
        ))
    }

    /// Гізмо поточного інструмента над вибраним об’єктом (якщо він на показаній пластині).
    pub(crate) fn current_gizmo(&self) -> Option<Gizmo> {
        if self.page != Page::Prepare || self.selected_instances.len() > 1 {
            return None;
        }
        let kind = self.tool?.gizmo()?;
        if kind == GizmoKind::Cut {
            return self.cut_gizmo();
        }
        if let Some(drag) = &self.gizmo_drag {
            // Під час перетягування рамка лишається стартовою: ручки не тікають з-під курсора.
            return Some(Gizmo { kind, ..drag.gizmo });
        }
        let key = self.selected?;
        let project = self.project.as_ref()?;
        let number = self.plates.get(self.active_plate)?.number;
        (project.instance(key).ok()?.plate.unwrap_or(0) == number).then_some(())?;
        let b = self.scene_selected_bounds()?;
        let mut gizmo = Gizmo::new(kind, b.min.as_vec3(), b.max.as_vec3(), &self.screen()?);
        if kind == GizmoKind::Scale {
            let object = project.object(key.object).ok()?;
            let (mut lo, mut hi) = (project::DVec3::MAX, project::DVec3::MIN);
            let transform;
            // Локальна рамка під Scale-гізмо — на кожен кадр орбіти; кеш за мешем і
            // трансформацією частини, щоб не обходити вершини щоразу.
            let mut boxes = self.local_box_cache.borrow_mut();
            if boxes.len() > 512 {
                boxes.clear();
            }
            let mut local_box = |volume: &project::Volume, with_transform: bool| {
                use std::hash::{Hash, Hasher};
                let mut hash = std::collections::hash_map::DefaultHasher::new();
                (std::sync::Arc::as_ptr(&volume.mesh) as usize).hash(&mut hash);
                with_transform.hash(&mut hash);
                if with_transform {
                    volume.transform.to_cols_array().map(f64::to_bits).hash(&mut hash);
                }
                *boxes.entry(hash.finish()).or_insert_with(|| {
                    let (mut lo, mut hi) = (project::DVec3::MAX, project::DVec3::MIN);
                    for point in &volume.mesh.positions {
                        let p = project::DVec3::from(*point);
                        let p = if with_transform { volume.transform.transform_point3(p) } else { p };
                        lo = lo.min(p);
                        hi = hi.max(p);
                    }
                    (lo, hi)
                })
            };
            if let Some(index) = self.selected_volume {
                let volume = object.volumes.get(index)?;
                (lo, hi) = local_box(volume, false);
                transform = project.instance(key).ok()?.transform * volume.transform;
            } else {
                for volume in object
                    .volumes
                    .iter()
                    .filter(|v| v.kind == project::VolumeKind::Part)
                {
                    let (a, b) = local_box(volume, true);
                    lo = lo.min(a);
                    hi = hi.max(b);
                }
                transform = project.instance(key).ok()?.transform;
            }
            drop(boxes);
            if lo.is_finite() && hi.is_finite() {
                gizmo.orient_scale(
                    glam::Mat4::from_translation(self.instance_offset(key))
                        * glam::DMat4::from(transform).as_mat4(),
                    lo.as_vec3(),
                    hi.as_vec3(),
                );
            }
        }
        Some(gizmo)
    }

    /// Перебудовує оверлей «Підготовки»: гізмо або підсвічену грань.
    pub fn refresh_overlay(&mut self) {
        if self.page != Page::Prepare {
            return;
        }
        let hot = self
            .gizmo_drag
            .as_ref()
            .map(|d| d.handle)
            .or(self.gizmo_hot);
        let mut triangles = self
            .current_gizmo()
            .map(|g| g.geometry(&self.camera, hot))
            .unwrap_or_default();
        if self.tool == Some(Tool::LayOnFace) {
            // Акцент Zed напівпрозорим, трохи над гранню.
            let color = [0.175, 0.418, 0.807, 0.55];
            let mut highlight = |tris: &[[project::DVec3; 3]], normal: Option<project::DVec3>, offset: glam::Vec3| {
                for tri in tris {
                    let n = normal
                        .unwrap_or_else(|| (tri[1] - tri[0]).cross(tri[2] - tri[0]))
                        .normalize_or_zero()
                        * 0.05;
                    for p in tri {
                        triangles.push(viewer::FlatVertex {
                            pos: ((*p + n).as_vec3() + offset).to_array(),
                            color,
                        });
                    }
                }
            };
            if let Some((key, volume, triangle)) = self.face_hover
                && let Some(project) = &self.project
                && let Ok(facet) = project.facet(key, volume, triangle)
            {
                highlight(&facet, None, self.instance_offset(key));
            }
            if let Some(index) = self.hull_hover
                && let Some((key, _, planes)) = &self.hull
                && let Some(plane) = planes.get(index)
            {
                highlight(&plane.triangles, Some(plane.normal), self.instance_offset(*key));
            }
        }
        // Після зміни геометрії (масштаб, undo) старі ознаки недійсні: сітка перебудовується.
        self.ensure_measure();
        let (surface, lines) = self.measure_overlay();
        triangles.extend(lines);
        self.renderer.set_surface_overlay(&surface);
        self.renderer.set_overlay(&triangles);
    }

    /// Чи можна зараз тримати активним цей інструмент: потрібен вибраний об’єкт.
    pub fn tool_allowed(&self, tool: Tool) -> bool {
        self.page == Page::Prepare
            && self.selected.is_some()
            && self.selected_instances.len() <= 1
            && !(matches!(tool, Tool::LayOnFace | Tool::Cut) && self.selected_volume.is_some())
    }

    /// Вмикає інструмент (`None` — вимикає). Без вибору інструмент не вмикається.
    pub fn set_tool(&mut self, tool: Option<Tool>, window: &mut Window, cx: &mut Context<Self>) {
        let tool = tool.filter(|t| self.tool_allowed(*t));
        self.tool = tool;
        self.measure = None;
        self.ensure_measure();
        self.gizmo_hot = None;
        self.face_hover = None;
        self.hull_hover = None;
        if self.tool == Some(Tool::Cut) {
            self.reset_cut_plane();
        }
        self.update_highlight();
        self.refresh_tool_panel(window, cx);
        self.dirty = true;
        cx.notify();
    }

    /// Повторний клік по активному інструменту вимикає його.
    pub fn toggle_tool(&mut self, tool: Tool, window: &mut Window, cx: &mut Context<Self>) {
        let next = (self.tool != Some(tool)).then_some(tool);
        self.set_tool(next, window, cx);
    }

    /// Прибирає інструмент, який більше не має цілі (знято вибір, інша сторінка тощо).
    /// Викликається з `update_highlight`, тож охоплює будь-яку зміну вибору.
    pub fn sync_tool(&mut self) {
        if self.tool.is_some_and(|t| !self.tool_allowed(t)) {
            self.tool = None;
            self.gizmo_hot = None;
            self.face_hover = None;
            self.hull_hover = None;
        }
        if self.tool == Some(Tool::Cut) {
            self.retarget_cut();
        }
    }

    /// Esc: спершу вимикає інструмент, потім знімає вибір. `false` — нічого не зроблено.
    pub fn escape_scene(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.page != Page::Prepare {
            return false;
        }
        if self.measure_escape() {
            cx.notify();
            true
        } else if self.tool.is_some() {
            self.set_tool(None, window, cx);
            true
        } else if self.selected.is_some() {
            self.select_instance(None, window, cx);
            true
        } else {
            false
        }
    }

    /// Клавіші інструментів: M, R, S, F, C — лише коли щось вибрано. Esc — вихід з інструмента.
    pub fn tool_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if key == "escape" {
            return self.escape_scene(window, cx);
        }
        match Tool::ALL.into_iter().find(|t| t.key() == key) {
            Some(tool) if self.tool_allowed(tool) => {
                self.toggle_tool(tool, window, cx);
                true
            }
            _ => false,
        }
    }

    /// Наведення без натиснутих кнопок: ручка гізмо або грань під курсором. `true` — щось змінилось.
    pub fn update_tool_hover(&mut self, position: Point<Pixels>) -> bool {
        let mut changed = false;
        let hot = match (self.current_gizmo(), self.screen(), self.local(position)) {
            (Some(g), Some(screen), Some(cursor)) => g.pick(&screen, cursor),
            _ => None,
        };
        if hot != self.gizmo_hot {
            self.gizmo_hot = hot;
            changed = true;
        }
        let lay = self.tool == Some(Tool::LayOnFace) && self.page == Page::Prepare;
        let facet_mode = self
            .tool_panel
            .as_ref()
            .is_none_or(|p| p.lay_mode == crate::tool_panels::LayMode::Facet);
        let face = if lay && facet_mode {
            self.face_at(position)
        } else {
            None
        };
        if face != self.face_hover {
            self.face_hover = face;
            changed = true;
        }
        let hull = if lay && !facet_mode {
            self.hull_at(position)
        } else {
            None
        };
        if hull != self.hull_hover {
            self.hull_hover = hull;
            changed = true;
        }
        changed |= self.measure_hover(position);
        if changed {
            self.dirty = true;
        }
        changed
    }

    /// Трикутник об’єкта показаної пластини під курсором.
    fn face_at(&self, position: Point<Pixels>) -> Option<(InstanceRef, usize, usize)> {
        let (origin, dir) = self.ray_at(position)?;
        let origin = origin - self.plate_offset(self.plates.get(self.active_plate)?.number);
        let project = self.project.as_ref()?;
        let number = self.plates.get(self.active_plate)?.number;
        let plate = (number > 0).then_some(number);
        project
            .plate_bounds(plate)
            .ok()?
            .into_iter()
            .filter(|(_, b)| {
                crate::objects::ray_box(origin, dir, b.min.as_vec3(), b.max.as_vec3()).is_some()
            })
            .filter_map(|(key, _)| {
                let (t, v, tri) = project
                    .raycast(key, origin.as_dvec3(), dir.as_dvec3())
                    .ok()??;
                Some((t, (key, v, tri)))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, hit)| hit)
    }

    /// Грані опуклого корпусу вибраного інстансу (кеш до зміни геометрії).
    fn hull_planes(&mut self) -> Option<&[project::HullPlane]> {
        let key = self.selected?;
        let fresh = matches!(&self.hull, Some((k, rev, _)) if *k == key && *rev == self.geometry_revision);
        if !fresh {
            let planes = self.project.as_ref()?.convex_hull_planes(key).ok()?;
            self.hull = Some((key, self.geometry_revision, planes));
        }
        self.hull.as_ref().map(|(_, _, planes)| planes.as_slice())
    }

    /// Грань корпусу вибраного інстансу під курсором.
    fn hull_at(&mut self, position: Point<Pixels>) -> Option<usize> {
        let (origin, dir) = self.ray_at(position)?;
        let origin = origin - self.instance_offset(self.selected?);
        let planes = self.hull_planes()?;
        project::pick_plane(planes, origin.as_dvec3(), dir.as_dvec3())
    }

    /// Клік у режимі «покласти на грань»: об’єкт лягає підсвіченою гранню на стіл.
    pub fn lay_hovered_face(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if let Some(index) = self.hull_hover {
            let normal = self
                .hull
                .as_ref()
                .and_then(|(_, _, planes)| planes.get(index))
                .map(|plane| plane.normal);
            self.hull_hover = None;
            let Some(normal) = normal else { return false };
            self.edit_selected("Покласти на грань", window, cx, move |p, k| {
                p.lay_on_normal(k, normal)?;
                Ok(Some(k))
            });
            return true;
        }
        let Some((key, volume, triangle)) = self.face_hover else {
            return false;
        };
        self.selected = Some(key);
        self.selected_instances = vec![key];
        self.selected_volume = None;
        self.face_hover = None;
        self.edit_selected(
            "Покласти на грань",
            window,
            cx,
            move |p, k| {
                p.lay_on_face(k, volume, triangle)?;
                Ok(Some(k))
            },
        );
        true
    }

    /// Натискання на ручку гізмо — початок перетягування.
    pub fn begin_gizmo(&mut self, position: Point<Pixels>) -> bool {
        if self.tool == Some(Tool::Cut) {
            return self.begin_cut_drag(position);
        }
        let (Some(handle), Some(gizmo), Some(key), Some(from)) = (
            self.gizmo_hot,
            self.current_gizmo(),
            self.selected,
            self.local(position),
        ) else {
            return false;
        };
        let Some(start) = self.project.as_ref().and_then(|p| {
            let instance = p.instance(key).ok()?;
            match self.selected_volume {
                Some(index) => Some(
                    instance.transform * p.object(key.object).ok()?.volumes.get(index)?.transform,
                ),
                None => Some(instance.transform),
            }
        }) else {
            return false;
        };
        let axis_t0 = match handle {
            Handle::Move(i) => self
                .ray_at(position)
                .and_then(|(o, d)| closest_on_axis(gizmo.center(), Gizmo::axis(i), o, d))
                .unwrap_or(0.0),
            _ => 0.0,
        };
        self.checkpoint(match handle {
            Handle::Move(_) => "Переміщення",
            Handle::Rotate(_) => "Поворот",
            _ => "Масштабування",
        });
        self.gizmo_drag = Some(GizmoDrag {
            handle,
            key,
            start,
            gizmo,
            from,
            axis_t0,
            readout: None,
        });
        true
    }

    /// Рух миші з натиснутою ручкою. Shift — крок 5 мм / 15° / 10 %.
    pub fn drag_gizmo(&mut self, position: Point<Pixels>, snap: bool) {
        if self.tool == Some(Tool::Cut) {
            return self.drag_cut(position, snap);
        }
        let (Some(drag), Some(cursor)) = (self.gizmo_drag.as_ref(), self.local(position)) else {
            return;
        };
        let Some(screen) = self.screen() else { return };
        let g = drag.gizmo;
        let c = g.center();
        let (pivot, change, readout): (glam::Vec3, DAffine3, String) = match drag.handle {
            Handle::Move(i) => {
                let Some((o, d)) = self.ray_at(position) else {
                    return;
                };
                let Some(t) = closest_on_axis(c, Gizmo::axis(i), o, d) else {
                    return;
                };
                let mut delta = t - drag.axis_t0;
                if snap {
                    delta = (delta / 5.0).round() * 5.0;
                }
                if i == 2 {
                    delta = delta.max(-g.lo.z); // не нижче столу
                }
                let shift = Gizmo::axis(i) * delta;
                (
                    c,
                    DAffine3::from_translation(shift.as_dvec3()),
                    crate::tr_format!("{} {delta:+.2} мм", ["X", "Y", "Z"][i]),
                )
            }
            Handle::Rotate(i) => {
                let Some(cs) = screen.project(c) else { return };
                let angle_of = |p: glam::Vec2| (-(p.y - cs.y)).atan2(p.x - cs.x);
                let mut delta = angle_of(cursor) - angle_of(drag.from);
                // Вісь до глядача — проти годинникової на екрані означає додатний поворот.
                if Gizmo::axis(i).dot(self.camera.eye() - c) < 0.0 {
                    delta = -delta;
                }
                let mut deg = delta.to_degrees();
                deg = (deg + 540.0).rem_euclid(360.0) - 180.0;
                if snap {
                    deg = (deg / 15.0).round() * 15.0;
                }
                let rot =
                    DQuat::from_axis_angle(Gizmo::axis(i).as_dvec3(), f64::from(deg).to_radians());
                (
                    c,
                    DAffine3::from_quat(rot),
                    format!("{} {deg:+.1}°", ["X", "Y", "Z"][i]),
                )
            }
            Handle::Scale(i, positive) => {
                // Ручка Z тягне верх від низу; X/Y — симетрично від центру.
                let base = if i == 2 {
                    g.scale_base()
                } else {
                    g.scale_center()
                };
                let handle_at = g.scale_handle(i, positive);
                let (Some(bs), Some(hs)) = (screen.project(base), screen.project(handle_at)) else {
                    return;
                };
                let d0 = hs - bs;
                if d0.length_squared() < 1.0 {
                    return;
                }
                let mut f = ((cursor - bs).dot(d0) / d0.length_squared()).max(0.02);
                if snap {
                    f = ((f * 10.0).round() / 10.0).max(0.1);
                }
                let mut factors = glam::DVec3::ONE;
                factors[i] = f64::from(f);
                (
                    base,
                    scale_in_frame(g.scale_axes(), factors),
                    format!("{} {:.0} %", ["X", "Y", "Z"][i], f * 100.0),
                )
            }
            Handle::CutMove => return,
            Handle::ScaleUniform(_) => {
                let base = g.scale_base();
                let (Some(bs), Some(hs)) = (
                    screen.project(base),
                    screen.project(drag.world_corner(&screen)),
                ) else {
                    return;
                };
                let d0 = (hs - bs).length();
                if d0 < 1.0 {
                    return;
                }
                let mut f = ((cursor - bs).length() / d0).max(0.02);
                if snap {
                    f = ((f * 10.0).round() / 10.0).max(0.1);
                }
                (
                    base,
                    DAffine3::from_scale(glam::DVec3::splat(f64::from(f))),
                    format!("{:.0} %", f * 100.0),
                )
            }
        };
        let pivot = (pivot - self.instance_offset(drag.key)).as_dvec3();
        let transform = DAffine3::from_translation(pivot)
            * change
            * DAffine3::from_translation(-pivot)
            * drag.start;
        let key = drag.key;
        if let Some(drag) = &mut self.gizmo_drag {
            drag.readout = Some(readout);
        }
        self.ensure_live_drag(key);
        let Some(project) = self.project.as_mut() else {
            return;
        };
        if let Some(index) = self.selected_volume {
            let Ok(instance) = project.instance(key) else {
                return;
            };
            let local = instance.transform.inverse() * transform;
            let Some(volume) = project
                .objects
                .iter_mut()
                .find(|o| o.id == key.object)
                .and_then(|o| o.volumes.get_mut(index))
            else {
                return;
            };
            volume.transform = local;
        } else {
            let Ok(instance) = project.instance_mut(key) else {
                return;
            };
            instance.transform = transform;
        }
        self.edited = true;
        self.live_drag_moved(key);
    }

    /// Кінець перетягування: після повороту й масштабу — на стіл, як у Bambu Studio.
    pub fn end_gizmo(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(drag) = self.gizmo_drag.take() else {
            return false;
        };
        if self.tool == Some(Tool::Cut) {
            self.end_cut_drag();
            return true;
        }
        self.finish_live_drag();
        if drag.readout.is_some() {
            let settle = self.selected_volume.is_none() && !matches!(drag.handle, Handle::Move(_));
            if settle && let Some(p) = self.project.as_mut() {
                let _ = p.drop_to_bed(drag.key);
            }
            self.project_changed(window, cx);
        }
        self.finish_checkpoint();
        true
    }

    pub fn gizmo_readout(&self) -> Option<SharedString> {
        self.gizmo_drag.as_ref()?.readout.clone().map(Into::into)
    }
}

impl GizmoDrag {
    /// Перетягування ручки площини різання: стан площини живе в `CutState`, тут — лише маршрутизація.
    pub(crate) fn for_cut(
        handle: Handle,
        key: InstanceRef,
        gizmo: Gizmo,
        from: glam::Vec2,
        axis_t0: f32,
    ) -> Self {
        Self {
            handle,
            key,
            start: DAffine3::IDENTITY,
            gizmo,
            from,
            axis_t0,
            readout: None,
        }
    }

    fn world_corner(&self, _screen: &Screen) -> glam::Vec3 {
        match self.handle {
            Handle::ScaleUniform(index) => self.gizmo.uniform_corner(index),
            _ => self.gizmo.scale_center(),
        }
    }
}

/// Плаваючий тулбар двох станів: без вибору — глобальні дії сцени, з вибором — лише
/// інструменти вибраного об’єкта (переміщення, поворот, масштаб, на грань, налаштування, дублікат).
pub fn toolbar(ws: &Workspace, cx: &mut Context<Workspace>) -> impl IntoElement {
    let z = zed();
    let separator = || div().w(px(1.)).h(px(22.)).mx(px(3.)).bg(z.border);
    let mut bar = ui::scene_control("prepare-floating-tools")
        .flex()
        .flex_wrap()
        .max_w_full()
        .min_w_0()
        .items_center()
        .justify_center()
        .gap(px(3.))
        .p(px(6.))
        .rounded(px(if crate::theme::gnome() { 20. } else { 10. }))
        .bg(z.panel.opacity(0.97))
        .border_1()
        .border_color(z.border)
        .shadow_lg()
        .child(crate::history::buttons(ws, cx))
        .child(separator())
        .child(
            ui::icon_button("tool-open", IconName::FolderOpen, "Додати STL до проєкту")
            .on_click(cx.listener(|this, _, window, cx| this.prompt_import(window, cx))),
        );
    bar = if ws.selected.is_some() {
        object_tools(bar, ws, cx, separator)
    } else {
        scene_tools(bar, ws, cx, separator)
    };
    if ws.jobs.is_running(crate::jobs::JobKind::Orient) {
        bar = bar.child(
            div()
                .px(px(4.))
                .text_xs()
                .text_color(z.text_muted)
                .child(crate::i18n::tr("Автоорієнтація…")),
        );
    }
    // Фонове віднімання негативних частин: статична позначка, без анімації.
    if ws.jobs.is_running(crate::jobs::JobKind::BooleanPreview) {
        bar = bar.child(ui::spinner("boolean-preview-progress"));
    } else if ws.geometry_error.is_some() {
        bar = bar.child(
            div()
                .px(px(4.))
                .text_xs()
                .text_color(z.warning)
                .child(crate::i18n::tr("Віднімання не обчислено")),
        );
    }
    div()
        .absolute()
        .bottom(px(16.))
        .left(px(12.))
        .right(px(12.))
        .flex()
        .justify_center()
        .child(bar)
}

fn orient_button(ws: &Workspace, cx: &mut Context<Workspace>) -> gpui::Stateful<gpui::Div> {
    ui::icon_button("tool-auto-orient", IconName::Rotate3d, "Автоорієнтація (Q)")
    .inert(ws.jobs.is_running(crate::jobs::JobKind::Orient))
    .on_click(cx.listener(|this, _, _, cx| this.orient_selection(cx)))
}

/// Глобальна група: розкладка пластини, показ модифікаторів, віднімання, переріз.
fn scene_tools(
    bar: gpui::Stateful<gpui::Div>,
    ws: &Workspace,
    cx: &mut Context<Workspace>,
    separator: impl Fn() -> gpui::Div,
) -> gpui::Stateful<gpui::Div> {
    bar.child(separator())
        .child(orient_button(ws, cx))
        .child(
            ui::icon_button("tool-arrange", IconName::LayoutGrid, "Розкласти пластину")
            .on_click(cx.listener(|this, _, window, cx| this.arrange_plate(window, cx))),
        )
        .child(
            ui::icon_button(
                "tool-modifiers",
                IconName::Layers,
                "Показати негативні частини й модифікатори",
            )
            .toggled(ws.show_modifiers)
            .on_click(cx.listener(|this, _, _, cx| {
                this.show_modifiers = !this.show_modifiers;
                this.refresh_modifiers();
                this.dirty = true;
                cx.notify();
            })),
        )
        .child(
            ui::icon_button(
                "tool-boolean",
                IconName::ScissorsLineDashed,
                "Віднімання негативних частин перед нарізкою",
            )
            .toggled(ws.boolean_preview)
            .on_click(cx.listener(|this, _, _, cx| {
                this.boolean_preview = !this.boolean_preview;
                this.geometry_error = None;
                this.refresh_prepare_mesh();
                cx.notify();
            })),
        )
        .child(
            ui::icon_button("tool-section", IconName::Scissors, "Переріз перед нарізкою")
                .toggled(ws.cut_enabled)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.cut_enabled = !this.cut_enabled;
                    this.dirty = true;
                    cx.notify();
                })),
        )
}

/// Група вибраного: інструменти (вимкнені, якщо ціль не підходить), налаштування, дублікат / видалення.
fn object_tools(
    mut bar: gpui::Stateful<gpui::Div>,
    ws: &Workspace,
    cx: &mut Context<Workspace>,
    separator: impl Fn() -> gpui::Div,
) -> gpui::Stateful<gpui::Div> {
    let z = zed();
    let merge = ws.selected_instances.len() >= 2;
    bar = bar.child(separator()).child(orient_button(ws, cx));
    if merge {
        bar = bar
            .child(
                div()
                    .text_xs()
                    .text_color(z.text_muted)
                    .px(px(4.))
                    .child(crate::tr_format!("{} об’єкти", ws.selected_instances.len())),
            )
            .child(
                ui::icon_button(
                    "tool-merge",
                    IconName::Combine,
                    "Об’єднати в збірку (Ctrl+G; Ctrl/Shift — вибір)",
                )
                .on_click(cx.listener(|this, _, window, cx| this.merge_selection(window, cx))),
            );
    } else {
        bar = bar
            .children(
                Tool::ALL
                    .into_iter()
                    .filter(|t| {
                        ws.selected_volume.is_none() || !matches!(t, Tool::LayOnFace | Tool::Cut)
                    })
                    .map(|tool| {
                        let allowed = ws.tool_allowed(tool);
                        ui::icon_button(
                            SharedString::from(format!("tool-{}", tool.key())),
                            tool.icon(),
                            tool.name(),
                        )
                        .toggled(ws.tool == Some(tool))
                        .inert(!allowed)
                        .when(allowed, |d| {
                            d.on_click(cx.listener(move |this, _, window, cx| {
                                this.toggle_tool(tool, window, cx)
                            }))
                        })
                    }),
            )
            .child(separator())
            .child(
                ui::icon_button(
                    "tool-scope",
                    IconName::SlidersHorizontal,
                    "Налаштування вибраного об’єкта / частини",
                )
                .on_click(
                    cx.listener(|this, _, window, cx| this.open_scoped_settings(None, window, cx)),
                ),
            );
        bar = bar.child(
            ui::icon_button("support-fins", IconName::Package, "Support Fins…")
                .on_click(cx.listener(|this, _, window, cx| this.open_support_fins(window, cx))),
        );
        if ws.selected_volume.is_some() {
            bar = bar.child(
                ui::icon_button(
                    "tool-kind",
                    IconName::SquareDashed,
                    "Роль частини: деталь, віднімання, модифікатор",
                )
                .on_click(cx.listener(|this, _, window, cx| this.open_part_kind(window, cx))),
            );
        } else {
            bar = bar
                .child(
                    ui::icon_button("tool-duplicate", IconName::Copy, "Дублювати (Ctrl+D)")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.duplicate_selected(window, cx)),
                        ),
                )
                .child(
                    ui::danger_icon_button("tool-delete", IconName::Trash, "Видалити (Delete)")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.delete_selected(window, cx)),
                        ),
                );
        }
    }
    if ws.can_split_selection() {
        bar = bar.child(
            ui::icon_button(
                "tool-split",
                IconName::Split,
                "Розділити збірку на об’єкти (Ctrl+Shift+G)",
            )
            .on_click(cx.listener(|this, _, window, cx| this.split_selection(window, cx))),
        );
    }
    bar
}

/// Підказка під час перетягування гізмо: зсув, кут чи масштаб.
pub fn readout(ws: &Workspace) -> Option<impl IntoElement> {
    let z = zed();
    let text = ws.gizmo_readout()?;
    Some(
        div()
            .absolute()
            .top(px(10.))
            .left_1_2()
            .px(px(8.))
            .py(px(3.))
            .rounded(px(4.))
            .bg(z.panel.opacity(0.94))
            .border_1()
            .border_color(z.border_variant)
            .font_family(crate::theme::MONO_FONT)
            .text_xs()
            .child(text),
    )
}

/// Apply axis scaling in the starting target frame, preserving its rotation.
fn scale_in_frame(axes: [glam::Vec3; 3], factors: glam::DVec3) -> DAffine3 {
    let basis = glam::DMat3::from_cols(axes[0].as_dvec3(), axes[1].as_dvec3(), axes[2].as_dvec3());
    DAffine3::from_mat3(basis * glam::DMat3::from_diagonal(factors) * basis.inverse())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scaling_rotated_target_keeps_axes_orthogonal() {
        let rotation = DQuat::from_rotation_z(std::f64::consts::FRAC_PI_4);
        let start = DAffine3::from_scale_rotation_translation(
            glam::DVec3::new(1.0, 2.0, 3.0),
            rotation,
            glam::DVec3::ZERO,
        );
        let axes = [
            start.matrix3.x_axis,
            start.matrix3.y_axis,
            start.matrix3.z_axis,
        ]
        .map(|v| v.normalize().as_vec3());
        let scaled = scale_in_frame(axes, glam::DVec3::new(2.0, 1.0, 1.0)) * start;
        assert!(scaled.matrix3.x_axis.dot(scaled.matrix3.y_axis).abs() < 1e-8);
        assert!((scaled.matrix3.x_axis.length() - 2.0).abs() < 1e-7);
        assert!((scaled.matrix3.y_axis.length() - 2.0).abs() < 1e-7);
        assert!((scaled.matrix3.z_axis.length() - 3.0).abs() < 1e-7);
    }
}
