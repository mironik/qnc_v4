//! Minimal player log helper for the modular app player path.

use std::sync::OnceLock;

pub fn log_info(scope: &str, message: impl AsRef<str>) {
    if trace_enabled() {
        println!("[qnc-player:{scope}] {}", message.as_ref());
    }
}

pub fn log_error(scope: &str, message: impl AsRef<str>) {
    eprintln!("[qnc-player:{scope}] {}", message.as_ref());
}

pub fn log_state(scope: &str, status: &str, playing: bool, frame: i64, sec: f64) {
    if trace_enabled() {
        println!(
            "[qnc-player:{scope}] state playing={playing} frame={frame} sec={sec:.3} status={status}"
        );
    }
}

fn trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("QNC_PLAYER_TRACE")
            .map(|value| {
                let value = value.trim().to_ascii_lowercase();
                matches!(value.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(false)
    })
}
