//! Спільна поведінка числових полів: жест, математика та відкладене підтвердження.
use crate::workspace::Workspace;
use gpui::{
    Context, DispatchPhase, Div, Entity, Focusable as _, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Window, canvas, prelude::*,
};
use gpui_component::input::{Input, InputEvent, InputState};
use project::{DAffine3, DVec3};

#[derive(Clone, Copy, Debug)]
pub struct Spec {
    pub integer: bool,
    pub step: f64,
    pub min: f64,
    pub max: f64,
}
impl Spec {
    pub fn unit(step: f64) -> Self {
        Self {
            integer: false,
            step,
            min: -f64::MAX,
            max: f64::MAX,
        }
    }
    pub fn value(self, start: f64, delta: f64, fine: bool, snap: bool) -> f64 {
        let value = start + delta * self.step * if fine { 0.1 } else { 1.0 };
        let value = if snap {
            (value / self.step).round() * self.step
        } else {
            value
        };
        let value = if self.integer { value.round() } else { value };
        value.clamp(self.min, self.max)
    }
    pub fn option(def: &settings::OptionDef, value: f64) -> Option<Self> {
        use settings::OptionType::*;
        if !matches!(def.kind.scalar(), Float | Int | Percent | FloatOrPercent) {
            return None;
        }
        let step = if matches!(def.kind.scalar(), Int | Percent)
            || def.sidetext.trim() == "%"
            || def.sidetext.contains('°')
        {
            1.0
        } else if value.abs() < 1.0 {
            0.01
        } else {
            0.1
        };
        Some(Self {
            integer: matches!(def.kind.scalar(), Int),
            step,
            min: def.min.unwrap_or(-f64::MAX),
            max: def.max.unwrap_or(f64::MAX),
        })
    }
}
#[derive(Clone, Copy)]
pub enum Transform {
    Position(usize),
    Rotate(usize),
    Percent(usize),
    Size(usize),
    UniformPercent,
}
#[derive(Clone)]
pub enum Target {
    Text,
    Setting(settings::PresetType, String),
    Transform(Transform),
}
#[derive(Clone)]
pub struct Binding {
    pub spec: Spec,
    pub target: Target,
}
pub struct Gesture {
    input: Entity<InputState>,
    binding: Binding,
    start_x: f64,
    start: f64,
    text: String,
    active: bool,
    pub transform: Option<Snapshot>,
}
#[derive(Clone, Copy)]
pub struct Snapshot {
    pub key: project::InstanceRef,
    pub volume: Option<usize>,
    pub world: DAffine3,
    pub local: DAffine3,
    pub bounds: project::Bounds,
    pub extent: DVec3,
    pub space: crate::tool_panels::CoordSpace,
    pub uniform: bool,
}
fn parse(text: &str) -> Option<f64> {
    text.trim()
        .trim_end_matches(['%', '°'])
        .replace(',', ".")
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
}

