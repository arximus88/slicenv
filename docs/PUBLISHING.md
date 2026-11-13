# Публікація Slicenv: Flathub і AUR

Покроковий чек-лист для людини. Агенти нічого з цього не роблять: ні тегів, ні релізів, ні
push в AUR, ні PR у Flathub. Що зібрано й перевірено локально — [packaging/README.md](../packaging/README.md).

Сталі дані:

| Що | Значення |
|---|---|
| App id | `app.borys.slicenv` (домен `borys.app` → перші два компоненти навпаки) |
| Репозиторій | `https://github.com/arximus88/slicenv` |
| Ліцензія | AGPL-3.0-or-later |
| Пакети AUR | `slicenv-bin` (готовий tar.gz релізу), `slicenv-git` (збірка з git) |
| Маніфест Flathub | `packaging/flatpak/app.borys.slicenv.yml` + `cargo-sources.json` (генерується) |
| metainfo | `crates/app/assets/app/app.borys.slicenv.metainfo.xml` |
| Скриншоти | `packaging/screenshots/*.png`, у metainfo — за URL `raw.githubusercontent.com/…/v<версія>/…` |

Назва перевірена 2026-11-09: `slicenv`, `slicenv-bin`, `slicenv-git` в AUR — 0 результатів;
Flathub — 0; crates.io, PyPI, npm, Snap Store — вільно; на GitHub публічних репозиторіїв з
такою назвою немає; зареєстрованих торговельних марок «Slicenv» пошук не знайшов (USPTO TESS
вручну не перевіряли — це варто зробити, якщо назва важлива комерційно).

## 1. Зробити репозиторій публічним

1. Ще раз переглянути історію на секрети: `git log -p | rg -i 'token|password|access.?code|serial'`
   (агенти не комітять секретів, але історію до 2cf5435 варто проглянути). Якщо щось
   знайдено — переписати історію **до** відкриття (після — вважати секрет скомпрометованим).
2. GitHub → Settings → General → Danger Zone → Change visibility → Public.
3. Увімкнути Issues (на них веде `<url type="bugtracker">` у metainfo).
4. Перевірити, що відкриваються:
   `https://github.com/arximus88/slicenv`,
   `https://raw.githubusercontent.com/arximus88/slicenv/main/packaging/screenshots/01-home.png`.

## 2. Перший реліз v0.1.0

1. На `main`: версія в `Cargo.toml` (`[workspace.package] version = "0.1.0"`) і
   `<release version="0.1.0" date="…">` у metainfo — дата має збігатися з днем тегу
   (виправити дату, якщо реліз пізніше 2026-11-09). Після правок — `tools/package.sh srcinfo`.
2. Тег (підписаний, якщо є GPG-ключ):
   ```sh
   git tag -s v0.1.0 -m 'Slicenv 0.1.0'     # або -a без підпису
   git push origin v0.1.0
   ```
3. Push тегу запускає `.github/workflows/package.yml`: job `linux` (ubuntu-22.04, glibc 2.35)
   — tar.gz, AppImage, .deb, .rpm з `.sha256`; `aur` — пакет `slicenv-bin`; `flatpak` —
   збірка за Flathub-маніфестом і flatpak-builder-lint. Дочекатися зеленого запуску.
4. Завантажити artifacts (`slicenv-linux-x86_64`) і створити реліз вручну (workflow
   навмисно без `contents: write`):
   ```sh
   gh run download <run-id> -n slicenv-linux-x86_64 -D /tmp/rel
   gh release create v0.1.0 --title 'Slicenv 0.1.0' --notes-file docs/releases/v0.1.0.md /tmp/rel/*
   ```
   Нотатки — англійською (розділи як у `docs/releases/v0.1.4.md`); той самий файл лишається в
   `docs/releases/`.
   Ім’я архіву має бути саме `slicenv-0.1.0-x86_64-linux.tar.gz` — на нього посилається
   `slicenv-bin`.
