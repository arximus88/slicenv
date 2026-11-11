//! Панель режимів і дій поточного проєкту над робочою областю.
//!
//! Заголовок вікна лишається для «Головної», вкладки проєкту й глобальних речей (стан
//! принтера, налаштування). Режими (Проєкт · Підготовка · Перегляд) і дії (зберегти,
//! нарізати, друкувати) живуть тут: панель плаває поверх сцени вгорі центру сторінки —
//! сцена видна довкола й крізь неї, власного рядка розкладки вона не займає.
//!
//! Панель стоїть на осі вікна, а не центру: згортання дока чи перехід між «Підготовкою»
//! й «Переглядом» (у них різні ліві доки) її не зсуває; де центр для цього завузький,
//! вона стає якнайближче до осі ([`slot`]). Куб навігації, колонка пластин, повзунок
//! шарів і панель інструмента починаються під нею ([`BarState::below`]), тож вона їх не закриває.
//!
//! Вільна ширина залежить від доків і масштабу, тому панель стискається сходинками
//! ([`Density`]): спершу зникає підказка клавіш, далі номер пластини в підписі нарізки,
//! потім підписи режимів і нарешті підписи дій; у зовсім вузькому центрі (нарізаний
//! «Перегляд» з обома доками у вікні мінімальної ширини) режими й дії стають у два рядки.
//! «Друкувати» не ховається ніколи.

use std::{cell::Cell, rc::Rc};

use gpui::{
    AnyElement, Bounds, Context, FontWeight, Pixels, SharedString, Stateful, canvas, div,
    prelude::*, px,
};
use gpui_component::tooltip::Tooltip;

use crate::{
    theme::zed,
    ui::{self, IconName, Toggle as _},
    workspace::{Page, Workspace},
};

/// Наскільки стиснуто панель; більше значення — менше тексту.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Density {
    /// Усі підписи й підказка клавіш.
    Full,
    /// Без підказки клавіш (вона лишається в tooltip).
    NoKeys,
    /// «Нарізати» без номера пластини.
    ShortSlice,
    /// Режими — лише іконки.
    IconModes,
    /// Усе — лише іконки.
    Icons,
    /// Лише іконки у два рядки: режими над діями.
    Stacked,
}

impl Density {
    const ALL: [Density; 6] = [
        Density::Full,
        Density::NoKeys,
        Density::ShortSlice,
        Density::IconModes,
        Density::Icons,
        Density::Stacked,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Density::Full => "full",
            Density::NoKeys => "no-keys",
            Density::ShortSlice => "short-slice",
            Density::IconModes => "icon-modes",
            Density::Icons => "icons",
            Density::Stacked => "stacked",
        }
    }
}

/// Підписи панелі мовою інтерфейсу — від них залежить, скільки місця їй треба.
pub(crate) struct Labels<'a> {
    /// Режими з видимим підписом («Підготовка», «Перегляд»); «Проєкт» — завжди іконка.
    pub modes: [&'a str; 2],
    /// Підпис нарізки з номером пластини і без нього (однакові, якщо пластина одна).
    pub slice_long: &'a str,
    pub slice_short: &'a str,
    pub print: &'a str,
    /// Іде нарізка: замість підказки клавіш — спінер.
    pub running: bool,
}

/// Середня ширина знака `text_sm` із запасом: шрифти Windows ширші, англійські підписи
/// довші.
const CHAR: f32 = 7.8;
const ICON_BUTTON: f32 = 24.;
/// Відступи й іконка кнопки з підписом (`ui::ghost_button`): 8 + 14 + 6 + текст + 8.
const MODE_CHROME: f32 = 36.;
/// Те саме для дії з рамкою: 1 + 10 + 14 + 6 + текст + 10 + 1.
const ACTION_CHROME: f32 = 42.;
const ACTION_ICON: f32 = 28.;
const KEYS: f32 = 58.;
const SPINNER: f32 = 20.;
const GAP: f32 = 2.;
/// Проміжок між групою режимів і діями.
const GROUP_GAP: f32 = 12.;
const PADDING: f32 = 4.;
/// Поле з кожного боку панелі до краю центру.
const MARGIN: f32 = 8.;

