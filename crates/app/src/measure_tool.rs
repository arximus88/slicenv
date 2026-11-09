//! Інструмент «Вимірювання» (U), як у Bambu Studio: наведення підсвічує ознаку поверхні
//! (точку, ребро, коло, площину), два кліки задають «Вибір 1» і «Вибір 2», панель показує
//! кут, відстань і різниці X/Y/Z, а олівець масштабує об’єкт під задану відстань.
//!
//! Геометрія інстансу береться в координатах його пластини; промені й малювання зсуваються
//! на `instance_offset`, тож інструмент працює на будь-якій пластині спільної сцени.

use std::sync::Arc;

use glam::{DVec3, Vec3};
use gpui::{Context, Focusable as _, Pixels, Point, SharedString, Window, div, prelude::*, px};
use project::InstanceRef;
use viewer::{
    FlatVertex,
    measure::{HoverOptions, MeasurementResult, MeasuringMesh, SingleInfo, SurfaceFeature},
};

use crate::{
    theme::{MONO_FONT, zed},
    tools::Tool,
    ui::{self, IconName},
    workspace::Workspace,
};

/// Кольори в лінійному просторі (як решта оверлеїв).
const HOVER: [f32; 4] = [0.175, 0.418, 0.807, 0.75];
const FIRST: [f32; 4] = [0.0, 0.62, 0.78, 0.9];
const SECOND: [f32; 4] = [0.78, 0.05, 0.62, 0.9];
const DIMENSION: [f32; 4] = [1.0, 0.42, 0.04, 1.0];

/// Стан відкритого інструмента: сітка для пошуку ознак і вибрані ознаки.
pub struct MeasureState {
    key: InstanceRef,
    revision: u64,
    mesh: Arc<MeasuringMesh>,
    /// Трикутники інстансу (координати пластини) — для заливки площин.
    triangles: Vec<[Vec3; 3]>,
    pub hover: Option<SurfaceFeature>,
    pub selection: Vec<SurfaceFeature>,
    /// Відкрите поле «нова відстань» біля олівця.
    pub editing: bool,
}

impl MeasureState {
    fn build(project: &project::Project, key: InstanceRef, revision: u64) -> Option<Self> {
        let triangles: Vec<[Vec3; 3]> = project
            .world_triangles(key)
            .ok()?
            .into_iter()
            .map(|(_, _, t)| t.map(|p| p.as_vec3()))
            .collect();
        let positions: Vec<[f32; 3]> = triangles
            .iter()
            .flat_map(|t| t.map(|p| p.to_array()))
            .collect();
        let indices: Vec<[u32; 3]> = (0..triangles.len() as u32)
            .map(|i| [i * 3, i * 3 + 1, i * 3 + 2])
            .collect();
        let mesh = MeasuringMesh::from_indexed(&positions, &indices, glam::Mat4::IDENTITY);
        Some(Self {
            key,
            revision,
            mesh: Arc::new(mesh),
            triangles,
            hover: None,
            selection: Vec::new(),
            editing: false,
        })
    }

    pub fn result(&self) -> Option<MeasurementResult> {
        match self.selection.as_slice() {
            [a, b] => Some(viewer::measure::measure(a, b)),
            _ => None,
        }
    }
}

impl Workspace {
    /// Сітка вимірювання для вибраного інстансу; перебудовується після зміни геометрії.
    /// Вибір скидається, бо старі ознаки належать старій геометрії.
    pub fn ensure_measure(&mut self) {
        if self.tool != Some(Tool::Measure) {
            self.measure = None;
            return;
        }
        let Some(key) = self.selected else {
            self.measure = None;
            return;
        };
        let fresh = self
            .measure
            .as_ref()
            .is_some_and(|m| m.key == key && m.revision == self.geometry_revision);
        if !fresh {
            let _profile = paths::profile::Span::new("measure_build");
            self.measure = self
                .project
                .as_ref()
                .and_then(|p| MeasureState::build(p, key, self.geometry_revision));
        }
    }

    /// Наведення: ознака під курсором. `true` — змінилась.
    pub fn measure_hover(&mut self, position: Point<Pixels>) -> bool {
        if self.tool != Some(Tool::Measure) {
            return false;
        }
        self.ensure_measure();
        let ray = self.ray_at(position);
        let Some(state) = &self.measure else {
            return false;
        };
        let offset = self.instance_offset(state.key);
        let hover = ray.and_then(|(origin, dir)| {
            state
                .mesh
                .feature_at_with(origin - offset, dir, HoverOptions::default())
        });
        let state = self.measure.as_mut().expect("checked above");
        if state.hover == hover {
            return false;
        }
        state.hover = hover;
        self.dirty = true;
        true
    }

