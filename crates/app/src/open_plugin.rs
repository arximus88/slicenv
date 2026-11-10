//! «Відкритий плагін (Open Bamboo Networking)»: вибір користувача на Linux замість закритого
//! плагіна Bambu (`cloud::obn`, docs/OPEN-PLUGIN.md).
//!
//! Одна модалка для налаштувань, майстра й діалогу друку: попередження, два джерела (уже
//! встановлений у OrcaSlicer / Bambu Studio або реліз persano на GitHub), перемикач хмари,
//! вимкнення. Друк по LAN іде в дочірньому процесі (`--obn-local-print`): плагін — сторонній
//! код, його аварія не має валити застосунок, а агент плагіна один на процес.

use std::{
    io::{BufRead as _, Write as _},
    process::{Command, Stdio},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use gpui::{Context, SharedString, Window, div, prelude::*, px};

use crate::{
    i18n::tr,
    theme::zed,
    ui::{self, IconName},
    widgets::{self, ToastKind},
    workspace::Workspace,
};

/// Аргумент дочірнього процесу друку по LAN через відкритий плагін.
pub const LOCAL_PRINT_ARG: &str = "--obn-local-print";

/// Стан модалки (живе у `Workspace`, модалка будується заново щокадру).
#[derive(Default)]
pub struct OpenPluginUi {
    /// Знайдені встановлені OBN; `None` — ще не шукали.
    found: Option<Vec<cloud::obn::Found>>,
    release: Option<Result<cloud::obn::Release, String>>,
    /// «Розумію ризики» — без цього кнопки встановлення неактивні.
    accepted: bool,
    busy: bool,
    progress: Option<f32>,
    cancel: Arc<AtomicBool>,
    error: Option<SharedString>,
}

/// Чи вибрано відкритий плагін (для діалогу друку й LAN).
pub fn active() -> bool {
    cloud::obn::active().is_some()
}

fn shorten(path: &std::path::Path) -> String {
    crate::shorten_home(path)
}

impl Workspace {
    pub fn open_open_plugin(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_plugin.error = None;
        self.open_plugin.accepted = cloud::obn::choice().is_some();
        self.obn_scan(cx);
        self.open_modal(Rc::new(|ws, _window, cx| ws.open_plugin_modal(cx)), window, cx);
    }

    fn obn_scan(&mut self, cx: &mut Context<Self>) {
        self.open_plugin.found = None;
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async { cloud::obn::find_installed() })
                .await;
            this.update(cx, |ws, cx| {
                ws.open_plugin.found = Some(found);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn obn_fetch_release(&mut self, cx: &mut Context<Self>) {
        self.open_plugin.release = None;
        self.open_plugin.error = None;
        self.open_plugin.busy = true;
        cx.spawn(async move |this, cx| {
            let release = cx
                .background_executor()
                .spawn(async { cloud::obn::fetch_release(&cloud::http::Curl).map_err(|e| format!("{e:#}")) })
                .await;
            this.update(cx, |ws, cx| {
                ws.open_plugin.busy = false;
                ws.open_plugin.release = Some(release);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Після будь-якої зміни: хмара підключається знову з новим плагіном.
    fn obn_changed(&mut self, result: Result<String, String>, window: &mut Window, cx: &mut Context<Self>) {
        self.open_plugin.busy = false;
        self.open_plugin.progress = None;
        match result {
            Ok(message) => {
                self.open_plugin.error = None;
                self.cloud.send(crate::device::CloudCmd::Reconnect);
                if self.wizard.is_some() {
                    self.wizard_check_cloud(window, cx);
                }
                self.toast(ToastKind::Success, message, cx);
            }
            Err(e) => self.open_plugin.error = Some(e.into()),
        }
        cx.notify();
    }

    fn obn_use_found(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(found) = self.open_plugin.found.as_ref().and_then(|f| f.get(index)).cloned() else {
            return;
        };
        if !self.open_plugin.accepted || self.open_plugin.busy {
            return;
        }
        self.open_plugin.busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    cloud::obn::install_found(&found)
                        .map(|_| tr("Відкритий плагін підключено"))
                        .map_err(|e| format!("{e:#}"))
                })
                .await;
            this.update_in(cx, |ws, window, cx| ws.obn_changed(result, window, cx)).ok();
        })
        .detach();
        cx.notify();
    }

    fn obn_install_release(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Ok(release)) = self.open_plugin.release.clone() else {
            return;
        };
        if !self.open_plugin.accepted || self.open_plugin.busy {
            return;
        }
        self.open_plugin.busy = true;
        self.open_plugin.progress = Some(0.);
        self.open_plugin.cancel = Default::default();
        let cancel = self.open_plugin.cancel.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<f32>();
        let size = release.size.max(1);
        let worker = std::thread::spawn(move || {
            let mut last = Instant::now() - Duration::from_secs(1);
            cloud::obn::install_release(
                &cloud::http::Curl,
                &release,
                &mut |got| {
                    if last.elapsed() > Duration::from_millis(150) {
                        last = Instant::now();
                        tx.unbounded_send(got as f32 / size as f32).ok();
                    }
                },
                &cancel,
            )
            .map(|_| tr("Відкритий плагін завантажено й підключено"))
            .map_err(|e| format!("{e:#}"))
        });
        cx.spawn_in(window, async move |this, cx| {
            use futures::StreamExt as _;
            while let Some(f) = rx.next().await {
                if this
                    .update(cx, |ws, cx| {
                        ws.open_plugin.progress = Some(f);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
            let result = cx
                .background_executor()
                .spawn(async move { worker.join().unwrap_or_else(|_| Err("panic".into())) })
                .await;
            this.update_in(cx, |ws, window, cx| ws.obn_changed(result, window, cx)).ok();
        })
        .detach();
        cx.notify();
    }

    fn obn_toggle_cloud(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.obn_set_cloud(!cloud::obn::cloud_enabled(), window, cx);
    }

    /// Перемикач хмари відкритого плагіна — лише дією користувача. Вимкнене лишається
    /// вимкненим: перевірка сесії в майстрі його більше не чіпає.
    pub(crate) fn obn_set_cloud(&mut self, enabled: bool, window: &mut Window, cx: &mut Context<Self>) {
        let result = cloud::obn::set_cloud(enabled)
            .map(|_| {
                if enabled {
                    tr("Друк і стан через хмару увімкнено")
                } else {
                    tr("Хмару вимкнено: друк лише по локальній мережі")
                }
            })
            .map_err(|e| format!("{e:#}"));
        self.obn_changed(result, window, cx);
    }

    fn obn_disable(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = cloud::obn::disable()
            .map(|_| tr("Відкритий плагін вимкнено й видалено з теки Slicenv"))
            .map_err(|e| format!("{e:#}"));
        self.open_plugin.accepted = false;
        self.obn_changed(result, window, cx);
    }

    fn open_plugin_modal(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let z = zed();
        let s = &self.open_plugin;
        let choice = cloud::obn::choice();
        let installed = cloud::obn::active().is_some();
        let small = |text: String| div().text_xs().text_color(z.text_muted).child(text);
        let mut body = div().flex().flex_col().gap(px(10.)).p(px(14.)).text_sm().max_w(px(600.));

        // Попередження — завжди першим.
        body = body.child(
            div()
                .flex()
                .gap(px(8.))
                .p(px(10.))
                .rounded(px(6.))
                .border_1()
                .border_color(z.warning)
                .child(ui::icon(IconName::TriangleAlert).flex_none().text_color(z.warning))
                .child(div().flex_1().min_w_0().whitespace_normal().child(tr(cloud::obn::WARNING_TEXT))),
        );
        body = body.child(small(tr(
            "Що він робить: сам отримує ключ слайсера з сервера Bambu, ставить сертифікат на принтер і підписує команду друку. Код — github.com/persano/open-bamboo-networking. У Slicenv плагін не вбудований: ви вибираєте джерело самі.",
        )));

        // Поточний стан.
        if let (true, Some(c)) = (installed, &choice) {
            let source = c.source.label();
            let version = c.release.clone().unwrap_or_else(|| "—".into());
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(ui::icon(IconName::CircleCheck).text_color(z.success))
                    .child(div().flex_1().child(crate::tr_format!(
                        "Підключено: {source}, версія {version}"
                    ))),
            );
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .py(px(4.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(tr("Друк і стан через хмару"))
                            .child(small(tr(
                                "Вимкнено — друк лише в локальній мережі (принтер додається за IP і кодом доступу), до Bambu Cloud плагін не ходить. Увімкнено — ще й друк не з дому; потрібен вхід в акаунт Bambu, і модель вивантажується в хмару Bambu.",
                            ))),
                    )
                    .child(widgets::switch("obn-cloud", c.cloud).on_click(cx.listener(|ws, _, window, cx| ws.obn_toggle_cloud(window, cx)))),
            );
        }

        // Вхід видно й з вимкненою хмарою: свіжий вхід — це явний запит на хмару, після нього
        // вона вмикається сама (з видимим рядком і «Вимкнути» в діалозі Bambu Cloud).
        if installed && !cloud::obn::has_session() {
            body = body.child(
                ui::accent_button("obn-login", IconName::Cloud, "Увійти в Bambu Cloud через відкритий плагін", !s.busy)
                    .when(!s.busy, |d| {
                        d.on_click(cx.listener(|ws, _, window, cx| {
                            ws.close_modal(cx);
                            ws.open_cloud_setup(window, cx)
                        }))
                    }),
            );
        }

        // Згода.
        let accepted = s.accepted;
        body = body.child(
            div()
                .id("obn-accept")
                .flex()
                .items_center()
                .gap(px(8.))
                .cursor_pointer()
                .child(widgets::checkbox("obn-accept-box", accepted, false))
                .child(div().flex_1().whitespace_normal().child(tr("Я розумію ризики й сам вибираю відкритий плагін")))
                .on_click(cx.listener(|ws, _, _, cx| {
                    ws.open_plugin.accepted = !ws.open_plugin.accepted;
                    cx.notify();
                })),
        );
        let enabled = accepted && !s.busy;

        // Джерело (а): уже встановлений.
        body = body.child(div().pt(px(4.)).font_weight(gpui::FontWeight::MEDIUM).child(tr("Уже встановлений на цьому комп’ютері")));
        match &s.found {
            None => body = body.child(div().flex().items_center().gap(px(8.)).child(ui::spinner("obn-scan")).child(small(tr("Шукаю в OrcaSlicer і Bambu Studio…")))),
            Some(list) if list.is_empty() => {
                body = body.child(small(tr(
                    "Не знайдено. Шукаю в Flatpak OrcaSlicer, ~/.config/OrcaSlicer, ~/.config/BambuStudio і плагінах маркетплейсу Orca.",
                )))
            }
            Some(list) => {
                for (i, f) in list.iter().enumerate() {
                    let flavor = match f.flavor {
                        cloud::obn::Flavor::Persano => tr("збірка persano: друк без Developer Mode"),
                        cloud::obn::Flavor::Upstream => tr("збірка ClusterM без ключів: лише з Developer Mode"),
                    };
                    let version = f.release.clone().map(|v| format!(" · {v}")).unwrap_or_default();
                    body = body.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .child(format!("{}{version}", f.source.label()))
                                    .child(small(flavor))
                                    .child(div().text_xs().font_family(crate::theme::MONO_FONT).text_color(z.text_placeholder).min_w_0().truncate().child(shorten(&f.library))),
                            )
                            .child(
                                ui::accent_button(("obn-use", i), IconName::Plug, "Використати", enabled)
                                    .when(enabled, |d| d.on_click(cx.listener(move |ws, _, window, cx| ws.obn_use_found(i, window, cx)))),
                            ),
                    );
                }
            }
        }
        body = body.child(small(tr("Файли копіюються в теку Slicenv; тека OrcaSlicer чи Bambu Studio не змінюється.")));

        // Джерело (б): GitHub.
        body = body.child(div().pt(px(4.)).font_weight(gpui::FontWeight::MEDIUM).child(tr("Завантажити реліз persano з GitHub")));
        match &s.release {
            None if s.busy && s.progress.is_none() => {
                body = body.child(div().flex().items_center().gap(px(8.)).child(ui::spinner("obn-release")).child(small(tr("Запитую GitHub…"))));
            }
            None => {
                body = body.child(
                    ui::ghost_button("obn-check-release", Some(IconName::Search), "Перевірити останній реліз")
                        .border_1()
                        .border_color(z.border_variant)
                        .on_click(cx.listener(|ws, _, _, cx| ws.obn_fetch_release(cx))),
                );
            }
            Some(Err(e)) => {
                body = body.child(small(crate::tr_format!("Не вдалося отримати реліз: {e}")));
            }
            Some(Ok(r)) => {
                let mb = r.size as f64 / 1048576.;
                body = body
                    .child(crate::tr_format!("Версія {v} · {mb:.1} МБ", v = r.version))
                    .child(div().text_xs().font_family(crate::theme::MONO_FONT).text_color(z.text_muted).whitespace_normal().child(r.url.clone()))
                    .child(div().text_xs().font_family(crate::theme::MONO_FONT).text_color(z.text_muted).whitespace_normal().child(format!("SHA-256 {}", r.sha256)));
                if let Some(p) = s.progress {
                    body = body.child(widgets::progress((p * 100.).max(2.))).child(
                        ui::ghost_button("obn-cancel", Some(IconName::X), "Скасувати").on_click(cx.listener(|ws, _, _, cx| {
                            ws.open_plugin.cancel.store(true, Ordering::Release);
                            cx.notify();
                        })),
                    );
                } else {
                    let page = r.page.clone();
                    body = body.child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(
                                ui::accent_button("obn-download", IconName::Download, "Погоджуюсь — завантажити", enabled)
                                    .when(enabled, |d| d.on_click(cx.listener(|ws, _, window, cx| ws.obn_install_release(window, cx)))),
                            )
                            .child(
                                ui::ghost_button("obn-release-page", Some(IconName::ExternalLink), "Сторінка релізу")
                                    .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&page))),
                            ),
                    );
                }
            }
        }
        body = body.child(small(tr("Розмір і SHA-256 звіряються з опублікованими в релізі, а кожна бібліотека — з RECORD архіву. Нічого з архіву не запускається.")));

        if let Some(e) = &s.error {
            body = body.child(div().text_color(z.error).whitespace_normal().child(e.clone()));
        }

        let footer = div()
            .flex()
            .gap(px(6.))
            .when(installed, |d| {
                d.child(
                    ui::ghost_button("obn-disable", Some(IconName::Trash), "Вимкнути відкритий плагін")
                        .border_1()
                        .border_color(z.border_variant)
                        .on_click(cx.listener(|ws, _, window, cx| ws.obn_disable(window, cx))),
                )
            })
            .child(ui::ghost_button("obn-close", Some(IconName::Check), "Готово").on_click(cx.listener(|ws, _, _, cx| ws.close_modal(cx))));
        widgets::modal(
            "Відкритий плагін (Open Bamboo Networking)",
            640.,
            div().id("obn-body").max_h(px(560.)).overflow_y_scroll().child(body),
            Some(footer.into_any_element()),
        )
        .into_any_element()
    }
}

