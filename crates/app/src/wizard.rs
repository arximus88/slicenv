//! Майстер налаштування: що таке Slicenv → рушій нарізки → принтер → з’єднання → готово.
//!
//! Показується сам при першому запуску (`Preferences::wizard_done`) і з налаштувань.
//! Кожен крок можна пропустити; стан кроків живе в `Wizard`, тож «Назад» нічого не губить.
//! Будівельні блоки — наявні: менеджер пакетів (`packages`), LAN-робітник (`device::lan`),
//! `cloud::plugin` / `cloud::account`. Усе, що вантажить закритий плагін Bambu, іде в
//! дочірньому процесі цього ж застосунку (`child_main`), як `--cloud-probe`.

use std::{
    io::{Read as _, Write as _},
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use futures::StreamExt as _;
use gpui::{
    AnyElement, Context, Entity, FocusHandle, FontWeight, SharedString, Subscription, Window, div,
    prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use slicer::{Engine, EngineKind, Kind};

use crate::{
    i18n::tr,
    theme::zed,
    ui::{self, IconName, Toggle as _},
    widgets::{self, ToastKind},
    workspace::{Page, Workspace},
};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Step {
    Welcome,
    Engine,
    Printer,
    Connection,
    Done,
}

impl Step {
    const ALL: [Step; 5] = [
        Step::Welcome,
        Step::Engine,
        Step::Printer,
        Step::Connection,
        Step::Done,
    ];
    fn title(self) -> &'static str {
        match self {
            Step::Welcome => "Вітання",
            Step::Engine => "Рушій нарізки",
            Step::Printer => "Принтер",
            Step::Connection => "З’єднання",
            Step::Done => "Готово",
        }
    }
    fn icon(self) -> IconName {
        match self {
            Step::Welcome => IconName::Sparkles,
            Step::Engine => IconName::Cpu,
            Step::Printer => IconName::Printer,
            Step::Connection => IconName::Network,
            Step::Done => IconName::CircleCheck,
        }
    }
    fn next(self) -> Option<Step> {
        Self::ALL.get(self as usize + 1).copied()
    }
    fn prev(self) -> Option<Step> {
        (self as usize).checked_sub(1).map(|i| Self::ALL[i])
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Method {
    Lan,
    Cloud,
    Later,
}

/// Профіль принтера для списку вибору.
#[derive(Clone)]
struct Machine {
    name: String,
    /// Код моделі Bambu (N2S…), щоб зіставити з принтером із мережі.
    model: Option<String>,
    vendor: String,
}

struct LanForm {
    /// Серійний номер вибраного (знайденого або збереженого) принтера.
    selected: Option<String>,
    manual: bool,
    ip: Entity<InputState>,
    serial: Entity<InputState>,
    code: Entity<InputState>,
    testing: bool,
    saved: Option<cloud::DeviceId>,
    error: Option<SharedString>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CloudStage {
    Checking,
    /// Плагіна немає або версія не підтримується (текст — чому).
    NeedPlugin(Option<String>),
    /// Знайдено вхід Bambu Studio / OrcaSlicer — запропонувати його.
    Sessions,
    Login,
    Ready,
    /// Шиму до плагіна для цієї ОС немає ([`cloud::OS_SUPPORTED`]): лише LAN.
    UnsupportedOs,
}

#[derive(Clone)]
enum LoginStage {
    Password,
    EmailCode,
    Tfa(cloud::account::TfaKey),
}

/// Знайдена чужа сесія і нік власника (нік відомий лише після завантаження плагіна).
struct Session {
    found: cloud::FoundSession,
    nickname: Option<Result<String, String>>,
}

struct CloudForm {
    stage: CloudStage,
    remote: Option<Result<cloud::plugin::RemotePlugin, String>>,
    installing: bool,
    progress: Option<f32>,
    cancel: Arc<AtomicBool>,
    plugin_error: Option<SharedString>,
    sessions: Vec<Session>,
    email: Entity<InputState>,
    password: Entity<InputState>,
    code: Entity<InputState>,
    china: bool,
    login: LoginStage,
    busy: bool,
    /// Очікування входу в системному браузері: скасування і посилання (відкрити ще раз).
    browser: Option<(Arc<AtomicBool>, String)>,
    error: Option<SharedString>,
    /// Нік, під яким увійшли (або чий вхід використано).
    signed_in: Option<String>,
    /// Хмару налаштовано в цьому сеансі: робочий потік підхопить її після перезапуску.
    changed: bool,
}

pub struct Wizard {
    pub step: Step,
    focus: FocusHandle,
    engines: Vec<Engine>,
    /// Родина, яку майстер зараз встановлює через менеджер пакетів.
    install_family: Option<&'static str>,
    search: Entity<InputState>,
    machines: Option<Vec<Machine>>,
    /// Для якого сховища профілів зібрано `machines` (сховище міняється з рушієм).
    machines_for: usize,
    machine: Option<String>,
    method: Method,
    lan: LanForm,
    cloud: CloudForm,
    _subs: Vec<Subscription>,
}

/// Тестовий режим інтерфейсу (`SLICENV_WIZARD_DEMO=login|session`): без мережі й плагіна,
/// щоб показати стани форми входу. Ніяких запитів до Bambu не робить.
fn demo() -> Option<String> {
    paths::env("SLICENV_WIZARD_DEMO").map(|v| v.to_string_lossy().into_owned())
}

impl Wizard {
    fn new(window: &mut Window, cx: &mut Context<Workspace>) -> Self {
        let mut subs = Vec::new();
        let mut input = |placeholder: &str, masked: bool, cx: &mut Context<Workspace>| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(tr(placeholder))
                    .masked(masked)
            })
        };
        let search = input("Пошук моделі: A1, P1S, X1…", false, cx);
        let lan = LanForm {
            selected: None,
            manual: false,
            ip: input("IP-адреса, напр. 192.168.1.50", false, cx),
            serial: input("Серійний номер", false, cx),
            code: input("8 символів з екрана принтера", true, cx),
            testing: false,
            saved: None,
            error: None,
        };
        let cloud = CloudForm {
            stage: CloudStage::Checking,
            remote: None,
            installing: false,
            progress: None,
            cancel: Default::default(),
            plugin_error: None,
            sessions: Vec::new(),
            email: input("Email акаунта Bambu Lab", false, cx),
            password: input("Пароль", true, cx),
            code: input("Код підтвердження", false, cx),
            china: false,
            login: LoginStage::Password,
            busy: false,
            browser: None,
            error: None,
            signed_in: None,
            changed: false,
        };
        subs.push(cx.subscribe(&search, |_, _, e: &InputEvent, cx| {
            if matches!(e, InputEvent::Change) {
                cx.notify();
            }
        }));
        // Enter у полі — дія цього поля, а не «Далі».
        for (entity, action) in [
            (&lan.code, FieldAction::TestLan),
            (&lan.ip, FieldAction::TestLan),
            (&lan.serial, FieldAction::TestLan),
            (&cloud.email, FieldAction::Login),
            (&cloud.password, FieldAction::Login),
            (&cloud.code, FieldAction::Login),
        ] {
            subs.push(cx.subscribe_in(
                entity,
                window,
                move |ws, _, e: &InputEvent, window, cx| {
                    if matches!(e, InputEvent::PressEnter { .. }) {
                        match action {
                            FieldAction::TestLan => ws.wizard_test_lan(cx),
                            FieldAction::Login => ws.wizard_login(window, cx),
                        }
                    }
                },
            ));
        }
        Self {
            step: Step::Welcome,
            focus: cx.focus_handle(),
            engines: Vec::new(),
            install_family: None,
            search,
            machines: None,
            machines_for: 0,
            machine: None,
            method: Method::Lan,
            lan,
            cloud,
            _subs: subs,
        }
    }

    /// Відповідь LAN-робітника на «Перевірити» (`LanCmd::Add`).
    pub fn on_lan_added(&mut self, dev: Option<&cloud::DeviceId>) {
        if !self.lan.testing {
            return;
        }
        self.lan.testing = false;
        if let Some(dev) = dev {
            self.lan.saved = Some(dev.clone());
            self.lan.selected = Some(dev.as_str().to_owned());
            self.lan.manual = false;
            self.lan.error = None;
        }
    }
}

#[derive(Clone, Copy)]
enum FieldAction {
    TestLan,
    Login,
}

/// «OrcaSlicer-2.4.2» → «2.4.2»: назву рушія вже показано поруч.
fn version_text(version: &str) -> String {
    let v = version.trim();
    ["OrcaSlicer", "BambuStudio", "Bambu Studio", "orca", "bambu"]
        .iter()
        .find_map(|p| v.strip_prefix(p))
        .map(|rest| rest.trim_start_matches(['-', ' ', '_', 'v', 'V']))
        .filter(|rest| !rest.is_empty())
        .unwrap_or(v)
        .to_owned()
}

fn engine_title(kind: EngineKind) -> &'static str {
    match kind {
        EngineKind::Bambu => "Bambu Studio",
        EngineKind::Orca => "OrcaSlicer",
    }
}

