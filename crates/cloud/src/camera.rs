//! Короткі хмарні MJPEG-сеанси через встановлений BambuSource.
use crate::{Client, DeviceId, checked, cstring, ffi, path_string};
use anyhow::{Result, bail, ensure};
use std::{
    ffi::c_void,
    fmt,
    marker::PhantomData,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// JPEG належить Rust; timestamp — вихідний decode_time, у тактах 100 нс.
/// Debug не показує зображення.
pub struct Frame {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub timestamp: u64,
}
impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Frame")
            .field("bytes", &self.jpeg.len())
            .field("width", &self.width)
            .field("height", &self.height)
            .field("timestamp", &self.timestamp)
            .finish()
    }
}
/// Власний потік декодує транспорт у JPEG-семпли. Черга до двох кадрів;
/// повільний читач втрачає нові кадри. Client має жити до stop/Drop.
pub struct CameraStream<'client> {
    frames: Receiver<Frame>,
    stopping: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<()>>>,
    _client: PhantomData<&'client Client>,
}
struct NativeCamera(*mut c_void);
impl Drop for NativeCamera {
    fn drop(&mut self) {
        unsafe { ffi::cloud_camera_close(self.0) };
    }
}
impl<'client> CameraStream<'client> {
    /// Запитує cloud URL один раз; не викликає printers(), subscribe() чи MQTT-команди.
    /// Client повинен бути запущений, авторизований і з’єднаний із сервером.
    pub fn open(client: &'client Client, dev: &DeviceId) -> Result<Self> {
        Self::open_with_firmware(client, dev, "")
    }
    /// Як у Studio: явна OTA-версія з відповіді info/get_version.
    pub fn open_with_firmware(
        client: &'client Client,
        dev: &DeviceId,
        firmware: &str,
    ) -> Result<Self> {
        ensure!(
            firmware.len() <= 64 && firmware.bytes().all(|c| c.is_ascii_digit() || c == b'.'),
            "Некоректна OTA-версія"
        );
        ensure!(
            client.started.get() && client.is_user_login()? && client.is_server_connected()?,
            "Для камери потрібна активна хмарна сесія"
        );
        // dev_id — серійний номер; забороняємо ін’єкцію параметрів URL.
        ensure!(
            !dev.as_str().is_empty()
                && dev
                    .as_str()
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
            "Некоректний формат ідентифікатора камери"
        );
        // Протоколи обов’язкові у форматі MediaPlayCtrl 02.06; порожня OTA-версія
        // дозволяє запит без попередніх MQTT-запитів до принтера.
        let request = format!("{}|{}|\"tutk\",\"agora\"", dev.as_str(), firmware);
        let (code, mut url) = client.raw_call(20, &request, "", 0)?;
        if code != 0 {
            return Err(camera_url_error(code, &url));
        }
        // Studio додає ці поля після callback; URL ніколи не виводиться.
        url.push_str("&device=");
        url.push_str(dev.as_str());
        url.push_str("&net_ver=02.06.00.50&cli_ver=02.06.00.51&dev_ver=");
        url.push_str(firmware);
        let url = cstring(&url)?;
        let path = cstring(path_string(&client.source_path)?)?;
        let (sender, frames) = mpsc::sync_channel(2);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = stopping.clone();
        let worker = thread::Builder::new()
            .name("bambu-camera".into())
            .spawn(move || {
                let mut handle = std::ptr::null_mut();
                // Усі виклики BambuSource, включно із Close/Destroy, на цьому потоці.
                checked(
                    unsafe { ffi::cloud_camera_open(path.as_ptr(), url.as_ptr(), &mut handle) },
                    "Bambu_Open",
                )?;
                let camera = NativeCamera(handle);
                let deadline = Instant::now() + Duration::from_secs(30);
                loop {
                    ensure!(!stop.load(Ordering::Acquire), "Камеру зупинено");
                    let code = unsafe { ffi::cloud_camera_start(camera.0) };
                    if code == 0 {
                        break;
                    }
                    ensure!(
                        code == 2 && Instant::now() < deadline,
                        "Bambu_StartStream: код {code} (-10007: потрібен MJPEG)"
                    );
                    thread::sleep(Duration::from_millis(50));
                }
                let _ = ready_tx.send(());
                while !stop.load(Ordering::Acquire) {
                    let mut bytes = ffi::Bytes::default();
                    let mut timestamp = 0;
                    let code =
                        unsafe { ffi::cloud_camera_read(camera.0, &mut bytes, &mut timestamp) };
                    if code == 2 {
                        thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                    checked(code, "Bambu_ReadSample")?;
                    let jpeg = bytes.to_vec();
                    let (width, height) = jpeg_dimensions(&jpeg)?;
                    match sender.try_send(Frame {
                        jpeg,
                        width,
                        height,
                        timestamp,
                    }) {
                        Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
                        Err(mpsc::TrySendError::Disconnected(_)) => break,
                    }
                }
                Ok(())
            })
            .map_err(|_| anyhow::anyhow!("Не вдалося створити потік камери"))?;
        let mut stream = Self {
            frames,
            stopping,
            worker: Some(worker),
            _client: PhantomData,
        };
        if ready_rx.recv().is_err() {
            stream.join()?;
            bail!("Камера не відкрилася");
        }
        Ok(stream)
    }
    /// Чекає кадр до 30 с. Таймаут не закриває потік автоматично.
    /// Кадр, якщо вже є в черзі; не блокує (для циклу подій хоста).
    pub fn try_frame(&mut self) -> Option<Frame> {
        self.frames.try_recv().ok()
    }

    pub fn next_frame(&mut self) -> Result<Frame> {
        match self.frames.recv_timeout(Duration::from_secs(30)) {
            Ok(frame) => Ok(frame),
            Err(RecvTimeoutError::Timeout) => bail!("Кадр камери не надійшов за 30 с"),
            Err(RecvTimeoutError::Disconnected) => {
                self.join()?;
                bail!("Потік камери завершено")
            }
        }
    }
    fn join(&mut self) -> Result<()> {
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("Потік камери аварійно завершився"))??;
        }
        Ok(())
    }
    /// Припиняє читання та чекає Close/Destroy на потоці камери.
    pub fn stop(&mut self) -> Result<()> {
        self.stopping.store(true, Ordering::Release);
        self.join()
    }
}
impl Drop for CameraStream<'_> {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
// Читаємо SOF із JPEG; BambuSource вже розділив MJPEG на самостійні кадри.
fn jpeg_dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    ensure!(
        bytes.starts_with(&[0xff, 0xd8]) && bytes.ends_with(&[0xff, 0xd9]),
        "Некоректний JPEG камери"
    );
    let mut pos = 2;
    while pos + 4 <= bytes.len() {
        ensure!(bytes[pos] == 0xff, "Некоректний маркер JPEG");
        while bytes.get(pos) == Some(&0xff) {
            pos += 1;
        }
        let marker = *bytes
            .get(pos)
            .ok_or_else(|| anyhow::anyhow!("Обрізаний JPEG"))?;
        pos += 1;
        if marker == 0xda || marker == 0xd9 {
            break;
        }
        if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        let length = bytes
            .get(pos..pos + 2)
            .ok_or_else(|| anyhow::anyhow!("Обрізаний JPEG"))?;
        let len = u16::from_be_bytes([length[0], length[1]]) as usize;
        ensure!(
            len >= 2 && pos + len <= bytes.len(),
            "Обрізаний сегмент JPEG"
        );
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            ensure!(len >= 8, "Обрізаний SOF JPEG");
            let h = u16::from_be_bytes([bytes[pos + 3], bytes[pos + 4]]) as u32;
            let w = u16::from_be_bytes([bytes[pos + 5], bytes[pos + 6]]) as u32;
            ensure!(w > 0 && h > 0, "Порожній JPEG");
            return Ok((w, h));
        }
        pos += len;
    }
    bail!("JPEG без розміру кадру")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn jpeg_markers_and_truncation() {
        let bytes = [
            0xff, 0xd8, 0xff, 0xe0, 0, 4, 0, 0, 0xff, 0xc0, 0, 8, 8, 1, 0xe0, 2, 0x80, 1, 0xff,
            0xd9,
        ];
        assert_eq!(jpeg_dimensions(&bytes).unwrap(), (640, 480));
        for end in 0..bytes.len() {
            assert!(jpeg_dimensions(&bytes[..end]).is_err());
        }
        assert!(jpeg_dimensions(&[0xff, 0xd8, 0xff, 0xc0, 0xff, 0xff, 0xff, 0xd9]).is_err());
    }
}

