//! Вкладки відкритих проєктів.
//!
//! `Workspace` тримає один активний документ у власних полях — на них спирається весь
//! інтерфейс. Решта відкритих проєктів «припарковані»: їхній стан ([`Doc`]) вийнято з
//! цих полів і складено у вкладку. Перемикання паркує активний документ і повертає в поля
//! стан іншого, тож редагування, історія скасування, вибрані профілі з правками, камера
//! й нарізки пластин переживають перехід між вкладками.
//!
//! Спільне для всіх вкладок: рушій, каталог профілів, принтери, бібліотека, налаштування.
//!
//! Нарізка йде лише в активному документі: рушій один, а її результат застосовується до
//! відкритого документа. Тому, поки триває нарізка, перейти на іншу вкладку чи відкрити
//! ще один файл не можна — спершу її треба дочекатись або зупинити.

use std::path::{Path, PathBuf};

use gpui::{Context, SharedString, Window, div, prelude::*, px};
use gpui_component::tooltip::Tooltip;

use crate::{
    jobs::JobKind,
    preset_editor::PresetEditor,
    theme::zed,
    ui::{self, IconName, NoWindowDrag as _, Toggle as _},
    widgets::ToastKind,
    workspace::{Page, Workspace},
};

/// Стан припаркованого документа: усе, що `Workspace` тримає про відкритий проєкт.
pub(crate) struct Doc {
    path: PathBuf,
    name: SharedString,
    project: Option<project::Project>,
    plates: Vec<viewer::Plate>,
    history: project::history::History,
    edited: bool,
    untitled: bool,
    save_target: Option<PathBuf>,
    active_plate: usize,
    camera: viewer::Camera,
    /// Сторінка проєкту, з якої пішли (`Page::PROJECT_TABS`).
    page: Page,
    selection: slicer::Selection,
    editors: [Option<PresetEditor>; 3],
    /// Нарізки пластин. Чинні, поки той самий `epoch` документа й вхід нарізки.
    plate_slicing: crate::plate_strip::PlateSlicing,
    epoch: u64,
    selected: Option<project::InstanceRef>,
    selected_instances: Vec<project::InstanceRef>,
    collapsed_objects: std::collections::BTreeSet<project::InstanceRef>,
    photo_memory: Option<crate::photo_mode::PhotoMemory>,
}

/// Вкладка: активний документ живе в полях `Workspace`, решта — припарковані.
pub(crate) enum Tab {
    Active,
    Parked(Box<Doc>),
}

/// Що показати на вкладці.
pub(crate) struct TabInfo {
    pub name: SharedString,
    pub path: PathBuf,
    pub edited: bool,
    pub untitled: bool,
    pub active: bool,
}

impl Workspace {
    /// Новий номер документа. Номери не повторюються між вкладками: за ним нарізки й
    /// фонові задачі впізнають свій документ.
    pub(crate) fn fresh_epoch(&mut self) {
        self.epoch_counter += 1;
        self.project_epoch = self.epoch_counter;
    }

    pub(crate) fn tab_infos(&self) -> Vec<TabInfo> {
        self.tabs
            .iter()
            .filter_map(|tab| match tab {
                Tab::Active => Some(TabInfo {
                    name: self.model_name.clone(),
                    path: self.path.clone()?,
                    edited: self.edited,
                    untitled: self.untitled,
                    active: true,
                }),
                Tab::Parked(doc) => Some(TabInfo {
                    name: doc.name.clone(),
                    path: doc.path.clone(),
                    edited: doc.edited,
                    untitled: doc.untitled,
                    active: false,
                }),
            })
            .collect()
    }

    fn active_tab(&self) -> Option<usize> {
        self.tabs.iter().position(|t| matches!(t, Tab::Active))
    }

    /// Вкладка припаркованого документа з цим файлом. Безіменні проєкти файлу не мають.
    pub(crate) fn parked_tab(&self, path: &Path) -> Option<usize> {
        self.tabs
            .iter()
            .position(|t| matches!(t, Tab::Parked(doc) if !doc.untitled && doc.path == path))
    }

