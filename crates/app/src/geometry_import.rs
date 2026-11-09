//! «Імпортувати лише геометрію», як Import geometry only у Bambu Studio / OrcaSlicer:
//! об’єкти, їх розміщення, пластини й назви лишаються, а налаштування друку беруться з
//! вибраних профілів принтера, філаменту й процесу.
use std::{cell::RefCell, path::PathBuf, rc::Rc};

use gpui::{Context, Window, div, prelude::*, px};
use serde::{Deserialize, Serialize};
use viewer::Plate;

use crate::{
    theme::zed,
    ui::{self, IconName},
    widgets::{self, ToastKind},
    workspace::Workspace,
};

/// Як відкривати 3MF з іншого слайсера або з несумісними налаштуваннями.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenMode {
    /// Питати щоразу.
    #[default]
    Ask,
    /// Відкривати як проєкт з налаштуваннями.
    Project,
    /// Імпортувати лише геометрію.
    Geometry,
}

/// Ключі, що описують сам об’єкт, а не друк: призначення екструдера (колір) і службові.
fn keeps(key: &str) -> bool {
    key == "extruder" || settings::is_metadata_key(key)
}

/// Прибирає з проєкту налаштування друку. Повертає кількість прибраних значень.
pub(crate) fn strip_settings(project: &mut project::Project) -> usize {
    let mut removed = 0;
    let before = project.config.len();
    project.config.retain(|k, _| keeps(k));
    removed += before - project.config.len();
    let ids: Vec<_> = project.objects.iter().map(|o| o.id).collect();
    for id in ids {
        let Ok(object) = project.object(id) else {
            continue;
        };
        let filter = |c: &project::Config| -> project::Config {
            c.iter()
                .filter(|(k, _)| keeps(k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        };
        let config = filter(&object.config);
        removed += object.config.len() - config.len();
        let volumes: Vec<_> = object
            .volumes
            .iter()
            .map(|v| {
                let kept = filter(&v.config);
                (v.config.len() - kept.len(), kept)
            })
            .collect();
        let _ = project.set_object_config(id, config);
        for (index, (count, config)) in volumes.into_iter().enumerate() {
            removed += count;
            let _ = project.set_volume_config(id, index, config);
        }
        if let Some(ranges) = project.object_layer_ranges(id) {
            let mut ranges: Vec<_> = ranges.into_iter().cloned().collect();
            for range in &mut ranges {
                removed += range.config.len();
                range.config.clear();
            }
            let _ = project.set_layer_ranges(id, ranges);
        }
    }
    removed
}

/// Чому варто спитати, як відкривати: файл з іншого слайсера або несумісні значення.
pub(crate) fn open_reasons(ws: &Workspace, project: &project::Project) -> Vec<String> {
    let mut reasons = Vec::new();
    let application = project
        .metadata
        .get("Application")
        .cloned()
        .unwrap_or_default();
    let family = |s: &str| {
        let s = s.to_lowercase();
        if s.contains("orca") {
            Some("orca")
        } else if s.contains("bambu") {
            Some("bambu")
        } else {
            None
        }
    };
    if let (Some(file), Some(engine)) = (
        family(&application),
        ws.engine.as_ref().map(|e| e.kind.name()),
    ) && file != engine
    {
        reasons.push(crate::tr_format!(
            "Файл створено в {}, а нарізатиме {}.",
            application,
            ws.engine_label
        ));
    }
    let (_, report) = crate::migration::report(project, ws.settings_schema);
    let problems = crate::migration::problems(&report).count();
    if problems > 0 {
        reasons.push(crate::tr_format!(
            "Налаштувань, що завадять нарізці: {}.",
            problems
        ));
    }
    reasons
}

impl Workspace {
    /// Відкритий проєкт: питаємо, як його відкрити, лише коли є причина й вибір не запам’ятано.
    pub(crate) fn install_with_open_mode(
        &mut self,
        project: Option<project::Project>,
        plates: Vec<Plate>,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let reasons = project
            .as_ref()
            .filter(|_| self.engine.is_some())
            .map(|p| open_reasons(self, p))
            .unwrap_or_default();
        if reasons.is_empty() {
            return self.install_open_project(project, plates, path, cx);
        }
        match self.preferences.open_3mf {
            OpenMode::Project => self.install_open_project(project, plates, path, cx),
            OpenMode::Geometry => self.install_geometry_only(project, plates, path, cx),
            OpenMode::Ask => self.ask_open_mode(project, plates, path, reasons, window, cx),
        }
    }

    fn install_geometry_only(
        &mut self,
        project: Option<project::Project>,
        plates: Vec<Plate>,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let selection = self.selection.clone();
        let project = project.map(|mut p| {
            strip_settings(&mut p);
            p
        });
        self.install_open_project(project, plates, path, cx);
        // Профілі з файлу не беремо: лишаються вибрані до відкриття.
        if self.selection != selection {
            self.set_selection(selection, cx);
        }
        self.machine_editor = None;
        self.process_editor = None;
        self.filament_editor = None;
        self.rebuild_editors();
        self.edited = true;
        self.toast(
            ToastKind::Info,
            crate::tr_format!(
                "Імпортовано лише геометрію. Налаштування — з профілів {} · {} · {}",
                self.selection.machine,
                self.selection.filament,
                self.selection.process
            ),
            cx,
        );
        cx.notify();
    }

    fn ask_open_mode(
        &mut self,
        project: Option<project::Project>,
        plates: Vec<Plate>,
        path: PathBuf,
        reasons: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pending = Rc::new(RefCell::new(Some((project, plates, path.clone()))));
        let remember = Rc::new(std::cell::Cell::new(false));
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        self.open_modal(
            Rc::new(move |ws, _, cx| {
                let z = zed();
                let option = |id: &'static str,
                              icon: IconName,
                              title: &str,
                              text: String,
                              geometry: bool| {
                    let pending = pending.clone();
                    let remember = remember.clone();
                    div()
                        .id(id)
                        .flex()
                        .gap(px(10.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(z.border)
                        .bg(z.element)
                        .cursor_pointer()
                        .hover(|s| s.border_color(z.border_focused).bg(z.element_hover))
                        .child(ui::icon(icon).text_color(z.accent).mt(px(2.)))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .gap(px(2.))
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .child(crate::i18n::tr(title)),
                                )
                                .child(div().w_full().text_xs().text_color(z.text_muted).child(text)),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let Some((project, plates, path)) = pending.borrow_mut().take() else {
                                return;
                            };
                            this.close_modal(cx);
                            if remember.get() {
                                this.preferences.open_3mf = if geometry {
                                    OpenMode::Geometry
                                } else {
                                    OpenMode::Project
                                };
                                this.save_preferences(cx);
                            }
                            if geometry {
                                this.install_geometry_only(project, plates, path, cx);
                            } else {
                                this.install_open_project(project, plates, path, cx);
                            }
                        }))
                };
                let profiles = format!(
                    "{} · {} · {}",
                    ws.selection.machine, ws.selection.filament, ws.selection.process
                );
                let checked = remember.get();
                let toggle = remember.clone();
                let body = div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .p(px(14.))
                    .child(div().text_sm().child(name.clone()))
                    .children(
                        reasons
                            .iter()
                            .map(|r| div().text_xs().text_color(z.text_muted).child(r.clone())),
                    )
                    .child(option(
                        "open-mode-project",
                        IconName::FileBox,
                        "Відкрити як проєкт (з налаштуваннями)",
                        crate::i18n::tr(
                            "Усе з файлу: об’єкти, пластини й налаштування друку. Несумісні значення можна виправити кнопкою «Перевірити сумісність».",
                        ),
                        false,
                    ))
                    .child(option(
                        "open-mode-geometry",
                        IconName::Box,
                        "Імпортувати лише геометрію",
                        crate::tr_format!(
                            "Об’єкти, їх розміщення, пластини й назви. Налаштування друку — з вибраних профілів: {}.",
                            profiles
                        ),
                        true,
                    ))
                    .child(
                        widgets::checkbox("open-mode-remember", checked, false)
                            .child(crate::i18n::tr("Запам’ятати вибір (змінити можна в налаштуваннях → «Рушій нарізки»)"))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                toggle.set(!toggle.get());
                                cx.notify();
                            })),
                    );
                widgets::modal("Як відкрити цей 3MF?", 520., body, None)
            }),
            window,
            cx,
        );
    }

    /// Прибрати з відкритого проєкту налаштування друку (картка помилки, діалог сумісності).
    pub(crate) fn keep_geometry_only(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.slicing() {
            return;
        }
        let Some(mut project) = self.project.clone() else {
            return;
        };
        strip_settings(&mut project);
        self.checkpoint("Імпорт лише геометрії");
        self.project = Some(project);
        self.close_modal(cx);
        self.machine_editor = None;
        self.process_editor = None;
        self.filament_editor = None;
        self.rebuild_editors();
        self.project_changed(window, cx);
        self.preview = None;
        self.toast(
            ToastKind::Success,
            crate::tr_format!(
                "Лишилась тільки геометрія. Налаштування — з профілів {} · {} · {}. Файл зміниться після збереження",
                self.selection.machine,
                self.selection.filament,
                self.selection.process
            ),
            cx,
        );
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_print_settings_but_keeps_objects_and_extruders() {
        let mut p = project::Project::default();
        p.config.insert("layer_height".into(), "0.08".into());
        p.config.insert("name".into(), "project_settings".into());
        let mut object = project::Object::default();
        object.id = 1;
        object.name = "Spider".into();
        object.config.insert("wall_loops".into(), "4".into());
        object.config.insert("extruder".into(), "2".into());
        p.objects.push(object);
        let objects = p.objects.clone();
        let removed = strip_settings(&mut p);
        assert_eq!(removed, 2);
        assert!(!p.config.contains_key("layer_height"));
        assert!(p.config.contains_key("name"));
        assert_eq!(p.objects[0].name, objects[0].name);
        assert_eq!(
            p.objects[0].config.get("extruder").map(String::as_str),
            Some("2")
        );
        assert!(!p.objects[0].config.contains_key("wall_loops"));
    }

    #[test]
    fn open_mode_round_trips_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&OpenMode::Geometry).unwrap(),
            "\"geometry\""
        );
        assert_eq!(
            serde_json::from_str::<OpenMode>("\"project\"").unwrap(),
            OpenMode::Project
        );
    }
}
