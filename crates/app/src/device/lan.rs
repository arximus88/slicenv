//! LAN-принтери на сторінці «Пристрій»: без плагіна й акаунта, поруч із хмарою.
//!
//! Окремий робочий потік (`lan-worker`) тримає MQTT лише з вибраним LAN-принтером,
//! слухає виявлення і шле в інтерфейс ті самі `CloudMsg` (статус, камера, результат друку),
//! що й хмара, плюс `CloudMsg::Lan` для власного стану. `CloudState::send` маршрутизує
//! команди сюди, якщо вибраний принтер збережений як LAN. Коди доступу — лише в
//! `lan::store` (файл 0600); ні в лог, ні в повідомлення вони не потрапляють.
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use cloud::{Control, DeviceId, PrinterStatus};
use futures::channel::mpsc as async_mpsc;
use gpui::{Context, SharedString, Window, div, prelude::*, px};
use gpui_component::input::InputState;
use lan::{LanClient, LanError, LanEvent, LanPrinter};

use super::{CloudCmd, CloudMsg, CloudState, guard};
use crate::{
    i18n::tr,
    theme::zed,
    ui::{self, IconName},
    widgets::ToastKind,
    workspace::Workspace,
};

pub enum LanCmd {
    /// Вибрано принтер; не LAN — робітник від’єднується.
    Select(DeviceId),
    Refresh,
    Control(DeviceId, Control),
    Print(Box<cloud::PrintRequest>),
    Camera(bool),
    Versions,
    /// Перевірити з’єднання з кодом і, якщо вдалося, зберегти.
    Add(Box<LanPrinter>),
    Remove(String),
    /// Модель, вибрана вручну (принтер додано без анонсу, `get_version` ще не відповів).
    SetModel(String, String),
    Shutdown,
}

/// Принтер, що оголосив себе в мережі, але ще не збережений.
#[derive(Clone, PartialEq)]
pub struct Found {
    pub serial: String,
    pub ip: String,
    pub name: String,
    pub model: String,
    pub lan_only: bool,
}

pub enum LanMsg {
    Printers(Vec<cloud::Printer>),
    Link(DeviceId, bool),
    Found(Vec<Found>),
    Added(Result<DeviceId, String>),
    /// Помилка з’єднання вибраного принтера (для панелі); `None` — усе гаразд.
    Problem(Option<String>),
    /// Частка завантаження файлу на принтер; `None` — завершено.
    Upload(Option<f32>),
}

impl LanMsg {
    /// Для `SLICENV_DEBUG`: без серійних номерів, IP і кодів.
    pub fn describe(&self) -> String {
        match self {
            Self::Printers(list) => format!("[lan] збережених принтерів: {}", list.len()),
            Self::Link(dev, up) => format!("[lan] {dev}: з’єднання {up}"),
            Self::Found(list) => format!("[lan] нових у мережі: {}", list.len()),
            Self::Added(r) => format!("[lan] додавання: {}", r.is_ok()),
            Self::Problem(p) => format!("[lan] проблема: {p:?}"),
            Self::Upload(p) => format!("[lan] завантаження: {p:?}"),
        }
    }
}

pub struct AddInputs {
    pub ip: gpui::Entity<InputState>,
    pub serial: gpui::Entity<InputState>,
    pub code: gpui::Entity<InputState>,
}

#[derive(Default)]
pub struct LanUi {
    commands: Option<mpsc::Sender<LanCmd>>,
    /// Збережені LAN-принтери у формі хмарних (online = з’єднання або недавній анонс).
    pub printers: Vec<cloud::Printer>,
    up: HashSet<DeviceId>,
    pub found: Vec<Found>,
    /// Останній список акаунта: об’єднання будується з нього й LAN.
    cloud_list: Vec<cloud::Printer>,
    pub adding: bool,
    pub add_error: Option<SharedString>,
    pub problem: Option<SharedString>,
    pub upload: Option<f32>,
    pub inputs: Option<AddInputs>,
    /// Модель для модалки додавання (код, напр. N2S); порожня — визначить принтер.
    pub add_model: String,
    /// Модалку відкрито для ручного вводу (не з анонсу): модель треба вибрати.
    pub add_manual: bool,
}

/// Моделі для ручного вибору, коли принтер не оголосив себе в мережі.
pub const MODELS: [&str; 10] = [
    "N2S", "N1", "C11", "C12", "N7", "BL-P001", "C13", "O1D", "O1S", "O1C",
];

