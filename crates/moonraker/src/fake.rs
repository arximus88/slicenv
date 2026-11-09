//! Фейковий Moonraker для тестів і ручної перевірки GUI (`cargo run -p moonraker --example
//! fake_moonraker`). Лише 127.0.0.1, лише std. Не емулює Klipper — тільки форму відповідей
//! тих ендпоінтів, якими користується [`crate::Client`], і простий «друк» за таймером.
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct Received {
    pub name: String,
    pub root: String,
    pub print: bool,
    pub size: usize,
}

#[derive(Debug)]
pub struct FakeState {
    /// Якщо задано — без правильного `X-Api-Key` усе відповідає 401.
    pub api_key: Option<String>,
    pub klippy_state: String,
    pub state_message: String,
    pub print_state: String,
    pub filename: String,
    /// Тривалість «друку» в секундах симуляції.
    pub job_secs: f64,
    pub print_duration: f64,
    pub files: Vec<String>,
    pub uploads: Vec<Received>,
    /// Обірвати з’єднання, прочитавши стільки байтів тіла завантаження.
    pub drop_upload_after: Option<usize>,
    /// Камера: `Some(jpeg)` — `/server/webcams/list` віддає одну камеру зі знімком.
    pub snapshot: Option<Vec<u8>>,
    /// Відносний `snapshot_url` (як у Mainsail) замість абсолютного.
    pub relative_snapshot: bool,
    /// «METHOD шлях» кожного запиту й чи був ключ.
    pub log: Vec<(String, bool)>,
    pub speedup: f64,
    #[doc(hidden)]
    pub last_tick: Option<Instant>,
}

impl Default for FakeState {
    fn default() -> Self {
        Self {
            api_key: None,
            klippy_state: "ready".into(),
            state_message: "Printer is ready".into(),
            print_state: "standby".into(),
            filename: String::new(),
            job_secs: 600.0,
            print_duration: 0.0,
            files: Vec::new(),
            uploads: Vec::new(),
            drop_upload_after: None,
            snapshot: None,
            relative_snapshot: false,
            log: Vec::new(),
            speedup: 1.0,
            last_tick: None,
        }
    }
}

impl FakeState {
    fn tick(&mut self) {
        let now = Instant::now();
        let dt = self
            .last_tick
            .map_or(0.0, |t| now.duration_since(t).as_secs_f64() * self.speedup);
        self.last_tick = Some(now);
        if self.print_state == "printing" {
            self.print_duration += dt;
            if self.print_duration >= self.job_secs {
                self.print_duration = self.job_secs;
                self.print_state = "complete".into();
            }
        }
    }
    fn progress(&self) -> f64 {
        if self.filename.is_empty() {
            0.0
        } else {
            (self.print_duration / self.job_secs).clamp(0.0, 1.0)
        }
    }
    fn heating(&self) -> bool {
        matches!(self.print_state.as_str(), "printing" | "paused")
    }
}

