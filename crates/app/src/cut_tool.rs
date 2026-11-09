//! Інструмент «Різання» (C), як `GLGizmoCut` в Orca/Bambu Studio: напівпрозора площина через
//! вибраний об’єкт, стрілка зсуву вздовж її нормалі, кільця нахилу й панель параметрів.
//! Верхня частина (A) у прев’ю блакитна, нижня (B) — пурпурна; саме різання виконує
//! `Project::cut_instance`, а єдина мутація проєкту — у `Workspace::perform_cut`.
//!
//! Площина живе в тих самих (локальних для пластини) світових координатах, що й
//! `selected_bounds`/`instance_bounds`, тож переклад у координати сцени — лише в `cut_preview`.

use gpui::{
    Context, Entity, Focusable as _, SharedString, Subscription, Window, div, prelude::*, px, rgb,
};
use gpui_component::input::{Input, InputEvent, InputState};
use project::{CutOptions, CutPlane, DQuat, DVec3, InstanceRef};
use viewer::{Gizmo, GizmoKind, Handle};

use crate::{
    theme::zed,
    tool_panels::{input_row, number, parse, row},
    tools::{GizmoDrag, Tool, closest_on_axis},
    ui::{self, IconName},
    widgets,
    workspace::{Page, Workspace},
};

/// Кольори частин у прев’ю: UPPER_PART_COLOR / LOWER_PART_COLOR в Orca (CYAN / MAGENTA).
const UPPER_SWATCH: u32 = 0x1fc8d4;
const LOWER_SWATCH: u32 = 0xd44fcf;

/// Площина різання: опорна точка об’єкта, точка на площині й орієнтація.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plane {
    /// Центр рамки об’єкта на момент відкриття — точка відліку для «Переміщення».
    base: DVec3,
    point: DVec3,
    rotation: DQuat,
}

impl Plane {
    /// Горизонтальна площина через `center` (за замовчуванням — середина висоти об’єкта).
    pub fn centered(center: DVec3) -> Self {
        Self {
            base: center,
            point: center,
            rotation: DQuat::IDENTITY,
        }
    }

    pub fn normal(&self) -> DVec3 {
        (self.rotation * DVec3::Z).normalize_or(DVec3::Z)
    }

    /// Відстань від опорної точки до площини вздовж нормалі, мм.
    pub fn movement(&self) -> f64 {
        (self.point - self.base).dot(self.normal())
    }

    pub fn set_movement(&mut self, movement: f64) {
        self.point += self.normal() * (movement - self.movement());
    }

    /// Абсолютна висота центру площини, мм.
    pub fn height(&self) -> f64 {
        self.point.z
    }

    /// Зсуває центр уздовж нормалі до висоти `z`. `false` — площина вертикальна.
    pub fn set_height(&mut self, z: f64) -> bool {
        let n = self.normal();
        if n.z.abs() < 1e-3 {
            return false;
        }
        self.point += n * ((z - self.point.z) / n.z);
        true
    }

    /// Кути X/Y/Z у градусах (R = Rz·Ry·Rx, як у панелі повороту).
    pub fn euler(&self) -> DVec3 {
        let (z, y, x) = self.rotation.to_euler(glam::EulerRot::ZYX);
        DVec3::new(x, y, z).map(f64::to_degrees)
    }

    pub fn set_euler(&mut self, degrees: DVec3) {
        let r = degrees.map(f64::to_radians);
        self.rotation = DQuat::from_euler(glam::EulerRot::ZYX, r.z, r.y, r.x);
    }

    pub fn cut_plane(&self) -> CutPlane {
        CutPlane {
            point: self.point,
            normal: self.normal(),
        }
    }

    /// Convert only the visual guide to scene space; mesh cutting stays plate-local.
    fn preview(&self, lo: DVec3, hi: DVec3, offset: DVec3) -> ([f32; 4], (glam::Vec3, glam::Vec3)) {
        let (n, p) = (self.normal(), self.point + offset);
        (
            [n.x as f32, n.y as f32, n.z as f32, -n.dot(p) as f32],
            ((lo + offset).as_vec3(), (hi + offset).as_vec3()),
        )
    }

