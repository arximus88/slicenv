//! «Головна»: ліворуч дерево тек з моделями (як панель проєкту Zed), по центру пошук і картки з прев’ю.
//!
//! Бібліотека — кілька закріплених тек (коренів), як бічна панель IDE чи Finder. Корені
//! зберігаються в налаштуваннях; вміст пересканується у фоні за кнопкою, F5 і подіями `notify`.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use gpui::{
    AnyElement, ClipboardItem, Context, MouseButton, MouseDownEvent, ObjectFit, Pixels, Point,
    SharedString, Window, anchored, deferred, div, img, prelude::*, px,
};
use gpui_component::input::Input;

use crate::file_menu::FileMenu as _;
use crate::{
    library::{self, Entry},
    theme::zed,
    ui::{self, IconName},
    widgets::ToastKind,
    workspace::Workspace,
};

/// Скільки тек пам’ятає «Недавні теки».
const RECENT_LIMIT: usize = 8;
/// Пауза після події файлової системи: копіювання чи розпакування дає їх десятки.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(500);

/// Що відкрило контекстне меню бібліотеки.
#[derive(Clone, Debug, PartialEq)]
pub enum MenuTarget {
    /// Кнопка теки в заголовку: вибрати, додати, недавні.
    Header,
    /// Корінь бібліотеки за індексом.
    Root(usize),
    /// Підтека.
    Folder(PathBuf),
}

impl Workspace {
    /// Корені з налаштувань (або типова тека) плюс тека з аргументів на цей сеанс.
    pub(crate) fn init_library(
        &mut self,
        arg: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut roots = self
            .preferences
            .library_roots
            .clone()
            .unwrap_or_else(|| vec![library::default_dir()]);
        if let Some(dir) = arg
            && !roots.contains(&dir)
        {
            roots.insert(0, dir.clone());
            self.library_session_root = Some(dir);
        }
        self.library_roots = roots;
        self.sync_library_dir();
        self.start_library_watch(window, cx);
        self.rescan_library(cx);
    }

    fn sync_library_dir(&mut self) {
        self.library_dir = self
            .library_roots
            .first()
            .cloned()
            .unwrap_or_else(library::default_dir);
    }

