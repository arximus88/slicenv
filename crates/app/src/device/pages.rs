//! Сторінка принтера: верхні вкладки Статус / Сховище / Оновлення / HMS.
//!
//! «Статус» — компактна шапка поточного завдання (мініатюра, назва, прогрес, пауза/продовження/
//! зупинка) і під нею камера ліворуч та єдина панель стану й керування праворуч. Решта вкладок
//! показує лише те, що принтер справді повідомив; чого хмарний агент не вміє — сказано прямо.

use cloud::{Axis, FanKind, Jog, SdCard};
use gpui::{AnyElement, Window};

use super::job::{self, ThumbState};
use super::*;
use crate::ui::Toggle as _;

const CONTROL_WIDTH: f32 = 340.;

pub(super) fn main_pane(
    ws: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let z = zed();
    let cloud = &ws.cloud;
    let printer = cloud
        .selected
        .as_ref()
        .and_then(|dev| cloud.printers.iter().find(|p| &p.dev_id == dev));
    let title = printer.map_or_else(
        || tr("Пристрої"),
        |p| {
            if p.name.is_empty() {
                model_name(&p.model).into()
            } else {
                p.name.clone()
            }
        },
    );
    let (state, color, live) = cloud.summary();

    let toolbar = ui::toolbar()
        .h(px(40.))
        .px(px(12.))
        .gap(px(10.))
        .border_b_1()
        .border_color(z.border_variant)
        .child(ui::icon(IconName::Printer).text_color(z.text_muted))
        .child(div().font_weight(gpui::FontWeight::MEDIUM).child(title))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_color(z.text_muted)
                .child(cloud.indicator("devices-cloud", color, live))
                .child(state),
        )
        .child(div().flex_1())
        .when(cloud.selected.is_some() && !cloud.link_up(), |d| {
            let lan = cloud.selected.as_ref().is_some_and(|dev| cloud.lan.has(dev));
            d.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .text_xs()
                    .text_color(z.warning)
                    .child(ui::icon(IconName::CloudOff))
                    .child(tr(if lan {
                        "немає зв’язку з принтером у мережі"
                    } else {
                        "немає зв’язку з хмарою"
                    })),
            )
        })
        .children(cloud.updated.map(|t| {
            let secs = t.elapsed().as_secs();
            div()
                .text_xs()
                .text_color(z.text_placeholder)
                .child(crate::tr_format!("оновлено {secs} с тому"))
        }));

    let tabs = cloud.selected.is_some().then(|| {
        let tab = |id: &'static str, icon: IconName, label: &'static str, which: DeviceTab| {
            ui::tab(id, icon, label, cloud.tab == which).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.cloud.tab = which;
                    // Версії модулів приходять лише на запит; запит — читання.
                    if which == DeviceTab::Update && this.cloud.versions.is_empty() {
                        this.cloud.send(CloudCmd::Versions);
                    }
                    cx.notify();
                },
            ))
        };
        let hms_count = cloud.status.as_ref().map_or(0, |s| s.hms.len());
        ui::tab_bar(
            vec![
                tab(
                    "device-tab-status",
                    IconName::Gauge,
                    "Статус",
                    DeviceTab::Status,
                )
                .into_any_element(),
                tab(
                    "device-tab-storage",
                    IconName::Database,
                    "Сховище",
                    DeviceTab::Storage,
                )
                .into_any_element(),
                tab(
                    "device-tab-update",
                    IconName::Download,
                    "Оновлення",
                    DeviceTab::Update,
                )
                .into_any_element(),
                tab(
                    "device-tab-hms",
                    IconName::TriangleAlert,
                    "HMS",
                    DeviceTab::Hms,
                )
                .when(hms_count > 0, |d| {
                    d.child(
                        div()
                            .text_xs()
                            .text_color(z.warning)
                            .child(hms_count.to_string()),
                    )
                })
                .into_any_element(),
            ],
            Vec::new(),
        )
    });

    let body: AnyElement = match (&cloud.status, &cloud.selected) {
        // Принтера ще немає: головний шлях — LAN без акаунта; стан хмари — окремим рядком.
        (_, None) => {
            let first_found = cloud.lan.found.first().cloned();
            let cloud_line = cloud
                .error
                .clone()
                .unwrap_or_else(|| tr(&cloud.phase).into());
            ui::empty_state(
                IconName::Printer,
                "Принтер ще не під’єднано",
                "Найпростіше — локальна мережа: без акаунта, потрібен лише код доступу з екрана принтера. Або пройдіть майстер налаштування — там є й Bambu Cloud.",
                vec![
                    ui::accent_button("device-empty-lan", IconName::Wifi, "Додати принтер у мережі", true)
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.open_lan_dialog(first_found.clone(), window, cx)
                        }))
                        .into_any_element(),
                    ui::ghost_button("device-empty-wizard", Some(IconName::Sparkles), "Майстер налаштування")
                        .border_1()
                        .border_color(z.border_variant)
                        .on_click(cx.listener(|ws, _, window, cx| ws.open_wizard(window, cx)))
                        .into_any_element(),
                ],
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .mt(px(10.))
                    .max_w(px(460.))
                    .text_xs()
                    .text_color(z.text_placeholder)
                    .child(ui::icon(if cloud.error.is_some() { IconName::CloudOff } else { IconName::Cloud }))
                    .child(div().flex_1().min_w_0().child(crate::tr_format!("Bambu Cloud: {cloud_line}"))),
            )
            .into_any_element()
        }
        // LAN-принтер не відповідає: «чекаю на статус» тут вводить в оману.
        (None, Some(dev)) if cloud.lan.has(dev) && cloud.lan.problem.is_some() => {
            let found = cloud
                .lan
                .found
                .iter()
                .find(|f| f.serial == dev.as_str())
                .cloned()
                .unwrap_or_else(|| lan::Found {
                    serial: dev.as_str().to_owned(),
                    ip: String::new(),
                    name: String::new(),
                    model: String::new(),
                    lan_only: false,
                });
            ui::empty_state(
                IconName::WifiOff,
                "Немає зв’язку з принтером у мережі",
                cloud.lan.problem.clone().unwrap_or_default(),
                vec![
                    ui::ghost_button("device-lan-fix", Some(IconName::Pencil), "Змінити IP або код доступу…")
                        .border_1()
                        .border_color(z.border_variant)
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            ws.open_lan_dialog(Some(found.clone()), window, cx)
                        }))
                        .into_any_element(),
                ],
            )
            .into_any_element()
        }
        (None, Some(_)) => ui::empty_state(
            IconName::Printer,
            "Чекаю на статус принтера…",
            "Перший знімок стану приходить за кілька секунд.",
            Vec::new(),
        )
        .into_any_element(),
        (Some(s), Some(_)) => match cloud.tab {
            DeviceTab::Status => status_tab(cloud, s, window, cx),
            DeviceTab::Storage => storage_tab(cloud, s),
            DeviceTab::Update => update_tab(cloud, s, cx),
            DeviceTab::Hms => hms_tab(cloud, s, cx),
        },
    };

    div()
        .flex()
        .flex_col()
        .size_full()
        .min_w_0()
        .bg(z.editor)
        .child(toolbar)
        .children(tabs)
        .when_some(
            // Помилка хмари стосується лише хмарних принтерів; LAN працює без неї.
            cloud
                .error
                .clone()
                .filter(|_| cloud.selected.as_ref().is_some_and(|d| !cloud.lan.has(d))),
            |d, e| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .px(px(12.))
                        .py(px(6.))
                        .bg(z.error.opacity(0.1))
                        .border_b_1()
                        .border_color(z.border_variant)
                        .text_sm()
                        .text_color(z.error)
                        .child(ui::icon(IconName::TriangleAlert))
                        .child(e),
                )
            },
        )
        .child(
            div()
                .id("device-main")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(body),
        )
        .into_any_element()
}

