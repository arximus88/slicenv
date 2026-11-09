//! Власні контроли в стилі Zed замість shadcn-подібних з gpui-component:
//! випадний список, чекбокс, перемикач, поле вводу, прогрес, модалка й тости.
//!
//! Від gpui-component лишається тільки рушій редагування тексту (`InputState` без оформлення).

use std::{cell::Cell, rc::Rc, time::Duration};

use gpui::{
    AnyElement, App, Bounds, ClickEvent, Context, ElementId, Entity, EventEmitter, FocusHandle,
    Focusable as _, FontWeight, KeyDownEvent, MouseButton, Pixels, ScrollHandle, SharedString,
    Stateful, Subscription, Window, anchored, canvas, deferred, div, point, prelude::*, px,
};
use gpui_component::{
    Icon, Sizable as _,
    input::{Input, InputEvent, InputState},
};

use crate::{
    theme::zed,
    ui::{self, IconName},
};

// ---- випадний список ----

pub enum DropdownEvent {
    Confirm(SharedString),
}

/// Випадний список як у Zed: кнопка з назвою й стрілками, меню-поповер з необовʼязковим пошуком,
/// клавіатура (↑ ↓ Enter Esc).
pub struct Dropdown {
    items: Vec<SharedString>,
    icons: Vec<Option<SharedString>>,
    selected: Option<usize>,
    open: bool,
    /// Підсвічений рядок серед відфільтрованих.
    highlighted: usize,
    disabled: bool,
    menu_max_h: Pixels,
    focus: FocusHandle,
    query: Option<Entity<InputState>>,
    query_source: Option<&'static str>,
    query_placeholder: String,
    scroll: ScrollHandle,
    trigger_bounds: Rc<Cell<Bounds<Pixels>>>,
    _subscription: Option<Subscription>,
}

impl EventEmitter<DropdownEvent> for Dropdown {}

impl Dropdown {
    pub fn new(items: Vec<SharedString>, selected: Option<usize>, cx: &mut Context<Self>) -> Self {
        Self {
            items,
            icons: Vec::new(),
            selected,
            open: false,
            highlighted: 0,
            disabled: false,
            menu_max_h: px(360.),
            focus: cx.focus_handle(),
            query: None,
            query_source: None,
            query_placeholder: String::new(),
            scroll: ScrollHandle::new(),
            trigger_bounds: Rc::default(),
            _subscription: None,
        }
    }

    pub fn with_icons(mut self, icons: Vec<Option<SharedString>>) -> Self {
        self.icons = icons;
        self
    }