    fn start_library_watch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (watch, events) = match library::Watch::new() {
            Ok(v) => v,
            Err(err) => {
                eprintln!("Slicenv: library watch: {err}");
                return;
            }
        };
        self.library_watch = Some(Arc::new(Mutex::new(watch)));
        cx.spawn_in(window, async move |this, cx| {
            while events.recv().await.is_ok() {
                cx.background_executor().timer(WATCH_DEBOUNCE).await;
                while events.try_recv().is_ok() {}
                if this.update(cx, |ws, cx| ws.rescan_library(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Пересканувати всі корені у фоні. Новий виклик скасовує незавершений.
    pub fn rescan_library(&mut self, cx: &mut Context<Self>) {
        let roots = self.library_roots.clone();
        let watch = self.library_watch.clone();
        self.library_scanning = true;
        self.library_scan = Some(cx.spawn(async move |this, cx| {
            let scan_roots = roots.clone();
            let scan = cx
                .background_executor()
                .spawn(async move {
                    let scan = library::scan(&scan_roots);
                    // На Linux це тисячі inotify_add_watch — не в потоці UI.
                    if let Some(watch) = watch
                        && let Ok(mut watch) = watch.lock()
                    {
                        watch.sync(&scan_roots, &scan);
                    }
                    scan
                })
                .await;
            this.update(cx, |ws, cx| {
                if ws.library_roots == roots {
                    ws.apply_library_scan(scan, cx);
                }
            })
            .ok();
        }));
        cx.notify();
    }

    fn apply_library_scan(&mut self, mut scan: library::Scan, cx: &mut Context<Self>) {
        let mut old: HashMap<PathBuf, Entry> = std::mem::take(&mut self.library)
            .into_iter()
            .map(|e| (e.path.clone(), e))
            .collect();
        for entry in &mut scan.entries {
            if let Some(prev) = old.remove(&entry.path)
                && prev.same_file(entry)
            {
                entry.thumbnail = prev.thumbnail;
            }
        }
        if scan.truncated {
            eprintln!(
                "Slicenv: library scan truncated at {} files / {} folders",
                scan.entries.len(),
                scan.dirs.len()
            );
        }
        self.library = scan.entries;
        self.library_scanning = false;
        self.load_thumbnails(cx);
        cx.notify();
    }

    /// Замінити всі корені (меню «Вибрати теку…», «Недавні теки»).
    pub fn set_library_roots(&mut self, roots: Vec<PathBuf>, cx: &mut Context<Self>) {
        for root in &roots {
            self.remember_recent(root.clone());
        }
        if self
            .library_session_root
            .as_ref()
            .is_some_and(|s| !roots.contains(s))
        {
            self.library_session_root = None;
        }
        self.library_roots = roots;
        self.library_changed(cx);
    }

    pub fn add_library_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.remember_recent(root.clone());
        if self.library_roots.contains(&root) {
            // Тека з аргументів, яку тепер додали явно, — уже не тимчасова.
            if self.library_session_root.as_ref() == Some(&root) {
                self.library_session_root = None;
                self.persist_library(cx);
            }
            return;
        }
        self.library_roots.push(root);
        self.library_changed(cx);
    }

    pub fn remove_library_root(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.library_roots.len() {
            return;
        }
        let root = self.library_roots.remove(index);
        if self.library_session_root.as_ref() == Some(&root) {
            self.library_session_root = None;
        }
        // Прибрану теку легко повернути з «Недавніх».
        self.remember_recent(root);
        self.library_changed(cx);
    }

    fn library_changed(&mut self, cx: &mut Context<Self>) {
        self.sync_library_dir();
        self.library.clear();
        self.persist_library(cx);
        self.rescan_library(cx);
    }

    fn remember_recent(&mut self, path: PathBuf) {
        let recent = &mut self.preferences.library_recent;
        recent.retain(|p| p != &path);
        recent.insert(0, path);
        recent.truncate(RECENT_LIMIT);
    }

    fn persist_library(&mut self, cx: &mut Context<Self>) {
        let session = self.library_session_root.clone();
        self.preferences.library_roots = Some(
            self.library_roots
                .iter()
                .filter(|r| Some(*r) != session.as_ref())
                .cloned()
                .collect(),
        );
        if let Err(err) = self.preferences.save() {
            self.toast(ToastKind::Error, format!("{err:#}"), cx);
        }
    }

    /// Системний діалог вибору теки: замінює корені або додає ще один.
    pub fn prompt_library_folder(
        &mut self,
        add: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = if add {
            "Додати теку до бібліотеки"
        } else {
            "Вибрати теку бібліотеки"
        };
        let dialog = rfd::AsyncFileDialog::new()
            .set_title(crate::i18n::tr(title))
            .set_directory(&self.library_dir)
            .set_parent(window);
        cx.spawn_in(window, async move |this, cx| {
            let Some(folder) = dialog.pick_folder().await else {
                return;
            };
            let path = folder.path().to_path_buf();
            this.update(cx, |ws, cx| {
                if add {
                    ws.add_library_root(path, cx);
                } else {
                    ws.set_library_roots(vec![path], cx);
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn open_library_menu(
        &mut self,
        position: Point<Pixels>,
        target: MenuTarget,
        cx: &mut Context<Self>,
    ) {
        self.file_menu = None;
        self.library_menu = Some((position, target));
        cx.notify();
    }

    fn close_library_menu(&mut self, cx: &mut Context<Self>) {
        if self.library_menu.take().is_some() {
            cx.notify();
        }
    }

    fn toggle_library_folder(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !self.library_collapsed.remove(&path) {
            self.library_collapsed.insert(path);
        }
        cx.notify();
    }
}

fn file_icon(path: &Path) -> IconName {
    match path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .as_deref()
    {
        Some("3mf") => IconName::FileBox,
        Some("stl" | "obj") => IconName::Box,
        Some("step" | "stp") => IconName::Cuboid,
        Some("gcode") => IconName::FileCode,
        _ => IconName::File,
    }
}

/// Записи, що підходять під пошук (без урахування регістру; збіг у назві або теці).
fn matching<'a>(ws: &'a Workspace, query: &str) -> Vec<(usize, &'a Entry)> {
    let query = query.trim().to_lowercase();
    ws.library
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            query.is_empty()
                || e.name.to_lowercase().contains(&query)
                || e.details.to_lowercase().contains(&query)
        })
        .collect()
}

pub fn render(
    ws: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let query = ws.library_search.read(cx).value().to_string();
    let left = tree(ws, &query, cx);
    let center = grid(ws, &query, window, cx);
    crate::prepare::docked("home-docks", ws, Some(left), center, None, cx)
}

/// Тека дерева: підтеки за абеткою (ключ — назва в нижньому регістрі) і файли.
#[derive(Default)]
struct Node<'a> {
    dirs: BTreeMap<String, (String, PathBuf, Node<'a>)>,
    files: Vec<(usize, &'a Entry)>,
}

impl<'a> Node<'a> {
    fn insert(&mut self, root: &Path, index: usize, entry: &'a Entry) {
        let rel = entry
            .path
            .parent()
            .and_then(|p| p.strip_prefix(root).ok())
            .unwrap_or(Path::new(""));
        let mut node = self;
        let mut path = root.to_path_buf();
        for part in rel.components() {
            let name = part.as_os_str().to_string_lossy().into_owned();
            path.push(&name);
            node = &mut node
                .dirs
                .entry(name.to_lowercase())
                .or_insert_with(|| (name, path.clone(), Node::default()))
                .2;
        }
        node.files.push((index, entry));
    }
}

/// Рядки дерева для `node` на глибині `depth`: спершу теки, потім файли (як в IDE).
fn tree_rows(
    ws: &Workspace,
    node: &Node,
    depth: usize,
    expand_all: bool,
    rows: &mut Vec<AnyElement>,
    cx: &mut Context<Workspace>,
) {
    let z = zed();
    for (name, path, child) in node.dirs.values() {
        // Ланцюжок тек без файлів — одним рядком «Downloads/Fidget Spider Web», як у Zed.
        let (mut label, mut path, mut child) = (name.clone(), path, child);
        while child.files.is_empty() && child.dirs.len() == 1 {
            let (next_name, next_path, next) =
                child.dirs.values().next().expect("single subfolder");
            label = format!("{label}/{next_name}");
            path = next_path;
            child = next;
        }
        let collapsed = !expand_all && ws.library_collapsed.contains(path);
        let (toggle, menu_path) = (path.clone(), path.clone());
        rows.push(
            ui::list_item(
                SharedString::from(format!("dir-{}", path.display())),
                depth,
                false,
            )
            .text_color(z.text_muted)
            .child(
                ui::icon(if collapsed {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronDown
                })
                .text_color(z.text_placeholder),
            )
            .child(
                ui::icon(if collapsed {
                    IconName::Folder
                } else {
                    IconName::FolderOpen
                })
                .text_color(z.text_muted),
            )
            .child(div().flex_1().min_w_0().truncate().child(label))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.open_library_menu(e.position, MenuTarget::Folder(menu_path.clone()), cx);
                }),
            )
            .on_click(
                cx.listener(move |this, _, _, cx| this.toggle_library_folder(toggle.clone(), cx)),
            )
            .into_any_element(),
        );
        if !collapsed {
            tree_rows(ws, child, depth + 1, expand_all, rows, cx);
        }
    }
    for &(i, entry) in &node.files {
        let selected = ws.path.as_deref() == Some(entry.path.as_path());
        let loading = ws.jobs.subject(crate::jobs::JobKind::Open) == Some(entry.path.as_path());
        let path = entry.path.clone();
        rows.push(
            ui::list_item(("lib-row", i), depth, selected)
                .child(ui::icon(file_icon(&entry.path)).text_color(if selected {
                    z.accent
                } else {
                    z.text_muted
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(entry.name.clone()),
                )
                .when(loading, |d| d.child(ui::spinner(("lib-spin", i))))
                .file_menu(entry.path.clone(), cx)
                .on_click(
                    cx.listener(move |this, _, window, cx| this.open(path.clone(), window, cx)),
                )
                .into_any_element(),
        );
    }
}

/// Пояснення під коренем без моделей: теки немає, або в ній порожньо.
fn empty_root_note(root: &Path, depth: usize) -> AnyElement {
    let z = zed();
    let text = if root.is_dir() {
        "Моделей не знайдено"
    } else {
        "Теку не знайдено"
    };
    div()
        .pl(px(10. + depth as f32 * 14. + 20.))
        .pr(px(8.))
        .py(px(2.))
        .text_xs()
        .text_color(z.text_placeholder)
        .child(crate::i18n::tr(text))
        .into_any_element()
}

/// Дерево: корені бібліотеки, теки з відступами, файли з іконкою за типом; клік відкриває модель.
fn tree(ws: &Workspace, query: &str, cx: &mut Context<Workspace>) -> AnyElement {
    let z = zed();
    let entries = matching(ws, query);
    let searching = !query.trim().is_empty();
    let single = ws.library_roots.len() == 1;
    let mut rows: Vec<AnyElement> = Vec::new();
    for (r, root) in ws.library_roots.iter().enumerate() {
        let mut node = Node::default();
        for &(i, entry) in entries.iter().filter(|(_, e)| e.root == r) {
            node.insert(root, i, entry);
        }
        let depth = usize::from(!single);
        let collapsed = !single && !searching && ws.library_collapsed.contains(root);
        if !single {
            let toggle = root.clone();
            rows.push(
                ui::list_item(("lib-root", r), 0, false)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(
                        ui::icon(if collapsed {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .text_color(z.text_placeholder),
                    )
                    .child(ui::icon(IconName::FolderRoot).text_color(z.text_muted))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(library::dir_name(root)),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.open_library_menu(e.position, MenuTarget::Root(r), cx);
                        }),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_library_folder(toggle.clone(), cx)
                    }))
                    .into_any_element(),
            );
        }
        if collapsed {
            continue;
        }
        if node.dirs.is_empty() && node.files.is_empty() {
            if !searching && !ws.library_scanning {
                rows.push(empty_root_note(root, depth));
            }
            continue;
        }
        tree_rows(ws, &node, depth, searching, &mut rows, cx);
    }

    // Справжня назва теки (не перекладається: тека «Головна» лишилась би «Головна»).
    let title: SharedString = match ws.library_roots.as_slice() {
        [root] => library::dir_name(root).into(),
        _ => crate::i18n::tr("Бібліотека").into(),
    };
    let header = ui::panel_header_text(
        title,
        vec![
            if ws.library_scanning {
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(24.))
                    .child(ui::spinner("lib-scan"))
                    .into_any_element()
            } else {
                ui::icon_button("lib-refresh", IconName::RefreshCw, "Оновити (F5)")
                    .on_click(cx.listener(|this, _, _, cx| this.rescan_library(cx)))
                    .into_any_element()
            },
            ui::icon_button("lib-folders", IconName::FolderOpen, "Теки бібліотеки…")
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, e: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.open_library_menu(e.position, MenuTarget::Header, cx);
                    }),
                )
                .into_any_element(),
        ],
    )
    .when(single, |d| {
        d.on_mouse_down(
            MouseButton::Right,
            cx.listener(|this, e: &MouseDownEvent, _, cx| {
                this.open_library_menu(e.position, MenuTarget::Root(0), cx);
            }),
        )
    });

