//! Camera-synchronized navigation cube: face/edge/corner snapping and orbit drag.
use crate::{
    theme::zed,
    ui::{self, IconName},
    workspace::Workspace,
};
use glam::{Vec2, Vec3};
use gpui::{
    Bounds, Context, DispatchPhase, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PathBuilder, Pixels, Point, SharedString, TextAlign, TextRun, Window, canvas, div, point,
    prelude::*, px,
};
use std::time::Instant;

const SIZE: f32 = 132.;
const CENTER: Vec2 = Vec2::new(66., 60.);
const SCALE: f32 = 25.;
#[derive(Default)]
pub struct Navigation {
    bounds: Option<Bounds<Pixels>>,
    hover: Option<usize>,
    drag: Option<Drag>,
    tween: Option<Tween>,
}
struct Drag {
    origin: Point<Pixels>,
    last: Point<Pixels>,
    moved: bool,
    direction: Option<Vec3>,
}
struct Tween {
    from: [f32; 2],
    to: [f32; 2],
    start: Instant,
}
struct Face {
    normal: Vec3,
    points: [Vec2; 4],
    center: Vec2,
    label: &'static str,
    id: usize,
}

fn basis(camera: &viewer::Camera) -> (Vec3, Vec3, Vec3) {
    let dir = (camera.eye() - camera.target).normalize();
    let right = Vec3::new(-camera.yaw.sin(), camera.yaw.cos(), 0.);
    (right, right.cross(-dir).normalize(), dir)
}
fn project(p: Vec3, camera: &viewer::Camera) -> Vec2 {
    let (right, up, _) = basis(camera);
    CENTER + Vec2::new(p.dot(right), -p.dot(up)) * SCALE
}
fn faces(camera: &viewer::Camera) -> Vec<Face> {
    let (_, _, dir) = basis(camera);
    let mut faces = Vec::new();
    for (id, (normal, u, v, label)) in [
        (Vec3::X, Vec3::Y, Vec3::Z, "Право"),
        (-Vec3::X, -Vec3::Y, Vec3::Z, "Ліво"),
        (Vec3::Y, -Vec3::X, Vec3::Z, "Зад"),
        (-Vec3::Y, Vec3::X, Vec3::Z, "Перед"),
        (Vec3::Z, Vec3::X, Vec3::Y, "Верх"),
        (-Vec3::Z, Vec3::X, -Vec3::Y, "Низ"),
    ]
    .into_iter()
    .enumerate()
    {
        if normal.dot(dir) <= 0.0001 {
            continue;
        }
        let corners = [
            normal - u - v,
            normal + u - v,
            normal + u + v,
            normal - u + v,
        ];
        faces.push(Face {
            normal,
            points: corners.map(|p| project(p, camera)),
            center: project(normal, camera),
            label,
            id,
        });
    }
    faces
}
fn inside(p: Vec2, polygon: &[Vec2; 4]) -> bool {
    let mut positive = false;
    let mut negative = false;
    for i in 0..4 {
        let d = (polygon[(i + 1) % 4] - polygon[i]).perp_dot(p - polygon[i]);
        positive |= d > 0.001;
        negative |= d < -0.001;
    }
    !(positive && negative)
}
fn hit(p: Vec2, camera: &viewer::Camera) -> Option<(usize, Vec3)> {
    let (right, up, dir) = basis(camera);
    for face in faces(camera) {
        if !inside(p, &face.points) {
            continue;
        }
        let xy = (p - CENTER) / SCALE;
        let base = right * xy.x - up * xy.y;
        let t = (1. - face.normal.dot(base)) / face.normal.dot(dir);
        let local = base + dir * t;
        let mut snap = face.normal;
        for axis in 0..3 {
            if face.normal[axis] == 0. && local[axis].abs() > 0.65 {
                snap[axis] = local[axis].signum();
            }
        }
        return Some((face.id, snap.normalize()));
    }
    None
}
fn polygon(
    window: &mut Window,
    origin: Point<Pixels>,
    points: &[Vec2],
    color: gpui::Hsla,
    stroke: bool,
) {
    let mut path = if stroke {
        PathBuilder::stroke(px(1.2))
    } else {
        PathBuilder::fill()
    };
    for (i, p) in points.iter().enumerate() {
        let p = origin + point(px(p.x), px(p.y));
        if i == 0 {
            path.move_to(p)
        } else {
            path.line_to(p)
        }
    }
    if !points.is_empty() {
        path.close();
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}
fn text(
    window: &mut Window,
    cx: &mut gpui::App,
    origin: Point<Pixels>,
    center: Vec2,
    label: &str,
    color: gpui::Hsla,
    font_size: f32,
) {
    let value: SharedString = crate::i18n::tr(label).into();
    let run = TextRun {
        len: value.len(),
        font: gpui::font(crate::theme::UI_FONT),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window
        .text_system()
        .shape_line(value, px(font_size), &[run], None);
    let pos = origin
        + point(
            px(center.x) - line.width / 2.,
            px(center.y - font_size * 0.65),
        );
    line.paint(pos, px(font_size * 1.3), TextAlign::Left, None, window, cx)
        .ok();
}
impl Workspace {
    pub fn cancel_navigation(&mut self) {
        self.navigation.drag = None;
        self.navigation.tween = None;
        self.drag_moved = false;
    }
    pub fn navigation_animating(&self) -> bool {
        self.navigation.tween.is_some()
    }
    pub fn navigation_dragging(&self) -> bool {
        self.navigation.drag.is_some()
    }
    fn snap_navigation(&mut self, direction: Vec3) {
        let mut yaw = direction.y.atan2(direction.x);
        if direction.x.abs() + direction.y.abs() < 0.01 {
            yaw = self.camera.yaw;
        }
        let delta = (yaw - self.camera.yaw + std::f32::consts::PI)
            .rem_euclid(std::f32::consts::TAU)
            - std::f32::consts::PI;
        self.navigation.tween = Some(Tween {
            from: [self.camera.yaw, self.camera.pitch],
            to: [
                self.camera.yaw + delta,
                direction.z.asin().clamp(-1.5603, 1.5603),
            ],
            start: Instant::now(),
        });
    }
    fn finish_navigation(&mut self, cx: &mut Context<Self>) {
        if let Some(drag) = self.navigation.drag.take() {
            if !drag.moved {
                if let Some(direction) = drag.direction {
                    self.snap_navigation(direction);
                }
            }
            self.drag_moved = false;
            self.dirty = true;
            cx.notify();
        }
    }
    fn move_navigation(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(drag) = &mut self.navigation.drag else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.finish_navigation(cx);
            return;
        }
        let total = event.position - drag.origin;
        if !drag.moved && f32::from(total.x).abs() + f32::from(total.y).abs() < 3. {
            return;
        }
        drag.moved = true;
        let delta = event.position - drag.last;
        drag.last = event.position;
        self.camera.orbit(f32::from(delta.x), f32::from(delta.y));
        self.drag_moved = true;
        self.dirty = true;
        self.notify_viewport(cx);
    }
    fn tick_navigation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tween) = &self.navigation.tween else {
            return;
        };
        let t = (tween.start.elapsed().as_secs_f32() / 0.18).min(1.);
        let smooth = t * t * (3. - 2. * t);
        self.camera.yaw = tween.from[0] + (tween.to[0] - tween.from[0]) * smooth;
        self.camera.pitch = tween.from[1] + (tween.to[1] - tween.from[1]) * smooth;
        if t >= 1. {
            self.navigation.tween = None;
        }
        self.dirty = true;
        window.request_animation_frame();
        self.notify_viewport(cx);
    }
}