5. Перевірити: `curl -sIL https://github.com/arximus88/slicenv/releases/download/v0.1.0/slicenv-0.1.0-x86_64-linux.tar.gz`
   → 200; скриншоти за тегом:
   `https://raw.githubusercontent.com/arximus88/slicenv/v0.1.0/packaging/screenshots/01-home.png` → 200;
   `appstreamcli validate --pedantic crates/app/assets/app/app.borys.slicenv.metainfo.xml`
   (уже **з** мережею — перевіряє URL).

## 3. Верифікація домену на Flathub (`.well-known`)

Робиться **після** того, як PR прийнято і застосунок з’явився на Flathub (токен видає
Flathub Developer Portal, раніше його немає).

1. Увійти на https://flathub.org через GitHub (акаунт, з якого подано PR).
2. Developer Portal → `app.borys.slicenv` → **Verification** → «Verify with website».
   Flathub покаже токен — рядок на кшталт `8a1b…` (унікальний для застосунку).
3. Розмістити файл (лише HTTPS, без редиректу на інший домен, `Content-Type: text/plain`):
   ```
   https://borys.app/.well-known/org.flathub.VerifiedApps.txt
   ```
   Вміст — по одному токену на рядок; рядки з `#` — коментарі. Для одного застосунку:
   ```
   # app.borys.slicenv
   <токен з Developer Portal>
   ```
   Для Cloudflare Pages: файл `public/.well-known/org.flathub.VerifiedApps.txt` (або `static/…`
   залежно від генератора), переконатися, що `.well-known` не відфільтровано.
   Перевірка: `curl -s https://borys.app/.well-known/org.flathub.VerifiedApps.txt`.
   Зараз (2026-11-09) там 404 — це очікувано.
4. Натиснути «Continue» у порталі. Файл не видаляти: Flathub перевіряє його й надалі;
   нові застосунки під `app.borys.*` — новий рядок у тому самому файлі.

## 4. PR у flathub/flathub

> **Правила Flathub про ШІ** (розділ «Generative AI policy» вимог, прочитано 2026-11-12):
> ШІ-агенти не можуть відкривати чи автоматизувати PR подання, писати його коміти, опис,
> відповіді рецензентам; **маніфест Flathub не може містити згенерованого чи написаного з
> допомогою ШІ вмісту** (розкриття цього не виправляє); подавач мусить розкрити, які частини
> застосунку й пакування згенеровано ШІ і в якому обсязі, а рецензенти можуть відхилити
> подання без розгляду. Повторні порушення — довічна заборона.
> Наслідок для Slicenv: `packaging/flatpak/app.borys.slicenv.yml` писали агенти, тож у поданні
> його використати не можна — маніфест і текст PR людина пише власноруч і чесно вказує, що
> код застосунку значною мірою написаний з ШІ. Шанс на прийняття низький. Без цих обмежень:
> власний репозиторій Flatpak або `.flatpak`-бандл у релізах GitHub.

Перед PR — локально (на машині з Flatpak):
```sh
tools/flatpak-cargo-sources.sh                    # cargo-sources.json з Cargo.lock тегу
git rev-parse 'v0.1.0^{commit}'                   # коміт тегу
flatpak run --command=flatpak-builder-lint org.flatpak.Builder manifest packaging/flatpak/app.borys.slicenv.yml
```

1. Fork https://github.com/flathub/flathub **з зняттям** галочки «Copy the master branch only».
2. ```sh
   git clone --branch=new-pr git@github.com:<you>/flathub.git && cd flathub
   git checkout -b app.borys.slicenv
   ```
3. Покласти в корінь:
   - `app.borys.slicenv.yml` — копія `packaging/flatpak/app.borys.slicenv.yml`, де в джерелі
     slicenv `COMMIT_OF_v0.1.0` замінено справжнім комітом тегу;
   - `cargo-sources.json` — щойно згенерований;
   - більше нічого (metainfo, іконка, desktop — у самому upstream-репозиторії).
