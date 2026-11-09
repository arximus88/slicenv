//! Preview «Фото»: реалістичний вигляд надрукованого виробу (docs/PHOTO-MODE.md).
//!
//! Стан режиму — матеріали слотів філаменту (автовибір із профілю + правки користувача),
//! сцена, підтримки — живе в `Preview` і переживає вимкнення режиму. GPU-частина (буфер
//! траєкторії «Фото», запечене IBL, тіні) існує лише, поки режим увімкнено.

use std::path::PathBuf;

use gpui::{Context, Entity, Hsla, SharedString, Subscription, Window, div, prelude::*, px};
use gpui_component::{
    color_picker::{ColorPickerEvent, ColorPickerState, ColorSelect},
    slider::{Slider, SliderEvent, SliderState},
};
use viewer::{MaterialTable, PhotoMaterial, PhotoSettings, Stage, SupportView, ToolpathGpuExt as _};

use crate::{
    jobs::{JobKind, JobTarget},
    theme::zed,
    ui::{self, IconName},
    widgets::{Dropdown, DropdownEvent},
    workspace::Workspace,
};

/// Найдовша сторона збереженого зображення, px.
const EXPORT_LONG_SIDE: f32 = 3840.0;

/// Матеріал одного слота філаменту.
#[derive(Clone, Debug, PartialEq)]
pub struct SlotMaterial {
    /// `filament_type` і назва профілю з G-code (для підпису й автовибору).
    pub kind: String,
    pub name: String,
    /// Колір профілю (`filament_colour`), sRGB.
    pub profile_color: [f32; 3],
    /// Пресет, визначений за профілем.
    pub auto: usize,
    pub preset: usize,
    /// Правки користувача; `None` — як у пресету й профілю.
    pub color: Option<[f32; 3]>,
    pub roughness: Option<f32>,
    pub gloss: Option<f32>,
}

impl SlotMaterial {
    fn from_slot(table: &MaterialTable, slot: &gcode::FilamentSlot) -> Self {
        let auto = table.select(&slot.kind, &slot.name);
        Self {
            kind: slot.kind.clone(),
            name: slot.name.clone(),
            profile_color: slot.colour.unwrap_or([0.85, 0.85, 0.85]),
            auto,
            preset: auto,
            color: None,
            roughness: None,
            gloss: None,
        }
    }

    /// Параметри для шейдера: пресет + колір + правки. «Блиск» — сила відблиску (specular).
    pub fn resolve(&self, table: &MaterialTable) -> PhotoMaterial {
        let mut m = table.resolve(self.preset, self.color.unwrap_or(self.profile_color));
        if let Some(r) = self.roughness {
            m.roughness = r;
        }
        if let Some(g) = self.gloss {
            m.specular = g;
        }
        m
    }

    fn reset(&mut self) {
        self.preset = self.auto;
        self.color = None;
        self.roughness = None;
        self.gloss = None;
    }
}

/// Слоти з конфігу G-code; без нього — один слот із типовим пресетом.
pub fn slots_of(table: &MaterialTable, toolpath: &gcode::Toolpath) -> Vec<SlotMaterial> {
    let slots = gcode::filament_slots(toolpath);
    if slots.is_empty() {
        return vec![SlotMaterial::from_slot(table, &gcode::FilamentSlot::default())];
    }
    slots.iter().map(|s| SlotMaterial::from_slot(table, s)).collect()
}

/// Слоти, якими друкується траєкторія (інструменти екструзії), за зростанням.
pub fn used_slots(toolpath: &gcode::Toolpath) -> Vec<usize> {
    let mut used = [false; 256];
    for s in toolpath.segments.iter().filter(|s| s.is_extrusion) {
        used[usize::from(s.tool)] = true;
    }
    (0..used.len()).filter(|&i| used[i]).collect()
}

/// Слот, який панель показує першим: перший, яким справді друкується пластина. Інакше
/// правка кольору могла йти в слот, якого на пластині немає (проєкт з кількома філаментами,
/// пластина друкується другим) — і модель не змінювалась.
pub fn initial_slot(used: &[usize], slots: usize) -> usize {
    used.iter().copied().find(|&i| i < slots).unwrap_or(0)
}