impl LanUi {
    pub fn has(&self, dev: &DeviceId) -> bool {
        self.printers.iter().any(|p| &p.dev_id == dev)
    }
    pub fn is_up(&self, dev: &DeviceId) -> bool {
        self.up.contains(dev)
    }
    pub fn send(&self, cmd: LanCmd) {
        if let Some(tx) = &self.commands {
            tx.send(cmd).ok();
        }
    }
    /// Команду для LAN-принтера забирає собі (`None`), решту повертає хмарі.
    pub fn route(&self, selected: Option<&DeviceId>, cmd: CloudCmd) -> Option<CloudCmd> {
        let lan_selected = selected.is_some_and(|d| self.has(d));
        match cmd {
            CloudCmd::Select(dev) => {
                // Робітник LAN має знати й про вибір хмарного — щоб від’єднатися.
                self.send(LanCmd::Select(dev.clone()));
                (!self.has(&dev)).then_some(CloudCmd::Select(dev))
            }
            CloudCmd::Control(dev, control) if self.has(&dev) => {
                self.send(LanCmd::Control(dev, control));
                None
            }
            CloudCmd::Print(req) if self.has(&req.dev_id) => {
                self.send(LanCmd::Print(req));
                None
            }
            CloudCmd::Camera(on) if lan_selected => {
                self.send(LanCmd::Camera(on));
                None
            }
            CloudCmd::Versions if lan_selected => {
                self.send(LanCmd::Versions);
                None
            }
            // Мініатюри завдань — хмарний сервіс; для LAN їх немає.
            CloudCmd::Thumbnail(key) if self.has(&key.dev) => None,
            CloudCmd::Refresh => {
                self.send(LanCmd::Refresh);
                Some(CloudCmd::Refresh)
            }
            CloudCmd::Shutdown => {
                self.send(LanCmd::Shutdown);
                Some(CloudCmd::Shutdown)
            }
            other => Some(other),
        }
    }
    /// Цей принтер у списку акаунта Bambu Cloud (незалежно від того, чи збережений він у LAN).
    pub fn cloud_printer(&self, dev: &DeviceId) -> Option<&cloud::Printer> {
        self.cloud_list.iter().find(|p| &p.dev_id == dev)
    }
    pub fn set_cloud_list(&mut self, list: Vec<cloud::Printer>) {
        self.cloud_list = list;
    }
    /// Хмарні принтери (крім тих, що збережені як LAN) + LAN.
    pub fn union(&self) -> Vec<cloud::Printer> {
        self.cloud_list
            .iter()
            .filter(|p| !self.has(&p.dev_id))
            .chain(&self.printers)
            .cloned()
            .collect()
    }
}

/// Текст помилки для інтерфейсу: перекладена фраза + деталь.
pub fn error_text(error: &LanError) -> String {
    let detail = error.detail();
    if detail.is_empty() {
        tr(error.key())
    } else {
        format!("{} ({})", tr(error.key()), tr(&detail))
    }
}

/// Помилки, що потребують дії користувача, показуємо тостом; решта — у панелі.
fn needs_attention(error: &LanError) -> bool {
    matches!(
        error,
        LanError::BadAccessCode
            | LanError::SerialMismatch
            | LanError::UntrustedCertificate(_)
            | LanError::ControlLocked { .. }
    )
}

fn as_cloud(p: &LanPrinter, online: bool, seen_name: Option<&str>) -> cloud::Printer {
    let name = if p.name.is_empty() {
        seen_name.unwrap_or_default().to_owned()
    } else {
        p.name.clone()
    };
    cloud::Printer {
        dev_id: DeviceId::new(p.serial.clone()),
        name,
        model: p.model.clone(),
        online,
    }
}

/// «Bambu Lab A1» (з `get_version`) → код моделі.
fn model_from_product(product: &str) -> Option<&'static str> {
    let name = product.trim().trim_start_matches("Bambu Lab").trim();
    [
        "N2S", "N1", "N9", "C11", "C12", "N7", "BL-P001", "BL-P002", "C13", "N6", "O1D", "O1E",
        "O1S", "O1C",
    ]
    .into_iter()
    .find(|code| lan::model_name(code).eq_ignore_ascii_case(name))
}

enum Internal {
    Connected(
        String,
        Result<(LanClient, mpsc::Receiver<LanEvent>), LanError>,
    ),
    Added(Result<LanPrinter, LanError>),
    Printed(Result<String, String>),
}

struct Conn {
    serial: String,
    client: Arc<LanClient>,
    events: mpsc::Receiver<LanEvent>,
}

/// Запускає робітника. `SLICENV_LAN_OFF` вимикає LAN повністю.
pub fn spawn() -> (mpsc::Sender<LanCmd>, async_mpsc::UnboundedReceiver<CloudMsg>) {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (ui_tx, ui_rx) = async_mpsc::unbounded();
    if paths::env("SLICENV_LAN_OFF").is_none() {
        thread::Builder::new()
            .name("lan-worker".into())
            .spawn(move || worker(cmd_rx, ui_tx))
            .ok();
    } else if let Some(n) = paths::env("SLICENV_FAKE_FOUND").and_then(|v| v.to_str()?.parse::<usize>().ok()) {
        // Перевірка інтерфейсу: N фіктивних «знайдених у мережі» без жодного зв’язку.
        let found = (1..=n)
            .map(|i| Found {
                serial: format!("FAKE{i:011}"),
                ip: format!("192.0.2.{i}"),
                name: format!("Fake A1 {i}"),
                model: if i % 2 == 0 { "C12" } else { "N2S" }.into(),
                lan_only: false,
            })
            .collect();
        ui_tx.unbounded_send(CloudMsg::Lan(LanMsg::Found(found))).ok();
    }
    (cmd_tx, ui_rx)
}

struct Worker {
    ui: async_mpsc::UnboundedSender<CloudMsg>,
    internal: mpsc::Sender<Internal>,
    saved: Vec<LanPrinter>,
    seen: HashMap<String, (lan::Discovered, Instant)>,
    selected: Option<String>,
    conn: Option<Conn>,
    connecting: bool,
    next_connect: Instant,
    backoff: Duration,
    status: Option<(PrinterStatus, Instant)>,
    camera: Option<Arc<AtomicBool>>,
    printing: Option<Arc<AtomicBool>>,
    last_printers: Vec<(String, String, bool)>,
    last_found: Vec<Found>,
    heartbeat: Instant,
}

const SEEN_FRESH: Duration = Duration::from_secs(90);

impl Worker {
    fn send(&self, msg: CloudMsg) {
        self.ui.unbounded_send(msg).ok();
    }
    fn lan(&self, msg: LanMsg) {
        self.send(CloudMsg::Lan(msg));
    }
    fn printer(&self, serial: &str) -> Option<&LanPrinter> {
        self.saved.iter().find(|p| p.serial == serial)
    }
    fn online(&self, serial: &str) -> bool {
        self.conn
            .as_ref()
            .is_some_and(|c| c.serial == serial && c.client.is_connected())
            || self
                .seen
                .get(serial)
                .is_some_and(|(_, at)| at.elapsed() < SEEN_FRESH)
    }

