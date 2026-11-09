//! «Пристрій»: Bambu Cloud через офіційний плагін — принтери акаунта, живий статус, керування.
//!
//! `cloud::Client` прив’язаний до свого потоку, тож живе в окремому робочому потоці;
//! інтерфейс спілкується з ним каналами.

mod guard;
mod job;
mod klipper;
mod lan;
mod pages;
pub mod print_paths;

pub use lan::{LanCmd, error_text as lan_error_text};

use std::{
    rc::Rc,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use cloud::{Client, Control, DeviceId, Event, ModuleVersion, Options, PrinterStatus, SpeedLevel};
use futures::{StreamExt as _, channel::mpsc as async_mpsc};
use gpui::{Context, Focusable as _, SharedString, Window, div, prelude::*, px};
use gpui_component::input::InputState;

use crate::{
    i18n::tr,
    theme::zed,
    ui::{self, IconName},
    widgets::ToastKind,
    workspace::Workspace,
};

pub enum CloudCmd {
    Select(DeviceId),
    Refresh,
    /// Проходить `guard::check` ще раз у робочому потоці: стан міг змінитися в польоті.
    Control(DeviceId, Control),
    Print(Box<cloud::PrintRequest>),
    /// Принтер за серійним номером, коли хмара не віддає список акаунта.
    AddManual(DeviceId),
    Tasks,
    Camera(bool),
    /// Лише читання: запит `info/get_version`.
    Versions,
    /// Мініатюра поточного завдання; ключ відсікає відповіді на попередні завдання.
    Thumbnail(job::JobKey),
    /// Підключитися знову (після входу в майстрі): без перезапуску застосунку.
    Reconnect,
    /// Close this client's camera/network session before process teardown.
    Shutdown,
}

/// Рядок історії друку для показу.
#[derive(Clone)]
pub struct TaskRow {
    pub title: String,
    pub status: Option<i32>,
    pub date: String,
    pub cost_minutes: Option<u64>,
    pub weight: Option<f64>,
}

enum CloudMsg {
    /// Українська «ключова» фраза; перекладається в інтерфейсі.
    Phase(String),
    /// Список принтерів недоступний: повтор через `secs` с.
    ListRetry {
        secs: u64,
        cached: bool,
    },
    /// З’єднання робочого потоку з сервером змінилося.
    Link(bool),
    Printers(Vec<cloud::Printer>),
    Status(DeviceId, Box<PrinterStatus>),
    Versions(DeviceId, Vec<ModuleVersion>),
    Thumbnail(job::JobKey, Result<image::RgbaImage, String>),
    Error(String),
    /// Робочий потік відхилив команду охоронцем (текст уже готовий до показу).
    Blocked(String),
    PrintSent(Result<String, String>),
    /// Плагін відмовив у хмарному друці непідписаній програмі (`BAMBU_NETWORK_SIGNED_ERROR`).
    PrintUnsigned,
    /// Хмара справді прийняла завдання від плагіна: відтепер діалог пропонує й хмарний шлях.
    CloudPrintWorks,
    Tasks(Vec<TaskRow>),
    /// Кадр камери в BGRA (як очікує RenderImage).
    CameraFrame(image::RgbaImage),
    CameraStopped(Option<String>),
    /// Стан LAN-принтерів (`device::lan`); статус, камера й друк ідуть звичайними варіантами.
    Lan(lan::LanMsg),
    /// Стан принтерів Klipper (`device::klipper`), так само поруч зі звичайними варіантами.
    Klipper(klipper::KlipperMsg),
}

/// Верхні вкладки сторінки принтера.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum DeviceTab {
    #[default]
    Status,
    Storage,
    Update,
    Hms,
}

/// Поля вводу цілей; значення синхронізуються зі знімком, поки поле не в фокусі.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeviceField {
    Nozzle,
    Bed,
    PartFan,
    AuxFan,
    ChamberFan,
}

pub struct ControlInputs {
    pub nozzle: gpui::Entity<InputState>,
    pub bed: gpui::Entity<InputState>,
    pub part_fan: gpui::Entity<InputState>,
    pub aux_fan: gpui::Entity<InputState>,
    pub chamber_fan: gpui::Entity<InputState>,
}

#[derive(Default)]
pub struct CloudState {
    pub phase: SharedString,
    /// Іде підключення (плагін, вхід, сервер): замість крапки — анімований індикатор.
    pub connecting: bool,
    pub printers: Vec<cloud::Printer>,
    pub selected: Option<DeviceId>,
    pub status: Option<PrinterStatus>,
    pub updated: Option<Instant>,
    pub error: Option<SharedString>,
    pub print: PrintOptions,
    pub sending: bool,
    /// Хмарний друк плагіном хоч раз пройшов у цьому сеансі. За замовчуванням false:
    /// плагін Bambu відхиляє непідписані програми (-26), і пробувати наосліп не варто.
    pub cloud_print_works: bool,
    pub serial_input: Option<gpui::Entity<InputState>>,
    pub inputs: Option<ControlInputs>,
    pub tab: DeviceTab,
    /// Крок руху головки, мм (1 або 10).
    pub jog_step: i8,
    /// Чи з’єднаний робочий потік із сервером (за замовчуванням false, доки не повідомить).
    pub connected: bool,
    pub tasks: Vec<TaskRow>,
    pub versions: Vec<ModuleVersion>,
    pub job: job::JobThumb<Arc<gpui::RenderImage>>,
    pub camera_on: bool,
    pub camera_frame: Option<Arc<gpui::RenderImage>>,
    pub camera_note: Option<SharedString>,
    /// Принтер, для якого камеру вже автоматично запускали (повторно не нав’язуємо).
    pub camera_auto: Option<DeviceId>,
    /// LAN-принтери поруч із хмарою; `printers` уже містить і їх.
    pub lan: lan::LanUi,
    /// Принтери Klipper (Moonraker); `printers` містить і їх.
    pub klipper: klipper::KlipperUi,
    commands: Option<mpsc::Sender<CloudCmd>>,
}

/// Галочки модалки «Надіслати на друк» (значення за замовчуванням як у Bambu Studio для A1).
#[derive(Clone, Copy)]
pub struct PrintOptions {
    pub bed_leveling: bool,
    pub flow_cali: bool,
    pub vibration_cali: bool,
    pub timelapse: bool,
}

impl Default for PrintOptions {
    fn default() -> Self {
        Self {
            bed_leveling: true,
            flow_cali: true,
            vibration_cali: true,
            timelapse: false,
        }
    }
}

impl CloudState {
    pub fn send(&self, cmd: CloudCmd) {
        let Some(cmd) = self.klipper.route(self.selected.as_ref(), cmd) else {
            return;
        };
        let Some(cmd) = self.lan.route(self.selected.as_ref(), cmd) else {
            return;
        };
        // Вибір принтера Klipper хмарі нецікавий (LAN-робітник уже від’єднався).
        if let CloudCmd::Select(dev) = &cmd
            && self.klipper.has(dev)
        {
            return;
        }
        if let Some(tx) = &self.commands {
            tx.send(cmd).ok();
        }
    }

    /// Знімок для охоронця команд.
    fn snapshot(&self) -> guard::Snapshot<'_> {
        guard::Snapshot {
            selected: self.selected.as_ref(),
            printer: self
                .selected
                .as_ref()
                .and_then(|dev| self.printers.iter().find(|p| &p.dev_id == dev)),
            status: self.status.as_ref(),
            age: self.updated.map(|t| t.elapsed()),
            connected: self.link_up(),
            lan: self.selected.as_ref().is_some_and(|d| self.lan.has(d)),
        }
    }

    /// Єдина точка, через яку інтерфейс відправляє `Control`: охоронець працює тут, а не в
    /// стилі кнопок. Заблоковану команду в канал не кладемо.
    pub fn dispatch(&self, control: Control) -> Result<(), guard::Blocked> {
        let dev = self.selected.clone().ok_or(guard::Blocked::NoPrinter)?;
        if self.klipper.has(&dev) {
            klipper::check(&self.klipper, self.selected.as_ref(), self.status.as_ref(), &dev, control)?;
            self.klipper.send(klipper::KlipperCmd::Control(dev, control));
            return Ok(());
        }
        guard::check(&self.snapshot(), &dev, control)?;
        if self.lan.has(&dev) {
            self.lan.send(lan::LanCmd::Control(dev, control));
            return Ok(());
        }
        self.commands
            .as_ref()
            .ok_or(guard::Blocked::Disconnected)?
            .send(CloudCmd::Control(dev, control))
            .map_err(|_| guard::Blocked::Disconnected)
    }

    /// Хмара + LAN + Klipper в одному списку для лівого доку.
    pub fn rebuild_printers(&mut self) {
        let mut list = self.lan.union();
        list.extend(self.klipper.printers().cloned());
        self.printers = list;
    }

    /// Вибір (або скидання) принтера: усе, що належало попередньому, зникає. Зображення
    /// повертаються вікну на звільнення.
    pub fn select(&mut self, dev: Option<DeviceId>, window: &mut Window) {
        self.selected = dev;
        self.status = None;
        self.updated = None;
        self.versions.clear();
        self.camera_on = false;
        self.camera_note = None;
        for image in [self.job.clear(), self.camera_frame.take()]
            .into_iter()
            .flatten()
        {
            window.drop_image(image).ok();
        }
    }

    /// Короткий стан вибраного принтера для заголовка й рядка стану: текст, колір, «живий».
    /// Позначка стану: анімований індикатор під час підключення, інакше кольорова крапка.
    pub fn indicator(&self, id: &'static str, color: gpui::Hsla, live: bool) -> gpui::AnyElement {
        let klipper_problem = self
            .selected
            .as_ref()
            .is_some_and(|d| self.klipper.has(d) && self.klipper.problem.is_some());
        let waiting_status = self.selected.is_some()
            && self.status.is_none()
            && self.error.is_none()
            && !klipper_problem;
        if self.connecting || waiting_status {
            ui::spinner(id)
        } else {
            ui::status_dot(color, live)
        }
    }

    pub fn summary(&self) -> (String, gpui::Hsla, bool) {
        let z = crate::theme::zed();
        let Some(dev) = &self.selected else {
            return (
                tr(&self.phase),
                z.text_placeholder,
                self.printers.is_empty() && self.error.is_none(),
            );
        };
        let printer = self.printers.iter().find(|p| &p.dev_id == dev);
        let name = printer.map_or_else(
            || tr("Пристрої"),
            |p| {
                if (p.model.is_empty() || self.klipper.has(dev)) && !p.name.is_empty() {
                    p.name.clone()
                } else {
                    model_label(p)
                }
            },
        );
        if printer.is_some_and(|p| !p.online) {
            return (
                format!("{name} · {}", tr("офлайн")),
                z.text_placeholder,
                false,
            );
        }
        if self.status.is_none() && self.klipper.has(dev) && self.klipper.problem.is_some() {
            return (format!("{name} · {}", tr("Klipper не готовий")), z.warning, false);
        }
        let Some(s) = &self.status else {
            return (
                format!("{name} · {}", tr("підключення…")),
                z.text_muted,
                true,
            );
        };
        let state = s.gcode_state.as_deref();
        match state {
            Some("RUNNING" | "PREPARE") => (
                format!(
                    "{name} · {} {:.0}%",
                    state_label(state),
                    s.mc_percent.unwrap_or(0.0)
                ),
                z.printer_green,
                true,
            ),
            Some("PAUSE") => (format!("{name} · {}", tr("пауза")), z.warning, true),
            Some("FAILED") => (format!("{name} · {}", tr("помилка")), z.error, false),
            _ => (
                format!("{name} · {}", state_label(state).to_lowercase()),
                z.success,
                false,
            ),
        }
    }
}

/// Модель для підписів; ручно доданий LAN-принтер може ще не мати коду моделі.
fn model_label(p: &cloud::Printer) -> String {
    if p.model.is_empty() {
        tr("модель невідома")
    } else {
        model_name(&p.model).to_owned()
    }
}

fn model_name(code: &str) -> &str {
    // Повна таблиця кодів живе в `lan` (DevModel з анонсів).
    ::lan::model_name(code)
}

fn state_name(state: Option<&str>) -> &'static str {
    match state {
        Some("IDLE") => "Очікує",
        Some("PREPARE") => "Підготовка",
        Some("SLICING") => "Нарізка",
        Some("RUNNING") => "Друкує",
        Some("PAUSE") => "Пауза",
        Some("FINISH") => "Завершено",
        Some("FAILED") => "Помилка",
        _ => "Невідомо",
    }
}

