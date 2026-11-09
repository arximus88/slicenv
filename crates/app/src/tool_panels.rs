//! Панелі параметрів активного інструмента сцени, як панель маніпуляцій Bambu Studio:
//! переміщення (позиція), поворот (відносний / абсолютний), масштаб (відсотки й розміри)
//! та режим «покласти на грань». Живуть над в’юпортом під хлібними крихтами, поки інструмент
//! активний і щось вибрано. Значення застосовуються через редагування проєкту (`edit.rs`).

use gpui::{
    AnyElement, Context, Entity, Focusable as _, Hsla, Rgba, SharedString, Window, div, prelude::*,
    px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use project::{Bounds, DAffine3, DQuat, DVec3};

use crate::{
    theme::{MONO_FONT, zed},
    tools::Tool,
    ui::{self, IconName},
    widgets::{self, ToastKind},
    workspace::Workspace,
};

/// Система координат для переміщення й повороту (як «Світ / Об’єкт» у Bambu Studio).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CoordSpace {
    #[default]
    World,
    Object,
}

/// Що вважати гранню в режимі «покласти на грань».
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LayMode {
    /// Плоскі грані опуклого корпусу моделі.
    #[default]
    Hull,
    /// Копланарна область навколо трикутника під курсором.
    Facet,
}

#[derive(Clone, Copy)]
enum Field {
    Position(usize),
    Rotate(usize),
    Percent(usize),
    Size(usize),
}

/// Зміна, яку панель накладає на вибрану ціль.
#[derive(Clone, Copy)]
enum Op {
    Translate(DVec3),
    Rotate(DQuat),
    /// Множники вздовж власних осей цілі.
    ScaleLocal(DVec3),
}

pub struct ToolPanel {
    pub space: CoordSpace,
    pub lay_mode: LayMode,
    /// «Однорідний масштаб»: зміна однієї осі масштабує всі три.
    pub uniform: bool,
    position: [Entity<InputState>; 3],
    rotate: [Entity<InputState>; 3],
    percent: [Entity<InputState>; 3],
    size: [Entity<InputState>; 3],
    /// Поле «нова відстань» інструмента вимірювання.
    pub measure_distance: Entity<InputState>,
}

/// Поточна ціль: світова матриця, нерозтягнуті розміри в її осях і габарити у світі.
struct Target {
    world: DAffine3,
    extent: DVec3,
    bounds: Bounds,
}

/// Числа для полів панелі.
struct Values {
    position: DVec3,
    percent: DVec3,
    size: DVec3,
    absolute: DVec3,
}

impl ToolPanel {
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
                        let text = input.read(cx).value().to_string();
                        this.commit_field(field, &text, window, cx);
                    }
                },
            ));
            input
        };
        let panel = Self {
            space: CoordSpace::World,
            lay_mode: LayMode::Hull,
            uniform: true,
            position: [0, 1, 2].map(|i| make(Field::Position(i), window, cx)),
            rotate: [0, 1, 2].map(|i| make(Field::Rotate(i), window, cx)),
            percent: [0, 1, 2].map(|i| make(Field::Percent(i), window, cx)),
            size: [0, 1, 2].map(|i| make(Field::Size(i), window, cx)),
            measure_distance: {
                let input = cx.new(|cx| InputState::new(window, cx));
                subs.push(cx.subscribe_in(
                    &input,
                    window,
                    |this, input, event: &InputEvent, window, cx| {
                        if matches!(event, InputEvent::PressEnter { .. }) {
                            let text = input.read(cx).value().to_string();
                            this.scale_by_measure(&text, window, cx);
                        }
                    },
                ));
                input
            },
        };
        (panel, subs)
    }
}

fn unit(i: usize) -> DVec3 {
    [DVec3::X, DVec3::Y, DVec3::Z][i]
}

