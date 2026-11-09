//! Передача нарізаного файлу в Bambu Connect — офіційний шлях Bambu Lab для сторонніх
//! слайсерів (wiki «Third-party integration»). Хмарний друк через плагін Bambu пускає лише
//! підписані програми Bambu (`BAMBU_NETWORK_SIGNED_ERROR`, -26); Bambu Connect сам підписує
//! завдання і надсилає його на принтер через хмару чи LAN.
//!
//! Схема: `bambu-connect://import-file?path=<абс. шлях>&name=<назва>&version=1.0.0`, значення
//! закодовані як `encodeURIComponent` (так само робить OrcaSlicer PR #13811 через
//! `Http::url_encode`). Slicenv лише відкриває посилання; друк запускає користувач у Connect.
//! Інших параметрів схема не має: вирівнювання столу, калібрування потоку й вібрацій,
//! таймлапс і вибір принтера задаються в самому Connect, тож діалог друку їх не показує.
use std::path::{Path, PathBuf};

/// Сторінка Bambu Connect з посиланнями на завантаження (bambulab.com відкидає ботів, вікі — ні).
pub const DOWNLOAD_URL: &str = "https://wiki.bambulab.com/en/software/bambu-connect";

/// `encodeURIComponent`: лишаються тільки `A-Z a-z 0-9 - _ . ! ~ * ' ( )`.
pub fn encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len() * 3);
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Назва завдання для Connect: без розширення; символи, які Connect може прочитати як
/// частину запиту, замінено на `_` (Unicode лишається).
pub fn job_name(file_stem: &str) -> String {
    let name: String = file_stem
        .trim_end_matches(".gcode.3mf")
        .trim_end_matches(".gcode")
        .chars()
        .map(|c| if "&=?#%+".contains(c) || c.is_control() { '_' } else { c })
        .collect();
    let name = name.trim();
    if name.is_empty() { "Slicenv".into() } else { name.to_owned() }
}

pub fn import_url(path: &Path, name: &str) -> String {
    format!(
        "bambu-connect://import-file?path={}&name={}&version=1.0.0",
        encode_component(&path.to_string_lossy()),
        encode_component(&job_name(name))
    )
}

/// Чи встановлено Bambu Connect (зареєстровано обробник `bambu-connect://` з наявним exe).
pub fn installed() -> bool {
    #[cfg(windows)]
    {
        for key in [
            r"HKCU\Software\Classes\bambu-connect\shell\open\command",
            r"HKLM\Software\Classes\bambu-connect\shell\open\command",
        ] {
            let Ok(out) = paths::host::command("reg").args(["query", key, "/ve"]).output() else {
                continue;
            };
            if out.status.success()
                && let Some(exe) = command_exe(&String::from_utf8_lossy(&out.stdout))
                && exe.is_file()
            {
                return true;
            }
        }
        false
    }
    #[cfg(target_os = "macos")]
    {
        // Electron-застосунок, bundle id com.bambulab.bambu-connect. Не перевірено на Mac.
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        [
            PathBuf::from("/Applications/Bambu Connect.app"),
            home.join("Applications/Bambu Connect.app"),
        ]
        .iter()
        .any(|p| p.is_dir())
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        // Офіційної збірки для Linux немає; якщо хтось зареєстрував обробник — поважаємо.
        paths::host::command("xdg-mime")
            .args(["query", "default", "x-scheme-handler/bambu-connect"])
            .output()
            .is_ok_and(|o| o.status.success() && !o.stdout.trim_ascii().is_empty())
    }
}

/// Шлях до exe з рядка `reg query … /ve`: `(Default)    REG_SZ    "C:\…\Bambu Connect.exe" "%1"`.
#[cfg_attr(not(windows), allow(dead_code))]
fn command_exe(reg_output: &str) -> Option<PathBuf> {
    let value = reg_output
        .lines()
        .find_map(|l| l.split_once("REG_SZ").or_else(|| l.split_once("REG_EXPAND_SZ")))?
        .1
        .trim();
    let exe = if let Some(rest) = value.strip_prefix('"') {
        rest.split('"').next()?
    } else {
        value.split_whitespace().next()?
    };
    (!exe.is_empty()).then(|| PathBuf::from(exe))
}

/// Тека, з якої Connect читатиме файл: кеш Slicenv може змінитися при новій нарізці, а
/// Connect відкриває файл пізніше. Старші за 3 дні копії прибираються.
fn handoff_dir() -> PathBuf {
    std::env::temp_dir().join("slicenv").join("bambu-connect")
}

/// Копіює нарізаний файл у теку передачі й повертає `bambu-connect://` посилання.
pub fn prepare(gcode_3mf: &Path, name: &str) -> anyhow::Result<(PathBuf, String)> {
    let dir = handoff_dir();
    std::fs::create_dir_all(&dir)?;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let max_age = std::time::Duration::from_secs(3 * 24 * 3600);
        for entry in entries.flatten() {
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > max_age);
            if old && entry.file_name().to_string_lossy().ends_with(".gcode.3mf") {
                std::fs::remove_file(entry.path()).ok();
            }
        }
    }
    let file_name: String = job_name(name)
        .chars()
        .map(|c| if r#"\/:*?"<>|"#.contains(c) { '_' } else { c })
        .collect();
    let target = dir.join(format!("{file_name}.gcode.3mf"));
    std::fs::copy(gcode_3mf, &target)?;
    let url = import_url(&target, name);
    Ok((target, url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_matches_the_bambu_wiki_example() {
        assert_eq!(
            import_url(Path::new("/tmp/cube.gcode.3mf"), "Cube"),
            "bambu-connect://import-file?path=%2Ftmp%2Fcube.gcode.3mf&name=Cube&version=1.0.0"
        );
    }

    #[test]
    fn windows_paths_and_unicode_names_are_encoded() {
        let url = import_url(
            Path::new(r"C:\Users\User\AppData\Local\Temp\slicenv\bambu-connect\Павук S.gcode.3mf"),
            "Павук & S+1",
        );
        assert!(url.starts_with("bambu-connect://import-file?path=C%3A%5CUsers%5CUser"), "{url}");
        assert!(url.contains("%D0%9F%D0%B0%D0%B2%D1%83%D0%BA%20S.gcode.3mf"), "{url}");
        assert!(url.ends_with("&name=%D0%9F%D0%B0%D0%B2%D1%83%D0%BA%20_%20S_1&version=1.0.0"), "{url}");
        assert_eq!(job_name("bowl.gcode.3mf"), "bowl");
        assert_eq!(job_name("  "), "Slicenv");
    }

    #[test]
    fn registry_command_gives_the_exe() {
        let out = "\r\nHKEY_CURRENT_USER\\Software\\Classes\\bambu-connect\\shell\\open\\command\r\n    (Default)    REG_SZ    \"C:\\Users\\User\\AppData\\Local\\Programs\\Bambu Connect\\Bambu Connect.exe\" \"%1\"\r\n";
        assert_eq!(
            command_exe(out),
            Some(PathBuf::from(r"C:\Users\User\AppData\Local\Programs\Bambu Connect\Bambu Connect.exe"))
        );
        assert_eq!(command_exe("ERROR: The system was unable to find"), None);
    }
}
