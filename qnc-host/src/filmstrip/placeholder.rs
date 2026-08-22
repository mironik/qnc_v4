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
    let ph = placeholder.trim();
    let ph = if ph.is_empty() { placeholder_url() } else { ph };
    let mut slots: Vec<Option<serde_json::Value>> = vec![None; n];
    for frame in frames {
        let Some(index) = frame
            .get("frame_index")
            .or_else(|| frame.get("index"))
            .and_then(|value| value.as_i64())
            .filter(|index| *index >= 0 && (*index as usize) < n)
        else {
            continue;
        };
        let slot = &mut slots[index as usize];
        if slot.is_none() {
            *slot = Some(frame);
        }
    }
    slots
        .into_iter()
        .enumerate()
        .map(|(index, frame)| {
            frame.unwrap_or_else(|| {
                serde_json::json!({
                    "index": index as i64,
                    "frame_index": index as i64,
                    "seek_sec": 0.0,
                    "url": ph,
                    "placeholder": true,
                })
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn padding_keeps_frame_indices_in_their_slots() {
        let frames = pad_frames_to_default_with_placeholder(
            vec![json!({
                "index": 5,
                "frame_index": 5,
                "seek_sec": 10.0,
                "url": "/frame_5.jpg",
            })],
            "/placeholder.jpg",
        );

        assert_eq!(frames.len(), DEFAULT_FILMSTRIP_FRAMES as usize);
        assert_eq!(
            frames[0].get("placeholder").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            frames[5].get("url").and_then(|v| v.as_str()),
            Some("/frame_5.jpg")
        );
        assert_eq!(frames[5].get("placeholder").and_then(|v| v.as_bool()), None);
    }

    #[test]
    fn padding_uses_placeholder_for_empty_slots() {
        let frames = pad_frames_to_default_with_placeholder(Vec::new(), "/placeholder.jpg");

        assert_eq!(frames.len(), DEFAULT_FILMSTRIP_FRAMES as usize);
        assert!(frames.iter().enumerate().all(|(index, frame)| {
            frame.get("index").and_then(|v| v.as_i64()) == Some(index as i64)
                && frame.get("frame_index").and_then(|v| v.as_i64()) == Some(index as i64)
                && frame.get("url").and_then(|v| v.as_str()) == Some("/placeholder.jpg")
        }));
    }
}