    /// Шле списки в інтерфейс лише за змінами.
    fn publish_lists(&mut self) {
        let printers: Vec<cloud::Printer> = self
            .saved
            .iter()
            .map(|p| {
                let seen = self.seen.get(&p.serial).map(|(d, _)| d.name.as_str());
                as_cloud(p, self.online(&p.serial), seen)
            })
            .collect();
        let key: Vec<_> = printers
            .iter()
            .map(|p| (p.dev_id.as_str().to_owned(), p.name.clone() + &p.model, p.online))
            .collect();
        if key != self.last_printers {
            self.last_printers = key;
            self.lan(LanMsg::Printers(printers));
        }
        let mut found: Vec<Found> = self
            .seen
            .values()
            .filter(|(d, at)| at.elapsed() < SEEN_FRESH && self.printer(&d.serial).is_none())
            .map(|(d, _)| Found {
                serial: d.serial.clone(),
                ip: d.ip.to_string(),
                name: d.name.clone(),
                model: d.model.clone(),
                lan_only: d.lan_only(),
            })
            .collect();
        found.sort_by(|a, b| a.serial.cmp(&b.serial));
        if found != self.last_found {
            self.last_found = found.clone();
            self.lan(LanMsg::Found(found));
        }
    }

    fn drop_connection(&mut self) {
        if let Some(stop) = self.camera.take() {
            stop.store(true, Ordering::Release);
        }
        if let Some(conn) = self.conn.take() {
            conn.client.disconnect();
            self.lan(LanMsg::Link(DeviceId::new(conn.serial), false));
        }
        self.status = None;
    }

    fn select(&mut self, dev: DeviceId) {
        let serial = dev.as_str().to_owned();
        if self.selected.as_deref() == Some(serial.as_str()) && self.printer(&serial).is_some() {
            return;
        }
        self.drop_connection();
        self.lan(LanMsg::Problem(None));
        self.selected = self.printer(&serial).map(|_| serial);
        self.next_connect = Instant::now();
        self.backoff = Duration::from_secs(5);
    }

    fn maybe_connect(&mut self) {
        let Some(serial) = self.selected.clone() else {
            return;
        };
        if self.conn.is_some() || self.connecting || Instant::now() < self.next_connect {
            return;
        }
        let Some(printer) = self.printer(&serial).cloned() else {
            return;
        };
        self.connecting = true;
        let internal = self.internal.clone();
        thread::spawn(move || {
            internal
                .send(Internal::Connected(serial, LanClient::connect(&printer)))
                .ok();
        });
    }

    fn on_internal(&mut self, msg: Internal) {
        match msg {
            Internal::Connected(serial, result) => {
                self.connecting = false;
                if self.selected.as_deref() != Some(serial.as_str()) {
                    return; // перемкнули, поки з’єднувались
                }
                match result {
                    Ok((client, events)) => {
                        self.backoff = Duration::from_secs(5);
                        self.lan(LanMsg::Problem(None));
                        client.request_version().ok();
                        self.conn = Some(Conn {
                            serial,
                            client: Arc::new(client),
                            events,
                        });
                    }
                    Err(error) => {
                        if needs_attention(&error) {
                            self.send(CloudMsg::Blocked(error_text(&error)));
                        }
                        self.lan(LanMsg::Problem(Some(error_text(&error))));
                        // Невірний код не виправиться повтором: чекаємо довше.
                        let wait = if error == LanError::BadAccessCode {
                            Duration::from_secs(300)
                        } else {
                            self.backoff
                        };
                        self.next_connect = Instant::now() + wait;
                        self.backoff = (self.backoff * 2).min(Duration::from_secs(60));
                    }
                }
            }
            Internal::Added(result) => match result {
                Ok(printer) => {
                    let dev = DeviceId::new(printer.serial.clone());
                    match lan::store::upsert(printer) {
                        Ok(list) => {
                            self.saved = list;
                            self.publish_lists();
                            self.lan(LanMsg::Added(Ok(dev.clone())));
                            self.selected = None;
                            self.select(dev);
                        }
                        Err(e) => self.lan(LanMsg::Added(Err(format!(
                            "{}: {e}",
                            tr("Не вдалося зберегти налаштування LAN")
                        )))),
                    }
                }
                Err(error) => self.lan(LanMsg::Added(Err(error_text(&error)))),
            },
            Internal::Printed(result) => {
                self.printing = None;
                self.lan(LanMsg::Upload(None));
                self.send(CloudMsg::PrintSent(result));
                if let Some(conn) = &self.conn {
                    conn.client.request_status().ok();
                }
            }
        }
    }

