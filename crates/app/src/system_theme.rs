//! Системна тема: палітра Omarchy, xdg-desktop-portal (GNOME/KDE) і математика кольору.
//!
//! # Omarchy (перевірено за github.com/basecamp/omarchy, `master` e284b83, листопад 2026)
//!
//! * Поточна тема — **каталог** `~/.local/state/omarchy/current/theme`. `bin/omarchy-theme-set`
//!   збирає нову тему в `…/current/next-theme`, потім робить `rm -rf theme && mv next-theme theme`,
//!   тобто inode каталогу змінюється, а символьного посилання вже немає. Ім’я теми лежить у
//!   `…/current/theme.name` (`bin/omarchy-theme-current`).
//! * Старі встановлення (до міграції `migrations/1781043107.sh`) тримали посилання
//!   `~/.config/omarchy/current/theme -> themes/<name>`; його теж читаємо, розв’язуючи симлінк.
//! * Палітра — `colors.toml`: плоскі рядки `key = "#rrggbb"`. Канонічні ключі (`themes/*/colors.toml`):
//!   `mode` (`"dark"`/`"light"`), `accent`, `selection`, `muted`, `background`, `dark_background`,
//!   `darker_background`, `lighter_background`, `foreground`, `dark_foreground`, `light_foreground`,
//!   `bright_foreground`, `red`, `yellow`, `orange`, `green`, `cyan`, `blue`, `magenta`, `brown`,
//!   `bright_*`. Сумісність із `bin/omarchy-theme-color`: короткі `bg`/`fg`/`lighter_bg`…,
//!   ANSI `color0`…`color15` (0 — фон, 7 — текст, 8 — muted, 1–6 — червоний…бірюзовий),
//!   `selection_background`, `purple` як `magenta`.
//! * Режим (так само, як `resolve_theme_mode` в `omarchy-theme-color`): ключ `mode`, далі
//!   застарілий `theme_type`, далі файл-маркер `light.mode` поруч, далі яскравість фону
//!   (сума R+G+B > 382 — світла), інакше темна.
//! * Тема без `colors.toml` (старий формат) — `alacritty.toml`: `[colors.primary] background/foreground`,
//!   `[colors.normal]`/`[colors.bright]` `black…white`, `[colors.selection] background`
//!   (як `bin/omarchy-theme-colors-from-alacritty`).
//! * Шрифт: Omarchy задає лише моноширинний, через fontconfig (`bin/omarchy-font-set` пише
//!   `~/.config/fontconfig/fonts.conf`, `bin/omarchy-font-current` = `fc-match monospace`).
//! * Хук `~/.config/omarchy/hooks/theme-set` виконується після зміни теми, але він належить
//!   користувачеві; ми його не чіпаємо, а опитуємо каталог теми кожні 2 с.
//!
//! # Портал
//! `org.freedesktop.portal.Settings`, простір `org.freedesktop.appearance`: `color-scheme`
//! (0 — без переваги, 1 — темна, 2 — світла) і `accent-color` (три `f64` 0…1; поза межами — не задано).
//! Через `ashpd`: він уже в дереві залежностей gpui-component, тож нових крейтів немає.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};