4. `git add . && git commit -m 'Add app.borys.slicenv' && git push -u origin app.borys.slicenv`
5. PR: base **`new-pr`** (не `master`!), заголовок `Add app.borys.slicenv`. В описі:
   - зв’язок із доменом: `borys.app` належить автору;
   - **дозволи** (обґрунтування — коментарі у маніфесті):
     - `--share=network` — LAN-принтери Bambu Lab (MQTT/FTPS/камера, SSDP), завантаження
       рушіїв з GitHub, Bambu Cloud;
     - `--device=dri` — Vulkan (wgpu);
     - `--filesystem=xdg-documents`, `xdg-download` — бібліотека моделей на головній
       сканує теки з моделями; окремі файли — через портал;
     - `--filesystem=xdg-config/BambuStudio:ro`, `xdg-config/OrcaSlicer:ro` — лише читання
       пресетів і входу в Bambu Cloud зі встановлених на хості слайсерів;
   - **завантаження коду під час роботи** — сказати прямо: рушій нарізки (OrcaSlicer / Bambu
     Studio, AGPL-3.0, офіційні релізи з GitHub) завантажується майстром за дією
     користувача й розпаковується в `~/.var/app/app.borys.slicenv/data`; необов’язковий
     закритий плагін Bambu Cloud — лише за явною згодою. Без них застосунок відкривається,
     але не ріже. Рецензенти можуть попросити це прибрати — тоді варіанти: рушій як
     extension, або чесна примітка в описі;
   - FFmpeg 7 у маніфесті — лише бібліотеки без кодеків, бо рушії мають DT_NEEDED на
     `libavcodec.so.61`, а GNOME 51 постачає FFmpeg 8;
   - Slicenv не пов’язаний з Bambu Lab (у metainfo це вже сказано).
6. Чекати рев’ю (волонтери, дні–тижні). Відповідати на коментарі в тому самому PR,
   `master` не підливати. Пробна збірка — коментар `bot, build`.
   Типові зауваження: дозволи (особливо файлові), runtime не останній (на момент подання
   має бути найновіший GNOME — зараз 51), скриншоти недоступні за URL, «завантаження
   бінарників». Linter-помилки — див. `packaging/README.md`, розділ «Flatpak».
7. Після злиття: запрошення в репозиторій `flathub/app.borys.slicenv` — увімкнути 2FA на
   GitHub і прийняти **протягом тижня**. Далі всі оновлення — PR у цей репозиторій.
8. Верифікація — крок 3.

## 5. AUR

Один раз: акаунт на https://aur.archlinux.org з публічним SSH-ключем у профілі. Якщо
ключ, доданий у профіль, — типовий для ssh, окремого ключа й запису в `~/.ssh/config` не треба. Перевірка доступу: `ssh aur@aur.archlinux.org list-repos`.
Пакет створюється першим push у ще не наявний репозиторій.

Push в AUR публічний і незворотний (видалити пакет можна лише запитом до Package
Maintainers) — лише після явного «так» людини, якій показано diff PKGBUILD і вивід namcap.

