//! Вспливне меню в стилі Zed: пункти з іконкою (або галочкою), підписом і підказкою клавіш, роздільники,
//! вимкнені пункти (з підказкою «чому») і один рівень підменю, що відкривається збоку
//! (ліворуч, якщо праворуч не вистачає місця). Меню лише малює; що саме відбувається при
//! кліку, вирішують замикання пунктів, а стан (позиція, відкрите підменю) тримає власник.

use std::rc::Rc;

use gpui::{
    AnyElement, Bounds, Context, MouseDownEvent, Pixels, Point, SharedString, Window, anchored,
    deferred, div, point, prelude::*, px, size,
};
use gpui_component::tooltip::Tooltip;

use crate::{
    theme::zed,
    ui::{self, IconName},
    workspace::Workspace,
};

pub type Action = Rc<dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)>;

const ROW: f32 = 24.;
const SEPARATOR: f32 = 9.;
const PAD: f32 = 4.;
const ROOT_WIDTH: f32 = 248.;
const SUB_WIDTH: f32 = 216.;
const MARGIN: f32 = 4.;

#[derive(Clone)]
pub enum Entry {
    Item(Item),
    Separator,
}

#[derive(Clone)]
pub struct Item {
    pub icon: Option<IconName>,
    pub label: SharedString,
    pub shortcut: Option<&'static str>,
    /// `Some` — пункт-перемикач: на місці іконки галочка (або порожньо).
    pub checked: Option<bool>,
    pub enabled: bool,
    /// Підказка до вимкненого пункту («скоро», «немає відомого файлу»…).
    pub hint: Option<SharedString>,
    pub action: Option<Action>,
    pub submenu: Vec<Entry>,
}

impl Item {
    /// `label` — ключ українською; перекладається за мовою інтерфейсу.
    pub fn new(label: &str) -> Self {
        Self::raw(crate::i18n::tr(label))
    }

