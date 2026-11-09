//! Тестовий seam: знімок стану UI одним JSON-об’єктом.
//!
//! Два споживачі, один формат:
//! * `SLICENV_UI_TRACE=1` — рядок `UI_TRACE {json}` у stderr, лише коли стан змінився
//!   (приймальні скрипти `crates/app/verification/*.py` читають останній рядок);
//! * MCP-інструмент `ui_state` (read-only) — той самий об’єкт на запит.
//!
//! Без `SLICENV_UI_TRACE` цикл кадру платить одне читання статичного прапорця.
use std::{cell::RefCell, sync::OnceLock};

use gpui::App;
use serde_json::{Value, json};

use crate::workspace::{Page, Workspace};

pub(crate) fn trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| paths::env("SLICENV_UI_TRACE").is_some())
}

thread_local! {
    /// Останній надрукований знімок: GPUI-стан живе на одному потоці.
    static LAST_TRACE: RefCell<String> = const { RefCell::new(String::new()) };
}

impl Workspace {
    pub(crate) fn ui_state(&self, cx: &App) -> Value {
        let layers = self.preview.as_ref().map(|p| p.layers.read(cx).values());
        let moves = self.preview.as_ref().map(|p| p.moves.read(cx).values().1);
        let viewport = self.viewport_bounds.map(|b| {
            [
                f32::from(b.origin.x),
                f32::from(b.origin.y),
                f32::from(b.size.width),
                f32::from(b.size.height),
            ]
        });
        json!({
            "page": match self.page { Page::Prepare => "prepare", Page::Preview => "preview", _ => "other" },
            "language": self.preferences.language,
            "process_dirty": self.process_editor.as_ref().is_some_and(|e| e.is_dirty()),
            "preferences_light": crate::theme::is_light(),
            "preferences_theme": self.preferences.theme,
            "theme_background": crate::theme::zed_hex_background(),
            "zoom_to_cursor": self.preferences.zoom_to_cursor,
            "reverse_zoom": self.preferences.reverse_zoom,
            "left_drag_moves": self.preferences.left_drag_moves,
            "navigation_cube": self.preferences.navigation_cube,
            "engine": self.engine.as_ref().map(|e| e.kind.name()),
            "engine_label": self.engine_label,
            "engine_loading": self.engine_loading,
            "schema_options": self.settings_schema.options.len(),
            "camera": [self.camera.yaw, self.camera.pitch, self.camera.distance, self.camera.target.x, self.camera.target.y, self.camera.target.z],
            "viewport": viewport,
            "preview_scheme": self.preview.as_ref().map(|p| p.options.scheme.name_uk()),
            "layers": layers,
            "moves": moves,
            "selected": self.selected.map(|k| [k.object as usize, k.instance]),
            "volume": self.selected_volume,
            "gizmo": self.gizmo_drag.is_some(),
            "selected_bounds": self.selected_bounds().map(|b| [b.min.to_array(), b.max.to_array()]),
            "object_bounds": self.selected.and_then(|k| self.project.as_ref()?.instance_bounds(k).ok()?).map(|b| [b.min.to_array(), b.max.to_array()]),
            "active_plate": self.plates.get(self.active_plate).map(|p| p.number),
            "plate_offsets": self.plates.iter().map(|p| (p.number, self.plate_offset(p.number).to_array())).collect::<Vec<_>>(),
            "object_count": self.project.as_ref().map(|p| p.objects.len()),
            "selection_count": self.selected_instances.len(),
            "volume_count": self.selected.and_then(|k| self.project.as_ref()?.object(k.object).ok()).map(|o| o.volumes.len()),
            "edited": self.edited,
            "save_confirmed": self.save_target.is_some(),
            "boolean_ready": self.boolean_mesh.is_some(),
            "section": self.cut_enabled,
            "section_axis": self.cut_axis,
            "section_position": self.cut_position.read(cx).values().1,
            "section_ghost": self.cut_ghost,
            "navigation_drag": self.navigation_dragging(),
            "navigation_tween": self.navigation_animating(),
        })
    }

    /// Друкує `UI_TRACE` лише при зміні стану. Викликати тільки під `trace_enabled()`.
    pub(crate) fn trace_ui_state(&self, cx: &App) {
        let state = self.ui_state(cx).to_string();
        LAST_TRACE.with_borrow_mut(|last| {
            if *last != state {
                eprintln!("UI_TRACE {state}");
                *last = state;
            }
        });
    }
}