    /// Клік: ознака під курсором стає вибором (повторний клік знімає її).
    /// `false` — під курсором немає ознаки, клік обробляється як звичайний.
    pub fn measure_click(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        if self.tool != Some(Tool::Measure) {
            return false;
        }
        self.measure_hover(position);
        let Some(state) = &mut self.measure else {
            return false;
        };
        let Some(feature) = state.hover.clone() else {
            return false;
        };
        if let Some(i) = state.selection.iter().position(|f| *f == feature) {
            state.selection.remove(i);
        } else if state.selection.len() < 2 {
            state.selection.push(feature);
        } else {
            state.selection[1] = feature;
        }
        state.editing = false;
        self.refresh_overlay();
        self.dirty = true;
        cx.notify();
        true
    }

    /// Esc: спершу знімає «Вибір 2», потім «Вибір 1». `false` — вибору вже немає.
    pub fn measure_escape(&mut self) -> bool {
        let Some(state) = self.measure.as_mut().filter(|_| self.tool == Some(Tool::Measure))
        else {
            return false;
        };
        state.editing = false;
        if state.selection.pop().is_none() {
            return false;
        }
        self.refresh_overlay();
        self.dirty = true;
        true
    }

    fn measure_reset(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.measure
            && index < state.selection.len()
        {
            state.selection.remove(index);
            state.editing = false;
        }
        self.refresh_overlay();
        self.dirty = true;
        cx.notify();
    }

    /// Олівець біля відстані: рівномірний масштаб об’єкта, щоб відстань стала `text` мм.
    pub fn scale_by_measure(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let old = self
            .measure
            .as_ref()
            .and_then(|m| m.result())
            .and_then(|r| r.primary_distance())
            .map(|d| d.dist);
        let new = text.trim().replace(',', ".").parse::<f64>().ok();
        let Some(factor) = old
            .zip(new)
            .and_then(|(old, new)| viewer::measure::scale_factor_for(old, new))
        else {
            self.toast(
                crate::widgets::ToastKind::Error,
                crate::i18n::tr("Потрібна додатна відстань у мм"),
                cx,
            );
            return;
        };
        if let Some(state) = &mut self.measure {
            state.editing = false;
        }
        // Поле сховане — фокус назад сцені, інакше Esc і гарячі клавіші йдуть у нього.
        window.focus(&self.focus, cx);
        self.edit_selected("Масштабування", window, cx, move |project, key| {
            project.scale(key, DVec3::splat(factor))?;
            project.drop_to_bed(key)?;
            Ok(Some(key))
        });
    }

    /// Оверлей поверх сцени: наведена й вибрані ознаки та розмірна лінія.
    /// Повертає (заливки площин з перевіркою глибини, лінії й точки поверх усього).
    pub fn measure_overlay(&self) -> (Vec<FlatVertex>, Vec<FlatVertex>) {
        let (mut surface, mut lines) = (Vec::new(), Vec::new());
        let (Some(state), Some(screen)) = (&self.measure, self.screen()) else {
            return (surface, lines);
        };
        if self.tool != Some(Tool::Measure) {
            return (surface, lines);
        }
        let offset = self.instance_offset(state.key);
        let mut pen = Pen {
            screen: &screen,
            eye: self.camera.eye(),
            offset,
            lines: &mut lines,
        };
        let mut draw = |feature: &SurfaceFeature, color: [f32; 4], pen: &mut Pen| match feature {
            SurfaceFeature::Plane(plane) => {
                let n = plane.normal.as_vec3() * 0.04;
                for &face in state.mesh.plane_triangles(plane.index) {
                    if let Some(tri) = state.triangles.get(face as usize) {
                        for p in tri {
                            surface.push(FlatVertex {
                                pos: (*p + n + offset).to_array(),
                                color: [color[0], color[1], color[2], 0.45],
                            });
                        }
                    }
                }
                for edge in &plane.outline.edges {
                    pen.line(edge.a, edge.b, 2.0, color);
                }
                for circle in &plane.outline.circles {
                    pen.circle(circle.center, circle.normal, circle.radius, 2.0, color);
                }
            }
            SurfaceFeature::Edge(edge) => pen.line(edge.a, edge.b, 4.0, color),
            SurfaceFeature::Circle(circle) => {
                pen.circle(circle.center, circle.normal, circle.radius, 4.0, color);
                pen.dot(circle.center, 7.0, color);
            }
            SurfaceFeature::Point(p) => pen.dot(*p, 9.0, color),
        };
        for (i, feature) in state.selection.iter().enumerate() {
            draw(feature, if i == 0 { FIRST } else { SECOND }, &mut pen);
        }
        if let Some(hover) = &state.hover
            && !state.selection.contains(hover)
        {
            draw(hover, HOVER, &mut pen);
        }
        if let Some(d) = state.result().and_then(|r| r.primary_distance()) {
            pen.line(d.from, d.to, 2.5, DIMENSION);
            pen.dot(d.from, 7.0, DIMENSION);
            pen.dot(d.to, 7.0, DIMENSION);
        }
        (surface, lines)
    }
}

