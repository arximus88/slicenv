//! Bambu Cloud через офіційний мережевий плагін Bambu Lab і приватну копію сесії.
//!
//! Працює на Linux x86_64 і Windows x64 MSVC ([`OS_SUPPORTED`]): на інших ОС крейт компілюється, але
//! [`setup`] і [`Client::load`] повертають [`UNSUPPORTED_OS`] (див. docs/PORTING.md).
//!
//! Схема для публічного релізу — див. `docs/CLOUD.md`: [`plugin`] завантажує закритий
//! плагін Bambu за згодою користувача, [`account`] знаходить сесії й виконує вхід,
//! [`setup`] каже UI, чого бракує.
mod abi;
pub mod account;
pub mod browser_login;
mod camera;
mod os;
mod control;
mod cover;
mod ffi;
pub mod http;
mod modules;
pub mod obn;
pub mod plugin;
mod session;
mod status;
mod tasks;

pub use abi::{Generation, LATEST_SERIES, SUPPORTED as SUPPORTED_PLUGINS, generation};
pub use account::{FoundSession, LoginError, LoginStep, SessionOrigin, Tokens};
pub use camera::{CameraStream, Frame};
pub use control::{
    Activity, Axis, Capabilities, Control, ControlContext, FanKind, Jog, MIN_TARGET, Profile,
    SpeedLevel,
};
pub use cover::download_cover;
pub use modules::{ModuleVersion, module_versions};
pub use plugin::{InstallProgress, InstalledPlugin, PluginOrigin, PluginState};
pub use session::{default_plugin_path, default_studio_config_dir};
pub use status::{
    AmsStatus, AmsUnit, Fans, HmsItem, Light, NewVersion, PrinterStatus, SdCard, Tray, UpgradeState,
    merge as merge_status,
};
pub use tasks::{Task, TaskQuery, plate_thumbnail};

use anyhow::{Context as _, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    cell::Cell,
    collections::HashMap,
    ffi::{CString, c_void},
    fmt,
    marker::PhantomData,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    time::Duration,
};

/// Чи є на цій ОС перевірений шим до плагіна Bambu (Linux x86_64, Windows x64 MSVC; `cfg(bambu_plugin)`).
pub const OS_SUPPORTED: bool = cfg!(bambu_plugin);
/// Повідомлення для UI й MCP, коли хмара недоступна на цій ОС.
pub const UNSUPPORTED_OS: &str = "Bambu Cloud не підтримується на цій ОС: мережевий плагін Bambu Lab поки підключається лише на Linux x86_64 і Windows x64. LAN-підключення до принтера працює.";

/// Повний ідентифікатор доступний явно; Debug/Display показують лише суфікс.
#[derive(Clone, Eq, PartialEq, Hash, Deserialize)]
#[serde(transparent)]
pub struct DeviceId(String);
impl DeviceId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn redacted(&self) -> String {
        let suffix: String = self
            .0
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if self.0.chars().count() <= 4 {
            "****".into()
        } else {
            format!("…{suffix}")
        }
    }
}
impl fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.redacted())
    }
}
impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.redacted())
    }
}

#[derive(Clone, Deserialize)]
pub struct Printer {
    pub dev_id: DeviceId,
    #[serde(default, rename = "dev_name")]
    pub name: String,
    #[serde(default, rename = "dev_model_name")]
    pub model: String,
    #[serde(default, rename = "dev_online")]
    pub online: bool,
}
impl fmt::Debug for Printer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Printer")
            .field("dev_id", &self.dev_id)
            .field("online", &self.online)
            .finish_non_exhaustive()
    }
}

/// Не друкує довільний JSON, який може містити секрети.
#[derive(Clone)]
pub struct JsonPayload(Value);
impl JsonPayload {
    pub fn as_value(&self) -> &Value {
        &self.0
    }
}
impl fmt::Debug for JsonPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JsonPayload([redacted])")
    }
}

#[derive(Debug)]
pub enum Event {
    /// Лише HTTP-код: тіло відповіді може містити приватні дані.
    HttpError {
        status_code: u32,
    },
    ServerConnected {
        return_code: i32,
        reason_code: i32,
    },
    Status {
        dev_id: DeviceId,
        /// Накопичений стан після застосування patch.
        status: Box<PrinterStatus>,
        patch: JsonPayload,
    },
    PrinterMessage {
        dev_id: DeviceId,
        payload: JsonPayload,
    },
    UserMessage {
        payload: JsonPayload,
    },
    PrintProgress {
        stage: i32,
        code: i32,
    },
    /// Кеш очищений; слід запитати request_status для повного знімка.
    Lagged {
        dropped: u64,
    },
    MalformedMessage,
}

