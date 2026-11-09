//! Керування принтером: межі, можливості моделей і побудова MQTT-повідомлень.
//!
//! Усе виведено з Bambu Studio (`DevAxisCtrl.cpp`, `DevFan.cpp`, `DeviceManager.cpp`,
//! `resources/printers/*.json`). Тут лише чисті функції: нічого не надсилається, тож усе
//! покривається тестами на синтетичних знімках. Перевірку меж виконує кожен виклик
//! `Control::message`, а не інтерфейс — вимкнена кнопка не є захистом.
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
    Z,
}
impl Axis {
    pub fn letter(self) -> &'static str {
        match self {
            Self::X => "X",
            Self::Y => "Y",
            Self::Z => "Z",
        }
    }
    pub fn index(self) -> usize {
        self as usize
    }
}

/// Тип вентилятора в `M106 P…` (1 — деталь, 2 — додатковий, 3 — камера).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FanKind {
    Part = 1,
    Aux = 2,
    Chamber = 3,
}

/// Один обмежений хід. `input` — аргумент `Ctrl_Axis` зі Studio: лише ±1 або ±10 мм;
/// знак і інверсію Y/Z для не-CoreXY застосовує `Control::message`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Jog {
    pub axis: Axis,
    pub input: i8,
}
impl Jog {
    pub fn new(axis: Axis, input: i8) -> Result<Self> {
        ensure!(
            matches!(input, -10 | -1 | 1 | 10),
            "Дозволені кроки: ±1 і ±10 мм"
        );
        Ok(Self { axis, input })
    }
}

/// Дані моделі з `resources/printers/<model>.json` Bambu Studio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    pub core_xy: bool,
    pub nozzle_max: u16,
    pub bed_max: u16,
    pub aux_fan: bool,
    pub chamber_fan: bool,
}
impl Profile {
    /// `None` — модель невідома: температури, вентилятори й ходи для неї заблоковані.
    pub fn for_model(code: &str) -> Option<Self> {
        let (core_xy, bed_max, aux_fan, chamber_fan) = match code {
            "N2S" => (false, 100, false, false),
            "N1" => (false, 80, false, false),
            "C11" => (true, 100, true, true),
            "C12" => (true, 100, true, true),
            // X1: Studio дає 120 (110 для 220 В); беремо нижню межу, 220 В не розпізнаємо.
            "BL-P001" | "BL-P002" => (true, 110, true, true),
            "C13" => (true, 110, true, true),
            _ => return None,
        };
        Some(Self {
            core_xy,
            nozzle_max: 300,
            bed_max,
            aux_fan,
            chamber_fan,
        })
    }
    pub fn has_fan(&self, kind: FanKind) -> bool {
        match kind {
            FanKind::Part => true,
            FanKind::Aux => self.aux_fan,
            FanKind::Chamber => self.chamber_fan,
        }
    }
}