/// Що з «Фото» переживає заміну прев’ю (інша пластина, нова нарізка): правки матеріалів
/// слотів, сцена, підтримки, переміщення.
#[derive(Clone, Debug)]
pub struct PhotoMemory {
    pub slots: Vec<SlotMaterial>,
    pub supports: SupportView,
    pub stage: Stage,
    pub travel: bool,
    /// Увімкнути «Фото» знову, щойно з’явиться прев’ю (перемикання пластини).
    pub resume: bool,
}

impl PhotoMemory {
    /// Переносить правки на слоти нової траєкторії: лише той самий філамент у тому самому
    /// слоті (тип, профіль і колір профілю).
    pub fn apply(&self, slots: &mut [SlotMaterial]) {
        for (slot, old) in slots.iter_mut().zip(&self.slots) {
            if (&slot.kind, &slot.name, slot.profile_color) == (&old.kind, &old.name, old.profile_color) {
                slot.preset = old.preset;
                slot.color = old.color;
                slot.roughness = old.roughness;
                slot.gloss = old.gloss;
            }
        }
    }
}

const SCENES: [&str; 2] = ["Стіл пластини", "Нейтральна підлога"];
const SUPPORTS: [&str; 3] = ["Як модель", "Приглушено", "Сховати"];

pub struct PhotoState {
    /// Режим увімкнено (вигляд «Фото» замість «Шари»).
    pub on: bool,
    /// Траєкторія «Фото» на GPU; `None`, доки будується або режим вимкнено.
    pub built: Option<viewer::BuiltToolpath>,
    table: MaterialTable,
    pub slots: Vec<SlotMaterial>,
    /// Слот, який редагує панель.
    pub slot: usize,
    pub supports: SupportView,
    pub stage: Stage,
    pub travel: bool,
    slot_picker: Entity<Dropdown>,
    preset_picker: Entity<Dropdown>,
    scene_picker: Entity<Dropdown>,
    support_picker: Entity<Dropdown>,
    color: Entity<ColorPickerState>,
    roughness: Entity<SliderState>,
    gloss: Entity<SliderState>,
    _subscriptions: Vec<Subscription>,
}

impl PhotoState {
    pub fn settings(&self) -> PhotoSettings {
        PhotoSettings {
            materials: self.slots.iter().map(|s| s.resolve(&self.table)).collect(),
            supports: self.supports,
            stage: self.stage,
            travel: self.travel,
            ..Default::default()
        }
    }

    fn current(&self) -> Option<&SlotMaterial> {
        self.slots.get(self.slot)
    }

    pub fn memory(&self, resume: bool) -> PhotoMemory {
        PhotoMemory {
            slots: self.slots.clone(),
            supports: self.supports,
            stage: self.stage,
            travel: self.travel,
            resume: resume && self.on,
        }
    }
}

fn to_hsla([r, g, b]: [f32; 3]) -> Hsla {
    gpui::Rgba { r, g, b, a: 1.0 }.into()
}

fn from_hsla(c: Hsla) -> [f32; 3] {
    let rgb = c.to_rgb();
    [rgb.r, rgb.g, rgb.b]
}

fn slot_label(i: usize, slot: &SlotMaterial, used: bool) -> SharedString {
    let name = slot
        .name
        .split('@')
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();
    let name = if name.is_empty() { slot.kind.clone() } else { name };
    let label = if name.is_empty() {
        crate::tr_format!("Філамент {}", i + 1)
    } else {
        format!("{} · {name}", i + 1)
    };
    if used {
        label.into()
    } else {
        crate::tr_format!("{label} — не на цій пластині").into()
    }
}

impl Workspace {
    /// Вигляд Preview: `true` — «Фото», `false` — «Шари».
    pub fn set_photo_mode(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.preview.as_ref().is_none_or(|p| p.photo_on() == on) {
            return;
        }
        self.stop_playback();
        let Some(preview) = &mut self.preview else {
            return;
        };
        if !on {
            if let Some(photo) = &mut preview.photo {
                photo.on = false;
                // Буфер «Фото» й ресурси рендерера звільняються: звичайний Preview — як був.
                photo.built = None;
            }
            self.renderer.set_photo(None);
            self.renderer.set_toolpath(&preview.built.gpu);
            self.apply_preview_view(cx);
            cx.notify();
            return;
        }
        if preview.photo.is_none() {
            let memory = self.photo_memory.take();
            let toolpath = preview.toolpath.clone();
            let state = new_state(&toolpath, memory.as_ref(), window, cx);
            if let Some(preview) = &mut self.preview {
                preview.photo = Some(state);
            }
        }
        if let Some(photo) = self.preview.as_mut().and_then(|p| p.photo.as_mut()) {
            photo.on = true;
        }
        self.build_photo_toolpath(cx);
        cx.notify();
    }

