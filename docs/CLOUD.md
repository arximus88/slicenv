# Bambu Cloud у публічному релізі

Стан на 2026-11-06. Гібридна схема: LAN-принтери — чистий Rust (`crates/lan`, окрема робота);
хмара Bambu — через **закритий мережевий плагін Bambu Lab**, який програма не розповсюджує,
а завантажує з серверів Bambu лише за явною згодою користувача (як OrcaSlicer). Вхід в акаунт —
власна форма (без webview) або наявна сесія Bambu Studio / OrcaSlicer на цьому комп’ютері.

На Linux замість закритого плагіна користувач може **сам вибрати** відкритий плагін Open Bamboo
Networking (збірка persano): друк без Developer Mode, сторонній код із вшитим ключем Bambu —
[OPEN-PLUGIN.md](OPEN-PLUGIN.md). Коли його вибрано, `plugin::find_plugins` повертає його першим,
а `account::find_sessions` — лише його сесію (`obn.auth.json`). Без вибору OBN у теках Studio/Orca
пропускається.

Цільовий сценарій: людина встановила лише Slicenv (без Bambu Studio), рушій поставила нашим
менеджером пакетів — і може увійти в Bambu Cloud.

## Схема

```
cloud::setup()  ──► NoPlugin ──► plugin::fetch_remote → Consent::accept → plugin::install
                ├─► UnsupportedPlugin{version}
                ├─► NoSession{plugin} ──► account::find_sessions → adopt_session
                │                        або Login::password → (email_code | tfa_code)
                │                        → Options::for_login → Client::login_with_tokens
                └─► Ready{plugin, session} ──► Options::for_session → Client::load → …
```

| Модуль | Що робить |
|---|---|
| `crates/cloud/src/plugin.rs` | Пошук плагіна, запит версії до API Bambu, згода, завантаження, перевірка й атомарна установка, видалення |
| `crates/cloud/src/abi.rs` | Таблиця поколінь ABI (дзеркало `abi_of` у `native/shim.cpp`) |
| `crates/cloud/src/account.rs` | Знайдені сесії, власне сховище сесії, REST-вхід (крок 1/крок 2), payload `change_user` |
| `crates/cloud/src/http.rs` | HTTPS через системний `curl` (секрети — через stdin, не argv), трейт `Transport` для моків |
| `crates/cloud/native/shim.cpp` | C-межа до C++ ABI плагіна: покоління 02.06 і 02.08.04, нові операції `change_user`, `user_logout`, `get_my_profile`, `get_my_token` |
| `crates/app/src/device.rs` | `cloud_options()`: потік хмари й `--cloud-probe` беруть плагін і сесію через `cloud::setup()` |

### Плагін

- API: `GET https://api.bambulab.com/v1/iot-service/api/slicer/resource?slicer/plugins/cloud=02.08.04.00`
  (для CN — `api.bambulab.cn`), заголовок `X-BBL-OS-Type: linux`. Так само будує URL Orca
  (`GUI_App::get_plugin_url`: серія `AA.BB.CC` + `.00`). Відповідь:
  `{"message":"success","resources":[{"type":"slicer/plugins/cloud","version":"02.08.04.60","url":"https://public-cdn.bblmw.com/…/linux_02.08.04.60.zip"}]}`.
- Посилання на архів приймається лише з доменів `bblmw.com`, `bambulab.com`, `bambulab.cn`, лише `https`.
- Тека: `~/.local/share/slicenv/bambu-plugin/<версія>/` (окремо від `plugins/` застосунку, де
  живуть розширення Slicenv). Поруч `consent.json` (редакція тексту згоди й час).
- Перевірки: розмір дорівнює `Content-Length` з HEAD; ≤ 256 МБ архів, ≤ 64 файли, ≤ 128 МБ
  на файл; розпаковуються **лише плоскі звичайні файли** з безпечними іменами (тека, симлінк,
  `..`, абсолютний шлях → відмова); CRC перевіряє zip-рідер; `libbambu_networking.so` має бути
  ELF. Розпакування в `.staging-*` у тій самій теці, далі `rename` — частково встановленого плагіна
  не буває; при помилці чернетка зникає.