fn state_label(state: Option<&str>) -> String {
    tr(state_name(state))
}

fn cache_path() -> std::path::PathBuf {
    slicer::cache_dir().with_file_name("printers.json")
}

/// Останній успішний список принтерів акаунта (лише для цього користувача).
fn load_cache() -> Vec<cloud::Printer> {
    std::fs::read(cache_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_cache(list: &[cloud::Printer]) {
    use paths::fs_mode::ModeExt as _;
    let json: Vec<_> = list
        .iter()
        .map(|p| {
            serde_json::json!({
                "dev_id": p.dev_id.as_str(),
                "dev_name": p.name,
                "dev_model_name": p.model,
                "dev_online": p.online,
            })
        })
        .collect();
    let path = cache_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .unix_mode(0o600)
        .open(&path);
    if let Ok(file) = file {
        serde_json::to_writer(file, &json).ok();
    }
}

/// Робочий потік: підключення, підписка на вибраний принтер, пересилання статусу в UI.
/// Відбиток налаштування хмари: плагін і файл сесії. Зміна (вхід у майстрі записав сесію,
/// встановлено плагін) — привід підключитися знову без перезапуску.
fn setup_stamp() -> String {
    match cloud::setup() {
        cloud::CloudSetup::Ready { session, .. } => {
            let modified = std::fs::read_dir(&session.dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| e.metadata().ok()?.modified().ok())
                .max();
            format!("ready:{}:{modified:?}", session.dir.display())
        }
        cloud::CloudSetup::NoSession { .. } => "no-session".into(),
        cloud::CloudSetup::NoPlugin => "no-plugin".into(),
        cloud::CloudSetup::UnsupportedPlugin { version, .. } => format!("unsupported:{version}"),
    }
}

/// Робочий потік хмари. Після невдачі (немає плагіна чи сесії, сесія прострочена) не
/// завершується, а чекає: `Reconnect` від майстра або зміну плагіна/сесії на диску — і
/// підключається знову. Так вхід у майстрі діє одразу, без перезапуску Slicenv.
fn worker(
    commands: mpsc::Receiver<CloudCmd>,
    ui: async_mpsc::UnboundedSender<CloudMsg>,
    stopping: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::Ordering;
    loop {
        let stamp = setup_stamp();
        let restart = session(&commands, &ui, &stopping);
        if stopping.load(Ordering::Acquire) || ui.is_closed() {
            return;
        }
        if restart {
            continue;
        }
        loop {
            match commands.recv_timeout(Duration::from_secs(2)) {
                Ok(CloudCmd::Reconnect) => break,
                Ok(CloudCmd::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                // Без сесії команди нікуди не йдуть; інтерфейс уже показує помилку.
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if stopping.load(Ordering::Acquire) || ui.is_closed() {
                        return;
                    }
                    if setup_stamp() != stamp {
                        break;
                    }
                }
            }
        }
    }
}

/// Одна сесія хмари: плагін → вхід → сервер → принтери, поки не зупинять або не впаде.
fn session(
    commands: &mpsc::Receiver<CloudCmd>,
    ui: &async_mpsc::UnboundedSender<CloudMsg>,
    stopping: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> bool {
    let mut restart = false;
    let send = |msg| {
        ui.unbounded_send(msg).ok();
    };
    let result = (|| -> anyhow::Result<()> {
        if stopping.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(());
        }
        let mut options = cloud_options()?;
        // Команди надсилає лише користувач кнопками; зупинка — з підтвердженням.
        options.allow_control = true;
        // Плагін уже завантажено (його ставить майстер); для користувача все це — підключення.
        send(CloudMsg::Phase("Підключення до Bambu Cloud…".into()));
        probe_plugin(stopping)?;
        if stopping.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(());
        }
        let (client, mut events) = Client::load(options)?;
        client.start()?;
        let deadline = Instant::now() + Duration::from_secs(8);
        while !stopping.load(std::sync::atomic::Ordering::Acquire)
            && !client.is_user_login()?
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(100));
        }
        if stopping.load(std::sync::atomic::Ordering::Acquire) {
            return client.shutdown();
        }
        if !client.is_user_login()? {
            anyhow::bail!("Сесія Bambu недійсна або прострочена — увійдіть в акаунт Bambu знову.");
        }
        client.connect_server()?;
        let deadline = Instant::now() + Duration::from_secs(15);
        while !stopping.load(std::sync::atomic::Ordering::Acquire)
            && !client.is_server_connected()?
            && Instant::now() < deadline
        {
            let _ = events.recv_timeout(Duration::from_millis(100));
        }
        if stopping.load(std::sync::atomic::Ordering::Acquire) {
            return client.shutdown();
        }
        // Список принтерів іде HTTP-запитом із жорстким лімітом частоти: часті спроби (і
        // кожен запуск) продовжують відмову -11. Тому повтори рідкі, а список кешується —
        // статус іде MQTT, тож із кешем моніторинг працює й під час відмов.
        let mut printers = load_cache();
        let mut selected = printers
            .iter()
            .find(|p| p.online)
            .or(printers.first())
            .map(|p| p.dev_id.clone());
        if !printers.is_empty() {
            send(CloudMsg::Printers(printers.clone()));
        }
        if let Some(dev) = &selected {
            client.subscribe(dev)?;
            client.request_status(dev).ok();
        }
        let mut list_due = Some(Instant::now());
        let mut backoff = Duration::from_secs(60);
        send(CloudMsg::Phase("Підключено".into()));
        send_tasks(&client, &send);
        // OTA-версія потрібна для камери; її повідомляє сам принтер у відповідь на get_version.
        let mut firmware: std::collections::HashMap<String, String> = Default::default();
        let mut version_asked: Option<Instant> = None;
        let mut camera: Option<cloud::CameraStream<'_>> = None;
        let mut camera_pending: Option<Instant> = None;
        let mut last_pushall = Instant::now();
        // Перед запитом URL плагіну потрібні свіжі get_version і pushall з полем ipcam.
        let mut camera_version: Option<String> = None;
        let mut camera_ipcam = false;

        let mut last_status = Instant::now();
        // Останній знімок вибраного принтера: охоронець у робочому потоці перевіряє команду
        // за ним, а не за тим, що бачив інтерфейс у момент натискання.
        let mut seen: Option<(DeviceId, PrinterStatus, Instant)> = None;
        let mut linked = false;
        let mut link_checked = Instant::now() - Duration::from_secs(10);
        let mut heartbeat = Instant::now();
        loop {
            if stopping.load(std::sync::atomic::Ordering::Acquire) || ui.is_closed() {
                drop(camera); // joins the camera reader and closes its native handle first
                return client.shutdown();
            }
            if link_checked.elapsed() > Duration::from_secs(2) {
                link_checked = Instant::now();
                let now = client.is_server_connected().unwrap_or(false);
                if now != linked {
                    linked = now;
                    send(CloudMsg::Link(now));
                }
            }
            // Пасивний принтер шле стан лише за змінами; без пульсу «свіжість» руху не перевірити.
            if heartbeat.elapsed() > Duration::from_secs(10) {
                heartbeat = Instant::now();
                if let Some(dev) = &selected {
                    client.request_status(dev).ok();
                }
            }
            if list_due.is_some_and(|due| Instant::now() >= due) {
                match client.printers() {
                    Ok(list) => {
                        save_cache(&list);
                        if selected.is_none() {
                            selected = list.iter().find(|p| p.online).map(|p| p.dev_id.clone());
                            if let Some(dev) = &selected {
                                client.subscribe(dev)?;
                                client.request_status(dev).ok();
                            }
                        }
                        printers = list;
                        send(CloudMsg::Printers(printers.clone()));
                        send(CloudMsg::Phase("Підключено".into()));
                        list_due = None;
                        backoff = Duration::from_secs(60);
                    }
                    Err(err) => {
                        if paths::env("SLICENV_DEBUG").is_some() {
                            eprintln!("[cloud] printers(): {err:#}");
                        }
                        // Запасний шлях: принтери, на яких друкувались завдання акаунта.
                        if printers.is_empty()
                            && let Some(list) = printers_from_tasks(&client)
                        {
                            save_cache(&list);
                            printers = list;
                            send(CloudMsg::Printers(printers.clone()));
                            if selected.is_none()
                                && let Some(dev) = printers.first().map(|p| p.dev_id.clone())
                            {
                                client.subscribe(&dev)?;
                                client.request_status(&dev).ok();
                                selected = Some(dev);
                            }
                        }
                        send(CloudMsg::ListRetry {
                            secs: backoff.as_secs(),
                            cached: !printers.is_empty(),
                        });
                        list_due = Some(Instant::now() + backoff);
                        backoff = (backoff * 2).min(Duration::from_secs(900));
                    }
                }
            }
            while let Ok(cmd) = commands.try_recv() {
                match cmd {
                    CloudCmd::Shutdown => {
                        drop(camera);
                        return client.shutdown();
                    }
                    // Інший акаунт у майстрі: закриваємо цю сесію й підключаємося знову.
                    CloudCmd::Reconnect => {
                        drop(camera);
                        restart = true;
                        return client.shutdown();
                    }
                    CloudCmd::Tasks => send_tasks(&client, &send),
                    CloudCmd::Camera(on) => {
                        if let Some(mut cam) = camera.take() {
                            cam.stop().ok();
                        }
                        camera_pending = None;
                        // Як Studio: свіжий get_version безпосередньо перед запитом URL камери —
                        // зі старою відповіддю сервер не віддає URL (-10006).
                        if on && let Some(dev) = &selected {
                            client.request_version(dev).ok();
                            client.request_status(dev).ok();
                            camera_pending = Some(Instant::now());
                            camera_version = None;
                            camera_ipcam = false;
                        }
                    }
                    CloudCmd::Select(dev) => {
                        if let Some(mut cam) = camera.take() {
                            cam.stop().ok();
                        }
                        // Камера, що ще відкривалась для попереднього принтера, не має перейти
                        // на новий.
                        camera_pending = None;
                        seen = None;
                        send(CloudMsg::CameraStopped(None));
                        version_asked = None;
                        client.subscribe(&dev)?;
                        client.request_status(&dev).ok();
                        client.request_version(&dev).ok();
                        selected = Some(dev);
                    }
                    CloudCmd::Versions => {
                        if let Some(dev) = &selected {
                            client.request_version(dev).ok();
                        }
                    }
                    CloudCmd::Thumbnail(key) => {
                        // Хмарні запити — лише в цьому потоці; саме завантаження байтів іде
                        // окремо, щоб не блокувати MQTT.
                        if Some(&key.dev) != selected.as_ref() {
                            continue;
                        }
                        match client.get_subtask_info(&key.subtask_id) {
                            Ok(info) => match cloud::plate_thumbnail(info.as_value()) {
                                Some(url) => {
                                    let ui = ui.clone();
                                    std::thread::spawn(move || {
                                        let result = cloud::download_cover(&url)
                                            .and_then(|bytes| {
                                                Ok(image::load_from_memory(&bytes)?.to_rgba8())
                                            })
                                            .map(|mut rgba| {
                                                for px in rgba.pixels_mut() {
                                                    px.0.swap(0, 2);
                                                }
                                                rgba
                                            })
                                            .map_err(|e| format!("{e:#}"));
                                        ui.unbounded_send(CloudMsg::Thumbnail(key, result)).ok();
                                    });
                                }
                                None => send(CloudMsg::Thumbnail(
                                    key,
                                    Err(tr("Хмара не віддала мініатюру для цієї пластини")),
                                )),
                            },
                            Err(err) => send(CloudMsg::Thumbnail(key, Err(format!("{err:#}")))),
                        }
                    }
                    CloudCmd::Refresh => {
                        list_due = Some(Instant::now());
                        if let Some(dev) = &selected {
                            client.request_status(dev).ok();
                        }
                    }
                    CloudCmd::AddManual(dev) => {
                        if !printers.iter().any(|p| p.dev_id == dev) {
                            printers.push(cloud::Printer {
                                dev_id: dev.clone(),
                                name: String::new(),
                                model: "N2S".into(),
                                online: true,
                            });
                            save_cache(&printers);
                            send(CloudMsg::Printers(printers.clone()));
                        }
                        client.subscribe(&dev)?;
                        client.request_status(&dev).ok();
                        selected = Some(dev);
                    }
                    CloudCmd::Print(request) => {
                        // Блокує потік, поки плагін вивантажує файл у хмару.
                        let result = client
                            .start_print(&request)
                            .map(|_| {
                                let name = &request.task_name;
                                crate::tr_format!("Завдання «{name}» надіслано на принтер")
                            });
                        if result.is_ok() {
                            send(CloudMsg::CloudPrintWorks);
                        }
                        match result {
                            Err(e) if cloud::PluginError::code_of(&e) == Some(cloud::PluginError::UNSIGNED_SOFTWARE) => {
                                send(CloudMsg::PrintUnsigned)
                            }
                            result => send(CloudMsg::PrintSent(result.map_err(|e| print_failure(&e)))),
                        }
                        if let Some(dev) = &selected {
                            client.request_status(dev).ok();
                        }
                    }
                    CloudCmd::Control(dev, control) => {
                        let printer = printers.iter().find(|p| p.dev_id == dev);
                        let fresh = seen.as_ref().filter(|(d, ..)| *d == dev);
                        let snapshot = guard::Snapshot {
                            selected: selected.as_ref(),
                            printer,
                            status: fresh.map(|(_, status, _)| status),
                            age: fresh.map(|(.., at)| at.elapsed()),
                            connected: client.is_server_connected().unwrap_or(false),
                            lan: false,
                        };
                        match guard::check(&snapshot, &dev, control) {
                            Err(blocked) => send(CloudMsg::Blocked(blocked.message())),
                            Ok(ctx) => {
                                if let Err(err) = client.control_in(&dev, control, &ctx) {
                                    send(CloudMsg::Blocked(format!(
                                        "{}: {err:#}",
                                        tr("Команда не пройшла")
                                    )));
                                }
                            }
                        }
                    }
                }
            }
            if let Some(cam) = &mut camera {
                // Беремо найсвіжіший кадр із черги; старі відкидаємо.
                let mut last = None;
                while let Some(frame) = cam.try_frame() {
                    last = Some(frame);
                }
                if let Some(frame) = last
                    && let Ok(decoded) =
                        image::load_from_memory_with_format(&frame.jpeg, image::ImageFormat::Jpeg)
                {
                    let mut bgra = decoded.to_rgba8();
                    for px in bgra.pixels_mut() {
                        px.0.swap(0, 2);
                    }
                    send(CloudMsg::CameraFrame(bgra));
                }
            }
            match events.recv_timeout(Duration::from_millis(if camera.is_some() {
                50
            } else {
                250
            })) {
                Ok(Event::Status {
                    dev_id,
                    status,
                    patch,
                }) if Some(&dev_id) == selected.as_ref() => {
                    last_status = Instant::now();
                    seen = Some((dev_id.clone(), (*status).clone(), last_status));
                    // Перший pushall часто губиться, поки канал прогрівається (-2): без повного
                    // знімка немає стану друку, лише часткові оновлення температур.
                    if status.gcode_state.is_none()
                        && last_pushall.elapsed() > Duration::from_secs(10)
                    {
                        last_pushall = Instant::now();
                        client.request_status(&dev_id).ok();
                    }
                    if camera_pending.is_some() && patch.as_value()["print"].get("ipcam").is_some()
                    {
                        camera_ipcam = true;
                    }
                    send(CloudMsg::Status(dev_id.clone(), status));
                    if !firmware.contains_key(dev_id.as_str())
                        && version_asked.is_none_or(|t| t.elapsed() > Duration::from_secs(30))
                    {
                        version_asked = Some(Instant::now());
                        client.request_version(&dev_id).ok();
                    }
                }
                Ok(Event::PrinterMessage { dev_id, payload }) => {
                    let modules = cloud::module_versions(payload.as_value());
                    if !modules.is_empty() && Some(&dev_id) == selected.as_ref() {
                        send(CloudMsg::Versions(dev_id.clone(), modules));
                    }
                    if let Some(version) = cloud::ota_version(payload.as_value()) {
                        if camera_pending.is_some() && Some(&dev_id) == selected.as_ref() {
                            camera_version = Some(version.clone());
                        }
                        firmware.insert(dev_id.as_str().to_owned(), version);
                    }
                }
                Ok(Event::Lagged { .. }) => {
                    if let Some(dev) = &selected {
                        client.request_status(dev).ok();
                    }
                }
                _ => {}
            }
            if camera_pending.is_some()
                && camera_ipcam
                && let (Some(version), Some(dev)) = (camera_version.take(), selected.clone())
            {
                camera_pending = None;
                if paths::env("SLICENV_DEBUG").is_some() {
                    eprintln!("[cloud] камера: прошивка {version}");
                }
                match cloud::CameraStream::open_with_firmware(&client, &dev, &version) {
                    Ok(cam) => camera = Some(cam),
                    // Заголовок панелі вже «Камера» — без префікса.
                    Err(err) => send(CloudMsg::CameraStopped(Some(format!("{err:#}")))),
                }
            }
            if camera_pending.is_some_and(|t| t.elapsed() > Duration::from_secs(20)) {
                camera_pending = None;
                send(CloudMsg::CameraStopped(Some(tr(
                    "Принтер не відповів на запит версії прошивки",
                ))));
            }
            // Тиша довше хвилини — просимо повний знімок.
            if last_status.elapsed() > Duration::from_secs(60) {
                last_status = Instant::now();
                if let Some(dev) = &selected {
                    client.request_status(dev).ok();
                }
            }
            if ui.is_closed() {
                return Ok(());
            }
        }
    })();
    if let Err(err) = result {
        send(CloudMsg::Error(format!("{err:#}")));
    }
    restart
}

