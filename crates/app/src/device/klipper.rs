//! Принтери Klipper через Moonraker на сторінці «Пристрій» — поруч із хмарою й LAN Bambu.
//!
//! Робочий потік `klipper-worker` опитує вибраний принтер по HTTP (раз на 2 с), решту
//! збережених — рідше, і шле в інтерфейс ті самі `CloudMsg` (статус, камера, результат
//! друку), що й хмара, плюс `CloudMsg::Klipper` для власного стану. API-ключі живуть лише в
//! `moonraker::store` (файл 0600); у лог і повідомлення вони не потрапляють.
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
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
use gpui::{AnyElement, Context, SharedString, Window, div, prelude::*, px};
use gpui_component::input::InputState;
use moonraker::{
    Client, JobStatus, KlippyState, MoonrakerError, PrintState, Probe, SavedPrinter,
    UploadOptions,
};

use super::{CloudCmd, CloudMsg, CloudState, guard};
use crate::{
    i18n::tr,
    theme::zed,
    ui::{self, IconName},
    widgets::ToastKind,
    workspace::Workspace,
};

/// Значення `cloud::Printer::model` для принтерів Klipper.
pub const MODEL: &str = "Klipper";

pub enum KlipperCmd {
    Select(DeviceId),
    Refresh,
    Control(DeviceId, Control),
    Print(Box<PrintJob>),
    Camera(bool),
    /// Перевірити з’єднання й доступ і, якщо вдалося, зберегти.
    Add(Box<SavedPrinter>),
    Remove(String),
    Shutdown,
}

pub struct PrintJob {
    pub dev: DeviceId,
    pub gcode_3mf: PathBuf,
    pub plate: u32,
    pub remote: String,
    pub options: UploadOptions,
}

/// Що відомо про Moonraker/Klippy вибраного принтера (для сторінки й діалогу друку).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Info {
    pub ready: bool,
    /// Перекладений стан Klippy.
    pub klippy: String,
    pub message: String,
    pub moonraker: String,
    pub klipper: String,
    pub hostname: String,
    pub web_url: String,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub printer: cloud::Printer,
    pub address: String,
    pub has_key: bool,
}

impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        let (a, b) = (&self.printer, &other.printer);
        (&a.dev_id, &a.name, &a.model, a.online, &self.address, self.has_key)
            == (&b.dev_id, &b.name, &b.model, b.online, &other.address, other.has_key)
    }
}

pub enum KlipperMsg {
    Printers(Vec<Entry>),
    Link(DeviceId, bool),
    Info(DeviceId, Box<Info>),
    /// Помилка з’єднання чи стан Klippy вибраного принтера; `None` — усе гаразд.
    Problem(Option<String>),
    /// Частка завантаження файлу; `None` — завершено.
    Upload(Option<f32>),
    Added(Result<DeviceId, String>),
}

impl KlipperMsg {
    /// Для `SLICENV_DEBUG`: без адрес і ключів.
    pub fn describe(&self) -> String {
        match self {
            Self::Printers(list) => format!("[klipper] збережених принтерів: {}", list.len()),
            Self::Link(_, up) => format!("[klipper] з’єднання {up}"),
            Self::Info(_, info) => format!("[klipper] klippy ready={}", info.ready),
            Self::Problem(p) => format!("[klipper] проблема: {}", p.is_some()),
            Self::Upload(p) => format!("[klipper] завантаження: {p:?}"),
            Self::Added(r) => format!("[klipper] додавання: {}", r.is_ok()),
        }
    }
}

pub struct AddInputs {
    pub name: gpui::Entity<InputState>,
    pub address: gpui::Entity<InputState>,
    pub key: gpui::Entity<InputState>,
}

#[derive(Default)]
pub struct KlipperUi {
    commands: Option<mpsc::Sender<KlipperCmd>>,
    pub entries: Vec<Entry>,
    up: HashSet<DeviceId>,
    pub info: Option<(DeviceId, Info)>,
    pub adding: bool,
    pub add_error: Option<SharedString>,
    pub problem: Option<SharedString>,
    pub upload: Option<f32>,
    pub inputs: Option<AddInputs>,
    /// Діалог друку: замінити файл з тією самою назвою.
    pub overwrite: bool,
}

impl KlipperUi {
    pub fn has(&self, dev: &DeviceId) -> bool {
        self.entries.iter().any(|e| &e.printer.dev_id == dev)
    }
    pub fn is_up(&self, dev: &DeviceId) -> bool {
        self.up.contains(dev)
    }
    pub fn printers(&self) -> impl Iterator<Item = &cloud::Printer> {
        self.entries.iter().map(|e| &e.printer)
    }
    pub fn entry(&self, dev: &DeviceId) -> Option<&Entry> {
        self.entries.iter().find(|e| &e.printer.dev_id == dev)
    }
    pub fn info_for(&self, dev: &DeviceId) -> Option<&Info> {
        self.info.as_ref().filter(|(d, _)| d == dev).map(|(_, i)| i)
    }
    pub fn send(&self, cmd: KlipperCmd) {
        if let Some(tx) = &self.commands {
            tx.send(cmd).ok();
        }
    }
    /// Команду для принтера Klipper забирає собі (`None`), решту повертає далі (LAN, хмара).
    pub fn route(&self, selected: Option<&DeviceId>, cmd: CloudCmd) -> Option<CloudCmd> {
        let mine = selected.is_some_and(|d| self.has(d));
        match cmd {
            CloudCmd::Select(dev) => {
                self.send(KlipperCmd::Select(dev.clone()));
                // Далі йде LAN-робітнику (щоб від’єднався); хмарі `CloudState::send` не віддасть.
                Some(CloudCmd::Select(dev))
            }
            CloudCmd::Control(dev, control) if self.has(&dev) => {
                self.send(KlipperCmd::Control(dev, control));
                None
            }
            // Друк Klipper іде окремою командою з вибором «лише завантажити».
            CloudCmd::Print(req) if self.has(&req.dev_id) => None,
            CloudCmd::Camera(on) if mine => {
                self.send(KlipperCmd::Camera(on));
                None
            }
            CloudCmd::Versions | CloudCmd::Tasks if mine => None,
            CloudCmd::Thumbnail(key) if self.has(&key.dev) => None,
            CloudCmd::Refresh => {
                self.send(KlipperCmd::Refresh);
                Some(CloudCmd::Refresh)
            }
            CloudCmd::Shutdown => {
                self.send(KlipperCmd::Shutdown);
                Some(CloudCmd::Shutdown)
            }
            other => Some(other),
        }
    }
}