    /// Запам’ятовує «Фото» прев’ю, яке зараз зміниться; `resume` — увімкнути режим знову
    /// на новому прев’ю (перемикання пластини).
    pub fn remember_photo(&mut self, resume: bool) {
        if let Some(photo) = self.preview.as_ref().and_then(|p| p.photo.as_ref()) {
            self.photo_memory = Some(photo.memory(resume));
        }
    }

    /// Буфер «Фото» будується у фоні; до готовності в’юпорт показує звичайні шари.
    fn build_photo_toolpath(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = &self.preview else { return };
        let toolpath = preview.toolpath.clone();
        let device = self.renderer.device().clone();
        let data = toolpath.clone();
        self.spawn_job(
            JobKind::ToolpathBuild,
            JobTarget::any(),
            move || {
                let _s = paths::profile::Span::new("preview::build_photo");
                data.build_photo(&device)
            },
            move |this, built, _, cx| {
                let Some(preview) = &mut this.preview else { return };
                if !std::sync::Arc::ptr_eq(&preview.toolpath, &toolpath) {
                    return;
                }
                let Some(photo) = &mut preview.photo else { return };
                if !photo.on {
                    return;
                }
                photo.built = Some(built);
                this.sync_photo(cx);
            },
            cx,
        );
    }

    /// Передає рендереру траєкторію й налаштування «Фото» (якщо режим увімкнено й готовий).
    pub fn sync_photo(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = &self.preview else { return };
        let Some(photo) = preview.photo.as_ref().filter(|p| p.on) else {
            return;
        };
        let Some(built) = &photo.built else { return };
        if self.page != crate::workspace::Page::Preview {
            return;
        }
        self.renderer.set_toolpath(&built.gpu);
        self.renderer.set_photo(Some(photo.settings()));
        self.apply_preview_view(cx);
        cx.notify();
    }

    /// Оновлює поля панелі (колір, повзунки, пресет) під вибраний слот.
    fn refresh_photo_controls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(photo) = self.preview.as_ref().and_then(|p| p.photo.as_ref()) else {
            return;
        };
        let Some(slot) = photo.current() else { return };
        let m = slot.resolve(&photo.table);
        let (preset, color, roughness, gloss) = (slot.preset, m.color, m.roughness, m.specular);
        let (preset_picker, color_state, rough_state, gloss_state) = (
            photo.preset_picker.clone(),
            photo.color.clone(),
            photo.roughness.clone(),
            photo.gloss.clone(),
        );
        preset_picker.update(cx, |d, cx| d.set_selected_index(Some(preset), cx));
        color_state.update(cx, |s, cx| s.set_value(to_hsla(color), window, cx));
        rough_state.update(cx, |s, cx| s.set_value(roughness, window, cx));
        gloss_state.update(cx, |s, cx| s.set_value(gloss, window, cx));
    }

    fn edit_photo_slot(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut SlotMaterial)) {
        let Some(photo) = self.preview.as_mut().and_then(|p| p.photo.as_mut()) else {
            return;
        };
        let index = photo.slot;
        if let Some(slot) = photo.slots.get_mut(index) {
            edit(slot);
        }
        self.sync_photo(cx);
    }

    /// «Зберегти зображення…»: PNG поза екраном, найдовша сторона 3840 px, пропорції в’юпорта.
    pub fn prompt_save_photo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.renderer.photo_active() {
            self.toast(crate::widgets::ToastKind::Warning, "Режим «Фото» ще готується", cx);
            return;
        }
        let dir = self
            .path
            .as_ref()
            .and_then(|p| p.parent())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.library_dir.clone());
        let stem = self
            .path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().trim_end_matches(".gcode").to_owned())
            .unwrap_or_else(|| "slicenv".into());
        let name = format!("{stem}-photo.png");
        let target = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut target))) = target.await else {
                return;
            };
            if target.extension().is_none_or(|e| !e.eq_ignore_ascii_case("png")) {
                target.set_extension("png");
            }
            this.update(cx, |this, cx| {
                use crate::widgets::ToastKind;
                let (w, h) = this
                    .viewport_bounds
                    .map(|b| (f32::from(b.size.width), f32::from(b.size.height)))
                    .filter(|(w, h)| *w > 1.0 && *h > 1.0)
                    .unwrap_or((16.0, 10.0));
                let k = EXPORT_LONG_SIDE / w.max(h);
                let (w, h) = ((w * k).round() as u32, (h * k).round() as u32);
                let camera = this.camera.clone();
                let result = this
                    .renderer
                    .render_photo_png(w, h, &camera)
                    .and_then(|png| Ok(std::fs::write(&target, png)?));
                this.dirty = true;
                match result {
                    Ok(()) => this.toast(
                        ToastKind::Success,
                        crate::tr_format!("Збережено {}", crate::shorten_home(&target)),
                        cx,
                    ),
                    Err(err) => this.toast(
                        ToastKind::Error,
                        crate::tr_format!("Зображення не збережено: {err:#}"),
                        cx,
                    ),
                }
            })
            .ok();
        })
        .detach();
    }
}

