//! Теки з моделями: сканування коренів бібліотеки, стеження за змінами й прев’ю.
//!
//! Сканування й стеження не залежать від gpui: `scan` виконується у фоні, а `Watch` лише
//! сигналізує в канал, що щось змінилось, — дебаунс і пересканування робить UI (`home.rs`).

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

use gpui::{Image, ImageFormat, SharedString};

/// Розширення, які показує бібліотека (порівняння без урахування регістру: `.STL`, `.3MF`).
/// 3MF/STL/OBJ відкриваються як проєкт, G-code — одразу в «Перегляді».
pub const EXTENSIONS: &[&str] = &["3mf", "stl", "obj", "gcode"];
/// Глибина підтек від кореня. Корінь — часто домашня тека, а моделі лежать у
/// `Downloads/<архів>/<модель>/files/…`, тож трьох рівнів (як було) замало.
pub const MAX_DEPTH: usize = 6;
/// Запобіжники від кореня на весь диск: решту просто не показуємо.
const MAX_ENTRIES: usize = 20_000;
const MAX_DIRS: usize = 5_000;
/// Теки, де моделей не буває, а файлів — тисячі (і на Windows вони ще й змінюються щосекунди).
const SKIPPED_DIRS: &[&str] = &[
    "appdata",
    "node_modules",
    "$recycle.bin",
    "system volume information",
    "__pycache__",
];
/// Виконувані файли рушіїв: тека з таким файлом — розпаковане встановлення (portable-архів
/// Orca/Bambu, `bin/` linux-збірки, `Contents/MacOS` бандла). Її STL — столи принтерів і
/// калібрувальні моделі, а не моделі користувача. AppImage сюди навмисно не входить:
/// це один самодостатній файл, і його часто кладуть просто в `Downloads` поруч зі
/// справжніми моделями. Розпакований AppImage (`squashfs-root`) ловить `resources/profiles`.
const ENGINE_EXECUTABLES: &[&str] = &[
    "orca-slicer.exe",
    "orcaslicer.exe",
    "bambu-studio.exe",
    "bambustudio.exe",
    "orca-slicer",
    "bambu-studio",
    "orcaslicer",
    "bambustudio",
];

/// Тека за замовчуванням, якщо користувач ще не вибрав своїх.
pub fn default_dir() -> PathBuf {
    let home = paths::home_dir();
    let models = home.join("Documents/3d-models");
    if models.is_dir() { models } else { home }
}

/// Назва теки для заголовка: остання складова шляху (`C:\` і `/` — як є).
pub fn dir_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

pub struct Entry {
    pub path: PathBuf,
    /// Індекс кореня в `Workspace::library_roots`.
    pub root: usize,
    pub name: SharedString,
    pub details: SharedString,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub thumbnail: Option<Arc<Image>>,
}

impl Entry {
    /// Той самий файл без змін: прев’ю можна взяти зі старого запису.
    pub fn same_file(&self, other: &Entry) -> bool {
        self.path == other.path && self.size == other.size && self.modified == other.modified
    }
}

/// Результат сканування всіх коренів.
#[derive(Default)]
pub struct Scan {
    pub entries: Vec<Entry>,
    /// Усі пройдені теки (з коренями) — для стеження на Linux і фільтра подій.
    pub dirs: Vec<PathBuf>,
    /// Спрацював запобіжник `MAX_ENTRIES`/`MAX_DIRS`.
    pub truncated: bool,
    /// Пропущені встановлення рушіїв під коренями — щоб watcher не реагував на зміни в них.
    pub skipped: Vec<PathBuf>,
}

/// Файл, який показує бібліотека.
pub fn is_library_file(path: &Path) -> bool {
    path.extension().is_some_and(|e| {
        let e = e.to_string_lossy();
        EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x))
    })
}

/// Тека, яку сканування оминає: приховані (`.git`, `.cache`) і службові.
fn skipped_name(name: &str) -> bool {
    name.starts_with('.') || SKIPPED_DIRS.iter().any(|s| name.eq_ignore_ascii_case(s))
}

/// Windows: приховані й системні теки (`AppData`, з’єднання «Мои документы» в старих профілях).
#[cfg(windows)]
fn hidden_by_attributes(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    meta.file_attributes() & (HIDDEN | SYSTEM) != 0
}
#[cfg(not(windows))]
fn hidden_by_attributes(_: &fs::Metadata) -> bool {
    false
}

