# Відкритий плагін Open Bamboo Networking (збірка persano) на Linux

Стан на 2026-11-10. Рішення дизайнера від 2026-11-10: на Linux друк на Bambu йде через форк
Open Bamboo Networking (OBN) від persano. Цей форк друкує з **Developer Mode OFF**. Його
підключають **лише за явним вибором користувача**.

Код Slicenv:
- `crates/cloud/src/obn.rs` — пошук, установка, `obn.conf`, вибір, реліз GitHub, дочірній друк;
- `crates/cloud/native/shim.cpp` — `cloud_print`, режим 1 = `start_local_print`;
- `crates/app/src/open_plugin.rs` — модалка й дочірній процес `--obn-local-print`;
- точки входу:
  - Налаштування → «Плагіни й хмара»;
  - майстер → «З’єднання»;
  - діалог друку на Linux без плагіна.

Попередній експеримент з усіма варіантами друку через інтернет — `docs/LINUX-CLOUD-PRINT.md`
у гілці `t3/linux-bambu-studio-handoff`.

## Правило проєкту

Сторонній плагін вмикається лише як **явний вибір користувача**: людина ставить галочку «Я
розумію ризики» під попередженням і сама вибирає джерело. Джерел два:
- копія вже встановленого плагіна (OrcaSlicer / Bambu Studio);
- реліз persano на GitHub, завантажений за згодою.

Бінарники форку й вшитий у них секрет ніколи не потрапляють у репозиторій, у бінарник Slicenv,
у zip чи пакунки. Без вибору OBN не підхоплюється навіть тоді, коли лежить у
`~/.config/OrcaSlicer/plugins`: `plugin::find_plugins` відкидає такі файли за рядками в
бінарнику (`obn::is_open_plugin`).

## Як persano друкує (за кодом, гілка `option-b-cloud-rescue`, `e7db84b`)

Апстрім — `ClusterM/open-bamboo-networking`, AGPL-3.0. Форк додає 149 комітів.

### Звідки ключі

| | ClusterM (апстрім) | persano |
|---|---|---|
| `bambu_network_update_cert` | заглушка, повертає 0 | `appcert::fetch_and_store` |
| Ключі слайсера | лише від користувача: `slicer_key_pem` / `slicer_cert_pem` / `slicer_crl_pem` в `obn.conf` або `slicer_*.pem` у теці конфігурації | так само, а якщо їх немає — плагін отримує їх сам |
| Вшитий секрет | немає | `client_auth_secret`, 43 байти ASCII (`appcert_cipher.cpp`, `OBN_EMBED_SECRET=ON` за замовчуванням); перевизначення — змінна `OBN_CLIENT_AUTH_SECRET` або файл `<config>/client_auth_secret.txt` |

Отримання ключа (`appcert_fetch.cpp`):
1. `GET <api>/v1/iot-service/api/user/applications/{enc_secret}/cert?aes256=…&ver=1`, де
   `enc_secret` — секрет, зашифрований AES-GCM ефемерним ключем. Ефемерний ключ загорнутий
   публічним ключем `CN=service.bambulab.com`.
2. Відповідь містить `cert`, `crl` і загорнутий `key`.
3. Плагін звіряє модуль RSA ключа й листа і атомарно пише `slicer_cert.pem`, `slicer_crl.pem`,
   `slicer_key.pem` у `config_dir` (`set_config_dir`).

Окремий виклик не потрібен: `ensure_app_cert_material_` запускає отримання сам, коли команду
треба підписати, а матеріалу немає. Orca `update_cert` не викликає.

У `<config>/certs/<серійник>.pem` плагін кладе сертифікат принтера (його повертає принтер у
відповідь `printer_cert`, `agent.cpp`).

### Підпис

`Agent::send_message` і `send_message_to_printer` працюють так:
1. Перед підписаною командою (`print`, `project_file`…) викликають `wait_for_app_cert` (до 8 с).
2. Той надсилає на принтер `security.app_cert_install` (ланцюжок + CRL) і чекає підтвердження.
3. Далі `signing::maybe_sign`: RSA-PKCS#1 v1.5 + SHA-256, `url` → `url_enc`.

Усе це в межах одного виклику друку. Окремо викликати `install_device_cert` не потрібно.

### LAN (`bambu_network_start_local_print` → `run_local_print_job`)

