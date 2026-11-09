//! HTTPS-запити через системний `curl`, як у `cover.rs`: у застосунку немає власного
//! HTTP-стеку, а `curl` є на кожному Linux. URL, заголовки й тіло (пароль, токени) йдуть
//! конфігом через stdin (`-K -`), а не argv — їх не видно в списку процесів.
//!
//! [`Transport`] — межа для тестів: розбір відповідей перевіряється на моках без мережі.
use anyhow::{Context as _, Result, bail, ensure};
use std::{
    io::{Read as _, Write as _},
    process::Stdio,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Head,
}

pub struct Request<'a> {
    pub method: Method,
    pub url: &'a str,
    pub headers: &'a [(String, String)],
    pub body: Option<&'a str>,
    pub timeout_secs: u32,
    pub max_bytes: u64,
}

impl<'a> Request<'a> {
    pub fn get(url: &'a str, headers: &'a [(String, String)]) -> Self {
        Self {
            method: Method::Get,
            url,
            headers,
            body: None,
            timeout_secs: 20,
            max_bytes: 1024 * 1024,
        }
    }
    pub fn post(url: &'a str, headers: &'a [(String, String)], body: &'a str) -> Self {
        Self {
            method: Method::Post,
            body: Some(body),
            ..Self::get(url, headers)
        }
    }
}

pub struct Response {
    pub status: u16,
    /// Імена в нижньому регістрі.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
    /// Значення cookie з усіх `Set-Cookie`.
    pub fn cookie(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .filter(|(k, _)| k == "set-cookie")
            .find_map(|(_, v)| {
                let pair = v.split(';').next()?;
                let (k, value) = pair.split_once('=')?;
                (k.trim() == name).then(|| value.trim())
            })
    }
}

pub trait Transport: Send + Sync {
    fn request(&self, request: &Request<'_>) -> Result<Response>;
    /// Потокове завантаження в `sink`; `progress` отримує кількість отриманих байтів.
    fn download(
        &self,
        url: &str,
        headers: &[(String, String)],
        max_bytes: u64,
        sink: &mut dyn std::io::Write,
        progress: &mut dyn FnMut(u64),
        cancel: &AtomicBool,
    ) -> Result<u64>;
}

/// Справжній транспорт через `curl`.
pub struct Curl;

fn quote(value: &str) -> Result<String> {
    ensure!(
        !value.chars().any(|c| c.is_control()),
        "HTTP: недопустимі керівні символи в запиті"
    );
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

pub(crate) fn validate_url(url: &str) -> Result<()> {
    ensure!(
        url.starts_with("https://") && url.len() <= 4096,
        "HTTP: дозволено лише https-посилання"
    );
    ensure!(
        !url.bytes().any(|b| b.is_ascii_control() || b == b' '),
        "HTTP: недопустимі символи в посиланні"
    );
    Ok(())
}

fn config(
    url: &str,
    headers: &[(String, String)],
    body: Option<&str>,
    method: Option<Method>,
) -> Result<String> {
    validate_url(url)?;
    let mut cfg = format!("url = {}\n", quote(url)?);
    for (k, v) in headers {
        ensure!(
            !k.is_empty() && k.bytes().all(|b| b.is_ascii_graphic() && b != b':'),
            "HTTP: недопустиме ім’я заголовка"
        );
        cfg += &format!("header = {}\n", quote(&format!("{k}: {v}"))?);
    }
    match method {
        Some(Method::Head) => cfg += "head\n",
        Some(Method::Post) => cfg += "request = \"POST\"\n",
        _ => {}
    }
    if let Some(body) = body {
        cfg += &format!("data-binary = {}\n", quote(body)?);
    }
    Ok(cfg)
}

fn curl(args: &[&str], cfg: &str) -> Result<std::process::Child> {
    let mut child = paths::host::command("curl")
        .args([
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--connect-timeout",
            "15",
        ])
        .args(args)
        .args(["-K", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("Не вдалося запустити curl")?;
    let mut stdin = child.stdin.take().context("curl без stdin")?;
    stdin.write_all(cfg.as_bytes())?;
    drop(stdin);
    Ok(child)
}

/// Розбирає вивід `curl -i`: блоки заголовків (1xx пропускаються), далі тіло.
pub(crate) fn parse_included(raw: &[u8]) -> Result<Response> {
    let mut rest = raw;
    loop {
        let end = rest
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|i| (i, 4))
            .or_else(|| rest.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2)))
            .context("HTTP: відповідь без заголовків")?;
        let head = String::from_utf8_lossy(&rest[..end.0]).into_owned();
        rest = &rest[end.0 + end.1..];
        let mut lines = head.lines();
        let status: u16 = lines
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .context("HTTP: некоректний рядок стану")?;
        if (100..200).contains(&status) {
            continue;
        }
        let headers = lines
            .filter_map(|l| {
                let (k, v) = l.split_once(':')?;
                Some((k.trim().to_ascii_lowercase(), v.trim().to_owned()))
            })
            .collect();
        return Ok(Response {
            status,
            headers,
            body: rest.to_vec(),
        });
    }
}

impl Transport for Curl {
    fn request(&self, request: &Request<'_>) -> Result<Response> {
        let cfg = config(
            request.url,
            request.headers,
            request.body,
            Some(request.method),
        )?;
        let timeout = request.timeout_secs.to_string();
        let mut child = curl(&["--include", "--max-time", &timeout], &cfg)?;
        let mut raw = Vec::new();
        child
            .stdout
            .take()
            .context("curl без stdout")?
            .take(request.max_bytes + 64 * 1024)
            .read_to_end(&mut raw)?;
        let status = child.wait()?;
        if !status.success() {
            // Без URL і тіла: у них можуть бути секрети.
            bail!("Мережевий запит не вдався (код curl {:?})", status.code());
        }
        let response = parse_included(&raw)?;
        ensure!(
            response.body.len() as u64 <= request.max_bytes,
            "HTTP: відповідь завелика"
        );
        Ok(response)
    }

