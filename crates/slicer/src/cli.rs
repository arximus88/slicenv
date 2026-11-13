//! Запуск консольного Bambu Studio: `bambu-studio --slice N … model.3mf`.

use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, bail};
use serde_json::{Map, Value, json};

use crate::{
    engine::{Engine, EngineInfo, EngineKind},
    safe_mode::{COMPATIBLE_NOTE, EngineCompat, Memory, NO_PROJECT_SETTINGS_NOTE, Stage},
    presets::{Kind, PresetStore, Selection},
};

static ACTIVE_SLICE: std::sync::Mutex<Option<SliceCancellation>> = std::sync::Mutex::new(None);

/// Stop the engine shared by GUI and MCP, e.g. when the document closes.
pub fn cancel_active_slice() {
    if let Some(token) = ACTIVE_SLICE.lock().unwrap().take() {
        token.cancel();
    }
}

/// Cancellation owns the live child, including engine probes. Kill always reaps.
#[derive(Clone, Default)]
pub struct SliceCancellation(std::sync::Arc<std::sync::Mutex<ProcessState>>);
#[derive(Default)]
struct ProcessState {
    cancelled: bool,
    child: Option<std::process::Child>,
    /// Логи рушія, з яких читається прогрес: Bambu пише в stdout (`slicer.log`),
    /// OrcaSlicer — у `00000.log` у робочій теці.
    logs: Vec<PathBuf>,
    progress: Option<u8>,
    /// Етап з логу рушія («Generating perimeters»).
    stage: Option<String>,
    /// Номер шару, який рушій зараз переводить у G-code (`Generating G-code: layer 80`).
    layer: Option<u32>,
    /// G-code пластини, який пише поточний запуск рушія.
    gcode: Option<PathBuf>,
    /// `gcode` уже є на диску.
    gcode_ready: bool,
}
#[derive(Debug)]
pub struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("slice cancelled")
    }
}
impl std::error::Error for Cancelled {}
impl Drop for ProcessState {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// `spawn` з повтором на ETXTBSY. Поки один потік щойно записав виконуваний
/// файл (рушій після розпакування, мок у тестах), інший потік міг зробити
/// fork, і дочірній процес до свого exec тримає копію дескриптора на запис.
/// Вікно коротке, тому кілька повторів із паузою його переживають.
fn spawn_retrying_busy(command: &mut Command) -> std::io::Result<std::process::Child> {
    let mut attempt = 0;
    loop {
        match command.spawn() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempt < 20 => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(5 * attempt));
            }
            other => return other,
        }
    }
}

impl SliceCancellation {
    pub fn cancel(&self) {
        let mut state = self.0.lock().unwrap();
        state.cancelled = true;
        if let Some(mut child) = state.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    pub fn progress(&self) -> Option<u8> {
        self.0.lock().unwrap().progress
    }

    /// Поточний етап нарізки англійською, як його пише рушій.
    pub fn stage(&self) -> Option<String> {
        self.0.lock().unwrap().stage.clone()
    }

    /// Номер шару на етапі генерації G-code. Загальної кількості рушій не повідомляє.
    pub fn layer(&self) -> Option<u32> {
        self.0.lock().unwrap().layer
    }

    /// G-code пластини, щойно рушій його записав, — ще до експорту `.gcode.3mf` і виходу
    /// процесу (0,13–0,25 с на малій пластині, 0,6–0,9 с на 250 тис. △). Bambu Studio й
    /// OrcaSlicer пишуть `plate_N.gcode.tmp` і перейменовують його в `plate_N.gcode` вже
    /// готовим, тож файл можна читати для Preview. Друкувати — лише з [`SliceOutput`]:
    /// після цього рушій ще може відмовити (і, наприклад, повторити запуск з `--no-check`).
    pub fn gcode_ready(&self) -> Option<PathBuf> {
        let state = self.0.lock().unwrap();
        state.gcode.clone().filter(|_| state.gcode_ready)
    }

    /// Новий запуск рушія (наступна стадія сумісного режиму) починає прогрес спочатку.
    fn reset_progress(&self) {
        let mut state = self.0.lock().unwrap();
        state.progress = None;
        state.stage = None;
        state.layer = None;
        state.gcode_ready = false;
    }

    fn watch_gcode(&self, path: Option<PathBuf>) {
        let mut state = self.0.lock().unwrap();
        state.gcode = path;
        state.gcode_ready = false;
    }

    fn check_gcode(&self) {
        let path = {
            let state = self.0.lock().unwrap();
            match &state.gcode {
                Some(path) if !state.gcode_ready => path.clone(),
                _ => return,
            }
        };
        if path.is_file() {
            self.0.lock().unwrap().gcode_ready = true;
        }
    }

    /// Останній статус з хвостів логів рушія.
    ///
    /// Не `--pipe`: з ним і Bambu Studio, і OrcaSlicer передають у FIFO (і в лог) лише перший
    /// «Generating G-code» на 80 %, а «Generating G-code: layer N» з тим самим відсотком
    /// відкидають — номера шару там немає (перевірено на Bambu 02.07.01, Orca 2.4.2 і 2.5.0-beta).
    /// Без `--pipe` лог на `--debug 4` має кожен виклик `default_status_callback`, на всіх ОС.
    fn read_logs(&self) {
        use std::io::{Read, Seek, SeekFrom};
        let logs = self.0.lock().unwrap().logs.clone();
        for path in logs {
            let Ok(mut file) = fs::File::open(path) else { continue };
            let size = file.metadata().map_or(0, |m| m.len());
            let _ = file.seek(SeekFrom::Start(size.saturating_sub(4096)));
            let mut bytes = Vec::new();
            let _ = file.read_to_end(&mut bytes);
            let tail = String::from_utf8_lossy(&bytes);
            if let Some(status) = status_line(&tail) {
                let mut state = self.0.lock().unwrap();
                if status.percent.is_some() {
                    state.progress = status.percent;
                }
                if !status.stage.is_empty() {
                    state.stage = Some(status.stage);
                    state.layer = status.layer;
                }
            } else if let Some(percent) = progress_percent(&tail) {
                self.0.lock().unwrap().progress = Some(percent);
            }
        }
    }
    pub fn check(&self) -> Result<()> {
        if self.0.lock().unwrap().cancelled {
            return Err(Cancelled.into());
        }
        Ok(())
    }
    pub(crate) fn run(
        &self,
        command: &mut Command,
        timeout: Option<Duration>,
    ) -> Result<std::process::ExitStatus> {
        {
            let mut state = self.0.lock().unwrap();
            if state.cancelled {
                return Err(Cancelled.into());
            }
            state.child = Some(spawn_retrying_busy(command)?);
        }
        let started = Instant::now();
        loop {
            {
                let mut state = self.0.lock().unwrap();
                if state.cancelled {
                    return Err(Cancelled.into());
                }
                let child = state.child.as_mut().expect("running child");
                match child.try_wait() {
                    Ok(Some(status)) => {
                        state.child = None;
                        return Ok(status);
                    }
                    Ok(None) => {}
                    Err(err) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        state.child = None;
                        return Err(err.into());
                    }
                }
            }
            if timeout.is_some_and(|limit| started.elapsed() >= limit) {
                self.cancel();
                bail!("engine probe timed out");
            }
            self.read_logs();
            self.check_gcode();
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
/// Хто має виправляти невдалу нарізку.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// Рушій відмовився від цієї моделі з цими параметрами — виправляє користувач.
    Model,
    /// Пам’ять, диск, аварія чи застаріла версія рушія.
    System,
    /// Рушій не зміг прочитати те, що передав Slicenv, — наш баг.
    App,
}

/// Невдала нарізка з поясненням для інтерфейсу. `Display` — колишній текстовий формат
/// (для MCP і логів); GUI бере поля.
#[derive(Clone, Debug)]
pub struct SliceFailure {
    pub engine: String,
    /// Текст рушія (`error_string` з result.json).
    pub reason: String,
    /// Код CLI (`CLI_*`, від’ємний); `None`, якщо рушій упав сигналом.
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub kind: FailureKind,
    /// Коротко, що сталося (заголовок картки).
    pub title: &'static str,
    /// Що зробити.
    pub hint: &'static str,
    /// Рядки `[error]` з логу рушія й stderr — найконкретніша причина.
    pub details: Vec<String>,
    pub log: PathBuf,
    /// Рушій аварійно завершився (сигнал/NTSTATUS або без result.json), а не відмовив з кодом CLI.
    pub crashed: bool,
    /// Аварія на всіх стадіях сумісного режиму: повтор тим самим рушієм не допоможе.
    pub safe_mode_exhausted: bool,
}

impl std::fmt::Display for SliceFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} не нарізав", self.engine)?;
        if !self.reason.is_empty() {
            write!(f, ": {}", self.reason)?;
        } else if self.crashed {
            write!(f, ": аварійно завершився до того, як повідомив причину")?;
        }
        if let Some(code) = self.code {
            write!(f, " (код {code})")?;
        } else if let Some(signal) = self.signal {
            if cfg!(windows) {
                write!(f, " (0x{:08X})", signal as u32)?;
            } else {
                write!(f, " (сигнал {signal})")?;
            }
        }
        write!(f, "\n{}. {}", self.title, self.hint)?;
        for line in &self.details {
            write!(f, "\n{line}")?;
        }
        write!(f, "\nлог: {}", self.log.display())
    }
}
impl std::error::Error for SliceFailure {}

