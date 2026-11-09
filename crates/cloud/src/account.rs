//! Акаунт Bambu: знайдені сесії, власне сховище сесії й вхід без webview.
//!
//! Сесію (`BambuNetworkEngine.conf`) шифрує й читає сам плагін; Rust її не розшифровує.
//! Наша сесія живе в `~/.local/share/slicenv/cloud/session/` (тека 0700, файли 0600) і
//! передається плагіну як його config dir. Сесії Bambu Studio й OrcaSlicer лише читаються:
//! «використати» = скопіювати файл до себе.
//!
//! Вхід (як у Orca, але без webview): REST `user-service/user/login` → за потреби другий
//! крок із кодом із пошти або 2FA → токени → `Client::login_with_tokens` (там профіль
//! через плагін `get_my_profile` і `change_user` з payload як у Orca `HttpServer`).
use crate::http::{Request, Response, Transport};
use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fmt, fs,
    io::Write as _,
    path::{Path, PathBuf},
    time::SystemTime,
};
use paths::fs_mode::ModeExt as _;

pub const SESSION_FILE: &str = "BambuNetworkEngine.conf";
const META: &str = "session.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionOrigin {
    /// Вхід через нашу форму.
    Own,
    /// Скопійована сесія Bambu Studio.
    BambuStudio,
    /// Скопійована сесія OrcaSlicer.
    OrcaSlicer,
}

/// Знайдена сесія. Без персональних даних: нік відомий лише після завантаження плагіна
/// (`Client::user_nickname`), бо файл зашифрований.
#[derive(Clone, Debug)]
pub struct FoundSession {
    pub origin: SessionOrigin,
    /// Тека з `BambuNetworkEngine.conf` (config dir плагіна).
    pub dir: PathBuf,
    pub modified: Option<SystemTime>,
    /// Код країни для плагіна (US/CN/Others).
    pub country_code: String,
    /// Для власної — звідки її взяли.
    pub adopted_from: Option<SessionOrigin>,
}

impl FoundSession {
    /// Власну теку плагін може змінювати (оновлення токена); чужу — лише копією.
    pub fn is_own(&self) -> bool {
        self.origin == SessionOrigin::Own
    }
}

#[derive(Serialize, Deserialize)]
struct Meta {
    origin: SessionOrigin,
    country_code: String,
}

pub fn own_session_dir() -> PathBuf {
    paths::data_dir().join("cloud/session")
}

fn session_file_ok(dir: &Path) -> Option<fs::Metadata> {
    fs::symlink_metadata(dir.join(SESSION_FILE))
        .ok()
        .filter(|m| m.is_file() && m.len() > 0)
}

fn foreign(dir: PathBuf, origin: SessionOrigin, conf: &str) -> Option<FoundSession> {
    let meta = session_file_ok(&dir)?;
    Some(FoundSession {
        origin,
        country_code: crate::session::country_code_from(&dir.join(conf)),
        modified: meta.modified().ok(),
        dir,
        adopted_from: None,
    })
}

fn own_in(dir: &Path) -> Option<FoundSession> {
    let file = session_file_ok(dir)?;
    let meta: Option<Meta> = fs::read(dir.join(META))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    Some(FoundSession {
        origin: SessionOrigin::Own,
        dir: dir.to_owned(),
        modified: file.modified().ok(),
        country_code: meta
            .as_ref()
            .map_or_else(|| "Others".into(), |m| m.country_code.clone()),
        adopted_from: meta.map(|m| m.origin).filter(|o| *o != SessionOrigin::Own),
    })
}

/// Сесії за пріоритетом: власна → Bambu Studio → OrcaSlicer.
pub fn find_sessions() -> Vec<FoundSession> {
    // Відкритий плагін має власну сесію (`obn.auth.json`); сесії закритого йому непридатні.
    if crate::obn::active().is_some() {
        return obn_session().into_iter().collect();
    }
    let config = paths::vendor_config_home();
    own_in(&own_session_dir())
        .into_iter()
        .chain(foreign(
            config.join("BambuStudio"),
            SessionOrigin::BambuStudio,
            "BambuStudio.conf",
        ))
        .chain(foreign(
            config.join("OrcaSlicer"),
            SessionOrigin::OrcaSlicer,
            "OrcaSlicer.conf",
        ))
        .collect()
}

