//! Будівельні блоки в стилі Zed: доки, заголовки панелей, рядки дерева, вкладки, рядок стану.
//!
//! Розміри — як у Zed: заголовок 34 px, вкладки 32 px, рядки списків 24 px, рядок стану 28 px,
//! іконки 14 px, скруглення 4 px. Наведення підсвічує фон, вибране — `element_selected`.

use gpui::{AnyElement, ElementId, FontWeight, Hsla, SharedString, Stateful, div, prelude::*, px};
use gpui_component::{Icon, tooltip::Tooltip};
pub use gpui_kit_assets::IconName;

use crate::theme::{MONO_FONT, zed};

fn translated(value: impl Into<SharedString>) -> String {
    let value: SharedString = value.into();
    crate::i18n::tr(&value)
}

pub const TAB_HEIGHT: f32 = 32.;
pub const ROW_HEIGHT: f32 = 24.;
const INDENT: f32 = 14.;

pub fn icon(name: IconName) -> Icon {
    Icon::new(name).size(px(14.))
}

/// Кнопка в заголовку вікна: натискання не віддається заголовку. Інакше заголовок
/// на найменшому русі миші запускає перетягування вікна, композитор забирає вказівник,
/// відпускання не доходить і клік губиться (кнопка лишається «натиснутою»).
pub trait NoWindowDrag: gpui::InteractiveElement + Sized {
    fn no_window_drag(self) -> Self {
        self.on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
    }
}
impl<E: gpui::InteractiveElement> NoWindowDrag for E {}

/// Квадратна кнопка-іконка без фону, як у тулбарах Zed.
pub fn icon_button(
    id: impl Into<ElementId>,
    name: IconName,
    tooltip: impl Into<SharedString>,
) -> Stateful<gpui::Div> {
    icon_button_content(id, icon(name), tooltip)
}

fn icon_button_content(
    id: impl Into<ElementId>,
    content: impl IntoElement,
    tooltip: impl Into<SharedString>,
) -> Stateful<gpui::Div> {
    let z = zed();
    let tooltip: SharedString = translated(tooltip).into();
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(24.))
        .rounded(px(4.))
        .cursor_pointer()
        .text_color(z.text_muted)
        .hover(|s| s.bg(z.element_hover).text_color(z.text))
        .active(|s| s.bg(z.element_selected))
        .child(content)
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
}

/// Іконка-кнопка руйнівної дії (видалити): червоніє при наведенні.
pub fn danger_icon_button(
    id: impl Into<ElementId>,
    name: IconName,
    tooltip: impl Into<SharedString>,
) -> Stateful<gpui::Div> {
    let z = zed();
    let tooltip: SharedString = translated(tooltip).into();
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(24.))
        .rounded(px(4.))
        .cursor_pointer()
        .text_color(z.text_muted)
        .hover(|s| s.bg(z.error.opacity(0.12)).text_color(z.error))
        .active(|s| s.bg(z.error.opacity(0.2)))
        .child(icon(name))
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
}

/// Кнопка руйнівної дії з підписом (зупинити друк).
pub fn danger_button(
    id: impl Into<ElementId>,
    icon_name: IconName,
    label: impl Into<SharedString>,
) -> Stateful<gpui::Div> {
    let z = zed();
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .h(px(24.))
        .px(px(8.))
        .rounded(px(4.))
        .border_1()
        .border_color(z.error.opacity(0.4))
        .cursor_pointer()
        .text_sm()
        .text_color(z.error)
        .hover(|s| s.bg(z.error.opacity(0.12)))
        .active(|s| s.bg(z.error.opacity(0.2)))
        .child(icon(icon_name))
        .child(translated(label))
}

pub trait Toggle: Sized + Styled {
    /// Увімкнений перемикач: фон вибраного, як у кнопок доків Zed.
    fn toggled(self, on: bool) -> Self {
        let z = zed();
        if on {
            self.bg(z.element_selected).text_color(z.text)
        } else {
            self
        }
    }

    /// Неактивний елемент: приглушений і без наведення.
    fn inert(self, disabled: bool) -> Self {
        if disabled {
            self.text_color(zed().text_placeholder).opacity(0.5)
        } else {
            self
        }
    }
}

impl<E: Styled> Toggle for E {}

/// Кнопка з іконкою й підписом без фону (ghost у Zed).
pub fn ghost_button(
    id: impl Into<ElementId>,
    icon_name: Option<IconName>,
    label: impl Into<SharedString>,
) -> Stateful<gpui::Div> {
    let z = zed();
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .h(px(24.))
        .px(px(8.))
        .rounded(px(4.))
        .cursor_pointer()
        .text_sm()
        .text_color(z.text)
        .hover(|s| s.bg(z.element_hover))
        .active(|s| s.bg(z.element_selected))
        .children(icon_name.map(|n| icon(n).text_color(z.text_muted)))
        .child(translated(label))
}