/// Перевірка команди до відправки: лише пауза, продовження й скасування, і лише у
/// відповідному стані. Решта керування (температури, рух) для Klipper поки не підтримана.
pub fn check(
    ui: &KlipperUi,
    selected: Option<&DeviceId>,
    status: Option<&PrinterStatus>,
    dev: &DeviceId,
    control: Control,
) -> Result<(), guard::Blocked> {
    use guard::Blocked;
    if selected != Some(dev) {
        return Err(Blocked::WrongDevice);
    }
    if !ui.is_up(dev) {
        return Err(Blocked::LanDisconnected);
    }
    let state = status
        .ok_or(Blocked::NoStatus)?
        .gcode_state
        .as_deref();
    match control {
        Control::Pause if state == Some("RUNNING") => Ok(()),
        Control::Resume if state == Some("PAUSE") => Ok(()),
        Control::Stop if matches!(state, Some("RUNNING" | "PAUSE")) => Ok(()),
        Control::Pause | Control::Resume | Control::Stop => Err(Blocked::WrongState),
        _ => Err(Blocked::Rejected(tr(
            "Для Klipper поки підтримано лише паузу, продовження й скасування друку",
        ))),
    }
}

pub fn klippy_label(state: &KlippyState) -> String {
    match state {
        KlippyState::Ready => tr("готовий"),
        KlippyState::Startup => tr("запускається"),
        KlippyState::Shutdown => tr("зупинений (shutdown)"),
        KlippyState::Error => tr("помилка"),
        KlippyState::Disconnected => tr("не з’єднаний з Moonraker"),
        KlippyState::Other(s) => s.clone(),
    }
}

/// Текст помилки для інтерфейсу: перекладена фраза + деталь.
pub fn error_text(error: &MoonrakerError) -> String {
    let detail = match error {
        MoonrakerError::KlippyNotReady { state, message } if !message.trim().is_empty() => {
            format!("{}: {}", klippy_label(state), message.trim())
        }
        MoonrakerError::KlippyNotReady { state, .. } => klippy_label(state),
        other => other.detail(),
    };
    if detail.is_empty() {
        tr(error.key())
    } else {
        format!("{} ({detail})", tr(error.key()))
    }
}

/// Стан Moonraker у форму знімка Bambu, яку вже вміє показувати сторінка «Пристрій».
pub fn to_printer_status(s: &JobStatus) -> PrinterStatus {
    let state = match &s.state {
        PrintState::Printing => "RUNNING",
        PrintState::Paused => "PAUSE",
        PrintState::Complete => "FINISH",
        PrintState::Error => "FAILED",
        PrintState::Standby | PrintState::Cancelled | PrintState::Other(_) => "IDLE",
    };
    let active = s.state.is_active();
    let homed = s.homed_axes.to_ascii_lowercase();
    let home_flag = ['x', 'y', 'z']
        .iter()
        .enumerate()
        .filter(|(_, a)| homed.contains(**a))
        .fold(0u32, |acc, (i, _)| acc | (1 << i));
    let name = Path::new(&s.filename)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty());
    PrinterStatus {
        gcode_state: Some(state.into()),
        mc_percent: s.progress.map(|p| p * 100.0),
        mc_remaining_time: s.remaining_secs().map(|secs| (secs / 60.0).round()),
        layer_num: s.current_layer.filter(|_| active),
        total_layer_num: s.total_layer.filter(|_| active),
        nozzle_temper: s.extruder.temperature,
        nozzle_target_temper: s.extruder.target,
        bed_temper: s.bed.temperature,
        bed_target_temper: s.bed.target,
        home_flag: Some(home_flag),
        print_type: Some("local".into()),
        gcode_file: (!s.filename.is_empty()).then(|| s.filename.clone()),
        subtask_name: name,
        ..Default::default()
    }
}

fn as_cloud(p: &SavedPrinter, online: bool) -> cloud::Printer {
    cloud::Printer {
        dev_id: DeviceId::new(p.id.clone()),
        name: p.name.clone(),
        model: MODEL.into(),
        online,
    }
}

/// `plate_N.gcode` поруч із `*.gcode.3mf`; якщо рушій його не лишив — витягує з архіву.
pub fn plain_gcode(gcode_3mf: &Path, plate: u32) -> Result<PathBuf, String> {
    let target = moonraker::plate_gcode(gcode_3mf, plate);
    if target.is_file() {
        return Ok(target);
    }
    let file = std::fs::File::open(gcode_3mf).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    let mut entry = archive
        .by_name(&format!("Metadata/plate_{plate}.gcode"))
        .map_err(|e| e.to_string())?;
    let tmp = target.with_extension("gcode.part");
    let mut out = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &target).map_err(|e| e.to_string())?;
    Ok(target)
}

enum Internal {
    Polled(String, Box<Result<(Probe, Option<JobStatus>), MoonrakerError>>),
    Added(Result<SavedPrinter, MoonrakerError>),
    Printed(Result<String, String>),
    Controlled(Result<(), MoonrakerError>),
}

/// Запускає робітника. `SLICENV_KLIPPER_OFF` вимикає Klipper повністю.
pub fn spawn() -> (mpsc::Sender<KlipperCmd>, async_mpsc::UnboundedReceiver<CloudMsg>) {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (ui_tx, ui_rx) = async_mpsc::unbounded();
    if paths::env("SLICENV_KLIPPER_OFF").is_none() {
        thread::Builder::new()
            .name("klipper-worker".into())
            .spawn(move || worker(cmd_rx, ui_tx))
            .ok();
    }
    (cmd_tx, ui_rx)
}

const POLL_SELECTED: Duration = Duration::from_secs(2);
const POLL_OTHERS: Duration = Duration::from_secs(30);

struct Worker {
    ui: async_mpsc::UnboundedSender<CloudMsg>,
    internal: mpsc::Sender<Internal>,
    saved: Vec<SavedPrinter>,
    selected: Option<String>,
    online: HashMap<String, bool>,
    polling: HashSet<String>,
    next_poll: HashMap<String, Instant>,
    backoff: Duration,
    status: Option<JobStatus>,
    camera: Option<Arc<AtomicBool>>,
    printing: Option<Arc<AtomicBool>>,
    last_entries: Option<Vec<Entry>>,
}