    /// Поле пошуку зверху меню — для довгих списків (філаменти, процеси).
    pub fn searchable(
        mut self,
        placeholder: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder(crate::i18n::tr(placeholder)));
        self._subscription = Some(cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, _, cx| match event {
                InputEvent::Change => {
                    this.highlighted = 0;
                    this.scroll.scroll_to_item(0);
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.confirm_highlighted(cx),
                _ => {}
            },
        ));
        self.query = Some(input);
        self.query_source = Some(placeholder);
        self.query_placeholder = crate::i18n::tr(placeholder);
        self
    }

    pub fn set_items(&mut self, items: Vec<SharedString>, cx: &mut Context<Self>) {
        self.items = items;
        self.selected = self.selected.filter(|i| *i < self.items.len());
        cx.notify();
    }

    pub fn set_selected_index(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        self.selected = index.filter(|i| *i < self.items.len());
        cx.notify();
    }

    pub fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    pub fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        if self.disabled != disabled {
            self.disabled = disabled;
            self.open &= !disabled;
            cx.notify();
        }
    }

    /// Індекси пунктів, що підходять під пошук.
    fn visible(&self, cx: &App) -> Vec<usize> {
        let query = self
            .query
            .as_ref()
            .map(|q| q.read(cx).value().trim().to_lowercase())
            .unwrap_or_default();
        (0..self.items.len())
            .filter(|&i| {
                query.is_empty()
                    || crate::i18n::tr(&self.items[i])
                        .to_lowercase()
                        .contains(&query)
            })
            .collect()
    }

    fn set_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled || self.open == open {
            return;
        }
        self.open = open;
        if open {
            if let Some(query) = &self.query {
                query.update(cx, |q, cx| q.set_value("", window, cx));
                let handle = query.focus_handle(cx);
                window.focus(&handle, cx);
            } else {
                window.focus(&self.focus, cx);
            }
            self.highlighted = self.selected.unwrap_or(0);
            self.scroll.scroll_to_item(self.highlighted);
        } else {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    fn confirm(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(value) = self.items.get(index).cloned() else {
            return;
        };
        self.selected = Some(index);
        self.open = false;
        cx.emit(DropdownEvent::Confirm(value));
        cx.notify();
    }

    fn confirm_highlighted(&mut self, cx: &mut Context<Self>) {
        if let Some(&index) = self.visible(cx).get(self.highlighted) {
            self.confirm(index, cx);
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.visible(cx).len();
        match event.keystroke.key.as_str() {
            "down" if self.open && count > 0 => {
                self.highlighted = (self.highlighted + 1).min(count - 1)
            }
            "up" if self.open => self.highlighted = self.highlighted.saturating_sub(1),
            "enter" if self.open => self.confirm_highlighted(cx),
            "enter" | "space" | "down" if !self.open => self.set_open(true, window, cx),
            "escape" if self.open => self.set_open(false, window, cx),
            _ => return,
        }
        self.scroll.scroll_to_item(self.highlighted);
        cx.stop_propagation();
        cx.notify();
    }
}

impl Render for Dropdown {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let (Some(query), Some(source)) = (&self.query, self.query_source) {
            let text = crate::i18n::tr(source);
            if text != self.query_placeholder {
                query.update(cx, |input, cx| {
                    input.set_placeholder(text.clone(), window, cx)
                });
                self.query_placeholder = text;
            }
        }
        let z = zed();
        let label = self
            .selected
            .and_then(|i| self.items.get(i))
            .cloned()
            .unwrap_or_else(|| "—".into());
        let bounds = self.trigger_bounds.clone();
        let disabled = self.disabled;

        let trigger = div()
            .id("dropdown-trigger")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .relative()
            .flex()
            .items_center()
            .gap(px(6.))
            .h(px(24.))
            .w_full()
            .min_w_0()
            .pl(px(8.))
            .pr(px(4.))
            .rounded(px(4.))
            .border_1()
            .text_sm()
            .map(|d| {
                if disabled {
                    d.border_color(z.border_variant)
                        .text_color(z.text_placeholder)
                } else {
                    d.cursor_pointer()
                        .bg(z.element)
                        .border_color(if self.open {
                            z.border_focused
                        } else {
                            z.border_variant
                        })
                        .hover(|s| s.bg(z.element_hover))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            let open = !this.open;
                            this.set_open(open, window, cx)
                        }))
                }
            })
            .children(
                self.selected
                    .and_then(|i| self.icons.get(i))
                    .and_then(|p| p.as_ref())
                    .map(|path| {
                        gpui::svg()
                            .path(path.clone())
                            .size(px(16.))
                            .text_color(z.text_muted)
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(crate::i18n::tr(&label)),
            )
            .child(ui::icon(IconName::ChevronsUpDown).text_color(z.text_placeholder))
            .child(
                canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {})
                    .absolute()
                    .inset_0(),
            );

        let menu = self.open.then(|| {
            let anchor = self.trigger_bounds.get();
            let width = anchor.size.width.max(px(220.));
            let visible = self.visible(cx);
            let rows = visible.iter().enumerate().map(|(row, &index)| {
                let selected = self.selected == Some(index);
                let highlighted = row == self.highlighted;
                div()
                    .id(("dropdown-item", index))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.))
                    .h(px(24.))
                    .px(px(8.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .text_sm()
                    .children(self.icons.get(index).and_then(|p| p.as_ref()).map(|path| {
                        gpui::svg()
                            .path(path.clone())
                            .size(px(16.))
                            .text_color(z.text_muted)
                    }))
                    .when(highlighted, |d| d.bg(z.element_hover))
                    .hover(|s| s.bg(z.element_hover))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(crate::i18n::tr(&self.items[index])),
                    )
                    .when(selected, |d| {
                        d.child(ui::icon(IconName::Check).text_color(z.accent))
                    })
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.highlighted != row {
                            this.highlighted = row;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.confirm(index, cx);
                        window.focus(&this.focus, cx);
                    }))
            });
            deferred(
                anchored()
                    .position(point(anchor.origin.x, anchor.bottom() + px(4.)))
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        div()
                            .id("dropdown-menu")
                            .occlude()
                            .w(width)
                            .flex()
                            .flex_col()
                            .rounded(px(6.))
                            .bg(z.panel)
                            .border_1()
                            .border_color(z.border)
                            .shadow_lg()
                            .text_color(z.text)
                            .font_family(crate::theme::UI_FONT)
                            .capture_key_down(cx.listener(Self::on_key))
                            .on_mouse_down_out(cx.listener(
                                move |this, e: &gpui::MouseDownEvent, window, cx| {
                                    // Клік по самій кнопці перемикає меню сам — тут не закриваємо.
                                    if !this.trigger_bounds.get().contains(&e.position) {
                                        this.set_open(false, window, cx);
                                    }
                                },
                            ))
                            .children(self.query.as_ref().map(|q| {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .h(px(30.))
                                    .px(px(8.))
                                    .border_b_1()
                                    .border_color(z.border_variant)
                                    .child(
                                        ui::icon(IconName::Search).text_color(z.text_placeholder),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .child(Input::new(q).appearance(false).small()),
                                    )
                            }))
                            .child(
                                div()
                                    .id("dropdown-rows")
                                    .flex()
                                    .flex_col()
                                    .p(px(4.))
                                    .max_h(self.menu_max_h)
                                    .overflow_y_scroll()
                                    .track_scroll(&self.scroll)
                                    .children(rows)
                                    .when(visible.is_empty(), |d| {
                                        d.child(
                                            div()
                                                .px(px(8.))
                                                .py(px(4.))
                                                .text_sm()
                                                .text_color(z.text_muted)
                                                .child(crate::i18n::tr("Нічого не знайдено")),
                                        )
                                    }),
                            ),
                    ),
            )
            .with_priority(3)
        });

        div().w_full().min_w_0().child(trigger).children(menu)
    }
}

