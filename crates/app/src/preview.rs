//! «Перегляд»: траєкторії нарізаної пластини, схеми кольорів як в Orca, повзунок шарів,
//! повзунок рухів у верхньому шарі з маркером сопла й відтворенням, легенда.

use std::{path::PathBuf, sync::Arc, time::Duration};

use viewer::ToolpathGpuExt;

use gcode::{ColorScheme, Role, Segment, Toolpath, ViewOptions};
use gpui::{Context, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px};

use crate::{
    jobs::{JobKind, JobTarget},
    theme::zed,
    ui::{self, IconName},
    widgets::{Dropdown, DropdownEvent, Slider, SliderEvent},
    workspace::Workspace,
};

pub struct Preview {
    pub toolpath: Arc<Toolpath>,
    pub built: viewer::BuiltToolpath,
    pub options: ViewOptions,
    /// Шари: індекси [lo, hi] включно.
    pub layers: Entity<Slider>,
    /// Рухи у верхньому шарі: скільки сегментів шару показано.
    pub moves: Entity<Slider>,
    pub scheme: Entity<Dropdown>,
    /// Статистика нарізки: типи ліній, час, філамент, вартість.
    report: gcode::Report,
    /// Частка часу екструзії в кожному кольорі градієнтної схеми.
    shares: Vec<f32>,
    pub show_travel: bool,
    pub show_nozzle: bool,
    pub show_bed: bool,
    pub single_layer: bool,
    pub flat_lines: bool,
    pub quality: Entity<Dropdown>,
    pub render_scale: f32,
    playing: Option<Task<()>>,
    /// Режим «Фото» (`photo_mode`): матеріали й GPU-траєкторія; `None` — ще не вмикався.
    pub photo: Option<crate::photo_mode::PhotoState>,
    _subscriptions: Vec<Subscription>,
}

impl Preview {
    /// Видимі шари [from, to).
    pub fn range(&self, cx: &gpui::App) -> (usize, usize) {
        let (lo, hi) = self.layers.read(cx).values();
        (
            if self.single_layer { hi } else { lo },
            (hi + 1).min(self.toolpath.layer_count()),
        )
    }

    /// Сегменти верхнього видимого шару й позиція повзунка рухів у ньому.
    fn top_layer(&self, cx: &gpui::App) -> Option<(std::ops::Range<usize>, usize)> {
        let (_, to) = self.range(cx);
        let layer = self.toolpath.layers.get(to.checked_sub(1)?)?;
        Some((layer.segment_range.clone(), self.moves.read(cx).values().1))
    }

    /// Траєкторія, яку зараз малює рендерер: «Фото», якщо режим увімкнено й буфер готовий.
    pub fn active_built(&self) -> &viewer::BuiltToolpath {
        self.photo
            .as_ref()
            .filter(|p| p.on)
            .and_then(|p| p.built.as_ref())
            .unwrap_or(&self.built)
    }

    pub fn photo_on(&self) -> bool {
        self.photo.as_ref().is_some_and(|p| p.on)
    }

    /// Рух, на якому стоїть повзунок (останній показаний), якщо шар показано не повністю.
    pub fn current_move(&self, cx: &gpui::App) -> Option<(usize, &Segment)> {
        let (segments, k) = self.top_layer(cx)?;
        if k >= segments.len() || k == 0 {
            return None;
        }
        let index = segments.start + k - 1;
        Some((index, self.toolpath.segments.get(index)?))
    }
}

/// Час як `short_time` у Bambu Studio: від години — з округленням до хвилин, інакше хв і с.
pub fn short_time(secs: f32) -> String {
    let secs = secs.max(0.0);
    if secs > 0.0 && secs < 1.0 {
        return crate::i18n::tr("<1 с");
    }
    let total = secs as u64;
    let (mut d, mut h, mut m, s) = (
        total / 86400,
        total / 3600 % 24,
        total / 60 % 60,
        total % 60,
    );
    if d + h > 0 && s >= 30 {
        m += 1;
        if m == 60 {
            m = 0;
            h += 1;
            if h == 24 {
                h = 0;
                d += 1;
            }
        }
    }
    if d > 0 {
        crate::tr_format!("{d} д {h} год {m} хв")
    } else if h > 0 {
        crate::tr_format!("{h} год {m} хв")
    } else if m > 0 {
        crate::tr_format!("{m} хв {s} с")
    } else {
        crate::tr_format!("{s} с")
    }
}

/// Частка як у Bambu: «0%», «<0.1%», інакше одна десяткова.
fn percent(share: f32) -> String {
    if share <= 0.0 {
        "0%".into()
    } else if share <= 0.001 {
        "<0.1%".into()
    } else {
        format!("{:.1}%", share * 100.0)
    }
}

fn srgb_hsla([r, g, b]: [f32; 3]) -> gpui::Hsla {
    let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    gpui::rgb((c(r) << 16) | (c(g) << 8) | c(b)).into()
}

impl Workspace {
    /// Розбирає G-code нарізаної пластини у фоні; `open` — потім відкрити «Перегляд»
    /// (інакше прев’ю лише готується, якщо ця сторінка не відкрита).
    pub fn load_preview(&mut self, gcode: PathBuf, open: bool, cx: &mut Context<Self>) {
        let device = self.renderer.device().clone();
        // Прев’ю належить цій нарізці: тій самій пластині й тому самому входу нарізки.
        self.spawn_job(
            JobKind::GcodePreview,
            self.job_target().settings().active_plate(),
            move || {
                let _total = paths::profile::Span::new("preview::load");
                let toolpath = {
                    let _s = paths::profile::Span::new("preview::parse_gcode");
                    gcode::parse_file(&gcode)?
                };
                let built = {
                    let _s = paths::profile::Span::new("preview::build_gpu");
                    toolpath.build(&device, &ViewOptions::default())
                };
                // Оцінки рушія лежать поруч: `result.json` у теці нарізки.
                let plate = gcode
                    .file_stem()
                    .and_then(|s| s.to_str()?.strip_prefix("plate_")?.parse().ok());
                let report = {
                    let _s = paths::profile::Span::new("preview::report");
                    let engine =
                        gcode::EngineReport::read(&gcode.with_file_name("result.json"), plate);
                    toolpath.report(engine.as_ref())
                };
                Ok::<_, anyhow::Error>((toolpath, built, report))
            },
            move |this, parsed, window, cx| match parsed {
                Ok((toolpath, built, report)) => {
                    let _s = paths::profile::Span::new("preview::show");
                    this.show_preview(toolpath, built, report, open, window, cx)
                }
                Err(err) => this.error = Some(format!("G-code: {err:#}").into()),
            },
            cx,
        );
    }