/// Заголовок і порада за кодом CLI Bambu/Orca (`CLI_*` у `libslic3r/Utils.hpp`).
/// Правило: рушій не прочитав наші дані — винен Slicenv; прочитав, але відмовився
/// друкувати таку модель з такими параметрами — виправляє користувач.
pub fn explain_failure(code: Option<i32>, signal: Option<i32>) -> (FailureKind, &'static str, &'static str) {
    use FailureKind::*;
    if signal.is_some() {
        return (System, "Рушій аварійно завершився", "Спробуй ще раз або іншу версію рушія. Якщо повторюється на цій моделі — надішли лог.");
    }
    match code {
        Some(-15 | -16) => (Model, "Проєкт для іншого принтера", "Рушій не переносить цей проєкт на вибраний принтер. Вибери принтер проєкту або перезбережи проєкт у Bambu Studio з цим принтером."),
        Some(-17) => (Model, "Процес не для цього принтера", "Вибери профіль процесу, сумісний з вибраним принтером."),
        Some(-18) => (Model, "Недопустимі значення в проєкті", "Натисни «Перевірити сумісність» нижче й застосуй запропоновані значення. Те саме є у вікні «Рушій нарізки» (кнопка з шестернею вгорі панелі «Друк»)."),
        Some(-20) => (Model, "Модель не вміщається в принтер", "Модель вища або ширша за робочу зону. Зменш її або вибери більший принтер."),
        Some(-21 | -22) => (Model, "Не вдалося розкласти об’єкти", "Розстав або зорієнтуй об’єкти вручну."),
        Some(-24) => (Model, "Файл новішої версії Bambu Studio", "Натисни «Дозволити новіші 3MF і нарізати» нижче або онови рушій нарізки. Цей перемикач є й у вікні «Рушій нарізки» (кнопка з шестернею вгорі панелі «Друк»)."),
        Some(-25) => (Model, "Функція, якої немає в цьому рушії", "Онови рушій до новішої версії."),
        Some(-50 | -60) => (Model, "На пластині немає що друкувати", "Додай об’єкти або увімкни друк для наявних."),
        Some(-51) => (Model, "Рушій забракував параметри", "Причина — у тексті рушія вище; зміни вказаний параметр."),
        Some(-52 | -102 | -104) => (Model, "Об’єкт за межами столу", "Об’єкт або траєкторія виходить за стіл чи в заборонену зону. Пересунь об’єкти всередину пластини."),
        Some(-58 | -59) => (Model, "Модель надто складна", "Забагато трикутників або нарізка задовга. Спрости сітку."),
        Some(-61) => (Model, "Філамент не для цієї пластини", "Зміни тип пластини або філамент."),
        Some(-62) => (Model, "Філаменти з різною температурою", "Гарячий і холодний пластик разом можуть забити сопло. Використай один тип пластику для всіх слотів."),
        Some(-63 | -64) => (Model, "Зіткнення з головою", "Об’єкти зачепить голова під час друку. Рознеси їх або вимкни друк по черзі."),
        Some(-65) => (Model, "Режим вази несумісний", "Вимкни режим спіралі або зміни стінки й заповнення."),
        Some(-68..=-66) => (Model, "Філаменти не розподілити", "Ці філаменти не можна призначити екструдерам цього принтера."),
        Some(-100) => (Model, "Рушій не зміг нарізати модель", "Найчастіше — пошкоджена сітка або крайні параметри. Подивись деталі нижче."),
        Some(-101) => (Model, "Траєкторії перетинаються", "Рознеси об’єкти, щоб вони не накладались."),
        Some(-103) => (Model, "Філамент не для першого шару", "Зміни філамент першого шару."),
        Some(-105) => (Model, "Траєкторія в зоні калібрування", "Пересунь об’єкти від зони калібрування."),
        Some(-1 | -14 | -53 | -54) => (System, "Бракує ресурсів", "Не вистачає пам’яті чи місця на диску, або недоступна тека кешу."),
        Some(-13..=-2 | -19 | -23 | -57..=-55) => (App, "Рушій не прийняв дані від Slicenv", "Це наша помилка, не твоя. Надішли звіт — кнопка «Копіювати звіт»."),
        _ => (System, "Невідома помилка рушія", "Подивись деталі й лог."),
    }
}