/// Колір sRGB без альфи.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// `#rrggbb`, `rrggbb`, `0xrrggbb`, `#rrggbbaa` (альфа відкидається), `rgb(r,g,b)`.
    pub fn parse(value: &str) -> Option<Self> {
        let v = value.trim();
        if let Some(inner) = v
            .strip_prefix("rgb(")
            .or_else(|| v.strip_prefix("rgba("))
            .and_then(|s| s.strip_suffix(')'))
        {
            let mut parts = inner.split(',').map(|p| p.trim().parse::<u8>().ok());
            return Some(Self(parts.next()??, parts.next()??, parts.next()??));
        }
        let hex = v
            .strip_prefix('#')
            .or_else(|| v.strip_prefix("0x"))
            .or_else(|| v.strip_prefix("0X"))
            .unwrap_or(v);
        if !(hex.len() == 6 || hex.len() == 8) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(Self(byte(0)?, byte(2)?, byte(4)?))
    }

    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }

    /// Як `mix_color` в Omarchy: `self·(1−t) + other·t`, з округленням.
    pub fn mix(self, other: Self, t: f32) -> Self {
        let t = t.clamp(0., 1.);
        let m = |a: u8, b: u8| (a as f32 * (1. - t) + b as f32 * t + 0.5) as u8;
        Self(m(self.0, other.0), m(self.1, other.1), m(self.2, other.2))
    }

    /// Відносна яскравість WCAG 2.x.
    pub fn luminance(self) -> f32 {
        let lin = |c: u8| {
            let c = c as f32 / 255.;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(self.0) + 0.7152 * lin(self.1) + 0.0722 * lin(self.2)
    }

    pub fn linear(self) -> [f32; 3] {
        let lin = |c: u8| {
            let c = c as f32 / 255.;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        [lin(self.0), lin(self.1), lin(self.2)]
    }

    pub fn hsla(self, alpha: u8) -> gpui::Hsla {
        gpui::rgba(u32::from_be_bytes([self.0, self.1, self.2, alpha])).into()
    }
}

pub const WHITE: Rgb = Rgb(255, 255, 255);
pub const BLACK: Rgb = Rgb(0, 0, 0);
/// WCAG AA для звичайного тексту.
pub const AA_TEXT: f32 = 4.5;
/// WCAG AA для великого тексту й нетекстових елементів (1.4.11).
pub const AA_UI: f32 = 3.0;

/// Контраст WCAG 2.x, 1…21.
pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (a.luminance(), b.luminance());
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

pub fn worst_contrast(color: Rgb, surfaces: &[Rgb]) -> f32 {
    surfaces
        .iter()
        .map(|s| contrast(color, *s))
        .fold(f32::INFINITY, f32::min)
}

/// Найменше зрушення `color` до білого або чорного, за якого контраст із кожною поверхнею
/// не нижчий за `min`. Відтінок зберігається, доки це можливо; якщо не досягнуто — найкраще.
pub fn ensure_contrast(color: Rgb, surfaces: &[Rgb], min: f32) -> Rgb {
    if surfaces.is_empty() || worst_contrast(color, surfaces) >= min {
        return color;
    }
    let mean = surfaces.iter().map(|s| s.luminance()).sum::<f32>() / surfaces.len() as f32;
    let first = if color.luminance() >= mean {
        WHITE
    } else {
        BLACK
    };
    let second = if first == WHITE { BLACK } else { WHITE };
    let mut best = color;
    for target in [first, second] {
        for step in 1..=50 {
            let c = color.mix(target, step as f32 / 50.);
            if worst_contrast(c, surfaces) >= min {
                return c;
            }
            if worst_contrast(c, surfaces) > worst_contrast(best, surfaces) {
                best = c;
            }
        }
    }
    best
}

/// Палітра теми Omarchy після розв’язання псевдонімів.
#[derive(Clone, Debug, PartialEq)]
pub struct OmarchyTheme {
    pub name: String,
    pub light: bool,
    pub background: Rgb,
    pub foreground: Rgb,
    pub accent: Rgb,
    pub selection: Option<Rgb>,
    pub muted: Option<Rgb>,
    pub red: Rgb,
    pub green: Rgb,
    pub yellow: Rgb,
    pub blue: Rgb,
    pub magenta: Rgb,
    pub cyan: Rgb,
    /// Моноширинний шрифт системи (`fc-match monospace`), якщо вдалося визначити.
    pub mono_font: Option<String>,
}

/// Рядки `key = value` з `colors.toml` так само, як `parse_colors_file` в Omarchy.
pub fn parse_colors_toml(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key: String = key
            .chars()
            .filter(|c| !matches!(c, '"' | '\'' | ' ' | '\t'))
            .collect();
        let value = value.trim();
        let value = if let Some(start) = value.find(['"', '\'']) {
            let rest = &value[start + 1..];
            rest.find(['"', '\'']).map_or(rest, |end| &rest[..end])
        } else {
            value.split('#').next().unwrap_or("").trim()
        };
        if !key.is_empty() {
            out.insert(key, value.to_owned());
        }
    }
    out
}

