//! FTPS з неявним TLS (порт 990), `bblp` / код доступу. Мінімальний клієнт: лише вхід
//! і `STOR` у пасивному режимі — достатньо для надсилання `.gcode.3mf`.
//!
//! Чому не suppaftp: потрібні та сама перевірка TLS, що й для MQTT (v1-лист, CN = SN),
//! точні коди відповідей для помилок і прогрес/скасування на кожному блоці.
//! Канал даних шифрується тим самим `Arc<ClientConfig>`: rustls відновлює TLS-сесію
//! керувального з’єднання, як `ImplicitFTP_TLS` у ha-bambulab (vsftpd цього вимагає).
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    net::{IpAddr, Shutdown, SocketAddr, TcpStream},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use rustls::{ClientConfig, ClientConnection, StreamOwned, pki_types::ServerName};

use crate::{
    error::{LanError, Result},
    mqtt::USERNAME,
    store::LanPrinter,
    tls::{self, ProblemSlot},
};

pub const FTPS_PORT: u16 = 990;
const TIMEOUT: Duration = Duration::from_secs(20);
const CHUNK: usize = 64 * 1024;

type Tls = StreamOwned<ClientConnection, TcpStream>;

struct Session {
    control: BufReader<Tls>,
    config: Arc<ClientConfig>,
    problem: ProblemSlot,
    host: IpAddr,
}

/// Читає відповідь FTP (з багаторядковими `123-…` / `123 …`).
fn read_reply(reader: &mut impl BufRead) -> std::io::Result<(u16, String)> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Err(std::io::ErrorKind::UnexpectedEof.into());
    }
    let code: u16 = line
        .get(..3)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| std::io::Error::other("некоректна відповідь FTP"))?;
    let mut text = line.get(4..).unwrap_or_default().trim_end().to_owned();
    if line.as_bytes().get(3) == Some(&b'-') {
        let end = format!("{code} ");
        loop {
            let mut next = String::new();
            if reader.read_line(&mut next)? == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            if next.starts_with(&end) {
                text = next[4..].trim_end().to_owned();
                break;
            }
        }
    }
    Ok((code, text))
}

/// `227 Entering Passive Mode (h1,h2,h3,h4,p1,p2)` → порт. Хост відповіді ігноруємо:
/// дані йдуть на ту саму IP, що й керування (захист від підміни адреси й NAT).
fn parse_pasv(text: &str) -> Option<u16> {
    let inner = text.split_once('(')?.1.split_once(')')?.0;
    let numbers: Vec<u16> = inner
        .split(',')
        .map(|n| n.trim().parse().ok())
        .collect::<Option<_>>()?;
    if numbers.len() != 6 || numbers.iter().any(|n| *n > 255) {
        return None;
    }
    let port = numbers[4] * 256 + numbers[5];
    (port != 0).then_some(port)
}

fn tls_connect(
    addr: SocketAddr,
    config: &Arc<ClientConfig>,
    problem: &ProblemSlot,
) -> Result<Tls> {
    let tcp = TcpStream::connect_timeout(&addr, TIMEOUT).map_err(|e| LanError::connect(addr.port(), &e))?;
    tcp.set_read_timeout(Some(TIMEOUT))?;
    tcp.set_write_timeout(Some(TIMEOUT))?;
    let connection = ClientConnection::new(config.clone(), ServerName::IpAddress(addr.ip().into()))
        .map_err(|e| LanError::Protocol(e.to_string()))?;
    let mut stream = StreamOwned::new(connection, tcp);
    // Рукостискання одразу, щоб помилку сертифіката не сплутати з помилкою FTP.
    while stream.conn.is_handshaking() {
        if let Err(error) = stream.conn.complete_io(&mut stream.sock) {
            return Err(problem
                .take()
                .map(LanError::from_tls)
                .unwrap_or_else(|| LanError::connect(addr.port(), &error)));
        }
    }
    Ok(stream)
}

impl Session {
    fn io<T>(&self, result: std::io::Result<T>) -> Result<T> {
        result.map_err(|e| {
            self.problem
                .take()
                .map(LanError::from_tls)
                .unwrap_or_else(|| LanError::Io(e.to_string()))
        })
    }

