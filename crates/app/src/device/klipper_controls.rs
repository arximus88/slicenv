//! Панель «Керування» принтера Klipper на сторінці «Пристрій»: температури з пресетами,
//! вентилятор обдуву, Home і джог з явними осями, консоль G-code з історією, аварійна
//! зупинка. Кожна команда проходить `KlipperUi::allow` тут і `command_block` у робітнику.
use gpui::{AnyElement, Context, Focusable as _, Window, div, prelude::*, px};
use gpui_component::input::InputState;
use moonraker::{Axis, Command, HeaterKind, Home, command::JOG_STEPS};

use super::{CloudState, card, klipper::KlipperCmd};
use crate::{
    i18n::tr,
    theme::zed,
    ui::{self, IconName, Toggle as _},
    widgets::ToastKind,
    workspace::Workspace,
};

const WIDTH: f32 = 380.;
const HISTORY: usize = 30;

pub struct ControlInputs {
    pub nozzle: gpui::Entity<InputState>,
    pub bed: gpui::Entity<InputState>,
    pub fan: gpui::Entity<InputState>,
    pub console: gpui::Entity<InputState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Nozzle,
    Bed,
    Fan,
    Console,
}

/// Рядок консолі; `result: None` — ще виконується.
#[derive(Clone, Debug, PartialEq)]
pub struct ConsoleLine {
    pub command: String,
    pub result: Option<Result<(), String>>,
}

/// Результат команди в історію: закриває найновіший незавершений рядок з тим самим текстом.
pub fn record(history: &mut Vec<ConsoleLine>, command: String, result: Result<(), String>) {
    if let Some(line) = history
        .iter_mut()
        .rev()
        .find(|l| l.command == command && l.result.is_none())
    {
        line.result = Some(result);
    } else {
        push(history, ConsoleLine { command, result: Some(result) });
    }
}

fn push(history: &mut Vec<ConsoleLine>, line: ConsoleLine) {
    history.push(line);
    if history.len() > HISTORY {
        let extra = history.len() - HISTORY;
        history.drain(..extra);
    }
}

/// Температури з профілю філаменту.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilamentTemps {
    /// `filament_type`, напр. `PLA`.
    pub kind: String,
    pub nozzle: Option<u16>,
    pub bed: Option<u16>,
}

/// Значення з розгорнутого (`flatten`) профілю філаменту. Стіл — `hot_plate_temp`
/// (гладкий/текстурований PEI — типовий для Klipper-принтерів), далі інші типи пластин.
pub fn filament_temps(map: &serde_json::Map<String, serde_json::Value>) -> Option<FilamentTemps> {
    let first = |key: &str| -> Option<String> {
        match map.get(key)? {
            serde_json::Value::Array(a) => a.first()?.as_str().map(str::to_owned),
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        }
    };
    let temp = |key: &str| first(key).and_then(|v| v.trim().parse::<f64>().ok()).filter(|t| *t > 0.0 && *t < 500.0).map(|t| t.round() as u16);
    let kind = first("filament_type").unwrap_or_default().trim().to_owned();
    let nozzle = temp("nozzle_temperature");
    let bed = ["hot_plate_temp", "textured_plate_temp", "eng_plate_temp", "cool_plate_temp"]
        .into_iter()
        .find_map(temp);
    (nozzle.is_some() || bed.is_some()).then_some(FilamentTemps { kind, nozzle, bed })
}

#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub label: String,
    pub nozzle: u16,
    pub bed: u16,
    /// Значення взято з активного філаменту.
    pub from_profile: bool,
}

