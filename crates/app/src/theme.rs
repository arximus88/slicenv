//! Вигляд як у Zed: палітра One Dark, IBM Plex Sans/Mono, щільні рядки, тонкі межі.
//!
//! Компоненти gpui-component фарбуються через `ThemeConfig` (той самий формат, що й теми Zed),
//! а власні елементи (доки, вкладки, рядок стану) беруть кольори з [`zed()`].
//!
//! Палітра не фіксована: [`refresh`] вибирає вбудовану One Dark/One Light, палітру Omarchy
//! або вбудовану з системним акцентом порталу — див. [`crate::system_theme`].

use std::{
    borrow::Cow,
    collections::HashMap,
    rc::Rc,
    sync::{Mutex, RwLock},
};

use gpui::{App, Hsla, SharedString};
use gpui_component::{Theme, ThemeConfig, ThemeMode};
use serde::{Deserialize, Serialize};

use crate::system_theme::{
    AA_TEXT, AA_UI, BLACK, OmarchyTheme, Portal, Rgb, WHITE, contrast, ensure_contrast,
};

/// Кольори One Dark з `assets/themes/one/one.json` Zed — ті ж імена, що й у теми Zed.
#[derive(Clone, Copy)]
pub struct Zed {
    /// Фон вікна, заголовка й рядка стану (`background`, `title_bar.background`).
    pub background: Hsla,
    /// Доки й неактивні вкладки (`panel.background`, `surface.background`).
    pub panel: Hsla,
    /// Робоча область — тут 3D-сцена й вміст сторінок (`editor.background`).
    pub editor: Hsla,
    pub border: Hsla,
    pub border_variant: Hsla,
    pub border_focused: Hsla,
    /// Фон контролів (полів, списків) — `element.background`.
    pub element: Hsla,
    pub element_hover: Hsla,
    pub element_selected: Hsla,
    pub text: Hsla,
    pub text_muted: Hsla,
    pub text_placeholder: Hsla,
    pub accent: Hsla,
    pub accent_bg: Hsla,
    pub accent_border: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    pub error: Hsla,
    pub indent_guide: Hsla,
    /// Зелений індикатор «живого» принтера й моделі.
    pub printer_green: Hsla,
}

/// Та сама палітра в sRGB: для змішування, перевірки контрасту й генерації `ThemeConfig`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Swatch {
    pub background: Rgb,
    pub panel: Rgb,
    pub editor: Rgb,
    pub border: Rgb,
    pub border_variant: Rgb,
    pub border_focused: Rgb,
    pub element: Rgb,
    pub element_hover: Rgb,
    pub element_selected: Rgb,
    pub text: Rgb,
    pub text_muted: Rgb,
    pub text_placeholder: Rgb,
    pub accent: Rgb,
    pub accent_border: Rgb,
    pub success: Rgb,
    pub warning: Rgb,
    pub error: Rgb,
    pub magenta: Rgb,
    pub cyan: Rgb,
    pub printer_green: Rgb,
}