// ---------------------------------------------------------------- Статус

fn status_tab(
    cloud: &CloudState,
    s: &PrinterStatus,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .p(px(12.))
        .child(job_header(cloud, s, cx))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(12.))
                .items_start()
                .child(camera_panel(cloud, cx))
                .child(control_panel(cloud, s, window, cx)),
        )
        .child(history_panel(cloud, cx))
        .into_any_element()
}

/// Компактна шапка: мініатюра, назва завдання, прогрес і кнопки керування друком.
fn job_header(cloud: &CloudState, s: &PrinterStatus, cx: &mut Context<Workspace>) -> gpui::Div {
    let z = zed();
    let state = s.gcode_state.as_deref();
    let active = job::is_active(state);
    let percent = s.mc_percent.unwrap_or(0.0) as f32;
    let name = s
        .subtask_name
        .clone()
        .or_else(|| {
            s.gcode_file
                .as_deref()
                .and_then(|f| f.rsplit('/').next())
                .map(str::to_owned)
        })
        .filter(|n| !n.is_empty());

    let thumb = div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(76.))
        .rounded(px(4.))
        .border_1()
        .border_color(z.border_variant)
        .bg(z.editor)
        .overflow_hidden()
        .map(|d| match (cloud.job.state(), cloud.job.image()) {
            (ThumbState::Ready, Some(image)) => d.child(
                gpui::img(image.clone())
                    .size_full()
                    .object_fit(gpui::ObjectFit::Cover),
            ),
            (ThumbState::Loading, _) => d.child(ui::spinner("job-thumb-spin")),
            (ThumbState::Unavailable(reason), _) => {
                let tip = if reason == "local" {
                    tr("Локальний друк: мініатюри в хмарі немає")
                } else {
                    tr(reason)
                };
                d.child(
                    div()
                        .id("job-thumb-missing")
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                        })
                        .child(
                            ui::icon(IconName::Box)
                                .size(px(24.))
                                .text_color(z.text_placeholder),
                        ),
                )
            }
            _ => d.child(
                ui::icon(IconName::Printer)
                    .size(px(24.))
                    .text_color(z.text_placeholder),
            ),
        });

    let stat = |text: String| {
        div()
            .font_family(crate::theme::MONO_FONT)
            .text_xs()
            .text_color(z.text_muted)
            .child(text)
    };
    let mut stats = Vec::new();
    if let (Some(l), Some(t)) = (s.layer_num, s.total_layer_num)
        && active
    {
        stats.push(stat(format!("{} {l} / {t}", tr("шар"))));
    }
    if let Some(m) = s.mc_remaining_time.filter(|_| active) {
        stats.push(stat(format!(
            "{} {}",
            tr("лишилось"),
            duration_text(m as u64)
        )));
    }

    let printing = state == Some("RUNNING");
    let paused = state == Some("PAUSE");
    let can_stop = matches!(state, Some("RUNNING" | "PAUSE" | "PREPARE" | "SLICING"));
    let actions = div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .when(printing, |d| {
            d.child(
                ui::ghost_button("job-pause", Some(IconName::Pause), "Пауза")
                    .border_1()
                    .border_color(z.border_variant)
                    .on_click(cx.listener(|this, _, _, cx| this.control(Control::Pause, cx))),
            )
        })
        .when(paused, |d| {
            d.child(
                ui::accent_button("job-resume", IconName::Play, "Продовжити", true)
                    .on_click(cx.listener(|this, _, _, cx| this.control(Control::Resume, cx))),
            )
        })
        .when(can_stop, |d| {
            d.child(
                ui::danger_button("job-stop", IconName::SquareStop, "Зупинити…")
                    .on_click(cx.listener(|this, _, window, cx| this.confirm_stop(window, cx))),
            )
        });

    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .p(px(10.))
        .rounded(px(6.))
        .border_1()
        .border_color(z.border_variant)
        .bg(z.panel)
        .child(thumb)
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(6.))
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(px(10.))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_base()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(match (&name, active) {
                                    (Some(n), true) => n.clone(),
                                    (_, true) => tr("Без назви"),
                                    _ => tr("Немає активного завдання"),
                                }),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_sm()
                                .text_color(z.text_muted)
                                .child(state_label(state)),
                        ),
                )
                .when(active, |d| {
                    d.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(div().flex_1().child(crate::widgets::progress(percent)))
                            .child(
                                div()
                                    .flex_none()
                                    .font_family(crate::theme::MONO_FONT)
                                    .text_sm()
                                    .child(format!("{percent:.0}%")),
                            ),
                    )
                    .child(div().flex().gap(px(16.)).children(stats))
                }),
        )
        .child(actions)
}