/// Плагін і сесія для хмари (`cloud::setup`): керований плагін → Bambu Studio → OrcaSlicer;
/// сесія власна → Bambu Studio → OrcaSlicer (чужі — лише копією). Помилки — текст для людини.
fn cloud_options() -> anyhow::Result<Options> {
    anyhow::ensure!(cloud::OS_SUPPORTED, "{}", crate::i18n::tr(cloud::UNSUPPORTED_OS));
    match cloud::setup() {
        cloud::CloudSetup::Ready { plugin, session } => Ok(Options::for_session(&plugin, &session)),
        cloud::CloudSetup::NoPlugin => anyhow::bail!(
            "Плагін Bambu не встановлено. Для Bambu Cloud потрібен закритий плагін від Bambu Lab — він завантажується з серверів Bambu Lab лише з вашої згоди."
        ),
        cloud::CloudSetup::UnsupportedPlugin { version, .. } => {
            let supported = cloud::SUPPORTED_PLUGINS;
            anyhow::bail!(crate::tr_format!(
                "Версія плагіна Bambu {version} не підтримується; підтримуються {supported}"
            ))
        }
        cloud::CloudSetup::NoSession { .. } => {
            anyhow::bail!("Увійдіть в акаунт Bambu: на цьому комп’ютері ще немає сесії Bambu.")
        }
    }
}

/// Аргумент, з яким застосунок запускає сам себе для перевірки плагіна.
pub const CLOUD_PROBE_ARG: &str = "--cloud-probe";

/// Дочірній процес `--cloud-probe`: той самий старт плагіна, що в робочому потоці, до
/// `connect_server` включно. Закритий плагін Bambu (упакований, без символів) уже падав
/// SIGSEGV усередині `connect_server`; у власному процесі це не зачіпає застосунок.
/// Звичайні помилки тут не важливі — їх покаже справжній робочий потік; важить лише аварія.
pub fn cloud_probe() -> i32 {
    let run = || -> anyhow::Result<()> {
        let (client, _events) = Client::load(cloud_options()?)?;
        client.start()?;
        let deadline = Instant::now() + Duration::from_secs(8);
        while !client.is_user_login()? && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        if client.is_user_login()? {
            client.connect_server()?;
            // Частина аварій буває у внутрішніх потоках плагіна вже після виклику.
            std::thread::sleep(Duration::from_secs(2));
        }
        client.shutdown()
    };
    let _ = run();
    0
}

/// Запускає `--cloud-probe` і чекає. Аварія дочірнього процесу — причина не вантажити
/// плагін у свій процес: інакше SIGSEGV плагіна вбиває весь застосунок.
fn probe_plugin(stopping: &std::sync::atomic::AtomicBool) -> anyhow::Result<()> {
    // Власний бінарник (не зовнішній процес): оточення AppImage залишаємо.
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .arg(CLOUD_PROBE_ARG)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(45);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if stopping.load(std::sync::atomic::Ordering::Acquire) || Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            if stopping.load(std::sync::atomic::Ordering::Acquire) {
                return Ok(());
            }
            anyhow::bail!("Плагін Bambu завис під час перевірки — хмару не запущено");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    if let Some(signal) = paths::host::exit_signal(&status) {
        anyhow::bail!(
            "Плагін Bambu аварійно завершився (сигнал {signal}) у перевірочному процесі — хмару вимкнено, щоб не впав застосунок. Причина в закритому плагіні, не в моделі."
        );
    }
    Ok(())
}

fn send_tasks(client: &Client, send: &impl Fn(CloudMsg)) {
    let tasks = match client.get_user_tasks(&cloud::TaskQuery::default()) {
        Ok(tasks) => tasks,
        Err(err) => {
            if paths::env("SLICENV_DEBUG").is_some() {
                eprintln!("[cloud] get_user_tasks: {err:#}");
            }
            return;
        }
    };
    let rows = tasks
        .into_iter()
        .map(|t| TaskRow {
            title: t.title.or(t.design_name).unwrap_or_else(|| tr("Без назви")),
            status: t.status,
            date: t
                .start_time
                .map(|s| s.chars().take(16).collect::<String>().replace('T', " "))
                .unwrap_or_default(),
            cost_minutes: t.cost_time.map(|c| (c / 60.0).round() as u64),
            weight: t.weight,
        })
        .collect();
    send(CloudMsg::Tasks(rows));
}

/// Принтери з історії завдань (коли endpoint списку відмовляє).
fn printers_from_tasks(client: &Client) -> Option<Vec<cloud::Printer>> {
    let tasks = client.get_user_tasks(&cloud::TaskQuery::default()).ok()?;
    let mut list: Vec<cloud::Printer> = Vec::new();
    for task in tasks {
        let Some(dev) = task.dev_id else { continue };
        if list.iter().any(|p| p.dev_id == dev) {
            continue;
        }
        let raw = task.raw.as_value();
        let text = |k: &str| raw[k].as_str().unwrap_or_default().to_owned();
        list.push(cloud::Printer {
            dev_id: dev,
            name: text("deviceName"),
            model: Some(text("deviceModel"))
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| "N2S".into()),
            online: true,
        });
    }
    (!list.is_empty()).then_some(list)
}

/// `SLICENV_DEBUG=1`: що отримує інтерфейс, без ідентифікаторів і довільного JSON.
fn debug_log(msg: &CloudMsg) {
    match msg {
        CloudMsg::Phase(p) => eprintln!("[cloud] {p}"),
        CloudMsg::ListRetry { secs, cached } => {
            eprintln!("[cloud] список принтерів: повтор через {secs} с (кеш: {cached})")
        }
        CloudMsg::Link(up) => eprintln!("[cloud] з’єднання: {up}"),
        CloudMsg::Versions(dev, list) => eprintln!("[cloud] {dev}: модулів {}", list.len()),
        CloudMsg::Thumbnail(_, r) => eprintln!("[cloud] мініатюра: {}", r.is_ok()),
        CloudMsg::Blocked(text) => eprintln!("[cloud] команду відхилено: {text}"),
        CloudMsg::Printers(list) => eprintln!("[cloud] принтерів: {}", list.len()),
        CloudMsg::Status(dev, s) => eprintln!(
            "[cloud] {dev}: {} {:?}% шар {:?}/{:?} сопло {:?} стіл {:?}",
            state_name(s.gcode_state.as_deref()),
            s.mc_percent,
            s.layer_num,
            s.total_layer_num,
            s.nozzle_temper,
            s.bed_temper
        ),
        CloudMsg::Error(e) => eprintln!("[cloud] помилка: {e}"),
        CloudMsg::PrintSent(r) => eprintln!("[cloud] друк: {r:?}"),
        CloudMsg::PrintUnsigned => eprintln!("[cloud] друк: плагін відмовив непідписаній програмі (-26)"),
        CloudMsg::CloudPrintWorks => eprintln!("[cloud] друк: хмара прийняла завдання"),
        CloudMsg::Tasks(t) => eprintln!("[cloud] історія: {} завдань", t.len()),
        CloudMsg::CameraFrame(f) => eprintln!("[cloud] кадр {}×{}", f.width(), f.height()),
        CloudMsg::CameraStopped(e) => eprintln!("[cloud] камера зупинена: {e:?}"),
        CloudMsg::Lan(m) => eprintln!("{}", m.describe()),
        CloudMsg::Klipper(m) => eprintln!("{}", m.describe()),
    }
}

