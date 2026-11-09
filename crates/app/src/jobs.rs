//! Фонові задачі документа: одна модель для нарізки, орієнтації, плавців, булевого
//! прев’ю, G-code, імпорту/заміни, мініатюр і важких MCP-операцій.
//!
//! - [`Jobs`] тримає не більше однієї задачі кожного [`JobKind`]: нова задача того ж
//!   виду скасовує попередню (хук скасування + drop `Task`).
//! - [`JobTarget`] — знімок ревізій документа, узятий ДО першого `await`; перед
//!   застосуванням результату [`JobTarget::is_current`] — єдина перевірка застарілості.
//! - Рядок стану читає [`Jobs::status`], тож усі задачі показуються однаково.
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use gpui::{AppContext as _, Context, SharedString};
use project::{InstanceRef, Project};

use crate::workspace::Workspace;

/// Вид фонової задачі. Одночасно — щонайбільше одна задача кожного виду.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum JobKind {
    Open,
    Slice,
    GcodePreview,
    ToolpathBuild,
    Orient,
    Fins,
    BooleanPreview,
    Import,
    Replace,
    Thumbnails,
    McpEdit,
    McpSlice,
}

impl JobKind {
    pub const ALL: [JobKind; 12] = [
        JobKind::Open,
        JobKind::Slice,
        JobKind::GcodePreview,
        JobKind::ToolpathBuild,
        JobKind::Orient,
        JobKind::Fins,
        JobKind::BooleanPreview,
        JobKind::Import,
        JobKind::Replace,
        JobKind::Thumbnails,
        JobKind::McpEdit,
        JobKind::McpSlice,
    ];

    /// Типова позначка в рядку стану (ключ перекладу); `None` — тиха задача.
    pub fn label(self) -> Option<&'static str> {
        Some(match self {
            JobKind::Open => "Відкриваю…",
            JobKind::Slice => "Нарізка…",
            JobKind::GcodePreview => "Завантажую G-code…",
            JobKind::Orient => "Автоорієнтація…",
            JobKind::Fins => "Плавці…",
            JobKind::BooleanPreview => "Обчислюю віднімання…",
            JobKind::Import => "Додаю моделі…",
            JobKind::Replace => "Замінюю сітки…",
            JobKind::McpEdit => "AI-агент редагує…",
            JobKind::McpSlice => "AI-агент нарізає…",
            JobKind::ToolpathBuild | JobKind::Thumbnails => return None,
        })
    }

    /// Попередження, коли результат відкинуто як застарілий; `None` — мовчки.
    pub fn stale_message(self) -> Option<&'static str> {
        Some(match self {
            JobKind::Orient => "Проєкт змінено. Автоорієнтацію не застосовано.",
            JobKind::Fins => "Проєкт змінено. Створіть плавці знову.",
            JobKind::Import => "Проєкт або пластину змінено. Повторіть додавання моделей.",
            JobKind::Replace => "Проєкт змінено. Повторіть заміну.",
            _ => return None,
        })
    }

    /// Показувати секунди роботи (довгі задачі з тікером).
    pub fn shows_elapsed(self) -> bool {
        matches!(self, JobKind::Slice | JobKind::McpSlice)
    }

    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct JobId(u64);

/// Що задача може скасувати поза своїм `Task` (наприклад, убити процес рушія).
type CancelHook = Box<dyn FnOnce()>;
/// Відсоток виконання, якщо задача його знає.
pub type Progress = Arc<dyn Fn() -> Option<u8> + Send + Sync>;
/// Поточний етап задачі, уже перекладений (для нарізки — з логу рушія, з номером шару).
pub type Stage = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// Те, що реєстр уміє «відпустити» без скасування: `gpui::Task` або тестова заглушка.
pub trait JobTask {
    fn detach(self);
}
impl JobTask for gpui::Task<()> {
    fn detach(self) {
        gpui::Task::detach(self)
    }
}

