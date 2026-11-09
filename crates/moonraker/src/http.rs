//! Мінімальний HTTP/1.1 клієнт: `http://` — напряму через `std::net`, `https://` — системним
//! `curl` (URL і заголовок з API-ключем ідуть конфігом через stdin, не через argv).
//!
//! Кожен запит — окреме з’єднання з `Connection: close`: Moonraker у LAN відповідає за
//! мілісекунди, а без keep-alive немає стану, який міг би зіпсуватися між запитами.
use std::{
    fs::File,
    io::{self, Read, Write},
    net::{TcpStream, ToSocketAddrs},
    path::Path,
    process::Stdio,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use crate::{
    address::Endpoint,
    error::{MoonrakerError, Result},
};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_BODY: usize = 16 << 20;
const CHUNK: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

impl Method {
    fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

#[derive(Debug)]
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
}

/// Файл для `multipart/form-data` разом зі звичайними полями.
pub struct Upload<'a> {
    pub fields: &'a [(&'a str, &'a str)],
    pub file_field: &'a str,
    pub file_name: &'a str,
    pub path: &'a Path,
}

pub struct Request<'a> {
    pub method: Method,
    /// Шлях із запитом відносно `Endpoint::base`, починається з `/`.
    pub path: &'a str,
    pub api_key: Option<&'a str>,
    pub timeout: Duration,
    /// Не підставляти `Endpoint::base` (знімок камери з вебсервера, не з Moonraker).
    pub raw_path: bool,
}

impl<'a> Request<'a> {
    pub fn get(path: &'a str, api_key: Option<&'a str>) -> Self {
        Self {
            method: Method::Get,
            path,
            api_key,
            timeout: Duration::from_secs(10),
            raw_path: false,
        }
    }
    pub fn post(path: &'a str, api_key: Option<&'a str>) -> Self {
        Self {
            method: Method::Post,
            ..Self::get(path, api_key)
        }
    }
}

fn check_header_value(value: &str) -> Result<()> {
    if value.bytes().any(|b| b.is_ascii_control()) {
        return Err(MoonrakerError::InvalidApiKey);
    }
    Ok(())
}

fn quote_field(value: &str) -> String {
    // RFC 7578: лапки й переводи рядка в імені файлу екрануються відсотками.
    value
        .replace('"', "%22")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

pub fn boundary() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("----slicenv{:x}{:x}", nanos, std::process::id())
}

/// Тіло multipart: (префікс з полями й заголовком файлу, суфікс).
pub fn multipart_frame(upload: &Upload<'_>, boundary: &str) -> (Vec<u8>, Vec<u8>) {
    let mut head = String::new();
    for (name, value) in upload.fields {
        head += &format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n{value}\r\n",
            quote_field(name)
        );
    }
    head += &format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\nContent-Type: application/octet-stream\r\n\r\n",
        quote_field(upload.file_field),
        quote_field(upload.file_name)
    );
    (head.into_bytes(), format!("\r\n--{boundary}--\r\n").into_bytes())
}

/// Виконує запит. `progress(відправлено, усього)` — лише для тіла файлу.
pub fn send(
    endpoint: &Endpoint,
    request: &Request<'_>,
    upload: Option<&Upload<'_>>,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<Response> {
    if let Some(key) = request.api_key {
        check_header_value(key)?;
    }
    if endpoint.tls {
        curl::send(endpoint, request, upload, progress, cancel)
    } else {
        native(endpoint, request, upload, progress, cancel)
    }
}

/// Відкритий потік відповіді (від рядка стану): для MJPEG, який не закінчується.
pub struct RawStream {
    pub reader: Box<dyn Read + Send>,
    /// `curl` для https: вбивається разом із потоком.
    child: Option<std::process::Child>,
}

impl Drop for RawStream {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            child.kill().ok();
            child.wait().ok();
        }
    }
}