pub fn cube(ws: &Workspace, cx: &mut Context<Workspace>) -> gpui::AnyElement {
    if !ws.preferences.navigation_cube {
        return div().into_any_element();
    }
    let entity = cx.entity();
    let prepare = ws.page == crate::workspace::Page::Prepare;
    let right = if prepare { 12. } else { 76. };
    div()
        .id("navigation-cube")
        .occlude()
        .absolute()
        .top(px(12.))
        .right(px(right))
        .w(px(SIZE))
        .h(px(SIZE + 22.))
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, event: &MouseDownEvent, _, cx| {
                let Some(bounds) = this.navigation.bounds else {
                    return;
                };
                let p = event.position - bounds.origin;
                let direction =
                    hit(Vec2::new(f32::from(p.x), f32::from(p.y)), &this.camera).map(|(_, d)| d);
                this.navigation.tween = None;
                this.drag_from = None;
                this.moving = None;
                this.panning = None;
                this.navigation.drag = Some(Drag {
                    origin: event.position,
                    last: event.position,
                    moved: false,
                    direction,
                });
                cx.stop_propagation();
                cx.notify();
            }),
        )
        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
        .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
            if let Some(bounds) = this.navigation.bounds {
                let p = event.position - bounds.origin;
                let hovered =
                    hit(Vec2::new(f32::from(p.x), f32::from(p.y)), &this.camera).map(|(i, _)| i);
                if this.navigation.hover != hovered {
                    this.navigation.hover = hovered;
                    cx.notify();
                }
            }
            cx.stop_propagation();
        }))
        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
            if !hovered {
                this.navigation.hover = None;
                cx.notify();
            }
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                this.finish_navigation(cx);
                cx.stop_propagation();
            }),
        )
        .on_scroll(|_, _, cx| cx.stop_propagation())
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, (), window, cx| {
                    entity.update(cx, |this, cx| {
                        this.navigation.bounds = Some(bounds);
                        this.tick_navigation(window, cx);
                        let palette = zed();
                        for face in faces(&this.camera) {
                            let hover = this.navigation.hover == Some(face.id);
                            let shade = 0.35 + face.normal.dot(basis(&this.camera).2) * 0.35;
                            polygon(
                                window,
                                bounds.origin,
                                &face.points,
                                if hover {
                                    palette.accent.opacity(0.75)
                                } else {
                                    gpui::hsla(0.61, 0.12, shade, 0.97)
                                },
                                false,
                            );
                            polygon(
                                window,
                                bounds.origin,
                                &face.points,
                                if hover {
                                    palette.accent
                                } else {
                                    palette.text_muted.opacity(0.7)
                                },
                                true,
                            );
                            let min = face
                                .points
                                .iter()
                                .copied()
                                .fold(Vec2::splat(f32::INFINITY), Vec2::min);
                            let max = face
                                .points
                                .iter()
                                .copied()
                                .fold(Vec2::splat(f32::NEG_INFINITY), Vec2::max);
                            if (max.x - min.x) > 31.
                                && (max.y - min.y) > 15.
                                && face.normal.dot(basis(&this.camera).2) > 0.25
                            {
                                text(
                                    window,
                                    cx,
                                    bounds.origin,
                                    face.center,
                                    face.label,
                                    palette.text,
                                    10.,
                                );
                            }
                        }
                        let axis_origin = Vec2::new(30., 112.);
                        let (right, up, _) = basis(&this.camera);
                        for (axis, label, color) in [
                            (
                                Vec3::X,
                                "X",
                                gpui::rgb(if crate::theme::is_light() {
                                    0xb94444
                                } else {
                                    0xe08080
                                })
                                .into(),
                            ),
                            (
                                Vec3::Y,
                                "Y",
                                gpui::rgb(if crate::theme::is_light() {
                                    0x467c24
                                } else {
                                    0x9dc77e
                                })
                                .into(),
                            ),
                            (
                                Vec3::Z,
                                "Z",
                                gpui::rgb(if crate::theme::is_light() {
                                    0x2869ad
                                } else {
                                    0x80b9ef
                                })
                                .into(),
                            ),
                        ] {
                            let delta = Vec2::new(axis.dot(right), -axis.dot(up));
                            let end = axis_origin + delta * 15.;
                            polygon(window, bounds.origin, &[axis_origin, end], color, true);
                            text(
                                window,
                                cx,
                                bounds.origin,
                                end + delta * 8.,
                                label,
                                color,
                                9.,
                            );
                        }
                    });
                    let move_entity = entity.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                        if phase == DispatchPhase::Capture
                            && move_entity.read(cx).navigation.drag.is_some()
                        {
                            move_entity.update(cx, |this, cx| this.move_navigation(event, cx));
                            cx.stop_propagation();
                        }
                    });
                    let up_entity = entity.clone();
                    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                        if phase == DispatchPhase::Capture
                            && event.button == MouseButton::Left
                            && up_entity.read(cx).navigation.drag.is_some()
                        {
                            up_entity.update(cx, |this, cx| this.finish_navigation(cx));
                            cx.stop_propagation();
                        }
                    });
                },
            )
            .w(px(SIZE))
            .h(px(SIZE)),
        )
        .child(
            div()
                .absolute()
                .bottom_0()
                .right(px(6.))
                .flex()
                .gap(px(2.))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    ui::icon_button("navigation-home", IconName::Box, "Ізометрія (0)").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.set_view(-100., 35.);
                            this.navigation.tween = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    ui::icon_button("navigation-fit", IconName::Focus, "Вписати (Z)").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.zoom_to_fit();
                            cx.notify();
                        }),
                    ),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cube_hits_faces_edges_and_corners_in_camera_frame() {
        let mut camera = viewer::Camera::default();
        camera.set_angles(-90., 0.);
        assert!(hit(CENTER, &camera).unwrap().1.abs_diff_eq(-Vec3::Y, 0.001));
        assert!(
            hit(CENTER + Vec2::new(SCALE * 0.9, 0.), &camera)
                .unwrap()
                .1
                .abs_diff_eq(Vec3::new(1., -1., 0.).normalize(), 0.001)
        );
        assert!(
            hit(CENTER + Vec2::new(SCALE * 0.9, -SCALE * 0.9), &camera)
                .unwrap()
                .1
                .abs_diff_eq(Vec3::new(1., -1., 1.).normalize(), 0.001)
        );
        assert!(hit(Vec2::ZERO, &camera).is_none());
        for yaw in [0., 90., 180., -90.] {
            camera.set_angles(yaw, 35.);
            for face in faces(&camera) {
                assert_eq!(hit(face.center, &camera).unwrap().0, face.id);
            }
        }
    }
}