/// Оцінка ширини панелі за такого стиснення.
fn estimated_width(density: Density, labels: &Labels) -> f32 {
    let text = |s: &str| s.chars().count() as f32 * CHAR;
    let mode = |s: &str| {
        if density >= Density::IconModes {
            ICON_BUTTON
        } else {
            MODE_CHROME + text(s)
        }
    };
    let action = |s: &str| {
        if density >= Density::Icons {
            ACTION_ICON
        } else {
            ACTION_CHROME + text(s)
        }
    };
    let slice = if density >= Density::ShortSlice {
        labels.slice_short
    } else {
        labels.slice_long
    };
    let trailing = match density {
        Density::Icons | Density::Stacked => 0.,
        _ if labels.running => SPINNER,
        Density::Full => KEYS,
        _ => 0.,
    };
    // Шість елементів: іконка проєкту, два режими; зберегти, дві дії.
    let modes = ICON_BUTTON + mode(labels.modes[0]) + mode(labels.modes[1]) + GAP * 2.;
    let actions = ICON_BUTTON + action(slice) + trailing + action(labels.print) + GAP * 2.;
    let content = if density == Density::Stacked {
        modes.max(actions)
    } else {
        modes + GROUP_GAP + actions
    };
    // Рамка панелі й поля.
    2. + PADDING * 2. + content
}

/// Найповніший вигляд, що вміщається в `available` (ширина центру). Невідома ширина
/// (перший кадр) — повний вигляд.
pub(crate) fn density_for(available: f32, labels: &Labels) -> Density {
    if available <= 0. {
        return Density::Full;
    }
    Density::ALL
        .into_iter()
        .find(|d| estimated_width(*d, labels) <= available - MARGIN * 2.)
        .unwrap_or(Density::Stacked)
}

/// Де стоїть панель: вісь і півширина її місця в координатах вікна, стиснення.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Slot {
    pub centre: f32,
    pub half: f32,
    pub density: Density,
}

/// Місце панелі: `x`, `width` — центр сторінки в координатах вікна, `window` — ширина
/// вікна. Панель стоїть на осі вікна; де центр для цього завузький чи збоку — якнайближче
/// до неї, не виходячи за його краї.
pub(crate) fn slot(x: f32, width: f32, window: f32, labels: &Labels) -> Slot {
    let density = density_for(width, labels);
    let half = estimated_width(density, labels) / 2. + MARGIN;
    let centre = if width <= half * 2. {
        x + width / 2.
    } else {
        (window / 2.).clamp(x + half, x + width - half)
    };
    Slot { centre, half, density }
}

/// Верх панелі від верху центру і її висота в один рядок.
const TOP: f32 = 8.;
const HEIGHT: f32 = 34.;
/// Висота панелі у два рядки ([`Density::Stacked`]).
const HEIGHT_STACKED: f32 = 60.;