    fn on_event(&mut self, event: LanEvent) {
        let Some(conn) = &self.conn else { return };
        let dev = DeviceId::new(conn.serial.clone());
        match event {
            LanEvent::Connected => {
                self.lan(LanMsg::Link(dev, true));
                self.lan(LanMsg::Problem(None));
            }
            LanEvent::Status(status) => {
                self.status = Some(((*status).clone(), Instant::now()));
                self.send(CloudMsg::Status(dev, status));
            }
            LanEvent::Message(message) => {
                let modules = cloud::module_versions(&message);
                if modules.is_empty() {
                    return;
                }
                // Модель для ручно доданого принтера — з відповіді get_version.
                let serial = conn.serial.clone();
                let product = modules
                    .iter()
                    .find_map(|m| model_from_product(&m.product))
                    .map(str::to_owned);
                if let (Some(code), Some(p)) = (
                    product,
                    self.saved.iter_mut().find(|p| p.serial == serial),
                ) && p.model.is_empty()
                {
                    p.model = code;
                    lan::store::save(&self.saved).ok();
                }
                self.send(CloudMsg::Versions(dev, modules));
            }
            LanEvent::Rejected(error) => self.send(CloudMsg::Blocked(error_text(&error))),
            LanEvent::Disconnected(error) => {
                self.lan(LanMsg::Link(dev, false));
                if needs_attention(&error) {
                    self.send(CloudMsg::Blocked(error_text(&error)));
                }
                self.lan(LanMsg::Problem(Some(error_text(&error))));
            }
        }
    }

    fn control(&mut self, dev: DeviceId, control: Control) {
        let printer = self.printer(dev.as_str()).map(|p| as_cloud(p, true, None));
        let conn = self.conn.as_ref().filter(|c| c.serial == dev.as_str());
        let selected = self.selected.clone().map(DeviceId::new);
        let snapshot = guard::Snapshot {
            selected: selected.as_ref(),
            printer: printer.as_ref(),
            status: self.status.as_ref().map(|(s, _)| s),
            age: self.status.as_ref().map(|(_, at)| at.elapsed()),
            connected: conn.is_some_and(|c| c.client.is_connected()),
            lan: true,
        };
        match guard::check(&snapshot, &dev, control) {
            Err(blocked) => self.send(CloudMsg::Blocked(blocked.message())),
            Ok(ctx) => {
                if let Some(conn) = conn
                    && let Err(error) = conn.client.control(control, &ctx)
                {
                    self.send(CloudMsg::Blocked(format!(
                        "{}: {}",
                        tr("Команда не пройшла"),
                        error_text(&error)
                    )));
                }
            }
        }
    }