/// Бандл рушія на macOS: `OrcaSlicer.app`, `BambuStudio.app`.
fn engine_bundle_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.ends_with(".app") && (name.contains("orca") || name.contains("bambu"))
}

/// Тека — встановлення рушія: поруч виконуваний файл рушія або `resources/profiles`.
/// Імена й тип беремо з уже прочитаних записів теки (без stat на Linux і Windows);
/// додатковий stat — лише для `resources/profiles`, коли поруч є `resources`.
fn is_engine_install(items: &[fs::DirEntry]) -> bool {
    items.iter().any(|item| {
        let name = item.file_name();
        let name = name.to_string_lossy();
        if ENGINE_EXECUTABLES
            .iter()
            .any(|e| name.eq_ignore_ascii_case(e))
        {
            return item.file_type().is_ok_and(|t| !t.is_dir());
        }
        name.eq_ignore_ascii_case("resources") && item.path().join("profiles").is_dir()
    })
}

/// Підтримувані моделі в усіх `roots` (до `MAX_DEPTH` рівнів), за абеткою.
pub fn scan(roots: &[PathBuf]) -> Scan {
    scan_with(roots, &crate::engine_manager::engines_dir())
}

/// `scan` з явною текою рушіїв менеджера (у тестах — тимчасова).
fn scan_with(roots: &[PathBuf], engines: &Path) -> Scan {
    let mut out = Scan::default();
    for (index, root) in roots.iter().enumerate() {
        out.dirs.push(root.clone());
        // Корінь, який вибрав користувач, не перевіряємо: навіть усередині теки рушія.
        walk(index, root, root, engines, 0, &mut out);
    }
    out.entries.sort_by_cached_key(|e| e.name.to_lowercase());
    out
}

fn walk(index: usize, root: &Path, dir: &Path, engines: &Path, depth: usize, out: &mut Scan) {
    let Ok(read) = fs::read_dir(dir) else { return };
    let items: Vec<fs::DirEntry> = read.flatten().collect();
    if depth > 0 {
        if is_engine_install(&items) {
            out.skipped.push(dir.to_path_buf());
            return;
        }
        out.dirs.push(dir.to_path_buf());
    }
    let mut subdirs = Vec::new();
    for item in items {
        let path = item.path();
        // Без переходу за посиланнями: символьні посилання й з’єднання могли б зациклити обхід.
        let Ok(meta) = item.metadata() else { continue };
        if meta.is_dir() {
            let name = item.file_name();
            let name = name.to_string_lossy();
            if depth >= MAX_DEPTH || skipped_name(&name) || hidden_by_attributes(&meta) {
                continue;
            }
            if path == engines || engine_bundle_name(&name) {
                out.skipped.push(path);
            } else {
                subdirs.push(path);
            }
            continue;
        }
        if !meta.is_file() || !is_library_file(&path) {
            continue;
        }
        if out.entries.len() >= MAX_ENTRIES {
            out.truncated = true;
            return;
        }
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_uppercase())
            .unwrap_or_default();
        // Тека відносно кореня — у підписі картки й для пошуку за текою.
        let folder = path
            .parent()
            .and_then(|p| p.strip_prefix(root).ok())
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| format!(" · {}", p.display()))
            .unwrap_or_default();
        out.entries.push(Entry {
            name: display_name(&path).into(),
            details: format!("{ext} · {}{folder}", human_size(meta.len())).into(),
            root: index,
            size: meta.len(),
            modified: meta.modified().ok(),
            path,
            thumbnail: None,
        });
    }
    for sub in subdirs {
        if out.dirs.len() >= MAX_DIRS {
            out.truncated = true;
            return;
        }
        walk(index, root, &sub, engines, depth + 1, out);
    }
}

/// Чи стосується зміна `path` бібліотеки: файл моделі, відома або нова тека в межах
/// глибини, не в прихованій/службовій теці й не в пропущеному встановленні рушія (`skipped`).
/// Решта (кеші браузера, `.crdownload`, AppData, столи принтерів Orca) пересканування не
/// викликає.
pub fn is_relevant(
    roots: &[PathBuf],
    known_dirs: &HashSet<PathBuf>,
    skipped: &[PathBuf],
    path: &Path,
) -> bool {
    roots
        .iter()
        .any(|root| relevant_under(root, known_dirs, skipped, path))
}

