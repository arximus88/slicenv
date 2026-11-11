//! Installed engine selection. Downloading/updating packages is a separate workflow.
use crate::ui::Toggle as _;
use crate::{
    theme::zed,
    ui::{self, IconName},
    widgets::{self, ToastKind},
    workspace::Workspace,
};
use gpui::{Context, SharedString, Window, div, prelude::*, px};
use slicer::{Engine, EngineKind, PresetStore};
use std::{
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex, OnceLock},
};

fn config_path() -> PathBuf {
    paths::config_dir().join("engines.json")
}
fn read_config() -> serde_json::Value {
    std::fs::read(config_path())
        .ok()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default()
}
fn decode(value: &serde_json::Value) -> Option<Engine> {
    Engine::decode(value)
}
fn encode(engine: &Engine) -> serde_json::Value {
    serde_json::json!({"kind":engine.kind.name(), "path":engine.path})
}
fn write_config(config: &serde_json::Value) -> anyhow::Result<()> {
    let path = config_path();
    std::fs::create_dir_all(path.parent().unwrap())?;
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    serde_json::to_writer_pretty(&mut file, config)?;
    file.persist(path)?;
    Ok(())
}
/// Оновлює або додає запис у `installed`; ручна позначка й версія не губляться.
fn upsert(
    config: &mut serde_json::Value,
    engine: &Engine,
    version: Option<&str>,
    manual: bool,
) -> serde_json::Value {
    let mut installed = config["installed"].as_array().cloned().unwrap_or_default();
    let same = |v: &serde_json::Value| decode(v).is_some_and(|e| e.kind == engine.kind && e.path == engine.path);
    let previous = installed.iter().find(|v| same(v)).cloned();
    installed.retain(|v| decode(v).is_some_and(|e| e.path.is_file()) && !same(v));
    let mut value = encode(engine);
    let old = previous.as_ref();
    if let Some(version) = version.or_else(|| old.and_then(|v| v["version"].as_str())) {
        value["version"] = version.into();
    }
    if manual || old.is_some_and(|v| v["manual"] == true) {
        value["manual"] = true.into();
    }
    installed.push(value.clone());
    config["installed"] = installed.into();
    value
}
fn remember(engine: &Engine, version: &str) -> anyhow::Result<()> {
    let mut config = read_config();
    if !config.is_object() {
        config = serde_json::json!({});
    }
    let value = upsert(&mut config, engine, Some(version), false);
    config["active"] = value;
    write_config(&config)
}
/// Додає рушій у список одразу після вибору, незалежно від того, чи вдасться активація.
pub(crate) fn add_installed(engine: &Engine, version: Option<&str>, manual: bool) -> anyhow::Result<()> {
    let mut config = read_config();
    if !config.is_object() {
        config = serde_json::json!({});
    }
    upsert(&mut config, engine, version, manual);
    write_config(&config)
}
/// Рушій вказано вручну (через «Вказати виконуваний файл…»).
pub(crate) fn is_manual(engine: &Engine) -> bool {
    read_config()["installed"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|v| v["manual"] == true && decode(v).is_some_and(|e| e.kind == engine.kind && e.path == engine.path))
}
/// Той самий вибір, що й у `slicenv-mcp` (`Engine::startup`).
pub fn startup_engine() -> anyhow::Result<Engine> {
    Engine::startup_in(&config_path(), &crate::engine_manager::engines_dir())
}
pub(crate) fn installed() -> Vec<Engine> {
    let mut result: Vec<_> = read_config()["installed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(decode)
        .filter(|e| e.path.is_file())
        .collect();
    // Версії, встановлені менеджером, — навіть якщо запис у engines.json загубився.
    for (family, path) in crate::engine_manager::managed_executables(&Default::default()) {
        let kind = if family == "orca" { EngineKind::Orca } else { EngineKind::Bambu };
        if !result.iter().any(|e| e.kind == kind && e.path == path) {
            result.push(Engine { kind, path });
        }
    }
    for kind in [EngineKind::Bambu, EngineKind::Orca] {
        if let Ok(engine) = Engine::discover(kind)
            && !result
                .iter()
                .any(|e| e.kind == engine.kind && e.path == engine.path)
        {
            result.push(engine);
        }
    }
    result.sort_by(|a, b| (a.kind.name(), &a.path).cmp(&(b.kind.name(), &b.path)));
    result
}

/// Версія з `engines.json` (запам’ятовується після першої активації); `None` — ще не запускали.
pub(crate) fn known_version(engine: &Engine) -> Option<String> {
    read_config()["installed"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|v| decode(v).is_some_and(|e| e.kind == engine.kind && e.path == engine.path))
        .and_then(|v| v["version"].as_str())
        .map(str::to_owned)
}

fn catalogs() -> &'static Mutex<std::collections::HashMap<String, Arc<settings::mo::Catalog>>> {
    static CATALOGS: OnceLock<Mutex<std::collections::HashMap<String, Arc<settings::mo::Catalog>>>> =
        OnceLock::new();
    CATALOGS.get_or_init(Default::default)
}

/// Український каталог рушія (`EngineInfo::identity`), прочитаний під час його активації.
pub(crate) fn engine_catalog(identity: &str) -> Option<Arc<settings::mo::Catalog>> {
    catalogs().lock().unwrap().get(identity).cloned()
}

