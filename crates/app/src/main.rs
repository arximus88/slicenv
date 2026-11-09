// Windows: релізний GUI без консольного вікна. `--version`, `--cloud-probe` і дочірні режими
// пишуть у stdout лише коли він перенаправлений (CI, `slicenv --version | more`).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod support_fins;
mod assembly;
mod assets;
mod bed_look;
mod cut_tool;
mod device;
mod bambu_connect;
mod engine_manager;
mod engines;
mod file_menu;
mod geometry_import;
mod history;
mod home;
mod jobs;
mod i18n;
mod library;
mod migration;
mod navigation;
mod objects;
mod open_plugin;
mod orient;
mod plate_controls;
mod plate_strip;
mod packages;
mod panel_cache;
mod pattern_icons;
mod preferences;
mod mcp;
mod prepare;
mod preset_editor;
mod profile_fetch;
mod preview;
mod photo_mode;
mod setting_help;
mod system_theme;
mod theme;
mod measure_tool;
mod menu;
mod scene_menu;
mod tool_panels;
mod tools;
mod ui;
mod ui_state;
mod viewport;
mod widgets;
mod save_flow;
mod scrub;
mod workspace;
mod wizard;

use std::path::{Path, PathBuf};

use gpui::{
    App, Bounds, TitlebarOptions, WindowBackgroundAppearance, WindowBounds, WindowDecorations,
    WindowOptions, prelude::*, px, size,
};
use gpui_component::Root;
use gpui_platform::application;
use viewer::Renderer;
use workspace::Workspace;

gpui::actions!(
    slicenv,
    [
        OpenPreferences,
        SaveProject,
        SaveProjectAs,
        CloseFile,
        NewProject,
        UndoProject,
        RedoProject,
        OpenFile,
        DeleteSelected,
        DuplicateSelected,
        SelectAll,
        DeleteAll,
        SliceNow,
        RefreshLibrary,
        AutoOrient,
        ToggleLeftDock,
        ToggleRightDock,
        CloseModal,
        GoHome,
        GoPrepare,
        GoPreview,
        GoProject,
        GoDevices,
        MergeAssembly,
        SplitAssembly,
        QuitApplication,
        WizardNext,
        WizardBack
    ]
);

/// Ідентифікатор застосунку: Wayland app_id, .desktop-файл, Windows AppUserModelID.
const APP_ID: &str = "app.borys.slicenv";