fn relevant_under(
    root: &Path,
    known_dirs: &HashSet<PathBuf>,
    skipped: &[PathBuf],
    path: &Path,
) -> bool {
    let Ok(rest) = path.strip_prefix(root) else {
        return false;
    };
    // Лише вкладені в корінь: корінь усередині теки рушія користувач вибрав сам.
    if skipped
        .iter()
        .any(|s| s.as_path() != root && s.starts_with(root) && path.starts_with(s))
    {
        return false;
    }
    let parts: Vec<_> = rest.components().collect();
    if parts.is_empty() {
        return true;
    }
    // Усі теки на шляху (без останньої складової) мають пройти той самий фільтр, що й `walk`.
    if parts.len() > MAX_DEPTH + 1
        || parts[..parts.len() - 1]
            .iter()
            .any(|c| skipped_name(&c.as_os_str().to_string_lossy()))
    {
        return false;
    }
    let last = parts[parts.len() - 1].as_os_str().to_string_lossy();
    if is_library_file(path) {
        return true;
    }
    // Тека: відома (видалили чи перейменували) або нова.
    !skipped_name(&last) && (known_dirs.contains(path) || path.is_dir())
}

/// MakerWorld зберігає пробіли як `+`: «Sazanami+I+·+250mm» → «Sazanami I · 250mm».
fn display_name(path: &Path) -> String {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    stem.replace('+', " ")
}

fn human_size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => crate::tr_format!("{:.1} МБ", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => crate::tr_format!("{} КБ", b >> 10),
        b => crate::tr_format!("{b} Б"),
    }
}

/// Швидке читання готового або кешованого PNG-прев’ю (без важкого рендеру).
pub fn load_thumbnail_fast(path: &Path) -> Option<Arc<Image>> {
    viewer::thumbnail_cached(path).map(|png| Arc::new(Image::from_bytes(ImageFormat::Png, png)))
}

/// Читає PNG-прев’ю або генерує його у фоні через wgpu-рендерер. Для G-code й OBJ
/// рендерера немає — лише сусідній PNG.
pub fn load_thumbnail(path: &Path) -> Option<Arc<Image>> {
    let renderable = path.extension().is_some_and(|e| {
        viewer::SUPPORTED_EXTENSIONS
            .iter()
            .any(|x| e.eq_ignore_ascii_case(x))
    });
    if !renderable {
        return load_thumbnail_fast(path);
    }
    viewer::thumbnail(path).map(|png| Arc::new(Image::from_bytes(ImageFormat::Png, png)))
}

/// Відкрити теку в системному файловому менеджері.
pub fn open_folder(path: &Path) -> anyhow::Result<()> {
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    paths::host::command(program)
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}

/// Стеження за коренями через `notify`. Подія лише будить канал (місткість 1 — події
/// зливаються); дебаунс і пересканування — на боці UI.
///
/// Windows і macOS стежать рекурсивно за кожним коренем (ReadDirectoryChangesW і FSEvents
/// дешеві). На Linux рекурсивний inotify обійшов би всю домашню теку з `.cache` і
/// `node_modules`, тож стежимо нерекурсивно лише за теками, які пройшло сканування.
#[derive(Default)]
struct Filter {
    roots: Vec<PathBuf>,
    dirs: HashSet<PathBuf>,
    skipped: Vec<PathBuf>,
}

pub struct Watch {
    watcher: notify::RecommendedWatcher,
    watched: HashSet<PathBuf>,
    /// Корені, відомі й пропущені теки для фільтра подій у потоці notify.
    filter: Arc<Mutex<Filter>>,
}