impl Workspace {
    pub fn open_wizard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.wizard.is_none() {
            self.wizard = Some(Wizard::new(window, cx));
        }
        self.close_modal(cx);
        if let Some(w) = &self.wizard {
            window.focus(&w.focus, cx);
        }
        self.wizard_enter(window, cx);
        cx.notify();
    }

    /// Esc і «Пропустити»: майстер більше не з’являється сам.
    pub fn close_wizard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let finished = self.wizard.as_ref().is_some_and(|w| w.step == Step::Done);
        self.finish_wizard(window, cx);
        if !finished {
            self.toast(
                ToastKind::Info,
                tr("Майстер можна відкрити знову: Налаштування редактора → «Майстер налаштування»"),
                cx,
            );
        }
    }

    fn finish_wizard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(w) = self.wizard.take() {
            w.cloud.cancel.store(true, Ordering::Release);
        }
        if !self.preferences.wizard_done {
            self.preferences.wizard_done = true;
            self.save_preferences(cx);
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub fn wizard_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(step) = self.wizard.as_ref().map(|w| w.step) else {
            return;
        };
        if step == Step::Printer {
            self.apply_wizard_machine(cx);
        }
        match step.next() {
            Some(next) => self.wizard_go(next, window, cx),
            None => self.finish_wizard(window, cx),
        }
    }

    pub fn wizard_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(prev) = self.wizard.as_ref().and_then(|w| w.step.prev()) {
            self.wizard_go(prev, window, cx);
        }
    }

    fn wizard_go(&mut self, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        let Some(w) = &mut self.wizard else { return };
        w.step = step;
        // Поле з фокусом могло лишитися на попередньому кроці: Enter знову має означати «Далі».
        window.focus(&w.focus, cx);
        self.wizard_enter(window, cx);
        cx.notify();
    }

    /// Перечитує список рушіїв у майстрі (після вибору файлу чи встановлення).
    pub(crate) fn refresh_engine_lists(&mut self) {
        if let Some(w) = &mut self.wizard {
            w.engines = crate::engines::installed();
        }
    }

    /// Підготовка даних кроку при вході (дешево; довге — у фоні).
    fn wizard_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(step) = self.wizard.as_ref().map(|w| w.step) else {
            return;
        };
        match step {
            Step::Engine => {
                let engines = crate::engines::installed();
                if let Some(w) = &mut self.wizard {
                    w.engines = engines;
                }
            }
            Step::Printer => self.wizard_load_machines(window, cx),
            Step::Connection => {
                if self.wizard.as_ref().is_some_and(|w| {
                    w.method == Method::Cloud && w.cloud.stage == CloudStage::Checking
                }) {
                    self.wizard_check_cloud(window, cx);
                }
                self.wizard_preselect_lan();
            }
            _ => {}
        }
    }

    // ---- рушій ----

    fn wizard_install(&mut self, family: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if self.packages.busy || self.engine_loading {
            return;
        }
        if let Some(w) = &mut self.wizard {
            w.install_family = Some(family);
        }
        self.packages.installed_executable = None;
        self.packages.error = None;
        let release = crate::packages::newest_stable(&self.packages.releases, family).cloned();
        match release {
            Some(release) if self.packages.channel == "stable" => {
                let Some(asset) = crate::packages::recommended_asset(&release).cloned() else {
                    self.packages.error = Some(tr("Для цієї версії немає перевіреної збірки для цієї системи"));
                    return;
                };
                self.install_asset(&release, &asset, window, cx);
            }
            // Списку стабільних випусків ще немає: спершу отримати, потім ставити.
            _ => {
                self.packages.pending_family = Some(family);
                self.refresh_packages("stable", window, cx);
            }
        }
        cx.notify();
    }

    /// Кожна завершена операція менеджера пакетів: продовжити установку з майстра.
    pub fn wizard_packages_done(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(family) = self.packages.pending_family.take() {
            if self.packages.error.is_none() {
                self.wizard_install(family, window, cx);
            }
            return;
        }
        let Some(w) = &mut self.wizard else { return };
        let Some(family) = w.install_family else {
            return;
        };
        if let Some(path) = self.packages.installed_executable.take() {
            w.install_family = None;
            w.engines = crate::engines::installed();
            let kind = if family == "orca" {
                EngineKind::Orca
            } else {
                EngineKind::Bambu
            };
            self.activate_engine(Ok(Engine { kind, path }), window, cx);
        } else if self.packages.error.is_some() {
            w.install_family = None;
        }
    }

    // ---- принтер ----

    fn wizard_load_machines(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(store) = self.presets.clone() else {
            return;
        };
        let key = Arc::as_ptr(&store) as usize;
        let Some(w) = &mut self.wizard else { return };
        if w.machines.is_some() && w.machines_for == key {
            return;
        }
        w.machines = None;
        w.machines_for = key;
        cx.spawn_in(window, async move |this, cx| {
            let list = cx
                .background_executor()
                .spawn(async move {
                    let mut list: Vec<Machine> = store
                        .compatible(Kind::Machine, "")
                        .into_iter()
                        .map(|p| Machine {
                            model: store.machine_model_id(&p.name),
                            vendor: store.vendor_for_machine(&p.name).unwrap_or_default(),
                            name: p.name.clone(),
                        })
                        .collect();
                    // Bambu Lab — угорі: майстер насамперед для них.
                    list.sort_by(|a, b| {
                        (a.vendor != "BBL", &a.vendor, &a.name).cmp(&(b.vendor != "BBL", &b.vendor, &b.name))
                    });
                    list
                })
                .await;
            this.update_in(cx, |this, _, cx| {
                let current = this.selection.machine.clone();
                let saved = this.preferences.printer.clone();
                if let Some(w) = &mut this.wizard
                    && w.machines_for == key
                {
                    if w.machine.is_none() && !saved.is_empty() && list.iter().any(|m| m.name == saved) {
                        w.machine = Some(current);
                    }
                    w.machines = Some(list);
                }
                this.wizard_pick_from_network(false);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Профіль для принтера з мережі: та сама модель, сопло 0.4, системний профіль Bambu.
    fn machine_for_model(&self, model: &str) -> Option<String> {
        let machines = self.wizard.as_ref()?.machines.as_ref()?;
        let mut candidates: Vec<&Machine> = machines
            .iter()
            .filter(|m| m.model.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(model)))
            .collect();
        candidates.sort_by_key(|m| (!m.name.contains("0.4 nozzle"), m.vendor != "BBL", m.name.len()));
        candidates.first().map(|m| m.name.clone())
    }

    /// Автовибір профілю за першим принтером у мережі (`force` — явний клік користувача).
    fn wizard_pick_from_network(&mut self, force: bool) {
        let Some(w) = &self.wizard else { return };
        if w.machine.is_some() && !force {
            return;
        }
        let model = self
            .cloud
            .lan
            .printers
            .iter()
            .map(|p| p.model.clone())
            .chain(self.cloud.lan.found.iter().map(|f| f.model.clone()))
            .find(|m| !m.is_empty());
        if let Some(name) = model.and_then(|m| self.machine_for_model(&m))
            && let Some(w) = &mut self.wizard
        {
            w.machine = Some(name);
        }
    }

    fn apply_wizard_machine(&mut self, cx: &mut Context<Self>) {
        let Some(name) = self.wizard.as_ref().and_then(|w| w.machine.clone()) else {
            return;
        };
        let Some(store) = self.presets.clone() else { return };
        if name != self.selection.machine {
            let selection = store.defaults_for_machine(&name).unwrap_or_else(|| slicer::Selection {
                machine: name.clone(),
                ..self.selection.clone()
            });
            self.set_selection(selection, cx);
        }
        if self.preferences.printer != name {
            self.preferences.printer = name;
            self.save_preferences(cx);
        }
    }

    // ---- LAN ----

    fn wizard_preselect_lan(&mut self) {
        let Some(w) = &mut self.wizard else { return };
        if w.lan.selected.is_some() || w.lan.manual {
            return;
        }
        w.lan.selected = self
            .cloud
            .lan
            .printers
            .first()
            .map(|p| p.dev_id.as_str().to_owned())
            .or_else(|| self.cloud.lan.found.first().map(|f| f.serial.clone()));
    }

    fn wizard_test_lan(&mut self, cx: &mut Context<Self>) {
        let Some(w) = &mut self.wizard else { return };
        if w.lan.testing || self.cloud.lan.adding {
            return;
        }
        let value = |e: &Entity<InputState>| e.read(cx).value().to_string();
        let code = value(&w.lan.code);
        let found = w
            .lan
            .selected
            .as_ref()
            .filter(|_| !w.lan.manual)
            .and_then(|s| self.cloud.lan.found.iter().find(|f| &f.serial == s));
        let (serial, ip, model, name) = match found {
            Some(f) => (f.serial.clone(), f.ip.clone(), f.model.clone(), f.name.clone()),
            None => (value(&w.lan.serial), value(&w.lan.ip), String::new(), String::new()),
        };
        if code.trim().is_empty() {
            w.lan.error = Some(tr("Введіть код доступу з екрана принтера").into());
            cx.notify();
            return;
        }
        match lan::LanPrinter::new(&serial, &ip, &code) {
            Ok(mut printer) => {
                printer.name = name;
                // Для ручного вводу модель — з профілю, вибраного на кроці «Принтер».
                printer.model = if model.is_empty() {
                    self.presets
                        .as_ref()
                        .and_then(|s| s.machine_model_id(&self.selection.machine))
                        .unwrap_or_default()
                } else {
                    model
                };
                w.lan.testing = true;
                w.lan.error = None;
                w.lan.saved = None;
                self.cloud.lan.adding = true;
                self.cloud.lan.add_error = None;
                self.cloud.lan.send(crate::device::LanCmd::Add(Box::new(printer)));
            }
            Err(error) => w.lan.error = Some(crate::device::lan_error_text(&error).into()),
        }
        cx.notify();
    }

    // ---- хмара ----

    fn wizard_check_cloud(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(w) = &mut self.wizard else { return };
        w.cloud.stage = CloudStage::Checking;
        w.cloud.error = None;
        cx.spawn_in(window, async move |this, cx| {
            let (setup, sessions) = cx
                .background_executor()
                .spawn(async move {
                    if let Some(mode) = demo() {
                        let stage = if mode == "session" {
                            CloudStage::Sessions
                        } else if mode == "plugin" {
                            CloudStage::NeedPlugin(None)
                        } else {
                            CloudStage::Login
                        };
                        return (stage, Vec::new());
                    }
                    if !cloud::OS_SUPPORTED {
                        return (CloudStage::UnsupportedOs, Vec::new());
                    }
                    let foreign: Vec<_> = cloud::account::find_sessions()
                        .into_iter()
                        .filter(|s| !s.is_own())
                        .collect();
                    let stage = match cloud::setup() {
                        cloud::CloudSetup::NoPlugin => CloudStage::NeedPlugin(None),
                        cloud::CloudSetup::UnsupportedPlugin { version, .. } => {
                            CloudStage::NeedPlugin(Some(version))
                        }
                        cloud::CloudSetup::NoSession { .. } => CloudStage::Login,
                        cloud::CloudSetup::Ready { session, .. } if session.is_own() => {
                            CloudStage::Ready
                        }
                        cloud::CloudSetup::Ready { .. } => CloudStage::Sessions,
                    };
                    (stage, foreign)
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                let Some(w) = &mut this.wizard else { return };
                let demo_session = matches!(setup, CloudStage::Sessions) && sessions.is_empty();
                w.cloud.sessions = sessions
                    .into_iter()
                    .map(|found| Session {
                        found,
                        nickname: None,
                    })
                    .collect();
                if demo_session {
                    w.cloud.sessions.push(Session {
                        found: cloud::FoundSession {
                            origin: cloud::SessionOrigin::BambuStudio,
                            dir: Default::default(),
                            modified: None,
                            country_code: "US".into(),
                            adopted_from: None,
                        },
                        nickname: Some(Ok("maker_demo".into())),
                    });
                }
                w.cloud.stage = setup.clone();
                match setup {
                    CloudStage::NeedPlugin(_) => this.wizard_fetch_plugin(window, cx),
                    CloudStage::Sessions if !demo_session => this.wizard_nicknames(window, cx),
                    CloudStage::Ready => this.wizard_own_nickname(window, cx),
                    _ => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn wizard_fetch_plugin(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(w) = &mut self.wizard else { return };
        if w.cloud.remote.as_ref().is_some_and(|r| r.is_ok()) {
            return;
        }
        w.cloud.remote = None;
        let country = if w.cloud.china { "CN" } else { "US" };
        if demo().is_some() {
            w.cloud.remote = Some(Err(tr("Тестовий режим: сервер Bambu не запитується")));
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let remote = cx
                .background_executor()
                .spawn(async move {
                    cloud::plugin::fetch_remote(&cloud::http::Curl, country, cloud::LATEST_SERIES)
                        .map_err(|e| format!("{e:#}"))
                })
                .await;
            this.update_in(cx, |this, _, cx| {
                if let Some(w) = &mut this.wizard {
                    w.cloud.remote = Some(remote);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// «Погоджуюсь — завантажити»: згода записується лише тут, у відповідь на клік.
    fn wizard_install_plugin(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(w) = &mut self.wizard else { return };
        let Some(Ok(remote)) = w.cloud.remote.clone() else {
            return;
        };
        if w.cloud.installing {
            return;
        }
        w.cloud.installing = true;
        w.cloud.progress = Some(0.0);
        w.cloud.plugin_error = None;
        w.cloud.cancel = Default::default();
        let cancel = w.cloud.cancel.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Option<f32>>();
        let worker = std::thread::spawn(move || -> Result<(), String> {
            let consent = cloud::plugin::Consent::accept().map_err(|e| format!("{e:#}"))?;
            let mut last = Instant::now() - Duration::from_secs(1);
            cloud::plugin::install(
                &cloud::http::Curl,
                &consent,
                &remote,
                &mut |p| {
                    if last.elapsed() > Duration::from_millis(150) {
                        last = Instant::now();
                        tx.unbounded_send(p.fraction()).ok();
                    }
                },
                &cancel,
            )
            .map(|_| ())
            .map_err(|e| format!("{e:#}"))
        });
        cx.spawn_in(window, async move |this, cx| {
            while let Some(fraction) = rx.next().await {
                let alive = this
                    .update_in(cx, |this, _, cx| {
                        if let Some(w) = &mut this.wizard {
                            w.cloud.progress = fraction;
                        }
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
            let result = cx
                .background_executor()
                .spawn(async move { worker.join().unwrap_or_else(|_| Err("panic".into())) })
                .await;
            this.update_in(cx, |this, window, cx| {
                let Some(w) = &mut this.wizard else { return };
                w.cloud.installing = false;
                w.cloud.progress = None;
                match result {
                    Ok(()) => {
                        w.cloud.changed = true;
                        this.wizard_check_cloud(window, cx);
                    }
                    Err(e) if w.cloud.cancel.load(Ordering::Acquire) => {
                        let _ = e;
                        w.cloud.plugin_error = Some(tr("Завантаження скасовано").into());
                    }
                    Err(e) => w.cloud.plugin_error = Some(e.into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Ніки знайдених сесій — по одному дочірньому процесу на сесію.
    fn wizard_nicknames(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(w) = &self.wizard else { return };
        let origins: Vec<_> = w.cloud.sessions.iter().map(|s| s.found.origin).collect();
        for origin in origins {
            cx.spawn_in(window, async move |this, cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move { run_child(&[WHOAMI_ARG, origin_key(origin)], None) })
                    .await;
                this.update_in(cx, |this, window, cx| {
                    let mut need_plugin = false;
                    if let Some(w) = &mut this.wizard {
                        // Вхід є, але знайдено лише застарий плагін (OrcaSlicer 2.3 — 01.10):
                        // спершу підтримуваний плагін, потім цей самий вхід ще раз.
                        if let Err(ChildFailure { unsupported_plugin: Some(version), .. }) = &result
                            && w.cloud.stage == CloudStage::Sessions
                        {
                            w.cloud.stage = CloudStage::NeedPlugin(Some(version.clone()));
                            need_plugin = true;
                        }
                        if let Some(s) = w.cloud.sessions.iter_mut().find(|s| s.found.origin == origin) {
                            s.nickname = Some(result.map_err(|e| e.text));
                        }
                    }
                    if need_plugin {
                        this.wizard_fetch_plugin(window, cx);
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    fn wizard_own_nickname(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { run_child(&[WHOAMI_ARG, "own"], None) })
                .await;
            this.update_in(cx, |this, _, cx| {
                if let Some(w) = &mut this.wizard {
                    match result {
                        Ok(nick) => w.cloud.signed_in = Some(nick),
                        // Власна сесія прострочена — просимо увійти знову.
                        Err(e) => {
                            w.cloud.stage = CloudStage::Login;
                            w.cloud.error = Some(e.text.into());
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn wizard_adopt(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(w) = &mut self.wizard else { return };
        let Some(session) = w.cloud.sessions.get(index) else {
            return;
        };
        let nickname = session.nickname.clone().and_then(Result::ok);
        let result = if demo().is_some() {
            Ok(())
        } else {
            cloud::account::adopt_session(&session.found).map(|_| ())
        };
        match result {
            Ok(()) => {
                w.cloud.signed_in = Some(nickname.unwrap_or_default());
                w.cloud.stage = CloudStage::Ready;
                w.cloud.changed = true;
                // Робочий потік хмари підхоплює нову сесію одразу — без перезапуску.
                self.cloud.send(crate::device::CloudCmd::Reconnect);
            }
            Err(e) => w.cloud.error = Some(format!("{e:#}").into()),
        }
        cx.notify();
    }

    fn wizard_login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(w) = &mut self.wizard else { return };
        if w.cloud.busy || w.cloud.stage != CloudStage::Login {
            return;
        }
        let value = |e: &Entity<InputState>| e.read(cx).value().trim().to_string();
        let email = value(&w.cloud.email);
        let password = w.cloud.password.read(cx).value().to_string();
        let code = value(&w.cloud.code);
        let stage = w.cloud.login.clone();
        let missing = match stage {
            LoginStage::Password if email.is_empty() || password.is_empty() => {
                Some("Введіть email і пароль")
            }
            LoginStage::EmailCode | LoginStage::Tfa(_) if code.is_empty() => {
                Some("Введіть код підтвердження")
            }
            _ => None,
        };
        if let Some(text) = missing {
            w.cloud.error = Some(tr(text).into());
            cx.notify();
            return;
        }
        w.cloud.busy = true;
        w.cloud.error = None;
        let country = if w.cloud.china { "CN" } else { "US" };
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { login_step(stage, &email, &password, &code, country) })
                .await;
            this.update_in(cx, |this, window, cx| {
                let Some(w) = &mut this.wizard else { return };
                w.cloud.busy = false;
                match result {
                    LoginOutcome::Next(next) => {
                        w.cloud.login = next;
                        w.cloud.code.update(cx, |s, cx| s.set_value("", window, cx));
                    }
                    LoginOutcome::Done(nickname) => {
                        w.cloud.signed_in = Some(nickname);
                        w.cloud.stage = CloudStage::Ready;
                        w.cloud.changed = true;
                        this.cloud.send(crate::device::CloudCmd::Reconnect);
                        // Пароль у пам’яті більше не потрібен.
                        w.cloud.password.update(cx, |s, cx| s.set_value("", window, cx));
                    }
                    LoginOutcome::Failed(text) => w.cloud.error = Some(text.into()),
                    LoginOutcome::Cancelled => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Вхід через системний браузер (Google, Apple, email): офіційна сторінка Bambu повертає
    /// квиток на локальний порт ([`cloud::browser_login`]), далі — як після входу паролем.
    fn wizard_browser_login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(w) = &mut self.wizard else { return };
        if w.cloud.busy {
            return;
        }
        let country = if w.cloud.china { "CN" } else { "US" };
        let server = match cloud::browser_login::Loopback::bind() {
            Ok(server) => server,
            Err(e) => {
                w.cloud.error = Some(tr(&format!("{e:#}")).into());
                cx.notify();
                return;
            }
        };
        let url = cloud::browser_login::sign_in_url(country, server.port(), "en");
        let cancel = Arc::new(AtomicBool::new(false));
        w.cloud.busy = true;
        w.cloud.error = None;
        w.cloud.browser = Some((cancel.clone(), url.clone()));
        cx.open_url(&url);
        let title = tr("Вхід виконано");
        let message = tr("Можна закрити цю вкладку й повернутися до Slicenv.");
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { browser_login(&server, &cancel, country, &title, &message) })
                .await;
            this.update_in(cx, |this, _, cx| {
                let Some(w) = &mut this.wizard else { return };
                w.cloud.busy = false;
                w.cloud.browser = None;
                match result {
                    LoginOutcome::Done(nickname) => {
                        w.cloud.signed_in = Some(nickname);
                        w.cloud.stage = CloudStage::Ready;
                        w.cloud.changed = true;
                        this.cloud.send(crate::device::CloudCmd::Reconnect);
                    }
                    LoginOutcome::Failed(text) => w.cloud.error = Some(text.into()),
                    LoginOutcome::Next(_) | LoginOutcome::Cancelled => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn wizard_browser_cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(w) = &self.wizard
            && let Some((cancel, _)) = &w.cloud.browser
        {
            cancel.store(true, Ordering::Release);
        }
        cx.notify();
    }

    fn wizard_resend_code(&mut self, cx: &mut Context<Self>) {
        let Some(w) = &mut self.wizard else { return };
        let email = w.cloud.email.read(cx).value().trim().to_string();
        let country = if w.cloud.china { "CN" } else { "US" };
        let result = if demo().is_some() {
            Ok(())
        } else {
            cloud::account::Login::new(&cloud::http::Curl, country).send_email_code(&email)
        };
        match result {
            Ok(()) => self.toast(ToastKind::Info, tr("Новий код надіслано на пошту"), cx),
            Err(e) => {
                if let Some(w) = &mut self.wizard {
                    w.cloud.error = Some(tr(&e.to_string()).into());
                }
            }
        }
        cx.notify();
    }
}

// ---- дочірні процеси для плагіна ----

pub const WHOAMI_ARG: &str = "--cloud-whoami";
pub const LOGIN_ARG: &str = "--cloud-login";
/// Рядок відповіді дочірнього процесу (плагін може писати у stdout і сам).
const NICK_PREFIX: &str = "SLICENV_NICK:";
/// Версія плагіна, яку шим не підтримує (код виходу 4).
const PLUGIN_PREFIX: &str = "SLICENV_PLUGIN:";

fn origin_key(origin: cloud::SessionOrigin) -> &'static str {
    match origin {
        cloud::SessionOrigin::Own => "own",
        cloud::SessionOrigin::BambuStudio => "studio",
        cloud::SessionOrigin::OrcaSlicer => "orca",
    }
}

/// `slicenv --cloud-whoami <own|studio|orca>` / `slicenv --cloud-login` (токени через stdin).
pub fn child_main(arg: Option<&Path>) -> Option<i32> {
    let arg = arg?.to_str()?;
    if arg != WHOAMI_ARG && arg != LOGIN_ARG {
        return None;
    }
    unsafe { cloud::configure_tls_env() };
    let result = if arg == WHOAMI_ARG {
        whoami_child(&std::env::args().nth(2).unwrap_or_default())
    } else {
        login_child()
    };
    Some(match result {
        Ok(nick) => {
            println!("{NICK_PREFIX}{}", nick.trim());
            0
        }
        // Текст без секретів; батько показує власну фразу за кодом виходу.
        Err(ChildError::Expired) => 3,
        Err(ChildError::UnsupportedPlugin(version)) => {
            println!("{PLUGIN_PREFIX}{version}");
            4
        }
        Err(ChildError::NoPlugin) => 5,
        Err(ChildError::Other) => 2,
    })
}

enum ChildError {
    /// Плагін завантажився, але сесія не відновилася (прострочена або записана іншою
    /// версією плагіна).
    Expired,
    /// Знайдено лише плагін, який шим не підтримує (наприклад 01.10 від OrcaSlicer 2.3).
    UnsupportedPlugin(String),
    NoPlugin,
    Other,
}

/// Плагін для дочірнього процесу. Версію плагіна Bambu Studio/Orca без номера в імені видно
/// лише після завантаження: тут (в окремому процесі) це безпечно, тож непідтримувану версію
/// повідомляємо точно, а не як «Bambu не прийняв вхід».
fn ready_plugin() -> Result<cloud::InstalledPlugin, ChildError> {
    let plugin = match cloud::plugin::plugin_state() {
        cloud::PluginState::Ready(p) => p,
        cloud::PluginState::Unsupported(list) => {
            let version = list.iter().find_map(|p| p.version.clone()).unwrap_or_default();
            return Err(ChildError::UnsupportedPlugin(version));
        }
        cloud::PluginState::Missing => return Err(ChildError::NoPlugin),
    };
    if plugin.version.is_none() {
        let version = cloud::read_plugin_version(&plugin.library).map_err(|_| ChildError::Other)?;
        if cloud::generation(&version).is_none() {
            return Err(ChildError::UnsupportedPlugin(version));
        }
    }
    Ok(plugin)
}

fn wait_login(client: &cloud::Client) -> bool {
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if client.is_user_login().unwrap_or(false) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

fn whoami_child(origin: &str) -> Result<String, ChildError> {
    let plugin = ready_plugin()?;
    let session = cloud::account::find_sessions()
        .into_iter()
        .find(|s| origin_key(s.origin) == origin)
        .ok_or(ChildError::Other)?;
    let (client, _events) =
        cloud::Client::load(cloud::Options::for_session(&plugin, &session)).map_err(|_| ChildError::Other)?;
    client.start().map_err(|_| ChildError::Other)?;
    if !wait_login(&client) {
        let _ = client.shutdown();
        return Err(ChildError::Expired);
    }
    let nick = client.user_nickname().unwrap_or_default();
    let _ = client.shutdown();
    Ok(nick)
}

fn login_child() -> Result<String, ChildError> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|_| ChildError::Other)?;
    let v: serde_json::Value = serde_json::from_str(&input).map_err(|_| ChildError::Other)?;
    let tokens = cloud::Tokens {
        access_token: v["access_token"].as_str().unwrap_or_default().into(),
        refresh_token: v["refresh_token"].as_str().unwrap_or_default().into(),
        expires_in: v["expires_in"].as_f64().unwrap_or(0.0),
        refresh_expires_in: v["refresh_expires_in"].as_f64().unwrap_or(0.0),
    };
    let country = v["country"].as_str().unwrap_or("US");
    let plugin = ready_plugin()?;
    let options = cloud::Options::for_login(&plugin, country).map_err(|_| ChildError::Other)?;
    let (client, _events) = cloud::Client::load(options).map_err(|_| ChildError::Other)?;
    client.start().map_err(|_| ChildError::Other)?;
    let result = client.login_with_tokens(&tokens);
    let nick = client.user_nickname().unwrap_or_default();
    let _ = client.shutdown();
    result.map_err(|_| ChildError::Other)?;
    Ok(nick)
}

/// Помилка дочірнього процесу: готовий текст для людини і, якщо причина — застарий плагін,
/// його версія (майстер тоді пропонує завантажити підтримуваний).
#[derive(Clone, Debug)]
struct ChildFailure {
    text: String,
    unsupported_plugin: Option<String>,
}

impl From<String> for ChildFailure {
    fn from(text: String) -> Self {
        Self {
            text,
            unsupported_plugin: None,
        }
    }
}

/// Запускає дочірній процес і чекає не довше хвилини.
fn run_child(args: &[&str], stdin: Option<String>) -> Result<String, ChildFailure> {
    let exe = std::env::current_exe().map_err(|e| ChildFailure::from(e.to_string()))?;
    // Власний бінарник, поки батько живий: оточення AppImage (rpath, APPDIR) йому потрібне,
    // тож тут свідомо не paths::host::command.
    let mut child = Command::new(exe)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| ChildFailure::from(e.to_string()))?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(text.as_bytes());
    }
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        if let Some(s) = &mut stdout {
            let _ = s.read_to_string(&mut out);
        }
        out
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(tr("Плагін Bambu не відповів вчасно").into());
            }
        }
    };
    let out = reader.join().unwrap_or_default();
    if let Some(signal) = paths::host::exit_signal(&status) {
        return Err(crate::tr_format!(
            "Плагін Bambu аварійно завершився (сигнал {signal}) — застосунок не постраждав"
        )
        .into());
    }
    match status.code() {
        Some(0) => Ok(out
            .lines()
            .find_map(|l| l.strip_prefix(NICK_PREFIX))
            .unwrap_or_default()
            .to_owned()),
        Some(3) => Err(tr(
            "Сесія не відновилася: вона прострочена або записана іншою версією плагіна — увійдіть знову",
        )
        .into()),
        Some(4) => {
            let version = out
                .lines()
                .find_map(|l| l.strip_prefix(PLUGIN_PREFIX))
                .unwrap_or_default()
                .trim()
                .to_owned();
            Err(ChildFailure {
                text: crate::tr_format!(
                    "Знайдено лише плагін Bambu {version}, який Slicenv не підтримує — завантажте підтримуваний, щоб використати цей вхід"
                ),
                unsupported_plugin: Some(version),
            })
        }
        Some(5) => Err(tr("Плагін Bambu не встановлено").into()),
        _ => Err(tr("Плагін Bambu не зміг використати цей вхід — увійдіть знову").into()),
    }
}

enum LoginOutcome {
    Next(LoginStage),
    Done(String),
    Failed(String),
    Cancelled,
}

/// Тестовий транспорт (`SLICENV_WIZARD_DEMO`): пароль «wrong» — помилка, код 123456 — успіх.
struct DemoTransport;
impl cloud::http::Transport for DemoTransport {
    fn request(&self, r: &cloud::http::Request<'_>) -> anyhow::Result<cloud::http::Response> {
        std::thread::sleep(Duration::from_millis(600));
        let body = r.body.unwrap_or_default();
        let (status, reply) = if r.url.ends_with("sendemail/code") {
            (200, "{}")
        } else if body.contains("\"password\":\"wrong\"") {
            (400, r#"{"code":1,"error":"Incorrect account or password."}"#)
        } else if body.contains("\"password\"") {
            (200, r#"{"loginType":"verifyCode"}"#)
        } else if body.contains("\"code\":\"123456\"") {
            (200, r#"{"accessToken":"demo"}"#)
        } else {
            (400, r#"{"code":2}"#)
        };
        Ok(cloud::http::Response {
            status,
            headers: Vec::new(),
            body: reply.as_bytes().to_vec(),
        })
    }
    fn download(
        &self,
        _: &str,
        _: &[(String, String)],
        _: u64,
        _: &mut dyn std::io::Write,
        _: &mut dyn FnMut(u64),
        _: &AtomicBool,
    ) -> anyhow::Result<u64> {
        anyhow::bail!("demo")
    }
}

fn login_step(stage: LoginStage, email: &str, password: &str, code: &str, country: &str) -> LoginOutcome {
    let demo = demo().is_some();
    let transport: &dyn cloud::http::Transport = if demo { &DemoTransport } else { &cloud::http::Curl };
    let login = cloud::account::Login::new(transport, country);
    let tokens = match stage {
        LoginStage::Password => match login.password(email, password) {
            Ok(cloud::LoginStep::Done(tokens)) => tokens,
            Ok(cloud::LoginStep::EmailCode) => return LoginOutcome::Next(LoginStage::EmailCode),
            Ok(cloud::LoginStep::Tfa(key)) => return LoginOutcome::Next(LoginStage::Tfa(key)),
            Err(e) => return LoginOutcome::Failed(tr(&e.to_string())),
        },
        LoginStage::EmailCode => match login.email_code(email, code) {
            Ok(tokens) => tokens,
            Err(e) => return LoginOutcome::Failed(tr(&e.to_string())),
        },
        LoginStage::Tfa(key) => match login.tfa_code(&key, code) {
            Ok(tokens) => tokens,
            Err(e) => return LoginOutcome::Failed(tr(&e.to_string())),
        },
    };
    if demo {
        return LoginOutcome::Done("maker_demo".into());
    }
    finish_login(&tokens, country)
}

/// Квиток з браузера → токени (відкритий REST Bambu) → вхід плагіна в дочірньому процесі.
fn browser_login(
    server: &cloud::browser_login::Loopback,
    cancel: &AtomicBool,
    country: &str,
    title: &str,
    message: &str,
) -> LoginOutcome {
    use cloud::browser_login::{Page, WaitError};
    // 10 хвилин: вхід через Google з 2FA або реєстрація займають час.
    let ticket = match server.wait_ticket(Duration::from_secs(600), cancel, &Page { title, message }) {
        Ok(ticket) => ticket,
        Err(WaitError::Cancelled) => return LoginOutcome::Cancelled,
        Err(e) => return LoginOutcome::Failed(tr(&e.to_string())),
    };
    let tokens = match cloud::account::Login::new(&cloud::http::Curl, country).ticket(&ticket) {
        Ok(tokens) => tokens,
        Err(e) => return LoginOutcome::Failed(tr(&e.to_string())),
    };
    finish_login(&tokens, country)
}

/// Токени → `--cloud-login` (плагін записує сесію у власну теку Slicenv). Токени йдуть
/// лише через stdin дочірнього процесу.
fn finish_login(tokens: &cloud::Tokens, country: &str) -> LoginOutcome {
    let payload = serde_json::json!({
        "country": country,
        "access_token": tokens.access_token,
        "refresh_token": tokens.refresh_token,
        "expires_in": tokens.expires_in,
        "refresh_expires_in": tokens.refresh_expires_in,
    })
    .to_string();
    match run_child(&[LOGIN_ARG], Some(payload)) {
        Ok(nick) => LoginOutcome::Done(nick),
        Err(e) => LoginOutcome::Failed(e.text),
    }
}

// ---- вигляд ----

const CARD_WIDTH: f32 = 920.;
const RAIL_WIDTH: f32 = 210.;

pub fn render(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let z = zed();
    let Some(w) = &ws.wizard else {
        return div().into_any_element();
    };
    let step = w.step;
    let focus = w.focus.clone();
    // Поле з фокусом могло зникнути (успішний вхід, збережений принтер): тоді Enter і Esc
    // нікуди не йдуть. Повертаємо фокус майстру, якщо його не тримає ні він, ні модалка.
    if ws.modal.is_none() && !focus.contains_focused(window, cx) {
        let focus = focus.clone();
        window.defer(cx, move |window, cx| window.focus(&focus, cx));
    }
    let viewport = window.viewport_size();
    let height = (f32::from(viewport.height) - 96.).clamp(460., 680.);
    let width = (f32::from(viewport.width) - 64.).min(CARD_WIDTH);

    let body = match step {
        Step::Welcome => welcome(),
        Step::Engine => engine_step(ws, cx),
        Step::Printer => printer_step(ws, window, cx),
        Step::Connection => connection_step(ws, window, cx),
        Step::Done => done_step(ws, cx),
    };
    let (title, subtitle) = step_heading(step);

    div()
        .id("wizard-layer")
        .absolute()
        .inset_0()
        .occlude()
        .flex()
        .items_center()
        .justify_center()
        .bg(z.background.opacity(0.94))
        .track_focus(&focus)
        .key_context("Wizard")
        .child(
            div()
                .id("wizard")
                .flex()
                .w(px(width))
                .when(step == Step::Welcome, |d| d.max_h(px(height)))
                .when(step != Step::Welcome, |d| d.h(px(height)))
                .rounded(px(8.))
                .border_1()
                .border_color(z.border)
                .bg(z.panel)
                .shadow_lg()
                .overflow_hidden()
                .child(rail(step, cx))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .bg(z.editor)
                        .child(
                            div()
                                .flex()
                                .flex_none()
                                .items_start()
                                .gap(px(12.))
                                .pl(px(24.))
                                .pr(px(12.))
                                .pt(px(20.))
                                .pb(px(14.))
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .flex_1()
                                        .min_w_0()
                                        .gap(px(2.))
                                        .child(
                                            div()
                                                .text_lg()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(tr(title)),
                                        )
                                        .child(div().text_sm().text_color(z.text_muted).child(tr(subtitle))),
                                )
                                // ✕ — те саме, що «Пропустити налаштування» / Esc (на «Готово» — «Завершити»).
                                .child(
                                    ui::icon_button(
                                        "wizard-close",
                                        IconName::X,
                                        if step == Step::Done { "Завершити" } else { "Пропустити налаштування (Esc)" },
                                    )
                                    .flex_none()
                                    .on_click(cx.listener(|ws, _, window, cx| ws.close_wizard(window, cx))),
                                ),
                        )
                        .child(
                            div()
                                .id("wizard-body")
                                // «Вітання» коротке: картка стискається до вмісту, а не лишає
                                // порожнє поле під ним. Решта кроків — фіксованої висоти, щоб
                                // картка не стрибала, поки дані кроку довантажуються.
                                .when(step == Step::Welcome, |d| d.flex_shrink(1.).min_h_0())
                                .when(step != Step::Welcome, |d| d.flex_1().min_h_0())
                                .overflow_y_scroll()
                                .px(px(24.))
                                .pb(px(16.))
                                .child(body),
                        )
                        .child(footer(ws, cx)),
                ),
        )
        .into_any_element()
}

fn step_heading(step: Step) -> (&'static str, &'static str) {
    match step {
        Step::Welcome => (
            "Вітаємо в Slicenv",
            "Кілька кроків — і можна нарізати модель та надіслати її на свій принтер.",
        ),
        Step::Engine => (
            "Рушій нарізки",
            "Slicenv готує модель, а G-code рахує перевірений рушій — Bambu Studio або OrcaSlicer.",
        ),
        Step::Printer => (
            "Ваш принтер",
            "Профіль моделі задає стіл, сопло й типові налаштування друку.",
        ),
        Step::Connection => (
            "З’єднання з принтером",
            "Як Slicenv надсилатиме завдання й показуватиме стан друку.",
        ),
        Step::Done => ("Усе готово", "Ось що налаштовано. Змінити можна будь-коли."),
    }
}

fn rail(current: Step, cx: &mut Context<Workspace>) -> AnyElement {
    let z = zed();
    let items = Step::ALL.into_iter().enumerate().map(|(i, step)| {
        let done = step < current;
        let active = step == current;
        div()
            .id(("wizard-rail", i))
            .flex()
            .items_center()
            .gap(px(10.))
            .h(px(34.))
            .px(px(10.))
            .rounded(px(5.))
            .text_sm()
            .when(active, |d| d.bg(z.element_selected).text_color(z.text))
            .when(!active, |d| d.text_color(z.text_muted))
            .when(step < current, |d| {
                d.cursor_pointer()
                    .hover(|s| s.bg(z.element_hover))
                    .on_click(cx.listener(move |ws, _, window, cx| ws.wizard_go(step, window, cx)))
            })
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(22.))
                    .rounded_full()
                    .border_1()
                    .map(|d| {
                        if done {
                            d.border_color(z.success.opacity(0.5)).bg(z.success.opacity(0.12))
                        } else if active {
                            d.border_color(z.accent_border).bg(z.accent_bg)
                        } else {
                            d.border_color(z.border_variant)
                        }
                    })
                    .child(if done {
                        ui::icon(IconName::Check).size(px(12.)).text_color(z.success)
                    } else {
                        ui::icon(step.icon()).size(px(12.)).text_color(if active {
                            z.accent
                        } else {
                            z.text_placeholder
                        })
                    }),
            )
            .child(tr(step.title()))
    });
    div()
        .flex()
        .flex_col()
        .flex_none()
        .w(px(RAIL_WIDTH))
        .p(px(14.))
        .gap(px(2.))
        .border_r_1()
        .border_color(z.border_variant)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(6.))
                .pb(px(14.))
                .child(gpui::img("app/slicenv.svg").size(px(22.)).flex_none())
                .child(div().font_weight(FontWeight::SEMIBOLD).child("Slicenv")),
        )
        .children(items)
        .child(div().flex_1())
        .child(
            div()
                .px(px(6.))
                .text_xs()
                .text_color(z.text_placeholder)
                .child(tr("Майстер можна відкрити знову в налаштуваннях редактора.")),
        )
        .into_any_element()
}

fn footer(ws: &Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let z = zed();
    let Some(w) = &ws.wizard else {
        return div().into_any_element();
    };
    let step = w.step;
    let next_label = match step {
        Step::Welcome => "Почати",
        Step::Done => "Завершити",
        Step::Engine if ws.engine.is_none() => "Пропустити крок",
        Step::Printer if w.machine.is_none() => "Пропустити крок",
        _ => "Далі",
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .px(px(24.))
        .py(px(12.))
        .border_t_1()
        .border_color(z.border_variant)
        .when(step != Step::Done, |d| {
            d.child(
                ui::ghost_button("wizard-skip", None, "Пропустити налаштування")
                    .text_color(z.text_muted)
                    .on_click(cx.listener(|ws, _, window, cx| ws.close_wizard(window, cx))),
            )
            .child(ui::kbd("Esc"))
        })
        // Короткий шлях для тих, хто прийшов лише відкрити файл: поруч із «Пропустити», а не
        // окремою кнопкою під вмістом кроку.
        .when(step == Step::Welcome, |d| {
            d.child(div().flex_none().w(px(1.)).h(px(16.)).mx(px(4.)).bg(z.border_variant))
                .child(
                    ui::ghost_button("wizard-open-now", Some(IconName::FolderOpen), "Відкрити модель…")
                        .text_color(z.text_muted)
                        .on_click(cx.listener(|ws, _, window, cx| {
                            ws.close_wizard(window, cx);
                            ws.prompt_open(window, cx);
                        })),
                )
        })
        .child(div().flex_1())
        .when(step.prev().is_some(), |d| {
            d.child(
                ui::ghost_button("wizard-back", Some(IconName::ArrowLeft), "Назад")
                    .border_1()
                    .border_color(z.border_variant)
                    .on_click(cx.listener(|ws, _, window, cx| ws.wizard_back(window, cx))),
            )
        })
        .child(
            ui::accent_button(
                "wizard-next",
                if step == Step::Done {
                    IconName::Check
                } else {
                    IconName::ArrowRight
                },
                next_label,
                true,
            )
            .on_click(cx.listener(|ws, _, window, cx| ws.wizard_next(window, cx))),
        )
        .child(ui::kbd("Enter"))
        .into_any_element()
}

// ---- дрібні будівельні блоки ----

fn card() -> gpui::Div {
    let z = zed();
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .p(px(14.))
        .rounded(px(6.))
        .border_1()
        .border_color(z.border_variant)
        .bg(z.panel)
}

fn muted(text: &str) -> gpui::Div {
    div().text_sm().text_color(zed().text_muted).child(tr(text))
}

fn small(text: &str) -> gpui::Div {
    div().text_xs().text_color(zed().text_placeholder).child(tr(text))
}

fn badge(text: &str, color: gpui::Hsla) -> gpui::Div {
    div()
        .flex_none()
        .px(px(6.))
        .py(px(1.))
        .rounded(px(4.))
        .border_1()
        .border_color(color.opacity(0.45))
        .bg(color.opacity(0.1))
        .text_xs()
        .text_color(color)
        .child(tr(text))
}

fn note(icon: IconName, color: gpui::Hsla, text: impl Into<SharedString>) -> gpui::Div {
    let z = zed();
    div()
        .flex()
        .items_start()
        .gap(px(8.))
        .p(px(10.))
        .rounded(px(6.))
        .bg(color.opacity(0.08))
        .border_1()
        .border_color(color.opacity(0.3))
        .text_sm()
        .child(div().pt(px(2.)).child(ui::icon(icon).text_color(color)))
        .child(div().flex_1().min_w_0().text_color(z.text).child(text.into()))
}

fn label(text: &str) -> gpui::Div {
    div()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(zed().text_muted)
        .child(tr(text))
}

fn field(
    entity: &Entity<InputState>,
    masked: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui::Div {
    let input = Input::new(entity);
    let input = if masked { input.mask_toggle() } else { input };
    widgets::field(entity, input, window, cx).h(px(28.))
}

// ---- кроки ----

fn welcome() -> AnyElement {
    let z = zed();
    let row = |icon: IconName, title: &str, text: &str| {
        div()
            .flex()
            .items_start()
            .gap(px(12.))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(32.))
                    .rounded(px(6.))
                    .bg(z.accent_bg)
                    .border_1()
                    .border_color(z.accent_border)
                    .child(ui::icon(icon).text_color(z.accent)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(tr(title)))
                    .child(muted(text)),
            )
    };
    div()
        .flex()
        .flex_col()
        .gap(px(16.))
        .child(muted(if cfg!(target_os = "linux") {
            "Slicenv — слайсер для Linux: відкриває моделі STL і 3MF, готує їх до друку, нарізає рушієм Bambu Studio чи OrcaSlicer і надсилає на принтер Bambu Lab."
        } else {
            "Slicenv відкриває моделі STL і 3MF, готує їх до друку, нарізає рушієм Bambu Studio чи OrcaSlicer і надсилає на принтер Bambu Lab."
        }))
        .child(
            card()
                .gap(px(14.))
                .child(row(
                    IconName::Cpu,
                    "Рушій нарізки",
                    // Встановлення з Slicenv є лише там, де працює менеджер рушіїв.
                    if crate::packages::MANAGER_SUPPORTED {
                        if cfg!(windows) {
                            "Знайдемо встановлений Bambu Studio чи OrcaSlicer або встановимо OrcaSlicer за кілька хвилин — без інсталятора й прав адміністратора."
                        } else {
                            "Знайдемо встановлений Bambu Studio чи OrcaSlicer або встановимо OrcaSlicer за кілька хвилин — без root і без Flatpak."
                        }
                    } else {
                        "Знайдемо встановлений Bambu Studio чи OrcaSlicer, а якщо його немає — покажемо, де завантажити офіційний інсталятор."
                    },
                ))
                .child(row(
                    IconName::Printer,
                    "Принтер",
                    "Виберемо профіль вашої моделі — або визначимо його за принтером у мережі.",
                ))
                .child(row(
                    IconName::Network,
                    "З’єднання",
                    "Локальна мережа без акаунта (рекомендовано), Bambu Cloud або без принтера — лише нарізка й експорт G-code.",
                )),
        )
        .child(note(
            IconName::Info,
            z.accent,
            tr("Займе 2–3 хвилини. Кожен крок можна пропустити, а майстер — відкрити знову: Налаштування редактора → «Майстер налаштування»."),
        ))
        .into_any_element()
}

/// Офіційні сторінки завантаження — там, де Slicenv не встановлює рушії сам.
const ORCA_DOWNLOAD: &str = "https://github.com/OrcaSlicer/OrcaSlicer/releases/latest";
const BAMBU_DOWNLOAD: &str = "https://bambulab.com/en/download/studio";

fn engine_step(ws: &Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let z = zed();
    let Some(w) = &ws.wizard else {
        return div().into_any_element();
    };
    let mut body = div().flex().flex_col().gap(px(12.));
    let busy = ws.packages.busy || ws.engine_loading;

    // Що активне зараз.
    if ws.engine_loading {
        body = body.child(note(
            IconName::LoaderCircle,
            z.accent,
            tr("Перевіряю рушій і завантажую профілі принтерів…"),
        ));
    } else if let Some(engine) = &ws.engine {
        body = body.child(note(
            IconName::CircleCheck,
            z.success,
            crate::tr_format!(
                "Готово: нарізатиме {name} {version}",
                name = engine_title(engine.kind),
                version = version_text(&ws.engine_label)
            ),
        ));
    }

    // Рушій є, але без каталогу профілів (наприклад, Bambu Studio ще жодного разу не запускали).
    if let Some(status) = crate::engines::profile_status(ws, cx) {
        body = body.child(card().child(status));
    }

    // Знайдені й встановлені.
    if !w.engines.is_empty() {
        let mut list = card().p(px(6.)).gap(px(2.));
        for (i, engine) in w.engines.iter().enumerate() {
            let active = ws.engine.as_ref().is_some_and(|e| e.kind == engine.kind && e.path == engine.path);
            let version = crate::engines::known_version(engine)
                .map(|v| version_text(&v))
                .unwrap_or_else(|| tr("версія ще не перевірена"));
            let candidate = engine.clone();
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(4.))
                    .when(active, |d| d.bg(z.element_selected))
                    .child(crate::packages::engine_logo(engine.kind.name()))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().child(format!("{} · {version}", engine_title(engine.kind))))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(z.text_placeholder)
                                    .truncate()
                                    .child(if crate::engines::is_manual(engine) {
                                        format!("{} · {}", tr("Вказано вручну"), crate::shorten_home(&engine.path))
                                    } else {
                                        crate::shorten_home(&engine.path).to_string()
                                    }),
                            ),
                    )
                    .child(if active {
                        badge("Активний", z.success).into_any_element()
                    } else {
                        ui::ghost_button(("wizard-engine-use", i), None, "Використовувати")
                            .border_1()
                            .border_color(z.border_variant)
                            .inert(busy)
                            .when(!busy, |d| {
                                d.on_click(cx.listener(move |ws, _, window, cx| {
                                    ws.activate_engine(Ok(candidate.clone()), window, cx)
                                }))
                            })
                            .into_any_element()
                    }),
            );
        }
        body = body.child(label("Знайдено на цьому комп’ютері")).child(list);
    }

    // Установка.
    let installing = w.install_family.filter(|_| ws.packages.busy);
    if !crate::packages::MANAGER_SUPPORTED {
        // Менеджер рушіїв уміє лише Linux-збірки: тут — офіційні сторінки завантаження.
        // Після установки «Шукати знову» перечитує `engines::installed()`.
        if w.engines.is_empty() && ws.engine.is_none() {
            body = body.child(note(
                IconName::Info,
                z.warning,
                tr("Рушія нарізки не знайдено. Встановіть Bambu Studio або OrcaSlicer офіційним інсталятором і натисніть «Шукати знову» — або вкажіть виконуваний файл вручну."),
            ));
        }
        let download = |id: &'static str, family: &'static str, title: &str, text: &str, url: &'static str| {
            card().child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(crate::packages::engine_logo(family))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.))
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(tr(title)))
                            .child(div().text_xs().text_color(z.text_muted).child(tr(text)))
                            .child(div().text_xs().text_color(z.text_placeholder).truncate().child(url)),
                    )
                    .child(
                        ui::ghost_button(id, Some(IconName::ExternalLink), "Завантажити")
                            .border_1()
                            .border_color(z.border_variant)
                            .on_click(move |_, _, cx| cx.open_url(url)),
                    ),
            )
        };
        body = body
            .child(label("Де завантажити"))
            .child(download(
                "wizard-download-orca",
                "orca",
                "OrcaSlicer",
                "Відкритий форк Bambu Studio: більше калібрувань і профілів інших виробників.",
                ORCA_DOWNLOAD,
            ))
            .child(download(
                "wizard-download-bambu",
                "bambu",
                "Bambu Studio",
                "Офіційний слайсер Bambu Lab: нові принтери й функції Bambu з’являються в ньому першими.",
                BAMBU_DOWNLOAD,
            ));
    } else if let Some(family) = installing {
        let progress = ws.packages.progress.clone();
        let text = progress
            .as_ref()
            .map(|p| p.label())
            .unwrap_or_else(|| tr(&ws.packages.status));
        body = body.child(
            card()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(crate::packages::engine_logo(family))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .child(div().text_sm().child(crate::tr_format!(
                                    "Встановлюю {name}",
                                    name = if family == "orca" { "OrcaSlicer" } else { "Bambu Studio" }
                                )))
                                .child(div().text_xs().text_color(z.text_muted).child(text)),
                        )
                        .child(
                            ui::ghost_button("wizard-install-cancel", Some(IconName::X), "Скасувати")
                                .border_1()
                                .border_color(z.border_variant)
                                .on_click(cx.listener(|ws, _, _, cx| {
                                    if let Some(w) = &mut ws.wizard {
                                        w.install_family = None;
                                    }
                                    ws.cancel_packages(cx)
                                })),
                        ),
                )
                .child(widgets::progress(progress.and_then(|p| p.percent).unwrap_or(2.)))
                .child(small(&crate::tr_format!(
                    "Пакет перевіряється за SHA256 і ставиться у вашу теку даних ({dir}), без root.",
                    dir = crate::shorten_home(&paths::data_dir())
                ))),
        );
    } else {
        let recommend = w.engines.is_empty() && ws.engine.is_none();
        let option = |id: &'static str, family: &'static str, title: &str, text: &str, primary: bool, cx: &mut Context<Workspace>| {
            // Розмір і «вже встановлено» — коли список випусків уже отримано.
            let release = crate::packages::newest_stable(&ws.packages.releases, family);
            let asset = release.and_then(crate::packages::recommended_asset);
            let installed = release
                .zip(asset)
                .is_some_and(|(r, a)| crate::packages::installed(&ws.packages.managed, r, a).is_some());
            let meta = release.zip(asset).map(|(r, a)| {
                crate::tr_format!(
                    "{tag} · {mb:.0} МБ",
                    tag = r["tag"].as_str().unwrap_or("").trim_start_matches('v'),
                    mb = a["size"].as_f64().unwrap_or(0.) / 1048576.
                )
            });
            card()
                .when(primary, |d| d.border_color(z.accent_border))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(crate::packages::engine_logo(family))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .gap(px(2.))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(6.))
                                        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(tr(title)))
                                        .when(primary, |d| d.child(badge("Рекомендовано", z.accent))),
                                )
                                .child(div().text_xs().text_color(z.text_muted).child(tr(text)))
                                .children(meta.map(|m| div().text_xs().text_color(z.text_placeholder).child(m))),
                        )
                        .child(if installed {
                            badge("Встановлено", z.success).into_any_element()
                        } else if primary {
                            ui::accent_button(id, IconName::Download, "Встановити", !busy)
                                .when(!busy, |d| {
                                    d.on_click(cx.listener(move |ws, _, window, cx| ws.wizard_install(family, window, cx)))
                                })
                                .into_any_element()
                        } else {
                            ui::ghost_button(id, Some(IconName::Download), "Встановити")
                                .border_1()
                                .border_color(z.border_variant)
                                .inert(busy)
                                .when(!busy, |d| {
                                    d.on_click(cx.listener(move |ws, _, window, cx| ws.wizard_install(family, window, cx)))
                                })
                                .into_any_element()
                        }),
                )
        };
        if recommend {
            body = body.child(note(
                IconName::Info,
                z.warning,
                tr("Рушія нарізки не знайдено. Встановіть його звідси: офіційні Linux-збірки з GitHub (OrcaSlicer ≈ 130 МБ, Bambu Studio ≈ 250 МБ), перевірені за SHA256."),
            ));
        }
        body = body
            .child(label(if recommend { "Встановити рушій" } else { "Встановити ще один" }))
            .child(option(
                "wizard-install-orca",
                "orca",
                "OrcaSlicer · стабільний випуск",
                "Відкритий форк Bambu Studio: більше калібрувань і профілів інших виробників.",
                recommend,
                cx,
            ))
            .child(option(
                "wizard-install-bambu",
                "bambu",
                "Bambu Studio · стабільний випуск",
                "Офіційний слайсер Bambu Lab: нові принтери й функції Bambu з’являються в ньому першими.",
                false,
                cx,
            ));
    }
    if let Some(error) = ws.packages.error.clone().filter(|_| w.install_family.is_some() || !ws.packages.busy) {
        if !ws.packages.status.is_empty() && ws.packages.status != "Готово" {
            body = body.child(note(IconName::CircleX, z.error, error));
        }
    }
    body = body.child(
        div()
            .flex()
            .gap(px(8.))
            .when(crate::packages::MANAGER_SUPPORTED, |d| {
                d.child(
                    ui::ghost_button("wizard-engine-packages", Some(IconName::Package), "Інші версії й канали…")
                        .on_click(cx.listener(|ws, _, window, cx| ws.open_packages(window, cx))),
                )
            })
            .when(!crate::packages::MANAGER_SUPPORTED, |d| {
                d.child(
                    ui::ghost_button("wizard-engine-rescan", Some(IconName::RefreshCw), "Шукати знову")
                        .inert(busy)
                        .when(!busy, |d| d.on_click(cx.listener(|ws, _, window, cx| {
                            ws.wizard_enter(window, cx);
                            cx.notify();
                        }))),
                )
            })
            .child(
                ui::ghost_button("wizard-engine-file", Some(IconName::FolderOpen), "Вказати виконуваний файл…")
                    .on_click(cx.listener(|ws, _, window, cx| ws.prompt_engine(EngineKind::Orca, window, cx))),
            ),
    );
    body.into_any_element()
}

fn printer_step(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let z = zed();
    if ws.presets.is_none() {
        return div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(note(
                IconName::TriangleAlert,
                z.warning,
                tr(if ws.engine_loading {
                    "Рушій ще завантажує профілі — список принтерів з’явиться за мить."
                } else {
                    "Профілі принтерів беруться з рушія нарізки. Спершу виберіть або встановіть рушій."
                }),
            ))
            .child(
                div().flex().child(
                    ui::ghost_button("wizard-printer-to-engine", Some(IconName::ArrowLeft), "До кроку «Рушій нарізки»")
                        .border_1()
                        .border_color(z.border_variant)
                        .on_click(cx.listener(|ws, _, window, cx| ws.wizard_go(Step::Engine, window, cx))),
                ),
            )
            .into_any_element();
    }
    // Сховище могло змінитися (рушій щойно активовано): перебудувати список.
    if ws
        .presets
        .as_ref()
        .is_some_and(|s| ws.wizard.as_ref().is_some_and(|w| w.machines_for != Arc::as_ptr(s) as usize))
    {
        ws.wizard_load_machines(window, cx);
    }
    let Some(w) = &ws.wizard else {
        return div().into_any_element();
    };
    let query = w.search.read(cx).value().trim().to_lowercase();
    let mut body = div().flex().flex_col().gap(px(12.));

    // Принтери з мережі.
    let network: Vec<(String, String, String)> = ws
        .cloud
        .lan
        .printers
        .iter()
        .map(|p| (p.name.clone(), p.model.clone(), String::new()))
        .chain(ws.cloud.lan.found.iter().map(|f| (f.name.clone(), f.model.clone(), f.ip.clone())))
        .filter(|(_, model, _)| !model.is_empty())
        .collect();
    if !network.is_empty() {
        let chips = network.into_iter().enumerate().map(|(i, (name, model, ip))| {
            let target = ws.machine_for_model(&model);
            let selected = target.is_some() && target == w.machine;
            let title = if name.is_empty() { lan::model_name(&model).to_owned() } else { name };
            div()
                .id(("wizard-net-printer", i))
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(10.))
                .py(px(6.))
                .rounded(px(6.))
                .border_1()
                .cursor_pointer()
                .map(|d| {
                    if selected {
                        d.border_color(z.accent_border).bg(z.accent_bg)
                    } else {
                        d.border_color(z.border_variant).bg(z.panel).hover(|s| s.bg(z.element_hover))
                    }
                })
                .child(ui::icon(IconName::Wifi).text_color(if selected { z.accent } else { z.text_muted }))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(div().text_sm().child(title))
                        .child(div().text_xs().text_color(z.text_muted).child(if ip.is_empty() {
                            format!("Bambu Lab {}", lan::model_name(&model))
                        } else {
                            format!("Bambu Lab {} · {ip}", lan::model_name(&model))
                        })),
                )
                .on_click(cx.listener(move |ws, _, _, cx| {
                    match ws.machine_for_model(&model) {
                        Some(name) => {
                            if let Some(w) = &mut ws.wizard {
                                w.machine = Some(name);
                            }
                        }
                        None => ws.toast(
                            ToastKind::Warning,
                            tr("У рушії немає профілю для цієї моделі — виберіть профіль зі списку"),
                            cx,
                        ),
                    }
                    cx.notify();
                }))
        });
        body = body
            .child(label("Знайдено у вашій мережі — натисніть, щоб узяти модель"))
            .child(div().flex().flex_wrap().gap(px(8.)).children(chips));
    }

    // Пошук і список профілів.
    let rows: Vec<AnyElement> = match &w.machines {
        None => vec![
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .p(px(10.))
                .child(ui::spinner("wizard-machines"))
                .child(muted("Читаю профілі принтерів…"))
                .into_any_element(),
        ],
        Some(list) => {
            let matches: Vec<&Machine> = list
                .iter()
                .filter(|m| query.is_empty() || m.name.to_lowercase().contains(&query))
                .collect();
            let total = matches.len();
            let mut rows: Vec<AnyElement> = matches
                .into_iter()
                .take(200)
                .enumerate()
                .map(|(i, m)| {
                    let selected = w.machine.as_deref() == Some(m.name.as_str());
                    let name = m.name.clone();
                    ui::list_item(("wizard-machine", i), 0, selected)
                        .h(px(28.))
                        .gap(px(8.))
                        .child(ui::icon(IconName::Printer).text_color(if selected { z.accent } else { z.text_placeholder }))
                        .child(div().flex_1().min_w_0().truncate().text_sm().child(m.name.clone()))
                        .when(m.vendor != "BBL" && !m.vendor.is_empty(), |d| {
                            d.child(div().text_xs().text_color(z.text_placeholder).child(m.vendor.clone()))
                        })
                        .when(selected, |d| d.child(ui::icon(IconName::Check).text_color(z.accent)))
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            if let Some(w) = &mut ws.wizard {
                                w.machine = Some(name.clone());
                            }
                            cx.notify();
                        }))
                        .into_any_element()
                })
                .collect();
            if total == 0 {
                rows.push(div().p(px(10.)).child(muted("Нічого не знайдено")).into_any_element());
            } else if total > 200 {
                rows.push(
                    div()
                        .p(px(8.))
                        .child(small("Показано перші 200 — уточніть пошук."))
                        .into_any_element(),
                );
            }
            rows
        }
    };
    body = body
        .child(label("Або виберіть профіль"))
        .child(field(&w.search, false, window, cx))
        .child(
            div()
                .id("wizard-machine-list")
                .h(px(230.))
                .overflow_y_scroll()
                .rounded(px(6.))
                .border_1()
                .border_color(z.border_variant)
                .bg(z.panel)
                .py(px(4.))
                .children(rows),
        )
        .child(match &w.machine {
            Some(name) => div()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_sm()
                .child(ui::icon(IconName::CircleCheck).text_color(z.success))
                .child(crate::tr_format!("Вибрано: {name}"))
                .into_any_element(),
            None => small("Не бачите свого сопла? Профіль 0.4 мм підходить для стандартного сопла; змінити можна будь-коли в «Підготовці».")
                .into_any_element(),
        });
    body.into_any_element()
}

fn connection_step(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let z = zed();
    let Some(w) = &ws.wizard else {
        return div().into_any_element();
    };
    let method = w.method;
    let option = |id: &'static str, m: Method, icon: IconName, title: &str, tag: (&str, gpui::Hsla), text: &str, cx: &mut Context<Workspace>| {
        let selected = method == m;
        div()
            .id(id)
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(px(6.))
            .p(px(12.))
            .rounded(px(6.))
            .border_1()
            .cursor_pointer()
            .map(|d| {
                if selected {
                    d.border_color(z.accent).bg(z.accent_bg)
                } else {
                    d.border_color(z.border_variant).bg(z.panel).hover(|s| s.bg(z.element_hover))
                }
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(widgets::radio((id, 0usize), selected, false))
                    .child(ui::icon(icon).text_color(if selected { z.accent } else { z.text_muted }))
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(tr(title))),
            )
            .child(div().flex().child(badge(tag.0, tag.1)))
            .child(div().text_xs().text_color(z.text_muted).child(tr(text)))
            .on_click(cx.listener(move |ws, _, window, cx| {
                if let Some(w) = &mut ws.wizard {
                    w.method = m;
                    if m == Method::Cloud && w.cloud.stage == CloudStage::Checking {
                        ws.wizard_check_cloud(window, cx);
                    }
                }
                cx.notify();
            }))
    };
    let options = div()
        .flex()
        .gap(px(10.))
        .child(option(
            "wizard-method-lan",
            Method::Lan,
            IconName::Wifi,
            "Локальна мережа",
            ("Рекомендовано · без акаунта", z.success),
            "Напряму по Wi‑Fi/LAN: стан, камера, друк. Потрібен лише код доступу з екрана принтера.",
            cx,
        ))
        .child(option(
            "wizard-method-cloud",
            Method::Cloud,
            IconName::Cloud,
            "Bambu Cloud",
            ("Потрібен акаунт Bambu", z.warning),
            "Друк і стан через сервери Bambu, також поза домом. Потрібен закритий плагін від Bambu Lab.",
            cx,
        ))
        .child(option(
            "wizard-method-later",
            Method::Later,
            IconName::Clock,
            "Пізніше",
            ("Без принтера", z.text_muted),
            "Лише нарізка й експорт G-code. Під’єднати принтер можна будь-коли на сторінці «Пристрій».",
            cx,
        ));
    let form = match method {
        Method::Lan => lan_form(ws, window, cx),
        Method::Cloud => cloud_form(ws, window, cx),
        Method::Later => note(
            IconName::Info,
            z.accent,
            tr("Без принтера Slicenv повністю працює як слайсер: «Перегляд» → «Експортувати…» зберігає G-code (.gcode.3mf) для microSD. Під’єднати принтер можна будь-коли: сторінка «Пристрій» або цей майстер."),
        )
        .into_any_element(),
    };
    div()
        .flex()
        .flex_col()
        .gap(px(14.))
        .child(options)
        .child(form)
        .into_any_element()
}

/// Схема екрана принтера: де шукати IP і код доступу (текстом, без чужих зображень).
fn screen_hint() -> gpui::Div {
    let z = zed();
    let row = |key: &str, value: &str, hot: bool| {
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(8.))
            .h(px(22.))
            .rounded(px(3.))
            .when(hot, |d| d.bg(z.accent_bg).border_1().border_color(z.accent))
            .child(div().flex_1().text_xs().text_color(if hot { z.text } else { z.text_muted }).child(tr(key)))
            .child(
                div()
                    .text_xs()
                    .font_family(crate::theme::MONO_FONT)
                    .text_color(if hot { z.accent } else { z.text_muted })
                    .child(value.to_owned()),
            )
    };
    div()
        .flex()
        .flex_col()
        .flex_none()
        .w(px(250.))
        .gap(px(6.))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(3.))
                .p(px(8.))
                .rounded(px(8.))
                .border_2()
                .border_color(z.border)
                .bg(gpui::black().opacity(0.35))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .pb(px(4.))
                        .mb(px(2.))
                        .border_b_1()
                        .border_color(z.border_variant)
                        .child(ui::icon(IconName::Settings).size(px(12.)).text_color(z.text_muted))
                        .child(div().text_xs().font_weight(FontWeight::MEDIUM).child(tr("Налаштування → WLAN"))),
                )
                .child(row("Мережа", "Home-WiFi", false))
                .child(row("IP", "192.168.0.23", false))
                .child(row("Access Code", "1a2b3c4d", true))
                .child(row("LAN Only", "○", false)),
        )
        .child(small(
            "A1, A1 mini: значок налаштувань → вкладка WLAN. P1, X1, P2S, H2D: Налаштування → Мережа (Network). Код змінюється, якщо скинути мережу на принтері.",
        ))
}

