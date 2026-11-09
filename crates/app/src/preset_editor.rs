//! Редактор пресета (процес / філамент / принтер): вкладки сторінок, групи, поля за типом,
//! позначка змінених значень і скидання — як у Bambu Studio.

use std::{collections::HashMap, rc::Rc, sync::OnceLock};

use anyhow::Context as _;

use gpui::{
    Context, Entity, Focusable as _, SharedString, Subscription, Window, div, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use serde_json::Value;
use settings::{EditablePreset, OptionDef, OptionType, PresetType, Schema};

/// Підписи з перекладу Bambu Studio; чого нема українською — лишається англійською.
use slicer::PresetStore;

use crate::{
    widgets::{Dropdown, DropdownEvent, ToastKind},
    workspace::Workspace,
};

pub fn schema() -> &'static Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA.get_or_init(|| Schema::load().expect("вбудована схема налаштувань зламана"))
}

pub struct PresetEditor {
    pub preset: EditablePreset<'static>,
    pub page: usize,
    pub advanced: bool,
    inputs: HashMap<String, Entity<InputState>>,
    /// Багаторядкові поля: G-code, примітки, списки сумісних пресетів.
    areas: HashMap<String, Entity<TextareaState>>,
    selects: HashMap<String, Entity<Dropdown>>,
    _subscriptions: Vec<Subscription>,
}

impl PresetEditor {
    pub fn new(
        schema: &'static Schema,
        store: &PresetStore,
        kind: PresetType,
        name: &str,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            preset: EditablePreset::from_system(schema, store, kind, name)?,
            page: 0,
            advanced: false,
            inputs: HashMap::new(),
            areas: HashMap::new(),
            selects: HashMap::new(),
            _subscriptions: Vec::new(),
        })
    }

    pub fn clear_controls(&mut self) {
        self.inputs.clear();
        self.areas.clear();
        self.selects.clear();
        self._subscriptions.clear();
    }

    pub fn is_dirty(&self) -> bool {
        self.preset.dirty_keys().next().is_some()
    }

    /// Лише змінені ключі — їх і передаємо в нарізку поверх системного профілю.
    pub fn overrides(&self) -> serde_json::Map<String, Value> {
        self.preset
            .dirty_keys()
            .filter_map(|k| Some((k.to_owned(), self.preset.value(k)?.clone())))
            .collect()
    }
}

/// Значення для показу: перший елемент вектора (A1 має один екструдер).
fn display_value(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .first()
            .map(|v| display_value(Some(v)))
            .unwrap_or_default(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// Нове значення в тій самій формі, що й старе: рядок або вектор (замінюємо всі елементи).
fn shaped_like(old: Option<&Value>, text: &str) -> Value {
    match old {
        Some(Value::Array(items)) if !items.is_empty() => Value::Array(
            items
                .iter()
                .map(|_| Value::String(text.to_owned()))
                .collect(),
        ),
        _ => Value::String(text.to_owned()),
    }
}

/// Як редагується значення опції.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TextKind {
    /// Однорядкове поле: пробіли по краях відкидаються.
    Line,
    /// Багаторядковий текст (G-code, примітки): зберігається байт-у-байт.
    Multiline,
    /// Список імен, по одному на рядок.
    List,
    /// Полігон (`printable_area`, `bed_exclude_area`): усі точки одним рядком через кому,
    /// `0x0,256x0,256x256,0x256`. Перший елемент вектора тут — лише одна вершина.
    Points,
    /// Кілька полігонів (`extruder_printable_area`), розділених `#`.
    PointGroups,
}

impl TextKind {
    /// Багаторядкове поле на всю ширину.
    fn is_wide(self) -> bool {
        matches!(self, Self::Multiline | Self::List)
    }
}

fn text_kind(schema: &Schema, key: &str) -> TextKind {
    if schema.is_list_option(key) {
        TextKind::List
    } else if schema.options.get(key).is_some_and(|d| d.multiline) {
        TextKind::Multiline
    } else if schema
        .options
        .get(key)
        .is_some_and(|d| d.kind == OptionType::Points && !d.per_extruder)
    {
        TextKind::Points
    } else if schema
        .options
        .get(key)
        .is_some_and(|d| d.kind == OptionType::PointsGroups)
    {
        TextKind::PointGroups
    } else {
        TextKind::Line
    }
}

/// Текст поля для значення опції.
fn field_text(kind: TextKind, value: Option<&Value>) -> String {
    match kind {
        TextKind::List => settings::list_text(value),
        TextKind::Line | TextKind::Multiline => display_value(value),
        TextKind::Points => joined_text(value, ","),
        TextKind::PointGroups => joined_text(value, "#"),
    }
}

/// Усі елементи вектора одним рядком.
fn joined_text(value: Option<&Value>, separator: &str) -> String {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
            .collect::<Vec<_>>()
            .join(separator),
        other => display_value(other),
    }
}

/// Рядок через `separator` → вектор (або рядок, якщо значення було рядком).
fn split_text(old: Option<&Value>, text: &str, separator: char) -> Value {
    if matches!(old, Some(Value::String(_))) {
        return Value::String(text.trim().to_owned());
    }
    Value::Array(
        text.split(separator)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| Value::String(s.to_owned()))
            .collect(),
    )
}

/// Значення опції з тексту поля, у формі попереднього значення `old`.
fn value_from_text(kind: TextKind, old: Option<&Value>, text: &str) -> Value {
    match kind {
        TextKind::List => settings::parse_list(text),
        TextKind::Multiline => shaped_like(old, text),
        TextKind::Line => shaped_like(old, text.trim()),
        TextKind::Points => split_text(old, text, ','),
        TextKind::PointGroups => split_text(old, text, '#'),
    }
}