/// Рядки з `[error]` у лозі рушія — там конкретніше, ніж в `error_string`.
fn engine_errors(log: &str) -> Vec<String> {
    let mut lines: Vec<String> = log
        .lines()
        .filter_map(|l| l.split_once("[error]").map(|(_, rest)| rest.trim().to_owned()))
        .filter(|l| !l.is_empty())
        .collect();
    lines.dedup();
    lines.truncate(8);
    lines
}

/// Статус нарізки з рядка `default_status_callback` у лозі рушія.
#[derive(Debug, PartialEq, Eq)]
struct Status {
    percent: Option<u8>,
    /// Етап без номера шару: «Generating G-code».
    stage: String,
    layer: Option<u32>,
}

/// Останній рядок статусу Bambu/Orca у лозі (`--debug 4`):
/// `default_status_callback: percent=80, warning_step=-1, message=Generating G-code: layer 80, message_type=0`.
/// Bambu пише його з часом і рівнем (`[debug]   default_status_callback: …`), Orca у
/// `00000.log` — голим рядком.
fn status_line(text: &str) -> Option<Status> {
    text.lines().rev().find_map(|line| {
        let rest = line.split_once("status_callback: percent=")?.1;
        let (percent, rest) = rest.split_once(',')?;
        let percent = percent.trim().parse::<i32>().ok().filter(|p| (0..=100).contains(p)).map(|p| p as u8);
        let message = rest
            .split_once("message=")
            .map(|(_, m)| m.split(", message_type").next().unwrap_or(m).trim())
            .unwrap_or_default();
        let (stage, layer) = match message.rsplit_once(": layer ") {
            Some((stage, n)) if n.trim().parse::<u32>().is_ok() => (stage.trim(), n.trim().parse().ok()),
            _ => (message, None),
        };
        Some(Status { percent, stage: stage.to_owned(), layer })
    })
}

fn progress_percent(text: &str) -> Option<u8> {
    text.lines().rev().find_map(|line| {
        let before = line.split_once('%')?.0;
        let digits: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let value: u8 = digits.chars().rev().collect::<String>().parse().ok()?;
        (value <= 100).then_some(value)
    })
}

/// Імена, під якими Bambu Studio встановлюється в різних дистрибутивах.
#[cfg(not(windows))]
const BINARIES: &[&str] = &["bambu-studio", "bambustudio", "BambuStudio"];
#[cfg(windows)]
const BINARIES: &[&str] = &["bambu-studio.exe", "BambuStudio.exe"];

pub fn find_bambu_studio() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("BAMBU_STUDIO") {
        return Some(PathBuf::from(path));
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .flat_map(|dir| BINARIES.iter().map(move |b| dir.join(b)))
        .find(|p| p.is_file())
}

#[derive(Clone, Debug)]
pub struct SliceRequest {
    pub input: PathBuf,
    /// Номер пластини з 1, як у Bambu Studio.
    pub plate: u32,
    /// Explicit user opt-in; version checking remains enabled by default.
    pub allow_newer_file: bool,
    pub selection: Selection,
    /// Зміни користувача поверх профілів (ще не збережені як пресет).
    pub overrides: Overrides,
}

#[derive(Clone, Debug, Default)]
pub struct Overrides {
    pub machine: Map<String, Value>,
    pub process: Map<String, Value>,
    pub filament: Map<String, Value>,
}

#[derive(Clone, Debug)]
pub struct SliceOutput {
    /// Готовий до друку `*.gcode.3mf`.
    pub gcode_3mf: PathBuf,
    pub print_time: Duration,
    pub filament_grams: f64,
    pub layer_height: f64,
    pub elapsed: Duration,
    pub warnings: Vec<String>,
    /// Нарізано в сумісному режимі ([`crate::safe_mode`]): тихе повідомлення, не помилка.
    pub compatibility_note: Option<&'static str>,
}

/// Код виходу CLI Bambu/Orca при конфлікті траєкторій (`CLI_GCODE_PATH_CONFLICTS`, -101).
const CLI_GCODE_PATH_CONFLICTS: i32 = 155;

/// Нарізає одну пластину. Блокує потік на час роботи слайсера (секунди — хвилини).
pub fn slice(store: &PresetStore, request: &SliceRequest) -> Result<SliceOutput> {
    let engine = Engine::discover(EngineKind::Bambu)?;
    slice_with_engine(&engine, store, request)
}

pub fn slice_with_engine(
    engine: &Engine,
    store: &PresetStore,
    request: &SliceRequest,
) -> Result<SliceOutput> {
    slice_with_engine_cancellable(engine, store, request, &SliceCancellation::default())
}