fn lan_form(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let z = zed();
    let Some(w) = &ws.wizard else {
        return div().into_any_element();
    };
    let lan = &ws.cloud.lan;
    // Збережені й знайдені — один список; серійник показуємо скорочено.
    struct Row {
        serial: String,
        title: String,
        detail: String,
        saved: bool,
        lan_only: bool,
    }
    let mut rows: Vec<Row> = lan
        .printers
        .iter()
        .map(|p| Row {
            serial: p.dev_id.as_str().to_owned(),
            title: if p.name.is_empty() { format!("Bambu Lab {}", lan::model_name(&p.model)) } else { p.name.clone() },
            detail: format!("{} · {}", lan::model_name(&p.model), lan::store::redact(p.dev_id.as_str())),
            saved: true,
            lan_only: false,
        })
        .collect();
    rows.extend(lan.found.iter().map(|f| Row {
        serial: f.serial.clone(),
        title: if f.name.is_empty() { format!("Bambu Lab {}", lan::model_name(&f.model)) } else { f.name.clone() },
        detail: format!("{} · {} · {}", lan::model_name(&f.model), f.ip, lan::store::redact(&f.serial)),
        saved: false,
        lan_only: f.lan_only,
    }));
    let selected_saved = w
        .lan
        .selected
        .as_ref()
        .filter(|_| !w.lan.manual)
        .is_some_and(|s| lan.printers.iter().any(|p| p.dev_id.as_str() == s));
    let manual = w.lan.manual || rows.is_empty();

    let mut list = div().flex().flex_col().gap(px(4.));
    for (i, row) in rows.into_iter().enumerate() {
        let selected = !w.lan.manual && w.lan.selected.as_deref() == Some(row.serial.as_str());
        let serial = row.serial.clone();
        list = list.child(
            div()
                .id(("wizard-lan-row", i))
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(10.))
                .py(px(7.))
                .rounded(px(6.))
                .border_1()
                .cursor_pointer()
                .map(|d| {
                    if selected {
                        d.border_color(z.accent_border).bg(z.accent_bg)
                    } else {
                        d.border_color(z.border_variant).bg(z.panel).hover(|s| s.bg(z.element_hover))
                    }
                })
                .child(widgets::radio(("wizard-lan-radio", i), selected, false))
                .child(ui::icon(IconName::Printer).text_color(z.text_muted))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().truncate().child(row.title))
                        .child(div().text_xs().text_color(z.text_muted).truncate().child(row.detail)),
                )
                .when(row.lan_only, |d| d.child(badge("LAN Only", z.success)))
                .when(row.saved, |d| d.child(badge("Збережено", z.success)))
                .on_click(cx.listener(move |ws, _, _, cx| {
                    if let Some(w) = &mut ws.wizard {
                        w.lan.selected = Some(serial.clone());
                        w.lan.manual = false;
                        w.lan.error = None;
                    }
                    cx.notify();
                })),
        );
    }
    let has_rows = !lan.found.is_empty() || !lan.printers.is_empty();
    let searching = div()
        .flex()
        .items_center()
        .gap(px(8.))
        .p(px(10.))
        .rounded(px(6.))
        .border_1()
        .border_color(z.border_variant)
        .bg(z.panel)
        .child(ui::spinner("wizard-lan-search"))
        .child(muted("Шукаю принтери в мережі… Принтер має бути увімкнений і в тій самій мережі, що й комп’ютер."));

    let mut left = div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap(px(8.))
        .child(label("Принтери в мережі"))
        .child(if has_rows { list.into_any_element() } else { searching.into_any_element() })
        .child(
            div().flex().child(
                ui::ghost_button(
                    "wizard-lan-manual",
                    Some(if manual { IconName::ChevronDown } else { IconName::ChevronRight }),
                    "Ввести IP і серійний номер вручну",
                )
                .on_click(cx.listener(|ws, _, _, cx| {
                    if let Some(w) = &mut ws.wizard {
                        w.lan.manual = !w.lan.manual;
                        w.lan.error = None;
                    }
                    cx.notify();
                })),
            ),
        );
    if manual {
        left = left
            .child(label("IP-адреса"))
            .child(field(&w.lan.ip, false, window, cx))
            .child(label("Серійний номер"))
            .child(field(&w.lan.serial, false, window, cx))
            .child(small("Серійний номер — на екрані принтера (Налаштування → Пристрій) або на наклейці ззаду."));
    }
    let testing = w.lan.testing || lan.adding;
    if selected_saved && !manual {
        left = left.child(note(
            IconName::CircleCheck,
            z.success,
            tr("Цей принтер уже збережено — з’єднання налаштоване. Можна переходити далі."),
        ));
    } else {
        left = left
            .child(label("Код доступу"))
            .child(field(&w.lan.code, true, window, cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        ui::accent_button("wizard-lan-test", IconName::Plug, "Перевірити й зберегти", !testing)
                            .when(!testing, |d| d.on_click(cx.listener(|ws, _, _, cx| ws.wizard_test_lan(cx)))),
                    )
                    .when(testing, |d| {
                        d.child(ui::spinner("wizard-lan-testing"))
                            .child(muted("Перевіряю з’єднання… до 15 с"))
                    }),
            );
        let error = w.lan.error.clone().or_else(|| lan.add_error.clone().filter(|_| !testing));
        if let Some(saved) = &w.lan.saved {
            let _ = saved;
            left = left.child(note(
                IconName::CircleCheck,
                z.success,
                tr("З’єднання є: принтер відповів і збережений. Стан і камера вже доступні на сторінці «Пристрій»."),
            ));
        } else if let Some(error) = error {
            // Невірний код: принтер після кількох спроб поспіль може тимчасово блокувати вхід.
            let bad_code = error.contains(&tr(lan::LanError::BadAccessCode.key()));
            left = left.child(note(IconName::CircleX, z.error, error)).when(bad_code, |d| {
                d.child(small("Кілька невдалих спроб поспіль принтер може тимчасово блокувати — звірте код з екраном, перш ніж пробувати знову."))
            });
        }
    }
    div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(div().flex().gap(px(16.)).child(left).child(screen_hint()))
        .child(note(
            IconName::Info,
            z.accent,
            tr("Стан, температури й камера працюють одразу. Щоб друкувати й керувати без хмари на прошивках 2025 року й новіших, увімкніть на екрані принтера LAN Only Mode і Developer Mode (Налаштування → Мережа). У режимі LAN Only принтер не з’єднується з Bambu Cloud і застосунком Bambu Handy."),
        ))
        .into_any_element()
}