struct NativeEvent {
    kind: i32,
    a: String,
    b: String,
    x: i32,
    y: i32,
}
struct CallbackState {
    sender: SyncSender<NativeEvent>,
    dropped: Arc<AtomicU64>,
}

// Плагін викликає на власних потоках; жодного доступу до Client тут немає.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn callback(
    user: *mut c_void,
    kind: i32,
    a: *const u8,
    alen: usize,
    b: *const u8,
    blen: usize,
    x: i32,
    y: i32,
) {
    let _ = std::panic::catch_unwind(|| {
        // Gate шиму гарантує, що user живий протягом callback.
        let state = unsafe { &*user.cast::<CallbackState>() };
        if alen > 4096 || blen > 4 * 1024 * 1024 {
            state.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let a =
            String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(a, alen) }).into_owned();
        let b =
            String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(b, blen) }).into_owned();
        if state
            .sender
            .try_send(NativeEvent { kind, a, b, x, y })
            .is_err()
        {
            state.dropped.fetch_add(1, Ordering::Relaxed);
        }
    });
}

/// Єдиний споживач подій; можна передати в інший потік незалежно від Client.
pub struct Events {
    receiver: Receiver<NativeEvent>,
    dropped: Arc<AtomicU64>,
    statuses: HashMap<DeviceId, Value>,
}
impl Events {
    pub fn recv_timeout(
        &mut self,
        timeout: Duration,
    ) -> std::result::Result<Event, RecvTimeoutError> {
        let dropped = self.dropped.swap(0, Ordering::Relaxed);
        if dropped != 0 {
            self.statuses.clear();
            return Ok(Event::Lagged { dropped });
        }
        let e = self.receiver.recv_timeout(timeout)?;
        Ok(match e.kind {
            1 => Event::ServerConnected {
                return_code: e.x,
                reason_code: e.y,
            },
            2 | 3 => {
                let Ok(payload) = serde_json::from_str::<Value>(&e.b) else {
                    return Ok(Event::MalformedMessage);
                };
                if e.kind == 3 {
                    return Ok(Event::UserMessage {
                        payload: JsonPayload(payload),
                    });
                }
                let dev_id = DeviceId(e.a);
                if payload["print"]["command"] == "push_status" {
                    let patch = &payload["print"];
                    let base = self.statuses.entry(dev_id.clone()).or_insert(Value::Null);
                    if patch["msg"] == 0 {
                        *base = Value::Null;
                    }
                    status::merge(base, patch);
                    Event::Status {
                        dev_id,
                        status: Box::new(PrinterStatus::from_json(base)),
                        patch: JsonPayload(payload),
                    }
                } else {
                    Event::PrinterMessage {
                        dev_id,
                        payload: JsonPayload(payload),
                    }
                }
            }
            5 => Event::HttpError {
                status_code: e.x as u32,
            },
            4 => Event::PrintProgress {
                stage: e.x,
                code: e.y,
            },
            _ => Event::MalformedMessage,
        })
    }
}

/// Вбудований у плагін TLS (curl/OpenSSL) шукає сертифікати за шляхом зі своєї збірки, якого
/// на Arch/CachyOS немає: без `SSL_CERT_FILE` усі REST-запити (список принтерів — -11,
/// історія — -1) падають, тоді як MQTT працює. Задає змінну, якщо її немає.
///
/// Заразом вимикає логи відкритого плагіна через змінні оточення ([`obn::quiet_process_env`]).
///
/// # Safety
/// Змінює оточення процесу: викликати на старті, до створення інших потоків.
pub unsafe fn configure_tls_env() {
    unsafe { obn::quiet_process_env() };
    if std::env::var_os("SSL_CERT_FILE").is_some() || std::env::var_os("SSL_CERT_DIR").is_some() {
        return;
    }
    let bundle = [
        "/etc/ssl/cert.pem",
        "/etc/ssl/certs/ca-certificates.crt",
        "/etc/pki/tls/certs/ca-bundle.crt",
        "/etc/ssl/ca-bundle.pem",
    ]
    .into_iter()
    .find(|p| Path::new(p).is_file());
    if let Some(bundle) = bundle {
        unsafe { std::env::set_var("SSL_CERT_FILE", bundle) };
    }
}