/// Сесія відкритого плагіна у [`paths::open_plugin_config_dir`].
fn obn_session() -> Option<FoundSession> {
    let dir = paths::open_plugin_config_dir();
    let file = fs::symlink_metadata(dir.join(crate::obn::SESSION_FILE))
        .ok()
        .filter(|m| m.is_file() && m.len() > 2)?;
    Some(FoundSession {
        origin: SessionOrigin::Own,
        modified: file.modified().ok(),
        country_code: country_of(&dir),
        dir,
        adopted_from: None,
    })
}

/// Код країни, записаний під час входу (`prepare_own_session`), інакше «Others».
pub(crate) fn country_of(dir: &Path) -> String {
    fs::read(dir.join(META))
        .ok()
        .and_then(|b| serde_json::from_slice::<Meta>(&b).ok())
        .map_or_else(|| "Others".into(), |m| m.country_code)
}

pub(crate) fn private_dir(dir: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .unix_mode(0o700)
        .create(dir)
        .context("не вдалося створити теку сесії")?;
    paths::fs_mode::set_mode(dir, 0o700)?;
    Ok(())
}

fn write_meta(dir: &Path, meta: &Meta) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .unix_mode(0o600)
        .open(dir.join(META))?;
    f.write_all(&serde_json::to_vec(meta)?)?;
    Ok(())
}

/// Підготувати власну теку перед входом через форму (плагін запише туди сесію).
pub fn prepare_own_session(country_code: &str) -> Result<PathBuf> {
    let dir = own_session_dir();
    prepare_session_in(&dir, country_code)?;
    Ok(dir)
}

pub(crate) fn prepare_session_in(dir: &Path, country_code: &str) -> Result<()> {
    private_dir(dir)?;
    write_meta(
        dir,
        &Meta {
            origin: SessionOrigin::Own,
            country_code: country_code.into(),
        },
    )
}

/// «Використати сесію Bambu Studio/Orca»: копія файлу у власну теку (0600). Оригінал не
/// змінюється. Увага: це той самий токен, тож вихід на сервері розлогінить і Studio.
pub fn adopt_session(found: &FoundSession) -> Result<FoundSession> {
    adopt_into(found, &own_session_dir())
}

fn adopt_into(found: &FoundSession, dir: &Path) -> Result<FoundSession> {
    if found.is_own() {
        return Ok(found.clone());
    }
    private_dir(dir)?;
    let tmp = dir.join(format!(".{SESSION_FILE}.tmp{}", std::process::id()));
    crate::session::copy_regular(&found.dir.join(SESSION_FILE), &tmp)?;
    fs::rename(&tmp, dir.join(SESSION_FILE))?;
    write_meta(
        dir,
        &Meta {
            origin: found.origin,
            country_code: found.country_code.clone(),
        },
    )?;
    own_in(dir).context("сесію не скопійовано")
}

/// Вихід з акаунта локально: видаляє власну сесію. Чужі (Studio/Orca) не чіпає.
pub fn forget_own_session() -> Result<()> {
    forget_in(&own_session_dir())
}

fn forget_in(dir: &Path) -> Result<()> {
    for name in [SESSION_FILE, META] {
        match fs::remove_file(dir.join(name)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    }
    Ok(())
}

/// Токени акаунта. Debug нічого не показує.
#[derive(Clone, PartialEq)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: f64,
    pub refresh_expires_in: f64,
}

impl fmt::Debug for Tokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Tokens([redacted])")
    }
}

/// Профіль з `get_my_profile` (поля як читає Orca: uidStr, name, account, avatar).
#[derive(Clone, Default, PartialEq)]
pub struct Profile {
    pub uid: String,
    pub name: String,
    pub account: String,
    pub avatar: String,
}

impl fmt::Debug for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Profile([redacted])")
    }
}