fn cloud_form(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let z = zed();
    let Some(w) = &ws.wizard else {
        return div().into_any_element();
    };
    let c = &w.cloud;
    let mut body = div().flex().flex_col().gap(px(12.));
    match &c.stage {
        CloudStage::Checking => {
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(ui::spinner("wizard-cloud-check"))
                    .child(muted("Перевіряю плагін і вхід Bambu…")),
            );
        }
        CloudStage::UnsupportedOs => {
            body = body.child(note(IconName::Info, z.warning, tr(cloud::UNSUPPORTED_OS)));
        }
        CloudStage::NeedPlugin(unsupported) => {
            let remote_line = match &c.remote {
                None => tr("Отримую відомості про плагін із сервера Bambu Lab…"),
                Some(Ok(r)) => match r.size {
                    Some(size) => crate::tr_format!(
                        "Версія {v} · {mb:.1} МБ · завантажується з серверів Bambu Lab",
                        v = r.version,
                        mb = size as f64 / 1048576.
                    ),
                    None => crate::tr_format!("Версія {v} · завантажується з серверів Bambu Lab", v = r.version),
                },
                Some(Err(e)) => crate::tr_format!("Не вдалося отримати відомості про плагін: {e}"),
            };
            let ready = matches!(c.remote, Some(Ok(_)));
            let mut plugin = card()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(ui::icon(IconName::Package).text_color(z.warning))
                        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(tr("Мережевий плагін Bambu Lab")))
                        .child(div().flex_1())
                        .child(badge("Закрита програма", z.warning)),
                )
                .child(div().text_sm().child(tr(cloud::plugin::CONSENT_TEXT)))
                .child(div().text_xs().text_color(z.text_muted).child(remote_line));
            if let Some(version) = unsupported {
                plugin = plugin.child(small(&crate::tr_format!(
                    "Знайдений плагін {version} цією версією Slicenv не підтримується — буде встановлено підтримувану поруч."
                )));
            }
            if c.installing {
                let pct = c.progress.unwrap_or(0.) * 100.;
                plugin = plugin
                    .child(widgets::progress(pct.max(2.)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(ui::spinner("wizard-plugin-progress"))
                            .child(div().flex_1().text_xs().text_color(z.text_muted).child(crate::tr_format!("Завантаження і перевірка… {pct:.0}%")))
                            .child(
                                ui::ghost_button("wizard-plugin-cancel", Some(IconName::X), "Скасувати")
                                    .border_1()
                                    .border_color(z.border_variant)
                                    .on_click(cx.listener(|ws, _, _, cx| {
                                        if let Some(w) = &ws.wizard {
                                            w.cloud.cancel.store(true, Ordering::Release);
                                        }
                                        cx.notify();
                                    })),
                            ),
                    );
            } else {
                plugin = plugin.child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(
                            ui::accent_button("wizard-plugin-accept", IconName::Download, "Погоджуюсь — завантажити плагін", ready)
                                .when(ready, |d| d.on_click(cx.listener(|ws, _, window, cx| ws.wizard_install_plugin(window, cx)))),
                        )
                        .when(matches!(c.remote, Some(Err(_))), |d| {
                            d.child(
                                ui::ghost_button("wizard-plugin-retry", Some(IconName::RefreshCw), "Спробувати знову")
                                    .on_click(cx.listener(|ws, _, window, cx| ws.wizard_fetch_plugin(window, cx))),
                            )
                        }),
                );
            }
            if let Some(error) = &c.plugin_error {
                plugin = plugin.child(note(IconName::CircleX, z.error, error.clone()));
            }
            body = body
                .child(plugin)
                .child(small("Без плагіна хмара недоступна, але локальна мережа працює повністю. Щоб видалити плагін і відкликати згоду, видаліть теку:"))
                .child(
                    div()
                        .text_xs()
                        .font_family(crate::theme::MONO_FONT)
                        .text_color(z.text_muted)
                        .child(crate::shorten_home(&cloud::plugin::managed_root())),
                );
        }
        CloudStage::Sessions => {
            let mut list = div().flex().flex_col().gap(px(8.));
            // Одна головна дія на стан картки: перевіряється — лише спінер; дійсний вхід —
            // «Використати»; не відновився — причина і «Увійти через браузер» прямо тут.
            let any_failed = c.sessions.iter().any(|s| matches!(s.nickname, Some(Err(_))));
            let any_usable = c.sessions.iter().any(|s| !matches!(s.nickname, Some(Err(_))));
            for (i, s) in c.sessions.iter().enumerate() {
                let app = match s.found.origin {
                    cloud::SessionOrigin::OrcaSlicer => "OrcaSlicer",
                    _ => "Bambu Studio",
                };
                let failed = matches!(s.nickname, Some(Err(_)));
                let line = match &s.nickname {
                    None => tr("Перевіряю вхід…"),
                    Some(Ok(nick)) if !nick.is_empty() => crate::tr_format!("Увійдено як {nick}"),
                    Some(Ok(_)) => tr("Вхід дійсний"),
                    Some(Err(e)) => e.clone(),
                };
                let action = match &s.nickname {
                    None => ui::spinner(("wizard-nick", i)),
                    Some(Ok(nick)) => ui::accent_button(
                        ("wizard-adopt", i),
                        IconName::LogIn,
                        if nick.is_empty() {
                            tr("Використати цей вхід")
                        } else {
                            crate::tr_format!("Використати вхід як {nick}")
                        },
                        true,
                    )
                    .on_click(cx.listener(move |ws, _, _, cx| ws.wizard_adopt(i, cx)))
                    .into_any_element(),
                    Some(Err(_)) => ui::accent_button(("wizard-session-browser", i), IconName::ExternalLink, "Увійти через браузер", !c.busy)
                        .when(!c.busy, |d| {
                            d.on_click(cx.listener(|ws, _, window, cx| {
                                if let Some(w) = &mut ws.wizard {
                                    w.cloud.stage = CloudStage::Login;
                                    w.cloud.login = LoginStage::Password;
                                }
                                ws.wizard_browser_login(window, cx);
                            }))
                        })
                        .into_any_element(),
                };
                list = list.child(
                    card().child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(crate::packages::engine_logo(if app == "OrcaSlicer" { "orca" } else { "bambu" }))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().text_sm().child(if failed {
                                        crate::tr_format!("Вхід {app} не відновився")
                                    } else {
                                        crate::tr_format!("Знайдено вхід {app}")
                                    }))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(if failed { z.warning } else { z.text_muted })
                                            .child(line),
                                    ),
                            )
                            .child(action),
                    ),
                );
            }
            body = body.child(list);
            if any_usable {
                body = body.child(small("Slicenv збереже власну копію цієї сесії; файли Bambu Studio / OrcaSlicer не змінюються. Це той самий вхід: вихід на сервері розлогінить обидві програми."));
            }
            // Вторинна дія не дублює головну: якщо в картці вже є «Увійти через браузер» —
            // лишається тільки вхід паролем.
            let (other_label, other_icon) = if any_failed && !any_usable {
                ("Увійти паролем", IconName::KeyRound)
            } else {
                ("Увійти іншим акаунтом", IconName::User)
            };
            body = body.child(
                div().flex().child(
                    ui::ghost_button("wizard-other-account", Some(other_icon), other_label)
                        .border_1()
                        .border_color(z.border_variant)
                        .on_click(cx.listener(|ws, _, _, cx| {
                            if let Some(w) = &mut ws.wizard {
                                w.cloud.stage = CloudStage::Login;
                            }
                            cx.notify();
                        })),
                ),
            );
        }
        CloudStage::Login => {
            let busy = c.busy;
            let region = div()
                .flex()
                .items_center()
                .gap(px(4.))
                .child(label("Регіон акаунта").mr(px(6.)))
                .children([(false, "Глобальний"), (true, "Китай")].into_iter().enumerate().map(|(i, (china, text))| {
                    ui::ghost_button(("wizard-region", i), None, text)
                        .toggled(c.china == china)
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            if let Some(w) = &mut ws.wizard {
                                w.cloud.china = china;
                                w.cloud.remote = None;
                            }
                            cx.notify();
                        }))
                }));
            let waiting = c.browser.as_ref().map(|(_, url)| url.clone());
            let password_stage = matches!(c.login, LoginStage::Password);
            let mut form = card().gap(px(8.)).when(password_stage, |d| d.child(region));
            if password_stage {
                // Головний шлях: офіційна сторінка входу Bambu в системному браузері —
                // там доступні Google, Apple і email; квиток повертається на localhost.
                let browser = match &waiting {
                    None => div().flex().child(
                        ui::accent_button("wizard-browser-login", IconName::ExternalLink, "Увійти через браузер (Google, Apple, email)", !busy)
                            .when(!busy, |d| d.on_click(cx.listener(|ws, _, window, cx| ws.wizard_browser_login(window, cx)))),
                    ),
                    Some(url) => {
                        let url = url.clone();
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(8.))
                            .child(ui::spinner("wizard-browser-busy"))
                            .child(muted("Чекаю на вхід у браузері…"))
                            .child(
                                ui::ghost_button("wizard-browser-reopen", Some(IconName::ExternalLink), "Відкрити сторінку ще раз")
                                    .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))),
                            )
                            .child(
                                ui::ghost_button("wizard-browser-cancel", Some(IconName::X), "Скасувати")
                                    .border_1()
                                    .border_color(z.border_variant)
                                    .on_click(cx.listener(|ws, _, _, cx| ws.wizard_browser_cancel(cx))),
                            )
                    }
                };
                form = form.child(browser).child(small(
                    "Відкриється офіційна сторінка bambulab.com; після входу вона сама поверне Slicenv одноразовий квиток.",
                ));
                if waiting.is_none() {
                    form = form.child(
                        div()
                            .pt(px(6.))
                            .border_t_1()
                            .border_color(z.border_variant)
                            .child(muted("Або email і паролем Bambu")),
                    );
                }
            }
            match &c.login {
                LoginStage::Password if waiting.is_some() => {}
                LoginStage::Password => {
                    form = form
                        .child(label("Email"))
                        .child(field(&c.email, false, window, cx))
                        .child(label("Пароль"))
                        .child(field(&c.password, true, window, cx));
                }
                LoginStage::EmailCode => {
                    form = form
                        .child(note(IconName::Mail, z.accent, tr("Bambu надіслав код підтвердження на вашу пошту. Введіть його нижче.")))
                        .child(label("Код із листа"))
                        .child(field(&c.code, false, window, cx))
                        .child(
                            div().flex().child(
                                ui::ghost_button("wizard-resend", Some(IconName::RefreshCw), "Надіслати код знову")
                                    .on_click(cx.listener(|ws, _, _, cx| ws.wizard_resend_code(cx))),
                            ),
                        );
                }
                LoginStage::Tfa(_) => {
                    form = form
                        .child(note(IconName::ShieldCheck, z.accent, tr("Для акаунта ввімкнено двофакторну перевірку. Введіть код із застосунку-автентифікатора.")))
                        .child(label("Код 2FA"))
                        .child(field(&c.code, false, window, cx));
                }
            }
            form = form.when(waiting.is_none(), |form| form.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        ui::accent_button(
                            "wizard-login",
                            IconName::LogIn,
                            if matches!(c.login, LoginStage::Password) { "Увійти" } else { "Підтвердити" },
                            !busy,
                        )
                        .when(!busy, |d| d.on_click(cx.listener(|ws, _, window, cx| ws.wizard_login(window, cx)))),
                    )
                    .when(!matches!(c.login, LoginStage::Password) && !busy, |d| {
                        d.child(
                            ui::ghost_button("wizard-login-restart", None, "Інший акаунт")
                                .on_click(cx.listener(|ws, _, _, cx| {
                                    if let Some(w) = &mut ws.wizard {
                                        w.cloud.login = LoginStage::Password;
                                        w.cloud.error = None;
                                    }
                                    cx.notify();
                                })),
                        )
                    })
                    .when(busy, |d| d.child(ui::spinner("wizard-login-busy")).child(muted("Зв’язуюся з Bambu…"))),
            ));
            if let Some(error) = &c.error {
                form = form.child(note(IconName::CircleX, z.error, error.clone()));
            }
            body = body
                .child(form)
                .child(small("Пароль іде лише на сервер Bambu Lab і ніде не зберігається; Slicenv зберігає тільки сесію плагіна (файл 0600)."));
            if !c.sessions.is_empty() {
                body = body.child(
                    div().flex().child(
                        ui::ghost_button("wizard-back-sessions", Some(IconName::ArrowLeft), "Знайдені входи")
                            .on_click(cx.listener(|ws, _, _, cx| {
                                if let Some(w) = &mut ws.wizard {
                                    w.cloud.stage = CloudStage::Sessions;
                                }
                                cx.notify();
                            })),
                    ),
                );
            }
        }
        CloudStage::Ready => {
            let who = match &c.signed_in {
                None => tr("Перевіряю вхід…"),
                Some(nick) if !nick.is_empty() => crate::tr_format!("Вхід у Bambu Cloud: {nick}"),
                Some(_) => tr("Вхід у Bambu Cloud виконано"),
            };
            body = body.child(note(IconName::CircleCheck, z.success, who));
        }
    }
    body.into_any_element()
}