// ---- повзунок ----

/// Користувач змінив значення (нові — через [`Slider::values`]).
pub enum SliderEvent {
    Change,
}

/// Повзунок у стилі Zed: тонка доріжка, акцентна заповнена частина, квадратні повзунці.
/// Подвійний (діапазон шарів) або одинарний (рухи в шарі); горизонтальний чи вертикальний
/// (тоді більші значення — вгорі). Тягнути можна й за межами доріжки.
pub struct Slider {
    max: usize,
    lo: usize,
    hi: usize,
    range: bool,
    vertical: bool,
    /// Який повзунець тягнемо: false — нижній, true — верхній.
    dragging: Option<bool>,
    bounds: Rc<Cell<Bounds<Pixels>>>,
}

impl EventEmitter<SliderEvent> for Slider {}

impl Slider {
    pub fn range(max: usize, vertical: bool) -> Self {
        Self {
            max,
            lo: 0,
            hi: max,
            range: true,
            vertical,
            dragging: None,
            bounds: Rc::default(),
        }
    }

    pub fn single(max: usize, vertical: bool) -> Self {
        Self {
            range: false,
            ..Self::range(max, vertical)
        }
    }

    pub fn values(&self) -> (usize, usize) {
        (self.lo, self.hi)
    }

    pub fn max(&self) -> usize {
        self.max
    }