/// Версія прошивки (OTA) з відповіді `info/get_version`; потрібна для запиту камери.
pub fn ota_version(message: &Value) -> Option<String> {
    message["info"]["module"]
        .as_array()?
        .iter()
        .find(|m| m["name"] == "ota")?["sw_ver"]
        .as_str()
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

/// Усі записи плагіна (сесія та логи) йдуть у приватний тимчасовий каталог.
pub struct Options {
    pub plugin_path: PathBuf,
    pub studio_config_dir: PathBuf,
    pub country_code: String,
    /// slicer_base64.cer зі Studio/Orca; потрібний перед connect_server.
    pub certificate: Option<PathBuf>,
    /// За замовчуванням команди керування і друк заблоковані.
    pub allow_control: bool,
    /// Заголовки клієнта для REST-запитів плагіна; за замовчуванням — як у Bambu Studio.
    pub http_headers: Vec<(String, String)>,
    /// `studio_config_dir` — власна тека застосунку: плагін працює в ній напряму (вхід і
    /// оновлення токена зберігаються). Інакше — тимчасова копія файлу сесії.
    pub persistent_session: bool,
}
impl Options {
    /// Знайдений плагін + сесія (власна — напряму, чужа — копією).
    pub fn for_session(plugin: &InstalledPlugin, session: &FoundSession) -> Self {
        // Відкритий плагін: своя тека з актуальним obn.conf (сесія — obn.auth.json там же).
        if plugin.origin == PluginOrigin::OpenBamboo
            && let Ok(mut options) = obn::options(plugin)
        {
            options.country_code = session.country_code.clone();
            return options;
        }
        Self {
            plugin_path: plugin.library.clone(),
            studio_config_dir: session.dir.clone(),
            country_code: session.country_code.clone(),
            http_headers: session::http_headers(
                &paths::vendor_config_dir("BambuStudio"),
                plugin.generation(),
            ),
            certificate: session::find_certificate(),
            allow_control: false,
            persistent_session: session.is_own(),
        }
    }
    /// Для першого входу через форму: порожня власна тека сесії.
    pub fn for_login(plugin: &InstalledPlugin, country_code: &str) -> Result<Self> {
        let dir = if plugin.origin == PluginOrigin::OpenBamboo {
            let dir = obn::prepare_config_dir()?;
            account::prepare_session_in(&dir, country_code)?;
            dir
        } else {
            account::prepare_own_session(country_code)?
        };
        Ok(Self::for_session(
            plugin,
            &FoundSession {
                origin: SessionOrigin::Own,
                dir,
                modified: None,
                country_code: country_code.into(),
                adopted_from: None,
            },
        ))
    }
    pub fn from_bambu_studio() -> Result<Self> {
        Self::from_studio_dir(default_studio_config_dir()?)
    }
    pub fn from_studio_dir(studio_config_dir: impl Into<PathBuf>) -> Result<Self> {
        let studio_config_dir = studio_config_dir.into();
        Ok(Self {
            plugin_path: studio_config_dir.join("plugins").join(plugin::LIBRARY),
            country_code: session::studio_country_code(&studio_config_dir),
            http_headers: session::studio_http_headers(&studio_config_dir),
            studio_config_dir,
            certificate: session::find_certificate(),
            allow_control: false,
            persistent_session: false,
        })
    }
}

/// Чого бракує для хмари — для повідомлень і майстра першого запуску.
#[derive(Clone, Debug)]
pub enum CloudSetup {
    /// «Плагін Bambu не встановлено — завантажити?»
    NoPlugin,
    /// Знайдено лише непідтримувані версії (наприклад, legacy від Orca).
    UnsupportedPlugin {
        /// Порожній, якщо версію не видно без завантаження.
        version: String,
        /// Готовий український текст (для логів і прикладів).
        message: String,
    },
    /// «Увійдіть в акаунт Bambu».
    NoSession { plugin: InstalledPlugin },
    Ready {
        plugin: InstalledPlugin,
        session: FoundSession,
    },
}

/// Плагін за пріоритетом (керований → Studio → Orca) і сесія (власна → Studio → Orca).
pub fn setup() -> CloudSetup {
    if !OS_SUPPORTED {
        return CloudSetup::UnsupportedPlugin {
            version: String::new(),
            message: UNSUPPORTED_OS.into(),
        };
    }
    match plugin::plugin_state() {
        PluginState::Missing => CloudSetup::NoPlugin,
        PluginState::Unsupported(list) => {
            let version = list
                .first()
                .and_then(|p| p.version.clone())
                .unwrap_or_default();
            CloudSetup::UnsupportedPlugin {
                message: abi::unsupported_message(&version),
                version,
            }
        }
        PluginState::Ready(plugin) => match account::find_sessions().into_iter().next() {
            Some(session) => CloudSetup::Ready { plugin, session },
            None => CloudSetup::NoSession { plugin },
        },
    }
}

/// Версія плагіна без створення агента. Завантажує бібліотеку в цей процес (dlopen);
/// для недовірених файлів краще викликати в окремому процесі.
pub fn read_plugin_version(path: &Path) -> Result<String> {
    ensure!(OS_SUPPORTED, "{UNSUPPORTED_OS}");
    let path = cstring(path_string(path)?)?;
    let mut out = ffi::Bytes::default();
    let code = unsafe { ffi::cloud_plugin_version(path.as_ptr(), &mut out) };
    checked(code, "read plugin version")?;
    Ok(out.text())
}

/// Агент прив'язаний до потоку; callback містить лише Send + Sync стан каналу.
/// Drop закриває callback-gate перед звільненням Rust-даних, потім видаляє копію сесії.
pub struct Client {
    handle: *mut c_void,
    _callback: Box<CallbackState>,
    session: session::Session,
    persistent: bool,
    allow_control: bool,
    has_certificate: bool,
    /// Завантажено відкритий плагін (OBN), а не закритий Bambu.
    open_plugin: bool,
    source_path: PathBuf,
    started: Cell<bool>,
    sequence: Cell<u64>,
    _thread_bound: PhantomData<Rc<()>>,
}

fn cstring(value: &str) -> Result<CString> {
    CString::new(value).context("value contains a NUL byte")
}
fn path_string(path: &Path) -> Result<&str> {
    path.to_str().context("plugin paths must be UTF-8")
}
/// Ненульовий код повернення плагіна (`BAMBU_NETWORK_ERR_*` з `bambu_networking.hpp`).
/// Застосунок дістає його через `downcast_ref`, щоб показати зрозумілу причину.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginError {
    pub operation: String,
    pub code: i32,
}