/// Як `ghost_button`, але без підпису: вміст (наприклад назву з трикрапкою) додає викликач.
pub fn ghost_button_bare(id: impl Into<ElementId>, icon_name: IconName) -> Stateful<gpui::Div> {
    let z = zed();
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .h(px(24.))
        .px(px(8.))
        .rounded(px(4.))
        .cursor_pointer()
        .text_sm()
        .text_color(z.text)
        .hover(|s| s.bg(z.element_hover))
        .active(|s| s.bg(z.element_selected))
        .child(icon(icon_name).flex_none().text_color(z.text_muted))
}

/// Головна дія — «тонований» стиль Zed: напівпрозорий акцент, акцентний текст.
pub fn accent_button(
    id: impl Into<ElementId>,
    icon_name: IconName,
    label: impl Into<SharedString>,
    enabled: bool,
) -> Stateful<gpui::Div> {
    let z = zed();
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .h(px(24.))
        .px(px(10.))
        .rounded(px(4.))
        .border_1()
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .map(|d| {
            if enabled {
                d.cursor_pointer()
                    .bg(z.accent_bg)
                    .border_color(z.accent_border)
                    .text_color(z.accent)
                    .hover(|s| s.bg(z.accent.opacity(0.18)).border_color(z.border_focused))
                    .active(|s| s.bg(z.accent.opacity(0.26)))
            } else {
                d.border_color(z.border_variant)
                    .text_color(z.text_placeholder)
            }
        })
        .child(icon(icon_name))
        .child(translated(label))
}

/// Підказка клавіш (Ctrl+R) — моноширинна й приглушена.
pub fn kbd(keys: &'static str) -> impl IntoElement {
    let z = zed();
    div()
        .flex_none()
        .px(px(4.))
        .rounded(px(3.))
        .border_1()
        .border_color(z.border_variant)
        .font_family(MONO_FONT)
        .text_xs()
        .text_color(z.text_placeholder)
        .child(keys)
}

/// Кругла позначка стану; `live` (друк, підключення) — з ореолом.
///
/// Без пульсації: безперервна анімація в GPUI перемальовує все вікно щокадру (≈150% CPU).
pub fn status_dot(color: Hsla, live: bool) -> AnyElement {
    let dot = div().flex_none().size(px(7.)).rounded_full().bg(color);
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(13.))
        .rounded_full()
        .when(live, |d| d.bg(color.opacity(0.22)))
        .child(dot)
        .into_any_element()
}

/// Обертовий індикатор роботи. Анімація обмежена 30 кадрами/с: безперервна анімація GPUI
/// перемальовує вікно, тож без обмеження вона з’їдала б процесор. Статичний індикатор
/// виглядав як зависання (відгук з Windows), тому він мусить рухатися.
pub fn spinner(id: impl Into<ElementId>) -> AnyElement {
    use gpui::{Animation, AnimationExt as _, Transformation, percentage};
    icon(IconName::LoaderCircle)
        .text_color(zed().accent)
        .with_animation(
            id,
            Animation::new(std::time::Duration::from_millis(900))
                .repeat()
                .with_max_fps(30.),
            |icon, delta| icon.transform(Transformation::rotate(percentage(delta))),
        )
        .into_any_element()
}

/// Заголовок дока: назва зліва, дії справа, тонка межа знизу.
pub fn panel_header(title: impl Into<SharedString>, trailing: Vec<AnyElement>) -> gpui::Div {
    panel_header_text(translated(title), trailing)
}

/// Заголовок панелі з готовим текстом без перекладу (назва теки, файлу).
pub fn panel_header_text(title: impl Into<SharedString>, trailing: Vec<AnyElement>) -> gpui::Div {
    let title: SharedString = title.into();
    let z = zed();
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(2.))
        .h(px(TAB_HEIGHT))
        .pl(px(10.))
        .pr(px(4.))
        .border_b_1()
        .border_color(z.border_variant)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .child(title),
        )
        .children(trailing)
}

/// Підзаголовок групи всередині панелі (як секції налаштувань Zed).
pub fn group_title(title: impl Into<SharedString>) -> gpui::Div {
    let z = zed();
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(28.))
        .px(px(10.))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(z.text_muted)
        .child(translated(title))
}

/// Рядок дерева/списку з напрямними відступів, як у панелі проєкту Zed.
pub fn list_item(id: impl Into<ElementId>, depth: usize, selected: bool) -> Stateful<gpui::Div> {
    let z = zed();
    div()
        .id(id)
        .relative()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .h(px(ROW_HEIGHT))
        .pl(px(10. + depth as f32 * INDENT))
        .pr(px(8.))
        .text_sm()
        .cursor_pointer()
        .border_1()
        .border_color(gpui::transparent_black())
        .map(|d| {
            if selected {
                d.bg(z.element_selected).border_color(z.border_focused)
            } else {
                d
            }
        })
        .hover(|s| s.bg(z.element_hover))
        .children((0..depth).map(move |level| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(16. + level as f32 * INDENT))
                .w(px(1.))
                .bg(z.indent_guide)
        }))
}

