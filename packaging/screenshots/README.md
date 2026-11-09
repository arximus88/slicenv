# Скриншоти для Flathub / AppStream

PNG 1600×900 (16:9), світла тема, англійський інтерфейс. На них посилається
`<screenshots>` у `crates/app/assets/app/app.borys.slicenv.metainfo.xml` за URL
`https://raw.githubusercontent.com/arximus88/slicenv/v<версія>/packaging/screenshots/<файл>` —
тож після зміни файлів потрібен новий тег, а URL у metainfo — на нього.

| Файл | Що | Підпис у metainfo |
|---|---|---|
| `01-home.png` | Головна: бібліотека моделей з мініатюрами | Home: model library… |
| `02-prepare.png` | Підготовка: дві пластини, вибраний об’єкт, налаштування процесу | Prepare: objects on build plates… |
| `03-preview-layers.png` | Preview «Шари» після нарізки, повзунок на шарі 77 | Preview: sliced layers… |
| `04-preview-photo.png` | Preview «Фото» з матеріалом PLA | Photo view… |
| `05-profile-editor.png` | Редактор профілю філаменту (Advanced) | Profile editor… |
| `06-engines.png` | Меню «Рушій нарізки»: OrcaSlicer 2.5.0 і Bambu Studio 02.09.01.70 | Slicing engine menu… |
| `07-device.png` | Сторінка «Пристрій» з фіктивним принтером (`SLICENV_FAKE_PRINTER=1`) | Device: printer status… |

## Як знято (2026-11-09)

Справжній застосунок — **Flatpak-бандл** з `packaging/flatpak/app.borys.slicenv.yml`
(`tools/package.sh flatpak`), Radeon 780M (Vulkan) у headless mutter
(`tools/screenshots/headless.sh`), тимчасовий HOME. Рушії встановлено менеджером рушіїв
усередині пісочниці. Жодних особистих даних: моделі згенеровані, принтер фіктивний, хмара й
LAN вимкнені, хостові конфіги не змонтовані.

Моделі власні, процедурні (без сторонніх ліцензій):
```sh
python3 -I tools/screenshots/models.py /tmp/models          # STL: вузли, вази, шестерні, кашпо
python3 -I tools/screenshots/project.py /tmp/models /tmp/models/slicenv-demo.3mf \
  --profiles <каталог профілів рушія з BBL/>                 # 3MF з двома пластинами, пресети A1
```

Сесія й кроки:
```sh
S=/tmp/shots; tools/screenshots/headless.sh start $S
# preferences.json у $S/home/.var/app/app.borys.slicenv/config/slicenv/:
#   "theme": "light", "language": "en", "wizard_done": true, "library_roots": ["…/data/Models"]
tools/screenshots/headless.sh flatpak $S …/data/Models/slicenv-demo.3mf
tools/screenshots/headless.sh click $S 1390 79      # напр. відкрити меню рушія
tools/screenshots/headless.sh shot $S 06-engines.png
tools/screenshots/headless.sh stop $S
```

Оптимізація: `oxipng -o 4 --strip safe *.png` (якщо немає — `magick … -strip -define png:compression-level=9`).
Кожен файл ≤ 500 КБ.