pub fn slice_with_engine_cancellable(
    engine: &Engine,
    store: &PresetStore,
    request: &SliceRequest,
    cancellation: &SliceCancellation,
) -> Result<SliceOutput> {
    // A new request supersedes the previous engine, including requests from MCP.
    cancellation.check()?;
    if let Some(previous) = ACTIVE_SLICE.lock().unwrap().replace(cancellation.clone())
        && !std::sync::Arc::ptr_eq(&previous.0, &cancellation.0)
    {
        previous.cancel();
    }
    // Serialize engines across GUI and MCP; cancelled waiters never launch a process.
    static ENGINE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _engine = loop {
        cancellation.check()?;
        if let Ok(guard) = ENGINE.try_lock() {
            break guard;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let started = Instant::now();
    let request = SliceRequest {
        input: fs::canonicalize(&request.input)
            .with_context(|| format!("не знайдено модель {}", request.input.display()))?,
        ..request.clone()
    };
    if store.engine_kind().is_some_and(|kind| kind != engine.kind) {
        bail!("каталог профілів належить іншому рушію");
    }
    let info = {
        let _s = paths::profile::Span::new("slice::probe");
        engine.probe_cancellable(cancellation)?
    };
    // Обов’язкові лише прапорці, без яких нарізати неможливо; решту передаємо, тільки якщо рушій
    // їх знає (старі збірки нарізають і без них), а без `--export-3mf` — одразу в сумісному режимі.
    for flag in ["--slice", "--outputdir", "--load-settings", "--load-filaments"] {
        if !info.supports_flag(flag) {
            bail!(
                "{} does not advertise required slicing flag {flag}",
                engine.kind.name()
            );
        }
    }
    let stem = request
        .input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "model".into());
    // Each run is isolated: stale outputs and slicer caches cannot cross engine/version boundaries.
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let work = cache_dir()
        .join(engine.kind.name())
        .join(safe_component(&info.identity))
        .join(format!(
            "{stem}-plate{}-{}-{nonce}",
            request.plate,
            std::process::id()
        ));
    fs::create_dir_all(&work)?;
    // Скасована нарізка не лишає по собі теку в кеші — хоч би де функція вийшла.
    let _cleanup = std::mem::DropGuard::new(&work, |work| {
        if cancellation.check().is_err() {
            let _ = fs::remove_dir_all(work);
        }
    });

    let selection = &request.selection;
    let overrides = &request.overrides;
    let flatten_span = paths::profile::Span::new("slice::flatten");
    let mut machine = store.flatten(Kind::Machine, &selection.machine)?;
    machine.extend(overrides.machine.clone());
    let mut process = store.flatten(Kind::Process, &selection.process)?;
    process.extend(overrides.process.clone());
    process.insert("curr_bed_type".into(), json!(selection.bed_type));
    if engine.kind == EngineKind::Orca {
        crate::orca_compat::normalize_orca_config(&mut process);
    }
    let mut filament = store.flatten(Kind::Filament, &selection.filament)?;
    filament.extend(overrides.filament.clone());
    // Налаштування, з якими нарізано, — для `.gcode.3mf`, який у сумісному режимі збирає Slicenv.
    let mut project_settings = machine.clone();
    project_settings.extend(process.clone());
    project_settings.extend(filament.clone());
    drop(flatten_span);
    // Конфіг проєкту в 3MF рушій бере за основу, а `--load-settings` лише перекриває ключі, що
    // є в наших JSON. Тому копію 3MF для нарізки виправляємо (файл користувача не змінюється):
    // OrcaSlicer ≤ 2.3 відкидає нулі «філамент для елемента» з Bambu Studio (код −18), а
    // проєкти, збережені Slicenv до виправлення розгортання `inherits`, несуть ключі від
    // батька чужого вендора, яких немає в пресетах (`use_relative_e_distances` = 0 → вага ×140).
    let rewrite_span = paths::profile::Span::new("slice::rewrite_input");
    let request = if is_stl(&request.input) {
        request
    } else {
        let fix = |config: &mut serde_json::Map<String, Value>| {
            let mut changed = engine.kind == EngineKind::Orca
                && crate::orca_compat::normalize_orca_config(config);
            changed |= drop_stale_keys(store, selection, &project_settings, config);
            changed
        };
        match crate::orca_compat::rewrite_project_config(&request.input, &work.join("input"), fix) {
            Ok(Some(input)) => SliceRequest { input, ..request },
            Ok(None) => request,
            Err(e) => {
                eprintln!("[slice] конфіг 3MF: {e:#}");
                request
            }
        }
    };
    drop(rewrite_span);
    let selection = &request.selection;
    let write_span = paths::profile::Span::new("slice::write_configs");
    // Для `.gcode.3mf`, який у сумісному режимі збирає Slicenv: модель принтера й кольори.
    let package_meta = crate::safe_mode::PackageMeta {
        printer_model_id: store.machine_model_id(&selection.machine).unwrap_or_default(),
        filament_colours: project_colours(&request.input),
    };
    let files = [
        ("machine.json", machine),
        ("process.json", process),
        ("filament.json", filament),
    ];
    for (name, config) in &files {
        fs::write(work.join(name), serde_json::to_vec_pretty(config)?)?;
    }

    let output_name = format!("{stem}-plate{}.gcode.3mf", request.plate);
    let log_path = work.join("slicer.log");
    cancellation.0.lock().unwrap().logs = vec![log_path.clone(), work.join("00000.log")];
    fs::write(
        work.join("provenance.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "engine": engine.kind.name(), "version": info.version, "identity": info.identity, "binary": engine.path,
            "preset_root": store.root(), "allow_newer_file": request.allow_newer_file,
            "selection": { "machine": selection.machine, "process": selection.process, "filament": selection.filament, "bed_type": selection.bed_type },
        }))?,
    )?;
    drop(write_span);

    // Сумісний режим (див. `safe_mode`): після аварії рушія — безпечніша стадія; досягнута
    // стадія запам’ятовується для цього рушія, щоб наступна нарізка не падала знову.
    let memory = Memory::new(Memory::default_path());
    let mut compat = memory.get(&info.identity);
    let mut stage = compat.stage.unwrap_or(Stage::Normal);
    if !info.supports_flag("--export-3mf") {
        stage = stage.max(Stage::GcodeOnly);
    }
    // Без налаштувань проєкту в 3MF (STL, проєкт без конфігу) остання стадія повторила б
    // попередню — її пропускаємо.
    let strippable = !is_stl(&request.input) && crate::safe_mode::has_project_settings(&request.input);
    let mut escalated = compat.stage.is_some_and(|s| s > Stage::Normal);
    loop {
        let attempt = SliceAttempt {
            engine,
            info: &info,
            request: &request,
            cancellation,
            work: &work,
            output_name: &output_name,
            log_path: &log_path,
            project_settings: &project_settings,
            package_meta: &package_meta,
            started,
        };
        match attempt.run(stage) {
            Ok(mut output) => {
                if escalated {
                    if compat.stage != Some(stage) || compat.crashes_in_safe_mode {
                        compat = EngineCompat {
                            stage: Some(stage),
                            crashes_in_safe_mode: false,
                        };
                        if let Err(e) = memory.set(&info.identity, &compat, "аварія рушія") {
                            eprintln!("[slice] сумісний режим не збережено: {e:#}");
                        }
                    }
                    output.compatibility_note = Some(if stage == Stage::NoProjectSettings && strippable {
                        NO_PROJECT_SETTINGS_NOTE
                    } else {
                        COMPATIBLE_NOTE
                    });
                }
                return Ok(output);
            }
            Err(error) => {
                let crashed = error
                    .downcast_ref::<SliceFailure>()
                    .is_some_and(|f| f.crashed);
                if !crashed || cancellation.check().is_err() {
                    return Err(error);
                }
                if let Some(next) = stage
                    .next()
                    .filter(|s| *s != Stage::NoProjectSettings || strippable)
                {
                    eprintln!("[slice] рушій аварійно завершився на стадії {stage:?}, далі {next:?}");
                    stage = next;
                    escalated = true;
                    continue;
                }
                // Упав і без --export-3mf: лише тепер — м’яка порада спробувати іншу версію.
                compat.crashes_in_safe_mode = true;
                if let Err(e) = memory.set(&info.identity, &compat, "аварія і в сумісному режимі") {
                    eprintln!("[slice] {e:#}");
                }
                let mut failure = error.downcast::<SliceFailure>()?;
                failure.title = "Рушій аварійно завершується навіть у сумісному режимі";
                failure.hint = SAFE_MODE_FAILED_HINT;
                failure.safe_mode_exhausted = true;
                return Err(failure.into());
            }
        }
    }
}