fn fan_percent(value: Option<u8>) -> String {
    value.map_or_else(|| "—".into(), |v| format!("{v}%"))
}

/// Єдина панель «Стан і керування»: температури, вентилятори, швидкість, світло, рух.
fn control_panel(
    cloud: &CloudState,
    s: &PrinterStatus,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui::Div {
    let z = zed();
    let profile = cloud
        .selected
        .as_ref()
        .and_then(|dev| cloud.printers.iter().find(|p| &p.dev_id == dev))
        .and_then(|p| cloud::Profile::for_model(&p.model));
    let row = |icon: IconName, label: &str, value: AnyElement| {
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .min_h(px(32.))
            .px(px(10.))
            .border_t_1()
            .border_color(z.border_variant)
            .child(ui::icon(icon).text_color(z.text_muted))
            .child(
                div()
                    .w(px(78.))
                    .flex_none()
                    .truncate()
                    .text_sm()
                    .text_color(z.text_muted)
                    .child(tr(label)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .font_family(crate::theme::MONO_FONT)
                    .text_sm()
                    .child(value),
            )
    };
    let text = |t: String| div().child(t).into_any_element();
    // Поточне значення + редагована ціль («220.0 / [ 220 ] °C»).
    let editable = |current: String,
                    input: Option<&gpui::Entity<InputState>>,
                    unit: &'static str,
                    window: &mut Window,
                    cx: &mut Context<Workspace>| {
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(div().min_w(px(44.)).child(current))
            .child(div().text_color(z.text_placeholder).child("→"))
            .children(input.map(|input| {
                div().w(px(62.)).flex_none().child(crate::scrub::field(
                    input,
                    gpui_component::input::Input::new(input),
                    window,
                    cx,
                ))
            }))
            .child(div().text_color(z.text_muted).child(unit))
            .into_any_element()
    };
    let inputs = cloud.inputs.as_ref();
    let cur = |v: Option<f64>| v.map_or_else(|| "—".to_owned(), |t| format!("{t:.1}"));

    let speeds = [
        ("Тихо", SpeedLevel::Silent),
        ("Стандарт", SpeedLevel::Standard),
        ("Спорт", SpeedLevel::Sport),
        ("Шалено", SpeedLevel::Ludicrous),
    ];
    let light_on = s
        .lights
        .iter()
        .any(|l| l.node == "chamber_light" && l.mode == "on");
    let filament = s
        .vt_tray
        .as_ref()
        .and_then(|t| t.tray_type.clone())
        .filter(|t| !t.is_empty());
    let filament_color = s
        .vt_tray
        .as_ref()
        .and_then(|t| t.tray_color.as_deref())
        .and_then(|c| u32::from_str_radix(c.get(..8)?, 16).ok())
        .map(|c| gpui::Hsla::from(gpui::rgba(c)));
    let error = match s.print_error {
        Some(0) | None => div()
            .text_color(z.success)
            .child(tr("немає"))
            .into_any_element(),
        Some(code) => div()
            .text_color(z.error)
            .child(format!("{code:#010X}"))
            .into_any_element(),
    };

    let mut panel = card("Стан і керування", Vec::new())
        .w(px(CONTROL_WIDTH))
        .flex_none()
        .child(
            row(
                IconName::Thermometer,
                "Сопло",
                editable(
                    cur(s.nozzle_temper),
                    inputs.map(|i| &i.nozzle),
                    "°C",
                    window,
                    cx,
                ),
            )
            .border_t_0(),
        )
        .child(row(
            IconName::Bed,
            "Стіл",
            editable(cur(s.bed_temper), inputs.map(|i| &i.bed), "°C", window, cx),
        ))
        .children(
            s.chamber_temper
                .map(|c| row(IconName::Box, "Темп. камери", text(format!("{c:.1} °C")))),
        );

    // Вентилятори: лише ті, що є в моделі; без відомої моделі показуємо тільки значення.
    let fans = [
        (
            FanKind::Part,
            "Обдув деталі",
            s.fans.part,
            inputs.map(|i| &i.part_fan),
        ),
        (
            FanKind::Aux,
            "Додатковий",
            s.fans.aux,
            inputs.map(|i| &i.aux_fan),
        ),
        (
            FanKind::Chamber,
            "Камерний",
            s.fans.chamber,
            inputs.map(|i| &i.chamber_fan),
        ),
    ];
    for (kind, label, value, input) in fans {
        let present = profile.is_some_and(|p| p.has_fan(kind));
        if present {
            panel = panel.child(row(
                IconName::Fan,
                label,
                editable(fan_percent(value), input, "%", window, cx),
            ));
        }
    }

    panel = panel
        .child(
            // Швидкість — на всю ширину панелі: чотири сегменти не вміщуються в колонку значення.
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .min_h(px(36.))
                .px(px(10.))
                .border_t_1()
                .border_color(z.border_variant)
                .child(ui::icon(IconName::Gauge).text_color(z.text_muted))
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .p(px(2.))
                        .gap(px(2.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(z.border_variant)
                        .bg(z.editor)
                        .children(speeds.into_iter().enumerate().map(|(i, (label, level))| {
                            ui::ghost_button(("speed", i), None, label)
                                .flex_1()
                                .justify_center()
                                .h(px(22.))
                                .px(px(4.))
                                .text_xs()
                                .text_color(z.text_muted)
                                .toggled(s.spd_lvl == Some(level as u32))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.control(Control::Speed(level), cx)
                                }))
                        })),
                ),
        )
        .child(row(
            if light_on {
                IconName::Lightbulb
            } else {
                IconName::LightbulbOff
            },
            "Світло",
            ui::ghost_button(
                "light",
                None,
                if light_on {
                    "Увімкнено"
                } else {
                    "Вимкнено"
                },
            )
            .border_1()
            .border_color(z.border_variant)
            .toggled(light_on)
            .w(px(110.))
            .on_click(
                cx.listener(move |this, _, _, cx| this.control(Control::Light(!light_on), cx)),
            )
            .into_any_element(),
        ))
        .child(row(
            IconName::Spool,
            "Філамент",
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .children(filament_color.map(|c| {
                    div()
                        .size(px(10.))
                        .rounded(px(2.))
                        .bg(c)
                        .border_1()
                        .border_color(z.border)
                }))
                .child(filament.unwrap_or_else(|| "—".into()))
                .into_any_element(),
        ))
        .child(row(
            IconName::Wifi,
            "Wi-Fi",
            text(s.wifi_signal.clone().unwrap_or_else(|| "—".into())),
        ))
        .child(row(IconName::TriangleAlert, "Помилка", error))
        .child(jog_section(cloud, cx));
    panel
}

