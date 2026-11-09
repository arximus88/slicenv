//! Відомості про ОС для заголовків Bambu і прибирання осиротілих копій сесії: Linux читає
//! `/proc`, Windows — Win32 API напряму (kernel32/ntdll/ole32, без додаткових крейтів).

/// Версія ядра/ОС для `X-BBL-OS-Version`: «6.12.1» на Linux, «10.0.26200» на Windows.
pub(crate) fn version() -> Option<String> {
    imp::version()
}

/// Новий випадковий UUID у вигляді `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`.
pub(crate) fn random_uuid() -> Option<String> {
    imp::random_uuid()
}

/// Чи живий процес; `None` — ОС не дає перевірити (тоді нічого не видаляємо).
pub(crate) fn process_alive(pid: u32) -> Option<bool> {
    imp::process_alive(pid)
}

#[cfg(target_os = "linux")]
mod imp {
    use std::{fs, path::Path};

    pub fn version() -> Option<String> {
        let release = fs::read_to_string("/proc/sys/kernel/osrelease").ok()?;
        let nums: Vec<&str> = release
            .trim()
            .split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .take(3)
            .collect();
        (!nums.is_empty()).then(|| nums.join("."))
    }

    pub fn random_uuid() -> Option<String> {
        Some(
            fs::read_to_string("/proc/sys/kernel/random/uuid")
                .ok()?
                .trim()
                .to_owned(),
        )
    }

    pub fn process_alive(pid: u32) -> Option<bool> {
        Some(Path::new(&format!("/proc/{pid}")).exists())
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    #[repr(C)]
    struct OsVersionInfo {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        service_pack: [u16; 128],
    }
    #[repr(C)]
    #[derive(Default)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn RtlGetVersion(info: *mut OsVersionInfo) -> i32;
    }
    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoCreateGuid(guid: *mut Guid) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn GetExitCodeProcess(process: *mut c_void, code: *mut u32) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn GetLastError() -> u32;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    const ERROR_INVALID_PARAMETER: u32 = 87;

    pub fn version() -> Option<String> {
        let mut info = OsVersionInfo {
            size: size_of::<OsVersionInfo>() as u32,
            major: 0,
            minor: 0,
            build: 0,
            platform: 0,
            service_pack: [0; 128],
        };
        // SAFETY: структура OSVERSIONINFOW з правильним dwOSVersionInfoSize; RtlGetVersion
        // (на відміну від GetVersionEx) не бреше про версію без маніфесту сумісності.
        (unsafe { RtlGetVersion(&mut info) } == 0)
            .then(|| format!("{}.{}.{}", info.major, info.minor, info.build))
    }

    pub fn random_uuid() -> Option<String> {
        let mut g = Guid::default();
        // SAFETY: CoCreateGuid лише заповнює GUID; COM ініціалізувати не потрібно.
        if unsafe { CoCreateGuid(&mut g) } != 0 {
            return None;
        }
        let d = g.data4;
        Some(format!(
            "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            g.data1, g.data2, g.data3, d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]
        ))
    }

    pub fn process_alive(pid: u32) -> Option<bool> {
        // SAFETY: прості виклики Win32; дескриптор закривається.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                // Немає такого PID — процес завершився. Інша помилка (доступ) — невідомо.
                return (GetLastError() == ERROR_INVALID_PARAMETER).then_some(false);
            }
            let mut code = 0;
            let ok = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            (ok != 0).then_some(code == STILL_ACTIVE)
        }
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    pub fn version() -> Option<String> {
        None
    }
    pub fn random_uuid() -> Option<String> {
        None
    }
    pub fn process_alive(_: u32) -> Option<bool> {
        None
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn os_facts_are_well_formed() {
        if cfg!(any(target_os = "linux", windows)) {
            let version = super::version().expect("версія ОС");
            assert!(
                version.split('.').all(|n| n.parse::<u32>().is_ok()),
                "{version}"
            );
            let id = super::random_uuid().expect("uuid");
            assert_eq!(id.len(), 36, "{id}");
            assert_ne!(id, super::random_uuid().unwrap());
            assert_eq!(super::process_alive(std::process::id()), Some(true));
        }
    }
}
