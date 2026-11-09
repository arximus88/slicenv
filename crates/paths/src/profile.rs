//! Opt-in UI spans, shared by the settings and app crates.
use std::{sync::OnceLock, time::Instant};

pub struct Span {
    name: &'static str,
    started: Option<Instant>,
    all: bool,
}
fn config() -> (bool, bool) {
    static CONFIG: OnceLock<(bool, bool)> = OnceLock::new();
    *CONFIG.get_or_init(|| {
        (
            crate::env("SLICENV_PROFILE").is_some(),
            crate::env("SLICENV_PROFILE_ALL").is_some(),
        )
    })
}

/// Тривалість, виміряна деінде (наприклад, від подання кадру GPU до готових пікселів),
/// у тому самому форматі й з тими самими умовами, що й `Span`.
pub fn record(name: &str, elapsed: std::time::Duration) {
    let (enabled, all) = config();
    let ms = elapsed.as_secs_f64() * 1000.;
    if enabled && (ms > 8. || all) {
        eprintln!("SLICENV_PROFILE {name} {ms:.3}ms");
    }
}

impl Span {
    pub fn new(name: &'static str) -> Self {
        let (enabled, all) = config();
        Self {
            name,
            started: enabled.then(Instant::now),
            all,
        }
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        if let Some(started) = self.started {
            let ms = started.elapsed().as_secs_f64() * 1000.;
            if ms > 8. || self.all {
                eprintln!("SLICENV_PROFILE {} {ms:.3}ms", self.name);
            }
        }
    }
}