pub struct FakeServer {
    pub addr: SocketAddr,
    pub state: Arc<Mutex<FakeState>>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl FakeServer {
    /// Випадковий вільний порт на 127.0.0.1.
    pub fn start(state: FakeState) -> io::Result<Self> {
        Self::start_on("127.0.0.1:0".parse().expect("addr"), state)
    }

    pub fn start_on(addr: SocketAddr, state: FakeState) -> io::Result<Self> {
        if !addr.ip().is_loopback() {
            return Err(io::Error::other("fake Moonraker listens on loopback only"));
        }
        let listener = TcpListener::bind(addr)?;
        let addr = listener.local_addr()?;
        let state = Arc::new(Mutex::new(state));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let state = state.clone();
            let stop = stop.clone();
            thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    let state = state.clone();
                    thread::spawn(move || {
                        let _ = handle(stream, addr, &state);
                    });
                }
            })
        };
        Ok(Self {
            addr,
            state,
            stop,
            handle: Some(handle),
        })
    }

    /// Адреса для `Endpoint::parse`.
    pub fn address(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut FakeState) -> R) -> R {
        f(&mut self.state.lock().expect("fake state"))
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

struct Incoming {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}

impl Incoming {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

fn respond(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        503 => "Service Unavailable",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}

fn json_reply(stream: &mut TcpStream, status: u16, value: Value) -> io::Result<()> {
    respond(stream, status, "application/json", value.to_string().as_bytes())
}

fn error(stream: &mut TcpStream, status: u16, message: &str) -> io::Result<()> {
    json_reply(stream, status, json!({"error": {"code": status, "message": message}}))
}

fn handle(stream: TcpStream, addr: SocketAddr, state: &Mutex<FakeState>) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
        return Ok(());
    };
    let mut req = Incoming {
        method: method.to_owned(),
        path: path.to_owned(),
        headers: Vec::new(),
    };
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h == "\r\n" || h == "\n" {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            req.headers.push((k.trim().to_ascii_lowercase(), v.trim().to_owned()));
        }
    }
    let length: usize = req.header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut stream = stream;
    let route = req.path.split('?').next().unwrap_or("").to_owned();

    let (authorized, drop_after) = {
        let mut s = state.lock().expect("fake state");
        s.tick();
        let has_key = req.header("x-api-key").is_some();
        s.log.push((format!("{} {}", req.method, req.path), has_key));
        let ok = s.api_key.as_deref().is_none_or(|k| req.header("x-api-key") == Some(k))
            || route.starts_with("/webcam");
        (ok, s.drop_upload_after)
    };

    if route == "/server/files/upload"
        && authorized
        && let Some(n) = drop_after.filter(|n| *n < length)
    {
        let mut sink = vec![0u8; n];
        reader.read_exact(&mut sink).ok();
        stream.shutdown(Shutdown::Both).ok();
        return Ok(());
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    if !authorized {
        return error(&mut stream, 401, "Unauthorized");
    }

    let mut s = state.lock().expect("fake state");
    let ready = s.klippy_state == "ready";
    match (req.method.as_str(), route.as_str()) {
        ("GET", "/server/info") => {
            let value = json!({"result": {
                "klippy_connected": s.klippy_state != "disconnected",
                "klippy_state": s.klippy_state,
                "components": ["file_manager", "webcam"],
                "failed_components": [],
                "warnings": [],
                "moonraker_version": "v0.9.3-fake",
                "api_version_string": "1.5.0"
            }});
            json_reply(&mut stream, 200, value)
        }
        ("GET", "/printer/info") => {
            if s.klippy_state == "disconnected" {
                return error(&mut stream, 503, "Klippy Disconnected");
            }
            let value = json!({"result": {
                "state": s.klippy_state,
                "state_message": s.state_message,
                "hostname": "fake-klipper",
                "software_version": "v0.12.0-fake"
            }});
            json_reply(&mut stream, 200, value)
        }
        ("GET", "/printer/objects/query") => {
            if !ready {
                return error(&mut stream, 503, "Klippy Host not connected");
            }
            let heating = s.heating();
            let (nozzle_t, bed_t) = if heating { (215.0, 60.0) } else { (0.0, 0.0) };
            let progress = s.progress();
            let total_layer = 100;
            let value = json!({"result": {"eventtime": 1234.5, "status": {
                "print_stats": {
                    "filename": s.filename, "state": s.print_state, "message": "",
                    "print_duration": s.print_duration, "total_duration": s.print_duration,
                    "filament_used": 0.0,
                    "info": {"total_layer": if s.filename.is_empty() { Value::Null } else { json!(total_layer) },
                             "current_layer": if s.filename.is_empty() { Value::Null } else { json!((progress * total_layer as f64).ceil() as u32) }}
                },
                "virtual_sdcard": {"progress": progress, "is_active": s.print_state == "printing", "file_position": 0},
                "display_status": {"progress": progress, "message": Value::Null},
                "extruder": {"temperature": if heating { 214.6 } else { 24.1 }, "target": nozzle_t, "power": 0.3},
                "heater_bed": {"temperature": if heating { 59.8 } else { 23.7 }, "target": bed_t, "power": 0.2},
                "toolhead": {"homed_axes": if heating { "xyz" } else { "" }, "position": [0.0, 0.0, 0.0, 0.0]}
            }}});
            json_reply(&mut stream, 200, value)
        }
        ("GET", "/server/files/list") => {
            let list: Vec<Value> = s
                .files
                .iter()
                .map(|f| json!({"path": f, "modified": 1.0, "size": 1, "permissions": "rw"}))
                .collect();
            json_reply(&mut stream, 200, json!({"result": list}))
        }
        ("POST", "/server/files/upload") => {
            let ct = req.header("content-type").unwrap_or("").to_owned();
            let Some(form) = parse_multipart(&ct, &body) else {
                return error(&mut stream, 400, "Bad multipart");
            };
            let field = |n: &str| form.iter().find(|p| p.name == n);
            let Some(file) = field("file").filter(|f| f.filename.is_some()) else {
                return error(&mut stream, 400, "No file");
            };
            let name = file.filename.clone().unwrap_or_default();
            let root = field("root").map(|p| String::from_utf8_lossy(&p.data).into_owned()).unwrap_or("gcodes".into());
            let print = field("print").is_some_and(|p| p.data == b"true");
            s.uploads.push(Received {
                name: name.clone(),
                root,
                print,
                size: file.data.len(),
            });
            if !s.files.contains(&name) {
                s.files.push(name.clone());
            }
            let started = print && ready && !s.heating();
            if started {
                s.filename = name.clone();
                s.print_state = "printing".into();
                s.print_duration = 0.0;
            }
            json_reply(&mut stream, 201, json!({
                "item": {"path": name, "root": "gcodes", "modified": 1.0, "size": file.data.len(), "permissions": "rw"},
                "print_started": started, "print_queued": false, "action": "create_file"
            }))
        }
        ("POST", "/printer/print/pause") | ("POST", "/printer/print/resume") | ("POST", "/printer/print/cancel") => {
            let (need, next) = match route.as_str() {
                "/printer/print/pause" => ("printing", "paused"),
                "/printer/print/resume" => ("paused", "printing"),
                _ => ("", "cancelled"),
            };
            let ok = if need.is_empty() { s.heating() } else { s.print_state == need };
            if !ready || !ok {
                return error(&mut stream, 400, "Print not in a valid state");
            }
            s.print_state = next.into();
            json_reply(&mut stream, 200, json!({"result": "ok"}))
        }
        ("GET", "/server/webcams/list") => {
            let cams = if s.snapshot.is_some() {
                let url = if s.relative_snapshot {
                    "/webcam/?action=snapshot".to_owned()
                } else {
                    format!("http://{addr}/webcam/?action=snapshot")
                };
                json!([{"name": "fake", "enabled": true, "service": "mjpegstreamer",
                        "snapshot_url": url, "stream_url": "/webcam/?action=stream",
                        "flip_horizontal": false, "flip_vertical": false, "rotation": 0}])
            } else {
                json!([])
            };
            json_reply(&mut stream, 200, json!({"result": {"webcams": cams}}))
        }
        ("GET", "/webcam/") => match s.snapshot.clone() {
            Some(jpeg) => {
                drop(s);
                respond(&mut stream, 200, "image/jpeg", &jpeg)
            }
            None => error(&mut stream, 404, "No webcam"),
        },
        _ => error(&mut stream, 404, "Not Found"),
    }
}