    /// Новий діапазон значень; повзунці стають на краї.
    pub fn reset(&mut self, max: usize, cx: &mut Context<Self>) {
        self.max = max;
        self.lo = 0;
        self.hi = max;
        cx.notify();
    }

    /// Встановлює значення без події (керування з клавіатури чи відтворення).
    pub fn set(&mut self, lo: usize, hi: usize, cx: &mut Context<Self>) {
        self.hi = hi.min(self.max);
        self.lo = if self.range { lo.min(self.hi) } else { 0 };
        cx.notify();
    }

    fn value_at(&self, pos: gpui::Point<Pixels>) -> usize {
        let b = self.bounds.get();
        let t = if self.vertical {
            1.0 - f32::from(pos.y - b.origin.y) / f32::from(b.size.height).max(1.0)
        } else {
            f32::from(pos.x - b.origin.x) / f32::from(b.size.width).max(1.0)
        };
        (t.clamp(0.0, 1.0) * self.max as f32).round() as usize
    }

    fn drag_to(&mut self, pos: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let Some(upper) = self.dragging else { return };
        let v = self.value_at(pos);
        let (lo, hi) = if upper {
            (self.lo, v.max(self.lo))
        } else {
            (v.min(self.hi), self.hi)
        };
        if (lo, hi) != (self.lo, self.hi) {
            self.lo = lo;
            self.hi = hi;
            cx.emit(SliderEvent::Change);
            cx.notify();
        }
    }
}

impl Render for Slider {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let z = zed();
        let frac = |v: usize| {
            if self.max == 0 {
                0.0
            } else {
                v as f32 / self.max as f32
            }
        };
        let (lo, hi) = (frac(self.lo), frac(self.hi));
        let vertical = self.vertical;
        let thumb = |t: f32, active: bool| {
            let d = div()
                .absolute()
                .size(px(12.))
                .rounded(px(3.))
                .bg(if active { z.accent } else { z.text })
                .border_2()
                .border_color(z.accent);
            if vertical {
                d.left(px(-5.)).bottom(gpui::relative(t)).mb(px(-6.))
            } else {
                d.top(px(-5.)).left(gpui::relative(t)).ml(px(-6.))
            }
        };
        let fill_from = if self.range { lo } else { 0.0 };
        let fill = div().absolute().bg(z.accent).rounded_full().map(|d| {
            if vertical {
                d.left_0()
                    .w_full()
                    .bottom(gpui::relative(fill_from))
                    .h(gpui::relative(hi - fill_from))
            } else {
                d.top_0()
                    .h_full()
                    .left(gpui::relative(fill_from))
                    .w(gpui::relative(hi - fill_from))
            }
        });
        let track = div()
            .relative()
            .rounded_full()
            .bg(z.border)
            .map(|d| {
                if vertical {
                    d.w(px(2.)).h_full()
                } else {
                    d.h(px(2.)).w_full()
                }
            })
            .child(fill)
            .when(self.range, |d| {
                d.child(thumb(lo, self.dragging == Some(false)))
            })
            .child(thumb(hi, self.dragging == Some(true)));

