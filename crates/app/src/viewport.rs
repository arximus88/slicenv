//! 3D-в’юпорт: wgpu рендерить кадр поза екраном, GPUI показує його як зображення.

use std::sync::Arc;

use gpui::{
    Bounds, Context, Corners, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, RenderImage,
    ScrollEvent, Window, canvas, div, prelude::*, px,
};

use crate::workspace::Workspace;

/// Правий клік, зсунутий менш ніж на стільки пікселів, відкриває меню, а не панорамує.
const CONTEXT_CLICK_SLOP: f32 = 4.0;

impl Workspace {
    /// Точка сцени під курсором — опора для зуму: вхід променя в рамку моделі чи траєкторії,
    /// інакше стіл.
    pub fn scene_point(&self, position: gpui::Point<Pixels>) -> Option<glam::Vec3> {
        let (origin, dir) = self.ray_at(position)?;
        let bounds = self.scene_bounds();
        if let Some((lo, hi)) = bounds
            && let Some(t) = crate::objects::ray_box(origin, dir, lo, hi)
        {
            return Some(origin + dir * t);
        }
        if dir.z.abs() > 1e-4 {
            let t = -origin.z / dir.z;
            if t > 0.0 {
                return Some(origin + dir * t);
            }
        }
        None
    }

    /// Рендерить кадр під поточний розмір в’юпорту (лише якщо щось змінилось) і малює його.
    fn paint_viewport(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _profile = settings::profile::Span::new("viewport::frame");
        let preview_scale = if self.page == crate::workspace::Page::Preview {
            self.preview.as_ref().map_or(1.0, |p| p.render_scale)
        } else {
            1.0
        };
        // Завжди повна роздільність: зменшення під час руху камери давало розмиття.
        let scale = window.scale_factor() * preview_scale;
        let width = (f32::from(bounds.size.width) * scale).round() as u32;
        let height = (f32::from(bounds.size.height) * scale).round() as u32;
        if width == 0 || height == 0 {
            return;
        }

        self.viewport_bounds = Some(bounds);
        if self.page == crate::workspace::Page::Prepare {
            let _s = settings::profile::Span::new("viewport::geometry_preview");
            self.ensure_geometry_preview(cx);
        }
        if !self.viewport_failed {
            let ready = {
                let _readback = settings::profile::Span::new("viewport::poll_readback");
                self.renderer.poll_frame()
            };
            match ready {
                // An in-flight resize belongs to its own dimensions; show it while
                // the next submission catches up, without losing newer input.
                Ok(Some(ready)) => self.install_frame(ready, window),
                Ok(None) => {}
                Err(err) => {
                    self.error = Some(format!("3D: {err:#}").into());
                    self.viewport_failed = true;
                }
            }
            if !self.viewport_failed
                && !self.renderer.has_pending_frame()
                && (self.dirty || self.frame.is_none() || self.frame_size != (width, height))
            {
                if self.page == crate::workspace::Page::Prepare {
                    let _s = settings::profile::Span::new("viewport::refresh_overlay");
                    self.refresh_overlay();
                    drop(_s);
                    let _s = settings::profile::Span::new("viewport::prepare_clip");
                    self.apply_prepare_clip(cx);
                } else if self.page == crate::workspace::Page::Preview {
                    self.apply_preview_view(cx);
                }
                {
                    let _s = settings::profile::Span::new("viewport::plate_icons");
                    self.sync_plate_icons();
                    self.sync_bed_look();
                }
                let _submit = settings::profile::Span::new("viewport::submit");
                match self.renderer.submit_frame(width, height, &self.camera) {
                    Ok(true) => {
                        self.dirty = false;
                        // Забрати щойно поданий кадр у цьому ж циклі: інакше його видно
                        // лише через кадр вікна (≤30 к/с і запізнення на рух миші).
                        // Легкий кадр (≤8 мс GPU) показуємо одразу; важкий Preview не тримає
                        // UI-потік — його забере наступний тік через request_animation_frame.
                        let _wait = settings::profile::Span::new("viewport::wait_gpu");
                        match self.renderer.wait_frame(std::time::Duration::from_millis(8)) {
                            Ok(Some(ready)) => self.install_frame(ready, window),
                            Ok(None) => {}
                            Err(err) => {
                                self.error = Some(format!("3D: {err:#}").into());
                                self.viewport_failed = true;
                            }
                        }
                    }
                    Ok(false) => {} // keep the latest request until the current readback completes
                    Err(err) => {
                        self.error = Some(format!("3D: {err:#}").into());
                        self.viewport_failed = true;
                    }
                }
            }
            if self.renderer.has_pending_frame() {
                self.await_gpu_frame(cx);
            }
        }

        self.viewport_bounds = Some(bounds);
        // Тестовий seam (`ui_state.rs`): друк лише при зміні стану; без SLICENV_UI_TRACE —
        // одне читання статичного прапорця.
        if crate::ui_state::trace_enabled() {
            self.trace_ui_state(cx);
        }
        if let Some(frame) = self.frame.clone() {
            let _paint = settings::profile::Span::new("viewport::paint_image");
            window
                .paint_image(bounds, bounds, Corners::default(), frame, 0, false)
                .ok();
        }
    }