    fn show_preview(
        &mut self,
        toolpath: Toolpath,
        built: viewer::BuiltToolpath,
        report: gcode::Report,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = toolpath.layer_count();
        let top_moves = toolpath.layers.last().map_or(0, |l| l.segment_range.len());
        let layers = cx.new(|_| Slider::range(count.saturating_sub(1), true));
        let moves = cx.new(|_| Slider::single(top_moves, false));
        let scheme_index = if self.preferences.remember_scheme {
            self.preferences.last_scheme.min(ColorScheme::ALL.len() - 1)
        } else {
            0
        };
        let scheme = cx.new(|cx| {
            Dropdown::new(
                ColorScheme::ALL
                    .iter()
                    .map(|s| SharedString::from(s.name_uk()))
                    .collect(),
                Some(scheme_index),
                cx,
            )
        });
        let quality = cx.new(|cx| {
            Dropdown::new(
                [
                    "Швидко · 65%",
                    "Стандарт · 100%",
                    "Висока · 150%",
                ]
                .into_iter()
                .map(SharedString::from)
                .collect(),
                Some(1),
                cx,
            )
        });
        let subscriptions = vec![
            cx.subscribe_in(&quality, window, |this, _, e: &DropdownEvent, _, cx| {
                let DropdownEvent::Confirm(name) = e;
                // Валик — стрічка з попіксельною нормаллю: граней більше немає, якість —
                // лише роздільність кадру.
                let scale = if name.starts_with("Швидко") {
                    0.65
                } else if name.starts_with("Висока") {
                    1.5
                } else {
                    1.0
                };
                if let Some(preview) = &mut this.preview {
                    preview.render_scale = scale;
                }
                this.dirty = true;
                cx.notify();
            }),
            cx.subscribe_in(&layers, window, |this, _, _: &SliderEvent, _, cx| {
                this.stop_playback();
                this.reset_moves(cx);
                this.apply_preview_view(cx);
                cx.notify();
            }),
            cx.subscribe_in(&moves, window, |this, _, _: &SliderEvent, _, cx| {
                this.stop_playback();
                this.apply_preview_view(cx);
                cx.notify();
            }),
            cx.subscribe_in(&scheme, window, |this, _, e: &DropdownEvent, _, cx| {
                let DropdownEvent::Confirm(name) = e;
                if let Some(s) = ColorScheme::ALL
                    .into_iter()
                    .find(|s| s.name_uk() == name.as_ref())
                    && let Some(preview) = &mut this.preview
                {
                    preview.options.scheme = s;
                    this.preferences.last_scheme =
                        ColorScheme::ALL.iter().position(|v| *v == s).unwrap_or(0);
                    this.save_preferences(cx);
                    this.rebuild_toolpath(cx);
                }
            }),
        ];
        let options = ViewOptions {
            scheme: ColorScheme::ALL[scheme_index],
            ..Default::default()
        };
        self.preview = Some(Preview {
            built,
            options,
            report,
            shares: Vec::new(),
            toolpath: Arc::new(toolpath),
            layers,
            moves,
            scheme,
            show_travel: false,
            show_nozzle: true,
            show_bed: true,
            single_layer: false,
            flat_lines: false,
            quality,
            render_scale: 1.0,
            playing: None,
            photo: None,
            _subscriptions: subscriptions,
        });
        // Нова нарізка — знову «Шари»: ресурси «Фото» попередньої звільняються.
        self.renderer.set_photo(None);
        self.renderer.set_show_travel(false);
        self.renderer.set_show_bed(true);
        self.renderer.set_flat_toolpath(false);
        if open || self.page == crate::workspace::Page::Preview {
            self.enter_page(crate::workspace::Page::Preview, cx);
        }
        if scheme_index != 0 {
            self.rebuild_toolpath(cx);
        }
    }

    /// Rebuild away from the UI; only the newest result for this toolpath may be installed.
    fn rebuild_toolpath(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = &mut self.preview else {
            return;
        };
        let toolpath = preview.toolpath.clone();
        let options = preview.options.clone();
        let device = self.renderer.device().clone();
        // Нова перебудова замінює попередню; результат — лише для того самого toolpath.
        let data = toolpath.clone();
        self.spawn_job(
            JobKind::ToolpathBuild,
            JobTarget::any(),
            move || {
                let built = data.build(&device, &options);
                let shares = built
                    .range
                    .map(|r| data.time_shares(&options, r, gcode::RANGE_COLORS.len()))
                    .unwrap_or_default();
                (built, shares)
            },
            move |this, (built, shares), _, cx| {
                let Some(preview) = &mut this.preview else {
                    return;
                };
                if !Arc::ptr_eq(&preview.toolpath, &toolpath) {
                    return;
                }
                preview.built = built;
                preview.shares = shares;
                if this.page == crate::workspace::Page::Preview {
                    this.renderer.set_toolpath(&preview.active_built().gpu);
                    this.apply_preview_view(cx);
                }
            },
            cx,
        );
    }

    /// Повзунок рухів — на весь новий верхній шар.
    fn reset_moves(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = &self.preview else { return };
        let (_, to) = preview.range(cx);
        let n = to
            .checked_sub(1)
            .and_then(|i| preview.toolpath.layers.get(i))
            .map_or(0, |l| l.segment_range.len());
        preview.moves.update(cx, |s, cx| s.reset(n, cx));
    }

    /// Передає рендереру видимі шари й рухи та ставить маркер сопла.
    pub fn apply_preview_view(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = &self.preview else { return };
        let (from, to) = preview.range(cx);
        let until = preview
            .top_layer(cx)
            .map(|(segments, k)| segments.start + k);
        let (instances, travel) = preview
            .active_built()
            .visible_ranges(&preview.toolpath, from..to, until);
        self.renderer.set_visible_ranges(instances, travel);
        let marker = preview
            .current_move(cx)
            .filter(|_| preview.show_nozzle)
            .map(|(_, s)| viewer::gizmo::nozzle_marker(&self.camera, s.to));
        self.renderer
            .set_overlay(marker.as_deref().unwrap_or_default());
        self.dirty = true;
    }

    pub fn stop_playback(&mut self) {
        if let Some(preview) = &mut self.preview {
            preview.playing = None;
        }
    }

    /// Відтворення: повзунок рухів іде від поточної позиції до кінця шару (~12 с на шар).
    fn toggle_playback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(preview) = &mut self.preview else {
            return;
        };
        if preview.playing.take().is_some() {
            cx.notify();
            return;
        }
        let moves = preview.moves.clone();
        if moves.read(cx).values().1 >= moves.read(cx).max() {
            moves.update(cx, |s, cx| s.set(0, 0, cx));
        }
        preview.playing = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(33))
                    .await;
                let Ok(done) = this.update(cx, |this, cx| {
                    let (k, max) = (moves.read(cx).values().1, moves.read(cx).max());
                    let step = (max / 360).max(1);
                    moves.update(cx, |s, cx| s.set(0, k + step, cx));
                    this.apply_preview_view(cx);
                    cx.notify();
                    k + step >= max
                }) else {
                    break;
                };
                if done {
                    this.update(cx, |this, cx| {
                        this.stop_playback();
                        cx.notify();
                    })
                    .ok();
                    break;
                }
            }
        }));
        cx.notify();
    }

    /// Стрілки на «Перегляді»: ↑↓ — верхній шар, ←→ — рух у шарі (Shift — по 10).
    pub fn preview_key(&mut self, key: &str, shift: bool, cx: &mut Context<Self>) -> bool {
        let Some(preview) = &self.preview else {
            return false;
        };
        let step = if shift { 10 } else { 1 };
        match key {
            "up" | "down" => {
                let (lo, hi) = preview.layers.read(cx).values();
                let max = preview.layers.read(cx).max();
                let hi = if key == "up" {
                    (hi + step).min(max)
                } else {
                    hi.saturating_sub(step).max(lo)
                };
                preview.layers.update(cx, |s, cx| s.set(lo, hi, cx));
                self.stop_playback();
                self.reset_moves(cx);
            }
            "left" | "right" => {
                let k = preview.moves.read(cx).values().1;
                let k = if key == "right" {
                    k + step
                } else {
                    k.saturating_sub(step)
                };
                preview.moves.update(cx, |s, cx| s.set(0, k, cx));
                self.stop_playback();
            }
            _ => return false,
        }
        self.apply_preview_view(cx);
        cx.notify();
        true
    }
}

