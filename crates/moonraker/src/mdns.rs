//! Пошук Moonraker у локальній мережі через mDNS / DNS-SD (`[zeroconf]` у moonraker.conf
//! оголошує сервіс `_moonraker._tcp.local`).
//!
//! Мінімальна реалізація на `std::net::UdpSocket`, без нових залежностей: один PTR-запит на
//! 224.0.0.251:5353 з ефемерного порту (RFC 6762 §6.7, «legacy unicast» — респондер
//! відповідає прямо на наш порт, тож порт 5353 займати не треба, і avahi не заважає), потім
//! розбір PTR → SRV → A/AAAA з відповіді. Лише читання: нічого не надсилається на принтер,
//! окрім стандартного запиту імен.
//!
//! `SLICENV_MDNS_TARGET=127.0.0.1:5354` спрямовує запит на фейковий респондер замість
//! multicast (тести GUI з `fake_moonraker --mdns-port 5354`).
use std::{
    collections::BTreeMap,
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket},
    time::{Duration, Instant},
};

/// Тип сервісу, який оголошує Moonraker.
pub const SERVICE: &str = "_moonraker._tcp.local";
pub const MDNS_ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(224, 0, 0, 251)), 5353);

const TYPE_A: u16 = 1;
const TYPE_PTR: u16 = 12;
const TYPE_TXT: u16 = 16;
const TYPE_AAAA: u16 = 28;
const TYPE_SRV: u16 = 33;
const CLASS_IN: u16 = 1;
/// Біт QU у класі запитання: «відповідай unicast».
const QU: u16 = 0x8000;

/// Знайдений екземпляр Moonraker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// Ім’я екземпляра без типу сервісу, напр. `Moonraker Instance on voron`.
    pub instance: String,
    /// Ціль SRV, напр. `voron.local`.
    pub host: String,
    pub port: u16,
    /// IPv4 спершу.
    pub addresses: Vec<IpAddr>,
}

impl Found {
    /// Адреса для поля «Адреса»: IPv4 (не потребує nss-mdns), інакше ім’я хоста.
    pub fn address(&self) -> String {
        let port = self.port;
        match self.addresses.iter().find(|a| a.is_ipv4()).or(self.addresses.first()) {
            Some(IpAddr::V4(ip)) => format!("{ip}:{port}"),
            Some(IpAddr::V6(ip)) => format!("[{ip}]:{port}"),
            None => format!("{}:{port}", self.host),
        }
    }

    /// Назва для поля «Назва»: ім’я хоста без `.local`, інакше ім’я екземпляра.
    pub fn display_name(&self) -> String {
        let host = self.host.trim_end_matches('.').trim_end_matches(".local");
        if host.is_empty() {
            self.instance.clone()
        } else {
            host.to_owned()
        }
    }
}

/// PTR-запит `_moonraker._tcp.local` з бітом QU.
pub fn query(id: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(48);
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
    write_name(&mut out, SERVICE);
    out.extend_from_slice(&TYPE_PTR.to_be_bytes());
    out.extend_from_slice(&(CLASS_IN | QU).to_be_bytes());
    out
}

fn write_name(out: &mut Vec<u8>, name: &str) {
    for label in name.trim_end_matches('.').split('.') {
        let bytes = &label.as_bytes()[..label.len().min(63)];
        out.push(bytes.len() as u8);
        out.extend_from_slice(bytes);
    }
    out.push(0);
}

/// Чи це DNS-запит (а не відповідь) про сервіс Moonraker — для фейкового респондера.
pub fn is_moonraker_query(packet: &[u8]) -> bool {
    let Some(flags) = packet.get(2..4) else { return false };
    if flags[0] & 0x80 != 0 {
        return false;
    }
    let mut at = 12;
    let count = u16::from_be_bytes([packet[4], packet[5]]);
    for _ in 0..count {
        let Some(name) = read_name(packet, &mut at) else { return false };
        at += 4;
        if name.eq_ignore_ascii_case(SERVICE) {
            return true;
        }
    }
    false
}

