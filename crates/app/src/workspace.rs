//! Головне вікно: стан проєкту, вкладки верхнього рівня (як в Orca) і фонові задачі.

use std::{path::PathBuf, sync::Arc};

use crate::ui::NoWindowDrag as _;
use gpui::{
    Context, Entity, Pixels, Point, RenderImage, SharedString, Subscription,
    Window, div, prelude::*, px,
};
use gpui_component::{TitleBar, tooltip::Tooltip};
use slicer::{BED_TYPES, Kind, PresetStore, Selection, SliceOutput, SliceRequest};
use viewer::{Camera, Mesh, Plate, Renderer, load_model};

use crate::{
    device::CloudState,
    jobs::{JobKind, JobTarget},
    library,
    preset_editor::PresetEditor,
    preview::Preview,
    theme::zed,
    ui::{self, IconName, Toggle as _},
    widgets::{Dropdown, DropdownEvent, ToastKind},
};

pub type PresetSelect = Entity<Dropdown>;
/// Рамка меша пластини (min, max).
pub type PlateBounds = (glam::Vec3, glam::Vec3);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Home,
    Prepare,
    Preview,
    Device,
    Project,
}

impl Page {
    pub const PROJECT_TABS: [Page; 3] = [Page::Prepare, Page::Preview, Page::Project];

    fn icon(self) -> IconName {
        match self {
            Page::Home => IconName::House,
            Page::Prepare => IconName::Box,
            Page::Preview => IconName::Layers,
            Page::Device => IconName::Printer,
            Page::Project => IconName::FileBox,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Page::Home => "Головна",
            Page::Prepare => "Підготовка",
            Page::Preview => "Перегляд",
            Page::Device => "Пристрій",
            Page::Project => "Проєкт",
        }
    }
}

/// Результат останньої нарізки. «Іде нарізка» — це `jobs.is_running(JobKind::Slice)`.
pub enum SliceState {
    Idle,
    Done(SliceOutput),
    /// Текст помилки й, якщо рушій відмовив, її розбір для картки в Preview.
    Failed(SharedString, Option<std::sync::Arc<slicer::SliceFailure>>),
}

pub struct Workspace {
    /// Фонові задачі документа (нарізка, орієнтація, плавці, імпорт, MCP…).
    pub jobs: crate::jobs::Jobs,
    pub scrub: Option<crate::scrub::Gesture>,
    pub scrub_bindings: std::collections::HashMap<gpui::EntityId, crate::scrub::Binding>,
    pub mcp: crate::mcp::State,
    pub preferences: crate::preferences::Preferences,
    pub page: Page,
    pub renderer: Renderer,
    pub camera: Camera,
    pub navigation: crate::navigation::Navigation,

    /// Перший корінь бібліотеки (або типова тека) — стартова тека діалогів.
    pub library_dir: PathBuf,
    /// Корені бібліотеки, закріплені теки (див. `home.rs`).
    pub library_roots: Vec<PathBuf>,
    /// Тека з аргументу командного рядка: показується лише в цьому сеансі.
    pub library_session_root: Option<PathBuf>,
    pub library: Vec<library::Entry>,
    /// Згорнуті теки дерева.
    pub library_collapsed: std::collections::HashSet<PathBuf>,
    pub library_menu: Option<(Point<Pixels>, crate::home::MenuTarget)>,
    /// Фонове сканування; нове замінює (скасовує) попереднє.
    pub library_scan: Option<gpui::Task<()>>,
    pub library_scanning: bool,
    /// Під м’ютексом: набір стежень оновлюється у фоні після сканування.
    pub library_watch: Option<Arc<std::sync::Mutex<library::Watch>>>,
    pub library_search: Entity<gpui_component::input::InputState>,

    /// Відкритий файл; `None` — демо-кубик.
    pub path: Option<PathBuf>,
    pub model_name: SharedString,
    /// Рамки мешів пластин (обхід усіх вершин — мілісекунди на важкій моделі).
    /// Скидається `invalidate_plate_bounds` скрізь, де змінюються меші пластин.
    pub plate_bounds: std::cell::RefCell<Option<Vec<Option<PlateBounds>>>>,
    pub plates: Vec<Plate>,
    /// Вікно цього Workspace — для застосування результатів фонових задач.
    pub window_handle: gpui::AnyWindowHandle,
    pub active_plate: usize,
    pub error: Option<SharedString>,

    pub engine: Option<slicer::Engine>,
    pub engine_label: SharedString,
    /// `EngineInfo::identity` активного рушія — ключ пам’яті сумісного режиму (`slicer::engine_compat`).
    pub engine_identity: Option<String>,
    pub packages: crate::packages::Packages,
    pub engine_loading: bool,
    pub profile_recovery: Option<(slicer::Engine, String, String)>,
    pub allow_newer_file: bool,
    pub engine_revision: u64,
    pub settings_schema: &'static settings::Schema,
    pub presets: Option<Arc<PresetStore>>,
    pub selection: Selection,
    pub machine_select: PresetSelect,
    pub bed_select: PresetSelect,
    pub process_select: PresetSelect,
    pub filament_select: PresetSelect,
    pub process_editor: Option<PresetEditor>,
    /// Рушій, на який перейдемо після «Зберегти як пресет…» з діалогу зміни рушія.
    pub pending_engine: Option<slicer::Engine>,
    /// Рушій, яким нарізати, щойно він активується («Нарізати в …» з картки невдалої нарізки).
    pub(crate) slice_after_engine_switch: Option<slicer::Engine>,
    /// Нарізка впала на всіх стадіях сумісного режиму: «Нарізати знову» не допоможе.
    pub(crate) slice_exhausted: bool,
    /// Головна дія картки в такому разі (інший рушій або менеджер рушіїв).
    pub(crate) slice_fallback: Option<crate::engines::SliceFallback>,
    /// Що робити зі змінами пресетів, коли профілі рушія довантажаться.
    pub(crate) recovery_carry: Option<crate::engines::Carry>,
    pub(crate) profile_fetch: crate::engines::ProfileFetch,
    pub filament_editor: Option<PresetEditor>,
    pub machine_editor: Option<PresetEditor>,
    pub slice_state: SliceState,
    /// Ревізія входу нарізки: зростає з кожною зміною, після якої G-code застарілий
    /// (значення редакторів, профілі, конфіг об’єктів, пластина, геометрія).
    pub slice_revision: u64,

    pub preview: Option<Preview>,
    pub cloud: CloudState,
    /// Відкрита модалка (будується заново щокадру — бачить поточний стан).
    pub modal: Option<crate::widgets::ModalBuilder>,
    /// Майстер налаштування (перший запуск або з налаштувань); малюється під модалками.
    pub wizard: Option<crate::wizard::Wizard>,
    pub toasts: Vec<crate::widgets::Toast>,
    pub next_toast: usize,