/// Прапорці `fun` (hex-рядок із push_status): біти за `DevAxis::ParseAxis` і `parse_new_info`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// Біт 32: `back_to_center` через MQTT.
    pub mqtt_homing: bool,
    /// Біт 38: `xyz_ctrl` через MQTT.
    pub mqtt_axis: bool,
    /// Біт 39: `set_bed_temp` через MQTT.
    pub mqtt_bed: bool,
}
impl Capabilities {
    /// Відсутній чи нерозібраний `fun` дає «жодних MQTT-можливостей»; Studio робить так само
    /// (`stoull` кидає виняток → 0) і тоді користується G-кодом.
    pub fn from_fun(fun: Option<&str>) -> Self {
        let Some(bits) = fun.and_then(|f| u64::from_str_radix(f.trim(), 16).ok()) else {
            return Self::default();
        };
        let bit = |n: u32| (bits >> n) & 1 == 1;
        Self {
            mqtt_homing: bit(32),
            mqtt_axis: bit(38),
            mqtt_bed: bit(39),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Activity {
    Idle,
    /// Друк, підготовка, пауза або нарізання.
    Busy,
    #[default]
    Unknown,
}
impl Activity {
    /// Лише явно «вільний» стан дозволяє рух; невідомий стан вважається небезпечним.
    pub fn from_gcode_state(state: Option<&str>) -> Self {
        match state {
            Some("IDLE" | "FINISH" | "FAILED") => Self::Idle,
            Some("RUNNING" | "PAUSE" | "PREPARE" | "SLICING") => Self::Busy,
            _ => Self::Unknown,
        }
    }
}

/// Що відомо про принтер у момент команди; будує його викликач із актуального знімка.
#[derive(Clone, Copy, Debug, Default)]
pub struct ControlContext {
    pub profile: Option<Profile>,
    pub caps: Capabilities,
    pub activity: Activity,
    /// X/Y/Z у домашній позиції; `None` — невідомо.
    pub homed: [Option<bool>; 3],
}

#[derive(Clone, Copy, Debug)]
pub enum SpeedLevel {
    Silent = 1,
    Standard = 2,
    Sport = 3,
    Ludicrous = 4,
}

#[derive(Clone, Copy, Debug)]
pub enum Control {
    Pause,
    Resume,
    Stop,
    Light(bool),
    Speed(SpeedLevel),
    /// Цільова температура сопла, °C; 0 вимикає нагрів.
    Nozzle(u16),
    /// Цільова температура столу, °C; 0 вимикає нагрів.
    Bed(u16),
    /// Швидкість вентилятора, відсотки.
    Fan(FanKind, u8),
    Jog(Jog),
    Home,
}

/// Найменша ненульова ціль нагріву (як мінімум поля температури столу в Studio).
pub const MIN_TARGET: u16 = 20;
const XY_FEED: u32 = 3000;
const Z_FEED: u32 = 900;

impl Control {
    /// QoS MQTT: 1 для pause/resume/stop (як у Studio), решта — 0.
    pub fn qos(self) -> i32 {
        if matches!(self, Self::Pause | Self::Resume | Self::Stop) {
            1
        } else {
            0
        }
    }

    /// Перевіряє межі та умови й будує повідомлення. Помилка означає, що в мережу нічого не піде.
    pub fn message(self, sequence: u64, ctx: &ControlContext) -> Result<Value> {
        let seq = sequence.to_string();
        let gcode =
            |code: String| json!({"print":{"command":"gcode_line","param":code,"sequence_id":seq}});
        Ok(match self {
            Self::Pause | Self::Resume | Self::Stop => {
                let command = match self {
                    Self::Pause => "pause",
                    Self::Resume => "resume",
                    _ => "stop",
                };
                json!({"print":{"command":command,"param":"","sequence_id":seq}})
            }
            Self::Light(on) => {
                json!({"system":{"command":"ledctrl","led_node":"chamber_light","led_mode":if on {"on"} else {"off"},"sequence_id":seq,"led_on_time":500,"led_off_time":500,"loop_times":0,"interval_time":0}})
            }
            Self::Speed(level) => {
                json!({"print":{"command":"print_speed","param":(level as u32).to_string(),"sequence_id":seq}})
            }
            Self::Nozzle(target) => {
                let profile = known(ctx)?;
                check_target(target, profile.nozzle_max, "сопла")?;
                gcode(format!("M104 S{target}\n"))
            }
            Self::Bed(target) => {
                let profile = known(ctx)?;
                check_target(target, profile.bed_max, "столу")?;
                if ctx.caps.mqtt_bed {
                    json!({"print":{"command":"set_bed_temp","temp":target,"sequence_id":seq}})
                } else {
                    gcode(format!("M140 S{target}\n"))
                }
            }
            Self::Fan(kind, percent) => {
                ensure!(percent <= 100, "Швидкість вентилятора — 0..=100 %");
                ensure!(
                    known(ctx)?.has_fan(kind),
                    "Цей вентилятор відсутній у моделі"
                );
                let speed = (u32::from(percent) * 255 + 50) / 100;
                gcode(format!("M106 P{} S{speed} \n", kind as u8))
            }
            Self::Home => {
                known(ctx)?;
                ensure!(
                    ctx.activity == Activity::Idle,
                    "Хоумінг дозволено лише у стані очікування"
                );
                if ctx.caps.mqtt_homing {
                    json!({"print":{"command":"back_to_center","sequence_id":seq}})
                } else {
                    gcode("G28 \n".into())
                }
            }
            Self::Jog(jog) => {
                ensure!(
                    ctx.activity == Activity::Idle,
                    "Рух головки дозволено лише у стані очікування"
                );
                let profile = known(ctx)?;
                Jog::new(jog.axis, jog.input)?;
                ensure!(
                    ctx.homed[jog.axis.index()] == Some(true),
                    "Вісь {} не в домашній позиції: спершу виконайте хоумінг",
                    jog.axis.letter()
                );
                // Не-CoreXY (A1, A1 mini): Y і Z інвертовані, як у DevAxis::Ctrl_Axis.
                let invert = !profile.core_xy && jog.axis != Axis::X;
                let signed = if invert { -jog.input } else { jog.input };
                if ctx.caps.mqtt_axis {
                    json!({"print":{
                        "command":"xyz_ctrl","axis":jog.axis.letter(),
                        "dir":if signed > 0 { 1 } else { -1 },
                        "mode":if jog.input.abs() >= 10 { 1 } else { 0 },
                        "sequence_id":seq}})
                } else {
                    let feed = if jog.axis == Axis::Z { Z_FEED } else { XY_FEED };
                    gcode(format!(
                        "M211 S \nM211 X1 Y1 Z1\nM1002 push_ref_mode\nG91 \nG1 {}{:.1} F{feed}\nM1002 pop_ref_mode\nM211 R\n",
                        jog.axis.letter(),
                        f64::from(signed)
                    ))
                }
            }
        })
    }
}

fn known(ctx: &ControlContext) -> Result<Profile> {
    match ctx.profile {
        Some(profile) => Ok(profile),
        None => bail!("Модель принтера невідома: команда заблокована"),
    }
}
fn check_target(target: u16, max: u16, what: &str) -> Result<()> {
    ensure!(
        target == 0 || (MIN_TARGET..=max).contains(&target),
        "Температура {what}: 0 (вимк.) або {MIN_TARGET}..={max} °C"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(model: &str, activity: Activity) -> ControlContext {
        ControlContext {
            profile: Profile::for_model(model),
            caps: Capabilities::default(),
            activity,
            homed: [Some(true); 3],
        }
    }
    fn param(v: &Value) -> &str {
        v["print"]["param"].as_str().unwrap()
    }

    #[test]
    fn capabilities_come_from_hex_fun_bits() {
        assert_eq!(Capabilities::from_fun(None), Capabilities::default());
        assert_eq!(Capabilities::from_fun(Some("zz")), Capabilities::default());
        let all = (1u64 << 32) | (1 << 38) | (1 << 39);
        let caps = Capabilities::from_fun(Some(&format!("{all:x}")));
        assert!(caps.mqtt_homing && caps.mqtt_axis && caps.mqtt_bed);
        let only_axis = Capabilities::from_fun(Some(&format!("{:x}", 1u64 << 38)));
        assert!(only_axis.mqtt_axis && !only_axis.mqtt_homing && !only_axis.mqtt_bed);
    }

    #[test]
    fn temperatures_are_bounded_by_model_and_use_studio_gcode() {
        let a1 = ctx("N2S", Activity::Busy); // температури дозволені й під час друку
        assert_eq!(
            param(&Control::Nozzle(220).message(1, &a1).unwrap()),
            "M104 S220\n"
        );
        assert_eq!(
            param(&Control::Nozzle(0).message(1, &a1).unwrap()),
            "M104 S0\n"
        );
        assert_eq!(
            param(&Control::Bed(65).message(2, &a1).unwrap()),
            "M140 S65\n"
        );
        for bad in [1, 19, 301, u16::MAX] {
            assert!(Control::Nozzle(bad).message(3, &a1).is_err(), "{bad}");
        }
        assert!(Control::Bed(101).message(4, &a1).is_err());
        let mini = ctx("N1", Activity::Idle);
        assert!(Control::Bed(80).message(5, &mini).is_ok());
        assert!(Control::Bed(81).message(5, &mini).is_err());
        // Невідома модель: жодних температур.
        assert!(
            Control::Nozzle(200)
                .message(6, &ctx("ZZZ", Activity::Idle))
                .is_err()
        );
        // MQTT-шлях столу лише за бітом 39.
        let mut mqtt = a1;
        mqtt.caps.mqtt_bed = true;
        let m = Control::Bed(60).message(7, &mqtt).unwrap();
        assert_eq!(m["print"]["command"], "set_bed_temp");
        assert_eq!(m["print"]["temp"], 60);
    }

    #[test]
    fn fans_are_checked_against_model_and_scaled_to_255() {
        let a1 = ctx("N2S", Activity::Busy);
        assert_eq!(
            param(&Control::Fan(FanKind::Part, 100).message(1, &a1).unwrap()),
            "M106 P1 S255 \n"
        );
        assert_eq!(
            param(&Control::Fan(FanKind::Part, 50).message(1, &a1).unwrap()),
            "M106 P1 S128 \n"
        );
        assert_eq!(
            param(&Control::Fan(FanKind::Part, 0).message(1, &a1).unwrap()),
            "M106 P1 S0 \n"
        );
        assert!(Control::Fan(FanKind::Part, 101).message(1, &a1).is_err());
        assert!(Control::Fan(FanKind::Aux, 50).message(1, &a1).is_err());
        assert!(Control::Fan(FanKind::Chamber, 50).message(1, &a1).is_err());
        let p1s = ctx("C12", Activity::Busy);
        assert_eq!(
            param(&Control::Fan(FanKind::Aux, 100).message(1, &p1s).unwrap()),
            "M106 P2 S255 \n"
        );
        assert!(
            Control::Fan(FanKind::Part, 10)
                .message(1, &ctx("ZZZ", Activity::Busy))
                .is_err()
        );
    }

    #[test]
    fn motion_is_refused_unless_idle_and_homed() {
        let jog = Control::Jog(Jog::new(Axis::X, 10).unwrap());
        for activity in [Activity::Busy, Activity::Unknown] {
            let c = ctx("N2S", activity);
            assert!(jog.message(1, &c).is_err());
            assert!(Control::Home.message(1, &c).is_err());
        }
        assert!(
            Control::Home
                .message(1, &ctx("ZZZ", Activity::Idle))
                .is_err()
        );
        let mut c = ctx("N2S", Activity::Idle);
        c.homed[0] = None;
        assert!(jog.message(1, &c).is_err(), "невідомий стан осі");
        c.homed[0] = Some(false);
        assert!(jog.message(1, &c).is_err(), "вісь не в домашній позиції");
        // Публічні поля дозволяють обійти Jog::new: message перевіряє крок ще раз.
        let forged = Control::Jog(Jog {
            axis: Axis::X,
            input: 11,
        });
        assert!(forged.message(1, &ctx("N2S", Activity::Idle)).is_err());
        assert!(Jog::new(Axis::Z, 0).is_err());
        assert!(Jog::new(Axis::Z, 50).is_err());
        assert!(
            jog.message(1, &ctx("ZZZ", Activity::Idle)).is_err(),
            "невідома геометрія"
        );
    }

    #[test]
    fn jog_gcode_matches_studio_for_bed_slinger_and_core_xy() {
        let a1 = ctx("N2S", Activity::Idle);
        let x = Control::Jog(Jog::new(Axis::X, 10).unwrap())
            .message(1, &a1)
            .unwrap();
        assert_eq!(
            param(&x),
            "M211 S \nM211 X1 Y1 Z1\nM1002 push_ref_mode\nG91 \nG1 X10.0 F3000\nM1002 pop_ref_mode\nM211 R\n"
        );
        // A1: стрілка «вгору» Y (+10) та Z (−10) інвертуються.
        let y = Control::Jog(Jog::new(Axis::Y, 10).unwrap())
            .message(1, &a1)
            .unwrap();
        assert!(param(&y).contains("G1 Y-10.0 F3000"));
        let z = Control::Jog(Jog::new(Axis::Z, -1).unwrap())
            .message(1, &a1)
            .unwrap();
        assert!(param(&z).contains("G1 Z1.0 F900"));
        let p1 = ctx("C11", Activity::Idle);
        let y = Control::Jog(Jog::new(Axis::Y, 10).unwrap())
            .message(1, &p1)
            .unwrap();
        assert!(param(&y).contains("G1 Y10.0 F3000"));
    }

    #[test]
    fn jog_and_home_use_mqtt_only_when_capability_bits_are_set() {
        let mut c = ctx("N2S", Activity::Idle);
        c.caps = Capabilities {
            mqtt_homing: true,
            mqtt_axis: true,
            mqtt_bed: false,
        };
        let m = Control::Jog(Jog::new(Axis::Y, 10).unwrap())
            .message(9, &c)
            .unwrap();
        assert_eq!(m["print"]["command"], "xyz_ctrl");
        assert_eq!(m["print"]["axis"], "Y");
        assert_eq!(m["print"]["dir"], -1); // інверсія A1
        assert_eq!(m["print"]["mode"], 1);
        let small = Control::Jog(Jog::new(Axis::X, -1).unwrap())
            .message(9, &c)
            .unwrap();
        assert_eq!(
            (
                small["print"]["dir"].as_i64(),
                small["print"]["mode"].as_i64()
            ),
            (Some(-1), Some(0))
        );
        assert_eq!(
            Control::Home.message(10, &c).unwrap()["print"]["command"],
            "back_to_center"
        );
        c.caps = Capabilities::default();
        assert_eq!(param(&Control::Home.message(10, &c).unwrap()), "G28 \n");
    }

    #[test]
    fn existing_controls_keep_their_wire_format() {
        let c = ControlContext::default();
        let pause = Control::Pause.message(5, &c).unwrap();
        assert_eq!(
            pause,
            json!({"print":{"command":"pause","param":"","sequence_id":"5"}})
        );
        assert_eq!(Control::Pause.qos(), 1);
        assert_eq!(
            Control::Speed(SpeedLevel::Sport).message(6, &c).unwrap()["print"]["param"],
            "3"
        );
        assert_eq!(Control::Nozzle(200).qos(), 0);
        assert_eq!(
            Control::Light(true).message(7, &c).unwrap()["system"]["led_mode"],
            "on"
        );
    }
}