    /// Чи можна зараз піти з активного документа; якщо ні — пояснює чому.
    pub(crate) fn can_leave_document(&mut self, cx: &mut Context<Self>) -> bool {
        if self.path.is_some() && self.slicing() {
            self.toast(
                ToastKind::Warning,
                "Іде нарізка цього проєкту — дочекайся її або зупини, щоб перейти до іншого",
                cx,
            );
            return false;
        }
        true
    }

    /// Після зміни активного документа: рівно одна активна вкладка, якщо документ є, і
    /// жодної, якщо його немає. Нова вкладка стає праворуч від останньої активної.
    pub(crate) fn sync_active_tab(&mut self) {
        match (self.active_tab(), self.path.is_some()) {
            (None, true) => {
                let at = self.tab_cursor.min(self.tabs.len());
                self.tabs.insert(at, Tab::Active);
            }
            (Some(index), false) => {
                self.tabs.remove(index);
                self.tab_cursor = index;
            }
            _ => {}
        }
    }

    /// Виймає активний документ із полів `Workspace` у вкладку. Після цього документа в
    /// полях немає: далі або встановлюється новий, або повертається припаркований.
    pub(crate) fn park_active(&mut self) {
        let Some(path) = self.path.take() else {
            return;
        };
        self.finish_scene_gesture();
        // Нарізку сюди не пускає `can_leave_document`; гонка з відкриттям файлу — зупиняємо.
        if self.slicing() {
            self.stop_slice();
        }
        for kind in [JobKind::GcodePreview, JobKind::ToolpathBuild, JobKind::BooleanPreview] {
            self.jobs.cancel(kind);
        }
        self.remember_photo(true);
        let doc = Doc {
            path,
            name: self.model_name.clone(),
            project: self.project.take(),
            plates: std::mem::take(&mut self.plates),
            history: std::mem::take(&mut self.history),
            edited: self.edited,
            untitled: self.untitled,
            save_target: self.save_target.take(),
            active_plate: self.active_plate,
            camera: self.camera.clone(),
            page: if Page::PROJECT_TABS.contains(&self.page) { self.page } else { Page::Prepare },
            selection: self.selection.clone(),
            editors: [
                self.machine_editor.take(),
                self.process_editor.take(),
                self.filament_editor.take(),
            ],
            plate_slicing: std::mem::take(&mut self.plate_slicing),
            epoch: self.project_epoch,
            selected: self.selected.take(),
            selected_instances: std::mem::take(&mut self.selected_instances),
            collapsed_objects: std::mem::take(&mut self.collapsed_objects),
            photo_memory: self.photo_memory.take(),
        };
        self.history_pending = None;
        self.selected_volume = None;
        self.edited = false;
        self.untitled = false;
        self.preview = None;
        self.early_preview = None;
        self.tool = None;
        self.tool_panel = None;
        self.measure = None;
        self.cut_enabled = false;
        self.error = None;
        match self.active_tab() {
            Some(index) => {
                self.tabs[index] = Tab::Parked(Box::new(doc));
                self.tab_cursor = index + 1;
            }
            None => {
                self.tabs.push(Tab::Parked(Box::new(doc)));
                self.tab_cursor = self.tabs.len();
            }
        }
    }