/// GET без читання тіла: повертає сирий потік з рядком стану й заголовками. `read_timeout`
/// — для http (на https тайм-аут простою дає `curl --speed-time`).
pub fn open_stream(endpoint: &Endpoint, request: &Request<'_>) -> Result<RawStream> {
    if let Some(key) = request.api_key {
        check_header_value(key)?;
    }
    if endpoint.tls {
        return curl::open_stream(endpoint, request);
    }
    let mut stream = connect(endpoint)?;
    stream.set_read_timeout(Some(request.timeout)).ok();
    stream.set_write_timeout(Some(Duration::from_secs(10))).ok();
    let path = if request.raw_path {
        request.path.to_owned()
    } else {
        format!("{}{}", endpoint.base, request.path)
    };
    let mut head = format!(
        "GET {path} HTTP/1.1\r\nHost: {}\r\nUser-Agent: slicenv\r\nAccept: */*\r\nConnection: close\r\n",
        endpoint.authority()
    );
    if let Some(key) = request.api_key {
        head += &format!("X-Api-Key: {key}\r\n");
    }
    head += "\r\n";
    stream.write_all(head.as_bytes()).map_err(|e| MoonrakerError::io(&e))?;
    Ok(RawStream {
        reader: Box::new(stream),
        child: None,
    })
}

fn connect(endpoint: &Endpoint) -> Result<TcpStream> {
    let addrs: Vec<_> = (endpoint.host.as_str(), endpoint.port)
        .to_socket_addrs()
        .map_err(|e| MoonrakerError::Unreachable(e.to_string()))?
        .collect();
    let mut last = String::from("no address");
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(stream) => return Ok(stream),
            Err(e) => last = e.to_string(),
        }
    }
    Err(MoonrakerError::Unreachable(last))
}

fn native(
    endpoint: &Endpoint,
    request: &Request<'_>,
    upload: Option<&Upload<'_>>,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<Response> {
    let mut file = None;
    let mut frame = None;
    let mut content_type = None;
    let mut length = 0u64;
    if let Some(up) = upload {
        let f = File::open(up.path).map_err(|e| MoonrakerError::Io(e.to_string()))?;
        let size = f.metadata().map_err(|e| MoonrakerError::Io(e.to_string()))?.len();
        let b = boundary();
        let (head, tail) = multipart_frame(up, &b);
        length = head.len() as u64 + size + tail.len() as u64;
        content_type = Some(format!("multipart/form-data; boundary={b}"));
        file = Some((f, size));
        frame = Some((head, tail));
    }
    let mut stream = connect(endpoint)?;
    stream.set_read_timeout(Some(request.timeout)).ok();
    stream.set_write_timeout(Some(Duration::from_secs(30))).ok();
    stream.set_nodelay(true).ok();

    let path = if request.raw_path {
        request.path.to_owned()
    } else {
        format!("{}{}", endpoint.base, request.path)
    };
    let mut head = format!(
        "{} {path} HTTP/1.1\r\nHost: {}\r\nUser-Agent: slicenv\r\nAccept: */*\r\nConnection: close\r\n",
        request.method.as_str(),
        endpoint.authority()
    );
    if let Some(key) = request.api_key {
        head += &format!("X-Api-Key: {key}\r\n");
    }
    if let Some(ct) = &content_type {
        head += &format!("Content-Type: {ct}\r\n");
    }
    if request.method == Method::Post || upload.is_some() {
        head += &format!("Content-Length: {length}\r\n");
    }
    head += "\r\n";

    let written = (|| -> io::Result<std::result::Result<(), MoonrakerError>> {
        stream.write_all(head.as_bytes())?;
        if let (Some((mut f, size)), Some((pre, post))) = (file, frame) {
            stream.write_all(&pre)?;
            let mut buf = vec![0u8; CHUNK];
            let mut sent = 0u64;
            progress(0, size);
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return Ok(Err(MoonrakerError::Cancelled));
                }
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                stream.write_all(&buf[..n])?;
                sent += n as u64;
                progress(sent, size);
            }
            if sent != size {
                return Ok(Err(MoonrakerError::Io("file changed during upload".into())));
            }
            stream.write_all(&post)?;
        }
        stream.flush()?;
        Ok(Ok(()))
    })();
    match written {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(e),
        Err(e) => {
            // Сервер міг відповісти й закрити з’єднання раніше (401, 413): спершу читаємо відповідь.
            stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
            if let Ok(response) = read_response(&mut stream) {
                return Ok(response);
            }
            return Err(if upload.is_some() {
                MoonrakerError::Interrupted(e.to_string())
            } else {
                MoonrakerError::io(&e)
            });
        }
    }
    read_response(&mut stream).map_err(|e| match (e, upload.is_some()) {
        (MoonrakerError::Io(d), true) => MoonrakerError::Interrupted(d),
        (e, _) => e,
    })
}

fn read_response(stream: &mut impl Read) -> Result<Response> {
    let mut raw = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if raw.len() > MAX_BODY {
                    return Err(MoonrakerError::Protocol("response too large".into()));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if raw.is_empty() => return Err(MoonrakerError::io(&e)),
            // Скидання після повної відповіді — не помилка, якщо довжина збігається.
            Err(_) => break,
        }
    }
    if raw.is_empty() {
        return Err(MoonrakerError::Io("connection closed without response".into()));
    }
    parse_response(&raw)
}

