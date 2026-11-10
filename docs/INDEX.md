# Документація Slicenv

Почати варто з [README.md](../README.md).

## Розробка

| Документ | Про що |
|---|---|
| [TESTING.md](TESTING.md) | Тести, CI, бюджети продуктивності |
| [BENCHMARK.md](BENCHMARK.md) | Порівняння ресурсів із Bambu Studio й OrcaSlicer, як відтворити |
| [PORTING.md](PORTING.md) | macOS і Windows: каталоги, матриця можливостей, що перевірено |
| [UPSTREAM.md](UPSTREAM.md) | Версії Bambu Studio / Orca і оновлення |
| [UI-PERFORMANCE.md](UI-PERFORMANCE.md) | Профілювання інтерфейсу (`SLICENV_PROFILE=1`) |
| [PREVIEW_ORBIT.md](PREVIEW_ORBIT.md), [PREVIEW_STATS.md](PREVIEW_STATS.md) | Продуктивність Preview |
| [PHOTO-MODE.md](PHOTO-MODE.md) | Preview «Фото»: PBR-валики, матеріали філаменту, студійне світло, етапи 2–3 |
| [WINDOW-FRAME.md](WINDOW-FRAME.md) | Рамка вікна, клієнтські декорації |
| [IDEAS.md](IDEAS.md) | Ідеї на майбутнє (не реалізовано) |
| [../packaging/README.md](../packaging/README.md) | Пакування для Linux, Windows, macOS |
| [PUBLISHING.md](PUBLISHING.md) | Публікація у Flathub і AUR, перший реліз, оновлення версій |

## Крейти

| Документ | Про що |
|---|---|
| [crates/cloud/README.md](../crates/cloud/README.md) | Обгортка над закритим плагіном Bambu |
| [crates/geometry/README.md](../crates/geometry/README.md) | CSG через `manifold-csg` |
| [crates/project/README.md](../crates/project/README.md) | Редагування 3MF-проєктів |
| [crates/settings/README.md](../crates/settings/README.md) | Метадані опцій, сторінки налаштувань, локалізація |

## Рушії, профілі, нарізка

| Документ | Про що |
|---|---|
| [ENGINE-COMPATIBILITY.md](ENGINE-COMPATIBILITY.md) | Сумісність Bambu Studio і Orca: схеми, CLI |
| [ENGINE-PACKAGES.md](ENGINE-PACKAGES.md) | Менеджер рушіїв |
| [PROFILE-LIBRARY.md](PROFILE-LIBRARY.md) | Бібліотека профілів |
| [MIGRATION.md](MIGRATION.md) | Перенесення налаштувань між рушіями |
| [AUTO-ORIENT.md](AUTO-ORIENT.md) | Автоорієнтація (Q) |
| [support-fins.md](support-fins.md) | Зовнішній інструмент Support Fins |

## Принтер

| Документ | Про що |
|---|---|
| [LAN.md](LAN.md) | Bambu Lab у локальній мережі без плагіна: протоколи, TLS |
| [CLOUD.md](CLOUD.md) | Bambu Cloud: плагін за згодою, сесії, вхід через браузер |
| [LINUX-CLOUD-PRINT.md](LINUX-CLOUD-PRINT.md) | Друк через інтернет на Linux: Bambu Studio, відкритий плагін, LAN — заміри |
| [OPEN-PLUGIN.md](OPEN-PLUGIN.md) | Linux: відкритий плагін Open Bamboo Networking (persano) за вибором користувача — як друкує без Developer Mode, установка, `obn.conf`, чек-лист |
| [KLIPPER.md](KLIPPER.md) | Принтери Klipper через Moonraker: пошук mDNS, додавання, стан, камера MJPEG, керування (температури, Home, джог, вентилятор, консоль, аварійна зупинка), друк; перевірено лише проти емулятора |

## MCP

| Документ | Про що |
|---|---|
| [MCP.md](MCP.md) | Сервер `slicenv-mcp` (stdio) і HTTP-сервер у застосунку |

## Інтерфейс і переклади

| Документ | Про що |
|---|---|
| [LANGUAGES.md](LANGUAGES.md) | Мови інтерфейсу |
| [TRANSLATION-COVERAGE.md](TRANSLATION-COVERAGE.md) | Покриття англійського перекладу |
| [SETTING-HELP.md](SETTING-HELP.md) | Довідка параметрів |
| [ILLUSTRATED-HELP.md](ILLUSTRATED-HELP.md) | Ілюстровані підказки й іконки шаблонів |

## Походження ресурсів

[THIRD-PARTY-NOTICES.md](../THIRD-PARTY-NOTICES.md) і README у теках `crates/app/assets/*`.