/// Канали робочого потоку хмари; потік стартує до GPUI, бо плагін Bambu ініціалізує
/// мережу (curl/TLS) некоректно, якщо в процесі вже живуть потоки GPUI.
pub struct CloudLink {
    commands: mpsc::Sender<CloudCmd>,
    messages: async_mpsc::UnboundedReceiver<CloudMsg>,
}

pub fn debug_drain(link: CloudLink) {
    let mut rx = link.messages;
    let commands = link.commands;
    // SLICENV_CAMERA_TEST: увімкнути камеру через 15 с (діагностика без інтерфейсу).
    if paths::env("SLICENV_CAMERA_TEST").is_some() {
        let commands = commands.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(15));
            commands.send(CloudCmd::Camera(true)).ok();
            std::thread::sleep(Duration::from_secs(600));
        });
    }
    futures::executor::block_on(async move {
        while let Some(msg) = rx.next().await {
            debug_log(&msg);
        }
    });
}

/// The guard belongs to main, outside GPUI: dropping the window must not detach
/// native SDK workers into process-wide library/TLS cleanup.
pub struct CloudShutdown {
    commands: mpsc::Sender<CloudCmd>,
    stopping: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Drop for CloudShutdown {
    fn drop(&mut self) {
        self.stopping
            .store(true, std::sync::atomic::Ordering::Release);
        let _ = self.commands.send(CloudCmd::Shutdown);
        if let Some(worker) = self.worker.take() {
            if worker.join().is_err() {
                eprintln!("Потік хмари завершився з помилкою під час закриття");
            }
        }
    }
}

/// Синтетичний знімок для `SLICENV_FAKE_PRINTER`; жодного зв’язку з реальним принтером.
fn fake_status(idle: bool) -> PrinterStatus {
    PrinterStatus {
        gcode_state: Some(if idle { "IDLE" } else { "RUNNING" }.into()),
        mc_percent: Some(if idle { 0.0 } else { 45.0 }),
        mc_remaining_time: Some(83.0),
        layer_num: Some(112),
        total_layer_num: Some(248),
        nozzle_temper: Some(if idle { 31.0 } else { 219.6 }),
        nozzle_target_temper: Some(if idle { 0.0 } else { 220.0 }),
        bed_temper: Some(if idle { 27.5 } else { 64.8 }),
        bed_target_temper: Some(if idle { 0.0 } else { 65.0 }),
        print_error: Some(0),
        spd_lvl: Some(2),
        wifi_signal: Some("-48dBm".into()),
        vt_tray: Some(cloud::Tray {
            tray_type: Some("PLA".into()),
            tray_color: Some("00AE42FF".into()),
            ..Default::default()
        }),
        fans: cloud::Fans {
            part: Some(if idle { 0 } else { 80 }),
            heatbreak: Some(100),
            ..Default::default()
        },
        // Біти 0..2 — осі вдома, 8 — SD-карта працює.
        home_flag: Some(0x107),
        hms: if idle {
            Vec::new()
        } else {
            vec![cloud::HmsItem {
                attr: 0x0C00_0200,
                code: 0x0004_0001,
            }]
        },
        print_type: Some(if idle { "idle" } else { "cloud" }.into()),
        subtask_name: (!idle).then(|| "DoorD".into()),
        subtask_id: (!idle).then(|| "FAKE-SUBTASK-1".into()),
        project_id: (!idle).then(|| "FAKE-PROJECT".into()),
        profile_id: (!idle).then(|| "FAKE-PROFILE".into()),
        task_id: (!idle).then(|| "FAKE-TASK-1".into()),
        gcode_file: (!idle).then(|| "Metadata/plate_1.gcode".into()),
        upgrade: Some(cloud::UpgradeState {
            status: Some("IDLE".into()),
            display_state: Some(0),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Синтетична «мініатюра» (BGRA): діагональний градієнт зі смугами.
fn fake_cover() -> image::RgbaImage {
    image::RgbaImage::from_fn(160, 160, |x, y| {
        let stripe = ((x + y) / 16) % 2 == 0;
        let base = (60 + (x + y) / 3) as u8;
        // BGRA: блакитно-зелений тон.
        image::Rgba([base, if stripe { 170 } else { 140 }, 40, 255])
    })
}

pub fn spawn_cloud() -> (CloudLink, CloudShutdown) {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (ui_tx, ui_rx) = async_mpsc::unbounded();
    let stopping = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    // Для тестових запусків інтерфейсу: без другого агента поруч із робочим.
    if paths::env("SLICENV_CLOUD_OFF").is_some() {
        ui_tx
            .unbounded_send(CloudMsg::Phase("Хмару вимкнено (SLICENV_CLOUD_OFF)".into()))
            .ok();
        // Фіктивний принтер для перевірки інтерфейсу; команди до нього нікуди не йдуть.
        if paths::env("SLICENV_FAKE_PRINTER").is_some() {
            let fake = cloud::Printer {
                dev_id: DeviceId::new("TEST00000000"),
                name: crate::i18n::tr("Тестовий A1 (фіктивний)"),
                model: "N2S".into(),
                online: true,
            };
            let dev = fake.dev_id.clone();
            ui_tx.unbounded_send(CloudMsg::Printers(vec![fake])).ok();
            // Правдоподібний знімок — щоб бачити сторінку без справжнього принтера.
            // `SLICENV_FAKE_PRINTER=idle` дає вільний принтер (рух головки доступний).
            let idle = paths::env("SLICENV_FAKE_PRINTER").is_some_and(|v| v == "idle");
            let status = fake_status(idle);
            ui_tx.unbounded_send(CloudMsg::Link(true)).ok();
            ui_tx
                .unbounded_send(CloudMsg::Status(dev.clone(), Box::new(status.clone())))
                .ok();
            if let Some(key) = job::job_key(&dev, &status) {
                ui_tx
                    .unbounded_send(CloudMsg::Thumbnail(key, Ok(fake_cover())))
                    .ok();
            }
            ui_tx
                .unbounded_send(CloudMsg::Versions(
                    dev.clone(),
                    vec![
                        ModuleVersion {
                            name: "ota".into(),
                            software: "01.07.00.00".into(),
                            hardware: String::new(),
                            product: "Bambu Lab A1".into(),
                        },
                        ModuleVersion {
                            name: "esp32".into(),
                            software: "00.00.18.50".into(),
                            hardware: "AP05".into(),
                            product: String::new(),
                        },
                    ],
                ))
                .ok();
            // Пульс, як у справжнього робочого потоку: без нього знімок «застаріє».
            let beat = ui_tx.clone();
            std::thread::spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_secs(5));
                    if beat
                        .unbounded_send(CloudMsg::Status(dev.clone(), Box::new(fake_status(idle))))
                        .is_err()
                    {
                        break;
                    }
                }
            });
            let task = |title: &str, status: i32, date: &str, minutes: u64, weight: f64| TaskRow {
                title: title.into(),
                status: Some(status),
                date: date.into(),
                cost_minutes: Some(minutes),
                weight: Some(weight),
            };
            ui_tx
                .unbounded_send(CloudMsg::Tasks(vec![
                    task("DoorD", 4, "2026-10-30 09:12", 135, 42.7),
                    task("120mm Fan Mesh", 2, "2026-10-29 21:40", 22, 5.1),
                    task("angel_medium_Body", 3, "2026-10-29 14:05", 318, 96.4),
                ]))
                .ok();
        }
        drop(cmd_rx);
        let guard = CloudShutdown {
            commands: cmd_tx.clone(),
            stopping,
            worker: None,
        };
        return (
            CloudLink {
                commands: cmd_tx,
                messages: ui_rx,
            },
            guard,
        );
    }
    let stop = stopping.clone();
    let worker = std::thread::Builder::new()
        .name("bambu-cloud".into())
        .stack_size(16 << 20)
        .spawn(move || worker(cmd_rx, ui_tx, stop))
        .expect("не стартував потік хмари");
    let guard = CloudShutdown {
        commands: cmd_tx.clone(),
        stopping,
        worker: Some(worker),
    };
    (
        CloudLink {
            commands: cmd_tx,
            messages: ui_rx,
        },
        guard,
    )
}

impl Workspace {
    pub fn start_cloud(&mut self, link: CloudLink, window: &mut Window, cx: &mut Context<Self>) {
        let CloudLink {
            commands: cmd_tx,
            messages: ui_rx,
        } = link;
        // LAN живе поруч і не залежить від хмари (зокрема при SLICENV_CLOUD_OFF).
        let lan_rx = self.start_lan(window, cx);
        let klipper_rx = self.start_klipper(window, cx);
        let mut ui_rx = futures::stream::select(futures::stream::select(ui_rx, lan_rx), klipper_rx);
        self.cloud.commands = Some(cmd_tx);
        self.cloud.phase = tr("Запуск…").into();
        self.cloud.jog_step = 1;
        let input = cx
            .new(|cx| InputState::new(window, cx).placeholder(tr("Серійний номер, напр. 03919C…")));
        let sub = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &gpui_component::input::InputEvent, window, cx| {
                if matches!(event, gpui_component::input::InputEvent::PressEnter { .. }) {
                    this.add_manual_printer(window, cx);
                }
            },
        );
        self._subscriptions.push(sub);
        self.cloud.serial_input = Some(input);

