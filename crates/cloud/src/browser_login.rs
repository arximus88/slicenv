//! Вхід через системний браузер (Google, Apple, email) без вбудованого WebView.
//!
//! Як у Bambu Studio/Orca, але сторінку відкриває системний браузер. Посилання будує сам сайт
//! bambulab.com для слайсера (кнопки Google/Apple на сторінці входу, `thirdparty_login`):
//! `/sign-in?from=studio&source=portal&to=<…/sign-in/callback?…&redirect_url=http://localhost:<порт>&slicerLoginType=ticket>`.
//! Після входу сторінка `/sign-in/callback` (функція `slicerLogin` у публічному JS сайту)
//! перевіряє `redirect_url` на `^http://localhost:\d+$`, бере короткий квиток і переходить на
//! `http://localhost:<порт>/?ticket=…&redirect_url=…`. Квиток обмінюється на токени відкритим
//! `POST /v1/user-service/user/ticket/<квиток>` ([`crate::account::Login::ticket`]) — без
//! жодного секрету клієнта. Токени в URL не передаються (режим `slicerLoginType=ticket`).
//!
//! Тут — лише посилання й локальний сервер на випадковому порту `127.0.0.1`, що чекає один
//! такий запит. Квиток не логувати.
use anyhow::{Context as _, Result};
use std::{
    io::{Read as _, Write as _},
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

/// Посилання на офіційну сторінку входу Bambu, яка після входу поверне квиток на `port`.
pub fn sign_in_url(country_code: &str, port: u16, locale: &str) -> String {
    let web = web_host(country_code);
    let callback = format!(
        "{web}/sign-in/callback?{}",
        query(&[
            ("openBy", "studio"),
            ("from", "studio"),
            ("source", "portal"),
            ("locale", locale),
            ("redirect_url", &format!("http://localhost:{port}")),
            ("slicerLoginType", "ticket"),
        ])
    );
    format!(
        "{web}/sign-in?{}",
        query(&[("from", "studio"), ("source", "portal"), ("to", &callback)])
    )
}

fn web_host(country_code: &str) -> &'static str {
    if country_code == "CN" {
        "https://bambulab.cn"
    } else {
        "https://bambulab.com"
    }
}

fn query(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Як `URLSearchParams`/`encodeURIComponent`: усе, крім `A-Za-z0-9-_.~`, — `%XX`.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn decode(s: &str) -> Option<String> {
    let mut out = Vec::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        match b {
            b'%' => {
                let hi = (bytes.next()? as char).to_digit(16)?;
                let lo = (bytes.next()? as char).to_digit(16)?;
                out.push((hi * 16 + lo) as u8);
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
    }
    String::from_utf8(out).ok()
}

/// Квиток Bambu — коротке слово з латинських літер і цифр (сайт: ~6 символів).
pub fn valid_ticket(ticket: &str) -> bool {
    (1..=256).contains(&ticket.len())
        && ticket
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Розбір рядка запиту `GET /?ticket=…&redirect_url=… HTTP/1.1`: квиток, якщо запит — саме
/// повернення з сайту Bambu (`redirect_url` веде на bambulab.com/.cn або відсутній).
pub fn parse_callback(request_line: &str) -> Option<String> {
    let mut parts = request_line.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    let (path, query) = target.split_once('?')?;
    if path != "/" {
        return None;
    }
    let mut ticket = None;
    let mut redirect = None;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k {
            "ticket" => ticket = Some(decode(v)?),
            "redirect_url" => redirect = Some(decode(v)?),
            _ => {}
        }
    }
    if let Some(redirect) = redirect.filter(|r| !r.is_empty()) {
        let host = redirect
            .strip_prefix("https://")?
            .split(['/', '?', '#'])
            .next()?;
        let trusted = ["bambulab.com", "bambulab.cn"]
            .iter()
            .any(|d| host == *d || host.ends_with(&format!(".{d}")));
        if !trusted {
            return None;
        }
    }
    ticket.filter(|t| valid_ticket(t))
}

/// Чому вхід через браузер не завершився.
#[derive(Debug, PartialEq, Eq)]
pub enum WaitError {
    TimedOut,
    Cancelled,
    Io(String),
}

impl std::fmt::Display for WaitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TimedOut => f.write_str("Час на вхід у браузері минув — спробуйте ще раз"),
            Self::Cancelled => f.write_str("Вхід через браузер скасовано"),
            Self::Io(e) => write!(f, "Локальний сервер входу: {e}"),
        }
    }
}
impl std::error::Error for WaitError {}