/// G-code — моноширинним шрифтом і без переносу рядків; решта — звичайний текст.
fn is_gcode(key: &str) -> bool {
    key.ends_with("_gcode") || key == "post_process"
}

/// Сторінка, яку показати: вибрана, якщо на ній є поля, інакше перша непорожня.
fn shown_page(selected: usize, visible: &[usize]) -> usize {
    if visible.contains(&selected) {
        selected
    } else {
        visible.first().copied().unwrap_or(selected)
    }
}

/// Підпис рядка з кількома полями (у Studio він задається в Tab.cpp, а не в описі опцій).
fn line_label(first_key: &str) -> Option<&'static str> {
    Some(match first_key {
        "overhang_1_4_speed" => "Швидкість на нависаннях",
        "filament_prime_volume" => "Об’єм продувки",
        "filament_change_length" => "Довжина при заміні",
        "filament_ramming_travel_time" => "Час рамінгу з переміщенням",
        "filament_pre_cooling_temperature" => "Температура попереднього охолодження",
        "filament_ramming_volumetric_speed" => "Об’ємна швидкість рамінгу",
        "nozzle_temperature_range_low" => "Рекомендована температура сопла",
        "supertack_plate_temp_initial_layer" => "Пластина Supertack",
        "cool_plate_temp_initial_layer" => "Cool Plate",
        "eng_plate_temp_initial_layer" => "Engineering Plate",
        "hot_plate_temp_initial_layer" => "High Temp Plate",
        "textured_plate_temp_initial_layer" => "Textured PEI Plate",
        "nozzle_temperature_initial_layer" => "Сопло",
        "close_fan_the_first_x_layers" => "Без обдуву на перших шарах",
        "fan_min_speed" => "Мінімальний обдув",
        "fan_max_speed" => "Максимальний обдув",
        "close_additional_fan_first_x_layers" => "Без допоміжного вентилятора на перших шарах",
        "additional_fan_full_speed_layer" => "Допоміжний вентилятор",
        _ => return None,
    })
}

fn is_bool(def: &OptionDef) -> bool {
    matches!(def.kind, OptionType::Bool | OptionType::Bools)
}

fn is_enum(def: &OptionDef) -> bool {
    matches!(def.kind, OptionType::Enum | OptionType::Enums) && !def.enum_values.is_empty()
}

impl Workspace {
    pub fn editor_mut(&mut self, kind: PresetType) -> Option<&mut PresetEditor> {
        match kind {
            PresetType::Process => self.process_editor.as_mut(),
            PresetType::Filament => self.filament_editor.as_mut(),
            PresetType::Machine => self.machine_editor.as_mut(),
        }
    }

    pub fn editor(&self, kind: PresetType) -> Option<&PresetEditor> {
        match kind {
            PresetType::Process => self.process_editor.as_ref(),
            PresetType::Filament => self.filament_editor.as_ref(),
            PresetType::Machine => self.machine_editor.as_ref(),
        }
    }

    /// Перестворює редактори під поточний вибір (незбережені зміни відкидаються).
    pub fn rebuild_editors(&mut self) {
        let Some(store) = self.presets.clone() else {
            return;
        };
        let make =
            |kind, name: &str| match PresetEditor::new(self.settings_schema, &store, kind, name) {
                Ok(editor) => Some(editor),
                Err(err) => {
                    eprintln!("редактор {name}: {err:#}");
                    None
                }
            };
        let keep =
            |old: Option<PresetEditor>, name: &str| old.filter(|e| e.preset.parent() == name);
        self.process_editor = keep(self.process_editor.take(), &self.selection.process)
            .or_else(|| make(PresetType::Process, &self.selection.process));
        self.filament_editor = keep(self.filament_editor.take(), &self.selection.filament)
            .or_else(|| make(PresetType::Filament, &self.selection.filament));
        self.machine_editor = keep(self.machine_editor.take(), &self.selection.machine)
            .or_else(|| make(PresetType::Machine, &self.selection.machine));
        self.sync_model_color();
    }

    /// Модель на «Підготовці» — кольору філаменту: з проєкту 3MF, інакше з пресету.
    pub fn sync_model_color(&mut self) {
        fn first_hex(value: &serde_json::Value) -> Option<[f32; 3]> {
            let text = match value {
                serde_json::Value::Array(items) => items.first()?.as_str()?,
                serde_json::Value::String(s) => s.split(';').next()?,
                _ => return None,
            };
            let hex = text.trim().trim_start_matches('#');
            let v = u32::from_str_radix(hex.get(..6)?, 16).ok()?;
            Some([
                ((v >> 16) & 0xff) as f32 / 255.0,
                ((v >> 8) & 0xff) as f32 / 255.0,
                (v & 0xff) as f32 / 255.0,
            ])
        }
        let color = self
            .project
            .as_ref()
            .and_then(|p| p.config.get("filament_colour"))
            .and_then(first_hex)
            .or_else(|| {
                self.filament_editor
                    .as_ref()?
                    .preset
                    .effective_value("filament_colour")
                    .and_then(first_hex)
            });
        self.renderer
            .set_model_color(color.unwrap_or([0.0, 0.682, 0.259]));
        self.dirty = true;
    }

    pub(crate) fn commit_value(
        &mut self,
        kind: PresetType,
        key: &str,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let schema = self.settings_schema;
        let Some(editor) = self.editor(kind) else {
            return;
        };
        let value = value_from_text(text_kind(schema, key), editor.preset.shown_value(key), text);
        self.commit_json(kind, key, value, window, cx);
    }