        let bounds = self.bounds.clone();
        let entity = cx.entity();
        div()
            .id("slider")
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .map(|d| {
                if vertical {
                    d.w(px(20.)).h_full().py(px(6.))
                } else {
                    d.h(px(20.)).w_full().px(px(6.))
                }
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &gpui::MouseDownEvent, _, cx| {
                    let v = this.value_at(e.position);
                    // Найближчий повзунець; при рівності — той, що в бік кліку.
                    let upper = !this.range
                        || v > this.hi
                        || (v >= this.lo && v.abs_diff(this.hi) <= v.abs_diff(this.lo));
                    this.dragging = Some(upper);
                    this.drag_to(e.position, cx);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(
                div()
                    .relative()
                    .map(|d| {
                        if vertical {
                            d.h_full().w(px(2.))
                        } else {
                            d.w_full().h(px(2.))
                        }
                    })
                    .child(track)
                    .child(
                        canvas(
                            move |b, _, _| bounds.set(b),
                            move |_, _, window, _| {
                                let e1 = entity.clone();
                                window.on_mouse_event(
                                    move |e: &gpui::MouseMoveEvent, phase, _, cx| {
                                        if phase == gpui::DispatchPhase::Bubble
                                            && e.pressed_button == Some(MouseButton::Left)
                                        {
                                            if e1.read(cx).dragging.is_some() {
                                                e1.update(cx, |this, cx| {
                                                    this.drag_to(e.position, cx)
                                                });
                                                cx.stop_propagation();
                                            }
                                        }
                                    },
                                );
                                let e2 = entity.clone();
                                window.on_mouse_event(
                                    move |_: &gpui::MouseUpEvent, phase, _, cx| {
                                        if phase == gpui::DispatchPhase::Bubble {
                                            e2.update(cx, |this, cx| {
                                                if this.dragging.take().is_some() {
                                                    cx.stop_propagation();
                                                    cx.notify();
                                                }
                                            });
                                        }
                                    },
                                );
                            },
                        )
                        .absolute()
                        .inset_0(),
                    ),
            )
    }
}

// ---- дрібні контроли ----

/// Чекбокс Zed: квадрат 14 px, акцентна галочка. Клік обробляє викликач.
pub fn checkbox(id: impl Into<ElementId>, checked: bool, disabled: bool) -> Stateful<gpui::Div> {
    let z = zed();
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .text_sm()
        .when(!disabled, |d| d.cursor_pointer())
        .when(disabled, |d| d.opacity(0.5))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(14.))
                .rounded(px(3.))
                .border_1()
                .map(|d| {
                    if checked {
                        d.bg(z.accent_bg).border_color(z.accent)
                    } else {
                        d.bg(z.element).border_color(z.border)
                    }
                })
                .when(checked, |d| {
                    d.child(
                        Icon::new(IconName::Check)
                            .size(px(10.))
                            .text_color(z.accent),
                    )
                }),
        )
}

/// Радіокнопка Zed: коло 16 px, у вибраної — акцентна крапка 8 px. Парні розміри
/// лишаються цілими пікселями на масштабах 125 % і 150 % — інакше крапка з’їжджає. Клік обробляє викликач.
pub fn radio(id: impl Into<ElementId>, selected: bool, disabled: bool) -> Stateful<gpui::Div> {
    let z = zed();
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .text_sm()
        .when(!disabled, |d| d.cursor_pointer())
        .when(disabled, |d| d.opacity(0.5))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(16.))
                .rounded_full()
                .border_1()
                .map(|d| {
                    if selected {
                        d.bg(z.accent_bg).border_color(z.accent)
                    } else {
                        d.bg(z.element).border_color(z.border)
                    }
                })
                .when(selected, |d| {
                    d.child(div().size(px(8.)).rounded_full().bg(z.accent))
                }),
        )
}

/// Перемикач Zed: невелика «пігулка», увімкнений — акцентний.
pub fn switch(id: impl Into<ElementId>, on: bool) -> Stateful<gpui::Div> {
    let z = zed();
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .text_sm()
        .cursor_pointer()
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .w(px(28.))
                .h(px(16.))
                .p(px(2.))
                .rounded_full()
                .border_1()
                .map(|d| {
                    if on {
                        d.justify_end().bg(z.accent).border_color(z.accent)
                    } else {
                        d.bg(z.element_selected).border_color(z.border)
                    }
                })
                .child(div().size(px(10.)).rounded_full().bg(if on {
                    z.editor
                } else {
                    z.text_muted
                })),
        )
}

/// Тонка смуга прогресу (0–100).
pub fn progress(value: f32) -> gpui::Div {
    let z = zed();
    div()
        .h(px(4.))
        .w_full()
        .rounded_full()
        .bg(z.element_hover)
        .child(
            div()
                .h_full()
                .w(gpui::relative((value / 100.0).clamp(0.0, 1.0)))
                .rounded_full()
                .bg(z.accent),
        )
}