impl Workspace {
    /// Зберігає нарізаний результат: `.gcode.3mf` (для друку з картки A1) або чистий `.gcode`.
    pub fn prompt_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let crate::workspace::SliceState::Done(out) = &self.slice_state else {
            self.toast(
                crate::widgets::ToastKind::Warning,
                "Спочатку нарізай пластину",
                cx,
            );
            return;
        };
        let source = out.gcode_3mf.clone();
        let Some(plate) = self.plate_to_slice() else {
            return;
        };
        let dir = self
            .path
            .as_ref()
            .and_then(|p| p.parent())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.library_dir.clone());
        let name = source
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let target = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(target))) = target.await else {
                return;
            };
            let plain = target.extension().is_some_and(|e| e == "gcode")
                && !target.to_string_lossy().ends_with(".gcode.3mf");
            let from = if plain {
                source.with_file_name(format!("plate_{plate}.gcode"))
            } else {
                source
            };
            let result = std::fs::copy(&from, &target);
            this.update(cx, |this, cx| {
                use crate::widgets::ToastKind;
                match result {
                    Ok(_) => this.toast(
                        ToastKind::Success,
                        crate::tr_format!("Експортовано {}", crate::shorten_home(&target)),
                        cx,
                    ),
                    Err(err) => this.toast(
                        ToastKind::Error,
                        crate::tr_format!("Не експортовано: {err}"),
                        cx,
                    ),
                }
            })
            .ok();
        })
        .detach();
    }
}

/// Етап нарізки з логу рушія українською: спершу наші формулювання, далі каталог перекладів
/// самого рушія, інакше — як є. На генерації G-code рушій повідомляє номер шару («шар 80»);
/// загальної кількості шарів він не дає, а оцінка з висоти моделі хибить на змінній висоті
/// шару, підтримках і друці по черзі, тож показується лише номер.
pub(crate) fn slice_stage_text(stage: &str, layer: Option<u32>, engine: Option<&str>) -> String {
    let text = slice_stage_name(stage, engine);
    match layer {
        Some(n) => crate::tr_format!("{}: шар {}", text, n),
        None => text,
    }
}

/// Етап для [`crate::jobs::JobStatus`]: текст з етапом і шаром запущеної нарізки.
pub(crate) fn slice_stage(token: slicer::SliceCancellation, engine: Option<String>) -> crate::jobs::Stage {
    std::sync::Arc::new(move || {
        token.stage().map(|s| slice_stage_text(&s, token.layer(), engine.as_deref()))
    })
}

fn slice_stage_name(stage: &str, engine: Option<&str>) -> String {
    let known = [
        ("Slicing mesh", "Нарізаю сітку на шари"),
        ("Generating perimeters", "Будую периметри"),
        ("Generating walls", "Будую стінки"),
        ("Preparing infill", "Готую заповнення"),
        ("Generating infill regions", "Розмічаю області заповнення"),
        ("Generating infill toolpath", "Будую траєкторії заповнення"),
        ("Generating support", "Будую підтримки"),
        ("Checking support necessity", "Перевіряю, чи потрібні підтримки"),
        // «for auto-lift» у Bambu 02.07 і Orca 2.5, «for auto-lower» у старіших.
        ("Detect overhangs", "Шукаю нависання"),
        ("Optimizing toolpath", "Оптимізую траєкторії"),
        ("Generating skirt & brim", "Будую спідницю й кайму"),
        ("Exporting G-code", "Записую G-code"),
        ("Generating G-code", "Генерую G-code"),
        ("Processing", "Обробляю"),
    ];
    let trimmed = stage.trim_end_matches(['.', '…']).trim();
    known
        .iter()
        .find(|(en, _)| trimmed.eq_ignore_ascii_case(en) || trimmed.starts_with(en))
        .map(|(_, uk)| crate::i18n::tr(uk).to_string())
        .or_else(|| {
            (crate::i18n::locale() == settings::Locale::Uk)
                .then(|| crate::engines::engine_catalog(engine?)?.get(trimmed).map(str::to_owned))
                .flatten()
        })
        .unwrap_or_else(|| trimmed.to_owned())
}

pub fn render(ws: &mut Workspace, cx: &mut Context<Workspace>) -> impl IntoElement {
    let z = zed();
    if ws.preview.is_none() {
        // Невдала нарізка — картка: що сталось, хто виправляє, що зробити, деталі.
        if let crate::workspace::SliceState::Failed(err, failure) = &ws.slice_state {
            let card = failure_card(err.clone(), failure.clone(), ws.can_slice(), ws.slice_exhausted, ws.slice_fallback.clone(), cx);
            return div()
                .relative()
                .flex()
                .flex_1()
                .bg(z.editor)
                .child(card)
                .children(crate::plate_strip::strip(ws, cx))
                .into_any_element();
        }
        // Прев’ю скинули, поки сторінка відкрита (зміна пластини, геометрії): сцена знову
        // показує моделі замість траєкторії.
        if ws.renderer.has_toolpath() {
            ws.enter_page(crate::workspace::Page::Preview, cx);
        }
        // Ще не нарізано (або йде нарізка): та сама сцена, що й на «Підготовці», моделі —
        // силуетом, підказка компактна поверх сцени.
        let center = ws.viewport_view("preview-unsliced", |ws, _, cx| unsliced_center(ws, cx), cx);
        let settings = ws.cached_panel(crate::prepare::SETTINGS_PANEL, crate::prepare::settings_panel, cx);
        return crate::prepare::docked("preview-unsliced-docks", ws, None, center, Some(settings))
            .into_any_element();
    }
    // Центр зі сценою — окремий вид: рух камери не перебудовує статистику праворуч.
    let center = ws.viewport_view("preview-center", |ws, _, cx| center(ws, cx), cx);
    // Праворуч — та сама панель «Друк», що й на «Підготовці» (параметри можна міняти, не
    // виходячи з Preview); статистика й легенда — ліворуч.
    let left = ws.cached_panel("preview-summary", |ws, _, cx| summary_panel(ws, cx), cx);
    let settings = ws.cached_panel(crate::prepare::SETTINGS_PANEL, crate::prepare::settings_panel, cx);
    crate::prepare::docked("preview-docks", ws, Some(left), center, Some(settings)).into_any_element()
}

/// Хлібні крихти Preview: модель і пластина.
fn crumbs(ws: &Workspace) -> Vec<SharedString> {
    let mut crumbs: Vec<SharedString> = vec![ws.model_name.clone()];
    if let Some(n) = ws.plate_to_slice() {
        crumbs.push(crate::tr_format!("Пластина {n}").into());
    }
    crumbs
}

/// Картка поверх сцени внизу по центру: не закриває об’єкти й не ловить орбіту камери.
fn scene_card(id: &'static str, content: impl IntoElement) -> gpui::Div {
    let z = zed();
    div()
        .absolute()
        .left(px(12.))
        .right(px(12.))
        .bottom(px(16.))
        .flex()
        .justify_center()
        .child(
            ui::scene_control(id)
                .flex()
                .items_center()
                .gap(px(12.))
                .max_w(px(560.))
                .px(px(12.))
                .py(px(8.))
                .rounded(px(6.))
                .bg(z.panel.opacity(0.94))
                .border_1()
                .border_color(z.border_variant)
                .shadow_sm()
                .child(content),
        )
}