        let mut field = |kind: DeviceField, placeholder: &str| {
            let entity = cx.new(|cx| InputState::new(window, cx).placeholder(tr(placeholder)));
            let sub = cx.subscribe_in(
                &entity,
                window,
                move |this, _, event: &gpui_component::input::InputEvent, window, cx| {
                    if matches!(event, gpui_component::input::InputEvent::PressEnter { .. }) {
                        this.apply_device_field(kind, window, cx);
                    }
                },
            );
            self._subscriptions.push(sub);
            let mut spec = crate::scrub::Spec::unit(1.0);
            spec.integer = true;
            spec.min = 0.;
            spec.max = match kind {
                DeviceField::Nozzle => 300.,
                DeviceField::Bed => 100.,
                _ => 100.,
            };
            self.bind_scrub(&entity, spec, crate::scrub::Target::Text);
            entity
        };
        self.cloud.inputs = Some(ControlInputs {
            nozzle: field(DeviceField::Nozzle, "°C"),
            bed: field(DeviceField::Bed, "°C"),
            part_fan: field(DeviceField::PartFan, "%"),
            aux_fan: field(DeviceField::AuxFan, "%"),
            chamber_fan: field(DeviceField::ChamberFan, "%"),
        });
        cx.spawn_in(window, async move |this, cx| {
            while let Some(msg) = ui_rx.next().await {
                let alive = this
                    .update_in(cx, |this, window, cx| {
                        let cloud = &mut this.cloud;
                        if paths::env("SLICENV_DEBUG").is_some() {
                            debug_log(&msg);
                        }
                        let mut status_for = None;
                        match msg {
                            CloudMsg::Phase(p) => {
                                cloud.connecting = p.starts_with("Підключення");
                                cloud.phase = tr(&p).into();
                            }
                            CloudMsg::ListRetry { secs, cached } => {
                                let mut text = crate::tr_format!(
                                    "Хмара не віддала список принтерів, повтор через {secs} с"
                                );
                                if cached {
                                    text += &tr(" (показую збережений)");
                                }
                                cloud.phase = text.into();
                            }
                            CloudMsg::Link(up) => cloud.connected = up,
                            CloudMsg::Printers(list) => {
                                if cloud.selected.is_none() {
                                    cloud.selected =
                                        list.iter().find(|p| p.online).map(|p| p.dev_id.clone());
                                }
                                cloud.lan.set_cloud_list(list);
                                cloud.rebuild_printers();
                            }
                            CloudMsg::Lan(msg) => lan::handle(this, msg, window, cx),
                            CloudMsg::Klipper(msg) => klipper::handle(this, msg, window, cx),
                            CloudMsg::Status(dev, status) => {
                                if cloud.selected.as_ref() == Some(&dev) {
                                    cloud.status = Some(*status);
                                    cloud.updated = Some(Instant::now());
                                    status_for = Some(dev);
                                }
                            }
                            CloudMsg::Versions(dev, list) => {
                                if cloud.selected.as_ref() == Some(&dev) {
                                    cloud.versions = list;
                                }
                            }
                            CloudMsg::Thumbnail(key, result) => {
                                let result = result.map(|rgba| {
                                    Arc::new(gpui::RenderImage::new(vec![image::Frame::new(rgba)]))
                                });
                                if let Some(stale) = cloud.job.accept(&key, result) {
                                    window.drop_image(stale).ok();
                                }
                            }
                            CloudMsg::Error(e) => {
                                cloud.connecting = false;
                                cloud.phase = tr("Хмара недоступна").into();
                                cloud.error = Some(tr(&e).into());
                            }
                            CloudMsg::Blocked(text) => this.toast(ToastKind::Error, text, cx),
                            CloudMsg::PrintUnsigned => {
                                cloud.sending = false;
                                cloud.cloud_print_works = false;
                                this.open_unsigned_print_dialog(window, cx);
                            }
                            CloudMsg::CloudPrintWorks => cloud.cloud_print_works = true,
                            CloudMsg::PrintSent(result) => {
                                cloud.sending = false;
                                cloud.klipper.upload = None;
                                match result {
                                    Ok(text) => this.toast(ToastKind::Success, text, cx),
                                    Err(text) => this.toast(ToastKind::Error, text, cx),
                                }
                            }
                            CloudMsg::Tasks(rows) => cloud.tasks = rows,
                            CloudMsg::CameraFrame(bgra) => {
                                if cloud.camera_on {
                                    let frame =
                                        Arc::new(gpui::RenderImage::new(vec![image::Frame::new(
                                            bgra,
                                        )]));
                                    if let Some(old) = cloud.camera_frame.replace(frame) {
                                        window.drop_image(old).ok();
                                    }
                                    cloud.camera_note = None;
                                }
                            }
                            CloudMsg::CameraStopped(note) => {
                                cloud.camera_on = false;
                                cloud.camera_note = note.map(Into::into);
                                if let Some(old) = cloud.camera_frame.take() {
                                    window.drop_image(old).ok();
                                }
                            }
                        }
                        if let Some(dev) = status_for {
                            this.after_status(&dev, window, cx);
                        }
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    /// Єдиний вхід для команд з інтерфейсу. Охоронець у `CloudState::dispatch` спрацьовує
    /// незалежно від того, чи кнопку намальовано вимкненою.
    pub(super) fn control(&mut self, control: Control, cx: &mut Context<Self>) {
        if let Err(blocked) = self.cloud.dispatch(control) {
            self.toast(ToastKind::Error, blocked.message(), cx);
        }
        cx.notify();
    }

    /// Підтвердження дії у власній модалці: на відміну від `confirm`, тіло перекладається.
    pub(super) fn confirm_action(
        &mut self,
        title: &'static str,
        body: &'static str,
        ok_label: &'static str,
        ok_icon: IconName,
        on_ok: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let on_ok = Rc::new(on_ok);
        self.open_modal(
            Rc::new(move |_, _, cx| {
                let on_ok = on_ok.clone();
                crate::widgets::modal(
                    title,
                    440.,
                    div()
                        .p(px(12.))
                        .text_sm()
                        .text_color(zed().text_muted)
                        .child(tr(body)),
                    Some(
                        div()
                            .flex()
                            .gap(px(6.))
                            .child(
                                ui::ghost_button("device-confirm-cancel", None, "Скасувати")
                                    .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))),
                            )
                            .child(
                                ui::danger_button("device-confirm-ok", ok_icon, ok_label).on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        this.close_modal(cx);
                                        on_ok(this, cx);
                                    }),
                                ),
                            )
                            .into_any_element(),
                    ),
                )
            }),
            window,
            cx,
        );
    }

    pub(super) fn confirm_stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_action(
            "Зупинити друк?",
            "Поточне завдання буде скасовано. Продовжити його потім не вийде.",
            "Зупинити",
            IconName::SquareStop,
            |ws, cx| ws.control(Control::Stop, cx),
            window,
            cx,
        );
    }

    /// Застосовує значення з поля вводу (Enter): розбір тут, межі — в охоронці.
    fn apply_device_field(
        &mut self,
        kind: DeviceField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(inputs) = &self.cloud.inputs else {
            return;
        };
        let entity = match kind {
            DeviceField::Nozzle => &inputs.nozzle,
            DeviceField::Bed => &inputs.bed,
            DeviceField::PartFan => &inputs.part_fan,
            DeviceField::AuxFan => &inputs.aux_fan,
            DeviceField::ChamberFan => &inputs.chamber_fan,
        };
        let text = entity.read(cx).value().trim().to_owned();
        let Ok(value) = text.parse::<u16>() else {
            self.toast(ToastKind::Error, tr("Введіть ціле невід’ємне число"), cx);
            return;
        };
        let fan = |kind, value: u16| {
            u8::try_from(value)
                .ok()
                .filter(|v| *v <= 100)
                .map(|v| Control::Fan(kind, v))
        };
        let control = match kind {
            DeviceField::Nozzle => Some(Control::Nozzle(value)),
            DeviceField::Bed => Some(Control::Bed(value)),
            DeviceField::PartFan => fan(cloud::FanKind::Part, value),
            DeviceField::AuxFan => fan(cloud::FanKind::Aux, value),
            DeviceField::ChamberFan => fan(cloud::FanKind::Chamber, value),
        };
        let Some(control) = control else {
            self.toast(ToastKind::Error, tr("Вентилятор: 0–100 %"), cx);
            return;
        };
        let printing = self
            .cloud
            .status
            .as_ref()
            .is_some_and(|s| job::is_active(s.gcode_state.as_deref()));
        match control {
            // Вимкнення нагріву посеред друку зупиняє його помилкою — питаємо.
            Control::Nozzle(0) | Control::Bed(0) if printing => self.confirm_action(
                "Вимкнути нагрів під час друку?",
                "Друк зупиниться з помилкою, коли температура впаде нижче потрібної.",
                "Вимкнути",
                IconName::Thermometer,
                move |ws, cx| ws.control(control, cx),
                window,
                cx,
            ),
            _ => self.control(control, cx),
        }
    }

    /// Після кожного знімка вибраного принтера: мініатюра завдання, поля вводу, камера.
    fn after_status(&mut self, dev: &DeviceId, window: &mut Window, cx: &mut Context<Self>) {
        let device_page = self.page == crate::workspace::Page::Device;
        let cloud = &mut self.cloud;
        if let Some(status) = &cloud.status {
            let observed = cloud.job.observe(dev, status);
            if let Some(stale) = observed.release {
                window.drop_image(stale).ok();
            }
            if let Some(key) = observed.fetch {
                cloud.send(CloudCmd::Thumbnail(key));
            }
        }
        // Камера на сторінці «Статус» стартує сама один раз на принтер; вимкнену
        // користувачем потім не вмикаємо.
        let online = cloud.printers.iter().any(|p| &p.dev_id == dev && p.online);
        // Klipper: знімки йдуть з власного принтера по HTTP, без хмари.
        let link = if cloud.klipper.has(dev) {
            cloud.klipper.is_up(dev)
        } else {
            cloud.connected && paths::env("SLICENV_CLOUD_OFF").is_none()
        };
        if device_page
            && cloud.tab == DeviceTab::Status
            && online
            && link
            && !cloud.camera_on
            && cloud.camera_auto.as_ref() != Some(dev)
        {
            cloud.camera_auto = Some(dev.clone());
            cloud.camera_on = true;
            cloud.camera_note = Some(tr("Підключаюсь до камери…").into());
            cloud.send(CloudCmd::Camera(true));
        }
        self.sync_inputs(window, cx);
    }

    fn sync_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(inputs), Some(s)) = (&self.cloud.inputs, &self.cloud.status) else {
            return;
        };
        let temp = |v: Option<f64>| v.map(|t| format!("{t:.0}")).unwrap_or_default();
        let fan = |v: Option<u8>| v.map(|f| f.to_string()).unwrap_or_default();
        let pairs = [
            (inputs.nozzle.clone(), temp(s.nozzle_target_temper)),
            (inputs.bed.clone(), temp(s.bed_target_temper)),
            (inputs.part_fan.clone(), fan(s.fans.part)),
            (inputs.aux_fan.clone(), fan(s.fans.aux)),
            (inputs.chamber_fan.clone(), fan(s.fans.chamber)),
        ];
        for (entity, text) in pairs {
            // Не перебиваємо те, що користувач зараз набирає.
            if entity.focus_handle(cx).is_focused(window) || entity.read(cx).value() == text {
                continue;
            }
            entity.update(cx, |state, cx| state.set_value(text, window, cx));
        }
    }
}

impl Workspace {
    fn add_manual_printer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = &self.cloud.serial_input else {
            return;
        };
        let serial = input.read(cx).value().trim().to_uppercase();
        if !(8..=24).contains(&serial.len()) || !serial.chars().all(|c| c.is_ascii_alphanumeric()) {
            self.toast(
                ToastKind::Error,
                tr("Серійний номер — 8–24 латинські літери й цифри"),
                cx,
            );
            return;
        }
        input.update(cx, |state, cx| state.set_value("", window, cx));
        let dev = DeviceId::new(serial);
        self.cloud.select(Some(dev.clone()), window);
        self.cloud.send(CloudCmd::AddManual(dev));
        cx.notify();
    }
}

pub fn render(
    ws: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let left = printers_panel(&ws.cloud, window, cx);
    let center = pages::main_pane(ws, window, cx);
    crate::prepare::docked("device-docks", ws, Some(left), center, None)
}

/// Лівий док: принтери акаунта зі станом і ручне додавання за серійним номером.
fn printers_panel(
    cloud: &CloudState,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui::AnyElement {
    let z = zed();
    let rows = cloud.printers.iter().enumerate().map(|(i, p)| {
        let selected = cloud.selected.as_ref() == Some(&p.dev_id);
        let dev = p.dev_id.clone();
        let name = if p.name.is_empty() {
            format!("{} {}", tr("Принтер"), p.dev_id)
        } else {
            p.name.clone()
        };
        let badge = if cloud.klipper.has(&p.dev_id) {
            Some("Klipper")
        } else if cloud.lan.has(&p.dev_id) {
            Some("LAN")
        } else {
            None
        };
        let (state, color, live) = if selected && p.online {
            cloud.summary()
        } else if p.online {
            (
                format!("{} · {}", model_label(p), tr("онлайн")),
                z.success,
                false,
            )
        } else {
            (
                format!("{} · {}", model_label(p), tr("офлайн")),
                z.text_placeholder,
                false,
            )
        };
        ui::list_item(("printer", i), 0, selected)
            .h(px(44.))
            .gap(px(8.))
            .child(ui::icon(IconName::Printer).text_color(if selected {
                z.accent
            } else {
                z.text_muted
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(div().truncate().child(name))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_xs()
                            .text_color(z.text_muted)
                            .child(ui::status_dot(color, live))
                            .child(div().truncate().child(state)),
                    ),
            )
            .when_some(badge, |d, badge| {
                d.child(
                    div()
                        .flex_none()
                        .px(px(4.))
                        .rounded(px(3.))
                        .border_1()
                        .border_color(z.border_variant)
                        .text_xs()
                        .text_color(z.text_muted)
                        .child(badge),
                )
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                // Повторний клік по вибраному принтеру нічого не скидає.
                if this.cloud.selected.as_ref() != Some(&dev) {
                    this.cloud.select(Some(dev.clone()), window);
                    this.cloud.send(CloudCmd::Select(dev.clone()));
                }
                cx.notify();
            }))
    });

    ui::dock()
        .child(ui::panel_header(
            "Принтери",
            vec![
                ui::icon_button("cloud-refresh", IconName::RefreshCw, "Оновити список")
                    .on_click(cx.listener(|this, _, _, _| this.cloud.send(CloudCmd::Refresh)))
                    .into_any_element(),
            ],
        ))
        .child(
            div()
                .id("printers-list")
                .flex_1()
                .min_h_0()
                .py(px(4.))
                .overflow_y_scroll()
                .children(rows)
                .when(cloud.printers.is_empty(), |d| {
                    d.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .p(px(10.))
                            .text_sm()
                            .text_color(z.text_muted)
                            .when(cloud.error.is_none(), |d| {
                                d.child(ui::spinner("printers-spin"))
                            })
                            .child(div().flex_1().min_w_0().child(if cloud.error.is_some() {
                                tr("Принтерів ще немає — додайте свій у блоці «Локальна мережа» нижче.")
                            } else {
                                tr(&cloud.phase)
                            })),
                    )
                }),
        )
        .child(lan::panel_section(cloud, cx))
        .child(klipper::panel_section(cloud, cx))
        // Ручне додавання за серійником — хмарне; без робочої хмари воно лише заважає.
        .children(cloud.serial_input.as_ref().filter(|_| cloud.error.is_none()).map(|input| {
            div()
                .flex()
                .flex_col()
                .flex_none()
                .gap(px(6.))
                .p(px(10.))
                .border_t_1()
                .border_color(z.border_variant)
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(z.text_muted)
                        .child(tr("Додати за серійним номером")),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(4.))
                        .child(crate::widgets::field(
                            input,
                            gpui_component::input::Input::new(input),
                            window,
                            cx,
                        ))
                        .child(
                            ui::icon_button("add-printer", IconName::Plus, "Додати").on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.add_manual_printer(window, cx)
                                }),
                            ),
                        ),
                )
                .child(div().text_xs().text_color(z.text_placeholder).child(tr(
                    "Серійний номер — на екрані A1 (Налаштування → Пристрій) або в Bambu Studio.",
                )))
        }))
        .into_any_element()
}