    /// Редагована модель; `plates` — її сітки для показу.
    pub project: Option<project::Project>,
    pub history: project::history::History,
    pub history_pending: Option<project::history::Entry>,
    pub selected: Option<project::InstanceRef>,
    pub selected_volume: Option<usize>,
    pub selected_instances: Vec<project::InstanceRef>,
    pub collapsed_objects: std::collections::BTreeSet<project::InstanceRef>,
    pub show_modifiers: bool,
    pub boolean_preview: bool,
    pub geometry_revision: u64,
    pub project_epoch: u64,
    /// Рамки частин і інстансів без зсуву інстанса (див. `project::bounds_cache`).
    pub volume_bounds_cache: project::bounds_cache::VolumeBoundsCache,
    pub instance_bounds_cache: project::bounds_cache::InstanceBoundsCache,
    pub boolean_mesh: Option<(u64, viewer::Mesh)>,
    /// Сітки частин, завантажені в GPU, за ідентичністю `Arc` меша. Клон `Arc` тримає
    /// адресу зайнятою, тож ключ не може перейти до іншого меша.
    pub gpu_meshes: std::collections::HashMap<u64, (std::sync::Arc<project::IndexedMesh>, glam::DVec3)>,
    /// Локальні рамки частин для Scale-гізмо (ключ — меш і трансформація частини).
    pub local_box_cache: std::cell::RefCell<std::collections::HashMap<u64, (glam::DVec3, glam::DVec3)>>,
    pub geometry_error: Option<SharedString>,
    pub cut_enabled: bool,
    pub cut_ghost: bool,
    pub cut_axis: usize,
    pub cut_position: Entity<crate::widgets::Slider>,
    /// Об’єкт під курсором у сцені (бліда рамка).
    pub hovered: Option<project::InstanceRef>,
    /// Курсор над вибраним обʼєктом (для курсора «схопити»).
    pub hover_on_selected: bool,
    /// Кнопка пластини в сцені під курсором і де курсор (для підказки).
    pub plate_hover: Option<(viewer::PlateButton, gpui::Point<gpui::Pixels>)>,
    /// Кнопка пластини, на якій натиснули ліву кнопку миші.
    pub plate_press: Option<viewer::PlateButton>,
    /// Є незбережені зміни розміщення.
    pub edited: bool,
    pub transform_inputs: Option<crate::objects::TransformInputs>,
    pub viewport_bounds: Option<gpui::Bounds<Pixels>>,
    /// Миша рушила після натискання — це обертання камери, а не клік-вибір.
    pub drag_moved: bool,
    /// Перетягування об’єкта мишею: інстанс і остання точка на площині столу.
    pub moving: Option<(project::InstanceRef, glam::Vec3)>,
    pub panning: Option<Point<Pixels>>,
    pub focus: gpui::FocusHandle,
    /// Інструмент сцени «Підготовки» та стан гізмо.
    pub tool: Option<crate::tools::Tool>,
    /// Поля й режими панелі параметрів активного інструмента.
    pub tool_panel: Option<crate::tool_panels::ToolPanel>,
    /// Стан інструмента «Різання» (площина, вибір «після різання», поля панелі).
    pub cut: Option<crate::cut_tool::CutState>,
    /// Відкритий інструмент «Вимірювання»: сітка ознак і вибір.
    pub measure: Option<crate::measure_tool::MeasureState>,
    /// Грані опуклого корпусу вибраного інстансу (кеш за ревізією геометрії) і грань під курсором.
    pub hull: Option<(project::InstanceRef, u64, Vec<project::HullPlane>)>,
    pub hull_hover: Option<usize>,
    pub gizmo_hot: Option<viewer::Handle>,
    pub gizmo_drag: Option<crate::tools::GizmoDrag>,
    /// Об’єкт під час перетягування малюється окремою GPU-сіткою з матрицею.
    pub live_drag: Option<crate::objects::LiveDrag>,
    /// Відкрите контекстне меню файлу: позиція й шлях.
    pub file_menu: Option<(gpui::Point<Pixels>, PathBuf)>,
    /// Контекстне меню 3D-сцени (правий клік без перетягування).
    pub scene_menu: Option<crate::scene_menu::SceneMenu>,
    /// Правий клік у сцені: де натиснуто і чи курсор вийшов за поріг кліку.
    pub context_press: Option<(Point<Pixels>, bool)>,
    /// Грань під курсором для «покласти на грань»: інстанс, частина, трикутник.
    pub face_hover: Option<(project::InstanceRef, usize, usize)>,

    /// Видимість лівого й правого доків (Ctrl+B / Ctrl+Alt+B, як у Zed).
    pub left_dock: bool,
    pub right_dock: bool,

