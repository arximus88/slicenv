//! GPUI owns the project. HTTP workers only send jobs and await replies.
//!
//! Легкі інструменти виконуються на UI-потоці. Важкі (`orient_plate`, `arrange_plate`,
//! `slice_plate`) — фоновими задачами реєстру [`crate::jobs`]: на UI-потоці лише знімок
//! і застосування з тією ж перевіркою застарілості, що й у GUI; відповідь чекає завершення.
use crate::{
    jobs::{JobKind, Progress},
    workspace::Workspace,
};
use anyhow::{Result, bail, ensure};
use gpui::{Context, Window};
use mcp::{
    host::{Host, Operation},
    session::{Session, SliceArgs, SliceJob},
};
use serde_json::Value;
use std::sync::{Arc, mpsc};

enum Job {
    Call(&'static str, Operation, mpsc::Sender<Result<Value>>),
    /// Редагування над знімком у фоні.
    Background(&'static str, Operation, mpsc::Sender<Result<Value>>),
    Slice(SliceArgs, mpsc::Sender<Result<Value>>),
    Snapshot(mpsc::Sender<Result<Session>>),
    UiState(mpsc::Sender<Value>),
}
/// Редагувальні інструменти, що рахують усю пластину: не блокують UI.
const BACKGROUND_TOOLS: &[&str] = &["orient_plate", "arrange_plate"];

/// Відповідь UI-потоку. Закритий канал — задачу скасовано, замінено або її результат
/// застарів (проєкт змінився під час роботи) і не застосовувався.
fn wait(rx: mpsc::Receiver<Result<Value>>) -> Result<Value> {
    rx.recv().unwrap_or_else(|_| {
        bail!("операцію не застосовано: проєкт змінено під час роботи або її скасовано")
    })
}
struct Bridge(async_channel::Sender<Job>);
impl Bridge {
    fn send(&self, job: Job) -> Result<()> {
        self.0
            .send_blocking(job)
            .map_err(|_| anyhow::anyhow!("MCP зупинено"))
    }
}
impl Host for Bridge {
    fn open_is_edit(&self) -> bool {
        true
    }
    fn has_ui(&self) -> bool {
        true
    }
    fn ui_state(&self) -> Result<Value> {
        let (tx, rx) = mpsc::channel();
        self.0
            .send_blocking(Job::UiState(tx))
            .map_err(|_| anyhow::anyhow!("MCP зупинено"))?;
        Ok(rx.recv()?)
    }
    fn call(&self, name: &'static str, op: Operation) -> Result<Value> {
        if [
            "list_profiles",
            "get_profile",
            "explain_setting",
            "gcode_stats",
            "printer_status",
        ]
        .contains(&name)
        {
            let (tx, rx) = mpsc::channel();
            self.0
                .send_blocking(Job::Snapshot(tx))
                .map_err(|_| anyhow::anyhow!("MCP зупинено"))?;
            let mut snapshot = rx.recv()??;
            return op(&mut snapshot);
        }
        let (tx, rx) = mpsc::channel();
        self.send(if BACKGROUND_TOOLS.contains(&name) {
            Job::Background(name, op, tx)
        } else {
            Job::Call(name, op, tx)
        })?;
        wait(rx)
    }
    fn slice(&self, args: SliceArgs) -> Result<Value> {
        let (tx, rx) = mpsc::channel();
        self.send(Job::Slice(args, tx))?;
        wait(rx)
    }
    fn prepare_slice(&self, args: SliceArgs) -> Result<SliceJob> {
        let (tx, rx) = mpsc::channel();
        self.0
            .send_blocking(Job::Snapshot(tx))
            .map_err(|_| anyhow::anyhow!("MCP зупинено"))?;
        rx.recv()??.prepare_slice(args)
    }
}
#[derive(Default)]
pub struct State {
    pub server: Option<mcp::http::HttpServer>,
    pub task: Option<gpui::Task<()>>,
    pub token: String,
    pub error: Option<String>,
}
impl Workspace {
    pub(crate) fn restart_mcp(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Drop the receiver too: queued callers receive an error on shutdown.
        self.mcp.server = None;
        self.mcp.task = None;
        self.mcp.error = None;
        let path = crate::preferences::Preferences::path().with_file_name("mcp-token");
        match mcp::http::token(&path, false) {
            Ok(token) => self.mcp.token = token,
            Err(err) => {
                self.mcp.error = Some(err.to_string());
                cx.notify();
                return;
            }
        }
        if !self.preferences.mcp_enabled {
            cx.notify();
            return;
        }
        let (tx, rx) = async_channel::bounded(32);
        match mcp::http::start(
            self.preferences.mcp_port,
            self.mcp.token.clone(),
            self.preferences.mcp_allow_edit,
            Arc::new(Bridge(tx)),
        ) {
            Ok(server) => self.mcp.server = Some(server),
            Err(err) => {
                self.mcp.error = Some(err.to_string());
                cx.notify();
                return;
            }
        }
        self.mcp.task = Some(cx.spawn_in(window, async move |this, cx| {
            while let Ok(job) = rx.recv().await {
                if this
                    .update_in(cx, |ws, window, cx| match job {
                        Job::Call(name, op, reply) => {
                            let _ = reply.send(ws.mcp_call(name, op, window, cx));
                        }
                        Job::Background(name, op, reply) => ws.mcp_background(name, op, reply, cx),
                        Job::Slice(args, reply) => ws.mcp_slice(args, reply, cx),
                        Job::Snapshot(reply) => {
                            let _ = reply.send(ws.mcp_snapshot());
                        }
                        Job::UiState(reply) => {
                            let _ = reply.send(ws.ui_state(cx));
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
        cx.notify();
    }
    fn mcp_snapshot(&self) -> Result<Session> {
        let Some(project) = self.project.clone() else {
            return Ok(Session::new(self.preferences.mcp_allow_edit));
        };
        Ok(Session::from_project(
            project,
            self.path.clone().unwrap_or_default(),
            self.edited,
            self.preferences.mcp_allow_edit,
        ))
    }
    /// Чи можна зараз виконати інструмент `name` (для редагувальних — без жестів у сцені).
    fn mcp_check(&self, name: &str) -> Result<bool> {
        let edit = mcp::session::EDIT_TOOLS.contains(&name) || name == "open_project";
        ensure!(
            !edit || self.preferences.mcp_allow_edit,
            "редагування AI-агентами вимкнено"
        );
        ensure!(
            !edit
                || (!self.jobs.is_running(JobKind::Open)
                    && self.scrub.is_none()
                    && self.gizmo_drag.is_none()
                    && self.live_drag.is_none()),
            "заверши поточну дію перед редагуванням MCP"
        );
        Ok(edit)
    }

    /// Важке редагування: знімок → `op` у фоні → застосування, якщо документ не змінився.
    /// Клон на UI-потоці один (меші спільні через `Arc`); порівняння «до/після» — у фоні.
    fn mcp_background(
        &mut self,
        name: &'static str,
        op: Operation,
        reply: mpsc::Sender<Result<Value>>,
        cx: &mut Context<Self>,
    ) {
        if let Err(err) = self.mcp_check(name) {
            let _ = reply.send(Err(err));
            return;
        }
        let Some(project) = self.project.clone() else {
            let _ = reply.send(op(&mut Session::new(self.preferences.mcp_allow_edit)));
            return;
        };
        let (path, dirty, allow_edit) = (
            self.path.clone().unwrap_or_default(),
            self.edited,
            self.preferences.mcp_allow_edit,
        );
        let target = self.job_target().geometry().idle_scene();
        self.spawn_job(
            JobKind::McpEdit,
            target,
            move || {
                let before = project.clone();
                let mut session = Session::from_project(project, path, dirty, allow_edit);
                let value = op(&mut session)?;
                let Some((after, _, dirty)) = session.into_project() else {
                    bail!("проєкт не відкрито");
                };
                let changed = (after != before).then_some(after);
                anyhow::Ok((value, changed, dirty))
            },
            move |this, result, window, cx| {
                let out = result.map(|(value, changed, dirty)| {
                    if let Some(project) = changed {
                        this.checkpoint("Редагування AI-агентом");
                        this.project = Some(project);
                        this.project_changed(window, cx);
                    }
                    this.edited = dirty;
                    value
                });
                let _ = reply.send(out);
            },
            cx,
        );
    }

    /// MCP slice_plate: та сама черга, що й GUI-нарізка (скасування правкою, kill рушія).
    fn mcp_slice(
        &mut self,
        args: SliceArgs,
        reply: mpsc::Sender<Result<Value>>,
        cx: &mut Context<Self>,
    ) {
        let mut session = match self.mcp_snapshot() {
            Ok(session) => session,
            Err(err) => {
                let _ = reply.send(Err(err));
                return;
            }
        };
        let cancellation = slicer::SliceCancellation::default();
        let token = cancellation.clone();
        let job = self.spawn_job(
            JobKind::McpSlice,
            self.job_target().geometry().settings(),
            move || session.prepare_slice(args)?.run_cancellable(&cancellation),
            move |_, result, _, _| {
                let _ = reply.send(result);
            },
            cx,
        );
        if let Some(job) = job {
            let progress = token.clone();
            let progress: Progress = Arc::new(move || progress.progress());
            self.jobs.set_progress(job, progress);
            self.jobs.set_stage(job, crate::preview::slice_stage(token.clone(), self.engine_identity.clone()));
            self.jobs.on_cancel(job, move || token.cancel());
        }
    }

    fn mcp_call(
        &mut self,
        name: &str,
        op: Operation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Value> {
        let edit = self.mcp_check(name)?;
        if name == "undo" || name == "redo" {
            ensure!(
                self.modal.is_none() && self.page == crate::workspace::Page::Prepare,
                "закрий діалог і відкрий Підготовку"
            );
            let redo = name == "redo";
            let label = if redo {
                self.history.redo_label()
            } else {
                self.history.undo_label()
            }
            .map(str::to_owned);
            let Some(label) = label else {
                bail!("історія порожня");
            };
            self.restore_history(redo, window, cx);
            return Ok(serde_json::json!({"applied": name, "label": label}));
        }
        let mut session = if name == "open_project" {
            Session::new(true)
        } else {
            self.mcp_snapshot()?
        };
        if name == "save_project"
            && let Some(mut project) = self.project.clone()
        {
            project.config = self.effective_project_config();
            session = Session::from_project(
                project,
                self.path.clone().unwrap_or_default(),
                self.edited,
                self.preferences.mcp_allow_edit,
            );
        }
        let mut result = op(&mut session)?;
        if edit && let Some((project, path, dirty)) = session.into_project() {
            if name == "open_project" {
                let plates = crate::objects::plates_of(&project)?;
                self.install_open_project(Some(project), plates, path, cx);
                return Ok(result);
            }
            if self.project.as_ref() != Some(&project) {
                self.checkpoint("Редагування AI-агентом");
                self.project = Some(project);
                self.project_changed(window, cx);
            }
            if name == "save_project" {
                self.model_name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
                    .into();
                self.path = Some(path);
                // Файл тепер на диску, навіть якщо проєкт був «Без назви».
                self.untitled = false;
            }
            self.edited = dirty;
            cx.notify();
        }
        if name == "project_summary" {
            result["can_undo"] = serde_json::json!(self.history.undo_label());
            result["can_redo"] = serde_json::json!(self.history.redo_label());
        }
        Ok(result)
    }
}
