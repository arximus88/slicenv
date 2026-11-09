//! Product-owned environment compatibility and one-time XDG directory migration.
use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
};

/// New names win, including an explicitly empty value. Vendor variables pass through.
pub fn env(name: &str) -> Option<OsString> {
    env_with(name, |key| std::env::var_os(key))
}

fn env_with(name: &str, read: impl Fn(&str) -> Option<OsString>) -> Option<OsString> {
    read(name).or_else(|| {
        name.strip_prefix("SLICENV_")
            .and_then(|suffix| read(&format!("BAMBU_{suffix}")))
    })
}

/// Run before any component reads or creates product directories. Existing destinations
/// are authoritative: never merge into them or remove the legacy directory in that case.
pub fn migrate_xdg() {
    // Стара назва `bambu-lab` існувала лише в XDG-каталогах Linux; на macOS/Windows корені
    // ще й збігаються між собою, тож міграція там не потрібна й небезпечна.
    if cfg!(any(target_os = "macos", windows)) {
        return;
    }
    let Some(_) = std::env::var_os("HOME") else {
        return;
    };
    for root in [config_home(), data_home(), cache_home(), state_home()] {
        match migrate_dir(&root.join("bambu-lab"), &root.join(PRODUCT)) {
            Ok(true) => eprintln!(
                "Slicenv: migrated {} to {}",
                root.join("bambu-lab").display(),
                root.join(PRODUCT).display()
            ),
            Ok(false) => {}
            Err(error) => eprintln!("Slicenv: migration of {} failed: {error}", root.display()),
        }
    }
}

pub fn migrate_dir(old: &Path, new: &Path) -> io::Result<bool> {
    migrate_with(old, new, |old, new| fs::rename(old, new))
}

