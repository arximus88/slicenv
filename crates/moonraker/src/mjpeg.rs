//! MJPEG-потік камери (`stream_url` з `/server/webcams/list`, mjpg-streamer / ustreamer /
//! camera-streamer у crowsnest): `multipart/x-mixed-replace`, кожна частина — один JPEG.
//!
//! Читач тримає не більше одного неповного кадру ([`MAX_FRAME`]); кадри, які застосунок не
//! встигає показати, просто читаються й відкидаються викликачем, тож пам’ять не росте.
use std::io::{self, BufRead, BufReader, Read};

use crate::{
    error::{MoonrakerError, Result},
    http::RawStream,
};

/// Найбільший кадр; більше — це не камера, а щось інше.
pub const MAX_FRAME: usize = 8 << 20;
const MAX_HEADERS: usize = 16 * 1024;
const READ: usize = 64 * 1024;

/// Розкодовує `Transfer-Encoding: chunked` на льоту. Стан зберігається між викликами, тож
/// тайм-аут читання посеред розміру частини нічого не губить.
struct Dechunk<R> {
    inner: BufReader<R>,
    left: usize,
    line: Vec<u8>,
    /// Після даних частини треба з’їсти `\r\n`.
    trailer: bool,
    done: bool,
}

impl<R: Read> Read for Dechunk<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.done {
                return Ok(0);
            }
            if self.left > 0 {
                let n = {
                    let buf = self.inner.fill_buf()?;
                    if buf.is_empty() {
                        return Ok(0);
                    }
                    let n = buf.len().min(self.left).min(out.len());
                    out[..n].copy_from_slice(&buf[..n]);
                    n
                };
                self.inner.consume(n);
                self.left -= n;
                if self.left == 0 {
                    self.trailer = true;
                }
                return Ok(n);
            }
            // Рядок: або `\r\n` після частини, або розмір наступної.
            let mut byte = [0u8; 1];
            if self.inner.read(&mut byte)? == 0 {
                return Ok(0);
            }
            if byte[0] != b'\n' {
                self.line.push(byte[0]);
                if self.line.len() > 1024 {
                    return Err(io::Error::other("bad chunk header"));
                }
                continue;
            }
            let line = String::from_utf8_lossy(&self.line).trim().to_owned();
            self.line.clear();
            if self.trailer {
                self.trailer = false;
                if line.is_empty() {
                    continue;
                }
            }
            let size = usize::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16)
                .map_err(|_| io::Error::other("bad chunk size"))?;
            if size == 0 {
                self.done = true;
            }
            self.left = size;
        }
    }
}

pub struct MjpegStream {
    reader: Box<dyn Read + Send>,
    /// Роздільник без `--`: деякі сервери пишуть `boundary=--x`, інші `boundary=x`.
    boundary: Vec<u8>,
    buf: Vec<u8>,
    /// Тримає `curl` живим (https).
    _raw: Option<RawStream>,
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() + from {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
}

impl MjpegStream {
    /// Читає рядок стану й заголовки з сирого потоку відповіді.
    pub fn open(mut raw: RawStream) -> Result<Self> {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        // Заголовки побайтно: решта потоку лишається в читачі (буферизує вже `MjpegStream`).
        loop {
            match raw.reader.read(&mut byte) {
                Ok(0) => return Err(MoonrakerError::Io("connection closed without response".into())),
                Ok(_) => head.push(byte[0]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(MoonrakerError::io(&e)),
            }
            if head.ends_with(b"\r\n\r\n") {
                // `HTTP/1.1 100 Continue` чи інший 1xx — читаємо далі.
                let text = String::from_utf8_lossy(&head);
                let code = text.split_whitespace().nth(1).unwrap_or("");
                if code.starts_with('1') && code.len() == 3 {
                    head.clear();
                    continue;
                }
                break;
            }
            if head.len() > MAX_HEADERS {
                return Err(MoonrakerError::Protocol("headers too large".into()));
            }
        }
        let text = String::from_utf8_lossy(&head).into_owned();
        let mut lines = text.lines();
        let status: u16 = lines
            .next()
            .filter(|l| l.starts_with("HTTP/"))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| MoonrakerError::Protocol("bad status line".into()))?;
        let headers: Vec<(String, String)> = lines
            .filter_map(|l| {
                let (k, v) = l.split_once(':')?;
                Some((k.trim().to_ascii_lowercase(), v.trim().to_owned()))
            })
            .collect();
        let header = |name: &str| {
            headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
                .unwrap_or("")
        };
        match status {
            200..=299 => {}
            401 | 403 => return Err(MoonrakerError::Unauthorized),
            status => {
                return Err(MoonrakerError::Http {
                    status,
                    message: String::new(),
                });
            }
        }
        let content_type = header("content-type").to_owned();
        let chunked = header("transfer-encoding").eq_ignore_ascii_case("chunked");
        let reader = raw_reader(&mut raw);
        let mut stream = Self::from_reader(reader, &content_type, chunked)?;
        stream._raw = Some(raw);
        Ok(stream)
    }