- Пріоритет пошуку: вибраний відкритий плагін (`obn::active`) → керований →
  `~/.config/BambuStudio/plugins` → `~/.config/OrcaSlicer/plugins` (Orca називає файл
  `libbambu_networking_<версія>.so`). Чужі теки лише читаються; OBN у них без вибору
  користувача пропускається (`obn::is_open_plugin`).

### ABI

| Покоління | Версії | Звідки макет | Стан |
|---|---|---|---|
| `V0208` | `02.08.04.xx` (серія, як `find_network_version_index` в Orca) | Orca d28c71c4 `bambu_networking.hpp` (`NetworkAbi::Current`), Bambu Studio 79df7ae3 (`BAMBU_NETWORK_AGENT_VERSION 02.08.04.57`) | перевірено наживо (02.08.04.60) |
| `V0206` | лише `02.06.00.50` | Bambu Studio v02.06.00.51 | перевірено раніше (2026-10-29) |
| Legacy, 02.03 | 01.10.01.xx, 02.03.00.xx | є в Orca | **не вмикаються**: шим їх не реалізує, наживо не перевірені |

Усі функції, які викликає шим, мають однакові сигнатури в 02.06 і 02.08.04 (звірено typedef-и
Orca `BBLNetworkPlugin.hpp` і Studio `NetworkAgent.hpp`; `send_message` в обох з `flag`).
Відрізняється лише `PrintParams`, що передається **за значенням**: у 02.08.04 у кінці додано
`svc_context`, `slicer_uid`, `queue_plate_id` (856 → 952 байти; `static_assert` на зсуви).
Таблиця символів 02.08.04.60 — надмножина 02.06.00.50 (131 проти 107 `bambu_network_*`).
Невідома версія → `Версія плагіна Bambu X не підтримується; підтримуються 02.08.04.xx, 02.06.00.50`
ще до `create_agent`.

### Сесія і вхід

- Сесію (`BambuNetworkEngine.conf`) шифрує й читає сам плагін; Rust її не розшифровує. Тому
  нік відомий лише після завантаження плагіна (`Client::user_nickname`).
- `account::find_sessions()`: власна → Bambu Studio → OrcaSlicer (без персональних даних:
  походження, тека, час зміни, код країни з `app.region`).
- Чужа сесія без «використати» — тимчасовою копією (як раніше). `adopt_session` копіює файл у
  `~/.local/share/slicenv/cloud/session/` (тека 0700, файл 0600, `O_NOFOLLOW`); далі плагін
  працює в цій теці напряму (`Options::persistent_session`), тож оновлення токена зберігаються.
- Власний вхід (`account::Login`, без webview):
  1. `POST /v1/user-service/user/login {"account","password","apiError":""}`;
  2. `accessToken` є → готово; `loginType:"verifyCode"` → `POST /v1/user-service/user/sendemail/code
     {"email","type":"codeLogin"}`, потім `login {"account","code"}` (400 `code:1` — прострочений,
     новий надсилається; `code:2` — невірний); `loginType:"tfa"` → `GET bambulab.com/api/csrf`,
     `POST bambulab.com/api/sign-in/tfa {"tfaKey","tfaCode"}` з `x-bbl-csrf-token`, токен у cookie
     `token` (без refresh-токена);
  3. `Client::login_with_tokens`: профіль через плагін `get_my_profile` (`uidStr`, `name`,
     `account`, `avatar` — як читає Orca), далі `change_user` з payload Orca
     (`{"command":"user_login","data":{"token","refresh_token","expires_in","refresh_expires_in","user":{"uid","name","account","avatar"}}}`;
     `expires_in` — рядок з `std::to_string(double)`, `"7776000.000000"`).