impl Worker {
    fn send(&self, msg: CloudMsg) {
        self.ui.unbounded_send(msg).ok();
    }
    fn klipper(&self, msg: KlipperMsg) {
        self.send(CloudMsg::Klipper(msg));
    }
    fn printer(&self, id: &str) -> Option<&SavedPrinter> {
        self.saved.iter().find(|p| p.id == id)
    }
    fn client(&self, id: &str) -> Option<Client> {
        self.printer(id).and_then(|p| Client::from_saved(p).ok())
    }

    fn publish(&mut self) {
        let entries: Vec<Entry> = self
            .saved
            .iter()
            .map(|p| Entry {
                printer: as_cloud(p, self.online.get(&p.id).copied().unwrap_or(false)),
                address: p.address.clone(),
                has_key: p.api_key.is_some(),
            })
            .collect();
        if self.last_entries.as_ref() != Some(&entries) {
            self.last_entries = Some(entries.clone());
            self.klipper(KlipperMsg::Printers(entries));
        }
    }

    fn stop_camera(&mut self) {
        if let Some(stop) = self.camera.take() {
            stop.store(true, Ordering::Release);
        }
    }

    fn select(&mut self, dev: DeviceId) {
        let id = dev.as_str().to_owned();
        if self.selected.as_deref() == Some(id.as_str()) {
            return;
        }
        self.stop_camera();
        self.status = None;
        if let Some(old) = self.selected.take() {
            self.klipper(KlipperMsg::Link(DeviceId::new(old), false));
        }
        self.klipper(KlipperMsg::Problem(None));
        self.selected = self.printer(&id).map(|_| id.clone());
        if self.selected.is_some() {
            self.next_poll.insert(id, Instant::now());
            self.backoff = Duration::from_secs(5);
        }
    }

    fn maybe_poll(&mut self) {
        let now = Instant::now();
        let ids: Vec<String> = self.saved.iter().map(|p| p.id.clone()).collect();
        for id in ids {
            if self.polling.contains(&id)
                || self.next_poll.get(&id).is_some_and(|at| now < *at)
            {
                continue;
            }
            let Some(client) = self.client(&id) else { continue };
            let full = self.selected.as_deref() == Some(id.as_str());
            self.polling.insert(id.clone());
            // Поки не прийде відповідь, наступний запит не плануємо.
            self.next_poll.insert(id.clone(), now + Duration::from_secs(3600));
            let internal = self.internal.clone();
            thread::spawn(move || {
                let result = client.probe().and_then(|probe| {
                    let status = if full && probe.klippy_state() == KlippyState::Ready {
                        Some(client.status()?)
                    } else {
                        None
                    };
                    Ok((probe, status))
                });
                internal.send(Internal::Polled(id, Box::new(result))).ok();
            });
        }
    }

    fn on_polled(&mut self, id: String, result: Result<(Probe, Option<JobStatus>), MoonrakerError>) {
        self.polling.remove(&id);
        let selected = self.selected.as_deref() == Some(id.as_str());
        let dev = DeviceId::new(id.clone());
        let next = if selected { POLL_SELECTED } else { POLL_OTHERS };
        match result {
            Ok((probe, status)) => {
                self.online.insert(id.clone(), true);
                self.next_poll.insert(id.clone(), Instant::now() + next);
                if !selected {
                    return;
                }
                self.backoff = Duration::from_secs(5);
                let state = probe.klippy_state();
                let web_url = self.client(&id).map(|c| c.web_url()).unwrap_or_default();
                let message = probe
                    .printer
                    .as_ref()
                    .map(|p| p.state_message.trim().to_owned())
                    .unwrap_or_default();
                let info = Info {
                    ready: state == KlippyState::Ready,
                    klippy: klippy_label(&state),
                    message: message.clone(),
                    moonraker: probe.server.moonraker_version.clone(),
                    klipper: probe
                        .printer
                        .as_ref()
                        .map(|p| p.software_version.clone())
                        .unwrap_or_default(),
                    hostname: probe
                        .printer
                        .as_ref()
                        .map(|p| p.hostname.clone())
                        .unwrap_or_default(),
                    web_url,
                };
                self.klipper(KlipperMsg::Link(dev.clone(), true));
                self.klipper(KlipperMsg::Info(dev.clone(), Box::new(info)));
                match status {
                    Some(status) => {
                        self.klipper(KlipperMsg::Problem(None));
                        self.send(CloudMsg::Status(dev, Box::new(to_printer_status(&status))));
                        self.status = Some(status);
                    }
                    None => {
                        self.status = None;
                        self.klipper(KlipperMsg::Problem(Some(error_text(
                            &MoonrakerError::KlippyNotReady { state, message },
                        ))));
                    }
                }
            }
            Err(error) => {
                self.online.insert(id.clone(), false);
                if !selected {
                    self.next_poll.insert(id, Instant::now() + POLL_OTHERS);
                    return;
                }
                self.status = None;
                self.klipper(KlipperMsg::Link(dev, false));
                self.klipper(KlipperMsg::Problem(Some(error_text(&error))));
                // Невірний ключ не виправиться повтором: чекаємо довше.
                let wait = if error == MoonrakerError::Unauthorized {
                    Duration::from_secs(60)
                } else {
                    self.backoff
                };
                self.next_poll.insert(id, Instant::now() + wait);
                self.backoff = (self.backoff * 2).min(Duration::from_secs(30));
            }
        }
    }

    fn control(&mut self, dev: DeviceId, control: Control) {
        let Some(client) = self.client(dev.as_str()) else { return };
        let state = self.status.as_ref().map(|s| s.state.clone());
        let allowed = match control {
            Control::Pause => state == Some(PrintState::Printing),
            Control::Resume => state == Some(PrintState::Paused),
            Control::Stop => state.as_ref().is_some_and(PrintState::is_active),
            _ => false,
        };
        if self.selected.as_deref() != Some(dev.as_str()) || !allowed {
            self.send(CloudMsg::Blocked(guard::Blocked::WrongState.message()));
            return;
        }
        let internal = self.internal.clone();
        thread::spawn(move || {
            let result = match control {
                Control::Pause => client.pause(),
                Control::Resume => client.resume(),
                _ => client.cancel(),
            };
            internal.send(Internal::Controlled(result)).ok();
        });
    }