impl Watch {
    pub fn new() -> notify::Result<(Self, async_channel::Receiver<()>)> {
        use notify::{
            EventKind,
            event::{AccessKind, AccessMode},
        };
        let (tx, rx) = async_channel::bounded(1);
        let filter: Arc<Mutex<Filter>> = Arc::default();
        let shared = filter.clone();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let Ok(event) = event else {
                // Переповнення черги чи помилка — краще зайве пересканування.
                let _ = tx.try_send(());
                return;
            };
            match event.kind {
                // Відкриття/читання файлу нічого не змінює; закриття після запису — так.
                EventKind::Access(AccessKind::Close(AccessMode::Write)) => {}
                EventKind::Access(_) => return,
                _ => {}
            }
            let relevant = {
                let Ok(guard) = shared.lock() else { return };
                let f = &*guard;
                event.need_rescan()
                    || event
                        .paths
                        .iter()
                        .any(|p| is_relevant(&f.roots, &f.dirs, &f.skipped, p))
            };
            if relevant {
                let _ = tx.try_send(());
            }
        })?;
        Ok((
            Self {
                watcher,
                watched: HashSet::new(),
                filter,
            },
            rx,
        ))
    }

    /// Привести набір стежень до коренів і тек останнього сканування.
    pub fn sync(&mut self, roots: &[PathBuf], scan: &Scan) {
        use notify::{RecursiveMode, Watcher as _};
        let (wanted, mode): (HashSet<PathBuf>, _) = if cfg!(target_os = "linux") {
            (
                scan.dirs.iter().cloned().collect(),
                RecursiveMode::NonRecursive,
            )
        } else {
            (roots.iter().cloned().collect(), RecursiveMode::Recursive)
        };
        if let Ok(mut guard) = self.filter.lock() {
            *guard = Filter {
                roots: roots.to_vec(),
                dirs: scan.dirs.iter().cloned().collect(),
                skipped: scan.skipped.clone(),
            };
        }
        for gone in self
            .watched
            .difference(&wanted)
            .cloned()
            .collect::<Vec<_>>()
        {
            let _ = self.watcher.unwatch(&gone);
            self.watched.remove(&gone);
        }
        for path in wanted {
            if !self.watched.contains(&path) && self.watcher.watch(&path, mode).is_ok() {
                self.watched.insert(path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"solid x\nendsolid x\n").unwrap();
    }

    fn names(scan: &Scan) -> Vec<String> {
        scan.entries.iter().map(|e| e.name.to_string()).collect()
    }

    #[test]
    fn scan_finds_upper_case_and_all_formats() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        touch(&root.join("A.STL"));
        touch(&root.join("b.3MF"));
        touch(&root.join("c.obj"));
        touch(&root.join("d.gcode"));
        touch(&root.join("e.gcode.3mf"));
        touch(&root.join("ignored.txt"));
        touch(&root.join("half.3mf.crdownload"));
        let scan = scan(std::slice::from_ref(&root));
        assert_eq!(names(&scan), ["A", "b", "c", "d", "e.gcode"]);
    }

    #[test]
    fn scan_goes_deep_but_skips_hidden_and_service_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        // Downloads/Fidget Spider Web/files/stl/v2/deep.stl — 5 рівнів тек.
        touch(&root.join("Downloads/Fidget Spider Web/files/stl/v2/deep.stl"));
        touch(&root.join(".cache/hidden.stl"));
        touch(&root.join("AppData/Local/app.stl"));
        touch(&root.join("node_modules/pkg/x.stl"));
        let too_deep: PathBuf = (0..=MAX_DEPTH).map(|i| format!("d{i}")).collect();
        touch(&root.join(too_deep).join("lost.stl"));
        let scan = scan(std::slice::from_ref(&root));
        assert_eq!(names(&scan), ["deep"]);
        assert!(
            scan.dirs
                .contains(&root.join("Downloads/Fidget Spider Web/files/stl/v2"))
        );
        assert!(!scan.dirs.iter().any(|d| d.ends_with(".cache")));
    }

    #[test]
    fn scan_skips_engine_installs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        // Розпакований portable Orca: exe поруч із resources/profiles.
        let orca = root.join("Downloads/OrcaSlicer_Windows_V2.5.0");
        touch(&orca.join("orca-slicer.exe"));
        touch(&orca.join("resources/profiles/Anycubic/plate.stl"));
        touch(&orca.join("resources/calib/filament_flow/flow.3mf"));
        touch(&orca.join("resources/printers/ams_load.gcode"));
        // Лише resources/profiles, без виконуваного файлу.
        touch(&root.join("Downloads/engine-copy/resources/profiles/BBL/bed.stl"));
        // Виконуваний файл без resources (bin/ linux-збірки).
        touch(&root.join("Downloads/Bambu/bin/bambu-studio"));
        touch(&root.join("Downloads/Bambu/bin/handy.stl"));
        // Бандл macOS і тека менеджера рушіїв Slicenv.
        touch(&root.join("Apps/OrcaSlicer.app/Contents/Resources/handy_models/cube.stl"));
        let engines = root.join("data/engines");
        touch(&engines.join("orca/2.4.2/handy_models/benchy.stl"));
        // Справжні моделі, зокрема з «Orca» в назві й тека з назвою як у виконуваного файлу.
        touch(&root.join("Downloads/Fidget_Spider_Web/Fidget_Spider_Web_A.stl"));
        touch(&root.join("Downloads/Fidget_Spider_Web/Fidget_Spider_Web_B.3mf"));
        touch(&root.join("Downloads/Orca models/vase.stl"));
        touch(&root.join("Projects/orca-slicer/part.stl"));
        touch(&root.join("Projects/resources/textures/rock.stl"));

        let scan = scan_with(std::slice::from_ref(&root), &engines);
        assert_eq!(
            names(&scan),
            [
                "Fidget_Spider_Web_A",
                "Fidget_Spider_Web_B",
                "part",
                "rock",
                "vase"
            ]
        );
        for skipped in [
            orca.clone(),
            root.join("Downloads/engine-copy"),
            root.join("Downloads/Bambu/bin"),
            root.join("Apps/OrcaSlicer.app"),
            engines.clone(),
        ] {
            assert!(scan.skipped.contains(&skipped), "{}", skipped.display());
            assert!(!scan.dirs.iter().any(|d| d.starts_with(&skipped)));
        }
    }

    #[test]
    fn explicit_root_inside_engine_install_is_scanned() {
        let dir = tempfile::tempdir().unwrap();
        let orca = dir.path().join("OrcaSlicer");
        touch(&orca.join("orca-slicer.exe"));
        touch(&orca.join("resources/profiles/plate.stl"));
        touch(&orca.join("my.stl"));
        // Корінь — сама тека рушія: її файли видно, а вкладене встановлення
        // (resources з profiles не є встановленням) обходимо як звичайно.
        let scan = scan_with(std::slice::from_ref(&orca), Path::new("/nonexistent"));
        assert_eq!(names(&scan), ["my", "plate"]);
    }

    #[test]
    fn scan_tags_entries_with_root_index() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        touch(&a.path().join("one.stl"));
        touch(&b.path().join("two.3mf"));
        let scan = scan(&[a.path().to_path_buf(), b.path().to_path_buf()]);
        let roots: Vec<_> = scan
            .entries
            .iter()
            .map(|e| (e.name.to_string(), e.root))
            .collect();
        assert_eq!(roots, [("one".to_string(), 0), ("two".to_string(), 1)]);
    }

    #[test]
    fn missing_root_is_empty_not_an_error() {
        let scan = scan(&[PathBuf::from("/definitely/not/here")]);
        assert!(scan.entries.is_empty());
    }

    #[test]
    fn relevant_events() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let roots = vec![root.clone()];
        fs::create_dir_all(root.join("new")).unwrap();
        let known: HashSet<PathBuf> = [root.join("old")].into();
        let skipped = vec![root.join("Downloads/OrcaSlicer")];
        let yes = |p: &str| is_relevant(&roots, &known, &skipped, &root.join(p));
        assert!(yes("model.3mf"));
        assert!(yes("sub/Model.STL"));
        assert!(yes("old"), "видалена відома тека");
        assert!(yes("new"), "нова тека");
        assert!(!yes("model.3mf.part"));
        assert!(!yes("notes.txt"));
        assert!(!yes(".cache/x.stl"));
        assert!(!yes("AppData/Local/x.3mf"));
        assert!(!yes("Downloads/OrcaSlicer/resources/profiles/plate.stl"));
        assert!(yes("Downloads/OrcaSlicerModels/x.stl"));
        assert!(!is_relevant(
            &roots,
            &known,
            &skipped,
            Path::new("/elsewhere/x.stl")
        ));
        // Корінь усередині пропущеної теки вибрав користувач — події з нього враховуємо.
        let inner = vec![root.join("Downloads/OrcaSlicer")];
        let plate = root.join("Downloads/OrcaSlicer/resources/profiles/plate.stl");
        assert!(is_relevant(&inner, &known, &skipped, &plate));
    }

    #[test]
    fn dir_name_is_the_real_folder_name() {
        assert_eq!(dir_name(Path::new("/home/user/Models")), "Models");
        assert_eq!(dir_name(Path::new("/")), "/");
    }
}