/// Рух головки: крок 1/10 мм, стрілки X/Y, Z, хоумінг. Кнопки сірі, коли рух заборонено, але
/// натискання все одно йде через охоронця (`CloudState::dispatch`), який повертає причину.
fn jog_section(cloud: &CloudState, cx: &mut Context<Workspace>) -> gpui::Div {
    let z = zed();
    let step = if cloud.jog_step == 10 { 10 } else { 1 };
    let blocked = cloud.selected.as_ref().and_then(|dev| {
        super::guard::check(&cloud.snapshot(), dev, Control::Home)
            .err()
            .map(|b| b.message())
    });
    let off = blocked.is_some();
    let button = |id: &'static str, icon: IconName, tip: &'static str| {
        ui::icon_button(id, icon, tip)
            .size(px(30.))
            .border_1()
            .border_color(z.border_variant)
            .inert(off)
    };
    let jog = |axis: Axis, sign: i8| {
        cx.listener(move |this, _, _, cx| {
            let input = sign * if this.cloud.jog_step == 10 { 10 } else { 1 };
            match Jog::new(axis, input) {
                Ok(jog) => this.control(Control::Jog(jog), cx),
                Err(err) => this.toast(ToastKind::Error, err.to_string(), cx),
            }
        })
    };
    let spacer = || div().size(px(30.));
    let pad = div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(
            div()
                .flex()
                .gap(px(2.))
                .child(spacer())
                .child(button("jog-y-plus", IconName::ArrowUp, "Y+").on_click(jog(Axis::Y, 1)))
                .child(spacer()),
        )
        .child(
            div()
                .flex()
                .gap(px(2.))
                .child(button("jog-x-minus", IconName::ArrowLeft, "X−").on_click(jog(Axis::X, -1)))
                .child(
                    button("jog-home", IconName::House, "Хоумінг (G28)").on_click(cx.listener(
                        |this, _, window, cx| {
                            this.confirm_action(
                                "Виконати хоумінг?",
                                "Головка переміститься в домашню позицію. Переконайтеся, що на столі нічого немає.",
                                "Хоумінг",
                                IconName::House,
                                |ws, cx| ws.control(Control::Home, cx),
                                window,
                                cx,
                            )
                        },
                    )),
                )
                .child(button("jog-x-plus", IconName::ArrowRight, "X+").on_click(jog(Axis::X, 1))),
        )
        .child(
            div()
                .flex()
                .gap(px(2.))
                .child(spacer())
                .child(button("jog-y-minus", IconName::ArrowDown, "Y−").on_click(jog(Axis::Y, -1)))
                .child(spacer()),
        );
    // Для Z, як у Studio, «вгору» — від’ємний аргумент (знак для моделі додає cloud).
    let z_col = div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(button("jog-z-up", IconName::ChevronUp, "Z↑").on_click(jog(Axis::Z, -1)))
        .child(spacer())
        .child(button("jog-z-down", IconName::ChevronDown, "Z↓").on_click(jog(Axis::Z, 1)));
    let step_button = |value: i8, label: &'static str| {
        ui::ghost_button(("jog-step", value as usize), None, label)
            .h(px(22.))
            .px(px(6.))
            .text_xs()
            .text_color(z.text_muted)
            .toggled(step == value)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.cloud.jog_step = value;
                cx.notify();
            }))
    };
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .p(px(10.))
        .border_t_1()
        .border_color(z.border_variant)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(ui::icon(IconName::Move).text_color(z.text_muted))
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(z.text_muted)
                        .child(tr("Рух головки")),
                )
                .child(step_button(1, "1 мм"))
                .child(step_button(10, "10 мм")),
        )
        .child(
            div()
                .flex()
                .justify_center()
                .gap(px(18.))
                .child(pad)
                .child(z_col),
        )
        .children(blocked.map(|reason| {
            div()
                .flex()
                .items_start()
                .gap(px(6.))
                .text_xs()
                .text_color(z.text_placeholder)
                .child(ui::icon(IconName::Info))
                .child(div().flex_1().child(reason))
        }))
}