// ---------------------------------------------------------------------------------------------
// Друк по LAN у дочірньому процесі

/// Тіло дочірнього процесу. Повертає код виходу.
pub fn child_main(arg: Option<&std::path::Path>) -> Option<i32> {
    if arg? != std::path::Path::new(LOCAL_PRINT_ARG) {
        return None;
    }
    unsafe { cloud::configure_tls_env() };
    Some(run_child())
}

#[cfg(unix)]
fn run_child() -> i32 {
    use std::io::Read as _;
    let Ok(out) = cloud::obn::private_stdout() else {
        return 2;
    };
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return 2;
    }
    let Ok(job) = serde_json::from_str::<cloud::obn::LocalJob>(&input) else {
        return 2;
    };
    drop(input);
    let report = Arc::new(std::sync::Mutex::new(out));
    let progress = {
        let report = report.clone();
        Box::new(move |stage: i32, code: i32| {
            if let Ok(mut out) = report.lock() {
                let _ = writeln!(out, "stage {stage} {code}");
            }
        })
    };
    let result = cloud::obn::run_local_print(&job, progress);
    let mut out = match report.lock() {
        Ok(out) => out,
        Err(_) => return 2,
    };
    match result {
        Ok(()) => {
            let _ = writeln!(out, "ok");
            0
        }
        Err(e) => {
            match cloud::PluginError::code_of(&e) {
                Some(code) => {
                    let _ = writeln!(out, "code {code}");
                }
                // Наші перевірки (не текст плагіна) — без секретів.
                None => {
                    let _ = writeln!(out, "error {}", format!("{e:#}").replace('\n', " "));
                }
            }
            3
        }
    }
}