pub(crate) fn parse_profile(body: &str) -> Result<Profile> {
    let v: Value = serde_json::from_str(body)
        .map_err(|_| anyhow::anyhow!("профіль Bambu: некоректна відповідь"))?;
    let s = |k: &str| v[k].as_str().unwrap_or_default().to_owned();
    // uidStr — рядок; деякі відповіді мають лише числовий uid.
    let uid = Some(s("uidStr"))
        .filter(|u| !u.is_empty())
        .or_else(|| v["uid"].as_u64().map(|u| u.to_string()))
        .unwrap_or_default();
    if uid.is_empty() {
        bail!("профіль Bambu без ідентифікатора користувача");
    }
    Ok(Profile {
        uid,
        name: s("name"),
        account: s("account"),
        avatar: s("avatar"),
    })
}

/// Payload для `bambu_network_change_user`, як у Orca (`WebUserLoginDialog` user_ticket_login /
/// `HttpServer` ticket-гілка): expires_in — рядок із double (`std::to_string(double)`).
pub(crate) fn change_user_payload(tokens: &Tokens, profile: &Profile) -> String {
    json!({
        "command": "user_login",
        "data": {
            "token": tokens.access_token,
            "refresh_token": tokens.refresh_token,
            "expires_in": format!("{:.6}", tokens.expires_in),
            "refresh_expires_in": format!("{:.6}", tokens.refresh_expires_in),
            "user": {
                "uid": profile.uid,
                "name": profile.name,
                "account": profile.account,
                "avatar": profile.avatar,
            }
        }
    })
    .to_string()
}

/// Типізовані помилки входу: майстер реагує на кожну по-своєму.
#[derive(Debug, PartialEq, Eq)]
pub enum LoginError {
    /// Невірний email або пароль.
    WrongCredentials,
    /// Код з пошти прострочений: новий уже надіслано.
    CodeExpired,
    CodeIncorrect,
    /// Сервер не дав токен (неочікувана відповідь).
    Unexpected,
    /// Cloudflare/антибот або блокування (HTTP 403/429).
    Blocked(u16),
    Server(u16),
    Network,
    /// Квиток входу з браузера недійсний або прострочений.
    TicketRejected,
}

impl fmt::Display for LoginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongCredentials => f.write_str("Невірний email або пароль"),
            Self::CodeExpired => f.write_str("Код прострочений — новий код надіслано на пошту"),
            Self::CodeIncorrect => f.write_str("Невірний код підтвердження"),
            Self::Unexpected => {
                f.write_str("Сервер Bambu відповів неочікувано — спробуйте пізніше")
            }
            Self::Blocked(s) => write!(
                f,
                "Сервер Bambu тимчасово відхиляє вхід (HTTP {s}) — спробуйте пізніше"
            ),
            Self::Server(s) => write!(f, "Помилка сервера Bambu (HTTP {s})"),
            Self::Network => f.write_str("Немає з’єднання з сервером Bambu"),
            Self::TicketRejected => {
                f.write_str("Bambu не прийняв квиток входу з браузера — увійдіть ще раз")
            }
        }
    }
}
impl std::error::Error for LoginError {}

/// Результат кроку входу.
#[derive(Debug)]
pub enum LoginStep {
    Done(Tokens),
    /// Код надіслано на пошту — крок 2: [`Login::email_code`].
    EmailCode,
    /// Увімкнено 2FA (застосунок-автентифікатор) — крок 2: [`Login::tfa_code`].
    Tfa(TfaKey),
}

/// Непрозорий ключ другого кроку 2FA.
#[derive(Clone)]
pub struct TfaKey(String);
impl fmt::Debug for TfaKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TfaKey([redacted])")
    }
}

/// REST-вхід без webview. Формат перевірено за відкритими клієнтами (ha-bambulab,
/// OpenBambuAPI) і живою відповіддю на вигаданий акаунт; справжній вхід паролем
/// наживо не перевірявся (див. docs/CLOUD.md).
pub struct Login<'t> {
    transport: &'t dyn Transport,
    api: &'static str,
    web: &'static str,
    headers: Vec<(String, String)>,
}