    fn print(&mut self, job: Box<PrintJob>) {
        let what = if job.options.start {
            tr("Друк не надіслано")
        } else {
            tr("Файл не завантажено")
        };
        let fail = |w: &Self, text: String| {
            w.send(CloudMsg::PrintSent(Err(format!("{what}: {text}"))));
        };
        if self.printing.is_some() {
            return fail(self, tr("попереднє завдання ще надсилається"));
        }
        let Some(client) = self.client(job.dev.as_str()) else {
            return fail(self, tr("принтер не збережений як Klipper"));
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.printing = Some(cancel.clone());
        let internal = self.internal.clone();
        let ui = self.ui.clone();
        thread::spawn(move || {
            let mut last = Instant::now() - Duration::from_secs(1);
            let mut progress = |sent: u64, total: u64| {
                if last.elapsed() > Duration::from_millis(200) || sent == total {
                    last = Instant::now();
                    let part = if total == 0 { 1.0 } else { sent as f32 / total as f32 };
                    ui.unbounded_send(CloudMsg::Klipper(KlipperMsg::Upload(Some(part)))).ok();
                }
            };
            let result = plain_gcode(&job.gcode_3mf, job.plate)
                .and_then(|path| {
                    client
                        .upload(&path, &job.remote, job.options, &mut progress, &cancel)
                        .map_err(|e| error_text(&e))
                })
                .map_err(|e| format!("{what}: {e}"))
                .and_then(|r| {
                    let name = r.path;
                    match (job.options.start, r.print_started, r.print_queued) {
                        (false, ..) => Ok(crate::tr_format!("Файл «{name}» завантажено на принтер")),
                        (true, true, _) => {
                            Ok(crate::tr_format!("Файл «{name}» завантажено, друк почався"))
                        }
                        (true, false, true) => {
                            Ok(crate::tr_format!("Файл «{name}» завантажено й додано в чергу друку"))
                        }
                        (true, false, false) => Err(crate::tr_format!(
                            "Файл «{name}» завантажено, але друк не почався: запустіть його в Mainsail чи Fluidd"
                        )),
                    }
                });
            internal.send(Internal::Printed(result)).ok();
        });
    }

    fn camera(&mut self, on: bool) {
        self.stop_camera();
        if !on {
            return;
        }
        let Some(client) = self.selected.as_deref().and_then(|id| self.client(id)) else {
            return;
        };
        let stop = Arc::new(AtomicBool::new(false));
        self.camera = Some(stop.clone());
        let ui = self.ui.clone();
        thread::spawn(move || {
            let note = (|| -> Option<String> {
                let cams = match client.webcams() {
                    Ok(c) => c,
                    Err(e) => return Some(error_text(&e)),
                };
                let Some(cam) = cams.into_iter().find(|c| client.snapshot_target(c).is_some())
                else {
                    return Some(tr(
                        "У Moonraker не налаштовано камери зі знімками (snapshot_url) — відкрийте Mainsail чи Fluidd",
                    ));
                };
                let mut failures = 0;
                while !stop.load(Ordering::Acquire) {
                    match client.snapshot(&cam) {
                        Ok(jpeg) => {
                            failures = 0;
                            if let Ok(decoded) =
                                image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg)
                            {
                                let decoded = match (cam.flip_horizontal, cam.flip_vertical) {
                                    (true, true) => decoded.rotate180(),
                                    (true, false) => decoded.fliph(),
                                    (false, true) => decoded.flipv(),
                                    _ => decoded,
                                };
                                let decoded = match cam.rotation {
                                    90 => decoded.rotate90(),
                                    180 => decoded.rotate180(),
                                    270 => decoded.rotate270(),
                                    _ => decoded,
                                };
                                let mut bgra = decoded.to_rgba8();
                                for px in bgra.pixels_mut() {
                                    px.0.swap(0, 2);
                                }
                                if ui.unbounded_send(CloudMsg::CameraFrame(bgra)).is_err() {
                                    return None;
                                }
                            }
                        }
                        Err(e) => {
                            failures += 1;
                            if failures >= 3 {
                                return Some(error_text(&e));
                            }
                        }
                    }
                    // Знімок раз на секунду: MJPEG-потік у GPUI тягнути нема як і нема потреби.
                    for _ in 0..10 {
                        if stop.load(Ordering::Acquire) {
                            return None;
                        }
                        thread::sleep(Duration::from_millis(100));
                    }
                }
                None
            })();
            if !stop.load(Ordering::Acquire) {
                ui.unbounded_send(CloudMsg::CameraStopped(note)).ok();
            }
        });
    }

    fn on_internal(&mut self, msg: Internal) {
        match msg {
            Internal::Polled(id, result) => self.on_polled(id, *result),
            Internal::Added(result) => match result {
                Ok(printer) => {
                    let dev = DeviceId::new(printer.id.clone());
                    match moonraker::store::upsert(printer) {
                        Ok(list) => {
                            self.saved = list;
                            self.online.insert(dev.as_str().to_owned(), true);
                            self.publish();
                            self.klipper(KlipperMsg::Added(Ok(dev.clone())));
                            self.selected = None;
                            self.select(dev);
                        }
                        Err(e) => self.klipper(KlipperMsg::Added(Err(format!(
                            "{}: {e}",
                            tr("Не вдалося зберегти налаштування Klipper")
                        )))),
                    }
                }
                Err(error) => self.klipper(KlipperMsg::Added(Err(error_text(&error)))),
            },
            Internal::Printed(result) => {
                self.printing = None;
                self.klipper(KlipperMsg::Upload(None));
                self.send(CloudMsg::PrintSent(result));
                self.poll_now();
            }
            Internal::Controlled(result) => {
                if let Err(error) = result {
                    self.send(CloudMsg::Blocked(format!(
                        "{}: {}",
                        tr("Команда не пройшла"),
                        error_text(&error)
                    )));
                }
                self.poll_now();
            }
        }
    }

    fn poll_now(&mut self) {
        if let Some(id) = &self.selected {
            self.next_poll.insert(id.clone(), Instant::now());
        }
    }

    /// `false` — завершити робітника.
    fn command(&mut self, cmd: KlipperCmd) -> bool {
        match cmd {
            KlipperCmd::Shutdown => {
                self.stop_camera();
                if let Some(cancel) = &self.printing {
                    cancel.store(true, Ordering::Release);
                }
                return false;
            }
            KlipperCmd::Select(dev) => self.select(dev),
            KlipperCmd::Refresh => {
                let now = Instant::now();
                for at in self.next_poll.values_mut() {
                    *at = (*at).min(now);
                }
            }
            KlipperCmd::Control(dev, control) => self.control(dev, control),
            KlipperCmd::Print(job) => self.print(job),
            KlipperCmd::Camera(on) => self.camera(on),
            KlipperCmd::Add(printer) => {
                let internal = self.internal.clone();
                thread::spawn(move || {
                    let result = Client::from_saved(&printer)
                        .and_then(|c| c.probe())
                        .map(|_| *printer);
                    internal.send(Internal::Added(result)).ok();
                });
            }
            KlipperCmd::Remove(id) => {
                if self.selected.as_deref() == Some(id.as_str()) {
                    self.stop_camera();
                    self.selected = None;
                    self.status = None;
                }
                self.online.remove(&id);
                self.next_poll.remove(&id);
                if let Ok(list) = moonraker::store::remove(&id) {
                    self.saved = list;
                }
            }
        }
        true
    }
}