/// `alacritty.toml` старих тем → ключі `colors.toml` (`background`, `foreground`, `color0…15`,
/// `selection`), як `omarchy-theme-colors-from-alacritty`.
pub fn parse_alacritty_toml(text: &str) -> HashMap<String, String> {
    const NAMES: [&str; 8] = [
        "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    ];
    let mut raw = HashMap::new();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(s) = line.strip_prefix('[').and_then(|s| s.split(']').next()) {
            section = s.trim().to_owned();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if section.is_empty() || key.trim().starts_with('#') {
            continue;
        }
        let value = value.trim().trim_start_matches(['"', '\'']);
        let value = value.split(['"', '\'', ' ', '\t']).next().unwrap_or("");
        if let Some(c) = Rgb::parse(value) {
            raw.entry(format!("{section}.{}", key.trim()))
                .or_insert(c.hex());
        }
    }
    let mut out = HashMap::new();
    let mut copy = |from: &str, to: &str| {
        if let Some(v) = raw.get(from) {
            out.insert(to.to_owned(), v.clone());
        }
    };
    copy("colors.primary.background", "background");
    copy("colors.primary.foreground", "foreground");
    copy("colors.selection.background", "selection");
    for (i, n) in NAMES.iter().enumerate() {
        copy(&format!("colors.normal.{n}"), &format!("color{i}"));
        copy(&format!("colors.bright.{n}"), &format!("color{}", i + 8));
    }
    out
}

/// Розв’язання псевдонімів і режиму за правилами `bin/omarchy-theme-color`.
pub fn resolve_palette(
    name: &str,
    keys: &HashMap<String, String>,
    light_marker: bool,
) -> Option<OmarchyTheme> {
    let get = |k: &str| keys.get(k).and_then(|v| Rgb::parse(v));
    let any = |ks: &[&str]| ks.iter().find_map(|k| get(k));
    let background = any(&["background", "bg", "color0"])?;
    let foreground = any(&["foreground", "fg", "color7"])?;
    let blue = any(&["blue", "color4"]).unwrap_or(foreground);
    let mode = keys
        .get("mode")
        .or_else(|| keys.get("theme_type"))
        .map(|m| m.trim().to_ascii_lowercase());
    let light = match mode.as_deref() {
        Some("light") => true,
        Some("dark") => false,
        _ if light_marker => true,
        _ => background.0 as u32 + background.1 as u32 + background.2 as u32 > 382,
    };
    Some(OmarchyTheme {
        name: name.to_owned(),
        light,
        background,
        foreground,
        accent: any(&["accent"]).unwrap_or(blue),
        selection: any(&["selection", "selection_background", "color8"]),
        muted: any(&["muted", "color8", "dark_foreground", "dark_fg"]),
        red: any(&["red", "color1"]).unwrap_or(Rgb(0xd0, 0x72, 0x77)),
        green: any(&["green", "color2"]).unwrap_or(Rgb(0xa1, 0xc1, 0x81)),
        yellow: any(&["yellow", "color3"]).unwrap_or(Rgb(0xde, 0xc1, 0x84)),
        blue,
        magenta: any(&["magenta", "purple", "color5"]).unwrap_or(Rgb(0xb4, 0x77, 0xcf)),
        cyan: any(&["cyan", "color6"]).unwrap_or(Rgb(0x6e, 0xb4, 0xbf)),
        mono_font: None,
    })
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// Каталог поточної теми: новий шлях у `~/.local/state`, потім старе посилання в `~/.config`.
pub fn omarchy_theme_dir() -> Option<PathBuf> {
    let home = home()?;
    [
        home.join(".local/state/omarchy/current/theme"),
        home.join(".config/omarchy/current/theme"),
    ]
    .into_iter()
    .find_map(|p| std::fs::canonicalize(&p).ok().filter(|p| p.is_dir()))
}

/// Читає тему з каталогу (`colors.toml`, інакше `alacritty.toml`). Шрифт не визначає.
pub fn read_omarchy_theme(dir: &Path) -> Option<OmarchyTheme> {
    let keys = match std::fs::read_to_string(dir.join("colors.toml")) {
        Ok(text) => parse_colors_toml(&text),
        Err(_) => parse_alacritty_toml(&std::fs::read_to_string(dir.join("alacritty.toml")).ok()?),
    };
    let name = dir
        .parent()
        .and_then(|p| std::fs::read_to_string(p.join("theme.name")).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "omarchy".into());
    resolve_palette(&name, &keys, dir.join("light.mode").exists())
}

/// Відбиток стану теми: шлях після розв’язання симлінка й вміст файлів, з яких беремо палітру.
fn omarchy_signature() -> Option<(PathBuf, Vec<u8>)> {
    let dir = omarchy_theme_dir()?;
    let mut bytes = Vec::new();
    for file in [
        "colors.toml",
        "alacritty.toml",
        "light.mode",
        "../theme.name",
    ] {
        if let Ok(b) = std::fs::read(dir.join(file)) {
            bytes.extend_from_slice(file.as_bytes());
            bytes.extend(b);
        }
    }
    Some((dir, bytes))
}

/// Перша сім’я `fc-match monospace`, як `omarchy-font-current`.
fn system_mono_font() -> Option<String> {
    let out = std::process::Command::new("fc-match")
        .args(["monospace", "-f", "%{family}\n"])
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let family = text.lines().next()?.split(',').next()?.trim().to_owned();
    (!family.is_empty()).then_some(family)
}

/// Поточна тема Omarchy з шрифтом. Блокуюче (читання файлів і `fc-match`).
pub fn load_omarchy() -> Option<OmarchyTheme> {
    let mut theme = read_omarchy_theme(&omarchy_theme_dir()?)?;
    theme.mono_font = system_mono_font();
    Some(theme)
}

/// Стан порталу `org.freedesktop.appearance`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Portal {
    /// 0 — без переваги, 1 — темна, 2 — світла.
    pub color_scheme: Option<u32>,
    pub accent: Option<Rgb>,
}

/// Налаштування заголовка вікна GNOME (`org.gnome.desktop.wm.preferences`), як їх віддає
/// портал: розкладка кнопок і дії подвійного, середнього та правого кліку. `None` — ключа немає.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TitlebarPrefs {
    pub button_layout: Option<String>,
    pub double_click: Option<String>,
    pub middle_click: Option<String>,
    pub right_click: Option<String>,
}

