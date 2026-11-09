# Режим LAN для принтерів Bambu Lab

Стан на 2026-11-06. Код: `crates/lan` (чистий Rust, без `libbambu_networking.so` і без
акаунта), інтеграція — `crates/app/src/device/lan.rs`.

## Що працює і як перевірено

Позначки: **наживо** — на справжньому принтері в мережі розробника; **тест** — юніт- чи
інтеграційний тест на синтетичних даних; **не перевірено** — реалізовано за джерелами, наживо
не запускалось (дії з фізичними наслідками — лише разом із користувачем).

| Можливість | A1 (N2S, прошивка 01.04.60.02, режим cloud) | P2S (N7, 01.03.00.00, режим cloud) |
|---|---|---|
| Виявлення (NOTIFY на UDP 2021) | наживо | наживо |
| Відповідь на M-SEARCH | **немає** (лише періодичні NOTIFY) | наживо, `HTTP/1.1 200 OK` з портів 2021 і 1990 |
| TLS: ланцюжок до BBL CA + CN = SN, порти 8883 / 990 / 6000 | наживо | наживо (6000 — через кеш проміжного CA, див. нижче) |
| MQTT: вхід `bblp`/код, `pushall`, злиття звітів, `get_version` | наживо | не перевірено (немає коду доступу); невірний код → `BadAccessCode` наживо |
| FTPS: вхід (без запису файлів) | наживо | не перевірено |
| Камера JPEG на 6000 (один кадр, 111 КБ) | наживо | не перевірено: порт 6000 відкритий (TLS пройшов), але за OpenBambuAPI P2S має RTSPS; у застосунку камера P2S вимкнена |
| Застосунок: збережений LAN-принтер, автовибір, статус, версії модулів | наживо (`XDG_CONFIG_HOME` у тимчасовій теці, `SLICENV_CLOUD_OFF=1`) | — |
| Пауза / продовжити / стоп / світло | тест (формат команд) | не перевірено |
| Завантаження .gcode.3mf + `project_file` | тест (FTPS проти локального TLS-сервера, формат команди) | не перевірено |
| Відмова прошивки (Authorization Control) → `ControlLocked` | тест (класифікація відповіді) | не перевірено |

Інтерфейс (лівий док «Локальна мережа (LAN)», модалка додавання, позначка «LAN» у списку)
зібраний і пройшов clippy, але візуально не оглядався.

Живу перевірку повторити: `cargo run -p lan --example probe -- --tls` (лише виявлення й
TLS-рукостискання, без коду) або з кодом у файлі:
`SLICENV_LAN_CODE_FILE=… cargo run -p lan --example probe -- --model N2S --watch --ftp --camera`.
Зонд нічого не змінює на принтері.

## Протоколи

### Виявлення
- Принтер шле `NOTIFY * HTTP/1.1` з вихідного порту 1900 на UDP 2021 (у тілі
  `HOST: 239.255.255.250:1900`). Слухаємо 2021 з `SO_REUSEADDR` + `SO_REUSEPORT` (порт може
  тримати Bambu Studio/Orca), приєднуємось до 239.255.255.250.
- Поля: `USN` — серійний номер, `Location` — IP, `DevModel.bambu.com` (N2S, N1, C11, C12,
  N7 = P2S, BL-P001 = X1C, O1D = H2D…), `DevName`, `DevConnect` (`lan`/`cloud`), `DevBind`
  (`free`/`occupied`), `DevSignal`, `DevVersion` (прошивка), `DevCap`, `Devseclink`.
- M-SEARCH (`ST: urn:bambulab-com:device:3dprinter:1`) шлемо на 239.255.255.250:1990/2021 і
  255.255.255.255:2021 раз на 30 с — відповідає P2S, A1 мовчить.
- Якщо мультикаст/бродкаст блокує роутер (інший VLAN) — ручний ввід IP.

### TLS (8883, 990, 6000)
- Сертифікати видає Bambu: A1 — лист **X.509 v1** `CN=<SN>` напряму від «BBL CA»;
  P2S — лист v3 від «BBL Device CA N7-V2» ← «BBL CA2 RSA». Корені — п’ять сертифікатів із
  `resources/cert/printer.cer` Bambu Studio (`crates/lan/assets/bbl-ca.pem`: BBL CA до 2032,
  BBL CA2 RSA/ECC самопідписані до 2050 і крос-підписані BBL CA).
