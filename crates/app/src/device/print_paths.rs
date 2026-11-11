//! Маршрут друку в діалозі «Надіслати на друк» — чиста логіка без gpui.
//!
//! Діалог показує всі маршрути (локальна мережа, Bambu Cloud, Bambu Connect) зі станом кожного,
//! а головна кнопка діє вибраним. Маршрут — це свідомий вибір користувача для цього принтера, а без нього —
//! з’єднання, яке зараз справді готове. Збережений LAN-принтер (IP і серійник) сам по собі не
//! означає, що LAN-друк доступний: потрібне живе з’єднання. Вибраний маршрут ніколи тихо не
//! підміняється іншим: не готовий — кнопка неактивна, а діалог називає, чого бракує.
//!
//! Режими принтера не взаємовиключні для видимості: принтер у режимі Cloud часто видно й у
//! локальній мережі, а LAN Only лише вимикає хмару (тоді хмара бачить його офлайн).
//! Хмарний друк плагіном Bambu для Slicenv закінчується -26 (`BAMBU_NETWORK_SIGNED_ERROR`), тож
//! хмара як маршрут з’являється лише тоді, коли друк нею можливий (`cloud_print`).

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
    /// Збережений LAN-принтер; `up` — з’єднання з ним зараз живе.
    Saved { up: bool },
    /// Оголосив себе в мережі, але не доданий: спершу код доступу.
    Found,
    Absent,
}

/// Хмара для цього принтера.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cloud {
    /// Сервер відповів, принтер є в акаунті й онлайн.
    Ready,
    /// Підключення ще йде.
    Connecting,
    /// Хмара з’єднана, але принтер в акаунті офлайн (вимкнений або LAN Only).
    PrinterOffline,
    /// Хмара з’єднана, а цього принтера в акаунті немає.
    NotInAccount,
    /// Хмару не налаштовано, не ввійшли або вимкнено: потрібна дія користувача.
    NeedsSetup,
}

/// Маршрут друку.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Route {
    Cloud,
    Lan,
    Connect,
}

/// Чому «Друкувати» зараз неактивна — і яку одну дію запропонувати.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Blocker {
    /// Увійти / налаштувати / увімкнути хмару.
    CloudSetup,
    CloudConnecting,
    CloudPrinterOffline,
    CloudNotInAccount,
    /// Принтер збережено в LAN, але з’єднання з ним зараз немає.
    LanOffline,
    /// Принтер видно в мережі, але не додано: код доступу.
    LanAddFound,
    /// Принтера немає в LAN: додати вручну (IP, серійник, код).
    LanAddManual,
    /// Немає жодного маршруту (Bambu Connect не встановлено, хмарний друк неможливий).
    NoRoute,
}

/// Що сказати про Bambu Connect, якщо його не встановлено.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConnectNotice {
    /// Встановлено, не потрібен або є інший маршрут.
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
    pub cloud: Cloud,
    /// Хмарний друк можливий: відкритий плагін з увімкненою хмарою або хмара вже приймала
    /// завдання в цьому сеансі. Без цього хмара — не маршрут друку.
    pub cloud_print: bool,
    /// Користувач вибрав відкритий плагін (Open Bamboo Networking): LAN-друк підписує він,
    /// Bambu Connect не потрібен.
    pub open_plugin: bool,
    /// Свідомий вибір маршруту для цього принтера (рядок у списку маршрутів).
    pub chosen: Option<Route>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Plan {
    /// Поточний (вибраний) маршрут; `None` — маршруту немає.
    pub route: Option<Route>,
    /// `None` — «Друкувати» активна.
    pub blocker: Option<Blocker>,
    /// Інші маршрути, які можна вибрати.
    pub alternatives: Vec<Route>,
    pub notice: ConnectNotice,
    /// Чекбокси калібрувань і таймлапсу: лише коли файл іде з команди Slicenv (LAN чи хмара).
    /// `bambu-connect://import-file` приймає тільки `path`, `name`, `version`.
    pub print_options: bool,
}

impl Plan {
    pub fn ready(&self) -> bool {
        self.route.is_some() && self.blocker.is_none()
    }

    /// Усі маршрути для вибору в незмінному порядку (LAN, хмара, Bambu Connect): вибір іншого
    /// не переставляє рядки під вказівником.
    pub fn options(&self) -> Vec<Route> {
        [Route::Lan, Route::Cloud, Route::Connect]
            .into_iter()
            .filter(|r| self.route == Some(*r) || self.alternatives.contains(r))
            .collect()
    }
}