    /// Тіло потоку з відомим `Content-Type` (тести, фейковий сервер).
    pub fn from_reader(
        reader: Box<dyn Read + Send>,
        content_type: &str,
        chunked: bool,
    ) -> Result<Self> {
        let lower = content_type.to_ascii_lowercase();
        if !lower.starts_with("multipart/") {
            return Err(MoonrakerError::NotMjpeg(content_type.to_owned()));
        }
        let boundary = content_type
            .split(';')
            .filter_map(|p| p.trim().split_once('='))
            .find(|(k, _)| k.trim().eq_ignore_ascii_case("boundary"))
            .map(|(_, v)| v.trim().trim_matches('"').trim_start_matches("--").to_owned())
            .filter(|b| !b.is_empty())
            .ok_or_else(|| MoonrakerError::NotMjpeg(content_type.to_owned()))?;
        let reader: Box<dyn Read + Send> = if chunked {
            Box::new(Dechunk {
                inner: BufReader::new(reader),
                left: 0,
                line: Vec::new(),
                trailer: false,
                done: false,
            })
        } else {
            reader
        };
        Ok(Self {
            reader,
            boundary: format!("--{boundary}").into_bytes(),
            buf: Vec::with_capacity(READ * 2),
            _raw: None,
        })
    }

    /// Дочитує буфер; `Ok(false)` — потік закінчився.
    fn fill(&mut self) -> Result<bool> {
        let start = self.buf.len();
        self.buf.resize(start + READ, 0);
        let got = loop {
            match self.reader.read(&mut self.buf[start..]) {
                Ok(n) => break Ok(n),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => break Err(e),
            }
        };
        match got {
            Ok(n) => {
                self.buf.truncate(start + n);
                Ok(n > 0)
            }
            Err(e) => {
                self.buf.truncate(start);
                Err(if is_timeout(&e) {
                    MoonrakerError::Timeout
                } else {
                    MoonrakerError::Io(e.to_string())
                })
            }
        }
    }

    /// Наступний JPEG. `Err(Timeout)` — кадру ще немає (дані не губляться, можна кликати
    /// знову); кінець потоку — `Err(NotMjpeg)`.
    pub fn next_frame(&mut self) -> Result<Vec<u8>> {
        loop {
            if let Some(frame) = self.take_frame()? {
                return Ok(frame);
            }
            if self.buf.len() > MAX_FRAME + MAX_HEADERS {
                return Err(MoonrakerError::Protocol("MJPEG frame too large".into()));
            }
            if !self.fill()? {
                return Err(MoonrakerError::NotMjpeg("stream ended".into()));
            }
        }
    }