`slicenv-bin` (після кроку 2, бо потрібен архів релізу). У релізі немає окремих
`*.sha256` — сума береться з `SHA256SUMS`:
```sh
v=0.1.1; rel=https://github.com/arximus88/slicenv/releases/download/v$v
sum=$(curl -sL $rel/SHA256SUMS | awk -v f=slicenv-$v-x86_64-linux.tar.gz '$2==f{print $1}')
curl -sL $rel/slicenv-$v-x86_64-linux.tar.gz | sha256sum        # має збігтися з $sum
sed -i "s/^sha256sums_x86_64=.*/sha256sums_x86_64=('$sum')/" packaging/aur/slicenv-bin/PKGBUILD
tools/package.sh srcinfo
```
Перевірка — у контейнері з **публічного URL** (`tools/package.sh aur-bin` підставляє
локальний tarball і реліз не перевіряє; `makepkg -si` на хості поставив би пакет у систему):
```sh
podman run --rm -v $PWD/packaging/aur/slicenv-bin:/w:ro docker.io/library/archlinux:base-devel bash -c '
  pacman -Syu --noconfirm --needed namcap
  useradd -m b; echo "b ALL=(ALL) NOPASSWD: ALL" >/etc/sudoers.d/b; cp -r /w /home/b/w; chown -R b: /home/b/w
  su b -c "cd /home/b/w && makepkg -sf --noconfirm && makepkg --printsrcinfo | diff - .SRCINFO"
  cd /home/b/w && pacman -U --noconfirm *.pkg.tar.zst && slicenv --version && slicenv-mcp --version
  namcap PKGBUILD *.pkg.tar.zst'
```
Публікація:
```sh
git clone ssh://aur@aur.archlinux.org/slicenv-bin.git /tmp/aur-slicenv-bin   # порожній — це нормально
cp packaging/aur/slicenv-bin/{PKGBUILD,.SRCINFO} /tmp/aur-slicenv-bin/
cd /tmp/aur-slicenv-bin && git branch -M master
git add PKGBUILD .SRCINFO && git commit -m "slicenv-bin $v-1" && git push origin master
```
Закомітити оновлені PKGBUILD і `.SRCINFO` і в репозиторій slicenv.

`slicenv-git` (після кроку 1 — потрібен публічний git): та сама перевірка в контейнері з
`packaging/aur/slicenv-git` (збірка з `git+https://github.com/…`, не `git+file://`; повна
збірка cargo), потім:
```sh
git clone ssh://aur@aur.archlinux.org/slicenv-git.git /tmp/aur-slicenv-git
cp packaging/aur/slicenv-git/{PKGBUILD,.SRCINFO} /tmp/aur-slicenv-git/
cd /tmp/aur-slicenv-git && git branch -M master
git add PKGBUILD .SRCINFO && git commit -m 'Initial import' && git push origin master
```
У `.SRCINFO` для `-git` — `pkgver` на момент генерації; AUR його не перераховує, це нормально.

Після push: сторінки `https://aur.archlinux.org/packages/slicenv-bin` і `/slicenv-git`
(версія, ліцензія, залежності) і
`curl -s 'https://aur.archlinux.org/rpc/v5/info?arg[]=slicenv-bin'` — Maintainer `arximus88`.

AUR вимагає, щоб у репозиторії пакета були лише `PKGBUILD`, `.SRCINFO` і потрібні
локальні файли; гілка — `master`.

## 6. Наступні версії

1. `Cargo.toml` → нова версія; у metainfo новий `<release version="X.Y.Z" date="…">`
   **першим** у `<releases>` з коротким описом змін (англійською й українською).
2. Нові скриншоти, якщо інтерфейс помітно змінився: `tools/screenshots/capture.py`
   (див. `packaging/screenshots/README.md`); URL у metainfo — на новий тег.
3. Тег `vX.Y.Z` → CI → реліз (крок 2).
4. AUR `slicenv-bin`: `pkgver=X.Y.Z`, `pkgrel=1`, новий `sha256sums_x86_64`,
   `tools/package.sh srcinfo`, push. Зміна лише PKGBUILD без нової версії — `pkgrel+1`.
   `slicenv-git` оновлювати лише при зміні залежностей чи кроків збірки.
5. Flathub: PR у `flathub/app.borys.slicenv` — новий `tag`/`commit` і перегенерований
   `cargo-sources.json` (`tools/flatpak-cargo-sources.sh`). `x-checker-data` у маніфесті
   стежить за FFmpeg 7.1.x; бот Flathub відкриє PR сам, коли вийде патч-реліз.
   Перед PR — runtime: якщо вийшов новий GNOME, підняти `runtime-version`.
6. Лишається ручним: macOS/Windows (`package-cross.yml`), winget, scoop —
   [packaging/README.md](../packaging/README.md).
