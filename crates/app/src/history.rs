//! Prepare history integration. Selection/camera changes never create entries.
use gpui::{Context, Window, div, prelude::*, px};
use project::history::{Entry, Selection};

use crate::{
    ui::{self, IconName, Toggle as _},
    workspace::{Page, Workspace},
};

impl Workspace {
    pub(crate) fn prompt_rename_plate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::input::{Input, InputState};
        let Some(number) = self
            .plates
            .get(self.active_plate)
            .map(|p| p.number)
            .filter(|n| *n > 0)
        else {
            return;
        };
        let Some(plate) = self
            .project
            .as_ref()
            .and_then(|p| p.plates.iter().find(|p| p.index == number))
        else {
            return;
        };
        let name = cx.new(|cx| InputState::new(window, cx).default_value(plate.name.clone()));
        self.open_modal(
            std::rc::Rc::new(move |_, window, cx| {
                let body = div().p(px(12.)).child(crate::widgets::field(
                    &name,
                    Input::new(&name),
                    window,
                    cx,
                ));
                let name = name.clone();
                let footer =
                    ui::accent_button("rename-plate-save", IconName::Save, "Перейменувати", true)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let value = name.read(cx).value().trim().to_owned();
                            this.checkpoint("Перейменування пластини");
                            let Some(p) = this.project.as_mut() else {
                                return;
                            };
                            match p.rename_plate(number, value) {
                                Ok(()) => {
                                    this.close_modal(cx);
                                    this.project_changed(window, cx);
                                }
                                Err(err) => this.toast(
                                    crate::widgets::ToastKind::Error,
                                    err.to_string(),
                                    cx,
                                ),
                            }
                        }))
                        .into_any_element();
                crate::widgets::modal("Перейменування пластини", 360., body, Some(footer))
            }),
            window,
            cx,
        );
    }

    fn history_selection(&self) -> Selection {
        Selection {
            primary: self.selected,
            instances: self.selected_instances.clone(),
            volume: self.selected_volume,
            plate: self.plates.get(self.active_plate).map(|p| p.number),
        }
    }

    /// Capture BEFORE a mutation. project_changed commits only actual changes.
    pub(crate) fn checkpoint(&mut self, label: &str) {
        self.finish_checkpoint();
        self.history_pending = self.project.as_ref().map(|project| Entry {
            label: label.to_owned(),
            project: project.clone(),
            selection: self.history_selection(),
        });
    }

    pub(crate) fn finish_checkpoint(&mut self) {
        if let Some(before) = self.history_pending.take()
            && let Some(project) = &self.project
        {
            self.history.record(before, project);
        }
    }

    pub(crate) fn restore_history(
        &mut self,
        redo: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.page != Page::Prepare || self.modal.is_some() || self.jobs.is_running(crate::jobs::JobKind::Open) {
            return;
        }
        self.finish_scene_gesture();
        let selection = self.history_selection();
        let Some(project) = self.project.as_mut() else {
            return;
        };
        let restored = if redo {
            self.history.redo(project, selection)
        } else {
            self.history.undo(project, selection)
        };
        let Some(selection) = restored else { return };
        self.selected = selection.primary;
        self.selected_instances = selection.instances;
        self.selected_volume = selection.volume;
        self.gizmo_hot = None;
        self.face_hover = None;
        self.hover_on_selected = false;
        self.sync_model_color();
        self.project_changed(window, cx);
        // Switch the displayed mesh without reframing the camera.
        if let Some(index) = selection
            .plate
            .and_then(|number| self.plates.iter().position(|p| p.number == number))
        {
            self.active_plate = index;
            self.sync_plate_scene();
            self.refresh_modifiers();
            self.sync_selection(window, cx);
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }
}

pub fn buttons(ws: &Workspace, cx: &mut Context<Workspace>) -> impl IntoElement {
    let undo = ws.history.undo_label();
    let redo = ws.history.redo_label();
    let undo_tip = undo.map_or_else(
        || crate::i18n::tr("Скасувати (Ctrl+Z)"),
        |label| crate::tr_format!("Скасувати: {} (Ctrl+Z)", crate::i18n::tr(label)),
    );
    let redo_tip = redo.map_or_else(
        || crate::i18n::tr("Повторити (Ctrl+Shift+Z / Ctrl+Y)"),
        |label| {
            crate::tr_format!(
                "Повторити: {} (Ctrl+Shift+Z / Ctrl+Y)",
                crate::i18n::tr(label)
            )
        },
    );
    div()
        .flex()
        .items_center()
        .gap(px(2.))
        .child(
            ui::icon_button("prepare-undo", IconName::Undo2, undo_tip)
                .inert(undo.is_none())
                .when(undo.is_some(), |d| {
                    d.on_click(
                        cx.listener(|this, _, window, cx| this.restore_history(false, window, cx)),
                    )
                }),
        )
        .child(
            ui::icon_button("prepare-redo", IconName::Redo2, redo_tip)
                .inert(redo.is_none())
                .when(redo.is_some(), |d| {
                    d.on_click(
                        cx.listener(|this, _, window, cx| this.restore_history(true, window, cx)),
                    )
                }),
        )
}