    let body = if ws.library_roots.is_empty() {
        div()
            .flex_1()
            .min_h_0()
            .child(ui::empty_state(
                IconName::FolderOpen,
                "Бібліотека порожня",
                "Додай теку з моделями — нові файли з’являтимуться тут самі.",
                vec![
                    ui::ghost_button(
                        "lib-empty-pick",
                        Some(IconName::FolderPlus),
                        "Вибрати теку…",
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.prompt_library_folder(false, window, cx)
                    }))
                    .into_any_element(),
                ],
            ))
            .into_any_element()
    } else {
        div()
            .id("library-tree")
            .flex_1()
            .min_h_0()
            .py(px(4.))
            .overflow_y_scroll()
            .children(rows)
            .into_any_element()
    };
    ui::dock().child(header).child(body).into_any_element()
}

/// Контекстне меню бібліотеки: тека в заголовку, корінь чи підтека.
pub fn library_menu(
    ws: &Workspace,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Option<AnyElement> {
    let (position, target) = ws.library_menu.clone()?;
    let z = zed();
    let item = |id: SharedString, icon: IconName, label: &'static str| {
        ui::ghost_button(id, Some(icon), label).w_full()
    };
    let separator = || div().my(px(3.)).h(px(1.)).bg(z.border_variant);
    let mut items: Vec<AnyElement> = Vec::new();
    match &target {
        MenuTarget::Header => {
            items.push(
                item(
                    "lib-menu-pick".into(),
                    IconName::FolderOpen,
                    "Вибрати теку…",
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.close_library_menu(cx);
                    this.prompt_library_folder(false, window, cx);
                }))
                .into_any_element(),
            );
            items.push(
                item("lib-menu-add".into(), IconName::FolderPlus, "Додати теку…")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.close_library_menu(cx);
                        this.prompt_library_folder(true, window, cx);
                    }))
                    .into_any_element(),
            );
            items.push(
                item(
                    "lib-menu-refresh".into(),
                    IconName::RefreshCw,
                    "Оновити (F5)",
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.close_library_menu(cx);
                    this.rescan_library(cx);
                }))
                .into_any_element(),
            );
            let recent: Vec<&PathBuf> = ws
                .preferences
                .library_recent
                .iter()
                // Видалені чи відключені теки не пропонуємо (stat лише поки меню відкрите).
                .filter(|p| !ws.library_roots.contains(p) && p.is_dir())
                .collect();
            if !recent.is_empty() {
                items.push(separator().into_any_element());
                items.push(
                    div()
                        .px(px(8.))
                        .py(px(2.))
                        .text_xs()
                        .text_color(z.text_placeholder)
                        .child(crate::i18n::tr("Недавні теки"))
                        .into_any_element(),
                );
                for (i, path) in recent.into_iter().enumerate() {
                    let pick = path.clone();
                    items.push(
                        div()
                            .id(("lib-menu-recent", i))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .h(px(24.))
                            .px(px(8.))
                            .rounded(px(4.))
                            .cursor_pointer()
                            .text_sm()
                            .hover(|s| s.bg(z.element_hover))
                            .child(ui::icon(IconName::FolderClock).text_color(z.text_muted))
                            .child(div().flex_none().child(library::dir_name(path)))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(z.text_placeholder)
                                    .child(crate::shorten_home(path)),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_library_menu(cx);
                                this.set_library_roots(vec![pick.clone()], cx);
                            }))
                            .into_any_element(),
                    );
                }
            }
        }
        MenuTarget::Root(_) | MenuTarget::Folder(_) => {
            let path = match &target {
                MenuTarget::Root(r) => ws.library_roots.get(*r)?.clone(),
                MenuTarget::Folder(p) => p.clone(),
                MenuTarget::Header => unreachable!(),
            };
            let (open, reveal, copy) = (path.clone(), path.clone(), path.clone());
            items.push(
                item("lib-menu-open".into(), IconName::FolderOpen, "Відкрити")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.close_library_menu(cx);
                        if let Err(err) = library::open_folder(&open) {
                            this.toast(ToastKind::Error, format!("{err:#}"), cx);
                        }
                    }))
                    .into_any_element(),
            );
            items.push(
                item(
                    "lib-menu-reveal".into(),
                    IconName::Folder,
                    "Відкрити розташування",
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.close_library_menu(cx);
                    if let Err(err) = crate::file_menu::reveal(&reveal) {
                        this.toast(ToastKind::Error, format!("{err:#}"), cx);
                    }
                }))
                .into_any_element(),
            );
            items.push(
                item("lib-menu-copy".into(), IconName::Copy, "Копіювати шлях")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.close_library_menu(cx);
                        cx.write_to_clipboard(ClipboardItem::new_string(
                            copy.display().to_string(),
                        ));
                    }))
                    .into_any_element(),
            );
            items.push(
                item(
                    "lib-menu-refresh".into(),
                    IconName::RefreshCw,
                    "Оновити (F5)",
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.close_library_menu(cx);
                    this.rescan_library(cx);
                }))
                .into_any_element(),
            );
            if let MenuTarget::Root(r) = target {
                items.push(separator().into_any_element());
                items.push(
                    item(
                        "lib-menu-remove".into(),
                        IconName::FolderMinus,
                        "Прибрати з бібліотеки",
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.close_library_menu(cx);
                        this.remove_library_root(r, cx);
                    }))
                    .into_any_element(),
                );
            }
        }
    }
    let menu = div()
        .id("library-menu")
        .occlude()
        .min_w(px(240.))
        .max_w(px(420.))
        .flex()
        .flex_col()
        .p(px(4.))
        .gap(px(1.))
        .rounded(px(6.))
        .bg(z.panel)
        .border_1()
        .border_color(z.border)
        .shadow_lg()
        .text_color(z.text)
        .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_library_menu(cx)))
        .children(items);
    Some(
        deferred(anchored().position(position).snap_to_window().child(menu))
            .with_priority(3)
            .into_any_element(),
    )
}