/// Пресети PLA / PETG / ABS (типові значення), де значення матеріалу активного філаменту
/// беруться з його профілю; філамент іншого типу додається окремою кнопкою.
pub fn presets(active: Option<&FilamentTemps>) -> Vec<Preset> {
    let mut list: Vec<Preset> = [("PLA", 210, 60), ("PETG", 240, 80), ("ABS", 250, 100)]
        .into_iter()
        .map(|(label, nozzle, bed)| Preset {
            label: label.into(),
            nozzle,
            bed,
            from_profile: false,
        })
        .collect();
    if let Some(f) = active {
        let own = |p: &Preset| Preset {
            label: p.label.clone(),
            nozzle: f.nozzle.unwrap_or(p.nozzle),
            bed: f.bed.unwrap_or(p.bed),
            from_profile: true,
        };
        match list.iter().position(|p| p.label.eq_ignore_ascii_case(&f.kind)) {
            Some(i) => list[i] = own(&list[i]),
            None if f.nozzle.is_some() && !f.kind.is_empty() => list.push(Preset {
                label: f.kind.clone(),
                nozzle: f.nozzle.unwrap_or_default(),
                bed: f.bed.unwrap_or_default(),
                from_profile: true,
            }),
            None => {}
        }
    }
    list
}

fn step_label(step: f64) -> String {
    if step < 1.0 { format!("{step:.1}") } else { format!("{step:.0}") }
}

