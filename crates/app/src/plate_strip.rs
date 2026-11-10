//! Пластини на «Перегляді»: результат нарізки кожної пластини (за номером), черга
//! «Нарізати всі», мініатюри й колонка вибору пластини ліворуч поверх сцени.
//!
//! Нарізка пластини чинна, доки не змінились: документ, рушій, спільний вхід нарізки
//! (профілі, значення редакторів — `PlateSlicing::settings`) і вміст самої пластини
//! (відбиток її об’єктів, `plate_content`). Правка однієї пластини не робить застарілими інші.

use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::Arc,
};

use gpui::{Context, Image, ImageFormat, ObjectFit, SharedString, Task, div, img, prelude::*, px};
use gpui_component::tooltip::Tooltip;

use crate::{
    jobs::JobKind,
    theme::{MONO_FONT, zed},
    ui::{self, IconName},
    widgets::ToastKind,
    workspace::{Page, SliceState, Workspace},
};

/// Від чого залежить G-code пластини.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct SliceStamp {
    epoch: u64,
    engine: u64,
    settings: u64,
    content: u64,
}

/// Результат останньої нарізки пластини: `Done` або `Failed`.
pub struct PlateSlice {
    state: SliceState,
    exhausted: bool,
    stamp: SliceStamp,
}

