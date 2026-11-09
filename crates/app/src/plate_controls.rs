//! Кнопки пластини, як у Bambu Studio: колонка на площині столу праворуч від активної
//! пластини (малює `viewer`), тут — наведення, натискання, дії й модалка налаштувань.
use gpui::{Context, Pixels, Point, Window, div, prelude::*, px};
use viewer::{PlateButton, PlateIcon, PlateIconQuad};
use gpui_component::input::{Input, InputState};
use slicer::BED_TYPES;

use crate::{
    theme::zed,
    ui::{self, IconName},
    widgets::ToastKind,
    workspace::{Page, Workspace},
};

impl Workspace {
    /// Дії над пластиною виконуються над активною — спершу робимо її активною.
    fn with_plate(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
        action: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>),
    ) {
        if self.plates.get(index).is_none_or(|p| p.number == 0) {
            return;
        }
        self.show_plate(index);
        self.sync_plate_scene();
        action(self, window, cx);
        cx.notify();
    }

    fn plate_number(&self, index: usize) -> Option<u32> {
        self.plates
            .get(index)
            .map(|p| p.number)
            .filter(|n| *n > 0)
    }

    pub(crate) fn toggle_plate_lock(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(number) = self.plate_number(index) else {
            return;
        };
        let locked = self
            .project
            .as_ref()
            .is_some_and(|p| p.plate_locked(number));
        self.checkpoint(if locked {
            "Розблокування пластини"
        } else {
            "Блокування пластини"
        });
        if let Some(project) = self.project.as_mut() {
            match project.set_plate_locked(number, !locked) {
                Ok(()) => self.project_changed(window, cx),
                Err(err) => self.toast(ToastKind::Error, format!("{err:#}"), cx),
            }
        }
    }

    fn set_plate_bed(
        &mut self,
        number: u32,
        bed: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.checkpoint("Тип столу пластини");
        if let Some(project) = self.project.as_mut() {
            match project.set_plate_bed_type(number, bed) {
                Ok(()) => self.project_changed(window, cx),
                Err(err) => self.toast(ToastKind::Error, format!("{err:#}"), cx),
            }
        }
    }

    /// Налаштування пластини: назва, тип столу, замок і видалення.
    pub(crate) fn plate_settings(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(number) = self.plate_number(index) else {
            return;
        };
        self.show_plate(index);
        self.sync_plate_scene();
        let name = self
            .project
            .as_ref()
            .and_then(|p| p.plates.iter().find(|p| p.index == number))
            .map(|p| p.name.clone())
            .unwrap_or_default();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        // Тип столу — випадний список: «як у принтера» + типи Bambu.
        let current_bed = self
            .project
            .as_ref()
            .and_then(|p| p.plates.iter().find(|p| p.index == number))
            .and_then(|p| p.bed_type.clone());
        let mut bed_items: Vec<gpui::SharedString> =
            vec![crate::i18n::tr("Як у налаштуваннях принтера").into()];
        bed_items.extend(BED_TYPES.iter().map(|k| gpui::SharedString::from(*k)));
        let bed_index = current_bed
            .as_deref()
            .and_then(|b| BED_TYPES.iter().position(|k| *k == b))
            .map_or(0, |i| i + 1);
        let bed_select = cx.new(|cx| {
            crate::widgets::Dropdown::new(bed_items, Some(bed_index), cx)
        });
        let bed_sub = cx.subscribe_in(
            &bed_select,
            window,
            move |this, state, _: &crate::widgets::DropdownEvent, window, cx| {
                let bed = state
                    .read(cx)
                    .selected_index()
                    .and_then(|i| i.checked_sub(1))
                    .and_then(|i| BED_TYPES.get(i))
                    .map(|k| (*k).to_owned());
                this.set_plate_bed(number, bed, window, cx);
            },
        );
        self.open_modal(
            std::rc::Rc::new(move |this: &mut Workspace, window, cx| {
                // Підписка живе, доки відкрита модалка.
                let _ = &bed_sub;
                let z = zed();
                let locked = this
                    .project
                    .as_ref()
                    .and_then(|p| p.plates.iter().find(|p| p.index == number))
                    .is_some_and(|p| p.locked);
                let rename = input.clone();
                let body = div()
                    .flex()
                    .flex_col()
                    .p(px(12.))
                    .gap(px(10.))
                    .child(ui::group_title("Назва"))
                    .child(
                        div()
                            .flex()
                            .gap(px(6.))
                            .items_center()
                            .child(div().flex_1().child(crate::widgets::field(
                                &input,
                                Input::new(&input),
                                window,
                                cx,
                            )))
                            .child(
                                ui::ghost_button("plate-rename-save", Some(IconName::Check), "Зберегти")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        let value = rename.read(cx).value().trim().to_owned();
                                        this.checkpoint("Перейменування пластини");
                                        if let Some(p) = this.project.as_mut() {
                                            match p.rename_plate(number, value) {
                                                Ok(()) => this.project_changed(window, cx),
                                                Err(err) => this.toast(
                                                    ToastKind::Error,
                                                    err.to_string(),
                                                    cx,
                                                ),
                                            }
                                        }
                                    })),
                            ),
                    )
                    .child(ui::group_title("Тип столу"))
                    .child(bed_select.clone())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(ui_label("Заблокувати пластину"))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(z.text_muted)
                                            .child(crate::i18n::tr(
                                                "Розкладання її пропускає, об’єкти не рухаються"
                                            )),
                                    ),
                            )
                            .child(crate::widgets::switch("plate-lock", locked).on_click(
                                cx.listener(move |this, _, window, cx| {
                                    this.toggle_plate_lock(index, window, cx)
                                }),
                            )),
                    );
                let footer = ui::danger_button(
                    "plate-settings-delete",
                    IconName::Trash,
                    "Видалити пластину",
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.close_modal(cx);
                    this.with_plate(index, window, cx, |this, window, cx| {
                        this.delete_plate(window, cx)
                    });
                }))
                .into_any_element();
                crate::widgets::modal(
                    crate::tr_format!("Пластина {number}"),
                    380.,
                    body,
                    Some(footer),
                )
            }),
            window,
            cx,
        );
    }
}