/// Тексти сторінки в браузері після повернення (мовою застосунку).
pub struct Page<'a> {
    pub title: &'a str,
    pub message: &'a str,
}

/// Локальний сервер на випадковому порту. Слухає `127.0.0.1` і, якщо можна, `::1` на тому
/// самому порту: браузер може розв’язати `localhost` у будь-яку з адрес.
pub struct Loopback {
    listeners: Vec<TcpListener>,
    port: u16,
}

impl Loopback {
    pub fn bind() -> Result<Self> {
        let v4 = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .context("не вдалося відкрити локальний порт для входу")?;
        let port = v4.local_addr()?.port();
        let mut listeners = vec![v4];
        if let Ok(v6) = TcpListener::bind(SocketAddr::from((Ipv6Addr::LOCALHOST, port))) {
            listeners.push(v6);
        }
        for l in &listeners {
            l.set_nonblocking(true)?;
        }
        Ok(Self { listeners, port })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Чекає повернення з квитком. Інші запити (favicon, сторонні) отримують 404 і не
    /// зупиняють очікування. Після квитка браузер бачить сторінку `page`.
    pub fn wait_ticket(
        &self,
        timeout: Duration,
        cancel: &AtomicBool,
        page: &Page<'_>,
    ) -> Result<String, WaitError> {
        let deadline = Instant::now() + timeout;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(WaitError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(WaitError::TimedOut);
            }
            let mut idle = true;
            for listener in &self.listeners {
                match listener.accept() {
                    Ok((stream, _)) => {
                        idle = false;
                        if let Some(ticket) = serve(stream, page) {
                            return Ok(ticket);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(WaitError::Io(e.to_string())),
                }
            }
            if idle {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Один HTTP-запит: квиток → сторінка «Готово», інше → 404.
fn serve(mut stream: TcpStream, page: &Page<'_>) -> Option<String> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 2048];
    // Потрібен лише рядок запиту; заголовки дочитуємо до порожнього рядка (≤ 16 КБ).
    while !buffer.windows(4).any(|w| w == b"\r\n\r\n") && buffer.len() < 16 * 1024 {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
        }
    }
    let text = String::from_utf8_lossy(&buffer);
    let ticket = text.lines().next().and_then(parse_callback);
    let (status, body) = match &ticket {
        Some(_) => ("200 OK", done_page(page)),
        None => ("404 Not Found", String::new()),
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
    ticket
}

fn done_page(page: &Page<'_>) -> String {
    let escape = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    format!(
        r##"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Slicenv</title>
<style>
:root{{color-scheme:light dark}}
body{{margin:0;min-height:100vh;display:grid;place-items:center;font:16px/1.5 system-ui,-apple-system,"Segoe UI",sans-serif;background:#f4f5f7;color:#1f2328}}
@media (prefers-color-scheme:dark){{body{{background:#16181d;color:#e6e7ea}}.card{{background:#20232a;border-color:#30343c}}}}
.card{{max-width:420px;margin:24px;padding:32px 36px;background:#fff;border:1px solid #e3e5e8;border-radius:14px;text-align:center;box-shadow:0 8px 30px rgba(0,0,0,.08)}}
.mark{{width:56px;height:56px;margin:0 auto 16px;border-radius:50%;background:#2ea043;display:grid;place-items:center}}
h1{{font-size:20px;margin:0 0 8px}}p{{margin:0;opacity:.75}}
</style></head><body><div class="card"><div class="mark"><svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12l5 5L20 7"/></svg></div>
<h1>{}</h1><p>{}</p></div><script>setTimeout(function(){{try{{window.close()}}catch(e){{}}}},2500)</script></body></html>"##,
        escape(page.title),
        escape(page.message)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: Page<'static> = Page {
        title: "Готово",
        message: "Можна закрити вкладку",
    };

    #[test]
    fn sign_in_url_matches_what_bambulab_builds_for_slicers() {
        let url = sign_in_url("US", 41234, "en");
        assert!(url.starts_with("https://bambulab.com/sign-in?from=studio&source=portal&to=https%3A%2F%2Fbambulab.com%2Fsign-in%2Fcallback%3F"), "{url}");
        let to = decode(url.split("&to=").nth(1).unwrap()).unwrap();
        assert_eq!(
            to,
            "https://bambulab.com/sign-in/callback?openBy=studio&from=studio&source=portal&locale=en&redirect_url=http%3A%2F%2Flocalhost%3A41234&slicerLoginType=ticket"
        );
        assert!(sign_in_url("CN", 1, "zh").starts_with("https://bambulab.cn/sign-in?"));
    }

    #[test]
    fn only_bambu_callbacks_with_a_plain_ticket_are_accepted() {
        let ok = "GET /?ticket=Ab12cD&redirect_url=https%3A%2F%2Fbambulab.com%2Fen%2Fsign-in%2Fstudio-callback HTTP/1.1";
        assert_eq!(parse_callback(ok).as_deref(), Some("Ab12cD"));
        assert_eq!(
            parse_callback("GET /?ticket=xyz HTTP/1.1").as_deref(),
            Some("xyz")
        );
        for bad in [
            "GET /favicon.ico HTTP/1.1",
            "POST /?ticket=Ab12cD HTTP/1.1",
            "GET /other?ticket=Ab12cD HTTP/1.1",
            "GET /?ticket=a%20b HTTP/1.1",
            "GET /?ticket= HTTP/1.1",
            "GET /?ticket=Ab12cD&redirect_url=https%3A%2F%2Fevil.example%2F HTTP/1.1",
            "GET /?ticket=Ab12cD&redirect_url=http%3A%2F%2Fbambulab.com%2F HTTP/1.1",
            "GET /?access_token=secret HTTP/1.1",
        ] {
            assert_eq!(parse_callback(bad), None, "{bad}");
        }
    }

    fn get(port: u16, path: &str) -> String {
        let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        write!(s, "GET {path} HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n").unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    #[test]
    fn loopback_returns_ticket_and_shows_done_page() {
        let server = Loopback::bind().unwrap();
        let port = server.port();
        let client = std::thread::spawn(move || {
            let miss = get(port, "/favicon.ico");
            let hit = get(port, "/?ticket=T0k3n&redirect_url=https%3A%2F%2Fbambulab.com%2Fen%2Fsign-in%2Fstudio-callback");
            (miss, hit)
        });
        let ticket = server
            .wait_ticket(Duration::from_secs(10), &AtomicBool::new(false), &PAGE)
            .unwrap();
        let (miss, hit) = client.join().unwrap();
        assert_eq!(ticket, "T0k3n");
        assert!(miss.starts_with("HTTP/1.1 404"));
        assert!(hit.starts_with("HTTP/1.1 200"));
        assert!(hit.contains("Можна закрити вкладку") && !hit.contains("T0k3n"));
    }

    #[test]
    fn waiting_stops_on_timeout_and_cancel() {
        let server = Loopback::bind().unwrap();
        let start = Instant::now();
        assert_eq!(
            server.wait_ticket(Duration::from_millis(200), &AtomicBool::new(false), &PAGE),
            Err(WaitError::TimedOut)
        );
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_eq!(
            server.wait_ticket(Duration::from_secs(10), &AtomicBool::new(true), &PAGE),
            Err(WaitError::Cancelled)
        );
    }
}