/// Стан запущеної задачі для UI.
pub struct JobStatus {
    pub kind: JobKind,
    pub label: Option<SharedString>,
    pub started: Instant,
    pub progress: Option<Progress>,
    pub stage: Option<Stage>,
    /// Файл, з яким працює задача (картка бібліотеки показує спінер саме на ньому).
    pub subject: Option<PathBuf>,
}

impl JobStatus {
    /// Текст рядка стану: позначка, секунди, етап (для нарізки — з номером шару) і відсоток,
    /// якщо відомі.
    pub fn text(&self) -> Option<String> {
        let mut text = self.label.as_ref()?.to_string();
        if self.kind.shows_elapsed() {
            text = crate::tr_format!("{} {} с", text, self.started.elapsed().as_secs());
        }
        if let Some(stage) = self.stage.as_ref().and_then(|s| s()) {
            text.push_str(&format!(" · {stage}"));
        }
        if let Some(percent) = self.progress.as_ref().and_then(|p| p()) {
            text.push_str(&format!(" · {percent}%"));
        }
        Some(text)
    }
}

struct Slot<T> {
    id: JobId,
    task: Option<T>,
    cancel: Option<CancelHook>,
    status: JobStatus,
}

/// Реєстр фонових задач Workspace.
pub struct Jobs<T: JobTask = gpui::Task<()>> {
    slots: [Option<Slot<T>>; JobKind::ALL.len()],
    next: u64,
    /// Посекундне оновлення рядка стану, поки йде задача з `shows_elapsed`.
    pub(crate) ticker: Option<gpui::Task<()>>,
}

impl<T: JobTask> Default for Jobs<T> {
    fn default() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
            next: 0,
            ticker: None,
        }
    }
}

impl<T: JobTask> Jobs<T> {
    /// Реєструє нову задачу виду `kind`, скасовуючи попередню того ж виду.
    pub fn begin(&mut self, kind: JobKind) -> JobId {
        self.cancel(kind);
        self.next += 1;
        let id = JobId(self.next);
        self.slots[kind.index()] = Some(Slot {
            id,
            task: None,
            cancel: None,
            status: JobStatus {
                kind,
                label: kind.label().map(|l| crate::i18n::tr(l).into()),
                started: Instant::now(),
                progress: None,
                stage: None,
                subject: None,
            },
        });
        id
    }

    fn slot_mut(&mut self, id: JobId) -> Option<&mut Slot<T>> {
        self.slots.iter_mut().flatten().find(|s| s.id == id)
    }

    /// Прив’язує `Task` до задачі. Якщо її вже замінили — `Task` скасовується (drop).
    pub fn attach(&mut self, id: JobId, task: T) {
        if let Some(slot) = self.slot_mut(id) {
            slot.task = Some(task);
        }
    }

    /// Хук скасування (kill процесу тощо); виконується при заміні, скасуванні чи drop.
    pub fn on_cancel(&mut self, id: JobId, hook: impl FnOnce() + 'static) {
        if let Some(slot) = self.slot_mut(id) {
            slot.cancel = Some(Box::new(hook));
        } else {
            hook();
        }
    }

    pub fn set_label(&mut self, id: JobId, label: impl Into<SharedString>) {
        if let Some(slot) = self.slot_mut(id) {
            slot.status.label = Some(label.into());
        }
    }

    pub fn set_progress(&mut self, id: JobId, progress: Progress) {
        if let Some(slot) = self.slot_mut(id) {
            slot.status.progress = Some(progress);
        }
    }

    pub fn set_stage(&mut self, id: JobId, stage: Stage) {
        if let Some(slot) = self.slot_mut(id) {
            slot.status.stage = Some(stage);
        }
    }

    pub fn set_subject(&mut self, id: JobId, subject: PathBuf) {
        if let Some(slot) = self.slot_mut(id) {
            slot.status.subject = Some(subject);
        }
    }