    fn print(&mut self, req: Box<cloud::PrintRequest>) {
        let fail = |w: &Self, text: String| {
            w.send(CloudMsg::PrintSent(Err(format!("{}: {text}", tr("Друк не надіслано")))));
        };
        if self.printing.is_some() {
            return fail(self, tr("попереднє завдання ще надсилається"));
        }
        let Some(conn) = self.conn.as_ref().filter(|c| c.serial == req.dev_id.as_str()) else {
            return fail(self, tr("немає з’єднання з принтером у мережі"));
        };
        let Some(printer) = self.printer(req.dev_id.as_str()).cloned() else {
            return fail(self, tr("принтер не збережений як LAN"));
        };
        // Відкритий плагін, вибраний користувачем, підписує project_file сам — друк без
        // Developer Mode. Наш чистий Rust-шлях підпису не має (ключа Bambu в Slicenv немає).
        if crate::open_plugin::active() {
            return self.print_open_plugin(req, printer);
        }
        let job = lan::PrintJob {
            plate: req.plate_index.max(1) as u32,
            remote_name: lan::print::remote_name(&req.project_name),
            md5: String::new(),
            subtask_name: req.task_name.clone(),
            use_ams: req.use_ams,
            ams_mapping: lan::print::parse_ams_mapping(&req.ams_mapping).unwrap_or(vec![-1]),
            bed_type: req.bed_type.clone(),
            bed_leveling: req.bed_leveling,
            flow_cali: req.flow_cali,
            vibration_cali: req.vibration_cali,
            layer_inspect: req.layer_inspect,
            timelapse: req.timelapse,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.printing = Some(cancel.clone());
        let client = conn.client.clone();
        let status = self.status.as_ref().map(|(s, _)| s.clone());
        let internal = self.internal.clone();
        let ui = self.ui.clone();
        thread::spawn(move || {
            let mut last = Instant::now() - Duration::from_secs(1);
            let mut progress = |sent: u64, total: u64| {
                if last.elapsed() > Duration::from_millis(250) || sent == total {
                    last = Instant::now();
                    let part = if total == 0 { 1.0 } else { sent as f32 / total as f32 };
                    ui.unbounded_send(CloudMsg::Lan(LanMsg::Upload(Some(part)))).ok();
                }
            };
            let result = lan::send_print(
                &client,
                &printer,
                status.as_ref(),
                &req.gcode_3mf,
                job,
                &mut progress,
                &cancel,
            )
            .map(|_| {
                let name = &req.task_name;
                crate::tr_format!("Завдання «{name}» надіслано на принтер")
            })
            .map_err(|e| format!("{}: {}", tr("Друк не надіслано"), error_text(&e)));
            internal.send(Internal::Printed(result)).ok();
        });
    }

    /// Друк по LAN через відкритий плагін у дочірньому процесі (`open_plugin::local_print`).
    fn print_open_plugin(&mut self, req: Box<cloud::PrintRequest>, printer: LanPrinter) {
        let job = cloud::obn::LocalJob {
            serial: printer.serial.clone(),
            ip: printer.ip.to_string(),
            access_code: printer.access_code.expose().to_owned(),
            name: printer.name.clone(),
            gcode_3mf: req.gcode_3mf.clone(),
            plate: req.plate_index.max(1),
            task_name: req.task_name.clone(),
            project_name: req.project_name.clone(),
            bed_type: req.bed_type.clone(),
            ams_mapping: req.ams_mapping.clone(),
            ams_mapping2: req.ams_mapping2.clone(),
            use_ams: req.use_ams,
            bed_leveling: req.bed_leveling,
            flow_cali: req.flow_cali,
            vibration_cali: req.vibration_cali,
            layer_inspect: req.layer_inspect,
            timelapse: req.timelapse,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.printing = Some(cancel.clone());
        let internal = self.internal.clone();
        let ui = self.ui.clone();
        thread::spawn(move || {
            let mut progress = |part: f32| {
                ui.unbounded_send(CloudMsg::Lan(LanMsg::Upload(Some(part)))).ok();
            };
            let result = crate::open_plugin::local_print(&job, &mut progress, &cancel)
                .map(|_| {
                    let name = &req.task_name;
                    crate::tr_format!("Завдання «{name}» надіслано на принтер")
                })
                .map_err(|e| format!("{}: {e}", tr("Друк не надіслано")));
            internal.send(Internal::Printed(result)).ok();
        });
    }

    fn camera(&mut self, on: bool) {
        if let Some(stop) = self.camera.take() {
            stop.store(true, Ordering::Release);
        }
        if !on {
            return;
        }
        let Some(printer) = self.selected.as_deref().and_then(|s| self.printer(s)).cloned() else {
            return;
        };
        if !lan::camera::has_jpeg_camera(&printer.model) {
            self.send(CloudMsg::CameraStopped(Some(tr(
                "Камера цієї моделі в LAN ще не підтримується (потрібен RTSPS)",
            ))));
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        self.camera = Some(stop.clone());
        let ui = self.ui.clone();
        thread::spawn(move || {
            let result = (|| -> Result<(), LanError> {
                let mut stream = lan::camera::CameraStream::open(&printer)?;
                while !stop.load(Ordering::Acquire) {
                    let jpeg = stream.next_frame()?;
                    let Some(bgra) = super::jpeg_to_bgra(&jpeg, super::CameraFlip::default()) else {
                        continue;
                    };
                    if ui.unbounded_send(CloudMsg::CameraFrame(bgra)).is_err() {
                        break;
                    }
                }
                Ok(())
            })();
            if !stop.load(Ordering::Acquire) {
                let note = result
                    .err()
                    .map(|e| error_text(&e));
                ui.unbounded_send(CloudMsg::CameraStopped(note)).ok();
            }
        });
    }

    /// `false` — завершити робітника.
    fn command(&mut self, cmd: LanCmd) -> bool {
        match cmd {
            LanCmd::Shutdown => {
                self.drop_connection();
                if let Some(cancel) = &self.printing {
                    cancel.store(true, Ordering::Release);
                }
                return false;
            }
            LanCmd::Select(dev) => self.select(dev),
            LanCmd::Refresh => {
                if let Some(conn) = &self.conn {
                    conn.client.request_status().ok();
                } else {
                    self.next_connect = Instant::now();
                }
            }
            LanCmd::Versions => {
                if let Some(conn) = &self.conn {
                    conn.client.request_version().ok();
                }
            }
            LanCmd::Control(dev, control) => self.control(dev, control),
            LanCmd::Print(req) => self.print(req),
            LanCmd::Camera(on) => self.camera(on),
            LanCmd::Add(mut printer) => {
                if printer.model.is_empty()
                    && let Some((d, _)) = self.seen.get(&printer.serial)
                {
                    printer.model = d.model.clone();
                    printer.name = d.name.clone();
                }
                // Поки перевіряємо, не тримаємо другу сесію з тим самим принтером.
                if self.conn.as_ref().is_some_and(|c| c.serial == printer.serial) {
                    self.drop_connection();
                }
                let internal = self.internal.clone();
                thread::spawn(move || {
                    let result = lan::test_connection(&printer, Duration::from_secs(15))
                        .map(|_| *printer);
                    internal.send(Internal::Added(result)).ok();
                });
            }
            LanCmd::SetModel(serial, model) => {
                if let Some(p) = self.saved.iter_mut().find(|p| p.serial == serial) {
                    p.model = model;
                    lan::store::save(&self.saved).ok();
                }
            }
            LanCmd::Remove(serial) => {
                if self.selected.as_deref() == Some(serial.as_str()) {
                    self.drop_connection();
                    self.selected = None;
                }
                if let Ok(list) = lan::store::remove(&serial) {
                    self.saved = list;
                }
            }
        }
        true
    }

    fn discovered(&mut self, d: lan::Discovered) {
        // DHCP видав принтеру нову IP: оновлюємо збережену й перепідключаємось.
        if let Some(saved) = self.saved.iter_mut().find(|p| p.serial == d.serial)
            && saved.ip != d.ip
        {
            saved.ip = d.ip;
            lan::store::save(&self.saved).ok();
            if self.conn.as_ref().is_some_and(|c| c.serial == d.serial) {
                let serial = d.serial.clone();
                self.drop_connection();
                self.next_connect = Instant::now();
                self.selected = Some(serial);
            }
        }
        self.seen.insert(d.serial.clone(), (d, Instant::now()));
    }
}

fn worker(commands: mpsc::Receiver<LanCmd>, ui: async_mpsc::UnboundedSender<CloudMsg>) {
    let (internal, internal_rx) = mpsc::channel();
    let mut w = Worker {
        ui,
        internal,
        saved: lan::store::load(),
        seen: HashMap::new(),
        selected: None,
        conn: None,
        connecting: false,
        next_connect: Instant::now(),
        backoff: Duration::from_secs(5),
        status: None,
        camera: None,
        printing: None,
        last_printers: vec![("".into(), "".into(), false)],
        last_found: Vec::new(),
        heartbeat: Instant::now(),
    };
    let discovery = match lan::discovery::Discovery::start() {
        Ok(pair) => Some(pair),
        Err(e) => {
            if paths::env("SLICENV_DEBUG").is_some() {
                eprintln!("[lan] виявлення недоступне: {e}");
            }
            None
        }
    };
    w.publish_lists();
    loop {
        if w.ui.is_closed() {
            w.drop_connection();
            return;
        }
        while let Ok(cmd) = commands.try_recv() {
            if !w.command(cmd) {
                return;
            }
        }
        while let Ok(msg) = internal_rx.try_recv() {
            w.on_internal(msg);
        }
        if let Some((_, rx)) = &discovery {
            let found: Vec<_> = rx.try_iter().collect();
            for d in found {
                w.discovered(d);
            }
        }
        w.maybe_connect();
        let events: Vec<_> = w
            .conn
            .as_ref()
            .map(|c| c.events.try_iter().collect())
            .unwrap_or_default();
        for event in events {
            w.on_event(event);
        }
        // Пасивний принтер шле лише зміни; охоронцю руху потрібен свіжий знімок (30 с).
        if w.heartbeat.elapsed() > Duration::from_secs(10) {
            w.heartbeat = Instant::now();
            if let Some(conn) = &w.conn {
                conn.client.request_status().ok();
            }
        }
        w.publish_lists();
        thread::sleep(Duration::from_millis(100));
    }
}

/// Обробка `CloudMsg::Lan` в інтерфейсі.
pub fn handle(ws: &mut Workspace, msg: LanMsg, window: &mut Window, cx: &mut Context<Workspace>) {
    let cloud = &mut ws.cloud;
    match msg {
        LanMsg::Printers(list) => {
            cloud.lan.printers = list;
            cloud.rebuild_printers();
            if cloud.selected.is_none()
                && let Some(dev) = cloud.lan.printers.first().map(|p| p.dev_id.clone())
            {
                cloud.select(Some(dev.clone()), window);
                cloud.send(CloudCmd::Select(dev));
            }
        }
        LanMsg::Link(dev, up) => {
            if up {
                cloud.lan.up.insert(dev);
            } else {
                cloud.lan.up.remove(&dev);
            }
        }
        LanMsg::Found(list) => cloud.lan.found = list,
        LanMsg::Problem(p) => cloud.lan.problem = p.map(Into::into),
        LanMsg::Upload(p) => cloud.lan.upload = p,
        LanMsg::Added(result) => {
            cloud.lan.adding = false;
            if let Some(wizard) = &mut ws.wizard {
                wizard.on_lan_added(result.as_ref().ok());
            }
            let cloud = &mut ws.cloud;
            match result {
                Ok(dev) => {
                    cloud.lan.add_error = None;
                    cloud.select(Some(dev), window);
                    ws.close_modal(cx);
                    ws.toast(ToastKind::Success, tr("Принтер у мережі додано"), cx);
                }
                Err(text) => cloud.lan.add_error = Some(text.into()),
            }
        }
    }
}

impl CloudState {
    /// Чи є зв’язок із вибраним принтером: LAN — власне MQTT, інакше — хмара.
    pub fn link_up(&self) -> bool {
        match &self.selected {
            Some(dev) if self.lan.has(dev) => self.lan.is_up(dev),
            Some(dev) if self.klipper.has(dev) => self.klipper.is_up(dev),
            _ => self.connected,
        }
    }
}

impl Workspace {
    /// Поля модалки й запуск робітника; викликається з `start_cloud`.
    pub(super) fn start_lan(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> async_mpsc::UnboundedReceiver<CloudMsg> {
        let (tx, rx) = spawn();
        self.cloud.lan.commands = Some(tx);
        let mut input = |placeholder: &str, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(tr(placeholder)))
        };
        self.cloud.lan.inputs = Some(AddInputs {
            ip: input("IP-адреса, напр. 192.168.1.50", cx),
            serial: input("Серійний номер", cx),
            // Маска лише малює крапки: `InputState::value()` повертає введений текст.
            code: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(tr("Код доступу (8 символів)"))
                    .masked(true)
            }),
        });
        rx
    }