pub enum Event {
    Omarchy(Option<OmarchyTheme>),
    /// Змінилися налаштування заголовка вікна (GNOME Tweaks) — без перезапуску.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Titlebar(TitlebarPrefs),
    /// Лише Linux (xdg-desktop-portal); на macOS/Windows тему ОС дає gpui
    /// (`observe_window_appearance` у `preferences.rs`).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Portal(Portal),
}

/// Запускає фонові спостерігачі: опитування теми Omarchy кожні 2 с і підписку на портал.
/// Потоки завершуються, щойно приймач закрито.
pub fn spawn_watchers() -> async_channel::Receiver<Event> {
    let (tx, rx) = async_channel::unbounded();
    let omarchy = tx.clone();
    let poll = paths::env("SLICENV_THEME_POLL_MS")
        .and_then(|v| v.to_str()?.parse().ok())
        .map_or(Duration::from_secs(2), Duration::from_millis);
    std::thread::Builder::new()
        .name("omarchy-theme".into())
        .spawn(move || {
            let mut last = omarchy_signature();
            loop {
                std::thread::sleep(poll);
                let now = omarchy_signature();
                if now != last {
                    last = now;
                    if omarchy
                        .send_blocking(Event::Omarchy(load_omarchy()))
                        .is_err()
                    {
                        return;
                    }
                } else if omarchy.is_closed() {
                    return;
                }
            }
        })
        .ok();
    #[cfg(target_os = "linux")]
    std::thread::Builder::new()
        .name("appearance-portal".into())
        .spawn(move || futures::executor::block_on(portal_loop(tx)))
        .ok();
    rx
}

#[cfg(target_os = "linux")]
async fn read_portal(settings: &ashpd::desktop::settings::Settings) -> Portal {
    use ashpd::desktop::settings::ColorScheme;
    let color_scheme = settings.color_scheme().await.ok().map(|s| match s {
        ColorScheme::NoPreference => 0,
        ColorScheme::PreferDark => 1,
        ColorScheme::PreferLight => 2,
    });
    let accent = settings.accent_color().await.ok().and_then(|c| {
        let ch = |v: f64| (0.0..=1.0).contains(&v).then(|| (v * 255.).round() as u8);
        Some(Rgb(ch(c.red())?, ch(c.green())?, ch(c.blue())?))
    });
    Portal {
        color_scheme,
        accent,
    }
}