/// Стан режиму з віджетами панелі; матеріали — з конфігу G-code.
fn new_state(
    toolpath: &gcode::Toolpath,
    memory: Option<&PhotoMemory>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> PhotoState {
    let table = MaterialTable::builtin();
    let mut slots = slots_of(&table, toolpath);
    if let Some(memory) = memory {
        memory.apply(&mut slots);
    }
    let used = used_slots(toolpath);
    let slot = initial_slot(&used, slots.len());
    let first = slots[slot].resolve(&table);
    let slot_picker = cx.new(|cx| {
        Dropdown::new(
            slots
                .iter()
                .enumerate()
                .map(|(i, s)| slot_label(i, s, used.is_empty() || used.contains(&i)))
                .collect(),
            Some(slot),
            cx,
        )
    });
    let preset_picker = cx.new(|cx| {
        Dropdown::new(
            table.presets.iter().map(|p| SharedString::from(p.label.clone())).collect(),
            Some(slots[slot].preset),
            cx,
        )
    });
    let stage = memory.map_or(Stage::Bed, |m| m.stage);
    let supports = memory.map_or(SupportView::Dimmed, |m| m.supports);
    let scene = usize::from(stage == Stage::Floor);
    let support = match supports {
        SupportView::Shown => 0,
        SupportView::Dimmed => 1,
        SupportView::Hidden => 2,
    };
    let scene_picker = cx.new(|cx| Dropdown::new(SCENES.map(SharedString::from).to_vec(), Some(scene), cx));
    let support_picker = cx.new(|cx| Dropdown::new(SUPPORTS.map(SharedString::from).to_vec(), Some(support), cx));
    let color = cx.new(|cx| ColorPickerState::new(window, cx).default_value(to_hsla(first.color)));
    let slider = |value: f32| SliderState::new().min(0.0).max(1.0).step(0.01).default_value(value);
    let roughness = cx.new(|_| slider(first.roughness));
    let gloss = cx.new(|_| slider(first.specular));

    let subscriptions = vec![
        cx.subscribe_in(&slot_picker, window, |this, picker, _: &DropdownEvent, window, cx| {
            let index = picker.read(cx).selected_index().unwrap_or(0);
            if let Some(photo) = this.preview.as_mut().and_then(|p| p.photo.as_mut()) {
                photo.slot = index.min(photo.slots.len().saturating_sub(1));
            }
            this.refresh_photo_controls(window, cx);
            cx.notify();
        }),
        cx.subscribe_in(&preset_picker, window, |this, picker, _: &DropdownEvent, window, cx| {
            let Some(index) = picker.read(cx).selected_index() else { return };
            // Новий пресет — зі своєю шорсткістю й блиском; колір лишається.
            this.edit_photo_slot(cx, |s| {
                s.preset = index;
                s.roughness = None;
                s.gloss = None;
            });
            this.refresh_photo_controls(window, cx);
        }),
        cx.subscribe_in(&scene_picker, window, |this, picker, _: &DropdownEvent, _, cx| {
            let index = picker.read(cx).selected_index().unwrap_or(0);
            if let Some(photo) = this.preview.as_mut().and_then(|p| p.photo.as_mut()) {
                photo.stage = if index == 1 { Stage::Floor } else { Stage::Bed };
            }
            this.sync_photo(cx);
        }),
        cx.subscribe_in(&support_picker, window, |this, picker, _: &DropdownEvent, _, cx| {
            let index = picker.read(cx).selected_index().unwrap_or(1);
            if let Some(photo) = this.preview.as_mut().and_then(|p| p.photo.as_mut()) {
                photo.supports = match index {
                    0 => SupportView::Shown,
                    2 => SupportView::Hidden,
                    _ => SupportView::Dimmed,
                };
            }
            this.sync_photo(cx);
        }),
        cx.subscribe_in(&color, window, |this, _, event: &ColorPickerEvent, _, cx| {
            let ColorPickerEvent::Change(Some(color)) = event else { return };
            let rgb = from_hsla(*color);
            this.edit_photo_slot(cx, |s| s.color = Some(rgb));
        }),
        cx.subscribe_in(&roughness, window, |this, _, event: &SliderEvent, _, cx| {
            let (SliderEvent::Change(v) | SliderEvent::Release(v)) = event;
            let v = v.end();
            this.edit_photo_slot(cx, |s| s.roughness = Some(v));
        }),
        cx.subscribe_in(&gloss, window, |this, _, event: &SliderEvent, _, cx| {
            let (SliderEvent::Change(v) | SliderEvent::Release(v)) = event;
            let v = v.end();
            this.edit_photo_slot(cx, |s| s.gloss = Some(v));
        }),
    ];
    PhotoState {
        on: false,
        built: None,
        table,
        slots,
        slot,
        supports,
        stage,
        travel: memory.is_some_and(|m| m.travel),
        slot_picker,
        preset_picker,
        scene_picker,
        support_picker,
        color,
        roughness,
        gloss,
        _subscriptions: subscriptions,
    }
}

/// Перемикач вигляду в тулбарі Preview: «Шари / Фото».
pub fn mode_switch(ws: &Workspace, cx: &mut Context<Workspace>) -> gpui::AnyElement {
    let z = zed();
    let on = ws
        .preview
        .as_ref()
        .and_then(|p| p.photo.as_ref())
        .is_some_and(|p| p.on);
    let segment = |id: &'static str, icon: IconName, label: &'static str, active: bool, photo: bool| {
        div()
            .id(id)
            .flex()
            .items_center()
            .gap(px(4.))
            .h(px(22.))
            .px(px(8.))
            .rounded(px(4.))
            .cursor_pointer()
            .text_sm()
            .text_color(if active { z.text } else { z.text_muted })
            .when(active, |d| d.bg(z.element_selected))
            .when(!active, |d| d.hover(|s| s.bg(z.element_hover)))
            .child(gpui_component::Icon::new(icon).size(px(14.)))
            .child(crate::i18n::tr(label))
            .on_click(cx.listener(move |this, _, window, cx| this.set_photo_mode(photo, window, cx)))
    };
    div()
        .flex()
        .items_center()
        .gap(px(2.))
        .p(px(2.))
        .rounded(px(6.))
        .border_1()
        .border_color(z.border_variant)
        .child(segment("preview-mode-layers", IconName::Layers, "Шари", !on, false))
        .child(segment("preview-mode-photo", IconName::Camera, "Фото", on, true))
        .into_any_element()
}

fn slider_row(label: &str, state: &Entity<SliderState>, cx: &gpui::App) -> gpui::Div {
    let value = state.read(cx).value().end();
    ui::prop_row(
        label.to_owned(),
        div()
            .flex()
            .flex_1()
            .items_center()
            .gap(px(8.))
            .child(div().flex_1().child(Slider::new(state)))
            .child(
                div()
                    .w(px(32.))
                    .text_xs()
                    .font_family(crate::theme::MONO_FONT)
                    .text_color(zed().text_muted)
                    .child(format!("{value:.2}")),
            ),
    )
}

/// Права панель у режимі «Фото»: матеріал, сцена, збереження.
pub fn panel(ws: &Workspace, cx: &mut Context<Workspace>) -> Vec<gpui::AnyElement> {
    let z = zed();
    let Some(photo) = ws.preview.as_ref().and_then(|p| p.photo.as_ref()) else {
        return Vec::new();
    };
    let mut rows: Vec<gpui::AnyElement> = Vec::new();
    rows.push(
        div()
            .px(px(10.))
            .pt(px(8.))
            .pb(px(2.))
            .text_xs()
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(z.text_muted)
            .child(crate::i18n::tr("Матеріал"))
            .into_any_element(),
    );
    if photo.slots.len() > 1 {
        rows.push(ui::prop_row("Філамент", photo.slot_picker.clone()).into_any_element());
    }
    if let Some(slot) = photo.current() {
        let profile = if slot.name.is_empty() { slot.kind.clone() } else { slot.name.clone() };
        if !profile.is_empty() {
            rows.push(
                div()
                    .px(px(10.))
                    .text_xs()
                    .text_color(z.text_placeholder)
                    .truncate()
                    .child(crate::tr_format!("Профіль: {profile}"))
                    .into_any_element(),
            );
        }
    }
    rows.push(ui::prop_row("Пресет", photo.preset_picker.clone()).into_any_element());
    rows.push(ui::prop_row("Колір", ColorSelect::new(&photo.color)).into_any_element());
    rows.push(slider_row("Шорсткість", &photo.roughness, cx).into_any_element());
    rows.push(slider_row("Блиск", &photo.gloss, cx).into_any_element());
    let edited = photo.current().is_some_and(|s| {
        s.preset != s.auto || s.color.is_some() || s.roughness.is_some() || s.gloss.is_some()
    });
    rows.push(
        ui::ghost_button("photo-reset", Some(IconName::Undo), "Скинути до профілю")
            .mx(px(4.))
            .when(!edited, |d| d.opacity(0.5))
            .on_click(cx.listener(|this, _, window, cx| {
                this.edit_photo_slot(cx, SlotMaterial::reset);
                this.refresh_photo_controls(window, cx);
            }))
            .into_any_element(),
    );
    rows.push(ui::divider().mt(px(6.)).into_any_element());
    rows.push(ui::prop_row("Сцена", photo.scene_picker.clone()).into_any_element());
    rows.push(ui::prop_row("Підтримки", photo.support_picker.clone()).into_any_element());
    let travel = photo.travel;
    rows.push(
        div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(32.))
            .px(px(10.))
            .child(div().text_sm().child(crate::i18n::tr("Переміщення")))
            .child(crate::widgets::switch("photo-travel", travel).on_click(cx.listener(
                move |this, _, _, cx| {
                    if let Some(photo) = this.preview.as_mut().and_then(|p| p.photo.as_mut()) {
                        photo.travel = !travel;
                    }
                    this.sync_photo(cx);
                },
            )))
            .into_any_element(),
    );
    if photo.built.is_none() {
        rows.push(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(10.))
                .text_xs()
                .text_color(z.text_muted)
                .child(ui::spinner("photo-building"))
                .child(crate::i18n::tr("Готую валики…"))
                .into_any_element(),
        );
    }
    rows
}