    /// Підпис без перекладу (назви пластин, об’єктів із файлу).
    pub fn raw(label: impl Into<SharedString>) -> Self {
        Self {
            icon: None,
            label: label.into(),
            shortcut: None,
            checked: None,
            enabled: true,
            hint: None,
            action: None,
            submenu: Vec::new(),
        }
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn shortcut(mut self, keys: &'static str) -> Self {
        self.shortcut = Some(keys);
        self
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    pub fn on(
        mut self,
        action: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
    ) -> Self {
        self.action = Some(Rc::new(action));
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Вимикає пункт; `hint` (ключ українською) з’являється підказкою при наведенні.
    pub fn disabled(mut self, hint: &str) -> Self {
        self.enabled = false;
        self.hint = Some(crate::i18n::tr(hint).into());
        self
    }

    pub fn submenu(mut self, entries: Vec<Entry>) -> Self {
        self.submenu = entries;
        self
    }

    pub fn entry(self) -> Entry {
        Entry::Item(self)
    }
}

fn height(entries: &[Entry]) -> f32 {
    PAD * 2.
        + entries
            .iter()
            .map(|e| match e {
                Entry::Item(_) => ROW,
                Entry::Separator => SEPARATOR,
            })
            .sum::<f32>()
}

/// Верх рядка `index` відносно верху меню.
fn row_top(entries: &[Entry], index: usize) -> f32 {
    PAD + entries[..index]
        .iter()
        .map(|e| match e {
            Entry::Item(_) => ROW,
            Entry::Separator => SEPARATOR,
        })
        .sum::<f32>()
}

/// Розкладка меню у вікні: рамка кореня й підменю. Усе в пікселях вікна.
struct Layout {
    root: Bounds<Pixels>,
    sub: Option<Bounds<Pixels>>,
}

fn layout(
    position: Point<Pixels>,
    entries: &[Entry],
    open_sub: Option<usize>,
    window: &Window,
) -> Layout {
    let win = window.viewport_size();
    let (win_w, win_h) = (f32::from(win.width), f32::from(win.height));
    let h = height(entries);
    let left = f32::from(position.x).min(win_w - ROOT_WIDTH - MARGIN).max(MARGIN);
    let top = f32::from(position.y).min(win_h - h - MARGIN).max(MARGIN);
    let root = Bounds::new(point(px(left), px(top)), size(px(ROOT_WIDTH), px(h)));
    let sub = open_sub.and_then(|i| {
        let Entry::Item(item) = entries.get(i)? else {
            return None;
        };
        if item.submenu.is_empty() {
            return None;
        }
        let sub_h = height(&item.submenu);
        // Праворуч, а за краєм вікна — ліворуч від кореня.
        let x = if left + ROOT_WIDTH + SUB_WIDTH + MARGIN <= win_w {
            left + ROOT_WIDTH - 2.
        } else {
            (left - SUB_WIDTH + 2.).max(MARGIN)
        };
        let y = (top + row_top(entries, i) - PAD)
            .min(win_h - sub_h - MARGIN)
            .max(MARGIN);
        Some(Bounds::new(point(px(x), px(y)), size(px(SUB_WIDTH), px(sub_h))))
    });
    Layout { root, sub }
}

/// Малює меню (і відкрите підменю) як окремі шари поверх вікна.
/// `close` викликається при кліку на пункт і поза меню; `hover_sub` — коли курсор
/// заходить на рядок кореня (`Some(індекс)` для рядка з підменю, інакше `None`).
pub fn render(
    id: &'static str,
    position: Point<Pixels>,
    entries: &[Entry],
    open_sub: Option<usize>,
    close: fn(&mut Workspace, &mut Context<Workspace>),
    hover_sub: fn(&mut Workspace, Option<usize>, &mut Context<Workspace>),
    window: &Window,
    cx: &mut Context<Workspace>,
) -> Vec<AnyElement> {
    let layout = layout(position, entries, open_sub, window);
    let (root, sub) = (layout.root, layout.sub);
    let mut out = vec![panel(
        (id, 0),
        root,
        entries,
        open_sub,
        sub,
        root,
        close,
        Some(hover_sub),
        cx,
    )];
    if let (Some(sub_bounds), Some(Entry::Item(parent))) =
        (sub, open_sub.and_then(|i| entries.get(i)))
    {
        out.push(panel(
            (id, 1),
            sub_bounds,
            &parent.submenu,
            None,
            None,
            root,
            close,
            None,
            cx,
        ));
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn panel(
    id: (&'static str, usize),
    bounds: Bounds<Pixels>,
    entries: &[Entry],
    open_sub: Option<usize>,
    sub_bounds: Option<Bounds<Pixels>>,
    root_bounds: Bounds<Pixels>,
    close: fn(&mut Workspace, &mut Context<Workspace>),
    hover_sub: Option<fn(&mut Workspace, Option<usize>, &mut Context<Workspace>)>,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let z = zed();
    let rows = entries.iter().enumerate().map(|(index, entry)| match entry {
        Entry::Separator => div()
            .h(px(SEPARATOR))
            .flex()
            .items_center()
            .child(div().h(px(1.)).w_full().bg(z.border_variant))
            .into_any_element(),
        Entry::Item(item) => {
            let has_sub = !item.submenu.is_empty();
            let enabled = item.enabled;
            let action = item.action.clone();
            let hint = item.hint.clone();
            let open = open_sub == Some(index);
            let color = if enabled { z.text } else { z.text_placeholder };
            div()
                .id((id.0, id.1 * 1000 + index))
                .flex()
                .flex_none()
                .items_center()
                .gap(px(8.))
                .h(px(ROW))
                .px(px(8.))
                .rounded(px(4.))
                .text_sm()
                .text_color(color)
                .when(enabled, |d| {
                    d.cursor_pointer()
                        .hover(|s| s.bg(z.element_hover))
                        .active(|s| s.bg(z.element_selected))
                })
                .when(open, |d| d.bg(z.element_hover))
                .child(
                    div()
                        .flex_none()
                        .size(px(14.))
                        .children(match item.checked {
                            Some(checked) => checked.then_some(IconName::Check),
                            None => item.icon,
                        }
                        .map(|i| ui::icon(i).text_color(if enabled {
                            z.text_muted
                        } else {
                            z.text_placeholder
                        }))),
                )
                .child(div().flex_1().min_w_0().truncate().child(item.label.clone()))
                .when_some(item.shortcut, |d, keys| {
                    d.child(
                        div()
                            .flex_none()
                            .font_family(crate::theme::MONO_FONT)
                            .text_xs()
                            .text_color(z.text_placeholder)
                            .child(keys),
                    )
                })
                .when(has_sub, |d| {
                    d.child(ui::icon(IconName::ChevronRight).text_color(z.text_muted))
                })
                .when_some(hint.filter(|_| !enabled), |d, hint| {
                    d.tooltip(move |window, cx| Tooltip::new(hint.clone()).build(window, cx))
                })
                .when_some(hover_sub, |d, hover_sub| {
                    d.on_hover(cx.listener(move |this, inside: &bool, _, cx| {
                        if *inside {
                            hover_sub(this, has_sub.then_some(index), cx);
                        }
                    }))
                })
                .when(enabled && !has_sub, |d| {
                    d.on_click(cx.listener(move |this, _, window, cx| {
                        close(this, cx);
                        if let Some(action) = &action {
                            action(this, window, cx);
                        }
                    }))
                })
                .into_any_element()
        }
    });
    let menu = div()
        .id(id)
        .occlude()
        .w(bounds.size.width)
        .flex()
        .flex_col()
        .p(px(PAD))
        .rounded(px(6.))
        .bg(z.panel)
        .border_1()
        .border_color(z.border)
        .shadow_lg()
        .text_color(z.text)
        .children(rows)
        // Клік поза обома панелями закриває меню; клік у підменю — не «поза» коренем.
        .on_mouse_down_out(cx.listener(move |this, e: &MouseDownEvent, _, cx| {
            let inside = bounds.contains(&e.position)
                || root_bounds.contains(&e.position)
                || sub_bounds.is_some_and(|b| b.contains(&e.position));
            if !inside {
                close(this, cx);
            }
        }));
    deferred(anchored().position(bounds.origin).snap_to_window().child(menu))
        .with_priority(3)
        .into_any_element()
}
