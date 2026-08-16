//! Frame/time helpers for the native client.
//!
//! Keep UI timecode frame-based. The host remains the source of truth for
//! persisted frame values; these helpers prevent native Story from falling back
//! to hardcoded 25 fps math.

pub const DEFAULT_FPS: f64 = 25.0;

pub fn normalize_fps(raw: f64) -> f64 {
    if raw.is_finite() && raw > 0.0 {
        raw
    } else {
        DEFAULT_FPS
    }
}

pub fn seconds_to_frame(seconds: f64, fps: f64) -> i64 {
    let fps = normalize_fps(fps);
    (seconds.max(0.0) * fps).round() as i64
}

pub fn frame_to_seconds(frame: i64, fps: f64) -> f64 {
    let fps = normalize_fps(fps);
    (frame.max(0) as f64) / fps
}

pub fn seconds_to_timecode(seconds: f64, fps: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "00:00:00:00".into();
    }
    frame_to_timecode(seconds_to_frame(seconds, fps), fps)
}

pub fn frame_to_timecode(frame: i64, fps: f64) -> String {
    let fps_int = normalize_fps(fps).round().max(1.0) as i64;
    let total = frame.max(0);
    let ff = total % fps_int;
    let total_sec = total / fps_int;
    let ss = total_sec % 60;
    let mm = (total_sec / 60) % 60;
    let hh = total_sec / 3600;
    format!("{hh:02}:{mm:02}:{ss:02}:{ff:02}")
}