/// Кнопка внизу панелі.
pub fn save_button(ws: &Workspace, cx: &mut Context<Workspace>) -> gpui::AnyElement {
    let ready = ws.renderer.photo_active();
    ui::ghost_button("photo-save", Some(IconName::Download), "Зберегти зображення…")
        .border_1()
        .border_color(zed().border_variant)
        .when(!ready, |d| d.opacity(0.5))
        .on_click(cx.listener(|this, _, window, cx| this.prompt_save_photo(window, cx)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_follow_gcode_profiles_and_edits_reset() {
        let table = MaterialTable::builtin();
        let t = gcode::parse(concat!(
            "; CONFIG_BLOCK_START\n",
            "; filament_colour = #FF0000;#00FF00\n",
            "; filament_settings_id = \"Bambu PLA Silk+ @BBL A1\";\"Bambu PETG HF @BBL A1\"\n",
            "; filament_type = PLA;PETG\n",
            "; CONFIG_BLOCK_END\n",
            "G1 X1 E1\n",
        ))
        .unwrap();
        let mut slots = slots_of(&table, &t);
        assert_eq!(slots.len(), 2);
        assert_eq!(table.presets[slots[0].preset].id, "silk");
        assert_eq!(table.presets[slots[1].preset].id, "petg");
        assert_eq!(slots[0].resolve(&table).color, [1.0, 0.0, 0.0]);
        assert_eq!(slot_label(0, &slots[0], true).as_ref(), "1 · Bambu PLA Silk+");

        let slot = &mut slots[0];
        slot.preset = table.index_of("matte").unwrap();
        slot.color = Some([0.0, 0.0, 1.0]);
        slot.roughness = Some(0.1);
        slot.gloss = Some(0.9);
        let m = slot.resolve(&table);
        assert_eq!((m.color, m.roughness, m.specular), ([0.0, 0.0, 1.0], 0.1, 0.9));
        slot.reset();
        assert_eq!(slot.preset, slot.auto);
        assert_eq!(slot.resolve(&table), table.resolve(slot.auto, [1.0, 0.0, 0.0]));

        // Без конфігу — один слот із типовим пресетом.
        let plain = slots_of(&table, &gcode::parse("G1 X1 E1\n").unwrap());
        assert_eq!(plain.len(), 1);
        assert_eq!(plain[0].preset, table.default);
    }

    /// Регрес: проєкт із двома філаментами «Bambu PLA Marble» (білий і жовтий), пластина
    /// друкується другим (`M620 S1A`). Панель відкривала слот 1 — правка кольору йшла в
    /// матеріал, якого на пластині немає, і модель лишалась жовтою.
    #[test]
    fn panel_edits_the_slot_the_plate_prints_with() {
        let table = MaterialTable::builtin();
        let t = gcode::parse(concat!(
            "; CONFIG_BLOCK_START\n",
            "; filament_colour = #FFFFFF;#F6DA5A\n",
            "; filament_settings_id = \"Bambu PLA Marble @BBL A1\";\"Bambu PLA Marble @BBL A1\"\n",
            "; filament_type = PLA;PLA\n",
            "; CONFIG_BLOCK_END\n",
            "M83\nM620 S1A\nT1\nM621 S1A\nG1 X10 E1\nG1 X20 E1\n",
        ))
        .unwrap();
        let mut slots = slots_of(&table, &t);
        let used = used_slots(&t);
        assert_eq!(used, [1]);
        let slot = initial_slot(&used, slots.len());
        assert_eq!(slot, 1);
        assert_eq!(slot_label(0, &slots[0], false).as_ref(), "1 · Bambu PLA Marble — не на цій пластині");

        // Те, що піде в GPU-буфер для інструмента сегментів, — тепер червоне.
        let red = [0xdb as f32 / 255.0, 0x26 as f32 / 255.0, 0x26 as f32 / 255.0];
        slots[slot].color = Some(red);
        let materials: Vec<_> = slots.iter().map(|s| s.resolve(&table)).collect();
        for seg in t.segments.iter().filter(|s| s.is_extrusion) {
            assert_eq!(materials[usize::from(seg.tool)].color, red);
        }

        // Без екструзії (порожня траєкторія) — перший слот.
        assert_eq!(initial_slot(&[], 2), 0);
        assert_eq!(initial_slot(&[5], 2), 0);
    }

    /// Правки «Фото» переживають заміну прев’ю (інша пластина, нова нарізка) — лише для
    /// того самого філаменту в тому самому слоті.
    #[test]
    fn photo_memory_carries_edits_to_matching_slots() {
        let table = MaterialTable::builtin();
        let config = |colours: &str| {
            gcode::parse(&format!(
                "; CONFIG_BLOCK_START\n; filament_colour = {colours}\n; filament_settings_id = \"Bambu PLA Marble @BBL A1\";\"Bambu PLA Basic @BBL A1\"\n; filament_type = PLA;PLA\n; CONFIG_BLOCK_END\nG1 X1 E1\n"
            ))
            .unwrap()
        };
        let mut old = slots_of(&table, &config("#FFFFFF;#000000"));
        old[0].color = Some([0.8, 0.1, 0.1]);
        old[0].roughness = Some(0.3);
        old[1].preset = table.index_of("silk").unwrap();
        let memory = PhotoMemory {
            slots: old.clone(),
            supports: SupportView::Hidden,
            stage: Stage::Floor,
            travel: false,
            resume: true,
        };
        let mut same = slots_of(&table, &config("#FFFFFF;#000000"));
        memory.apply(&mut same);
        assert_eq!(same, old);
        // Інший колір профілю в слоті 2 — це вже інший філамент: правка не переноситься.
        let mut other = slots_of(&table, &config("#FFFFFF;#00FF00"));
        memory.apply(&mut other);
        assert_eq!(other[0], old[0]);
        assert_eq!(other[1].preset, other[1].auto);
    }
}