fn worker(commands: mpsc::Receiver<KlipperCmd>, ui: async_mpsc::UnboundedSender<CloudMsg>) {
    let (internal, internal_rx) = mpsc::channel();
    let mut w = Worker {
        ui,
        internal,
        saved: moonraker::store::load(),
        selected: None,
        online: HashMap::new(),
        polling: HashSet::new(),
        next_poll: HashMap::new(),
        backoff: Duration::from_secs(5),
        status: None,
        camera: None,
        printing: None,
        last_entries: None,
    };
    w.publish();
    loop {
        if w.ui.is_closed() {
            w.stop_camera();
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
        w.maybe_poll();
        w.publish();
        thread::sleep(Duration::from_millis(100));
    }
}

// ---------------------------------------------------------------- інтерфейс

/// Обробка `CloudMsg::Klipper` в інтерфейсі.
pub fn handle(ws: &mut Workspace, msg: KlipperMsg, window: &mut Window, cx: &mut Context<Workspace>) {
    let cloud = &mut ws.cloud;
    match msg {
        KlipperMsg::Printers(list) => {
            cloud.klipper.entries = list;
            cloud.rebuild_printers();
            if cloud.selected.is_none()
                && let Some(dev) = cloud.klipper.entries.first().map(|e| e.printer.dev_id.clone())
            {
                cloud.select(Some(dev.clone()), window);
                cloud.send(CloudCmd::Select(dev));
            }
        }
        KlipperMsg::Link(dev, up) => {
            if up {
                cloud.klipper.up.insert(dev);
            } else {
                cloud.klipper.up.remove(&dev);
            }
        }
        KlipperMsg::Info(dev, info) => cloud.klipper.info = Some((dev, *info)),
        KlipperMsg::Problem(p) => {
            cloud.klipper.problem = p.map(Into::into);
            // Klippy не готовий: старий знімок «друкує» вводив би в оману.
            if cloud.klipper.problem.is_some()
                && cloud.selected.as_ref().is_some_and(|d| cloud.klipper.has(d))
            {
                cloud.status = None;
                cloud.updated = None;
            }
        }
        KlipperMsg::Upload(p) => cloud.klipper.upload = p,
        KlipperMsg::Added(result) => {
            cloud.klipper.adding = false;
            match result {
                Ok(dev) => {
                    cloud.klipper.add_error = None;
                    cloud.select(Some(dev), window);
                    ws.close_modal(cx);
                    ws.toast(ToastKind::Success, tr("Принтер Klipper додано"), cx);
                }
                Err(text) => cloud.klipper.add_error = Some(text.into()),
            }
        }
    }
}

impl Workspace {
    /// Поля модалки й запуск робітника; викликається з `start_cloud`.
    pub(super) fn start_klipper(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> async_mpsc::UnboundedReceiver<CloudMsg> {
        let (tx, rx) = spawn();
        self.cloud.klipper.commands = Some(tx);
        let mut input = |placeholder: &str, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(tr(placeholder)))
        };
        self.cloud.klipper.inputs = Some(AddInputs {
            name: input("Назва, напр. Ender 3 V3 SE", cx),
            address: input("Адреса, напр. 192.168.1.60 або ender.local:7125", cx),
            key: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(tr("API-ключ (якщо Moonraker його вимагає)"))
                    .masked(true)
            }),
        });
        rx
    }

    fn submit_klipper(&mut self, cx: &mut Context<Self>) {
        let k = &mut self.cloud.klipper;
        let Some(inputs) = &k.inputs else { return };
        let value = |e: &gpui::Entity<InputState>| e.read(cx).value().to_string();
        match SavedPrinter::new(&value(&inputs.name), &value(&inputs.address), &value(&inputs.key)) {
            Ok(printer) => {
                k.adding = true;
                k.add_error = None;
                k.send(KlipperCmd::Add(Box::new(printer)));
            }
            Err(error) => k.add_error = Some(error_text(&error).into()),
        }
        cx.notify();
    }

    /// Модалка «Принтер Klipper (Moonraker)»; `edit` заповнює назву й адресу збереженого.
    pub(super) fn open_klipper_dialog(
        &mut self,
        edit: Option<DeviceId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let k = &mut self.cloud.klipper;
        k.add_error = None;
        let (name, address) = edit
            .as_ref()
            .and_then(|d| k.entry(d))
            .map(|e| (e.printer.name.clone(), e.address.clone()))
            .unwrap_or_default();
        if let Some(inputs) = &k.inputs {
            for (entity, text) in [(&inputs.name, name), (&inputs.address, address), (&inputs.key, String::new())] {
                entity.update(cx, |s, cx| s.set_value(text, window, cx));
            }
            inputs.key.update(cx, |s, cx| s.set_masked(true, window, cx));
        }
        self.open_modal(std::rc::Rc::new(klipper_dialog), window, cx);
    }

    /// Діалог друку для Klipper: «Завантажити й друкувати» або «Лише завантажити».
    pub(super) fn open_klipper_print_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let crate::workspace::SliceState::Done(out) = &self.slice_state else {
            return;
        };
        let Some(dev) = self.cloud.selected.clone() else { return };
        let Some(plate) = self.plate_to_slice() else { return };
        let Some(entry) = self.cloud.klipper.entry(&dev).cloned() else { return };
        let gcode_3mf = out.gcode_3mf.clone();
        let project = self
            .path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "model".into());
        let remote = moonraker::remote_name(&format!("{project}_plate{plate}"));
        let summary = format!(
            "{project} · {} {plate} · {} · {} · {}",
            tr("пластина"),
            super::duration_text(out.print_time.as_secs() / 60),
            crate::tr_format!("{grams:.1} г", grams = out.filament_grams),
            self.selection.filament
        );
        // G-code з профілю Bambu на Klipper-принтері — майже напевно не те, що треба.
        let bambu_profile = self
            .presets
            .as_ref()
            .and_then(|s| s.machine_model_id(&self.selection.machine))
            .is_some_and(|m| ::lan::model_name(&m) != m.as_str());
        let machine = self.selection.machine.clone();
        let send = Rc::new(move |ws: &mut Workspace, start: bool, cx: &mut Context<Workspace>| {
            let overwrite = ws.cloud.klipper.overwrite;
            ws.cloud.sending = true;
            ws.cloud.klipper.upload = Some(0.0);
            ws.cloud.klipper.send(KlipperCmd::Print(Box::new(PrintJob {
                dev: dev.clone(),
                gcode_3mf: gcode_3mf.clone(),
                plate,
                remote: remote.clone(),
                options: UploadOptions { start, overwrite },
            })));
            ws.close_modal(cx);
            ws.toast(ToastKind::Info, tr("Надсилаю файл на принтер Klipper…"), cx);
        });
        let file_line = crate::tr_format!("Файл на принтері: {name}", name = moonraker::remote_name(&format!("{project}_plate{plate}")));
        self.open_modal(
            Rc::new(move |ws, _, cx| {
                let z = zed();
                let k = &ws.cloud.klipper;
                let info = ws.cloud.selected.as_ref().and_then(|d| k.info_for(d)).cloned();
                let not_ready = info.as_ref().is_some_and(|i| !i.ready);
                let overwrite = k.overwrite;
                let warn = |text: String| {
                    div()
                        .flex()
                        .items_start()
                        .gap(px(6.))
                        .pb(px(6.))
                        .child(ui::icon(IconName::TriangleAlert).text_color(z.warning))
                        .child(div().flex_1().min_w_0().whitespace_normal().text_xs().child(text))
                };
                let (send_print, send_only) = (send.clone(), send.clone());
                crate::widgets::modal(
                    "Надіслати на Klipper",
                    680.,
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
                                .child(div().text_sm().font_weight(gpui::FontWeight::MEDIUM).child(entry.printer.name.clone()))
                                .child(div().text_xs().text_color(z.text_placeholder).child(entry.address.clone())),
                        )
                        .child(div().text_sm().text_color(z.text_muted).child(summary.clone()))
                        .child(
                            div()
                                .pb(px(6.))
                                .text_xs()
                                .text_color(z.text_placeholder)
                                .child(file_line.clone()),
                        )
                        .when(bambu_profile, |d| {
                            d.child(warn(crate::tr_format!(
                                "Профіль «{machine}» — для принтера Bambu Lab. G-code з нього може не підійти для Klipper: виберіть профіль свого принтера.",
                                machine = machine
                            )))
                        })
                        .when(not_ready, |d| {
                            let i = info.clone().unwrap_or_default();
                            d.child(warn(crate::tr_format!(
                                "Klipper зараз не готовий ({state}): друк не почнеться, але файл можна завантажити.",
                                state = i.klippy
                            )))
                        })
                        .child(
                            div()
                                .pb(px(6.))
                                .text_xs()
                                .text_color(z.text_muted)
                                .whitespace_normal()
                                .child(tr("«Завантажити й друкувати» — Moonraker почне друк одразу після передачі файлу. «Лише завантажити» — файл з’явиться в списку G-code у Mainsail чи Fluidd.")),
                        )
                        .child(
                            crate::widgets::checkbox("klipper-overwrite", overwrite, false)
                                .h(px(28.))
                                .child(tr("Замінити файл, якщо такий уже є на принтері"))
                                .on_click(cx.listener(move |ws, _, _, cx| {
                                    ws.cloud.klipper.overwrite = !overwrite;
                                    cx.notify();
                                })),
                        ),
                    Some(
                        div()
                            .flex()
                                                        .gap(px(6.))
                            .child(
                                ui::ghost_button("klipper-print-cancel", None, "Скасувати")
                                    .on_click(cx.listener(|ws, _, _, cx| ws.close_modal(cx))),
                            )
                            .child(
                                ui::ghost_button("klipper-print-export", Some(IconName::Download), "Експортувати…")
                                    .border_1()
                                    .border_color(z.border_variant)
                                    .on_click(cx.listener(|ws, _, window, cx| {
                                        ws.close_modal(cx);
                                        ws.prompt_export(window, cx);
                                    })),
                            )
                            .child(
                                ui::ghost_button("klipper-upload-only", Some(IconName::Upload), "Лише завантажити")
                                    .border_1()
                                    .border_color(z.border_variant)
                                    .on_click(cx.listener(move |ws, _, _, cx| send_only(ws, false, cx))),
                            )
                            .child(
                                ui::accent_button("klipper-upload-print", IconName::Send, "Завантажити й друкувати", true)
                                    .on_click(cx.listener(move |ws, _, _, cx| send_print(ws, true, cx))),
                            )
                            .into_any_element(),
                    ),
                )
            }),
            window,
            cx,
        );
    }
}

