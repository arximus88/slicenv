//! Explicit settings migration; source archives are only written by the normal Save action.
use crate::{
    theme::zed,
    ui::{self, Toggle as _},
    widgets::{self, ToastKind},
    workspace::Workspace,
};
use gpui::{Context, Window, div, prelude::*, px};
use serde_json::{Map, Value};
use settings::{ConfigSet, MigrationReport, MigrationStatus, Scope};
use std::{collections::HashSet, rc::Rc};

#[derive(Clone, Debug, PartialEq)]
enum Target {
    Global,
    Object(u32),
    Volume(u32, usize),
    Range(u32, usize),
}
#[derive(Clone)]
pub(crate) struct Config {
    target: Target,
    label: String,
    values: Map<String, Value>,
    scope: Scope,
}
fn scoped(config: &project::Config) -> Map<String, Value> {
    config
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect()
}
fn configs(project: &project::Project) -> Vec<Config> {
    let mut configs = vec![Config {
        target: Target::Global,
        label: "Проєкт".into(),
        values: project.config.clone(),
        scope: Scope::Global,
    }];
    for object in &project.objects {
        configs.push(Config {
            target: Target::Object(object.id),
            label: crate::tr_format!("Об’єкт {} · {}", object.id, object.name),
            values: scoped(&object.config),
            scope: Scope::Object,
        });
        for (index, volume) in object.volumes.iter().enumerate() {
            configs.push(Config {
                target: Target::Volume(object.id, index),
                label: crate::tr_format!("Частина {}:{} · {}", object.id, index + 1, volume.name),
                values: scoped(&volume.config),
                scope: Scope::Region,
            });
        }
        for (index, range) in project
            .object_layer_ranges(object.id)
            .unwrap_or_default()
            .into_iter()
            .enumerate()
        {
            configs.push(Config {
                target: Target::Range(object.id, index),
                label: crate::tr_format!(
                    "Діапазон {}:{} · {}–{} мм",
                    object.id,
                    index + 1,
                    range.min_z,
                    range.max_z
                ),
                values: scoped(&range.config),
                scope: Scope::Region,
            });
        }
    }
    for config in &mut configs {
        config
            .values
            .retain(|key, _| !settings::is_metadata_key(key));
    }
    configs
}
pub(crate) fn report(project: &project::Project, schema: &settings::Schema) -> (Vec<Config>, MigrationReport) {
    let configs = configs(project);
    let references = configs
        .iter()
        .map(|c| ConfigSet {
            label: &c.label,
            scope: c.scope,
            values: &c.values,
        })
        .collect::<Vec<_>>();
    let defaults = schema
        .options
        .iter()
        .map(|(k, v)| (k.clone(), v.default.clone()))
        .collect();
    let report = settings::preview_migration(&references, schema, &defaults);
    (configs, report)
}
pub(crate) fn apply(
    project: &project::Project,
    configs: &[Config],
    report: &MigrationReport,
    selected: &HashSet<usize>,
) -> anyhow::Result<project::Project> {
    let mut next = project.clone();
    for &index in selected {
        let entry = report
            .entries
            .get(index)
            .ok_or_else(|| anyhow::anyhow!("невідомий пункт міграції"))?;
        let value = entry
            .proposed_value
            .clone()
            .ok_or_else(|| anyhow::anyhow!("немає безпечної заміни"))?;
        let config = configs
            .iter()
            .find(|c| c.label == entry.label)
            .ok_or_else(|| anyhow::anyhow!("не знайдено область налаштування"))?;
        let current = match config.target {
            Target::Global => project.config.get(&entry.key).cloned(),
            Target::Object(id) => project
                .object(id)
                .ok()
                .and_then(|o| o.config.get(&entry.key))
                .cloned()
                .map(Value::String),
            Target::Volume(id, i) => project
                .object(id)
                .ok()
                .and_then(|o| o.volumes.get(i))
                .and_then(|v| v.config.get(&entry.key))
                .cloned()
                .map(Value::String),
            Target::Range(id, i) => project
                .object_layer_ranges(id)
                .and_then(|r| r.get(i).and_then(|r| r.config.get(&entry.key)).cloned())
                .map(Value::String),
        };
        anyhow::ensure!(
            current.as_ref() == Some(&entry.old_value),
            "Проєкт змінився після аналізу: {}",
            entry.key
        );
        let text = match &value {
            Value::String(v) => v.clone(),
            _ => value.to_string(),
        };
        match config.target {
            Target::Global => {
                next.config.insert(entry.key.clone(), value);
            }
            Target::Object(id) => {
                let mut values = next.object(id)?.config.clone();
                values.insert(entry.key.clone(), text);
                next.set_object_config(id, values)?;
            }
            Target::Volume(id, i) => {
                let mut values = next.object(id)?.volumes[i].config.clone();
                values.insert(entry.key.clone(), text);
                next.set_volume_config(id, i, values)?;
            }
            Target::Range(id, i) => {
                let mut ranges = next
                    .object_layer_ranges(id)
                    .unwrap_or_default()
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>();
                ranges[i].config.insert(entry.key.clone(), text);
                next.set_layer_ranges(id, ranges)?;
            }
        }
    }
    Ok(next)
}
/// Пункти, що потребують рішення: усе, крім невідомих ключів (рушій їх ігнорує, а у файлі
/// вони лишаються). Нові види перевірок (наприклад, вихід за діапазон) теж потрапляють сюди.
pub(crate) fn problems(report: &MigrationReport) -> impl Iterator<Item = (usize, &settings::MigrationEntry)> {
    report
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.status != MigrationStatus::Unsupported)
}