    /// Записує значення; помилку перевірки показує й повертає поле до чинного значення.
    fn commit_json(
        &mut self,
        kind: PresetType,
        key: &str,
        value: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.editor_mut(kind) else {
            return;
        };
        // Типове значення ключа, якого нема в профілі, — не зміна.
        if editor.preset.shown_value(key) == Some(&value) && !editor.preset.is_dirty(key) {
            return;
        }
        if let Err(err) = editor.preset.set(key, value) {
            self.sync_controls(kind, key, window, cx);
            self.toast(crate::widgets::ToastKind::Error, err.to_string(), cx);
            return;
        }
        self.edited = true;
        self.cancel_slice();
        cx.notify();
    }

    /// Поля вводу й випадні списки ключа — до чинного значення пресета.
    fn sync_controls(
        &mut self,
        kind: PresetType,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let schema = self.settings_schema;
        let Some(editor) = self.editor(kind) else {
            return;
        };
        let value = editor.preset.shown_value(key);
        let shown = field_text(text_kind(schema, key), value);
        if let Some(input) = editor.inputs.get(key).cloned() {
            let shown = shown.clone();
            input.update(cx, |state, cx| state.set_value(shown, window, cx));
        }
        if let Some(area) = editor.areas.get(key).cloned() {
            let shown = shown.clone();
            area.update(cx, |state, cx| state.set_value(shown, window, cx));
        }
        let index = schema
            .options
            .get(key)
            .and_then(|def| def.enum_values.iter().position(|v| *v == shown));
        if let (Some(select), Some(index)) = (editor.selects.get(key).cloned(), index) {
            select.update(cx, |state, cx| state.set_selected_index(Some(index), cx));
        }
    }

    fn reset_value(
        &mut self,
        kind: PresetType,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.editor_mut(kind) else {
            return;
        };
        editor.preset.reset(key);
        self.sync_controls(kind, key, window, cx);
        self.edited = true;
        self.cancel_slice();
        cx.notify();
    }

    /// Значення, яке перевизначає `filament_<key>`: з принтера, інакше з процесу.
    fn inherited_value(&self, key: &str) -> Option<Value> {
        let base = self.settings_schema.override_base_key(key)?;
        [PresetType::Machine, PresetType::Process]
            .into_iter()
            .filter_map(|kind| self.editor(kind)?.preset.effective_value(base))
            .find(|v| !settings::is_nil(v))
            .cloned()
    }

    /// Вмикач «Перевизначити»: вимкнено — `nil` (як у Studio), увімкнено — успадковане значення.
    fn toggle_override(
        &mut self,
        kind: PresetType,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let schema = self.settings_schema;
        let inherited = self.inherited_value(key);
        let Some(editor) = self.editor(kind) else {
            return;
        };
        let current = editor.preset.shown_value(key).cloned();
        let value = if editor.preset.is_overridden(key) {
            settings::nil_like(current.as_ref())
        } else {
            let default = schema
                .options
                .get(key)
                .map(|d| d.default.clone())
                .filter(|v| !settings::is_nil(v));
            match inherited.or(default) {
                Some(source) => shaped_like(current.as_ref(), &display_value(Some(&source))),
                None => return,
            }
        };
        self.commit_json(kind, key, value, window, cx);
        self.sync_controls(kind, key, window, cx);
    }