fn ui_label(text: &str) -> impl IntoElement {
    div().text_sm().child(crate::i18n::tr(text))
}

/// Частка ширини столу між пластинами в сцені (див. `Workspace::plate_offset`).
const PLATE_GAP: f32 = 0.2;

impl Workspace {
    /// Кнопки активної пластини у світових координатах сцени. Лише на «Підготовці» й лише
    /// для активної пластини — як у Bambu Studio, решта пластин чиста.
    fn plate_buttons(&self) -> Vec<(PlateButton, glam::Vec2, glam::Vec2)> {
        if self.page != Page::Prepare || self.project.is_none() || self.camera.eye().z < 0. {
            return Vec::new();
        }
        let Some(number) = self.plate_number(self.active_plate) else {
            return Vec::new();
        };
        let (lo, hi) = self.bed_bounds();
        let offset = self.plate_offset(number).truncate();
        let lo = glam::Vec2::new(lo[0] as f32, lo[1] as f32) + offset;
        let hi = glam::Vec2::new(hi[0] as f32, hi[1] as f32) + offset;
        viewer::plate_button_rects(lo, hi, PLATE_GAP).to_vec()
    }

    /// Кнопка пластини під курсором.
    pub(crate) fn plate_button_at(&self, position: Point<Pixels>) -> Option<PlateButton> {
        let buttons = self.plate_buttons();
        if buttons.is_empty() {
            return self.plate_label_at(position).map(|_| PlateButton::Rename);
        }
        let p = self.bed_point(position)?.truncate();
        buttons
            .into_iter()
            .find(|(_, lo, hi)| p.cmpge(*lo).all() && p.cmple(*hi).all())
            .map(|(button, ..)| button)
            .or_else(|| self.plate_label_at(position).map(|_| PlateButton::Rename))
    }

    fn active_plate_locked(&self) -> bool {
        self.plate_number(self.active_plate)
            .zip(self.project.as_ref())
            .is_some_and(|(n, p)| p.plate_locked(n))
    }

