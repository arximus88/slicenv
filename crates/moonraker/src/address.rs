//! Адреса Moonraker: `host`, `host:порт`, `http://host[:порт][/шлях]`, `https://…`, `[IPv6]:порт`.
//!
//! Без схеми й порту — типовий порт Moonraker 7125. Зі схемою без порту — 80 / 443
//! (Moonraker за nginx Mainsail чи Fluidd, що проксує `/server`, `/printer`, `/access`).
use crate::error::{MoonrakerError, Result};

pub const DEFAULT_PORT: u16 = 7125;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Endpoint {
    pub tls: bool,
    /// Ім’я хоста чи IP; IPv6 — без дужок.
    pub host: String,
    pub port: u16,
    /// Префікс шляху без кінцевого `/` (порожній — корінь).
    pub base: String,
}

fn valid_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    if host.contains(':') {
        return host.parse::<std::net::Ipv6Addr>().is_ok();
    }
    host.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
        && !host.starts_with(['-', '.'])
}

impl Endpoint {
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        if input.is_empty() || input.len() > 512 || input.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(MoonrakerError::InvalidAddress);
        }
        let lower = input.to_ascii_lowercase();
        let (tls, scheme, rest) = if lower.starts_with("https://") {
            (true, true, &input[8..])
        } else if lower.starts_with("http://") {
            (false, true, &input[7..])
        } else if input.contains("://") {
            return Err(MoonrakerError::InvalidAddress);
        } else {
            (false, false, input)
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        if path.contains(['?', '#', '\\']) || authority.contains('@') {
            return Err(MoonrakerError::InvalidAddress);
        }
        let default_port = match (scheme, tls) {
            (false, _) => DEFAULT_PORT,
            (true, false) => 80,
            (true, true) => 443,
        };
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let (host, after) = v6.split_once(']').ok_or(MoonrakerError::InvalidAddress)?;
            let port = match after {
                "" => None,
                p => Some(p.strip_prefix(':').ok_or(MoonrakerError::InvalidAddress)?),
            };
            (host, port)
        } else {
            match authority.matches(':').count() {
                0 => (authority, None),
                1 => {
                    let (h, p) = authority.split_once(':').unwrap_or_default();
                    (h, Some(p))
                }
                // IPv6 без дужок: порт не вказати.
                _ => (authority, None),
            }
        };
        let port = match port {
            None => default_port,
            Some(p) => match p.parse::<u16>() {
                Ok(p) if p > 0 => p,
                _ => return Err(MoonrakerError::InvalidAddress),
            },
        };
        if !valid_host(host) {
            return Err(MoonrakerError::InvalidAddress);
        }
        Ok(Self {
            tls,
            host: host.to_ascii_lowercase(),
            port,
            base: path.trim_end_matches('/').to_owned(),
        })
    }

    fn default_port(&self) -> u16 {
        if self.tls { 443 } else { 80 }
    }

    fn host_part(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        }
    }

    /// `host[:порт]` для заголовка `Host` (типовий порт схеми опускається).
    pub fn authority(&self) -> String {
        if self.port == self.default_port() {
            self.host_part()
        } else {
            format!("{}:{}", self.host_part(), self.port)
        }
    }

    pub fn origin(&self) -> String {
        format!("{}://{}", if self.tls { "https" } else { "http" }, self.authority())
    }

    /// Повна адреса запиту; `path` починається з `/`.
    pub fn url(&self, path: &str) -> String {
        format!("{}{}{path}", self.origin(), self.base)
    }

    /// Нормалізований запис для збереження й показу (`192.168.1.60:7125`, `https://x.local`).
    pub fn display(&self) -> String {
        if !self.tls && self.port == DEFAULT_PORT && self.base.is_empty() {
            format!("{}:{}", self.host_part(), self.port)
        } else {
            format!("{}{}", self.origin(), self.base)
        }
    }

    /// Вебінтерфейс (Mainsail / Fluidd). Moonraker на 7125 — інтерфейс зазвичай на 80 того ж хоста.
    pub fn web(&self) -> Endpoint {
        let mut web = self.clone();
        if web.port == DEFAULT_PORT {
            web.port = web.default_port();
        }
        web.base.clear();
        web
    }

    /// Розбирає абсолютне `http(s)://…` посилання на (адреса, шлях із запитом).
    pub fn split_url(url: &str) -> Option<(Endpoint, String)> {
        let lower = url.to_ascii_lowercase();
        let skip = if lower.starts_with("https://") {
            8
        } else if lower.starts_with("http://") {
            7
        } else {
            return None;
        };
        let rest = &url[skip..];
        let cut = rest.find(['/', '?']).unwrap_or(rest.len());
        let mut endpoint = Endpoint::parse(&format!("{}{}", &url[..skip], &rest[..cut])).ok()?;
        endpoint.base.clear();
        let path = match &rest[cut..] {
            "" => "/".to_owned(),
            p if p.starts_with('?') => format!("/{p}"),
            p => p.to_owned(),
        };
        if path.bytes().any(|b| b.is_ascii_control() || b == b' ') {
            return None;
        }
        Some((endpoint, path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_host_gets_moonraker_port_and_scheme_gets_web_port() {
        let e = Endpoint::parse(" 192.168.1.60 ").unwrap();
        assert_eq!((e.tls, e.host.as_str(), e.port), (false, "192.168.1.60", 7125));
        assert_eq!(e.url("/server/info"), "http://192.168.1.60:7125/server/info");
        assert_eq!(e.display(), "192.168.1.60:7125");
        assert_eq!(e.web().origin(), "http://192.168.1.60");

        let e = Endpoint::parse("HTTP://Ender.local").unwrap();
        assert_eq!((e.host.as_str(), e.port), ("ender.local", 80));
        assert_eq!(e.authority(), "ender.local");

        let e = Endpoint::parse("https://printer.example:8443/moonraker/").unwrap();
        assert!(e.tls);
        assert_eq!(e.url("/printer/info"), "https://printer.example:8443/moonraker/printer/info");
        assert_eq!(e.display(), "https://printer.example:8443/moonraker");

        let e = Endpoint::parse("[fe80::1]:7125").unwrap();
        assert_eq!(e.host, "fe80::1");
        assert_eq!(e.authority(), "[fe80::1]:7125");
        assert_eq!(Endpoint::parse("ender:7126").unwrap().port, 7126);
    }

    #[test]
    fn bad_addresses_are_rejected() {
        for bad in [
            "", "http://", "ftp://x", "a b", "host:0", "host:99999", "host:abc", "user@host",
            "-host", "host/?q", "[::1", "x\ny",
        ] {
            assert!(Endpoint::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn absolute_urls_split_into_endpoint_and_path() {
        let (e, p) = Endpoint::split_url("http://10.0.0.5:8080/webcam/?action=snapshot").unwrap();
        assert_eq!((e.host.as_str(), e.port, p.as_str()), ("10.0.0.5", 8080, "/webcam/?action=snapshot"));
        let (e, p) = Endpoint::split_url("http://cam.local?x=1").unwrap();
        assert_eq!((e.port, p.as_str()), (80, "/?x=1"));
        assert!(Endpoint::split_url("/webcam/?action=snapshot").is_none());
        assert!(Endpoint::split_url("file:///etc/passwd").is_none());
    }
}