- webpki не приймає v1, а ім’я в сертифікаті — серійник, не IP. Тому власний
  `ServerCertVerifier` (`tls.rs`): розбір `x509-parser`, підписи ланок і рукостискання —
  алгоритми ring із rustls, перевірка термінів, `CA:TRUE` у проміжних, `CN == SN`
  (без урахування регістру). Нічого не вимкнено.
- P2S на порту 6000 надсилає лише лист, без проміжного CA. Проміжні, що вже довели ланцюжок
  до кореня (на 8883/990), кешуються в процесі й додаються до наступних перевірок; довіри
  це не розширює — підписи перевіряються щоразу (тест
  `verified_intermediate_is_cached_for_connections_without_it`). Якщо камеру P2S відкрити
  до будь-якого іншого з’єднання, вона не пройде перевірку — це очікувано.
- Невідомий CA → `LanError::UntrustedCertificate`, чужий серійник → `SerialMismatch`.

### MQTT (8883)
- `bblp` / код доступу, keep-alive 30 с, clean session. Звіти — `device/<SN>/report`,
  команди — `device/<SN>/request`.
- `pushall` (`{"pushing":{"command":"pushall","version":1,"push_target":1}}`) не частіше
  1 разу на 3 с (`REQUEST_PUSH_MIN_TIME` в Orca); пульс кожні 10 с, бо A1/P1 шлють лише
  зміни, а охоронцю руху головки потрібен знімок не старший за 30 с.
- Часткові `push_status` зливаються тим самим `cloud::merge_status`, що й хмарні; `msg: 0`
  — повний знімок, стан скидається. Розбір — `cloud::PrinterStatus`, команди —
  `cloud::Control::message` (той самий формат і ті самі межі, що для хмари).
- Після обриву rumqttc перепідключається сам; підписка й `pushall` відновлюються.
- Відповідь із `result: fail…` → `LanEvent::Rejected`; причина зі словами verify/auth/sign
  → `ControlLocked` (евристика: точних текстів відмови Authorization Control у джерелах немає).

### FTPS (990) і друк
- Неявний TLS одразу після TCP; `USER bblp`, `PASS <код>`, `PBSZ 0`, `PROT P`, `TYPE I`,
  `PASV` (хост із відповіді ігноруємо — дані на ту саму IP), `STOR`. Канал даних — той самий
  `Arc<ClientConfig>`, rustls відновлює TLS-сесію (vsftpd на принтері цього вимагає, як
  `ImplicitFTP_TLS` у ha-bambulab). Власний мінімальний клієнт замість suppaftp — заради тієї
  самої перевірки TLS, точних кодів (530 → `BadAccessCode`) і прогресу/скасування.
- Файл кладемо в корінь FTP (на A1/P1 це microSD, `ftp_folder: sdcard/` в Orca), ім’я —
  ASCII `<проєкт>.gcode.3mf`.
- Команда (`print.rs`, поля за OpenBambuAPI `print.project_file` і ha-bambulab):
  `param: Metadata/plate_<N>.gcode`, `url: ftp:///<файл>`, `md5` (верхній регістр, як
  `bbl_calc_md5` у Studio), `project_id/profile_id/task_id/subtask_id: "0"`, `subtask_name`,
  `use_ams`, `ams_mapping` (масив; `-1` — зовнішня котушка), `bed_type` (з 3MF, інакше
  `auto`), `bed_leveling`, `flow_cali`, `vibration_cali`, `layer_inspect`, `timelapse`.
  OpenBambuAPI пише `bed_levelling`, ha-bambulab і Studio (`task_bed_leveling`) —
  `bed_leveling`; беремо друге. Наживо не підтверджено.
- A1/P1 без microSD друкувати не можуть: перед надсиланням перевіряємо стан SD зі звіту
  (`home_flag`/`aux`) → `LanError::NoSdCard`; 550/553/452/552 зі згадкою card/storage — теж.

### Камера
- A1, A1 mini, P1P, P1S: TLS 6000, 80-байтна автентифікація (0x40, 0x3000, 0, 0, `bblp`,
  код по 32 байти), далі 16-байтний заголовок (розмір LE) + JPEG. Наживо на A1 — так.
- X1, P2S, H2D/H2S/H2C: RTSPS `rtsps://bblp:<код>@<ip>:322/streaming/live/1` (H.264).
  **Наступний крок**: клієнт RTSP поверх TLS (наприклад `retina`) з тим самим верифікатором +
  декодер H.264. На H2 порт 322 закритий, доки на екрані не ввімкнено «LAN Only Liveview»
  (`ipcam.rtsp_url = "disable"` у звіті); на нашому P2S порт 322 теж закритий.

