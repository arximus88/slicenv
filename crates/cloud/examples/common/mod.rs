//! Приватний вивід і відновлення сесії для прикладів лише читання.
use anyhow::{Result, ensure};
use cloud::{Client, DeviceId, Events, Options};
use std::{
    fs::File,
    time::{Duration, Instant},
};

#[cfg(unix)]
pub fn private_output() -> Result<File> {
    use std::{
        fs::OpenOptions,
        os::fd::{AsRawFd, FromRawFd},
    };
    let null = OpenOptions::new().write(true).open("/dev/null")?;
    // Окремий fd для перевірених повідомлень; native stdout/stderr приховано.
    unsafe {
        let saved = libc::dup(libc::STDOUT_FILENO);
        ensure!(saved >= 0, "Не вдалося створити приватний вивід");
        let output = File::from_raw_fd(saved);
        ensure!(
            libc::dup2(null.as_raw_fd(), 1) >= 0 && libc::dup2(null.as_raw_fd(), 2) >= 0,
            "Не вдалося приховати native-вивід"
        );
        Ok(output)
    }
}

/// Windows: плагін з динамічним CRT пише в ті самі дескриптори CRT 1 і 2.
#[cfg(windows)]
pub fn private_output() -> Result<File> {
    use std::os::windows::io::FromRawHandle as _;
    unsafe {
        let saved = libc::dup(1);
        let null = libc::open(c"NUL".as_ptr(), libc::O_WRONLY);
        ensure!(
            saved >= 0 && null >= 0 && libc::dup2(null, 1) == 0 && libc::dup2(null, 2) == 0,
            "Не вдалося приховати native-вивід"
        );
        let handle = libc::get_osfhandle(saved);
        ensure!(handle != -1, "Не вдалося створити приватний вивід");
        Ok(File::from_raw_handle(handle as _))
    }
}
pub fn session() -> Result<(Client, Events)> {
    // Приклади однопотокові на цей момент.
    unsafe { cloud::configure_tls_env() };
    let mut options = if let Some(path) = std::env::var_os("BAMBU_STUDIO_CONFIG") {
        Options::from_studio_dir(std::path::PathBuf::from(path))?
    } else {
        Options::from_bambu_studio()?
    };
    if let Some(path) = std::env::var_os("BAMBU_PLUGIN") {
        options.plugin_path = path.into();
    }
    if let Ok(country) = std::env::var("BAMBU_COUNTRY") {
        options.country_code = country;
    }
    if let Some(cert) = std::env::var_os("BAMBU_CERTIFICATE") {
        options.certificate = Some(cert.into());
    }
    let (client, events) = Client::load(options)?;
    client.start()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !client.is_user_login()? && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    ensure!(
        client.is_user_login()?,
        "Немає сесії. Увійдіть у Bambu Studio на цьому комп’ютері та повторіть запуск"
    );
    Ok((client, events))
}
pub fn device_override() -> Option<DeviceId> {
    std::env::var("BAMBU_DEV_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .map(DeviceId::new)
}
