//! Завантаження обкладинки завдання за підписаним URL.
//!
//! Плагін Bambu віддає лише URL (як і в Studio, де картинку тягне власний HTTP-стек Studio);
//! завантаження байтів у ABI плагіна немає. Тому запит робить системний `curl`: URL іде через
//! stdin (`-K -`), а не argv, щоб підпис не потрапив у список процесів.
use anyhow::{Context as _, Result, bail, ensure};
use std::{
    io::{Read as _, Write as _},
    process::Stdio,
};

const MAX_BYTES: usize = 8 * 1024 * 1024;

fn validate(url: &str) -> Result<()> {
    ensure!(
        url.starts_with("https://") && url.len() <= 4096,
        "Обкладинка: дозволено лише https-посилання"
    );
    ensure!(
        !url.bytes()
            .any(|b| b.is_ascii_control() || b == b'"' || b == b'\\' || b == b' '),
        "Обкладинка: недопустимі символи в посиланні"
    );
    Ok(())
}

/// Блокує потік до 20 с; повертає байти зображення (PNG/JPEG) до 8 МБ. Помилки не містять URL.
pub fn download_cover(url: &str) -> Result<Vec<u8>> {
    validate(url)?;
    let mut child = paths::host::command("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-redirs",
            "3",
            "--max-time",
            "20",
            "--max-filesize",
            "8388608",
            "-K",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("Не вдалося запустити curl")?;
    child
        .stdin
        .take()
        .context("curl без stdin")?
        .write_all(format!("url = \"{url}\"\n").as_bytes())?;
    let mut body = Vec::new();
    child
        .stdout
        .take()
        .context("curl без stdout")?
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut body)?;
    let status = child.wait()?;
    if !status.success() {
        bail!(
            "Не вдалося завантажити обкладинку (код curl {:?})",
            status.code()
        );
    }
    ensure!(body.len() <= MAX_BYTES, "Обкладинка завелика");
    ensure!(!body.is_empty(), "Порожня відповідь обкладинки");
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_https_urls_are_accepted_and_nothing_is_fetched_otherwise() {
        for bad in [
            "http://example.com/a.png",
            "file:///etc/passwd",
            "ftp://x/y",
            "https://x/\"\nurl = \"file:///etc/passwd",
            "https://x/a b",
            "",
        ] {
            let err = download_cover(bad).unwrap_err().to_string();
            assert!(
                !err.contains("passwd"),
                "помилка не повинна повторювати URL"
            );
        }
        assert!(validate("https://example.com/a.png?sig=abc").is_ok());
    }
}
