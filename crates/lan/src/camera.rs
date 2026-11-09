//! Камера A1 / P1: TLS на порту 6000, 80-байтна автентифікація, далі кадри JPEG
//! (16-байтний заголовок + тіло). Формат — OpenBambuAPI `video.md`; TLS той самий, що й
//! для MQTT (перевірено наживо: A1 на 6000 віддає той самий сертифікат CN = SN від BBL CA).
//!
//! X1 / P2S / H2D замість цього мають RTSPS `rtsps://<ip>:322/streaming/live/1` (H.264);
//! це наступний крок — див. docs/LAN.md.
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    time::Duration,
};

use rustls::{ClientConnection, StreamOwned, pki_types::ServerName};

use crate::{
    error::{LanError, Result},
    mqtt::USERNAME,
    store::LanPrinter,
    tls,
};

pub const CAMERA_PORT: u16 = 6000;
const MAX_FRAME: usize = 4 << 20;

/// Моделі з JPEG-камерою на 6000 (A1, A1 mini, P1P, P1S). Решта — RTSPS або невідомо.
pub fn has_jpeg_camera(model: &str) -> bool {
    matches!(model, "N2S" | "N1" | "C11" | "C12")
}

/// 80 байт: розмір 0x40, тип 0x3000, два нулі (u32 LE), логін і пароль по 32 байти з нулями.
pub fn auth_packet(code: &str) -> Result<[u8; 80]> {
    if code.len() > 32 || USERNAME.len() > 32 {
        return Err(LanError::Invalid("код доступу задовгий"));
    }
    let mut packet = [0u8; 80];
    packet[0..4].copy_from_slice(&0x40u32.to_le_bytes());
    packet[4..8].copy_from_slice(&0x3000u32.to_le_bytes());
    packet[16..16 + USERNAME.len()].copy_from_slice(USERNAME.as_bytes());
    packet[48..48 + code.len()].copy_from_slice(code.as_bytes());
    Ok(packet)
}

/// Розмір тіла кадру з 16-байтного заголовка.
pub fn frame_size(header: &[u8; 16]) -> Result<usize> {
    let size = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    if size == 0 || size > MAX_FRAME {
        return Err(LanError::Protocol(format!("кадр камери {size} байт")));
    }
    Ok(size)
}

pub struct CameraStream {
    stream: StreamOwned<ClientConnection, TcpStream>,
}

impl CameraStream {
    pub fn open(printer: &LanPrinter) -> Result<Self> {
        let (config, problem) = tls::client_config(&printer.serial);
        let addr = SocketAddr::new(printer.ip, CAMERA_PORT);
        let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(8))
            .map_err(|e| LanError::connect(CAMERA_PORT, &e))?;
        tcp.set_read_timeout(Some(Duration::from_secs(10)))?;
        tcp.set_write_timeout(Some(Duration::from_secs(10)))?;
        let conn = ClientConnection::new(config, ServerName::IpAddress(printer.ip.into()))
            .map_err(|e| LanError::Protocol(e.to_string()))?;
        let mut stream = StreamOwned::new(conn, tcp);
        let packet = auth_packet(printer.access_code.expose())?;
        stream.write_all(&packet).map_err(|e| {
            problem
                .take()
                .map(LanError::from_tls)
                .unwrap_or_else(|| LanError::connect(CAMERA_PORT, &e))
        })?;
        stream.flush()?;
        Ok(Self { stream })
    }

    /// Наступний кадр JPEG. Невірний код доступу принтер не повідомляє явно — просто
    /// закриває з’єднання або мовчить; це видно як `BadAccessCode` на першому кадрі.
    pub fn next_frame(&mut self) -> Result<Vec<u8>> {
        let mut header = [0u8; 16];
        self.stream.read_exact(&mut header).map_err(|e| match e.kind() {
            std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset => {
                LanError::BadAccessCode
            }
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
                LanError::Timeout("камера")
            }
            _ => LanError::Io(e.to_string()),
        })?;
        let size = frame_size(&header)?;
        let mut jpeg = vec![0u8; size];
        self.stream.read_exact(&mut jpeg)?;
        if !(jpeg.starts_with(&[0xFF, 0xD8]) && jpeg.ends_with(&[0xFF, 0xD9])) {
            return Err(LanError::Protocol("кадр камери — не JPEG".into()));
        }
        Ok(jpeg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_packet_layout() {
        let p = auth_packet("12345678").unwrap();
        assert_eq!(&p[0..4], &[0x40, 0, 0, 0]);
        assert_eq!(&p[4..8], &[0, 0x30, 0, 0]);
        assert_eq!(&p[8..16], &[0; 8]);
        assert_eq!(&p[16..20], b"bblp");
        assert!(p[20..48].iter().all(|b| *b == 0));
        assert_eq!(&p[48..56], b"12345678");
        assert!(p[56..80].iter().all(|b| *b == 0));
        assert!(auth_packet(&"x".repeat(33)).is_err());
    }

    #[test]
    fn frame_header_bounds() {
        let mut h = [0u8; 16];
        h[0..4].copy_from_slice(&1234u32.to_le_bytes());
        assert_eq!(frame_size(&h).unwrap(), 1234);
        assert!(frame_size(&[0; 16]).is_err());
        h[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(frame_size(&h).is_err());
        assert!(has_jpeg_camera("N2S") && !has_jpeg_camera("BL-P001"));
    }
}