    /// Кадр із буфера, якщо він уже повний.
    fn take_frame(&mut self) -> Result<Option<Vec<u8>>> {
        let Some(at) = find(&self.buf, &self.boundary, 0) else {
            // Сміття до першого роздільника тримати не треба, крім можливого початку роздільника.
            let keep = self.boundary.len();
            if self.buf.len() > MAX_HEADERS + keep {
                let cut = self.buf.len() - keep;
                self.buf.drain(..cut);
            }
            return Ok(None);
        };
        // Заголовки частини.
        let Some(line_end) = find(&self.buf, b"\n", at) else { return Ok(None) };
        let Some(head_end) = find(&self.buf, b"\r\n\r\n", line_end.saturating_sub(1).max(at)).map(|p| p + 4).or_else(|| {
            // Голий роздільник одразу перед тілом (деякі камери без заголовків частин).
            (self.buf.get(line_end + 1..line_end + 3) == Some(&[0xFF, 0xD8])).then_some(line_end + 1)
        }) else {
            if self.buf.len() - at > MAX_HEADERS {
                return Err(MoonrakerError::Protocol("bad MJPEG part headers".into()));
            }
            return Ok(None);
        };
        let head = String::from_utf8_lossy(&self.buf[line_end..head_end]).to_ascii_lowercase();
        let length = head
            .lines()
            .find_map(|l| l.trim().strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse::<usize>().ok());
        let body = match length {
            Some(n) if n > MAX_FRAME => {
                return Err(MoonrakerError::Protocol("MJPEG frame too large".into()));
            }
            Some(n) => {
                if self.buf.len() < head_end + n {
                    return Ok(None);
                }
                head_end..head_end + n
            }
            None => {
                let Some(next) = find(&self.buf, &self.boundary, head_end) else {
                    return Ok(None);
                };
                head_end..next
            }
        };
        let raw = &self.buf[body.clone()];
        // Між тілом і роздільником буває `\r\n`; беремо від SOI до останнього EOI.
        let frame = raw
            .windows(2)
            .position(|w| w == [0xFF, 0xD8])
            .and_then(|s| {
                let e = raw.windows(2).rposition(|w| w == [0xFF, 0xD9])?;
                (e > s).then(|| raw[s..e + 2].to_vec())
            });
        self.buf.drain(..body.end);
        match frame {
            Some(frame) => Ok(Some(frame)),
            // Частина без JPEG (порожня, текст) — пропускаємо.
            None => self.take_frame(),
        }
    }
}

fn raw_reader(raw: &mut RawStream) -> Box<dyn Read + Send> {
    std::mem::replace(&mut raw.reader, Box::new(io::empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(tag: u8) -> Vec<u8> {
        vec![0xFF, 0xD8, 0xFF, 0xE0, tag, tag, 0x00, 0xFF, 0xD9]
    }

    fn body(boundary: &str, frames: &[Vec<u8>], lengths: bool) -> Vec<u8> {
        let mut out = Vec::new();
        for f in frames {
            out.extend_from_slice(format!("--{boundary}\r\nContent-Type: image/jpeg\r\n").as_bytes());
            if lengths {
                out.extend_from_slice(format!("Content-Length: {}\r\n", f.len()).as_bytes());
            }
            out.extend_from_slice(b"X-Timestamp: 1.0\r\n\r\n");
            out.extend_from_slice(f);
            out.extend_from_slice(b"\r\n");
        }
        out
    }

    /// Читач, що віддає дані дрібними шматками й інколи «тайм-аутить».
    struct Trickle {
        data: Vec<u8>,
        at: usize,
        calls: usize,
    }
    impl Read for Trickle {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            self.calls += 1;
            if self.calls.is_multiple_of(3) {
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
            let n = 7.min(out.len()).min(self.data.len() - self.at);
            out[..n].copy_from_slice(&self.data[self.at..self.at + n]);
            self.at += n;
            Ok(n)
        }
    }

    fn frames_of(mut s: MjpegStream, count: usize) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while out.len() < count {
            match s.next_frame() {
                Ok(f) => out.push(f),
                Err(MoonrakerError::Timeout) => continue,
                Err(e) => panic!("{e:?}"),
            }
        }
        out
    }

    #[test]
    fn frames_with_and_without_content_length() {
        let frames = vec![jpeg(1), jpeg(2), jpeg(3)];
        for lengths in [true, false] {
            let mut data = body("boundarydonotcross", &frames, lengths);
            // Ще один роздільник, щоб останній кадр без довжини теж закрився.
            data.extend_from_slice(b"--boundarydonotcross\r\n");
            let reader = Trickle { data, at: 0, calls: 0 };
            let s = MjpegStream::from_reader(
                Box::new(reader),
                "multipart/x-mixed-replace;boundary=boundarydonotcross",
                false,
            )
            .unwrap();
            assert_eq!(frames_of(s, 3), frames, "lengths={lengths}");
        }
    }

    #[test]
    fn chunked_and_dashed_boundary() {
        let frames = vec![jpeg(7), jpeg(8)];
        let plain = body("--frame", &frames, true);
        // Тіло частинами по 5 байтів у chunked.
        let mut chunked = Vec::new();
        for part in plain.chunks(5) {
            chunked.extend_from_slice(format!("{:x}\r\n", part.len()).as_bytes());
            chunked.extend_from_slice(part);
            chunked.extend_from_slice(b"\r\n");
        }
        chunked.extend_from_slice(b"0\r\n\r\n");
        let s = MjpegStream::from_reader(
            Box::new(io::Cursor::new(chunked)),
            "multipart/x-mixed-replace; boundary=\"--frame\"",
            true,
        )
        .unwrap();
        assert_eq!(frames_of(s, 2), frames);
    }

    #[test]
    fn not_mjpeg_and_end_of_stream() {
        assert!(matches!(
            MjpegStream::from_reader(Box::new(io::empty()), "text/html", false),
            Err(MoonrakerError::NotMjpeg(_))
        ));
        assert!(matches!(
            MjpegStream::from_reader(Box::new(io::empty()), "multipart/x-mixed-replace", false),
            Err(MoonrakerError::NotMjpeg(_))
        ));
        let mut s = MjpegStream::from_reader(
            Box::new(io::Cursor::new(b"--b\r\n\r\nnot a jpeg\r\n--b\r\n".to_vec())),
            "multipart/x-mixed-replace;boundary=b",
            false,
        )
        .unwrap();
        assert!(matches!(s.next_frame(), Err(MoonrakerError::NotMjpeg(_))));
    }

    #[test]
    fn oversized_garbage_is_bounded() {
        // Нескінченне сміття без роздільника: буфер не росте понад межу.
        struct Zeros;
        impl Read for Zeros {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                out.fill(0);
                Ok(out.len())
            }
        }
        let mut s =
            MjpegStream::from_reader(Box::new(Zeros), "multipart/x-mixed-replace;boundary=b", false)
                .unwrap();
        for _ in 0..200 {
            if s.fill().is_ok() {
                let _ = s.take_frame();
            }
            assert!(s.buf.len() <= MAX_HEADERS + READ + 8);
        }
        // Кадр більший за межу з Content-Length — помилка, а не алокація.
        let huge = format!("--b\r\nContent-Length: {}\r\n\r\n", MAX_FRAME + 1);
        let mut s = MjpegStream::from_reader(
            Box::new(io::Cursor::new(huge.into_bytes())),
            "multipart/x-mixed-replace;boundary=b",
            false,
        )
        .unwrap();
        assert!(matches!(s.next_frame(), Err(MoonrakerError::Protocol(_))));
    }
}