/// Смуга прогресу без відомого відсотка: відрізок біжить зліва направо. Для нарізки без
/// `--pipe`, підготовки профілів тощо — щоб було видно, що робота йде.
pub fn progress_indeterminate(id: impl Into<gpui::ElementId>) -> gpui::AnyElement {
    use gpui::{Animation, AnimationExt as _};
    let z = zed();
    div()
        .relative()
        .h(px(4.))
        .w_full()
        .rounded_full()
        .overflow_hidden()
        .bg(z.element_hover)
        .child(
            div()
                .absolute()
                .top_0()
                .h_full()
                .w(gpui::relative(0.3))
                .rounded_full()
                .bg(z.accent)
                .with_animation(
                    id,
                    Animation::new(Duration::from_millis(1400))
                        .repeat()
                        .with_max_fps(30.),
                    |bar, delta| bar.left(gpui::relative(delta * 1.3 - 0.3)),
                ),
        )
        .into_any_element()
}

/// Поле вводу в рамці Zed: без тіні й кілець, при фокусі межа синіє.
pub fn field(state: &Entity<InputState>, input: Input, window: &Window, cx: &App) -> gpui::Div {
    let z = zed();
    let focused = state.focus_handle(cx).is_focused(window);
    div()
        .flex()
        .items_center()
        .h(px(24.))
        .w_full()
        .min_w_0()
        .px(px(2.))
        .rounded(px(4.))
        .bg(z.element)
        .border_1()
        .border_color(if focused {
            z.border_focused
        } else {
            z.border_variant
        })
        .child(input.appearance(false).xsmall())
}

// ---- модалка ----

pub type ModalBuilder = Rc<
    dyn Fn(
        &mut crate::workspace::Workspace,
        &mut Window,
        &mut Context<crate::workspace::Workspace>,
    ) -> AnyElement,
>;

/// Каркас модалки Zed: заголовок, тіло, дії; без затемнення, зверху по центру.
pub fn modal(
    title: impl Into<SharedString>,
    width: f32,
    body: impl IntoElement,
    footer: Option<AnyElement>,
) -> AnyElement {
    let z = zed();
    div()
        .w(px(width))
        .max_h(px(720.))
        .flex()
        .flex_col()
        .rounded(px(6.))
        .bg(z.panel)
        .border_1()
        .border_color(z.border)
        .shadow_lg()
        .overflow_hidden()
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .h(px(36.))
                .px(px(12.))
                .border_b_1()
                .border_color(z.border_variant)
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child({
                            let title: SharedString = title.into();
                            crate::i18n::tr(&title)
                        }),
                )
                .child(ui::kbd("Esc")),
        )
        .child(div().flex().flex_col().flex_1().min_h_0().child(body))
        .children(footer.map(|f| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_end()
                .gap(px(6.))
                .px(px(12.))
                .py(px(8.))
                .border_t_1()
                .border_color(z.border_variant)
                .child(f)
        }))
        .into_any_element()
}

// ---- тости ----

#[derive(Clone, Copy, PartialEq)]
pub enum ToastKind {
    Info,
    Success,
    Warning,
    Error,
}

pub struct Toast {
    pub id: usize,
    pub kind: ToastKind,
    pub text: SharedString,
}

