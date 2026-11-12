//! Application/editor preferences, separate from engine presets and project data.
use crate::{
    theme,
    ui::{self, IconName, Toggle as _},
    widgets::{self, ToastKind},
    workspace::Workspace,
};
use gpui::{Context, Window, div, prelude::*, px};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    rc::Rc,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Preferences {
    pub mcp_enabled: bool,
    pub mcp_allow_edit: bool,
    pub mcp_port: u16,
    /// Режим теми; старі файли з `light` мігрує `load_from`.
    pub theme: theme::ThemeChoice,
    /// Моноширинний шрифт Omarchy (fontconfig `monospace`) для інтерфейсу.
    pub omarchy_font: bool,
    pub language: String,
    pub zoom_to_cursor: bool,
    pub reverse_zoom: bool,
    pub left_drag_moves: bool,
    pub navigation_cube: bool,
    pub section_plane: bool,
    pub remember_scheme: bool,
    pub last_scheme: usize,
    /// Майстер першого запуску пройдено або пропущено. Відсутнє поле в наявному файлі —
    /// давній користувач: майстер не нав’язуємо (див. `load_from`).
    pub wizard_done: bool,
    /// Останній вибраний профіль принтера (машини); порожній — як у рушія.
    pub printer: String,
    /// Як відкривати 3MF з іншого слайсера чи з несумісними налаштуваннями.
    pub open_3mf: crate::geometry_import::OpenMode,
    /// Корені бібліотеки моделей на «Головній». `None` — ще не вибирали (типова тека),
    /// порожній список — користувач прибрав усі.
    pub library_roots: Option<Vec<PathBuf>>,
    /// Недавні теки бібліотеки, найновіша перша.
    pub library_recent: Vec<PathBuf>,
    /// Галочки діалогу друку окремо для кожного принтера (ключ — ідентифікатор пристрою).
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub print_options: std::collections::BTreeMap<String, crate::device::PrintOptions>,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            mcp_enabled: false,
            mcp_allow_edit: false,
            mcp_port: 7357,
            theme: theme::ThemeChoice::System,
            omarchy_font: false,
            language: "uk".into(),
            zoom_to_cursor: true,
            reverse_zoom: false,
            left_drag_moves: true,
            navigation_cube: true,
            section_plane: true,
            remember_scheme: true,
            last_scheme: 0,
            wizard_done: false,
            printer: String::new(),
            open_3mf: Default::default(),
            library_roots: None,
            library_recent: Vec::new(),
            print_options: Default::default(),
            extra: Default::default(),
        }
    }
}
impl Preferences {
    /// Галочки діалогу друку для принтера; ще не вибирали — типові.
    pub fn print_options_for(&self, dev: &str) -> crate::device::PrintOptions {
        self.print_options.get(dev).copied().unwrap_or_default()
    }
    pub(crate) fn path() -> PathBuf {
        paths::config_dir().join("preferences.json")
    }
    fn load_from(path: &Path) -> anyhow::Result<Self> {
        let bytes = match std::fs::read(path) {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        let mut prefs: Self = serde_json::from_slice(&bytes)?;
        let raw = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap_or_default();
        // Файл без поля створила старіша версія: це не перший запуск.
        if raw.get("wizard_done").is_none() {
            prefs.wizard_done = true;
        }
        // Давніше було лише `light`: true — явний вибір світлої; false був типовим,
        // тож такий користувач отримує нову типову «Системну».
        if let Some(light) = prefs.extra.remove("light")
            && raw.get("theme").is_none()
            && light.as_bool() == Some(true)
        {
            prefs.theme = theme::ThemeChoice::Light;
        }
        if prefs.last_scheme >= gcode::ColorScheme::ALL.len() {
            prefs.last_scheme = 0;
        }
        Ok(prefs)
    }
    pub fn load() -> anyhow::Result<Self> {
        Self::load_from(&Self::path())
    }
    fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("invalid preferences path"))?;
        std::fs::create_dir_all(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut temp, self)?;
        temp.persist(path)?;
        Ok(())
    }
    pub fn save(&self) -> anyhow::Result<()> {
        self.save_to(&Self::path())
    }
}
#[derive(Clone, Copy)]
enum Flag {
    ZoomToCursor,
    ReverseZoom,
    LeftDrag,
    Cube,
    Plane,
    Scheme,
}
/// Заголовок групи на одній лінії з рядками налаштувань.
fn pref_title(title: &str) -> gpui::Div {
    ui::group_title(title.to_owned()).px_0()
}