    /// Чи ділить площина рамку на дві непорожні частини.
    pub fn crosses(&self, lo: DVec3, hi: DVec3) -> bool {
        let n = self.normal();
        let (mut below, mut above) = (false, false);
        for i in 0..8 {
            let corner = DVec3::new(
                if i & 1 == 0 { lo.x } else { hi.x },
                if i & 2 == 0 { lo.y } else { hi.y },
                if i & 4 == 0 { lo.z } else { hi.z },
            );
            let d = (corner - self.point).dot(n);
            below |= d < -1e-6;
            above |= d > 1e-6;
        }
        below && above
    }
}

/// Вибір «після різання» для частин A (верх) і B (низ).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Choices {
    pub keep: [bool; 2],
    pub place: [bool; 2],
    pub flip: [bool; 2],
    pub to_parts: bool,
}

impl Default for Choices {
    /// Як у скріншоті Orca: A — покласти на розріз, B — зберегти орієнтацію.
    fn default() -> Self {
        Self {
            keep: [true, true],
            place: [true, false],
            flip: [false, false],
            to_parts: false,
        }
    }
}

impl Choices {
    /// Принаймні одну частину слід залишити.
    pub fn toggle_keep(&mut self, i: usize) {
        if self.keep[i] && !self.keep[1 - i] {
            return;
        }
        self.keep[i] = !self.keep[i];
    }

    /// «Покласти на розріз» і «Перевернути» взаємовиключні.
    pub fn set_place(&mut self, i: usize, on: bool) {
        self.place[i] = on;
        if on {
            self.flip[i] = false;
        }
    }

    pub fn toggle_flip(&mut self, i: usize) {
        self.flip[i] = !self.flip[i];
        if self.flip[i] {
            self.place[i] = false;
        }
    }

    /// «Розрізати на частини» лишає обидві половини в об’єкті на місці.
    pub fn set_parts(&mut self, on: bool) {
        self.to_parts = on;
        if on {
            self.keep = [true, true];
            self.place = [false, false];
            self.flip = [false, false];
        }
    }

    pub fn options(&self) -> CutOptions {
        CutOptions {
            keep_upper: self.keep[0],
            keep_lower: self.keep[1],
            upper_place_on_cut: self.keep[0] && self.place[0],
            upper_flip: self.keep[0] && self.flip[0],
            lower_place_on_cut: self.keep[1] && self.place[1],
            lower_flip: self.keep[1] && self.flip[1],
            cut_to_parts: self.to_parts,
        }
    }
}

#[derive(Clone, Copy)]
enum Field {
    Rotate(usize),
    Movement,
    Height,
}

/// Початок перетягування ручки площини: усе рахується від нього, тож похибка не накопичується.
#[derive(Clone, Copy)]
struct DragStart {
    handle: Handle,
    plane: Plane,
    cursor: glam::Vec2,
    axis_t0: f32,
}

pub struct CutState {
    /// Інстанс, для якого побудована площина.
    pub target: Option<InstanceRef>,
    bounds: Option<(DVec3, DVec3)>,
    revision: u64,
    pub plane: Plane,
    pub choices: Choices,
    /// Остання помилка «Виконати різання» (відкритий контур, шари тощо) — під кнопками.
    pub error: Option<SharedString>,
    rotate: [Entity<InputState>; 3],
    movement: Entity<InputState>,
    height: Entity<InputState>,
    drag: Option<DragStart>,
}

impl CutState {
    pub fn new(window: &mut Window, cx: &mut Context<Workspace>) -> (Self, Vec<Subscription>) {
        let mut subs = Vec::new();
        let mut make = |field: Field, window: &mut Window, cx: &mut Context<Workspace>| {
            let input = cx.new(|cx| InputState::new(window, cx));
            subs.push(cx.subscribe_in(
                &input,
                window,
                move |this, input, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        let text = input.read(cx).value().to_string();
                        this.commit_cut_field(field, &text, window, cx);
                    }
                },
            ));
            input
        };
        let state = Self {
            target: None,
            bounds: None,
            revision: 0,
            plane: Plane::centered(DVec3::ZERO),
            choices: Choices::default(),
            error: None,
            rotate: [0, 1, 2].map(|i| make(Field::Rotate(i), window, cx)),
            movement: make(Field::Movement, window, cx),
            height: make(Field::Height, window, cx),
            drag: None,
        };
        (state, subs)
    }
}