fn migrate_with(
    old: &Path,
    new: &Path,
    rename: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<bool> {
    if fs::symlink_metadata(new).is_ok() || !old.try_exists()? {
        return Ok(false);
    }
    match rename(old, new) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
            // Copy to a private staging directory first. A failed copy leaves the source
            // intact and never exposes a partial destination as a completed migration.
            let staging = tempfile::Builder::new()
                .prefix(".slicenv-migration-")
                .tempdir_in(
                    new.parent()
                        .ok_or_else(|| io::Error::other("missing parent"))?,
                )?;
            let copied = staging.path().join("data");
            copy_tree(old, &copied)?;
            if fs::symlink_metadata(new).is_ok() {
                return Ok(false);
            }
            fs::rename(&copied, new)?;
            fs::remove_dir_all(old)?;
            Ok(true)
        }
        Err(error) => Err(error),
    }
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(from)?;
    if metadata.file_type().is_symlink() {
        fs_mode::symlink(&fs::read_link(from)?, to)
    } else if metadata.is_dir() {
        fs::create_dir(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        fs::set_permissions(to, metadata.permissions())
    } else if metadata.is_file() {
        fs::copy(from, to).map(|_| ())
    } else {
        Err(io::Error::other("unsupported entry in legacy directory"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn environment_precedence_and_vendor_passthrough() {
        let read = |key: &str| match key {
            "SLICENV_DEBUG" => Some("".into()),
            "BAMBU_DEBUG" | "BAMBU_PROFILE" | "BAMBU_STUDIO" => Some("legacy".into()),
            _ => None,
        };
        assert_eq!(env_with("SLICENV_DEBUG", read), Some("".into()));
        assert_eq!(env_with("SLICENV_PROFILE", read), Some("legacy".into()));
        assert_eq!(env_with("BAMBU_STUDIO", read), Some("legacy".into()));
        assert_eq!(env_with("SLICENV_MISSING", read), None);
    }
    #[test]
    fn moves_once_and_preserves_existing_destination() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("old");
        let new = root.path().join("new");
        assert!(!migrate_dir(&old, &new).unwrap());
        fs::create_dir(&old).unwrap();
        fs::write(old.join("mcp-token"), "secret").unwrap();
        assert!(migrate_dir(&old, &new).unwrap());
        assert!(!old.exists());
        assert_eq!(fs::read_to_string(new.join("mcp-token")).unwrap(), "secret");
        assert!(!migrate_dir(&old, &new).unwrap());
        fs::create_dir(&old).unwrap();
        fs::write(old.join("keep"), "keep").unwrap();
        assert!(!migrate_dir(&old, &new).unwrap());
        assert!(old.join("keep").exists());
    }
    #[test]
    #[cfg(unix)]
    fn cross_device_copy_preserves_contents_permissions_and_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("old");
        let new = root.path().join("new");
        fs::create_dir_all(old.join("nested")).unwrap();
        fs::write(old.join("nested/token"), "secret").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(old.join("nested/token"), fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink("nested/token", old.join("link")).unwrap();
        assert!(
            migrate_with(&old, &new, |_, _| Err(io::Error::from(
                io::ErrorKind::CrossesDevices
            )))
            .unwrap()
        );
        assert!(!old.exists());
        assert_eq!(fs::read_to_string(new.join("link")).unwrap(), "secret");
        assert_eq!(
            fs::metadata(new.join("nested/token"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    #[test]
    #[cfg(unix)]
    fn failed_copy_leaves_source_and_no_partial_destination() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("old");
        let new = root.path().join("new");
        fs::create_dir(&old).unwrap();
        fs::write(old.join("keep"), "keep").unwrap();
        let _socket = std::os::unix::net::UnixListener::bind(old.join("socket")).unwrap();
        assert!(
            migrate_with(&old, &new, |_, _| Err(io::Error::from(
                io::ErrorKind::CrossesDevices
            )))
            .is_err()
        );
        assert_eq!(fs::read_to_string(old.join("keep")).unwrap(), "keep");
        assert!(!new.exists());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }
    #[test]
    #[cfg(unix)]
    fn failures_preserve_source() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("old");
        let new = root.path().join("new");
        fs::create_dir(&old).unwrap();
        fs::write(old.join("keep"), "keep").unwrap();
        assert!(
            migrate_with(&old, &new, |_, _| Err(io::Error::from(
                io::ErrorKind::PermissionDenied
            )))
            .is_err()
        );
        assert!(old.join("keep").exists());
        assert!(!new.exists());
        // A missing symlink target must be copied as a link, never followed.
        std::os::unix::fs::symlink("missing", old.join("broken")).unwrap();
        assert!(
            migrate_with(&old, &new, |_, _| Err(io::Error::from(
                io::ErrorKind::CrossesDevices
            )))
            .unwrap()
        );
        assert_eq!(
            fs::read_link(new.join("broken")).unwrap(),
            Path::new("missing")
        );
    }
}

/// Product directory name shared by all runtime crates.
pub const PRODUCT: &str = "slicenv";
pub mod fs_mode;
pub mod host;
pub mod profile;

fn xdg_home(value: Option<OsString>, home: PathBuf, fallback: &str) -> PathBuf {
    value
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(fallback))
}

/// Домашній каталог користувача: `HOME` на Unix, `USERPROFILE` на Windows (`HOME` — запасний,
/// його виставляють MSYS/Git Bash). Порожній шлях, якщо не задано нічого.
pub fn home_dir() -> PathBuf {
    let read = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    #[cfg(windows)]
    let home = read("USERPROFILE").or_else(|| read("HOME"));
    #[cfg(not(windows))]
    let home = read("HOME");
    home.map(PathBuf::from).unwrap_or_default()
}

/// Абсолютне значення змінної оточення (`XDG_*` на Linux, `%APPDATA%`/`%LOCALAPPDATA%` на
/// Windows), інакше `fallback` від домашнього каталогу.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn home(variable: &str, fallback: &str) -> PathBuf {
    xdg_home(std::env::var_os(variable), home_dir(), fallback)
}

// Корені каталогів за ОС. `config_home()` — там, де настільні застосунки тримають свої
// налаштування; саме туди Bambu Studio й OrcaSlicer кладуть `data_dir` (`GUI_App::init_app_config`:
// `wxStandardPaths::GetUserDataDir()` на Windows/macOS, `$XDG_CONFIG_HOME` на Linux), тому
// `config_home().join("BambuStudio")` правильний на всіх трьох ОС (див. docs/PORTING.md).
//
// | корінь        | Linux / BSD          | macOS                          | Windows          |
// |---------------|----------------------|--------------------------------|------------------|
// | config_home   | $XDG_CONFIG_HOME     | ~/Library/Application Support  | %APPDATA%        |
// | data_home     | $XDG_DATA_HOME       | ~/Library/Application Support  | %LOCALAPPDATA%   |
// | cache_home    | $XDG_CACHE_HOME      | ~/Library/Caches               | %LOCALAPPDATA%   |
// | state_home    | $XDG_STATE_HOME      | ~/Library/Application Support  | %LOCALAPPDATA%   |

#[cfg(target_os = "macos")]
fn mac_library(sub: &str) -> PathBuf {
    home_dir().join("Library").join(sub)
}

pub fn config_home() -> PathBuf {
    #[cfg(target_os = "macos")]
    return mac_library("Application Support");
    #[cfg(windows)]
    return home("APPDATA", "AppData\\Roaming");
    #[cfg(not(any(target_os = "macos", windows)))]
    return home("XDG_CONFIG_HOME", ".config");
}
pub fn data_home() -> PathBuf {
    #[cfg(target_os = "macos")]
    return mac_library("Application Support");
    #[cfg(windows)]
    return home("LOCALAPPDATA", "AppData\\Local");
    #[cfg(not(any(target_os = "macos", windows)))]
    return home("XDG_DATA_HOME", ".local/share");
}
pub fn cache_home() -> PathBuf {
    #[cfg(target_os = "macos")]
    return mac_library("Caches");
    #[cfg(windows)]
    return home("LOCALAPPDATA", "AppData\\Local");
    #[cfg(not(any(target_os = "macos", windows)))]
    return home("XDG_CACHE_HOME", ".cache");
}
pub fn state_home() -> PathBuf {
    #[cfg(target_os = "macos")]
    return mac_library("Application Support");
    #[cfg(windows)]
    return home("LOCALAPPDATA", "AppData\\Local");
    #[cfg(not(any(target_os = "macos", windows)))]
    return home("XDG_STATE_HOME", ".local/state");
}
/// Корінь налаштувань сторонніх слайсерів (Bambu Studio, OrcaSlicer). Зазвичай це
/// [`config_home`]. У Flatpak `XDG_CONFIG_HOME` веде в `~/.var/app/<id>/config`, а маніфест
/// відкриває хостові `xdg-config/BambuStudio` і `xdg-config/OrcaSlicer` лише для читання —
/// тож беремо хостовий корінь (`HOST_XDG_CONFIG_HOME`, інакше `~/.config`).
pub fn vendor_config_home() -> PathBuf {
    vendor_config_home_with(|k| std::env::var_os(k), config_home, home_dir())
}

fn vendor_config_home_with(
    read: impl Fn(&str) -> Option<OsString>,
    config_home: impl FnOnce() -> PathBuf,
    home: PathBuf,
) -> PathBuf {
    if cfg!(not(any(target_os = "macos", windows))) && read("FLATPAK_ID").is_some() {
        return xdg_home(read("HOST_XDG_CONFIG_HOME"), home, ".config");
    }
    config_home()
}

/// Каталог налаштувань стороннього застосунку (`BambuStudio`, `OrcaSlicer`) — його `data_dir`.
pub fn vendor_config_dir(app: &str) -> PathBuf {
    vendor_config_home().join(app)
}
pub fn config_dir() -> PathBuf {
    config_home().join(PRODUCT)
}
pub fn data_dir() -> PathBuf {
    data_home().join(PRODUCT)
}
/// На Windows кеш і дані ділять `%LOCALAPPDATA%\slicenv`, тож кеш — у підкаталозі `cache`.
pub fn cache_dir() -> PathBuf {
    #[cfg(windows)]
    return cache_home().join(PRODUCT).join("cache");
    #[cfg(not(windows))]
    return cache_home().join(PRODUCT);
}
/// На macOS і Windows корінь стану збігається з даними, тож стан — у підкаталозі `state`.
pub fn state_dir() -> PathBuf {
    #[cfg(any(target_os = "macos", windows))]
    return state_home().join(PRODUCT).join("state");
    #[cfg(not(any(target_os = "macos", windows)))]
    return state_home().join(PRODUCT);
}

#[cfg(test)]
mod xdg_tests {
    use super::*;
    /// Те саме, що Bambu Studio / OrcaSlicer: `GetUserDataDir()` на macOS/Windows, XDG на Linux.
    #[test]
    fn vendor_dirs_match_upstream_data_dir() {
        let bambu = vendor_config_dir("BambuStudio");
        assert!(bambu.ends_with("BambuStudio"), "{}", bambu.display());
        #[cfg(target_os = "macos")]
        {
            assert!(bambu.ends_with("Library/Application Support/BambuStudio"));
            assert!(cache_dir().ends_with("Library/Caches/slicenv"));
        }
        #[cfg(windows)]
        {
            if let Some(appdata) = std::env::var_os("APPDATA") {
                assert_eq!(bambu, PathBuf::from(appdata).join("BambuStudio"));
            }
            assert!(cache_dir().ends_with("slicenv\\cache"));
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        assert_eq!(bambu, config_home().join("BambuStudio"));
    }
    #[test]
    #[cfg(not(any(target_os = "macos", windows)))]
    fn flatpak_reads_vendor_configs_from_host() {
        let own = || PathBuf::from("/home/test/.var/app/app.borys.slicenv/config");
        let at = |pairs: &'static [(&'static str, &'static str)]| {
            let read = move |k: &str| {
                pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v))
            };
            vendor_config_home_with(read, own, "/home/test".into())
        };
        const ID: (&str, &str) = ("FLATPAK_ID", "app.borys.slicenv");
        assert_eq!(at(&[]), own());
        assert_eq!(at(&[ID]), PathBuf::from("/home/test/.config"));
        assert_eq!(
            at(&[ID, ("HOST_XDG_CONFIG_HOME", "/srv/cfg")]),
            PathBuf::from("/srv/cfg")
        );
    }
    #[test]
    #[cfg(unix)]
    fn empty_relative_and_missing_values_use_home_fallback() {
        for fallback in [".config", ".local/share", ".cache", ".local/state"] {
            for value in [
                None,
                Some(""),
                Some("relative"),
                Some("./config"),
                Some("~/config"),
            ] {
                assert_eq!(
                    xdg_home(value.map(OsString::from), "/home/test".into(), fallback),
                    PathBuf::from("/home/test").join(fallback)
                );
            }
            assert_eq!(
                xdg_home(Some("/tmp/xdg".into()), "/home/test".into(), fallback),
                PathBuf::from("/tmp/xdg")
            );
        }
    }
}
