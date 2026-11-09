//! `slicenv-mcp` — MCP-сервер через stdio. Лише локально: жодного мережевого слухача.

use anyhow::{Context as _, Result};
use mcp::Server;
use rmcp::ServiceExt as _;

const HELP: &str = "slicenv-mcp [--allow-edit]\n\n\
Сервер MCP (stdio) для проєктів і слайсера Slicenv.\n\
  --allow-edit  дозволити зміну проєкту (transform_object, arrange_plate, orient_plate, undo/redo, save_project)\n\
  --version     показати версію і вийти\n\
Без прапорця сервер лише читає.";

/// Нативний плагін Bambu пише в stdout/stderr напряму й може зіпсувати протокол або витекти
/// секрети. Беремо для протоколу приватну копію fd 1, а самі fd 1 і 2 відправляємо в /dev/null.
#[cfg(unix)]
fn take_protocol_stdout() -> Result<std::fs::File> {
    use std::os::fd::FromRawFd as _;
    let null = std::fs::OpenOptions::new().write(true).open("/dev/null")?;
    let (saved, ok) = unsafe {
        use std::os::fd::AsRawFd as _;
        let saved = libc::dup(libc::STDOUT_FILENO);
        let ok = saved >= 0
            && libc::dup2(null.as_raw_fd(), libc::STDOUT_FILENO) >= 0
            && libc::dup2(null.as_raw_fd(), libc::STDERR_FILENO) >= 0;
        (saved, ok)
    };
    anyhow::ensure!(
        ok,
        "не вдалося ізолювати stdout: {}",
        std::io::Error::last_os_error()
    );
    Ok(unsafe { std::fs::File::from_raw_fd(saved) })
}

/// Те саме на Windows: плагін зібрано з динамічним CRT (ucrtbase), тож його printf пише в ті
/// самі дескриптори CRT 1 і 2; `_dup2` для 0–2 ще й міняє `SetStdHandle` консольного процесу.
#[cfg(windows)]
fn take_protocol_stdout() -> Result<std::fs::File> {
    use std::os::windows::io::FromRawHandle as _;
    // SAFETY: функції CRT над власними дескрипторами; рантайм ще однопотоковий.
    let (saved, ok) = unsafe {
        let saved = libc::dup(1);
        let null = libc::open(c"NUL".as_ptr(), libc::O_WRONLY);
        let ok = saved >= 0 && null >= 0 && libc::dup2(null, 1) == 0 && libc::dup2(null, 2) == 0;
        (saved, ok)
    };
    anyhow::ensure!(
        ok,
        "не вдалося ізолювати stdout: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: дескриптор CRT `saved` більше ніде не використовується: дескриптор ОС під ним
    // переходить у власність File.
    let handle = unsafe { libc::get_osfhandle(saved) };
    anyhow::ensure!(handle != -1, "не вдалося ізолювати stdout");
    Ok(unsafe { std::fs::File::from_raw_handle(handle as _) })
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut allow_edit = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--allow-edit" => allow_edit = true,
            "-h" | "--help" => return Ok(println!("{HELP}")),
            "-V" | "--version" => {
                return Ok(println!("slicenv-mcp {}", env!("CARGO_PKG_VERSION")));
            }
            other => anyhow::bail!("невідомий аргумент {other}\n\n{HELP}"),
        }
    }
    // Однопотоковий старт (рантайм ще не розгорнув потоки обробників): безпечно для оточення.
    unsafe { cloud::configure_tls_env() };
    #[cfg(any(unix, windows))]
    let out = tokio::fs::File::from_std(take_protocol_stdout()?);
    #[cfg(not(any(unix, windows)))]
    let out = tokio::io::stdout();
    let service = Server::new(allow_edit)
        .serve((tokio::io::stdin(), out))
        .await
        .context("не вдалося запустити MCP-сервер")?;
    service.waiting().await?;
    Ok(())
}