impl Workspace {
    pub(crate) fn cut_scrub_binding(&self, id: gpui::EntityId) -> Option<crate::scrub::Binding> {
        use crate::scrub::{Binding, Spec, Target};
        let cut = self.cut.as_ref()?;
        let mut spec = if cut.rotate.iter().any(|input| input.entity_id() == id) {
            Spec::unit(1.)
        } else if cut.height.entity_id() == id || cut.movement.entity_id() == id {
            Spec::unit(0.1)
        } else {
            return None;
        };
        if cut.height.entity_id() == id {
            spec.min = 0.;
        }
        Some(Binding {
            spec,
            target: Target::Text,
        })
    }

    /// Скидає площину до середини висоти вибраного об’єкта (опції «після різання» лишаються).
    pub fn reset_cut_plane(&mut self) {
        let bounds = self.selected_bounds().map(|b| (b.min, b.max));
        let (target, revision) = (self.selected, self.geometry_revision);
        let Some(cut) = self.cut.as_mut() else { return };
        cut.target = target;
        cut.bounds = bounds;
        cut.revision = revision;
        cut.error = None;
        cut.drag = None;
        if let Some((lo, hi)) = bounds {
            cut.plane = Plane::centered((lo + hi) * 0.5);
        }
        self.dirty = true;
    }

    /// Інший об’єкт — нова площина; змінена геометрія того ж — лише нова рамка.
    pub fn retarget_cut(&mut self) {
        let Some(cut) = &self.cut else { return };
        if cut.target != self.selected {
            self.reset_cut_plane();
        } else if cut.revision != self.geometry_revision {
            let bounds = self.selected_bounds().map(|b| (b.min, b.max));
            let revision = self.geometry_revision;
            if let Some(cut) = self.cut.as_mut() {
                cut.bounds = bounds;
                cut.revision = revision;
            }
            self.dirty = true;
        }
    }

    /// Вибраний об’єкт лежить на показаній пластині.
    fn cut_target_visible(&self) -> bool {
        let (Some(key), Some(project), Some(plate)) = (
            self.selected,
            self.project.as_ref(),
            self.plates.get(self.active_plate),
        ) else {
            return false;
        };
        project
            .instance(key)
            .is_ok_and(|i| i.plate.unwrap_or(0) == plate.number)
    }

    /// Гізмо площини: кільця навколо її центру й стрілка вздовж нормалі.
    pub(crate) fn cut_gizmo(&self) -> Option<Gizmo> {
        if self.page != Page::Prepare
            || self.selected_instances.len() > 1
            || !self.cut_target_visible()
        {
            return None;
        }
        let cut = self.cut.as_ref()?;
        let (lo, hi) = cut.bounds?;
        let offset = self.instance_offset(self.selected?).as_dvec3();
        let (c, half) = (cut.plane.point + offset, (hi - lo) * 0.5);
        let mut gizmo = Gizmo::new(
            GizmoKind::Cut,
            (c - half).as_vec3(),
            (c + half).as_vec3(),
            &self.screen()?,
        );
        gizmo.cut_normal = cut.plane.normal().as_vec3();
        Some(gizmo)
    }

    /// Площина `dot(n, p) + w = 0` і рамка об’єкта для рендерера (напівпрозора площина й підсвітка частин).
    pub fn cut_preview(&self) -> Option<([f32; 4], (glam::Vec3, glam::Vec3))> {
        if self.tool != Some(Tool::Cut) || self.page != Page::Prepare || !self.cut_target_visible()
        {
            return None;
        }
        let cut = self.cut.as_ref()?;
        let (lo, hi) = cut.bounds?;
        let offset = self.instance_offset(self.selected?).as_dvec3();
        Some(cut.plane.preview(lo, hi, offset))
    }