/// Картка з тонкою межею й заголовком — як групи в налаштуваннях Zed.
pub(self) fn card(title: &str, trailing: Vec<gpui::AnyElement>) -> gpui::Div {
    let z = zed();
    div()
        .flex()
        .flex_col()
        .rounded(px(6.))
        .border_1()
        .border_color(z.border_variant)
        .bg(z.panel)
        .overflow_hidden()
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .h(px(32.))
                .pl(px(10.))
                .pr(px(4.))
                .border_b_1()
                .border_color(z.border_variant)
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(tr(title)),
                )
                .children(trailing),
        )
}

pub(self) const CAMERA_HEIGHT: f32 = 420.;

pub(self) fn camera_panel(cloud: &CloudState, cx: &mut Context<Workspace>) -> gpui::Div {
    let z = zed();
    let on = cloud.camera_on;
    let toggle = ui::ghost_button(
        "camera-toggle",
        Some(if on {
            IconName::VideoOff
        } else {
            IconName::Video
        }),
        if on {
            "Зупинити"
        } else {
            "Увімкнути"
        },
    )
    .text_xs()
    .on_click(cx.listener(move |this, _, _, cx| {
        this.cloud.camera_on = !on;
        this.cloud.camera_note = (!on).then(|| tr("Підключаюсь до камери…").into());
        this.cloud.send(CloudCmd::Camera(!on));
        cx.notify();
    }));
    let frame = cloud.camera_frame.clone().filter(|_| on);
    card("Камера", vec![toggle.into_any_element()])
        .flex_1()
        .min_w(px(360.))
        .child(
            div()
                .relative()
                .w_full()
                .h(px(CAMERA_HEIGHT))
                .flex()
                .items_center()
                .justify_center()
                .bg(z.editor)
                .map(|d| match frame {
                    Some(frame) => d.child(
                        gpui::img(frame)
                            .size_full()
                            .object_fit(gpui::ObjectFit::Contain),
                    ),
                    None => d.child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(8.))
                            .text_sm()
                            .text_color(z.text_muted)
                            .child(if on {
                                ui::spinner("camera-spin")
                            } else {
                                gpui_component::Icon::new(IconName::Camera)
                                    .size(px(28.))
                                    .text_color(z.text_placeholder)
                                    .into_any_element()
                            })
                            .child(
                                // Довга порада переноситься в рядки, а не тече за краї панелі.
                                div()
                                    .max_w(px(560.))
                                    .px(px(16.))
                                    .text_center()
                                    .whitespace_normal()
                                    .child(
                                        cloud
                                            .camera_note
                                            .clone()
                                            .unwrap_or_else(|| tr("Камера вимкнена").into()),
                                    ),
                            ),
                    ),
                })
                .when(on && cloud.camera_frame.is_some(), |d| {
                    d.child(
                        div()
                            .absolute()
                            .top(px(8.))
                            .left(px(8.))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .px(px(6.))
                            .py(px(2.))
                            .rounded(px(4.))
                            .bg(gpui::black().opacity(0.55))
                            .text_xs()
                            .child(ui::status_dot(z.error, true))
                            .child(tr("наживо")),
                    )
                }),
        )
}

pub(self) fn history_panel(cloud: &CloudState, cx: &mut Context<Workspace>) -> gpui::Div {
    let z = zed();
    let status = |s: Option<i32>| match s {
        Some(1 | 4) => (tr("друкується"), z.accent),
        Some(2) => (tr("успіх"), z.success),
        Some(3) => (tr("невдача"), z.error),
        _ => ("—".to_owned(), z.text_placeholder),
    };
    let refresh = ui::icon_button("tasks-refresh", IconName::RefreshCw, "Оновити історію")
        .on_click(cx.listener(|this, _, _, _| this.cloud.send(CloudCmd::Tasks)));
    let cell = |w: f32| div().w(px(w)).flex_none().truncate();
    card("Історія друку", vec![refresh.into_any_element()])
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .h(px(28.))
                .px(px(10.))
                .text_xs()
                .text_color(z.text_placeholder)
                .border_b_1()
                .border_color(z.border_variant)
                .child(cell(130.).child(tr("Дата")))
                .child(div().flex_1().child(tr("Модель")))
                .child(cell(110.).child(tr("Результат")))
                .child(cell(90.).child(tr("Час")))
                .child(cell(60.).child(tr("Вага"))),
        )
        .when(cloud.tasks.is_empty(), |d| {
            d.child(
                div()
                    .p(px(10.))
                    .text_sm()
                    .text_color(z.text_muted)
                    .child(tr("Немає даних")),
            )
        })
        .children(cloud.tasks.iter().enumerate().map(|(i, t)| {
            let (label, color) = status(t.status);
            div()
                .id(("task", i))
                .flex()
                .items_center()
                .gap(px(12.))
                .h(px(30.))
                .px(px(10.))
                .text_sm()
                .hover(|s| s.bg(z.element_hover))
                .when(i > 0, |d| d.border_t_1().border_color(z.border_variant))
                .child(
                    cell(130.)
                        .font_family(crate::theme::MONO_FONT)
                        .text_xs()
                        .text_color(z.text_muted)
                        .child(t.date.clone()),
                )
                .child(div().flex_1().min_w_0().truncate().child(t.title.clone()))
                .child(
                    cell(110.)
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_xs()
                        .text_color(color)
                        .child(div().size(px(6.)).rounded_full().bg(color))
                        .child(label),
                )
                .child(
                    cell(90.)
                        .text_xs()
                        .text_color(z.text_muted)
                        .child(t.cost_minutes.map(duration_text).unwrap_or_default()),
                )
                .child(
                    cell(60.).text_xs().text_color(z.text_muted).child(
                        t.weight
                            .map(|w| crate::tr_format!("{w:.1} г"))
                            .unwrap_or_default(),
                    ),
                )
        }))
}

/// «1 год 23 хв» / «45 хв»; шаблони перекладаються цілком.
pub(self) fn duration_text(minutes: u64) -> String {
    let (h, m) = (minutes / 60, minutes % 60);
    if h > 0 {
        crate::tr_format!("{h} год {m:02} хв")
    } else {
        crate::tr_format!("{m} хв")
    }
}

#[allow(dead_code)]
fn temp(current: Option<f64>, target: Option<f64>) -> String {
    match (current, target) {
        (Some(c), Some(t)) if t > 0.0 => format!("{c:.0} / {t:.0} °C"),
        (Some(c), _) => format!("{c:.0} °C"),
        _ => "—".into(),
    }
}

/// Тип пластини в термінах завдання друку ("textured_plate"…) з нарізаного 3MF.
fn plate_bed_type(gcode_3mf: &std::path::Path, plate: u32, fallback: &str) -> String {
    use std::io::Read as _;
    let from_archive = (|| {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(gcode_3mf).ok()?).ok()?;
        let mut text = String::new();
        zip.by_name(&format!("Metadata/plate_{plate}.json"))
            .ok()?
            .read_to_string(&mut text)
            .ok()?;
        let json: serde_json::Value = serde_json::from_str(&text).ok()?;
        json.get("bed_type")?.as_str().map(str::to_owned)
    })();
    from_archive.unwrap_or_else(|| {
        match fallback {
            "High Temp Plate" => "hot_plate",
            "Cool Plate" => "cool_plate",
            "Engineering Plate" => "engineering_plate",
            "Supertack Plate" => "supertack_plate",
            _ => "textured_plate",
        }
        .into()
    })
}

/// Причина невдалого хмарного друку людською мовою з порадою. Коди — `BAMBU_NETWORK_ERR_*`
/// з `bambu_networking.hpp` Bambu Studio; так само їх розрізняє `PrintJob.cpp`.
fn print_failure(error: &anyhow::Error) -> String {
    let text = match cloud::PluginError::code_of(error) {
        Some(-15 | -2040 | -3090 | -4010) => tr("Файл завеликий для надсилання (понад 1 ГБ) — спростіть модель і наріжте ще раз."),
        Some(-14 | -2100 | -3070) => tr("Нарізаний файл не знайдено — наріжте модель ще раз і повторіть друк."),
        Some(-16 | -2020 | -3020) => tr("Файл пошкодився під час надсилання (не збігся MD5) — наріжте ще раз і повторіть."),
        Some(-17 | -2060 | -3050) => tr("Bambu Cloud не відповів вчасно — перевірте інтернет і спробуйте ще раз."),
        Some(-3130) => tr("Принтер не підтвердив завдання — перевірте, що він увімкнений і в мережі, і спробуйте ще раз."),
        Some(-20 | -2130 | -4020) => tr("Не вдалося вивантажити файл на принтер (FTP) — перевірте мережу й спробуйте ще раз."),
        Some(code) => crate::tr_format!("Bambu Cloud не прийняв завдання (код плагіна {code}) — спробуйте ще раз; якщо повторюється, надішліть друк через локальну мережу."),
        None => format!("{error:#}"),
    };
    format!("{}: {text}", tr("Друк не надіслано"))
}

/// Типи філаменту з нарізаного `.gcode.3mf` (`filament_type` у project_settings).
fn filament_types(gcode_3mf: &std::path::Path) -> Vec<String> {
    use std::io::Read as _;
    (|| {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(gcode_3mf).ok()?).ok()?;
        let mut text = String::new();
        zip.by_name("Metadata/project_settings.config")
            .ok()?
            .read_to_string(&mut text)
            .ok()?;
        let json: serde_json::Value = serde_json::from_str(&text).ok()?;
        Some(
            json.get("filament_type")?
                .as_array()?
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
        )
    })()
    .unwrap_or_default()
}

/// М’яке попередження: відкритий принтер (A1, A1 mini, P1P) і пластик, який без камери
/// деформується. Друк не блокує.
pub(crate) fn open_frame_warning(model_id: &str, types: &[String]) -> Option<String> {
    let open = matches!(model_id.to_ascii_uppercase().as_str(), "N2S" | "N1" | "C11");
    let warm = types
        .iter()
        .find(|t| {
            let t = t.to_ascii_uppercase();
            ["ABS", "ASA", "PC"].iter().any(|m| t == *m || t.starts_with(&format!("{m}-")) || t.starts_with(&format!("{m} ")))
        })?
        .clone();
    open.then(|| {
        crate::tr_format!(
            "Принтер відкритий — {warm} може деформуватися й відшаруватися. Рекомендовано PLA або PETG; друк не заблоковано."
        )
    })
}

/// Скільки слотів філаменту в проєкті (довжина ams_mapping).
fn filament_slots(gcode_3mf: &std::path::Path) -> usize {
    use std::io::Read as _;
    (|| {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(gcode_3mf).ok()?).ok()?;
        let mut text = String::new();
        zip.by_name("Metadata/project_settings.config")
            .ok()?
            .read_to_string(&mut text)
            .ok()?;
        let json: serde_json::Value = serde_json::from_str(&text).ok()?;
        json.get("filament_settings_id")?.as_array().map(Vec::len)
    })()
    .unwrap_or(1)
    .max(1)
}

