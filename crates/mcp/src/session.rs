//! Шар інструментів: уся логіка без залежності від MCP-транспорту.
//!
//! `Session` тримає відкритий проєкт, історію undo і (ліниво) каталог профілів. Сервер лише
//! розкладає виклики `tools/call` на методи цього типу, тож той самий шар можна підключити
//! до іншого транспорту або до запущеного GUI.

use std::sync::OnceLock;
use std::{
    borrow::Cow,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail, ensure};
use glam::{DQuat, DVec3, EulerRot};
use project::{
    InstanceRef, Project,
    history::{Entry, History, Selection},
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use settings::{Locale, Schema};
use slicer::{Engine, EngineKind, Kind, PresetStore, SliceRequest};

/// Інструменти, що змінюють проєкт або пишуть файл; без `--allow-edit` їх немає в `tools/list`.
pub const EDIT_TOOLS: &[&str] = &[
    "transform_object",
    "arrange_plate",
    "orient_plate",
    "undo",
    "redo",
    "save_project",
];

#[derive(Deserialize, JsonSchema)]
pub struct OpenArgs {
    /// Шлях до `.3mf` (Bambu/Orca) або `.stl`.
    pub path: String,
}

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ProfileKind {
    Machine,
    Process,
    Filament,
}

impl ProfileKind {
    fn kind(self) -> Kind {
        match self {
            Self::Machine => Kind::Machine,
            Self::Process => Kind::Process,
            Self::Filament => Kind::Filament,
        }
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct ListProfilesArgs {
    pub kind: ProfileKind,
    /// Принтер, з яким профіль має бути сумісний. За замовчуванням — принтер відкритого проєкту.
    pub machine: Option<String>,
    /// Підрядок у назві (без урахування регістру).
    pub contains: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct GetProfileArgs {
    pub kind: ProfileKind,
    pub name: String,
    /// Точні ключі, які треба повернути (напр. `["layer_height", "wall_loops"]`).
    pub keys: Option<Vec<String>>,
    /// Підрядок у ключі. Без `keys` і `contains` повертається весь профіль.
    pub contains: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ExplainArgs {
    /// Ключ налаштування, напр. `sparse_infill_density`.
    pub key: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct GcodeArgs {
    /// `.gcode` або `.gcode.3mf` (береться перша `Metadata/plate_N.gcode`).
    pub path: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct SliceArgs {
    /// Номер пластини з 1.
    pub plate: u32,
    /// Імена профілів; відсутні беруться з проєкту, далі з Bambu Studio.
    pub machine: Option<String>,
    pub process: Option<String>,
    pub filament: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct TransformArgs {
    /// `id` об’єкта з `project_summary`.
    pub object: u32,
    /// Номер інстанса (за замовчуванням 0).
    pub instance: Option<usize>,
    /// Масштаб уздовж осей світу навколо центру bounding box.
    pub scale: Option<[f64; 3]>,
    /// Поворот у градусах (Euler XYZ) навколо центру bounding box.
    pub rotate_deg: Option<[f64; 3]>,
    /// Зсув у мм у координатах пластини.
    pub translate: Option<[f64; 3]>,
    /// Центрувати по XY на столі (не з `translate` по XY).
    pub center_on_plate: Option<bool>,
    /// Опустити нижню точку на стіл (Z = 0).
    pub drop_to_bed: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ArrangeArgs {
    /// Номер пластини з 1.
    pub plate: u32,
    /// Проміжок між об’єктами, мм (за замовчуванням 5).
    pub spacing_mm: Option<f64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SaveArgs {
    /// Куди записати `.3mf`. Оригінал не перезаписується мовчки: шлях обов’язковий.
    pub path: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct PrinterArgs {
    /// Ім’я принтера з акаунта Bambu; за замовчуванням — перший онлайн.
    pub printer: Option<String>,
}

struct Loaded {
    project: Project,
    path: PathBuf,
    dirty: bool,
}

struct Presets {
    engine: Option<Engine>,
    store: PresetStore,
}

pub struct Session {
    allow_edit: bool,
    loaded: Option<Loaded>,
    history: History,
    presets: Option<Presets>,
}

fn r2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn vec3(v: DVec3) -> Value {
    json!([r2(v.x), r2(v.y), r2(v.z)])
}

fn check_ext(path: &str, allowed: &[&str]) -> Result<PathBuf> {
    let p = PathBuf::from(path);
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    ensure!(
        allowed.contains(&ext.as_str()),
        "очікується файл {} (отримано «{path}»)",
        allowed.join("/")
    );
    Ok(p)
}

fn duration_json(d: Option<std::time::Duration>) -> Value {
    d.map_or(Value::Null, |d| json!(d.as_secs()))
}

fn schemas() -> &'static [(&'static str, Schema)] {
    static SCHEMAS: OnceLock<Vec<(&'static str, Schema)>> = OnceLock::new();
    SCHEMAS.get_or_init(|| {
        [("bambu", Schema::load()), ("orca", Schema::load_orca())]
            .into_iter()
            .filter_map(|(name, schema)| Some((name, schema.ok()?)))
            .collect()
    })
}

/// Схожі ключі для підказки, коли ключ не знайдено.
fn similar_keys(key: &str) -> Vec<&'static str> {
    let mut out: Vec<&str> = schemas()
        .iter()
        .flat_map(|(_, s)| s.options.keys())
        .map(String::as_str)
        .filter(|k| k.contains(key) || key.contains(*k))
        .collect();
    out.sort_unstable();
    out.dedup();
    out.truncate(10);
    out
}

impl Session {
    pub fn new(allow_edit: bool) -> Self {
        Self {
            allow_edit,
            loaded: None,
            history: History::default(),
            presets: None,
        }
    }

    /// Snapshot of the host project; shared mesh data stays Arc-backed.
    pub fn from_project(project: Project, path: PathBuf, dirty: bool, allow_edit: bool) -> Self {
        let mut session = Self::new(allow_edit);
        session.loaded = Some(Loaded {
            project,
            path,
            dirty,
        });
        session
    }

    pub fn into_project(self) -> Option<(Project, PathBuf, bool)> {
        self.loaded.map(|l| (l.project, l.path, l.dirty))
    }

    /// Підставляє готовий каталог профілів (тести, нестандартні встановлення).
    pub fn with_store(mut self, store: PresetStore) -> Self {
        self.presets = Some(Presets {
            engine: None,
            store,
        });
        self
    }

    fn loaded(&self) -> Result<&Loaded> {
        self.loaded
            .as_ref()
            .context("проєкт не відкрито: спершу виклич open_project")
    }

    fn presets(&mut self) -> Result<&Presets> {
        if self.presets.is_none() {
            let engine = Engine::discover(EngineKind::Bambu)
                .or_else(|_| Engine::discover(EngineKind::Orca))
                .ok();
            let store = match &engine {
                Some(e) => PresetStore::open_for_engine(e).or_else(|_| PresetStore::open_default()),
                None => PresetStore::open_default(),
            }
            .context("не вдалося відкрити каталог профілів")?;
            self.presets = Some(Presets { engine, store });
        }
        Ok(self.presets.as_ref().unwrap())
    }

    // ---------- читання ----------

    pub fn open_project(&mut self, args: OpenArgs) -> Result<Value> {
        let path = check_ext(&args.path, &["3mf", "stl"])?;
        let project = Project::load(&path)
            .with_context(|| format!("не вдалося відкрити {}", path.display()))?;
        self.loaded = Some(Loaded {
            project,
            path,
            dirty: false,
        });
        self.history.clear();
        self.project_summary()
    }

    pub fn project_summary(&self) -> Result<Value> {
        let l = self.loaded()?;
        let p = &l.project;
        let mut plates = Vec::new();
        for plate in &p.plates {
            let objects = plate
                .objects
                .iter()
                .map(|k| instance_info(p, *k))
                .collect::<Result<Vec<_>>>()?;
            plates.push(json!({
                "index": plate.index, "name": plate.name, "locked": plate.locked,
                "bed_type": plate.bed_type, "objects": objects,
            }));
        }
        let mut unplaced = Vec::new();
        for o in &p.objects {
            for (instance, i) in o.instances.iter().enumerate() {
                if i.plate.is_none() {
                    unplaced.push(instance_info(
                        p,
                        InstanceRef {
                            object: o.id,
                            instance,
                        },
                    )?);
                }
            }
        }
        let profiles = slicer::project_selection(&l.path).map(|s| {
            json!({"machine": s.machine, "process": s.process, "filament": s.filament, "bed_type": s.bed_type})
        });
        Ok(json!({
            "path": l.path, "unsaved_changes": l.dirty, "plates": plates, "unplaced": unplaced,
            "profiles": profiles, "can_undo": self.history.undo_label(), "can_redo": self.history.redo_label(),
        }))
    }

    pub fn list_profiles(&mut self, args: ListProfilesArgs) -> Result<Value> {
        let machine = match args.machine {
            Some(m) => m,
            None => self
                .loaded
                .as_ref()
                .and_then(|l| slicer::project_selection(&l.path))
                .map(|s| s.machine)
                .unwrap_or_default(),
        };
        let kind = args.kind.kind();
        ensure!(
            kind == Kind::Machine || !machine.is_empty(),
            "вкажи machine або відкрий проєкт із принтером"
        );
        let needle = args.contains.map(|s| s.to_lowercase());
        let store = &self.presets()?.store;
        let names: Vec<&str> = store
            .compatible(kind, &machine)
            .into_iter()
            .map(|p| p.name.as_str())
            .filter(|n| needle.as_ref().is_none_or(|s| n.to_lowercase().contains(s)))
            .collect();
        Ok(json!({"kind": kind.dir(), "machine": machine, "count": names.len(), "names": names}))
    }

    pub fn get_profile(&mut self, args: GetProfileArgs) -> Result<Value> {
        let mut values = self
            .presets()?
            .store
            .flatten(args.kind.kind(), &args.name)?;
        let total = values.len();
        let needle = args.contains.map(|s| s.to_lowercase());
        if args.keys.is_some() || needle.is_some() {
            values.retain(|k, _| {
                args.keys.as_ref().is_some_and(|keys| keys.contains(k))
                    || needle
                        .as_ref()
                        .is_some_and(|s| k.to_lowercase().contains(s))
            });
        }
        Ok(
            json!({"name": args.name, "total_keys": total, "returned": values.len(), "values": values}),
        )
    }

    pub fn explain_setting(&self, args: ExplainArgs) -> Result<Value> {
        for (source, schema) in schemas() {
            let Some(o) = schema.options.get(&args.key) else {
                continue;
            };
            let text = |t: &str| t.replace("%%", "%");
            let label = |locale| match o.full_label(locale) {
                "" => o.label(locale),
                full => full,
            };
            return Ok(json!({
                "key": args.key, "source": source,
                "label": label(Locale::En), "label_uk": label(Locale::Uk),
                "description": text(o.description(Locale::En)), "description_uk": text(o.description(Locale::Uk)),
                "category": o.category(Locale::En), "type": format!("{:?}", o.kind),
                "unit": o.sidetext(Locale::En), "min": o.min, "max": o.max, "default": o.default,
                "enum_values": o.enum_values, "enum_labels": o.enum_labels(Locale::En).collect::<Vec<_>>(),
                "mode": format!("{:?}", o.mode),
                "can_override_per_object": o.can_override_object(),
                "documentation_url": o.documentation_url(),
            }));
        }
        bail!(
            "невідомий ключ «{}»; схожі: {:?}",
            args.key,
            similar_keys(&args.key)
        )
    }

    pub fn gcode_stats(&self, args: GcodeArgs) -> Result<Value> {
        let path = check_ext(&args.path, &["gcode", "3mf"])?;
        gcode_stats_value(&path)
    }

    /// Готує нарізку під блокуванням сесії; сам запуск (`SliceJob::run`) можна робити поза нею.
    pub fn prepare_slice(&mut self, args: SliceArgs) -> Result<SliceJob> {
        let selection = slicer::project_selection(&self.loaded()?.path);
        let presets = self.presets()?;
        let engine = presets
            .engine
            .clone()
            .context("слайсер не знайдено: встанови Bambu Studio чи OrcaSlicer або задай BAMBU_STUDIO / ORCA_SLICER")?;
        let store = presets.store.clone();
        let base = selection
            .or_else(|| store.studio_selection())
            .map(|s| store.sanitize(s));
        let mut sel = base.unwrap_or_else(|| slicer::Selection {
            machine: String::new(),
            process: String::new(),
            filament: String::new(),
            bed_type: slicer::BED_TYPES[0].into(),
        });
        for (slot, kind, value) in [
            (&mut sel.machine, Kind::Machine, args.machine),
            (&mut sel.process, Kind::Process, args.process),
            (&mut sel.filament, Kind::Filament, args.filament),
        ] {
            if let Some(name) = value {
                ensure!(
                    store.get(kind, &name).is_some(),
                    "немає профілю {} «{name}»",
                    kind.dir()
                );
                *slot = name;
            }
            ensure!(
                !slot.is_empty(),
                "не визначено профіль {}: передай його явно",
                kind.dir()
            );
        }
        // Нарізаємо поточний (можливо, відредагований) стан, а не файл на диску.
        let dir = slicer::cache_dir().join("mcp");
        std::fs::create_dir_all(&dir)?;
        let input = tempfile::Builder::new()
            .prefix("mcp-")
            .suffix(".3mf")
            .tempfile_in(&dir)?
            .into_temp_path();
        let project = &self.loaded()?.project;
        project.save_3mf_for_engine(&input)?;
        // Як у GUI (Workspace::start_slice): 3MF з Bambu Studio в Orca інакше падає з −24.
        let allow_newer_file =
            engine.kind == slicer::EngineKind::Orca && project.from_bambu_studio();
        Ok(SliceJob {
            engine,
            store,
            plate: args.plate,
            input,
            selection: sel,
            allow_newer_file,
        })
    }

    pub fn printer_status(&self, args: PrinterArgs) -> Result<Value> {
        crate::status::printer_status(args.printer.as_deref())
    }

    // ---------- зміни (undo-чекпоінт на кожну) ----------

    /// Застосовує зміну до копії: невдала операція не лишає частково зміненого проєкту.
    fn edit<T>(
        &mut self,
        label: &str,
        change: impl FnOnce(&mut Project) -> Result<T>,
    ) -> Result<T> {
        ensure!(
            self.allow_edit,
            "редагування вимкнено: запусти сервер з --allow-edit"
        );
        let l = self
            .loaded
            .as_mut()
            .context("проєкт не відкрито: спершу виклич open_project")?;
        let mut next = l.project.clone();
        let out = change(&mut next)?;
        let before = Entry {
            label: label.into(),
            project: std::mem::replace(&mut l.project, next),
            selection: Selection::default(),
        };
        l.dirty |= self.history.record(before, &l.project);
        Ok(out)
    }

    pub fn transform_object(&mut self, args: TransformArgs) -> Result<Value> {
        let key = InstanceRef {
            object: args.object,
            instance: args.instance.unwrap_or(0),
        };
        ensure!(
            !(args.center_on_plate == Some(true)
                && args.translate.is_some_and(|t| t[0] != 0.0 || t[1] != 0.0)),
            "center_on_plate несумісний із translate по XY"
        );
        self.edit("Перетворення об’єкта (MCP)", |p| {
            if let Some(plate) = p.instance(key)?.plate {
                ensure!(!p.plate_locked(plate), "пластина {plate} заблокована");
            }
            if let Some(s) = args.scale {
                p.scale(key, s.into())?;
            }
            if let Some(r) = args.rotate_deg {
                let [x, y, z] = r.map(f64::to_radians);
                p.rotate(key, DQuat::from_euler(EulerRot::XYZ, x, y, z))?;
            }
            if args.center_on_plate == Some(true) {
                p.center_on_plate(key)?;
            }
            if let Some(t) = args.translate {
                p.translate(key, t.into())?;
            }
            if args.drop_to_bed == Some(true) {
                p.drop_to_bed(key)?;
            }
            instance_info(p, key)
        })
    }

    pub fn arrange_plate(&mut self, args: ArrangeArgs) -> Result<Value> {
        let bed = self.bed_for_project();
        self.edit("Розкладання пластини (MCP)", |p| {
            let spacing = args.spacing_mm.unwrap_or(5.0);
            match bed {
                Some((lo, hi)) => p.arrange_on_bed(args.plate, spacing, lo, hi)?,
                None => p.arrange(args.plate, spacing)?,
            }
            Ok(())
        })?;
        let p = &self.loaded()?.project;
        let objects = p
            .plates
            .iter()
            .find(|pl| pl.index == args.plate)
            .map(|pl| pl.objects.iter().map(|k| instance_info(p, *k)).collect())
            .unwrap_or_else(|| Ok(Vec::new()))?;
        Ok(json!({"plate": args.plate, "objects": objects}))
    }

    /// Автоорієнтація всіх об’єктів пластини (алгоритм Orca), потім опускання на стіл.
    pub fn orient_plate(&mut self, args: ArrangeArgs) -> Result<Value> {
        self.edit("Автоорієнтація пластини (MCP)", |p| {
            let plate = p
                .plates
                .iter()
                .find(|pl| pl.index == args.plate)
                .context("немає пластини")?;
            ensure!(!plate.locked, "пластина заблокована");
            for key in plate.objects.clone() {
                p.orient_instance(key)?;
            }
            Ok(())
        })?;
        let p = &self.loaded()?.project;
        let objects = p
            .plates
            .iter()
            .find(|pl| pl.index == args.plate)
            .map(|pl| pl.objects.iter().map(|k| instance_info(p, *k)).collect())
            .unwrap_or_else(|| Ok(Vec::new()))?;
        Ok(json!({"plate": args.plate, "objects": objects}))
    }

    /// Стіл принтера проєкту, якщо каталог профілів уже доступний; інакше — типовий.
    fn bed_for_project(&mut self) -> Option<([f64; 2], [f64; 2])> {
        let machine = slicer::project_selection(&self.loaded.as_ref()?.path)?.machine;
        self.presets().ok()?.store.bed_bounds(&machine)
    }

    pub fn undo(&mut self) -> Result<Value> {
        self.restore(false)
    }

    pub fn redo(&mut self) -> Result<Value> {
        self.restore(true)
    }

    fn restore(&mut self, redo: bool) -> Result<Value> {
        ensure!(
            self.allow_edit,
            "редагування вимкнено: запусти сервер з --allow-edit"
        );
        let label = if redo {
            self.history.redo_label()
        } else {
            self.history.undo_label()
        }
        .map(str::to_owned)
        .context(if redo {
            "нічого повторювати"
        } else {
            "нічого скасовувати"
        })?;
        let l = self.loaded.as_mut().context("проєкт не відкрито")?;
        if redo {
            self.history.redo(&mut l.project, Selection::default());
        } else {
            self.history.undo(&mut l.project, Selection::default());
        }
        l.dirty = true;
        Ok(json!({"applied": if redo { "redo" } else { "undo" }, "label": label}))
    }

    pub fn save_project(&mut self, args: SaveArgs) -> Result<Value> {
        ensure!(
            self.allow_edit,
            "редагування вимкнено: запусти сервер з --allow-edit"
        );
        let path = check_ext(&args.path, &["3mf"])?;
        let l = self.loaded.as_mut().context("проєкт не відкрито")?;
        l.project.save_3mf(&path)?;
        l.path = path.clone();
        l.dirty = false;
        Ok(json!({"saved": path}))
    }
}

pub struct SliceJob {
    engine: Engine,
    store: PresetStore,
    plate: u32,
    input: tempfile::TempPath,
    selection: slicer::Selection,
    allow_newer_file: bool,
}

impl SliceJob {
    /// Блокує потік на час роботи слайсера.
    pub fn run(self) -> Result<Value> {
        self.run_cancellable(&slicer::SliceCancellation::default())
    }

    pub fn run_cancellable(self, cancellation: &slicer::SliceCancellation) -> Result<Value> {
        let out = slicer::slice_with_engine_cancellable(
            &self.engine,
            &self.store,
            &SliceRequest {
                input: self.input.to_path_buf(),
                plate: self.plate,
                allow_newer_file: self.allow_newer_file,
                selection: self.selection.clone(),
                overrides: Default::default(),
            },
            cancellation,
        )?;
        let mut warnings = out.warnings.clone();
        let layers = match gcode_stats_value(&out.gcode_3mf) {
            Ok(stats) => stats["layers"].clone(),
            Err(err) => {
                warnings.push(format!("не вдалося розібрати G-code: {err:#}"));
                Value::Null
            }
        };
        let s = &self.selection;
        Ok(json!({
            "plate": self.plate, "engine": self.engine.kind.name(),
            "profiles": {"machine": s.machine, "process": s.process, "filament": s.filament, "bed_type": s.bed_type},
            "print_time_s": out.print_time.as_secs(), "filament_g": (out.filament_grams * 100.0).round() / 100.0,
            "layer_height_mm": r2(out.layer_height), "layers": layers, "warnings": warnings,
            "elapsed_s": out.elapsed.as_secs(), "gcode_3mf": out.gcode_3mf,
            "compatibility_mode": out.compatibility_note,
        }))
    }
}

fn instance_info(p: &Project, key: InstanceRef) -> Result<Value> {
    let object = p.object(key.object)?;
    let instance = p.instance(key)?;
    let (scale, rotation, position) = instance.transform.to_scale_rotation_translation();
    let (rx, ry, rz) = rotation.to_euler(EulerRot::XYZ);
    let parts: Vec<_> = object
        .volumes
        .iter()
        .filter(|v| v.kind == project::VolumeKind::Part)
        .collect();
    let bounds = p.instance_bounds(key)?;
    Ok(json!({
        "object": object.id, "instance": key.instance, "name": object.name,
        "printable": instance.printable, "plate": instance.plate,
        "parts": parts.len(), "triangles": parts.iter().map(|v| v.mesh.triangles.len()).sum::<usize>(),
        "position": vec3(position), "rotation_deg": vec3(DVec3::new(rx, ry, rz).map(f64::to_degrees)),
        "scale": vec3(scale),
        "bounds_min": bounds.map(|b| vec3(b.min)), "bounds_max": bounds.map(|b| vec3(b.max)),
        "size_mm": bounds.map(|b| vec3(b.size())),
    }))
}

/// `.gcode` читається напряму, `.gcode.3mf` — через першу `Metadata/plate_N.gcode`.
fn read_toolpath(path: &Path) -> Result<gcode::Toolpath> {
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("gcode"))
    {
        return gcode::parse_file(path);
    }
    use std::io::Read as _;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(std::fs::File::open(path)?))?;
    let name = (0..zip.len())
        .filter_map(|i| zip.name_for_index(i)?.ok())
        .filter(|n| n.starts_with("Metadata/plate_") && n.ends_with(".gcode"))
        .min()
        .map(Cow::into_owned)
        .context("у 3MF немає Metadata/plate_N.gcode: файл не нарізаний")?;
    let mut text = String::new();
    zip.by_name(&name)?.read_to_string(&mut text)?;
    gcode::parse(&text)
}

fn gcode_stats_value(path: &Path) -> Result<Value> {
    let tp =
        read_toolpath(path).with_context(|| format!("не вдалося розібрати {}", path.display()))?;
    let s = &tp.stats;
    let mut roles: Map<String, Value> = Map::new();
    for seg in tp.segments.iter().filter(|s| s.is_extrusion) {
        let slot = roles
            .entry(seg.role.name_uk().to_owned())
            .or_insert(json!(0.0));
        *slot = json!(slot.as_f64().unwrap_or(0.0) + f64::from(seg.length()));
    }
    for v in roles.values_mut() {
        *v = json!(r2(v.as_f64().unwrap_or(0.0)));
    }
    let point = |v: glam::Vec3| v.to_array().map(|c| r2(f64::from(c)));
    let bounds = tp
        .bounds()
        .map(|(min, max)| json!({"min": point(min), "max": point(max)}));
    Ok(json!({
        "layers": tp.layer_count(), "segments": tp.segment_count(),
        "print_time_s": duration_json(s.total_estimated_time), "model_time_s": duration_json(s.model_printing_time),
        "filament_g": s.total_filament_weight_g, "filament_m": s.total_filament_length_mm.map(|mm| r2(mm / 1000.0)),
        "filament_cm3": s.total_filament_volume_cm3, "max_z_mm": s.max_z_height,
        "extrusion_bounds_mm": bounds, "extrusion_mm_by_role": roles,
    }))
}
