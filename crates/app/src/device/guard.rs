//! Охоронці команд принтера. Викликаються і в обробнику інтерфейсу, і в робочому потоці перед
//! `Client::control_in`: вимкнений стиль кнопки не є захистом. Чиста логіка без I/O.

use std::time::Duration;

use cloud::{
    Activity, Axis, Capabilities, Control, ControlContext, DeviceId, Printer, PrinterStatus,
    Profile,
};

use crate::i18n::tr;

/// Рух головки вимагає свіжого знімка: пасивний принтер надсилає стан лише за змінами,
/// тому робочий потік запитує pushall кожні 10 с, а тут межа втричі більша.
pub const MOTION_MAX_AGE: Duration = Duration::from_secs(30);

/// Що відомо про принтер у момент команди.
pub struct Snapshot<'a> {
    pub selected: Option<&'a DeviceId>,
    pub printer: Option<&'a Printer>,
    pub status: Option<&'a PrinterStatus>,
    /// Скільки минуло від останнього знімка вибраного принтера.
    pub age: Option<Duration>,
    /// Чи з’єднаний робочий потік із сервером.
    pub connected: bool,
    /// Принтер у локальній мережі (LAN): відсутність з’єднання — з принтером, а не з хмарою.
    pub lan: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Blocked {
    NoPrinter,
    /// Команда адресована не вибраному принтеру (перемкнули, поки вона летіла).
    WrongDevice,
    Offline,
    Disconnected,
    /// Немає MQTT-з’єднання з LAN-принтером.
    LanDisconnected,
    NoStatus,
    Stale,
    /// Рух головки поза станом очікування (друк, пауза, підготовка, невідомий стан).
    Busy,
    /// Команда не відповідає стану: пауза не під час друку тощо.
    WrongState,
    UnknownModel,
    NotHomed(Axis),
    /// Не пройшла перевірку меж у `cloud` (текст помилки вже українською).
    Rejected(String),
}

impl Blocked {
    pub fn message(&self) -> String {
        match self {
            Self::NoPrinter => tr("Принтер не вибрано"),
            Self::WrongDevice => tr("Команда адресована іншому принтеру"),
            Self::Offline => tr("Принтер офлайн"),
            Self::Disconnected => tr("Немає з’єднання з хмарою"),
            Self::LanDisconnected => tr("Немає з’єднання з принтером у локальній мережі"),
            Self::NoStatus => tr("Стан принтера ще не отримано"),
            Self::Stale => tr("Стан принтера застарів: чекаю на свіжий знімок"),
            Self::Busy => tr("Рух головки дозволено лише коли принтер вільний"),
            Self::WrongState => tr("Команда недоступна в поточному стані принтера"),
            Self::UnknownModel => tr("Модель принтера невідома: команду заблоковано"),
            Self::NotHomed(axis) => format!(
                "{} {}",
                tr("Вісь не в домашній позиції, спершу хоумінг:"),
                axis.letter()
            ),
            Self::Rejected(text) => text.clone(),
        }
    }
}

fn is_motion(control: Control) -> bool {
    matches!(control, Control::Jog(_) | Control::Home)
}

/// Контекст для `cloud` зі знімка: модель, можливості з `fun`, активність, домашня позиція.
pub fn context(printer: &Printer, status: &PrinterStatus) -> ControlContext {
    ControlContext {
        profile: Profile::for_model(&printer.model),
        caps: Capabilities::from_fun(status.fun.as_deref()),
        activity: Activity::from_gcode_state(status.gcode_state.as_deref()),
        homed: [0, 1, 2].map(|axis| status.axis_homed(axis)),
    }
}