## Обмеження прошивок (Authorization Control)

З січня 2025 Bambu додає Authorization Control (X1 з 01.08.03.00, потім P1, A1; H2D, P2S —
з коробки). За даними спільноти (не перевірено наживо):

| | Без LAN Only Mode / Developer Mode | LAN Only Mode + Developer Mode |
|---|---|---|
| Статус (MQTT report), `pushall` | працює (A1 у режимі cloud — наживо) | працює |
| Керування, `project_file` | відхиляється прошивкою → `ControlLocked` | працює |
| FTPS | вхід працює (A1 — наживо); запис може бути обмежено | працює |
| Камера | A1 у режимі cloud — наживо | працює |

Тож для друку без хмари користувач має ввімкнути на екрані принтера **LAN Only Mode** і
**Developer Mode**; в інтерфейсі це сказано в модалці додавання й у тексті помилки
`ControlLocked`. Принтери тримають обмежену кількість MQTT-клієнтів: застосунок з’єднується
лише з вибраним LAN-принтером.

## Застосунок

- Лівий док «Пристрій» → блок «Локальна мережа (LAN)»: знайдені в мережі (клік відкриває
  модалку з IP і серійником), «+» — ручне додавання (IP, серійник, код). «Перевірити й
  додати» виконує `lan::test_connection` (лише читання) і лише після успіху зберігає.
- Збережені LAN-принтери стоять у загальному списку з позначкою «LAN»; якщо принтер є і в
  акаунті, перемагає LAN. Команди з інтерфейсу маршрутизує `LanUi::route`; охоронець
  (`device/guard.rs`) перевіряє їх і в інтерфейсі, і в робочому потоці LAN.
- «Друкувати» відкриває ту саму модалку підтвердження, з приміткою про LAN; далі — SD,
  MD5, FTPS із прогресом у доку, `project_file`.
- Нова IP від DHCP підхоплюється з анонсів автоматично.
- LAN не залежить від хмари: працює і з `SLICENV_CLOUD_OFF=1`. `SLICENV_LAN_OFF=1` вимикає LAN.

### Код доступу
`~/.config/slicenv/lan-printers.json` (`$XDG_CONFIG_HOME/slicenv`), файл 0600, тека 0700,
запис атомарний. `AccessCode` не має `Display`, `Debug` друкує `***`; у `SLICENV_DEBUG` —
лише останні 4 символи серійника. Поле коду замасковане (`InputState::masked`) з кнопкою
«показати»; у gpui-component 0.8.1 `value()` маскованого поля повертає введений текст
(перевірено: код проходить валідацію `AccessCode` і доходить до `test_connection`).

## API для майстра першого запуску

```rust
let found = lan::discovery::discover(Duration::from_secs(8))?;      // Vec<Discovered>
let mut printer = lan::LanPrinter::new(&found[0].serial, &found[0].ip.to_string(), code)?;
printer.model = found[0].model.clone();
let report = lan::test_connection(&printer, Duration::from_secs(15))?; // статус + прошивка
lan::store::upsert(printer)?;                                        // 0600
```
Помилки — `lan::LanError` (`key()` — ключ перекладу, `detail()` — технічна деталь).
Фонове виявлення — `lan::discovery::Discovery::start()`; перевірка FTPS без запису —
`lan::ftps::check_login`.

## Відкриті ризики
- Друк, керування, світло й запис через FTPS наживо не запускались.
- Точний текст відмови Authorization Control невідомий — класифікація евристична.
- Поведінка FTPS на P1/X1/H2D (каталог, вимога SD, повторне використання сесії) — за
  джерелами, не перевірена.
- Модель для ручно доданого принтера без анонсу: у модалці додавання вибирається вручну
  (за замовчуванням — з профілю принтера), для вже збереженого без моделі — кнопки моделей у
  блоці LAN (`LanCmd::SetModel`); пізніше її уточнює `get_version`. Чому «Друкувати»
  неактивна — підказка на кнопці (`Workspace::print_blocker`).
- Майстер налаштування (`crates/app/src/wizard.rs`) додає LAN-принтер тим самим
  `LanCmd::Add` (перевірка `test_connection` і лише потім збереження).