impl PluginError {
    /// `BAMBU_NETWORK_SIGNED_ERROR`: плагін обмежує друк для програм без офіційного підпису
    /// Bambu (Bambu Studio показує «Your software is not signed…»).
    pub const UNSIGNED_SOFTWARE: i32 = -26;
    /// Код плагіна з ланцюжка помилок, якщо він там є.
    pub fn code_of(error: &anyhow::Error) -> Option<i32> {
        error.chain().find_map(|e| e.downcast_ref::<Self>()).map(|e| e.code)
    }
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} failed (code {})", self.operation, self.code)
    }
}

impl std::error::Error for PluginError {}

fn checked(code: i32, operation: &str) -> Result<()> {
    if code != 0 {
        return Err(PluginError {
            operation: operation.to_owned(),
            code,
        }
        .into());
    }
    Ok(())
}

impl Client {
    /// Копіює лише BambuNetworkEngine.conf; ніколи не передає оригінальний шлях плагіну.
    /// Підтримує лише точний ABI 02.06.00.50; інші версії відхиляє до create_agent.
    pub fn load(options: Options) -> Result<(Self, Events)> {
        ensure!(OS_SUPPORTED, "{UNSUPPORTED_OS}");
        if let Some(cert) = &options.certificate {
            ensure!(cert.is_file(), "certificate file does not exist");
        }
        ensure!(
            (options.country_code.len() == 2
                && options.country_code.bytes().all(|c| c.is_ascii_uppercase()))
                || options.country_code == "Others",
            "country code must be two uppercase letters or Others"
        );
        let session = if options.persistent_session {
            session::Session::persistent(&options.studio_config_dir)?
        } else {
            session::Session::copy_from(&options.studio_config_dir)?
        };
        // Відкритий плагін читає налаштування логів з log_dir ще в create_agent — до
        // set_config_dir. Без цього він пише e-mail, user id і серійники в stderr застосунку.
        obn::seed_log_dir(&session.config, &session.log)?;
        let (sender, receiver) = mpsc::sync_channel(256);
        let dropped = Arc::new(AtomicU64::new(0));
        let mut state = Box::new(CallbackState {
            sender,
            dropped: dropped.clone(),
        });
        let plugin = cstring(path_string(&options.plugin_path)?)?;
        let log = cstring(path_string(&session.log)?)?;
        let mut handle = std::ptr::null_mut();
        // Шим зберігає state, доки cloud_close не закриє gate.
        let code = unsafe {
            ffi::cloud_open(
                plugin.as_ptr(),
                log.as_ptr(),
                callback,
                (&mut *state as *mut CallbackState).cast(),
                &mut handle,
            )
        };
        if code == -10002 {
            let version = read_plugin_version(&options.plugin_path).unwrap_or_default();
            anyhow::bail!("{} (-10002)", abi::unsupported_message(&version));
        }
        checked(
            code,
            "load plugin (-10001: dlopen, -10002: unsupported ABI, -10003: existing agent)",
        )?;
        let client = Self {
            handle,
            _callback: state,
            persistent: options.persistent_session,
            session,
            allow_control: options.allow_control,
            has_certificate: options.certificate.is_some(),
            open_plugin: options.plugin_path.starts_with(paths::open_plugin_root()),
            source_path: options.plugin_path.with_file_name(plugin::SOURCE_LIBRARY),
            started: Cell::new(false),
            sequence: Cell::new(0),
            _thread_bound: PhantomData,
        };
        client.call(1, path_string(&client.session.config)?, "", 0)?;
        client.call(2, "", "", 0)?;
        if let Some(cert) = options.certificate {
            let parent = cert.parent().context("certificate has no parent")?;
            let name = cert
                .file_name()
                .and_then(|n| n.to_str())
                .context("invalid certificate name")?;
            client.call(15, path_string(parent)?, name, 0)?;
        }
        client.call(4, "", "", 0)?;
        client.call(3, &options.country_code, "", 0)?;
        if !options.http_headers.is_empty() {
            ensure!(
                options
                    .http_headers
                    .iter()
                    .all(|(k, v)| !k.contains('\n') && !v.contains('\n')),
                "HTTP header contains a newline"
            );
            let packed: String = options
                .http_headers
                .iter()
                .map(|(k, v)| format!("{k}\n{v}\n"))
                .collect();
            client.call(21, &packed, "", 0)?;
        }
        client.call(16, "", "", 0)?;
        Ok((
            client,
            Events {
                receiver,
                dropped,
                statuses: HashMap::new(),
            },
        ))
    }