/// Вісь `i` цілі у світі (нормована) — осі об’єкта, а не світу.
fn local_axis(world: &DAffine3, i: usize) -> DVec3 {
    world.matrix3.col(i).try_normalize().unwrap_or(unit(i))
}

fn axis_lengths(world: &DAffine3) -> DVec3 {
    DVec3::new(
        world.matrix3.x_axis.length(),
        world.matrix3.y_axis.length(),
        world.matrix3.z_axis.length(),
    )
}

/// Кути X/Y/Z у градусах для обертання без масштабу (порядок Bambu: R = Rz·Ry·Rx).
fn euler_degrees(world: &DAffine3) -> DVec3 {
    let axes = [0, 1, 2].map(|i| local_axis(world, i));
    let rotation = DQuat::from_mat3(&glam::DMat3::from_cols(axes[0], axes[1], axes[2]));
    let (z, y, x) = rotation.to_euler(glam::EulerRot::ZYX);
    DVec3::new(x, y, z).map(f64::to_degrees)
}

pub(crate) fn number(value: f64, precision: usize) -> String {
    let text = format!("{value:.precision$}");
    if text
        .trim_start_matches('-')
        .chars()
        .all(|c| matches!(c, '0' | '.'))
    {
        text.trim_start_matches('-').to_string()
    } else {
        text
    }
}

pub(crate) fn parse(text: &str) -> Option<f64> {
    text.trim()
        .replace(',', ".")
        .trim_end_matches(['%', '°'])
        .trim_end_matches("мм")
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
}

fn axis_color(i: usize) -> Hsla {
    let [r, g, b] = viewer::AXIS_COLORS[i];
    Rgba { r, g, b, a: 1.0 }.into()
}