/// Значення без JSON-лапок: `["0"]` → `0`, `"by_first_filament"` → `by_first_filament`.
fn plain(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(plain).collect::<Vec<_>>().join(", "),
        Value::Null => "—".into(),
        other => other.to_string(),
    }
}

/// Українська форма числа: 1 — `one`, 2–4 — `few`, решта — `many`.
pub(crate) fn plural<'a>(n: usize, one: &'a str, few: &'a str, many: &'a str) -> &'a str {
    match (n % 10, n % 100) {
        (1, r) if r != 11 => one,
        (2..=4, r) if !(12..=14).contains(&r) => few,
        _ => many,
    }
}

/// Значення для людини: назва варіанта зі схеми, якщо вона є.
fn display_value(schema: &settings::Schema, key: &str, value: &Value) -> String {
    let raw = plain(value);
    schema
        .options
        .get(key)
        .and_then(|d| {
            let index = d.enum_values.iter().position(|v| *v == raw)?;
            d.enum_label(index, crate::i18n::locale()).map(str::to_owned)
        })
        .filter(|l| !l.is_empty())
        .unwrap_or(raw)
}

/// Людська назва параметра зі схеми рушія; ключ — якщо схема його не знає.
fn setting_name(schema: &settings::Schema, key: &str) -> String {
    let locale = crate::i18n::locale();
    schema
        .options
        .get(key)
        .map(|d| {
            let label = d.full_label(locale);
            if label.is_empty() { d.label(locale).to_owned() } else { label.to_owned() }
        })
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| key.to_owned())
}

/// Пояснення українською з даних схеми, а не англійський текст валідатора.
fn problem_reason(schema: &settings::Schema, entry: &settings::MigrationEntry) -> String {
    let Some(def) = schema.options.get(&entry.key) else {
        return crate::i18n::tr("Значення не підходить для цього рушія нарізки");
    };
    if entry.reason.contains("scope") {
        return crate::i18n::tr("Цей параметр діє лише на весь проєкт, а у файлі його задано для окремого об’єкта");
    }
    if !def.enum_values.is_empty() {
        let value = plain(&entry.old_value);
        if !def.enum_values.contains(&value) {
            return crate::tr_format!("Рушій нарізки не знає варіанта «{}»", value);
        }
    }
    // Великий максимум (i32::MAX) — фактично «без верхньої межі».
    let max = def.max.filter(|m| *m < 1e9);
    match (def.min, max) {
        (Some(min), Some(max)) => crate::tr_format!("Допустимо від {} до {}", min, max),
        (Some(min), None) => crate::tr_format!("Має бути не менше {}", min),
        (None, Some(max)) => crate::tr_format!("Має бути не більше {}", max),
        (None, None) => crate::i18n::tr("Значення не підходить для цього рушія нарізки"),
    }
}

/// Область налаштування людською мовою: «Проєкт» перекладається, решта вже з назвою.
fn scope_label(label: &str) -> String {
    crate::i18n::tr(label)
}