const fn h(hex: u32) -> Rgb {
    Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// One Dark з `assets/themes/one/one.json` Zed.
pub const ONE_DARK_SWATCH: Swatch = Swatch {
    background: h(0x3b414d),
    panel: h(0x2f343e),
    editor: h(0x282c33),
    border: h(0x464b57),
    border_variant: h(0x363c46),
    border_focused: h(0x47679e),
    element: h(0x2e343e),
    element_hover: h(0x363c46),
    element_selected: h(0x454a56),
    text: h(0xdce0e5),
    text_muted: h(0xa9afbc),
    text_placeholder: h(0x878a98),
    accent: h(0x74ade8),
    accent_border: h(0x293b5b),
    success: h(0xa1c181),
    warning: h(0xdec184),
    error: h(0xd07277),
    magenta: h(0xb477cf),
    cyan: h(0x6eb4bf),
    printer_green: h(0x00ae42),
};

pub const ONE_LIGHT_SWATCH: Swatch = Swatch {
    background: h(0xf1f3f6),
    panel: h(0xe8ebf0),
    editor: h(0xffffff),
    border: h(0xcbd1db),
    border_variant: h(0xdde1e8),
    border_focused: h(0x3974ba),
    element: h(0xe9edf2),
    element_hover: h(0xdce3ec),
    element_selected: h(0xcbd7e6),
    text: h(0x202733),
    text_muted: h(0x586474),
    text_placeholder: h(0x788494),
    accent: h(0x2869ad),
    accent_border: h(0x9ab9dc),
    success: h(0x34764a),
    warning: h(0x93620a),
    error: h(0xb33b43),
    magenta: h(0x8651a6),
    cyan: h(0x287c88),
    printer_green: h(0x008f38),
};

impl Swatch {
    pub fn zed(&self) -> Zed {
        let c = |rgb: Rgb| rgb.hsla(0xff);
        Zed {
            background: c(self.background),
            panel: c(self.panel),
            editor: c(self.editor),
            border: c(self.border),
            border_variant: c(self.border_variant),
            border_focused: c(self.border_focused),
            element: c(self.element),
            element_hover: c(self.element_hover),
            element_selected: c(self.element_selected),
            text: c(self.text),
            text_muted: c(self.text_muted),
            text_placeholder: c(self.text_placeholder),
            accent: c(self.accent),
            accent_bg: self.accent.hsla(0x1a),
            accent_border: c(self.accent_border),
            success: c(self.success),
            warning: c(self.warning),
            error: c(self.error),
            indent_guide: c(self.border_variant),
            printer_green: c(self.printer_green),
        }
    }

    /// Поверхні вторинного тексту: виділений рядок перемикає його на основний `text`.
    pub fn muted_surfaces(&self) -> [Rgb; 5] {
        [
            self.editor,
            self.panel,
            self.background,
            self.element,
            self.element_hover,
        ]
    }

    /// Де бувають поля вводу з підказкою (не заголовок вікна).
    pub fn field_surfaces(&self) -> [Rgb; 4] {
        [self.editor, self.panel, self.element, self.element_hover]
    }

    /// Поверхні, на яких лежить звичайний текст.
    pub fn text_surfaces(&self) -> [Rgb; 6] {
        [
            self.editor,
            self.panel,
            self.background,
            self.element,
            self.element_hover,
            self.element_selected,
        ]
    }

    /// Поверхні, де з’являється акцентний і статусний текст (посилання, тости, підсумки).
    fn accent_surfaces(&self) -> [Rgb; 3] {
        [self.editor, self.panel, self.background]
    }

    /// Системний акцент (портал): акцент, рамка фокуса й акцентна межа; контраст тексту ≥ AA.
    pub fn with_accent(mut self, accent: Rgb) -> Self {
        self.accent = ensure_contrast(accent, &self.accent_surfaces(), AA_TEXT);
        self.border_focused = self.accent.mix(self.editor, 0.35);
        self.accent_border = self.accent.mix(self.editor, 0.65);
        self
    }

    /// Палітра Omarchy → усі поля `Zed`. Похідні тони — змішування фону до тексту (так само
    /// Omarchy сам виводить відтінки в `default/themed/*.tpl` через `mix`).
    pub fn from_omarchy(t: &OmarchyTheme) -> Self {
        let (bg, fg) = (t.background, t.foreground);
        let base = if t.light {
            ONE_LIGHT_SWATCH
        } else {
            ONE_DARK_SWATCH
        };
        let editor = bg;
        let panel = bg.mix(fg, 0.05);
        let background = bg.mix(fg, 0.10);
        let element = bg.mix(fg, 0.07);
        let element_hover = bg.mix(fg, 0.13);
        // Виділення теми, якщо воно помітне на фоні; інакше — змішаний тон.
        let element_selected = t
            .selection
            .filter(|s| contrast(*s, bg) >= 1.15)
            .unwrap_or_else(|| bg.mix(fg, 0.20));
        let mut s = Swatch {
            background,
            panel,
            editor,
            border: bg.mix(fg, 0.22),
            border_variant: bg.mix(fg, 0.12),
            border_focused: base.border_focused,
            element,
            element_hover,
            element_selected,
            text: fg,
            text_muted: fg.mix(bg, 0.30),
            text_placeholder: fg.mix(bg, 0.45),
            accent: t.accent,
            accent_border: base.accent_border,
            success: t.green,
            warning: t.yellow,
            error: t.red,
            magenta: t.magenta,
            cyan: t.cyan,
            printer_green: base.printer_green,
        };
        let surfaces = s.text_surfaces();
        s.text = ensure_contrast(s.text, &surfaces, AA_TEXT);
        s.text_muted = ensure_contrast(s.text_muted, &s.muted_surfaces(), AA_TEXT);
        s.text_placeholder = ensure_contrast(s.text_placeholder, &s.field_surfaces(), AA_UI);
        let accent_surfaces = s.accent_surfaces();
        for c in [&mut s.success, &mut s.warning, &mut s.error] {
            *c = ensure_contrast(*c, &accent_surfaces, AA_TEXT);
        }
        s.with_accent(t.accent)
    }

    /// Текст на заливці акцентом (кнопки primary, бейджі).
    fn on_accent(&self) -> Rgb {
        let dark = self.editor.mix(BLACK, 0.5).mix(h(0x1b2029), 0.5);
        [dark, WHITE, BLACK]
            .into_iter()
            .max_by(|a, b| contrast(*a, self.accent).total_cmp(&contrast(*b, self.accent)))
            .unwrap_or(WHITE)
    }
}

/// Поточна палітра (і чи вона світла). `None` — ще не застосована: One Dark.
static CURRENT: RwLock<Option<(Swatch, bool, bool)>> = RwLock::new(None);

fn current() -> (Swatch, bool, bool) {
    CURRENT
        .read()
        .ok()
        .and_then(|c| *c)
        .unwrap_or((ONE_DARK_SWATCH, false, false))
}

pub fn is_light() -> bool {
    current().1
}

pub fn zed() -> Zed {
    current().0.zed()
}

/// Фон вікна у `#rrggbb` — для `ui_state` і перевірок.
pub fn zed_hex_background() -> String {
    current().0.background.hex()
}

/// Фон 3D-сцени, коли його задає тема ОС (Omarchy); `None` — вбудовані фони рендерера.
pub fn scene_background() -> Option<Rgb> {
    let (swatch, _, omarchy) = current();
    omarchy.then_some(swatch.editor)
}

pub const UI_FONT: &str = "IBM Plex Sans";
pub const MONO_FONT: &str = "IBM Plex Mono";

/// Вбудовані шрифти: IBM Plex — UI-шрифт Zed, з кирилицею.
fn load_fonts(cx: &mut App) {
    let fonts: Vec<Cow<'static, [u8]>> = vec![
        Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexSans-Medium.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexMono-Medium.ttf")),
    ];
    if let Err(err) = cx.text_system().add_fonts(fonts) {
        eprintln!("шрифти IBM Plex не завантажились: {err:#}");
    }
}