// ---------------------------------------------------------------- Сховище

fn note(text: &'static str) -> gpui::Div {
    let z = zed();
    div()
        .flex()
        .items_start()
        .gap(px(8.))
        .p(px(10.))
        .border_t_1()
        .border_color(z.border_variant)
        .text_sm()
        .text_color(z.text_muted)
        .child(ui::icon(IconName::Info).text_color(z.text_placeholder))
        .child(div().flex_1().min_w_0().child(tr(text)))
}

fn storage_tab(cloud: &CloudState, s: &PrinterStatus) -> AnyElement {
    let z = zed();
    let (sd_text, sd_color) = match s.sd_card() {
        Some(SdCard::Normal) => (tr("Карта є, працює"), z.success),
        Some(SdCard::None) => (tr("Карти немає"), z.warning),
        Some(SdCard::Abnormal) => (tr("Карта є, але стан несправний"), z.error),
        Some(SdCard::ReadOnly) => (tr("Карта лише для читання"), z.warning),
        None => (tr("Принтер не повідомив"), z.text_placeholder),
    };
    let value = |t: String| {
        div()
            .font_family(crate::theme::MONO_FONT)
            .text_sm()
            .child(t)
    };
    let print_kind = match s.print_type.as_deref() {
        Some("cloud") => tr("з хмари"),
        Some("local") => tr("локальний (з SD-карти)"),
        Some("idle") => tr("немає"),
        Some(other) => other.to_owned(),
        None => "—".into(),
    };
    div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .p(px(12.))
        .max_w(px(760.))
        .child(
            card("SD-карта", Vec::new())
                .child(ui::prop_row(
                    "Стан",
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_sm()
                        .child(ui::status_dot(sd_color, false))
                        .child(sd_text),
                ))
                .child(ui::prop_row("Тип друку", value(print_kind)))
                .child(ui::prop_row(
                    "Файл завдання",
                    value(s.gcode_file.clone().unwrap_or_else(|| "—".into())),
                ))
                .child(note(
                    "Перегляд, завантаження й видалення файлів на SD недоступні: Bambu Studio читає карту по FTP у локальній мережі, а хмарний агент цього каналу не дає. Тут показано лише стан карти з телеметрії.",
                )),
        )
        .child(
            card("Хмарна історія", Vec::new())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .p(px(10.))
                        .text_sm()
                        .child(
                            div()
                                .text_color(z.text_muted)
                                .child(tr("Записів в історії друку акаунта")),
                        )
                        .child(value(cloud.tasks.len().to_string())),
                )
                .child(note("Сама історія — внизу вкладки «Статус».")),
        )
        .into_any_element()
}

