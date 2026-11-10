//! Стан джерела «Bambu Cloud» — одна правда для всього інтерфейсу.
//!
//! Його обчислює робочий потік хмари (`device::worker`); док, майстер, діалог друку й рядок
//! стану лише читають. Тут також чисті рішення, які раніше були розкидані по колбеках
//! майстра: коли вмикати хмару відкритого плагіна, що робить потік у режимі очікування,
//! який принтер вибрати за замовчуванням. Без gpui — тестуються звичайними тестами.

use crate::i18n::tr;

/// Що зараз із хмарою. `Connected` означає, що сервер справді відповів, а не «є файл сесії».
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    /// Робочий потік ще не повідомив стану.
    #[default]
    Starting,
    /// `SLICENV_CLOUD_OFF`: тестові запуски без хмари.
    Disabled,
    /// Плагіна немає (або версія не підтримується — тоді текст).
    NeedPlugin(Option<String>),
    /// Сесії немає або вона прострочена.
    NeedLogin { expired: bool },
    /// Відкритий плагін вибрано, а хмару в ньому вимкнено (друк — по LAN).
    CloudOff,
    Connecting,
    /// Плагін і вхід є, але сервер не підтвердив з’єднання; потік чекає далі.
    NoServer,
    Connected,
    /// З’єднано, але список принтерів відмовив; повтор через `secs` с.
    ListRetry { secs: u64, cached: bool },
    /// Інша помилка (текст-джерело українською, перекладається під час показу).
    Failed(String),
}

/// Одна дія, яку інтерфейс пропонує для поточного стану.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Відкрити налаштування хмари (плагін, вхід).
    Setup,
    SignIn,
    /// Увімкнути хмару відкритого плагіна — одна пряма дія, без майстра.
    EnableCloud,
    Retry,
}

impl Phase {
    /// Короткий текст для рядка джерела (перекладений).
    pub fn text(&self) -> String {
        match self {
            Phase::Starting => tr("запуск…"),
            Phase::Disabled => tr("вимкнено (SLICENV_CLOUD_OFF)"),
            Phase::NeedPlugin(None) => tr("не налаштовано"),
            Phase::NeedPlugin(Some(version)) => {
                crate::tr_format!("плагін {version} не підтримується")
            }
            Phase::NeedLogin { expired: false } => tr("потрібен вхід"),
            Phase::NeedLogin { expired: true } => tr("сесія прострочена — увійдіть знову"),
            Phase::CloudOff => tr("вимкнено у відкритому плагіні"),
            Phase::Connecting => tr("підключення…"),
            Phase::NoServer => tr("сервер не відповідає, чекаю"),
            Phase::Connected => tr("підключено"),
            Phase::ListRetry { secs, .. } => {
                crate::tr_format!("список принтерів недоступний, повтор через {secs} с")
            }
            Phase::Failed(text) => tr(text),
        }
    }

    /// Спінер — лише коли справді йде підключення.
    pub fn busy(&self) -> bool {
        matches!(self, Phase::Starting | Phase::Connecting)
    }

    /// Стан, що вимагає уваги (жовтий/червоний), а не нейтральний.
    pub fn problem(&self) -> bool {
        matches!(
            self,
            Phase::NoServer | Phase::ListRetry { .. } | Phase::Failed(_)
        ) || matches!(self, Phase::NeedLogin { expired: true })
    }

    pub fn connected(&self) -> bool {
        matches!(self, Phase::Connected | Phase::ListRetry { .. })
    }

    pub fn action(&self) -> Option<Action> {
        match self {
            Phase::NeedPlugin(_) => Some(Action::Setup),
            Phase::NeedLogin { .. } => Some(Action::SignIn),
            Phase::CloudOff => Some(Action::EnableCloud),
            Phase::NoServer | Phase::Failed(_) | Phase::ListRetry { .. } => Some(Action::Retry),
            Phase::Starting | Phase::Disabled | Phase::Connecting | Phase::Connected => None,
        }
    }
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Action::Setup => "Налаштувати…",
            Action::SignIn => "Увійти",
            Action::EnableCloud => "Увімкнути хмару",
            Action::Retry => "Повторити",
        }
    }
}

