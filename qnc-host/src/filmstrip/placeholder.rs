//! Filmstrip constants + dark-gray placeholder for empty timeline slots.

/// Default number of filmstrip thumbnails generated per clip.
pub const DEFAULT_FILMSTRIP_FRAMES: u32 = 13;

/// Dark-gray 112×64 JPEG for empty / pending filmstrip slots.
pub static PLACEHOLDER_JPEG: &[u8] = include_bytes!("../../assets/filmstrip_placeholder.jpg");

/// Default placeholder served by Story editor_assets (`/api/story/filmstrip/placeholder`).
pub fn placeholder_url() -> &'static str {
    "/api/story/filmstrip/placeholder"
}

pub fn placeholder_url_for_api(api_prefix: &str) -> String {
    let base = api_prefix.trim().trim_end_matches('/');
    if base.is_empty() {
        return placeholder_url().to_string();
    }
    format!("{base}/filmstrip/placeholder")
}

pub fn pad_frames_to_default(frames: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    pad_frames_to_default_with_placeholder(frames, placeholder_url())
}

pub fn pad_frames_to_default_with_placeholder(
    frames: Vec<serde_json::Value>,
    placeholder: &str,
) -> Vec<serde_json::Value> {
    let n = DEFAULT_FILMSTRIP_FRAMES as usize;
    if frames.len() >= n {
        return frames.into_iter().take(n).collect();
    }
    let mut out = frames;
    let ph = placeholder.trim();
    let ph = if ph.is_empty() { placeholder_url() } else { ph };
    while out.len() < n {
        let index = out.len() as i64;
        out.push(serde_json::json!({
            "frame_index": index,
            "seek_sec": 0.0,
            "url": ph,
            "placeholder": true,
        }));
    }
    out
}