/// Режими теми для перемикача; «Omarchy» — лише коли його тему знайдено.
fn theme_choices() -> Vec<(theme::ThemeChoice, &'static str)> {
    let mut out = vec![
        (theme::ThemeChoice::System, "Наслідувати системну"),
        (theme::ThemeChoice::Light, "Світла"),
        (theme::ThemeChoice::Dark, "Темна"),
    ];
    if theme::omarchy_detected() {
        out.push((theme::ThemeChoice::Omarchy, "Omarchy"));
    }
    out
}

/// Спільна ширина вікна налаштувань для всіх розділів.
const PREFS_WIDTH: f32 = 820.;

/// Розділи налаштувань у бічній навігації.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Section {
    General,
    Viewport,
    Engines,
    Plugins,
    Printers,
    Mcp,
    About,
}
impl Section {
    const ALL: [Section; 7] = [
        Section::General,
        Section::Viewport,
        Section::Engines,
        Section::Plugins,
        Section::Printers,
        Section::Mcp,
        Section::About,
    ];
    fn title(self) -> &'static str {
        match self {
            Section::General => "Загальні",
            Section::Viewport => "3D і миша",
            Section::Engines => "Рушій нарізки",
            Section::Plugins => "Плагіни й хмара",
            Section::Printers => "Принтери й з’єднання",
            Section::Mcp => "MCP (AI-агенти)",
            Section::About => "Про програму",
        }
    }
    fn summary(self) -> &'static str {
        match self {
            Section::General => "Мова й вигляд інтерфейсу.",
            Section::Viewport => "Керування камерою й об’єктами в 3D-вікні.",
            Section::Engines => "Програма, що нарізає модель, і як відкривати чужі 3MF.",
            Section::Plugins => "Додаткові інструменти й хмарні сервіси.",
            Section::Printers => "Вибраний принтер і зв’язок з ним.",
            Section::Mcp => "Доступ AI-агентів (Claude та інших) до відкритого проєкту.",
            Section::About => "Версія, ліцензія й де лежать файли.",
        }
    }
    fn icon(self) -> IconName {
        match self {
            Section::General => IconName::SlidersHorizontal,
            Section::Viewport => IconName::Rotate3d,
            Section::Engines => IconName::Cpu,
            Section::Plugins => IconName::Plug,
            Section::Printers => IconName::Printer,
            Section::Mcp => IconName::Sparkles,
            Section::About => IconName::Info,
        }
    }
}

/// Рядок налаштування: назва й опис під нею ліворуч, контрол праворуч.
fn row(label: &str, description: Option<&str>, control: impl IntoElement) -> gpui::Div {
    row_owned(crate::i18n::tr(label), description, control)
}
fn row_owned(label: String, description: Option<&str>, control: impl IntoElement) -> gpui::Div {
    let z = theme::zed();
    div().flex().items_center().gap(px(16.)).py(px(5.)).min_h(px(32.))
        .child(div().flex().flex_col().flex_1().min_w_0().gap(px(1.))
            .child(div().text_sm().child(label))
            .children(description.map(|d| div().text_xs().text_color(z.text_muted).child(crate::i18n::tr(d)))))
        .child(div().flex_none().child(control))
}
fn hint(text: &str) -> gpui::Div {
    div().text_xs().text_color(theme::zed().text_muted).child(crate::i18n::tr(text))
}
fn section_gap() -> gpui::Div {
    div().h(px(10.))
}