- Вихід: кнопка «Вийти» в діалозі Bambu Cloud видаляє лише сесію Slicenv —
  `account::forget_own_session()` (закритий плагін) або `obn::forget_session()` (відкритий),
  без мережевих запитів; далі `Reconnect`. Сесії Bambu Studio / OrcaSlicer не чіпаються, тож
  із закритим плагіном потік може далі взяти сесію Studio/Orca (як і раніше, `cloud::setup`).

### Стан хмари в інтерфейсі

Робочий потік (`device::worker`) — єдине джерело стану: `device::source::Phase`
(`NeedPlugin`, `NeedLogin`, `CloudOff`, `Connecting`, `NoServer`, `Connected`, `ListRetry`,
`Failed`). «Підключено» — лише після `is_server_connected`. Стан показується в одному місці —
рядок «Bambu Cloud · …» унизу доку з однією дією (`Налаштувати…`, `Увійти`,
`Увімкнути хмару`, `Повторити`); заголовок вікна, тулбар і рядок стану показують вибраний
принтер або «Немає принтера». Кожна сесія починається з порожнього списку; кеш
`printers.json` прив’язаний до хешу id акаунта. Після кінця сесії потік шле `Link(false)`, а
в режимі очікування відповідає на `Print`/`Control` помилкою замість мовчазного відкидання.

Вхід: свіжий (пароль, браузер, «Використати вхід» Studio/Orca) — `source::Login::Fresh`:
перепідключення, а з відкритим плагіном ще й увімкнення хмари (видно рядок з «Вимкнути»).
Перевірка вже збереженої сесії — `Login::Existing`: лише нік, без зміни згоди й без
`Reconnect`. Закриття діалогу скасовує очікування браузера; пізній результат входу, якщо він
усе ж прийшов, застосовується й показується тостом.
- Google/Apple: **наступний крок**. Плагін уже вміє `get_my_token(ticket)` (`Client::tokens_from_ticket`,
  перевірено офлайн), але адресу переходу на сторінку Google/Apple сайт Bambu віддає лише через
  JS-міст webview (`thirdparty_login` → `data.url` у `WebUserLoginDialog`). Потрібно або
  знайти стабільний URL OAuth Bambu з параметром loopback, або вбудований webview. Loopback-сервер
  — як Orca `HttpServer` (ticket + redirect_url → 302 `?result=success`), порт не 13618, щоб не
  конфліктувати зі Studio/Orca.

## API для майстра першого запуску

```rust,ignore
// 1. Стан
match cloud::setup() { NoPlugin | UnsupportedPlugin{version, ..} | NoSession{plugin} | Ready{plugin, session} }

// 2. Плагін (окремий потік; згода — окремий крок у UI)
let remote = cloud::plugin::fetch_remote(&cloud::http::Curl, country, cloud::LATEST_SERIES)?; // version, size
let consent = cloud::plugin::Consent::accept()?;           // лише після «Погоджуюсь»
// або cloud::plugin::Consent::recorded() для оновлення без повторного питання
cloud::plugin::install(&Curl, &consent, &remote, &mut |p: InstallProgress| p.fraction(), &cancel)?;
cloud::plugin::remove(version)?; cloud::plugin::remove_all()?;  // remove_all відкликає згоду

// 3. Знайдені сесії
let found = cloud::account::find_sessions();               // FoundSession{origin, modified, country_code, …}
// нік: Client::load(Options::for_session(&plugin, &s)) → start → is_user_login → user_nickname
//      (краще в окремому процесі, як --cloud-probe)
cloud::account::adopt_session(&found[i])?;                 // «Використати»

// 4. Вхід
let login = cloud::account::Login::new(&Curl, country);
match login.password(email, password)? {                    // крок 1
    LoginStep::Done(tokens) => …,
    LoginStep::EmailCode => login.email_code(email, code)?, // крок 2 (send_email_code — повтор)
    LoginStep::Tfa(key) => login.tfa_code(&key, code)?,
}
let (client, events) = Client::load(Options::for_login(&plugin, country)?)?;
client.start()?; client.login_with_tokens(&tokens)?;
// LoginError: WrongCredentials | CodeExpired | CodeIncorrect | Blocked(u16) | Server(u16) | Network | Unexpected

// 5. Вихід
client.logout()?; cloud::account::forget_own_session()?;
```

