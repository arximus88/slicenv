---
name: add-ui-string
description: Додавання або зміна українського тексту в Slicenv. Запускай, коли пишеш чи змінюєш кириличний рядковий літерал у crates/app/src (UI, повідомлення, eprintln!/логи) або у файлах gcode/cloud/lan зі списку SOURCES у check_translations.py, а також коли check_translations.py впав.
---

# Український рядок → ключ у en.json

Каталог: `crates/app/locales/en.json` — точний український текст як ключ, англійський як значення. Вшивається в бінарник (`include_str!` у `crates/app/src/i18n.rs`). Огляд для людей — [docs/LANGUAGES.md](../../../docs/LANGUAGES.md).

## Що перевіряється
`python3 -I crates/app/tools/check_translations.py` шукає кириличні літерали в:
- `crates/app/src/**/*.rs`, крім `i18n.rs`;
- файлах зі списку `SOURCES` у скрипті (`gcode/src/model.rs`, `view.rs`, `cloud/src/{status,control,cover}.rs`, `lan/src/{error,tls}.rs`).

Коментарі й блоки `#[cfg(test)] mod … { }` маскуються. Не перевіряються літерали з префіксом `[cloud]`/`[lan]` і кілька винятків у скрипті. Отже кожен кириличний літерал у робочому коді цих файлів — і в `eprintln!`, `format!`, `anyhow!` теж — потребує ключа, навіть якщо не показується в UI.

## Як перекладається
| API | Коли |
|---|---|
| `i18n::tr("Текст")` | статичний текст; для `uk` повертає як є, інакше мовний пакет → en.json → сам текст |
| `crate::tr_format!("Шар {n} · {z:.2} мм")` | шаблон із підстановками; ключ — шаблон дослівно (`"Шар {n} · {z:.2} мм"`), значення має ті самі плейсхолдери |
| `ui::ghost_button`, `accent_button`, `danger_button`, `icon_button`, `danger_icon_button` (підказка), `panel_header`, `group_title`, `prop_row`, `tab`, `empty_state` | перекладають підпис самі (`ui::translated`); `tr()` не потрібен |

`format!("Пластина {}", n)` без `tr_format!` пройде перевірку (ключ є), але в англійському інтерфейсі лишиться українським.

## Кроки
1. Пишеш літерал — одразу додай пару в en.json.
2. Нові ключі — **у кінець файлу**. Наявний порядок не змінювати, не сортувати й не переформатовувати: інакше кожне злиття гілок дає конфлікт на весь файл.
3. Зберегти формат: 2 пробіли, UTF-8 без `\u`-екранування, `\n` наприкінці. Безпечна правка скриптом:
   ```bash
   python3 -I - <<'EOF'
   import json; p = "crates/app/locales/en.json"
   d = json.load(open(p, encoding="utf-8"))
   d.setdefault("Новий текст", "New text")
   open(p, "w", encoding="utf-8").write(json.dumps(d, ensure_ascii=False, indent=2) + "\n")
   EOF
   ```
4. Змінив український текст — старий ключ залиш, якщо він ще десь використовується (`rg -nF '"Старий текст"' crates`), інакше заміни на місці, не переносячи в кінець.
5. Плейсхолдери й модифікатори (`{n}`, `{:.2}`, `{err:#}`) в англійському значенні — ті самі. Дві підстановки поспіль без тексту між ними не перекладаються.
6. Не перекладати назви моделей, ID профілів, шляхи, G-code.
7. Перевірка: `python3 -I crates/app/tools/check_translations.py` → `All scanned Cyrillic literals are present`.

## UI-пастки поруч із текстом
- Англійський текст зазвичай довший, шрифти Windows ширші — лишай запас ширини.
- Довгий текст: контейнер з `max_w(…)` і `whitespace_normal()`.
- Трикрапка: `min_w_0()` + `truncate()` на дочірньому div, не на самому контейнері. `ghost_button` має `flex_none` і підпис не обріже — бери `ghost_button_bare` і додай `div().min_w_0().truncate().child(…)` (зразок — назва проєкту в `workspace.rs`).