impl Workspace {
    pub fn save_preferences(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = self.preferences.save() {
            self.toast(
                ToastKind::Error,
                crate::tr_format!("Не вдалося зберегти налаштування: {e:#}"),
                cx,
            );
        }
    }
    /// Перерахувати тему з налаштувань і стану ОС; рендерер бере той самий фон.
    pub fn refresh_theme(&mut self, cx: &mut Context<Self>) -> bool {
        let changed = theme::refresh(self.preferences.theme, self.preferences.omarchy_font, cx);
        self.renderer.set_light_theme(theme::is_light());
        self.renderer
            .set_scene_background(theme::scene_background().map(|c| c.linear()));
        if changed {
            self.dirty = true;
        }
        changed
    }
    /// Слухати тему ОС (Omarchy, портал) і застосовувати зміни наживо, без перезапуску.
    pub fn watch_system_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if paths::env("SLICENV_NO_SYSTEM_THEME").is_some() {
            return;
        }
        // Світла/темна тема ОС від gpui: на macOS і Windows це єдине джерело (портал є лише на
        // Linux, і там він має пріоритет — див. `theme::resolve`).
        cx.observe_window_appearance(window, |ws, window, cx| {
            let dark = matches!(
                window.appearance(),
                gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
            );
            theme::update_system(|s| s.appearance_dark = Some(dark));
            if ws.refresh_theme(cx) {
                window.refresh();
                cx.notify();
            }
        })
        .detach();
        let events = crate::system_theme::spawn_watchers();
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = events.recv().await {
                match event {
                    crate::system_theme::Event::Omarchy(t) => {
                        eprintln!(
                            "Slicenv: Omarchy: {}",
                            t.as_ref().map_or("тему не знайдено".into(), |t| format!(
                                "«{}», фон {}, акцент {}",
                                t.name,
                                t.background.hex(),
                                t.accent.hex()
                            ))
                        );
                        theme::update_system(|s| s.omarchy = t);
                    }
                    crate::system_theme::Event::Portal(p) => {
                        eprintln!(
                            "Slicenv: портал: color-scheme={:?}, accent-color={:?}",
                            p.color_scheme,
                            p.accent.map(|c| c.hex())
                        );
                        theme::update_system(|s| s.portal = p);
                    }
                    crate::system_theme::Event::Titlebar(prefs) => {
                        // Кнопки й дії заголовка: тема не міняється, лише перемальовується вікно.
                        if theme::update_titlebar(&prefs) {
                            let alive = this.update_in(cx, |_, window, cx| {
                                window.refresh();
                                cx.notify();
                            });
                            if alive.is_err() {
                                return;
                            }
                        }
                        continue;
                    }
                }
                let alive = this.update_in(cx, |ws, window, cx| {
                    if ws.refresh_theme(cx) {
                        window.refresh();
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }
    fn update_preferences(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_theme(cx);
        self.cut_ghost = self.preferences.section_plane;
        if !self.preferences.navigation_cube {
            self.cancel_navigation();
        }
        self.dirty = true;
        self.save_preferences(cx);
        window.refresh();
        cx.notify();
    }
    pub fn open_preferences(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_preferences_at(Section::General, window, cx);
    }
    pub(crate) fn open_preferences_at(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        let current = Rc::new(std::cell::Cell::new(section));
        let port = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx)
                .default_value(self.preferences.mcp_port.to_string())
        });
        self.open_modal(Rc::new(move |ws,window,cx|{
            let z = theme::zed();
            let section = current.get();
            // Одна рамка для всіх розділів: розмір і навігація не стрибають.
            let height = (f32::from(window.viewport_size().height) - 200.).clamp(300., 600.);

            // Бічна навігація, як у налаштуваннях GNOME/macOS; «Майстер» — дія внизу.
            let mut nav = div().flex().flex_col().flex_none().w(px(224.)).h_full().p(px(8.)).gap(px(2.))
                .border_r_1().border_color(z.border_variant).bg(z.panel);
            for (i, item) in Section::ALL.into_iter().enumerate() {
                let current = current.clone();
                nav = nav.child(div().id(("prefs-section", i)).flex().items_center().gap(px(8.)).h(px(30.)).px(px(8.))
                    .rounded(px(4.)).cursor_pointer().text_sm()
                    .map(|d| if item == section { d.bg(z.element_selected).text_color(z.text) } else { d.text_color(z.text_muted).hover(|s| s.bg(z.element_hover).text_color(z.text)) })
                    .child(ui::icon(item.icon()))
                    .child(crate::i18n::tr(item.title()))
                    .on_click(cx.listener(move |_,_,_,cx| { current.set(item); cx.notify(); })));
            }
            nav = nav.child(div().flex_1()).child(ui::divider().my(px(6.)))
                .child(ui::ghost_button("prefs-wizard", Some(IconName::Sparkles), "Майстер налаштування…")
                    .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new(crate::i18n::tr("Покроково вибрати рушій нарізки, принтер і профілі")).build(window, cx))
                    .on_click(cx.listener(|this,_,window,cx| { this.close_modal(cx); this.open_wizard(window,cx); })));

            let content = match section {
                Section::Mcp => ws.mcp_preferences(&port, window, cx),
                _ => ws.preferences_section(section, cx),
            };
            let content = div().flex().flex_col().flex_1().min_w_0()
                .child(div().flex().flex_col().gap(px(2.)).px(px(20.)).pt(px(16.)).pb(px(10.))
                    .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child(crate::i18n::tr(section.title())))
                    .child(div().text_xs().text_color(z.text_muted).child(crate::i18n::tr(section.summary()))))
                .child(div().id("prefs-content").flex_1().min_h_0().overflow_y_scroll().child(content));
            let body = div().flex().h(px(height)).child(nav).child(content);
            let footer = div().flex().items_center().gap(px(8.))
                .child(div().text_xs().text_color(z.text_placeholder).child(crate::i18n::tr("Зміни застосовуються одразу · Ctrl+,")))
                .child(ui::ghost_button("preferences-close",Some(IconName::Check),"Готово").on_click(cx.listener(|this,_,_,cx|this.close_modal(cx))));
            widgets::modal("Налаштування",PREFS_WIDTH,body,Some(footer.into_any_element())).into_any_element()
        }),window,cx);
    }

    /// Вміст розділу (крім MCP — він у `mcp_preferences`).
    fn preferences_section(&mut self, section: Section, cx: &mut Context<Self>) -> gpui::AnyElement {
        let z = theme::zed();
        let mut body = div().flex().flex_col().px(px(20.)).pb(px(20.)).gap(px(6.)).text_sm();
        match section {
            Section::General => {
                let languages = crate::i18n::languages();
                body = body.child(pref_title("Мова інтерфейсу"))
                    .child(div().flex().flex_wrap().gap(px(6.)).children(languages.into_iter().enumerate().map(|(i,(code,name))|{
                        ui::ghost_button(("preferences-language",i),None,name).toggled(self.preferences.language==code)
                            .on_click(cx.listener(move|this,_,window,cx|{
                                if let Err(e)=crate::i18n::set_language(&code) {this.toast(ToastKind::Error,e.to_string(),cx);return;}
                                this.preferences.language=code.clone();
                                this.library_search.update(cx,|input,cx|input.set_placeholder(crate::i18n::tr("Пошук моделей…"),window,cx));
                                if let Some(input)=this.cloud.serial_input.clone() {input.update(cx,|input,cx|input.set_placeholder(crate::i18n::tr("Серійний номер, напр. 03919C…"),window,cx));}
                                if let Some(e)=&mut this.process_editor {e.clear_controls();}
                                if let Some(e)=&mut this.machine_editor {e.clear_controls();}
                                if let Some(e)=&mut this.filament_editor {e.clear_controls();}
                                this.update_preferences(window,cx);
                            }))
                    })))
                    .child(hint("Назви параметрів рушія нарізки теж перекладаються, якщо рушій має переклад."))
                    .child(section_gap())
                    .child(pref_title("Вигляд"))
                    .child(row("Тема", Some("«Наслідувати системну» бере тему Omarchy, якщо вона є, інакше світлу чи темну тему ОС. Вибір «Світла» чи «Темна» тема ОС не змінює."),
                        div().flex().gap(px(4.)).children(theme_choices().into_iter().enumerate().map(|(i,(choice,label))|{
                            ui::ghost_button(("preferences-theme",i),None,label).toggled(self.preferences.theme==choice)
                                .on_click(cx.listener(move|this,_,window,cx|{this.preferences.theme=choice;this.update_preferences(window,cx);} ))
                        }))))
                    .children(theme::applied_description().map(|text| div().pb(px(4.)).text_xs().text_color(theme::zed().text_muted).child(text)))
                    .children(theme::omarchy_detected().then(|| row("Моноширинний шрифт Omarchy в інтерфейсі", None,
                        widgets::switch("omarchy-font",self.preferences.omarchy_font).on_click(cx.listener(|this,_,window,cx|{
                            this.preferences.omarchy_font=!this.preferences.omarchy_font;this.update_preferences(window,cx);
                        })))));
            }
            Section::Viewport => {
                body = body.child(pref_title("Камера"));
                let mut flags = vec![
                    ("zoom-cursor","Масштабувати до курсора","Коліщатко наближає до точки під курсором, а не до центру вікна.",self.preferences.zoom_to_cursor,Flag::ZoomToCursor),
                    ("reverse-zoom","Зворотний напрямок коліщатка","Прокрутка вгору віддаляє, як у деяких CAD.",self.preferences.reverse_zoom,Flag::ReverseZoom),
                    ("navigation-cube","Показувати навігаційний куб","Куб у куті 3D-вікна для швидких ракурсів.",self.preferences.navigation_cube,Flag::Cube),
                ];
                let edit = vec![
                    ("left-drag","Переміщувати об’єкти лівою кнопкою","Вимкнено — ліва кнопка обертає камеру; гізмо доступне завжди.",self.preferences.left_drag_moves,Flag::LeftDrag),
                    ("section-plane","Показувати площину перерізу","Напівпрозора площина, коли ріжеш або переглядаєш переріз моделі.",self.preferences.section_plane,Flag::Plane),
                    ("remember-scheme","Запам’ятовувати схему кольорів перегляду","«Перегляд» відкривається з останньою вибраною схемою.",self.preferences.remember_scheme,Flag::Scheme),
                ];
                let push = |body: gpui::Stateful<gpui::Div>, list: Vec<(&'static str,&'static str,&'static str,bool,Flag)>| {
                    let mut body = body;
                    for (id,label,description,value,flag) in list {
                        body = body.child(row(label, Some(description), widgets::switch(id,value).on_click(cx.listener(move|this,_,window,cx|{
                            let field=match flag {Flag::ZoomToCursor=>&mut this.preferences.zoom_to_cursor,Flag::ReverseZoom=>&mut this.preferences.reverse_zoom,Flag::LeftDrag=>&mut this.preferences.left_drag_moves,Flag::Cube=>&mut this.preferences.navigation_cube,Flag::Plane=>&mut this.preferences.section_plane,Flag::Scheme=>&mut this.preferences.remember_scheme};
                            *field=!*field;this.update_preferences(window,cx);
                        }))));
                    }
                    body
                };
                let mut b = push(body.id("prefs-viewport"), std::mem::take(&mut flags));
                b = b.child(section_gap()).child(pref_title("Редагування"));
                return push(b, edit).into_any_element();
            }
            Section::Engines => {
                let engine = self.engine_label.clone();
                body = body.child(pref_title("Активний рушій нарізки"))
                    .child(row_owned(engine.to_string(), Some("Рушій нарізки перетворює модель на G-code. Bambu Studio й OrcaSlicer мають власні профілі."),
                        ui::ghost_button("prefs-engines", Some(IconName::Settings), "Змінити рушій…").border_1().border_color(z.border_variant)
                            .on_click(cx.listener(|this,_,window,cx| { this.close_modal(cx); this.open_engines(window, cx); }))))
                    .child(row("Перевірити сумісність проєкту", Some("Показує, чи завадять налаштування відкритого 3MF нарізці цим рушієм."),
                        ui::ghost_button("prefs-migration", Some(IconName::SlidersHorizontal), "Перевірити…").border_1().border_color(z.border_variant)
                            .on_click(cx.listener(|this,_,window,cx| { this.close_modal(cx); this.open_migration(window, cx); }))))
                    .child(row("Версії рушіїв", Some("Встановити, оновити чи видалити Bambu Studio й OrcaSlicer, якими керує Slicenv."),
                        ui::ghost_button("prefs-packages", Some(IconName::Download), "Пакети…").border_1().border_color(z.border_variant)
                            .on_click(cx.listener(|this,_,window,cx| { this.close_modal(cx); this.open_packages(window, cx); }))))
                    .child(section_gap())
                    .child(pref_title("Відкриття 3MF з іншого слайсера"))
                    .child(hint("Коли файл створено в іншому слайсері чи версії, або його налаштування завадять нарізці."));
                for (i, (mode, label, description)) in [
                    (crate::geometry_import::OpenMode::Ask, "Питати щоразу", "Показати вибір під час відкриття."),
                    (crate::geometry_import::OpenMode::Project, "Відкривати як проєкт (з налаштуваннями)", "Усе з файлу; несумісне можна виправити перевіркою сумісності."),
                    (crate::geometry_import::OpenMode::Geometry, "Імпортувати лише геометрію", "Об’єкти, розміщення, пластини й назви; налаштування — з вибраних профілів."),
                ].into_iter().enumerate() {
                    body = body.child(widgets::radio(("prefs-open-mode", i), self.preferences.open_3mf == mode, false)
                        .items_start().py(px(3.))
                        .child(div().flex().flex_col().child(div().text_sm().child(crate::i18n::tr(label)))
                            .child(div().text_xs().text_color(z.text_muted).child(crate::i18n::tr(description))))
                        .on_click(cx.listener(move |this,_,window,cx| { this.preferences.open_3mf = mode; this.update_preferences(window, cx); })));
                }
                body = body.child(section_gap())
                    .child(row("Дозволити новіші 3MF", Some("Експериментально: рушій нарізки відкриє файл новішої версії, ніж він сам. Діє до перезапуску."),
                        widgets::switch("prefs-allow-newer", self.allow_newer_file).on_click(cx.listener(|this,_,_,cx| {
                            if this.engine_loading || this.slicing() { return; }
                            this.allow_newer_file = !this.allow_newer_file; this.cancel_slice(); this.preview = None; cx.notify();
                        }))));
            }
            Section::Plugins => {
                body = body.child(pref_title("Плагіни"))
                    .child(row("Support Fins", Some("Ребра підтримки для високих тонких моделей. Потрібен bun, Node.js 20.10+ або Deno 2."),
                        ui::ghost_button("prefs-plugins", Some(IconName::Plug), "Керувати…").border_1().border_color(z.border_variant)
                            .on_click(cx.listener(|this,_,window,cx| this.open_plugins(window,cx)))))
                    .child(section_gap())
                    .child(pref_title("Хмара Bambu"))
                    .child(hint("Вхід в обліковий запис Bambu і друк через хмару — на сторінці «Пристрій». Мережевий плагін Bambu завантажується під час першого входу."));
                if cloud::obn::SUPPORTED {
                    let status = match cloud::obn::choice() {
                        Some(c) if cloud::obn::active().is_some() => crate::tr_format!(
                            "Підключено ({source}). Хмара: {cloud}",
                            source = c.source.label(),
                            cloud = if c.cloud { crate::i18n::tr("увімкнена") } else { crate::i18n::tr("вимкнена, лише LAN") }
                        ),
                        _ => crate::i18n::tr("Не вибрано"),
                    };
                    body = body.child(section_gap())
                        .child(pref_title("Відкритий плагін (Open Bamboo Networking)"))
                        .child(row_owned(status, Some("Сторонній плагін за вашим вибором: друк на Bambu з Linux без Developer Mode. Не від Bambu Lab, ризик блокування."),
                            ui::ghost_button("prefs-open-plugin", Some(IconName::Plug), "Налаштувати…").border_1().border_color(z.border_variant)
                                .on_click(cx.listener(|this,_,window,cx| { this.close_modal(cx); this.open_open_plugin(window, cx); }))));
                }
            }
            Section::Printers => {
                let printer = self.selection.machine.clone();
                body = body.child(pref_title("Принтер"))
                    .child(row_owned(if printer.is_empty() { crate::i18n::tr("Не вибрано") } else { printer }, Some("Профіль принтера вибирається в панелі «Друк» на «Підготовці»."), div()))
                    .child(section_gap())
                    .child(pref_title("З’єднання"))
                    .child(row("Принтери в мережі та хмарі", Some("Пошук у локальній мережі (LAN), код доступу, камера й надсилання на друк."),
                        ui::ghost_button("prefs-devices", Some(IconName::Printer), "Відкрити «Пристрій»").border_1().border_color(z.border_variant)
                            .on_click(cx.listener(|this,_,_,cx| { this.close_modal(cx); this.enter_page(crate::workspace::Page::Device, cx); }))));
            }
            Section::About => {
                body = body
                    .child(div().flex().items_center().gap(px(10.)).py(px(4.))
                        .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child("Slicenv"))
                        .child(div().text_sm().text_color(z.text_muted).child(env!("CARGO_PKG_VERSION"))))
                    .child(hint("Слайсер для принтерів Bambu Lab: власний інтерфейс поверх рушіїв нарізки Bambu Studio й OrcaSlicer."))
                    .child(section_gap())
                    .child(row("Ліцензія", None, div().text_sm().child(env!("CARGO_PKG_LICENSE"))))
                    .child(row_owned(crate::i18n::tr("Налаштування"), None, div().text_xs().font_family(theme::MONO_FONT).text_color(z.text_muted).child(Preferences::path().display().to_string())))
                    .child(row_owned(crate::i18n::tr("Рушій нарізки"), None, div().text_sm().child(self.engine_label.clone())))
                    .child(hint("Налаштування редактора зберігаються окремо від проєктів і профілів рушія."));
            }
            Section::Mcp => {}
        }
        body.into_any_element()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preferences_keep_unknown_fields_and_load_old_partial_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preferences.json");
        std::fs::write(
            &path,
            br#"{"light":true,"last_scheme":999,"future_editor_option":{"x":2}}"#,
        )
        .unwrap();
        let mut prefs = Preferences::load_from(&path).unwrap();
        assert!(prefs.theme == theme::ThemeChoice::Light && prefs.zoom_to_cursor);
        assert!(!prefs.extra.contains_key("light"));
        // Старий файл без маркера — давній користувач.
        assert!(prefs.wizard_done);
        assert_eq!(prefs.last_scheme, 0);
        prefs.reverse_zoom = true;
        prefs.save_to(&path).unwrap();
        let loaded = Preferences::load_from(&path).unwrap();
        assert_eq!(loaded, prefs);
        assert_eq!(loaded.extra["future_editor_option"]["x"], 2);
        assert_eq!(
            Preferences::load_from(&dir.path().join("missing")).unwrap(),
            Preferences::default()
        );
        assert!(!Preferences::default().wizard_done);
        std::fs::write(&path, br#"{"wizard_done":false}"#).unwrap();
        assert!(!Preferences::load_from(&path).unwrap().wizard_done);
    }

    #[test]
    fn theme_choice_defaults_to_system_and_migrates_old_flag() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preferences.json");
        assert_eq!(Preferences::default().theme, theme::ThemeChoice::System);
        std::fs::write(&path, br#"{"light":false}"#).unwrap();
        assert_eq!(
            Preferences::load_from(&path).unwrap().theme,
            theme::ThemeChoice::System
        );
        // Новий ключ перемагає застарілий.
        std::fs::write(&path, br#"{"light":true,"theme":"omarchy"}"#).unwrap();
        let prefs = Preferences::load_from(&path).unwrap();
        assert_eq!(prefs.theme, theme::ThemeChoice::Omarchy);
        prefs.save_to(&path).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["theme"], "omarchy");
        assert!(saved.get("light").is_none());
    }
}

