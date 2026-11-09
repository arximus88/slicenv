//! Права доступу Unix (0600/0700), `O_NOFOLLOW`, власник файлу й симлінки — одним API для
//! всіх ОС. На Unix це тонкі обгортки над `std::os::unix`; на Windows режимів немає (доступ
//! визначає ACL профілю користувача, `%APPDATA%`/`%LOCALAPPDATA%` і так приватні), тому
//! виклики стають no-op, а перевірки — «так».
use std::{fs, io, path::Path};

/// `.unix_mode(0o600)` для `OpenOptions` і `DirBuilder`; на Windows нічого не робить.
pub trait ModeExt {
    fn unix_mode(&mut self, mode: u32) -> &mut Self;
}

impl ModeExt for fs::OpenOptions {
    #[allow(unused_variables)]
    fn unix_mode(&mut self, mode: u32) -> &mut Self {
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(self, mode);
        self
    }
}

impl ModeExt for fs::DirBuilder {
    #[allow(unused_variables)]
    fn unix_mode(&mut self, mode: u32) -> &mut Self {
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(self, mode);
        self
    }
}

/// Не йти за симлінком в останньому компоненті шляху (`O_NOFOLLOW`). На Windows —
/// `FILE_FLAG_OPEN_REPARSE_POINT`: відкривається сама точка повторного розбору, не ціль.
pub trait NoFollowExt {
    fn no_follow(&mut self) -> &mut Self;
}

impl NoFollowExt for fs::OpenOptions {
    fn no_follow(&mut self) -> &mut Self {
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::custom_flags(self, libc::O_NOFOLLOW);
        #[cfg(windows)]
        std::os::windows::fs::OpenOptionsExt::custom_flags(self, 0x0020_0000);
        self
    }
}

/// `chmod`; на Windows — no-op.
#[allow(unused_variables)]
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

/// `fchmod` відкритого файлу; на Windows — no-op.
#[allow(unused_variables)]
pub fn set_file_mode(file: &fs::File, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

/// Біти режиму (`& 0o7777`); `None` на Windows.
#[allow(unused_variables)]
pub fn mode(metadata: &fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        Some(metadata.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    None
}

/// Чи належить файл поточному користувачеві. На Windows завжди `true`: власника перевіряє
/// ACL, а файли в профілі користувача й так недоступні іншим.
#[allow(unused_variables)]
pub fn owned_by_current_user(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        // SAFETY: getuid не має передумов і не може завершитися помилкою.
        metadata.uid() == unsafe { libc::getuid() }
    }
    #[cfg(not(unix))]
    true
}

/// Виконуваний файл: на Unix — біт `x` у будь-кого, на Windows — розширення `.exe`/`.bat`/`.cmd`.
pub fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["exe", "bat", "cmd", "com"].iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// Симлінк із тим самим значенням `target`. На Windows обирає файловий чи каталожний
/// варіант за ціллю (без цілі — файловий); потребує Developer Mode або прав адміністратора.
pub fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    #[cfg(unix)]
    return std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    {
        let resolved = link.parent().map_or(target.to_path_buf(), |p| p.join(target));
        if resolved.is_dir() {
            std::os::windows::fs::symlink_dir(target, link)
        } else {
            std::os::windows::fs::symlink_file(target, link)
        }
    }
    #[cfg(not(any(unix, windows)))]
    Err(io::Error::new(io::ErrorKind::Unsupported, "symlink"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn modes_apply_on_unix() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("private");
        fs::DirBuilder::new().unix_mode(0o700).create(&sub).unwrap();
        assert_eq!(mode(&fs::metadata(&sub).unwrap()), Some(0o700));
        let file = sub.join("token");
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .unix_mode(0o600)
            .open(&file)
            .unwrap();
        assert_eq!(mode(&fs::metadata(&file).unwrap()), Some(0o600));
        set_mode(&file, 0o644).unwrap();
        assert_eq!(mode(&fs::metadata(&file).unwrap()), Some(0o644));
        assert!(owned_by_current_user(&fs::metadata(&file).unwrap()));
        assert!(!is_executable(&file));
        set_mode(&file, 0o755).unwrap();
        assert!(is_executable(&file));
    }

    #[test]
    fn no_follow_rejects_symlink() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("target"), "x").unwrap();
        symlink(Path::new("target"), &dir.path().join("link")).unwrap();
        assert!(
            fs::OpenOptions::new()
                .read(true)
                .no_follow()
                .open(dir.path().join("link"))
                .is_err()
        );
    }
}