impl Workspace {
    /// Поля панелі керування; викликається з `start_klipper`.
    pub(super) fn start_klipper_controls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut field = |kind: Field, placeholder: &str, this: &mut Self| {
            let entity = cx.new(|cx| InputState::new(window, cx).placeholder(tr(placeholder)));
            let sub = cx.subscribe_in(
                &entity,
                window,
                move |this, _, event: &gpui_component::input::InputEvent, window, cx| {
                    if matches!(event, gpui_component::input::InputEvent::PressEnter { .. }) {
                        this.klipper_apply(kind, window, cx);
                    }
                },
            );
            this._subscriptions.push(sub);
            entity
        };
        let inputs = ControlInputs {
            nozzle: field(Field::Nozzle, "°C", self),
            bed: field(Field::Bed, "°C", self),
            fan: field(Field::Fan, "%", self),
            console: field(Field::Console, "G-code, напр. QUERY_ENDSTOPS (Enter — надіслати)", self),
        };
        self.cloud.klipper.controls = Some(inputs);
        self.cloud.klipper.jog_step = 1;
    }

    /// Пресети температур — з активного філаменту; перераховується лише при зміні філаменту.
    pub(super) fn refresh_klipper_filament(&mut self) {
        let name = self.selection.filament.clone();
        if self.cloud.klipper.filament.as_ref().is_some_and(|(n, _)| *n == name) {
            return;
        }
        let temps = self
            .presets
            .as_ref()
            .and_then(|p| p.flatten(slicer::Kind::Filament, &name).ok())
            .and_then(|map| filament_temps(&map));
        self.cloud.klipper.filament = Some((name, temps));
    }

    /// Цілі температур і вентилятора в поля, якщо користувач їх зараз не редагує.
    pub(super) fn sync_klipper_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let k = &self.cloud.klipper;
        let (Some(inputs), Some(dev)) = (&k.controls, self.cloud.selected.as_ref()) else {
            return;
        };
        let Some(job) = k.job_for(dev) else { return };
        let temp = |v: Option<f64>| v.map(|t| format!("{t:.0}")).unwrap_or_default();
        let pairs = [
            (inputs.nozzle.clone(), temp(job.extruder.target)),
            (inputs.bed.clone(), temp(job.bed.target)),
            (
                inputs.fan.clone(),
                job.part_fan.map(|f| format!("{:.0}", f * 100.0)).unwrap_or_default(),
            ),
        ];
        for (entity, text) in pairs {
            if entity.focus_handle(cx).is_focused(window) || entity.read(cx).value() == text {
                continue;
            }
            entity.update(cx, |s, cx| s.set_value(text, window, cx));
        }
    }

    /// Надсилає команди вибраному принтеру Klipper, якщо охоронець дозволяє.
    pub(super) fn klipper_run(
        &mut self,
        commands: Vec<Command>,
        console: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(dev) = self.cloud.selected.clone() else { return };
        let k = &mut self.cloud.klipper;
        if let Some(text) = commands.iter().find_map(|c| k.allow(&dev, c).err()) {
            self.toast(ToastKind::Error, text, cx);
            return;
        }
        if let Some(line) = &console {
            push(&mut k.history, ConsoleLine { command: line.clone(), result: None });
        }
        k.running += 1;
        k.send(KlipperCmd::Run { dev, commands, console });
        cx.notify();
    }

    fn klipper_apply(&mut self, field: Field, window: &mut Window, cx: &mut Context<Self>) {
        let Some(inputs) = &self.cloud.klipper.controls else { return };
        let entity = match field {
            Field::Nozzle => inputs.nozzle.clone(),
            Field::Bed => inputs.bed.clone(),
            Field::Fan => inputs.fan.clone(),
            Field::Console => inputs.console.clone(),
        };
        let text = entity.read(cx).value().trim().to_owned();
        if field == Field::Console {
            if text.is_empty() {
                return;
            }
            entity.update(cx, |s, cx| s.set_value("", window, cx));
            self.klipper_run(vec![Command::Raw(text.clone())], Some(text), cx);
            return;
        }
        let Ok(value) = text.parse::<u16>() else {
            self.toast(ToastKind::Error, tr("Введіть ціле невід’ємне число"), cx);
            return;
        };
        let command = match field {
            Field::Nozzle => Command::Heater(HeaterKind::Nozzle, value),
            Field::Bed => Command::Heater(HeaterKind::Bed, value),
            _ => match u8::try_from(value).ok().filter(|v| *v <= 100) {
                Some(v) => Command::PartFan(v),
                None => {
                    self.toast(ToastKind::Error, tr("Вентилятор: 0–100 %"), cx);
                    return;
                }
            },
        };
        self.klipper_heat(vec![command], window, cx);
    }

    /// Температури: вимкнення нагріву посеред друку — лише після підтвердження.
    fn klipper_heat(&mut self, commands: Vec<Command>, window: &mut Window, cx: &mut Context<Self>) {
        let printing = self
            .cloud
            .selected
            .as_ref()
            .and_then(|d| self.cloud.klipper.job_for(d))
            .is_some_and(|j| j.state.is_active());
        let turns_off = commands.iter().any(|c| matches!(c, Command::Heater(_, 0)));
        if printing && turns_off {
            self.confirm_action(
                "Вимкнути нагрів під час друку?",
                "Друк зупиниться з помилкою, коли температура впаде нижче потрібної.",
                "Вимкнути",
                IconName::Thermometer,
                move |ws, cx| ws.klipper_run(commands.clone(), None, cx),
                window,
                cx,
            );
        } else {
            self.klipper_run(commands, None, cx);
        }
    }

    fn confirm_klipper_home(&mut self, home: Home, window: &mut Window, cx: &mut Context<Self>) {
        // Заблоковано (друк, не готовий) — пояснення одразу, без зайвого підтвердження.
        if let Some(dev) = self.cloud.selected.clone()
            && let Err(text) = self.cloud.klipper.allow(&dev, &Command::Home(home))
        {
            self.toast(ToastKind::Error, text, cx);
            return;
        }
        let (title, body) = match home {
            Home::All => (
                "Home усіх осей (G28)?",
                "Головка й стіл поїдуть до кінцевиків по X, Y і Z. Переконайтеся, що на столі нічого не заважає.",
            ),
            Home::Xy => (
                "Home осей X і Y (G28 X Y)?",
                "Головка поїде до кінцевиків X і Y. Переконайтеся, що на шляху нічого немає.",
            ),
            Home::Z => (
                "Home осі Z (G28 Z)?",
                "Сопло опуститься до столу (датчик чи кінцевик Z). Переконайтеся, що на столі нічого немає.",
            ),
        };
        self.confirm_action(
            title,
            body,
            "Home",
            IconName::House,
            move |ws, cx| ws.klipper_run(vec![Command::Home(home)], None, cx),
            window,
            cx,
        );
    }

    pub(super) fn confirm_klipper_estop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_action(
            "Аварійна зупинка?",
            "Klipper негайно вимкне нагрів і мотори (M112) і перейде в shutdown. Друк, якщо йде, буде втрачено. Повернути принтер до роботи — «Перезапустити прошивку».",
            "Зупинити негайно",
            IconName::OctagonAlert,
            |ws, cx| {
                if let Some(dev) = ws.cloud.selected.clone() {
                    ws.cloud.klipper.running += 1;
                    ws.cloud.klipper.send(KlipperCmd::EmergencyStop(dev));
                }
                cx.notify();
            },
            window,
            cx,
        );
    }

    pub(super) fn confirm_firmware_restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_action(
            "Перезапустити прошивку?",
            "Klipper перезапустить MCU (FIRMWARE_RESTART). Нагрів і мотори лишаться вимкненими, осі доведеться знову виконати Home.",
            "Перезапустити",
            IconName::RefreshCw,
            |ws, cx| {
                if let Some(dev) = ws.cloud.selected.clone() {
                    ws.cloud.klipper.running += 1;
                    ws.cloud.klipper.send(KlipperCmd::FirmwareRestart(dev));
                }
                cx.notify();
            },
            window,
            cx,
        );
    }
}