/// Малювання ліній і точок постійної екранної товщини стрічками, повернутими до камери.
struct Pen<'a, 'b> {
    screen: &'a viewer::Screen<'b>,
    eye: Vec3,
    offset: Vec3,
    lines: &'a mut Vec<FlatVertex>,
}

impl Pen<'_, '_> {
    /// Скільки міліметрів сцени займає один піксель біля точки `p`.
    fn mm_per_px(&self, p: Vec3) -> f32 {
        let view = (p - self.eye).normalize_or_zero();
        let side = view.cross(Vec3::Z).try_normalize().unwrap_or(Vec3::X);
        match (self.screen.project(p), self.screen.project(p + side)) {
            (Some(a), Some(b)) if (b - a).length() > 1e-4 => 1.0 / (b - a).length(),
            _ => 0.0,
        }
    }

    fn quad(&mut self, corners: [Vec3; 4], color: [f32; 4]) {
        for i in [0, 1, 2, 0, 2, 3] {
            self.lines.push(FlatVertex {
                pos: corners[i].to_array(),
                color,
            });
        }
    }

    fn line(&mut self, a: DVec3, b: DVec3, px: f32, color: [f32; 4]) {
        let (a, b) = (a.as_vec3() + self.offset, b.as_vec3() + self.offset);
        let mid = (a + b) * 0.5;
        let Some(side) = (b - a).cross(self.eye - mid).try_normalize() else {
            return;
        };
        let half = side * self.mm_per_px(mid) * px * 0.5;
        self.quad([a - half, b - half, b + half, a + half], color);
    }

    fn dot(&mut self, p: DVec3, px: f32, color: [f32; 4]) {
        let p = p.as_vec3() + self.offset;
        let view = (p - self.eye).normalize_or_zero();
        let right = view.cross(Vec3::Z).try_normalize().unwrap_or(Vec3::X);
        let up = right.cross(view).normalize_or_zero();
        let r = self.mm_per_px(p) * px * 0.5;
        let (right, up) = (right * r, up * r);
        self.quad([p - right - up, p + right - up, p + right + up, p - right + up], color);
    }

    fn circle(&mut self, center: DVec3, normal: DVec3, radius: f64, px: f32, color: [f32; 4]) {
        let u = normal.any_orthonormal_vector();
        let v = normal.cross(u);
        const STEPS: usize = 64;
        let at = |i: usize| {
            let t = i as f64 / STEPS as f64 * std::f64::consts::TAU;
            center + (u * t.cos() + v * t.sin()) * radius
        };
        for i in 0..STEPS {
            self.line(at(i), at(i + 1), px, color);
        }
    }
}

fn fmt_mm(v: f64) -> String {
    format!("{v:.3} mm")
}

fn describe(feature: &SurfaceFeature) -> String {
    let tr = crate::i18n::tr;
    match feature.describe() {
        SingleInfo::Point { position } => format!(
            "{} ({:.2}, {:.2}, {:.2})",
            tr("Точка"),
            position.x,
            position.y,
            position.z
        ),
        SingleInfo::Edge { length, .. } => {
            crate::tr_format!("Ребро (довжина: {})", fmt_mm(length))
        }
        SingleInfo::Circle { diameter, .. } => {
            crate::tr_format!("Коло (діаметр: {})", fmt_mm(diameter))
        }
        SingleInfo::Plane { area, .. } => crate::tr_format!("Площина (площа: {} мм²)", format!("{area:.2}")),
    }
}