/// Звідки взявся вхід в акаунт.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Login {
    /// Користувач щойно ввійшов (пароль, браузер) або взяв сесію Studio/Orca.
    Fresh,
    /// Перевірка вже збереженої сесії при відкритті вкладки.
    Existing,
}

/// Що зробити після входу.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AfterLogin {
    /// Увімкнути хмару відкритого плагіна (лише свіжий вхід — це явний запит на хмару).
    pub enable_cloud: bool,
    /// Перепідключити робочий потік. Перевірка старої сесії нічого не міняє — і не рве
    /// робоче з’єднання.
    pub reconnect: bool,
}

pub fn after_login(login: Login, open_plugin: bool, cloud_enabled: bool) -> AfterLogin {
    match login {
        Login::Fresh => AfterLogin {
            enable_cloud: open_plugin && !cloud_enabled,
            reconnect: true,
        },
        Login::Existing => AfterLogin {
            enable_cloud: false,
            reconnect: false,
        },
    }
}

/// Принтер за замовчуванням — однаково в потоці й в інтерфейсі: перший онлайн, інакше перший.
pub fn default_printer(list: &[cloud::Printer]) -> Option<cloud::DeviceId> {
    list.iter()
        .find(|p| p.online)
        .or(list.first())
        .map(|p| p.dev_id.clone())
}

/// Що зробити з вибором принтера після нового списку акаунта.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reselect {
    Keep,
    /// Нічого не було вибрано: взяти принтер за замовчуванням (потік вибрав той самий).
    Pick(cloud::DeviceId),
    /// Вибраного принтера більше немає (вихід, інший акаунт, хмару вимкнено): замінити й
    /// сказати потоку. `None` — список порожній.
    Replace(Option<cloud::DeviceId>),
}

/// `all` — об’єднаний список доку (хмара + LAN + Klipper), `account` — щойно отриманий
/// список акаунта.
pub fn reselect(
    selected: Option<&cloud::DeviceId>,
    all: &[cloud::Printer],
    account: &[cloud::Printer],
) -> Reselect {
    match selected {
        None => default_printer(account).map_or(Reselect::Keep, Reselect::Pick),
        Some(dev) if all.iter().any(|p| &p.dev_id == dev) => Reselect::Keep,
        Some(_) => Reselect::Replace(default_printer(account).or_else(|| default_printer(all))),
    }
}

