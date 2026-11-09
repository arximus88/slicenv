//! Контекстне меню файлу (правий клік у бібліотеці чи на назві проєкту).
use std::path::{Path, PathBuf};

use std::rc::Rc;

use gpui::{
    AnyElement, ClipboardItem, Context, MouseButton, Pixels, Point, Window, anchored, deferred, div,
    prelude::*, px,
};

use crate::{
    theme::zed,
    ui::{self, IconName},
    widgets::ToastKind,
    workspace::Workspace,
};

/// Що зробити після вдалого збереження (наприклад, закрити файл).
pub type AfterSave = Box<dyn FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>)>;

impl Workspace {
    /// «Закрити файл» (Ctrl+W): з незбереженими змінами — «Зберегти / Не зберігати /
    /// Скасувати»; після закриття — «Головна».
    pub fn close_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.path.is_none() {
            return;
        }
        if !self.edited {
            self.reset_document(cx);
            return;
        }
        let name = self.model_name.clone();
        self.open_modal(
            Rc::new(move |_, _, cx| {
                crate::widgets::modal(
                    "Закрити файл",
                    460.,
                    div()
                        .p(px(14.))
                        .text_sm()
                        .child(crate::tr_format!("Зберегти зміни в «{name}» перед закриттям?", name = name.as_ref())),
                    Some(
                        div()
                            .flex()
                            .gap(px(6.))
                            .child(
                                ui::ghost_button("close-file-cancel", None, "Скасувати")
                                    .on_click(cx.listener(|ws, _, _, cx| ws.close_modal(cx))),
                            )
                            .child(
                                ui::ghost_button("close-file-discard", Some(IconName::Trash), "Не зберігати")
                                    .border_1()
                                    .border_color(zed().border_variant)
                                    .on_click(cx.listener(|ws, _, _, cx| {
                                        ws.close_modal(cx);
                                        ws.reset_document(cx);
                                    })),
                            )
                            .child(
                                ui::accent_button("close-file-save", IconName::Save, "Зберегти", true).on_click(
                                    cx.listener(|ws, _, window, cx| {
                                        ws.close_modal(cx);
                                        ws.prompt_save_then(
                                            Some(Box::new(|ws, _, cx| ws.reset_document(cx))),
                                            window,
                                            cx,
                                        );
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

    /// Порожній документ (демо-кубик, як після запуску) і «Головна».
    pub(crate) fn reset_document(&mut self, cx: &mut Context<Self>) {
        self.finish_scene_gesture();
        self.history.clear();
        self.history_pending = None;
        self.cancel_slice();
        self.jobs.cancel(crate::jobs::JobKind::Open);
        self.project_epoch = self.project_epoch.wrapping_add(1);
        self.project = None;
        self.rebuild_instance_bounds();
        self.sync_model_color();
        self.selected = None;
        self.selected_volume = None;
        self.selected_instances.clear();
        self.collapsed_objects.clear();
        self.cut_enabled = false;
        self.renderer.set_highlight(None);
        self.model_name = "кубик 20 мм".into();
        self.path = None;
        self.untitled = false;
        self.plates = vec![viewer::Plate {
            number: 1,
            name: None,
            mesh: viewer::Mesh::calibration_cube(),
        }];
        self.invalidate_plate_bounds();
        self.error = None;
        self.active_plate = usize::MAX;
        self.show_plate(0);
        self.preview = None;
        self.machine_editor = None;
        self.process_editor = None;
        self.filament_editor = None;
        self.rebuild_editors();
        self.edited = false;
        self.enter_page(crate::workspace::Page::Home, cx);
        cx.notify();
    }

    /// «Новий проєкт» (Ctrl+N): порожня пластина з поточними принтером, процесом і
    /// філаментом, вкладка «Без назви», сторінка «Підготовка». Файлу немає, доки його не
    /// збережуть: `path` — лише запропоноване ім’я в теці бібліотеки для діалогу збереження.
    pub fn new_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !crate::objects::confirm_discard(self, window, cx, |ws, window, cx| {
            ws.new_project(window, cx)
        }) {
            return;
        }
        let mut project = project::Project::default();
        let plates = project
            .add_plate("")
            .and_then(|_| crate::objects::plates_of(&project));
        let plates = match plates {
            Ok(plates) => plates,
            Err(err) => {
                self.toast(ToastKind::Error, format!("{err:#}"), cx);
                return;
            }
        };
        let name = crate::i18n::tr("Без назви");
        let path = untitled_path(&self.library_dir, &name);
        self.install_open_project(Some(project), plates, path, cx);
        self.model_name = name.into();
        self.untitled = true;
        cx.notify();
    }

    pub fn open_file_menu(&mut self, position: Point<Pixels>, path: PathBuf, cx: &mut Context<Self>) {
        self.file_menu = Some((position, path));
        cx.notify();
    }

    fn close_file_menu(&mut self, cx: &mut Context<Self>) {
        if self.file_menu.take().is_some() {
            cx.notify();
        }
    }
}

/// Вільне ім’я для безіменного проєкту: `Без назви.3mf`, `Без назви 2.3mf`… Файл не
/// створюється; ім’я лише не повинно збігатися з наявним (інакше картка в бібліотеці
/// виглядала б відкритою, а діалог збереження пропонував би перезапис).
fn untitled_path(dir: &Path, name: &str) -> PathBuf {
    (1..)
        .map(|n| match n {
            1 => dir.join(format!("{name}.3mf")),
            n => dir.join(format!("{name} {n}.3mf")),
        })
        .find(|p| !p.exists())
        .unwrap_or_else(|| dir.join(format!("{name}.3mf")))
}

/// `file://` URI з відсотковим кодуванням (пробіли, `+`, кирилиця).
#[cfg(not(any(target_os = "macos", windows)))]
fn file_uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_encoded_bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Файл, виділений у Finder (`open -R`).
#[cfg(target_os = "macos")]
pub fn reveal(path: &Path) -> anyhow::Result<()> {
    paths::host::command("open").arg("-R").arg(path).spawn()?;
    Ok(())
}

/// Файл, виділений у Провіднику. Explorer розбирає командний рядок сам: аргумент, який
/// Rust бере в лапки цілком (`"/select,C:\\Мої файли\\a.3mf"`), він не впізнає й відкриває
/// «Документи». Тому рядок передається як є, з лапками лише навколо шляху. `explorer`
/// повертає 1 навіть після успіху, тож код виходу не перевіряємо.
#[cfg(windows)]
pub fn reveal(path: &Path) -> anyhow::Result<()> {
    use std::os::windows::process::CommandExt;
    paths::host::command("explorer").raw_arg(select_arg(path)).spawn()?;
    Ok(())
}

/// `/select,"C:\…"`: абсолютний шлях зі зворотними скісними, без префікса `\\?\`
/// (його explorer не розуміє). Лапок у шляхах Windows бути не може.
#[cfg_attr(not(windows), allow(dead_code))]
fn select_arg(path: &Path) -> String {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    let text = absolute.to_string_lossy().replace('/', "\\");
    let text = text
        .strip_prefix(r"\\?\UNC\")
        .map(|rest| format!(r"\\{rest}"))
        .unwrap_or_else(|| text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned());
    format!("/select,\"{}\"", text.replace('"', ""))
}

/// Тека з виділеним файлом через org.freedesktop.FileManager1 (Nautilus, Dolphin…),
/// інакше просто тека через xdg-open.
#[cfg(not(any(target_os = "macos", windows)))]
pub fn reveal(path: &Path) -> anyhow::Result<()> {
    use std::process::Stdio;
    let shown = paths::host::command("dbus-send")
        .args([
            "--session",
            "--print-reply",
            "--dest=org.freedesktop.FileManager1",
            "--type=method_call",
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1.ShowItems",
        ])
        .arg(format!("array:string:{}", file_uri(path)))
        .arg("string:")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !shown {
        let dir = path.parent().unwrap_or(path);
        paths::host::command("xdg-open")
            .arg(dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
    }
    Ok(())
}

pub fn render(ws: &Workspace, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let (position, path) = ws.file_menu.clone()?;
    let z = zed();
    let item = |id: &'static str, icon: IconName, label: &'static str| {
        ui::ghost_button(id, Some(icon), label).w_full()
    };
    let (open, reveal_path, copy) = (path.clone(), path.clone(), path.clone());
    // Меню вкладки відкритого проєкту (не картки бібліотеки): спершу збереження.
    let current = ws.path.as_deref() == Some(path.as_path());
    // Безіменного файлу на диску ще немає: відкривати, показувати й копіювати нічого.
    let on_disk = !(current && ws.untitled);
    let shortcut = |keys: &'static str| {
        (div().flex_1(), div().text_xs().text_color(z.text_muted).child(keys))
    };
    let menu = div()
        .id("file-menu")
        .occlude()
        .min_w(px(220.))
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
        .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_file_menu(cx)))
        // «Зберегти проєкт» — та сама дія, що Ctrl+S і кнопка в заголовку (`prompt_save`):
        // вона завжди питає шлях, тож окремий «Зберегти як…» був би її копією.
        .when(current && ws.project.is_some(), |menu| {
            let (gap, keys) = shortcut("Ctrl+S");
            menu.child(
                ui::ghost_button("file-menu-save", Some(IconName::Save), "Зберегти проєкт…")
                    .w_full()
                    .child(gap)
                    .child(keys)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.close_file_menu(cx);
                        this.prompt_save(window, cx);
                    })),
            )
            .when(on_disk, |menu| menu.child(div().h(px(1.)).my(px(3.)).bg(z.border_variant)))
        })
        .when(on_disk, |menu| {
            menu.child(item("file-menu-open", IconName::FolderOpen, "Відкрити").on_click(
                cx.listener(move |this, _, window, cx| {
                    this.close_file_menu(cx);
                    this.open(open.clone(), window, cx);
                }),
            ))
            .child(
                item("file-menu-reveal", IconName::Folder, "Відкрити розташування файлу").on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.close_file_menu(cx);
                        if let Err(err) = reveal(&reveal_path) {
                            this.toast(ToastKind::Error, format!("{err:#}"), cx);
                        }
                    }),
                ),
            )
            .child(item("file-menu-copy", IconName::Copy, "Копіювати шлях").on_click(cx.listener(
                move |this, _, _, cx| {
                    this.close_file_menu(cx);
                    cx.write_to_clipboard(ClipboardItem::new_string(copy.display().to_string()));
                },
            )))
        })
        // Лише для відкритого файлу (меню назви проєкту), не для карток бібліотеки.
        .when(current, |menu| {
            let (gap, keys) = shortcut("Ctrl+W");
            menu.child(div().h(px(1.)).my(px(3.)).bg(z.border_variant)).child(
                ui::ghost_button("file-menu-close", Some(IconName::X), "Закрити файл")
                    .w_full()
                    .child(gap)
                    .child(keys)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.close_file_menu(cx);
                        this.close_file(window, cx);
                    })),
            )
        });
    Some(
        deferred(anchored().position(position).snap_to_window().child(menu))
            .with_priority(3)
            .into_any_element(),
    )
}