impl Workspace {
    pub(crate) fn tool_scrub_binding(&self, id: gpui::EntityId) -> Option<crate::scrub::Binding> {
        use crate::scrub::{Binding, Spec, Target, Transform};
        let panel = self.tool_panel.as_ref()?;
        if panel.measure_distance.entity_id() == id {
            let mut spec = Spec::unit(0.01);
            spec.min = 0.01;
            return Some(Binding {
                spec,
                target: Target::Text,
            });
        }
        for (inputs, kind, step) in [
            (&panel.position, 0, 0.1),
            (&panel.rotate, 1, 1.),
            (&panel.percent, 2, 1.),
            (&panel.size, 3, 0.1),
        ] {
            if let Some(i) = inputs.iter().position(|v| v.entity_id() == id) {
                let field = match kind {
                    0 => Transform::Position(i),
                    1 => Transform::Rotate(i),
                    2 => Transform::Percent(i),
                    _ => Transform::Size(i),
                };
                let mut spec = Spec::unit(step);
                if kind >= 2 {
                    spec.min = 0.1;
                    if kind == 2 {
                        spec.max = 10000.;
                    }
                }
                return Some(Binding {
                    spec,
                    target: Target::Transform(field),
                });
            }
        }
        None
    }
    pub(crate) fn scrub_snapshot(&self) -> Option<crate::scrub::Snapshot> {
        let target = self.panel_target()?;
        let key = self.selected?;
        let local = if let Some(index) = self.selected_volume {
            self.project
                .as_ref()?
                .object(key.object)
                .ok()?
                .volumes
                .get(index)?
                .transform
        } else {
            target.world
        };
        Some(crate::scrub::Snapshot {
            key,
            volume: self.selected_volume,
            world: target.world,
            local,
            bounds: target.bounds,
            extent: target.extent,
            space: self
                .tool_panel
                .as_ref()
                .map_or(CoordSpace::World, |p| p.space),
            uniform: self.tool_panel.as_ref().is_none_or(|p| p.uniform),
        })
    }
    pub(crate) fn restore_scrub_transform(&mut self, s: crate::scrub::Snapshot) {
        if let Some(project) = &mut self.project {
            if let Some(index) = s.volume {
                if let Some(v) = project
                    .objects
                    .iter_mut()
                    .find(|o| o.id == s.key.object)
                    .and_then(|o| o.volumes.get_mut(index))
                {
                    v.transform = s.local;
                }
            } else if let Ok(instance) = project.instance_mut(s.key) {
                instance.transform = s.local;
            }
        }
    }
    pub(crate) fn scrub_transform(
        &mut self,
        s: crate::scrub::Snapshot,
        field: crate::scrub::Transform,
        value: f64,
        start: f64,
    ) {
        use crate::scrub::Transform;
        let center = s.bounds.center();
        let pivot = |change| {
            DAffine3::from_translation(center) * change * DAffine3::from_translation(-center)
        };
        let change = match field {
            Transform::Position(i) => {
                let axis = if s.space == CoordSpace::Object {
                    local_axis(&s.world, i)
                } else {
                    unit(i)
                };
                DAffine3::from_translation(axis * (value - start))
            }
            Transform::Rotate(i) => {
                let axis = if s.space == CoordSpace::Object {
                    local_axis(&s.world, i)
                } else {
                    unit(i)
                };
                pivot(DAffine3::from_quat(DQuat::from_axis_angle(
                    axis,
                    (value - start).to_radians(),
                )))
            }
            Transform::Percent(i) | Transform::Size(i) => {
                let lengths = axis_lengths(&s.world);
                let current = if matches!(field, Transform::Size(_)) {
                    s.extent[i] * lengths[i]
                } else {
                    lengths[i] * 100.
                };
                if current <= 1e-9 {
                    return;
                }
                let mut factors = DVec3::ONE;
                if s.uniform {
                    factors = DVec3::splat(value / current);
                } else {
                    factors[i] = value / current;
                }
                let basis = s.world.matrix3;
                pivot(DAffine3::from_mat3(
                    basis * glam::DMat3::from_diagonal(factors) * basis.inverse(),
                ))
            }
            Transform::UniformPercent => pivot(DAffine3::from_scale(DVec3::splat(
                value / (axis_lengths(&s.world).x * 100.),
            ))),
        };
        let mut world = change * s.world;
        // Нижня межа з восьми кутів, без обходу трикутників під час руху.
        let mut bottom = f64::MAX;
        for i in 0..8 {
            let p = DVec3::new(
                if i & 1 == 0 {
                    s.bounds.min.x
                } else {
                    s.bounds.max.x
                },
                if i & 2 == 0 {
                    s.bounds.min.y
                } else {
                    s.bounds.max.y
                },
                if i & 4 == 0 {
                    s.bounds.min.z
                } else {
                    s.bounds.max.z
                },
            );
            bottom = bottom.min(change.transform_point3(p).z);
        }
        if bottom < 0. {
            world = DAffine3::from_translation(-DVec3::Z * bottom) * world;
        }
        if let Some(project) = &mut self.project {
            if let Some(index) = s.volume {
                let instance = project
                    .instance(s.key)
                    .map(|i| i.transform)
                    .unwrap_or(DAffine3::IDENTITY);
                if let Some(v) = project
                    .objects
                    .iter_mut()
                    .find(|o| o.id == s.key.object)
                    .and_then(|o| o.volumes.get_mut(index))
                {
                    v.transform = instance.inverse() * world;
                }
            } else if let Ok(instance) = project.instance_mut(s.key) {
                instance.transform = world;
            }
        }
        self.live_drag_moved(s.key);
        self.edited = true;
    }