    fn submit_lan(&mut self, cx: &mut Context<Self>) {
        let lan = &mut self.cloud.lan;
        let Some(inputs) = &lan.inputs else { return };
        let value = |e: &gpui::Entity<InputState>| e.read(cx).value().to_string();
        match LanPrinter::new(&value(&inputs.serial), &value(&inputs.ip), &value(&inputs.code)) {
            Ok(mut printer) => {
                if !lan.add_model.is_empty() {
                    printer.model = lan.add_model.clone();
                }
                lan.adding = true;
                lan.add_error = None;
                lan.send(LanCmd::Add(Box::new(printer)));
            }
            Err(error) => lan.add_error = Some(error_text(&error).into()),
        }
        cx.notify();
    }

    /// Модалка «Принтер у локальній мережі»; `found` заповнює IP і серійник.
    pub(crate) fn open_lan_dialog(
        &mut self,
        found: Option<Found>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cloud.lan.add_error = None;
        // Модель: з анонсу; для ручного вводу — з вибраного профілю принтера (змінюється в модалці).
        self.cloud.lan.add_manual = found.is_none();
        self.cloud.lan.add_model = match &found {
            Some(f) => f.model.clone(),
            None => self
                .presets
                .as_ref()
                .and_then(|s| s.machine_model_id(&self.selection.machine))
                .filter(|m| MODELS.contains(&m.as_str()))
                .unwrap_or_default(),
        };
        if let Some(inputs) = &self.cloud.lan.inputs {
            let (ip, serial) = found
                .as_ref()
                .map(|f| (f.ip.clone(), f.serial.clone()))
                .unwrap_or_default();
            for (entity, text) in [(&inputs.ip, ip), (&inputs.serial, serial), (&inputs.code, String::new())] {
                entity.update(cx, |s, cx| s.set_value(text, window, cx));
            }
            inputs.code.update(cx, |s, cx| s.set_masked(true, window, cx));
        }
        self.open_modal(
            std::rc::Rc::new(lan_dialog),
            window,
            cx,
        );
    }
}