/// Правий клік відкриває меню файлу.
pub trait FileMenu: gpui::InteractiveElement + Sized {
    fn file_menu(self, path: PathBuf, cx: &Context<Workspace>) -> Self {
        self.on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, e: &gpui::MouseDownEvent, _, cx| {
                cx.stop_propagation();
                this.open_file_menu(e.position, path.clone(), cx);
            }),
        )
    }
}
impl<E: gpui::InteractiveElement> FileMenu for E {}

/// Що відкриває системний діалог: від цього залежить фільтр типів.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenKind {
    /// «Відкрити модель»: `Project::load` (3MF, STL) і готовий G-code для Перегляду (`Workspace::open`).
    Project,
    /// Додати моделі на пластину (`project::read_model`).
    Models,
    /// Збірка: лише STL (`Project::load_stl`).
    Stl,
    /// Виконуваний файл рушія. На Windows — `*.exe`; деінде фільтра немає
    /// (AppImage і бінарники Linux розширення не мають, `.app` на macOS — тека-пакет).
    Engine,
}

/// Розширення, які відкриває `Workspace::open`.
pub const OPEN_EXTENSIONS: &[&str] = &["3mf", "stl", "gcode"];

impl OpenKind {
    fn extensions(self) -> &'static [&'static str] {
        match self {
            Self::Project => OPEN_EXTENSIONS,
            Self::Models => project::IMPORT_EXTENSIONS,
            Self::Stl => &["stl"],
            Self::Engine if cfg!(windows) => &["exe"],
            Self::Engine => &[],
        }
    }

    fn filter_name(self) -> String {
        let patterns = self
            .extensions()
            .iter()
            .map(|e| format!("*.{e}"))
            .collect::<Vec<_>>()
            .join(", ");
        match self {
            Self::Engine => crate::tr_format!("Програми ({patterns})"),
            Self::Project => crate::tr_format!("Підтримувані файли ({patterns})"),
            Self::Models | Self::Stl => crate::tr_format!("Підтримувані моделі ({patterns})"),
        }
    }
}