    /// Ціль панелі: вибрана частина або весь вибраний інстанс.
    fn panel_target(&self) -> Option<Target> {
        if let Some(snapshot) = self.scrub.as_ref().and_then(|g| g.transform) {
            return Some(Target {
                world: snapshot.world,
                extent: snapshot.extent,
                bounds: snapshot.bounds,
            });
        }
        let key = self.selected?;
        let project = self.project.as_ref()?;
        let instance = project.instance(key).ok()?;
        let object = project.object(key.object).ok()?;
        let (mut lo, mut hi) = (DVec3::MAX, DVec3::MIN);
        let world;
        match self.selected_volume {
            Some(index) => {
                let volume = object.volumes.get(index)?;
                for point in &volume.mesh.positions {
                    let p = DVec3::from(*point);
                    lo = lo.min(p);
                    hi = hi.max(p);
                }
                world = instance.transform * volume.transform;
            }
            None => {
                for volume in object
                    .volumes
                    .iter()
                    .filter(|v| v.kind == project::VolumeKind::Part)
                {
                    for point in &volume.mesh.positions {
                        let p = volume.transform.transform_point3(DVec3::from(*point));
                        lo = lo.min(p);
                        hi = hi.max(p);
                    }
                }
                world = instance.transform;
            }
        }
        (lo.is_finite() && hi.is_finite()).then_some(())?;
        Some(Target {
            world,
            extent: hi - lo,
            bounds: self.selected_bounds()?,
        })
    }

    fn panel_values(&self) -> Option<Values> {
        let target = self.panel_target()?;
        let lengths = axis_lengths(&target.world);
        Some(Values {
            position: target.bounds.center(),
            percent: lengths * 100.0,
            size: target.extent * lengths,
            absolute: euler_degrees(&target.world),
        })
    }