impl Workspace {
    pub fn can_send_print(&self) -> bool {
        // Klipper: модель не звіряється (профіль користувача), попередження — у діалозі.
        if self.cloud.selected.as_ref().is_some_and(|d| self.cloud.klipper.has(d)) {
            return !self.engine_loading
                && matches!(self.slice_state, crate::workspace::SliceState::Done(_))
                && !self.cloud.sending;
        }
        !self.engine_loading
            && matches!(self.slice_state, crate::workspace::SliceState::Done(_))
            && self.cloud.selected.as_ref().is_some_and(|dev| {
                let printer = self.cloud.printers.iter().find(|p| &p.dev_id == dev);
                let profile_model = self
                    .presets
                    .as_ref()
                    .and_then(|s| s.machine_model_id(&self.selection.machine));
                printer
                    .zip(profile_model.as_ref())
                    .is_some_and(|(printer, model)| printer.model.eq_ignore_ascii_case(model))
            })
            && !self.cloud.sending
    }

    /// Чому «Друкувати» неактивна — для підказки на кнопці; `None`, якщо друк доступний.
    pub fn print_blocker(&self) -> Option<SharedString> {
        if self.can_send_print() {
            return None;
        }
        let why = |text: &str| Some(SharedString::from(tr(text)));
        if self.cloud.sending {
            return why("Попереднє завдання ще надсилається");
        }
        let Some(dev) = &self.cloud.selected else {
            return why("Принтер не вибрано — додайте його на сторінці «Пристрій»");
        };
        if !matches!(self.slice_state, crate::workspace::SliceState::Done(_)) {
            return why("Спершу наріжте модель");
        }
        let printer = self.cloud.printers.iter().find(|p| &p.dev_id == dev)?;
        if printer.model.is_empty() {
            return why(
                "Модель принтера ще невідома: виберіть її на сторінці «Пристрій» у блоці «Локальна мережа (LAN)»",
            );
        }
        Some(
            crate::tr_format!(
                "Профіль «{machine}» не для цього принтера ({model}) — виберіть відповідний профіль принтера",
                machine = self.selection.machine,
                model = model_name(&printer.model)
            )
            .into(),
        )
    }

    /// Bambu Connect: копія файлу в теку передачі й відкриття `bambu-connect://` посилання.
    /// Друк запускає користувач у самому Connect.
    pub(crate) fn send_via_bambu_connect(
        &mut self,
        gcode_3mf: &std::path::Path,
        name: &str,
        cx: &mut Context<Self>,
    ) {
        match crate::bambu_connect::prepare(gcode_3mf, name) {
            Ok((_, url)) => {
                cx.open_url(&url);
                self.toast(
                    ToastKind::Info,
                    tr("Файл передано в Bambu Connect: виберіть там принтер і натисніть «Друк»."),
                    cx,
                );
            }
            Err(e) => self.toast(
                ToastKind::Error,
                crate::tr_format!("Не вдалося підготувати файл для Bambu Connect: {e:#}"),
                cx,
            ),
        }
    }

    /// Хмарний друк заблоковано плагіном: пояснення й робочі шляхи замість коду -26.
    /// Головна дія — Bambu Connect (офіційний шлях для сторонніх слайсерів).
    fn open_unsigned_print_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let connect = crate::bambu_connect::installed();
        let sliced = match &self.slice_state {
            crate::workspace::SliceState::Done(out) => Some(out.gcode_3mf.clone()),
            _ => None,
        };
        let name = format!(
            "{} {}",
            self.path
                .as_ref()
                .and_then(|p| p.file_stem())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "model".into()),
            self.plate_to_slice().unwrap_or(1)
        );
        self.open_modal(
            Rc::new(move |_, _, cx| {
                let z = zed();
                let para = |text: &str| div().text_sm().child(tr(text));
                let primary = if connect {
                    let sliced = sliced.clone();
                    let name = name.clone();
                    ui::accent_button("unsigned-connect", IconName::Send, "Надіслати через Bambu Connect", sliced.is_some())
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            ws.close_modal(cx);
                            if let Some(path) = &sliced {
                                ws.send_via_bambu_connect(path, &name, cx);
                            }
                        }))
                        .into_any_element()
                } else {
                    ui::accent_button("unsigned-get-connect", IconName::ExternalLink, "Завантажити Bambu Connect", true)
                        .on_click(cx.listener(|_, _, _, cx| cx.open_url(crate::bambu_connect::DOWNLOAD_URL)))
                        .into_any_element()
                };
                crate::widgets::modal(
                    "Хмарний друк недоступний",
                    540.,
                    div()
                        .flex()
                        .flex_col()
                        .p(px(14.))
                        .gap(px(10.))
                        .child(para("Bambu Cloud не прийняв завдання. Плагін Bambu дозволяє надсилати друк через хмару лише з програм, підписаних Bambu Lab. Нарізаний файл і принтер тут ні до чого."))
                        .child(para("Як надрукувати:"))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(6.))
                                .pl(px(8.))
                                .text_sm()
                                .child(if connect {
                                    tr("1. Через Bambu Connect (рекомендовано) — офіційний шлях Bambu Lab для сторонніх слайсерів: файл відкриється в Bambu Connect, там виберіть принтер і натисніть «Друк». Працює і через хмару, і в локальній мережі.")
                                } else {
                                    tr("1. Через Bambu Connect (рекомендовано) — безкоштовна програма Bambu Lab для сторонніх слайсерів. Встановіть її, увійдіть тим самим акаунтом Bambu, і тут з’явиться кнопка «Надіслати через Bambu Connect».")
                                })
                                .child(tr("2. Через локальну мережу — додайте принтер за кодом доступу з його екрана. На прошивках з авторизацією Bambu для цього потрібен режим LAN Only з Developer Mode на принтері."))
                                .child(tr("3. Експортуйте .gcode.3mf і відкрийте його в Bambu Studio чи Bambu Connect.")),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(z.text_muted)
                                .child(tr("Стан принтера, температури й історія через хмару працюють як і раніше.")),
                        ),
                    Some(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(6.))
                            .child(
                                ui::ghost_button("unsigned-export", Some(IconName::Download), "Експортувати…")
                                    .border_1()
                                    .border_color(z.border_variant)
                                    .on_click(cx.listener(|ws, _, window, cx| {
                                        ws.close_modal(cx);
                                        ws.prompt_export(window, cx);
                                    })),
                            )
                            .child(
                                ui::ghost_button("unsigned-lan", Some(IconName::Wifi), "Додати принтер у мережі")
                                    .border_1()
                                    .border_color(z.border_variant)
                                    .on_click(cx.listener(|ws, _, window, cx| {
                                        ws.close_modal(cx);
                                        ws.open_lan_dialog(None, window, cx);
                                    })),
                            )
                            .child(primary)
                            .into_any_element(),
                    ),
                )
            }),
            window,
            cx,
        );
    }

    pub fn open_print_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_send_print() {
            return;
        }
        if self.cloud.selected.as_ref().is_some_and(|d| self.cloud.klipper.has(d)) {
            return self.open_klipper_print_dialog(window, cx);
        }
        let crate::workspace::SliceState::Done(out) = &self.slice_state else {
            return;
        };
        let Some(dev) = self.cloud.selected.clone() else {
            return;
        };
        let Some(plate) = self.plate_to_slice() else {
            return;
        };
        let printer = self
            .cloud
            .printers
            .iter()
            .find(|p| p.dev_id == dev)
            .map(|p| {
                if p.name.is_empty() {
                    format!("{} {}", tr("Принтер"), p.dev_id)
                } else {
                    p.name.clone()
                }
            })
            .unwrap_or_else(|| format!("{} {dev}", tr("Принтер")));
        let gcode_3mf = out.gcode_3mf.clone();
        let project = self
            .path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "model".into());
        let bed_type = plate_bed_type(&gcode_3mf, plate, &self.selection.bed_type);
        let minutes = out.print_time.as_secs() / 60;
        let summary = format!(
            "{project} · {} {plate} · {} · {} · {}",
            tr("пластина"),
            duration_text(minutes),
            crate::tr_format!("{grams:.1} г", grams = out.filament_grams),
            self.selection.filament
        );
        let bed_label = bed_type.clone();
        let model_id = self
            .cloud
            .printers
            .iter()
            .find(|p| p.dev_id == dev)
            .map(|p| p.model.clone())
            .unwrap_or_default();
        let material_warning = open_frame_warning(&model_id, &filament_types(&gcode_3mf));
        // Шляхи вибираються ДО будь-якої спроби: хмарний друк плагіном Bambu для непідписаних
        // програм завжди дає -26, тож його кнопки немає, доки він хоч раз не пройде.
        // Наявність Bambu Connect перевіряється при кожному відкритті (встановили — помітимо).
        let found = self
            .cloud
            .lan
            .found
            .iter()
            .find(|f| f.serial == dev.as_str())
            .cloned();
        let lan_state = if self.cloud.lan.has(&dev) {
            print_paths::Lan::Saved
        } else if found.is_some() {
            print_paths::Lan::Found
        } else {
            print_paths::Lan::Absent
        };
        let paths = print_paths::choose(print_paths::Inputs {
            os: print_paths::Os::current(),
            connect_installed: crate::bambu_connect::installed(),
            lan: lan_state,
            cloud_print_works: self.cloud.cloud_print_works
                || std::env::var_os("SLICENV_CLOUD_PRINT").is_some_and(|v| v == "1"),
        });
        let connect_send = {
            let gcode_3mf = gcode_3mf.clone();
            let name = format!("{project} {plate}");
            Rc::new(move |ws: &mut Workspace, cx: &mut Context<Workspace>| {
                ws.close_modal(cx);
                ws.send_via_bambu_connect(&gcode_3mf, &name, cx);
            })
        };
        let send = Rc::new(move |ws: &mut Workspace, cx: &mut Context<Workspace>| {
            let o = ws.cloud.print;
            let slots = filament_slots(&gcode_3mf);
            let mut req = cloud::PrintRequest::new(
                dev.clone(),
                gcode_3mf.clone(),
                gcode_3mf.clone(),
                project.clone(),
            );
            req.plate_index = plate as i32;
            req.preset_name = format!("{project}_plate_{plate}");
            req.bed_type = bed_type.clone();
            req.bed_leveling = o.bed_leveling;
            req.flow_cali = o.flow_cali;
            req.vibration_cali = o.vibration_cali;
            req.timelapse = o.timelapse;
            // A1 без AMS: усі слоти — зовнішня котушка.
            req.use_ams = false;
            req.ams_mapping = format!("[{}]", vec!["-1"; slots].join(","));
            req.ams_mapping2 = format!(
                "[{}]",
                vec![r#"{"ams_id":255,"slot_id":255}"#; slots].join(",")
            );
            ws.cloud.sending = true;
            ws.cloud.send(CloudCmd::Print(Box::new(req)));
            ws.close_modal(cx);
            ws.toast(ToastKind::Info, tr("Надсилаю завдання на принтер…"), cx);
        });

        self.open_modal(
            Rc::new(move |ws, _, cx| {
                let z = zed();
                let opts = ws.cloud.print;
                let toggle = |id: &'static str,
                              label: &'static str,
                              checked: bool,
                              set: fn(&mut PrintOptions, bool),
                              cx: &mut Context<Workspace>| {
                    crate::widgets::checkbox(id, checked, false)
                        .h(px(28.))
                        .child(tr(label))
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            set(&mut ws.cloud.print, !checked);
                            cx.notify();
                        }))
                };
                let note = |text: &str| div().pb(px(6.)).text_xs().text_color(z.text_muted).child(tr(text));
                let action_button = |action: print_paths::Action, primary: bool, cx: &mut Context<Workspace>| {
                    use print_paths::Action;
                    let (id, icon, label): (&'static str, IconName, &'static str) = match action {
                        Action::LanSend => ("print-send-lan", IconName::Wifi, "Надіслати по локальній мережі"),
                        Action::LanAdd => ("print-add-lan", IconName::Wifi, "Надіслати по локальній мережі"),
                        Action::LanManual => ("print-lan-manual", IconName::Wifi, "Додати принтер у мережі"),
                        Action::Connect => ("print-send-connect", IconName::Send, "Надіслати через Bambu Connect"),
                        Action::Cloud => ("print-send-cloud", IconName::Cloud, "Через хмару"),
                    };
                    let button = if primary {
                        ui::accent_button(id, icon, label, true)
                    } else {
                        ui::ghost_button(id, Some(icon), label)
                            .border_1()
                            .border_color(z.border_variant)
                    };
                    let send = send.clone();
                    let connect_send = connect_send.clone();
                    let found = found.clone();
                    button
                        .on_click(cx.listener(move |ws, _, window, cx| match action {
                            Action::LanSend | Action::Cloud => send(ws, cx),
                            Action::Connect => connect_send(ws, cx),
                            Action::LanAdd => {
                                ws.close_modal(cx);
                                ws.open_lan_dialog(found.clone(), window, cx);
                            }
                            Action::LanManual => {
                                ws.close_modal(cx);
                                ws.open_lan_dialog(None, window, cx);
                            }
                        }))
                        .into_any_element()
                };
                let material_warning = material_warning.clone();
                let primary = paths.primary;
                let secondary = paths.secondary.clone();
                let notice = paths.notice;
                crate::widgets::modal(
                    "Надіслати на друк",
                    520.,
                    div()
                        .flex()
                        .flex_col()
                        .p(px(12.))
                        .gap(px(4.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(ui::icon(IconName::Printer).text_color(z.accent))
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .child(printer.clone()),
                                ),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(z.text_muted)
                                .child(summary.clone()),
                        )
                        .child(
                            div()
                                .pb(px(6.))
                                .text_xs()
                                .text_color(z.text_placeholder)
                                .child(format!("{}: {bed_label}", tr("Пластина"))),
                        )
                        .when_some(material_warning.clone(), |d, text| {
                            d.child(
                                div()
                                    .flex()
                                    .items_start()
                                    .gap(px(6.))
                                    .pb(px(6.))
                                    .child(ui::icon(IconName::TriangleAlert).text_color(z.warning))
                                    .child(div().text_xs().child(text)),
                            )
                        })
                        .map(|d| match primary {
                            Some(print_paths::Action::LanSend) => d.child(note(
                                "Через локальну мережу: файл буде завантажено на microSD принтера, і друк почнеться одразу.",
                            )),
                            Some(print_paths::Action::Connect) => d.child(note(
                                "Файл відкриється в Bambu Connect — там виберіть принтер, калібрування й натисніть «Друк».",
                            )),
                            _ => d,
                        })
                        .when(notice != print_paths::ConnectNotice::None, |d| {
                            d.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(6.))
                                    .mb(px(6.))
                                    .p(px(10.))
                                    .rounded(px(6.))
                                    .border_1()
                                    .border_color(z.border_variant)
                                    .child(div().text_sm().child(tr(
                                        "Щоб друкувати через інтернет, потрібен Bambu Connect — безкоштовна програма Bambu Lab.",
                                    )))
                                    .map(|d| match notice {
                                        print_paths::ConnectNotice::NoLinuxBuild => d.child(
                                            div().text_xs().text_color(z.text_muted).child(tr(
                                                "Версії Bambu Connect для Linux поки немає. Друкуйте по локальній мережі або експортуйте файл і відкрийте його на іншому комп’ютері.",
                                            )),
                                        ),
                                        _ => d.child(
                                            div().flex().child(
                                                ui::accent_button(
                                                    "print-get-connect",
                                                    IconName::ExternalLink,
                                                    "Завантажити Bambu Connect",
                                                    true,
                                                )
                                                .on_click(cx.listener(|_, _, _, cx| {
                                                    cx.open_url(crate::bambu_connect::DOWNLOAD_URL)
                                                })),
                                            ),
                                        ),
                                    })
                                    .when(primary == Some(print_paths::Action::LanAdd), |d| {
                                        d.child(div().text_xs().text_color(z.text_muted).child(tr(
                                            "Принтер знайдено в локальній мережі: введіть код доступу з його екрана, і файл можна буде надіслати напряму.",
                                        )))
                                    }),
                            )
                        })
                        .when(paths.print_options, |d| {
                            d.child(ui::divider())
                                .child(toggle(
                                    "opt-level",
                                    "Вирівнювання столу",
                                    opts.bed_leveling,
                                    |o, v| o.bed_leveling = v,
                                    cx,
                                ))
                                .child(toggle(
                                    "opt-flow",
                                    "Калібрування потоку",
                                    opts.flow_cali,
                                    |o, v| o.flow_cali = v,
                                    cx,
                                ))
                                .child(toggle(
                                    "opt-vib",
                                    "Калібрування вібрацій",
                                    opts.vibration_cali,
                                    |o, v| o.vibration_cali = v,
                                    cx,
                                ))
                                .child(toggle(
                                    "opt-timelapse",
                                    "Таймлапс",
                                    opts.timelapse,
                                    |o, v| o.timelapse = v,
                                    cx,
                                ))
                        }),
                    Some(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(6.))
                            .child(
                                ui::ghost_button("print-cancel", None, "Скасувати")
                                    .on_click(cx.listener(|ws, _, _, cx| ws.close_modal(cx))),
                            )
                            .child(
                                ui::ghost_button("print-export", Some(IconName::Download), "Експортувати .gcode.3mf…")
                                    .border_1()
                                    .border_color(z.border_variant)
                                    .on_click(cx.listener(|ws, _, window, cx| {
                                        ws.close_modal(cx);
                                        ws.prompt_export(window, cx);
                                    })),
                            )
                            .children(secondary.iter().map(|&a| action_button(a, false, cx)))
                            .children(primary.map(|a| action_button(a, true, cx)))
                            .into_any_element(),
                    ),
                )
            }),
            window,
            cx,
        );
    }
}