- Обов’язкові поля: `dev_ip` і `password` (код доступу); інакше -6010. `username` = `bblp`.
- Файл іде у FTPS :990 у корінь (`ftp_folder` порожній, як у закритого плагіна). Для
  `try_emmc_print` — у eMMC через :6000.
- Далі плагін сам відкриває LAN MQTT (`ensure_lan_session`) і публікує підписаний
  `{"print":{"command":"project_file",…}}`.
- Хмара не потрібна; `block_cloud` не заважає. Ключ слайсера отримується HTTPS-запитом до
  `api.bambulab.com` (див. вище) — це єдине звернення до Bambu при LAN-друку.
- `start_local_print_with_record` — той самий FTPS плюс запис у хмару (`/user/project`,
  `/my/task`). Slicenv його не викликає.

### Хмара (`bambu_network_start_print` → `run_cloud_print_job`)

- `block_cloud = 1` → одразу `-1` «cloud print blocked by config».
- `cloud_print = lan_only` → відмова.
- Потрібен вхід (`obn.auth.json`).
- Конвеєр: `POST /user/project` → presigned S3 (основний `.3mf`, `config_filename`) →
  `PATCH` → `POST /my/task`. На `/my/task` хмара відповідає 403, якщо
  `X-BBL-Client-Name` ≠ `BambuStudio` (тому `client_name = BambuStudio`). Саму команду на
  принтер надсилає хмара, непідписаною.
- Принтер відповідає `err_code 84033543`. Плагін ловить цю відмову у звіті хмарного MQTT
  (`rescue_cloud_project_file`) і перепублікує той самий `project_file` уже підписаним. Тому
  хмарний MQTT мусить бути з’єднаний: `connect_server` з `block_cloud = 0`.

### Поля `PrintParams`, які читає OBN

- Завжди: `dev_id`, `filename`, `plate_index`, `project_name`, `task_name`, `ams_mapping`,
  `ams_mapping2`, `task_use_ams`, `task_bed_type`, калібрування й таймлапс
  (`task_*`, `auto_*`, `extruder_cali_manual_mode`).
- LAN: `dev_ip`, `password`, `use_ssl_for_ftp`, `ftp_folder`, `try_emmc_print`.
- Хмара: `config_filename`, `nozzle_mapping`, `nozzles_info`, `ams_mapping_info`,
  `origin_*`, `stl_design_id`.
- Серія 02.08.04: `svc_context`, `slicer_uid`, `queue_plate_id`.

Шим заповнює їх у `fill` (`shim.cpp`): `connection_type` = `lan` / `cloud`,
`print_type` = `from_normal`, SSL увімкнено.

### Ключі `obn.conf`

Формат: `ключ = значення`. Рядок, що починається з `#`, — коментар. Останнє входження ключа
виграє. Slicenv пише свій `obn.conf` у `~/.local/share/slicenv/cloud/obn/` (тека 0700, файл
0600) і перезаписує лише свої ключі:

| Ключ | Значення | Чому |
|---|---|---|
| `block_cloud` | `1` (типово) / `0` (перемикач «Друк і стан через хмару», свіжий вхід у Bambu Cloud або «Увімкнути хмару» в рядку хмари) | `1` — лише LAN і REST акаунта, як у робочому конфігу дизайнера; `0` — хмарний MQTT і хмарний друк. Перевірка збереженої сесії `block_cloud` не змінює |
| `client_name` | `BambuStudio` | інакше 403 на `/my/task` |
| `cloud_print` | `cloud_only` | типове; `lan_only` заблокував би `start_print` |
| `log_stderr`, `log_to_file`, `bambusource_log_stderr`, `bambusource_log_to_file` | `0`; `log_file` порожній, `bambusource_log_file = off` | за README OBN логи містять токени, коди доступу й серійники без маскування |