fn klipper_dialog(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let z = zed();
    let k = &ws.cloud.klipper;
    let Some(inputs) = &k.inputs else {
        return div().into_any_element();
    };
    let label = |text: &str| div().text_xs().text_color(z.text_muted).child(tr(text));
    let field = |entity: &gpui::Entity<InputState>, window: &mut Window, cx: &mut Context<Workspace>| {
        crate::widgets::field(entity, gpui_component::input::Input::new(entity), window, cx)
    };
    let [name_field, address_field] = [&inputs.name, &inputs.address].map(|e| field(e, window, cx));
    let key_field = crate::widgets::field(
        &inputs.key,
        gpui_component::input::Input::new(&inputs.key).mask_toggle(),
        window,
        cx,
    );
    let saved: Vec<_> = k
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let id = e.printer.dev_id.as_str().to_owned();
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .h(px(28.))
                .text_sm()
                .child(ui::icon(IconName::Printer).text_color(z.text_muted))
                .child(div().flex_1().min_w_0().truncate().child(format!("{} · {}", e.printer.name, e.address)))
                .child(
                    ui::icon_button(("klipper-remove", i), IconName::Trash, "Видалити принтер Klipper").on_click(
                        cx.listener(move |ws, _, window, cx| {
                            ws.cloud.klipper.send(KlipperCmd::Remove(id.clone()));
                            if ws.cloud.selected.as_ref().map(DeviceId::as_str) == Some(id.as_str()) {
                                ws.cloud.select(None, window);
                            }
                            cx.notify();
                        }),
                    ),
                )
        })
        .collect();
    let body = div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .p(px(12.))
        .child(
            div()
                .text_sm()
                .text_color(z.text_muted)
                .whitespace_normal()
                .child(tr("Принтер з Klipper і Moonraker (Mainsail, Fluidd, Creality Helper Script). Адреса — та сама, за якою відкривається вебінтерфейс; порт Moonraker типово 7125.")),
        )
        .child(label("Назва"))
        .child(name_field)
        .child(label("Адреса"))
        .child(address_field)
        .child(label("API-ключ"))
        .child(key_field)
        .child(
            div()
                .pt(px(4.))
                .text_xs()
                .text_color(z.text_placeholder)
                .whitespace_normal()
                .child(tr("Ключ потрібен, лише якщо в moonraker.conf увімкнено [authorization] і ця мережа не в trusted_clients. Його видно в Mainsail: Налаштування → Авторизація, або в ~/printer_data/moonraker.secrets. Ключ зберігається в окремому файлі з доступом лише для вас.")),
        )
        .children(k.add_error.clone().map(|e| div().text_sm().text_color(z.error).whitespace_normal().child(e)))
        .when(!k.entries.is_empty(), |d| {
            d.child(ui::divider()).child(label("Збережені принтери Klipper")).children(saved)
        });
    let adding = k.adding;
    crate::widgets::modal(
        "Принтер Klipper (Moonraker)",
        500.,
        body,
        Some(
            div()
                .flex()
                .gap(px(6.))
                .items_center()
                .when(adding, |d| d.child(ui::spinner("klipper-add-spin")))
                .child(
                    ui::ghost_button("klipper-cancel", None, "Скасувати")
                        .on_click(cx.listener(|ws, _, _, cx| ws.close_modal(cx))),
                )
                .child(
                    ui::accent_button("klipper-add", IconName::Plus, "Перевірити й додати", !adding)
                        .when(!adding, |b| b.on_click(cx.listener(|ws, _, _, cx| ws.submit_klipper(cx)))),
                )
                .into_any_element(),
        ),
    )
}