/// Код у кінці відповіді плагіна: `…[-2]` → `-2` (так його бере MediaPlayCtrl Studio).
fn trailing_code(text: &str) -> Option<i32> {
    let inner = text.trim().strip_suffix(']')?;
    inner[inner.rfind('[')? + 1..].trim().parse().ok()
}

/// Людське пояснення, чому плагін не дав посилання на відео. Текст плагіна не показуємо й не
/// логуємо — лише код.
pub(crate) fn camera_url_error(code: i32, reply: &str) -> anyhow::Error {
    match code {
        -10006 => anyhow::anyhow!(
            "Принтер не відповів на запит відео за 20 с. Перевірте, що він увімкнений і в мережі, і спробуйте ще раз."
        ),
        -10010 => {
            let detail = trailing_code(reply)
                .map(|c| format!(" (Bambu {c})"))
                .unwrap_or_default();
            anyhow::anyhow!(
                "Камера через хмару недоступна{detail}. Перевірте, що принтер онлайн і камера на ньому ввімкнена. У локальній мережі камера показує відео напряму — додайте принтер на сторінці «Пристрій» → «Додати принтер у мережі»."
            )
        }
        -10009 => anyhow::anyhow!("Плагін Bambu повернув завелику відповідь на запит відео."),
        code => anyhow::Error::new(crate::PluginError {
            operation: "get_camera_url".into(),
            code,
        })
        .context("Камеру не вдалося запустити через хмару"),
    }
}

#[cfg(test)]
mod url_error_tests {
    use super::*;

    #[test]
    fn plugin_refusals_become_human_text_without_the_raw_reply() {
        assert_eq!(trailing_code("liveview failed [-2]"), Some(-2));
        assert_eq!(trailing_code("no code"), None);
        let text = camera_url_error(-10010, "secret-ish reply [7]").to_string();
        assert!(text.contains("(Bambu 7)") && !text.contains("secret"), "{text}");
        assert!(camera_url_error(-10006, "").to_string().contains("20 с"));
        assert!(!camera_url_error(-10010, "").to_string().contains("plugin operation"));
    }
}