/// Хід нарізки: етап і номер шару з логу рушія; до першого звіту смуга біжить.
fn slice_progress(ws: &Workspace, cx: &mut Context<Workspace>) -> Option<gpui::Div> {
    let z = zed();
    let job = ws.jobs.get(JobKind::Slice)?;
    let percent = job.progress.as_ref().and_then(|p| p());
    let stage = job.stage.as_ref().and_then(|s| s());
    let elapsed = job.started.elapsed().as_secs();
    let detail = match percent {
        Some(p) => crate::tr_format!("{} % · {} с", p, elapsed),
        None => crate::tr_format!("{} с", elapsed),
    };
    let title: SharedString = job.label.clone().unwrap_or_else(|| "Нарізка…".into());
    Some(
        div()
            .flex()
            .min_w_0()
            .items_center()
            .gap(px(12.))
            .child(ui::icon(IconName::Scissors).text_color(z.text_muted))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_shrink_1()
                    .min_w_0()
                    .gap(px(4.))
                    .w(px(280.))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .text_sm()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(crate::i18n::tr(&title)),
                            )
                            .child(div().flex_none().text_xs().text_color(z.text_muted).child(detail)),
                    )
                    .child(match percent {
                        // Відсоток невідомий (рушій ще не звітував, сумісний режим) — смуга біжить,
                        // щоб було видно, що робота йде.
                        Some(p) if p > 0 => crate::widgets::progress(f32::from(p)).into_any_element(),
                        _ => crate::widgets::progress_indeterminate("slice-progress"),
                    })
                    .children(stage.map(|text| {
                        div().min_w_0().truncate().text_xs().text_color(z.text_muted).child(text)
                    })),
            )
            .child(
                ui::ghost_button("empty-slice-cancel", Some(IconName::X), "Скасувати")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.stop_slice();
                        cx.notify();
                    })),
            ),
    )
}

/// Preview до нарізки: сцена «Підготовки» з моделями-силуетами й компактна підказка.
fn unsliced_center(ws: &mut Workspace, cx: &mut Context<Workspace>) -> gpui::AnyElement {
    let z = zed();
    let can_slice = ws.can_slice();
    let stale = ws.active_slice_stale();
    let hint = match slice_progress(ws, cx) {
        Some(progress) => progress,
        None => div()
            .flex()
            .items_center()
            .gap(px(12.))
            .child(ui::icon(IconName::Layers).text_color(z.text_muted))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .max_w(px(340.))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(crate::i18n::tr(if stale { "Нарізка застаріла" } else { "Ще нічого не нарізано" })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(z.text_muted)
                            .whitespace_normal()
                            .child(crate::i18n::tr(if stale {
                                "Пластину або параметри змінено після нарізки — наріж її знову."
                            } else {
                                "Нарізай пластину — тут з’являться траєкторії по шарах, час і витрата філаменту."
                            })),
                    ),
            )
            .child(
                ui::accent_button("empty-slice", IconName::Scissors, "Нарізати", can_slice)
                    .flex_none()
                    .when(can_slice, |d| {
                        d.on_click(cx.listener(|this, _, window, cx| this.start_slice(window, cx)))
                    }),
            ),
    };
    div()
        .flex()
        .flex_col()
        .size_full()
        .min_w_0()
        .bg(z.editor)
        .child(ui::toolbar().child(ui::breadcrumbs(crumbs(ws))))
        .child({
            let strip = crate::plate_strip::strip(ws, cx);
            ws.viewport(cx)
                .child(crate::navigation::cube(ws, cx))
                .child(scene_card("preview-unsliced-card", hint))
                .children(strip)
        })
        .into_any_element()
}

/// Центр Preview: панель шару, в’юпорт, повзунки шарів і рухів.
fn center(ws: &mut Workspace, cx: &mut Context<Workspace>) -> gpui::AnyElement {
    let z = zed();
    let stale = stale_card(ws, cx);
    let strip = crate::plate_strip::strip(ws, cx);
    let Some(preview) = &ws.preview else {
        return div().into_any_element();
    };
    let toolpath = &preview.toolpath;
    let (from, to) = preview.range(cx);
    let layer = toolpath.layers.get(to.saturating_sub(1));
    let mono = |d: gpui::Div| d.font_family(crate::theme::MONO_FONT).text_xs();

    let mut crumbs = crumbs(ws);
    crumbs.push(
        if from + 1 == to {
            crate::tr_format!("шар {to} з {}", toolpath.layer_count())
        } else {
            crate::tr_format!("шари {}–{to} з {}", from + 1, toolpath.layer_count())
        }
        .into(),
    );
    let toolbar = ui::toolbar()
        .child(ui::breadcrumbs(crumbs))
        .child(div().flex_1())
        .child(crate::photo_mode::mode_switch(ws, cx))
        .child(div().w(px(8.)))
        .children(layer.map(|l| {
            mono(div())
                .flex()
                .gap(px(12.))
                .px(px(6.))
                .text_color(z.text_muted)
                .child(crate::tr_format!("Z {:.2} мм", l.z))
                .child(crate::tr_format!("h {:.2} мм", l.height))
        }))
        // Місце під спінер перебудови зарезервоване завжди: поява й зникнення не зсувають рядок.
        .child(
            div()
                .flex_none()
                .size(px(16.))
                .flex()
                .items_center()
                .justify_center()
                .when(ws.jobs.is_running(JobKind::ToolpathBuild), |d| {
                    d.child(ui::spinner("preview-rebuild"))
                }),
        );

    // Вертикальний повзунок шарів праворуч.
    let layers_card = ui::scene_control("preview-layer-card")
        .absolute()
        .top(px(12.))
        .bottom(px(64.))
        .right(px(12.))
        .w(px(52.))
        .py(px(8.))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(6.))
        .rounded(px(6.))
        .bg(z.panel.opacity(0.92))
        .border_1()
        .border_color(z.border_variant)
        .child(mono(div()).text_color(z.text).child(format!("{to}")))
        .child(
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .child(preview.layers.clone()),
        )
        .child(
            mono(div())
                .text_color(z.text_muted)
                .child(format!("{}", from + 1)),
        );

    // Повзунок рухів у верхньому шарі з кнопкою відтворення й параметрами поточного руху.
    let (k, n) = {
        let s = preview.moves.read(cx);
        (s.values().1, s.max())
    };
    let playing = preview.playing.is_some();
    let current = preview.current_move(cx).map(|(_, s)| *s);
    let readout = current.map(|s| {
        let role = if s.is_extrusion {
            s.role.name_uk()
        } else {
            "Холостий хід"
        };
        mono(div())
            .flex()
            .gap(px(10.))
            .text_color(z.text_muted)
            .child(
                div()
                    .text_color(z.text)
                    .font_family(crate::theme::UI_FONT)
                    .child(crate::i18n::tr(role)),
            )
            .child(crate::tr_format!("{:.0} мм/с", s.speed()))
            .when(s.is_extrusion, |d| {
                d.child(crate::tr_format!("{:.2}×{:.2} мм", s.width, s.height))
                    .child(crate::tr_format!("{:.1} мм³/с", s.volumetric_flow()))
            })
            .child(crate::tr_format!("вент. {}%", s.fan))
            .child(format!("{} °C", s.temp))
    });
    let moves_bar = ui::scene_control("preview-move-card")
        .absolute()
        .left(px(12.))
        .right(px(76.))
        .bottom(px(12.))
        .flex()
        .flex_col()
        .gap(px(4.))
        .px(px(8.))
        .py(px(6.))
        .rounded(px(6.))
        .bg(z.panel.opacity(0.92))
        .border_1()
        .border_color(z.border_variant)
        .children(readout)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    ui::icon_button(
                        "moves-play",
                        if playing {
                            IconName::Pause
                        } else {
                            IconName::Play
                        },
                        if playing {
                            "Пауза"
                        } else {
                            "Відтворити шар"
                        },
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_playback(window, cx))),
                )
                .child(div().flex_1().child(preview.moves.clone()))
                .child(
                    mono(div())
                        .w(px(96.))
                        .text_color(z.text_muted)
                        .child(format!("{k} / {n}")),
                ),
        );

    div()
        .flex()
        .flex_col()
        .size_full()
        .min_w_0()
        .bg(z.editor)
        .child(toolbar)
        .child(
            ws.viewport(cx)
                .child(crate::navigation::cube(ws, cx))
                .child(layers_card)
                .child(moves_bar)
                .children(stale)
                .children(strip),
        )
        .into_any_element()
}