    /// Повертає припаркований документ у поля `Workspace` і показує його сторінку.
    fn unpark(&mut self, index: usize, cx: &mut Context<Self>) {
        let Tab::Parked(doc) = std::mem::replace(&mut self.tabs[index], Tab::Active) else {
            return;
        };
        let doc = *doc;
        self.project_epoch = doc.epoch;
        // Кеші, прив’язані до ревізії геометрії, належали іншому документу.
        self.geometry_revision = self.geometry_revision.wrapping_add(1);
        self.project = doc.project;
        self.history = doc.history;
        self.history_pending = None;
        self.rebuild_instance_bounds();
        self.renderer.set_highlight(None);
        self.model_name = doc.name;
        self.path = Some(doc.path);
        self.untitled = doc.untitled;
        self.save_target = doc.save_target;
        self.plates = doc.plates;
        self.invalidate_plate_bounds();
        self.error = None;
        // Профілі: спершу вибір (списки сумісних), потім редактори з незбереженими правками.
        self.machine_editor = None;
        self.process_editor = None;
        self.filament_editor = None;
        self.set_selection(doc.selection, cx);
        let [machine, process, filament] = doc.editors;
        let keep = |parked: Option<PresetEditor>, rebuilt: &mut Option<PresetEditor>, name: &str| {
            // Профіль міг зникнути з каталогу (інший рушій): тоді лишається перебудований.
            if let Some(editor) = parked.filter(|e| e.preset.parent() == name) {
                *rebuilt = Some(editor);
            }
        };
        self.rebuild_editors();
        let selection = self.selection.clone();
        keep(machine, &mut self.machine_editor, &selection.machine);
        keep(process, &mut self.process_editor, &selection.process);
        keep(filament, &mut self.filament_editor, &selection.filament);
        self.sync_model_color();
        // Нарізки — після профілів: зміна вибору вище не повинна їх зістарити.
        self.plate_slicing = doc.plate_slicing;
        self.preview = None;
        self.early_preview = None;
        self.photo_memory = doc.photo_memory;
        self.collapsed_objects = doc.collapsed_objects;
        self.active_plate = usize::MAX;
        self.show_plate(doc.active_plate.min(self.plates.len().saturating_sub(1)));
        self.selected = doc.selected;
        self.selected_instances = doc.selected_instances;
        self.selected_volume = None;
        self.edited = doc.edited;
        // Сторінку ставимо до `enter_page`: камера збережена в координатах саме цієї
        // сторінки («Перегляд» тримає пластину в початку координат).
        self.page = doc.page;
        self.enter_page(doc.page, cx);
        self.camera = doc.camera;
        self.update_highlight();
        self.dirty = true;
        cx.notify();
    }