impl<'t> Login<'t> {
    pub fn new(transport: &'t dyn Transport, country_code: &str) -> Self {
        let cn = country_code == "CN";
        Self {
            transport,
            api: if cn {
                "https://api.bambulab.cn"
            } else {
                "https://api.bambulab.com"
            },
            web: if cn {
                "https://bambulab.cn"
            } else {
                "https://bambulab.com"
            },
            headers: vec![
                ("Content-Type".into(), "application/json".into()),
                ("X-BBL-Client-Type".into(), "slicer".into()),
                ("X-BBL-Client-Name".into(), "BambuStudio".into()),
                ("X-BBL-OS-Type".into(), crate::plugin::OS_TYPE.into()),
            ],
        }
    }

    fn post(
        &self,
        url: &str,
        body: &Value,
        extra: &[(String, String)],
    ) -> Result<Response, LoginError> {
        let mut headers = self.headers.clone();
        headers.extend_from_slice(extra);
        let body = body.to_string();
        self.transport
            .request(&Request::post(url, &headers, &body))
            .map_err(|_| LoginError::Network)
    }

    /// Крок 1: email + пароль.
    pub fn password(&self, account: &str, password: &str) -> Result<LoginStep, LoginError> {
        let r = self.post(
            &format!("{}/v1/user-service/user/login", self.api),
            &json!({"account": account, "password": password, "apiError": ""}),
            &[],
        )?;
        let v = json_of(&r)?;
        if let Some(tokens) = tokens_of(&v) {
            return Ok(LoginStep::Done(tokens));
        }
        match v["loginType"].as_str() {
            Some("verifyCode") => {
                self.send_email_code(account)?;
                Ok(LoginStep::EmailCode)
            }
            Some("tfa") => v["tfaKey"]
                .as_str()
                .filter(|k| !k.is_empty())
                .map(|k| LoginStep::Tfa(TfaKey(k.into())))
                .ok_or(LoginError::Unexpected),
            _ => Err(LoginError::Unexpected),
        }
    }

    /// Надіслати (повторно) код на пошту.
    pub fn send_email_code(&self, account: &str) -> Result<(), LoginError> {
        let r = self.post(
            &format!("{}/v1/user-service/user/sendemail/code", self.api),
            &json!({"email": account, "type": "codeLogin"}),
            &[],
        )?;
        status_ok(&r).map(|_| ())
    }

    /// Крок 2: код із пошти.
    pub fn email_code(&self, account: &str, code: &str) -> Result<Tokens, LoginError> {
        let r = self.post(
            &format!("{}/v1/user-service/user/login", self.api),
            &json!({"account": account, "code": code.trim()}),
            &[],
        )?;
        if r.status == 400 {
            let v: Value = serde_json::from_slice(&r.body).unwrap_or_default();
            return Err(match v["code"].as_i64() {
                Some(1) => {
                    // Як у ha-bambulab: прострочений код — одразу надсилаємо новий.
                    let _ = self.send_email_code(account);
                    LoginError::CodeExpired
                }
                Some(2) => LoginError::CodeIncorrect,
                _ => LoginError::Unexpected,
            });
        }
        tokens_of(&json_of(&r)?).ok_or(LoginError::Unexpected)
    }

    /// Вхід через браузер ([`crate::browser_login`]): короткий квиток → токени. Той самий
    /// відкритий запит, що робить плагін у `get_my_token` (Orca `HttpServer`, Bambu Studio
    /// `TicketLoginTask`) і сайт bambulab.com (`ticketLogin`): без секрету клієнта.
    pub fn ticket(&self, ticket: &str) -> Result<Tokens, LoginError> {
        if !crate::browser_login::valid_ticket(ticket) {
            return Err(LoginError::TicketRejected);
        }
        let r = self.post(
            &format!("{}/v1/user-service/user/ticket/{ticket}", self.api),
            &json!({"ticket": ticket}),
            &[],
        )?;
        if matches!(r.status, 400 | 401 | 404) {
            return Err(LoginError::TicketRejected);
        }
        tokens_of(&json_of(&r)?).ok_or(LoginError::Unexpected)
    }