pub fn parse_response(raw: &[u8]) -> Result<Response> {
    let mut rest = raw;
    loop {
        let end = rest
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(|| MoonrakerError::Io("incomplete response headers".into()))?;
        let head = String::from_utf8_lossy(&rest[..end]).into_owned();
        rest = &rest[end + 4..];
        let mut lines = head.lines();
        let status: u16 = lines
            .next()
            .filter(|l| l.starts_with("HTTP/"))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| MoonrakerError::Protocol("bad status line".into()))?;
        if (100..200).contains(&status) {
            continue;
        }
        let headers: Vec<(String, String)> = lines
            .filter_map(|l| {
                let (k, v) = l.split_once(':')?;
                Some((k.trim().to_ascii_lowercase(), v.trim().to_owned()))
            })
            .collect();
        let mut response = Response {
            status,
            headers,
            body: Vec::new(),
        };
        if response
            .header("transfer-encoding")
            .is_some_and(|v| v.eq_ignore_ascii_case("chunked"))
        {
            response.body = dechunk(rest)?;
        } else if let Some(len) = response.header("content-length") {
            let len: usize = len
                .parse()
                .map_err(|_| MoonrakerError::Protocol("bad content-length".into()))?;
            if rest.len() < len {
                return Err(MoonrakerError::Io("response body cut short".into()));
            }
            response.body = rest[..len].to_vec();
        } else {
            response.body = rest.to_vec();
        }
        return Ok(response);
    }
}

fn dechunk(mut rest: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line_end = rest
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| MoonrakerError::Io("chunked body cut short".into()))?;
        let size_text = String::from_utf8_lossy(&rest[..line_end]);
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| MoonrakerError::Protocol("bad chunk size".into()))?;
        rest = &rest[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if rest.len() < size + 2 {
            return Err(MoonrakerError::Io("chunked body cut short".into()));
        }
        out.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
}

mod curl {
    use super::*;

    fn quote(value: &str) -> Result<String> {
        if value.chars().any(|c| c.is_control()) {
            return Err(MoonrakerError::InvalidAddress);
        }
        Ok(format!(
            "\"{}\"",
            value.replace('\\', "\\\\").replace('"', "\\\"")
        ))
    }

    pub(super) fn config(
        endpoint: &Endpoint,
        request: &Request<'_>,
        upload: Option<&Upload<'_>>,
    ) -> Result<String> {
        let url = if request.raw_path {
            format!("{}{}", endpoint.origin(), request.path)
        } else {
            endpoint.url(request.path)
        };
        let mut cfg = format!("url = {}\n", quote(&url)?);
        if let Some(key) = request.api_key {
            cfg += &format!("header = {}\n", quote(&format!("X-Api-Key: {key}"))?);
        }
        if request.method == Method::Post && upload.is_none() {
            cfg += "request = \"POST\"\ndata = \"\"\n";
        }
        if let Some(up) = upload {
            for (name, value) in up.fields {
                cfg += &format!("form-string = {}\n", quote(&format!("{name}={value}"))?);
            }
            let path = up.path.to_str().ok_or(MoonrakerError::InvalidFileName)?;
            // У `-F` `;` і `"` у шляху змінюють зміст поля; наші шляхи — тимчасові, без них.
            if path.contains(['"', ';']) || up.file_name.contains(['"', ';']) {
                return Err(MoonrakerError::InvalidFileName);
            }
            cfg += &format!(
                "form = {}\n",
                quote(&format!(
                    "{}=@\"{path}\";filename=\"{}\"",
                    up.file_field, up.file_name
                ))?
            );
        }
        Ok(cfg)
    }