/// Порада, коли не допоміг і сумісний режим: не вимога, а варіант.
pub const SAFE_MODE_FAILED_HINT: &str = "Slicenv уже спробував сумісний режим (лише G-code, без назв пластин, без налаштувань проєкту). Найчастіше допомагає інша версія рушія; якщо повторюється лише на цій моделі — надішли лог.";

/// Одна спроба нарізки на заданій стадії сумісного режиму.
struct SliceAttempt<'a> {
    engine: &'a Engine,
    info: &'a EngineInfo,
    request: &'a SliceRequest,
    cancellation: &'a SliceCancellation,
    work: &'a std::path::Path,
    output_name: &'a str,
    log_path: &'a std::path::Path,
    project_settings: &'a serde_json::Map<String, Value>,
    package_meta: &'a crate::safe_mode::PackageMeta,
    started: Instant,
}

impl SliceAttempt<'_> {
    fn run(&self, stage: Stage) -> Result<SliceOutput> {
        let Self {
            engine,
            info,
            work,
            output_name,
            log_path,
            cancellation,
            ..
        } = *self;
        let gcode_only = stage >= Stage::GcodeOnly;
        // Кількість слотів — з повного проєкту: у копії без налаштувань її вже не прочитати.
        let filaments = project_filament_count(&self.request.input);
        let gcode_path = work.join(format!("plate_{}.gcode", self.request.plate));
        let gcode_3mf = work.join(output_name);
        let _ = fs::remove_file(&gcode_3mf);
        let _ = fs::remove_file(&gcode_path);
        // Назви пластин у копії 3MF прибираються з першої безпечної стадії (OrcaSlicer#7920).
        let mut request = self.request.clone();
        if stage >= Stage::PlainPlates
            && !is_stl(&request.input)
            && let Some(input) = crate::safe_mode::without_plate_names(
                &request.input,
                &work.join("input-safe"),
            )?
        {
            request.input = input;
        }
        if stage >= Stage::NoProjectSettings
            && !is_stl(&request.input)
            && let Some(input) = crate::safe_mode::without_project_settings(
                &request.input,
                &work.join("input-bare"),
                self.project_settings,
            )?
        {
            request.input = input;
        }
        let export = (!gcode_only).then_some(output_name);
        let binary = fs::canonicalize(&engine.path)?;
        let run = |no_check: bool| -> Result<(std::process::ExitStatus, Option<Value>)> {
            let _ = fs::remove_file(work.join("result.json"));
            // Хвіст логу попередньої стадії не має показувати старий етап.
            let _ = fs::remove_file(work.join("00000.log"));
            cancellation.reset_progress();
            // Повтор з `--no-check` перепише G-code: готовим вважається лише новий файл.
            let _ = fs::remove_file(&gcode_path);
            cancellation.watch_gcode(Some(gcode_path.clone()));
            let mut args = command_args(&request, filaments, export, |f| info.supports_flag(f));
            if no_check {
                args.insert(0, "--no-check".into());
            }
            let _s = paths::profile::Span::new("slice::engine_process");
            let status = cancellation
                .run(
                    crate::engine::engine_command(&binary)
                        .current_dir(work)
                        .args(args)
                        .stdin(Stdio::null())
                        .stdout(fs::File::create(log_path)?)
                        .stderr(fs::File::create(work.join("slicer.err.log"))?),
                    None,
                )
                .with_context(|| format!("не запустився {}", engine.path.display()))?;
            let result = fs::read_to_string(work.join("result.json"))
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok());
            Ok((status, result))
        };
        let (mut status, mut result) = run(false)?;
        // Конфлікт траєкторій (об’єкти перетинаються): GUI Bambu/Orca лише попереджає
        // в Preview, а CLI завершується з помилкою. Повторюємо без перевірки й попереджаємо.
        let mut conflict_warning = None;
        if status.code() == Some(CLI_GCODE_PATH_CONFLICTS) && info.supports_flag("--no-check") {
            (status, result) = run(true)?;
            conflict_warning = Some(
                "Траєкторії об’єктів перетинаються (G-code path conflict): об’єкти накладаються \
                 або стоять надто близько. Bambu Studio й Orca лише попереджають про це — \
                 перевір шари в Preview або рознеси об’єкти."
                    .to_owned(),
            );
        }
        let produced = if gcode_only { &gcode_path } else { &gcode_3mf };
        if !status.success()
            || !produced.is_file()
            || result
                .as_ref()
                .and_then(|r| r.get("return_code"))
                .and_then(Value::as_i64)
                .is_some_and(|code| code != 0)
        {
            let reason = result
                .as_ref()
                .and_then(|r| r.get("error_string"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mut details = engine_errors(&fs::read_to_string(log_path).unwrap_or_default());
            details.extend(
                fs::read_to_string(work.join("slicer.err.log"))
                    .unwrap_or_default()
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .take(4)
                    .map(|l| l.chars().take(300).collect::<String>()),
            );
            // return_code у result.json — код CLI (-62); без нього — код виходу як i8 (194 → -62).
            // Аварія (сигнал, на Windows NTSTATUS 0xC0000005) — не код CLI: молодший байт дав би
            // оманливий «код 5».
            let signal = paths::host::exit_signal(&status);
            let code = result
                .as_ref()
                .and_then(|r| r.get("return_code"))
                .and_then(Value::as_i64)
                .map(|c| c as i32)
                .or_else(|| {
                    status
                        .code()
                        .filter(|_| signal.is_none())
                        .map(|c| c as u8 as i8 as i32)
                });
            let (kind, title, hint) = explain_failure(code, signal);
            // Аварія — сигнал/NTSTATUS або вихід без result.json (рушій не дійшов до власної
            // обробки помилок): привід спробувати безпечнішу стадію.
            let crashed = signal.is_some() || result.is_none();
            return Err(SliceFailure {
                engine: engine.kind.name().to_owned(),
                reason: reason.to_owned(),
                code,
                signal,
                kind,
                title,
                hint,
                details,
                log: log_path.to_owned(),
                crashed,
                safe_mode_exhausted: false,
            }
            .into());
        }

        let _read_span = paths::profile::Span::new("slice::read_result");
        let result_plate = result
            .as_ref()
            .and_then(|r| r.get("sliced_plates"))
            .and_then(Value::as_array)
            .and_then(|plates| {
                plates
                    .iter()
                    .find(|p| p.get("id").and_then(Value::as_u64) == Some(request.plate as u64))
                    .or_else(|| {
                        (plates.len() == 1 && plates[0].get("id").is_none()).then(|| &plates[0])
                    })
            });
        let result_time = result_plate
            .and_then(|p| finite_nonnegative(p.get("main_predication").and_then(Value::as_f64)));
        let result_weight = result_plate
            .and_then(|p| p.get("filaments"))
            .and_then(Value::as_array)
            .and_then(|fs| {
                fs.iter()
                    .map(|f| f.get("total_used_g").and_then(Value::as_f64))
                    .collect::<Option<Vec<_>>>()
            })
            .map(|values| values.into_iter().sum::<f64>())
            .and_then(|value| finite_nonnegative(Some(value)));
        // Лише G-code: час, вага й шари — з шапки та хвоста G-code, архів збирає Slicenv.
        let summary = if gcode_only {
            Some(crate::safe_mode::summarize_gcode(&gcode_path)?)
        } else {
            None
        };
        let (archive_time, archive_weight, archive_warnings) = match &summary {
            Some(summary) => (summary.print_time, summary.filament_grams, Vec::new()),
            None => read_slice_info(&gcode_3mf, request.plate)?,
        };
        let print_time = result_time.or(archive_time).context("slicer output is missing a valid print-time prediction (result.json and Metadata/slice_info.config)")?;
        let filament_grams = result_weight.or(archive_weight).context("slicer output is missing a valid filament weight (result.json and Metadata/slice_info.config)")?;
        if let Some(summary) = &summary {
            let summary = crate::safe_mode::GcodeSummary {
                print_time: Some(print_time),
                filament_grams: Some(filament_grams),
                ..summary.clone()
            };
            crate::safe_mode::package_gcode(
                &gcode_path,
                &gcode_3mf,
                request.plate,
                &summary,
                self.project_settings,
                self.package_meta,
            )?;
        }
        let mut warnings: Vec<String> = conflict_warning.into_iter().collect();
        if let Some(message) = result_plate
            .and_then(|p| p.get("warning_message").and_then(Value::as_str))
            .filter(|s| !s.trim().is_empty())
        {
            warnings.push(message.to_owned());
        }
        warnings.extend(archive_warnings);
        for name in ["slicer.log", "slicer.err.log", "00000.log"] {
            if let Ok(log) = fs::read_to_string(work.join(name)) {
                for message in log.lines().filter_map(log_warning) {
                    if !warnings.contains(&message) && warnings.len() < 20 {
                        warnings.push(message);
                    }
                }
            }
        }

        Ok(SliceOutput {
            gcode_3mf,
            print_time: Duration::try_from_secs_f64(print_time)
                .context("invalid print-time prediction")?,
            filament_grams,
            layer_height: result
                .as_ref()
                .and_then(|r| r.get("layer_height"))
                .and_then(Value::as_f64)
                .or(summary.as_ref().and_then(|s| s.layer_height))
                // result.json Orca 2.3 висоти шару не має; беремо з процесу, з яким нарізано.
                .or_else(|| {
                    let value = self.project_settings.get("layer_height")?;
                    value.as_f64().or_else(|| value.as_str()?.trim().parse().ok())
                })
                .unwrap_or(0.0),
            elapsed: self.started.elapsed(),
            warnings,
            compatibility_note: None,
        })
    }
}