/// Відповідь DNS-SD для одного екземпляра (PTR + SRV + TXT + A) — як шле python-zeroconf.
/// Для фейкового респондера й тестів; стискання імен не використовує.
pub fn response(id: u16, found: &Found) -> Vec<u8> {
    let instance = format!("{}.{SERVICE}", found.instance);
    let mut records: Vec<(String, u16, Vec<u8>)> = Vec::new();
    let mut ptr = Vec::new();
    write_name(&mut ptr, &instance);
    records.push((SERVICE.into(), TYPE_PTR, ptr));
    let mut srv = vec![0, 0, 0, 0];
    srv.extend_from_slice(&found.port.to_be_bytes());
    write_name(&mut srv, &found.host);
    records.push((instance.clone(), TYPE_SRV, srv));
    records.push((instance, TYPE_TXT, b"\x0aversion=v1".to_vec()));
    for ip in &found.addresses {
        match ip {
            IpAddr::V4(v4) => records.push((found.host.clone(), TYPE_A, v4.octets().to_vec())),
            IpAddr::V6(v6) => records.push((found.host.clone(), TYPE_AAAA, v6.octets().to_vec())),
        }
    }
    let mut out = Vec::new();
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&[0x84, 0x00, 0, 0]);
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&((records.len() - 1) as u16).to_be_bytes());
    for (name, kind, data) in records {
        write_name(&mut out, &name);
        out.extend_from_slice(&kind.to_be_bytes());
        out.extend_from_slice(&(CLASS_IN | 0x8000).to_be_bytes());
        out.extend_from_slice(&120u32.to_be_bytes());
        out.extend_from_slice(&(data.len() as u16).to_be_bytes());
        out.extend_from_slice(&data);
    }
    out
}

/// Ім’я DNS з підтримкою стискання (RFC 1035 §4.1.4). `at` — після імені в записі.
fn read_name(packet: &[u8], at: &mut usize) -> Option<String> {
    let mut labels: Vec<String> = Vec::new();
    let mut pos = *at;
    let mut jumped = false;
    let mut hops = 0;
    loop {
        let len = *packet.get(pos)? as usize;
        match len & 0xC0 {
            0xC0 => {
                let low = *packet.get(pos + 1)? as usize;
                if !jumped {
                    *at = pos + 2;
                }
                jumped = true;
                hops += 1;
                if hops > 32 {
                    return None;
                }
                pos = ((len & 0x3F) << 8) | low;
            }
            0x00 => {
                if len == 0 {
                    if !jumped {
                        *at = pos + 1;
                    }
                    return Some(labels.join("."));
                }
                let label = packet.get(pos + 1..pos + 1 + len)?;
                labels.push(String::from_utf8_lossy(label).into_owned());
                if labels.len() > 64 {
                    return None;
                }
                pos += 1 + len;
            }
            _ => return None,
        }
    }
}

#[derive(Debug, Default)]
struct Records {
    /// Екземпляри з PTR (повні імена).
    instances: Vec<String>,
    /// Екземпляр → (ціль, порт).
    srv: BTreeMap<String, (String, u16)>,
    /// Хост → адреси.
    hosts: BTreeMap<String, Vec<IpAddr>>,
}

fn key(name: &str) -> String {
    name.trim_end_matches('.').to_ascii_lowercase()
}

fn parse_into(packet: &[u8], records: &mut Records) -> Option<()> {
    if packet.len() < 12 || packet[2] & 0x80 == 0 {
        return None;
    }
    let count = |i: usize| u16::from_be_bytes([packet[i], packet[i + 1]]) as usize;
    let (questions, total) = (count(4), count(6) + count(8) + count(10));
    let mut at = 12;
    for _ in 0..questions {
        read_name(packet, &mut at)?;
        at += 4;
    }
    for _ in 0..total {
        let name = read_name(packet, &mut at)?;
        let head = packet.get(at..at + 10)?;
        let kind = u16::from_be_bytes([head[0], head[1]]);
        let len = u16::from_be_bytes([head[8], head[9]]) as usize;
        let start = at + 10;
        let data = packet.get(start..start + len)?;
        match kind {
            TYPE_PTR if key(&name) == SERVICE => {
                let mut p = start;
                let target = read_name(packet, &mut p)?;
                if !records.instances.iter().any(|i| key(i) == key(&target)) {
                    records.instances.push(target);
                }
            }
            TYPE_SRV if len >= 7 => {
                let port = u16::from_be_bytes([data[4], data[5]]);
                let mut p = start + 6;
                let target = read_name(packet, &mut p)?;
                records.srv.insert(key(&name), (target, port));
            }
            TYPE_A if len == 4 => {
                let ip = IpAddr::V4(Ipv4Addr::new(data[0], data[1], data[2], data[3]));
                push_unique(records.hosts.entry(key(&name)).or_default(), ip);
            }
            TYPE_AAAA if len == 16 => {
                let mut octets = [0u8; 16];
                octets.copy_from_slice(data);
                let ip = IpAddr::V6(Ipv6Addr::from(octets));
                push_unique(records.hosts.entry(key(&name)).or_default(), ip);
            }
            _ => {}
        }
        at = start + len;
    }
    Some(())
}