fn main() {
    // До міграції й будь-якого доступу до XDG-каталогів: smoke-тест пакетів.
    if matches!(std::env::args().nth(1).as_deref(), Some("--version" | "-V")) {
        println!("slicenv {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    paths::migrate_xdg();
    // `slicenv [тека-з-моделями | файл-моделі]`
    let arg = std::env::args_os().nth(1).map(PathBuf::from);
    if arg.as_deref() == Some(Path::new(device::CLOUD_PROBE_ARG)) {
        unsafe { cloud::configure_tls_env() };
        std::process::exit(device::cloud_probe());
    }
    // Майстер: нік знайденої сесії і вхід токенами — плагін вантажиться лише в дочірньому
    // процесі (його SIGSEGV не зачіпає застосунок).
    if let Some(code) = wizard::child_main(arg.as_deref()) {
        std::process::exit(code);
    }
    // Друк по LAN через відкритий плагін (Open Bamboo Networking) — теж в окремому процесі.
    if let Some(code) = open_plugin::child_main(arg.as_deref()) {
        std::process::exit(code);
    }
    // Тека в аргументах — додатковий корінь бібліотеки на цей сеанс; файл просто відкривається.
    let (library_dir, open_file) = match arg {
        Some(p) if p.is_dir() => (Some(p), None),
        Some(p) => (None, Some(p)),
        None => (None, None),
    };

    // До будь-яких потоків: інакше REST-запити плагіна Bambu не бачать сертифікатів.
    unsafe { cloud::configure_tls_env() };
    // Мова — до потоків хмари й LAN: їхні перші повідомлення вже мають бути перекладені.
    let language = preferences::Preferences::load()
        .unwrap_or_default()
        .language;
    if let Err(e) = i18n::set_language(&language) {
        eprintln!("Language pack: {e:#}");
    }
    let (cloud, cloud_shutdown) = device::spawn_cloud();
    if paths::env("SLICENV_CLOUD_ONLY").is_some() {
        device::debug_drain(cloud);
        return;
    }
    let renderer = match Renderer::new() {
        Ok(r) => r,
        Err(err) => {
            eprintln!("3D-рендерер не стартував: {err:#}");
            drop(cloud_shutdown);
            std::process::exit(1);
        }
    };

    application()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            // Windows: AppUserModelID процесу — той самий, що в ярлику Inno Setup
            // (packaging/windows/slicenv.iss), інакше закріплений ярлик і вікно на панелі
            // завдань не зливаються в одну кнопку. Іконку вікна gpui бере з ресурсу ID 1 (build.rs).
            #[cfg(target_os = "windows")]
            cx.set_app_identity(APP_ID, "Slicenv");
            gpui_component::init(cx);
            theme::apply(cx);
            let prefs = preferences::Preferences::load().unwrap_or_default();
            theme::refresh(prefs.theme, prefs.omarchy_font, cx);
            // Як у Bambu Studio: Ctrl+R — нарізати, Ctrl+S — зберегти, Del — видалити.
            cx.bind_keys([
                gpui::KeyBinding::new("ctrl-,", OpenPreferences, None),
                gpui::KeyBinding::new("ctrl-s", SaveProject, None),
                gpui::KeyBinding::new("ctrl-shift-s", SaveProjectAs, None),
                gpui::KeyBinding::new("ctrl-z", UndoProject, Some("Workspace && !Input")),
                gpui::KeyBinding::new("ctrl-shift-z", RedoProject, Some("Workspace && !Input")),
                gpui::KeyBinding::new("ctrl-y", RedoProject, Some("Workspace && !Input")),
                gpui::KeyBinding::new("ctrl-o", OpenFile, None),
                gpui::KeyBinding::new("ctrl-n", NewProject, None),
                gpui::KeyBinding::new("ctrl-w", CloseFile, Some("Workspace")),
                gpui::KeyBinding::new("ctrl-q", QuitApplication, None),
                gpui::KeyBinding::new("ctrl-1", GoHome, None),
                gpui::KeyBinding::new("ctrl-2", GoPrepare, None),
                gpui::KeyBinding::new("ctrl-3", GoPreview, None),
                gpui::KeyBinding::new("ctrl-4", GoProject, None),
                gpui::KeyBinding::new("ctrl-5", GoDevices, None),
                // Не в текстових полях: Delete там стирає символи, а не вибраний об’єкт.
                gpui::KeyBinding::new("delete", DeleteSelected, Some("Workspace && !Input")),
                gpui::KeyBinding::new("ctrl-d", DuplicateSelected, Some("Workspace && !Input")),
                gpui::KeyBinding::new("ctrl-a", SelectAll, Some("Workspace && !Input")),
                gpui::KeyBinding::new("ctrl-shift-d", DeleteAll, Some("Workspace && !Input")),
                gpui::KeyBinding::new("ctrl-g", MergeAssembly, Some("Workspace")),
                gpui::KeyBinding::new("ctrl-shift-g", SplitAssembly, Some("Workspace")),
                gpui::KeyBinding::new("ctrl-r", SliceNow, None),
                gpui::KeyBinding::new("f5", RefreshLibrary, Some("Workspace")),
                gpui::KeyBinding::new("q", AutoOrient, Some("Workspace && !Input")),
                gpui::KeyBinding::new("ctrl-b", ToggleLeftDock, None),
                gpui::KeyBinding::new("ctrl-alt-b", ToggleRightDock, None),
                gpui::KeyBinding::new("escape", CloseModal, Some("Workspace")),
                gpui::KeyBinding::new("enter", WizardNext, Some("Wizard && !Input")),
                gpui::KeyBinding::new("alt-left", WizardBack, Some("Wizard")),
                gpui::KeyBinding::new("alt-right", WizardNext, Some("Wizard")),
            ]);

            let bounds = Bounds::centered(None, size(px(1560.), px(940.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Slicenv".into()),
                        // Windows: без системного заголовка, як у Zed. Перетягування, подвійний
                        // клік і кнопки вікна дає TitleBar з gpui-component через
                        // WindowControlArea (HTCAPTION/HTMINBUTTON/… у WM_NCHITTEST). На Linux
                        // рамка клієнтська (WindowDecorations::Client), на macOS лишаємо системну.
                        appears_transparent: cfg!(target_os = "windows"),
                        ..Default::default()
                    }),
                    app_id: Some(APP_ID.into()),
                    window_decorations: Some(WindowDecorations::Client),
                    // Клієнтська рамка без gutter (див. Workspace::render) — поверхня непрозора,
                    // тож композитору нема що змішувати по краях.
                    window_background: WindowBackgroundAppearance::Opaque,
                    window_min_size: Some(size(px(960.), px(600.))),
                    ..Default::default()
                },
                |window, cx| {
                    let workspace = cx.new(|cx| {
                        let mut workspace =
                            Workspace::new(renderer, library_dir, cloud, window, cx);
                        if let Some(path) = open_file {
                            workspace.open(path, window, cx);
                        }
                        workspace
                    });
                    cx.new(|cx| Root::new(workspace, window, cx))
                },
            )
            .expect("не відкрилось вікно");
            cx.on_window_closed(|cx, _| cx.quit()).detach();
            cx.activate(true);
        });
    drop(cloud_shutdown);
}

pub fn shorten_home(path: &Path) -> String {
    // `~` — домовленість Unix; на Windows вийшло б `~/AppData\Roaming\…`.
    if cfg!(windows) {
        return path.display().to_string();
    }
    let home = Some(paths::home_dir()).filter(|h| !h.as_os_str().is_empty());
    match home.and_then(|h| path.strip_prefix(h).ok().map(Path::to_path_buf)) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// 1431825 → «1 431 825».
pub fn group_digits(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('\u{202f}');
        }
        out.push(c);
    }
    out
}