    pub(super) fn open_stream(endpoint: &Endpoint, request: &Request<'_>) -> Result<RawStream> {
        let cfg = config(endpoint, request, None)?;
        let mut child = paths::host::command("curl")
            .args([
                "--silent",
                "--include",
                "--no-buffer",
                "--proto",
                "=https",
                "--connect-timeout",
                "5",
                // Камера замовкла на 10 с — curl виходить, потік закінчується.
                "--speed-time",
                "10",
                "--speed-limit",
                "1",
                "-K",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    MoonrakerError::NoCurl
                } else {
                    MoonrakerError::Io(e.to_string())
                }
            })?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(cfg.as_bytes())
                .map_err(|e| MoonrakerError::Io(e.to_string()))?;
        }
        let out = child.stdout.take().ok_or(MoonrakerError::Io("curl stdout".into()))?;
        Ok(RawStream {
            reader: Box::new(out),
            child: Some(child),
        })
    }

    pub(super) fn send(
        endpoint: &Endpoint,
        request: &Request<'_>,
        upload: Option<&Upload<'_>>,
        progress: &mut dyn FnMut(u64, u64),
        cancel: &AtomicBool,
    ) -> Result<Response> {
        let cfg = config(endpoint, request, upload)?;
        let size = upload
            .and_then(|u| std::fs::metadata(u.path).ok())
            .map_or(0, |m| m.len());
        let timeout = if upload.is_some() { 3600 } else { request.timeout.as_secs().max(1) };
        let mut child = paths::host::command("curl")
            .args([
                "--silent",
                "--show-error",
                "--include",
                "--proto",
                "=https",
                "--connect-timeout",
                "5",
                "--max-time",
                &timeout.to_string(),
                "-K",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    MoonrakerError::NoCurl
                } else {
                    MoonrakerError::Io(e.to_string())
                }
            })?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(cfg.as_bytes())
                .map_err(|e| MoonrakerError::Io(e.to_string()))?;
        }
        if upload.is_some() {
            progress(0, size);
        }
        let mut out = child.stdout.take().ok_or(MoonrakerError::Io("curl stdout".into()))?;
        let mut raw = Vec::new();
        let mut buf = [0u8; 16 * 1024];
        loop {
            if cancel.load(Ordering::Relaxed) {
                child.kill().ok();
                child.wait().ok();
                return Err(MoonrakerError::Cancelled);
            }
            match out.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => raw.extend_from_slice(&buf[..n]),
                Err(e) => return Err(MoonrakerError::Io(e.to_string())),
            }
            if raw.len() > MAX_BODY {
                child.kill().ok();
                return Err(MoonrakerError::Protocol("response too large".into()));
            }
        }
        let status = child.wait().map_err(|e| MoonrakerError::Io(e.to_string()))?;
        match status.code() {
            Some(0) => {}
            // 6 — DNS, 7 — не з’єднався; 28 — таймаут; 35/51/60 — TLS/сертифікат.
            Some(6 | 7) => return Err(MoonrakerError::Unreachable(format!("curl {status}"))),
            Some(28) => return Err(MoonrakerError::Timeout),
            Some(c @ (35 | 51 | 58 | 60)) => {
                return Err(MoonrakerError::Unreachable(format!("TLS (curl {c})")));
            }
            Some(55 | 56) if upload.is_some() => {
                return Err(MoonrakerError::Interrupted(format!("curl {status}")));
            }
            code => return Err(MoonrakerError::Io(format!("curl {code:?}"))),
        }
        if upload.is_some() {
            progress(size, size);
        }
        parse_response(&raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunked_and_continue_responses_parse() {
        let raw = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n";
        let r = parse_response(raw).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, b"{\"a\":1}");
        let cut = b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc";
        assert!(matches!(parse_response(cut), Err(MoonrakerError::Io(_))));
        assert!(parse_response(b"garbage\r\n\r\n").is_err());
    }

    #[test]
    fn multipart_escapes_names_and_curl_config_hides_nothing_in_argv() {
        let path = Path::new("/tmp/x.gcode");
        let up = Upload {
            fields: &[("root", "gcodes"), ("print", "true")],
            file_field: "file",
            file_name: "a\"b.gcode",
            path,
        };
        let (head, tail) = multipart_frame(&up, "BND");
        let head = String::from_utf8(head).unwrap();
        assert!(head.contains("name=\"root\"\r\n\r\ngcodes\r\n"));
        assert!(head.contains("filename=\"a%22b.gcode\""));
        assert_eq!(tail, b"\r\n--BND--\r\n");

        let endpoint = Endpoint::parse("https://printer.example").unwrap();
        let req = Request::get("/server/info", Some("abc123"));
        let cfg = curl::config(&endpoint, &req, None).unwrap();
        assert!(cfg.contains("url = \"https://printer.example/server/info\""));
        assert!(cfg.contains("header = \"X-Api-Key: abc123\""));
        let bad = Request::get("/server/info", Some("a\nb"));
        assert!(send(&endpoint, &bad, None, &mut |_, _| {}, &AtomicBool::new(false)).is_err());
    }
}