    /// Підставляє в поля панелі поточні значення (крім поля, у якому зараз друкують).
    pub fn refresh_cut_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cut) = &self.cut else { return };
        let euler = cut.plane.euler();
        let values = [
            (&cut.rotate[0], number(euler.x, 1)),
            (&cut.rotate[1], number(euler.y, 1)),
            (&cut.rotate[2], number(euler.z, 1)),
            (&cut.movement, number(cut.plane.movement(), 2)),
            (&cut.height, number(cut.plane.height(), 2)),
        ];
        for (input, text) in values {
            if input.focus_handle(cx).is_focused(window) {
                continue;
            }
            if input.read(cx).value() != text {
                input.update(cx, |state, cx| state.set_value(text, window, cx));
            }
        }
    }

    fn commit_cut_field(
        &mut self,
        field: Field,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tool != Some(Tool::Cut) {
            return;
        }
        let Some(cut) = self.cut.as_mut() else { return };
        if let Some(value) = parse(text) {
            cut.error = None;
            match field {
                Field::Rotate(i) => {
                    let mut euler = cut.plane.euler();
                    euler[i] = value.clamp(-180.0, 180.0);
                    cut.plane.set_euler(euler);
                }
                Field::Movement => cut.plane.set_movement(value),
                Field::Height => {
                    if !cut.plane.set_height(value) {
                        cut.error = Some(
                            crate::i18n::tr("Площина вертикальна — висоту задати не можна").into(),
                        );
                    }
                }
            }
            self.dirty = true;
        }
        self.refresh_cut_fields(window, cx);
        cx.notify();
    }

    // ---- перетягування ручок ----

    /// Натискання на стрілку чи кільце площини.
    pub(crate) fn begin_cut_drag(&mut self, position: gpui::Point<gpui::Pixels>) -> bool {
        let (Some(handle), Some(gizmo), Some(key), Some(cursor)) = (
            self.gizmo_hot,
            self.current_gizmo(),
            self.selected,
            self.local(position),
        ) else {
            return false;
        };
        let ray = self.ray_at(position);
        let Some(cut) = self.cut.as_mut() else {
            return false;
        };
        let axis_t0 = match handle {
            Handle::CutMove => ray
                .and_then(|(o, d)| {
                    closest_on_axis(gizmo.center(), cut.plane.normal().as_vec3(), o, d)
                })
                .unwrap_or(0.0),
            _ => 0.0,
        };
        cut.drag = Some(DragStart {
            handle,
            plane: cut.plane,
            cursor,
            axis_t0,
        });
        self.gizmo_drag = Some(GizmoDrag::for_cut(handle, key, gizmo, cursor, axis_t0));
        true
    }

    /// Рух миші з натиснутою ручкою. Shift — крок 5 мм / 15°.
    pub(crate) fn drag_cut(&mut self, position: gpui::Point<gpui::Pixels>, snap: bool) {
        let (Some(start), Some(cursor), Some(screen)) = (
            self.cut.as_ref().and_then(|c| c.drag),
            self.local(position),
            self.screen(),
        ) else {
            return;
        };
        let offset = self
            .selected
            .map_or(glam::Vec3::ZERO, |key| self.instance_offset(key));
        let center = start.plane.point.as_vec3() + offset;
        let (plane, readout) = match start.handle {
            Handle::CutMove => {
                let n = start.plane.normal();
                let Some((o, d)) = self.ray_at(position) else {
                    return;
                };
                let Some(t) = closest_on_axis(center, n.as_vec3(), o, d) else {
                    return;
                };
                let mut delta = t - start.axis_t0;
                if snap {
                    delta = (delta / 5.0).round() * 5.0;
                }
                let mut plane = start.plane;
                plane.point += n * f64::from(delta);
                (plane, crate::tr_format!("Зсув {delta:+.2} мм"))
            }
            Handle::Rotate(i) => {
                let Some(cs) = screen.project(center) else {
                    return;
                };
                let angle_of = |p: glam::Vec2| (-(p.y - cs.y)).atan2(p.x - cs.x);
                let mut delta = angle_of(cursor) - angle_of(start.cursor);
                // Вісь до глядача — проти годинникової на екрані означає додатний поворот.
                if Gizmo::axis(i).dot(self.camera.eye() - center) < 0.0 {
                    delta = -delta;
                }
                let mut deg = delta.to_degrees();
                deg = (deg + 540.0).rem_euclid(360.0) - 180.0;
                if snap {
                    deg = (deg / 15.0).round() * 15.0;
                }
                let mut plane = start.plane;
                plane.rotation =
                    DQuat::from_axis_angle(Gizmo::axis(i).as_dvec3(), f64::from(deg).to_radians())
                        * start.plane.rotation;
                (plane, format!("{} {deg:+.1}°", ["X", "Y", "Z"][i]))
            }
            _ => return,
        };
        if let Some(cut) = self.cut.as_mut() {
            cut.plane = plane;
            cut.error = None;
        }
        if let Some(drag) = &mut self.gizmo_drag {
            drag.readout = Some(readout);
        }
        self.dirty = true;
    }

    pub(crate) fn end_cut_drag(&mut self) {
        if let Some(cut) = self.cut.as_mut() {
            cut.drag = None;
        }
        self.dirty = true;
    }

    // ---- виконання ----

    /// Єдина мутація проєкту інструментом: ріже вибраний інстанс площиною, закриває
    /// інструмент і вибирає нові частини. Помилки (відкритий контур, діапазони шарів)
    /// лишаються в панелі, проєкт при цьому не змінюється.
    pub fn perform_cut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(key), Some(cut)) = (self.selected, self.cut.as_ref()) else {
            return;
        };
        let (plane, options) = (cut.plane.cut_plane(), cut.choices.options());
        self.checkpoint("Різання");
        let Some(project) = self.project.as_mut() else {
            return;
        };
        match project.cut_instance(key, plane, options) {
            Ok(result) => {
                let mut parts = Vec::new();
                for part in [result.upper, result.lower].into_iter().flatten() {
                    if !parts.contains(&part) {
                        parts.push(part);
                    }
                }
                self.tool = None;
                self.gizmo_hot = None;
                self.gizmo_drag = None;
                self.selected = parts.last().copied();
                self.selected_volume = None;
                self.selected_instances = parts;
                self.project_changed(window, cx);
            }
            Err(err) => {
                self.finish_checkpoint();
                let reason = format!("{err:#}").replace('\u{02bc}', "’");
                if let Some(cut) = self.cut.as_mut() {
                    cut.error = Some(
                        crate::tr_format!("Не вдалося розрізати: {}", crate::i18n::tr(&reason))
                            .into(),
                    );
                }
                cx.notify();
            }
        }
    }
}