**Логи — до `set_config_dir`.** OBN читає налаштування логів не з теки конфігурації, а з
`log_dir`, який отримує `bambu_network_create_agent` (`abi_lifecycle.cpp`:
`config::load_or_create(log_dir)` → `log::apply_config`, одноразово). Без `obn.conf` там плагін
створює шаблон із `log_stderr = 1` і пише в stderr застосунку e-mail, user id і серійники ще до
`set_config_dir`. Тому `Client::load` перед `create_agent` кладе в приватний `log_dir` копію
нашого `obn.conf` із примусово вимкненими логами (`obn::seed_log_dir`), а на старті процесу
задаються `OBN_LOG_STDERR=0`, `OBN_LOG_TO_FILE=0`, порожній `OBN_LOG_FILE` і
`OBN_BAMBUSOURCE_LOG_*` (`obn::quiet_process_env`; змінні мають пріоритет над файлом).
Обмеження: рядок `[obn] Loaded Open Bamboo Networking plugin, … commit …, ABI …` друкує
конструктор бібліотеки під час `dlopen`, і жоден конфіг його не вимикає. Ідентифікаторів у
ньому немає; глушити заради нього stderr усього GUI-процесу не будемо.

Решту ключів (наприклад, `slicer_key_pem`, якщо користувач принесе власні ключі) Slicenv не
чіпає. Плагін сам дописує в теку `obn.env` (реєстр IP ↔ серійник для `libBambuSource`) і
`obn.state.json`.

### Релізи

- У `persano/open-bamboo-networking` GitHub-релізів **немає**: теги v1.0.0–v2.1.0 успадковані від
  ClusterM, Actions-артефакти тимчасові й потребують входу.
- Релізи для користувачів виходять в окремому репозиторії
  **`persano/open-bambu-networking-plugin`**. Це колеса маркетплейсу OrcaSlicer.
  - Останній реліз — `v0.2.48` від 2026-11-08.
  - Linux x86_64: `open_bambu_networking_linux_x86_64.whl`, 4 454 732 байт, SHA-256
    `c4c1c111…5d04` (`digest` в API GitHub).
  - Є також `_linux_arm64.whl`, macOS, Windows і `open_bambu_networking_plugin.zip`.
- Колесо — це zip:
  - `open_bambu_networking/bin/linux_x64/libbambu_networking.so` — серія 02.08.04, звітує
    `02.08.04.99`;
  - `libbambu_networking_{02.03.00,02.08.01,02.09.01}.so`, `libBambuSource.so`,
    `liblive555.so` (заглушка 15 КБ);
  - Python-інсталятор `open_bambu_networking.py` і `RECORD` з SHA-256 кожного файлу.
- Orca розпаковує колесо в
  `orca_plugins/_subscribed/<автор>/<плагін>/__whl_extracted__/open_bambu_networking/…`, а
  `.install_state.json` поруч містить `installed_version`. Python-інсталятор копіює бібліотеки
  в `plugins/` (02.08.04 — і як `libbambu_networking_02.08.04.so`, і як
  `libbambu_networking.so`; закриті оригінали — у `plugins/backup` і `*.bak`) та виставляє в
  `obn.conf` значення `block_cloud = 0` і `client_name = BambuStudio`.
- Збірки апстріму ClusterM (`obn-linux-x64.tar.gz`, `lib/v<серія>/libbambu_networking.so`) без
  ключів. Без Developer Mode вони не друкують.

## Що робить Slicenv