// ---------------------------------------------------------------- Оновлення

fn update_tab(cloud: &CloudState, s: &PrinterStatus, cx: &mut Context<Workspace>) -> AnyElement {
    let z = zed();
    let mono = |t: String| {
        div()
            .font_family(crate::theme::MONO_FONT)
            .text_sm()
            .truncate()
            .child(t)
    };
    let refresh = ui::icon_button("versions-refresh", IconName::RefreshCw, "Запитати версії")
        .on_click(cx.listener(|this, _, _, _| this.cloud.send(CloudCmd::Versions)));
    let dash = |t: &str| {
        if t.is_empty() {
            "—".to_owned()
        } else {
            t.to_owned()
        }
    };
    let versions = card("Версії модулів", vec![refresh.into_any_element()])
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .h(px(28.))
                .px(px(10.))
                .text_xs()
                .text_color(z.text_placeholder)
                .child(div().w(px(120.)).child(tr("Модуль")))
                .child(div().w(px(150.)).child(tr("ПЗ")))
                .child(div().w(px(110.)).child(tr("Апаратна")))
                .child(div().flex_1().child(tr("Продукт"))),
        )
        .when(cloud.versions.is_empty(), |d| {
            d.child(note(
                "Принтер ще не повідомив версії модулів. Відповідь приходить на запит — натисніть «оновити» вище.",
            ))
        })
        .children(cloud.versions.iter().enumerate().map(|(i, m)| {
            div()
                .id(("module", i))
                .flex()
                .items_center()
                .gap(px(12.))
                .h(px(28.))
                .px(px(10.))
                .border_t_1()
                .border_color(z.border_variant)
                .child(div().w(px(120.)).child(mono(m.name.clone())))
                .child(div().w(px(150.)).child(mono(dash(&m.software))))
                .child(div().w(px(110.)).child(mono(dash(&m.hardware))))
                .child(div().flex_1().min_w_0().child(mono(dash(&m.product))))
        }));

    let upgrade = s.upgrade.as_ref();
    let display = match upgrade.and_then(|u| u.display_state) {
        Some(0) => tr("Оновлень немає"),
        Some(1) => tr("Є нова версія"),
        Some(2) => tr("Оновлення триває"),
        Some(3) => tr("Оновлення завершено"),
        _ => tr("Принтер не повідомив"),
    };
    let state_card = card("Стан оновлення", Vec::new())
        .child(ui::prop_row("Стан", mono(display)))
        .children(upgrade.and_then(|u| {
            u.ota_new_version
                .clone()
                .map(|v| ui::prop_row("Нова версія", mono(v)))
        }))
        .children(upgrade.and_then(|u| {
            u.progress
                .clone()
                .filter(|p| !p.is_empty() && p != "0")
                .map(|p| ui::prop_row("Прогрес", mono(format!("{p}%"))))
        }))
        .children(upgrade.and_then(|u| {
            u.message
                .clone()
                .filter(|m| !m.is_empty())
                .map(|m| ui::prop_row("Повідомлення", mono(m)))
        }))
        .children(upgrade.into_iter().flat_map(|u| {
            u.new_versions.iter().map(|n| {
                ui::prop_row(
                    n.name.clone(),
                    mono(format!(
                        "{} → {}",
                        n.current.clone().unwrap_or_else(|| "—".into()),
                        n.new.clone().unwrap_or_else(|| "—".into())
                    )),
                )
            })
        }))
        .child(note(
            "Встановлення прошивки тут не підтримується: використайте екран принтера або Bambu Studio. Ця вкладка лише показує версії та стан, які повідомив принтер.",
        ));

    div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .p(px(12.))
        .max_w(px(760.))
        .child(state_card)
        .child(versions)
        .into_any_element()
}

