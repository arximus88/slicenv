//! Які шляхи друку пропонує діалог «Надіслати на друк» — чиста логіка без gpui.
//!
//! Хмарний друк через плагін Bambu для Slicenv завжди закінчується -26
//! (`BAMBU_NETWORK_SIGNED_ERROR`), тож кнопки «через хмару» за замовчуванням немає:
//! вона з’являється лише після реального успіху (`cloud_print_works`).

/// ОС, від якої залежить, чи є що завантажувати (Bambu Connect для Linux не випускають).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Os {
    Windows,
    Mac,
    Linux,
}

impl Os {
    pub fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Mac
        } else {
            Self::Linux
        }
    }
}

/// Вибраний принтер у локальній мережі.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lan {
    /// Збережений LAN-принтер: друк іде напряму.
    Saved,
    /// Оголосив себе в мережі, але не доданий: спершу код доступу.
    Found,
    Absent,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// Надіслати по локальній мережі (принтер уже доданий).
    LanSend,
    /// Додати знайдений у мережі принтер за кодом доступу й надіслати по LAN.
    LanAdd,
    /// Додати принтер у мережі вручну (IP, серійник, код).
    LanManual,
    /// Передати файл у Bambu Connect.
    Connect,
    /// Друк через хмару плагіном Bambu — лише після підтвердженого успіху.
    Cloud,
}

/// Що сказати про Bambu Connect, якщо його не встановлено.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConnectNotice {
    /// Встановлено або не потрібен (LAN-принтер).
    None,
    /// Пояснення й кнопка «Завантажити Bambu Connect».
    Download,
    /// Пояснення, що версії для Linux поки немає.
    NoLinuxBuild,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Inputs {
    pub os: Os,
    pub connect_installed: bool,
    pub lan: Lan,
    /// Хмарний друк уже хоч раз пройшов (напр. майбутній підписаний плагін).
    pub cloud_print_works: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Paths {
    /// Головна кнопка; `None` — надіслати звідси нема як (лишається експорт).
    pub primary: Option<Action>,
    /// Другорядні кнопки (експорт .gcode.3mf є завжди й тут не перелічується).
    pub secondary: Vec<Action>,
    pub notice: ConnectNotice,
    /// Чекбокси калібрувань і таймлапсу: лише коли файл іде з команди Slicenv (LAN чи хмара).
    /// `bambu-connect://import-file` приймає тільки `path`, `name`, `version`.
    pub print_options: bool,
}

pub fn choose(i: Inputs) -> Paths {
    let mut secondary = Vec::new();
    let (primary, notice) = if i.lan == Lan::Saved {
        if i.connect_installed {
            secondary.push(Action::Connect);
        }
        (Some(Action::LanSend), ConnectNotice::None)
    } else if i.connect_installed {
        if i.lan == Lan::Found {
            secondary.push(Action::LanAdd);
        }
        if i.cloud_print_works {
            secondary.push(Action::Cloud);
        }
        (Some(Action::Connect), ConnectNotice::None)
    } else {
        let notice = if i.os == Os::Linux {
            ConnectNotice::NoLinuxBuild
        } else {
            ConnectNotice::Download
        };
        let primary = match (i.lan, i.cloud_print_works) {
            (Lan::Found, true) => {
                secondary.push(Action::LanAdd);
                Some(Action::Cloud)
            }
            (Lan::Found, false) => Some(Action::LanAdd),
            (_, true) => Some(Action::Cloud),
            (_, false) => None,
        };
        if i.lan == Lan::Absent {
            secondary.push(Action::LanManual);
        }
        (primary, notice)
    };
    let print_options = matches!(primary, Some(Action::LanSend | Action::Cloud));
    Paths { primary, secondary, notice, print_options }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(os: Os, connect_installed: bool, lan: Lan) -> Paths {
        choose(Inputs { os, connect_installed, lan, cloud_print_works: false })
    }

    #[test]
    fn cloud_is_never_offered_by_default() {
        for os in [Os::Windows, Os::Mac, Os::Linux] {
            for bc in [true, false] {
                for lan in [Lan::Saved, Lan::Found, Lan::Absent] {
                    let p = paths(os, bc, lan);
                    assert_ne!(p.primary, Some(Action::Cloud), "{os:?} {bc} {lan:?}");
                    assert!(!p.secondary.contains(&Action::Cloud), "{os:?} {bc} {lan:?}");
                }
            }
        }
    }

    #[test]
    fn saved_lan_printer_sends_directly_with_options() {
        let p = paths(Os::Linux, false, Lan::Saved);
        assert_eq!(p.primary, Some(Action::LanSend));
        assert!(p.secondary.is_empty());
        assert_eq!(p.notice, ConnectNotice::None);
        assert!(p.print_options);
        let p = paths(Os::Windows, true, Lan::Saved);
        assert_eq!(p.primary, Some(Action::LanSend));
        assert_eq!(p.secondary, vec![Action::Connect]);
        assert!(p.print_options);
    }

    #[test]
    fn installed_connect_is_primary_without_options() {
        for os in [Os::Windows, Os::Mac, Os::Linux] {
            let p = paths(os, true, Lan::Absent);
            assert_eq!(p.primary, Some(Action::Connect));
            assert!(p.secondary.is_empty());
            assert_eq!(p.notice, ConnectNotice::None);
            assert!(!p.print_options, "Connect URL carries no calibration flags");
        }
        let p = paths(Os::Windows, true, Lan::Found);
        assert_eq!(p.primary, Some(Action::Connect));
        assert_eq!(p.secondary, vec![Action::LanAdd]);
    }

    #[test]
    fn missing_connect_explains_before_any_attempt() {
        let p = paths(Os::Windows, false, Lan::Absent);
        assert_eq!(p.primary, None);
        assert_eq!(p.notice, ConnectNotice::Download);
        assert_eq!(p.secondary, vec![Action::LanManual]);
        assert!(!p.print_options);
        assert_eq!(paths(Os::Mac, false, Lan::Absent).notice, ConnectNotice::Download);
        let p = paths(Os::Linux, false, Lan::Absent);
        assert_eq!(p.notice, ConnectNotice::NoLinuxBuild);
        assert_eq!(p.primary, None);
    }

    #[test]
    fn printer_found_on_lan_becomes_primary_when_connect_is_missing() {
        for os in [Os::Windows, Os::Linux] {
            let p = paths(os, false, Lan::Found);
            assert_eq!(p.primary, Some(Action::LanAdd));
            assert!(p.secondary.is_empty());
            assert!(!p.print_options, "options appear after the printer is added");
        }
    }

    #[test]
    fn cloud_appears_only_after_real_success() {
        let ok = |connect_installed, lan| {
            choose(Inputs { os: Os::Windows, connect_installed, lan, cloud_print_works: true })
        };
        let p = ok(false, Lan::Absent);
        assert_eq!(p.primary, Some(Action::Cloud));
        assert!(p.print_options);
        let p = ok(true, Lan::Absent);
        assert_eq!(p.primary, Some(Action::Connect));
        assert_eq!(p.secondary, vec![Action::Cloud]);
        let p = ok(false, Lan::Found);
        assert_eq!(p.primary, Some(Action::Cloud));
        assert_eq!(p.secondary, vec![Action::LanAdd]);
        assert_eq!(ok(false, Lan::Saved).primary, Some(Action::LanSend));
    }
}