    /// Перехід на вкладку. Клік по активній вкладці з «Головної» чи «Пристрою» повертає
    /// до проєкту.
    pub(crate) fn switch_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        match self.tabs.get(index) {
            Some(Tab::Active) => {
                if !Page::PROJECT_TABS.contains(&self.page) {
                    self.enter_page(Page::Prepare, cx);
                }
                cx.notify();
            }
            Some(Tab::Parked(_)) => {
                if !self.can_leave_document(cx) {
                    return;
                }
                self.park_active();
                self.unpark(index, cx);
            }
            None => {}
        }
    }

    /// Наступна (`forward`) чи попередня вкладка по колу.
    pub(crate) fn cycle_tab(&mut self, forward: bool, cx: &mut Context<Self>) {
        let count = self.tabs.len();
        let Some(active) = self.active_tab() else { return };
        if count < 2 {
            return;
        }
        let next = if forward { (active + 1) % count } else { (active + count - 1) % count };
        self.switch_tab(next, cx);
    }

    /// Закриває вкладку. Про незбережені зміни питає діалог «Закрити файл» — для цього
    /// вкладка спершу стає активною.
    pub(crate) fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        match self.tabs.get(index) {
            Some(Tab::Active) => self.close_file(window, cx),
            Some(Tab::Parked(doc)) if doc.edited => {
                self.switch_tab(index, cx);
                if matches!(self.tabs.get(index), Some(Tab::Active)) {
                    self.close_file(window, cx);
                }
            }
            Some(Tab::Parked(_)) => {
                self.tabs.remove(index);
                if let Some(active) = self.active_tab() {
                    self.tab_cursor = active + 1;
                }
                cx.notify();
            }
            None => {}
        }
    }

    /// Активний документ закрито (`reset_document`): його вкладка зникає, а сусідня, якщо
    /// є, стає активною — «Головна» лишається тільки після останньої.
    pub(crate) fn after_document_closed(&mut self, cx: &mut Context<Self>) {
        self.sync_active_tab();
        if self.tabs.is_empty() {
            return;
        }
        let next = self.tab_cursor.min(self.tabs.len() - 1);
        self.unpark(next, cx);
    }

    /// Вкладки відкритих проєктів у заголовку вікна.
    pub(crate) fn tab_strip(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let z = zed();
        let on_project = Page::PROJECT_TABS.contains(&self.page);
        let tabs = self.tab_infos();
        // Однакові назви розрізняє тека файлу.
        let duplicate = |name: &SharedString| tabs.iter().filter(|t| &t.name == name).count() > 1;
        let elements: Vec<_> = tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let shown = tab.active && on_project;
                let folder = (duplicate(&tab.name) && !tab.untitled)
                    .then(|| tab.path.parent().and_then(|p| p.file_name()))
                    .flatten()
                    .map(|f| SharedString::from(f.to_string_lossy().into_owned()));
                let tip: SharedString = if tab.untitled {
                    crate::i18n::tr("Новий проєкт, ще не збережено — правий клік: зберегти чи закрити")
                } else {
                    crate::tr_format!("{} — правий клік: дії з файлом", tab.path.display())
                }
                .into();
                let close_tip: SharedString = crate::i18n::tr("Закрити (Ctrl+W)").into();
                let path = tab.path.clone();
                let group: SharedString = format!("project-tab-{index}").into();
                ui::ghost_button_bare(("project-tab", index), IconName::FileBox)
                    .group(group.clone())
                    .no_window_drag()
                    .flex_shrink(1.)
                    .min_w(px(72.))
                    .max_w(px(220.))
                    .overflow_hidden()
                    .pr(px(2.))
                    .role(gpui::Role::Tab)
                    .aria_label(tab.name.clone())
                    .aria_selected(shown)
                    .toggled(shown)
                    .when(!shown, |d| d.text_color(z.text_muted))
                    .child(div().min_w_0().truncate().child(tab.name.clone()))
                    .children(folder.map(|f| {
                        div()
                            .flex_shrink(1.)
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(z.text_placeholder)
                            .child(f)
                    }))
                    // Позначка змін і хрестик ділять одне місце: хрестик — під курсором.
                    .child(
                        div()
                            .id(("project-tab-close", index))
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(px(16.))
                            .rounded(px(3.))
                            .text_color(z.text_muted)
                            .hover(|s| s.bg(z.element_hover).text_color(z.text))
                            .child(
                                div()
                                    .when(!tab.edited, |d| d.invisible())
                                    .group_hover(group.clone(), |s| s.invisible())
                                    .text_xs()
                                    .child("●"),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .invisible()
                                    .group_hover(group, |s| s.visible())
                                    .child(ui::icon(IconName::X).size(px(12.))),
                            )
                            .tooltip(move |window, cx| Tooltip::new(close_tip.clone()).build(window, cx))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_tab(index, window, cx);
                            })),
                    )
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, _, _, cx| this.switch_tab(index, cx)))
                    .on_mouse_down(
                        gpui::MouseButton::Middle,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_tab(index, window, cx);
                        }),
                    )
                    .on_mouse_down(
                        gpui::MouseButton::Right,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            // Меню файлу діє на активний документ: спершу показуємо вкладку.
                            this.switch_tab(index, cx);
                            if this.path.as_deref() == Some(path.as_path()) {
                                this.open_file_menu(event.position, path.clone(), cx);
                            }
                        }),
                    )
            })
            .collect();
        div()
            .id("project-tabs")
            .role(gpui::Role::TabList)
            .flex()
            .flex_shrink(1.)
            .min_w_0()
            .items_center()
            .gap(px(2.))
            .overflow_hidden()
            .children(elements)
    }
}