    /// Задача завершилась сама. `false` — її вже замінили чи скасували: результат не застосовувати.
    /// `Task` відпускається (detach), бо це викликається зсередини нього.
    pub fn finish(&mut self, id: JobId) -> bool {
        let Some(entry) = self
            .slots
            .iter_mut()
            .find(|s| s.as_ref().is_some_and(|s| s.id == id))
        else {
            return false;
        };
        if let Some(task) = entry.take().and_then(|s| s.task) {
            task.detach();
        }
        true
    }

    /// Скасовує задачу виду `kind`: хук, потім drop `Task`. `true` — щось ішло.
    pub fn cancel(&mut self, kind: JobKind) -> bool {
        let Some(slot) = self.slots[kind.index()].take() else {
            return false;
        };
        if let Some(hook) = slot.cancel {
            hook();
        }
        drop(slot.task);
        true
    }

    pub fn cancel_all(&mut self) {
        for kind in JobKind::ALL {
            self.cancel(kind);
        }
    }

    pub fn is_running(&self, kind: JobKind) -> bool {
        self.slots[kind.index()].is_some()
    }

    #[cfg(test)]
    pub fn current(&self, kind: JobKind) -> Option<JobId> {
        self.slots[kind.index()].as_ref().map(|s| s.id)
    }

    pub fn get(&self, kind: JobKind) -> Option<&JobStatus> {
        self.slots[kind.index()].as_ref().map(|s| &s.status)
    }

    /// Файл, який відкриває задача `kind`.
    pub fn subject(&self, kind: JobKind) -> Option<&Path> {
        self.get(kind)?.subject.as_deref()
    }

    /// Запущені задачі в порядку важливості для рядка стану.
    pub fn status(&self) -> impl Iterator<Item = &JobStatus> {
        self.slots.iter().flatten().map(|s| &s.status)
    }

    fn needs_ticker(&self) -> bool {
        self.status().any(|s| s.kind.shows_elapsed())
    }
}

impl<T: JobTask> Drop for Jobs<T> {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

/// Ревізії документа на момент запуску задачі.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Stamp {
    /// Відкритий документ (`project_epoch`).
    pub epoch: u64,
    /// Геометрія й розташування (`geometry_revision`).
    pub geometry: u64,
    /// Рушій (`engine_revision`).
    pub engine: u64,
    /// Вхід нарізки: значення редакторів, профілі, конфіг об’єктів (`slice_revision`).
    pub settings: u64,
    /// Показана пластина: індекс у `plates` і її номер.
    pub active_plate: Option<(usize, u32)>,
}

/// Те, проти чого перевіряється знімок: поточний стан Workspace (або тестовий).
pub struct JobContext<'a> {
    pub stamp: Stamp,
    pub project: Option<&'a Project>,
    /// Іде живий жест у сцені (перетягування, гізмо).
    pub gesture: bool,
}

/// Інстанс і його частини на момент запуску: ідентичність `Arc` меша замість
/// порівняння мільйонів вершин, трансформації — за значенням.
#[derive(Clone)]
struct InstanceStamp {
    key: InstanceRef,
    instance: project::Instance,
    volumes: Vec<(
        project::DAffine3,
        project::VolumeKind,
        Arc<project::IndexedMesh>,
    )>,
}

impl InstanceStamp {
    fn capture(project: &Project, key: InstanceRef) -> Option<Self> {
        Some(Self {
            key,
            instance: project.instance(key).ok()?.clone(),
            volumes: project
                .object(key.object)
                .ok()?
                .volumes
                .iter()
                .map(|v| (v.transform, v.kind, v.mesh.clone()))
                .collect(),
        })
    }