/// Системний діалог відкриття з фільтром типів і «Усі файли» другим пунктом.
///
/// gpui `PathPromptOptions` фільтрів не має, тому — `rfd`: Windows `IFileOpenDialog`,
/// macOS `NSOpenPanel`, Linux — портал `org.freedesktop.portal.FileChooser`. Діалог прив’язано
/// до вікна (модальний над ним); сам він живе поза потоком UI, тож майбутнє можна чекати в
/// `cx.spawn_in`. Скасування — порожній список.
pub fn prompt_open(
    window: &gpui::Window,
    kind: OpenKind,
    title: &str,
    multiple: bool,
) -> impl std::future::Future<Output = Vec<PathBuf>> + use<> {
    let mut dialog = rfd::AsyncFileDialog::new().set_title(crate::i18n::tr(title));
    if !kind.extensions().is_empty() {
        // Портал (GTK) зіставляє glob із урахуванням регістру: без `*.STL` файл `MODEL.STL`
        // зник би з фільтра. Windows і macOS регістр і так ігнорують.
        let mut globs: Vec<String> = kind.extensions().iter().map(|e| e.to_string()).collect();
        if cfg!(not(any(windows, target_os = "macos"))) {
            globs.extend(kind.extensions().iter().map(|e| e.to_ascii_uppercase()));
        }
        dialog = dialog
            .add_filter(kind.filter_name(), &globs)
            .add_filter(crate::i18n::tr("Усі файли (*.*)"), &["*"]);
    }
    // Без дескриптора (`set_parent` його лише пробує взяти) діалог просто не модальний.
    dialog = dialog.set_parent(window);
    async move {
        if multiple {
            dialog
                .pick_files()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        } else {
            dialog
                .pick_file()
                .await
                .map(|f| f.path().to_path_buf())
                .into_iter()
                .collect()
        }
    }
}