fn lan_dialog(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> gpui::AnyElement {
    let z = zed();
    let lan = &ws.cloud.lan;
    let Some(inputs) = &lan.inputs else {
        return div().into_any_element();
    };
    let label = |text: &str| {
        div()
            .text_xs()
            .text_color(z.text_muted)
            .child(tr(text))
    };
    let field = |entity: &gpui::Entity<InputState>, window: &mut Window, cx: &mut Context<Workspace>| {
        crate::widgets::field(entity, gpui_component::input::Input::new(entity), window, cx)
    };
    let [ip_field, serial_field] = [&inputs.ip, &inputs.serial].map(|e| field(e, window, cx));
    let code_field = crate::widgets::field(
        &inputs.code,
        gpui_component::input::Input::new(&inputs.code).mask_toggle(),
        window,
        cx,
    );
    let model_picker = model_chips("lan-add-model", &lan.add_model, cx, |ws, code, _| {
        ws.cloud.lan.add_model = code;
    });
    let saved: Vec<_> = lan.printers.iter().enumerate().map(|(i, p)| {
        let serial = p.dev_id.as_str().to_owned();
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(28.))
            .text_sm()
            .child(ui::icon(IconName::Printer).text_color(z.text_muted))
            .child(
                div().flex_1().truncate().child(format!(
                    "{} · {}",
                    if p.name.is_empty() { lan::model_name(&p.model).to_owned() } else { p.name.clone() },
                    p.dev_id
                )),
            )
            .child(
                ui::icon_button(("lan-remove", i), IconName::Trash, "Видалити з LAN").on_click(cx.listener(
                    move |ws, _, window, cx| {
                        if ws.cloud.lan.has(&DeviceId::new(serial.clone())) {
                            ws.cloud.lan.send(LanCmd::Remove(serial.clone()));
                            if ws.cloud.selected.as_ref().map(DeviceId::as_str) == Some(serial.as_str()) {
                                ws.cloud.select(None, window);
                            }
                        }
                        cx.notify();
                    },
                )),
            )
    }).collect();
    let body = div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .p(px(12.))
        .child(
            div()
                .text_sm()
                .text_color(z.text_muted)
                .child(tr("Принтер і комп’ютер мають бути в одній мережі. IP-адреса, серійний номер і код доступу — на екрані принтера: Налаштування → Мережа (WLAN)."))
        )
        .child(label("IP-адреса"))
        .child(ip_field)
        .child(label("Серійний номер"))
        .child(serial_field)
        .child(label("Код доступу"))
        .child(code_field)
        .when(lan.add_manual, |d| {
            d.child(label("Модель"))
                .child(model_picker)
                .child(
                    div()
                        .text_xs()
                        .text_color(z.text_placeholder)
                        .child(tr("Без моделі профіль нарізки нема з чим звірити, і «Друкувати» буде неактивна.")),
                )
        })
        .child(
            div()
                .pt(px(4.))
                .text_xs()
                .text_color(z.text_placeholder)
                .child(tr("Для керування й друку без хмари на новіших прошивках увімкни на принтері LAN Only Mode і Developer Mode. Перегляд стану працює і без них.")),
        )
        .children(lan.add_error.clone().map(|e| div().text_sm().text_color(z.error).child(e)))
        .when(!lan.printers.is_empty(), |d| {
            d.child(ui::divider())
                .child(label("Збережені в LAN"))
                .children(saved)
        });
    let adding = lan.adding;
    crate::widgets::modal(
        "Принтер у локальній мережі",
        480.,
        body,
        Some(
            div()
                .flex()
                .gap(px(6.))
                .items_center()
                .when(adding, |d| d.child(ui::spinner("lan-add-spin")))
                .child(
                    ui::ghost_button("lan-cancel", None, "Скасувати")
                        .on_click(cx.listener(|ws, _, _, cx| ws.close_modal(cx))),
                )
                .child(
                    ui::accent_button("lan-add", IconName::Plus, "Перевірити й додати", !adding)
                        .when(!adding, |b| {
                            b.on_click(cx.listener(|ws, _, _, cx| ws.submit_lan(cx)))
                        }),
                )
                .into_any_element(),
        ),
    )
}