    pub frame: Option<Arc<RenderImage>>,
    pub frame_size: (u32, u32),
    pub viewport_failed: bool,
    pub dirty: bool,
    /// Кешовані панелі доків і види в’юпорта (`panel_cache`).
    pub panels: crate::panel_cache::PanelCache,
    /// Фонове очікування кадру GPU (readback), див. `viewport::await_gpu_frame`.
    pub gpu_wait: Option<gpui::Task<()>>,
    pub drag_from: Option<Point<Pixels>>,
    pub _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(
        renderer: Renderer,
        library_arg: Option<PathBuf>,
        cloud: crate::device::CloudLink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let selection = Selection {
            machine: "Bambu Lab A1 0.4 nozzle".into(),
            process: "0.20mm Standard @BBL A1".into(),
            filament: "Bambu PLA Basic @BBL A1".into(),
            bed_type: BED_TYPES[0].into(),
        };
        let select = |items: Vec<SharedString>, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| Dropdown::new(items, Some(0), cx).searchable("Пошук…", window, cx))
        };
        let machine_select = select(vec![selection.machine.clone().into()], window, cx);
        let bed_select = cx.new(|cx| {
            Dropdown::new(
                BED_TYPES.iter().map(|s| SharedString::from(*s)).collect(),
                Some(0),
                cx,
            )
        });
        let process_select = select(vec![selection.process.clone().into()], window, cx);
        let filament_select = select(vec![selection.filament.clone().into()], window, cx);

        let subscriptions = vec![
            cx.on_app_quit(|this, _| {
                this.cancel_slice();
                async {}
            }),
            cx.subscribe_in(&machine_select, window, |this, _, e, window, cx| {
                this.on_preset_picked(Kind::Machine, e, window, cx)
            }),
            cx.subscribe_in(&process_select, window, |this, _, e, window, cx| {
                this.on_preset_picked(Kind::Process, e, window, cx)
            }),
            cx.subscribe_in(&filament_select, window, |this, _, e, window, cx| {
                this.on_preset_picked(Kind::Filament, e, window, cx)
            }),
            cx.subscribe_in(&bed_select, window, |this, _, e: &DropdownEvent, _, cx| {
                let DropdownEvent::Confirm(value) = e;
                this.selection.bed_type = value.to_string();
                this.preview = None;
                this.cancel_slice();
                // Панель «Друк» є й на «Перегляді»: лишаємось там, сцена — знову моделі.
                if this.page == Page::Preview {
                    this.enter_page(Page::Preview, cx);
                }
                cx.notify();
            }),
        ];

        let library_search = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx)
                .placeholder(crate::i18n::tr("Пошук моделей…"))
        });
        let mut subscriptions = subscriptions;
        subscriptions.push(cx.subscribe(
            &library_search,
            |_, _, event: &gpui_component::input::InputEvent, cx| {
                if matches!(event, gpui_component::input::InputEvent::Change) {
                    cx.notify();
                }
            },
        ));

        let cut_position = cx.new(|_| crate::widgets::Slider::single(1000, false));
        subscriptions.push(cx.subscribe(
            &cut_position,
            |this, _, _: &crate::widgets::SliderEvent, cx| {
                this.dirty = true;
                cx.notify();
            },
        ));
        let mut this = Self {
            mcp: Default::default(),
            preferences: crate::preferences::Preferences::load().unwrap_or_default(),
            jobs: Default::default(),
            page: Page::Home,
            library_search,
            renderer,
            camera: Camera::default(),
            navigation: Default::default(),
            library: Vec::new(),
            library_dir: library::default_dir(),
            library_roots: Vec::new(),
            library_session_root: None,
            library_collapsed: Default::default(),
            library_menu: None,
            library_scan: None,
            library_scanning: false,
            library_watch: None,
            path: None,
            model_name: "кубик 20 мм".into(),
            plate_bounds: Default::default(),
            plates: vec![Plate {
                number: 1,
                name: None,
                mesh: Mesh::calibration_cube(),
            }],
            active_plate: 0,
            error: None,
            engine: None,
            engine_label: "Рушій…".into(),
            engine_identity: None,
            packages: Default::default(),
            engine_loading: false,
            profile_recovery: None,
            allow_newer_file: false,
            engine_revision: 0,
            settings_schema: crate::preset_editor::schema(),
            presets: None,
            selection,
            machine_select,
            bed_select,
            process_select,
            filament_select,
            process_editor: None,
            pending_engine: None,
            slice_after_engine_switch: None,
            slice_exhausted: false,
            slice_fallback: None,
            recovery_carry: None,
            profile_fetch: Default::default(),
            filament_editor: None,
            machine_editor: None,
            slice_state: SliceState::Idle,
            slice_revision: 0,
            preview: None,
            cloud: CloudState::default(),
            modal: None,
            wizard: None,
            toasts: Vec::new(),
            next_toast: 0,
            project: None,
            history: Default::default(),
            history_pending: None,
            selected: None,
            selected_volume: None,
            selected_instances: Vec::new(),
            collapsed_objects: Default::default(),
            show_modifiers: false,
            boolean_preview: true,
            geometry_revision: 0,
            project_epoch: 0,
            volume_bounds_cache: Default::default(),
            instance_bounds_cache: Default::default(),
            boolean_mesh: None,
            window_handle: window.window_handle(),
            gpu_meshes: Default::default(),
            local_box_cache: Default::default(),
            geometry_error: None,
            cut_enabled: false,
            cut_ghost: crate::preferences::Preferences::load()
                .unwrap_or_default()
                .section_plane,
            cut_axis: 2,
            cut_position,
            hovered: None,
            hover_on_selected: false,
            plate_hover: None,
            plate_press: None,
            edited: false,
            transform_inputs: None,
            scrub: None,
            scrub_bindings: Default::default(),
            viewport_bounds: None,
            drag_moved: false,
            moving: None,
            panning: None,
            focus: cx.focus_handle(),
            left_dock: true,
            right_dock: true,
            frame: None,
            frame_size: (0, 0),
            viewport_failed: false,
            panels: Default::default(),
            gpu_wait: None,
            dirty: true,
            drag_from: None,
            tool: None,
            tool_panel: None,
            cut: None,
            measure: None,
            hull: None,
            hull_hover: None,
            gizmo_hot: None,
            gizmo_drag: None,
            live_drag: None,
            file_menu: None,
            scene_menu: None,
            context_press: None,
            face_hover: None,
            _subscriptions: subscriptions,
        };
        let (inputs, subs) = crate::objects::TransformInputs::new(window, cx);
        this.transform_inputs = Some(inputs);
        this._subscriptions.extend(subs);
        let (panel, subs) = crate::tool_panels::ToolPanel::new(window, cx);
        this.tool_panel = Some(panel);
        this._subscriptions.extend(subs);
        let (cut, subs) = crate::cut_tool::CutState::new(window, cx);
        this.cut = Some(cut);
        this._subscriptions.extend(subs);
        this.show_plate(0);
        this.init_library(library_arg, window, cx);
        this.refresh_theme(cx);
        this.watch_system_theme(window, cx);
        this.load_presets(window, cx);
        this.start_cloud(cloud, window, cx);
        this.restart_mcp(window, cx);
        window.focus(&this.focus, cx);
        if !this.preferences.wizard_done && paths::env("SLICENV_NO_WIZARD").is_none() {
            this.open_wizard(window, cx);
        }
        this
    }

    /// Перемикає вкладку й те, що малює 3D-сцена: модель на «Підготовці», траєкторії на «Перегляді».
    pub fn enter_page(&mut self, page: Page, cx: &mut Context<Self>) {
        self.finish_scene_gesture();
        self.cancel_navigation();
        self.scene_menu = None;
        self.page = page;
        self.drag_from = None;
        self.panning = None;
        self.moving = None;
        self.gizmo_drag = None;
        self.drag_moved = false;
        self.stop_playback();
        self.renderer.set_clip_plane(None);
        self.renderer.set_section_plane(None);
        self.renderer.set_cut_tint(None);
        if page == Page::Preview
            && let Some(preview) = &self.preview
        {
            self.renderer.set_mesh(&Mesh::default());
            self.renderer.set_toolpath(&preview.active_built().gpu);
            self.renderer.set_show_travel(preview.show_travel);
            self.renderer.set_show_bed(preview.show_bed);
            // «Фото» живе лише на цій сторінці: повернення відновлює його з того самого стану.
            let photo = preview
                .photo
                .as_ref()
                .filter(|p| p.on && p.built.is_some())
                .map(|p| p.settings());
            self.renderer.set_photo(photo);
        } else {
            self.renderer.set_photo(None);
            self.renderer.set_show_bed(true);
            self.renderer.clear_toolpath();
            self.renderer.set_overlay(&[]);
            self.sync_plate_scene();
        }
        // «Перегляд» до нарізки показує ту саму сцену, що й «Підготовка», але моделі —
        // напівпрозорим сірим силуетом, як у Bambu Studio.
        self.renderer
            .set_ghost_models(page == Page::Preview && self.preview.is_none());
        if page == Page::Prepare {
            self.refresh_prepare_mesh();
        }
        self.refresh_modifiers();
        self.update_highlight();
        if page == Page::Preview {
            self.apply_preview_view(cx);
        }
        self.dirty = true;
    }

    /// Клавіші сцени, коли фокус на самій сцені (а не в полі вводу).
    fn scene_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.focus.is_focused(window)
            || event.keystroke.modifiers.control
            || event.keystroke.modifiers.alt
        {
            return;
        }
        let key = event.keystroke.key.as_str();
        let handled = match self.page {
            Page::Preview => {
                self.preview_key(key, event.keystroke.modifiers.shift, cx) || self.camera_key(key)
            }
            Page::Prepare => {
                self.camera_key(key)
                    || self.tool_key(key, window, cx)
                    || (!event.keystroke.modifiers.shift && self.scene_action_key(key, window, cx))
            }
            _ => false,
        };
        if handled {
            if self.page == Page::Preview {
                self.apply_preview_view(cx);
            }
            cx.notify();
            cx.stop_propagation();
        }
    }

    // ---- модель ----

    pub(crate) fn load_thumbnails(&mut self, cx: &mut Context<Self>) {
        // Лише записи без прев’ю: після пересканування решта вже має своє.
        let paths: Vec<PathBuf> = self
            .library
            .iter()
            .filter(|e| e.thumbnail.is_none())
            .map(|e| e.path.clone())
            .collect();
        if paths.is_empty() {
            return;
        }
        // Багатоетапна задача: проміжні оновлення бібліотеки, `finish` — наприкінці.
        let job = self.jobs.begin(JobKind::Thumbnails);
        let task = cx.spawn(async move |this, cx| {
            // Етап 1: швидке читання наявних превʼю (3MF, сусідні PNG, диск-кеш)
            let (paths_back, cached) = cx
                .background_executor()
                .spawn(async move {
                    let cached = paths
                        .iter()
                        .map(|p| library::load_thumbnail_fast(p))
                        .collect::<Vec<_>>();
                    (paths, cached)
                })
                .await;

            let mut missing = Vec::new();
            this.update(cx, |this, cx| {
                // Бібліотеку могли пересканувати, поки читали диск: зіставляємо за шляхом.
                for (path, thumb) in paths_back.into_iter().zip(cached) {
                    let Some(i) = this.library.iter().position(|e| e.path == path) else {
                        continue;
                    };
                    if thumb.is_some() {
                        this.library[i].thumbnail = thumb;
                    } else {
                        missing.push((i, path));
                    }
                }
                cx.notify();
            })
            .ok();

            if !missing.is_empty() {
                // Етап 2: фонова генерація відсутніх превʼю з прогресивним оновленням
                for (index, path) in missing {
                    let load_path = path.clone();
                    let thumb = cx
                        .background_executor()
                        .spawn(async move { library::load_thumbnail(&load_path) })
                        .await;
                    if thumb.is_some() {
                        let _ = this.update(cx, |this, cx| {
                            let found = match this.library.get(index) {
                                Some(e) if e.path == path => Some(index),
                                _ => this.library.iter().position(|e| e.path == path),
                            };
                            if let Some(entry) = found.map(|i| &mut this.library[i]) {
                                entry.thumbnail = thumb;
                                cx.notify();
                            }
                        });
                    }
                }
            }
            let _ = this.update(cx, |this, _| this.jobs.finish(job));
        });
        self.jobs.attach(job, task);
    }

    pub fn open(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_slice();
        // Готовий G-code — одразу «Перегляд» (GUI-бенч Preview, файли з інших слайсерів).
        // Проєкт не замінюється, тож і питати про незбережені зміни не треба: confirm_discard
        // скинув би `edited`, а зміни лишились би в пам’яті без захисту.
        if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("gcode")) {
            self.load_preview(path, window, cx);
            return;
        }
        let retry = path.clone();
        if !crate::objects::confirm_discard(self, window, cx, move |ws, window, cx| {
            ws.open(retry.clone(), window, cx)
        }) {
            return;
        }
        let load_path = path.clone();
        let subject = path.clone();
        // Нове відкриття замінює попереднє; документ воно й так замінює повністю.
        let job = self.spawn_job(
            JobKind::Open,
            JobTarget::any(),
            move || {
                // Редагований проєкт; якщо його формат ще не підтримується — лише перегляд.
                match crate::objects::load_project(&load_path) {
                    Ok((project, plates)) => Ok((Some(project), plates)),
                    Err(err) => {
                        eprintln!(
                            "проєкт {}: {err:#}; відкриваю лише для перегляду",
                            load_path.display()
                        );
                        load_model(&load_path).map(|plates| (None, plates))
                    }
                }
            },
            move |this, result, window, cx| match result {
                Ok((project, plates)) => this.install_with_open_mode(project, plates, path, window, cx),
                Err(err) => this.error = Some(format!("{err:#}").into()),
            },
            cx,
        );
        if let Some(job) = job {
            let name = subject
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            self.jobs
                .set_label(job, crate::tr_format!("Відкриваю {name}…"));
            self.jobs.set_subject(job, subject);
        }
    }

    /// Common completion path for UI file loading and MCP open_project.
    pub(crate) fn install_open_project(
        &mut self,
        project: Option<project::Project>,
        plates: Vec<Plate>,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        self.finish_scene_gesture();
        self.history.clear();
        self.history_pending = None;
        self.cancel_slice();
        // MCP open_project замінює й незавершене відкриття з UI.
        self.jobs.cancel(JobKind::Open);
        self.project_epoch = self.project_epoch.wrapping_add(1);
        self.project = project;
        self.rebuild_instance_bounds();
        self.sync_model_color();
        self.selected = None;
        self.selected_volume = None;
        self.selected_instances.clear();
        self.collapsed_objects.clear();
        self.cut_enabled = false;
        self.edited = false;
        self.renderer.set_highlight(None);
        self.model_name = path
            .file_name()
            .map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            )
            .into();
        self.path = Some(path);
        self.plates = plates;
        self.invalidate_plate_bounds();
        self.error = None;
        self.active_plate = usize::MAX;
        self.show_plate(0);
        self.preview = None;
        self.enter_page(Page::Prepare, cx);
        self.machine_editor = None;
        self.process_editor = None;
        self.filament_editor = None;
        self.rebuild_editors();
        self.apply_project_presets(cx);
        self.edited = false;
        // MCP open_project повертається одразу: без цього вікно лишається зі старим кадром.
        cx.notify();
    }

    pub fn show_plate(&mut self, index: usize) {
        if self.active_plate != index {
            self.finish_scene_gesture();
        }
        if index >= self.plates.len() {
            return;
        }
        if self.active_plate != index {
            self.invalidate_geometry();
            self.preview = None;
            self.cancel_slice();
        }
        let initial = self.active_plate >= self.plates.len();
        self.active_plate = index;
        self.sync_plate_scene();
        if initial {
            self.fit_all_plates();
        }
        self.refresh_modifiers();
        self.update_highlight();
        self.dirty = true;
    }

    pub fn prompt_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = crate::file_menu::prompt_open(
            window,
            crate::file_menu::OpenKind::Project,
            "Відкрити модель",
            false,
        );
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = paths.await.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| this.open(path, window, cx))
                .ok();
        })
        .detach();
    }

    // ---- пресети ----

    fn load_presets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match crate::engines::startup_engine() {
            // Перший запуск без рушія: це пояснить майстер, тост англійською тут зайвий.
            Err(_) if !self.preferences.wizard_done => {}
            engine => self.activate_engine(engine, window, cx),
        }
    }

    /// Для 3MF беремо профілі, з якими проєкт збережений (якщо вони підходять до принтера).
    pub(crate) fn apply_project_presets(&mut self, cx: &mut Context<Self>) {
        let edited = self.edited;
        let Some(path) = &self.path else { return };
        if let Some(project) = slicer::project_selection(path) {
            let machine = self.selection.machine.clone();
            self.set_selection(
                Selection {
                    machine,
                    ..project.clone()
                },
                cx,
            );
            if let Some(config) = self.project.as_ref().map(|p| p.config.clone()) {
                // Значення з 3MF — лише в той профіль, для якого проєкт збережено. Інакше
                // проєкт для A1 mini 0.2 переписав би поточному «A1 0.4 nozzle» сопло й стіл,
                // а нарізка й Support Fins узяли б 0.2 під назвою 0.4.
                for (kind, editor, saved_for) in [
                    (Kind::Machine, &mut self.machine_editor, &project.machine),
                    (Kind::Process, &mut self.process_editor, &project.process),
                    (Kind::Filament, &mut self.filament_editor, &project.filament),
                ] {
                    let Some(editor) = editor.as_mut() else { continue };
                    if editor.preset.parent() != saved_for.as_str() {
                        continue;
                    }
                    // Проєкти, збережені до виправлення розгортання `inherits`, несуть значення
                    // від батька чужого вендора (Anker: 3 стінки, cubic) — це не правки.
                    let stale = self
                        .presets
                        .as_ref()
                        .map(|store| store.stale_project_values(kind, saved_for, &config))
                        .unwrap_or_default();
                    let keys: Vec<_> = editor.preset.base().keys().cloned().collect();
                    for key in keys {
                        if let Some(value) = config.get(&key)
                            && stale.get(&key) != Some(value)
                        {
                            let _ = editor.preset.set(&key, value.clone());
                        }
                    }
                    editor.clear_controls();
                }
            }
        }
        self.edited = edited;
    }

    /// Нормалізує вибір і синхронізує з ним усі списки.
    pub fn set_selection(&mut self, selection: Selection, cx: &mut Context<Self>) {
        if selection != self.selection && self.project.is_some() {
            self.edited = true;
        }
        let Some(store) = self.presets.clone() else {
            return;
        };
        self.selection = store.sanitize(selection);

        let names = |kind: Kind, machine: &str| -> Vec<SharedString> {
            store
                .compatible(kind, machine)
                .into_iter()
                .map(|p| SharedString::from(p.name.clone()))
                .collect()
        };
        let machine = self.selection.machine.clone();
        let lists = [
            (
                &self.machine_select,
                names(Kind::Machine, &machine),
                self.selection.machine.clone(),
            ),
            (
                &self.process_select,
                names(Kind::Process, &machine),
                self.selection.process.clone(),
            ),
            (
                &self.filament_select,
                names(Kind::Filament, &machine),
                self.selection.filament.clone(),
            ),
            (
                &self.bed_select,
                {
                    let mut beds: Vec<SharedString> =
                        BED_TYPES.iter().map(|s| SharedString::from(*s)).collect();
                    if !BED_TYPES.contains(&self.selection.bed_type.as_str()) {
                        beds.push(self.selection.bed_type.clone().into());
                    }
                    beds
                },
                self.selection.bed_type.clone(),
            ),
        ];
        for (select, items, current) in lists {
            let index = items
                .iter()
                .position(|n| n.as_ref() == current)
                .unwrap_or(0);
            select.update(cx, |state, cx| {
                state.set_items(items, cx);
                state.set_selected_index(Some(index), cx);
            });
        }
        self.rebuild_editors();
        let (lo, hi) = self.bed_bounds();
        self.renderer
            .set_bed_bounds(lo.map(|v| v as f32), hi.map(|v| v as f32));
        self.preview = None;
        if self.page == Page::Preview {
            self.enter_page(Page::Preview, cx);
        }
        self.sync_plate_scene();
        self.dirty = true;
        self.cancel_slice();
        cx.notify();
    }

    fn on_preset_picked(
        &mut self,
        kind: Kind,
        event: &DropdownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let DropdownEvent::Confirm(value) = event;
        let mut selection = self.selection.clone();
        match kind {
            Kind::Machine => {
                selection.machine = value.to_string();
                // Вибір принтера переживає перезапуск і для керованих рушіїв (без BambuStudio.conf).
                self.preferences.printer = value.to_string();
                self.save_preferences(cx);
                if let Some(defaults) = self
                    .presets
                    .as_ref()
                    .and_then(|s| s.defaults_for_machine(value))
                {
                    selection = defaults;
                }
            }
            Kind::Process => selection.process = value.to_string(),
            Kind::Filament => selection.filament = value.to_string(),
        }
        self.set_selection(selection, cx);
    }

    // ---- нарізка ----

    pub fn plate_to_slice(&self) -> Option<u32> {
        self.path.as_ref()?;
        let number = self.plates.get(self.active_plate)?.number;
        (number > 0).then_some(number)
    }

    pub fn start_slice(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let _span = paths::profile::Span::new("slice::ui_snapshot");
        let (Some(store), Some(plate)) = (self.presets.clone(), self.plate_to_slice()) else {
            return;
        };
        let Some(engine) = self.engine.clone().filter(|_| !self.engine_loading) else {
            return;
        };
        self.cancel_slice();
        self.slice_exhausted = false;
        self.slice_fallback = None;
        // Знімок після скасування попередньої: ревізії документа, рушія й входу нарізки.
        let target = self.job_target().geometry().settings().active_plate();
        let mut snapshot = crate::objects::SliceInput {
            // Стіл — поточного принтера: від нього Bambu рахує крок пластин у файлі.
            project: self.project.clone().map(|p| self.project_for_write(p)),
            path: self.path.clone(),
            settings_schema: self.settings_schema,
        };
        let overrides = slicer::Overrides {
            machine: self
                .machine_editor
                .as_ref()
                .map(|e| e.overrides())
                .unwrap_or_default(),
            process: self
                .process_editor
                .as_ref()
                .map(|e| e.overrides())
                .unwrap_or_default(),
            filament: self
                .filament_editor
                .as_ref()
                .map(|e| e.overrides())
                .unwrap_or_default(),
        };
        let cross_product =
            engine.kind == slicer::EngineKind::Orca && self.project_from_bambu_studio();
        let mut request = SliceRequest {
            allow_newer_file: self.allow_newer_file || cross_product,
            input: PathBuf::new(),
            plate,
            selection: self.selection.clone(),
            overrides,
        };
        let cancellation = slicer::SliceCancellation::default();
        let token = cancellation.clone();
        let job = self.spawn_job(
            JobKind::Slice,
            target,
            move || {
                let _span = paths::profile::Span::new("slice::background");
                cancellation.check()?;
                let input_dir = tempfile::tempdir()?;
                let (path, substitutions) = {
                    let _s = paths::profile::Span::new("slice::prepare_input");
                    snapshot.keep_only_plate(plate);
                    snapshot.prepare(input_dir.path())?
                };
                cancellation.check()?;
                request.input = path;
                let mut output = {
                    let _s = paths::profile::Span::new("slice::engine");
                    slicer::slice_with_engine_cancellable(&engine, &store, &request, &cancellation)?
                };
                output.warnings.extend(substitutions);
                cancellation.check()?;
                // Сумісний режим: архів зібрав Slicenv без мініатюр рушія. Принтер і Bambu
                // Connect показують plate_N.png, тож малюємо їх своїм рендерером.
                let _thumbs = paths::profile::Span::new("slice::thumbnails");
                if slicer::is_slicenv_package(&output.gcode_3mf)
                    && let Err(e) = attach_thumbnails(&request.input, &output.gcode_3mf, plate, &request.selection.bed_type)
                {
                    eprintln!("[slice] мініатюри для пакета: {e:#}");
                }
                Ok::<_, anyhow::Error>(output)
            },
            move |this, result, window, cx| match result {
                Ok(out) => {
                    if let Some(note) = out.compatibility_note {
                        this.toast(ToastKind::Info, crate::i18n::tr(note), cx);
                    }
                    if !out.warnings.is_empty() {
                        this.toast(
                            ToastKind::Warning,
                            crate::tr_format!("Попередження рушія: {}", out.warnings.join("; ")),
                            cx,
                        );
                    }
                    let minutes = out.print_time.as_secs() / 60;
                    this.toast(
                        ToastKind::Success,
                        crate::tr_format!(
                            "Пластина {plate} нарізана: {} год {:02} хв, {:.1} г",
                            minutes / 60,
                            minutes % 60,
                            out.filament_grams
                        ),
                        cx,
                    );
                    let gcode = out.gcode_3mf.with_file_name(format!("plate_{plate}.gcode"));
                    this.slice_state = SliceState::Done(out);
                    this.load_preview(gcode, window, cx);
                }
                Err(err) if err.is::<slicer::Cancelled>() => this.slice_state = SliceState::Idle,
                Err(err) => {
                    let failure = err.downcast_ref::<slicer::SliceFailure>().cloned().map(std::sync::Arc::new);
                    // Аварія на всіх стадіях (зараз або вже запам’ятована для цього рушія):
                    // головна дія — інший рушій, а не повтор тим самим.
                    this.slice_exhausted = failure.as_ref().is_some_and(|f| {
                        f.safe_mode_exhausted
                            || (f.crashed
                                && this
                                    .engine_identity
                                    .as_deref()
                                    .is_some_and(|id| slicer::engine_compat(id).crashes_in_safe_mode))
                    });
                    this.slice_fallback = if this.slice_exhausted { this.crash_fallback() } else { None };
                    this.slice_state = SliceState::Failed(format!("{err:#}").into(), failure);
                }
            },
            cx,
        );
        if let Some(job) = job {
            self.jobs
                .set_label(job, crate::tr_format!("Нарізаю пластину {}…", plate));
            let progress = token.clone();
            self.jobs
                .set_progress(job, Arc::new(move || progress.progress()));
            self.jobs.set_stage(job, crate::preview::slice_stage(token.clone(), self.engine_identity.clone()));
            // Скасування вбиває процес рушія й чекає на нього (kill + wait).
            self.jobs.on_cancel(job, move || token.cancel());
        }
    }

    /// Текст рядка стану для запущених задач (нарізка, орієнтація, плавці…).
    fn jobs_activity(&self) -> Option<String> {
        let shown: Vec<String> = self.jobs.status().filter_map(|s| s.text()).collect();
        (!shown.is_empty()).then(|| shown.join(" · "))
    }

    /// Іде нарізка з UI.
    pub fn slicing(&self) -> bool {
        self.jobs.is_running(JobKind::Slice)
    }

    /// Усе, що робить G-code застарілим: скасовує нарізку (GUI і MCP) та завантаження
    /// прев’ю й підвищує ревізію входу нарізки, тож запізнілі результати відкидаються.
    pub(crate) fn cancel_slice(&mut self) {
        self.slice_revision = self.slice_revision.wrapping_add(1);
        for kind in [JobKind::Slice, JobKind::McpSlice, JobKind::GcodePreview] {
            self.jobs.cancel(kind);
        }
        // Рушій спільний для GUI і MCP-сесій поза цим вікном.
        slicer::cancel_active_slice();
        self.slice_state = SliceState::Idle;
    }

    // ---- оболонка ----

    pub fn bed_bounds(&self) -> ([f64; 2], [f64; 2]) {
        self.presets
            .as_ref()
            .and_then(|s| s.bed_bounds(&self.selection.machine))
            .unwrap_or(([0.0, 0.0], [project::BED_SIZE, project::BED_SIZE]))
    }

    pub fn can_slice(&self) -> bool {
        self.plate_to_slice().is_some()
            && !self.selection.process.is_empty()
            && !self.selection.filament.is_empty()
            && self.engine.is_some()
            && !self.engine_loading
            && self.presets.is_some()
            && !self.slicing()
    }

    /// Заголовок як у Zed: проєкт, навігація між сторінками, стан принтера й головні дії.
    fn title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let z = zed();
        let running = self.slicing();
        let slice_label = match (running, self.plate_to_slice()) {
            (true, _) => crate::i18n::tr("Скасувати").to_string(),
            (_, Some(n)) if self.plates.len() > 1 => crate::tr_format!("Нарізати пластину {n}"),
            _ => "Нарізати".to_string(),
        };
        let (printer, printer_color, live) = self.cloud.summary();
        let can_print = self.can_send_print();
        let print_primary = can_print && !running;
        let has_project = self.project.is_some();

        let separator = || {
            div()
                .flex_none()
                .w(px(1.))
                .h(px(16.))
                .mx(px(4.))
                .bg(z.border)
        };

        TitleBar::new().child(
            div()
                .flex()
                .flex_1()
                .items_center()
                .gap(px(2.))
                .h_full()
                .pr(px(6.))
                .child(
                    ui::icon_button("home-page", Page::Home.icon(), Page::Home.title())
                        .no_window_drag()
                        .toggled(self.page == Page::Home)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.enter_page(Page::Home, cx);
                            cx.notify();
                        })),
                )
                .child(separator())
                .child(
                    // Довга назва обрізається трикрапкою; позначка змін «●» лишається видимою.
                    if self.path.is_some() {
                        ui::ghost_button_bare("project-name", IconName::FileBox)
                            .child(div().min_w_0().truncate().child(self.model_name.clone()))
                            .when(self.edited, |d| d.child(div().flex_none().child("●")))
                    } else {
                        ui::ghost_button("project-name", Some(IconName::FileBox), "Відкрити модель…")
                    }
                    .no_window_drag()
                    .when_some(self.path.clone(), |d, path| {
                        use crate::file_menu::FileMenu as _;
                        d.file_menu(path, cx)
                    })
                    .max_w(px(220.))
                    .overflow_hidden()
                    .text_color(z.text_muted)
                    .tooltip({
                        let tip: SharedString = match &self.path {
                            Some(path) => crate::tr_format!(
                                "{} — клацни, щоб перейти до «Підготовки»; правий клік — дії з файлом",
                                path.display()
                            ),
                            None => crate::i18n::tr("Відкрити 3MF, STL чи G-code (Ctrl+O)"),
                        }
                        .into();
                        move |window, cx| Tooltip::new(tip.clone()).build(window, cx)
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        if this.path.is_some() {
                            this.enter_page(Page::Prepare, cx);
                            cx.notify();
                        } else {
                            this.prompt_open(window, cx);
                        }
                    })),
                )
                .children(
                    Page::PROJECT_TABS
                        .iter()
                        .enumerate()
                        .filter(|_| self.path.is_some())
                        .map(|(i, page)| {
                            let page = *page;
                            ui::ghost_button(("page", i), Some(page.icon()), page.title())
                                .no_window_drag()
                                .text_color(z.text_muted)
                                .toggled(self.page == page)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.enter_page(page, cx);
                                    cx.notify();
                                }))
                        }),
                )
                .child(separator())
                .child(
                    ui::icon_button(
                        "open-preferences",
                        IconName::Settings,
                        "Налаштування (Ctrl+,)",
                    )
                    .no_window_drag()
                    .on_click(cx.listener(|this, _, window, cx| this.open_preferences(window, cx))),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .id("printer-pill")
                        .no_window_drag()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(6.))
                        .h(px(24.))
                        .px(px(8.))
                        .rounded(px(4.))
                        .cursor_pointer()
                        .text_sm()
                        .text_color(z.text_muted)
                        .hover(|s| s.bg(z.element_hover).text_color(z.text))
                        .child(ui::icon(IconName::Printer))
                        .child(self.cloud.indicator("titlebar-cloud", printer_color, live))
                        .child(printer)
                        .tooltip({
                            let machine = self.selection.machine.clone();
                            move |window, cx| {
                                Tooltip::new(crate::tr_format!("Пристрої · профіль: {machine}"))
                                    .build(window, cx)
                            }
                        })
                        .when(self.page == Page::Device, |d| d.bg(z.element_selected))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.enter_page(Page::Device, cx);
                            cx.notify();
                        })),
                )
                .child(separator())
                .child(
                    ui::icon_button("save-project", IconName::Save, "Зберегти проєкт (Ctrl+S)")
                        .no_window_drag()
                        .inert(!has_project)
                        .when(has_project, |d| {
                            d.on_click(
                                cx.listener(|this, _, window, cx| this.prompt_save(window, cx)),
                            )
                        }),
                )
                .child(
                    // Виділена кнопка — наступний крок: до нарізки «Нарізати», після — «Друкувати».
                    if print_primary {
                        ui::ghost_button("slice", Some(IconName::Scissors), slice_label)
                    } else {
                        ui::accent_button(
                            "slice",
                            IconName::Scissors,
                            slice_label,
                            running || self.can_slice(),
                        )
                    }
                    .no_window_drag()
                    .tooltip({
                        let tip: SharedString = if running {
                            crate::i18n::tr("Зупинити нарізку")
                        } else {
                            crate::tr_format!(
                                "Нарізати активну пластину рушієм нарізки {} (Ctrl+R)",
                                self.engine_label
                            )
                        }
                        .into();
                        move |window, cx| Tooltip::new(tip.clone()).build(window, cx)
                    })
                    .when(running, |d| d.child(ui::spinner("slice-btn-spin")))
                    .when(!running, |d| d.child(ui::kbd("Ctrl+R")))
                    .when(running || self.can_slice(), |d| {
                        d.on_click(cx.listener(|this, _, window, cx| {
                            if this.slicing() {
                                this.cancel_slice();
                                cx.notify();
                            } else {
                                this.start_slice(window, cx);
                            }
                        }))
                    }),
                )
                .child(
                    {
                        let label = if self.cloud.sending {
                            "Надсилаю…"
                        } else {
                            "Друкувати"
                        };
                        if print_primary {
                            ui::accent_button("send-print", IconName::Send, label, true)
                        } else {
                            ui::ghost_button("send-print", Some(IconName::Send), label)
                        }
                    }
                    .no_window_drag()
                    .inert(!can_print)
                    .when(can_print, |d| {
                        d.on_click(
                            cx.listener(|this, _, window, cx| this.open_print_dialog(window, cx)),
                        )
                    })
                    .when_some(self.print_blocker(), |d, why| {
                        d.tooltip(move |window, cx| Tooltip::new(why.clone()).build(window, cx))
                    }),
                ),
        )
    }

    /// Рядок стану Zed: перемикачі доків зліва, поточна дія, справа — сцена й хмара.
    fn status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let z = zed();
        let has_left = matches!(self.page, Page::Home | Page::Prepare | Page::Device)
            || (self.page == Page::Preview && self.preview.is_some());
        let has_right = matches!(self.page, Page::Prepare | Page::Preview);
        let (activity, color): (Option<gpui::AnyElement>, gpui::Hsla) =
            // Усі фонові задачі показуються однаково: спінер і текст із реєстру.
            if let Some(text) = self.jobs_activity() {
                (
                    Some(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(ui::spinner("job-spin"))
                            .child(div().truncate().child(text))
                            .into_any_element(),
                    ),
                    z.text_muted,
                )
            } else if let Some(err) = &self.error {
                (
                    Some(div().truncate().child(err.clone()).into_any_element()),
                    z.error,
                )
            } else {
                match &self.slice_state {
                    // Лише причина: решта (хто виправляє, лог) — у Preview.
                    SliceState::Failed(err, failure) => (
                        Some(
                            div()
                                .truncate()
                                .child(match failure {
                                    Some(f) => crate::tr_format!(
                                        "Не нарізано: {}",
                                        crate::i18n::tr(f.title)
                                    ),
                                    None => err.lines().next().unwrap_or_default().to_owned(),
                                })
                                .into_any_element(),
                        ),
                        z.error,
                    ),
                    SliceState::Done(done) => {
                        let minutes = done.print_time.as_secs() / 60;
                        (
                            Some(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .child(ui::icon(IconName::CircleCheck).text_color(z.success))
                                    .child(crate::tr_format!(
                                        "Нарізано: {} год {:02} хв · {:.1} г",
                                        minutes / 60,
                                        minutes % 60,
                                        done.filament_grams
                                    ))
                                    .into_any_element(),
                            ),
                            z.text_muted,
                        )
                    }
                    SliceState::Idle => (None, z.text_muted),
                }
            };
        let triangles = self.plate_triangle_count(self.active_plate);
        let scene = matches!(self.page, Page::Prepare | Page::Preview);
        let geometry = if self.page == Page::Preview {
            crate::tr_format!(
                "{} сегментів",
                crate::group_digits(
                    self.preview
                        .as_ref()
                        .map_or(0, |p| p.toolpath.segments.len())
                )
            )
        } else {
            crate::tr_format!("{} трикутників", crate::group_digits(triangles))
        };
        let (cloud_text, cloud_color, _) = self.cloud.summary();

        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.))
            .h(px(28.))
            .px(px(6.))
            .bg(z.background)
            .border_t_1()
            .border_color(z.border_variant)
            .text_xs()
            .text_color(z.text_muted)
            .when(has_left, |d| {
                d.child(
                    ui::icon_button("toggle-left", IconName::PanelLeft, "Лівий док (Ctrl+B)")
                        .toggled(self.left_dock)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.left_dock = !this.left_dock;
                            cx.notify();
                        })),
                )
            })
            .child(
                div()
                    .min_w_0()
                    .px(px(6.))
                    .text_color(color)
                    .children(activity),
            )
            .child(div().flex_1())
            .when(scene, |d| {
                d.child(div().px(px(6.)).child(geometry))
                    .child(div().px(px(6.)).child(crate::tr_format!(
                        "{:.1} мс",
                        self.renderer.last_frame_time.as_secs_f64() * 1000.0
                    )))
                    .child(
                        div()
                            .px(px(6.))
                            .text_color(z.text_placeholder)
                            .child(self.renderer.adapter_summary()),
                    )
            })
            .child(
                div()
                    .id("status-cloud")
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(22.))
                    .px(px(6.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|s| s.bg(z.element_hover).text_color(z.text))
                    .child(
                        ui::icon(if self.cloud.error.is_some() {
                            IconName::CloudOff
                        } else {
                            IconName::Cloud
                        })
                        .text_color(cloud_color),
                    )
                    .child(cloud_text)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.enter_page(Page::Device, cx);
                        cx.notify();
                    })),
            )
            .when(has_right, |d| {
                d.child(
                    ui::icon_button(
                        "toggle-right",
                        IconName::PanelRight,
                        "Правий док (Ctrl+Alt+B)",
                    )
                    .toggled(self.right_dock)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.right_dock = !this.right_dock;
                        cx.notify();
                    })),
                )
            })
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _profile = paths::profile::Span::new("Workspace::render");
        let z = zed();
        let content = match self.page {
            Page::Home => crate::home::render(self, window, cx).into_any_element(),
            Page::Prepare => crate::prepare::render(self, window, cx).into_any_element(),
            Page::Preview => crate::preview::render(self, cx).into_any_element(),
            Page::Device => crate::device::render(self, window, cx).into_any_element(),
            Page::Project => self.project_details(cx),
        };

        let overlays = self.overlays(window, cx);
        // Рамку вікна (тінь, смуга зміни розміру) малює `Root` gpui-component 0.8 — один раз.
        // Друга рамка тут із іншою тінню (0 замість 20 px) щокадру перемикала
        // `set_client_inset` 20 ↔ 0: на X11 це `_GTK_FRAME_EXTENTS`, mutter двічі
        // переконфігуровував вікно, а GPUI на кожен ConfigureNotify робив `refresh()` і малював
        // усе вікно наново без кешу — 1,5–12 с перемальовок на ≈50 % ядра після будь-якого кадру.
        div().size_full().child(
            div()
                .child(crate::scrub::capture(cx))
                .when(self.scrub.is_some(), |d| d.cursor_ew_resize())
                .capture_any_mouse_up(cx.listener(
                    |this, event: &gpui::MouseUpEvent, window, cx| {
                        if event.button == gpui::MouseButton::Left {
                            this.scrub_release(event, window, cx);
                        }
                    },
                ))
                .capture_any_mouse_down(cx.listener(
                    |this, event: &gpui::MouseDownEvent, window, cx| {
                        if event.button == gpui::MouseButton::Right && this.scrub.is_some() {
                            this.scrub_end(true, window, cx);
                        }
                    },
                ))
                .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        if this.slicing() {
                            this.cancel_slice();
                            cx.notify();
                            cx.stop_propagation();
                        } else if this.scrub.is_some() {
                            this.scrub_end(true, window, cx);
                        }
                    }
                }))
                .track_focus(&self.focus)
                .key_context("Workspace")
                .on_key_down(cx.listener(Self::scene_key))
                .relative()
                .on_action(cx.listener(|_, _: &crate::QuitApplication, _, cx| cx.quit()))
                .on_action(cx.listener(|this, _: &crate::OpenPreferences, window, cx| {
                    this.open_preferences(window, cx)
                }))
                .on_action(cx.listener(|this, _: &crate::GoHome, _, cx| {
                    this.enter_page(Page::Home, cx);
                    cx.notify();
                }))
                .on_action(cx.listener(|this, _: &crate::GoPrepare, _, cx| {
                    if this.path.is_some() {
                        this.enter_page(Page::Prepare, cx);
                        cx.notify();
                    }
                }))
                .on_action(cx.listener(|this, _: &crate::GoPreview, _, cx| {
                    if this.path.is_some() {
                        this.enter_page(Page::Preview, cx);
                        cx.notify();
                    }
                }))
                .on_action(cx.listener(|this, _: &crate::GoProject, _, cx| {
                    if this.path.is_some() {
                        this.enter_page(Page::Project, cx);
                        cx.notify();
                    }
                }))
                .on_action(cx.listener(|this, _: &crate::GoDevices, _, cx| {
                    this.enter_page(Page::Device, cx);
                    cx.notify();
                }))
                .on_action(cx.listener(|this, _: &crate::RefreshLibrary, _, cx| {
                    if this.page == Page::Home {
                        this.rescan_library(cx);
                    }
                }))
                .on_action(cx.listener(|this, _: &crate::SaveProject, window, cx| {
                    this.prompt_save(window, cx)
                }))
                .on_action(cx.listener(|this, _: &crate::CloseFile, window, cx| {
                    this.close_file(window, cx)
                }))
                .on_action(
                    cx.listener(|this, _: &crate::OpenFile, window, cx| {
                        this.prompt_open(window, cx)
                    }),
                )
                .on_action(cx.listener(|this, _: &crate::UndoProject, window, cx| {
                    if this.slicing() {
                        this.cancel_slice();
                        cx.notify();
                    } else if this.scrub.is_some() {
                        this.scrub_end(true, window, cx);
                    }
                    this.restore_history(false, window, cx);
                }))
                .on_action(cx.listener(|this, _: &crate::RedoProject, window, cx| {
                    if this.scrub.is_some() {
                        this.scrub_end(true, window, cx);
                    }
                    this.restore_history(true, window, cx);
                }))
                .on_action(cx.listener(|this, _: &crate::DeleteSelected, window, cx| {
                    if this.page == Page::Prepare {
                        this.delete_selected(window, cx)
                    }
                }))
                .on_action(
                    cx.listener(|this, _: &crate::DuplicateSelected, window, cx| {
                        if this.page == Page::Prepare {
                            this.duplicate_selected(window, cx)
                        }
                    }),
                )
                .on_action(cx.listener(|this, _: &crate::SelectAll, window, cx| {
                    if this.page == Page::Prepare {
                        this.select_all(false, window, cx)
                    }
                }))
                .on_action(cx.listener(|this, _: &crate::DeleteAll, window, cx| {
                    if this.page == Page::Prepare {
                        this.confirm_delete_all(window, cx)
                    }
                }))
                .on_action(cx.listener(|this, _: &crate::MergeAssembly, window, cx| {
                    if this.page == Page::Prepare && this.selected_instances.len() >= 2 {
                        this.merge_selection(window, cx);
                    }
                }))
                .on_action(cx.listener(|this, _: &crate::SplitAssembly, window, cx| {
                    if this.page == Page::Prepare {
                        this.split_selection(window, cx);
                    }
                }))
                .on_action(cx.listener(|this, _: &crate::AutoOrient, _, cx| {
                    this.orient_selection(cx);
                }))
                .on_action(cx.listener(|this, _: &crate::SliceNow, window, cx| {
                    if this.can_slice() {
                        this.start_slice(window, cx)
                    }
                }))
                .on_action(cx.listener(|this, _: &crate::CloseModal, window, cx| {
                    if this.scrub.is_some() {
                        this.scrub_end(true, window, cx);
                    } else if this.modal.is_some() {
                        this.close_modal(cx)
                    } else if this.wizard.is_some() {
                        this.close_wizard(window, cx)
                    } else if this.scene_menu.is_some() {
                        this.close_scene_menu(cx)
                    } else if !this.escape_scene(window, cx) {
                        cx.propagate()
                    }
                }))
                .on_action(cx.listener(|this, _: &crate::WizardNext, window, cx| {
                    if this.modal.is_none() {
                        this.wizard_next(window, cx)
                    }
                }))
                .on_action(cx.listener(|this, _: &crate::WizardBack, window, cx| {
                    if this.modal.is_none() {
                        this.wizard_back(window, cx)
                    }
                }))
                .on_action(cx.listener(|this, _: &crate::ToggleLeftDock, _, cx| {
                    this.left_dock = !this.left_dock;
                    cx.notify();
                }))
                .on_action(cx.listener(|this, _: &crate::ToggleRightDock, _, cx| {
                    this.right_dock = !this.right_dock;
                    cx.notify();
                }))
                .size_full()
                .flex()
                .flex_col()
                .bg(z.background)
                .text_color(z.text)
                .text_sm()
                .font_family(crate::theme::UI_FONT)
                .child(self.title_bar(cx))
                .child(div().flex().flex_1().min_h_0().child(content))
                .child(self.status_bar(cx))
                .children(overlays)
                .when(self.scrub.is_some(), |d| {
                    d.child(div().absolute().inset_0().cursor_ew_resize())
                }),
        )
    }
}