fn push_unique(list: &mut Vec<IpAddr>, ip: IpAddr) {
    if !list.contains(&ip) {
        list.push(ip);
    }
}

/// Розбір однієї відповіді. `source` — звідки прийшов пакет: якщо A-запису немає, адресою
/// стає джерело (так роблять і браузери сервісів).
pub fn parse(packet: &[u8], source: Option<IpAddr>) -> Vec<Found> {
    let mut records = Records::default();
    if parse_into(packet, &mut records).is_none() {
        return Vec::new();
    }
    assemble(&records, source)
}

fn assemble(records: &Records, source: Option<IpAddr>) -> Vec<Found> {
    let suffix = format!(".{SERVICE}");
    let mut out: Vec<Found> = Vec::new();
    for full in &records.instances {
        let Some((host, port)) = records.srv.get(&key(full)) else { continue };
        let mut addresses = records.hosts.get(&key(host)).cloned().unwrap_or_default();
        if addresses.is_empty() {
            addresses.extend(source);
        }
        // Link-local IPv6 без зони непридатні для з’єднання — лише IPv4 і глобальні.
        addresses.retain(|a| match a {
            IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) != 0xfe80,
            IpAddr::V4(_) => true,
        });
        addresses.sort_by_key(|a| !a.is_ipv4());
        let trimmed = full.trim_end_matches('.');
        let instance = if trimmed.to_ascii_lowercase().ends_with(&suffix) {
            trimmed[..trimmed.len() - suffix.len()].to_owned()
        } else {
            trimmed.to_owned()
        };
        let found = Found {
            instance,
            host: host.trim_end_matches('.').to_owned(),
            port: *port,
            addresses,
        };
        if !out.iter().any(|f| f.address() == found.address()) {
            out.push(found);
        }
    }
    out
}

/// Куди слати запит: `SLICENV_MDNS_TARGET` (тести) або multicast-група mDNS.
pub fn target() -> SocketAddr {
    paths::env("SLICENV_MDNS_TARGET")
        .and_then(|v| v.to_str().and_then(|s| s.parse().ok()))
        .unwrap_or(MDNS_ADDR)
}

/// Шукає Moonraker протягом `timeout` (запит повторюється раз на секунду, бо UDP).
pub fn discover(timeout: Duration) -> io::Result<Vec<Found>> {
    discover_at(target(), timeout)
}