#[cfg(not(unix))]
fn run_child() -> i32 {
    2
}

/// Батьківський бік: запускає дочірній процес, передає завдання через stdin, читає стадії.
/// `upload(частка)` — для смужки прогресу. Скасування — завершенням процесу.
pub fn local_print(
    job: &cloud::obn::LocalJob,
    upload: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<(), String> {
    let input = serde_json::to_string(job).map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = Command::new(exe)
        .arg(LOCAL_PRINT_ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Some(mut pipe) = child.stdin.take() {
        pipe.write_all(input.as_bytes()).map_err(|e| e.to_string())?;
    }
    drop(input);
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(20 * 60);
    let mut last: Option<String> = None;
    let status = loop {
        while let Ok(line) = rx.try_recv() {
            let mut parts = line.split_whitespace();
            if parts.next() == Some("stage") {
                let stage: i32 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(-1);
                let code: i32 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                // PrintingStageUpload = 1: code — відсотки.
                if stage == 1 {
                    upload((code.clamp(0, 100) as f32) / 100.);
                }
            } else {
                last = Some(line);
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => return Err(e.to_string()),
        }
        if cancel.load(Ordering::Acquire) || Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(tr("Друк скасовано"));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    // Дочитати рядки, що прийшли разом із виходом.
    std::thread::sleep(Duration::from_millis(50));
    while let Ok(line) = rx.try_recv() {
        if !line.starts_with("stage ") {
            last = Some(line);
        }
    }
    if let Some(signal) = paths::host::exit_signal(&status) {
        return Err(crate::tr_format!(
            "Відкритий плагін аварійно завершився (сигнал {signal}); застосунок працює далі"
        ));
    }
    match last.as_deref() {
        Some("ok") if status.success() => Ok(()),
        Some(line) if line.starts_with("code ") => {
            let code: i32 = line[5..].trim().parse().unwrap_or(0);
            Err(format!("{} ({code})", tr(cloud::obn::local_print_error(code))))
        }
        Some(line) if line.starts_with("error ") => Err(line[6..].to_owned()),
        _ => Err(tr("Відкритий плагін не надіслав друк")),
    }
}