/// One Dark у термінах gpui-component: кнопки — як ghost/filled-кнопки Zed, поля — як його інпути.
const ONE_DARK: &str = r##"{
  "name": "Zed One Dark",
  "mode": "dark",
  "font.size": 16,
  "font.family": "IBM Plex Sans",
  "mono_font.family": "IBM Plex Mono",
  "mono_font.size": 13,
  "radius": 4,
  "radius.lg": 6,
  "shadow": false,
  "colors": {
    "background": "#282c33",
    "foreground": "#dce0e5",
    "border": "#363c46",
    "input.border": "#464b57",
    "ring": "#47679e",
    "caret": "#74ade8",
    "selection.background": "#74ade83d",

    "accent.background": "#363c46",
    "accent.foreground": "#dce0e5",
    "muted.background": "#2e343e",
    "muted.foreground": "#a9afbc",

    "primary.background": "#74ade8",
    "primary.hover.background": "#8bbcee",
    "primary.active.background": "#5f9be0",
    "primary.foreground": "#1b2029",
    "secondary.background": "#2e343e",
    "secondary.hover.background": "#363c46",
    "secondary.active.background": "#454a56",
    "secondary.foreground": "#dce0e5",

    "button.background": "#2e343e",
    "button.hover.background": "#363c46",
    "button.active.background": "#454a56",
    "button.foreground": "#dce0e5",
    "button.primary.background": "#74ade8",
    "button.primary.hover.background": "#8bbcee",
    "button.primary.active.background": "#5f9be0",
    "button.primary.foreground": "#1b2029",
    "button.secondary.background": "#2e343e",
    "button.secondary.hover.background": "#363c46",
    "button.secondary.active.background": "#454a56",
    "button.secondary.foreground": "#dce0e5",
    "button.danger.background": "#d0727726",
    "button.danger.hover.background": "#d0727740",
    "button.danger.active.background": "#d0727755",
    "button.danger.foreground": "#d07277",
    "button.success.background": "#a1c18126",
    "button.success.hover.background": "#a1c18140",
    "button.success.active.background": "#a1c18155",
    "button.success.foreground": "#a1c181",

    "danger.background": "#d07277",
    "danger.hover.background": "#d98589",
    "danger.active.background": "#c25f64",
    "danger.foreground": "#1b2029",
    "success.background": "#a1c181",
    "success.hover.background": "#b0cc94",
    "success.active.background": "#8fb36d",
    "success.foreground": "#1b2029",
    "warning.background": "#dec184",
    "warning.hover.background": "#e5cd99",
    "warning.active.background": "#d4b36d",
    "warning.foreground": "#1b2029",
    "info.background": "#74ade8",
    "info.hover.background": "#8bbcee",
    "info.active.background": "#5f9be0",
    "info.foreground": "#1b2029",

    "popover.background": "#2f343e",
    "popover.foreground": "#dce0e5",
    "list.background": "#2f343e",
    "list.hover.background": "#363c46",
    "list.active.background": "#454a56",
    "list.active.border": "#47679e",
    "list.even.background": "#2f343e",
    "list.head.background": "#2f343e",
    "table.background": "#282c33",
    "table.hover.background": "#2f343e",
    "table.active.background": "#454a56",
    "table.active.border": "#47679e",
    "table.even.background": "#282c33",
    "table.head.background": "#2f343e",
    "table.head.foreground": "#a9afbc",
    "table.row.border": "#363c46",

    "sidebar.background": "#2f343e",
    "sidebar.foreground": "#dce0e5",
    "sidebar.border": "#363c46",
    "sidebar.accent.background": "#363c46",
    "sidebar.accent.foreground": "#dce0e5",
    "sidebar.primary.background": "#74ade8",
    "sidebar.primary.foreground": "#1b2029",

    "tab_bar.background": "#2f343e",
    "tab_bar.segmented.background": "#2e343e",
    "tab.background": "#2f343e",
    "tab.foreground": "#a9afbc",
    "tab.active.background": "#282c33",
    "tab.active.foreground": "#dce0e5",

    "title_bar.background": "#3b414d",
    "title_bar.border": "#363c46",
    "status_bar.background": "#3b414d",
    "status_bar.border": "#363c46",
    "window.border": "#464b57",
    "overlay": "#0d0f1280",

    "scrollbar.background": "#00000000",
    "scrollbar.thumb.background": "#c8ccd44c",
    "scrollbar.thumb.hover.background": "#c8ccd480",
    "progress.bar.background": "#74ade8",
    "slider.background": "#74ade8",
    "slider.thumb.background": "#dce0e5",
    "switch.background": "#454a56",
    "switch.thumb.background": "#dce0e5",
    "skeleton.background": "#363c46",
    "link": "#74ade8",
    "link.hover": "#8bbcee",
    "link.active": "#5f9be0",
    "drag.border": "#74ade8",
    "drop_target.background": "#74ade81a",
    "group_box.background": "#2f343e",
    "accordion.background": "#2f343e",
    "description_list.label.background": "#2f343e",
    "description_list.label.foreground": "#a9afbc",

    "chart.1": "#74ade8",
    "chart.2": "#a1c181",
    "chart.3": "#dec184",
    "chart.4": "#b477cf",
    "chart.5": "#6eb4bf",
    "chart.grid": "#363c46",

    "base.red": "#d07277",
    "base.red.light": "#d0727726",
    "base.green": "#a1c181",
    "base.green.light": "#a1c18126",
    "base.blue": "#74ade8",
    "base.blue.light": "#74ade81a",
    "base.yellow": "#dec184",
    "base.yellow.light": "#dec18426",
    "base.magenta": "#b477cf",
    "base.magenta.light": "#b477cf26",
    "base.cyan": "#6eb4bf",
    "base.cyan.light": "#6eb4bf26"
  }
}"##;