// Cache each binary's runtime-filtered schema for the lifetime of its editors.
pub(crate) fn runtime_schema(
    engine: &Engine,
    identity: &str,
) -> anyhow::Result<&'static settings::Schema> {
    static SCHEMAS: OnceLock<Mutex<std::collections::HashMap<String, &'static settings::Schema>>> =
        OnceLock::new();
    let schemas = SCHEMAS.get_or_init(Default::default);
    if let Some(schema) = schemas.lock().unwrap().get(identity) {
        return Ok(*schema);
    }
    let defaults = engine.export_defaults()?;
    let mut schema = match engine.kind {
        EngineKind::Bambu => settings::Schema::load()?,
        EngineKind::Orca => settings::Schema::load_orca()?,
    };
    schema.options.retain(|key, _| defaults.contains_key(key));
    // Never offer a setting's new source default to an older executable.
    for (key, def) in &mut schema.options {
        def.default = defaults[key].clone();
    }
    // A known key can have changed type/enums between releases. An actual
    // runtime default rejected by the source catalog is not a verified editor.
    let mismatched: Vec<_> = schema
        .options
        .iter()
        .filter(|(key, def)| schema.validate(key, &def.default).is_err())
        .map(|(key, _)| key.clone())
        .collect();
    for key in mismatched {
        schema.options.remove(&key);
    }
    for pages in schema.layout.values_mut() {
        for page in pages {
            for group in &mut page.groups {
                for line in &mut group.lines {
                    line.options.retain(|k| schema.options.contains_key(k));
                }
            }
        }
    }
    // Підписи, підказки й категорії — словами самого рушія з його `resources/i18n/uk/*.mo`
    // (у схемі Orca власних українських підписів немає). Вивірені переклади схеми лишаються.
    let catalog = Arc::new(settings::mo::engine_catalog(&engine.path, "uk"));
    catalogs().lock().unwrap().insert(identity.to_owned(), catalog.clone());
    let mut filled = schema.fill_ukrainian(&catalog);
    if engine.kind == EngineKind::Orca {
        filled += schema.fill_ukrainian_from(&settings::Schema::load()?);
    }
    eprintln!("[i18n] {}: {} рядків перекладу рушія, заповнено {filled}", engine.kind.name(), catalog.len());
    let schema: &'static settings::Schema = Box::leak(Box::new(schema));
    schemas.lock().unwrap().insert(identity.to_owned(), schema);
    Ok(schema)
}

/// Незбережена зміна пресета: що переносимо на інший рушій.
#[derive(Clone, Debug)]
pub(crate) struct PresetEdit {
    pub kind: settings::PresetType,
    pub key: String,
    pub old: Option<serde_json::Value>,
    pub new: serde_json::Value,
}

/// Стан підготовки профілів для рушія без JSON-каталогу (див. [`crate::profile_fetch`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum ProfileFetch {
    #[default]
    Idle,
    Running { done: usize, total: usize },
    Failed(String),
}

/// «OrcaSlicer 2.5.0» для повідомлень.
pub(crate) fn engine_name(kind: EngineKind, version: &str) -> String {
    let name = match kind {
        EngineKind::Orca => "OrcaSlicer",
        EngineKind::Bambu => "Bambu Studio",
    };
    format!("{name} {}", slicer::version_number(version)).trim_end().to_owned()
}

/// Номер версії як числа: «OrcaSlicer 2.5.0» → [2, 5, 0], «02.09.01.70» → [2, 9, 1, 70].
fn version_numbers(version: &str) -> Vec<u64> {
    let number = slicer::version_number(version);
    let start = number.find(|c: char| c.is_ascii_digit()).unwrap_or(number.len());
    number[start..]
        .split(|c: char| !c.is_ascii_digit())
        .take_while(|p| !p.is_empty())
        .filter_map(|p| p.parse().ok())
        .collect()
}

/// Нічні збірки, бети й кандидати у випуск — не перший вибір для «нарізати в іншому рушії».
fn stable_version(version: &str) -> bool {
    !slicer::version_number(version)
        .to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|t| ["nightly", "beta", "alpha", "rc", "dev", "preview"].iter().any(|w| t.starts_with(w)))
}

fn same_engine(a: &Engine, b: &Engine) -> bool {
    a.kind == b.kind
        && (a.path == b.path
            || std::fs::canonicalize(&a.path).ok().is_some_and(|p| std::fs::canonicalize(&b.path).ok() == Some(p)))
}

/// Встановлені рушії з версіями з `engines.json` або маніфесту менеджера (порожня — невідома).
pub(crate) fn installed_with_versions() -> Vec<(Engine, String)> {
    installed()
        .into_iter()
        .map(|engine| {
            let version = known_version(&engine)
                .or_else(|| crate::engine_manager::manifest_version(&engine.path))
                .unwrap_or_default();
            (engine, version)
        })
        .collect()
}

/// Шлях коротко: «C:\…\2.5.0\orca-slicer.exe» (корінь і два останні елементи).
fn short_path(path: &str, max: usize) -> String {
    if path.chars().count() <= max {
        return path.to_owned();
    }
    let sep = if path.contains('\\') { '\\' } else { '/' };
    let parts: Vec<&str> = path.split(['/', '\\']).collect();
    if parts.len() <= 3 {
        return path.to_owned();
    }
    let head = if parts[0].is_empty() { String::new() } else { parts[0].to_owned() };
    format!("{head}{sep}…{sep}{}{sep}{}", parts[parts.len() - 2], parts[parts.len() - 1])
}

/// Звідки рушій — дрібним рядком під назвою (повний шлях — у підказці).
#[derive(Debug, PartialEq)]
enum EngineSource {
    Managed,
    /// Скорочений шлях до виконуваного файлу.
    Manual(String),
    /// Скорочена тека встановлення.
    System(String),
}

impl EngineSource {
    fn of(managed: bool, manual: bool, path: &str) -> Self {
        if managed {
            Self::Managed
        } else if manual {
            Self::Manual(short_path(path, 48))
        } else {
            let dir = path.rsplit_once(['/', '\\']).map_or(path, |(dir, _)| dir);
            Self::System(short_path(dir, 40))
        }
    }