/// Виміряний центр сторінки й місце, де панель намальовано востаннє.
#[derive(Clone, Default)]
pub(crate) struct BarState {
    /// Лівий край, верх і ширина центру в координатах вікна.
    region: Rc<Cell<(f32, f32, f32)>>,
    /// Ширина вікна в останньому кадрі.
    pub(crate) window: Rc<Cell<f32>>,
    slot: Rc<Cell<Option<Slot>>>,
    shown: Rc<Cell<Option<Density>>>,
    /// Рамка самої панелі у вікні — лише для знімка стану (`ui_state`).
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl BarState {
    /// Стиснення панелі в останньому кадрі; `None` — панелі на сторінці немає.
    pub(crate) fn shown(&self) -> Option<Density> {
        self.shown.get()
    }

    /// Рамка панелі `[x, y, ширина, висота]` і ширина рядка, в якому вона стоїть.
    pub(crate) fn geometry(&self) -> Option<([f32; 4], f32)> {
        self.shown.get()?;
        let b = self.bounds.get()?;
        Some((
            [b.origin.x, b.origin.y, b.size.width, b.size.height].map(f32::from),
            self.region.get().2,
        ))
    }

    /// Ставить `child` на вісь панелі за `top` px від верху центру сторінки. Позиція —
    /// у координатах вікна: коли док згортається чи з’являється, вісь не чекає наступного
    /// кадру з новими розмірами центру, і панель не блимає на старому місці.
    pub(crate) fn on_axis(&self, top: f32, child: impl IntoElement) -> Option<gpui::Anchored> {
        let slot = self.slot.get()?;
        let (_, y, width) = self.region.get();
        // До першого кадру розкладки місце невідоме.
        (width > 0.).then(|| {
            gpui::anchored()
                .position(gpui::point(px(slot.centre - slot.half), px(y + top)))
                .snap_to_window_with_margin(px(0.))
                .child(div().flex().justify_center().w(px(slot.half * 2.)).child(child))
        })
    }

    /// Низ панелі від верху центру сторінки: звідси починається те, що в сцені стоїть під
    /// нею (куб навігації, колонка пластин, повзунок шарів, панель інструмента).
    pub(crate) fn below(&self) -> f32 {
        let height = if self.shown.get() == Some(Density::Stacked) { HEIGHT_STACKED } else { HEIGHT };
        TOP + height + 6.
    }

    /// Сторінка без панелі («Головна», «Пристрій»).
    pub(crate) fn hide(&self) {
        self.shown.set(None);
    }
}

/// Кнопка дії: головна — тонований акцент (`ui::accent_button`), інша — без фону. Без
/// підпису лишається сама іконка.
fn action_button(
    id: &'static str,
    icon: AnyElement,
    label: Option<SharedString>,
    primary: bool,
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
        .px(px(if label.is_some() { 10. } else { 6. }))
        .rounded(px(4.))
        .border_1()
        .border_color(gpui::transparent_black())
        .text_sm()
        .map(|d| match (primary, enabled) {
            (true, true) => d
                .cursor_pointer()
                .font_weight(FontWeight::MEDIUM)
                .bg(z.accent_bg)
                .border_color(z.accent_border)
                .text_color(z.accent)
                .hover(|s| s.bg(z.accent.opacity(0.18)).border_color(z.border_focused))
                .active(|s| s.bg(z.accent.opacity(0.26))),
            (true, false) => d
                .font_weight(FontWeight::MEDIUM)
                .border_color(z.border_variant)
                .text_color(z.text_placeholder),
            (false, true) => d
                .cursor_pointer()
                .text_color(z.text)
                .hover(|s| s.bg(z.element_hover))
                .active(|s| s.bg(z.element_selected)),
            (false, false) => d.inert(true),
        })
        .child(icon)
        .children(label)
}

impl Workspace {
    /// Сторінки поточного проєкту: лише на них є панель режимів і дій.
    pub(crate) fn shows_project_bar(&self) -> bool {
        self.path.is_some() && Page::PROJECT_TABS.contains(&self.page)
    }

    /// Центр сторінки проєкту з панеллю режимів і дій поверх нього. `scene` — у центрі
    /// 3D-сцена: панель плаває над нею; без сцени («Проєкт») вміст починається під панеллю.
    pub(crate) fn with_project_bar(
        &self,
        content: AnyElement,
        scene: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !self.shows_project_bar() {
            self.project_bar.hide();
            return content;
        }
        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .min_h_0()
            .bg(zed().editor)
            .when(!scene, |d| d.pt(px(self.project_bar.below())))
            .child(div().flex().flex_1().min_w_0().min_h_0().child(content))
            .child(self.project_bar(scene, cx))
            .into_any_element()
    }