/// Блок «Klipper (Moonraker)» у лівому доку: кнопка додавання, прогрес, проблема.
pub fn panel_section(cloud: &CloudState, cx: &mut Context<Workspace>) -> gpui::Div {
    let z = zed();
    let mine = cloud.selected.as_ref().is_some_and(|d| cloud.klipper.has(d));
    div()
        .flex()
        .flex_col()
        .flex_none()
        .gap(px(4.))
        .p(px(10.))
        .border_t_1()
        .border_color(z.border_variant)
        .child(
            div()
                .flex()
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(z.text_muted)
                        .child(tr("Klipper (Moonraker)")),
                )
                .child(
                    ui::icon_button("klipper-open", IconName::Plus, "Додати принтер Klipper")
                        .on_click(cx.listener(|ws, _, window, cx| ws.open_klipper_dialog(None, window, cx))),
                ),
        )
        .when(cloud.klipper.entries.is_empty(), |d| {
            d.child(div().text_xs().text_color(z.text_placeholder).whitespace_normal().child(tr(
                "Creality, Voron, Prusa з Klipper та інші — за адресою Moonraker.",
            )))
        })
        .children(cloud.klipper.upload.map(|p| {
            div()
                .text_xs()
                .text_color(z.accent)
                .child(crate::tr_format!("Завантаження на принтер: {pct:.0}%", pct = p * 100.0))
        }))
        .children(
            cloud
                .klipper
                .problem
                .clone()
                .filter(|_| mine)
                .map(|p| div().text_xs().text_color(z.warning).whitespace_normal().child(p)),
        )
}

