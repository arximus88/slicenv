//! Обгортка `rmcp`: кожен інструмент — один рядок над методом [`Session`].

use std::sync::{Arc, Mutex};

use anyhow::Result;
use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Implementation, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router,
};
use serde_json::Value;

use crate::session::*;

#[derive(Clone)]
pub struct Server {
    host: Arc<dyn crate::host::Host>,
    tool_router: ToolRouter<Self>,
}

/// Помилка інструмента — це результат із `isError`, а не збій протоколу: агент її бачить і виправляє.
fn reply(result: Result<Value>) -> CallToolResult {
    match result {
        Ok(value) => CallToolResult::structured(value),
        Err(err) => {
            CallToolResult::error(vec![rmcp::model::ContentBlock::text(format!("{err:#}"))])
        }
    }
}

impl Server {
    /// Без `allow_edit` інструментів зміни немає в `tools/list`, а сесія відхиляє їх і напряму.
    pub fn new(allow_edit: bool) -> Self {
        Self::with_host(allow_edit, Arc::new(Mutex::new(Session::new(allow_edit))))
    }

    pub fn with_host(allow_edit: bool, host: Arc<dyn crate::host::Host>) -> Self {
        let mut tool_router = Self::tool_router();
        if !allow_edit {
            for name in EDIT_TOOLS {
                tool_router.remove_route(name);
            }
            if host.open_is_edit() {
                tool_router.remove_route("open_project");
            }
        }
        if !host.has_ui() {
            tool_router.remove_route("ui_state");
        }
        Self { host, tool_router }
    }

    /// Виконує синхронний інструмент поза асинхронним виконавцем.
    async fn run(
        &self,
        name: &'static str,
        f: impl FnOnce(&mut Session) -> Result<Value> + Send + 'static,
    ) -> CallToolResult {
        let host = self.host.clone();
        reply(
            tokio::task::spawn_blocking(move || host.call(name, Box::new(f)))
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("збій інструмента: {e}"))),
        )
    }
}

#[tool_router]
impl Server {
    /// Open a .3mf or .stl project; returns the same data as project_summary. Resets undo history.
    #[tool(annotations(destructive_hint = false))]
    async fn open_project(&self, Parameters(a): Parameters<OpenArgs>) -> CallToolResult {
        self.run("open_project", move |s| s.open_project(a)).await
    }

    /// Plates, objects, sizes (mm) and transforms of the open project, plus its saved profile names.
    #[tool(annotations(read_only_hint = true))]
    async fn project_summary(&self) -> CallToolResult {
        self.run("project_summary", |s| s.project_summary()).await
    }

    /// List preset names of a kind (machine/process/filament) compatible with a printer.
    #[tool(annotations(read_only_hint = true))]
    async fn list_profiles(&self, Parameters(a): Parameters<ListProfilesArgs>) -> CallToolResult {
        self.run("list_profiles", move |s| s.list_profiles(a)).await
    }

    /// Fully flattened preset (inherits resolved). Filter with `keys` / `contains` to keep output small.
    #[tool(annotations(read_only_hint = true))]
    async fn get_profile(&self, Parameters(a): Parameters<GetProfileArgs>) -> CallToolResult {
        self.run("get_profile", move |s| s.get_profile(a)).await
    }

    /// Explain a slicer setting key: label, description (en/uk), type, range, default, enum values, docs URL.
    #[tool(annotations(read_only_hint = true))]
    async fn explain_setting(&self, Parameters(a): Parameters<ExplainArgs>) -> CallToolResult {
        self.run("explain_setting", move |s| s.explain_setting(a))
            .await
    }

    /// Statistics of a .gcode / .gcode.3mf: layers, time, filament, bounds, extrusion length per role.
    #[tool(annotations(read_only_hint = true))]
    async fn gcode_stats(&self, Parameters(a): Parameters<GcodeArgs>) -> CallToolResult {
        self.run("gcode_stats", move |s| s.gcode_stats(a)).await
    }

    /// Slice one plate of the current (even unsaved) project with the installed slicer engine.
    /// Writes only to the app cache; takes seconds to minutes. Returns time, filament, layers, warnings.
    #[tool(annotations(read_only_hint = true))]
    async fn slice_plate(&self, Parameters(a): Parameters<SliceArgs>) -> CallToolResult {
        let host = self.host.clone();
        reply(
            tokio::task::spawn_blocking(move || host.slice(a))
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("збій інструмента: {e}"))),
        )
    }

    /// Read-only snapshot of a Bambu printer via the cloud (needs a Bambu Studio login on this machine).
    #[tool(annotations(read_only_hint = true, open_world_hint = true))]
    async fn printer_status(&self, Parameters(a): Parameters<PrinterArgs>) -> CallToolResult {
        self.run("printer_status", move |s| s.printer_status(a))
            .await
    }

    /// Scale / rotate / translate / center / drop an object instance (applied in that order). Undoable.
    #[tool(annotations(destructive_hint = false, idempotent_hint = false))]
    async fn transform_object(&self, Parameters(a): Parameters<TransformArgs>) -> CallToolResult {
        self.run("transform_object", move |s| s.transform_object(a))
            .await
    }

    /// Pack the objects of a plate on the printer bed. Undoable.
    #[tool(annotations(destructive_hint = false))]
    async fn arrange_plate(&self, Parameters(a): Parameters<ArrangeArgs>) -> CallToolResult {
        self.run("arrange_plate", move |s| s.arrange_plate(a)).await
    }

    /// Auto-orient every object on a plate for printing (minimal overhang, OrcaSlicer algorithm), then drop to bed. `spacing_mm` is ignored. Undoable.
    #[tool(annotations(destructive_hint = false))]
    async fn orient_plate(&self, Parameters(a): Parameters<ArrangeArgs>) -> CallToolResult {
        self.run("orient_plate", move |s| s.orient_plate(a)).await
    }

    /// Undo the last project change in the host history.
    #[tool(annotations(destructive_hint = false))]
    async fn undo(&self) -> CallToolResult {
        self.run("undo", |s| s.undo()).await
    }

    /// Redo the change that was undone.
    #[tool(annotations(destructive_hint = false))]
    async fn redo(&self) -> CallToolResult {
        self.run("redo", |s| s.redo()).await
    }

    /// Read-only snapshot of the desktop app UI: page, camera, viewport rect, selection and its
    /// bounds, preview sliders, preferences. Same JSON as SLICENV_UI_TRACE. Desktop host only.
    #[tool(annotations(read_only_hint = true))]
    async fn ui_state(&self) -> CallToolResult {
        let host = self.host.clone();
        reply(
            tokio::task::spawn_blocking(move || host.ui_state())
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("збій інструмента: {e}"))),
        )
    }

    /// Save the current project to a new .3mf path (writes a file).
    #[tool(annotations(destructive_hint = true))]
    async fn save_project(&self, Parameters(a): Parameters<SaveArgs>) -> CallToolResult {
        self.run("save_project", move |s| s.save_project(a)).await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        let mut config = ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "Slicenv slicer. Start with open_project, then project_summary. \
                 Lengths are mm, times are seconds. Edit tools exist only when the host permits editing.",
            );
        config.server_info = Implementation::new("slicenv", env!("CARGO_PKG_VERSION"));
        config
    }
}