// Keep the same complete component theme schema as One Dark, with a deliberate
// light surface/ink mapping so popovers, tables, fields and hover states switch too.
fn light_theme_json() -> String {
    let mut json = ONE_DARK
        .replace("Zed One Dark", "Zed One Light")
        .replace("\"dark\"", "\"light\"");
    for (dark, light) in [
        ("#282c33", "#ffffff"),
        ("#2f343e", "#e8ebf0"),
        ("#3b414d", "#f1f3f6"),
        ("#363c46", "#dde1e8"),
        ("#464b57", "#cbd1db"),
        ("#2e343e", "#e9edf2"),
        ("#454a56", "#cbd7e6"),
        ("#dce0e5", "#202733"),
        ("#a9afbc", "#586474"),
        ("#878a98", "#788494"),
        ("#74ade8", "#2869ad"),
        ("#8bbcee", "#397bbd"),
        ("#5f9be0", "#205b99"),
        ("#1b2029", "#ffffff"),
        ("#47679e", "#3974ba"),
        ("#293b5b", "#9ab9dc"),
        ("#a1c181", "#34764a"),
        ("#b0cc94", "#43855a"),
        ("#8fb36d", "#28613b"),
        ("#dec184", "#93620a"),
        ("#e5cd99", "#a97612"),
        ("#d4b36d", "#7c5100"),
        ("#d07277", "#b33b43"),
        ("#d98589", "#c24b53"),
        ("#c25f64", "#982c34"),
        ("#b477cf", "#8651a6"),
        ("#6eb4bf", "#287c88"),
        ("#c8ccd4", "#596575"),
        ("#0d0f12", "#202733"),
    ] {
        json = json.replace(dark, light);
    }
    json
}