/// Тіло панелі інструмента «Вимірювання».
pub fn panel_body(ws: &Workspace, window: &Window, cx: &mut Context<Workspace>) -> gpui::Div {
    let z = zed();
    let tr = crate::i18n::tr;
    let mut body = div().flex().flex_col().gap(px(6.));
    let Some(state) = &ws.measure else {
        return body.child(
            div()
                .text_xs()
                .text_color(z.text_muted)
                .child(tr("Немає геометрії для вимірювання.")),
        );
    };
    let copy_button = |id: &'static str, text: String| {
        ui::icon_button(id, IconName::Copy, "Копіювати").on_click(move |_, _, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()))
        })
    };
    for i in 0..2 {
        let (label, color) = if i == 0 {
            (tr("Вибір 1"), FIRST)
        } else {
            (tr("Вибір 2"), SECOND)
        };
        let swatch = gpui::Rgba {
            r: color[0].powf(1.0 / 2.2),
            g: color[1].powf(1.0 / 2.2),
            b: color[2].powf(1.0 / 2.2),
            a: 1.0,
        };
        let text = state.selection.get(i).map(describe);
        body = body.child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .h(px(24.))
                .child(div().size(px(8.)).rounded_full().bg(swatch))
                .child(
                    div()
                        .flex_none()
                        .w(px(64.))
                        .text_xs()
                        .text_color(z.text_muted)
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(if text.is_some() { z.text } else { z.text_muted })
                        .child(text.clone().unwrap_or_else(|| tr("клацніть ознаку").to_string())),
                )
                .when(text.is_some(), |d| {
                    d.child(
                        ui::icon_button(
                            SharedString::from(format!("measure-reset-{i}")),
                            IconName::RotateCcw,
                            "Скинути вибір",
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.measure_reset(i, cx))),
                    )
                }),
        );
    }
    let Some(result) = state.result() else {
        let hint = if state.selection.is_empty() {
            "Наведіть на точку, ребро, коло чи площину й клацніть."
        } else {
            "Виберіть другу ознаку, щоб виміряти відстань і кут."
        };
        return body.child(div().text_xs().text_color(z.text_muted).child(tr(hint)));
    };
    let value_row = |label: SharedString, value: String| {
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .h(px(24.))
            .child(
                div()
                    .flex_none()
                    .w(px(78.))
                    .text_xs()
                    .text_color(z.text_muted)
                    .child(label),
            )
            .child(
                div()
                    .flex_1()
                    .font_family(MONO_FONT)
                    .text_xs()
                    .child(value),
            )
    };
    body = body.child(div().h(px(1.)).bg(z.border_variant));
    if let Some(angle) = result.angle {
        let text = format!("{:.3}°", angle.radians.to_degrees());
        body = body.child(
            value_row(tr("Кут").into(), text.clone()).child(copy_button("measure-copy-angle", text)),
        );
    }
    if let Some(d) = result.primary_distance() {
        let text = fmt_mm(d.dist);
        let pencil = ui::icon_button("measure-edit", IconName::Pencil, "Змінити відстань масштабом")
            .on_click(cx.listener(|this, _, window, cx| {
                if let Some(state) = &mut this.measure {
                    state.editing = !state.editing;
                }
                if let Some(panel) = &this.tool_panel {
                    let input = panel.measure_distance.clone();
                    window.focus(&input.focus_handle(cx), cx);
                }
                cx.notify();
            }));
        body = body.child(
            value_row(tr("Відстань").into(), text.clone())
                .child(pencil)
                .child(copy_button("measure-copy-distance", format!("{:.3}", d.dist))),
        );
        if state.editing
            && let Some(panel) = &ws.tool_panel
        {
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_none()
                            .w(px(78.))
                            .text_xs()
                            .text_color(z.text_muted)
                            .child(tr("Нова, мм")),
                    )
                    .child(div().flex_1().child(crate::scrub::field(
                        &panel.measure_distance,
                        gpui_component::input::Input::new(&panel.measure_distance),
                        window,
                        cx,
                    ))),
            );
        }
    }
    if let Some(xyz) = result.distance_xyz {
        let text = format!("{:.3}  {:.3}  {:.3}", xyz.x.abs(), xyz.y.abs(), xyz.z.abs());
        body = body.child(value_row("X  Y  Z".into(), text));
    }
    body
}

/// Підпис розмірної лінії в екранних координатах в’юпорту.
pub fn labels(ws: &Workspace) -> Option<impl IntoElement> {
    if ws.tool != Some(Tool::Measure) {
        return None;
    }
    let state = ws.measure.as_ref()?;
    let d = state.result()?.primary_distance()?;
    let screen = ws.screen()?;
    let mid = ((d.from + d.to) * 0.5).as_vec3() + ws.instance_offset(state.key);
    let at = screen.project(mid)?;
    let z = zed();
    Some(
        div()
            .absolute()
            .left(px(at.x + 8.))
            .top(px(at.y - 22.))
            .px(px(6.))
            .py(px(2.))
            .rounded(px(4.))
            .bg(z.panel.opacity(0.94))
            .border_1()
            .border_color(z.border_variant)
            .font_family(MONO_FONT)
            .text_xs()
            .text_color(z.text)
            .child(fmt_mm(d.dist)),
    )
}