// ---------------------------------------------------------------- HMS

fn module_name(id: u8) -> String {
    match id {
        0x03 => "MC".into(),
        0x05 => tr("Головна плата"),
        0x07 => "AMS".into(),
        0x08 => tr("Головка (TH)"),
        0x0C => "XCam".into(),
        other => crate::tr_format!("Модуль {other:02X}"),
    }
}

fn wiki_url(code: &str) -> String {
    format!("https://e.bambulab.com/index.php?e={code}&s=device_hms&lang=en")
}

fn hms_tab(_cloud: &CloudState, s: &PrinterStatus, cx: &mut Context<Workspace>) -> AnyElement {
    let z = zed();
    let wiki = |id: String, url: String| {
        ui::ghost_button(id, Some(IconName::ExternalLink), "Wiki")
            .text_xs()
            .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url)))
    };
    let mono = |t: String| {
        div()
            .font_family(crate::theme::MONO_FONT)
            .text_xs()
            .text_color(z.text_muted)
            .child(t)
    };
    let items = s.hms.iter().enumerate().map(|(i, item)| {
        let (level, color) = match item.level() {
            1 => (tr("Критична"), z.error),
            2 => (tr("Серйозна"), z.error),
            3 => (tr("Звичайна"), z.warning),
            4 => (tr("Інформація"), z.accent),
            _ => (tr("Невідомо"), z.text_placeholder),
        };
        let code = item.long_code();
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .min_h(px(36.))
            .px(px(10.))
            .when(i > 0, |d| d.border_t_1().border_color(z.border_variant))
            .child(ui::status_dot(color, false))
            .child(
                div()
                    .w(px(90.))
                    .flex_none()
                    .text_sm()
                    .text_color(color)
                    .child(level),
            )
            .child(
                div()
                    .w(px(120.))
                    .flex_none()
                    .text_sm()
                    .child(module_name(item.module_id())),
            )
            .child(div().flex_1().min_w_0().child(mono(code.clone())))
            .child(wiki(format!("hms-wiki-{i}"), wiki_url(&code)))
    });

    let hms = card("Повідомлення HMS", Vec::new())
        .children(items)
        .when(s.hms.is_empty(), |d| {
            d.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .p(px(10.))
                    .text_sm()
                    .text_color(z.success)
                    .child(ui::icon(IconName::CircleCheck))
                    .child(tr("Активних повідомлень HMS немає")),
            )
        })
        .child(note(
            "Опис кожного коду відкривається на wiki Bambu Lab у браузері: локальної бази описів у застосунку немає.",
        ));

    let error = match s.print_error {
        Some(code) if code != 0 => {
            let hex = format!("{:08X}", code as u32);
            Some(
                card("Помилка друку", Vec::new()).child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .min_h(px(36.))
                        .px(px(10.))
                        .child(ui::status_dot(z.error, false))
                        .child(div().flex_1().child(mono(hex.clone())))
                        .child(wiki("print-error-wiki".to_owned(), wiki_url(&hex))),
                ),
            )
        }
        _ => None,
    };

    div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .p(px(12.))
        .max_w(px(760.))
        .children(error)
        .child(hms)
        .into_any_element()
}
