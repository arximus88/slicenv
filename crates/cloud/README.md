# cloud

> Схема публічного релізу (менеджер плагіна за згодою, ABI 02.08.04, власний вхід) — у
> [docs/CLOUD.md](../../docs/CLOUD.md). Відкритий плагін Open Bamboo Networking за вибором
> користувача (`obn.rs`, `Client::start_local_print`) — [docs/OPEN-PLUGIN.md](../../docs/OPEN-PLUGIN.md).
> Нижче — історичний опис першої версії (02.06).

Linux x86_64 Rust wrapper around the user's installed Bambu networking plugin. It
restores the existing Studio session into a private temporary directory, lists bound
printers, and streams typed cloud telemetry. No plugin binaries or credentials are
included.

```sh
cargo build -p cloud
cargo clippy -p cloud --all-targets -- -D warnings
cargo test -p cloud
cargo run -p cloud --example status
```

The example selects the first online printer and listens for 30 seconds. It only
connects, subscribes, selects a monitoring target, and sends `pushing/pushall` telemetry
requests. It never invokes controls, printing, binding, logout, device certificate
installation, or firmware operations. Native stdout/stderr are redirected to
`/dev/null`; sanitized output uses a separate descriptor. Account details, custom
printer names, arbitrary JSON, and native logs are not printed; device IDs show at
most their final four characters.

Optional environment variables: `BAMBU_PLUGIN`, `BAMBU_STUDIO_CONFIG`,
`BAMBU_COUNTRY`, `BAMBU_CERTIFICATE`. The default plugin is
`~/.config/BambuStudio/plugins/libbambu_networking.so`. The certificate is the public
`slicer_base64.cer` from an installed Studio/Orca resource directory; common install
locations are searched, including `/opt/orca-slicer/resources/cert`. Connecting is
refused if no certificate was configured. Studio's `app.region` determines the API
country code: Europe/USA/North America → `US`, China → `CN`, otherwise `Others`.

## ABI

The actual installed plugin reports **`02.06.00.50`**, despite being installed by a
newer Studio application. The local Orca reference at
`d28c71c485cf51949c5ed4f6ee59998b7f17566c` supports Legacy, 02.03, and 02.08.04;
**none of those print layouts is suitable** for this plugin.

