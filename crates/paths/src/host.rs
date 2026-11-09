//! Зовнішні процеси з «чистим» оточенням хоста.
//!
//! Усередині AppImage рантайм і наш `AppRun` виставляють `APPDIR`, `APPIMAGE`, `ARGV0`,
//! `OWD` і дописують шляхи бандла в `LD_LIBRARY_PATH`/`PATH`/`XDG_DATA_DIRS`. Дочірні
//! процеси (рушії-AppImage, `python3 manage_engines.py`, `curl`, `xdg-open`…) успадковують
//! це й ламаються: чужий AppImage підхоплює наші `.so`, python — не той `PYTHONHOME`.
//!
//! `AppRun` (packaging/appimage/AppRun) зберігає початкові значення у
//! `SLICENV_ORIG_<ЗМІННА>` (порожнє значення = змінної не було). Тут ми їх відновлюємо,
//! а без збереженого значення прибираємо шляхи, що ведуть у `$APPDIR`. Поза AppImage
//! (`APPDIR` не задано) команда не змінюється.
use std::{
    ffi::{OsStr, OsString},
    process::Command,
};

/// Префікс змінних, у яких `AppRun` зберігає початкове оточення.
pub const ORIG_PREFIX: &str = "SLICENV_ORIG_";

/// Змінні AppImage-рантайму — дочірньому процесу вони лише шкодять.
const APPIMAGE_VARS: &[&str] = &["APPDIR", "APPIMAGE", "ARGV0", "OWD"];

/// Змінні, які `AppRun` може змінити; відновлюються з `SLICENV_ORIG_*`.
const RESTORED_VARS: &[&str] = &[
    "LD_LIBRARY_PATH",
    "LD_PRELOAD",
    "PATH",
    "XDG_DATA_DIRS",
    "PYTHONHOME",
    "PYTHONPATH",
    "GSETTINGS_SCHEMA_DIR",
    "GDK_PIXBUF_MODULE_FILE",
    "GIO_MODULE_DIR",
    "GTK_PATH",
    "QT_PLUGIN_PATH",
];

/// Що зробити зі змінною в дочірньому процесі.
#[derive(Debug, PartialEq, Eq)]
pub enum EnvAction {
    Remove(String),
    Set(String, OsString),
}

/// `Command::new` для зовнішньої програми: в AppImage — з відновленим оточенням хоста.
///
/// На Windows процес стартує з `CREATE_NO_WINDOW`: GUI-застосунок (підсистема `windows`)
/// інакше відкривав би порожнє консольне вікно для кожного `curl`/`tar`/рушія.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    sanitize(&mut command);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Застосовує [`plan`] до вже створеної команди.
pub fn sanitize(command: &mut Command) -> &mut Command {
    let vars: Vec<(String, OsString)> = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v)))
        .collect();
    for action in plan(
        |key| vars.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()),
        vars.iter().map(|(k, _)| k.as_str()),
    ) {
        match action {
            EnvAction::Remove(key) => command.env_remove(key),
            EnvAction::Set(key, value) => command.env(key, value),
        };
    }
    command
}

/// Сигнал, яким завершився процес (Unix). На Windows сигналів немає: аварійне завершення
/// видно за кодом виходу NTSTATUS класу «помилка» (`0xC0000005` — access violation тощо),
/// тож повертаємо його як «сигнал». `ExitProcess(-62)` дає `0xFFFFFFC2` і сюди не потрапляє.
pub fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    return std::os::unix::process::ExitStatusExt::signal(status);
    #[cfg(not(unix))]
    return status
        .code()
        .filter(|c| (*c as u32) & 0xF000_0000 == 0xC000_0000);
}

/// Виконуваний файл для перезапуску самого застосунку. В AppImage `current_exe` лежить
/// у тимчасовому FUSE-монтуванні, яке зникне разом із батьком, тому беремо `$APPIMAGE`.
pub fn self_exe_for_restart() -> std::io::Result<std::path::PathBuf> {
    if cfg!(target_os = "linux")
        && std::env::var_os("APPDIR").is_some()
        && let Some(image) = std::env::var_os("APPIMAGE").filter(|v| !v.is_empty())
    {
        return Ok(image.into());
    }
    std::env::current_exe()
}

/// Чиста функція: за читачем оточення й списком імен повертає зміни для дитини.
/// Окремо від процесу, щоб тест не чіпав глобальне оточення.
pub fn plan<'a>(
    read: impl Fn(&str) -> Option<OsString>,
    names: impl Iterator<Item = &'a str>,
) -> Vec<EnvAction> {
    if !cfg!(target_os = "linux") {
        return Vec::new();
    }
    let Some(appdir) = read("APPDIR").filter(|v| !v.is_empty()) else {
        return Vec::new();
    };
    let mut actions: Vec<EnvAction> = APPIMAGE_VARS
        .iter()
        .map(|k| EnvAction::Remove((*k).into()))
        .collect();
    for key in RESTORED_VARS {
        match read(&format!("{ORIG_PREFIX}{key}")) {
            Some(orig) if orig.is_empty() => actions.push(EnvAction::Remove((*key).into())),
            Some(orig) => actions.push(EnvAction::Set((*key).into(), orig)),
            None => {
                if let Some(current) = read(key) {
                    match strip_appdir(&current, &appdir) {
                        Some(clean) if clean.is_empty() => {
                            actions.push(EnvAction::Remove((*key).into()))
                        }
                        Some(clean) => actions.push(EnvAction::Set((*key).into(), clean)),
                        None => {}
                    }
                }
            }
        }
    }
    for name in names {
        if name.starts_with(ORIG_PREFIX) {
            actions.push(EnvAction::Remove(name.into()));
        }
    }
    actions
}