impl Workspace {
    pub fn open_migration(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.engine_loading || self.slicing() {
            return;
        }
        let Some(project) = &self.project else {
            self.toast(
                ToastKind::Info,
                "Відкрий 3MF-проєкт для перевірки сумісності",
                cx,
            );
            return;
        };
        let schema = self.settings_schema;
        let (configs, report) = report(project, schema);
        let selected = Rc::new(std::cell::RefCell::new(
            problems(&report)
                .filter(|(_, e)| e.proposed_value.is_some())
                .map(|(i, _)| i)
                .collect::<HashSet<_>>(),
        ));
        let revision = self.engine_revision;
        let show_unsupported = Rc::new(std::cell::Cell::new(false));
        self.open_modal(Rc::new(move |ws,_,cx|{
            let z = zed();
            let unsupported: Vec<_> = report.entries.iter().filter(|e| e.status == MigrationStatus::Unsupported).collect();
            let issues: Vec<_> = problems(&report).collect();
            let without_fix = issues.iter().filter(|(_, e)| e.proposed_value.is_none()).count();
            let active = selected.borrow().len();
            let ok = issues.is_empty();

            // Вердикт згори: одне речення, яке відповідає на «це проблема?».
            let (icon, color, title, subtitle) = if ok {
                (ui::IconName::CircleCheck, z.success, crate::i18n::tr("Проєкт сумісний — нарізати можна"),
                 crate::tr_format!("Рушій нарізки: {}", ws.engine_label))
            } else {
                (ui::IconName::TriangleAlert, z.warning, { let n = issues.len(); match plural(n, "one", "few", "many") { "one" => crate::tr_format!("Знайдено {} проблему, що завадить нарізці", n), "few" => crate::tr_format!("Знайдено {} проблеми, що завадять нарізці", n), _ => crate::tr_format!("Знайдено {} проблем, що завадять нарізці", n) } },
                 if without_fix > 0 {
                     crate::tr_format!("Для {} з них автоматичної заміни немає — зміни значення вручну або імпортуй лише геометрію.", without_fix)
                 } else {
                     crate::i18n::tr("Для кожної є безпечне значення з профілю рушія нарізки. Натисни «Виправити й нарізати».")
                 })
            };
            let verdict = div().flex().gap(px(10.)).p(px(12.)).rounded(px(6.)).border_1()
                .border_color(color.opacity(0.45)).bg(color.opacity(0.10))
                .child(ui::icon(icon).size(px(18.)).text_color(color).mt(px(1.)))
                .child(div().flex().flex_col().gap(px(2.)).flex_1()
                    .child(div().text_base().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
                    .child(div().text_xs().text_color(z.text_muted).child(subtitle)));

            let mut body = div().flex().flex_col().p(px(14.)).gap(px(12.)).child(verdict);

            if !issues.is_empty() {
                let mut rows = div().id("migration-rows").max_h(px(330.)).overflow_y_scroll().flex().flex_col().gap(px(6.));
                for (index, entry) in issues {
                    let checked = selected.borrow().contains(&index);
                    let fixable = entry.proposed_value.is_some();
                    let selected = selected.clone();
                    let change = div().flex().items_center().gap(px(6.)).text_xs()
                        .child(div().text_color(z.text_muted).child(crate::i18n::tr("Зараз")))
                        .child(div().font_family(crate::theme::MONO_FONT).text_color(z.error).child(display_value(schema, &entry.key, &entry.old_value)))
                        .child(match &entry.proposed_value {
                            Some(v) => div().flex().gap(px(6.))
                                .child(div().text_color(z.text_muted).child("→"))
                                .child(div().font_family(crate::theme::MONO_FONT).text_color(z.success).child(display_value(schema, &entry.key, v)))
                                .into_any_element(),
                            None => div().text_color(z.text_muted).child(crate::i18n::tr("· автоматичної заміни немає")).into_any_element(),
                        });
                    let line = div().flex().gap(px(10.)).p(px(10.)).rounded(px(4.)).bg(z.element)
                        .child(widgets::checkbox(("migration-check", index), checked && fixable, !fixable))
                        .child(div().flex().flex_col().flex_1().min_w_0().gap(px(3.))
                            .child(div().flex().items_baseline().gap(px(8.))
                                .child(div().text_sm().font_weight(gpui::FontWeight::MEDIUM).child(setting_name(schema, &entry.key)))
                                .child(div().text_xs().text_color(z.text_placeholder).font_family(crate::theme::MONO_FONT).child(entry.key.clone())))
                            .child(change)
                            .child(div().text_xs().text_color(z.text_muted).child(format!("{} · {}", problem_reason(schema, entry), scope_label(&entry.label)))));
                    rows = rows.child(div().id(("migration-entry", index))
                        .when(fixable, |d| d.cursor_pointer().on_click(cx.listener(move |_,_,_,cx| {
                            let mut set = selected.borrow_mut();
                            if !set.remove(&index) { set.insert(index); }
                            cx.notify();
                        })))
                        .child(line));
                }
                body = body.child(rows);
            }

            // Невідомі ключі — не проблема: тихий рядок і необов’язковий список.
            if !unsupported.is_empty() {
                let show = show_unsupported.clone();
                let open = show.get();
                body = body.child(div().flex().flex_col().gap(px(6.))
                    .child(div().id("migration-unsupported").flex().items_center().gap(px(6.)).cursor_pointer()
                        .text_xs().text_color(z.text_muted).hover(|s| s.text_color(z.text))
                        .child(ui::icon(if open { ui::IconName::ChevronDown } else { ui::IconName::ChevronRight }).size(px(12.)))
                        .child({ let n = unsupported.len(); if plural(n, "one", "few", "many") == "many" { crate::tr_format!("{} налаштувань з іншої версії слайсера буде збережено без змін", n) } else { crate::tr_format!("{} налаштування з іншої версії слайсера буде збережено без змін", n) } })
                        .on_click(cx.listener(move |_,_,_,cx| { show.set(!show.get()); cx.notify(); })))
                    .when(open, |d| d.child(div().id("migration-unsupported-list").max_h(px(140.)).overflow_y_scroll()
                        .p(px(8.)).rounded(px(4.)).bg(z.editor)
                        .child(div().text_xs().text_color(z.text_muted).child(crate::i18n::tr("Цей рушій нарізки їх не знає й ігнорує; у файлі вони лишаються для слайсера, який їх створив.")))
                        .child(div().mt(px(4.)).text_xs().font_family(crate::theme::MONO_FONT).text_color(z.text_muted)
                            .child(unsupported.iter().map(|e| e.key.as_str()).collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>().join(", "))))));
            }
            body = body.child(div().text_xs().text_color(z.text_placeholder).child(crate::i18n::tr("Зміни стосуються відкритого проєкту; файл на диску зміниться лише після збереження.")));

            let stale = ws.engine_revision != revision || ws.engine_loading;
            let configs = configs.clone(); let report = report.clone(); let chosen = selected.clone();
            let fix = move |this: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>, slice: bool| {
                if this.engine_revision != revision || this.engine_loading || this.slicing() { return; }
                let Some(project) = &this.project else { return };
                match apply(project, &configs, &report, &chosen.borrow()) {
                    Ok(project) => {
                        this.checkpoint("Міграція налаштувань");
                        this.project = Some(project);
                        this.close_modal(cx);
                        this.project_changed(window, cx);
                        if slice { this.start_slice(window, cx); }
                        else { this.toast(ToastKind::Success, "Застосовано до відкритого проєкту. Вихідний файл не перезаписано", cx); }
                    }
                    Err(e) => this.toast(ToastKind::Error, format!("{e:#}"), cx),
                }
            };
            let fix = Rc::new(fix);
            let footer = if ok {
                div().flex().gap(px(6.))
                    .child(ui::ghost_button("migration-close", None, "Закрити").on_click(cx.listener(|this,_,_,cx| this.close_modal(cx))))
                    .child(ui::accent_button("migration-slice", ui::IconName::Scissors, "Нарізати", !stale)
                        .on_click(cx.listener(|this,_,window,cx| { this.close_modal(cx); this.start_slice(window, cx); })))
                    .into_any_element()
            } else {
                let (a, b) = (fix.clone(), fix.clone());
                div().flex().gap(px(6.))
                    .child(ui::ghost_button("migration-geometry", Some(ui::IconName::Box), "Імпортувати лише геометрію")
                        .on_click(cx.listener(|this,_,window,cx| this.keep_geometry_only(window, cx))))
                    .child(ui::ghost_button("migration-apply", Some(ui::IconName::Check), crate::tr_format!("Лише виправити ({})", active))
                        .inert(active == 0 || stale)
                        .when(active > 0 && !stale, |d| d.on_click(cx.listener(move |this,_,window,cx| a(this, window, cx, false)))))
                    .child(ui::accent_button("migration-fix-slice", ui::IconName::Scissors, "Виправити й нарізати", active > 0 && !stale)
                        .when(active > 0 && !stale, |d| d.on_click(cx.listener(move |this,_,window,cx| b(this, window, cx, true)))))
                    .into_any_element()
            };
            widgets::modal("Сумісність із рушієм нарізки", 720., body, Some(footer)).into_any_element()
        }),window,cx);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ukrainian_plural_forms() {
        let f = |n| plural(n, "one", "few", "many");
        assert_eq!([f(1), f(2), f(5), f(11), f(21), f(22), f(14), f(271)], ["one", "few", "many", "many", "one", "few", "many", "one"]);
    }
    #[test]
    fn applies_only_reviewed_values_and_preserves_archive_and_unknown_fields() {
        let mut schema = settings::Schema::load_orca().unwrap();
        schema
            .options
            .get_mut("raft_first_layer_expansion")
            .unwrap()
            .default = Value::String("2".into());
        let mut p = project::Project::default();
        p.config.insert(
            "raft_first_layer_expansion".into(),
            Value::String("-1".into()),
        );
        p.config
            .insert("future_setting".into(), Value::String("retain".into()));
        p.archive.insert(
            "Metadata/painter.bin".into(),
            std::sync::Arc::from([1u8, 2, 3]),
        );
        let (configs, report) = report(&p, &schema);
        let index = report
            .entries
            .iter()
            .position(|e| e.key == "raft_first_layer_expansion")
            .unwrap();
        let selected = HashSet::from([index]);
        let next = apply(&p, &configs, &report, &selected).unwrap();
        assert_eq!(p.config["raft_first_layer_expansion"], "-1");
        assert_ne!(next.config["raft_first_layer_expansion"], "-1");
        assert_eq!(next.config["future_setting"], "retain");
        assert_eq!(next.archive, p.archive);
        p.config.insert(
            "raft_first_layer_expansion".into(),
            Value::String("4".into()),
        );
        assert!(apply(&p, &configs, &report, &selected).is_err());
    }
    #[test]
    #[ignore = "requires ANGEL_MIGRATION_FIXTURE and installed Orca CLI"]
    fn real_angel_migration_preserves_parts_and_ranges_and_slices_in_orca() -> anyhow::Result<()> {
        let path = std::path::PathBuf::from(
            std::env::var_os("ANGEL_MIGRATION_FIXTURE")
                .ok_or_else(|| anyhow::anyhow!("set ANGEL_MIGRATION_FIXTURE"))?,
        );
        let engine = slicer::Engine::discover(slicer::EngineKind::Orca)?;
        let info = engine.probe()?;
        let schema = crate::engines::runtime_schema(&engine, &info.identity)?;
        let original = project::Project::load(&path)?;
        let (configs, report) = report(&original, schema);
        for entry in report
            .entries
            .iter()
            .filter(|e| e.status == MigrationStatus::Invalid)
        {
            eprintln!(
                "{} · {}: {} -> {:?}",
                entry.label, entry.key, entry.old_value, entry.proposed_value
            );
        }
        let selected = report
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.proposed_value.is_some())
            .map(|(i, _)| i)
            .collect();
        let migrated = apply(&original, &configs, &report, &selected)?;
        assert_eq!(original.config["raft_first_layer_expansion"], "-1");
        assert_eq!(migrated.objects, original.objects);
        assert_eq!(migrated.layer_ranges().len(), original.layer_ranges().len());
        for (a, b) in original.layer_ranges().iter().zip(migrated.layer_ranges()) {
            assert_eq!(
                (a.source_object_id, a.min_z, a.max_z),
                (b.source_object_id, b.min_z, b.max_z)
            );
        }
        assert_eq!(migrated.archive, original.archive);
        let dir = tempfile::tempdir()?;
        let input = dir.path().join("angel-migrated.3mf");
        migrated.save_3mf(&input)?;
        let loaded = project::Project::load(&input)?;
        assert_eq!(loaded.objects.len(), original.objects.len());
        for (a, b) in migrated.layer_ranges().iter().zip(loaded.layer_ranges()) {
            assert_eq!(
                (a.source_object_id, a.min_z, a.max_z, &a.config),
                (b.source_object_id, b.min_z, b.max_z, &b.config)
            );
        }
        for (before, after) in original.objects.iter().zip(&loaded.objects) {
            assert_eq!(before.volumes.len(), after.volumes.len());
            for (a, b) in before.volumes.iter().zip(&after.volumes) {
                assert_eq!(a.kind, b.kind);
                assert_eq!(a.mesh, b.mesh);
                assert_eq!(a.config, b.config);
            }
        }
        let store = slicer::PresetStore::open_for_engine(&engine)?;
        let selection = store
            .defaults_for_machine("Bambu Lab A1 0.4 nozzle")
            .ok_or_else(|| anyhow::anyhow!("missing A1 defaults"))?;
        let output = slicer::slice_with_engine(
            &engine,
            &store,
            &slicer::SliceRequest {
                input,
                plate: 2,
                allow_newer_file: true,
                selection,
                overrides: Default::default(),
            },
        )?;
        assert!(output.filament_grams > 0. && output.print_time.as_secs() > 0);
        eprintln!(
            "MIGRATED_ORCA {} · {:?} · {} g",
            output.gcode_3mf.display(),
            output.print_time,
            output.filament_grams
        );
        Ok(())
    }
}