struct Part {
    name: String,
    filename: Option<String>,
    data: Vec<u8>,
}

fn attr(header: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let start = header.find(&needle)? + needle.len();
    let end = header[start..].find('"')? + start;
    Some(header[start..end].to_owned())
}

fn parse_multipart(content_type: &str, body: &[u8]) -> Option<Vec<Part>> {
    let boundary = content_type.split("boundary=").nth(1)?.trim().trim_matches('"');
    let delim = format!("--{boundary}").into_bytes();
    let mut parts = Vec::new();
    let mut rest = body;
    loop {
        let at = find(rest, &delim)?;
        rest = &rest[at + delim.len()..];
        if rest.starts_with(b"--") {
            return Some(parts);
        }
        rest = rest.strip_prefix(b"\r\n")?;
        let head_end = find(rest, b"\r\n\r\n")?;
        let head = String::from_utf8_lossy(&rest[..head_end]).into_owned();
        rest = &rest[head_end + 4..];
        let mut end_delim = b"\r\n".to_vec();
        end_delim.extend_from_slice(&delim);
        let data_end = find(rest, &end_delim)?;
        parts.push(Part {
            name: attr(&head, "name")?,
            filename: attr(&head, "filename"),
            data: rest[..data_end].to_vec(),
        });
        rest = &rest[data_end + 2..];
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}