    pub(crate) fn raw_call(&self, op: i32, a: &str, b: &str, number: i32) -> Result<(i32, String)> {
        let a = cstring(a)?;
        let b = cstring(b)?;
        let mut out = ffi::Bytes::default();
        // &self не може перейти між потоками; опкоди визначені шимом.
        let code =
            unsafe { ffi::cloud_call(self.handle, op, a.as_ptr(), b.as_ptr(), number, &mut out) };
        Ok((code, out.text()))
    }
    fn call(&self, op: i32, a: &str, b: &str, number: i32) -> Result<String> {
        let (code, text) = self.raw_call(op, a, b, number)?;
        checked(code, &format!("plugin operation {op}"))?;
        Ok(text)
    }
    fn boolean(&self, op: i32) -> Result<bool> {
        let (code, _) = self.raw_call(op, "", "", 0)?;
        ensure!(
            code == 0 || code == 1,
            "plugin query {op} failed (code {code})"
        );
        Ok(code == 1)
    }
    pub fn plugin_version(&self) -> Result<String> {
        self.call(0, "", "", 0)
    }
    pub fn start(&self) -> Result<()> {
        ensure!(!self.started.get(), "agent has already started");
        self.call(5, "", "", 0)?;
        self.started.set(true);
        Ok(())
    }
    pub fn connect_server(&self) -> Result<()> {
        ensure!(self.started.get(), "start the agent before connecting");
        ensure!(
            self.is_user_login()?,
            "no restored session; log in using Bambu Studio first"
        );
        ensure!(
            self.has_certificate,
            "set Options::certificate to the installed slicer_base64.cer before connecting"
        );
        self.call(6, "", "", 0).map(|_| ())
    }
    pub fn is_user_login(&self) -> Result<bool> {
        self.boolean(7)
    }
    pub fn is_server_connected(&self) -> Result<bool> {
        self.boolean(14)
    }
    /// Містить персональні дані; не логувати.
    pub fn user_id(&self) -> Result<String> {
        self.call(8, "", "", 0)
    }
    /// Містить персональні дані; не логувати.
    pub fn user_nickname(&self) -> Result<String> {
        self.call(9, "", "", 0)
    }
    /// Вхід токенами з [`account::Login`]: профіль через плагін (`get_my_profile`), далі
    /// `change_user` з payload як у Orca. Сесію плагін запише у власну теку застосунку.
    pub fn login_with_tokens(&self, tokens: &Tokens) -> Result<()> {
        ensure!(self.started.get(), "start the agent before login");
        ensure!(
            self.persistent,
            "login needs Options::for_login (own session directory)"
        );
        let body = self.call(24, &tokens.access_token, "", 0)?;
        let profile = account::parse_profile(&body)?;
        self.call(22, &account::change_user_payload(tokens, &profile), "", 0)?;
        ensure!(self.is_user_login()?, "Bambu не прийняв вхід");
        Ok(())
    }
    /// Квиток SSO (Google/Apple, loopback як у Orca HttpServer) → токени через плагін.
    pub fn tokens_from_ticket(&self, ticket: &str) -> Result<Tokens> {
        ensure!(self.started.get(), "start the agent before login");
        let body = self.call(25, ticket, "", 0)?;
        let v: Value = serde_json::from_str(&body)
            .map_err(|_| anyhow::anyhow!("invalid token response (body redacted)"))?;
        let access = v["accessToken"].as_str().unwrap_or_default();
        ensure!(!access.is_empty(), "Bambu не видав токен");
        Ok(Tokens {
            access_token: access.into(),
            refresh_token: v["refreshToken"].as_str().unwrap_or_default().into(),
            expires_in: v["expiresIn"].as_f64().unwrap_or(0.0),
            refresh_expires_in: v["refreshExpiresIn"].as_f64().unwrap_or(0.0),
        })
    }
    /// Локальний вихід у плагіні (`user_logout(false)`: без запиту до сервера, бо скопійований
    /// токен спільний із Bambu Studio). Файл власної сесії прибирає [`account::forget_own_session`].
    pub fn logout(&self) -> Result<()> {
        self.call(23, "", "", 0).map(|_| ())
    }
    /// Тимчасовий каталог містить секрети; доступний для локальної діагностики.
    pub fn private_dir(&self) -> &Path {
        self.session.root.path()
    }
    pub fn printers(&self) -> Result<Vec<Printer>> {
        ensure!(
            self.started.get(),
            "start the agent before listing printers"
        );
        ensure!(
            self.is_user_login()?,
            "no session: log in using Bambu Studio, then create a new Client"
        );
        let body = self.call(10, "", "", 0)?;
        #[derive(Deserialize)]
        struct List {
            devices: Vec<Printer>,
        }
        // Помилка serde може містити вхідні значення; не повертаємо її текст.
        serde_json::from_str::<List>(&body)
            .map(|v| v.devices)
            .map_err(|_| anyhow::anyhow!("invalid printer-list response (body redacted)"))
    }
    pub fn subscribe(&self, dev: &DeviceId) -> Result<()> {
        ensure!(self.is_server_connected()?, "cloud server is not connected");
        self.call(11, "", "", 0)?;
        self.call(12, dev.as_str(), "", 0)?;
        // Як у DeviceManager: обираємо активний принтер для повної телеметрії.
        self.call(17, dev.as_str(), "", 0)?;
        Ok(())
    }
    fn send(&self, dev: &DeviceId, value: Value, qos: i32) -> Result<()> {
        ensure!(
            self.started.get() && self.is_server_connected()?,
            "cloud server is not connected"
        );
        self.call(13, dev.as_str(), &value.to_string(), qos)?;
        Ok(())
    }
    /// Лише запит телеметрії pushall; не змінює параметри друку.
    fn next_sequence(&self) -> u64 {
        self.sequence.set(self.sequence.get() + 1);
        self.sequence.get()
    }

