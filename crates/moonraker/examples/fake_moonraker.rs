//! Фейковий Moonraker на 127.0.0.1 для ручної перевірки GUI без справжнього принтера.
//!
//! ```text
//! cargo run -p moonraker --example fake_moonraker -- [--port 7125] [--api-key КЛЮЧ]
//!     [--klippy shutdown] [--printing] [--speedup 20] [--no-camera]
//! ```
//! Друк «іде» за таймером (`--speedup` прискорює), камера віддає синтетичний JPEG.
use std::{net::SocketAddr, thread, time::Duration};

use moonraker::fake::{FakeServer, FakeState};

fn jpeg(frame: u32) -> Vec<u8> {
    let (w, h) = (640u32, 480u32);
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        let band = ((x + frame * 8) / 40 + y / 40) % 2;
        if band == 0 {
            image::Rgb([40, 44, 52])
        } else {
            image::Rgb([200, 120 + (frame % 100) as u8, 60])
        }
    });
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, image::ImageFormat::Jpeg)
        .expect("jpeg");
    out.into_inner()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut port = 7125u16;
    let mut state = FakeState {
        files: vec!["benchy.gcode".into()],
        ..Default::default()
    };
    let mut camera = true;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => port = args.next().and_then(|p| p.parse().ok()).expect("--port N"),
            "--api-key" => state.api_key = args.next(),
            "--klippy" => state.klippy_state = args.next().expect("--klippy STATE"),
            "--speedup" => state.speedup = args.next().and_then(|p| p.parse().ok()).expect("--speedup X"),
            "--printing" => {
                state.print_state = "printing".into();
                state.filename = "benchy.gcode".into();
                state.print_duration = 120.0;
            }
            "--no-camera" => camera = false,
            other => panic!("невідомий аргумент {other}"),
        }
    }
    if state.klippy_state == "shutdown" {
        state.state_message = "MCU 'mcu' shutdown: Timer too close".into();
    }
    if camera {
        state.snapshot = Some(jpeg(0));
    }
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let server = FakeServer::start_on(addr, state).expect("bind");
    println!("fake Moonraker: {}", server.address());
    let mut frame = 0;
    loop {
        thread::sleep(Duration::from_millis(500));
        frame += 1;
        if camera {
            let next = jpeg(frame);
            server.with(|s| s.snapshot = Some(next));
        }
    }
}