    /// Підставляє в поля панелі поточні значення (крім поля, у якому зараз друкують).
    pub fn refresh_tool_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tool.is_none() {
            return;
        }
        if self.tool == Some(Tool::Cut) {
            return self.refresh_cut_fields(window, cx);
        }
        let (Some(values), Some(panel)) = (self.panel_values(), &self.tool_panel) else {
            return;
        };
        let position = match panel.space {
            CoordSpace::World => values.position,
            CoordSpace::Object => DVec3::ZERO,
        };
        let sets: [(&[Entity<InputState>; 3], DVec3, usize); 4] = [
            (&panel.position, position, 2),
            (&panel.rotate, DVec3::ZERO, 0),
            (&panel.percent, values.percent, 1),
            (&panel.size, values.size, 2),
        ];
        for (inputs, vector, precision) in sets {
            for (i, input) in inputs.iter().enumerate() {
                if input.focus_handle(cx).is_focused(window) {
                    continue;
                }
                let text = number(vector[i], precision);
                if input.read(cx).value() != text {
                    input.update(cx, |state, cx| state.set_value(text, window, cx));
                }
            }
        }
    }

    fn commit_field(
        &mut self,
        field: Field,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(target), Some(panel)) = (self.panel_target(), &self.tool_panel) else {
            return;
        };
        let (space, uniform) = (panel.space, panel.uniform);
        let Some(value) = parse(text) else {
            self.refresh_tool_panel(window, cx);
            return;
        };
        let lengths = axis_lengths(&target.world);
        let op = match field {
            Field::Position(i) => {
                let delta = match space {
                    CoordSpace::World => {
                        let mut delta = value - target.bounds.center()[i];
                        if i == 2 {
                            delta = delta.max(-target.bounds.min.z); // не нижче столу
                        }
                        unit(i) * delta
                    }
                    CoordSpace::Object => local_axis(&target.world, i) * value,
                };
                (delta.length() > 1e-6).then_some(Op::Translate(delta))
            }
            Field::Rotate(i) => {
                let axis = match space {
                    CoordSpace::World => unit(i),
                    CoordSpace::Object => local_axis(&target.world, i),
                };
                (value.abs() > 1e-9)
                    .then(|| Op::Rotate(DQuat::from_axis_angle(axis, value.to_radians())))
            }
            Field::Percent(i) | Field::Size(i) => {
                let current = match field {
                    Field::Percent(_) => lengths[i] * 100.0,
                    _ => target.extent[i] * lengths[i],
                };
                let factor = if current > 1e-9 { value / current } else { 1.0 };
                let mut factors = DVec3::ONE;
                if uniform {
                    factors = DVec3::splat(factor);
                } else {
                    factors[i] = factor;
                }
                let result = lengths * factors * 100.0;
                if value <= 0.0 || result.min_element() < 0.1 || result.max_element() > 10_000.0 {
                    self.toast(
                        ToastKind::Error,
                        crate::i18n::tr("Масштаб має бути від 0.1 до 10000 %"),
                        cx,
                    );
                    None
                } else {
                    ((factor - 1.0).abs() > 1e-9).then_some(Op::ScaleLocal(factors))
                }
            }
        };
        match op {
            Some(op) => self.apply_op(op, window, cx),
            None => self.refresh_tool_panel(window, cx),
        }
    }

    /// Накладає зміну на вибраний інстанс (через `edit.rs`) чи на вибрану частину.
    fn apply_op(&mut self, op: Op, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.selected_volume {
            return self.apply_op_to_volume(index, op, window, cx);
        }
        let label = match op {
            Op::Translate(_) => "Переміщення",
            Op::Rotate(_) => "Поворот",
            Op::ScaleLocal(_) => "Масштабування",
        };
        self.edit_selected(label, window, cx, move |project, key| {
            match op {
                Op::Translate(delta) => {
                    project.translate(key, delta)?;
                    if let Some(bounds) = project.instance_bounds(key)?
                        && bounds.min.z < 0.0
                    {
                        project.translate(key, DVec3::Z * -bounds.min.z)?;
                    }
                }
                Op::Rotate(rotation) => {
                    project.rotate(key, rotation)?;
                    project.drop_to_bed(key)?;
                }
                Op::ScaleLocal(factors) => {
                    project.scale_along_axes(key, factors)?;
                    project.drop_to_bed(key)?;
                }
            }
            Ok(Some(key))
        });
    }

    fn apply_op_to_volume(
        &mut self,
        index: usize,
        op: Op,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(key), Some(center)) = (self.selected, self.selected_bounds().map(|b| b.center()))
        else {
            return;
        };
        let label = match op {
            Op::Translate(_) => "Переміщення",
            Op::Rotate(_) => "Поворот",
            Op::ScaleLocal(_) => "Масштабування",
        };
        self.checkpoint(label);
        let Some(project) = self.project.as_mut() else {
            return;
        };
        let Ok(instance) = project.instance(key) else {
            return;
        };
        let instance = instance.transform;
        let Some(volume) = project
            .objects
            .iter_mut()
            .find(|o| o.id == key.object)
            .and_then(|o| o.volumes.get_mut(index))
        else {
            return;
        };
        let world = instance * volume.transform;
        let pivot = |change: DAffine3| {
            DAffine3::from_translation(center) * change * DAffine3::from_translation(-center)
        };
        let change = match op {
            Op::Translate(delta) => DAffine3::from_translation(delta),
            Op::Rotate(rotation) => pivot(DAffine3::from_quat(rotation)),
            Op::ScaleLocal(factors) => {
                let basis = world.matrix3;
                pivot(DAffine3::from_mat3(
                    basis * glam::DMat3::from_diagonal(factors) * basis.inverse(),
                ))
            }
        };
        volume.transform = instance.inverse() * change * world;
        self.project_changed(window, cx);
    }

    /// Скидає поворот інстансу до нуля, лишаючи масштаб (через `project.rotate`).
    fn reset_rotation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.panel_target() else {
            return;
        };
        let axes = [0, 1, 2].map(|i| local_axis(&target.world, i));
        let rotation = DQuat::from_mat3(&glam::DMat3::from_cols(axes[0], axes[1], axes[2]));
        if rotation.angle_between(DQuat::IDENTITY) < 1e-9 {
            return;
        }
        self.apply_op(Op::Rotate(rotation.inverse()), window, cx);
    }
}

// ---- вигляд ----