The shim instead uses the layout from Bambu Studio
[`v02.06.00.51/bambu_networking.hpp`](https://github.com/bambulab/BambuStudio/blob/v02.06.00.51/src/slic3r/Utils/bambu_networking.hpp),
whose `BAMBU_NETWORK_AGENT_VERSION` explicitly equals `02.06.00.50`, and the
[matching function typedefs](https://github.com/bambulab/BambuStudio/blob/v02.06.00.51/src/slic3r/Utils/NetworkAgent.hpp).
It includes `task_timelapse_use_internal` and `extruder_cali_manual_mode`, but has no
`svc_context`, `slicer_uid`, or `queue_plate_id`. All other plugin versions are rejected
before creating an agent. Supporting another version requires a separately verified
layout and dispatch path, as in Orca's version table.

`build.rs` uses `cc`, C++17, the system libstdc++, and the C++11 string ABI. Compile-time
checks require 32-byte `std::string`, 856-byte `PrintParams`, and the expected field
offsets. `dlopen(RTLD_NOW | RTLD_LOCAL | RTLD_NODELETE)` and matching C++ function
pointers keep all STL objects on the C++ side. Rust sees only C integers, pointers,
byte buffers, and callbacks. C++ exceptions are caught at the C boundary.

One agent may exist per process. `Client` is intentionally neither Send nor Sync;
create it on the worker thread that owns it. Plugin callbacks serialize through a
shared gate and copy into a bounded channel without blocking plugin threads.
`Events` can move to another thread. Shutdown closes the gate before releasing Rust
callback state. The library stays mapped for late native callbacks. Explicit
`client.shutdown()` reports a destruction failure; a failed destruction prevents a
new agent in that process. Offline integration tests exercise version rejection,
config isolation, cross-thread delivery, and callbacks after destruction.

## Startup and session

`Client::load(Options::from_bambu_studio()?)` performs:

1. Create a mode-0700 temporary root with private `config` and `log` directories.
2. Copy **only** the regular `BambuNetworkEngine.conf` file, mode 0600. Symlinks are
   refused. The plugin decrypts its own session; Rust never decrypts or logs it.
3. Load/check the plugin and create the agent with the private log directory.
4. Set the private config directory, initialize logs, set certificate/callbacks and
   country code, and disable telemetry tracking.

Call `start()`, check `is_user_login()`, then `connect_server()`. Wait for
`is_server_connected()`, call `printers()`, then `subscribe(&dev_id)`. Subscription
selects the monitoring target in the isolated agent, matching Studio/Orca's device
selection sequence. A fresh telemetry request can return `-2` while that channel is
starting; the example retries once after five seconds. REST listing alone does not
prove the live telemetry channel is ready.

No original config path is ever passed to an agent operation. The source Studio config
is read only to determine region, and its encrypted session file is copied. Session
refreshes and native logs remain inside `Client::private_dir()` and are deleted on
normal shutdown. A crash can leave this private directory behind; it contains secrets.
The wrapper itself logs no payloads; other hosts must also suppress native console
output if they expose a console. Do not publish native logs, session files, account
getters, or explicitly accessed `JsonPayload::as_value()` contents.

If no session restores, **log in interactively in Bambu Studio on the same machine,
then create a fresh Client/rerun the example**. Never paste tokens into chat. No custom
login browser is implemented. In Studio/Orca, the web login returns a `user_login`
message containing access/refresh tokens, expiry data, and user identity; the host
passes that JSON to `bambu_network_change_user`. Orca's embedded web flow can exchange a
ticket via `get_my_token` and obtain the profile via `get_my_profile` first. Since
2026-11-06 the crate exposes `Client::login_with_tokens`, `tokens_from_ticket` and a
local `logout`; see docs/CLOUD.md.

## Events and actions

`Event::Status` contains the device ID, original JSON payload, and a typed accumulated
`PrinterStatus`: state, percentage, remaining minutes, layer counts, current/target
nozzle and bed temperatures, error code, speed level, Wi-Fi signal, lights, AMS units
and trays, and external `vt_tray`. Missing values remain `None`. Full `msg=0` snapshots
reset the cache; sparse updates merge objects and AMS/tray arrays by ID. Unknown fields
remain available in the explicitly accessed JSON. `Lagged` reports bounded-channel
loss and clears the cache; request another snapshot. Newer printer schemas and firmware
may require additional fields or delta rules.

`Options::allow_control` defaults to false. Only an explicit opt-in enables
`control(dev, Control::{Pause, Resume, Stop, Light, Speed})` or `start_print`.
**Neither was executed against the printer during verification.** Firmware authorization
and physical behavior of these operations remain unverified.

For cloud printing, build `PrintRequest::new(dev_id, gcode_3mf, config_3mf, project_name)`;
set the 1-based `plate_index`, task/preset names, explicit `bed_type` (for example
`textured_plate`), `use_ams`, `timelapse`, `bed_leveling`, `flow_cali`, and
`vibration_cali`. The input must be a sliced `.gcode.3mf`; `config_3mf` is the slicer's
separate configuration 3MF, as passed by Orca's PrintJob. Mapping fields are JSON
strings in Bambu's format: `ams_mapping` (for example `[0,1,-1]`), optional
`ams_mapping2`, `ams_mapping_info`, `nozzle_mapping`, and `nozzles_info`. Supply mappings
from the actual printer/project. Defaults leave AMS, timelapse, and calibrations off;
bed type must be chosen explicitly.

`start_print` blocks while the official plugin uploads/submits the job. Progress
stage/code events use the same channel; native text is discarded. Cancellation and
host-side acknowledgement waiting are not implemented (`WasCancelledFn=false`,
`OnWaitFn=true`). A successful return must be confirmed through subsequent printer
status; it is not a guarantee that a physical print began. LAN connection, firmware management, binding, and preset synchronization are not implemented.
The read-only cloud MJPEG camera API is described below.

The proprietary plugin remains in-process and can fault outside the wrapper's error
handling. An early prototype run without the certificate/readiness safeguards exited
with SIGSEGV; the guarded path completed subsequent live runs. A process boundary is a
future option for stronger fault isolation. Concurrent Studio sessions or cloud token
rotation may interrupt monitoring; never solve that by pointing this agent at the
original writable config.

## Verification

`cargo test -p cloud` covers typed telemetry/history, JPEG headers, and an offline
C++ integration test through both native ABIs.
`cargo build -p cloud` and Clippy with `-D warnings` pass. To repeat the live status
example while watching the entire source config tree for writes:

```sh
python3 crates/cloud/scripts/verify_read_only.py
```

This checks inotify mutation events plus before/after session/Studio-config content,
inode, permissions, timestamps, and size. It prints no hashes or file contents. If
Studio independently writes during the check, the script also reports those writes;
it cannot attribute them to a process. The root lockfile is intentionally excluded
from this workstream's commit per the requested file scope; Cargo regenerates its new
workspace-package entry when building.

Recorded on 2026-10-29 (redacted excerpt):

```text
Plugin: 02.06.00.50 (exact 02.06 ABI)
Logged in: true (isolated session copy)
Cloud connected: true
Printers: 1 (custom names omitted for privacy)
  #1: …1744 model=A1 online=true
Status updates: 15; no control or print commands sent.
Original config tree mutation events: 0; monitor overflow: False
Original session and Studio config content/metadata unchanged: True
```

The full snapshot reported `RUNNING`, 42% progress, layer 434/1067, nozzle target
245 °C, bed target 70 °C, `print_error=0`, and speed level 1. Initial sparse packets
correctly reported unavailable fields as `None` until the full snapshot arrived.

## W5: історія завдань

```rust,ignore
use cloud::{Client, JsonPayload, Task, TaskQuery};

// Методи Client:
fn get_user_tasks(&self, query: &TaskQuery) -> anyhow::Result<Vec<Task>>;
fn get_subtask_info(&self, subtask_id: &str) -> anyhow::Result<JsonPayload>;
```

Після `Client::load` викличте `start()` і дочекайтеся `is_user_login()`.
Для історії MQTT-з’єднання, підписка й `printers()` не потрібні. Запит синхронний.
`TaskQuery::default()` задає всі пристрої, `status=0`, `offset=0`, `limit=20`;
можна задати `dev_id: Some(DeviceId)`, статус, зміщення й ліміт 1–100.
Окремі сторінки запитує викликач; автоматичних повторів немає.

`Task` містить `id: String`, `title`, `design_name`, `status: Option<i32>`,
`dev_id: Option<DeviceId>`, `start_time`, `end_time`, `cost_time: Option<f64>`,
`weight: Option<f64>`, `plate_index: Option<i32>` та `cover_url`.
Рядкові поля, крім id, мають тип `Option<String>`. Час збережено у форматі сервера,
без припущення про часовий пояс; числові величини передаються без перерахунку.
Відсутні або некоректні необов’язкові поля дають `None`.
Коди статусу з Orca TaskManager: 1/4 — виконується, 2 — успіх, 3 — невдача;
невідомі числові коди лишаються доступними.
`task.raw.as_value()` повертає початковий JSON конкретного завдання, включно з
невідомими полями. Назви, id, підписані cover URL і raw JSON не слід логувати.
`Debug` для Task/JsonPayload приховує ці дані.

Новий shim op **18** викликає `bambu_network_get_user_tasks` із точною структурою
02.06: `std::string dev_id; int status; int offset; int limit;` — 48 байтів,
зсуви 32/36/40 перевіряє компілятор. Op **19** викликає
`bambu_network_get_subtask_info`, перевіряє код HTTP і повертає `task_json`.
Тіла помилок не перетинають межу Rust у тексті помилки.
Джерела ABI — ті самі [bambu_networking.hpp](https://github.com/bambulab/BambuStudio/blob/v02.06.00.51/src/slic3r/Utils/bambu_networking.hpp)
і [NetworkAgent.hpp](https://github.com/bambulab/BambuStudio/blob/v02.06.00.51/src/slic3r/Utils/NetworkAgent.hpp).

```sh
cargo run -p cloud --example tasks
```

Приклад показує останні п’ять записів: приховану назву, відомий стан, лише дату
і останні чотири символи пристрою. Довільна назва прихована повністю, оскільки
може містити email, пароль або серійний номер. `BAMBU_DEV_ID` необов’язково
обмежує історію конкретним пристроєм. Список принтерів не запитується.

## W5: хмарна камера MJPEG

```rust,ignore
use cloud::{CameraStream, Client, DeviceId, Frame};

impl<'client> CameraStream<'client> {
    pub fn open(client: &'client Client, dev: &DeviceId) -> anyhow::Result<Self>;
    pub fn next_frame(&mut self) -> anyhow::Result<Frame>;
    pub fn stop(&mut self) -> anyhow::Result<()>;
}
// Frame { jpeg: Vec<u8>, width: u32, height: u32, timestamp: u64 }
```

Перед `open` потрібні `start()`, відновлена сесія і готове `connect_server()`.
Камера не викликає `printers()`, не підписується на MQTT і не надсилає жодної
команди друку або керування. Позика Client не дозволяє знищити його раніше за
CameraStream. Шлях `libBambuSource.so` береться поруч із вибраним networking-плагіном.

У [MediaPlayCtrl.cpp Studio 02.06](https://github.com/bambulab/BambuStudio/blob/v02.06.00.51/src/slic3r/GUI/MediaPlayCtrl.cpp)
запит має формат `dev_id|ota_version|"tutk","agora"`, а callback повертає
`bambu:///...` з приватними даними авторизації. Op **20** очікує цей callback
до 20 с, безпечний для пізніх відповідей і ніколи не додає URL до помилок.
Порожня OTA-версія дозволяє обійти попередні MQTT-запити до принтера.
URL живе лише в пам’яті; до нього додаються device/net_ver/cli_ver, як у Studio.

Orca `GUI/Printer/BambuTunnel.h`, `wxMediaCtrl3.cpp` і `AVVideoDecoder.cpp`
описують послідовність `Bambu_Create → SetLogger → Open → StartStream(true) →
GetStreamInfo → ReadSample → Close → Destroy`. Вона виконується на окремому
потоці. `Bambu_would_block=2` опитується з паузою; потік не створює іншого
networking-агента. ABI перевіряє 40-байтний StreamInfo та 32-байтний Sample.
Підтримується саме `VIDE/MJPG/video_jpeg`; H.264/AVC1 відхиляється кодом -10007.
BambuSource розпаковує транспорт, але JPEG не декодується в RGBA: байти кадру
копіюються до наступного `ReadSample`, розміри читаються з JPEG SOF,
`timestamp` зберігає вихідний decode_time у тактах 100 нс. Повного декодера
JPEG у бібліотеці немає; споживач може використати власний.

Черга вміщує два кадри; при переповненні нові кадри відкидаються.
`next_frame()` чекає до 30 с. `stop()` і Drop приєднують потік і звільняють тунель,
навіть якщо читач перестав забирати кадри. Завислий синхронний виклик самого
закритого SDK неможливо примусово перервати: це обмеження in-process інтеграції.
Автоматичного оновлення Agora URL через `refresh_url` і повторного з’єднання
немає; для тривалих сеансів потрібна подальша реалізація поновлення авторизації.
Встановлені `liblive555.so` і `libagora_rtc_sdk.so` залишаються власністю Studio;
жодні пропрієтарні бінарники не копіюються в репозиторій.

```sh
cargo run -p cloud --example camera -- --frames 5
cargo run -p cloud --example camera -- --frames 3 --save /tmp/x.jpg
```

Без `--save` кадри не записуються. З ним лише перший кадр зберігається у `/tmp`,
з правами 0600, без перезапису наявного файлу. Приклад друкує лише розміри,
кількість байтів і fps. `BAMBU_DEV_ID` пропускає `printers()`; без нього виконується
один запит списку і вибирається перший принтер онлайн. Автоматичних повторів немає;
після помилки списку зачекайте щонайменше 60 с. Цей override підтримує і `status`.
Не запускайте приклади одночасно. Native stdout/stderr приховані; у власному хості
теж потрібно придушувати native-вивід, який може містити секрети.

Контроль початкової конфігурації під час читання:

```sh
python3 crates/cloud/scripts/verify_read_only.py tasks
python3 crates/cloud/scripts/verify_read_only.py camera --frames 3
```

Жива перевірка історії 2026-10-29 (редагований уривок):

```text
Завдань: 5; список принтерів не запитувався
Назва: [приховано]; стан: невдача; дата: 2026-10-29; пристрій: …1744
Назва: [приховано]; стан: успіх; дата: 2026-10-23; пристрій: …1744
Original config tree mutation events: 0; monitor overflow: False
Original session and Studio config content/metadata unchanged: True
```

### Перевірено наживо 2026-10-29

- `tasks`: 5 записів історії, усі з пристрою …1744. REST працює, хоча список принтерів (op 10) у той самий час повертав -11.
- `camera`: камера відповідає лише тоді, коли в запиті є версія прошивки. Порожня OTA-версія дає таймаут callback (-10006). Приклад сам підписується на принтер, надсилає `info/get_version` (лише читання; перший запит може повернути -2, поки канал прогрівається) і бере версію через `cloud::ota_version`. A1 (прошивка 01.04.60.02, `tutk_server=disable`) віддав MJPEG 1536×1080 із частотою близько 0.6 кадра/с.
- `request_status`, `request_version` і `control` приймають `&self` (лічильник послідовності — `Cell`), тож `CameraStream` може жити поруч із клієнтом. Для циклу подій є неблокувальний `CameraStream::try_frame()`.

### Сертифікати TLS (причина -11 / -1)

Вбудований у плагін curl/OpenSSL шукає CA-сертифікати за шляхом зі своєї збірки. На Arch/CachyOS цього шляху немає, тому без `SSL_CERT_FILE`/`SSL_CERT_DIR` відмовляють **усі REST-запити**: `printers()` повертає -11, історія — -1. MQTT (телеметрія, керування) при цьому працює, бо використовує інший сертифікат. `cargo run` успадковує ці змінні від rustup, тому приклади через cargo «інколи працювали», а той самий бінарник напряму — ні. Хост має викликати `unsafe { cloud::configure_tls_env() }` на старті процесу, до появи інших потоків.