/// Стани для `SLICENV_FAKE_PRINT_ROUTE` — діалог друку без принтера, хмари й Bambu Connect
/// (лише перевірка інтерфейсу). Свідомий вибір `chosen` лишається справжнім.
pub fn demo(name: &str, chosen: Option<Route>) -> Option<Inputs> {
    let mut i = Inputs {
        os: Os::Linux,
        connect_installed: false,
        lan: Lan::Absent,
        cloud: Cloud::NeedsSetup,
        cloud_print: false,
        open_plugin: false,
        chosen,
    };
    match name {
        "connect" => {
            i.os = Os::Windows;
            i.connect_installed = true;
        }
        "lan" => i.lan = Lan::Saved { up: true },
        "lan-offline" => i.lan = Lan::Saved { up: false },
        "lan-found" => i.lan = Lan::Found,
        "cloud" => {
            i.os = Os::Windows;
            i.connect_installed = true;
            i.cloud = Cloud::Ready;
            i.cloud_print = true;
        }
        "cloud-setup" => i.cloud_print = true,
        "cloud-connecting" => {
            i.cloud_print = true;
            i.cloud = Cloud::Connecting;
        }
        "cloud-offline" => {
            i.cloud_print = true;
            i.cloud = Cloud::PrinterOffline;
            i.lan = Lan::Found;
        }
        "open-plugin" => i.open_plugin = true,
        "none" => {}
        "none-download" => i.os = Os::Windows,
        _ => return None,
    }
    Some(i)
}

/// Що заважає друкувати маршрутом `route` (`None` — готовий).
pub fn blocker(i: &Inputs, route: Route) -> Option<Blocker> {
    match route {
        Route::Lan => match i.lan {
            Lan::Saved { up: true } => None,
            Lan::Saved { up: false } => Some(Blocker::LanOffline),
            Lan::Found => Some(Blocker::LanAddFound),
            Lan::Absent => Some(Blocker::LanAddManual),
        },
        Route::Cloud => match i.cloud {
            Cloud::Ready => None,
            Cloud::Connecting => Some(Blocker::CloudConnecting),
            Cloud::PrinterOffline => Some(Blocker::CloudPrinterOffline),
            Cloud::NotInAccount => Some(Blocker::CloudNotInAccount),
            Cloud::NeedsSetup => Some(Blocker::CloudSetup),
        },
        Route::Connect => (!i.connect_installed).then_some(Blocker::NoRoute),
    }
}

/// Маршрути, які взагалі мають сенс тут. LAN — завжди (принтер можна додати); хмара — лише
/// коли друк нею можливий; Bambu Connect — коли встановлений.
fn candidates(i: &Inputs) -> Vec<Route> {
    let mut list = vec![Route::Lan];
    if i.cloud_print {
        list.push(Route::Cloud);
    }
    if i.connect_installed {
        list.push(Route::Connect);
    }
    list
}

