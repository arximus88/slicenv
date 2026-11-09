//! Останні п’ять завдань без довільних назв, URL та ідентифікаторів облікового запису.
mod common;
use anyhow::Result;
use cloud::TaskQuery;
use std::{fs::File, io::Write};
fn run(out: &mut File) -> Result<()> {
    let (client, _events) = common::session()?;
    let tasks = client.get_user_tasks(&TaskQuery {
        dev_id: common::device_override(),
        limit: 5,
        ..Default::default()
    })?;
    writeln!(
        out,
        "Завдань: {}; список принтерів не запитувався",
        tasks.len()
    )?;
    for task in tasks {
        let status = match task.status {
            Some(1 | 4) => "виконується",
            Some(2) => "успіх",
            Some(3) => "невдача",
            _ => "невідомо",
        };
        // Навіть назва моделі може містити email, пароль або серійний номер.
        let date = task
            .start_time
            .as_deref()
            .and_then(|s| s.get(..10))
            .filter(|s| {
                s.bytes().enumerate().all(|(i, c)| {
                    if i == 4 || i == 7 {
                        c == b'-'
                    } else {
                        c.is_ascii_digit()
                    }
                })
            })
            .unwrap_or("невідомо");
        let dev = task
            .dev_id
            .as_ref()
            .map_or("невідомо".into(), ToString::to_string);
        writeln!(
            out,
            "Назва: [приховано]; стан: {status}; дата: {date}; пристрій: {dev}"
        )?;
    }
    client.shutdown()
}
fn main() {
    let Ok(mut out) = common::private_output() else {
        std::process::exit(1)
    };
    if let Err(error) = run(&mut out) {
        let _ = writeln!(out, "Історія: {error}");
        std::process::exit(1);
    }
}