    fn unchanged_in(&self, project: &Project) -> bool {
        let (Ok(instance), Ok(object)) =
            (project.instance(self.key), project.object(self.key.object))
        else {
            return false;
        };
        *instance == self.instance
            && object.volumes.len() == self.volumes.len()
            && object
                .volumes
                .iter()
                .zip(&self.volumes)
                .all(|(v, (transform, kind, mesh))| {
                    v.transform == *transform && v.kind == *kind && Arc::ptr_eq(&v.mesh, mesh)
                })
    }
}

/// Знімок, до якого належить результат задачі. Будується до першого `await`:
/// `ws.job_target().geometry().idle_scene()`. Перевіряється перед роботою (якщо між
/// знімком і запуском був діалог) і перед застосуванням — однією функцією.
#[derive(Clone, Default)]
pub struct JobTarget {
    stamp: Stamp,
    document: bool,
    geometry: bool,
    settings: bool,
    active_plate: bool,
    idle_scene: bool,
    plate: Option<u32>,
    /// `None` — не перевіряється; `Some` з меншою кількістю — ціль зникла ще до запуску.
    instances: Option<(usize, Vec<InstanceStamp>)>,
}

impl JobTarget {
    /// Не прив’язана до документа (мініатюри, встановлення плагіна).
    pub fn any() -> Self {
        Self::default()
    }

    /// Прив’язка до відкритого документа (епохи).
    pub fn document(stamp: Stamp) -> Self {
        Self {
            stamp,
            document: true,
            ..Self::default()
        }
    }

    /// Будь-яка зміна геометрії чи розташування робить результат застарілим.
    pub fn geometry(mut self) -> Self {
        self.geometry = true;
        self
    }

    /// Зміна рушія або входу нарізки робить результат застарілим.
    pub fn settings(mut self) -> Self {
        self.settings = true;
        self
    }

    /// Результат належить показаній пластині.
    pub fn active_plate(mut self) -> Self {
        self.active_plate = true;
        self
    }

    /// Пластина з номером `number` має існувати.
    pub fn plate_exists(mut self, number: u32) -> Self {
        self.plate = Some(number);
        self
    }

    /// Застосовувати лише без живого жесту в сцені.
    pub fn idle_scene(mut self) -> Self {
        self.idle_scene = true;
        self
    }

    /// Ці інстанси (їхні трансформації, частини й меші) мають лишитися як у `project`.
    pub fn instances(mut self, project: &Project, keys: &[InstanceRef]) -> Self {
        let stamps = keys
            .iter()
            .filter_map(|k| InstanceStamp::capture(project, *k))
            .collect();
        self.instances = Some((keys.len(), stamps));
        self
    }

    /// Єдина перевірка застарілості.
    pub fn is_current_in(&self, now: &JobContext) -> bool {
        let (a, b) = (&self.stamp, &now.stamp);
        let needs_project = self.plate.is_some() || self.instances.is_some();
        !(self.document && a.epoch != b.epoch
            || self.geometry && a.geometry != b.geometry
            || self.settings && (a.engine != b.engine || a.settings != b.settings)
            || self.active_plate && a.active_plate != b.active_plate
            || self.idle_scene && now.gesture
            || needs_project && now.project.is_none()
            || self.plate.is_some_and(|number| {
                !now.project
                    .is_some_and(|p| p.plates.iter().any(|plate| plate.index == number))
            })
            || self.instances.as_ref().is_some_and(|(count, stamps)| {
                stamps.len() != *count
                    || !now
                        .project
                        .is_some_and(|p| stamps.iter().all(|s| s.unchanged_in(p)))
            }))
    }

    pub fn is_current(&self, ws: &Workspace) -> bool {
        self.is_current_in(&ws.job_context())
    }
}

impl Workspace {
    pub(crate) fn job_stamp(&self) -> Stamp {
        Stamp {
            epoch: self.project_epoch,
            geometry: self.geometry_revision,
            engine: self.engine_revision,
            settings: self.slice_revision,
            active_plate: self
                .plates
                .get(self.active_plate)
                .map(|p| (self.active_plate, p.number)),
        }
    }