/// Рядок кнопок-моделей. Без моделі «Друкувати» неактивна: профіль нема з чим звірити.
pub fn model_chips(
    id: &'static str,
    current: &str,
    cx: &mut Context<Workspace>,
    pick: fn(&mut Workspace, String, &mut Context<Workspace>),
) -> gpui::Div {
    let z = zed();
    div()
        .flex()
        .flex_wrap()
        .gap(px(4.))
        .children(MODELS.into_iter().enumerate().map(|(i, code)| {
            let on = current == code;
            div()
                .id((id, i))
                .px(px(6.))
                .h(px(22.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .border_1()
                .text_xs()
                .cursor_pointer()
                .map(|d| {
                    if on {
                        d.border_color(z.accent_border).bg(z.accent_bg).text_color(z.accent)
                    } else {
                        d.border_color(z.border_variant).text_color(z.text_muted).hover(|s| s.bg(z.element_hover))
                    }
                })
                .child(lan::model_name(code).to_owned())
                .on_click(cx.listener(move |ws, _, _, cx| {
                    pick(ws, code.to_owned(), cx);
                    cx.notify();
                }))
        }))
}

/// Знайдені в мережі, але не додані Bambu-принтери — рядки згорнутої групи в списку доку.
pub fn found_rows(cloud: &CloudState, filter: &str, cx: &mut Context<Workspace>) -> Vec<gpui::AnyElement> {
    let z = zed();
    cloud
        .lan
        .found
        .iter()
        .enumerate()
        .filter(|(_, f)| super::matches_filter(filter, &[&f.name, &f.serial, lan::model_name(&f.model), &f.ip]))
        .map(|(i, f)| {
            let item = f.clone();
            let title = if f.name.is_empty() { f.serial.clone() } else { f.name.clone() };
            ui::list_item(("lan-found", i), 0, false)
                .h(px(40.))
                .px(px(6.))
                .gap(px(8.))
                .child(ui::icon(IconName::Wifi).flex_none().text_color(z.text_muted))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(div().truncate().text_sm().child(title))
                        .child(
                            div()
                                .text_xs()
                                .text_color(z.text_muted)
                                .truncate()
                                .child(format!("{} · {}", lan::model_name(&f.model), f.ip)),
                        ),
                )
                .child(ui::icon(IconName::Plus).flex_none().text_color(z.text_placeholder))
                .on_click(cx.listener(move |ws, _, window, cx| {
                    ws.open_lan_dialog(Some(item.clone()), window, cx)
                }))
                .into_any_element()
        })
        .collect()
}

/// Те, що стосується вибраного LAN-принтера зараз: вивантаження, проблема з’єднання, невідома
/// модель. Порожньо — нічого не показується.
pub fn context_lines(cloud: &CloudState, cx: &mut Context<Workspace>) -> Vec<gpui::AnyElement> {
    let z = zed();
    let unknown_model = cloud.selected.as_ref().is_some_and(|dev| {
        cloud
            .lan
            .printers
            .iter()
            .any(|p| &p.dev_id == dev && p.model.is_empty())
    });
    let mut out: Vec<gpui::AnyElement> = Vec::new();
    if let Some(p) = cloud.lan.upload {
        out.push(
            div()
                .text_xs()
                .text_color(z.accent)
                .child(crate::tr_format!("Завантаження на принтер: {pct:.0}%", pct = p * 100.0))
                .into_any_element(),
        );
    }
    if let Some(problem) = cloud
        .selected
        .as_ref()
        .filter(|d| cloud.lan.has(d))
        .and(cloud.lan.problem.clone())
    {
        out.push(div().text_xs().text_color(z.warning).whitespace_normal().child(problem).into_any_element());
    }
    if unknown_model {
        out.push(
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(div().text_xs().text_color(z.warning).whitespace_normal().child(tr(
                    "Модель принтера ще невідома — без неї «Друкувати» неактивна. Виберіть модель:",
                )))
                .child(model_chips("lan-set-model", "", cx, |ws, code, _| {
                    if let Some(dev) = ws.cloud.selected.clone() {
                        ws.cloud.lan.send(LanCmd::SetModel(dev.as_str().to_owned(), code));
                    }
                }))
                .into_any_element(),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn printer(serial: &str, online: bool) -> cloud::Printer {
        cloud::Printer {
            dev_id: DeviceId::new(serial),
            name: String::new(),
            model: "N2S".into(),
            online,
        }
    }

    #[test]
    fn routing_sends_lan_commands_to_lan_and_rest_to_cloud() {
        let (tx, rx) = mpsc::channel();
        let mut ui = LanUi {
            commands: Some(tx),
            ..Default::default()
        };
        ui.printers = vec![printer("LAN000000001", true)];
        let lan_dev = DeviceId::new("LAN000000001");
        let cloud_dev = DeviceId::new("CLD000000001");
        assert!(ui.route(Some(&lan_dev), CloudCmd::Control(lan_dev.clone(), Control::Pause)).is_none());
        assert!(matches!(rx.try_recv(), Ok(LanCmd::Control(..))));
        assert!(matches!(
            ui.route(Some(&cloud_dev), CloudCmd::Control(cloud_dev.clone(), Control::Pause)),
            Some(CloudCmd::Control(..))
        ));
        assert!(rx.try_recv().is_err());
        assert!(ui.route(Some(&lan_dev), CloudCmd::Camera(true)).is_none());
        assert!(matches!(rx.try_recv(), Ok(LanCmd::Camera(true))));
        assert!(matches!(ui.route(Some(&cloud_dev), CloudCmd::Camera(true)), Some(CloudCmd::Camera(true))));
        // Вибір хмарного принтера йде обом: LAN-робітник від’єднується.
        assert!(matches!(ui.route(None, CloudCmd::Select(cloud_dev.clone())), Some(CloudCmd::Select(_))));
        assert!(matches!(rx.try_recv(), Ok(LanCmd::Select(_))));
        assert!(ui.route(None, CloudCmd::Select(lan_dev.clone())).is_none());
        assert!(matches!(ui.route(None, CloudCmd::Tasks), Some(CloudCmd::Tasks)));
    }

    #[test]
    fn union_prefers_lan_entry_for_same_printer() {
        let mut ui = LanUi::default();
        ui.set_cloud_list(vec![printer("BOTH00000001", false), printer("CLD000000001", true)]);
        ui.printers = vec![printer("BOTH00000001", true)];
        let union = ui.union();
        assert_eq!(union.len(), 2);
        assert!(union.iter().any(|p| p.dev_id.as_str() == "BOTH00000001" && p.online));
    }

    #[test]
    fn model_codes_from_get_version_product() {
        assert_eq!(model_from_product("Bambu Lab A1"), Some("N2S"));
        assert_eq!(model_from_product("Bambu Lab A1 mini"), Some("N1"));
        assert_eq!(model_from_product("Bambu Lab H2D"), Some("O1D"));
        assert_eq!(model_from_product(""), None);
    }
}