fn done_step(ws: &Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let z = zed();
    let Some(w) = &ws.wizard else {
        return div().into_any_element();
    };
    let row = |icon: IconName, ok: bool, title: &str, value: String| {
        div()
            .flex()
            .items_center()
            .gap(px(12.))
            .py(px(8.))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(28.))
                    .rounded_full()
                    .bg(if ok { z.success.opacity(0.12) } else { z.element })
                    .child(ui::icon(icon).text_color(if ok { z.success } else { z.text_placeholder })),
            )
            .child(div().w(px(120.)).flex_none().text_sm().text_color(z.text_muted).child(tr(title)))
            .child(div().flex_1().min_w_0().text_sm().child(value))
    };
    let engine = match &ws.engine {
        Some(e) => (true, format!("{} {}", engine_title(e.kind), version_text(&ws.engine_label))),
        None => (false, tr("не вибрано — нарізка недоступна, поки не встановите рушій")),
    };
    let printer = (ws.presets.is_some(), ws.selection.machine.clone());
    let connection = match w.method {
        Method::Lan => match ws.cloud.lan.printers.first() {
            Some(p) => (
                true,
                crate::tr_format!(
                    "Локальна мережа · {name}",
                    name = if p.name.is_empty() { lan::model_name(&p.model).to_owned() } else { p.name.clone() }
                ),
            ),
            None => (false, tr("локальна мережа — принтер ще не додано")),
        },
        Method::Cloud => match (&w.cloud.stage, &w.cloud.signed_in) {
            (CloudStage::Ready, Some(nick)) if !nick.is_empty() => (true, crate::tr_format!("Bambu Cloud · {nick}")),
            (CloudStage::Ready, _) => (true, tr("Bambu Cloud")),
            _ => (false, tr("Bambu Cloud — вхід не завершено")),
        },
        Method::Later => (false, tr("без принтера: нарізка й експорт G-code")),
    };
    // Хмару щойно налаштовано: вона вже підключається, кнопка веде на «Пристрій».
    let cloud_ready = w.cloud.changed && w.cloud.stage == CloudStage::Ready;
    div()
        .flex()
        .flex_col()
        .gap(px(14.))
        .child(
            card()
                .gap(px(0.))
                .py(px(6.))
                .child(row(IconName::Cpu, engine.0, "Рушій", engine.1))
                .child(ui::divider())
                .child(row(IconName::Printer, printer.0, "Принтер", printer.1))
                .child(ui::divider())
                .child(row(IconName::Network, connection.0, "З’єднання", connection.1)),
        )
        .child(label("Що далі"))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(8.))
                .child(
                    ui::accent_button("wizard-open-model", IconName::FolderOpen, "Відкрити модель…", true)
                        .on_click(cx.listener(|ws, _, window, cx| {
                            ws.finish_wizard(window, cx);
                            ws.prompt_open(window, cx);
                        })),
                )
                .when(!ws.cloud.lan.printers.is_empty() || cloud_ready, |d| {
                    d.child(
                        ui::ghost_button("wizard-open-device", Some(IconName::Printer), "Сторінка «Пристрій»")
                            .border_1()
                            .border_color(z.border_variant)
                            .on_click(cx.listener(|ws, _, window, cx| {
                                ws.finish_wizard(window, cx);
                                ws.enter_page(Page::Device, cx);
                                cx.notify();
                            })),
                    )
                })
        )
        .child(small(
            "Клавіші: Ctrl+O — відкрити модель, Ctrl+R — нарізати, Ctrl+5 — сторінка «Пристрій», Ctrl+, — налаштування.",
        ))
        .into_any_element()
}