#[cfg(target_os = "linux")]
const WM_PREFERENCES: &str = "org.gnome.desktop.wm.preferences";

#[cfg(target_os = "linux")]
async fn read_titlebar(settings: &ashpd::desktop::settings::Settings) -> TitlebarPrefs {
    let read = async |key: &str| settings.read::<String>(WM_PREFERENCES, key).await.ok();
    TitlebarPrefs {
        button_layout: read("button-layout").await,
        double_click: read("action-double-click-titlebar").await,
        middle_click: read("action-middle-click-titlebar").await,
        right_click: read("action-right-click-titlebar").await,
    }
}

/// Одноразове читання порталу — для живої перевірки (`live_portal_read`).
#[cfg(all(test, target_os = "linux"))]
pub fn read_portal_blocking() -> anyhow::Result<Portal> {
    futures::executor::block_on(async {
        let settings = ashpd::desktop::settings::Settings::new().await?;
        Ok(read_portal(&settings).await)
    })
}

#[cfg(target_os = "linux")]
async fn portal_loop(tx: async_channel::Sender<Event>) {
    use futures::StreamExt as _;
    let settings = match ashpd::desktop::settings::Settings::new().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Slicenv: портал оформлення недоступний ({e}), тема без системного акценту");
            return;
        }
    };
    let mut last = read_portal(&settings).await;
    if tx.send(Event::Portal(last)).await.is_err() {
        return;
    }
    let mut titlebar = read_titlebar(&settings).await;
    if tx.send(Event::Titlebar(titlebar.clone())).await.is_err() {
        return;
    }
    let Ok(mut changes) = settings.receive_setting_changed().await else {
        return;
    };
    while let Some(setting) = changes.next().await {
        if setting.namespace() == WM_PREFERENCES {
            let now = read_titlebar(&settings).await;
            if now != titlebar {
                titlebar = now.clone();
                if tx.send(Event::Titlebar(now)).await.is_err() {
                    return;
                }
            }
            continue;
        }
        if setting.namespace() != "org.freedesktop.appearance" {
            continue;
        }
        let now = read_portal(&settings).await;
        if now != last {
            last = now;
            if tx.send(Event::Portal(now)).await.is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/omarchy");

    fn fixture(name: &str) -> OmarchyTheme {
        read_omarchy_theme(&Path::new(FIXTURES).join(name)).expect(name)
    }

    #[test]
    fn parses_colour_forms() {
        assert_eq!(Rgb::parse("#1a1b26"), Some(Rgb(0x1a, 0x1b, 0x26)));
        assert_eq!(Rgb::parse("0x1A1B26"), Some(Rgb(0x1a, 0x1b, 0x26)));
        assert_eq!(Rgb::parse("#1a1b26ff"), Some(Rgb(0x1a, 0x1b, 0x26)));
        assert_eq!(Rgb::parse("rgb(1, 2, 3)"), Some(Rgb(1, 2, 3)));
        assert_eq!(Rgb::parse("nope"), None);
        // Те саме округлення, що й awk у omarchy-theme-color.
        assert_eq!(
            Rgb(0x1a, 0x1b, 0x26).mix(BLACK, 0.25),
            Rgb(0x14, 0x14, 0x1d)
        );
    }

    #[test]
    fn contrast_matches_wcag_reference_values() {
        assert!((contrast(WHITE, BLACK) - 21.).abs() < 0.01);
        assert!((contrast(WHITE, WHITE) - 1.).abs() < 0.001);
        // #767676 на білому — класична межа AA (4.54).
        assert!((contrast(Rgb(0x76, 0x76, 0x76), WHITE) - 4.54).abs() < 0.01);
        let fixed = ensure_contrast(Rgb(0x99, 0x99, 0x99), &[WHITE], AA_TEXT);
        assert!(contrast(fixed, WHITE) >= AA_TEXT);
        // Колір, що вже проходить, не змінюється.
        assert_eq!(ensure_contrast(BLACK, &[WHITE], AA_TEXT), BLACK);
    }

    #[test]
    fn reads_canonical_colors_toml_fixtures() {
        let tokyo = fixture("tokyo-night");
        assert_eq!(tokyo.name, "tokyo-night");
        assert!(!tokyo.light);
        assert_eq!(tokyo.background, Rgb(0x1a, 0x1b, 0x26));
        assert_eq!(tokyo.foreground, Rgb(0xa9, 0xb1, 0xd6));
        assert_eq!(tokyo.accent, Rgb(0x7a, 0xa2, 0xf7));
        assert_eq!(tokyo.selection, Some(Rgb(0x29, 0x2e, 0x42)));

        let latte = fixture("catppuccin-latte");
        assert!(latte.light, "mode = \"light\"");
        assert_eq!(latte.accent, Rgb(0x1e, 0x66, 0xf5));

        let gruvbox = fixture("gruvbox");
        assert!(!gruvbox.light);
        assert_eq!(gruvbox.red, Rgb(0xea, 0x69, 0x62));
    }

    #[test]
    fn legacy_alacritty_theme_and_light_marker() {
        let legacy = fixture("legacy-alacritty-light");
        assert!(legacy.light, "light.mode маркер");
        assert_eq!(legacy.background, Rgb(0xfa, 0xf4, 0xed));
        assert_eq!(legacy.red, Rgb(0xb4, 0x63, 0x7a));
        // Без `accent` Omarchy бере синій (color4).
        assert_eq!(legacy.accent, legacy.blue);
    }

    #[test]
    fn mode_falls_back_to_background_brightness() {
        let mut keys = HashMap::new();
        keys.insert("bg".into(), "#f0f0f0".into());
        keys.insert("fg".into(), "#202020".into());
        assert!(resolve_palette("x", &keys, false).unwrap().light);
        keys.insert("theme_type".into(), "dark".into());
        assert!(!resolve_palette("x", &keys, true).unwrap().light);
        keys.clear();
        assert!(resolve_palette("x", &keys, false).is_none());
    }

    #[test]
    fn current_theme_found_via_state_dir_and_legacy_symlink() {
        let home = tempfile::tempdir().unwrap();
        let themes = home.path().join(".config/omarchy/themes");
        std::fs::create_dir_all(&themes).unwrap();
        for name in ["tokyo-night", "gruvbox"] {
            let dir = themes.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::copy(
                Path::new(FIXTURES).join(name).join("colors.toml"),
                dir.join("colors.toml"),
            )
            .unwrap();
        }
        let current = home.path().join(".config/omarchy/current");
        std::fs::create_dir_all(&current).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(themes.join("tokyo-night"), current.join("theme")).unwrap();
        let resolve = |home: &Path| {
            let dirs = [
                home.join(".local/state/omarchy/current/theme"),
                home.join(".config/omarchy/current/theme"),
            ];
            dirs.into_iter()
                .find_map(|p| std::fs::canonicalize(p).ok())
                .and_then(|d| read_omarchy_theme(&d))
        };
        #[cfg(unix)]
        assert_eq!(resolve(home.path()).unwrap().name, "tokyo-night");
        // Новий формат має пріоритет: каталог у ~/.local/state, ім’я з theme.name.
        let state = home.path().join(".local/state/omarchy/current");
        std::fs::create_dir_all(state.join("theme")).unwrap();
        std::fs::copy(
            themes.join("gruvbox/colors.toml"),
            state.join("theme/colors.toml"),
        )
        .unwrap();
        std::fs::write(state.join("theme.name"), "gruvbox\n").unwrap();
        let t = resolve(home.path()).unwrap();
        assert_eq!(t.name, "gruvbox");
        assert_eq!(t.background, Rgb(0x28, 0x28, 0x28));
    }

    /// Жива перевірка на машині розробника (лише читання):
    /// `cargo test -p app live_portal_read -- --ignored --nocapture`.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "потребує сесії D-Bus із xdg-desktop-portal"]
    fn live_portal_read() {
        let portal = read_portal_blocking().expect("портал Settings");
        println!(
            "color-scheme = {:?}, accent-color = {:?}",
            portal.color_scheme,
            portal.accent.map(Rgb::hex)
        );
        assert!(portal.color_scheme.is_some());
    }
}