    /// Поле вводу для ключа; створюється при першому показі й живе разом із редактором.
    fn input_for(
        &mut self,
        kind: PresetType,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<InputState>> {
        let editor = self.editor(kind)?;
        if let Some(input) = editor.inputs.get(key) {
            return Some(input.clone());
        }
        let text_kind = text_kind(self.settings_schema, key);
        let text = field_text(text_kind, editor.preset.shown_value(key));
        let input = cx.new(|cx| InputState::new(window, cx).default_value(text.clone()));
        if let Some(spec) = schema()
            .options
            .get(key)
            .and_then(|def| crate::scrub::Spec::option(def, text.parse().unwrap_or(0.0)))
        {
            self.bind_scrub(
                &input,
                spec,
                crate::scrub::Target::Setting(kind, key.to_owned()),
            );
        }
        let owned_key = key.to_owned();
        let sub = cx.subscribe_in(
            &input,
            window,
            move |this, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    let text = input.read(cx).value().to_string();
                    let current = this
                        .editor(kind)
                        .map(|e| field_text(text_kind, e.preset.shown_value(&owned_key)));
                    if current.as_deref() != Some(text.trim()) {
                        this.commit_value(kind, &owned_key, &text, window, cx);
                    }
                }
            },
        );
        let editor = self.editor_mut(kind)?;
        editor.inputs.insert(key.to_owned(), input.clone());
        editor._subscriptions.push(sub);
        Some(input)
    }

    /// Багаторядкове поле (G-code, примітки, списки). Enter переносить рядок, а значення
    /// записується, коли поле втрачає фокус — текст без змін, включно з `\n` і `\r\n`.
    fn area_for(
        &mut self,
        kind: PresetType,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<TextareaState>> {
        let schema = self.settings_schema;
        let editor = self.editor(kind)?;
        if let Some(area) = editor.areas.get(key) {
            return Some(area.clone());
        }
        let kind_of_text = text_kind(schema, key);
        let text = field_text(kind_of_text, editor.preset.shown_value(key));
        let gcode = is_gcode(key);
        let area = cx.new(|cx| {
            let state = TextareaState::new(window, cx);
            let state = match kind_of_text {
                TextKind::List => state.auto_grow(2, 6),
                _ if gcode => state.rows(10).soft_wrap(false),
                _ => state.auto_grow(4, 10),
            };
            state.default_value(text)
        });
        let owned_key = key.to_owned();
        let sub = cx.subscribe_in(
            &area,
            window,
            move |this, area, event: &InputEvent, window, cx| {
                if !matches!(event, InputEvent::Blur) {
                    return;
                }
                let text = area.read(cx).value().to_string();
                let Some(editor) = this.editor(kind) else {
                    return;
                };
                let kind_of_text = text_kind(this.settings_schema, &owned_key);
                let old = editor.preset.shown_value(&owned_key);
                if field_text(kind_of_text, old) != text {
                    let value = value_from_text(kind_of_text, old, &text);
                    this.commit_json(kind, &owned_key, value, window, cx);
                }
            },
        );
        let editor = self.editor_mut(kind)?;
        editor.areas.insert(key.to_owned(), area.clone());
        editor._subscriptions.push(sub);
        Some(area)
    }

    fn select_for(
        &mut self,
        kind: PresetType,
        key: &str,
        def: &'static OptionDef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<Dropdown>> {
        let editor = self.editor(kind)?;
        if let Some(select) = editor.selects.get(key) {
            return Some(select.clone());
        }
        let current = display_value(editor.preset.shown_value(key));
        let labels: Vec<SharedString> = def
            .enum_values
            .iter()
            .enumerate()
            .map(|(i, v)| {
                def.enum_label(i, crate::i18n::locale())
                    .filter(|l| !l.is_empty())
                    .unwrap_or(v)
                    .to_owned()
                    .into()
            })
            .collect();
        let index = def.enum_values.iter().position(|v| *v == current);
        let icons = crate::pattern_icons::option_icons(key, &def.enum_values);
        let select = cx.new(|cx| Dropdown::new(labels, index, cx).with_icons(icons));
        let owned_key = key.to_owned();
        let sub = cx.subscribe_in(
            &select,
            window,
            move |this, state, _: &DropdownEvent, window, cx| {
                let Some(index) = state.read(cx).selected_index() else {
                    return;
                };
                if let Some(value) = def.enum_values.get(index) {
                    this.commit_value(kind, &owned_key, value, window, cx);
                }
            },
        );
        let editor = self.editor_mut(kind)?;
        editor.selects.insert(key.to_owned(), select.clone());
        editor._subscriptions.push(sub);
        Some(select)
    }

    /// Вкладки сторінок + групи з полями.
    pub fn preset_editor_view(
        &mut self,
        kind: PresetType,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let _profile = paths::profile::Span::new("preset_editor::render");
        let Some(editor) = self.editor(kind) else {
            return div()
                .text_sm()
                .child(crate::i18n::tr("Профілі ще завантажуються…"))
                .into_any_element();
        };
        let schema = self.settings_schema;
        let pages = schema.pages(kind);
        let advanced = editor.advanced;
        // Вкладки — лише для сторінок, де є хоч одне видиме поле.
        let visible_pages = editor.preset.visible_pages(advanced);
        let page_index = shown_page(editor.page, &visible_pages);
        let dirty = editor.is_dirty();
        let app_owned = self
            .presets
            .as_ref()
            .and_then(|s| s.get(kind.into(), editor.preset.parent()))
            .is_some_and(|p| p.own);
        let Some(page) = pages.get(page_index) else {
            return div().into_any_element();
        };

        // Збираємо, що показувати, до того як позичати self мутабельно під віджети.
        struct Row {
            key: &'static str,
            def: Option<&'static OptionDef>,
            dirty: bool,
            enabled: bool,
            /// Заголовок рядка з кількох полів — показується над першим із них.
            heading: Option<&'static str>,
            value: String,
            /// Перевизначення філаменту: `Some(увімкнено)`.
            overridden: Option<bool>,
        }
        let mut groups: Vec<(&'static str, &'static str, Vec<Row>)> = Vec::new();
        for group in &page.groups {
            let rows: Vec<Row> = group
                .lines
                .iter()
                .flat_map(|line| {
                    let heading = (line.options.len() > 1)
                        .then(|| line_label(&line.options[0]))
                        .flatten();
                    line.options
                        .iter()
                        .enumerate()
                        .map(move |(i, key)| (key, heading.filter(|_| i == 0)))
                })
                .filter_map(|(key, heading)| {
                    let def = schema.options.get(key.as_str());
                    // Як у Studio: залежні поля ховаються (is_visible) або сіріють (is_enabled).
                    if !editor.preset.is_shown(key, advanced) {
                        return None;
                    }
                    Some(Row {
                        key: key.as_str(),
                        def,
                        dirty: editor.preset.is_dirty(key),
                        enabled: editor.preset.is_enabled(key),
                        heading,
                        value: field_text(text_kind(schema, key), editor.preset.shown_value(key)),
                        overridden: (kind == PresetType::Filament
                            && schema.is_filament_override(key))
                        .then(|| editor.preset.is_overridden(key)),
                    })
                })
                .collect();
            if !rows.is_empty() {
                groups.push((
                    group.title.as_str(),
                    group.title(crate::i18n::locale()),
                    rows,
                ));
            }
        }

        let z = crate::theme::zed();
        let mut group_elements = Vec::new();
        for (source_title, title, rows) in groups {
            let mut row_elements = Vec::new();
            for row in rows {
                let key = row.key;
                let enabled = row.enabled;
                if let Some(heading) = row.heading {
                    row_elements.push(
                        div()
                            .px(px(10.))
                            .pt(px(4.))
                            .text_xs()
                            .text_color(z.text_placeholder)
                            .child(crate::i18n::tr(heading))
                            .into_any_element(),
                    );
                }
                let label = row.def.map_or(key.to_owned(), |d| {
                    let label = d.label(crate::i18n::locale());
                    if label.is_empty() {
                        d.full_label(crate::i18n::locale())
                    } else {
                        label
                    }
                    .to_owned()
                });
                let tooltip = row
                    .def
                    .map(|d| d.description(crate::i18n::locale()).to_owned())
                    .unwrap_or_default();
                let help_title = label.clone();
                let help_description = tooltip.clone();
                let help_key = key.to_owned();
                let help_url = row.def.and_then(|def| def.documentation_url.clone());
                let sidetext = row
                    .def
                    .map(|d| d.sidetext(crate::i18n::locale()).to_owned())
                    .unwrap_or_default();

                let wide = text_kind(schema, key).is_wide() && row.overridden != Some(false);
                let control = match row.def {
                    // Не перевизначено — приглушено показуємо, що прийде з принтера/процесу.
                    Some(_) if row.overridden == Some(false) => {
                        let inherited = self
                            .inherited_value(key)
                            .map(|v| display_value(Some(&v)))
                            .unwrap_or_else(|| "—".into());
                        let text = if sidetext.is_empty() {
                            inherited
                        } else {
                            format!("{inherited} {sidetext}")
                        };
                        div()
                            .px(px(6.))
                            .text_sm()
                            .truncate()
                            .text_color(z.text_placeholder)
                            .child(crate::tr_format!("успадковано: {text}"))
                            .into_any_element()
                    }
                    Some(_) if wide => match self.area_for(kind, key, window, cx) {
                        Some(area) => div()
                            .w_full()
                            .when(is_gcode(key), |d| d.font_family(crate::theme::MONO_FONT))
                            .text_sm()
                            .when(!enabled, |d| d.opacity(0.5))
                            .child(Textarea::new(&area).disabled(!enabled))
                            .into_any_element(),
                        None => div().child(row.value.clone()).into_any_element(),
                    },
                    Some(def) if is_bool(def) => {
                        let checked = matches!(row.value.as_str(), "1" | "true");
                        crate::widgets::checkbox(
                            SharedString::from(format!("{kind:?}-{key}")),
                            checked,
                            !enabled,
                        )
                        .when(enabled, |d| {
                            d.on_click(cx.listener(move |this, _, window, cx| {
                                this.commit_value(
                                    kind,
                                    key,
                                    if checked { "0" } else { "1" },
                                    window,
                                    cx,
                                )
                            }))
                        })
                        .into_any_element()
                    }
                    Some(def) if is_enum(def) => {
                        match self.select_for(kind, key, def, window, cx) {
                            Some(select) => {
                                select.update(cx, |s, cx| s.set_disabled(!enabled, cx));
                                select.into_any_element()
                            }
                            None => div().child(row.value.clone()).into_any_element(),
                        }
                    }
                    _ => match self.input_for(kind, key, window, cx) {
                        Some(input) => {
                            let editor = Input::new(&input).disabled(!enabled).when(
                                !sidetext.is_empty(),
                                |i| {
                                    i.suffix(
                                        div()
                                            .text_xs()
                                            .text_color(z.text_placeholder)
                                            .child(sidetext.clone()),
                                    )
                                },
                            );
                            let field = if enabled
                                && self.scrub_bindings.contains_key(&input.entity_id())
                            {
                                crate::scrub::field(&input, editor, window, cx)
                            } else {
                                crate::widgets::field(&input, editor, window, cx)
                            };
                            field.when(!enabled, |d| d.opacity(0.5)).into_any_element()
                        }
                        None => div().child(row.value.clone()).into_any_element(),
                    },
                };

                let label_area = div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .when(!wide, |d| d.w(gpui::relative(0.48)).flex_none())
                                .when(wide, |d| d.flex_1())
                                .min_w_0()
                                .text_sm()
                                .text_color(if row.dirty {
                                    z.warning
                                } else if enabled {
                                    z.text
                                } else {
                                    z.text_placeholder
                                })
                                .when_some(row.overridden, |d, on| {
                                    d.child(
                                        crate::widgets::checkbox(
                                            SharedString::from(format!("override-{kind:?}-{key}")),
                                            on,
                                            false,
                                        )
                                        .tooltip(|window, cx| {
                                            gpui_component::tooltip::Tooltip::new(
                                                crate::i18n::tr("Перевизначити значення принтера"),
                                            )
                                            .build(window, cx)
                                        })
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.toggle_override(kind, key, window, cx)
                                        })),
                                    )
                                })
                                .child({
                                    let label = div().min_w_0().flex_1().truncate().child(label);
                                    if let Some(input) = self
                                        .editor(kind)
                                        .and_then(|e| e.inputs.get(key))
                                        .filter(|input| {
                                            enabled
                                                && self
                                                    .scrub_bindings
                                                    .contains_key(&input.entity_id())
                                        })
                                    {
                                        crate::scrub::zone(label, input, window, cx)
                                    } else {
                                        label
                                    }
                                })
                                .when(!tooltip.is_empty(), |d| {
                                    let title = help_title.clone();
                                    let description = help_description.clone();
                                    let key = help_key.clone();
                                    let url = help_url.clone();
                                    d.child(
                                        crate::ui::icon_button(
                                            SharedString::from(format!(
                                                "setting-help-{kind:?}-{key}"
                                            )),
                                            crate::ui::IconName::CircleQuestionMark,
                                            "Повний опис налаштування",
                                        )
                                        .text_color(z.text_placeholder)
                                        .on_click(
                                            cx.listener(move |this, _, window, cx| {
                                                this.open_setting_help(
                                                    title.clone(),
                                                    key.clone(),
                                                    description.clone(),
                                                    url.clone(),
                                                    window,
                                                    cx,
                                                )
                                            }),
                                        ),
                                    )
                                });
                let reset = div().w(px(24.)).flex_none().when(row.dirty, |d| {
                            d.child(
                                crate::ui::icon_button(
                                    SharedString::from(format!("reset-{kind:?}-{key}")),
                                    crate::ui::IconName::Undo2,
                                    "Повернути системне значення",
                                )
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.reset_value(kind, key, window, cx)
                                    },
                                )),
                            )
                        });
                let row_element = div()
                    .id(SharedString::from(format!("row-{kind:?}-{key}")))
                    .flex()
                    .gap(px(8.))
                    .min_h(px(28.))
                    .pl(px(26.))
                    .pr(px(4.))
                    .hover(|s| s.bg(z.element_hover.opacity(0.5)));
                let row_element = if wide {
                    // Багаторядкове поле — на всю ширину під підписом.
                    row_element
                        .flex_col()
                        .gap(px(4.))
                        .pb(px(6.))
                        .child(div().flex().items_center().gap(px(8.)).child(label_area).child(reset))
                        .child(div().pr(px(28.)).child(control))
                } else {
                    row_element
                        .items_center()
                        .child(label_area)
                        .child(div().flex_1().min_w_0().child(control))
                        .child(reset)
                };
                row_elements.push(
                    row_element
                        .when(!tooltip.is_empty(), |d| {
                            d.tooltip(move |window, cx| {
                                crate::setting_help::tooltip_with_key(
                                    help_title.clone().into(),
                                    tooltip.clone().into(),
                                    help_url.is_some(),
                                    key,
                                    window,
                                    cx,
                                )
                            })
                        })
                        .into_any_element(),
                );
            }
            group_elements.push(
                div()
                    .flex()
                    .flex_col()
                    .pb(px(12.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .px(px(10.))
                            .pt(px(10.))
                            .pb(px(8.))
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(z.text)
                            .children(crate::pattern_icons::section_icon(source_title).map(
                                |path| {
                                    gpui::svg()
                                        .path(path)
                                        .size(px(16.))
                                        .flex_none()
                                        .text_color(z.text)
                                },
                            ))
                            .child(title.to_owned())
                            .child(div().flex_1().h(px(1.)).bg(z.border)),
                    )
                    .children(row_elements),
            );
        }

        let pages_row = div()
            .flex()
            .flex_wrap()
            .gap(px(2.))
            .px(px(6.))
            .py(px(4.))
            .children(visible_pages.iter().map(|&i| (i, &pages[i])).map(|(i, p)| {
                use crate::ui::Toggle as _;
                crate::ui::ghost_button(
                    SharedString::from(format!("page-{kind:?}-{i}")),
                    None,
                    p.title(crate::i18n::locale()).to_owned(),
                )
                .text_color(z.text_muted)
                .toggled(i == page_index)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(editor) = this.editor_mut(kind) {
                        editor.page = i;
                    }
                    cx.notify();
                }))
            }));

        let actions = {
            use crate::ui::Toggle as _;
            div()
                .flex()
                .items_center()
                .gap(px(2.))
                .pl(px(6.))
                .pr(px(10.))
                .pb(px(4.))
                .child(
                    crate::ui::ghost_button(
                        SharedString::from(format!("save-as-{kind:?}")),
                        Some(crate::ui::IconName::Save),
                        "Зберегти як…",
                    )
                    .text_xs()
                    .inert(!dirty)
                    .when(dirty, |d| {
                        d.on_click(cx.listener(move |this, _, window, cx| {
                            this.open_save_preset_dialog(kind, window, cx)
                        }))
                    }),
                )
                .child(
                    crate::ui::ghost_button(
                        SharedString::from(format!("reset-all-{kind:?}")),
                        Some(crate::ui::IconName::Undo2),
                        "Скинути все",
                    )
                    .text_xs()
                    .inert(!dirty)
                    .when(dirty, |d| {
                        d.on_click(cx.listener(move |this, _, window, cx| {
                            if let Some(editor) = this.editor_mut(kind) {
                                let keys: Vec<String> =
                                    editor.preset.dirty_keys().map(str::to_owned).collect();
                                for key in keys {
                                    this.reset_value(kind, &key, window, cx);
                                }
                            }
                            this.cancel_slice();
                        }))
                    }),
                )
                .when(app_owned, |d| {
                    d.child(
                        crate::ui::icon_button(
                            SharedString::from(format!("rename-preset-{kind:?}")),
                            crate::ui::IconName::Pencil,
                            "Перейменувати пресет",
                        )
                        .inert(dirty)
                        .when(!dirty, |d| {
                            d.on_click(cx.listener(move |this, _, window, cx| {
                                this.open_preset_management(kind, false, window, cx)
                            }))
                        }),
                    )
                    .child(
                        crate::ui::danger_icon_button(
                            SharedString::from(format!("delete-preset-{kind:?}")),
                            crate::ui::IconName::Trash,
                            "Видалити власний пресет",
                        )
                        .inert(dirty)
                        .when(!dirty, |d| {
                            d.on_click(cx.listener(move |this, _, window, cx| {
                                this.open_preset_management(kind, true, window, cx)
                            }))
                        }),
                    )
                })
                .child(div().flex_1())
                .child(
                    crate::widgets::switch(
                        SharedString::from(format!("advanced-{kind:?}")),
                        advanced,
                    )
                    .text_xs()
                    .text_color(z.text_muted)
                    .child(crate::i18n::tr("Розширені"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(editor) = this.editor_mut(kind) {
                            editor.advanced = !advanced;
                        }
                        cx.notify();
                    })),
                )
        };

        div()
            .flex()
            .flex_col()
            .child(pages_row)
            .child(actions)
            .child(crate::ui::divider())
            .children(group_elements)
            .into_any_element()
    }

    /// Модалка з назвою нового пресета; зберігає дельту від поточного батьківського.
    pub fn open_save_preset_dialog(
        &mut self,
        kind: PresetType,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.editor(kind) else {
            return;
        };
        let suggested = crate::tr_format!(
            "{} (мій)",
            editor.preset.parent().trim_end_matches(" (мій)")
        );
        let name = cx.new(|cx| InputState::new(window, cx).default_value(suggested));
        let save = {
            let name = name.clone();
            Rc::new(
                move |ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>| {
                    let text = name.read(cx).value().trim().to_owned();
                    match ws.save_preset(kind, &text, cx) {
                        Ok(()) => {
                            ws.close_modal(cx);
                            ws.toast(
                                ToastKind::Success,
                                crate::tr_format!("Збережено пресет «{text}»"),
                                cx,
                            );
                            // Збереження з діалогу зміни рушія: продовжуємо перехід (інші
                            // незбережені зміни знову покаже той самий діалог).
                            if let Some(engine) = ws.pending_engine.take() {
                                ws.activate_engine(Ok(engine), window, cx);
                            }
                        }
                        Err(err) => ws.toast(ToastKind::Error, format!("{err:#}"), cx),
                    }
                },
            )
        };
        let sub = {
            let save = save.clone();
            cx.subscribe_in(
                &name,
                window,
                move |ws, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        save(ws, window, cx);
                    }
                },
            )
        };
        self._subscriptions.push(sub);
        let focus = name.focus_handle(cx);
        self.open_modal(
            Rc::new(move |ws, window, cx| {
                let z = crate::theme::zed();
                let save = save.clone();
                crate::widgets::modal(
                    "Зберегти пресет",
                    460.,
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .p(px(12.))
                        .child(
                            div()
                                .text_xs()
                                .text_color(z.text_muted)
                                .child(crate::i18n::tr("Назва")),
                        )
                        .child(crate::widgets::field(&name, Input::new(&name), window, cx))
                        .child(div().text_xs().text_color(z.text_placeholder).child(
                            crate::tr_format!(
                                    "Зберігається в {}",
                                    crate::shorten_home(
                                        ws.presets
                                            .as_ref()
                                            .map(|s| s.user_root())
                                            .unwrap_or(std::path::Path::new(""))
                                    )
                                ),
                        )),
                    Some(
                        div()
                            .flex()
                            .gap(px(6.))
                            .child(
                                crate::ui::ghost_button("save-preset-cancel", None, "Скасувати")
                                    .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))),
                            )
                            .child(
                                crate::ui::accent_button(
                                    "save-preset-ok",
                                    crate::ui::IconName::Save,
                                    "Зберегти",
                                    true,
                                )
                                .on_click(
                                    cx.listener(move |this, _, window, cx| save(this, window, cx)),
                                ),
                            )
                            .into_any_element(),
                    ),
                )
            }),
            window,
            cx,
        );
        window.focus(&focus, cx);
    }

    fn save_preset(
        &mut self,
        kind: PresetType,
        name: &str,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        let store = self.presets.clone().context("профілі ще не завантажені")?;
        if store.get(kind.into(), name).is_some() {
            anyhow::bail!("Пресет «{name}» уже існує — обери іншу назву");
        }
        let editor = self.editor(kind).context("немає редактора")?;
        let dir = store.user_root().join(slicer::Kind::from(kind).dir());
        let version = editor
            .preset
            .base()
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let info = settings::UserPresetInfo {
            version: version.or_else(|| store.profile_version()),
            ..Default::default()
        };
        editor.preset.save_user(&dir, name, &info)?;
        // Перечитуємо профілі й одразу вибираємо новий.
        let store = std::sync::Arc::new(store.reload()?);
        let mut selection = self.selection.clone();
        match kind {
            PresetType::Process => selection.process = name.to_owned(),
            PresetType::Filament => selection.filament = name.to_owned(),
            PresetType::Machine => selection.machine = name.to_owned(),
        }
        self.presets = Some(store);
        self.set_selection(selection, cx);
        Ok(())
    }

    /// Окреме вікно-модалка редактора (для філаменту й принтера, як шестерня в Bambu Studio).
    pub fn open_editor_dialog(
        &mut self,
        kind: PresetType,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_modal(
            Rc::new(move |ws, window, cx| {
                let title = match kind {
                    PresetType::Process => crate::tr_format!("Процес · {}", ws.selection.process),
                    PresetType::Filament => {
                        crate::tr_format!("Філамент · {}", ws.selection.filament)
                    }
                    PresetType::Machine => crate::tr_format!("Принтер · {}", ws.selection.machine),
                };
                let body = ws.preset_editor_view(kind, window, cx);
                crate::widgets::modal(
                    title,
                    720.,
                    div()
                        .id("editor-dialog-body")
                        .flex_1()
                        .min_h_0()
                        .max_h(px(640.))
                        .overflow_y_scroll()
                        .pb(px(8.))
                        .child(body),
                    None,
                )
            }),
            window,
            cx,
        );
    }
}