| Крок | Як |
|---|---|
| Знайти встановлений | `obn::find_installed`, лише читання. Місця пошуку: `~/.var/app/com.orcaslicer.OrcaSlicer/config/OrcaSlicer/plugins`, `~/.config/OrcaSlicer/plugins`, Bambu Studio (і Flatpak), колеса маркетплейсу `orca_plugins/_subscribed/*/*`. Береться `libbambu_networking_02.08.04.so`, інакше `libbambu_networking.so`. Лише звичайні файли (симлінки відкидаються), ELF, OBN за рядками. Збірка persano / ClusterM визначається за `rescue_cloud_project_file` / `appcert fetch`. Версія пакета — з `.install_state.json` (той самий SHA у `plugins/` теж її отримує) |
| Скопіювати | `obn::install_found`: копії в `~/.local/share/slicenv/open-plugin/<джерело>-<версія>/` через `.staging-*` і `rename`, плюс `manifest.json` із SHA-256 файлів. Перевірка рядка `02.08.04.99` — шим уміє лише цю серію |
| Завантажити з GitHub | `obn::fetch_release`: `api.github.com/repos/persano/open-bambu-networking-plugin/releases/latest`, без пре-релізів. Посилання приймається лише з префіксом `https://github.com/persano/open-bambu-networking-plugin/releases/download/`. Без `digest` реліз відхиляється. Далі `install_release`: розмір і SHA-256 як в API. Потім `install_wheel`: лише бібліотеки з `bin/linux_x64/`, кожна звіряється з `RECORD`. Python-код колеса не виконується |
| Запам’ятати | `~/.local/share/slicenv/cloud/open-plugin.json`: джерело, версія, збірка, тека, перемикач хмари, редакція попередження (`WARNING_REVISION`). Зміна тексту попередження вимагає згоди знову |
| Вимкнути | `obn::disable`: вибір, копії й тека конфігурації (сесія з токеном відкритим текстом) видаляються. Чужі теки не чіпаються |
| Вхід | Той самий потік, що й для закритого плагіна (браузер → квиток → `get_my_token` → `change_user`). Коли OBN вибрано, `Options::for_login` / `for_session` ведуть у його теку, і сесія лягає в `obn.auth.json`. Сесії закритого плагіна для OBN непридатні, тож `find_sessions` показує лише сесію OBN |
| Стан і хмара | `block_cloud = 1` — робочий потік хмари не підключається (фраза «Відкритий плагін: хмару вимкнено, друк — по локальній мережі»), стан іде по LAN (`crates/lan`). `block_cloud = 0` — звичайний потік хмари з плагіном OBN |
| Друк по LAN | Збережений LAN-принтер + вибраний OBN → дочірній процес `slicenv --obn-local-print`. Завдання (з кодом доступу) передається через stdin, не argv. Stdout/stderr плагіна йдуть у `/dev/null`, звіти — окремим дескриптором. Далі `Client::start_local_print` → `bambu_network_start_local_print`. Аварія плагіна не зачіпає застосунок; агент плагіна в кожному процесі свій |
| Друк через хмару | Лише з `block_cloud = 0`: кнопка «Через хмару» → `Client::start_print`. Без з’єднання з хмарою — зрозуміла помилка замість «cloud server is not connected» |
| Діалог друку | Коли OBN вибрано, Bambu Connect / Bambu Studio не пропонуються, а діалог -26 не відкривається. Без OBN на Linux додано кнопку «Відкритий плагін…» |

## Що перевірено

Без мережі (`unshare -rn`), з тимчасовими HOME / XDG_*, 2026-11-10:
- `open_plugin_probe <копія libbambu_networking_02.08.04.so дизайнера>`:
  - `get_version 02.08.04.99`, покоління V0208;
  - `dlopen` + `create_agent` + `set_config_dir` + `init_log` — 1 мс;
  - `is_user_login false`.
- `open_plugin_probe --find "$HOME"` — повний шлях Slicenv:
  - знайдено дві копії у Flatpak Orca (`plugins/` і колесо маркетплейсу 0.2.48), обидві persano;
  - `install_found` у тимчасовий XDG — 29 мс;
  - `plugin_state` → OpenBamboo;
  - `Client::load` з нашим `obn.conf` — 3 мс, `start`, `destroy_agent`;
  - після запуску `obn.conf` має `block_cloud = 1`, `client_name = BambuStudio`, логи вимкнено;
  - плагін дописав лише `obn.env`;
  - mtime чужого файлу не змінився.
- Колесо `v0.2.48` з GitHub завантажено в `/tmp`. SHA-256 збігся з `digest` в API і з колесом
  у Flatpak Orca дизайнера. Бібліотеки в колесі побайтово ті самі, що в його `plugins/`.
- Тести (`cargo test`):
  - `obn::tests` — розбір і запис `obn.conf`, пошук на фейковому дереві тек (Flatpak,
    маркетплейс, закритий плагін, ClusterM, симлінк), установка копії й колеса, перевірка
    `RECORD`, розбір релізу;
  - `abi0208` — `start_local_print` через справжній шим із фейковим плагіном: макет
    `PrintParams` за значенням, поля LAN, коди помилок без секретів;
  - `print_paths` — шляхи діалогу з OBN.
- `slicenv --obn-local-print` на копії плагіна дизайнера: мережу відрізано, IP з TEST-NET
  (192.0.2.1), серійник і код вигадані. Повний шлях: stdin → `obn::run_local_print` → шим
  → `bambu_network_start_local_print`. Звіт: `stage 1 0`, `stage 7 -4020`, `code -4020`
  (FTPS не вдався, бо мережі немає), вихід 3 за 11 мс. Застосунок показав би «Не вдалося
  вивантажити файл на принтер (FTPS) (-4020)».

