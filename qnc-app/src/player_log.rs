//! Minimal player log helper for the modular app player path.

pub fn log_info(scope: &str, message: impl AsRef<str>) {
    println!("[qnc-player:{scope}] {}", message.as_ref());
}

pub fn log_error(scope: &str, message: impl AsRef<str>) {
    eprintln!("[qnc-player:{scope}] {}", message.as_ref());
}

pub fn log_state(scope: &str, status: &str, playing: bool, frame: i64, sec: f64) {
    println!(
        "[qnc-player:{scope}] state playing={playing} frame={frame} sec={sec:.3} status={status}"
    );
}