    pub fn request_status(&self, dev: &DeviceId) -> Result<()> {
        self.send(
            dev,
            json!({"pushing":{"command":"pushall","sequence_id":self.next_sequence().to_string()}}),
            0,
        )
    }
    /// Лише читання версій модулів; відповідь info/get_version надходить у PrinterMessage.
    pub fn request_version(&self, dev: &DeviceId) -> Result<()> {
        self.send(
            dev,
            json!({"info":{"command":"get_version","sequence_id":self.next_sequence().to_string()}}),
            1,
        )
    }
    fn control_allowed(&self) -> Result<()> {
        ensure!(
            self.allow_control,
            "control disabled: set Options::allow_control explicitly"
        );
        Ok(())
    }
    /// Виконує фізичну дію; status example ніколи не викликає цей метод.
    /// Команди, що потребують моделі чи стану (температури, вентилятори, рух), тут відхиляються:
    /// для них є `control_in`.
    pub fn control(&self, dev: &DeviceId, command: Control) -> Result<()> {
        self.control_in(dev, command, &ControlContext::default())
    }
    /// Перевіряє команду проти контексту (модель, можливості, стан, домашня позиція) ще до
    /// відправки: порушення меж — помилка, у мережу нічого не йде.
    pub fn control_in(&self, dev: &DeviceId, command: Control, ctx: &ControlContext) -> Result<()> {
        self.control_allowed()?;
        let message = command.message(self.next_sequence(), ctx)?;
        self.send(dev, message, command.qos())
    }
    /// Блокує потік на завантаження і запуск друку; події PrintProgress надходять у Events.
    /// Успіх означає результат плагіна, а не підтвердження фактичного початку друку.
    pub fn start_print(&self, request: &PrintRequest) -> Result<()> {
        self.control_allowed()?;
        ensure!(self.started.get(), "start the agent before printing");
        if !self.is_server_connected()? {
            // Відкритий плагін з block_cloud = 1 хмари не з’єднує взагалі: друк — лише LAN.
            ensure!(
                !self.open_plugin || obn::cloud_enabled(),
                "Хмару для відкритого плагіна вимкнено: друкуйте по локальній мережі або увімкніть «Друк і стан через хмару» в налаштуваннях відкритого плагіна"
            );
            anyhow::bail!("cloud server is not connected");
        }
        self.dispatch_print(request, None)
    }
    /// Друк по локальній мережі через плагін (`start_local_print`, як Orca для LAN-принтера):
    /// плагін сам вивантажує файл FTPS і надсилає `project_file` по MQTT принтера. Хмара не
    /// потрібна. Відкритий плагін persano при цьому сам підписує команду, тож Developer Mode
    /// не потрібен; закритий плагін Bambu підпису для Slicenv не дає.
    pub fn start_local_print(&self, request: &PrintRequest, lan: &LanTarget) -> Result<()> {
        self.control_allowed()?;
        ensure!(self.started.get(), "start the agent before printing");
        ensure!(
            !lan.ip.is_empty() && !lan.access_code.is_empty(),
            "LAN print needs the printer IP and access code"
        );
        self.dispatch_print(request, Some(lan))
    }
    fn dispatch_print(&self, request: &PrintRequest, lan: Option<&LanTarget>) -> Result<()> {
        let fields = print_fields(request, lan)?;
        let fields: Vec<CString> = fields.iter().map(|s| cstring(s)).collect::<Result<_>>()?;
        let pointers: Vec<_> = fields.iter().map(|s| s.as_ptr()).collect();
        let (mode, operation) = if lan.is_some() {
            (1, "start_local_print")
        } else {
            (0, "start_print")
        };
        // Всі C-рядки живуть до повернення синхронного C++ виклику.
        checked(
            unsafe {
                ffi::cloud_print(
                    self.handle,
                    pointers.as_ptr(),
                    pointers.len() as i32,
                    request.plate_index,
                    request.flags(),
                    mode,
                )
            },
            operation,
        )
    }
    /// Явне завершення з перевіркою результату плагіна; Drop також закриває агент.
    pub fn shutdown(mut self) -> Result<()> {
        let handle = std::mem::replace(&mut self.handle, std::ptr::null_mut());
        // Gate закривається навіть за помилки destroy_agent.
        checked(unsafe { ffi::cloud_close(handle) }, "destroy_agent")
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        // Шим вимикає callbacks синхронно, навіть якщо destroy_agent повертає помилку.
        if !self.handle.is_null() {
            unsafe { ffi::cloud_close(self.handle) };
        }
    }
}