    fn download(
        &self,
        url: &str,
        headers: &[(String, String)],
        max_bytes: u64,
        sink: &mut dyn std::io::Write,
        progress: &mut dyn FnMut(u64),
        cancel: &AtomicBool,
    ) -> Result<u64> {
        let cfg = config(url, headers, None, None)?;
        let max = max_bytes.to_string();
        let mut child = curl(
            &[
                "--fail",
                "--location",
                "--max-redirs",
                "3",
                "--max-time",
                "900",
                "--max-filesize",
                &max,
            ],
            &cfg,
        )?;
        let mut out = child.stdout.take().context("curl без stdout")?;
        let mut buf = vec![0u8; 256 * 1024];
        let mut total = 0u64;
        let result = (|| -> Result<()> {
            loop {
                if cancel.load(Ordering::Relaxed) {
                    bail!("Завантаження скасовано");
                }
                let n = out.read(&mut buf)?;
                if n == 0 {
                    return Ok(());
                }
                total += n as u64;
                ensure!(total <= max_bytes, "Завантаження завелике");
                sink.write_all(&buf[..n])?;
                progress(total);
            }
        })();
        if result.is_err() {
            let _ = child.kill();
        }
        let status = child.wait()?;
        result?;
        if !status.success() {
            bail!("Завантаження не вдалося (код curl {:?})", status.code());
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_quotes_secrets_and_rejects_injection() {
        let cfg = config(
            "https://api.example.com/login",
            &[("Content-Type".into(), "application/json".into())],
            Some(r#"{"password":"a\"b\\c"}"#),
            Some(Method::Post),
        )
        .unwrap();
        assert!(cfg.contains(r#"data-binary = "{\"password\":\"a\\\"b\\\\c\"}""#));
        assert!(config("http://x.example/", &[], None, None).is_err());
        assert!(
            config(
                "https://x.example/",
                &[],
                Some("a\nurl = \"file:///\""),
                None
            )
            .is_err()
        );
        assert!(
            config(
                "https://x.example/",
                &[("X: y".into(), "z".into())],
                None,
                None
            )
            .is_err()
        );
    }
    #[test]
    fn included_headers_skip_continue_and_read_cookies() {
        let raw = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/2 200 \r\ncontent-type: application/json\r\nSet-Cookie: token=abc.def; Path=/; HttpOnly\r\nset-cookie: other=1\r\n\r\n{\"ok\":1}";
        let r = parse_included(raw).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.header("content-type"), Some("application/json"));
        assert_eq!(r.cookie("token"), Some("abc.def"));
        assert_eq!(r.cookie("missing"), None);
        assert_eq!(r.body, b"{\"ok\":1}");
    }
}