/// Один прохід по `#rrggbb` у JSON: послідовні `replace` могли б перефарбувати вже замінене.
fn recolor(source: &str, map: &HashMap<&str, String>) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(i) = rest.find('#') {
        out.push_str(&rest[..i]);
        let token = rest.get(i..i + 7);
        match token.and_then(|t| map.get(t)) {
            Some(v) => {
                out.push_str(v);
                rest = &rest[i + 7..];
            }
            None => {
                out.push('#');
                rest = &rest[i + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `ThemeConfig` для довільної палітри: кожен токен One Dark отримує роль із `Swatch`.
pub fn theme_json(s: &Swatch, light: bool, name: &str) -> String {
    let hover = |c: Rgb| c.mix(s.text, 0.15).hex();
    let active = |c: Rgb| c.mix(s.editor, 0.15).hex();
    let map: HashMap<&str, String> = [
        ("#282c33", s.editor.hex()),
        ("#2f343e", s.panel.hex()),
        ("#3b414d", s.background.hex()),
        ("#363c46", s.border_variant.hex()),
        ("#464b57", s.border.hex()),
        ("#2e343e", s.element.hex()),
        ("#454a56", s.element_selected.hex()),
        ("#dce0e5", s.text.hex()),
        ("#a9afbc", s.text_muted.hex()),
        ("#878a98", s.text_placeholder.hex()),
        ("#74ade8", s.accent.hex()),
        ("#8bbcee", hover(s.accent)),
        ("#5f9be0", active(s.accent)),
        ("#1b2029", s.on_accent().hex()),
        ("#47679e", s.border_focused.hex()),
        ("#293b5b", s.accent_border.hex()),
        ("#a1c181", s.success.hex()),
        ("#b0cc94", hover(s.success)),
        ("#8fb36d", active(s.success)),
        ("#dec184", s.warning.hex()),
        ("#e5cd99", hover(s.warning)),
        ("#d4b36d", active(s.warning)),
        ("#d07277", s.error.hex()),
        ("#d98589", hover(s.error)),
        ("#c25f64", active(s.error)),
        ("#b477cf", s.magenta.hex()),
        ("#6eb4bf", s.cyan.hex()),
        ("#c8ccd4", s.text_muted.hex()),
        ("#0d0f12", s.editor.mix(BLACK, 0.6).hex()),
    ]
    .into_iter()
    .collect();
    recolor(ONE_DARK, &map)
        .replacen("Zed One Dark", name, 1)
        .replacen("\"dark\"", if light { "\"light\"" } else { "\"dark\"" }, 1)
}

/// Режим теми в налаштуваннях.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    /// Тема ОС: Omarchy, якщо він є, інакше портал (GNOME/KDE) з акцентом.
    #[default]
    System,
    Light,
    Dark,
    Omarchy,
}

/// Що відомо про тему ОС. Оновлюється спостерігачами з [`crate::system_theme`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SystemState {
    pub portal: Portal,
    pub omarchy: Option<OmarchyTheme>,
    /// Початкова підказка gpui (`window_appearance`), поки портал ще не відповів.
    pub appearance_dark: Option<bool>,
}

static SYSTEM: Mutex<SystemState> = Mutex::new(SystemState {
    portal: Portal {
        color_scheme: None,
        accent: None,
    },
    omarchy: None,
    appearance_dark: None,
});

pub fn update_system(update: impl FnOnce(&mut SystemState)) {
    if let Ok(mut s) = SYSTEM.lock() {
        update(&mut s);
    }
}

pub fn omarchy_detected() -> bool {
    SYSTEM.lock().is_ok_and(|s| s.omarchy.is_some())
}

/// Підсумок вибору: палітра, режим і шрифт.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub swatch: Swatch,
    pub light: bool,
    pub omarchy: bool,
    pub mono_font: Option<String>,
    pub label: String,
}

pub fn resolve(choice: ThemeChoice, omarchy_font: bool, sys: &SystemState) -> Resolved {
    let omarchy = matches!(choice, ThemeChoice::System | ThemeChoice::Omarchy)
        .then_some(sys.omarchy.as_ref())
        .flatten();
    if let Some(t) = omarchy {
        return Resolved {
            swatch: Swatch::from_omarchy(t),
            light: t.light,
            omarchy: true,
            mono_font: omarchy_font.then(|| t.mono_font.clone()).flatten(),
            label: format!(
                "Omarchy «{}» ({})",
                t.name,
                if t.light {
                    "світла"
                } else {
                    "темна"
                }
            ),
        };
    }
    let light = match choice {
        ThemeChoice::Light => true,
        ThemeChoice::Dark => false,
        _ => match sys.portal.color_scheme {
            Some(2) => true,
            Some(_) => false,
            None => sys.appearance_dark == Some(false),
        },
    };
    let base = if light {
        ONE_LIGHT_SWATCH
    } else {
        ONE_DARK_SWATCH
    };
    // «Omarchy» без Omarchy поводиться як «Системна».
    let system = matches!(choice, ThemeChoice::System | ThemeChoice::Omarchy);
    let accent = sys.portal.accent.filter(|_| system);
    let mut label = String::from(if light { "світла" } else { "темна" });
    if system {
        label = format!("системна, {label}");
    }
    if let Some(a) = accent {
        label += &format!(", акцент {}", a.hex());
    }
    Resolved {
        swatch: accent.map_or(base, |a| base.with_accent(a)),
        light,
        omarchy: false,
        mono_font: None,
        label,
    }
}

static APPLIED: Mutex<Option<Resolved>> = Mutex::new(None);

fn apply_resolved(r: &Resolved, cx: &mut App) {
    if let Ok(mut c) = CURRENT.write() {
        *c = Some((r.swatch, r.light, r.omarchy));
    }
    let mode = if r.light {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    Theme::change(mode, None, cx);
    let source = if r.swatch == ONE_DARK_SWATCH && !r.light {
        ONE_DARK.to_owned()
    } else if r.swatch == ONE_LIGHT_SWATCH && r.light {
        light_theme_json()
    } else {
        theme_json(&r.swatch, r.light, &r.label)
    };
    let config: ThemeConfig = serde_json::from_str(&source).expect("тема — валідний JSON");
    let config = Rc::new(config);
    let ui: SharedString = r.mono_font.clone().map_or(UI_FONT.into(), Into::into);
    let mono: SharedString = r.mono_font.clone().map_or(MONO_FONT.into(), Into::into);
    Theme::update(cx, |theme| {
        theme.apply_config(&config);
        theme.font_family = ui;
        theme.mono_font_family = mono;
    });
}

/// Застосовує вибір користувача з урахуванням теми ОС. `true` — палітра змінилась
/// (тоді викликач має оновити вікно й рендерер).
pub fn refresh(choice: ThemeChoice, omarchy_font: bool, cx: &mut App) -> bool {
    let resolved = {
        let sys = SYSTEM.lock().map(|s| s.clone()).unwrap_or_default();
        resolve(choice, omarchy_font, &sys)
    };
    let mut applied = APPLIED.lock().unwrap_or_else(|e| e.into_inner());
    if applied.as_ref() == Some(&resolved) {
        return false;
    }
    eprintln!("Slicenv: тема → {}", resolved.label);
    apply_resolved(&resolved, cx);
    *applied = Some(resolved);
    true
}

/// Шрифти й початковий стан ОС; тему застосовує [`refresh`].
pub fn apply(cx: &mut App) {
    load_fonts(cx);
    let dark = matches!(
        cx.window_appearance(),
        gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
    );
    let omarchy = crate::system_theme::load_omarchy();
    update_system(|s| {
        s.appearance_dark = Some(dark);
        s.omarchy = omarchy;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system_theme::{read_omarchy_theme, worst_contrast as worst};
    use std::path::Path;

    fn fixture(name: &str) -> OmarchyTheme {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/omarchy");
        read_omarchy_theme(&dir.join(name)).unwrap()
    }

    /// Текст і `text_muted` — завжди AA. Акцент, підказки й статуси — там, де палітру виводимо
    /// ми (Omarchy, системний акцент); вбудовані One Dark/Light лишаються як у Zed.
    fn assert_aa(s: &Swatch, what: &str, accent: bool, status: bool) {
        let text = s.text_surfaces();
        assert!(worst(s.text, &text) >= AA_TEXT, "{what}: text");
        assert!(
            worst(s.text_muted, &s.muted_surfaces()) >= AA_TEXT,
            "{what}: text_muted"
        );
        let surfaces = s.accent_surfaces();
        if accent {
            assert!(worst(s.accent, &surfaces) >= AA_TEXT, "{what}: accent");
            assert!(
                contrast(s.on_accent(), s.accent) >= AA_UI,
                "{what}: on-accent"
            );
        }
        if status {
            assert!(
                worst(s.text_placeholder, &s.field_surfaces()) >= AA_UI,
                "{what}: placeholder"
            );
            for (c, n) in [
                (s.success, "success"),
                (s.warning, "warning"),
                (s.error, "error"),
            ] {
                assert!(worst(c, &surfaces) >= AA_TEXT, "{what}: {n}");
            }
        }
    }

    #[test]
    fn omarchy_fixtures_map_to_accessible_palettes() {
        for name in [
            "tokyo-night",
            "catppuccin-latte",
            "gruvbox",
            "legacy-alacritty-light",
        ] {
            let t = fixture(name);
            let s = Swatch::from_omarchy(&t);
            assert_aa(&s, name, true, true);
            assert_eq!(s.editor, t.background, "{name}: сцена = фон теми");
            // Світла тема — темніші панелі, темна — світліші: тон іде до тексту.
            let lum = |c: Rgb| c.luminance();
            if t.light {
                assert!(
                    lum(s.panel) < lum(s.editor) && lum(s.border) < lum(s.panel),
                    "{name}"
                );
            } else {
                assert!(
                    lum(s.panel) > lum(s.editor) && lum(s.border) > lum(s.panel),
                    "{name}"
                );
            }
        }
        let tokyo = Swatch::from_omarchy(&fixture("tokyo-night"));
        // Tokyo Night вже контрастна: кольори теми лишаються як є.
        assert_eq!(tokyo.text, Rgb(0xa9, 0xb1, 0xd6));
        assert_eq!(tokyo.accent, Rgb(0x7a, 0xa2, 0xf7));
        assert_eq!(tokyo.element_selected, Rgb(0x29, 0x2e, 0x42));
    }

    #[test]
    fn builtin_palettes_and_portal_accents_stay_accessible() {
        assert_aa(&ONE_DARK_SWATCH, "one dark", false, false);
        assert_aa(&ONE_LIGHT_SWATCH, "one light", false, false);
        // Типові акценти GNOME 47+: синій, жовтий (поганий на світлому), сланцевий.
        for accent in [
            h(0x3584e4),
            h(0xc88800),
            h(0x6f8396),
            h(0xffffff),
            h(0x000000),
        ] {
            assert_aa(
                &ONE_DARK_SWATCH.with_accent(accent),
                "dark+accent",
                true,
                false,
            );
            assert_aa(
                &ONE_LIGHT_SWATCH.with_accent(accent),
                "light+accent",
                true,
                false,
            );
        }
    }

    #[test]
    fn resolve_follows_choice_and_system_state() {
        let mut sys = SystemState::default();
        assert!(!resolve(ThemeChoice::System, false, &sys).light);
        sys.appearance_dark = Some(false);
        assert!(resolve(ThemeChoice::System, false, &sys).light);
        sys.portal.color_scheme = Some(1);
        assert!(!resolve(ThemeChoice::System, false, &sys).light);
        sys.portal.color_scheme = Some(2);
        sys.portal.accent = Some(h(0x3584e4));
        let r = resolve(ThemeChoice::System, false, &sys);
        assert!(r.light && r.swatch.accent != ONE_LIGHT_SWATCH.accent);
        // Явний вибір не бере системний акцент.
        assert_eq!(
            resolve(ThemeChoice::Dark, false, &sys).swatch,
            ONE_DARK_SWATCH
        );
        // Omarchy без Omarchy — як системна.
        assert_eq!(resolve(ThemeChoice::Omarchy, false, &sys), r);
        let mut t = fixture("tokyo-night");
        t.mono_font = Some("JetBrainsMono Nerd Font".into());
        sys.omarchy = Some(t);
        let r = resolve(ThemeChoice::System, true, &sys);
        assert!(r.omarchy && !r.light);
        assert_eq!(r.mono_font.as_deref(), Some("JetBrainsMono Nerd Font"));
        assert!(
            resolve(ThemeChoice::Omarchy, false, &sys)
                .mono_font
                .is_none()
        );
        assert!(!resolve(ThemeChoice::Light, true, &sys).omarchy);
    }

    #[test]
    fn generated_theme_json_is_valid_and_recolored() {
        let s = Swatch::from_omarchy(&fixture("catppuccin-latte"));
        let json = theme_json(&s, true, "Omarchy");
        let config: ThemeConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config.mode, ThemeMode::Light);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["colors"]["background"], s.editor.hex());
        assert_eq!(v["colors"]["foreground"], s.text.hex());
        // Альфа-суфікси токенів зберігаються.
        assert_eq!(
            v["colors"]["selection.background"],
            format!("{}3d", s.accent.hex())
        );
        // Жодного токена One Dark не лишилось.
        for token in ["#282c33", "#dce0e5", "#74ade8", "#2f343e"] {
            assert!(!json.contains(token), "{token}");
        }
    }
}