impl crate::workspace::Workspace {
    /// Повідомлення в правому нижньому куті над рядком стану; зникає саме.
    pub fn toast(
        &mut self,
        kind: ToastKind,
        text: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.next_toast += 1;
        let id = self.next_toast;
        self.toasts.push(Toast {
            id,
            kind,
            text: text.into(),
        });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
        let ttl = if kind == ToastKind::Error { 9 } else { 5 };
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_secs(ttl))
                .await;
            this.update(cx, |this, cx| {
                this.toasts.retain(|t| t.id != id);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn open_modal(
        &mut self,
        builder: ModalBuilder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modal = Some(builder);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Підтвердження руйнівної дії: «Скасувати» і червона кнопка `ok_label`.
    pub fn confirm(
        &mut self,
        title: &'static str,
        body: &'static str,
        ok_label: &'static str,
        on_ok: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let on_ok = Rc::new(on_ok);
        self.open_modal(
            Rc::new(move |_, _, cx| {
                let on_ok = on_ok.clone();
                let ok = ui::danger_button("confirm-ok", IconName::Trash, ok_label);
                modal(
                    title,
                    440.,
                    div()
                        .p(px(12.))
                        .text_sm()
                        .text_color(zed().text_muted)
                        .child(crate::i18n::tr(body)),
                    Some(
                        div()
                            .flex()
                            .gap(px(6.))
                            .child(
                                ui::ghost_button("confirm-cancel", None, "Скасувати")
                                    .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))),
                            )
                            .child(ok.on_click(cx.listener(move |this, _, window, cx| {
                                this.close_modal(cx);
                                on_ok(this, window, cx);
                            })))
                            .into_any_element(),
                    ),
                )
            }),
            window,
            cx,
        );
    }

    pub fn close_modal(&mut self, cx: &mut Context<Self>) {
        if self.modal.take().is_some() {
            cx.notify();
        }
    }

    /// Шар модалки й тостів поверх усього вікна.
    pub fn overlays(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let z = zed();
        let mut out = Vec::new();
        if self.wizard.is_some() {
            out.push(
                deferred(crate::wizard::render(self, window, cx))
                    .with_priority(1)
                    .into_any_element(),
            );
        }
        if let Some(builder) = self.modal.clone() {
            let content = builder(self, window, cx);
            out.push(
                deferred(
                    div()
                        .id("modal-layer")
                        .absolute()
                        .inset_0()
                        .occlude()
                        .flex()
                        .justify_center()
                        .items_start()
                        .pt(px(72.))
                        // The dismiss target is a sibling behind the surface,
                        // not an ancestor of inputs whose mouse events bubble.
                        .child(
                            div()
                                .absolute()
                                .inset_0()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.close_modal(cx)),
                                ),
                        )
                        .child(
                            div()
                                .id("modal")
                                .relative()
                                .occlude()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .child(content),
                        ),
                )
                .with_priority(2)
                .into_any_element(),
            );
        }
        out.extend(crate::file_menu::render(self, cx));
        out.extend(crate::home::library_menu(self, window, cx));
        out.extend(self.scene_menu_overlay(window, cx));
        if !self.toasts.is_empty() {
            let toasts = self.toasts.iter().map(|t| {
                let (icon, color) = match t.kind {
                    ToastKind::Info => (IconName::Info, z.accent),
                    ToastKind::Success => (IconName::CircleCheck, z.success),
                    ToastKind::Warning => (IconName::TriangleAlert, z.warning),
                    ToastKind::Error => (IconName::CircleX, z.error),
                };
                let id = t.id;
                div()
                    .id(("toast", id))
                    .flex()
                    .items_start()
                    .gap(px(8.))
                    .w(px(380.))
                    .pl(px(10.))
                    .pr(px(4.))
                    .py(px(6.))
                    .rounded(px(6.))
                    .bg(z.panel)
                    .border_1()
                    .border_color(z.border)
                    .shadow_lg()
                    .text_sm()
                    .child(div().pt(px(3.)).child(ui::icon(icon).text_color(color)))
                    .child(div().flex_1().min_w_0().pt(px(1.)).child(t.text.clone()))
                    .child(
                        ui::icon_button(("toast-close", id), IconName::X, "Закрити").on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.toasts.retain(|t| t.id != id);
                                cx.notify();
                            }),
                        ),
                    )
            });
            out.push(
                deferred(
                    div()
                        .absolute()
                        .right(px(12.))
                        .bottom(px(40.))
                        .flex()
                        .flex_col()
                        .items_end()
                        .gap(px(6.))
                        .children(toasts),
                )
                .with_priority(4)
                .into_any_element(),
            );
        }
        out
    }
}