/// Архіви створює слайсер: filename — sliced .gcode.3mf, config_filename — його config 3MF.
/// ams_mapping та інші mapping-поля — JSON-рядки у форматі Bambu; bed_type — gcode-ім'я,
/// наприклад textured_plate, hot_plate, cool_plate або engineering_plate.
#[derive(Clone)]
pub struct PrintRequest {
    pub dev_id: DeviceId,
    pub plate_index: i32,
    pub task_name: String,
    pub project_name: String,
    pub preset_name: String,
    pub gcode_3mf: PathBuf,
    pub config_3mf: PathBuf,
    pub use_ams: bool,
    pub timelapse: bool,
    pub timelapse_use_internal: bool,
    pub bed_leveling: bool,
    pub flow_cali: bool,
    pub vibration_cali: bool,
    pub layer_inspect: bool,
    pub ams_mapping: String,
    pub ams_mapping2: String,
    pub ams_mapping_info: String,
    pub nozzle_mapping: String,
    pub nozzles_info: String,
    pub bed_type: String,
    pub extra_options: String,
}

impl PrintRequest {
    /// Початкові опції: без AMS і timelapse; калібрування задає викликач.
    pub fn new(
        dev_id: DeviceId,
        gcode_3mf: PathBuf,
        config_3mf: PathBuf,
        project_name: String,
    ) -> Self {
        Self {
            dev_id,
            plate_index: 1,
            task_name: project_name.clone(),
            preset_name: format!("{project_name}_plate_1"),
            project_name,
            gcode_3mf,
            config_3mf,
            use_ams: false,
            timelapse: false,
            timelapse_use_internal: false,
            bed_leveling: false,
            flow_cali: false,
            vibration_cali: false,
            layer_inspect: false,
            ams_mapping: String::new(),
            ams_mapping2: String::new(),
            ams_mapping_info: String::new(),
            nozzle_mapping: String::new(),
            nozzles_info: String::new(),
            bed_type: String::new(),
            extra_options: String::new(),
        }
    }
}