    /// Кадр GPU ще читається: чекаємо його у фоновому потоці й будимо лише в’юпорт.
    /// Раніше тут був `request_animation_frame`, і поки readback не готовий, вікно
    /// перемальовувалось ціле на кожен тік (сотні кадрів на секунду без нових пікселів).
    fn await_gpu_frame(&mut self, cx: &mut Context<Self>) {
        if self.gpu_wait.is_some() {
            return;
        }
        let wait = self.renderer.gpu_waiter();
        self.gpu_wait = Some(cx.spawn(async move |this, cx| {
            // Тайм-аут не помилка: наступний paint перевірить і за потреби чекатиме знову.
            cx.background_executor()
                .spawn(async move { wait(std::time::Duration::from_millis(250)) })
                .await;
            this.update(cx, |this, cx| {
                this.gpu_wait = None;
                this.notify_viewport(cx);
            })
            .ok();
        }));
    }

    fn install_frame(&mut self, ready: Arc<viewer::RenderedFrame>, window: &mut Window) {
        let _profile = settings::profile::Span::new("viewport::install_frame");
        // Рендерер більше не тримає кадр — пікселі переходять у зображення без копії.
        self.renderer.release_ready();
        let (width, height) = (ready.width, ready.height);
        let pixels = Arc::try_unwrap(ready).map_or_else(|shared| shared.pixels.clone(), |f| f.pixels);
        if let Some(buffer) = image::RgbaImage::from_raw(width, height, pixels) {
            let frame = Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]));
            if let Some(old) = self.frame.replace(frame) {
                window.drop_image(old).ok();
            }
            self.frame_size = (width, height);
        }
    }

    pub fn viewport(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let this = cx.entity();
        div()
            .id("viewport")
            .flex_1()
            .relative()
            .overflow_hidden()
            // Курсор підказує, що станеться: схопити обʼєкт, тягнути його чи обертати камеру.
            .map(|d| {
                if self.moving.is_some() && self.drag_moved {
                    d.cursor_grabbing()
                } else if self.plate_hover.is_some() {
                    d.cursor_pointer()
                } else if self.hovered.is_some() || self.hover_on_selected {
                    d.cursor_grab()
                } else {
                    d
                }
            })
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                let left = !*hovered && this.plate_hover.take().is_some();
                if left {
                    this.dirty = true;
                }
                if (!*hovered && this.update_hover(None)) || left {
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus, cx);
                    this.drag_from = Some(e.position);
                    this.drag_moved = false;
                    if this.page == crate::workspace::Page::Prepare {
                        this.update_tool_hover(e.position);
                        if this.press_plate_button(e.position) {
                            this.drag_from = None;
                        } else if this.measure_click(e.position, cx) {
                            this.drag_from = None;
                        } else if this.tool == Some(crate::tools::Tool::LayOnFace)
                            && this.lay_hovered_face(window, cx)
                        {
                            this.drag_from = None;
                        } else if this.begin_gizmo(e.position) {
                            this.moving = None;
                        } else {
                            this.begin_pointer(e.position, e.modifiers, window, cx);
                        }
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, e: &gpui::MouseUpEvent, window, cx| {
                    if this.release_plate_button(e.position, window, cx) {
                        return;
                    }
                    let clicked = this.drag_from.is_some() && !this.drag_moved;
                    let was_moving = this.moving.is_some()
                        || this.hit_test(this.drag_from.unwrap_or_default()).is_some();
                    let was_gizmo = this.end_gizmo(window, cx);
                    this.drag_from = None;
                    this.end_drag(window, cx);
                    this.drag_moved = false;
                    this.dirty = true;
                    cx.notify();
                    // Клік у порожнє місце знімає вибір.
                    if clicked
                        && !was_moving
                        && !was_gizmo
                        && this.page == crate::workspace::Page::Prepare
                    {
                        this.select_instance(None, window, cx);
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    // Mouse-up outside also arrives for clicks in settings. Only finish
                    // a gesture that actually started in this viewport.
                    if this.drag_from.is_none()
                        && this.moving.is_none()
                        && this.gizmo_drag.is_none()
                    {
                        return;
                    }
                    this.drag_from = None;
                    this.end_gizmo(window, cx);
                    this.end_drag(window, cx);
                    this.drag_moved = false;
                    this.dirty = true;
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, e: &MouseDownEvent, _, _| {
                    this.panning = Some(e.position);
                    this.context_press = Some((e.position, false));
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, e: &MouseDownEvent, _, _| this.panning = Some(e.position)),
            )
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(|this, e: &gpui::MouseUpEvent, window, cx| {
                    this.panning = None;
                    this.dirty = true;
                    // Натискання й відпускання без руху — не панорама, а контекстне меню.
                    if let Some((_, false)) = this.context_press.take() {
                        this.open_scene_menu(e.position, window, cx);
                    }
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Right,
                cx.listener(|this, _, _, _| this.context_press = None),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, _, _, cx| {
                    this.panning = None;
                    this.dirty = true;
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, window, cx| {
                if this.navigation_dragging() {
                    return;
                }
                if e.pressed_button.is_none() {
                    let over_selected = this.page == crate::workspace::Page::Prepare
                        && this.selected.is_some()
                        && this.hit_test(e.position) == this.selected;
                    let changed = this.update_plate_hover(e.position)
                        | this.update_hover(Some(e.position))
                        | this.update_tool_hover(e.position)
                        | (over_selected != this.hover_on_selected);
                    this.hover_on_selected = over_selected;
                    if changed {
                        cx.notify();
                    }
                }
                if let Some((press, moved)) = &mut this.context_press
                    && !*moved
                {
                    let delta = e.position - *press;
                    *moved = f32::from(delta.x).hypot(f32::from(delta.y)) >= CONTEXT_CLICK_SLOP;
                }
                if let Some(from) = this.panning {
                    if !matches!(
                        e.pressed_button,
                        Some(MouseButton::Right | MouseButton::Middle)
                    ) {
                        this.panning = None;
                        return;
                    }
                    let delta = e.position - from;
                    this.camera.pan(f32::from(delta.x), f32::from(delta.y));
                    this.panning = Some(e.position);
                    this.dirty = true;
                    // Лише камера: панелі доків лишаються з кешу кадру.
                    this.notify_viewport(cx);
                    return;
                }
                let Some(from) = this.drag_from else { return };
                if e.pressed_button != Some(MouseButton::Left) {
                    this.end_gizmo(window, cx);
                    this.end_drag(window, cx);
                    this.drag_from = None;
                    this.drag_moved = false;
                    this.dirty = true;
                    cx.notify();
                    return;
                }
                let delta = e.position - from;
                if !this.drag_moved && f32::from(delta.x).abs() + f32::from(delta.y).abs() < 3.0 {
                    return;
                }
                this.drag_moved = true;
                let camera_only = if this.gizmo_drag.is_some() {
                    this.drag_gizmo(e.position, e.modifiers.shift);
                    this.refresh_tool_panel(window, cx);
                    false
                } else if this.moving.is_some() {
                    this.drag_selected(e.position);
                    this.refresh_tool_panel(window, cx);
                    false
                } else {
                    this.camera.orbit(f32::from(delta.x), f32::from(delta.y));
                    true
                };
                this.drag_from = Some(e.position);
                this.dirty = true;
                if camera_only {
                    this.notify_viewport(cx);
                } else {
                    cx.notify();
                }
            }))
            .on_scroll(cx.listener(|this, e: &ScrollEvent, _, cx| {
                let delta = e.delta.pixel_delta(px(20.));
                let amount = f32::from(delta.y)
                    * if this.preferences.reverse_zoom {
                        -1.
                    } else {
                        1.
                    };
                if !this.preferences.zoom_to_cursor {
                    this.camera.zoom(amount);
                } else {
                    match this.scene_point(e.position) {
                        Some(point) => this.camera.zoom_toward(point, amount),
                        None => this.camera.zoom(amount),
                    }
                }
                this.dirty = true;
                this.notify_viewport(cx);
            }))
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, cx| {
                        this.update(cx, |this, cx| this.paint_viewport(bounds, window, cx));
                    },
                )
                .size_full(),
            )
    }
}