fn finite_nonnegative(value: Option<f64>) -> Option<f64> {
    value.filter(|number| number.is_finite() && *number >= 0.0)
}

pub(crate) fn read_slice_info(
    path: &std::path::Path,
    requested_plate: u32,
) -> Result<(Option<f64>, Option<f64>, Vec<String>)> {
    use quick_xml::events::Event;
    let mut archive = zip::ZipArchive::new(fs::File::open(path)?)?;
    let mut file = archive
        .by_name("Metadata/slice_info.config")
        .with_context(|| format!("{} is missing Metadata/slice_info.config", path.display()))?;
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut file, &mut xml)?;
    let mut reader = quick_xml::Reader::from_str(&xml);
    let mut in_plate = false;
    let mut plate_index = None;
    let mut prediction = None;
    let mut weight = None;
    let mut warnings = Vec::new();
    let mut plate_warnings = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref event)) | Ok(Event::Empty(ref event))
                if event.name().as_ref() == "plate" =>
            {
                in_plate = true;
                plate_index = None;
                prediction = None;
                weight = None;
                plate_warnings.clear();
            }
            Ok(Event::End(ref event)) if event.name().as_ref() == "plate" => {
                if plate_index == Some(requested_plate) {
                    warnings = std::mem::take(&mut plate_warnings);
                    break;
                }
                in_plate = false;
            }
            Ok(Event::Empty(ref event)) if in_plate && event.name().as_ref() == "metadata" => {
                let mut key = None;
                let mut value = None;
                for attr in event.attributes().with_checks(false).flatten() {
                    let decoded = attr
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)?
                        .into_owned();
                    match attr.key.as_ref() {
                        "key" => key = Some(decoded),
                        "value" => value = Some(decoded),
                        _ => {}
                    }
                }
                match (key.as_deref(), value.as_deref()) {
                    (Some("index"), Some(value)) => plate_index = value.parse::<u32>().ok(),
                    (Some("prediction"), Some(value)) => {
                        prediction = value
                            .parse::<f64>()
                            .ok()
                            .and_then(|v| finite_nonnegative(Some(v)))
                    }
                    (Some("weight"), Some(value)) => {
                        weight = value
                            .parse::<f64>()
                            .ok()
                            .and_then(|v| finite_nonnegative(Some(v)))
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(ref event)) if in_plate && event.name().as_ref() == "warning" => {
                for attr in event.attributes().with_checks(false).flatten() {
                    if attr.key.as_ref() == "msg" {
                        let message = attr
                            .normalized_value(quick_xml::XmlVersion::Implicit1_0)?
                            .into_owned();
                        if !message.trim().is_empty() {
                            plate_warnings.push(message);
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(error.into()),
            _ => {}
        }
    }
    if plate_index != Some(requested_plate) {
        return Ok((None, None, Vec::new()));
    }
    Ok((prediction, weight, warnings))
}

/// Прибирає з конфігу проєкту значення, які туди записав Slicenv до виправлення розгортання
/// `inherits` ([`PresetStore::stale_project_values`]) і які наші JSON не перекриють: рушій
/// візьме типове, як GUI з цим пресетом. Повертає, чи щось прибрано.
pub(crate) fn drop_stale_keys(
    store: &PresetStore,
    selection: &crate::Selection,
    effective: &serde_json::Map<String, Value>,
    config: &mut serde_json::Map<String, Value>,
) -> bool {
    let mut stale = Vec::new();
    for (kind, name) in [
        (Kind::Machine, &selection.machine),
        (Kind::Process, &selection.process),
        (Kind::Filament, &selection.filament),
    ] {
        let found = store.stale_project_values(kind, name, config);
        stale.extend(found.into_iter().map(|(key, _)| key).filter(|key| !effective.contains_key(key)));
    }
    for key in &stale {
        config.remove(key);
    }
    if !stale.is_empty() {
        eprintln!("[slice] із конфігу проєкту прибрано застарілі ключі: {}", stale.join(", "));
    }
    !stale.is_empty()
}

/// Аргументи CLI. Необов’язкові прапорці передаються, лише якщо рушій їх знає (`supports`):
/// старі збірки не мають, наприклад, `--allow-newer-file`, а невідомий прапорець для них —
/// помилка. `export` — ім’я `.gcode.3mf`; `None` — лише G-code (сумісний режим).
fn command_args(
    request: &SliceRequest,
    filaments: usize,
    export: Option<&str>,
    supports: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    if request.allow_newer_file && supports("--allow-newer-file") {
        args.push("--allow-newer-file".into());
    }
    if supports("--debug") {
        // Відсоток, етап і номер шару — лише в лозі: статус нарізки рушій пише на рівні
        // debug (`default_status_callback`), див. `SliceCancellation::read_logs`.
        args.extend(["--debug".into(), "4".into()]);
    }
    args.extend([
        "--load-settings".into(),
        "machine.json;process.json".into(),
        "--load-filaments".into(),
        // Вибраний філамент — у кожен слот проєкту: інакше решта слотів лишається з
        // 3MF (наприклад PLA поруч з вибраним ABS), і Bambu відмовляє: -62.
        vec!["filament.json"; filaments.max(1)].join(";"),
    ]);
    if supports("--load-defaultfila") {
        args.push("--load-defaultfila".into());
    }
    if supports("--orient") {
        args.extend(["--orient".into(), "0".into()]);
    }
    if supports("--arrange") {
        args.extend([
            "--arrange".into(),
            if is_stl(&request.input) { "1" } else { "0" }.into(),
        ]);
    }
    args.extend([
        "--slice".into(),
        request.plate.to_string(),
        "--outputdir".into(),
        ".".into(),
    ]);
    if let Some(name) = export {
        args.extend(["--export-3mf".into(), name.into()]);
    }
    args.push(request.input.to_string_lossy().into_owned());
    args
}

fn safe_component(s: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hash);
    let prefix: String = s
        .chars()
        .take(64)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{prefix}-{:016x}", hash.finish())
}

/// Кількість слотів філаменту в проєкті 3MF (`filament_settings_id`); для STL — 1.
fn project_filament_count(path: &std::path::Path) -> usize {
    let count = (|| -> Option<usize> {
        let mut archive = zip::ZipArchive::new(std::io::BufReader::new(fs::File::open(path).ok()?)).ok()?;
        let file = archive.by_name("Metadata/project_settings.config").ok()?;
        let config: Value = serde_json::from_reader(std::io::BufReader::new(file)).ok()?;
        ["filament_settings_id", "filament_colour", "filament_type"]
            .iter()
            .find_map(|key| config[key].as_array().map(Vec::len))
    })();
    count.unwrap_or(1).clamp(1, 32)
}

/// Кольори філаментів із `Metadata/project_settings.config` вхідного 3MF.
fn project_colours(input: &std::path::Path) -> Vec<String> {
    use std::io::Read as _;
    if is_stl(input) {
        return Vec::new();
    }
    (|| {
        let mut zip = zip::ZipArchive::new(fs::File::open(input).ok()?).ok()?;
        let mut text = String::new();
        zip.by_name("Metadata/project_settings.config")
            .ok()?
            .read_to_string(&mut text)
            .ok()?;
        let json: Value = serde_json::from_str(&text).ok()?;
        Some(
            json.get("filament_colour")?
                .as_array()?
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.get(..7).unwrap_or(s).to_owned()))
                .collect(),
        )
    })()
    .unwrap_or_default()
}