/// Та сама рамка й текстовий редактор; захоплення миші лише поза режимом набору.
pub fn field(
    input: &Entity<InputState>,
    editor: Input,
    window: &Window,
    cx: &Context<Workspace>,
) -> Div {
    zone(
        crate::widgets::field(input, editor, window, cx),
        input,
        window,
        cx,
    )
}
pub fn zone(div: Div, input: &Entity<InputState>, window: &Window, cx: &Context<Workspace>) -> Div {
    let input = input.clone();
    let editing = input.focus_handle(cx).is_focused(window);
    div.relative().when(!editing, |div| {
        div.child(gpui::div().absolute().inset_0().cursor_ew_resize())
            .cursor_ew_resize()
            .capture_any_mouse_down(
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    if event.button != MouseButton::Left {
                        return;
                    }
                    let text = input.read(cx).value().to_string();
                    let Some(start) = parse(&text) else {
                        return;
                    };
                    let mut binding = this
                        .scrub_bindings
                        .get(&input.entity_id())
                        .cloned()
                        .or_else(|| this.transform_scrub_binding(input.entity_id()))
                        .or_else(|| this.tool_scrub_binding(input.entity_id()))
                        .or_else(|| this.cut_scrub_binding(input.entity_id()))
                        .unwrap_or(Binding {
                            spec: Spec::unit(0.1),
                            target: Target::Text,
                        });
                    if text.trim().ends_with('%') {
                        binding.spec.step = 1.0;
                    }
                    this.scrub = Some(Gesture {
                        input: input.clone(),
                        binding,
                        start_x: f64::from(event.position.x),
                        start,
                        text,
                        active: false,
                        transform: None,
                    });
                    window.focus(&this.focus, cx);
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
    })
}
/// Віконне захоплення зберігає жест, коли курсор покидає поле чи панель.
pub fn capture(cx: &Context<Workspace>) -> impl IntoElement {
    let entity = cx.entity().downgrade();
    canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            let release = entity.clone();
            window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                    let _ = release.update(cx, |this, cx| this.scrub_release(event, window, cx));
                }
            });
            let entity = entity.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture {
                    let _ = entity.update(cx, |this, cx| this.scrub_move(event, window, cx));
                }
            });
        },
    )
}
impl Workspace {
    pub fn bind_scrub(&mut self, input: &Entity<InputState>, spec: Spec, target: Target) {
        self.scrub_bindings
            .insert(input.entity_id(), Binding { spec, target });
    }
    fn scrub_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut gesture) = self.scrub.take() else {
            return;
        };
        let delta = f64::from(event.position.x) - gesture.start_x;
        if !gesture.active && delta.abs() >= 3.0 {
            let _profile = paths::profile::Span::new("numeric_scrub_begin");
            if let Target::Transform(_) = gesture.binding.target {
                gesture.transform = self.scrub_snapshot();
                if self
                    .transform_scrub_binding(gesture.input.entity_id())
                    .is_some()
                {
                    if let Some(snapshot) = &mut gesture.transform {
                        snapshot.space = crate::tool_panels::CoordSpace::World;
                        snapshot.uniform = true;
                    }
                }
                if let (Target::Transform(field), Some(snapshot)) =
                    (&gesture.binding.target, gesture.transform)
                {
                    let lengths = DVec3::new(
                        snapshot.world.matrix3.x_axis.length(),
                        snapshot.world.matrix3.y_axis.length(),
                        snapshot.world.matrix3.z_axis.length(),
                    );
                    let scaling = match field {
                        Transform::Percent(i) => Some((lengths[*i] * 100., *i, snapshot.uniform)),
                        Transform::Size(i) => {
                            Some((lengths[*i] * snapshot.extent[*i], *i, snapshot.uniform))
                        }
                        Transform::UniformPercent => Some((lengths.x * 100., 0, true)),
                        _ => None,
                    };
                    if let Some((current, i, uniform)) = scaling {
                        let low = if uniform {
                            lengths.min_element()
                        } else {
                            lengths[i]
                        };
                        let high = if uniform {
                            lengths.max_element()
                        } else {
                            lengths[i]
                        };
                        if low > 1e-9 && current > 1e-9 {
                            gesture.binding.spec.min = current * 0.001 / low;
                            gesture.binding.spec.max = current * 100. / high;
                        }
                    }
                }
                if let Some(snapshot) = gesture.transform {
                    self.checkpoint("Трансформація");
                    self.ensure_live_drag(snapshot.key);
                }
            }
            gesture.active = true;
        }
        if gesture.active {
            let _profile = paths::profile::Span::new("numeric_scrub_move");
            let value = gesture.binding.spec.value(
                gesture.start,
                delta,
                event.modifiers.shift,
                event.modifiers.control,
            );
            let suffix = if gesture.text.trim().ends_with('%') {
                "%"
            } else {
                ""
            };
            gesture.input.update(cx, |state, cx| {
                state.set_value(
                    format!(
                        "{}{suffix}",
                        format!("{value:.4}")
                            .trim_end_matches('0')
                            .trim_end_matches('.')
                    ),
                    window,
                    cx,
                )
            });
            if let (Target::Transform(field), Some(snapshot)) =
                (&gesture.binding.target, gesture.transform)
            {
                self.scrub_transform(snapshot, *field, value, gesture.start);
            }
            cx.notify();
            cx.stop_propagation();
        }
        self.scrub = Some(gesture);
    }
    pub fn scrub_release(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.scrub.is_none() {
            return;
        }
        self.scrub_move(
            &MouseMoveEvent {
                position: event.position,
                modifiers: event.modifiers,
                pressed_button: Some(MouseButton::Left),
            },
            window,
            cx,
        );
        self.scrub_end(false, window, cx);
    }
    pub fn scrub_end(&mut self, cancel: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(gesture) = self.scrub.take() else {
            return;
        };
        if !gesture.active {
            if !cancel {
                window.focus(&gesture.input.focus_handle(cx), cx);
            }
            return;
        }
        let _profile = paths::profile::Span::new("numeric_scrub_finish");
        if cancel {
            gesture
                .input
                .update(cx, |s, cx| s.set_value(gesture.text, window, cx));
        }
        match gesture.binding.target {
            Target::Transform(field) => {
                if cancel && let Some(s) = gesture.transform {
                    self.restore_scrub_transform(s);
                }
                if !cancel && !matches!(field, Transform::Position(_)) {
                    if let Some(snapshot) = gesture.transform.filter(|s| s.volume.is_none()) {
                        if let Some(project) = &mut self.project {
                            let _ = project.drop_to_bed(snapshot.key);
                        }
                    }
                }
                self.finish_live_drag();
                self.project_changed(window, cx);
            }
            Target::Setting(kind, key) if !cancel => {
                let text = gesture.input.read(cx).value().to_string();
                self.commit_value(kind, &key, &text, window, cx);
            }
            Target::Text if !cancel => gesture.input.update(cx, |_, cx| {
                cx.emit(InputEvent::PressEnter {
                    secondary: false,
                    shift: false,
                })
            }),
            _ => {}
        }
        cx.notify();
        cx.stop_propagation();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn math() {
        let s = Spec {
            integer: false,
            step: 1.0,
            min: 0.0,
            max: 200.0,
        };
        assert_eq!(s.value(100., 10., false, false), 110.);
        assert_eq!(s.value(100., 10., true, false), 101.);
        assert_eq!(s.value(100.3, 4., true, true), 101.);
        assert_eq!(s.value(1., -10., false, false), 0.);
        assert_eq!(s.value(199., 10., false, false), 200.);
    }
    #[test]
    fn schema_steps_and_integer_fine_control() {
        let schema = crate::preset_editor::schema();
        let layer = Spec::option(&schema.options["layer_height"], 0.2).unwrap();
        assert_eq!(layer.step, 0.01);
        assert_eq!(layer.value(0.2, -100., false, false), 0.);
        let integer = Spec::option(&schema.options["wall_loops"], 2.).unwrap();
        assert_eq!(integer.value(2., 10., true, false), 3.);
        assert_eq!(integer.value(2., 2., true, false), 2.);
        assert_eq!(integer.value(999., 10., false, false), 1000.);
        let percent = Spec::option(&schema.options["sparse_infill_density"], 15.).unwrap();
        assert_eq!(percent.step, 1.);
        assert_eq!(percent.value(90., 20., false, false), 100.);
    }
    #[test]
    fn fractional() {
        assert!((Spec::unit(0.01).value(0.2, 10., true, false) - 0.21).abs() < 1e-9);
    }
}