/// Смуга вкладок Zed: вкладки зліва, порожнє місце з нижньою межею, дії справа.
pub fn tab_bar(tabs: Vec<AnyElement>, trailing: Vec<AnyElement>) -> gpui::Div {
    let z = zed();
    div()
        .flex()
        .flex_none()
        .h(px(TAB_HEIGHT))
        .bg(z.panel)
        .children(tabs)
        .child(
            div()
                .flex_1()
                .h_full()
                .border_b_1()
                .border_color(z.border_variant),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(2.))
                .px(px(4.))
                .h_full()
                .border_b_1()
                .border_l_1()
                .border_color(z.border_variant)
                .children(trailing),
        )
}

/// Вкладка: активна зливається з робочою областю (без нижньої межі), неактивні — на фоні панелі.
pub fn tab(
    id: impl Into<ElementId>,
    icon_name: IconName,
    label: impl Into<SharedString>,
    active: bool,
) -> Stateful<gpui::Div> {
    let z = zed();
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .h_full()
        .px(px(12.))
        .text_sm()
        .cursor_pointer()
        .border_r_1()
        .border_color(z.border_variant)
        .map(|d| {
            if active {
                d.bg(z.editor).text_color(z.text)
            } else {
                d.border_b_1()
                    .text_color(z.text_muted)
                    .hover(|s| s.bg(z.element_hover).text_color(z.text))
            }
        })
        .child(icon(icon_name).text_color(if active { z.text } else { z.text_muted }))
        .child(translated(label))
}

/// Тулбар під вкладками (у Zed тут хлібні крихти й дії редактора).
pub fn toolbar() -> gpui::Div {
    let z = zed();
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(2.))
        .h(px(32.))
        .px(px(8.))
        .bg(z.editor)
        .text_sm()
}

/// Хлібні крихти: «Пластина 1 › Body1».
pub fn breadcrumbs(parts: Vec<SharedString>) -> gpui::Div {
    let z = zed();
    let last = parts.len().saturating_sub(1);
    div().flex().items_center().gap(px(4.)).min_w_0().children(
        parts.into_iter().enumerate().flat_map(move |(i, p)| {
            let sep = (i > 0).then(|| {
                div()
                    .text_color(z.text_placeholder)
                    .child("›")
                    .into_any_element()
            });
            let part = div()
                .truncate()
                .text_color(if i == last { z.text } else { z.text_muted })
                .child(p)
                .into_any_element();
            sep.into_iter().chain(std::iter::once(part))
        }),
    )
}

/// Рядок «назва — значення» (властивості, статус принтера).
pub fn prop_row(label: impl Into<SharedString>, value: impl IntoElement) -> gpui::Div {
    let z = zed();
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .min_h(px(28.))
        .px(px(10.))
        .child(
            div()
                .w(px(112.))
                .flex_none()
                .truncate()
                .text_sm()
                .text_color(z.text_muted)
                .child(translated(label)),
        )
        .child(div().flex_1().min_w_0().child(value))
}

/// Порожній стан по центру: іконка, заголовок, пояснення, дії.
pub fn empty_state(
    name: IconName,
    title: impl Into<SharedString>,
    body: impl Into<SharedString>,
    actions: Vec<AnyElement>,
) -> gpui::Div {
    let z = zed();
    div()
        .flex()
        .flex_col()
        .flex_1()
        .size_full()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .child(Icon::new(name).size(px(32.)).text_color(z.text_placeholder))
        .child(
            div()
                .text_base()
                .font_weight(FontWeight::MEDIUM)
                .child(translated(title)),
        )
        .child(
            div()
                .max_w(px(420.))
                .text_sm()
                .text_color(z.text_muted)
                .text_center()
                .child(translated(body)),
        )
        .child(div().flex().gap(px(6.)).mt(px(6.)).children(actions))
}

/// Тонка горизонтальна межа.
pub fn divider() -> gpui::Div {
    div()
        .flex_none()
        .h(px(1.))
        .w_full()
        .bg(zed().border_variant)
}

/// Тло дока (ліва/права панель).
pub fn dock() -> gpui::Div {
    let z = zed();
    div()
        .flex()
        .flex_col()
        .size_full()
        .bg(z.panel)
        .text_color(z.text)
}

/// An interactive card over the 3D canvas owns its pointer gestures, including gaps.
pub fn scene_control(id: impl Into<ElementId>) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .occlude()
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(gpui::MouseButton::Right, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(gpui::MouseButton::Middle, |_, _, cx| cx.stop_propagation())
        .on_mouse_move(|_, _, cx| cx.stop_propagation())
        .on_scroll(|_, _, cx| cx.stop_propagation())
}