pub fn plan(i: Inputs) -> Plan {
    let candidates = candidates(&i);
    let ready = |r: Route| blocker(&i, r).is_none();
    let route = i
        .chosen
        .filter(|r| candidates.contains(r))
        // Без вибору — те, що справді готове: живий LAN, хмара, Bambu Connect.
        .or_else(|| [Route::Lan, Route::Cloud, Route::Connect].into_iter().find(|r| candidates.contains(r) && ready(*r)))
        // Нічого не готово — маршрут, який найближчий до готовності (його дія відкриє шлях).
        .or_else(|| {
            if candidates.contains(&Route::Cloud) && i.cloud != Cloud::NotInAccount {
                Some(Route::Cloud)
            } else if i.lan != Lan::Absent || i.open_plugin {
                Some(Route::Lan)
            } else {
                None
            }
        });
    let blocker = match route {
        Some(r) => blocker(&i, r),
        None => Some(Blocker::NoRoute),
    };
    let notice = if route.is_none() && !i.open_plugin && !i.connect_installed {
        if i.os == Os::Linux {
            ConnectNotice::NoLinuxBuild
        } else {
            ConnectNotice::Download
        }
    } else {
        ConnectNotice::None
    };
    Plan {
        route,
        blocker,
        alternatives: candidates.into_iter().filter(|r| Some(*r) != route).collect(),
        notice,
        // Галочки — лише коли «Друкувати» справді надішле файл командою Slicenv.
        print_options: blocker.is_none() && matches!(route, Some(Route::Lan | Route::Cloud)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(lan: Lan, cloud: Cloud, cloud_print: bool) -> Inputs {
        Inputs {
            os: Os::Linux,
            connect_installed: false,
            lan,
            cloud,
            cloud_print,
            open_plugin: true,
            chosen: None,
        }
    }

    #[test]
    fn cloud_with_stale_saved_lan_prints_via_cloud() {
        // Скрин користувача: у Cloud, а збережений колись LAN-запис робив головною LAN.
        let p = plan(inputs(Lan::Saved { up: false }, Cloud::Ready, true));
        assert_eq!(p.route, Some(Route::Cloud));
        assert!(p.ready());
        assert!(p.print_options);
        assert_eq!(p.alternatives, vec![Route::Lan]);
    }

    #[test]
    fn live_lan_is_used_when_nothing_was_chosen() {
        let p = plan(inputs(Lan::Saved { up: true }, Cloud::PrinterOffline, true));
        assert_eq!(p.route, Some(Route::Lan));
        assert!(p.ready());
        // Принтер у Cloud-режимі може бути видно й у мережі: обидва маршрути готові,
        // хмару можна вибрати свідомо.
        let p = plan(inputs(Lan::Saved { up: true }, Cloud::Ready, true));
        assert_eq!(p.route, Some(Route::Lan));
        assert_eq!(p.alternatives, vec![Route::Cloud]);
    }

    #[test]
    fn lan_only_printer_is_offline_in_cloud_and_prints_over_lan() {
        let p = plan(inputs(Lan::Saved { up: true }, Cloud::PrinterOffline, true));
        assert_eq!(p.route, Some(Route::Lan));
        // Хмара вимкнена у відкритому плагіні — маршрут лише LAN, без хмарної альтернативи.
        let p = plan(inputs(Lan::Saved { up: true }, Cloud::NeedsSetup, false));
        assert_eq!(p.route, Some(Route::Lan));
        assert!(p.alternatives.is_empty());
    }

    #[test]
    fn explicit_choice_is_never_silently_replaced() {
        // Вибрано хмару, хмара ще підключається, а LAN живий: друкувати по LAN не можна.
        let mut i = inputs(Lan::Saved { up: true }, Cloud::Connecting, true);
        i.chosen = Some(Route::Cloud);
        let p = plan(i);
        assert_eq!(p.route, Some(Route::Cloud));
        assert_eq!(p.blocker, Some(Blocker::CloudConnecting));
        assert!(!p.ready());
        // Вибрано LAN, з’єднання немає, хмара готова — теж без підміни.
        let mut i = inputs(Lan::Saved { up: false }, Cloud::Ready, true);
        i.chosen = Some(Route::Lan);
        let p = plan(i);
        assert_eq!(p.route, Some(Route::Lan));
        assert_eq!(p.blocker, Some(Blocker::LanOffline));
        // Вибір, якого вже не існує (хмарний друк вимкнули), не тримається.
        let mut i = inputs(Lan::Saved { up: true }, Cloud::NeedsSetup, false);
        i.chosen = Some(Route::Cloud);
        assert_eq!(plan(i).route, Some(Route::Lan));
    }

    #[test]
    fn offline_everywhere_names_what_is_missing() {
        let p = plan(inputs(Lan::Saved { up: false }, Cloud::PrinterOffline, true));
        assert_eq!(p.route, Some(Route::Cloud));
        assert_eq!(p.blocker, Some(Blocker::CloudPrinterOffline));
        let p = plan(inputs(Lan::Saved { up: false }, Cloud::NeedsSetup, false));
        assert_eq!(p.route, Some(Route::Lan));
        assert_eq!(p.blocker, Some(Blocker::LanOffline));
        let p = plan(inputs(Lan::Found, Cloud::NeedsSetup, false));
        assert_eq!(p.blocker, Some(Blocker::LanAddFound));
        let p = plan(inputs(Lan::Absent, Cloud::NeedsSetup, true));
        assert_eq!(p.route, Some(Route::Cloud));
        assert_eq!(p.blocker, Some(Blocker::CloudSetup));
    }

    #[test]
    fn closed_plugin_never_offers_cloud_by_default() {
        for os in [Os::Windows, Os::Mac, Os::Linux] {
            for connect_installed in [true, false] {
                for lan in [Lan::Saved { up: true }, Lan::Saved { up: false }, Lan::Found, Lan::Absent] {
                    let p = plan(Inputs {
                        os,
                        connect_installed,
                        lan,
                        cloud: Cloud::Ready,
                        cloud_print: false,
                        open_plugin: false,
                        chosen: None,
                    });
                    assert_ne!(p.route, Some(Route::Cloud), "{os:?} {connect_installed} {lan:?}");
                    assert!(!p.alternatives.contains(&Route::Cloud));
                }
            }
        }
    }

    #[test]
    fn bambu_connect_when_no_direct_route_is_ready() {
        let i = Inputs {
            os: Os::Windows,
            connect_installed: true,
            lan: Lan::Absent,
            cloud: Cloud::Ready,
            cloud_print: false,
            open_plugin: false,
            chosen: None,
        };
        let p = plan(i);
        assert_eq!(p.route, Some(Route::Connect));
        assert!(p.ready());
        assert!(!p.print_options, "Connect URL carries no calibration flags");
        assert_eq!(p.alternatives, vec![Route::Lan]);
        // Живий LAN-принтер важливіший за Connect.
        let p = plan(Inputs { lan: Lan::Saved { up: true }, ..i });
        assert_eq!(p.route, Some(Route::Lan));
        assert_eq!(p.alternatives, vec![Route::Connect]);
    }

    #[test]
    fn missing_connect_explains_before_any_attempt() {
        let i = Inputs {
            os: Os::Windows,
            connect_installed: false,
            lan: Lan::Absent,
            cloud: Cloud::NeedsSetup,
            cloud_print: false,
            open_plugin: false,
            chosen: None,
        };
        let p = plan(i);
        assert_eq!(p.route, None);
        assert_eq!(p.blocker, Some(Blocker::NoRoute));
        assert_eq!(p.notice, ConnectNotice::Download);
        assert!(!p.print_options);
        assert_eq!(plan(Inputs { os: Os::Linux, ..i }).notice, ConnectNotice::NoLinuxBuild);
        // Знайдений у мережі принтер — маршрут LAN із дією «додати», без пояснення про Connect.
        let p = plan(Inputs { lan: Lan::Found, ..i });
        assert_eq!(p.route, Some(Route::Lan));
        assert_eq!(p.blocker, Some(Blocker::LanAddFound));
        assert_eq!(p.notice, ConnectNotice::None);
    }

    #[test]
    fn options_keep_their_order_whatever_is_chosen() {
        let i = demo("cloud", None).unwrap();
        for chosen in [None, Some(Route::Lan), Some(Route::Cloud), Some(Route::Connect)] {
            let p = plan(Inputs { chosen, ..i });
            assert_eq!(p.options(), vec![Route::Lan, Route::Cloud, Route::Connect], "{chosen:?}");
        }
        // Немає маршруту — LAN усе одно в списку: принтер можна додати.
        let p = plan(demo("none", None).unwrap());
        assert_eq!(p.route, None);
        assert_eq!(p.options(), vec![Route::Lan]);
    }

    #[test]
    fn demo_states_cover_every_dialog_state() {
        let state = |name| {
            let p = plan(demo(name, None).unwrap());
            (p.route, p.blocker, p.notice)
        };
        assert_eq!(state("connect"), (Some(Route::Connect), None, ConnectNotice::None));
        assert_eq!(state("lan"), (Some(Route::Lan), None, ConnectNotice::None));
        assert_eq!(state("lan-offline"), (Some(Route::Lan), Some(Blocker::LanOffline), ConnectNotice::None));
        assert_eq!(state("lan-found"), (Some(Route::Lan), Some(Blocker::LanAddFound), ConnectNotice::None));
        assert_eq!(state("cloud"), (Some(Route::Cloud), None, ConnectNotice::None));
        assert_eq!(state("cloud-setup"), (Some(Route::Cloud), Some(Blocker::CloudSetup), ConnectNotice::None));
        assert_eq!(state("cloud-connecting").1, Some(Blocker::CloudConnecting));
        assert_eq!(state("cloud-offline").1, Some(Blocker::CloudPrinterOffline));
        assert_eq!(state("open-plugin"), (Some(Route::Lan), Some(Blocker::LanAddManual), ConnectNotice::None));
        assert_eq!(state("none"), (None, Some(Blocker::NoRoute), ConnectNotice::NoLinuxBuild));
        assert_eq!(state("none-download").2, ConnectNotice::Download);
        assert!(demo("unknown", None).is_none());
    }
}