pub fn discover_at(target: SocketAddr, timeout: Duration) -> io::Result<Vec<Found>> {
    let bind: SocketAddr = if target.ip().is_loopback() {
        (Ipv4Addr::LOCALHOST, 0).into()
    } else {
        (Ipv4Addr::UNSPECIFIED, 0).into()
    };
    let socket = UdpSocket::bind(bind)?;
    if target.ip().is_multicast() {
        socket.set_multicast_ttl_v4(255).ok();
        socket.set_multicast_loop_v4(true).ok();
    }
    let id = (std::process::id() as u16) | 1;
    let packet = query(id);
    let deadline = Instant::now() + timeout;
    let mut next_send = Instant::now();
    let mut records = Records::default();
    let mut sources: Vec<IpAddr> = Vec::new();
    let mut buf = vec![0u8; 9000];
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        if now >= next_send {
            socket.send_to(&packet, target)?;
            next_send = now + Duration::from_secs(1);
        }
        let wait = deadline.min(next_send).saturating_duration_since(now);
        socket.set_read_timeout(Some(wait.max(Duration::from_millis(10))))?;
        match socket.recv_from(&mut buf) {
            Ok((n, from)) => {
                if parse_into(&buf[..n], &mut records).is_some() && !sources.contains(&from.ip()) {
                    sources.push(from.ip());
                }
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            // ICMP «порт недоступний» після unicast на loopback — респондера просто немає.
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {}
            Err(e) => return Err(e),
        }
    }
    // Адреса-джерело як запасна — лише якщо відповідав один хост (інакше не знати, чия).
    let fallback = if sources.len() == 1 { sources.first().copied() } else { None };
    Ok(assemble(&records, fallback))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Відповідь python-zeroconf (Moonraker `[zeroconf]`) зі стисканням імен: PTR у відповіді,
    /// SRV, TXT, A і AAAA (link-local) у додатковому розділі.
    fn zeroconf_fixture() -> Vec<u8> {
        let mut p: Vec<u8> = vec![
            0x00, 0x00, 0x84, 0x00, // id 0, відповідь, авторитетна
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x04, // qd 0, an 1, ns 0, ar 4
        ];
        // @12: _moonraker._tcp.local PTR
        p.extend_from_slice(b"\x0a_moonraker\x04_tcp\x05local\x00");
        p.extend_from_slice(&[0x00, 0x0c, 0x00, 0x01, 0x00, 0x00, 0x11, 0x94]);
        let instance = b"\x1bMoonraker Instance on voron";
        p.extend_from_slice(&((instance.len() + 2) as u16).to_be_bytes());
        let instance_at = p.len();
        p.extend_from_slice(instance);
        p.extend_from_slice(&[0xC0, 0x0C]); // → _moonraker._tcp.local
        // SRV: ім’я — вказівник на екземпляр; ціль voron.local (local — вказівник).
        p.extend_from_slice(&[0xC0, instance_at as u8, 0x00, 0x21, 0x80, 0x01, 0, 0, 0, 120]);
        let target = b"\x05voron";
        p.extend_from_slice(&((6 + target.len() + 2) as u16).to_be_bytes());
        p.extend_from_slice(&[0, 0, 0, 0, 0x1B, 0xD5]); // пріоритет, вага, порт 7125
        let host_at = p.len();
        p.extend_from_slice(target);
        p.extend_from_slice(&[0xC0, 12 + 16]); // → local
        // TXT
        p.extend_from_slice(&[0xC0, instance_at as u8, 0x00, 0x10, 0x80, 0x01, 0, 0, 0, 120]);
        p.extend_from_slice(&[0x00, 0x0A]);
        p.extend_from_slice(b"\x09path=/abc");
        // A voron.local → 192.168.1.60
        p.extend_from_slice(&[0xC0, host_at as u8, 0x00, 0x01, 0x80, 0x01, 0, 0, 0, 120, 0, 4]);
        p.extend_from_slice(&[192, 168, 1, 60]);
        // AAAA fe80::1 — має відкинутися
        p.extend_from_slice(&[0xC0, host_at as u8, 0x00, 0x1C, 0x80, 0x01, 0, 0, 0, 120, 0, 16]);
        p.extend_from_slice(&[0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        p
    }

    #[test]
    fn zeroconf_answer_with_compression_is_parsed() {
        let found = parse(&zeroconf_fixture(), None);
        assert_eq!(found.len(), 1, "{found:?}");
        let f = &found[0];
        assert_eq!(f.instance, "Moonraker Instance on voron");
        assert_eq!(f.host, "voron.local");
        assert_eq!(f.port, 7125);
        assert_eq!(f.addresses, vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 60))]);
        assert_eq!(f.address(), "192.168.1.60:7125");
        assert_eq!(f.display_name(), "voron");
    }

    #[test]
    fn missing_a_record_falls_back_to_source_or_host() {
        let mut fixture = zeroconf_fixture();
        // Лишаємо лише PTR і SRV: ar = 1, обрізаємо після SRV.
        fixture[11] = 1;
        let srv_end = fixture.windows(2).position(|w| w == [0x00, 0x10]).unwrap() - 2;
        fixture.truncate(srv_end);
        let src = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 9));
        assert_eq!(parse(&fixture, Some(src))[0].address(), "10.0.0.9:7125");
        assert_eq!(parse(&fixture, None)[0].address(), "voron.local:7125");
    }

    #[test]
    fn garbage_and_loops_do_not_panic() {
        assert!(parse(&[], None).is_empty());
        assert!(parse(&[0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0xC0, 12], None).is_empty());
        let fixture = zeroconf_fixture();
        for cut in 0..fixture.len() {
            let _ = parse(&fixture[..cut], None);
        }
        // Запит — не відповідь.
        assert!(parse(&query(7), None).is_empty());
    }

    #[test]
    fn query_and_builder_roundtrip() {
        let q = query(0x1234);
        assert!(is_moonraker_query(&q));
        assert_eq!(&q[..2], &[0x12, 0x34]);
        // Клас з бітом QU.
        assert_eq!(&q[q.len() - 4..], &[0x00, 0x0c, 0x80, 0x01]);
        let found = Found {
            instance: "Moonraker Instance on ender".into(),
            host: "ender.local".into(),
            port: 7126,
            addresses: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
        };
        let r = response(1, &found);
        assert!(!is_moonraker_query(&r));
        assert_eq!(parse(&r, None), vec![found]);
    }
}