// ---- вигляд ----

fn chip(
    id: &'static str,
    label: SharedString,
    on: bool,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    let z = zed();
    div()
        .id(id)
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .h(px(20.))
        .px(px(8.))
        .rounded(px(3.))
        .text_xs()
        .text_color(if on { z.text } else { z.text_muted })
        .when(on, |d| d.bg(z.element_selected))
        .when(!enabled, |d| d.opacity(0.5))
        .child(label)
}

fn part_block(i: usize, state: &CutState, cx: &mut Context<Workspace>) -> gpui::Div {
    let z = zed();
    let tr = crate::i18n::tr;
    let choices = state.choices;
    let (title, swatch) = if i == 0 {
        (tr("Об’єкт A (верхній)"), UPPER_SWATCH)
    } else {
        (tr("Об’єкт B (нижній)"), LOWER_SWATCH)
    };
    let editable = choices.keep[i] && !choices.to_parts;
    let place = choices.place[i];
    div()
        .flex()
        .flex_col()
        .gap(px(5.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .flex_none()
                        .size(px(10.))
                        .rounded(px(2.))
                        .bg(rgb(swatch)),
                )
                .child(div().flex_1().min_w_0().text_xs().child(title))
                .child(
                    widgets::checkbox(("cut-keep", i), choices.keep[i], choices.to_parts)
                        .child(div().text_xs().child(tr("Залишити")))
                        .when(!choices.to_parts, |d| {
                            d.on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(cut) = this.cut.as_mut() {
                                    cut.choices.toggle_keep(i);
                                }
                                cx.notify();
                            }))
                        }),
                ),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_x(px(12.))
                .gap_y(px(4.))
                .pl(px(16.))
                .child(
                    widgets::radio(
                        ("cut-orientation", i),
                        !place && !choices.flip[i],
                        !editable,
                    )
                    .child(div().text_xs().child(tr("Зберегти орієнтацію")))
                    .when(editable, |d| {
                        d.on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(cut) = this.cut.as_mut() {
                                cut.choices.set_place(i, false);
                                cut.choices.flip[i] = false;
                            }
                            cx.notify();
                        }))
                    }),
                )
                .child(
                    widgets::radio(("cut-place", i), place, !editable)
                        .child(div().text_xs().child(tr("Покласти на розріз")))
                        .when(editable, |d| {
                            d.on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(cut) = this.cut.as_mut() {
                                    cut.choices.set_place(i, true);
                                }
                                cx.notify();
                            }))
                        }),
                )
                .child(
                    widgets::checkbox(("cut-flip", i), choices.flip[i], !editable)
                        .child(div().text_xs().text_color(z.text).child(tr("Перевернути")))
                        .when(editable, |d| {
                            d.on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(cut) = this.cut.as_mut() {
                                    cut.choices.toggle_flip(i);
                                }
                                cx.notify();
                            }))
                        }),
                ),
        )
}