fn segmented(
    items: Vec<(
        SharedString,
        SharedString,
        bool,
        Box<dyn Fn(&mut Window, &mut gpui::App)>,
    )>,
) -> gpui::Div {
    let z = zed();
    div()
        .flex()
        .items_center()
        .gap(px(2.))
        .p(px(2.))
        .rounded(px(5.))
        .bg(z.element)
        .border_1()
        .border_color(z.border_variant)
        .children(items.into_iter().map(|(id, label, on, click)| {
            div()
                .id(id)
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .h(px(20.))
                .px(px(8.))
                .rounded(px(3.))
                .cursor_pointer()
                .text_xs()
                .text_color(if on { z.text } else { z.text_muted })
                .when(on, |d| d.bg(z.element_selected))
                .when(!on, |d| {
                    d.hover(|s| s.bg(z.element_hover).text_color(z.text))
                })
                .child(label)
                .on_click(move |_, window, cx| click(window, cx))
        }))
}

pub(crate) fn row(label: impl Into<SharedString>) -> gpui::Div {
    div().flex().items_center().gap(px(6.)).child(
        div()
            .flex_none()
            .w(px(82.))
            .text_xs()
            .text_color(zed().text_muted)
            .child(label.into()),
    )
}

fn axis_tag(i: usize) -> gpui::Div {
    div()
        .flex_none()
        .w(px(8.))
        .text_xs()
        .text_color(axis_color(i))
        .child(["X", "Y", "Z"][i])
}

pub(crate) fn input_row(
    label: SharedString,
    inputs: &[Entity<InputState>; 3],
    window: &Window,
    cx: &mut Context<Workspace>,
) -> gpui::Div {
    row(label).children((0..3).map(|i| {
        crate::scrub::zone(
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(px(3.))
                .child(axis_tag(i))
                .child(crate::scrub::field(
                    &inputs[i],
                    Input::new(&inputs[i]),
                    window,
                    cx,
                )),
            &inputs[i],
            window,
            cx,
        )
    }))
}

fn readout_row(label: SharedString, values: DVec3, precision: usize) -> gpui::Div {
    let z = zed();
    row(label).children((0..3).map(|i| {
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(px(3.))
            .child(axis_tag(i))
            .child(
                div()
                    .flex_1()
                    .px(px(4.))
                    .font_family(MONO_FONT)
                    .text_xs()
                    .text_color(z.text_muted)
                    .child(number(values[i], precision)),
            )
    }))
}

