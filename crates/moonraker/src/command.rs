//! Команди керування Klipper — G-code для `POST /printer/gcode/script`.
//!
//! Лише побудова й перевірка меж; чи можна команду слати зараз (друк, хоумінг) — вирішує
//! застосунок за станом принтера ([`Command::allowed_while_printing`]). Klipper і сам
//! відхиляє рух поза межами та нагрів вище `max_temp` з printer.cfg.
use crate::error::{MoonrakerError, Result};

/// Верхня межа сопла, яку ми взагалі погоджуємося відправити (Klipper перевірить свою).
pub const MAX_NOZZLE: u16 = 320;
pub const MAX_BED: u16 = 130;
/// Найдовший крок джогу одним натисканням, мм.
pub const MAX_JOG: f64 = 100.0;
/// Кроки джогу в інтерфейсі, мм.
pub const JOG_STEPS: [f64; 3] = [0.1, 1.0, 10.0];
/// Швидкість джогу, мм/хв.
pub const XY_FEED: u32 = 3000;
pub const Z_FEED: u32 = 600;
const MAX_SCRIPT: usize = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaterKind {
    /// `[extruder]`.
    Nozzle,
    /// `[heater_bed]`.
    Bed,
}

impl HeaterKind {
    pub fn klipper_name(self) -> &'static str {
        match self {
            Self::Nozzle => "extruder",
            Self::Bed => "heater_bed",
        }
    }
    pub fn max(self) -> u16 {
        match self {
            Self::Nozzle => MAX_NOZZLE,
            Self::Bed => MAX_BED,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    pub fn letter(self) -> char {
        match self {
            Self::X => 'X',
            Self::Y => 'Y',
            Self::Z => 'Z',
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Home {
    All,
    Xy,
    Z,
}

impl Home {
    /// Осі, які стануть «вдома» (`toolhead.homed_axes`).
    pub fn axes(self) -> &'static [Axis] {
        match self {
            Self::All => &[Axis::X, Axis::Y, Axis::Z],
            Self::Xy => &[Axis::X, Axis::Y],
            Self::Z => &[Axis::Z],
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Ціль нагрівача, °C; 0 — вимкнути.
    Heater(HeaterKind, u16),
    Home(Home),
    /// Відносний рух осі, мм (знак — напрямок; для Z «+» — сопло від столу).
    Jog(Axis, f64),
    /// Вентилятор обдуву деталі (`[fan]`), 0–100 %.
    PartFan(u8),
    /// Довільний рядок з консолі.
    Raw(String),
}

fn number(value: f64) -> String {
    let text = format!("{value:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" { "0".into() } else { text.into() }
}

impl Command {
    /// G-code для Moonraker. Помилка — значення поза межами, у мережу нічого не піде.
    pub fn script(&self) -> Result<String> {
        let invalid = |detail: String| Err(MoonrakerError::InvalidCommand(detail));
        Ok(match self {
            Self::Heater(kind, target) => {
                if *target > kind.max() {
                    return invalid(format!("{} > {} °C", target, kind.max()));
                }
                format!(
                    "SET_HEATER_TEMPERATURE HEATER={} TARGET={target}",
                    kind.klipper_name()
                )
            }
            Self::Home(Home::All) => "G28".into(),
            Self::Home(Home::Xy) => "G28 X Y".into(),
            Self::Home(Home::Z) => "G28 Z".into(),
            Self::Jog(axis, distance) => {
                if !distance.is_finite() || *distance == 0.0 || distance.abs() > MAX_JOG {
                    return invalid(format!("{distance} mm"));
                }
                let feed = if *axis == Axis::Z { Z_FEED } else { XY_FEED };
                // Відносний режим лише на цей рух: G90 повертає абсолютний, як у Mainsail.
                format!("G91\nG1 {}{} F{feed}\nG90", axis.letter(), number(*distance))
            }
            Self::PartFan(percent) => {
                if *percent > 100 {
                    return invalid(format!("{percent} %"));
                }
                let pwm = (f64::from(*percent) * 255.0 / 100.0).round() as u16;
                format!("M106 S{pwm}")
            }
            Self::Raw(line) => {
                let line = line.trim();
                if line.is_empty() || line.len() > MAX_SCRIPT || line.chars().any(char::is_control) {
                    return invalid(String::new());
                }
                line.to_owned()
            }
        })
    }

    /// Klipper виконує зміну цілі нагріву посеред друку безпечно; рух, хоумінг, вентилятор
    /// і довільні рядки — ні (вони втручаються в G-code, що друкується).
    pub fn allowed_while_printing(&self) -> bool {
        matches!(self, Self::Heater(..))
    }

    /// Чи рухає команда осі (потребує хоумінгу й вільного принтера).
    pub fn moves(&self) -> bool {
        matches!(self, Self::Home(_) | Self::Jog(..))
    }
}

/// Відсоткове кодування значення параметра запиту (RFC 3986, усе крім unreserved).
pub fn query_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len() * 3);
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out += &format!("%{b:02X}");
        }
    }
    out
}

/// Зворотне до [`query_encode`] (і `+` як пробіл) — для фейкового сервера.
pub fn query_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = bytes
                    .get(i + 1..i + 3)
                    .and_then(|h| std::str::from_utf8(h).ok())
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                        continue;
                    }
                    None => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_match_klipper_syntax() {
        let s = |c: Command| c.script().unwrap();
        assert_eq!(
            s(Command::Heater(HeaterKind::Nozzle, 215)),
            "SET_HEATER_TEMPERATURE HEATER=extruder TARGET=215"
        );
        assert_eq!(
            s(Command::Heater(HeaterKind::Bed, 0)),
            "SET_HEATER_TEMPERATURE HEATER=heater_bed TARGET=0"
        );
        assert_eq!(s(Command::Home(Home::All)), "G28");
        assert_eq!(s(Command::Home(Home::Xy)), "G28 X Y");
        assert_eq!(s(Command::Home(Home::Z)), "G28 Z");
        assert_eq!(s(Command::Jog(Axis::X, 10.0)), "G91\nG1 X10 F3000\nG90");
        assert_eq!(s(Command::Jog(Axis::Y, -0.1)), "G91\nG1 Y-0.1 F3000\nG90");
        assert_eq!(s(Command::Jog(Axis::Z, 1.0)), "G91\nG1 Z1 F600\nG90");
        assert_eq!(s(Command::PartFan(100)), "M106 S255");
        assert_eq!(s(Command::PartFan(50)), "M106 S128");
        assert_eq!(s(Command::PartFan(0)), "M106 S0");
        assert_eq!(s(Command::Raw("  QUERY_ENDSTOPS ".into())), "QUERY_ENDSTOPS");
    }

    #[test]
    fn out_of_range_values_are_rejected() {
        for bad in [
            Command::Heater(HeaterKind::Nozzle, MAX_NOZZLE + 1),
            Command::Heater(HeaterKind::Bed, 200),
            Command::Jog(Axis::X, 0.0),
            Command::Jog(Axis::X, 150.0),
            Command::Jog(Axis::Z, f64::NAN),
            Command::PartFan(101),
            Command::Raw("   ".into()),
            Command::Raw("G28\nG1 Z-50".into()),
        ] {
            assert!(matches!(bad.script(), Err(MoonrakerError::InvalidCommand(_))), "{bad:?}");
        }
        assert!(Command::Heater(HeaterKind::Bed, 60).allowed_while_printing());
        assert!(!Command::Jog(Axis::X, 1.0).allowed_while_printing());
        assert!(!Command::PartFan(10).allowed_while_printing());
        assert!(!Command::Raw("M117 hi".into()).allowed_while_printing());
    }

    #[test]
    fn query_encoding_roundtrips() {
        let script = "G91\nG1 X-0.1 F3000\nG90 +Кубик&=";
        let encoded = query_encode(script);
        assert!(!encoded.contains(['\n', ' ', '+', '&', '=']));
        assert_eq!(query_decode(&encoded), script);
        assert_eq!(query_decode("a+b%2"), "a b%2");
    }
}