/// Нарізка застаріла (змінено параметри після неї) або вже йде нова: картка вгорі ліворуч,
/// між лівим краєм і навігаційним кубом, щоб не налазити на повзунки.
fn stale_card(ws: &Workspace, cx: &mut Context<Workspace>) -> Option<gpui::Div> {
    let z = zed();
    ws.preview.as_ref()?;
    // Нарізка іншої пластини (черга «Нарізати всі») видна в колонці пластин.
    let own = ws.slicing_plate().is_none_or(|n| Some(n) == ws.plate_to_slice());
    let content = match slice_progress(ws, cx).filter(|_| own) {
        Some(progress) => progress,
        None if ws.active_slice_stale() => {
            let can_slice = ws.can_slice();
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(div().flex_none().size(px(8.)).rounded_full().bg(z.warning))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .whitespace_normal()
                        .child(crate::i18n::tr("Параметри змінено — нарізка застаріла")),
                )
                .child(
                    ui::accent_button("stale-reslice", IconName::Scissors, "Перерізати", can_slice)
                        .flex_none()
                        .when(can_slice, |d| {
                            d.on_click(cx.listener(|this, _, window, cx| this.start_slice(window, cx)))
                        }),
                )
        }
        None => return None,
    };
    let cube = if ws.preferences.navigation_cube { 76. + 132. + 12. } else { 76. };
    // Поруч із колонкою пластин, а не під нею.
    let left = crate::plate_strip::strip_width(ws).map_or(12., |w| 12. + w + 8.);
    Some(
        div()
            .absolute()
            .top(px(12.))
            .left(px(left))
            .right(px(cube))
            .flex()
            .child(
                ui::scene_control("preview-stale-card")
                    .flex()
                    .min_w_0()
                    .overflow_hidden()
                    .max_w(px(520.))
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(px(6.))
                    .bg(z.panel.opacity(0.94))
                    .border_1()
                    .border_color(z.border_variant)
                    .shadow_sm()
                    .child(content),
            ),
    )
}

/// Ширина колонки за найдовшим значенням, як `calculate_offsets` у Bambu: моноширинний
/// text_xs — 7.2 px на символ, підпис заголовка — приблизно 6.6 px.
fn col_width<'a>(values: impl IntoIterator<Item = &'a str>, header: &str) -> f32 {
    let chars = values
        .into_iter()
        .map(|v| v.chars().count())
        .max()
        .unwrap_or(0);
    (chars as f32 * 7.2)
        .max(header.chars().count() as f32 * 6.6)
        .ceil()
        + 2.0
}

const COL_SHARE: f32 = 44.;

fn mono(d: gpui::Div) -> gpui::Div {
    d.font_family(crate::theme::MONO_FONT).text_xs()
}

/// Числова клітинка таблиці, вирівняна праворуч.
fn cell(width: f32, text: impl Into<SharedString>) -> gpui::Div {
    mono(div())
        .w(px(width))
        .flex_none()
        .flex()
        .justify_end()
        .child(text.into())
}

/// Перемикає видимість типу лінії чи позначки й перебудовує буфер.
fn toggle_role(this: &mut Workspace, role: Role, cx: &mut Context<Workspace>) {
    if let Some(preview) = &mut this.preview {
        if let Some(pos) = preview.options.hidden.iter().position(|r| *r == role) {
            preview.options.hidden.remove(pos);
        } else {
            preview.options.hidden.push(role);
        }
        this.rebuild_toolpath(cx);
    }
}

/// Рядок таблиці: квадрат кольору (або крапка для позначок), назва, клітинки, «Показ».
fn table_row(
    id: impl Into<gpui::ElementId>,
    color: Option<gpui::Hsla>,
    dot: bool,
    name: String,
    cells: Vec<gpui::Div>,
    visible: Option<bool>,
) -> gpui::Stateful<gpui::Div> {
    let z = zed();
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(8.))
        .mx(px(4.))
        .px(px(6.))
        .h(px(24.))
        .rounded(px(4.))
        .when(visible == Some(false), |d| d.text_color(z.text_placeholder))
        .when(visible.is_some(), |d| {
            d.cursor_pointer().hover(|s| s.bg(z.element_hover))
        })
        .child(
            div()
                .size(px(10.))
                .flex_none()
                .rounded(px(if dot { 5. } else { 2. }))
                .border_1()
                .border_color(z.border_variant)
                .when_some(color, |d, c| {
                    d.bg(if visible == Some(false) {
                        c.opacity(0.35)
                    } else {
                        c
                    })
                }),
        )
        .child(div().flex_1().min_w_0().truncate().text_sm().child(name))
        .children(cells)
        .child(match visible {
            Some(on) => crate::widgets::checkbox(gpui::ElementId::Name("cb".into()), on, false)
                .into_any_element(),
            None => div().w(px(14.)).flex_none().into_any_element(),
        })
}

/// Заголовок таблиці.
fn table_header(first: &str, columns: &[(f32, &str)], display: bool) -> gpui::Div {
    let z = zed();
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .mx(px(4.))
        .px(px(6.))
        .h(px(22.))
        .text_xs()
        .text_color(z.text_muted)
        .child(div().w(px(10.)).flex_none())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(crate::i18n::tr(first)),
        )
        .children(columns.iter().map(|(w, label)| {
            div()
                .w(px(*w))
                .flex_none()
                .flex()
                .justify_end()
                .truncate()
                .child(crate::i18n::tr(label))
        }))
        .child(
            div()
                .w(px(14.))
                .flex_none()
                .when(display, |d| d.child(ui::icon(IconName::Eye).size(px(12.)))),
        )
}