/// Червона кнопка аварійної зупинки (з підтвердженням).
pub fn estop_button(cx: &mut Context<Workspace>) -> AnyElement {
    let z = zed();
    ui::danger_button("klipper-estop", IconName::OctagonAlert, "Аварійна зупинка")
        .bg(z.error.opacity(0.12))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .on_click(cx.listener(|ws, _, window, cx| ws.confirm_klipper_estop(window, cx)))
        .into_any_element()
}

fn note(text: String, color: gpui::Hsla) -> gpui::Div {
    div()
        .flex()
        .items_start()
        .gap(px(6.))
        .px(px(10.))
        .py(px(4.))
        .text_xs()
        .text_color(color)
        .child(ui::icon(IconName::Info).flex_none())
        .child(div().flex_1().min_w_0().whitespace_normal().child(text))
}

fn section(icon: IconName, title: &str) -> gpui::Div {
    let z = zed();
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(10.))
        .pt(px(8.))
        .pb(px(4.))
        .border_t_1()
        .border_color(z.border_variant)
        .child(ui::icon(icon).text_color(z.text_muted))
        .child(
            div()
                .flex_1()
                .text_sm()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(z.text_muted)
                .child(tr(title)),
        )
}

/// Панель керування вибраного принтера Klipper.
pub fn panel(cloud: &CloudState, window: &mut Window, cx: &mut Context<Workspace>) -> gpui::Div {
    let z = zed();
    let k = &cloud.klipper;
    let Some(dev) = cloud.selected.clone() else { return div() };
    let job = k.job_for(&dev).cloned();
    let inputs = k.controls.as_ref();
    let field = |entity: Option<&gpui::Entity<InputState>>, w: f32, window: &mut Window, cx: &mut Context<Workspace>| {
        entity.map(|e| {
            div()
                .w(px(w))
                .flex_none()
                .child(crate::widgets::field(e, gpui_component::input::Input::new(e), window, cx))
        })
    };
    let cur = |v: Option<f64>| v.map_or_else(|| "—".to_owned(), |t| format!("{t:.1}"));
    // Чому заблоковано: температури — за `Heater`, рух — за `Home`.
    let heat_block = k.allow(&dev, &Command::Heater(HeaterKind::Nozzle, 0)).err();
    let move_block = k.allow(&dev, &Command::Home(Home::All)).err();

    let temp_row = |icon: IconName,
                    label: &str,
                    current: String,
                    entity: Option<&gpui::Entity<InputState>>,
                    unit: &'static str,
                    window: &mut Window,
                    cx: &mut Context<Workspace>| {
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .min_h(px(32.))
            .px(px(10.))
            .child(ui::icon(icon).text_color(z.text_muted))
            .child(div().w(px(110.)).flex_none().truncate().text_sm().text_color(z.text_muted).child(tr(label)))
            .child(div().min_w(px(48.)).font_family(crate::theme::MONO_FONT).text_sm().child(current))
            .child(div().text_color(z.text_placeholder).child("→"))
            .children(field(entity, 62., window, cx))
            .child(div().text_sm().text_color(z.text_muted).child(unit))
    };

    let nozzle_row = temp_row(
        IconName::Thermometer,
        "Сопло",
        cur(job.as_ref().and_then(|j| j.extruder.temperature)),
        inputs.map(|i| &i.nozzle),
        "°C",
        window,
        cx,
    );
    let bed_row = temp_row(
        IconName::Bed,
        "Стіл",
        cur(job.as_ref().and_then(|j| j.bed.temperature)),
        inputs.map(|i| &i.bed),
        "°C",
        window,
        cx,
    );
    let fan_row = temp_row(
        IconName::Fan,
        "Обдув деталі",
        job.as_ref()
            .and_then(|j| j.part_fan)
            .map_or_else(|| "—".to_owned(), |f| format!("{:.0}%", f * 100.0)),
        inputs.map(|i| &i.fan),
        "%",
        window,
        cx,
    );
    let console_input = inputs.map(|i| {
        div()
            .flex_1()
            .min_w_0()
            .child(crate::widgets::field(&i.console, gpui_component::input::Input::new(&i.console), window, cx))
    });
    let estop = estop_button(cx);

    let active = k.filament.as_ref().and_then(|(_, t)| t.as_ref());
    let preset_buttons: Vec<_> = presets(active).into_iter().enumerate().map(|(i, p)| {
        let commands = vec![
            Command::Heater(HeaterKind::Nozzle, p.nozzle),
            Command::Heater(HeaterKind::Bed, p.bed),
        ];
        ui::ghost_button_bare(("klipper-preset", i), if p.from_profile { IconName::Spool } else { IconName::Flame })
            .border_1()
            .border_color(z.border_variant)
            .text_xs()
            .child(format!("{} {}/{}", p.label, p.nozzle, p.bed))
            .inert(heat_block.is_some())
            .on_click(cx.listener(move |ws, _, window, cx| ws.klipper_heat(commands.clone(), window, cx)))
    }).collect();
    let off = ui::ghost_button("klipper-preset-off", Some(IconName::Snowflake), "Вимкнути")
        .border_1()
        .border_color(z.border_variant)
        .text_xs()
        .inert(heat_block.is_some())
        .on_click(cx.listener(|ws, _, window, cx| {
            ws.klipper_heat(
                vec![Command::Heater(HeaterKind::Nozzle, 0), Command::Heater(HeaterKind::Bed, 0)],
                window,
                cx,
            )
        }));
    let profile_note = active.map(|f| {
        crate::tr_format!(
            "Пресет із значком котушки — з активного філаменту ({kind})",
            kind = if f.kind.is_empty() { "—".to_owned() } else { f.kind.clone() }
        )
    });

    // ---- рух
    let step = JOG_STEPS[k.jog_step.min(JOG_STEPS.len() - 1)];
    let step_button = |i: usize| {
        ui::ghost_button_bare(("klipper-step", i), IconName::Ruler)
            .h(px(22.))
            .px(px(6.))
            .text_xs()
            .text_color(z.text_muted)
            .toggled(k.jog_step == i)
            .child(crate::tr_format!("{step} мм", step = step_label(JOG_STEPS[i])))
            .on_click(cx.listener(move |ws, _, _, cx| {
                ws.cloud.klipper.jog_step = i;
                cx.notify();
            }))
    };
    let home_button = |id: &'static str, label: &'static str, home: Home| {
        ui::ghost_button(id, Some(IconName::House), label)
            .border_1()
            .border_color(z.border_variant)
            .text_xs()
            .inert(move_block.is_some())
            .on_click(cx.listener(move |ws, _, window, cx| ws.confirm_klipper_home(home, window, cx)))
    };
    let axis_row = |axis: Axis, index: usize| {
        let homed = job.as_ref().is_some_and(|j| j.homed(axis));
        let position = job
            .as_ref()
            .and_then(|j| j.position)
            .map_or_else(|| "—".to_owned(), |p| format!("{:.2}", p[index]));
        let blocked = k.allow(&dev, &Command::Jog(axis, step)).is_err();
        let letter = axis.letter();
        let jog = |sign: f64, id: usize| {
            let text = format!("{letter}{}{}", if sign < 0.0 { "−" } else { "+" }, step_label(step));
            ui::ghost_button_bare(("klipper-jog", index * 2 + id), if sign < 0.0 { IconName::Minus } else { IconName::Plus })
                .w(px(84.))
                .justify_center()
                .border_1()
                .border_color(z.border_variant)
                .font_family(crate::theme::MONO_FONT)
                .text_xs()
                .child(text)
                .inert(blocked)
                .on_click(cx.listener(move |ws, _, _, cx| {
                    ws.klipper_run(vec![Command::Jog(axis, sign * step)], None, cx)
                }))
        };
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(30.))
            .px(px(10.))
            .child(
                div()
                    .w(px(18.))
                    .flex_none()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(letter.to_string()),
            )
            .child(ui::status_dot(if homed { z.success } else { z.text_placeholder }, false))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(crate::theme::MONO_FONT)
                    .text_xs()
                    .text_color(z.text_muted)
                    .child(if homed { position } else { tr("не вдома") }),
            )
            .child(jog(-1.0, 0))
            .child(jog(1.0, 1))
    };

    // ---- консоль
    let console_block = k.allow(&dev, &Command::Raw("M117".into())).err();
    let history: Vec<_> = k.history.iter().rev().take(6).enumerate().map(|(i, line)| {
        let (icon, color) = match &line.result {
            None => (IconName::LoaderCircle, z.text_placeholder),
            Some(Ok(())) => (IconName::Check, z.success),
            Some(Err(_)) => (IconName::X, z.error),
        };
        let text = line.command.clone();
        let error = line.result.as_ref().and_then(|r| r.as_ref().err()).cloned();
        div()
            .id(("klipper-history", i))
            .flex()
            .flex_col()
            .px(px(10.))
            .py(px(2.))
            .cursor_pointer()
            .hover(|s| s.bg(z.element_hover))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(ui::icon(icon).text_color(color).flex_none())
                    .child(div().flex_1().min_w_0().truncate().font_family(crate::theme::MONO_FONT).text_xs().child(text.clone())),
            )
            .children(error.map(|e| {
                div().pl(px(20.)).text_xs().text_color(z.error).whitespace_normal().child(e)
            }))
            .on_click(cx.listener(move |ws, _, window, cx| {
                if let Some(inputs) = &ws.cloud.klipper.controls {
                    inputs.console.update(cx, |s, cx| s.set_value(text.clone(), window, cx));
                }
            }))
    }).collect();

    let running = k.running > 0;
    card(
        "Керування",
        vec![estop],
    )
    .w(px(WIDTH))
    .flex_none()
    .pb(px(8.))
    .child(section(IconName::Thermometer, "Температури").border_t_0().when(running, |d| d.child(ui::spinner("klipper-running"))))
    .child(nozzle_row)
    .child(bed_row)
    .child(
        div()
            .flex()
            .flex_wrap()
            .gap(px(4.))
            .px(px(10.))
            .py(px(4.))
            .child(off)
            .children(preset_buttons),
    )
    .children(profile_note.map(|t| note(t, z.text_placeholder)))
    .child(fan_row)
    .children(heat_block.map(|t| note(t, z.warning)))
    .child(
        section(IconName::Move, "Рух")
            .child(step_button(0))
            .child(step_button(1))
            .child(step_button(2)),
    )
    .child(
        div()
            .flex()
            .flex_wrap()
            .gap(px(4.))
            .px(px(10.))
            .py(px(4.))
            .child(home_button("klipper-home-all", "Home XYZ", Home::All))
            .child(home_button("klipper-home-xy", "Home XY", Home::Xy))
            .child(home_button("klipper-home-z", "Home Z", Home::Z)),
    )
    .child(axis_row(Axis::X, 0))
    .child(axis_row(Axis::Y, 1))
    .child(axis_row(Axis::Z, 2))
    .child(note(tr("Z+ — сопло від столу, Z− — до столу. Крапка: вісь у домашній позиції."), z.text_placeholder))
    .children(move_block.clone().map(|t| note(t, z.warning)))
    .child(section(IconName::SquareTerminal, "Консоль G-code"))
    .child(
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(10.))
            .py(px(4.))
            .children(console_input)
            .child(
                ui::icon_button("klipper-console-send", IconName::Send, "Надіслати (Enter)")
                    .inert(console_block.is_some())
                    .on_click(cx.listener(|ws, _, window, cx| ws.klipper_apply(Field::Console, window, cx))),
            ),
    )
    .children(history)
    .children(console_block.filter(|_| move_block.is_none()).map(|t| note(t, z.warning)))
}