/// Сторінка вибраного принтера Klipper (замість вкладок Bambu).
pub fn body(cloud: &CloudState, _window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let z = zed();
    let Some(dev) = cloud.selected.clone() else {
        return div().into_any_element();
    };
    let info = cloud.klipper.info_for(&dev).cloned();
    let web_url = info.as_ref().map(|i| i.web_url.clone()).filter(|u| !u.is_empty());
    let open_web = |id: &'static str, url: Option<String>, cx: &mut Context<Workspace>| {
        ui::ghost_button(id, Some(IconName::ExternalLink), "Відкрити Mainsail / Fluidd")
            .border_1()
            .border_color(z.border_variant)
            .on_click(cx.listener(move |_, _, _, cx| {
                if let Some(url) = &url {
                    cx.open_url(url);
                }
            }))
            .into_any_element()
    };
    let Some(status) = &cloud.status else {
        let Some(problem) = cloud.klipper.problem.clone() else {
            return ui::empty_state(
                IconName::Printer,
                "Чекаю на статус принтера…",
                "Перший знімок стану приходить за кілька секунд.",
                Vec::new(),
            )
            .into_any_element();
        };
        let reachable = cloud.klipper.is_up(&dev);
        let edit = dev.clone();
        let mut actions = vec![
            ui::ghost_button("klipper-fix", Some(IconName::Pencil), "Змінити адресу або ключ…")
                .border_1()
                .border_color(z.border_variant)
                .on_click(cx.listener(move |ws, _, window, cx| {
                    ws.open_klipper_dialog(Some(edit.clone()), window, cx)
                }))
                .into_any_element(),
        ];
        if reachable {
            actions.push(open_web("klipper-web-problem", web_url.clone(), cx));
        }
        return ui::empty_state(
            if reachable { IconName::TriangleAlert } else { IconName::WifiOff },
            if reachable {
                "Klipper не готовий"
            } else {
                "Немає зв’язку з принтером Klipper"
            },
            problem,
            actions,
        )
        .into_any_element();
    };

    let row = |label: &str, value: String| {
        div()
            .flex()
            .items_center()
            .h(px(26.))
            .px(px(10.))
            .gap(px(8.))
            .text_sm()
            .child(div().flex_1().text_color(z.text_muted).child(tr(label)))
            .child(div().font_family(crate::theme::MONO_FONT).child(value))
    };
    let temp = |cur: Option<f64>, target: Option<f64>| match (cur, target) {
        (Some(c), Some(t)) if t > 0.0 => format!("{c:.1} / {t:.0} °C"),
        (Some(c), _) => format!("{c:.1} °C"),
        _ => "—".into(),
    };
    let info = info.unwrap_or_default();
    let mut state_card = super::card("Стан", vec![open_web("klipper-web", web_url, cx)])
        .flex_none()
        .w(px(340.))
        .pb(px(6.))
        .child(row("Сопло", temp(status.nozzle_temper, status.nozzle_target_temper)))
        .child(row("Стіл", temp(status.bed_temper, status.bed_target_temper)))
        .child(row("Klipper", info.klippy.clone()));
    if let Some(file) = status.gcode_file.clone() {
        state_card = state_card.child(row("Файл", file));
    }
    if !info.hostname.is_empty() {
        state_card = state_card.child(row("Хост", info.hostname.clone()));
    }
    if !info.moonraker.is_empty() {
        state_card = state_card.child(row("Moonraker", info.moonraker.clone()));
    }
    if !info.klipper.is_empty() {
        state_card = state_card.child(row("Версія Klipper", info.klipper.clone()));
    }
    state_card = state_card.child(
        div()
            .px(px(10.))
            .pt(px(6.))
            .text_xs()
            .text_color(z.text_placeholder)
            .whitespace_normal()
            .child(tr("Температури, рух і макроси — у Mainsail чи Fluidd. Тут: стан, пауза, продовження й скасування.")),
    );
    div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .p(px(12.))
        .child(super::pages::job_header(cloud, status, cx))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(12.))
                .items_start()
                .child(super::camera_panel(cloud, cx))
                .child(state_card),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use moonraker::Heater;

    #[test]
    fn moonraker_state_maps_to_device_page_status() {
        let s = JobStatus {
            state: PrintState::Printing,
            filename: "sub/cube.gcode".into(),
            progress: Some(0.5),
            print_duration: Some(600.0),
            current_layer: Some(10),
            total_layer: Some(20),
            extruder: Heater { temperature: Some(214.0), target: Some(215.0) },
            bed: Heater { temperature: Some(60.0), target: Some(60.0) },
            homed_axes: "xz".into(),
            ..Default::default()
        };
        let p = to_printer_status(&s);
        assert_eq!(p.gcode_state.as_deref(), Some("RUNNING"));
        assert_eq!(p.mc_percent, Some(50.0));
        assert_eq!(p.mc_remaining_time, Some(10.0));
        assert_eq!((p.layer_num, p.total_layer_num), (Some(10), Some(20)));
        assert_eq!(p.subtask_name.as_deref(), Some("cube.gcode"));
        assert_eq!(p.home_flag, Some(0b101));
        assert_eq!(p.nozzle_target_temper, Some(215.0));
        for (state, bambu) in [
            (PrintState::Paused, "PAUSE"),
            (PrintState::Complete, "FINISH"),
            (PrintState::Cancelled, "IDLE"),
            (PrintState::Standby, "IDLE"),
            (PrintState::Error, "FAILED"),
        ] {
            let p = to_printer_status(&JobStatus { state, ..Default::default() });
            assert_eq!(p.gcode_state.as_deref(), Some(bambu));
            assert_eq!(p.layer_num, None);
        }
    }

    #[test]
    fn only_print_controls_pass_and_only_in_matching_state() {
        let dev = DeviceId::new("klipper:10.0.0.2:7125");
        let mut ui = KlipperUi::default();
        ui.up.insert(dev.clone());
        let status = |s: &str| PrinterStatus { gcode_state: Some(s.into()), ..Default::default() };
        let run = status("RUNNING");
        let pause = status("PAUSE");
        let idle = status("IDLE");
        let chk = |s: &PrinterStatus, c| check(&ui, Some(&dev), Some(s), &dev, c);
        assert!(chk(&run, Control::Pause).is_ok());
        assert!(chk(&run, Control::Stop).is_ok());
        assert!(chk(&pause, Control::Resume).is_ok());
        assert_eq!(chk(&idle, Control::Stop), Err(guard::Blocked::WrongState));
        assert_eq!(chk(&run, Control::Resume), Err(guard::Blocked::WrongState));
        assert!(matches!(chk(&idle, Control::Home), Err(guard::Blocked::Rejected(_))));
        let other = DeviceId::new("klipper:10.0.0.3:7125");
        assert_eq!(check(&ui, Some(&other), Some(&run), &dev, Control::Pause), Err(guard::Blocked::WrongDevice));
        ui.up.clear();
        assert_eq!(
            check(&ui, Some(&dev), Some(&run), &dev, Control::Pause),
            Err(guard::Blocked::LanDisconnected)
        );
    }

    #[test]
    fn routing_keeps_klipper_commands_away_from_cloud() {
        let (tx, rx) = mpsc::channel();
        let mut ui = KlipperUi { commands: Some(tx), ..Default::default() };
        let dev = DeviceId::new("klipper:10.0.0.2:7125");
        ui.entries = vec![Entry {
            printer: cloud::Printer { dev_id: dev.clone(), name: "E3".into(), model: MODEL.into(), online: true },
            address: "10.0.0.2:7125".into(),
            has_key: false,
        }];
        assert!(ui.route(Some(&dev), CloudCmd::Control(dev.clone(), Control::Pause)).is_none());
        assert!(matches!(rx.try_recv(), Ok(KlipperCmd::Control(..))));
        assert!(ui.route(Some(&dev), CloudCmd::Camera(true)).is_none());
        assert!(matches!(rx.try_recv(), Ok(KlipperCmd::Camera(true))));
        assert!(ui.route(Some(&dev), CloudCmd::Versions).is_none());
        assert!(matches!(ui.route(None, CloudCmd::Select(dev.clone())), Some(CloudCmd::Select(_))));
        assert!(matches!(rx.try_recv(), Ok(KlipperCmd::Select(_))));
        let bambu = DeviceId::new("03919C000000");
        assert!(matches!(ui.route(Some(&bambu), CloudCmd::Camera(true)), Some(CloudCmd::Camera(true))));
    }

    #[test]
    fn errors_are_translated_with_klippy_state() {
        let text = error_text(&MoonrakerError::KlippyNotReady {
            state: KlippyState::Shutdown,
            message: "MCU shutdown".into(),
        });
        assert!(text.contains("shutdown") && text.contains("MCU shutdown"), "{text}");
        assert_eq!(error_text(&MoonrakerError::Unauthorized), tr(MoonrakerError::Unauthorized.key()));
    }

    #[test]
    fn plain_gcode_is_extracted_from_3mf_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let three_mf = dir.path().join("out.gcode.3mf");
        {
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&three_mf).unwrap());
            zip.start_file("Metadata/plate_2.gcode", zip::write::SimpleFileOptions::default()).unwrap();
            std::io::Write::write_all(&mut zip, b"G28\n").unwrap();
            zip.finish().unwrap();
        }
        let path = plain_gcode(&three_mf, 2).unwrap();
        assert_eq!(path, dir.path().join("plate_2.gcode"));
        assert_eq!(std::fs::read(&path).unwrap(), b"G28\n");
        assert!(plain_gcode(&three_mf, 3).is_err());
    }
}