/// Сітка карток із прев’ю; вибрана — з акцентною рамкою.
fn grid(
    ws: &Workspace,
    query: &str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let z = zed();
    let items = matching(ws, query);
    let count = items.len();
    let cards = items.into_iter().map(|(i, entry)| {
        let selected = ws.path.as_deref() == Some(entry.path.as_path());
        let loading = ws.jobs.subject(crate::jobs::JobKind::Open) == Some(entry.path.as_path());
        let path = entry.path.clone();
        div()
            .id(("model-card", i))
            .w(px(188.))
            .flex()
            .flex_col()
            .rounded(px(6.))
            .overflow_hidden()
            .bg(z.panel)
            .border_1()
            .border_color(if selected { z.accent } else { z.border_variant })
            .cursor_pointer()
            .hover(|s| {
                s.border_color(if selected { z.accent } else { z.border })
                    .bg(z.element_hover)
            })
            .file_menu(entry.path.clone(), cx)
            .on_click(cx.listener(move |this, _, window, cx| this.open(path.clone(), window, cx)))
            .child(
                div()
                    .h(px(140.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(z.editor)
                    .border_b_1()
                    .border_color(z.border_variant)
                    .map(|d| match &entry.thumbnail {
                        Some(thumb) => d.child(
                            img(thumb.clone())
                                .size_full()
                                .object_fit(ObjectFit::Contain),
                        ),
                        None => d.child(
                            gpui_component::Icon::new(file_icon(&entry.path))
                                .size(px(28.))
                                .text_color(z.text_placeholder),
                        ),
                    }),
            )
            .child(
                div()
                    .px(px(8.))
                    .py(px(6.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().text_sm().truncate().child(entry.name.clone()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .text_xs()
                            .text_color(if loading {
                                z.accent
                            } else {
                                z.text_placeholder
                            })
                            .when(loading, |d| d.child(ui::spinner(("card-spin", i))))
                            .child(div().truncate().child(if loading {
                                crate::i18n::tr("відкриваю…").into()
                            } else {
                                entry.details.clone()
                            })),
                    ),
            )
    });

    div()
        .flex()
        .flex_col()
        .size_full()
        .min_w_0()
        .bg(z.editor)
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(8.))
                .h(px(40.))
                .px(px(12.))
                .border_b_1()
                .border_color(z.border_variant)
                .child(
                    div().w(px(320.)).child(crate::widgets::field(
                        &ws.library_search,
                        Input::new(&ws.library_search)
                            .cleanable(true)
                            .prefix(ui::icon(IconName::Search).text_color(z.text_placeholder)),
                        window,
                        cx,
                    )),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(z.text_muted)
                        .child(crate::tr_format!("{count} з {}", ws.library.len())),
                )
                .child(div().flex_1())
                .child(
                    ui::ghost_button("home-new", Some(IconName::Plus), "Новий проєкт")
                        .tooltip(|window, cx| {
                            gpui_component::tooltip::Tooltip::new(crate::i18n::tr(
                                "Порожня пластина з поточними профілями (Ctrl+N)",
                            ))
                            .build(window, cx)
                        })
                        .on_click(cx.listener(|this, _, window, cx| this.new_project(window, cx))),
                )
                .child(
                    ui::ghost_button("home-open", Some(IconName::FolderOpen), "Відкрити модель…")
                        .on_click(cx.listener(|this, _, window, cx| this.prompt_open(window, cx))),
                )
                .when(ws.library_roots.len() == 1, |d| {
                    d.child(
                        div()
                            .text_xs()
                            .text_color(z.text_placeholder)
                            .child(crate::shorten_home(&ws.library_dir)),
                    )
                }),
        )
        .child(
            div()
                .id("home-grid")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p(px(16.))
                .child(div().flex().flex_wrap().gap(px(12.)).children(cards))
                .when(count == 0, |d| {
                    d.child(ui::empty_state(
                        IconName::Search,
                        "Нічого не знайдено",
                        "Спробуй іншу назву або теку.",
                        Vec::new(),
                    ))
                }),
        )
        .into_any_element()
}