#[cfg(test)]
mod open_filter_tests {
    use super::*;

    #[test]
    fn filters_list_real_extensions() {
        assert_eq!(OpenKind::Project.extensions(), ["3mf", "stl", "gcode"]);
        assert!(OpenKind::Models.extensions().contains(&"obj"));
        assert_eq!(OpenKind::Stl.extensions(), ["stl"]);
        assert_eq!(OpenKind::Engine.extensions().is_empty(), !cfg!(windows));
    }
}

#[cfg(test)]
mod reveal_tests {
    #[cfg(windows)]
    #[test]
    fn explorer_gets_the_path_quoted_after_select() {
        use std::path::Path;
        assert_eq!(
            super::select_arg(Path::new(r"C:\Users\User\Мої моделі\Павук S.3mf")),
            r#"/select,"C:\Users\User\Мої моделі\Павук S.3mf""#
        );
        assert_eq!(
            super::select_arg(Path::new(r"\\?\F:\a b\c.3mf")),
            r#"/select,"F:\a b\c.3mf""#
        );
        assert_eq!(
            super::select_arg(Path::new(r"\\?\UNC\nas\share\c.3mf")),
            r#"/select,"\\nas\share\c.3mf""#
        );
    }
}

#[cfg(test)]
mod untitled_tests {
    #[test]
    fn untitled_name_skips_existing_files() {
        let dir = std::env::temp_dir().join(format!("slicenv-untitled-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(super::untitled_path(&dir, "Без назви"), dir.join("Без назви.3mf"));
        std::fs::write(dir.join("Без назви.3mf"), b"").unwrap();
        assert_eq!(super::untitled_path(&dir, "Без назви"), dir.join("Без назви 2.3mf"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Порожній проєкт з «Нового проєкту» зберігається в 3MF і відкривається з тією самою пластиною.
    #[test]
    fn empty_project_round_trips_through_3mf() {
        let dir = std::env::temp_dir().join(format!("slicenv-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut project = project::Project::default();
        project.add_plate("").unwrap();
        assert_eq!(crate::objects::plates_of(&project).unwrap().len(), 1);
        let path = dir.join("Без назви.3mf");
        project.save_3mf(&path).unwrap();
        let (loaded, plates) = crate::objects::load_project(&path).unwrap();
        assert!(loaded.objects.is_empty());
        assert_eq!(plates.len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