#[cfg(test)]
mod shutdown_tests {
    use super::*;
    #[test]
    fn shutdown_guard_stops_and_joins_before_returning() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let (commands, receiver) = mpsc::channel();
        let stopping = Arc::new(AtomicBool::new(false));
        let stopped = Arc::new(AtomicBool::new(false));
        let seen = stopped.clone();
        let stop = stopping.clone();
        let worker = std::thread::spawn(move || {
            assert!(matches!(receiver.recv().unwrap(), CloudCmd::Shutdown));
            assert!(stop.load(Ordering::Acquire));
            std::thread::sleep(Duration::from_millis(20));
            seen.store(true, Ordering::Release);
        });
        drop(CloudShutdown {
            commands,
            stopping,
            worker: Some(worker),
        });
        assert!(
            stopped.load(Ordering::Acquire),
            "shutdown returned before worker teardown completed"
        );
    }
}

#[cfg(test)]
mod dispatch_tests {
    use super::*;
    use cloud::{Axis, FanKind, Jog};
    use guard::Blocked;

    /// Підроблений транспорт: замість робочого потоку — канал, який читає тест.
    fn world(
        gcode: &str,
        age: Duration,
        connected: bool,
    ) -> (CloudState, mpsc::Receiver<CloudCmd>) {
        let (tx, rx) = mpsc::channel();
        let dev = DeviceId::new("TESTDEVICE01");
        let mut state = CloudState::default();
        state.commands = Some(tx);
        state.printers = vec![cloud::Printer {
            dev_id: dev.clone(),
            name: String::new(),
            model: "N2S".into(),
            online: true,
        }];
        state.selected = Some(dev);
        state.connected = connected;
        state.updated = Some(Instant::now() - age);
        state.status = Some(PrinterStatus {
            gcode_state: Some(gcode.into()),
            home_flag: Some(0b111),
            ..Default::default()
        });
        (state, rx)
    }
    fn jog(axis: Axis, input: i8) -> Control {
        Control::Jog(Jog::new(axis, input).unwrap())
    }
    const FRESH: Duration = Duration::from_secs(1);

    #[test]
    fn blocked_commands_never_reach_the_transport() {
        let (printing, rx) = world("RUNNING", FRESH, true);
        assert_eq!(printing.dispatch(Control::Home).unwrap_err(), Blocked::Busy);
        assert_eq!(
            printing.dispatch(jog(Axis::X, 10)).unwrap_err(),
            Blocked::Busy
        );
        assert!(matches!(
            printing.dispatch(Control::Nozzle(900)).unwrap_err(),
            Blocked::Rejected(_)
        ));

        let (stale, stale_rx) = world("IDLE", Duration::from_secs(120), true);
        assert_eq!(stale.dispatch(jog(Axis::X, 1)).unwrap_err(), Blocked::Stale);

        let (down, down_rx) = world("IDLE", FRESH, false);
        assert_eq!(
            down.dispatch(Control::Home).unwrap_err(),
            Blocked::Disconnected
        );
        assert_eq!(
            down.dispatch(Control::Nozzle(200)).unwrap_err(),
            Blocked::Disconnected
        );

        let (mut none, none_rx) = world("IDLE", FRESH, true);
        none.selected = None;
        assert_eq!(
            none.dispatch(Control::Light(true)).unwrap_err(),
            Blocked::NoPrinter
        );

        for rx in [rx, stale_rx, down_rx, none_rx] {
            assert!(
                rx.try_recv().is_err(),
                "заблокована команда потрапила в канал"
            );
        }
    }

    #[test]
    fn allowed_commands_are_queued_for_the_worker_exactly_once() {
        let (idle, rx) = world("IDLE", FRESH, true);
        idle.dispatch(jog(Axis::X, 10)).unwrap();
        idle.dispatch(Control::Home).unwrap();
        let queued: Vec<_> = rx.try_iter().collect();
        assert_eq!(queued.len(), 2);
        assert!(matches!(&queued[0], CloudCmd::Control(_, Control::Jog(_))));
        assert!(matches!(&queued[1], CloudCmd::Control(_, Control::Home)));

        // Під час друку температура, вентилятор і швидкість дозволені.
        let (printing, rx) = world("RUNNING", FRESH, true);
        printing.dispatch(Control::Nozzle(215)).unwrap();
        printing.dispatch(Control::Bed(60)).unwrap();
        printing.dispatch(Control::Fan(FanKind::Part, 70)).unwrap();
        printing
            .dispatch(Control::Speed(SpeedLevel::Sport))
            .unwrap();
        printing.dispatch(Control::Pause).unwrap();
        assert_eq!(rx.try_iter().count(), 5);
    }

    #[test]
    fn closed_transport_does_not_report_success() {
        let (state, rx) = world("IDLE", FRESH, true);
        drop(rx);
        assert_eq!(
            state.dispatch(Control::Light(true)).unwrap_err(),
            Blocked::Disconnected
        );
        let (mut state, _rx) = world("IDLE", FRESH, true);
        state.commands = None;
        assert_eq!(
            state.dispatch(Control::Home).unwrap_err(),
            Blocked::Disconnected
        );
    }

    #[test]
    fn an_axis_that_is_not_homed_cannot_jog_even_when_idle() {
        let (mut idle, rx) = world("IDLE", FRESH, true);
        idle.status.as_mut().unwrap().home_flag = Some(0b001); // лише X вдома
        assert_eq!(
            idle.dispatch(jog(Axis::Y, 10)).unwrap_err(),
            Blocked::NotHomed(Axis::Y)
        );
        assert_eq!(
            idle.dispatch(jog(Axis::Z, 1)).unwrap_err(),
            Blocked::NotHomed(Axis::Z)
        );
        assert!(idle.dispatch(jog(Axis::X, 1)).is_ok());
        assert_eq!(rx.try_iter().count(), 1);
    }
}

#[cfg(test)]
mod material_tests {
    use super::open_frame_warning;

    #[test]
    fn open_printers_warn_about_warping_plastics_only() {
        let abs = vec!["ABS".to_owned()];
        assert!(open_frame_warning("N2S", &abs).unwrap().contains("ABS"));
        assert!(open_frame_warning("n1", &["ASA".to_owned()]).is_some());
        assert!(open_frame_warning("C11", &["PC".to_owned()]).is_some());
        assert!(open_frame_warning("C11", &["ABS-GF".to_owned()]).is_some());
        // Закритий P1S і безпечні пластики — без попередження; PCTG — не PC.
        assert!(open_frame_warning("C12", &abs).is_none());
        assert!(open_frame_warning("N2S", &["PLA".to_owned(), "PETG".to_owned()]).is_none());
        assert!(open_frame_warning("N2S", &["PCTG".to_owned()]).is_none());
        assert!(open_frame_warning("", &abs).is_none());
    }
}