    fn command(&mut self, line: &str) -> Result<(u16, String)> {
        let sent = self.control.get_mut().write_all(format!("{line}\r\n").as_bytes());
        self.io(sent)?;
        let flushed = self.control.get_mut().flush();
        self.io(flushed)?;
        let reply = read_reply(&mut self.control);
        self.io(reply)
    }

    fn expect(&mut self, line: &str, ok: &[u16]) -> Result<String> {
        let (code, text) = self.command(line)?;
        if ok.contains(&code) {
            Ok(text)
        } else {
            Err(LanError::Ftp {
                code,
                message: text,
            })
        }
    }

    fn open(addr: SocketAddr, config: Arc<ClientConfig>, problem: ProblemSlot, code: &str) -> Result<Self> {
        let control = tls_connect(addr, &config, &problem)?;
        let mut session = Self {
            control: BufReader::new(control),
            config,
            problem,
            host: addr.ip(),
        };
        let greeting = read_reply(&mut session.control);
        let (greet, text) = session.io(greeting)?;
        if greet != 220 {
            return Err(LanError::Ftp {
                code: greet,
                message: text,
            });
        }
        session.expect(&format!("USER {USERNAME}"), &[331, 230])?;
        // Код доступу йде лише в TLS-канал; у помилку потрапляє тільки код відповіді.
        match session.command(&format!("PASS {code}"))? {
            (230 | 202, _) => {}
            (530, _) => return Err(LanError::BadAccessCode),
            (code, message) => return Err(LanError::Ftp { code, message }),
        }
        session.expect("PBSZ 0", &[200])?;
        session.expect("PROT P", &[200])?;
        session.expect("TYPE I", &[200])?;
        Ok(session)
    }

    fn store(
        &mut self,
        local: &Path,
        remote: &str,
        progress: &mut dyn FnMut(u64, u64),
        cancel: &AtomicBool,
    ) -> Result<()> {
        if remote.is_empty() || remote.contains(['/', '\r', '\n']) {
            return Err(LanError::Invalid("ім’я файлу на принтері"));
        }
        let mut file = File::open(local)?;
        let total = file.metadata()?.len();
        let pasv = self.expect("PASV", &[227])?;
        let port = parse_pasv(&pasv).ok_or_else(|| LanError::Protocol(format!("PASV: {pasv}")))?;
        let data_addr = SocketAddr::new(self.host, port);
        let tcp = TcpStream::connect_timeout(&data_addr, TIMEOUT).map_err(|e| LanError::connect(port, &e))?;
        tcp.set_read_timeout(Some(TIMEOUT))?;
        tcp.set_write_timeout(Some(TIMEOUT))?;
        match self.command(&format!("STOR {remote}"))? {
            (125 | 150, _) => {}
            (code, message) => return Err(storage_error(code, message)),
        }
        let connection = ClientConnection::new(self.config.clone(), ServerName::IpAddress(self.host.into()))
            .map_err(|e| LanError::Protocol(e.to_string()))?;
        let mut data = StreamOwned::new(connection, tcp);
        let mut sent = 0u64;
        let mut buf = vec![0u8; CHUNK];
        progress(0, total);
        loop {
            if cancel.load(Ordering::Acquire) {
                let _ = data.sock.shutdown(Shutdown::Both);
                drop(data);
                // Відповідь на обірвану передачу (426/451) і прибирання часткового файлу.
                let _ = read_reply(&mut self.control);
                let _ = self.command(&format!("DELE {remote}"));
                return Err(LanError::Cancelled);
            }
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            let written = data.write_all(&buf[..n]);
            self.io(written)?;
            sent += n as u64;
            progress(sent, total);
        }
        data.conn.send_close_notify();
        let flushed = data.flush();
        self.io(flushed)?;
        while data.conn.wants_write() {
            let done = data.conn.complete_io(&mut data.sock);
            self.io(done)?;
        }
        let _ = data.sock.shutdown(Shutdown::Write);
        // Чекаємо, доки сервер закриє канал даних, а потім 226.
        let mut sink = [0u8; 256];
        while matches!(data.sock.read(&mut sink), Ok(n) if n > 0) {}
        drop(data);
        let reply = read_reply(&mut self.control);
        match self.io(reply)? {
            (226 | 250, _) => Ok(()),
            (code, message) => Err(storage_error(code, message)),
        }
    }