    /// Передає рендереру, що малювати; рендерер сам пропускає незмінений набір.
    pub(crate) fn sync_plate_icons(&mut self) {
        let locked = self.active_plate_locked();
        let hover = self.plate_hover.map(|(b, _)| b);
        let quads: Vec<PlateIconQuad> = self
            .plate_buttons()
            .into_iter()
            .map(|(button, min, max)| PlateIconQuad {
                icon: match button {
                    PlateButton::Delete => PlateIcon::Delete,
                    PlateButton::Orient => PlateIcon::Orient,
                    PlateButton::Arrange => PlateIcon::Arrange,
                    PlateButton::Lock if locked => PlateIcon::Locked,
                    PlateButton::Lock => PlateIcon::Unlocked,
                    PlateButton::Settings | PlateButton::Rename => PlateIcon::Settings,
                },
                min,
                max,
                hover: hover == Some(button),
                dimmed: locked && matches!(button, PlateButton::Orient | PlateButton::Arrange),
            })
            .collect();
        self.renderer.set_plate_icons(&quads);
    }

    /// Наведення без натиснутих кнопок; `true`, якщо треба перемалювати.
    pub(crate) fn update_plate_hover(&mut self, position: Point<Pixels>) -> bool {
        let next = self.plate_button_at(position).map(|b| (b, position));
        let changed = next.map(|(b, _)| b) != self.plate_hover.map(|(b, _)| b);
        // Позицію оновлюємо завжди: підказка йде за курсором.
        let moved = next.is_some() && next != self.plate_hover;
        self.plate_hover = next;
        if changed {
            self.dirty = true;
        }
        changed || moved
    }

    pub(crate) fn press_plate_button(&mut self, position: Point<Pixels>) -> bool {
        self.plate_press = self.plate_button_at(position);
        // Назва іншої пластини: спершу робимо її активною, як у Bambu Studio.
        if self.plate_press == Some(PlateButton::Rename)
            && let Some(index) = self.plate_label_at(position)
            && index != self.active_plate
        {
            self.show_plate(index);
            self.sync_plate_scene();
        }
        self.plate_press.is_some()
    }

    /// Відпускання: дія спрацьовує, лише якщо курсор досі над тією самою кнопкою.
    pub(crate) fn release_plate_button(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(pressed) = self.plate_press.take() else {
            return false;
        };
        if self.plate_button_at(position) != Some(pressed) {
            return true;
        }
        let index = self.active_plate;
        let locked = self.active_plate_locked();
        match pressed {
            PlateButton::Delete => self.with_plate(index, window, cx, |this, window, cx| {
                this.delete_plate(window, cx)
            }),
            PlateButton::Orient if !locked => {
                self.with_plate(index, window, cx, |this, _, cx| this.orient_plate(index, cx))
            }
            PlateButton::Arrange if !locked => self.with_plate(index, window, cx, |this, window, cx| {
                this.arrange_plate(window, cx)
            }),
            PlateButton::Orient | PlateButton::Arrange => self.toast(
                ToastKind::Info,
                crate::i18n::tr("Пластина заблокована").to_string(),
                cx,
            ),
            PlateButton::Lock => self.toggle_plate_lock(index, window, cx),
            PlateButton::Settings => self.plate_settings(index, window, cx),
            PlateButton::Rename => self.prompt_rename_plate(window, cx),
        }
        self.plate_hover = None;
        self.dirty = true;
        cx.notify();
        true
    }
}

/// Підказка біля курсора над кнопкою пластини (сама кнопка — у 3D-сцені).
pub fn tooltip(ws: &Workspace) -> Option<impl IntoElement> {
    let (button, at) = ws.plate_hover?;
    let origin = ws.viewport_bounds?.origin;
    let locked = ws.active_plate_locked();
    let text = match button {
        PlateButton::Delete => "Видалити пластину",
        PlateButton::Orient => "Автоорієнтація пластини",
        PlateButton::Arrange => "Розкласти пластину",
        PlateButton::Lock if locked => "Розблокувати пластину",
        PlateButton::Lock => "Заблокувати пластину",
        PlateButton::Settings => "Налаштування пластини",
        PlateButton::Rename => "Перейменувати пластину",
    };
    let z = zed();
    Some(
        div()
            .absolute()
            .left(at.x - origin.x + px(14.))
            .top(at.y - origin.y + px(18.))
            .px(px(6.))
            .py(px(2.))
            .rounded(px(4.))
            .bg(z.panel)
            .border_1()
            .border_color(z.border)
            .text_xs()
            .text_color(z.text)
            .whitespace_nowrap()
            .child(crate::i18n::tr(text)),
    )
}