impl PrintRequest {
    fn flags(&self) -> u32 {
        u32::from(self.use_ams)
            | (u32::from(self.timelapse) << 1)
            | (u32::from(self.bed_leveling) << 2)
            | (u32::from(self.flow_cali) << 3)
            | (u32::from(self.vibration_cali) << 4)
            | (u32::from(self.layer_inspect) << 5)
            | (u32::from(self.timelapse_use_internal) << 6)
    }
}

/// Принтер у локальній мережі для [`Client::start_local_print`]. Debug не показує ні IP,
/// ні код доступу.
#[derive(Clone)]
pub struct LanTarget {
    pub ip: String,
    /// Код доступу з екрана принтера (пароль FTPS і MQTT `bblp`).
    pub access_code: String,
    pub name: String,
}
impl fmt::Debug for LanTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LanTarget([redacted])")
    }
}

/// Поля в порядку шима (`fill` у `native/shim.cpp`): 0..12 — спільні, 13..15 — лише LAN.
fn print_fields(request: &PrintRequest, lan: Option<&LanTarget>) -> Result<Vec<String>> {
    ensure!(request.plate_index > 0, "plate_index is 1-based");
    ensure!(
        !request.bed_type.is_empty(),
        "bed_type must be selected explicitly"
    );
    ensure!(
        request.gcode_3mf.is_file() && request.config_3mf.is_file(),
        "print archives do not exist"
    );
    ensure!(
        path_string(&request.gcode_3mf)?.ends_with(".gcode.3mf"),
        "expected a .gcode.3mf archive"
    );
    let mut fields = vec![
        request.dev_id.as_str().to_owned(),
        request.task_name.clone(),
        request.project_name.clone(),
        request.preset_name.clone(),
        path_string(&request.gcode_3mf)?.to_owned(),
        path_string(&request.config_3mf)?.to_owned(),
        request.ams_mapping.clone(),
        request.ams_mapping2.clone(),
        request.ams_mapping_info.clone(),
        request.nozzle_mapping.clone(),
        request.nozzles_info.clone(),
        request.bed_type.clone(),
        request.extra_options.clone(),
    ];
    if let Some(lan) = lan {
        fields.extend([lan.ip.clone(), lan.access_code.clone(), lan.name.clone()]);
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
    #[test]
    fn plugin_codes_survive_context_and_unsigned_is_minus_26() {
        let err = checked(-26, "start_print").unwrap_err().context("друк");
        assert_eq!(PluginError::code_of(&err), Some(PluginError::UNSIGNED_SOFTWARE));
        assert_eq!(checked(-26, "start_print").unwrap_err().to_string(), "start_print failed (code -26)");
        assert!(checked(0, "x").is_ok());
        assert_eq!(PluginError::code_of(&anyhow::anyhow!("інше")), None);
    }
    use super::*;
    #[test]
    fn ids_and_raw_json_do_not_leak_in_debug() {
        let id = DeviceId::new("SYNTHETIC12345678");
        assert_eq!(format!("{id:?}"), "…5678");
        assert_eq!(format!("{:?}", DeviceId::new("1234")), "****");
        assert!(
            !format!("{:?}", JsonPayload(json!({"token":"test-secret"}))).contains("test-secret")
        );
    }
    #[test]
    fn session_copy_does_not_change_source() {
        let source = tempfile::tempdir().unwrap();
        let path = source.path().join("BambuNetworkEngine.conf");
        std::fs::write(&path, "synthetic encrypted session").unwrap();
        let session = session::Session::copy_from(source.path()).unwrap();
        std::fs::write(session.config.join("BambuNetworkEngine.conf"), "changed").unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "synthetic encrypted session"
        );
    }
    #[test]
    #[cfg(unix)]
    fn session_symlink_is_rejected() {
        let source = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/dev/null", source.path().join("BambuNetworkEngine.conf"))
            .unwrap();
        assert!(session::Session::copy_from(source.path()).is_err());
    }
}
