//! Кешовані частини вікна: панелі доків і 3D-в’юпорт як окремі сутності GPUI.
//!
//! Уся сторінка — один `Workspace`, тож будь-який `cx.notify()` будував заново все дерево
//! елементів (дерево об’єктів, редактор процесу, статистику Preview) і розкладав його.
//! Панель тут — тонкий вид, що малює ту саму функцію від `&mut Workspace`, але вбудований
//! як [`AnyView::cached`]: GPUI повторює її минулий кадр, поки вид не позначено брудним.
//! Панель стає брудною, коли сповіщає сам `Workspace` (підписка `observe`) або її власний
//! елемент (наприклад, анімація скролбара). Рух камери сповіщає лише вид в’юпорта
//! ([`Workspace::notify_viewport`]): `Workspace` перебудовується як предок, але панелі
//! лишаються з кешу.

use gpui::{
    AnyElement, AnyView, App, AppContext as _, Context, Entity, IntoElement, ParentElement, Render,
    StyleRefinement, Styled, Subscription, WeakEntity, Window, div,
};

use crate::workspace::Workspace;

/// Малює частину вікна зі стану `Workspace`.
pub type PanelFn = fn(&mut Workspace, &mut Window, &mut Context<Workspace>) -> AnyElement;

pub struct PanelView {
    workspace: WeakEntity<Workspace>,
    render: PanelFn,
    _observe: Option<Subscription>,
}

impl Render for PanelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let render = self.render;
        self.workspace
            .update(cx, |ws, cx| render(ws, window, cx))
            .unwrap_or_else(|_| div().into_any_element())
    }
}

/// Види панелей одного вікна за ключем (ключ — місце на сторінці, функція — що малювати).
#[derive(Default)]
pub struct PanelCache {
    views: Vec<(&'static str, Entity<PanelView>)>,
    viewports: Vec<(&'static str, Entity<PanelView>)>,
}

impl Workspace {
    /// Панель доку з кешу кадру: перебудовується лише після `cx.notify()` самого
    /// `Workspace` або власного елемента.
    pub fn cached_panel(
        &mut self,
        key: &'static str,
        render: PanelFn,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = match self.panels.views.iter().find(|(k, _)| *k == key) {
            Some((_, view)) => view.clone(),
            None => {
                let workspace = cx.entity();
                let view = cx.new(|cx| PanelView {
                    workspace: workspace.downgrade(),
                    render,
                    _observe: Some(cx.observe(&workspace, |_, _, cx| cx.notify())),
                });
                self.panels.views.push((key, view.clone()));
                view
            }
        };
        AnyView::from(view)
            .cached(StyleRefinement::default().size_full())
            .into_any_element()
    }

    /// Центр сторінки з 3D-в’юпортом. Не кешується (перебудовується з кожним кадром
    /// `Workspace`), але має власну сутність: рух камери сповіщає лише її, і панелі доків
    /// лишаються з кешу.
    pub fn viewport_view(
        &mut self,
        key: &'static str,
        render: PanelFn,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = match self.panels.viewports.iter().find(|(k, _)| *k == key) {
            Some((_, view)) => view.clone(),
            None => {
                let workspace = cx.weak_entity();
                let view = cx.new(|_| PanelView {
                    workspace,
                    render,
                    _observe: None,
                });
                self.panels.viewports.push((key, view.clone()));
                view
            }
        };
        div()
            .size_full()
            .min_w_0()
            .child(AnyView::from(view))
            .into_any_element()
    }

    /// Перемалювати лише в’юпорт (камера, підсвітка під курсором, готовий кадр GPU).
    /// Без видів (тести, перший кадр) — звичайне сповіщення.
    pub fn notify_viewport(&self, cx: &mut Context<Self>) {
        if self.panels.viewports.is_empty() {
            cx.notify();
            return;
        }
        notify_views(&self.panels.viewports, cx);
    }
}

fn notify_views(views: &[(&'static str, Entity<PanelView>)], cx: &mut App) {
    for (_, view) in views {
        view.update(cx, |_, cx| cx.notify());
    }
}