    fn text(&self) -> String {
        match self {
            Self::Managed => crate::i18n::tr("Встановлено Slicenv"),
            Self::Manual(path) => crate::tr_format!("Вказано вручну · {path}", path = path.as_str()),
            Self::System(dir) => crate::tr_format!("Системний ({dir})", dir = dir.as_str()),
        }
    }
}

/// Інший встановлений рушій, коли поточний падає навіть у сумісному режимі: стабільний
/// найновіший, у межах цього — того самого виду (Orca → Orca). Поточний рушій і та сама
/// версія того самого виду (копія за іншим шляхом) не підходять. `candidates` — рушій і його
/// версія з `engines.json` (порожня, якщо невідома).
pub(crate) fn alternative_engine(
    current: &Engine,
    current_version: &str,
    candidates: &[(Engine, String)],
) -> Option<(Engine, String)> {
    let current_numbers = version_numbers(current_version);
    candidates
        .iter()
        .filter(|(engine, version)| {
            !same_engine(engine, current)
                && !(engine.kind == current.kind
                    && !current_numbers.is_empty()
                    && version_numbers(version) == current_numbers)
        })
        .max_by_key(|(engine, version)| {
            (stable_version(version), engine.kind == current.kind, version_numbers(version))
        })
        .cloned()
}

/// Головна дія картки, коли рушій падає навіть у сумісному режимі.
#[derive(Clone, Debug)]
pub(crate) enum SliceFallback {
    /// Перейти на інший встановлений рушій і одразу нарізати; `name` — «OrcaSlicer 2.5.0».
    Switch { engine: Engine, name: String },
    /// Інших рушіїв немає: відкрити менеджер рушіїв; `name` — «OrcaSlicer 2.5.0» або «OrcaSlicer».
    Install { name: String },
}

/// Блок «Готую профілі OrcaSlicer 2.5.0…» / «Спробувати знову» для панелі «Друк» і майстра.
pub(crate) fn profile_status(ws: &Workspace, cx: &mut Context<Workspace>) -> Option<gpui::AnyElement> {
    let (engine, version, _) = ws.profile_recovery.as_ref()?;
    let z = zed();
    let name = engine_name(engine.kind, version);
    let body = div().flex().flex_col().gap(px(6.)).py(px(6.));
    Some(match &ws.profile_fetch {
        ProfileFetch::Failed(_) => body
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(6.))
                    .child(ui::icon(IconName::TriangleAlert).text_color(z.warning))
                    .child(div().text_xs().child(crate::tr_format!(
                        "Не вдалося завантажити профілі принтерів для {name}. Перевірте інтернет і спробуйте ще раз.",
                        name = name.as_str()
                    ))),
            )
            .child(
                div().flex().child(
                    ui::ghost_button("fetch-profiles-retry", Some(IconName::RefreshCw), "Спробувати знову")
                        .border_1()
                        .border_color(z.border_variant)
                        .inert(ws.engine_loading)
                        .on_click(cx.listener(|this, _, window, cx| this.fetch_engine_profiles(window, cx))),
                ),
            )
            .into_any_element(),
        state => {
            let (done, total) = match state {
                ProfileFetch::Running { done, total } => (*done, *total),
                _ => (0, 0),
            };
            body.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(ui::spinner("profiles-busy"))
                    .child(div().text_xs().child(crate::tr_format!(
                        "Готую профілі {name}…",
                        name = name.as_str()
                    ))),
            )
            .child(if total > 0 && done > 0 {
                widgets::progress(done as f32 * 100. / total as f32).into_any_element()
            } else {
                widgets::progress_indeterminate("profiles-progress")
            })
            .child(div().text_xs().text_color(z.text_muted).child(crate::i18n::tr(
                "Одноразово: беремо профілі принтерів з офіційного випуску рушія. Тека рушія не змінюється.",
            )))
            .into_any_element()
        }
    })
}

/// Що робити з незбереженими змінами пресетів при зміні рушія.
#[derive(Clone)]
pub(crate) enum Carry {
    /// Змін немає або ще не питали — спитати діалогом.
    Ask,
    Edits(Vec<PresetEdit>),
    Discard,
}

fn kind_title(kind: settings::PresetType) -> &'static str {
    match kind {
        settings::PresetType::Process => "Процес",
        settings::PresetType::Filament => "Філамент",
        settings::PresetType::Machine => "Принтер",
    }
}

/// Значення для показу в діалозі: перший елемент вектора, без лапок.
fn shown(value: Option<&serde_json::Value>) -> String {
    match value {
        // Значення на кшталт G-code — багаторядкові: у переліку змін лише початок першого рядка.
        Some(serde_json::Value::String(s)) => {
            let line = s.lines().next().unwrap_or_default().trim();
            let mut short: String = line.chars().take(40).collect();
            if short.len() < s.trim().len() {
                short.push('…');
            }
            short
        }
        Some(serde_json::Value::Array(items)) => shown(items.first()),
        Some(other) => other.to_string(),
        None => "—".into(),
    }
}