    pub(crate) fn job_context(&self) -> JobContext<'_> {
        JobContext {
            stamp: self.job_stamp(),
            project: self.project.as_ref(),
            gesture: self.moving.is_some() || self.gizmo_drag.is_some() || self.live_drag.is_some(),
        }
    }

    /// Знімок для нової задачі, прив’язаний до відкритого документа.
    pub(crate) fn job_target(&self) -> JobTarget {
        JobTarget::document(self.job_stamp())
    }

    /// Відкинутий застарілий результат: попередження за видом задачі.
    pub(crate) fn reject_stale(&mut self, kind: JobKind, cx: &mut Context<Self>) {
        if let Some(message) = kind.stale_message() {
            self.toast(
                crate::widgets::ToastKind::Warning,
                crate::i18n::tr(message),
                cx,
            );
        }
        cx.notify();
    }

    /// Запускає `work` у фоні як задачу `kind` (попередня того ж виду скасовується).
    /// `apply` виконується на UI-потоці лише якщо задача не замінена й `target` чинний;
    /// інакше — попередження виду, а `apply` (з усім захопленим) просто відкидається.
    pub(crate) fn spawn_job<R: Send + 'static>(
        &mut self,
        kind: JobKind,
        target: JobTarget,
        work: impl FnOnce() -> R + Send + 'static,
        apply: impl FnOnce(&mut Self, R, &mut gpui::Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> Option<JobId> {
        // Знімок міг застаріти ще до запуску (наприклад, поки був відкритий діалог файлів).
        if !target.is_current(self) {
            self.reject_stale(kind, cx);
            return None;
        }
        // Не `cx.windows()`: під час створення вікна (файл з командного рядка) воно ще не
        // зареєстроване, і задача мовчки не стартувала б.
        let window = self.window_handle;
        let id = self.jobs.begin(kind);
        let task = cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { work() }).await;
            cx.update_window(window, |_, window, cx| {
                this.update(cx, |this, cx| {
                    if !this.jobs.finish(id) {
                        return;
                    }
                    if !target.is_current(this) {
                        this.reject_stale(kind, cx);
                        return;
                    }
                    apply(this, result, window, cx);
                    cx.notify();
                })
                .ok();
            })
            .ok();
        });
        self.jobs.attach(id, task);
        self.ensure_job_ticker(cx);
        cx.notify();
        Some(id)
    }

    /// Один тікер на всі задачі з секундоміром; зупиняється, щойно таких немає.
    pub(crate) fn ensure_job_ticker(&mut self, cx: &mut Context<Self>) {
        if self.jobs.ticker.is_some() || !self.jobs.needs_ticker() {
            return;
        }
        self.jobs.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                let active = this
                    .update(cx, |this, cx| {
                        if !this.jobs.needs_ticker() {
                            // Власний Task: відпустити, а не скасовувати посеред виконання.
                            if let Some(ticker) = this.jobs.ticker.take() {
                                ticker.detach();
                            }
                            return false;
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !active {
                    break;
                }
            }
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};

    /// Заглушка `Task`: знає, чи її скасовано (drop) чи відпущено (detach).
    struct Probe {
        dropped: Rc<Cell<u32>>,
        detached: bool,
    }
    impl JobTask for Probe {
        fn detach(mut self) {
            self.detached = true;
        }
    }
    impl Drop for Probe {
        fn drop(&mut self) {
            if !self.detached {
                self.dropped.set(self.dropped.get() + 1);
            }
        }
    }
    fn probe(dropped: &Rc<Cell<u32>>) -> Probe {
        Probe {
            dropped: dropped.clone(),
            detached: false,
        }
    }

    #[test]
    fn new_job_of_same_kind_cancels_the_old_one() {
        let mut jobs = Jobs::<Probe>::default();
        let dropped = Rc::new(Cell::new(0));
        let killed = Rc::new(Cell::new(false));
        let first = jobs.begin(JobKind::Slice);
        jobs.attach(first, probe(&dropped));
        let flag = killed.clone();
        jobs.on_cancel(first, move || flag.set(true));
        // Інший вид не зачіпає нарізку.
        let orient = jobs.begin(JobKind::Orient);
        jobs.attach(orient, probe(&dropped));
        assert!(!killed.get());

        let second = jobs.begin(JobKind::Slice);
        assert!(killed.get(), "хук скасування (kill рушія) виконано");
        assert_eq!(dropped.get(), 1, "старий Task скасовано");
        assert_eq!(jobs.current(JobKind::Slice), Some(second));
        assert!(
            !jobs.finish(first),
            "результат заміненої задачі не застосовується"
        );
        assert!(jobs.is_running(JobKind::Orient));

        jobs.attach(second, probe(&dropped));
        assert!(jobs.finish(second));
        assert_eq!(
            dropped.get(),
            1,
            "завершена задача відпускається, а не скасовується"
        );
        assert!(!jobs.is_running(JobKind::Slice));
        assert!(!jobs.finish(second));
    }

    #[test]
    fn cancel_and_drop_run_hooks_once_and_late_hooks_fire_immediately() {
        let mut jobs = Jobs::<Probe>::default();
        let dropped = Rc::new(Cell::new(0));
        let hooks = Rc::new(Cell::new(0));
        let id = jobs.begin(JobKind::Fins);
        jobs.attach(id, probe(&dropped));
        let h = hooks.clone();
        jobs.on_cancel(id, move || h.set(h.get() + 1));
        assert!(jobs.cancel(JobKind::Fins));
        assert!(!jobs.cancel(JobKind::Fins));
        assert_eq!((hooks.get(), dropped.get()), (1, 1));
        // Хук для вже скасованої задачі виконується одразу.
        let h = hooks.clone();
        jobs.on_cancel(id, move || h.set(h.get() + 1));
        assert_eq!(hooks.get(), 2);

        let id = jobs.begin(JobKind::McpSlice);
        let h = hooks.clone();
        jobs.on_cancel(id, move || h.set(h.get() + 1));
        drop(jobs);
        assert_eq!(hooks.get(), 3, "drop реєстру скасовує все");
    }

    #[test]
    fn status_reports_running_kinds_with_labels() {
        let mut jobs = Jobs::<Probe>::default();
        jobs.begin(JobKind::Thumbnails);
        let slice = jobs.begin(JobKind::Slice);
        jobs.set_label(slice, "Нарізаю пластину 2…");
        jobs.set_progress(slice, Arc::new(|| Some(40)));
        jobs.begin(JobKind::Orient);
        let shown: Vec<_> = jobs.status().filter_map(JobStatus::text).collect();
        assert_eq!(shown, ["Нарізаю пластину 2… 0 с · 40%", "Автоорієнтація…"]);
        assert!(jobs.needs_ticker());
        jobs.cancel(JobKind::Slice);
        assert!(!jobs.needs_ticker());
    }

    #[test]
    fn status_shows_stage_between_label_and_percent() {
        let mut jobs = Jobs::<Probe>::default();
        let id = jobs.begin(JobKind::Orient);
        jobs.set_label(id, "Orient");
        jobs.set_stage(id, Arc::new(|| Some("layer 80".into())));
        jobs.set_progress(id, Arc::new(|| Some(80)));
        let shown: Vec<_> = jobs.status().filter_map(JobStatus::text).collect();
        assert_eq!(shown, ["Orient · layer 80 · 80%"]);
    }

    fn project_with_plate() -> (Project, u32) {
        let mut project = Project::default();
        let plate = project.add_plate("plate").unwrap();
        project.objects.push(project::Object {
            id: 1,
            volumes: vec![project::Volume::default()],
            instances: vec![project::Instance::default()],
            ..Default::default()
        });
        (project, plate)
    }

    fn stamp() -> Stamp {
        Stamp {
            epoch: 1,
            geometry: 3,
            engine: 1,
            settings: 7,
            active_plate: Some((0, 1)),
        }
    }

    fn at<'a>(stamp: Stamp, project: Option<&'a Project>) -> JobContext<'a> {
        JobContext {
            stamp,
            project,
            gesture: false,
        }
    }

    #[test]
    fn result_is_applied_while_its_snapshot_is_current() {
        let (project, plate) = project_with_plate();
        let key = InstanceRef {
            object: 1,
            instance: 0,
        };
        let target = JobTarget::document(stamp())
            .geometry()
            .settings()
            .active_plate()
            .idle_scene()
            .plate_exists(plate)
            .instances(&project, &[key]);
        assert!(target.is_current_in(&at(stamp(), Some(&project))));
        // Незалежні зміни (метадані, інші ревізії, не перевірені цим видом) не заважають.
        let mut other = project.clone();
        other.metadata.insert("note".into(), "x".into());
        assert!(target.is_current_in(&at(stamp(), Some(&other))));
        assert!(JobTarget::document(stamp()).is_current_in(&at(
            Stamp {
                geometry: 9,
                settings: 9,
                ..stamp()
            },
            None
        )));
        assert!(JobTarget::any().is_current_in(&at(Stamp::default(), None)));
    }

    #[test]
    fn stale_result_is_rejected_after_epoch_geometry_or_settings_change() {
        let (project, plate) = project_with_plate();
        let target = JobTarget::document(stamp())
            .geometry()
            .settings()
            .plate_exists(plate);
        let changed = [
            Stamp {
                epoch: 2,
                ..stamp()
            },
            Stamp {
                geometry: 4,
                ..stamp()
            },
            Stamp {
                settings: 8,
                ..stamp()
            },
            Stamp {
                engine: 2,
                ..stamp()
            },
        ];
        for now in changed {
            assert!(!target.is_current_in(&at(now, Some(&project))), "{now:?}");
        }
        // Відкрито інший документ без проєкту або пластину видалено.
        assert!(!target.is_current_in(&at(stamp(), None)));
        let mut removed = project.clone();
        removed.plates.clear();
        assert!(!target.is_current_in(&at(stamp(), Some(&removed))));
        // Геометрія не перевіряється для задачі, яка її не замовила (G-code): лише налаштування.
        let gcode = JobTarget::document(stamp()).settings().active_plate();
        let moved = Stamp {
            geometry: 4,
            ..stamp()
        };
        assert!(gcode.is_current_in(&at(moved, None)));
        let other_plate = Stamp {
            active_plate: Some((1, 2)),
            ..stamp()
        };
        assert!(!gcode.is_current_in(&at(other_plate, None)));
    }

    #[test]
    fn live_gesture_and_changed_targets_reject_orientation() {
        let (project, _) = project_with_plate();
        let key = InstanceRef {
            object: 1,
            instance: 0,
        };
        let target = JobTarget::document(stamp())
            .idle_scene()
            .instances(&project, &[key]);
        let mut busy = at(stamp(), Some(&project));
        busy.gesture = true;
        assert!(!target.is_current_in(&busy));

        let mut moved = project.clone();
        moved.translate(key, project::DVec3::X).unwrap();
        assert!(!target.is_current_in(&at(stamp(), Some(&moved))));

        let mut remeshed = project.clone();
        Arc::make_mut(&mut remeshed.objects[0].volumes[0].mesh)
            .positions
            .push([0., 0., 0.]);
        assert!(!target.is_current_in(&at(stamp(), Some(&remeshed))));

        // Ціль, якої не було вже на момент запуску, ніколи не стає «чинною».
        let missing = JobTarget::document(stamp()).instances(
            &project,
            &[InstanceRef {
                object: 9,
                instance: 0,
            }],
        );
        assert!(!missing.is_current_in(&at(stamp(), Some(&project))));
    }
}