/// Текст для порожнього стану «Klipper не готовий»: кнопка перезапуску прошивки.
pub fn restart_button(cx: &mut Context<Workspace>) -> AnyElement {
    let z = zed();
    ui::ghost_button("klipper-restart", Some(IconName::RefreshCw), "Перезапустити прошивку")
        .border_1()
        .border_color(z.border_variant)
        .on_click(cx.listener(|ws, _, window, cx| ws.confirm_firmware_restart(window, cx)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::klipper::command_block;
    use moonraker::{JobStatus, PrintState};

    #[test]
    fn presets_take_values_from_active_filament() {
        let base = presets(None);
        assert_eq!(base.iter().map(|p| p.label.as_str()).collect::<Vec<_>>(), ["PLA", "PETG", "ABS"]);
        let petg = FilamentTemps { kind: "PETG".into(), nozzle: Some(245), bed: Some(75) };
        let list = presets(Some(&petg));
        assert_eq!(list[1], Preset { label: "PETG".into(), nozzle: 245, bed: 75, from_profile: true });
        assert!(!list[0].from_profile);
        let tpu = FilamentTemps { kind: "TPU".into(), nozzle: Some(225), bed: None };
        let list = presets(Some(&tpu));
        assert_eq!(list.len(), 4);
        assert_eq!((list[3].label.as_str(), list[3].nozzle, list[3].bed), ("TPU", 225, 0));

        let map = serde_json::json!({
            "filament_type": ["PLA"], "nozzle_temperature": ["220"], "hot_plate_temp": ["55"]
        });
        let t = filament_temps(map.as_object().unwrap()).unwrap();
        assert_eq!(t, FilamentTemps { kind: "PLA".into(), nozzle: Some(220), bed: Some(55) });
        assert!(filament_temps(serde_json::json!({"filament_type": ["PLA"]}).as_object().unwrap()).is_none());
    }

    #[test]
    fn printing_allows_only_temperatures_and_jog_needs_home() {
        let idle = JobStatus { homed_axes: "xy".into(), ..Default::default() };
        let printing = JobStatus { state: PrintState::Printing, homed_axes: "xyz".into(), ..Default::default() };
        let paused = JobStatus { state: PrintState::Paused, ..printing.clone() };
        let ok = |job: &JobStatus, c: Command| command_block(true, true, Some(job), &c);
        assert!(ok(&idle, Command::Jog(Axis::X, 1.0)).is_ok());
        assert!(ok(&idle, Command::Jog(Axis::Z, 1.0)).is_err(), "Z не вдома");
        assert!(ok(&idle, Command::Home(Home::Z)).is_ok());
        for job in [&printing, &paused] {
            assert!(ok(job, Command::Heater(HeaterKind::Nozzle, 220)).is_ok());
            assert!(ok(job, Command::Heater(HeaterKind::Bed, 0)).is_ok());
            assert!(ok(job, Command::Home(Home::All)).is_err());
            assert!(ok(job, Command::Jog(Axis::X, 1.0)).is_err());
            assert!(ok(job, Command::PartFan(50)).is_err());
            assert!(ok(job, Command::Raw("G28".into())).is_err());
        }
        assert!(ok(&idle, Command::Heater(HeaterKind::Nozzle, 999)).is_err());
        assert!(command_block(false, true, Some(&idle), &Command::Home(Home::All)).is_err());
        assert!(command_block(true, false, Some(&idle), &Command::Home(Home::All)).is_err());
        assert!(command_block(true, true, None, &Command::Home(Home::All)).is_err());
    }

    #[test]
    fn history_is_bounded_and_results_close_pending_lines() {
        let mut h = Vec::new();
        push(&mut h, ConsoleLine { command: "G28".into(), result: None });
        record(&mut h, "G28".into(), Ok(()));
        assert_eq!(h, vec![ConsoleLine { command: "G28".into(), result: Some(Ok(())) }]);
        for i in 0..50 {
            push(&mut h, ConsoleLine { command: format!("M117 {i}"), result: None });
        }
        assert_eq!(h.len(), HISTORY);
        assert_eq!(h.last().unwrap().command, "M117 49");
    }
}
