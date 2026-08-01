//! Runtime diagnostics for the broadcast player (preview path).
//!
//! Always logs Error. Other lines when `QNC_PLAYER_LOG=1` or in debug builds.
//! Forms stay unchanged — this is stderr for developers.

use std::sync::atomic::{AtomicBool, Ordering};

static FORCE_LOG: AtomicBool = AtomicBool::new(false);

pub fn set_force_log(on: bool) {
    FORCE_LOG.store(on, Ordering::Relaxed);
}

fn enabled() -> bool {
    if FORCE_LOG.load(Ordering::Relaxed) {
        return true;
    }
    if cfg!(debug_assertions) {
        return true;
    }
    matches!(
        std::env::var("QNC_PLAYER_LOG").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes")
    )
}

pub fn log_info(scope: &str, msg: impl AsRef<str>) {
    if enabled() {
        eprintln!("[qnc-player:{scope}] {}", msg.as_ref());
    }
}

pub fn log_error(scope: &str, msg: impl AsRef<str>) {
    // Errors always surface — this is the Space/play diagnostic Composer asked for.
    eprintln!("[qnc-player:{scope}] ERROR {}", msg.as_ref());
}

pub fn log_state(scope: &str, status: &str, playing: bool, frame: i64, sec: f64) {
    if enabled() {
        eprintln!(
            "[qnc-player:{scope}] state playing={playing} frame={frame} sec={sec:.3} status={status}"
        );
    }
}