/// Таблиця «Тип лінії», як у Bambu: час, частка, філамент, показ; далі переміщення й позначки.
fn line_type_table(ws: &Workspace, cx: &mut Context<Workspace>) -> Vec<gpui::AnyElement> {
    let z = zed();
    let Some(preview) = &ws.preview else {
        return Vec::new();
    };
    let report = &preview.report;
    let rows: Vec<_> = report
        .rows
        .iter()
        .map(|row| {
            (
                row.role,
                if row.time > 0.0 {
                    short_time(row.time)
                } else {
                    String::new()
                },
                percent(row.share),
                crate::tr_format!("{:.2} м", row.filament_m),
                row.filament_g.map(|g| crate::tr_format!("{:.2} г", g)).unwrap_or_default(),
            )
        })
        .collect();
    let travel = report.travel.map(|(t, s)| (short_time(t), percent(s)));
    let (time_h, share_h) = (crate::i18n::tr("Час"), crate::i18n::tr("Частка"));
    let w_time = col_width(
        rows.iter()
            .map(|r| r.1.as_str())
            .chain(travel.iter().map(|t| t.0.as_str())),
        &time_h,
    );
    let w_share = col_width(
        rows.iter()
            .map(|r| r.2.as_str())
            .chain(travel.iter().map(|t| t.1.as_str())),
        &share_h,
    );
    let w_m = col_width(rows.iter().map(|r| r.3.as_str()), "");
    let w_g = col_width(rows.iter().map(|r| r.4.as_str()), "");
    let mut out = vec![
        table_header(
            "Тип лінії",
            &[
                (w_time, "Час"),
                (w_share, "Частка"),
                (w_m + w_g + 12., "Філамент"),
            ],
            true,
        )
        .into_any_element(),
    ];
    for (role, time, share, m, g) in rows {
        let [r, gr, b, _] = role.color_srgb();
        let visible = preview.options.is_visible(role);
        out.push(
            table_row(
                ("line-type", role as usize),
                Some(srgb_hsla([r, gr, b])),
                false,
                crate::i18n::tr(role.name_uk()),
                vec![
                    cell(w_time, time),
                    cell(w_share, share),
                    cell(w_m, m).ml(px(4.)),
                    cell(w_g, g),
                ],
                Some(visible),
            )
            .on_click(cx.listener(move |this, _, _, cx| toggle_role(this, role, cx)))
            .into_any_element(),
        );
    }
    // Переміщення: перемикач рендерера, буфер не перебудовується.
    let on = preview.show_travel;
    let [r, g, b, _] = Role::Travel.color_srgb();
    let (travel_time, travel_share) = travel.unwrap_or_default();
    out.push(
        table_row(
            "line-type-travel",
            Some(srgb_hsla([r, g, b])),
            false,
            crate::i18n::tr(Role::Travel.name_uk()),
            vec![
                cell(w_time, travel_time),
                cell(w_share, travel_share),
                div().w(px(w_m + w_g + 12.)).flex_none(),
            ],
            Some(on),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            if let Some(preview) = &mut this.preview {
                preview.show_travel = !on;
            }
            this.renderer.set_show_travel(!on);
            this.dirty = true;
            cx.notify();
        }))
        .into_any_element(),
    );
    // Позначки, як опції Bambu: скільки їх у G-code.
    for (role, label, count) in [
        (Role::Retract, "Ретракція", report.retracts),
        (Role::Unretract, "Повернення філаменту", report.unretracts),
        (Role::Wipe, "Витирання сопла", report.wipes),
        (Role::Seam, "Шви", report.seams),
    ] {
        if count == 0 {
            continue;
        }
        let [r, g, b, _] = role.color_srgb();
        let visible = preview.options.is_visible(role);
        out.push(
            table_row(
                ("line-type-option", role as usize),
                Some(srgb_hsla([r, g, b])),
                role != Role::Wipe,
                crate::i18n::tr(label),
                vec![
                    mono(div())
                        .flex_none()
                        .text_color(z.text_muted)
                        .child(crate::tr_format!("{count} ×")),
                ],
                Some(visible),
            )
            .on_click(cx.listener(move |this, _, _, cx| toggle_role(this, role, cx)))
            .into_any_element(),
        );
    }
    out
}

/// Легенда схеми кольорів: таблиця типів ліній, філаменти або шкала градієнта з часткою часу.
fn legend_section(
    ws: &Workspace,
    scheme: ColorScheme,
    cx: &mut Context<Workspace>,
) -> Vec<gpui::AnyElement> {
    let z = zed();
    let Some(preview) = &ws.preview else {
        return Vec::new();
    };
    // У таблиць типів ліній і філаментів назву несе перша колонка заголовка.
    let mut out = Vec::new();
    if scheme.is_gradient() {
        out.push(ui::group_title(scheme.name_uk()).into_any_element());
    } else {
        out.push(div().h(px(6.)).into_any_element());
    }
    match scheme {
        ColorScheme::LineType => out.extend(line_type_table(ws, cx)),
        ColorScheme::Filament => {
            let colors = gcode::filament_colors(&preview.toolpath);
            let with_cost = preview.report.tools.iter().any(|t| t.cost.is_some());
            let m: Vec<String> = preview
                .report
                .tools
                .iter()
                .map(|t| crate::tr_format!("{:.2} м", t.m))
                .collect();
            let g: Vec<String> = preview
                .report
                .tools
                .iter()
                .map(|t| t.g.map(|g| crate::tr_format!("{:.2} г", g)).unwrap_or_default())
                .collect();
            let (w_m, w_g) = (
                col_width(m.iter().map(String::as_str), ""),
                col_width(g.iter().map(String::as_str), ""),
            );
            let cost_h = crate::i18n::tr("Вартість");
            let w_cost = col_width(["0.00"], &cost_h);
            let mut columns = vec![(w_m + w_g + 12., "Філамент")];
            if with_cost {
                columns.push((w_cost, "Вартість"));
            }
            out.push(table_header("", &columns, false).into_any_element());
            for (i, tool) in preview.report.tools.iter().enumerate() {
                let color = colors.get(usize::from(tool.tool)).copied().map(srgb_hsla);
                let mut cells = vec![cell(w_m, m[i].clone()), cell(w_g, g[i].clone())];
                if with_cost {
                    cells.push(cell(
                        w_cost,
                        tool.cost.map(|c| format!("{c:.2}")).unwrap_or_default(),
                    ));
                }
                out.push(
                    table_row(
                        ("filament-row", usize::from(tool.tool)),
                        color,
                        false,
                        crate::tr_format!("Філамент {}", tool.tool + 1),
                        cells,
                        None,
                    )
                    .into_any_element(),
                );
            }
        }
        scheme => {
            let (lo, hi) = preview.built.range.unwrap_or((0.0, 0.0));
            let digits = if hi - lo < 2.0 {
                2
            } else if hi - lo < 20.0 {
                1
            } else {
                0
            };
            out.push(
                table_header(scheme.unit(), &[(COL_SHARE + 40., "Частка часу")], false)
                    .into_any_element(),
            );
            // Шкала згори вниз від максимуму до мінімуму, як в Orca; праворуч — частка часу.
            for step in (0..gcode::RANGE_COLORS.len()).rev() {
                let t = step as f32 / (gcode::RANGE_COLORS.len() - 1) as f32;
                let value = lo + (hi - lo) * t;
                let share = preview.shares.get(step).copied();
                out.push(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .mx(px(4.))
                        .px(px(6.))
                        .h(px(18.))
                        .child(
                            div()
                                .w(px(18.))
                                .h_full()
                                .flex_none()
                                .bg(srgb_hsla(gcode::RANGE_COLORS[step])),
                        )
                        .child(
                            mono(div())
                                .w(px(72.))
                                .flex_none()
                                .text_color(z.text_muted)
                                .child(format!(
                                    "{value:.digits$} {}",
                                    crate::i18n::tr(scheme.unit())
                                )),
                        )
                        .child(
                            div()
                                .flex_1()
                                .h(px(4.))
                                .rounded_full()
                                .bg(z.element_hover)
                                .child(
                                    div()
                                        .h_full()
                                        .rounded_full()
                                        .w(gpui::relative(share.unwrap_or(0.0).clamp(0.0, 1.0)))
                                        .bg(srgb_hsla(gcode::RANGE_COLORS[step]).opacity(0.8)),
                                ),
                        )
                        .child(
                            cell(COL_SHARE + 40., share.map(percent).unwrap_or_default())
                                .text_color(z.text_muted),
                        )
                        .child(div().w(px(14.)).flex_none())
                        .into_any_element(),
                );
            }
        }
    }
    out
}