    fn project_bar(&self, scene: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let z = zed();
        let running = self.slicing();
        let can_slice = self.can_slice();
        let can_print = self.can_send_print();
        let print_primary = can_print && !running;
        let has_project = self.project.is_some();

        // Без рушія головна дія — встановити його: сіра «Нарізати» нічого не пояснює.
        let no_engine = self.engine_missing() && !running;
        let slice_short = crate::i18n::tr(if running {
            "Скасувати"
        } else if no_engine {
            "Рушій"
        } else {
            "Нарізати"
        });
        let slice_long = match self.plate_to_slice() {
            _ if no_engine => crate::i18n::tr("Встановити рушій"),
            Some(n) if !running && self.plates.len() > 1 => {
                crate::tr_format!("Нарізати пластину {n}")
            }
            _ => slice_short.clone(),
        };
        let print_label = crate::i18n::tr(if self.cloud.sending { "Надсилаю…" } else { "Друкувати" });
        let titles = [Page::Prepare, Page::Preview].map(|p| crate::i18n::tr(p.title()));
        let (region_x, _, region_width) = self.project_bar.region.get();
        let slot = slot(
            region_x,
            region_width,
            self.project_bar.window.get(),
            &Labels {
                modes: [&titles[0], &titles[1]],
                slice_long: &slice_long,
                slice_short: &slice_short,
                print: &print_label,
                running,
            },
        );
        let density = slot.density;
        self.project_bar.shown.set(Some(density));
        self.project_bar.slot.set(Some(slot));

        let modes = Page::PROJECT_TABS.iter().enumerate().map(|(i, page)| {
            let page = *page;
            let active = self.page == page;
            // «Проєкт» — завжди іконка; назву дають tooltip і доступне ім’я.
            let labelled = page != Page::Project && density < Density::IconModes;
            let button = if labelled {
                let tip: SharedString = crate::i18n::tr(page.hint()).into();
                ui::ghost_button(("page", i), Some(page.icon()), page.title())
                    .text_color(z.text_muted)
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            } else {
                ui::icon_button(("page", i), page.icon(), page.hint())
            };
            button
                .toggled(active)
            .role(gpui::Role::Tab)
            .aria_label(crate::i18n::tr(page.title()))
            .aria_selected(active)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.enter_page(page, cx);
                cx.notify();
            }))
        });
        let modes: Vec<_> = modes.collect();

        let slice_tip: SharedString = if running {
            crate::i18n::tr("Зупинити нарізку")
        } else if no_engine {
            crate::i18n::tr("Рушія нарізки немає — встановити OrcaSlicer або Bambu Studio")
        } else {
            crate::tr_format!(
                "Нарізати активну пластину рушієм нарізки {} (Ctrl+R)",
                self.engine_label
            )
        }
        .into();
        // Іконка без підпису під час нарізки — спінер: видно, що кнопка зупиняє.
        let icons = density >= Density::Icons;
        let slice_icon = if running && icons {
            ui::spinner("slice-btn-spin")
        } else if no_engine {
            ui::icon(IconName::Download).into_any_element()
        } else {
            ui::icon(IconName::Scissors).into_any_element()
        };
        let slice_text = match density {
            Density::Icons | Density::Stacked => None,
            Density::Full | Density::NoKeys => Some(slice_long.into()),
            Density::ShortSlice | Density::IconModes => Some(slice_short.into()),
        };
        // Виділена кнопка — наступний крок: до нарізки «Нарізати», після — «Друкувати».
        let slice = action_button(
            "slice",
            slice_icon,
            slice_text,
            !print_primary,
            print_primary || running || can_slice || no_engine,
        )
        .role(gpui::Role::Button)
        .aria_label(slice_tip.clone())
        .tooltip(move |window, cx| Tooltip::new(slice_tip.clone()).build(window, cx))
        .when(running && !icons, |d| {
            d.child(ui::spinner("slice-btn-spin"))
        })
        .when(!running && !no_engine && density == Density::Full, |d| d.child(ui::kbd("Ctrl+R")))
        .when(no_engine, |d| {
            d.on_click(cx.listener(|this, _, window, cx| this.open_engine_setup(window, cx)))
        })
        .when(running || can_slice, |d| {
            d.on_click(cx.listener(|this, _, window, cx| {
                if this.slicing() {
                    this.stop_slice();
                    cx.notify();
                } else {
                    this.start_slice(window, cx);
                }
            }))
        });

        // Чому друк недоступний; без підпису на кнопці підказка називає саму дію.
        let print_tip = self
            .print_blocker()
            .or_else(|| icons.then(|| print_label.clone().into()));
        let print = action_button(
            "send-print",
            ui::icon(IconName::Send).into_any_element(),
            (!icons).then(|| print_label.clone().into()),
            print_primary,
            can_print,
        )
        .role(gpui::Role::Button)
        .aria_label(print_label)
        .when(can_print, |d| {
            d.on_click(cx.listener(|this, _, window, cx| this.open_print_dialog(window, cx)))
        })
        .when_some(print_tip, |d, tip| {
            d.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        });

        let save = ui::icon_button("save-project", IconName::Save, "Зберегти проєкт (Ctrl+S)")
            .inert(!has_project)
            .when(has_project, |d| {
                d.on_click(cx.listener(|this, _, window, cx| this.prompt_save(window, cx)))
            });

        // Край і ширину центру знає лише розкладка: якщо за ними панель мала стояти інакше,
        // наступний кадр перемалює її. Центр від панелі не залежить, тож це не цикл.
        let measure = {
            let region = self.project_bar.region.clone();
            let entity = cx.entity().downgrade();
            canvas(
                move |bounds, _, cx| {
                    let measured = (
                        f32::from(bounds.origin.x),
                        f32::from(bounds.origin.y),
                        f32::from(bounds.size.width),
                    );
                    let (x, y, width) = region.get();
                    if (x - measured.0).abs() >= 0.5
                        || (y - measured.1).abs() >= 0.5
                        || (width - measured.2).abs() >= 0.5
                    {
                        region.set(measured);
                        let entity = entity.clone();
                        cx.defer(move |cx| {
                            let _ = entity.update(cx, |_, cx| cx.notify());
                        });
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0()
        };
        // Рамка панелі потрібна лише приймальним сценаріям (`SLICENV_UI_TRACE`, MCP `ui_state`).
        let outline = {
            let slot = self.project_bar.bounds.clone();
            let entity = cx.entity().downgrade();
            canvas(
                move |bounds, _, cx| {
                    if slot.replace(Some(bounds)) != Some(bounds) && crate::ui_state::trace_enabled() {
                        let entity = entity.clone();
                        cx.defer(move |cx| {
                            let _ = entity.update(cx, |_, cx| cx.notify());
                        });
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0()
        };

        let bar =                 ui::scene_control("project-bar")
                    .relative()
                    .flex()
                    .flex_none()
                    .items_center()
                    .map(|d| {
                        if density == Density::Stacked {
                            d.flex_col().gap(px(GAP))
                        } else {
                            d.gap(px(GROUP_GAP))
                        }
                    })
                    .p(px(PADDING))
                    .rounded(px(8.))
                    .bg(z.panel.opacity(if scene { 0.94 } else { 1. }))
                    .border_1()
                    .border_color(z.border_variant)
                    .shadow_md()
                    .child(outline)
                    .child(
                        div()
                            .id("project-modes")
                            .role(gpui::Role::TabList)
                            .flex()
                            .items_center()
                            .gap(px(GAP))
                            .children(modes),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(GAP))
                            .child(save)
                            .child(slice)
                            .child(print),
                    );
        // Шар на весь центр: сам він подій не ловить, сцену під собою закриває лише панель.
        div()
            .absolute()
            .inset_0()
            .child(measure)
            .children(self.project_bar.on_axis(TOP, bar))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(running: bool) -> Labels<'static> {
        Labels {
            modes: ["Підготовка", "Перегляд"],
            slice_long: if running { "Скасувати" } else { "Нарізати пластину 12" },
            slice_short: if running { "Скасувати" } else { "Нарізати" },
            print: "Друкувати",
            running,
        }
    }

    #[test]
    fn project_icon_leads_the_modes() {
        assert_eq!(Page::PROJECT_TABS[0], Page::Project);
        assert_eq!(Page::PROJECT_TABS.len(), 3);
    }

    /// Вужчий центр ніколи не дає повнішої панелі, а вибраний вигляд справді вміщається.
    #[test]
    fn density_only_tightens_as_the_centre_narrows() {
        for running in [false, true] {
            let labels = labels(running);
            let mut previous = Density::Full;
            for width in (150..=1400).rev() {
                let density = density_for(width as f32, &labels);
                assert!(density >= previous, "{width}px: {density:?} після {previous:?}");
                if density != Density::Stacked {
                    assert!(estimated_width(density, &labels) <= width as f32 - MARGIN * 2.);
                }
                previous = density;
            }
            assert_eq!(density_for(1400., &labels), Density::Full);
            assert_eq!(density_for(320., &labels), Density::Icons);
            assert_eq!(density_for(150., &labels), Density::Stacked);
        }
    }

    /// Усі шість кнопок («Друкувати» серед них) вміщаються в найвужчий штатний центр:
    /// нарізаний «Перегляд» у вікні мінімальної ширини (960) з лівим доком 460 і правим 380.
    /// Масштаб 125/150 % цього не змінює — мінімальна ширина вікна задана в логічних px.
    #[test]
    fn every_button_fits_the_narrowest_centre() {
        let centre = 960. - 460. - 380.;
        for running in [false, true] {
            let labels = labels(running);
            assert_eq!(density_for(centre, &labels), Density::Stacked);
            assert!(estimated_width(Density::Stacked, &labels) <= centre - MARGIN * 2.);
        }
    }

    /// Підказка клавіш зникає першою, номер пластини — другим, підписи режимів — раніше
    /// за підписи дій.
    #[test]
    fn labels_give_way_in_order() {
        let labels = labels(false);
        let at = |d| estimated_width(d, &labels);
        for pair in Density::ALL.windows(2) {
            assert!(at(pair[1]) < at(pair[0]), "{:?} не вужчий за {:?}", pair[1], pair[0]);
        }
        let between = |a, b| density_for((at(a) + at(b)) / 2. + MARGIN * 2., &labels);
        assert_eq!(between(Density::Full, Density::NoKeys), Density::NoKeys);
        assert_eq!(between(Density::NoKeys, Density::ShortSlice), Density::ShortSlice);
        assert_eq!(between(Density::ShortSlice, Density::IconModes), Density::IconModes);
    }

    /// Панель стоїть на осі вікна, хоч який лівий док: «Підготовка» (док 272), «Перегляд»
    /// до нарізки (без дока) і згорнуті доки дають ту саму позицію у вікні.
    #[test]
    fn docks_do_not_move_the_bar() {
        let labels = labels(false);
        let window = 1534.;
        for (x, width) in [(272., 1534. - 272. - 380.), (0., 1534. - 380.), (0., 1534.), (272., 1534. - 272.)] {
            let s = slot(x, width, window, &labels);
            assert_eq!(s.density, Density::Full);
            assert_eq!(s.centre, window / 2., "док {x}, центр {width}");
        }
    }

    /// Центр збоку від осі вікна: панель якнайближче до осі, але ціла в його межах.
    #[test]
    fn bar_stays_inside_an_off_axis_centre() {
        let labels = labels(false);
        for (x, width) in [(1000., 400.), (460., 500.), (0., 300.), (20., 120.)] {
            let s = slot(x, width, 1534., &labels);
            assert_eq!(s.density, density_for(width, &labels));
            if s.density != Density::Stacked {
                assert!(s.centre - s.half >= x - 0.01 && s.centre + s.half <= x + width + 0.01, "{x}+{width}: {s:?}");
            }
        }
        // Центр праворуч від осі: панель притиснута до його лівого краю.
        let s = slot(1000., 400., 1534., &labels);
        assert!((s.centre - s.half - 1000.).abs() < 0.01);
    }

    /// Невідома ширина (до першого кадру розкладки) не ховає підписів.
    #[test]
    fn unknown_width_keeps_full_labels() {
        assert_eq!(density_for(0., &labels(false)), Density::Full);
    }
}