    /// Крок 2: код 2FA. Сайт віддає токен у cookie `token` (без refresh-токена).
    pub fn tfa_code(&self, key: &TfaKey, code: &str) -> Result<Tokens, LoginError> {
        let csrf = self
            .transport
            .request(&Request::get(
                &format!("{}/api/csrf", self.web),
                &self.headers,
            ))
            .map_err(|_| LoginError::Network)?;
        let csrf = status_ok(&csrf)?
            .cookie("bbl_csrf_token")
            .map(str::to_owned)
            .ok_or(LoginError::Unexpected)?;
        let r = self.post(
            &format!("{}/api/sign-in/tfa", self.web),
            &json!({"tfaKey": key.0, "tfaCode": code.trim()}),
            &[
                ("x-bbl-csrf-token".into(), csrf.clone()),
                ("Cookie".into(), format!("bbl_csrf_token={csrf}")),
            ],
        )?;
        if r.status == 400 {
            return Err(LoginError::CodeIncorrect);
        }
        let token = status_ok(&r)?
            .cookie("token")
            .filter(|t| !t.is_empty())
            .ok_or(LoginError::Unexpected)?;
        Ok(Tokens {
            access_token: token.into(),
            refresh_token: String::new(),
            expires_in: 0.0,
            refresh_expires_in: 0.0,
        })
    }
}

fn status_ok(r: &Response) -> Result<&Response, LoginError> {
    match r.status {
        200..=299 => Ok(r),
        400 | 401 => Err(LoginError::WrongCredentials),
        403 | 429 => Err(LoginError::Blocked(r.status)),
        s => Err(LoginError::Server(s)),
    }
}

fn json_of(r: &Response) -> Result<Value, LoginError> {
    status_ok(r)?;
    serde_json::from_slice(&r.body).map_err(|_| LoginError::Unexpected)
}

