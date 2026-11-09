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
    /// Цілі й поточні температури (°C), вентилятор 0.0–1.0, осі вдома, позиція.
    pub nozzle_target: f64,
    pub bed_target: f64,
    pub nozzle_temp: f64,
    pub bed_temp: f64,
    pub fan: f64,
    pub homed: String,
    pub position: [f64; 3],
    /// Кожен скрипт `/printer/gcode/script` як прийшов.
    pub scripts: Vec<String>,
    /// Скільки разів натискали аварійну зупинку.
    pub emergency_stops: u32,
    /// `stream_url` віддає `multipart/x-mixed-replace` з поточного `snapshot`.
    pub stream: bool,
    /// Пауза між кадрами потоку.
    pub stream_interval: Duration,
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
            nozzle_target: 0.0,
            bed_target: 0.0,
            nozzle_temp: 24.1,
            bed_temp: 23.7,
            fan: 0.0,
            homed: String::new(),
            position: [0.0; 3],
            scripts: Vec::new(),
            emergency_stops: 0,
            stream: true,
            stream_interval: Duration::from_millis(100),
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
                self.nozzle_target = 0.0;
                self.bed_target = 0.0;
            }
        }
        // Нагрів і охолодження до кімнатної — експонентою, без перескоку.
        let k = (dt * 0.3).min(1.0);
        let goal = |target: f64, ambient: f64| if target > 0.0 { target } else { ambient };
        let (n, b) = (goal(self.nozzle_target, 24.1), goal(self.bed_target, 23.7));
        if (self.nozzle_temp - n).abs() > 0.05 {
            self.nozzle_temp += (n - self.nozzle_temp) * k;
        }
        if (self.bed_temp - b).abs() > 0.05 {
            self.bed_temp += (b - self.bed_temp) * k;
        }
    }

    /// Виконує скрипт як Klipper: `Err(повідомлення)` — те, що Klipper відповів би помилкою.
    fn run_script(&mut self, script: &str) -> Result<(), String> {
        let mut relative = false;
        for line in script.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let mut words = line.split_whitespace();
            let cmd = words.next().unwrap_or("").to_ascii_uppercase();
            let args: Vec<(char, String)> = words
                .map(|w| {
                    let mut c = w.chars();
                    (c.next().unwrap_or(' ').to_ascii_uppercase(), c.as_str().to_owned())
                })
                .collect();
            let arg = |k: &str| {
                line.split_whitespace()
                    .find_map(|w| w.strip_prefix(k).map(str::to_owned))
            };
            match cmd.as_str() {
                "G28" => {
                    let axes: String = args
                        .iter()
                        .map(|(c, _)| c.to_ascii_lowercase())
                        .filter(|c| "xyz".contains(*c))
                        .collect();
                    let axes = if axes.is_empty() { "xyz".to_owned() } else { axes };
                    for (i, a) in ['x', 'y', 'z'].into_iter().enumerate() {
                        if axes.contains(a) {
                            self.position[i] = 0.0;
                            if !self.homed.contains(a) {
                                self.homed.push(a);
                            }
                        }
                    }
                }
                "G90" => relative = false,
                "G91" => relative = true,
                "G1" | "G0" => {
                    for (c, v) in &args {
                        let Some(i) = "XYZ".find(*c) else { continue };
                        let v: f64 = v.parse().map_err(|_| format!("Unable to parse '{line}'"))?;
                        if !self.homed.contains(c.to_ascii_lowercase()) {
                            return Err("Must home axis first: 0.000 0.000 0.000 [0.000]".into());
                        }
                        let next = if relative { self.position[i] + v } else { v };
                        let max = if i == 2 { 250.0 } else { 220.0 };
                        if !(0.0..=max).contains(&next) {
                            return Err(format!("Move out of range: {next:.3}"));
                        }
                        self.position[i] = next;
                    }
                }
                "SET_HEATER_TEMPERATURE" => {
                    let heater = arg("HEATER=").unwrap_or_default();
                    let target: f64 = arg("TARGET=")
                        .and_then(|t| t.parse().ok())
                        .ok_or_else(|| "Error on 'SET_HEATER_TEMPERATURE': missing TARGET".to_owned())?;
                    let max = match heater.as_str() {
                        "extruder" => 300.0,
                        "heater_bed" => 120.0,
                        other => return Err(format!("Unknown heater '{other}'")),
                    };
                    if target > max {
                        return Err(format!("Requested temperature ({target:.1}) out of range (0.0:{max:.1})"));
                    }
                    if heater == "extruder" {
                        self.nozzle_target = target;
                    } else {
                        self.bed_target = target;
                    }
                }
                "M106" => {
                    let pwm: f64 = arg("S").and_then(|v| v.parse().ok()).unwrap_or(255.0);
                    self.fan = (pwm / 255.0).clamp(0.0, 1.0);
                }
                "M107" => self.fan = 0.0,
                "M117" | "QUERY_ENDSTOPS" | "STATUS" | "M114" => {}
                other => return Err(format!("Unknown command:\"{other}\"")),
            }
        }
        Ok(())
    }
    fn progress(&self) -> f64 {
        if self.filename.is_empty() {
            0.0
        } else {
            (self.print_duration / self.job_secs).clamp(0.0, 1.0)
        }
    }
    fn active(&self) -> bool {
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
                "extruder": {"temperature": (s.nozzle_temp * 10.0).round() / 10.0, "target": s.nozzle_target, "power": 0.3},
                "heater_bed": {"temperature": (s.bed_temp * 10.0).round() / 10.0, "target": s.bed_target, "power": 0.2},
                "toolhead": {"homed_axes": s.homed, "position": [s.position[0], s.position[1], s.position[2], 0.0]},
                "fan": {"speed": s.fan, "rpm": Value::Null}
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
            let started = print && ready && !s.active();
            if started {
                s.nozzle_target = 215.0;
                s.bed_target = 60.0;
                s.homed = "xyz".into();
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
            let ok = if need.is_empty() { s.active() } else { s.print_state == need };
            if !ready || !ok {
                return error(&mut stream, 400, "Print not in a valid state");
            }
            s.print_state = next.into();
            if next == "cancelled" {
                s.nozzle_target = 0.0;
                s.bed_target = 0.0;
            }
            json_reply(&mut stream, 200, json!({"result": "ok"}))
        }
        ("POST", "/printer/gcode/script") => {
            let script = req
                .path
                .split_once('?')
                .map(|(_, q)| q)
                .unwrap_or("")
                .split('&')
                .find_map(|kv| kv.strip_prefix("script="))
                .map(crate::command::query_decode)
                .unwrap_or_default();
            s.scripts.push(script.clone());
            if !ready {
                return error(&mut stream, 503, "Klippy Host not connected");
            }
            match s.run_script(&script) {
                Ok(()) => json_reply(&mut stream, 200, json!({"result": "ok"})),
                Err(message) => error(&mut stream, 400, &message),
            }
        }
        ("POST", "/printer/emergency_stop") => {
            s.emergency_stops += 1;
            s.klippy_state = "shutdown".into();
            s.state_message = "Shutdown due to webhooks request".into();
            s.nozzle_target = 0.0;
            s.bed_target = 0.0;
            s.homed.clear();
            if s.active() {
                s.print_state = "error".into();
            }
            json_reply(&mut stream, 200, json!({"result": "ok"}))
        }
        ("POST", "/printer/firmware_restart") => {
            s.klippy_state = "ready".into();
            s.state_message = "Printer is ready".into();
            json_reply(&mut stream, 200, json!({"result": "ok"}))
        }
        ("GET", "/server/webcams/list") => {
            let cams = if s.snapshot.is_some() {
                let base = if s.relative_snapshot { String::new() } else { format!("http://{addr}") };
                let url = format!("{base}/webcam/?action=snapshot");
                let stream_url = format!("{base}/webcam/?action=stream");
                json!([{"name": "fake", "enabled": true, "service": "mjpegstreamer",
                        "snapshot_url": url, "stream_url": stream_url,
                        "flip_horizontal": false, "flip_vertical": false, "rotation": 0}])
            } else {
                json!([])
            };
            json_reply(&mut stream, 200, json!({"result": {"webcams": cams}}))
        }
        ("GET", "/webcam/") if req.path.contains("action=stream") => {
            if s.snapshot.is_none() || !s.stream {
                return error(&mut stream, 404, "No stream");
            }
            let interval = s.stream_interval;
            drop(s);
            serve_mjpeg(&mut stream, state, interval)
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

/// `multipart/x-mixed-replace` як mjpg-streamer: кадр — поточний `snapshot`, доки клієнт
/// не закриє з’єднання.
fn serve_mjpeg(stream: &mut TcpStream, state: &Mutex<FakeState>, interval: Duration) -> io::Result<()> {
    const BOUNDARY: &str = "boundarydonotcross";
    write!(
        stream,
        "HTTP/1.0 200 OK\r\nContent-Type: multipart/x-mixed-replace;boundary={BOUNDARY}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n"
    )?;
    loop {
        let Some(jpeg) = state.lock().expect("fake state").snapshot.clone() else {
            return Ok(());
        };
        write!(
            stream,
            "--{BOUNDARY}\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
            jpeg.len()
        )?;
        stream.write_all(&jpeg)?;
        stream.write_all(b"\r\n")?;
        stream.flush()?;
        thread::sleep(interval);
    }
}

/// Фейковий mDNS-респондер на 127.0.0.1: на PTR-запит `_moonraker._tcp.local` відповідає
/// unicast на порт запитувача, як python-zeroconf на «legacy unicast».
pub struct FakeMdns {
    pub addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl FakeMdns {
    pub fn start(addr: SocketAddr, found: Vec<crate::mdns::Found>) -> io::Result<Self> {
        if !addr.ip().is_loopback() {
            return Err(io::Error::other("fake mDNS listens on loopback only"));
        }
        let socket = std::net::UdpSocket::bind(addr)?;
        socket.set_read_timeout(Some(Duration::from_millis(100)))?;
        let addr = socket.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let stop = stop.clone();
            thread::spawn(move || {
                let mut buf = [0u8; 1500];
                while !stop.load(Ordering::Acquire) {
                    let Ok((n, from)) = socket.recv_from(&mut buf) else { continue };
                    if !crate::mdns::is_moonraker_query(&buf[..n]) {
                        continue;
                    }
                    let id = u16::from_be_bytes([buf[0], buf[1]]);
                    for f in &found {
                        socket.send_to(&crate::mdns::response(id, f), from).ok();
                    }
                }
            })
        };
        Ok(Self {
            addr,
            stop,
            handle: Some(handle),
        })
    }
}

impl Drop for FakeMdns {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
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
