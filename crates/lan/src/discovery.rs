//! Виявлення принтерів у локальній мережі: SSDP-подібні `NOTIFY` на UDP 2021.
//!
//! Перевірено наживо (A1, прошивка 01.04.60.02): принтер періодично шле `NOTIFY` з
//! вихідного порту 1900 на порт 2021 цього хоста (у тілі `HOST: 239.255.255.250:1900`).
//! Bambu Studio / Orca слухають 2021 через закритий плагін, тож порт може бути зайнятий —
//! звідси `SO_REUSEADDR` + `SO_REUSEPORT`.
use std::{
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use socket2::{Domain, Protocol, Socket, Type};

pub const DISCOVERY_PORT: u16 = 2021;
const SSDP_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const DEVICE_URN: &str = "urn:bambulab-com:device:3dprinter:1";

/// Принтер, що оголосив себе в мережі.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Discovered {
    /// Серійний номер (`USN`).
    pub serial: String,
    pub ip: IpAddr,
    /// Назва, яку дав користувач (`DevName.bambu.com`).
    pub name: String,
    /// Внутрішній код моделі (`DevModel.bambu.com`): N2S = A1, N1 = A1 mini, C12 = P1S…
    pub model: String,
    /// `lan` або `cloud` (`DevConnect.bambu.com`).
    pub connect: Option<String>,
    /// `free` / `occupied` (`DevBind.bambu.com`): чи прив’язаний до акаунта.
    pub bind: Option<String>,
    /// Версія прошивки (`DevVersion.bambu.com`), якщо принтер її повідомляє.
    pub version: Option<String>,
    /// Рівень Wi-Fi, dBm.
    pub signal: Option<i32>,
}

impl Discovered {
    /// Принтер у режимі «лише LAN» (хмару вимкнено на екрані принтера).
    pub fn lan_only(&self) -> bool {
        self.connect.as_deref() == Some("lan")
    }
}

/// Розбирає `NOTIFY * HTTP/1.1` або відповідь `HTTP/1.1 200 OK` на M-SEARCH.
/// `source` — адреса відправника; `Location` має пріоритет, якщо це коректна IP.
pub fn parse_notify(text: &str, source: IpAddr) -> Option<Discovered> {
    let mut lines = text.lines();
    let start = lines.next()?.trim();
    if !(start.starts_with("NOTIFY") || start.starts_with("HTTP/1.1 200")) {
        return None;
    }
    let mut d = Discovered {
        serial: String::new(),
        ip: source,
        name: String::new(),
        model: String::new(),
        connect: None,
        bind: None,
        version: None,
        signal: None,
    };
    let mut is_printer = false;
    for line in lines {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "nt" | "st" => is_printer |= value.eq_ignore_ascii_case(DEVICE_URN),
            "usn" => d.serial = value.trim_start_matches("uuid:").to_owned(),
            "location" => {
                // Буває голою IP, буває URL; беремо хост, якщо він — IP.
                let host = value
                    .trim_start_matches("http://")
                    .trim_start_matches("https://")
                    .split(['/', ':'])
                    .next()
                    .unwrap_or_default();
                if let Ok(ip) = host.parse() {
                    d.ip = ip;
                }
            }
            "devname.bambu.com" => d.name = value.to_owned(),
            "devmodel.bambu.com" => {
                d.model = value.to_owned();
                is_printer = true;
            }
            "devconnect.bambu.com" => d.connect = Some(value.to_owned()),
            "devbind.bambu.com" => d.bind = Some(value.to_owned()),
            "devversion.bambu.com" => d.version = Some(value.to_owned()),
            "devsignal.bambu.com" => d.signal = value.parse().ok(),
            _ => {}
        }
    }
    let serial_ok =
        (6..=32).contains(&d.serial.len()) && d.serial.chars().all(|c| c.is_ascii_alphanumeric());
    (is_printer && serial_ok).then_some(d)
}

fn bind_socket(port: u16) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    // SO_REUSEPORT є лише на Unix; на Windows SO_REUSEADDR уже дозволяє ділити порт з
    // Bambu Studio/Orca, що слухають той самий SSDP.
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.set_broadcast(true)?;
    socket.bind(&SocketAddr::from(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port)).into())?;
    let socket: UdpSocket = socket.into();
    // Без групи мультикасту частина роутерів не доставляє анонси; помилка не фатальна.
    let _ = socket.join_multicast_v4(&SSDP_GROUP, &Ipv4Addr::UNSPECIFIED);
    socket.set_read_timeout(Some(Duration::from_millis(300)))?;
    Ok(socket)
}

/// Активний запит. Наживо: P2S (01.03.00.00) відповідає `HTTP/1.1 200 OK` одразу, A1
/// (01.04.60.02) не відповідає — його чути лише з періодичних NOTIFY (кожні кілька секунд).
pub fn m_search(socket: &UdpSocket) {
    let message = format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1990\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\nST: {DEVICE_URN}\r\n\r\n"
    );
    for target in [
        SocketAddrV4::new(SSDP_GROUP, 1990),
        SocketAddrV4::new(SSDP_GROUP, DISCOVERY_PORT),
        SocketAddrV4::new(Ipv4Addr::BROADCAST, DISCOVERY_PORT),
    ] {
        let _ = socket.send_to(message.as_bytes(), target);
    }
}