impl Workspace {
    fn mcp_preferences(
        &mut self,
        port_input: &gpui::Entity<gpui_component::input::InputState>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let port = self
            .mcp
            .server
            .as_ref()
            .map_or(self.preferences.mcp_port, |s| s.port);
        let url = format!("http://127.0.0.1:{port}/mcp");
        let token = self.mcp.token.clone();
        let claude = format!(
            "claude mcp add --transport http slicenv {url} --header \"Authorization: Bearer {token}\""
        );
        let config = serde_json::to_string_pretty(&serde_json::json!({"mcpServers":{"slicenv":{"type":"http","url":url,"headers":{"Authorization":format!("Bearer {token}")}}}})).unwrap();
        // `slicenv-mcp.exe` на Windows.
        let mcp_name = format!("slicenv-mcp{}", std::env::consts::EXE_SUFFIX);
        let binary = std::env::current_exe()
            .ok()
            .map(|p| p.with_file_name(&mcp_name))
            .filter(|p| p.is_file())
            .or_else(|| {
                std::env::var_os("PATH").and_then(|v| {
                    std::env::split_paths(&v)
                        .map(|p| p.join(&mcp_name))
                        .find(|p| p.is_file())
                })
            })
            .map_or_else(|| "slicenv-mcp".to_owned(), |p| p.display().to_string());
        let stdio = serde_json::to_string_pretty(
            &serde_json::json!({"mcpServers":{"slicenv":{"command":binary,"args":[]}}}),
        )
        .unwrap();
        let status = if let Some(err) = &self.mcp.error {
            crate::tr_format!("Помилка MCP: {}", err)
        } else if self.mcp.server.is_some() {
            crate::tr_format!("Слухає 127.0.0.1:{}", port)
        } else {
            crate::i18n::tr("Сервер вимкнено")
        };
        let mut body = div()
            .id("mcp-prefs-body")
            .flex()
            .flex_col()
            .p(px(16.))
            .gap(px(10.))
            .text_sm()
            .child(pref_title("Сервер"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .child(crate::i18n::tr("Увімкнути MCP-сервер")),
                    )
                    .child(
                        widgets::switch("mcp-enabled", self.preferences.mcp_enabled).on_click(
                            cx.listener(|ws, _, window, cx| {
                                ws.preferences.mcp_enabled = !ws.preferences.mcp_enabled;
                                ws.save_preferences(cx);
                                ws.restart_mcp(window, cx);
                            }),
                        ),
                    ),
            )
            .child(div().text_sm().child(status))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .child(crate::i18n::tr("Дозволити AI-агентам редагувати проєкт")),
                    )
                    .child(
                        widgets::switch("mcp-edit", self.preferences.mcp_allow_edit).on_click(
                            cx.listener(|ws, _, window, cx| {
                                ws.preferences.mcp_allow_edit = !ws.preferences.mcp_allow_edit;
                                ws.save_preferences(cx);
                                ws.restart_mcp(window, cx);
                            }),
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .items_center()
                    .child(crate::i18n::tr("Порт (0 — випадковий)"))
                    .child(div().w(px(90.)).child(widgets::field(
                        port_input,
                        gpui_component::input::Input::new(port_input),
                        window,
                        cx,
                    )))
                    .child({
                        let input = port_input.clone();
                        ui::ghost_button("mcp-port-apply", None, "Застосувати").on_click(
                            cx.listener(move |ws, _, window, cx| {
                                match input.read(cx).value().trim().parse::<u16>() {
                                    Ok(p) => {
                                        ws.preferences.mcp_port = p;
                                        ws.save_preferences(cx);
                                        ws.restart_mcp(window, cx);
                                    }
                                    Err(_) => ws.toast(
                                        ToastKind::Error,
                                        crate::i18n::tr("Порт має бути числом від 0 до 65535"),
                                        cx,
                                    ),
                                }
                            }),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .items_center()
                    .child(crate::i18n::tr("Токен: ••••••••••••••••"))
                    .child({
                        let token = token.clone();
                        ui::ghost_button("mcp-token-copy", None, "Копіювати токен").on_click(
                            cx.listener(move |_, _, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                    token.clone(),
                                ))
                            }),
                        )
                    })
                    .child(
                        ui::ghost_button("mcp-token-new", None, "Згенерувати новий").on_click(
                            cx.listener(|ws, _, window, cx| {
                                match mcp::http::token(
                                    &Preferences::path().with_file_name("mcp-token"),
                                    true,
                                ) {
                                    Ok(_) => ws.restart_mcp(window, cx),
                                    Err(e) => ws.toast(ToastKind::Error, e.to_string(), cx),
                                }
                            }),
                        ),
                    ),
            )
            .child(ui::divider());
        for (id, label, text) in [
            ("mcp-claude", "Claude Code", claude),
            ("mcp-json", "JSON: .mcp.json / HTTP-клієнт", config),
            ("mcp-stdio", "Stdio: окрема копія проєкту", stdio),
        ] {
            let display = if token.is_empty() {
                text.clone()
            } else {
                text.replace(&token, "TOKEN")
            };
            body = body
                .child(
                    div()
                        .flex()
                        .items_center()
                        .child(div().flex_1().text_sm().child(crate::i18n::tr(label)))
                        .child(
                            ui::ghost_button(id, None, "Копіювати").on_click(cx.listener(
                                move |_, _, _, cx| {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                        text.clone(),
                                    ))
                                },
                            )),
                        ),
                )
                .child(div().text_xs().child(display));
        }
        body=body.child(div().text_xs().child(crate::i18n::tr("HTTP працює з відкритим у вікні проєктом і спільним undo. Stdio відкриває власну копію. Для Claude Desktop без підтримки HTTP використай stdio.")))
            .child(pref_title("Інструменти"));
        for label in [
            "project_summary — пластини, об’єкти, розміри та перетворення",
            "open_project — відкрити файл (потрібен дозвіл редагування)",
            "list_profiles / get_profile — профілі слайсера",
            "explain_setting — опис параметра",
            "gcode_stats — статистика G-code",
            "slice_plate — нарізати поточний стан без друку",
            "printer_status — лише читання стану принтера",
            "transform_object — масштаб, поворот, переміщення",
            "arrange_plate / orient_plate — розкладка й орієнтація",
            "undo / redo — спільна історія змін",
            "save_project — зберегти 3MF",
        ] {
            body = body.child(div().text_xs().child(crate::i18n::tr(label)));
        }
        body.into_any_element()
    }
}