impl Workspace {
    fn project_details(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let z = zed();
        let value = |text: String| div().min_w_0().whitespace_normal().text_sm().child(text);
        let mut rows = vec![
            ui::prop_row(
                "Файл",
                value(
                    self.path
                        .as_ref()
                        .map_or_else(|| "—".into(), |p| crate::shorten_home(p)),
                ),
            )
            .into_any_element(),
            ui::prop_row("Принтер", value(self.selection.machine.clone())).into_any_element(),
            ui::prop_row("Процес", value(self.selection.process.clone())).into_any_element(),
            ui::prop_row("Філамент", value(self.selection.filament.clone())).into_any_element(),
            ui::prop_row("Пластин", value(self.plates.len().to_string())).into_any_element(),
        ];
        if let Some(project) = &self.project {
            rows.push(
                ui::prop_row("Об’єктів", value(project.objects.len().to_string()))
                    .into_any_element(),
            );
            for (key, label) in [
                ("Title", "Назва"),
                ("Designer", "Автор"),
                ("License", "Ліцензія"),
                ("CreationDate", "Створено"),
                ("ModificationDate", "Змінено"),
                ("Description", "Опис"),
                ("ProfileTitle", "Профіль моделі"),
                ("ProfileDescription", "Опис профілю"),
            ] {
                if let Some(text) = project.metadata.get(key) {
                    // Imported descriptions may contain escaped HTML. Display plain text.
                    let decoded = text
                        .replace("&lt;", "<")
                        .replace("&gt;", ">")
                        .replace("&amp;", "&")
                        .replace("&quot;", "\"")
                        .replace("&apos;", "'")
                        .replace("&#39;", "'")
                        .replace("&nbsp;", " ");
                    let mut plain = String::new();
                    let mut in_tag = false;
                    for ch in decoded.chars() {
                        match ch {
                            '<' => {
                                in_tag = true;
                                plain.push(' ');
                            }
                            '>' => in_tag = false,
                            _ if !in_tag => plain.push(ch),
                            _ => {}
                        }
                    }
                    rows.push(
                        ui::prop_row(label, value(plain))
                            .flex_none()
                            .py(px(4.))
                            .items_start()
                            .into_any_element(),
                    );
                }
            }
        }
        div()
            .id("project-details")
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .bg(z.editor)
            .p(px(20.))
            .child(ui::panel_header("Проєкт", Vec::new()))
            .children(rows)
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .mt(px(16.))
                    .child(
                        ui::ghost_button("project-prepare", Some(IconName::Box), "Підготовка")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.enter_page(Page::Prepare, cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        ui::ghost_button("project-save", Some(IconName::Save), "Зберегти як…")
                            .inert(self.project.is_none())
                            .when(self.project.is_some(), |d| {
                                d.on_click(
                                    cx.listener(|this, _, window, cx| this.prompt_save(window, cx)),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        // `Jobs` скасовує свої задачі сам (kill рушія GUI й MCP); тут — чужі сесії рушія.
        self.jobs.cancel_all();
        slicer::cancel_active_slice();
    }
}

/// Мініатюри й `plate_N.json` для `.gcode.3mf`, який Slicenv зібрав сам.
fn attach_thumbnails(
    input: &std::path::Path,
    archive: &std::path::Path,
    plate: u32,
    bed_type: &str,
) -> anyhow::Result<()> {
    let color = (|| {
        use std::io::Read as _;
        let mut zip = zip::ZipArchive::new(std::fs::File::open(input).ok()?).ok()?;
        let mut text = String::new();
        zip.by_name("Metadata/project_settings.config").ok()?.read_to_string(&mut text).ok()?;
        let json: serde_json::Value = serde_json::from_str(&text).ok()?;
        let hex = json["filament_colour"].as_array()?.first()?.as_str()?.trim_start_matches('#');
        let v = u32::from_str_radix(hex.get(..6)?, 16).ok()?;
        Some([
            ((v >> 16) & 0xff) as f32 / 255.0,
            ((v >> 8) & 0xff) as f32 / 255.0,
            (v & 0xff) as f32 / 255.0,
        ])
    })();
    let images = viewer::plate_images(input, plate, color)
        .ok_or_else(|| anyhow::anyhow!("не вдалося відрендерити пластину {plate}"))?;
    let (lo, hi) = images.bounds;
    let bed = match bed_type {
        "Cool Plate" => "cool_plate",
        "Engineering Plate" => "engineering_plate",
        "High Temp Plate" => "hot_plate",
        "Supertack Plate" => "supertack_plate",
        _ => "textured_plate",
    };
    let json = serde_json::json!({
        "bbox_all": [lo.x, lo.y, hi.x, hi.y],
        "bed_type": bed,
        "is_seq_print": false,
        "version": 2,
    });
    slicer::attach_plate_assets(
        archive,
        plate,
        &slicer::PlateAssets {
            no_light: Some(images.plate.clone()),
            plate: Some(images.plate),
            small: Some(images.small),
            pick: Some(images.top.clone()),
            top: Some(images.top),
            json: Some(serde_json::to_vec(&json)?),
        },
    )
}

#[cfg(test)]
mod package_tests {
    /// Наскрізно на справжньому рушії: `SLICENV_TEST_3MF=<проєкт> SLICENV_TEST_OUT=<файл>`
    /// `cargo test -p app -- --ignored compatibility_package`. Якщо рушій нарізав у сумісному
    /// режимі, пакет отримує мініатюри від нашого рендерера.
    #[test]
    #[ignore]
    fn compatibility_package_gets_thumbnails_end_to_end() {
        use std::io::Read as _;
        let input = std::path::PathBuf::from(std::env::var_os("SLICENV_TEST_3MF").unwrap());
        let plate: u32 = std::env::var("SLICENV_TEST_PLATE").ok().and_then(|p| p.parse().ok()).unwrap_or(1);
        let engine = slicer::Engine::discover(slicer::EngineKind::Orca).unwrap();
        let store = slicer::PresetStore::open_for_engine(&engine).unwrap();
        let request = slicer::SliceRequest {
            input: input.clone(),
            plate,
            allow_newer_file: true,
            selection: slicer::Selection {
                machine: "Bambu Lab A1 0.4 nozzle".into(),
                process: "0.20mm Standard @BBL A1".into(),
                filament: "Bambu PLA Basic @BBL A1".into(),
                bed_type: "Textured PEI Plate".into(),
            },
            overrides: Default::default(),
        };
        let out = slicer::slice_with_engine(&engine, &store, &request).unwrap();
        println!("compatibility: {:?}", out.compatibility_note);
        if slicer::is_slicenv_package(&out.gcode_3mf) {
            super::attach_thumbnails(&input, &out.gcode_3mf, plate, "Textured PEI Plate").unwrap();
        }
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&out.gcode_3mf).unwrap()).unwrap();
        let names: Vec<String> = zip.file_names().map(|n| n.unwrap().into_owned()).collect();
        println!("{names:#?}");
        for name in [format!("Metadata/plate_{plate}.png"), format!("Metadata/plate_{plate}.gcode"), "Metadata/slice_info.config".into()] {
            assert!(names.contains(&name), "немає {name}");
        }
        let mut info = String::new();
        zip.by_name("Metadata/slice_info.config").unwrap().read_to_string(&mut info).unwrap();
        println!("{info}");
        if let Some(target) = std::env::var_os("SLICENV_TEST_OUT") {
            std::fs::copy(&out.gcode_3mf, target).unwrap();
        }
    }
}