fn is_stl(path: &std::path::Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("stl"))
}

pub fn cache_dir() -> PathBuf {
    paths::cache_dir().join("sliced")
}

/// Рядок журналу рушія, вартий показу як попередження. «No substitutions performed» —
/// звіт про те, що нічого не замінено, а не попередження.
fn log_warning(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    if lower.contains("no substitutions") {
        return None;
    }
    ["unknown option", "unrecognized", "substitut", "out of range"]
        .iter()
        .any(|s| lower.contains(s))
        .then(|| line.chars().take(500).collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn log_warning_skips_reports_of_no_substitutions() {
        assert_eq!(log_warning("no substitutions performed from file machine.json"), None);
        assert_eq!(log_warning("[info] loading model"), None);
        assert_eq!(
            log_warning("substituted value of layer_height").as_deref(),
            Some("substituted value of layer_height")
        );
        assert!(log_warning("Unknown option --foo").is_some());
    }

    use super::*;

    #[test]
    fn failures_are_attributed() {
        assert_eq!(explain_failure(Some(-62), None).0, FailureKind::Model);
        assert_eq!(explain_failure(Some(-5), None).0, FailureKind::App);
        assert_eq!(explain_failure(Some(-14), None).0, FailureKind::System);
        assert_eq!(explain_failure(None, Some(11)).0, FailureKind::System);
        assert_eq!(
            engine_errors("[x] [warning] a\n[x] [error]   got error when validate: hot\n"),
            vec!["got error when validate: hot".to_owned()]
        );
        // Код виходу процесу — return_code як u8: 194 → -62.
        assert_eq!(194u8 as i8, -62);
    }

    #[test]
    fn coarse_progress_uses_last_percentage() {
        assert_eq!(progress_percent("Slicing 15%\nExporting 92%\n"), Some(92));
        assert_eq!(progress_percent("Slicing..."), None);
        let status = |percent, stage: &str, layer| Some(Status { percent, stage: stage.into(), layer });
        let log = "[debug] default_status_callback: percent=10, warning_step=-1, message=Slicing mesh, message_type=0\n[debug] default_status_callback: percent=45, warning_step=-1, message=Generating perimeters, message_type=0\n[trace] x\n";
        assert_eq!(status_line(log), status(Some(45), "Generating perimeters", None));
        assert_eq!(
            status_line("default_status_callback: percent=-1, warning_step=3, message=, message_type=0"),
            status(None, "", None)
        );
        assert_eq!(status_line("nothing"), None);
        assert_eq!(progress_percent("invalid 200%"), None);
    }

    /// Справжні рядки `--debug 4` (без `--pipe`): Bambu Studio 02.07.01.62 пише в stdout з часом
    /// і потоком, OrcaSlicer 2.4.2 і 2.5.0-beta — у `00000.log` голими рядками.
    #[test]
    fn gcode_layer_comes_from_engine_log() {
        let bambu = "[2026-11-09 00:19:02.600407] [0x00007f87a116bf80] [debug]   default_status_callback: percent=71, warning_step=-1, message=Detect overhangs for auto-lift, message_type=0\n\
[2026-11-09 00:19:02.753057] [0x00007f87a116bf80] [debug]   default_status_callback: percent=80, warning_step=-1, message=Generating G-code: layer 80, message_type=0\n\
[2026-11-09 00:19:02.785241] [0x00007f87a116bf80] [debug]   Start processing gcode,  Resident memory: 118MB; Shared memory: 68MB; Private memory: 50MB; Peak memory usage: 117MB\n";
        let expected = Some(Status { percent: Some(80), stage: "Generating G-code".into(), layer: Some(80) });
        assert_eq!(status_line(bambu), expected);
        let orca = "default_status_callback: percent=80, warning_step=-1, message=Generating G-code, message_type=0\n\
default_status_callback: percent=80, warning_step=-1, message=Generating G-code: layer 80, message_type=0\n\
Start processing gcode,  Resident memory: 333MB; Shared memory: 84MB; Private memory: 249MB; Peak memory usage: 333MB\n";
        assert_eq!(status_line(orca), expected);
        // Етап без шару скидає номер; двокрапка без числа — частина назви етапу.
        let plain = "default_status_callback: percent=80, warning_step=-1, message=Generating G-code, message_type=0";
        assert_eq!(status_line(plain).unwrap().layer, None);
        let odd = "default_status_callback: percent=5, warning_step=-1, message=Check: layer x, message_type=0";
        assert_eq!(status_line(odd).unwrap().stage, "Check: layer x");
    }

    #[test]
    fn progress_is_read_from_orca_log_in_work_dir() {
        let dir = std::env::temp_dir().join(format!("slicenv-orca-log-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let orca = dir.join("00000.log");
        fs::write(&orca, "default_status_callback: percent=80, warning_step=-1, message=Generating G-code: layer 12, message_type=0\n").unwrap();
        let token = SliceCancellation::default();
        token.0.lock().unwrap().logs = vec![dir.join("slicer.log"), orca];
        token.read_logs();
        assert_eq!((token.progress(), token.stage().as_deref(), token.layer()), (Some(80), Some("Generating G-code"), Some(12)));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn cancellation_kills_and_reaps_sleeping_engine() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("engine");
        fs::write(&script, "#!/bin/sh\nexec sleep 30\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let token = SliceCancellation::default();
        let worker = token.clone();
        let thread = std::thread::spawn(move || worker.run(&mut Command::new(script), None));
        let deadline = Instant::now() + Duration::from_secs(2);
        let pid = loop {
            if let Some(child) = token.0.lock().unwrap().child.as_ref() {
                break child.id();
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        };
        let start = Instant::now();
        token.cancel();
        assert!(thread.join().unwrap().unwrap_err().is::<Cancelled>());
        assert!(start.elapsed() < Duration::from_millis(200));
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        assert!(
            token
                .run(Command::new("sleep").arg("30"), None)
                .unwrap_err()
                .is::<Cancelled>()
        );
    }

    fn request(input: &str) -> SliceRequest {
        SliceRequest {
            input: input.into(),
            plate: 3,
            allow_newer_file: false,
            selection: Selection {
                machine: "m".into(),
                process: "p".into(),
                filament: "f".into(),
                bed_type: "bed".into(),
            },
            overrides: Overrides::default(),
        }
    }

    #[test]
    #[cfg(unix)]
    fn mock_process_receives_exact_orca_arguments() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let capture = dir.path().join("args");
        let script = dir.path().join("mock-orca");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
                capture.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let args = command_args(&request("part.stl"), 1, Some("part-plate3.gcode.3mf"), |_| true);
        assert!(spawn_retrying_busy(Command::new(script).args(&args)).unwrap().wait().unwrap().success());
        assert_eq!(
            fs::read_to_string(capture).unwrap(),
            "--debug\n4\n--load-settings\nmachine.json;process.json\n--load-filaments\nfilament.json\n--load-defaultfila\n--orient\n0\n--arrange\n1\n--slice\n3\n--outputdir\n.\n--export-3mf\npart-plate3.gcode.3mf\npart.stl\n"
        );
    }

    #[test]
    fn reads_archive_metrics_and_warnings_for_requested_plate() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("slice.gcode.3mf");
        let file = fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file(
            "Metadata/slice_info.config",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(br#"<config><plate><metadata key="index" value="1"/><metadata key="prediction" value="99"/><metadata key="weight" value="8"/></plate><plate><metadata key="index" value="2"/><metadata key="prediction" value="1212"/><metadata key="weight" value="3.67"/><warning msg="bed temperature warning" level="3"/></plate></config>"#).unwrap();
        zip.finish().unwrap();
        let (time, weight, warnings) = read_slice_info(&path, 2).unwrap();
        assert_eq!(time, Some(1212.0));
        assert_eq!(weight, Some(3.67));
        assert_eq!(warnings, ["bed temperature warning"]);
        assert_eq!(read_slice_info(&path, 3).unwrap().0, None);
    }

    #[test]
    fn result_metrics_must_be_finite_and_nonnegative() {
        assert_eq!(finite_nonnegative(Some(4.0)), Some(4.0));
        assert_eq!(finite_nonnegative(Some(-1.0)), None);
        assert_eq!(finite_nonnegative(Some(f64::INFINITY)), None);
        assert_eq!(finite_nonnegative(Some(f64::NAN)), None);
        let result: Value =
            serde_json::from_str(r#"{"return_code":2,"error_string":"failed"}"#).unwrap();
        assert_ne!(result.get("return_code").and_then(Value::as_i64), Some(0));
    }

    #[test]
    fn newer_file_opt_in_is_a_bare_flag() {
        let mut request = request("part.3mf");
        request.allow_newer_file = true;
        assert_eq!(
            command_args(&request, 1, Some("out.3mf"), |_| true)[0],
            "--allow-newer-file"
        );
        assert_ne!(command_args(&request, 1, Some("out.3mf"), |_| true)[1], "1");
    }

    #[test]
    fn old_engine_gets_only_flags_it_knows_and_safe_mode_drops_export() {
        let mut request = request("part.3mf");
        request.allow_newer_file = true;
        // Як OrcaSlicer 1.9/2.0: без --allow-newer-file.
        let old = |f: &str| f != "--allow-newer-file";
        let args = command_args(&request, 1, Some("out.gcode.3mf"), old);
        assert!(!args.contains(&"--allow-newer-file".to_owned()));
        assert!(args.contains(&"--export-3mf".to_owned()));
        let minimal = |f: &str| ["--slice", "--outputdir", "--load-settings", "--load-filaments"].contains(&f);
        assert_eq!(
            command_args(&request, 1, None, minimal),
            ["--load-settings", "machine.json;process.json", "--load-filaments", "filament.json", "--slice", "3", "--outputdir", ".", "part.3mf"]
        );
    }
}