/// Ключ акаунта для кешу списку принтерів: інший вхід — інший кеш. Сам ідентифікатор у файл
/// не пишеться.
pub fn account_key(user_id: &str) -> String {
    use std::hash::{Hash as _, Hasher as _};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    "slicenv-printers".hash(&mut h);
    user_id.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Чи кеш належить цьому акаунту. Старий формат (масив без ключа) не належить нікому.
pub fn cache_for(json: &serde_json::Value, account: &str) -> Option<Vec<cloud::Printer>> {
    if json.get("account")?.as_str()? != account {
        return None;
    }
    serde_json::from_value(json.get("printers")?.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn printer(id: &str, online: bool) -> cloud::Printer {
        cloud::Printer {
            dev_id: cloud::DeviceId::new(id),
            name: String::new(),
            model: "N2S".into(),
            online,
        }
    }

    #[test]
    fn fresh_login_turns_on_open_plugin_cloud_once() {
        let a = after_login(Login::Fresh, true, false);
        assert!(a.enable_cloud && a.reconnect);
        // Уже ввімкнено — не чіпаємо налаштування, але сесія нова: перепідключення.
        let a = after_login(Login::Fresh, true, true);
        assert!(!a.enable_cloud && a.reconnect);
        // Закритий плагін: перемикача немає.
        let a = after_login(Login::Fresh, false, false);
        assert!(!a.enable_cloud && a.reconnect);
    }

    #[test]
    fn checking_saved_session_never_changes_consent_or_reconnects() {
        for (open, on) in [(true, false), (true, true), (false, false)] {
            let a = after_login(Login::Existing, open, on);
            assert!(!a.enable_cloud, "перевірка сесії ввімкнула хмару");
            assert!(!a.reconnect, "перевірка сесії порвала з’єднання");
        }
    }

    #[test]
    fn default_printer_matches_worker_rule() {
        assert_eq!(default_printer(&[]), None);
        // Єдиний офлайн — усе одно вибраний (раніше інтерфейс лишав порожньо).
        assert_eq!(
            default_printer(&[printer("A", false)]).unwrap().as_str(),
            "A"
        );
        assert_eq!(
            default_printer(&[printer("A", false), printer("B", true)])
                .unwrap()
                .as_str(),
            "B"
        );
        let ten: Vec<_> = (0..10).map(|i| printer(&format!("P{i}"), i == 7)).collect();
        assert_eq!(default_printer(&ten).unwrap().as_str(), "P7");
    }

    #[test]
    fn cache_is_bound_to_account() {
        let me = account_key("1001");
        let other = account_key("2002");
        assert_ne!(me, other);
        assert!(!me.contains("1001"));
        let json = serde_json::json!({
            "account": me,
            "printers": [{"dev_id": "A", "dev_name": "", "dev_model_name": "N2S", "dev_online": true}],
        });
        assert_eq!(cache_for(&json, &me).unwrap().len(), 1);
        assert!(cache_for(&json, &other).is_none());
        // Старий кеш без ключа акаунта не підхоплюється.
        let legacy = serde_json::json!([{"dev_id": "A"}]);
        assert!(cache_for(&legacy, &me).is_none());
    }

    #[test]
    fn selection_follows_account_list_for_0_1_10_printers() {
        let id = |s: &str| cloud::DeviceId::new(s);
        // 0 принтерів, нічого не вибрано — нічого не вигадуємо.
        assert_eq!(reselect(None, &[], &[]), Reselect::Keep);
        // 1 офлайн — вибирається, як у потоці (D7).
        let one = [printer("A", false)];
        assert_eq!(reselect(None, &one, &one), Reselect::Pick(id("A")));
        // 10, вибраний є в списку — не чіпаємо.
        let ten: Vec<_> = (0..10).map(|i| printer(&format!("P{i}"), i % 3 == 0)).collect();
        assert_eq!(reselect(Some(&id("P5")), &ten, &ten), Reselect::Keep);
        // Вихід / інший акаунт: вибраного більше немає — замінюємо, а не «підключення…».
        assert_eq!(reselect(Some(&id("OLD")), &ten, &ten), Reselect::Replace(Some(id("P0"))));
        assert_eq!(reselect(Some(&id("OLD")), &[], &[]), Reselect::Replace(None));
        // Вибраний LAN-принтер є в об’єднаному списку, хоч акаунт порожній — лишається.
        let lan = [printer("LAN1", true)];
        assert_eq!(reselect(Some(&id("LAN1")), &lan, &[]), Reselect::Keep);
    }

    #[test]
    fn spinner_and_actions_follow_phase() {
        assert!(Phase::Connecting.busy());
        for idle in [
            Phase::Disabled,
            Phase::CloudOff,
            Phase::Connected,
            Phase::NeedLogin { expired: false },
            Phase::NoServer,
        ] {
            assert!(!idle.busy(), "{idle:?}: вічний спінер");
        }
        assert_eq!(Phase::CloudOff.action(), Some(Action::EnableCloud));
        assert_eq!(
            Phase::NeedLogin { expired: true }.action(),
            Some(Action::SignIn)
        );
        assert_eq!(Phase::NoServer.action(), Some(Action::Retry));
        assert_eq!(Phase::Connected.action(), None);
        assert!(!Phase::NoServer.connected());
        assert!(Phase::ListRetry { secs: 60, cached: true }.connected());
    }
}