/// Прибирає з `:`-списку елементи всередині `appdir`. `None` — нічого не змінилося.
/// AppImage буває лише на Linux; на інших ОС `APPDIR` не наш, і список не чіпаємо.
#[cfg(not(unix))]
fn strip_appdir(_value: &OsStr, _appdir: &OsStr) -> Option<OsString> {
    None
}
#[cfg(unix)]
fn strip_appdir(value: &OsStr, appdir: &OsStr) -> Option<OsString> {
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
    let dir = appdir
        .as_bytes()
        .strip_suffix(b"/")
        .unwrap_or(appdir.as_bytes());
    let inside = |p: &[u8]| p == dir || (p.starts_with(dir) && p.get(dir.len()) == Some(&b'/'));
    let parts: Vec<&[u8]> = value.as_bytes().split(|b| *b == b':').collect();
    if !parts.iter().any(|p| inside(p)) {
        return None;
    }
    let kept: Vec<&[u8]> = parts
        .into_iter()
        .filter(|p| !p.is_empty() && !inside(p))
        .collect();
    Some(OsString::from_vec(kept.join(&b':')))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn run(env: &[(&str, &str)]) -> Vec<EnvAction> {
        let map: HashMap<&str, &str> = env.iter().copied().collect();
        plan(|k| map.get(k).map(OsString::from), map.keys().copied())
    }

    #[test]
    fn outside_appimage_nothing_changes() {
        assert!(run(&[("LD_LIBRARY_PATH", "/x"), ("PATH", "/usr/bin")]).is_empty());
        assert!(run(&[("APPDIR", "")]).is_empty());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn appimage_vars_removed_and_originals_restored() {
        let actions = run(&[
            ("APPDIR", "/tmp/.mount_abc"),
            ("APPIMAGE", "/home/u/Slicenv.AppImage"),
            ("ARGV0", "Slicenv"),
            ("OWD", "/home/u"),
            ("LD_LIBRARY_PATH", "/tmp/.mount_abc/usr/lib:/opt/lib"),
            ("SLICENV_ORIG_LD_LIBRARY_PATH", "/opt/lib"),
            ("PATH", "/tmp/.mount_abc/usr/bin:/usr/bin"),
            ("SLICENV_ORIG_PATH", "/usr/bin:/bin"),
            ("PYTHONHOME", "/tmp/.mount_abc/usr"),
            ("SLICENV_ORIG_PYTHONHOME", ""),
        ]);
        for k in ["APPDIR", "APPIMAGE", "ARGV0", "OWD", "PYTHONHOME"] {
            assert!(actions.contains(&EnvAction::Remove(k.into())), "{k}");
        }
        assert!(actions.contains(&EnvAction::Set("LD_LIBRARY_PATH".into(), "/opt/lib".into())));
        assert!(actions.contains(&EnvAction::Set("PATH".into(), "/usr/bin:/bin".into())));
        for k in [
            "SLICENV_ORIG_LD_LIBRARY_PATH",
            "SLICENV_ORIG_PATH",
            "SLICENV_ORIG_PYTHONHOME",
        ] {
            assert!(actions.contains(&EnvAction::Remove(k.into())), "{k}");
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn without_saved_originals_appdir_entries_are_stripped() {
        let actions = run(&[
            ("APPDIR", "/tmp/.mount_abc/"),
            ("LD_LIBRARY_PATH", "/tmp/.mount_abc/usr/lib"),
            (
                "PATH",
                "/tmp/.mount_abc/usr/bin::/usr/bin:/tmp/.mount_abcd/bin",
            ),
            ("XDG_DATA_DIRS", "/usr/share"),
        ]);
        assert!(actions.contains(&EnvAction::Remove("LD_LIBRARY_PATH".into())));
        assert!(actions.contains(&EnvAction::Set(
            "PATH".into(),
            "/usr/bin:/tmp/.mount_abcd/bin".into()
        )));
        assert!(!actions.iter().any(
            |a| matches!(a, EnvAction::Set(k, _) | EnvAction::Remove(k) if k == "XDG_DATA_DIRS")
        ));
    }

    #[test]
    fn command_applies_plan() {
        // Поза AppImage (у тестах APPDIR не задано) команда без змін оточення.
        if std::env::var_os("APPDIR").is_none() {
            assert_eq!(command("true").get_envs().count(), 0);
        }
    }
}