Текст згоди (для майстра; `CONSENT_REVISION = 1`, зміна тексту → нова редакція):
«Для Bambu Cloud потрібен мережевий плагін Bambu Lab. Це закрита програма Bambu Lab, не частина
Slicenv; Slicenv її не розробляє і не перевіряє. Плагін буде завантажено з серверів Bambu Lab
і встановлено у вашу теку даних. Використовуєте на власний розсуд.»

## Що перевірено наживо (2026-11-06, ця машина)

`XDG_DATA_HOME=$(mktemp -d) cargo run -p cloud --example plugin_live` (вивід без персональних даних):

```text
Сервер Bambu: плагін 02.08.04.60 (Some(38169068) байт)
Установлено: Some("02.08.04.60"), покоління Some(V0208), у керованій теці: true
Версія з бібліотеки: 02.08.04.60
Знайдені сесії: [BambuStudio, OrcaSlicer]
— сесія копія      : is_user_login true, connect_server true, принтерів 1 (онлайн 1), завдань 5
— сесія власна     : is_user_login true (після adopt_session)
— сесія OrcaSlicer : is_user_login true (сесія, записана плагіном 01.10.01.01)
```

- `slicenv --cloud-probe` з тим самим `XDG_DATA_HOME`: у `/proc/<pid>/maps` — керований
  `bambu-plugin/02.08.04.60/libbambu_networking.so`, вихід 0, без сигналу.
- `~/.config/BambuStudio` і `~/.config/OrcaSlicer` не змінювались (mtime сесій до/після однакові).
- Формат помилки REST-логіну — одним запитом із вигаданим акаунтом: `HTTP 400 {"code":1,"error":"Incorrect account or password."}`;
  звичайний `curl` Cloudflare не блокує.
- Принтеру не надсилалось нічого (ні друку, ні керування); вихід з акаунта наживо не викликався.

**Не перевірено наживо**: справжній вхід паролем, код із пошти, 2FA, `change_user` з реальними
токенами, `user_logout`, друк через 02.08.04 (макет `PrintParams` перевірено лише офлайн
фейковим плагіном — `tests/abi0208.rs`). Розбір відповідей входу покрито моками
(`account::tests`).

## Ризики

- **Ліцензія**: плагін закритий, власність Bambu Lab; розповсюджувати його (у репо, AppImage,
  пакетах) не можна. Програма лише завантажує його з серверів Bambu за згодою користувача.
  Чи сумісне динамічне завантаження закритого плагіна з AGPL Slicenv — юридичне питання; Orca
  (AGPL) робить так само, але це не гарантія.
- **ToS Bambu**: прямі запити до `user-service/user/login` і заголовки клієнта `BambuStudio`
  можуть порушувати умови використання; Bambu може змінити API, додати антибот (Cloudflare вже
  стоїть) або заблокувати клієнт. Вхід через форму — найкрихкіша частина.
- **Authorization Control** (прошивки з січня 2025): хмарний друк і керування приймаються лише
  з підписом плагіна. Тому друк/керування через хмару можливі лише з плагіном; LAN — окремий шлях.
- **Спільний токен**: скопійована сесія Studio/Orca — той самий токен. Якщо плагін оновить його
  в нашій копії, сесія Studio може стати недійсною (і навпаки). Для власного входу такої проблеми
  немає. Тому вихід — лише локальний `user_logout(false)`.
- **Нові серії**: 02.08.04 — поточна серія. Наступна серія (наприклад 02.09) відхилятиметься, доки
  її макети не звірено й не додано рядок у `abi.rs` + `abi_of` у шимі.
- **Аварії плагіна**: закритий код може впасти SIGSEGV; застосунок перевіряє його в окремому
  процесі `--cloud-probe` перед завантаженням у свій.
- **Регіон**: CN → `api.bambulab.cn`/`bambulab.cn`; для інших — `.com`. Китайський акаунт за SMS
  (`sendsmscode`) не реалізовано.