/// Дозволяє або блокує команду. `Ok` повертає контекст для `Client::control_in`, який
/// повторно перевіряє межі.
pub fn check(
    snapshot: &Snapshot,
    dev: &DeviceId,
    control: Control,
) -> Result<ControlContext, Blocked> {
    let selected = snapshot.selected.ok_or(Blocked::NoPrinter)?;
    if selected != dev {
        return Err(Blocked::WrongDevice);
    }
    let printer = snapshot.printer.ok_or(Blocked::NoPrinter)?;
    if !printer.online {
        return Err(Blocked::Offline);
    }
    if !snapshot.connected {
        return Err(if snapshot.lan {
            Blocked::LanDisconnected
        } else {
            Blocked::Disconnected
        });
    }
    let status = snapshot.status.ok_or(Blocked::NoStatus)?;
    let ctx = context(printer, status);

    if is_motion(control) {
        if snapshot.age.is_none_or(|age| age > MOTION_MAX_AGE) {
            return Err(Blocked::Stale);
        }
        if ctx.activity != Activity::Idle {
            return Err(Blocked::Busy);
        }
    }
    let state = status.gcode_state.as_deref();
    match control {
        Control::Pause if state != Some("RUNNING") => return Err(Blocked::WrongState),
        Control::Resume if state != Some("PAUSE") => return Err(Blocked::WrongState),
        Control::Stop if ctx.activity != Activity::Busy => return Err(Blocked::WrongState),
        Control::Nozzle(_)
        | Control::Bed(_)
        | Control::Fan(..)
        | Control::Jog(_)
        | Control::Home
            if ctx.profile.is_none() =>
        {
            return Err(Blocked::UnknownModel);
        }
        Control::Jog(jog) if ctx.homed[jog.axis.index()] != Some(true) => {
            return Err(Blocked::NotHomed(jog.axis));
        }
        _ => {}
    }
    // Остаточна перевірка меж тим самим кодом, що збирає повідомлення.
    control
        .message(0, &ctx)
        .map_err(|err| Blocked::Rejected(err.to_string()))?;
    Ok(ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cloud::{FanKind, Jog};

    fn printer(model: &str, online: bool) -> Printer {
        Printer {
            dev_id: DeviceId::new("TESTDEVICE01"),
            name: String::new(),
            model: model.into(),
            online,
        }
    }
    fn status(state: &str) -> PrinterStatus {
        PrinterStatus {
            gcode_state: Some(state.into()),
            home_flag: Some(0b111),
            ..Default::default()
        }
    }
    struct World {
        dev: DeviceId,
        printer: Printer,
        status: Option<PrinterStatus>,
        age: Option<Duration>,
        connected: bool,
        lan: bool,
    }
    impl World {
        fn new(state: &str) -> Self {
            Self {
                dev: DeviceId::new("TESTDEVICE01"),
                printer: printer("N2S", true),
                status: Some(status(state)),
                age: Some(Duration::from_secs(1)),
                connected: true,
                lan: false,
            }
        }
        fn check(&self, control: Control) -> Result<ControlContext, Blocked> {
            self.check_for(&self.dev, control)
        }
        fn check_for(&self, dev: &DeviceId, control: Control) -> Result<ControlContext, Blocked> {
            let snapshot = Snapshot {
                selected: Some(&self.dev),
                printer: Some(&self.printer),
                status: self.status.as_ref(),
                age: self.age,
                connected: self.connected,
                lan: self.lan,
            };
            check(&snapshot, dev, control)
        }
    }
    fn jog(axis: Axis, input: i8) -> Control {
        Control::Jog(Jog::new(axis, input).unwrap())
    }

    #[test]
    fn motion_is_blocked_while_printing_paused_preparing_or_unknown() {
        for state in ["RUNNING", "PAUSE", "PREPARE", "SLICING", "WEIRD"] {
            let w = World::new(state);
            assert_eq!(
                w.check(Control::Home).unwrap_err(),
                Blocked::Busy,
                "{state}"
            );
            assert_eq!(
                w.check(jog(Axis::X, 10)).unwrap_err(),
                Blocked::Busy,
                "{state}"
            );
        }
        let idle = World::new("IDLE");
        assert!(idle.check(Control::Home).is_ok());
        assert!(idle.check(jog(Axis::X, 10)).is_ok());
        assert!(World::new("FINISH").check(jog(Axis::Z, -1)).is_ok());
    }

    #[test]
    fn stale_disconnected_offline_and_missing_status_block_everything() {
        let mut w = World::new("IDLE");
        w.age = Some(MOTION_MAX_AGE + Duration::from_secs(1));
        assert_eq!(w.check(Control::Home).unwrap_err(), Blocked::Stale);
        w.age = None;
        assert_eq!(w.check(jog(Axis::X, 1)).unwrap_err(), Blocked::Stale);
        // Температури не потребують свіжості руху, але потребують зв’язку.
        w.age = Some(Duration::from_secs(120));
        assert!(w.check(Control::Nozzle(200)).is_ok());
        w.connected = false;
        for control in [
            Control::Nozzle(200),
            Control::Home,
            Control::Pause,
            Control::Light(true),
        ] {
            assert_eq!(w.check(control).unwrap_err(), Blocked::Disconnected);
        }
        // Для LAN-принтера — текст про локальну мережу, а не про хмару.
        w.lan = true;
        assert_eq!(
            w.check(Control::Light(true)).unwrap_err(),
            Blocked::LanDisconnected
        );
        assert!(!Blocked::LanDisconnected.message().contains("хмар"));
        w.lan = false;
        w.connected = true;
        w.printer.online = false;
        assert_eq!(w.check(Control::Light(true)).unwrap_err(), Blocked::Offline);
        w.printer.online = true;
        w.status = None;
        assert_eq!(
            w.check(Control::Light(true)).unwrap_err(),
            Blocked::NoStatus
        );
    }

    #[test]
    fn command_for_another_device_is_refused() {
        let w = World::new("IDLE");
        let other = DeviceId::new("OTHERDEVICE99");
        assert_eq!(
            w.check_for(&other, Control::Light(true)).unwrap_err(),
            Blocked::WrongDevice
        );
    }

    #[test]
    fn jog_needs_known_model_and_homed_axis() {
        let mut w = World::new("IDLE");
        w.status.as_mut().unwrap().home_flag = Some(0b101); // Y не в домашній позиції
        assert_eq!(
            w.check(jog(Axis::Y, 10)).unwrap_err(),
            Blocked::NotHomed(Axis::Y)
        );
        assert!(w.check(jog(Axis::X, 10)).is_ok());
        w.status.as_mut().unwrap().home_flag = None;
        assert_eq!(
            w.check(jog(Axis::X, 10)).unwrap_err(),
            Blocked::NotHomed(Axis::X)
        );
        // Хоумінг при цьому дозволений: він і ставить осі в домашню позицію.
        assert!(w.check(Control::Home).is_ok());
        w.printer.model = "UNKNOWN".into();
        assert_eq!(w.check(Control::Home).unwrap_err(), Blocked::UnknownModel);
        assert_eq!(
            w.check(jog(Axis::X, 10)).unwrap_err(),
            Blocked::UnknownModel
        );
    }

    #[test]
    fn temperature_fan_and_speed_are_allowed_while_printing_within_bounds() {
        let w = World::new("RUNNING");
        assert!(w.check(Control::Nozzle(215)).is_ok());
        assert!(w.check(Control::Bed(60)).is_ok());
        assert!(w.check(Control::Fan(FanKind::Part, 80)).is_ok());
        assert!(w.check(Control::Speed(cloud::SpeedLevel::Sport)).is_ok());
        assert!(matches!(
            w.check(Control::Nozzle(500)).unwrap_err(),
            Blocked::Rejected(_)
        ));
        assert!(matches!(
            w.check(Control::Bed(101)).unwrap_err(),
            Blocked::Rejected(_)
        ));
        assert!(matches!(
            w.check(Control::Fan(FanKind::Aux, 10)).unwrap_err(),
            Blocked::Rejected(_)
        ));
        assert!(matches!(
            w.check(Control::Fan(FanKind::Part, 101)).unwrap_err(),
            Blocked::Rejected(_)
        ));
    }

    #[test]
    fn pause_resume_stop_match_the_job_state() {
        assert!(World::new("RUNNING").check(Control::Pause).is_ok());
        assert_eq!(
            World::new("PAUSE").check(Control::Pause).unwrap_err(),
            Blocked::WrongState
        );
        assert!(World::new("PAUSE").check(Control::Resume).is_ok());
        assert_eq!(
            World::new("RUNNING").check(Control::Resume).unwrap_err(),
            Blocked::WrongState
        );
        assert!(World::new("PAUSE").check(Control::Stop).is_ok());
        assert!(World::new("PREPARE").check(Control::Stop).is_ok());
        assert_eq!(
            World::new("IDLE").check(Control::Stop).unwrap_err(),
            Blocked::WrongState
        );
    }

    #[test]
    fn capabilities_in_the_context_follow_the_fun_flags() {
        let mut w = World::new("IDLE");
        w.status.as_mut().unwrap().fun = Some(format!("{:x}", 1u64 << 38));
        let ctx = w.check(jog(Axis::X, 10)).unwrap();
        assert!(ctx.caps.mqtt_axis && !ctx.caps.mqtt_homing);
    }
}