/// Фонове прослуховування: кожен розібраний анонс іде в канал. Drop зупиняє потік.
pub struct Discovery {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Discovery {
    pub fn start() -> io::Result<(Self, mpsc::Receiver<Discovered>)> {
        let socket = bind_socket(DISCOVERY_PORT)?;
        m_search(&socket);
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let flag = stop.clone();
        let thread = thread::Builder::new()
            .name("lan-discovery".into())
            .spawn(move || {
                let mut buf = [0u8; 2048];
                let mut asked = Instant::now();
                while !flag.load(Ordering::Acquire) {
                    if asked.elapsed() > Duration::from_secs(30) {
                        asked = Instant::now();
                        m_search(&socket);
                    }
                    let Ok((n, from)) = socket.recv_from(&mut buf) else {
                        continue;
                    };
                    let text = String::from_utf8_lossy(&buf[..n]);
                    if let Some(found) = parse_notify(&text, from.ip())
                        && tx.send(found).is_err()
                    {
                        break;
                    }
                }
            })?;
        Ok((
            Self {
                stop,
                thread: Some(thread),
            },
            rx,
        ))
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Блокує на `timeout` і повертає унікальні (за серійним номером) принтери. A1 шле
/// анонс приблизно раз на кілька секунд, тож 5–10 с вистачає.
pub fn discover(timeout: Duration) -> io::Result<Vec<Discovered>> {
    let (discovery, rx) = Discovery::start()?;
    let deadline = Instant::now() + timeout;
    let mut found: Vec<Discovered> = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(d) => match found.iter_mut().find(|f| f.serial == d.serial) {
                Some(existing) => *existing = d,
                None => found.push(d),
            },
            Err(_) => break,
        }
    }
    drop(discovery);
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Формат справжнього анонсу A1 (серійник і назва замінені).
    const A1: &str = "NOTIFY * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nServer: UPnP/1.0\r\nLocation: 192.168.1.50\r\nNT: urn:bambulab-com:device:3dprinter:1\r\nUSN: 03919C000000000\r\nCache-Control: max-age=1800\r\nDevModel.bambu.com: N2S\r\nDevName.bambu.com: My A1\r\nDevSignal.bambu.com: -41\r\nDevConnect.bambu.com: cloud\r\nDevBind.bambu.com: occupied\r\nDevseclink.bambu.com: secure\r\nDevVersion.bambu.com: 01.04.60.02\r\nDevCap.bambu.com: 1\r\n\r\n";

    #[test]
    fn parses_real_a1_notify() {
        let d = parse_notify(A1, "10.0.0.9".parse().unwrap()).unwrap();
        assert_eq!(d.serial, "03919C000000000");
        assert_eq!(d.ip, "192.168.1.50".parse::<IpAddr>().unwrap());
        assert_eq!(d.model, "N2S");
        assert_eq!(d.name, "My A1");
        assert_eq!(d.signal, Some(-41));
        assert_eq!(d.connect.as_deref(), Some("cloud"));
        assert!(!d.lan_only());
        assert_eq!(d.bind.as_deref(), Some("occupied"));
        assert_eq!(d.version.as_deref(), Some("01.04.60.02"));
    }

    #[test]
    fn search_reply_lowercase_headers_and_source_fallback() {
        let reply = "HTTP/1.1 200 OK\nst: urn:bambulab-com:device:3dprinter:1\nusn: 01P00A000000000\nlocation: not-an-ip\ndevconnect.bambu.com: lan\ndevmodel.bambu.com: C12\n";
        let d = parse_notify(reply, "192.168.0.50".parse().unwrap()).unwrap();
        assert_eq!(d.ip, "192.168.0.50".parse::<IpAddr>().unwrap());
        assert_eq!(d.model, "C12");
        assert!(d.lan_only());
    }

    #[test]
    fn foreign_or_broken_announcements_are_ignored() {
        let src = "192.168.0.2".parse().unwrap();
        // Звичайний UPnP-пристрій.
        assert!(parse_notify("NOTIFY * HTTP/1.1\r\nNT: upnp:rootdevice\r\nUSN: uuid:abc\r\n", src).is_none());
        // Немає серійника / серійник із недопустимими символами.
        assert!(parse_notify("NOTIFY * HTTP/1.1\r\nNT: urn:bambulab-com:device:3dprinter:1\r\n", src).is_none());
        assert!(parse_notify("NOTIFY * HTTP/1.1\r\nDevModel.bambu.com: N1\r\nUSN: ../../etc\r\n", src).is_none());
        assert!(parse_notify("M-SEARCH * HTTP/1.1\r\nST: urn:bambulab-com:device:3dprinter:1\r\n", src).is_none());
        assert!(parse_notify("", src).is_none());
    }
}