    fn quit(mut self) {
        let _ = self.command("QUIT");
        self.control.get_mut().conn.send_close_notify();
        let _ = self.control.get_mut().flush();
    }
}

/// 452/552 — немає місця; 550/553 з згадкою про карту/сховище — немає SD (A1/P1).
fn storage_error(code: u16, message: String) -> LanError {
    let lower = message.to_ascii_lowercase();
    if matches!(code, 550 | 553 | 452 | 552)
        && (lower.contains("sd") || lower.contains("card") || lower.contains("storage") || lower.contains("no space"))
    {
        LanError::NoSdCard
    } else {
        LanError::Ftp { code, message }
    }
}

fn addr(printer: &LanPrinter) -> SocketAddr {
    SocketAddr::new(printer.ip, FTPS_PORT)
}

/// Перевіряє вхід (TLS + код доступу) і виходить. Файлів не чіпає.
pub fn check_login(printer: &LanPrinter) -> Result<()> {
    let (config, problem) = tls::client_config(&printer.serial);
    Session::open(addr(printer), config, problem, printer.access_code.expose())?.quit();
    Ok(())
}

/// Завантажує `local` у корінь FTP принтера під іменем `remote`. `progress(sent, total)`
/// викликається після кожного блоку; `cancel` перевіряється між блоками.
pub fn upload(
    printer: &LanPrinter,
    local: &Path,
    remote: &str,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<()> {
    let (config, problem) = tls::client_config(&printer.serial);
    upload_to(addr(printer), config, problem, printer.access_code.expose(), local, remote, progress, cancel)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn upload_to(
    addr: SocketAddr,
    config: Arc<ClientConfig>,
    problem: ProblemSlot,
    code: &str,
    local: &Path,
    remote: &str,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<()> {
    let mut session = Session::open(addr, config, problem, code)?;
    session.store(local, remote, progress, cancel)?;
    session.quit();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::{
        ServerConfig, ServerConnection,
        pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject as _},
        server::{ClientHello, ResolvesServerCert},
        sign::CertifiedKey,
    };
    use std::net::TcpListener;

    #[test]
    fn replies_and_pasv_parse() {
        let mut r = std::io::Cursor::new(b"220-Welcome\r\n more\r\n220 ready\r\n530 Login incorrect.\r\n".to_vec());
        assert_eq!(read_reply(&mut r).unwrap(), (220, "ready".into()));
        assert_eq!(read_reply(&mut r).unwrap(), (530, "Login incorrect.".into()));
        assert!(read_reply(&mut r).is_err());
        assert_eq!(parse_pasv("Entering Passive Mode (192,168,1,137,195,80)."), Some(50000));
        assert_eq!(parse_pasv("Entering Passive Mode (1,2,3,4,0,0)"), None);
        assert_eq!(parse_pasv("(1,2,3,4,999,1)"), None);
        assert_eq!(parse_pasv("garbage"), None);
        assert!(matches!(storage_error(553, "Could not create file: no SD card".into()), LanError::NoSdCard));
        assert!(matches!(storage_error(550, "Permission denied".into()), LanError::Ftp { code: 550, .. }));
    }

    /// v1-сертифікат: `with_single_cert` перевіряє його через webpki, тому ключ віддаємо напряму.
    #[derive(Debug)]
    struct Fixed(Arc<CertifiedKey>);
    impl ResolvesServerCert for Fixed {
        fn resolve(&self, _: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
            Some(self.0.clone())
        }
    }

    fn server_config() -> Arc<ServerConfig> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let cert = CertificateDer::from_pem_slice(include_bytes!("../tests/fixtures/leaf.pem")).unwrap();
        let key = PrivateKeyDer::from_pem_slice(include_bytes!("../tests/fixtures/leaf.key.pem")).unwrap();
        let signing = provider.key_provider.load_private_key(key).unwrap();
        let certified = Arc::new(CertifiedKey::new(vec![cert], signing));
        Arc::new(
            ServerConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_cert_resolver(Arc::new(Fixed(certified))),
        )
    }

    /// Мінімальний FTPS-сервер на один вхід; повертає отримані байти файлу.
    fn fake_server(code: &'static str) -> (SocketAddr, std::thread::JoinHandle<Option<Vec<u8>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let data_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let data_port = data_listener.local_addr().unwrap().port();
        let config = server_config();
        let handle = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let mut tls = StreamOwned::new(ServerConnection::new(config.clone()).unwrap(), tcp);
            let say = |tls: &mut StreamOwned<ServerConnection, TcpStream>, s: &str| {
                tls.write_all(format!("{s}\r\n").as_bytes()).unwrap();
                tls.flush().unwrap();
            };
            say(&mut tls, "220 Bambu FTP");
            let mut reader = BufReader::new(tls);
            let mut line = String::new();
            let mut next = |r: &mut BufReader<_>| {
                line.clear();
                r.read_line(&mut line).ok()?;
                Some(line.trim_end().to_owned())
            };
            let mut received = None;
            while let Some(cmd) = next(&mut reader) {
                let tls = reader.get_mut();
                match cmd.split_once(' ').map_or(cmd.as_str(), |c| c.0) {
                    "USER" => say(tls, "331 Please specify the password."),
                    "PASS" if cmd == format!("PASS {code}") => say(tls, "230 Login successful."),
                    "PASS" => say(tls, "530 Login incorrect."),
                    "PBSZ" | "PROT" | "TYPE" => say(tls, "200 OK"),
                    "PASV" => say(
                        tls,
                        &format!("227 Entering Passive Mode (10,9,8,7,{},{})", data_port / 256, data_port % 256),
                    ),
                    "STOR" => {
                        let (dtcp, _) = data_listener.accept().unwrap();
                        say(tls, "150 Ok to send data.");
                        let mut data = StreamOwned::new(ServerConnection::new(config.clone()).unwrap(), dtcp);
                        let mut bytes = Vec::new();
                        let _ = data.read_to_end(&mut bytes);
                        received = Some(bytes);
                        say(tls, "226 Transfer complete.");
                    }
                    "QUIT" => {
                        say(tls, "221 Goodbye.");
                        break;
                    }
                    _ => say(tls, "502 Command not implemented."),
                }
            }
            received
        });
        (addr, handle)
    }

    #[test]
    fn upload_over_implicit_tls_with_progress() {
        let (addr, server) = fake_server("CODE1234");
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("job.gcode.3mf");
        let body: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&local, &body).unwrap();
        let (config, problem) = tls::client_config_with_roots("TESTSERIAL0001", include_bytes!("../tests/fixtures/ca.pem"));
        let mut seen = Vec::new();
        upload_to(addr, config, problem, "CODE1234", &local, "job.gcode.3mf", &mut |s, t| seen.push((s, t)), &AtomicBool::new(false)).unwrap();
        assert_eq!(server.join().unwrap().unwrap(), body);
        assert_eq!(seen.first(), Some(&(0, 200_000)));
        assert_eq!(seen.last(), Some(&(200_000, 200_000)));
    }

    #[test]
    fn wrong_code_and_wrong_serial_are_typed() {
        let (addr, server) = fake_server("CODE1234");
        let (config, problem) = tls::client_config_with_roots("TESTSERIAL0001", include_bytes!("../tests/fixtures/ca.pem"));
        let err = Session::open(addr, config, problem, "WRONG000").err().unwrap();
        assert_eq!(err, LanError::BadAccessCode);
        drop(server);

        let (addr, _server) = fake_server("CODE1234");
        let (config, problem) = tls::client_config_with_roots("OTHERSERIAL1", include_bytes!("../tests/fixtures/ca.pem"));
        let err = Session::open(addr, config, problem, "CODE1234").err().unwrap();
        assert_eq!(err, LanError::SerialMismatch);

        let (addr, _server) = fake_server("CODE1234");
        let (config, problem) = tls::client_config_with_roots("TESTSERIAL0001", include_bytes!("../tests/fixtures/rogue.pem"));
        let err = Session::open(addr, config, problem, "CODE1234").err().unwrap();
        assert!(matches!(err, LanError::UntrustedCertificate(_)), "{err:?}");
    }
}