/// Панель параметрів активного інструмента; `None` — інструмент вимкнений чи нема цілі.
pub fn panel(ws: &Workspace, window: &Window, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let z = zed();
    let tool = ws.tool?;
    let state = ws.tool_panel.as_ref()?;
    let tr = crate::i18n::tr;
    let values = ws.panel_values()?;
    let entity = cx.entity();

    let space_switch = || {
        let choice = |space: CoordSpace, id: &'static str, label: &'static str| {
            let entity = entity.clone();
            (
                SharedString::from(id),
                SharedString::from(tr(label)),
                state.space == space,
                Box::new(move |window: &mut Window, cx: &mut gpui::App| {
                    entity.update(cx, |this, cx| {
                        if let Some(panel) = &mut this.tool_panel {
                            panel.space = space;
                        }
                        this.refresh_tool_panel(window, cx);
                        cx.notify();
                    })
                }) as Box<dyn Fn(&mut Window, &mut gpui::App)>,
            )
        };
        row(tr("Координати")).child(div().flex_1().child(segmented(vec![
            choice(CoordSpace::World, "space-world", "Світ"),
            choice(CoordSpace::Object, "space-object", "Об’єкт"),
        ])))
    };

    let mut body = div().flex().flex_col().gap(px(8.));
    match tool {
        Tool::Move => {
            let label = match state.space {
                CoordSpace::World => tr("Позиція, мм"),
                CoordSpace::Object => tr("Відносно, мм"),
            };
            body = body.child(space_switch()).child(input_row(
                label.into(),
                &state.position,
                window,
                cx,
            ));
        }
        Tool::Rotate => {
            let entity = entity.clone();
            body = body
                .child(space_switch())
                .child(input_row(
                    tr("Поворот, °").into(),
                    &state.rotate,
                    window,
                    cx,
                ))
                .child(readout_row(tr("Абсолютний, °").into(), values.absolute, 1))
                .child(
                    div().flex().justify_end().child(
                        ui::ghost_button(
                            "reset-rotation",
                            Some(IconName::RotateCcw),
                            "Скинути поворот",
                        )
                        .text_xs()
                        .on_click(move |_, window, cx| {
                            entity.update(cx, |this, cx| this.reset_rotation(window, cx))
                        }),
                    ),
                );
        }
        Tool::Scale => {
            let entity = entity.clone();
            body = body
                .child(input_row(
                    tr("Масштаб, %").into(),
                    &state.percent,
                    window,
                    cx,
                ))
                .child(input_row(tr("Розмір, мм").into(), &state.size, window, cx))
                .child(
                    widgets::checkbox("uniform-scale", state.uniform, false)
                        .child(div().text_xs().child(tr("Однорідний масштаб")))
                        .on_click(move |_, _, cx| {
                            entity.update(cx, |this, cx| {
                                if let Some(panel) = &mut this.tool_panel {
                                    panel.uniform = !panel.uniform;
                                }
                                cx.notify();
                            })
                        }),
                );
        }
        Tool::Cut => {
            body = crate::cut_tool::panel_body(ws, window, cx)?;
        }
        Tool::LayOnFace => {
            let choice = |mode: LayMode, id: &'static str, label: &'static str| {
                let entity = entity.clone();
                (
                    SharedString::from(id),
                    SharedString::from(tr(label)),
                    state.lay_mode == mode,
                    Box::new(move |_: &mut Window, cx: &mut gpui::App| {
                        entity.update(cx, |this, cx| {
                            if let Some(panel) = &mut this.tool_panel {
                                panel.lay_mode = mode;
                            }
                            this.face_hover = None;
                            this.hull_hover = None;
                            this.dirty = true;
                            cx.notify();
                        })
                    }) as Box<dyn Fn(&mut Window, &mut gpui::App)>,
                )
            };
            let hint = match state.lay_mode {
                LayMode::Hull => "Наведіть на плоску грань опуклого корпусу й клацніть.",
                LayMode::Facet => "Наведіть на грань моделі й клацніть.",
            };
            body = body
                .child(row(tr("Режим")).child(div().flex_1().child(segmented(vec![
                    choice(LayMode::Hull, "lay-hull", "Опуклий корпус"),
                    choice(LayMode::Facet, "lay-facet", "Грань"),
                ]))))
                .child(div().text_xs().text_color(z.text_muted).child(tr(hint)));
        }
        Tool::Measure => {
            body = body.child(crate::measure_tool::panel_body(ws, window, cx));
        }
    }

    let close = {
        let entity = entity.clone();
        ui::icon_button("tool-panel-close", IconName::X, "Закрити інструмент (Esc)").on_click(
            move |_, window, cx| entity.update(cx, |this, cx| this.set_tool(None, window, cx)),
        )
    };
    Some(
        ui::scene_control("tool-panel")
            .absolute()
            .top(px(10.))
            .left(px(10.))
            .w(px(if tool == Tool::Cut { 344. } else { 318. }))
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(10.))
            .rounded(px(8.))
            .bg(z.panel.opacity(0.97))
            .border_1()
            .border_color(z.border)
            .shadow_lg()
            .text_color(z.text)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(ui::icon(tool.icon()).text_color(z.accent))
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(tr(tool.title())),
                    )
                    .child(close),
            )
            .child(body)
            .into_any_element(),
    )
}