**Лише за кодом, наживо не перевірено:**
- вхід через OBN;
- список принтерів з OBN;
- отримання ключа слайсера;
- `app_cert_install` на A1;
- справжній друк по LAN і через хмару;
- хмарний статус із `block_cloud = 0`.

Ніяких звернень до Bambu, MQTT, FTP чи команд принтеру не було.

## Відомі обмеження й ризики

- **Ризики для користувача** (текст попередження в UI):
  - сторонній код із секретом, витягнутим із програми Bambu;
  - плагін представляється як Bambu Studio;
  - можливе порушення ToS Bambu, блокування доступу чи акаунта;
  - Bambu може відкликати секрет, і друк зламається в усіх одночасно;
  - сертифікат ставиться на принтер.
- **З’єднань MQTT два.** Під час LAN-друку OBN відкриває власне MQTT-з’єднання з принтером,
  поки LAN-робітник Slicenv тримає своє. Принтери серій A/P мають мало слотів. Якщо друк
  падає на «MQTT», варто перевірити на A1.
- **У дизайнера не знайдено ключів.** У теці Flatpak Orca дизайнера немає `slicer_*.pem`,
  хоча друк 2026-11-09 пройшов з `block_cloud = 1`. Отже, або A1 (01.04.60.02) по LAN не
  вимагав підпису, або ключі не збереглися. Чек-лист нижче це з’ясовує.
- **Лише серія 02.08.04.** 02.09.01 шим ще не знає.
- **Лише Linux x86_64** (`obn::SUPPORTED`). Колеса для aarch64, Windows і macOS є, але не
  підключені.

## Чек-лист для дизайнера (A1, справжній друк)

1. Відкрити Slicenv. У Налаштуваннях → «Плагіни й хмара» → «Відкритий плагін (Open Bamboo
   Networking)» натиснути «Налаштувати…».
   - Має бути: попередження і дві копії з «OrcaSlicer (Flatpak) · 0.2.48 — збірка persano».
2. Поставити галочку «Я розумію ризики…», потім «Використати» на першій копії.
   - Має бути: тост «Відкритий плагін підключено» і рядок «Підключено: OrcaSlicer (Flatpak),
     версія 0.2.48». Перемикач хмари вимкнено.
   - У `~/.local/share/slicenv/cloud/obn/obn.conf` — `block_cloud = 1`.
3. На сторінці «Пристрій» має бути фраза «Відкритий плагін: хмару вимкнено, друк — по
   локальній мережі». A1 має бути доданий як LAN-принтер (IP + код доступу). Якщо ні — додати.
4. Нарізати невеликий тест, натиснути «Друкувати».
   - Має бути: головна кнопка «Надіслати по локальній мережі», підказка «…з відкритим
     плагіном: він підписує команду друку…». Згадки Bambu Connect / Bambu Studio немає.
5. «Надіслати по локальній мережі».
   - Має бути: смужка завантаження, тост «Завдання «…» надіслано на принтер», A1 починає друк
     з Developer Mode OFF.
   - Якщо на екрані принтера `MQTT Command verification failed 84033543`, ключ не
     отримано: перевірити, чи з’явились `slicer_*.pem` у `~/.local/share/slicenv/cloud/obn/`.
   - Якщо `84033545`, сертифікат на принтер не встановився.
6. (За бажання, хмара.) У модалці ввімкнути «Друк і стан через хмару».
   - Має бути `block_cloud = 0`.
   - Майстер → «З’єднання» → «Bambu Cloud» → увійти через браузер; сесія має з’явитися в
     `~/.local/share/slicenv/cloud/obn/obn.auth.json`.
   - На «Пристрої» — принтери акаунта зі статусом.
   - У діалозі друку поруч є «Через хмару». Друк має стартувати (модель вивантажується в
     хмару Bambu; на принтер іде підписаний `project_file` після відмови 84033543).
7. Відкат: «Вимкнути відкритий плагін» прибирає все з тек Slicenv. Flatpak Orca лишається
   як був.

## Як відтворити

```sh
cargo build -p cloud --example open_plugin_probe
T=$(mktemp -d); HOME=$T XDG_DATA_HOME=$T/d XDG_CONFIG_HOME=$T/c XDG_CACHE_HOME=$T/k TMPDIR=$T \
  unshare -rn target/debug/examples/open_plugin_probe --find /home/<користувач>
```