/// Рядок підсумку: підпис ліворуч, значення праворуч.
fn stat_row(label: &str, value: String) -> gpui::Div {
    let z = zed();
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .min_h(px(24.))
        .px(px(10.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .text_color(z.text_muted)
                .child(crate::i18n::tr(label)),
        )
        .child(
            div()
                .font_family(crate::theme::MONO_FONT)
                .text_sm()
                .flex_none()
                .child(value),
        )
}

/// «Загальна оцінка» Bambu: філамент, вартість і час; рядки без даних не показуються.
fn estimation(preview: &Preview) -> Vec<gpui::AnyElement> {
    let z = zed();
    let r = &preview.report;
    let mut rows = vec![ui::group_title("Загальна оцінка").into_any_element()];
    let filament = |(m, g): (f64, f64)| crate::tr_format!("{m:.2} м  {g:.2} г");
    if let Some(total) = r.total_filament {
        rows.push(stat_row("Увесь філамент", filament(total)).into_any_element());
    }
    if let Some(model) = r.model_filament {
        rows.push(stat_row("Філамент моделі", filament(model)).into_any_element());
    }
    if let Some(cost) = r.cost {
        rows.push(stat_row("Вартість", format!("{cost:.2}")).into_any_element());
    }
    if let Some(p) = r.prepare {
        rows.push(stat_row("Підготовка", short_time(p)).into_any_element());
    }
    if let Some(t) = r.timelapse {
        rows.push(stat_row("Таймлапс", short_time(t)).into_any_element());
    }
    rows.push(stat_row("Друк моделі", short_time(r.model_time)).into_any_element());
    rows.push(stat_row("Загальний час", short_time(r.total_time)).into_any_element());
    rows.push(stat_row("Шарів", preview.toolpath.layer_count().to_string()).into_any_element());
    let note = match (r.rows_source, r.total_source) {
        (gcode::Source::Engine, _) => {
            "Час за типами й загальний — з рушія нарізки; таймлапс і філамент за типами — з G-code."
        }
        (gcode::Source::Estimate, gcode::Source::Engine) => {
            "Загальний час — з рушія; час за типами — оцінка Slicenv з G-code. Частки — від суми рядків і переміщень."
        }
        _ => "Час — оцінка Slicenv з G-code (даних рушія немає).",
    };
    rows.push(
        div()
            .px(px(10.))
            .pt(px(2.))
            .text_xs()
            .text_color(z.text_placeholder)
            .child(crate::i18n::tr(note))
            .into_any_element(),
    );
    rows
}

/// Лівий док: схема кольорів, легенда-таблиця, оцінка, відображення, експорт і друк.
fn summary_panel(ws: &Workspace, cx: &mut Context<Workspace>) -> gpui::AnyElement {
    let z = zed();
    let Some(preview) = &ws.preview else {
        return div().into_any_element();
    };
    let mut legend = legend_section(ws, preview.options.scheme, cx);
    if preview.options.scheme != ColorScheme::LineType {
        legend.extend(legend_section(ws, ColorScheme::LineType, cx));
    }

    let can_print = ws.can_send_print();
    let photo = preview.photo_on();
    if photo {
        legend = crate::photo_mode::panel(ws, cx);
    }
    ui::dock()
        .child(ui::panel_header(if photo { "Фото" } else { "Нарізка" }, Vec::new()))
        .child(
            div()
                .id("preview-summary")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .pb(px(12.))
                .when(!photo, |d| d.child(ui::prop_row("Схема кольорів", preview.scheme.clone())))
                .children(legend)
                .when(!photo, |d| d.child(ui::ghost_button("preview-show-all", Some(IconName::Eye), "Показати всі типи ліній")
                    .mx(px(4.))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(p) = &mut this.preview { p.options.hidden.retain(|role| gcode::ViewOptions::HIDDEN_BY_DEFAULT.contains(role)); }
                        this.rebuild_toolpath(cx);
                    }))))
                .child(ui::divider().mt(px(6.)))
                .children(estimation(preview))
                .child(ui::divider().mt(px(6.)))
                .children(display_controls(ws, cx))
                .child(
                    div()
                        .px(px(10.))
                        .text_xs()
                        .text_color(z.text_placeholder)
                        .child(crate::i18n::tr("↑ ↓ — шар, ← → — рух у шарі (Shift — по 10). Клік по типу лінії ховає його.")),
                ),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .gap(px(6.))
                .p(px(10.))
                .border_t_1()
                .border_color(z.border_variant)
                .map(|d| {
                    if photo {
                        d.child(crate::photo_mode::save_button(ws, cx))
                    } else {
                        d.child(
                            ui::ghost_button("export-gcode", Some(IconName::Download), "Експортувати…")
                                .border_1()
                                .border_color(z.border_variant)
                                .on_click(cx.listener(|this, _, window, cx| this.prompt_export(window, cx))),
                        )
                    }
                })
                .child(div().flex_1())
                .child(
                    ui::accent_button("preview-print", IconName::Send, "Надіслати на друк", can_print)
                        .when(can_print, |d| d.on_click(cx.listener(|this, _, window, cx| this.open_print_dialog(window, cx))))
                        .when_some(ws.print_blocker(), |d, why| {
                            d.tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(why.clone()).build(window, cx))
                        }),
                ),
        )
        .into_any_element()
}

/// Display switches do not rebuild the extrusion buffer.
fn display_controls(ws: &Workspace, cx: &mut Context<Workspace>) -> Vec<gpui::AnyElement> {
    let Some(p) = &ws.preview else {
        return Vec::new();
    };
    let mut rows = vec![ui::prop_row("Якість", p.quality.clone()).into_any_element()];
    for (i, (label, on)) in [
        ("Лише поточний шар", p.single_layer),
        ("Маркер сопла", p.show_nozzle),
        ("Стіл і сітка", p.show_bed),
        ("Плоскі лінії", p.flat_lines),
    ]
    .into_iter()
    .enumerate()
    {
        // У «Фото» маркера, сітки й плоских ліній немає: стіл і підтримки — у панелі режиму.
        if p.photo_on() && i != 0 {
            continue;
        }
        rows.push(
            div()
                .flex()
                .items_center()
                .justify_between()
                .h(px(32.))
                .px(px(10.))
                .child(div().text_sm().child(crate::i18n::tr(label)))
                .child(
                    crate::widgets::switch(("preview-display", i), on).on_click(cx.listener(
                        move |this, _, _, cx| {
                            if let Some(p) = &mut this.preview {
                                match i {
                                    0 => p.single_layer = !on,
                                    1 => p.show_nozzle = !on,
                                    2 => {
                                        p.show_bed = !on;
                                        this.renderer.set_show_bed(!on);
                                    }
                                    _ => {
                                        p.flat_lines = !on;
                                        this.renderer.set_flat_toolpath(!on);
                                    }
                                }
                            }
                            this.apply_preview_view(cx);
                            cx.notify();
                        },
                    )),
                )
                .into_any_element(),
        );
    }
    rows
}