#[derive(Default)]
pub struct PlateSlicing {
    results: HashMap<u32, PlateSlice>,
    /// Зростає зі зміною входу нарізки, спільного для всіх пластин.
    settings: u64,
    /// Правки геометрії пластини (меш міг змінитися на місці, відбиток цього не бачить).
    edits: HashMap<u32, u64>,
    /// Відбитки пластин для `(project_epoch, geometry_revision)`.
    content: RefCell<Option<((u64, u64), HashMap<u32, u64>)>>,
    queue: VecDeque<u32>,
    /// «Нарізати всі»: скільки пластин було в черзі на старті; `None` — одна пластина.
    batch: Option<usize>,
    batch_failed: usize,
    /// Пластина, яку зараз ріже рушій.
    current: Option<u32>,
    /// Мініатюри: ключ (документ, вміст, колір) і зображення (`None` — пластина порожня).
    thumbs: HashMap<u32, (u64, Option<Arc<Image>>)>,
    thumbs_task: Option<Task<()>>,
    /// Прокрутка колонки: переживає перебудову сцени при зміні пластини.
    scroll: gpui::ScrollHandle,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PlateStatus {
    Unsliced,
    Queued,
    Slicing(Option<u8>),
    Sliced,
    Stale,
    Failed,
}

/// Відбиток вмісту пластини: об’єкти на ній (трансформації, частини, налаштування, діапазони
/// шарів) і її власні налаштування. Сітки — за ідентичністю `Arc` і розміром.
fn plate_content(project: &project::Project, number: u32) -> u64 {
    fn affine(a: &project::DAffine3, h: &mut impl Hasher) {
        for v in a.to_cols_array() {
            v.to_bits().hash(h);
        }
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    number.hash(&mut h);
    if let Some(plate) = project.plates.iter().find(|p| p.index == number) {
        plate.bed_type.hash(&mut h);
        format!("{:?}", plate.metadata).hash(&mut h);
    }
    for object in &project.objects {
        let mut on = object
            .instances
            .iter()
            .enumerate()
            .filter(|(_, i)| i.plate == Some(number))
            .peekable();
        if on.peek().is_none() {
            continue;
        }
        object.id.hash(&mut h);
        object.config.hash(&mut h);
        format!("{:?}", project.layer_ranges_for_object(object.id)).hash(&mut h);
        for (index, instance) in on {
            index.hash(&mut h);
            affine(&instance.transform, &mut h);
            instance.printable.hash(&mut h);
        }
        for volume in &object.volumes {
            (Arc::as_ptr(&volume.mesh) as usize).hash(&mut h);
            volume.mesh.positions.len().hash(&mut h);
            volume.mesh.triangles.len().hash(&mut h);
            affine(&volume.transform, &mut h);
            std::mem::discriminant(&volume.kind).hash(&mut h);
            volume.config.hash(&mut h);
        }
    }
    h.finish()
}

/// Сторона мініатюри: парна, щоб не з’їжджала на масштабах 125/150 %.
fn thumb_size(count: usize) -> f32 {
    if count > 8 { 56. } else { 64. }
}

const STRIP_PAD: f32 = 6.;

impl Workspace {
    fn plate_content_of(&self, number: u32) -> u64 {
        let Some(project) = &self.project else { return 0 };
        let key = (self.project_epoch, self.geometry_revision);
        let mut memo = self.plate_slicing.content.borrow_mut();
        if memo.as_ref().is_none_or(|(k, _)| *k != key) {
            *memo = Some((key, HashMap::new()));
        }
        let map = &mut memo.as_mut().expect("memo is filled above").1;
        let content = *map
            .entry(number)
            .or_insert_with(|| plate_content(project, number));
        let edits = self.plate_slicing.edits.get(&number).copied().unwrap_or(0);
        content ^ edits.wrapping_mul(0x9E37_79B9_7F4A_7C15)
    }

    pub(crate) fn slice_stamp(&self, number: u32) -> SliceStamp {
        SliceStamp {
            epoch: self.project_epoch,
            engine: self.engine_revision,
            settings: self.plate_slicing.settings,
            content: self.plate_content_of(number),
        }
    }

    fn plate_result(&self, number: u32) -> Option<&PlateSlice> {
        self.plate_slicing
            .results
            .get(&number)
            .filter(|r| r.stamp.epoch == self.project_epoch)
    }

    pub(crate) fn plate_status(&self, number: u32) -> PlateStatus {
        if self.plate_slicing.current == Some(number) && self.slicing() {
            let percent = self
                .jobs
                .get(JobKind::Slice)
                .and_then(|j| j.progress.as_ref())
                .and_then(|p| p());
            return PlateStatus::Slicing(percent);
        }
        if self.plate_slicing.queue.contains(&number) {
            return PlateStatus::Queued;
        }
        let Some(result) = self.plate_result(number) else {
            return PlateStatus::Unsliced;
        };
        let fresh = result.stamp == self.slice_stamp(number);
        match (&result.state, fresh) {
            (SliceState::Done(_), true) => PlateStatus::Sliced,
            (SliceState::Done(_), false) => PlateStatus::Stale,
            (SliceState::Failed(..), true) => PlateStatus::Failed,
            _ => PlateStatus::Unsliced,
        }
    }

    /// Нарізка показаної пластини застаріла: Preview показує старий G-code з карткою.
    pub(crate) fn active_slice_stale(&self) -> bool {
        self.plate_to_slice()
            .is_some_and(|n| self.plate_status(n) == PlateStatus::Stale)
    }

    /// G-code останньої вдалої нарізки пластини, зокрема застарілої.
    pub(crate) fn plate_gcode(&self, number: u32) -> Option<PathBuf> {
        match &self.plate_result(number)?.state {
            SliceState::Done(out) => Some(out.gcode_3mf.with_file_name(format!("plate_{number}.gcode"))),
            _ => None,
        }
    }

    /// Стан нарізки показаної пластини з її чинного результату (інакше — нічого).
    pub(crate) fn restore_slice_state(&mut self) {
        let fresh = self.plate_to_slice().and_then(|n| {
            self.plate_result(n)
                .filter(|r| r.stamp == self.slice_stamp(n))
                .map(|r| (r.state.clone(), r.exhausted))
        });
        let (state, exhausted) = fresh.unwrap_or((SliceState::Idle, false));
        self.slice_exhausted = exhausted;
        self.slice_fallback = if exhausted { self.crash_fallback() } else { None };
        self.slice_state = state;
    }

    pub(crate) fn record_plate_slice(
        &mut self,
        number: u32,
        state: SliceState,
        exhausted: bool,
        stamp: SliceStamp,
    ) {
        if matches!(state, SliceState::Failed(..)) && self.plate_slicing.batch.is_some() {
            self.plate_slicing.batch_failed += 1;
        }
        self.plate_slicing
            .results
            .retain(|_, r| r.stamp.epoch == stamp.epoch);
        self.plate_slicing.results.insert(
            number,
            PlateSlice {
                state,
                exhausted,
                stamp,
            },
        );
    }

    /// Пластина, яку ріже рушій; `None` — нарізка скінчилась.
    pub(crate) fn set_slicing_plate(&mut self, number: Option<u32>) {
        self.plate_slicing.current = number;
    }

    /// Змінився вхід нарізки, спільний для всіх пластин: усі нарізки застаріли.
    pub(crate) fn bump_slice_settings(&mut self) {
        self.plate_slicing.settings = self.plate_slicing.settings.wrapping_add(1);
    }

    /// Змінилась геометрія показаної пластини.
    pub(crate) fn note_plate_edit(&mut self) {
        if let Some(number) = self.plates.get(self.active_plate).map(|p| p.number) {
            *self.plate_slicing.edits.entry(number).or_default() += 1;
        }
    }

    /// Черга «Нарізати всі» скасовується разом з нарізкою.
    pub(crate) fn clear_slice_queue(&mut self) {
        self.plate_slicing.queue.clear();
        self.plate_slicing.batch = None;
        self.plate_slicing.batch_failed = 0;
        self.plate_slicing.current = None;
    }

    /// Пластина, яку зараз ріже рушій.
    pub(crate) fn slicing_plate(&self) -> Option<u32> {
        self.plate_slicing.current.filter(|_| self.slicing())
    }

    pub(crate) fn slice_batch_running(&self) -> bool {
        self.plate_slicing.batch.is_some()
    }

    /// «Нарізати всі»: черга з пластин без чинної нарізки, показана — першою.
    pub(crate) fn slice_all(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if self.slicing() || !self.can_slice() {
            return;
        }
        let mut queue: VecDeque<u32> = self
            .plates
            .iter()
            .map(|p| p.number)
            .filter(|n| *n > 0 && self.plate_status(*n) != PlateStatus::Sliced)
            .collect();
        if let Some(active) = self.plate_to_slice()
            && let Some(i) = queue.iter().position(|n| *n == active)
        {
            queue.remove(i);
            queue.push_front(active);
        }
        if queue.is_empty() {
            self.toast(ToastKind::Info, crate::i18n::tr("Усі пластини вже нарізано"), cx);
            return;
        }
        self.plate_slicing.batch = Some(queue.len());
        self.plate_slicing.batch_failed = 0;
        self.plate_slicing.queue = queue;
        self.continue_slice_queue(window, cx);
    }

    /// Наступна пластина черги; черга скінчилась — підсумок «Нарізати всі».
    pub(crate) fn continue_slice_queue(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        while let Some(next) = self.plate_slicing.queue.pop_front() {
            if self.start_plate_slice(next, window, cx) {
                return;
            }
        }
        if let Some(total) = self.plate_slicing.batch.take() {
            let failed = std::mem::take(&mut self.plate_slicing.batch_failed);
            if failed == 0 {
                self.toast(ToastKind::Success, crate::tr_format!("Нарізано пластин: {}", total), cx);
            } else {
                self.toast(
                    ToastKind::Warning,
                    crate::tr_format!("Нарізано пластин: {} з {}", total - failed.min(total), total),
                    cx,
                );
            }
        }
    }

    /// Вибір пластини в колонці «Перегляду»: та сама активна пластина, що й на «Підготовці».
    pub(crate) fn select_plate(&mut self, index: usize, cx: &mut Context<Self>) {
        if index == self.active_plate {
            return;
        }
        self.show_plate(index);
        if let Some(position) = self
            .plates
            .iter()
            .filter(|p| p.number > 0)
            .position(|p| p.number == self.plates[index].number)
        {
            self.plate_slicing.scroll.scroll_to_item(position);
        }
        if self.page == Page::Preview {
            self.enter_page(Page::Preview, cx);
        }
        cx.notify();
    }

    /// Мініатюри пластин у фоні — лише для пластин, чий вміст змінився відтоді.
    fn ensure_plate_thumbs(&mut self, cx: &mut Context<Self>) {
        if self.plate_slicing.thumbs_task.is_some() {
            return;
        }
        let Some(project) = &self.project else { return };
        let color = self.model_color();
        let wanted: Vec<(u32, u64)> = self
            .plates
            .iter()
            .map(|p| p.number)
            .filter(|n| *n > 0)
            .map(|n| {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                (self.project_epoch, self.plate_content_of(n)).hash(&mut h);
                color.map(f32::to_bits).hash(&mut h);
                (n, h.finish())
            })
            .filter(|(n, key)| self.plate_slicing.thumbs.get(n).map(|t| t.0) != Some(*key))
            .collect();
        if wanted.is_empty() {
            return;
        }
        let project = project.clone();
        let task = cx.spawn(async move |this, cx| {
            let images = cx
                .background_executor()
                .spawn(async move {
                    wanted
                        .into_iter()
                        .map(|(number, key)| {
                            let image = project
                                .plate_mesh_with_kinds(Some(number), &[project::VolumeKind::Part])
                                .ok()
                                .and_then(|soup| {
                                    let mesh = viewer::Mesh {
                                        positions: soup.positions,
                                        normals: soup.normals,
                                    };
                                    viewer::mesh_image(&mesh, 128, Some(color))
                                })
                                .map(|png| Arc::new(Image::from_bytes(ImageFormat::Png, png)));
                            (number, key, image)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            this.update(cx, |this, cx| {
                for (number, key, image) in images {
                    this.plate_slicing.thumbs.insert(number, (key, image));
                }
                this.plate_slicing.thumbs_task = None;
                cx.notify();
            })
            .ok();
        });
        self.plate_slicing.thumbs_task = Some(task);
    }
}

fn status_text(status: PlateStatus) -> String {
    match status {
        PlateStatus::Unsliced => crate::i18n::tr("Не нарізано"),
        PlateStatus::Queued => crate::i18n::tr("У черзі на нарізку"),
        PlateStatus::Slicing(Some(p)) => crate::tr_format!("Нарізається: {} %", p),
        PlateStatus::Slicing(None) => crate::i18n::tr("Нарізається…"),
        PlateStatus::Sliced => crate::i18n::tr("Нарізано"),
        PlateStatus::Stale => crate::i18n::tr("Нарізка застаріла"),
        PlateStatus::Failed => crate::i18n::tr("Нарізка не вдалася"),
    }
}

/// Значок стану в правому нижньому куті мініатюри.
fn status_badge(status: PlateStatus, id: SharedString) -> gpui::AnyElement {
    let z = zed();
    let badge = div()
        .absolute()
        .bottom(px(2.))
        .right(px(2.))
        .flex()
        .items_center()
        .justify_center()
        .min_w(px(16.))
        .h(px(16.))
        .px(px(2.))
        .rounded(px(8.))
        .bg(z.panel.opacity(0.9));
    let icon = |name: IconName, color: gpui::Hsla| ui::icon(name).size(px(12.)).text_color(color);
    match status {
        PlateStatus::Unsliced => badge.child(icon(IconName::Scissors, z.text_muted)),
        PlateStatus::Queued => badge.child(icon(IconName::Clock, z.text_muted)),
        PlateStatus::Slicing(Some(p)) => badge
            .px(px(4.))
            .font_family(MONO_FONT)
            .text_size(px(10.))
            .text_color(z.accent)
            .child(format!("{p}%")),
        PlateStatus::Slicing(None) => badge.child(ui::spinner(id)),
        PlateStatus::Sliced => badge.child(icon(IconName::CircleCheck, z.success)),
        PlateStatus::Stale => badge.child(div().size(px(8.)).rounded_full().bg(z.warning)),
        PlateStatus::Failed => badge.child(icon(IconName::CircleX, z.error)),
    }
    .into_any_element()
}

/// Ширина колонки пластин, якщо вона показана (для карток, що стоять поруч).
pub(crate) fn strip_width(ws: &Workspace) -> Option<f32> {
    let count = ws.plates.iter().filter(|p| p.number > 0).count();
    (count > 1 && ws.path.is_some()).then(|| thumb_size(count) + 2. * STRIP_PAD + 2.)
}

/// Колонка пластин ліворуч у 3D-області «Перегляду»: «Нарізати всі» з прогресом і мініатюри
/// зі станом нарізки. Одна пластина — колонки немає.
pub(crate) fn strip(ws: &mut Workspace, cx: &mut Context<Workspace>) -> Option<gpui::AnyElement> {
    let width = strip_width(ws)?;
    ws.ensure_plate_thumbs(cx);
    let z = zed();
    let plates: Vec<(usize, u32, Option<String>)> = ws
        .plates
        .iter()
        .enumerate()
        .filter(|(_, p)| p.number > 0)
        .map(|(i, p)| (i, p.number, p.name.clone()))
        .collect();
    let total = plates.len();
    let size = thumb_size(total);
    let statuses: Vec<PlateStatus> = plates.iter().map(|(_, n, _)| ws.plate_status(*n)).collect();
    let sliced = statuses.iter().filter(|s| **s == PlateStatus::Sliced).count();
    let batch = ws.slice_batch_running();
    let can = ws.can_slice() || batch;

    // «Нарізати всі»: N/M нарізаних, під час черги — загальний прогрес і скасування.
    let current = statuses.iter().find_map(|s| match s {
        PlateStatus::Slicing(p) => Some(f32::from(p.unwrap_or(0)) / 100.),
        _ => None,
    });
    let overall = (sliced as f32 + current.unwrap_or(0.)) / total as f32 * 100.;
    let tip: SharedString = crate::i18n::tr(if batch {
        "Скасувати нарізку всіх пластин"
    } else {
        "Нарізати всі пластини"
    })
    .into();
    let header = div()
        .id("plate-slice-all")
        .flex()
        .flex_none()
        .flex_col()
        .items_center()
        .gap(px(3.))
        .py(px(5.))
        .px(px(2.))
        .rounded(px(6.))
        .text_xs()
        .when(can, |d| d.cursor_pointer().hover(|s| s.bg(z.element_hover)))
        .when(!can, |d| d.opacity(0.5))
        .child(if batch {
            ui::icon(IconName::X).text_color(z.text_muted).into_any_element()
        } else {
            ui::icon(IconName::Scissors).text_color(z.accent).into_any_element()
        })
        // Місце підпису однакової висоти: під час черги в ньому смуга загального прогресу
        // (підпис скасування в колонку не влазить), і мініатюри під заголовком не зсуваються.
        .child(
            div()
                .w_full()
                .h(px(32.))
                .flex()
                .items_center()
                .justify_center()
                .text_center()
                .whitespace_normal()
                .text_color(z.text)
                .map(|d| {
                    if batch {
                        d.child(div().w_full().child(crate::widgets::progress(overall)))
                    } else {
                        d.child(div().w_full().child(crate::i18n::tr("Нарізати всі")))
                    }
                }),
        )
        .child(
            div()
                .font_family(MONO_FONT)
                .text_color(z.text_muted)
                .child(format!("{sliced}/{total}")),
        )
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .when(can, |d| {
            d.on_click(cx.listener(|this, _, window, cx| {
                if this.slice_batch_running() {
                    this.stop_slice();
                } else {
                    this.slice_all(window, cx);
                }
                cx.notify();
            }))
        });

    let active = ws.active_plate;
    let items = plates.into_iter().zip(statuses).map(|((index, number, name), status)| {
        let selected = index == active;
        let thumb = ws.plate_slicing.thumbs.get(&number).and_then(|t| t.1.clone());
        let title = match &name {
            Some(name) => crate::tr_format!("Пластина {}: {}", number, name),
            None => crate::tr_format!("Пластина {}", number),
        };
        let tip: SharedString = format!("{title}\n{}", status_text(status)).into();
        div()
            .id(SharedString::from(format!("plate-strip-{number}")))
            .relative()
            .flex_none()
            .size(px(size))
            .rounded(px(6.))
            .border_2()
            .border_color(if selected { z.accent } else { z.border_variant })
            .bg(if selected { z.accent.opacity(0.14) } else { z.editor.opacity(0.6) })
            .cursor_pointer()
            .when(!selected, |d| d.hover(|s| s.bg(z.element_hover)))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .p(px(4.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(match thumb {
                        Some(image) => img(image).size_full().object_fit(ObjectFit::Contain).into_any_element(),
                        None => ui::icon(IconName::Box).text_color(z.text_muted.opacity(0.5)).into_any_element(),
                    }),
            )
            .child(
                div()
                    .absolute()
                    .top(px(1.))
                    .left(px(4.))
                    .font_family(MONO_FONT)
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(if selected { z.accent } else { z.text })
                    .child(number.to_string()),
            )
            .when_some(
                match status {
                    PlateStatus::Slicing(Some(p)) => Some(f32::from(p)),
                    _ => None,
                },
                |d, p| {
                    d.child(
                        div()
                            .absolute()
                            .left(px(4.))
                            .right(px(22.))
                            .bottom(px(8.))
                            .child(crate::widgets::progress(p)),
                    )
                },
            )
            .child(status_badge(status, format!("plate-strip-spin-{number}").into()))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _, _, cx| this.select_plate(index, cx)))
    });

    Some(
        div()
            .absolute()
            .top(px(ws.project_bar.below()))
            .left(px(12.))
            .bottom(px(84.))
            .w(px(width))
            .flex()
            .flex_col()
            .child(
                ui::scene_control("plate-strip")
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .max_h_full()
                    .p(px(STRIP_PAD))
                    .gap(px(6.))
                    .rounded(px(8.))
                    .bg(z.panel.opacity(0.92))
                    .border_1()
                    .border_color(z.border_variant)
                    .shadow_sm()
                    .child(header)
                    .child(div().flex_none().h(px(1.)).bg(z.border_variant))
                    .child(
                        div()
                            .id("plate-strip-list")
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&ws.plate_slicing.scroll)
                            .children(items),
                    ),
            )
            .into_any_element(),
    )
}