/// Тіло панелі інструмента «Різання» (заголовок і кнопка закриття — у `tool_panels::panel`).
pub fn panel_body(
    ws: &Workspace,
    window: &Window,
    cx: &mut Context<Workspace>,
) -> Option<gpui::Div> {
    let z = zed();
    let tr = crate::i18n::tr;
    let cut = ws.cut.as_ref()?;
    let can_cut = cut.bounds.is_some_and(|(lo, hi)| cut.plane.crosses(lo, hi));
    let field = |state: &Entity<InputState>, cx: &mut Context<Workspace>| {
        div()
            .flex_1()
            .min_w_0()
            .child(crate::scrub::field(state, Input::new(state), window, cx))
    };

    let mode = row(tr("Режим")).child(
        div()
            .flex_1()
            .flex()
            .items_center()
            .gap(px(2.))
            .p(px(2.))
            .rounded(px(5.))
            .bg(z.element)
            .border_1()
            .border_color(z.border_variant)
            .child(chip("cut-mode-planar", tr("Площина").into(), true, true))
            .child(chip(
                "cut-mode-dovetail",
                tr("Ластівчин хвіст").into(),
                false,
                false,
            )),
    );
    let movement = row(tr("Переміщення, мм")).child(field(&cut.movement, cx));
    let height = row(tr("Висота, мм")).child(field(&cut.height, cx));

    let parts_toggle = widgets::checkbox("cut-to-parts", cut.choices.to_parts, false)
        .child(div().text_xs().child(tr("Розрізати на частини")))
        .on_click(cx.listener(|this, _, _, cx| {
            if let Some(cut) = this.cut.as_mut() {
                let on = !cut.choices.to_parts;
                cut.choices.set_parts(on);
            }
            cx.notify();
        }));

    let perform = ui::accent_button("cut-perform", IconName::Check, "Виконати різання", can_cut)
        .when(can_cut, |d| {
            d.on_click(cx.listener(|this, _, window, cx| this.perform_cut(window, cx)))
        });
    let reset = ui::ghost_button("cut-reset", Some(IconName::RotateCcw), "Скинути").on_click(
        cx.listener(|this, _, window, cx| {
            this.reset_cut_plane();
            this.refresh_cut_fields(window, cx);
            cx.notify();
        }),
    );

    let hint = if can_cut {
        None
    } else if cut.bounds.is_some() {
        Some(tr("Площина не перетинає об’єкт"))
    } else {
        None
    };

    Some(
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(mode)
            .child(input_row(tr("Поворот, °").into(), &cut.rotate, window, cx))
            .child(movement)
            .child(height)
            .child(ui::divider())
            .child(
                div()
                    .text_xs()
                    .text_color(z.text_muted)
                    .child(tr("Після різання")),
            )
            .child(part_block(0, cut, cx))
            .child(part_block(1, cut, cx))
            .child(parts_toggle)
            .child(ui::divider())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(perform)
                    .child(reset),
            )
            .children(hint.map(|text| div().text_xs().text_color(z.warning).child(text)))
            .children(cut.error.clone().map(|text| {
                div()
                    .id("cut-error")
                    .text_xs()
                    .text_color(z.error)
                    .child(text)
            }))
            .child(div().text_xs().text_color(z.text_muted).child(tr(
                "Тягніть стрілку, щоб зсунути площину, або кільця, щоб її нахилити.",
            ))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilted_preview_on_second_plate_uses_scene_offset_only() {
        let mut plane = Plane::centered(DVec3::splat(10.0));
        plane.set_euler(DVec3::new(15.0, 45.0, 0.0));
        let offset = DVec3::new(307.2, 0.0, 0.0);
        let (equation, (lo, hi)) = plane.preview(DVec3::ZERO, DVec3::splat(20.0), offset);
        let n = glam::Vec3::from_array(equation[..3].try_into().unwrap());
        assert!((n.dot((plane.point + offset).as_vec3()) + equation[3]).abs() < 1e-4);
        assert_eq!(lo, offset.as_vec3());
        assert_eq!(hi, (offset + DVec3::splat(20.0)).as_vec3());
        assert_eq!(plane.cut_plane().point, DVec3::splat(10.0));
    }

    #[test]
    fn default_plane_is_horizontal_at_mid_height() {
        let plane = Plane::centered(DVec3::new(10.0, 20.0, 15.0));
        assert!((plane.normal() - DVec3::Z).length() < 1e-12);
        assert_eq!(plane.height(), 15.0);
        assert_eq!(plane.movement(), 0.0);
        assert!(plane.crosses(DVec3::new(0.0, 0.0, 0.0), DVec3::new(20.0, 40.0, 30.0)));
    }

    #[test]
    fn height_and_movement_move_along_the_normal() {
        let mut plane = Plane::centered(DVec3::new(0.0, 0.0, 10.0));
        plane.set_euler(DVec3::new(0.0, 45.0, 0.0));
        assert!(plane.set_height(20.0));
        assert!((plane.height() - 20.0).abs() < 1e-9);
        // Зсув вздовж нахиленої нормалі: cos 45° · 14.14 ≈ 10 мм по висоті.
        assert!((plane.movement() - 10.0 / 45f64.to_radians().cos()).abs() < 1e-9);
        plane.set_movement(0.0);
        assert!((plane.height() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn vertical_plane_has_no_height() {
        let mut plane = Plane::centered(DVec3::ZERO);
        plane.set_euler(DVec3::new(0.0, 90.0, 0.0));
        assert!(!plane.set_height(5.0));
    }

    #[test]
    fn euler_roundtrips() {
        let mut plane = Plane::centered(DVec3::ZERO);
        plane.set_euler(DVec3::new(20.0, -35.0, 70.0));
        let e = plane.euler();
        assert!((e - DVec3::new(20.0, -35.0, 70.0)).length() < 1e-9);
    }

    #[test]
    fn plane_outside_bounds_does_not_cross() {
        let mut plane = Plane::centered(DVec3::new(0.0, 0.0, 50.0));
        assert!(!plane.crosses(DVec3::ZERO, DVec3::splat(10.0)));
        plane.set_height(5.0);
        assert!(plane.crosses(DVec3::ZERO, DVec3::splat(10.0)));
    }

    #[test]
    fn defaults_match_orca_and_options_follow_choices() {
        let o = Choices::default().options();
        assert!(o.keep_upper && o.keep_lower);
        assert!(o.upper_place_on_cut && !o.lower_place_on_cut);
        assert!(!o.upper_flip && !o.lower_flip && !o.cut_to_parts);
    }

    #[test]
    fn at_least_one_side_is_kept() {
        let mut c = Choices::default();
        c.toggle_keep(0);
        assert_eq!(c.keep, [false, true]);
        c.toggle_keep(1);
        assert_eq!(c.keep, [false, true]);
        assert!(!c.options().keep_upper && !c.options().upper_place_on_cut);
    }

    #[test]
    fn place_and_flip_are_exclusive_and_parts_clear_them() {
        let mut c = Choices::default();
        c.toggle_flip(0);
        assert!(c.flip[0] && !c.place[0]);
        c.set_place(0, true);
        assert!(c.place[0] && !c.flip[0]);
        c.set_parts(true);
        let o = c.options();
        assert!(o.cut_to_parts && !o.upper_place_on_cut && !o.upper_flip);
        assert!(o.keep_upper && o.keep_lower);
    }
}
