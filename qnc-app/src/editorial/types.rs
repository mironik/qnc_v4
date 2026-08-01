use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum LibraryTab {
    #[default]
    All,
    Virtual,
    Segment,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub(crate) struct StoryPart {
    pub part_id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub clip_id: String,
    #[serde(default)]
    pub virtual_shot_id: String,
    #[serde(default)]
    pub in_seconds: Option<f64>,
    #[serde(default)]
    pub out_seconds: Option<f64>,
    #[serde(default)]
    pub duration_label: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub(crate) struct StoryShot {
    #[serde(default)]
    pub shot_id: String,
    #[serde(default)]
    pub root_shot_id: String,
    #[serde(default)]
    pub clip_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub virtual_name: String,
    #[serde(default)]
    pub duration_sec: f64,
    #[serde(default)]
    pub fps: f64,
    #[serde(default)]
    pub has_audio: bool,
    #[serde(default)]
    pub audio_channels: u8,
    #[serde(default)]
    pub duration_label: String,
    #[serde(default)]
    pub thumb_url: String,
    #[serde(default)]
    pub in_seconds: Option<f64>,
    #[serde(default)]
    pub out_seconds: Option<f64>,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub import_status: String,
    /// Proxy import indicator: idle|pending|ready|error (red/yellow/green).
    #[serde(default)]
    pub status_proxy: String,
    /// Original archive indicator: idle|pending|ready|error (red/yellow/blue).
    #[serde(default)]
    pub status_original: String,
    #[serde(default)]
    pub original_in_project: bool,
    /// Absolute local proxy path for native player (from host snapshot).
    #[serde(default)]
    pub play_path: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub(crate) struct StoryCover {
    #[serde(default)]
    pub cover_id: String,
    #[serde(default)]
    pub slot_id: String,
    #[serde(default)]
    pub clip_id: String,
    #[serde(default)]
    pub virtual_shot_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub timeline_start_sec: f64,
    #[serde(default)]
    pub timeline_end_sec: f64,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub(crate) struct MarkerSlot {
    #[serde(default)]
    pub slot_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub start_sec: f64,
    #[serde(default)]
    pub end_sec: f64,
    #[serde(default)]
    pub has_cover: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub(crate) struct StoryMarker {
    #[serde(default)]
    pub marker_id: String,
    #[serde(default)]
    pub timeline_sec: f64,
    #[serde(default)]
    pub part_id: String,
    #[serde(default)]
    pub label: String,
}