impl Workspace {
    /// Усі незбережені зміни в редакторах пресетів.
    pub(crate) fn pending_edits(&self) -> Vec<PresetEdit> {
        use settings::PresetType::*;
        [Process, Filament, Machine]
            .into_iter()
            .filter_map(|kind| self.editor(kind).map(|e| (kind, e)))
            .flat_map(|(kind, editor)| {
                editor
                    .preset
                    .dirty_keys()
                    .map(|key| PresetEdit {
                        kind,
                        key: key.to_owned(),
                        old: editor.preset.base().get(key).cloned(),
                        new: editor.preset.value(key).cloned().unwrap_or_default(),
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// «Процес — Top surface pattern (Monotonic → Spiral Inset)».
    fn edit_line(&self, edit: &PresetEdit) -> String {
        let label = self
            .settings_schema
            .options
            .get(&edit.key)
            .map(|d| {
                let label = d.label(crate::i18n::locale());
                if label.is_empty() { d.full_label(crate::i18n::locale()) } else { label }.to_owned()
            })
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| edit.key.clone());
        format!(
            "{} — {label} ({} → {})",
            crate::i18n::tr(kind_title(edit.kind)),
            shown(edit.old.as_ref()),
            shown(Some(&edit.new))
        )
    }

    pub fn activate_engine(
        &mut self,
        candidate: anyhow::Result<Engine>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.switch_engine(candidate, Carry::Ask, window, cx)
    }

    /// Діалог у момент зміни рушія: незбережені зміни й чотири дії замість глухого тосту.
    fn open_engine_switch_dialog(&mut self, engine: Engine, window: &mut Window, cx: &mut Context<Self>) {
        let edits = self.pending_edits();
        let lines: Vec<String> = edits.iter().map(|e| self.edit_line(e)).collect();
        // Назва рушія, а не шлях до файлу: «OrcaSlicer 2.5.0».
        let title = crate::wizard::engine_title(engine.kind);
        let target = known_version(&engine)
            .or_else(|| crate::engine_manager::manifest_version(&engine.path))
            .filter(|v| !v.is_empty())
            .map_or_else(|| title.to_owned(), |v| format!("{title} {}", crate::wizard::version_text(&v)));
        self.open_modal(
            Rc::new(move |_, _, cx| {
                let z = zed();
                let engine = engine.clone();
                let edits = edits.clone();
                let mut list = div().flex().flex_col().gap(px(4.)).pl(px(8.));
                for line in lines.iter().take(12) {
                    list = list.child(div().text_sm().child(line.clone()));
                }
                if lines.len() > 12 {
                    list = list.child(
                        div()
                            .text_xs()
                            .text_color(z.text_muted)
                            .child(crate::tr_format!("…і ще {n}", n = lines.len() - 12)),
                    );
                }
                let carry = {
                    let engine = engine.clone();
                    let edits = edits.clone();
                    cx.listener(move |ws, _, window, cx| {
                        ws.close_modal(cx);
                        ws.switch_engine(Ok(engine.clone()), Carry::Edits(edits.clone()), window, cx)
                    })
                };
                let discard = {
                    let engine = engine.clone();
                    cx.listener(move |ws, _, window, cx| {
                        ws.close_modal(cx);
                        ws.switch_engine(Ok(engine.clone()), Carry::Discard, window, cx)
                    })
                };
                let save = cx.listener(move |ws, _, window, cx| {
                    let Some(kind) = ws.pending_edits().first().map(|e| e.kind) else {
                        return;
                    };
                    ws.close_modal(cx);
                    ws.pending_engine = Some(engine.clone());
                    ws.open_save_preset_dialog(kind, window, cx);
                });
                widgets::modal(
                    "Є незбережені зміни пресетів",
                    560.,
                    div()
                        .flex()
                        .flex_col()
                        .p(px(14.))
                        .gap(px(10.))
                        .child(div().text_sm().child(crate::tr_format!(
                            "Перед переходом на {target} вирішіть, що зробити зі змінами:",
                            target = target.as_str()
                        )))
                        .child(list)
                        .child(div().text_xs().text_color(z.text_muted).child(crate::i18n::tr(
                            "«Перенести» застосує значення там, де новий рушій знає цей параметр і таке значення; решту покажемо окремо.",
                        ))),
                    Some(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(6.))
                            .child(ui::ghost_button("engine-switch-cancel", None, "Скасувати")
                                .on_click(cx.listener(|ws, _, _, cx| {
                                    ws.slice_after_engine_switch = None;
                                    ws.close_modal(cx);
                                })))
                            .child(ui::ghost_button("engine-switch-discard", Some(IconName::Undo2), "Скинути")
                                .border_1().border_color(z.border_variant).on_click(discard))
                            .child(ui::ghost_button("engine-switch-save", Some(IconName::Save), "Зберегти як пресет…")
                                .border_1().border_color(z.border_variant).on_click(save))
                            .child(ui::accent_button("engine-switch-carry", IconName::ArrowRight, "Перенести на новий рушій", true)
                                .on_click(carry))
                            .into_any_element(),
                    ),
                )
            }),
            window,
            cx,
        );
    }

    fn switch_engine(
        &mut self,
        candidate: anyhow::Result<Engine>,
        carry: Carry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.engine_loading {
            return;
        }
        if self.packages.busy {
            self.toast(
                ToastKind::Warning,
                "Дочекайся завершення операції з пакетами рушіїв",
                cx,
            );
            return;
        }
        let candidate = match candidate {
            Ok(e) => e,
            Err(e) => {
                self.toast(ToastKind::Error, format!("{e:#}"), cx);
                return;
            }
        };
        let was_clean = self.pending_edits().is_empty();
        if matches!(carry, Carry::Ask) && !was_clean {
            self.open_engine_switch_dialog(candidate, window, cx);
            return;
        }
        if matches!(carry, Carry::Discard) {
            for editor in [
                self.process_editor.as_mut(),
                self.machine_editor.as_mut(),
                self.filament_editor.as_mut(),
            ]
            .into_iter()
            .flatten()
            {
                editor.preset.reset_all();
                editor.clear_controls();
            }
        }
        self.pending_engine = None;
        let carried = match &carry {
            Carry::Edits(edits) => edits.clone(),
            _ => Vec::new(),
        };
        // Зміни, які були на момент старту: якщо під час завантаження додалися нові — скасовуємо.
        let edits_at_start = self.pending_edits().len();
        // Нарізка іде старим рушієм — її результат усе одно застаріє. Раніше кнопки
        // були неактивні під час нарізки, і клік мовчки нічого не робив.
        if self.slicing() {
            self.cancel_slice();
        }
        self.profile_recovery = None;
        let recovery_candidate = candidate.clone();
        let initial = self.engine.is_none();
        self.engine_loading = true;
        self.engine_revision += 1;
        let revision = self.engine_revision;
        cx.spawn_in(window, async move |this, cx| {
            let (result, recovery_version) = cx
                .background_executor()
                .spawn(async move {
                    let version = PresetStore::engine_version(&candidate);
                    let result = (|| {
                        let info = candidate.probe()?;
                        let store = PresetStore::open_for_engine(&candidate)?;
                        let schema = runtime_schema(&candidate, &info.identity)?;
                        anyhow::Ok((candidate, info, schema, store))
                    })();
                    (result, version)
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                if this.engine_revision != revision {
                    return;
                }
                this.engine_loading = false;
                match result {
                    Ok((engine, info, schema, store)) => {
                        if this.pending_edits().len() != edits_at_start {
                            this.toast(
                                ToastKind::Warning,
                                "Зміну рушія скасовано: під час завантаження змінено пресет",
                                cx,
                            );
                            return;
                        }
                        // engines.json — лише для показу в списках рушіїв, тож туди йде `label`.
                        if let Err(e) = remember(&engine, &info.label) {
                            this.toast(
                                ToastKind::Error,
                                crate::tr_format!("Не вдалося зберегти вибір рушія: {e:#}"),
                                cx,
                            );
                        }
                        this.profile_recovery = None;
                        this.engine_label = info.label.into();
                        this.engine_identity = Some(info.identity);
                        this.engine = Some(engine);
                        this.allow_newer_file = false;
                        this.settings_schema = schema;
                        this.process_editor = None;
                        this.machine_editor = None;
                        this.filament_editor = None;
                        let saved = Some(this.preferences.printer.clone())
                            .filter(|m| !m.is_empty())
                            .and_then(|m| store.defaults_for_machine(&m));
                        let selection = if initial {
                            saved
                                .or_else(|| store.studio_selection())
                                .unwrap_or_else(|| this.selection.clone())
                        } else {
                            this.selection.clone()
                        };
                        this.presets = Some(Arc::new(store));
                        this.set_selection(selection, cx);
                        if initial {
                            this.apply_project_presets(cx);
                        }
                        this.close_modal(cx);
                        if !carried.is_empty() {
                            this.apply_carried_edits(carried, window, cx);
                        }
                        // «Нарізати в …» з картки невдалої нарізки: ріжемо щойно активованим рушієм.
                        if this
                            .slice_after_engine_switch
                            .take()
                            .is_some_and(|wanted| same_engine(&wanted, this.engine.as_ref().unwrap()))
                        {
                            this.start_slice(window, cx);
                        }
                    }
                    Err(e) if e.is::<slicer::ProfileCatalogMissing>() => {
                        // Не помилка для користувача: профілі готуються самі, тихо, з прогресом.
                        eprintln!("[engines] {e:#} — завантажую профілі з тегу випуску");
                        this.profile_recovery =
                            Some((recovery_candidate, recovery_version, format!("{e:#}")));
                        this.recovery_carry = Some(carry.clone());
                        this.profile_fetch = ProfileFetch::Idle;
                        this.fetch_engine_profiles(window, cx);
                    }
                    Err(e) => {
                        this.slice_after_engine_switch = None;
                        this.toast(
                            ToastKind::Error,
                            crate::tr_format!("Рушій не активовано: {e:#}"),
                            cx,
                        );
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Переносить зміни на щойно активований рушій: лише ключі, які знає його схема, і лише
    /// значення, які вона приймає. Що не перенеслося — показуємо списком.
    fn apply_carried_edits(&mut self, edits: Vec<PresetEdit>, window: &mut Window, cx: &mut Context<Self>) {
        let schema = self.settings_schema;
        let mut skipped = Vec::new();
        let mut applied = 0;
        for edit in edits {
            let known = schema.options.contains_key(&edit.key);
            let ok = known
                && self
                    .editor_mut(edit.kind)
                    .is_some_and(|editor| editor.preset.set(&edit.key, edit.new.clone()).is_ok());
            if ok {
                applied += 1;
            } else {
                skipped.push(edit);
            }
        }
        if applied > 0 {
            self.cancel_slice();
        }
        if skipped.is_empty() {
            self.toast(
                ToastKind::Success,
                crate::tr_format!("Рушій змінено, перенесено змін: {applied}"),
                cx,
            );
            return;
        }
        let lines: Vec<String> = skipped.iter().map(|e| self.edit_line(e)).collect();
        self.open_modal(
            Rc::new(move |_, _, cx| {
                let mut list = div().flex().flex_col().gap(px(4.)).pl(px(8.));
                for line in &lines {
                    list = list.child(div().text_sm().child(line.clone()));
                }
                widgets::modal(
                    "Перенесено не все",
                    540.,
                    div()
                        .flex()
                        .flex_col()
                        .p(px(14.))
                        .gap(px(10.))
                        .child(div().text_sm().child(crate::tr_format!(
                            "Перенесено змін: {applied}. Новий рушій не знає цих параметрів або не приймає таких значень — вони лишилися як у профілі:"
                        )))
                        .child(list),
                    Some(
                        ui::accent_button("carry-ok", IconName::Check, "Зрозуміло", true)
                            .on_click(cx.listener(|ws, _, _, cx| ws.close_modal(cx)))
                            .into_any_element(),
                    ),
                )
            }),
            window,
            cx,
        );
    }

    /// Завантажує JSON-профілі з git-тегу випуску й повторює активацію рушія.
    pub fn fetch_engine_profiles(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.engine_loading || self.slicing() {
            return;
        }
        let Some((engine, version, _)) = self.profile_recovery.clone() else {
            return;
        };
        let vendors = crate::profile_fetch::vendors_for(
            self.presets
                .as_ref()
                .and_then(|store| store.vendor_for_machine(&self.selection.machine)),
        );
        self.engine_loading = true;
        self.profile_fetch = ProfileFetch::Running { done: 0, total: 0 };
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<(usize, usize)>();
        cx.spawn_in(window, async move |this, cx| {
            let kind = engine.kind;
            let task = cx.background_executor().spawn(async move {
                let cancel = std::sync::atomic::AtomicBool::new(false);
                crate::profile_fetch::fetch(
                    kind,
                    &version,
                    &vendors,
                    &mut |done, total| {
                        tx.unbounded_send((done, total)).ok();
                    },
                    &cancel,
                )
            });
            use futures::StreamExt as _;
            while let Some((done, total)) = rx.next().await {
                this.update(cx, |this, cx| {
                    this.profile_fetch = ProfileFetch::Running { done, total };
                    cx.notify();
                })
                .ok();
            }
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.engine_loading = false;
                match result {
                    Ok(fetched) => {
                        eprintln!(
                            "[engines] профілі {} {}: {} файлів",
                            engine.kind.name(),
                            fetched.version,
                            fetched.files
                        );
                        this.profile_fetch = ProfileFetch::Idle;
                        let carry = this.recovery_carry.take().unwrap_or(Carry::Ask);
                        this.switch_engine(Ok(engine), carry, window, cx);
                    }
                    Err(e) => {
                        eprintln!("[engines] профілі не завантажено: {e:#}");
                        this.profile_fetch = ProfileFetch::Failed(format!("{e:#}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn prompt_engine(&mut self, kind: EngineKind, window: &mut Window, cx: &mut Context<Self>) {
        let paths = crate::file_menu::prompt_open(
            window,
            crate::file_menu::OpenKind::Engine,
            "Вибрати виконуваний файл рушія",
            false,
        );
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = paths.await.into_iter().next() else {
                return;
            };
            // Тип рушія — з імені файлу: у майстрі кнопка одна для обох.
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            let kind = if name.contains("bambu") {
                EngineKind::Bambu
            } else if name.contains("orca") {
                EngineKind::Orca
            } else {
                kind
            };
            let engine = Engine { kind, path };
            this.update_in(cx, |this, window, cx| {
                // У списку одразу, навіть якщо активація не вдасться чи чекає на рішення.
                if let Err(e) = add_installed(&engine, None, true) {
                    eprintln!("[engines] не вдалося записати рушій у список: {e:#}");
                }
                this.refresh_engine_lists();
                this.activate_engine(Ok(engine.clone()), window, cx)
            })
            .ok();
            let probe = engine.clone();
            let label = cx
                .background_executor()
                .spawn(async move { probe.probe().map(|info| info.label) })
                .await;
            if let Ok(label) = label {
                add_installed(&engine, Some(&label), true).ok();
                this.update(cx, |this, cx| {
                    this.refresh_engine_lists();
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Що запропонувати головною дією, коли рушій падає навіть у сумісному режимі.
    pub(crate) fn crash_fallback(&self) -> Option<SliceFallback> {
        let current = self.engine.as_ref()?;
        let candidates = installed_with_versions();
        if let Some((engine, version)) = alternative_engine(current, &self.engine_label, &candidates) {
            return Some(SliceFallback::Switch { name: engine_name(engine.kind, &version), engine });
        }
        if !crate::packages::MANAGER_SUPPORTED {
            return None;
        }
        let newest = crate::packages::newest_stable(&self.packages.releases, "orca")
            .and_then(|r| r["tag"].as_str())
            .map(|tag| crate::engine_manager::clean_tag(tag).to_owned());
        // Поточний рушій і є найновішим OrcaSlicer — ставити нічого.
        if current.kind == EngineKind::Orca
            && newest.as_deref().is_some_and(|v| version_numbers(v) == version_numbers(&self.engine_label))
        {
            return None;
        }
        Some(SliceFallback::Install { name: engine_name(EngineKind::Orca, newest.as_deref().unwrap_or("")) })
    }

    /// «Нарізати в OrcaSlicer 2.5.0»: перемкнути рушій (через діалог незбережених змін, якщо
    /// вони є) і нарізати, щойно він активується.
    pub(crate) fn slice_in_engine(&mut self, engine: Engine, window: &mut Window, cx: &mut Context<Self>) {
        self.slice_after_engine_switch = Some(engine.clone());
        self.activate_engine(Ok(engine), window, cx);
    }

    pub fn open_engines(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_modal(Rc::new(move |ws,_,cx| {
            let z = zed();
            let tr = crate::i18n::tr;
            let engines = installed_with_versions();
            let active = ws.engine.clone();
            let active_version = ws.engine_label.to_string();
            let active_crashes = ws.engine_identity.as_deref().is_some_and(|id| slicer::engine_compat(id).crashes_in_safe_mode);
            let better = active.as_ref().filter(|_| active_crashes)
                .and_then(|current| alternative_engine(current, &active_version, &engines));
            let mut body = div().flex().flex_col().p(px(12.)).gap(px(8.))
                .child(div().text_xs().text_color(z.text_muted).child(tr("Кожен рушій використовує власні профілі. Нові варіанти параметрів потребують перевірки; повна сумісність версій ще тестується.")));
            let mut list = div().flex().flex_col().gap(px(4.));
            for (i, (engine, version)) in engines.iter().enumerate() {
                let selected = active.as_ref().is_some_and(|e| same_engine(e, engine));
                let newer = !selected && active.as_ref().is_some_and(|a| {
                    a.kind == engine.kind && version_numbers(version) > version_numbers(&active_version)
                });
                let crashes = if selected { active_crashes } else { slicer::engine_crashes_in_safe_mode(engine) };
                let managed = crate::engine_manager::manifest_version(&engine.path).is_some();
                let source = EngineSource::of(managed, is_manual(engine), &engine.path.display().to_string()).text();
                let full_path: SharedString = engine.path.display().to_string().into();
                let candidate = engine.clone();
                let badge = |text: &'static str, color: gpui::Hsla| {
                    div().flex_none().px(px(6.)).rounded(px(3.)).border_1().border_color(color).text_xs().text_color(color).child(tr(text))
                };
                let mut row = widgets::radio(("engine-choice", i), selected, ws.engine_loading)
                    .w_full()
                    .gap(px(10.))
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(6.))
                    .border_1()
                    .map(|d| if selected { d.bg(z.accent_bg).border_color(z.accent_border) } else { d.border_color(z.border_variant).hover(|d| d.bg(z.element_hover)) })
                    .child(div().flex().flex_col().flex_1().min_w_0()
                        .child(div().text_base().font_weight(gpui::FontWeight::MEDIUM).child(engine_name(engine.kind, version)))
                        .child(div().text_xs().text_color(z.text_muted).truncate().child(source)))
                    .when(newer, |d| d.child(badge("Новіший", z.accent)))
                    .when(crashes, |d| d.child(badge("Падає", z.error)))
                    .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(full_path.clone()).build(window, cx));
                if !selected && !ws.engine_loading {
                    row = row.on_click(cx.listener(move |this,_,window,cx| this.activate_engine(Ok(candidate.clone()),window,cx)));
                }
                list = list.child(row);
                // Попередження — біля активного рядка, з переходом на кращий встановлений рушій.
                if selected && active_crashes {
                    let mut warning = div().flex().items_center().gap(px(8.)).px(px(10.)).py(px(6.))
                        .child(ui::icon(IconName::TriangleAlert).text_color(z.warning))
                        .child(div().flex_1().text_xs().text_color(z.warning).child(tr(if better.is_some() {
                            "Цей рушій аварійно завершувався навіть у сумісному режимі."
                        } else {
                            "Цей рушій аварійно завершувався навіть у сумісному режимі. Якщо нарізка не вдається, спробуйте іншу або новішу версію рушія."
                        })));
                    if let Some((target, target_version)) = better.clone() {
                        warning = warning.child(ui::accent_button("engine-switch-better", IconName::ArrowRight,
                            crate::tr_format!("Перейти на {name}", name = engine_name(target.kind, &target_version).as_str()), !ws.engine_loading)
                            .when(!ws.engine_loading, |d| d.on_click(cx.listener(move |this,_,window,cx| this.activate_engine(Ok(target.clone()),window,cx)))));
                    }
                    list = list.child(warning);
                }
            }
            body = body.child(list);
            if ws.engine_loading {
                body = body.child(div().flex().items_center().gap(px(6.)).child(ui::spinner("engine-switching"))
                    .child(div().text_xs().text_color(z.text_muted).child(tr("Перемикання рушія…"))));
            }
            let busy = ws.engine_loading || ws.slicing();
            body = body.child(ui::divider()).child(
                div().flex().items_center().gap(px(12.))
                    .child(div().flex().flex_col().flex_1()
                        .child(div().text_sm().child(tr("Дозволити новіші 3MF")))
                        .child(div().text_xs().text_color(z.text_muted).child(tr("Експериментально: рушій нарізки відкриє файл новішої версії, ніж він сам. Діє до перезапуску."))))
                    .child(widgets::switch("allow-newer-project", ws.allow_newer_file)
                        .when(busy, |d| d.opacity(0.5))
                        .when(!busy, |d| d.on_click(cx.listener(|this,_,_,cx| { this.allow_newer_file = !this.allow_newer_file; this.cancel_slice(); this.preview = None; cx.notify(); })))),
            );
            let mut actions = div().flex().flex_wrap().items_center().gap(px(6.))
                .child(div().text_xs().text_color(z.text_muted).child(tr("Додати вручну…")))
                .children([EngineKind::Bambu,EngineKind::Orca].into_iter().enumerate().map(|(i,kind)| {
                    ui::ghost_button(("engine-import",i),Some(IconName::FolderOpen),match kind { EngineKind::Bambu => "Bambu Studio…", EngineKind::Orca => "OrcaSlicer…" })
                        .border_1().border_color(z.border_variant)
                        .inert(ws.engine_loading).on_click(cx.listener(move |this,_,window,cx| this.prompt_engine(kind,window,cx)))
                }));
            let mut more = div().flex().flex_wrap().gap(px(6.));
            if crate::packages::MANAGER_SUPPORTED {
                more = more.child(ui::ghost_button("engine-packages",Some(IconName::Download),"Встановити / видалити версії…")
                    .border_1().border_color(z.border_variant)
                    .on_click(cx.listener(|this,_,window,cx| this.open_packages(window,cx))));
            }
            more = more.child(ui::ghost_button("engine-migration",Some(IconName::SlidersHorizontal),"Перевірити сумісність проєкту…")
                .border_1().border_color(z.border_variant)
                .on_click(cx.listener(|this,_,window,cx| this.open_migration(window,cx))));
            actions = actions.child(div().flex_1()).child(more);
            body = body.child(ui::divider()).child(actions);
            widgets::modal("Рушій нарізки",700.,body,None).into_any_element()
        }),window,cx);
    }
}

#[cfg(test)]
mod list_tests {
    use super::*;

    #[test]
    fn manual_engine_is_listed_without_becoming_active_and_keeps_its_mark() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("orca-slicer.exe");
        std::fs::write(&exe, b"").unwrap();
        let engine = Engine { kind: EngineKind::Orca, path: exe };
        let mut config = serde_json::json!({"active": {"kind": "orca", "path": "C:/old.exe"}});
        upsert(&mut config, &engine, None, true);
        assert_eq!(config["active"]["path"], "C:/old.exe", "вибір файлу не міняє активний рушій");
        let entry = &config["installed"][0];
        assert_eq!(entry["manual"], true);
        assert!(entry.get("version").is_none());
        // Версія з’являється пізніше; ручна позначка не губиться ні тут, ні при активації.
        upsert(&mut config, &engine, Some("OrcaSlicer 2.5.0"), false);
        let installed = config["installed"].as_array().unwrap();
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0]["version"], "OrcaSlicer 2.5.0");
        assert_eq!(installed[0]["manual"], true);
        upsert(&mut config, &engine, None, false);
        assert_eq!(config["installed"][0]["version"], "OrcaSlicer 2.5.0");
    }

    fn orca(path: &str) -> Engine {
        Engine { kind: EngineKind::Orca, path: path.into() }
    }

    fn bambu(path: &str) -> Engine {
        Engine { kind: EngineKind::Bambu, path: path.into() }
    }

    fn pick(current: &Engine, version: &str, candidates: &[(Engine, &str)]) -> Option<String> {
        let candidates: Vec<_> = candidates.iter().map(|(e, v)| (e.clone(), (*v).to_owned())).collect();
        alternative_engine(current, version, &candidates).map(|(e, _)| e.path.display().to_string())
    }

    #[test]
    fn alternative_engine_is_newest_stable_of_the_same_kind() {
        let system = orca("C:/Program Files/OrcaSlicer/orca-slicer.exe");
        let candidates = [
            (system.clone(), "OrcaSlicer 2.3.0"),
            (orca("/m/orca-2.4.1"), "OrcaSlicer 2.4.1"),
            (orca("/m/orca-2.5.0"), "OrcaSlicer 2.5.0"),
            (orca("/m/orca-nightly"), "OrcaSlicer 2.6.0-nightly"),
            (orca("/m/orca-beta"), "2.6.0-beta2"),
            (bambu("/m/bambu"), "02.09.01.70"),
        ];
        // Як у звіті: системний OrcaSlicer 2.3.0 падає — пропонуємо OrcaSlicer 2.5.0 з менеджера,
        // а не нічну 2.6.0 і не Bambu Studio з більшим номером.
        assert_eq!(pick(&system, "OrcaSlicer 2.3.0", &candidates).as_deref(), Some("/m/orca-2.5.0"));
        // Падає найновіший: старша стабільна того самого виду.
        assert_eq!(pick(&orca("/m/orca-2.5.0"), "OrcaSlicer 2.5.0", &candidates).as_deref(), Some("/m/orca-2.4.1"));
    }

    #[test]
    fn alternative_engine_falls_back_to_other_kind_then_unstable() {
        let current = orca("/m/orca-2.3.0");
        // Та сама версія за іншим шляхом — не альтернатива; лишається Bambu Studio.
        let candidates = [
            (current.clone(), "OrcaSlicer 2.3.0"),
            (orca("/copy/orca"), "OrcaSlicer 2.3.0"),
            (bambu("/m/bambu"), "02.09.01.70"),
            (orca("/m/orca-nightly"), "OrcaSlicer 2.6.0-nightly"),
        ];
        assert_eq!(pick(&current, "OrcaSlicer 2.3.0", &candidates).as_deref(), Some("/m/bambu"));
        // Лише нічна збірка — краще, ніж нічого.
        let candidates = [(current.clone(), "OrcaSlicer 2.3.0"), (orca("/m/orca-nightly"), "2.6.0-nightly")];
        assert_eq!(pick(&current, "OrcaSlicer 2.3.0", &candidates).as_deref(), Some("/m/orca-nightly"));
        // Інших рушіїв немає.
        assert_eq!(pick(&current, "OrcaSlicer 2.3.0", &[(current.clone(), "OrcaSlicer 2.3.0")]), None);
        assert_eq!(pick(&current, "OrcaSlicer 2.3.0", &[]), None);
    }

    #[test]
    fn engine_source_is_short_and_says_where_it_comes_from() {
        assert_eq!(EngineSource::of(true, false, "/home/u/.local/share/slicenv/engines/orca/2.5.0/AppRun"), EngineSource::Managed);
        assert_eq!(
            EngineSource::of(false, false, r"C:\Program Files\OrcaSlicer\orca-slicer.exe"),
            EngineSource::System(r"C:\Program Files\OrcaSlicer".into())
        );
        assert_eq!(
            EngineSource::of(false, true, r"C:\Users\User\AppData\Local\Programs\OrcaSlicer 2.4\bin\orca-slicer.exe"),
            EngineSource::Manual(r"C:\…\bin\orca-slicer.exe".into())
        );
        assert_eq!(short_path("/usr/bin/orca-slicer", 40), "/usr/bin/orca-slicer");
        assert_eq!(short_path("/opt/very/long/path/to/some/engine/bin/orca", 20), "/…/bin/orca");
    }

    #[test]
    fn engine_names_have_no_duplicated_product() {
        assert_eq!(engine_name(EngineKind::Orca, "OrcaSlicer-2.5.0"), "OrcaSlicer 2.5.0");
        assert_eq!(engine_name(EngineKind::Orca, "2.5.0"), "OrcaSlicer 2.5.0");
    }
}