fn tokens_of(v: &Value) -> Option<Tokens> {
    let access = v["accessToken"].as_str().filter(|t| !t.is_empty())?;
    Some(Tokens {
        access_token: access.into(),
        refresh_token: v["refreshToken"].as_str().unwrap_or_default().into(),
        expires_in: v["expiresIn"].as_f64().unwrap_or(0.0),
        refresh_expires_in: v["refreshExpiresIn"].as_f64().unwrap_or(0.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::Method;
    use std::sync::{Mutex, atomic::AtomicBool};

    /// Відповіді за шляхом; записує тіла запитів для перевірки.
    struct Mock {
        replies: Mutex<Vec<(&'static str, u16, Vec<(String, String)>, &'static str)>>,
        seen: Mutex<Vec<(Method, String, String)>>,
    }
    impl Mock {
        fn new(replies: Vec<(&'static str, u16, Vec<(String, String)>, &'static str)>) -> Self {
            Self {
                replies: Mutex::new(replies),
                seen: Mutex::default(),
            }
        }
    }
    impl Transport for Mock {
        fn request(&self, r: &Request<'_>) -> Result<Response> {
            self.seen.lock().unwrap().push((
                r.method,
                r.url.to_owned(),
                r.body.unwrap_or_default().to_owned(),
            ));
            let mut replies = self.replies.lock().unwrap();
            let i = replies
                .iter()
                .position(|(path, ..)| r.url.ends_with(path))
                .expect("unexpected request");
            let (_, status, headers, body) = replies.remove(i);
            Ok(Response {
                status,
                headers,
                body: body.as_bytes().to_vec(),
            })
        }
        fn download(
            &self,
            _: &str,
            _: &[(String, String)],
            _: u64,
            _: &mut dyn std::io::Write,
            _: &mut dyn FnMut(u64),
            _: &AtomicBool,
        ) -> Result<u64> {
            unreachable!()
        }
    }

    const TOKENS: &str = r#"{"accessToken":"syn-access","refreshToken":"syn-refresh","expiresIn":7776000,"refreshExpiresIn":7776000,"loginType":""}"#;

    #[test]
    fn password_login_without_second_step() {
        let mock = Mock::new(vec![("/user/login", 200, vec![], TOKENS)]);
        let step = Login::new(&mock, "US")
            .password("a@example.invalid", "p\"w")
            .unwrap();
        let LoginStep::Done(t) = step else { panic!() };
        assert_eq!(t.access_token, "syn-access");
        assert_eq!(t.expires_in, 7776000.0);
        assert!(!format!("{t:?}").contains("syn"));
        let seen = mock.seen.lock().unwrap();
        assert!(seen[0].1.starts_with("https://api.bambulab.com/"));
        let body: Value = serde_json::from_str(&seen[0].2).unwrap();
        assert_eq!(body["password"], "p\"w");
    }

    #[test]
    fn browser_ticket_is_exchanged_without_secrets_and_rejections_are_typed() {
        let mock = Mock::new(vec![
            ("/user/ticket/Ab12cD", 200, vec![], TOKENS),
            ("/user/ticket/Old999", 400, vec![], r#"{"code":2,"error":"ticket expired"}"#),
            ("/user/ticket/Slow00", 503, vec![], "{}"),
        ]);
        let login = Login::new(&mock, "US");
        let t = login.ticket("Ab12cD").unwrap();
        assert_eq!(t.refresh_token, "syn-refresh");
        assert_eq!(login.ticket("Old999").unwrap_err(), LoginError::TicketRejected);
        assert_eq!(login.ticket("Slow00").unwrap_err(), LoginError::Server(503));
        // Квиток зі сторонніми символами не йде в URL.
        assert_eq!(login.ticket("../x").unwrap_err(), LoginError::TicketRejected);
        let seen = mock.seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0].0, Method::Post);
        assert_eq!(
            seen[0].1,
            "https://api.bambulab.com/v1/user-service/user/ticket/Ab12cD"
        );
        let body: Value = serde_json::from_str(&seen[0].2).unwrap();
        assert_eq!(body, json!({"ticket": "Ab12cD"}));
    }

    #[test]
    fn email_code_flow_sends_code_and_maps_errors() {
        let mock = Mock::new(vec![
            (
                "/user/login",
                200,
                vec![],
                r#"{"accessToken":"","loginType":"verifyCode"}"#,
            ),
            ("/sendemail/code", 200, vec![], "{}"),
            ("/user/login", 400, vec![], r#"{"code":2,"error":"x"}"#),
            ("/user/login", 400, vec![], r#"{"code":1,"error":"x"}"#),
            ("/sendemail/code", 200, vec![], "{}"),
            ("/user/login", 200, vec![], TOKENS),
        ]);
        let login = Login::new(&mock, "CN");
        assert!(matches!(
            login.password("a@example.invalid", "pw").unwrap(),
            LoginStep::EmailCode
        ));
        assert_eq!(
            login.email_code("a@example.invalid", "000000").unwrap_err(),
            LoginError::CodeIncorrect
        );
        assert_eq!(
            login.email_code("a@example.invalid", "111111").unwrap_err(),
            LoginError::CodeExpired
        );
        let t = login.email_code("a@example.invalid", " 123456 ").unwrap();
        assert_eq!(t.refresh_token, "syn-refresh");
        let seen = mock.seen.lock().unwrap();
        assert!(
            seen.iter()
                .all(|s| s.1.starts_with("https://api.bambulab.cn/"))
        );
        let code: Value = serde_json::from_str(&seen[5].2).unwrap();
        assert_eq!(code["code"], "123456");
        assert!(code.get("password").is_none());
        let send: Value = serde_json::from_str(&seen[1].2).unwrap();
        assert_eq!(send["type"], "codeLogin");
    }

    #[test]
    fn wrong_password_and_blocking_are_typed() {
        let mock = Mock::new(vec![
            (
                "/user/login",
                400,
                vec![],
                r#"{"code":1,"error":"Incorrect account or password."}"#,
            ),
            ("/user/login", 403, vec![], "<html>cf</html>"),
            ("/user/login", 200, vec![], "not json"),
        ]);
        let login = Login::new(&mock, "US");
        assert_eq!(
            login.password("a", "b").unwrap_err(),
            LoginError::WrongCredentials
        );
        assert_eq!(
            login.password("a", "b").unwrap_err(),
            LoginError::Blocked(403)
        );
        assert_eq!(
            login.password("a", "b").unwrap_err(),
            LoginError::Unexpected
        );
    }

    #[test]
    fn tfa_flow_reads_token_cookie() {
        let mock = Mock::new(vec![
            (
                "/user/login",
                200,
                vec![],
                r#"{"accessToken":"","loginType":"tfa","tfaKey":"syn-key"}"#,
            ),
            (
                "/api/csrf",
                200,
                vec![(
                    "set-cookie".into(),
                    "bbl_csrf_token=syn-csrf; Path=/".into(),
                )],
                "{}",
            ),
            (
                "/api/sign-in/tfa",
                200,
                vec![(
                    "set-cookie".into(),
                    "token=syn-tfa-token; Path=/; HttpOnly".into(),
                )],
                "{}",
            ),
        ]);
        let login = Login::new(&mock, "US");
        let LoginStep::Tfa(key) = login.password("a", "b").unwrap() else {
            panic!()
        };
        assert!(!format!("{key:?}").contains("syn"));
        let t = login.tfa_code(&key, "654321").unwrap();
        assert_eq!(t.access_token, "syn-tfa-token");
        let seen = mock.seen.lock().unwrap();
        let body: Value = serde_json::from_str(&seen[2].2).unwrap();
        assert_eq!(
            (body["tfaKey"].as_str(), body["tfaCode"].as_str()),
            (Some("syn-key"), Some("654321"))
        );
    }

    #[test]
    fn profile_and_change_user_payload_match_orca() {
        let p = parse_profile(r#"{"uidStr":"123","uid":123,"name":"syn","account":"a@example.invalid","avatar":"https://x/a.png"}"#).unwrap();
        assert_eq!(p.uid, "123");
        assert_eq!(parse_profile(r#"{"uid":77}"#).unwrap().uid, "77");
        assert!(parse_profile(r#"{"name":"x"}"#).is_err());
        assert!(!format!("{p:?}").contains("syn"));
        let t = Tokens {
            access_token: "A".into(),
            refresh_token: "R".into(),
            expires_in: 7776000.0,
            refresh_expires_in: 0.5,
        };
        let v: Value = serde_json::from_str(&change_user_payload(&t, &p)).unwrap();
        assert_eq!(v["command"], "user_login");
        assert_eq!(v["data"]["token"], "A");
        assert_eq!(v["data"]["refresh_token"], "R");
        assert_eq!(v["data"]["expires_in"], "7776000.000000");
        assert_eq!(v["data"]["refresh_expires_in"], "0.500000");
        assert_eq!(v["data"]["user"]["uid"], "123");
        assert_eq!(v["data"]["user"]["account"], "a@example.invalid");
    }

    #[test]
    #[cfg(unix)]
    fn adopt_copies_privately_and_forget_removes_only_own() {
        use std::os::unix::fs::PermissionsExt as _;
        let studio = tempfile::tempdir().unwrap();
        fs::write(studio.path().join(SESSION_FILE), "synthetic encrypted").unwrap();
        fs::write(
            studio.path().join("BambuStudio.conf"),
            r#"{"app":{"region":"China"}}"#,
        )
        .unwrap();
        let found = foreign(
            studio.path().to_owned(),
            SessionOrigin::BambuStudio,
            "BambuStudio.conf",
        )
        .unwrap();
        assert_eq!(found.country_code, "CN");
        let own_root = tempfile::tempdir().unwrap();
        let own = own_root.path().join("session");
        let adopted = adopt_into(&found, &own).unwrap();
        assert!(adopted.is_own());
        assert_eq!(adopted.adopted_from, Some(SessionOrigin::BambuStudio));
        assert_eq!(adopted.country_code, "CN");
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&own), 0o700);
        assert_eq!(mode(&own.join(SESSION_FILE)), 0o600);
        forget_in(&own).unwrap();
        assert!(own_in(&own).is_none());
        assert_eq!(
            fs::read_to_string(studio.path().join(SESSION_FILE)).unwrap(),
            "synthetic encrypted"
        );
        // Порожній файл — не сесія.
        fs::write(studio.path().join(SESSION_FILE), "").unwrap();
        assert!(foreign(studio.path().to_owned(), SessionOrigin::BambuStudio, "x").is_none());
    }
}
