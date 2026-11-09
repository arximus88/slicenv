# Принтери Klipper (Moonraker)

Стан на 2026-11-09. Код: крейт `crates/moonraker` (без GUI), інтеграція — `crates/app/src/device/klipper.rs`.

Slicenv надсилає G-code на принтери з Klipper через [Moonraker HTTP API](https://moonraker.readthedocs.io): Creality з Klipper (Ender 3 V3 SE з Creality Helper Script тощо), Voron, саморобні принтери з Mainsail чи Fluidd.

> **Чесно:** усе перевірено лише проти емулятора Moonraker (`crates/moonraker/src/fake.rs`) на 127.0.0.1 — юніт-тестами й вручну в GUI. На справжньому принтері з Klipper не запускалося. Що варто перевірити першим — у кінці документа.

## Що підтримано

| Можливість | Ендпоінт Moonraker | Як перевірено |
|---|---|---|
| Адреса: `host`, `host:порт`, `http://…`, `https://…`, `[IPv6]:порт`, префікс шляху | — | тест |
| API-ключ (`X-Api-Key`), 401 → зрозуміле повідомлення | усі запити | тест, GUI (емулятор) |
| Перевірка з’єднання й стан Klippy (ready / startup / shutdown / error / не з’єднаний) | `GET /server/info`, `GET /printer/info` | тест, GUI (емулятор) |
| Стан друку: стан, файл, прогрес, шари, час, сопло, стіл, осі вдома | `GET /printer/objects/query?print_stats&virtual_sdcard&display_status&extruder&heater_bed&toolhead` | тест, GUI (емулятор) |
| Завантаження G-code з прогресом: «Завантажити й друкувати» (`print=true`) або «Лише завантажити» | `POST /server/files/upload` (multipart, `root=gcodes`) | тест, GUI (емулятор) |
| Файл з такою назвою вже є → помилка або заміна за галочкою | `GET /server/files/list?root=gcodes` | тест, GUI (емулятор) |
| Перед друком: Klippy `ready` і принтер не друкує | `/server/info`, `/printer/objects/query` | тест |
| Обрив з’єднання під час передачі → «З’єднання обірвалося під час передачі файлу» | — | тест |
| Пауза / продовжити / скасувати (скасування — з підтвердженням) | `POST /printer/print/pause`, `/resume`, `/cancel` | тест, GUI (емулятор) |
| Камера: знімок раз на секунду з `snapshot_url` першої камери, віддзеркалення й поворот з налаштувань | `GET /server/webcams/list` + `snapshot_url` | тест, GUI (емулятор) |
| Кнопка «Відкрити Mainsail / Fluidd» | — | GUI (лише відкриття URL) |

Не зроблено:
- **Пошук у мережі (mDNS / zeroconf `_moonraker._tcp`)** — у workspace немає mDNS-крейта, а нову залежність заради цього не додавали. Принтер додається вручну за адресою.
- Температури, рух головки, вентилятори, макроси, консоль G-code — лише в Mainsail чи Fluidd. На сторінці «Пристрій» для Klipper є тільки стан, пауза, продовження й скасування.
- WebSocket-підписка на стан: Slicenv опитує вибраний принтер HTTP-запитами раз на 2 с, інші збережені — раз на 30 с (лише `/server/info`).
- Потокове відео (MJPEG-потік, WebRTC): лише знімки `snapshot_url`. Камера без `snapshot_url` — повідомлення й посилання на вебінтерфейс.
- Черга завдань Moonraker (`job_queue`): результат `print_queued` показується, але чергою Slicenv не керує.
- Мініатюри й метадані файлу: «лишилось» рахується грубо з тривалості й прогресу.
- Вхід логіном і паролем Moonraker (`/access/login`, JWT) — лише API-ключ або `trusted_clients`.

## Як додати принтер

1. Наріжте модель з профілем **свого** принтера (наприклад, «Creality Ender-3 V3 SE 0.4 nozzle» в OrcaSlicer). Якщо вибрано профіль Bambu Lab, діалог друку попередить.
2. «Пристрій» → у лівому доку блок **«Klipper (Moonraker)»** → **+**.
3. **Назва** — будь-яка. **Адреса** — та, за якою відкривається Mainsail чи Fluidd:
   - `192.168.1.60` або `ender.local` без порту — Slicenv іде на типовий порт Moonraker **7125**;
   - `http://192.168.1.60` — порт 80 (Moonraker за nginx Mainsail/Fluidd, який проксує `/server`, `/printer`);
   - `https://…` — через системний `curl` (див. нижче).
4. **API-ключ** — лише якщо в `moonraker.conf` увімкнено `[authorization]` і ваша мережа не в `trusted_clients`. Ключ — у Mainsail (Налаштування → Авторизація) або у `~/printer_data/moonraker.secrets`. Без ключа там, де він потрібен, — повідомлення про 401.
5. **«Перевірити й додати»**: Slicenv робить `GET /server/info` і `/printer/info` (лише читання) і зберігає принтер.

Після нарізки кнопка **«Друкувати»** відкриває діалог «Надіслати на Klipper»: назва файлу на принтері (`<проєкт>_plate<N>.gcode`), галочка «Замінити файл, якщо такий уже є на принтері», кнопки **«Лише завантажити»** і **«Завантажити й друкувати»**. Надсилається чистий `plate_N.gcode`, який рушій пише поруч із `.gcode.3mf`; якщо його немає, він витягується з архіву.

## Де що зберігається

- `~/.config/slicenv/klipper-printers.json` — назва, адреса, API-ключ. Файл з правами 0600 у каталозі 0700, запис атомарний — як `lan-printers.json` з кодами доступу Bambu (`moonraker::store`).
- `ApiKey` не має `Display`, `Debug` друкує `ApiKey(***)`; ключ іде лише в заголовок `X-Api-Key`. Для https він передається `curl` конфігом через stdin, не в argv. `SLICENV_DEBUG=1` пише лише `[klipper] …` без адрес і ключів.
- Ключ надсилається лише на адресу самого Moonraker. Якщо камера на іншому хості чи порту, знімок запитується без ключа.

## Технічні подробиці

- **HTTP.** `http://` — власний мінімальний HTTP/1.1 клієнт на `std::net` (`moonraker::http`): `Connection: close`, `Content-Length` і chunked, multipart потоком із файлу з прогресом і скасуванням. `https://` — системний `curl` (`--proto =https`), як у крейті `cloud`; прогрес тоді лише 0 % → 100 %. Нових мережевих залежностей немає.
- **Окремий крейт**, а не модуль `lan`: `lan` — протоколи Bambu (MQTT, FTPS, BBL CA, rustls), спільного коду з Moonraker немає. `moonraker` залежить лише від `paths`, `serde`, `serde_json` і входить у CI-перевірку «без wgpu/GPUI».
- **Відносний `snapshot_url`** (`/webcam/?action=snapshot`, як у Mainsail) береться з вебсервера того самого хоста: якщо Moonraker на 7125, знімок іде на порт 80/443, інакше — на порт з адреси. Абсолютний URL — як є.
- **Стан → сторінка «Пристрій».** `print_stats.state` відображається у стани Bambu, які вже вміє показувати сторінка: `printing` → «Друкує», `paused` → «Пауза», `complete` → «Завершено», `error` → «Помилка», `standby` і `cancelled` → «Очікує». Прогрес — `virtual_sdcard.progress`, без нього — `display_status.progress`.
- **Помилки** українською з ключами в `en.json` (`MoonrakerError::key()`): немає зв’язку, тайм-аут, 401, Klippy не ready (зі станом і `state_message`), файл уже є, принтер друкує, обрив під час передачі, інша HTTP-помилка з повідомленням Moonraker.
- `SLICENV_KLIPPER_OFF=1` вимикає робочий потік Klipper.

## Перевірка без принтера

```bash
# Юніт- та інтеграційні тести проти фейкового Moonraker на 127.0.0.1
cargo test -p moonraker
# Фейковий Moonraker для ручної перевірки GUI (лише 127.0.0.1)
cargo run -p moonraker --example fake_moonraker -- --port 7125 --api-key 0123456789abcdef0123456789abcdef
#   --klippy shutdown   Klippy у shutdown з повідомленням MCU
#   --printing          уже друкує
#   --speedup 5         «друк» іде вп’ятеро швидше
#   --no-camera         без камери
```

GUI — лише з тимчасовими `HOME`/`XDG_*` і `SLICENV_CLOUD_OFF=1 SLICENV_LAN_OFF=1` (skill `real-engine-test`); у тимчасовому HOME принтер додається з адресою `127.0.0.1:7125`.

Сценарії, пройдені вручну в GUI проти емулятора (Xvfb, OrcaSlicer 2.4.2, профіль Creality Ender-3 V3 SE): додавання без ключа (401) і з ключем; сторінка «Пристрій» зі станом, температурами, версіями й камерою; «Завантажити й друкувати» з прогресом; пауза; скасування з підтвердженням; «Лише завантажити» з наявним файлом (помилка); зупинений емулятор («немає зв’язку»); Klippy shutdown.

## Що перевірити на справжньому Klipper

1. **Адреса й порт.** Creality Helper Script, KIAUH, Sonic Pad, Creality OS (K1) — Moonraker на 7125 чи лише за nginx на 80; чи проходять `/server/*` через nginx.
2. **`trusted_clients` і API-ключ:** доступ без ключа з домашньої мережі; з ключем — 401 при неправильному.
3. **Завантаження великого G-code** (десятки МБ) по Wi-Fi: швидкість, прогрес, тайм-аут 120 с після передачі (Moonraker аналізує метадані файлу).
4. **Імена файлів з кирилицею й пробілами** у multipart (`filename="Кубик 1_plate1.gcode"`): чи зберігає Moonraker їх як є.
5. **`print=true`**: чи стартує друк і чи `print_started` правдивий; поведінка з увімкненим `job_queue` (`print_queued`).
6. **Стан під час друку:** `print_stats.info.current_layer/total_layer` є лише якщо слайсер пише `SET_PRINT_STATS_INFO` (OrcaSlicer — так, з відповідним профілем). Без цього шарів не буде.
7. **Пауза / продовжити / скасувати** — залежать від макросів `PAUSE`, `RESUME`, `CANCEL_PRINT` у `printer.cfg` (Mainsail/Fluidd їх додають). Перевіряти лише разом із людиною біля принтера.
8. **Камера:** crowsnest / mjpg-streamer — чи `snapshot_url` відносний і на якому порту; камери Creality (K1) — чи є `snapshot_url` взагалі.
9. **https** через reverse proxy з самопідписаним сертифікатом: `curl` перевіряє сертифікат системним сховищем; самопідписаний не пройде (вимикати перевірку Slicenv не вміє і не буде).
10. **Klippy у `startup`** одразу після ввімкнення принтера: повідомлення «Klipper не готовий» має зникнути саме, коли Klippy стане `ready`.