/// Картка невдалої нарізки: вердикт (хто виправляє), заголовок, порада, текст і код
/// рушія, деталі з логу й дії.
fn failure_card(
    text: SharedString,
    failure: Option<std::sync::Arc<slicer::SliceFailure>>,
    can_slice: bool,
    exhausted: bool,
    fallback: Option<crate::engines::SliceFallback>,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    use slicer::FailureKind;
    let z = zed();
    let tr = crate::i18n::tr;
    let (kind, title, hint, reason, code, details, log, engine) = match &failure {
        Some(f) => (
            f.kind,
            tr(f.title),
            tr(f.hint),
            f.reason.clone(),
            f.code.map(|c| crate::tr_format!("код {}", c)).or(f.signal.map(|s| {
                // Windows: NTSTATUS аварії (0xC0000005), а не «сигнал −1073741819».
                if cfg!(windows) { format!("0x{:08X}", s as u32) } else { crate::tr_format!("сигнал {}", s) }
            })),
            f.details.clone(),
            Some(f.log.clone()),
            match f.engine.as_str() {
                "bambu" => "Bambu Studio".to_owned(),
                "orca" => "OrcaSlicer".to_owned(),
                other => other.to_owned(),
            },
        ),
        // Помилка до запуску рушія (немає профілю, файлу…) — показуємо як є.
        None => (
            FailureKind::System,
            tr("Нарізка не вдалася"),
            String::new(),
            text.to_string(),
            None,
            Vec::new(),
            None,
            String::new(),
        ),
    };
    let (verdict, verdict_color, icon) = match kind {
        FailureKind::Model => (tr("Виправ модель або налаштування"), z.warning, IconName::TriangleAlert),
        FailureKind::System => (tr("Проблема рушія або системи"), z.error, IconName::CircleAlert),
        FailureKind::App => (tr("Помилка Slicenv — не твоя"), z.error, IconName::Bug),
    };
    // Без `error_string` рушій нічого не повідомив: для аварії так і кажемо, інакше рядок ховаємо.
    let reason = if !reason.is_empty() {
        reason
    } else if failure.as_ref().is_some_and(|f| f.crashed) {
        match &code {
            Some(code) => crate::tr_format!("Рушій аварійно завершився ({}) до того, як повідомив причину", code),
            None => tr("Рушій аварійно завершився до того, як повідомив причину"),
        }
    } else {
        String::new()
    };
    let report = text.to_string();
    // Коди, коли рушій прочитав проєкт, але відкинув його налаштування: допомагає перевірка
    // сумісності або імпорт лише геометрії з налаштуваннями поточних профілів.
    let code_of = failure.as_ref().and_then(|f| f.code);
    let settings_problem =
        code_of.is_some_and(|c| matches!(c, -15 | -16 | -17 | -18 | -25 | -51));
    // Новіша версія 3MF: допомагає лише дозвіл рушію відкривати такі файли.
    let newer_file = code_of == Some(-24);
    div()
        .flex()
        .size_full()
        .items_center()
        .justify_center()
        .p(px(24.))
        .child(
            div()
                .flex()
                .flex_col()
                .w(px(560.))
                .max_w_full()
                .gap(px(12.))
                .p(px(16.))
                .rounded(px(6.))
                .border_1()
                .border_color(z.border)
                .bg(z.panel)
                // Вердикт і код.
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(ui::icon(icon).text_color(verdict_color))
                        .child(div().text_xs().font_weight(gpui::FontWeight::MEDIUM).text_color(verdict_color).child(verdict))
                        .child(div().flex_1())
                        .when_some(code, |d, code| {
                            d.child(
                                div()
                                    .px(px(6.))
                                    .rounded(px(3.))
                                    .bg(z.element)
                                    .text_xs()
                                    .font_family(crate::theme::MONO_FONT)
                                    .text_color(z.text_muted)
                                    .child(if engine.is_empty() { code } else { format!("{engine} · {code}") }),
                            )
                        }),
                )
                .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
                // Що зробити.
                .when(!hint.is_empty(), |d| {
                    d.child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .p(px(10.))
                            .rounded(px(4.))
                            .bg(z.accent_bg)
                            .border_1()
                            .border_color(z.accent_border)
                            .child(ui::icon(IconName::Lightbulb).text_color(z.accent))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(2.))
                                    .child(div().text_xs().font_weight(gpui::FontWeight::MEDIUM).text_color(z.accent).child(tr("Що зробити")))
                                    .child(div().text_sm().child(hint)),
                            ),
                    )
                })
                // Слова рушія і конкретика з логу.
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .when(!reason.is_empty(), |d| {
                            d.child(div().text_xs().text_color(z.text_muted).child(tr("Повідомлення рушія")))
                                .child(div().text_sm().child(reason))
                        })
                        .when(!details.is_empty(), |d| {
                            d.child(
                                div()
                                    .mt(px(4.))
                                    .p(px(8.))
                                    .rounded(px(4.))
                                    .bg(z.editor)
                                    .max_h(px(140.))
                                    .overflow_hidden()
                                    .text_xs()
                                    .font_family(crate::theme::MONO_FONT)
                                    .text_color(z.text_muted)
                                    .children(details.into_iter().map(|l| div().child(l))),
                            )
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(6.))
                        .when_some(fallback, |d, fallback| match fallback {
                            crate::engines::SliceFallback::Switch { engine, name } => d.child(
                                ui::accent_button(
                                    "failed-slice-other-engine",
                                    IconName::Scissors,
                                    crate::tr_format!("Нарізати в {name}", name = name.as_str()),
                                    true,
                                )
                                .tooltip(|window, cx| {
                                    gpui_component::tooltip::Tooltip::new(crate::i18n::tr(
                                        "Зробити цей рушій активним і одразу нарізати пластину",
                                    ))
                                    .build(window, cx)
                                })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.slice_in_engine(engine.clone(), window, cx)
                                })),
                            ),
                            crate::engines::SliceFallback::Install { name } => d.child(
                                ui::accent_button(
                                    "failed-slice-install-engine",
                                    IconName::Download,
                                    crate::tr_format!("Встановити {name}", name = name.as_str()),
                                    true,
                                )
                                .on_click(cx.listener(|this, _, window, cx| this.open_packages(window, cx))),
                            ),
                        })
                        .when(newer_file, |d| {
                            d.child(
                                ui::accent_button("failed-slice-newer", IconName::Scissors, "Дозволити новіші 3MF і нарізати", true)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.allow_newer_file = true;
                                        this.start_slice(window, cx);
                                    })),
                            )
                        })
                        .when(settings_problem, |d| {
                            d.child(
                                ui::accent_button("failed-slice-compat", IconName::SlidersHorizontal, "Перевірити сумісність", true)
                                    .on_click(cx.listener(|this, _, window, cx| this.open_migration(window, cx))),
                            )
                            .child(
                                ui::ghost_button("failed-slice-geometry", Some(IconName::Box), "Імпортувати лише геометрію")
                                    .tooltip(|window, cx| {
                                        gpui_component::tooltip::Tooltip::new(crate::i18n::tr(
                                            "Залишити об’єкти, їх розміщення й пластини, а налаштування взяти з вибраних профілів принтера, філаменту й процесу",
                                        ))
                                        .build(window, cx)
                                    })
                                    .on_click(cx.listener(|this, _, window, cx| this.keep_geometry_only(window, cx))),
                            )
                        })
                        .child(
                            (if settings_problem || newer_file || exhausted {
                                ui::ghost_button("failed-slice-retry", Some(IconName::Scissors), "Нарізати знову").when(!can_slice, |d| ui::Toggle::inert(d, true))
                            } else {
                                ui::accent_button("failed-slice-retry", IconName::Scissors, "Нарізати знову", can_slice)
                            })
                                .when(can_slice, |d| {
                                    d.on_click(cx.listener(|this, _, window, cx| this.start_slice(window, cx)))
                                }),
                        )
                        .when_some(log, |d, log| {
                            d.child(
                                ui::ghost_button("failed-slice-log", Some(IconName::FolderOpen), "Відкрити лог")
                                    .on_click(move |_, _, _| {
                                        let _ = crate::file_menu::reveal(&log);
                                    }),
                            )
                        })
                        .child(
                            ui::ghost_button("failed-slice-copy", Some(IconName::Copy), "Копіювати звіт")
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(report.clone()))
                                }),
                        ),
                ),
        )
}

#[cfg(test)]
mod tests {
    use super::slice_stage_text;

    #[test]
    fn gcode_stage_shows_layer_number() {
        let gcode = crate::i18n::tr("Генерую G-code").to_string();
        assert_eq!(slice_stage_text("Generating G-code", Some(80), None), crate::tr_format!("{}: шар {}", gcode, 80));
        assert_eq!(slice_stage_text("Generating G-code", None, None), gcode);
        assert_eq!(
            slice_stage_text("Detect overhangs for auto-lift", None, None),
            crate::i18n::tr("Шукаю нависання").to_string()
        );
        assert_eq!(slice_stage_text("Something new", None, None), "Something new");
    }
}