impl Workspace {
    fn open_preset_management(
        &mut self,
        kind: PresetType,
        delete: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.editor(kind) else {
            return;
        };
        if editor.is_dirty() || self.slicing() {
            return;
        }
        let old = editor.preset.parent().to_string();
        let name = cx.new(|cx| InputState::new(window, cx).default_value(old.clone()));
        self.open_modal(Rc::new(move |_, window, cx| {
            let text = if delete { crate::tr_format!("Видалити власний пресет «{old}»? Пресет із дочірніми профілями видалити не можна.") }
                else { crate::tr_format!("Нова назва для «{old}»") };
            let body = div().flex().flex_col().p(px(12.)).gap(px(10.)).child(div().text_sm().child(text))
                .when(!delete, |d| d.child(crate::widgets::field(&name, Input::new(&name), window, cx)));
            let name = name.clone(); let old = old.clone();
            let action = move |this: &mut Workspace, _: &gpui::ClickEvent, _: &mut Window, cx: &mut Context<Workspace>| {
                let new = name.read(cx).value().trim().to_string();
                let result = (|| -> anyhow::Result<()> {
                    let store = this.presets.as_mut().context("Профілі ще не завантажені")?;
                    let store = std::sync::Arc::make_mut(store);
                    if delete { store.delete_user(kind.into(), &old)?; }
                    else { store.rename_user(kind.into(), &old, &new)?; }
                    let mut selection = this.selection.clone();
                    let value = if delete { String::new() } else { new };
                    match kind {
                        PresetType::Machine => selection.machine = value,
                        PresetType::Process => selection.process = value,
                        PresetType::Filament => selection.filament = value,
                    }
                    this.set_selection(selection, cx);
                    Ok(())
                })();
                match result {
                    Ok(()) => { this.close_modal(cx); this.toast(ToastKind::Success, if delete { "Пресет видалено" } else { "Пресет перейменовано" }, cx); },
                    Err(err) => this.toast(ToastKind::Error, format!("{err:#}"), cx),
                }
            };
            let footer = if delete {
                crate::ui::danger_button("preset-manage-ok", crate::ui::IconName::Trash, "Видалити").on_click(cx.listener(action)).into_any_element()
            } else {
                crate::ui::accent_button("preset-manage-ok", crate::ui::IconName::Save, "Перейменувати", true).on_click(cx.listener(action)).into_any_element()
            };
            crate::widgets::modal(if delete { "Видалення пресета" } else { "Перейменування пресета" }, 520., body, Some(footer))
        }), window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn multiline_text_keeps_whitespace_and_line_endings() {
        let text = "G28\r\n  M104 S200 \n\n";
        assert_eq!(
            value_from_text(TextKind::Multiline, Some(&json!("x")), text),
            json!(text)
        );
        assert_eq!(
            value_from_text(TextKind::Multiline, Some(&json!(["a"])), text),
            json!([text])
        );
        assert_eq!(
            value_from_text(TextKind::Line, Some(&json!(["0.2"])), " 0.4 "),
            json!(["0.4"])
        );
        let value = json!(text);
        assert_eq!(field_text(TextKind::Multiline, Some(&value)), text);
    }

    #[test]
    fn gcode_options_are_multiline() {
        let schema = Schema::load_orca().unwrap();
        for key in ["machine_start_gcode", "machine_end_gcode", "layer_change_gcode", "change_filament_gcode", "filament_start_gcode", "filament_notes", "printer_notes"] {
            assert_eq!(text_kind(&schema, key), TextKind::Multiline, "{key}");
        }
        assert_eq!(text_kind(&schema, "compatible_printers"), TextKind::List);
        assert_eq!(text_kind(&schema, "layer_height"), TextKind::Line);
        assert!(is_gcode("machine_start_gcode") && !is_gcode("filament_notes"));
    }

    /// Системний принтер Bambu бере `printable_area` з `fdm_bbl_3dp_001_common`; редактор
    /// показував лише першу вершину («0x0»), а правка заміняла всі вершини одним значенням.
    #[test]
    fn printable_area_is_inherited_and_shown_as_whole_polygon() {
        let dir = tempfile::tempdir().unwrap();
        let machine = dir.path().join("system/BBL/machine");
        std::fs::create_dir_all(&machine).unwrap();
        let write = |name: &str, value: Value| {
            std::fs::write(machine.join(format!("{name}.json")), value.to_string()).unwrap();
        };
        write(
            "fdm_bbl_3dp_001_common",
            json!({"name": "fdm_bbl_3dp_001_common", "type": "machine", "instantiation": "false",
                   "printable_area": ["0x0", "256x0", "256x256", "0x256"]}),
        );
        write(
            "Bambu Lab A1 0.4 nozzle",
            json!({"name": "Bambu Lab A1 0.4 nozzle", "type": "machine", "instantiation": "true",
                   "inherits": "fdm_bbl_3dp_001_common", "nozzle_diameter": ["0.4"]}),
        );
        let store = PresetStore::open_with_own_root(dir.path(), &dir.path().join("own")).unwrap();
        let schema = Schema::load().unwrap();
        let mut preset =
            EditablePreset::from_system(&schema, &store, PresetType::Machine, "Bambu Lab A1 0.4 nozzle")
                .unwrap();
        let kind = text_kind(&schema, "printable_area");
        assert_eq!(kind, TextKind::Points);
        assert!(!kind.is_wide());
        let shown = field_text(kind, preset.shown_value("printable_area"));
        assert_eq!(shown, "0x0,256x0,256x256,0x256");

        let edited = value_from_text(kind, preset.shown_value("printable_area"), " 0x0, 180x0,180x180 ,0x180");
        assert_eq!(edited, json!(["0x0", "180x0", "180x180", "0x180"]));
        preset.set("printable_area", edited).unwrap();
        assert_eq!(field_text(kind, preset.shown_value("printable_area")), "0x0,180x0,180x180,0x180");

        // Рядок замість вектора (так пишуть деякі профілі Orca) лишається рядком.
        assert_eq!(value_from_text(kind, Some(&json!("0x0,1x0")), "0x0,2x0"), json!("0x0,2x0"));
        // Зміщення екструдерів — по одній точці на екструдер, як і раніше.
        assert_eq!(text_kind(&schema, "extruder_offset"), TextKind::Line);
        let groups = json!(["0x0,256x0,256x256,0x256", "25x0,281x0,281x256,25x256"]);
        let kind = text_kind(&schema, "extruder_printable_area");
        assert_eq!(kind, TextKind::PointGroups);
        let text = field_text(kind, Some(&groups));
        assert_eq!(value_from_text(kind, Some(&groups), &text), groups);
    }

    #[test]
    fn hidden_selected_page_falls_back_to_first_visible() {
        assert_eq!(shown_page(2, &[0, 2, 3]), 2);
        assert_eq!(shown_page(1, &[0, 2, 3]), 0);
        assert_eq!(shown_page(4, &[]), 4);
    }
}
