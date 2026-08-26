use serde::Deserialize;

use crate::api::EditorialSourceTimebase;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum LibraryTab {
    #[default]
    All,
    Virtual,
    Cover,
    Segment,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub(crate) struct StoryPart {
    #[serde(default)]
    pub shot_id: String,
    #[serde(default)]
    pub root_shot_id: String,
    pub part_id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub clip_id: String,
    #[serde(default)]
    pub virtual_shot_id: String,
    #[serde(default)]
    pub in_seconds: Option<f64>,
    #[serde(default)]
    pub out_seconds: Option<f64>,
    #[serde(default)]
    pub in_frame: i64,
    #[serde(default)]
    pub out_frame: i64,
    #[serde(default)]
    pub fps: f64,
    #[serde(default)]
    pub source_timebase: EditorialSourceTimebase,
    #[serde(default)]
    pub duration_frames: i64,
    #[serde(default)]
    pub duration_label: String,
    #[serde(default)]
    pub source_class: String,
    #[serde(default)]
    pub virtual_category: String,
    #[serde(default = "default_true")]
    pub active: bool,
}

impl Default for StoryPart {
    fn default() -> Self {
        Self {
            shot_id: String::new(),
            root_shot_id: String::new(),
            part_id: String::new(),
            kind: String::new(),
            title: String::new(),
            text: String::new(),
            clip_id: String::new(),
            virtual_shot_id: String::new(),
            in_seconds: None,
            out_seconds: None,
            in_frame: 0,
            out_frame: 0,
            fps: 0.0,
            source_timebase: EditorialSourceTimebase::default(),
            duration_frames: 0,
            duration_label: String::new(),
            source_class: String::new(),
            virtual_category: String::new(),
            active: true,
        }
    }
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
    pub source_timebase: EditorialSourceTimebase,
    #[serde(default)]
    pub in_frame: i64,
    #[serde(default)]
    pub out_frame: i64,
    #[serde(default)]
    pub duration_frames: i64,
    #[serde(default)]
    pub has_audio: bool,
    #[serde(default)]
    pub audio_channels: u8,
    #[serde(default)]
    pub field_order: String,
    #[serde(default)]
    pub interlaced: bool,
    #[serde(default)]
    pub source_class: String,
    #[serde(default)]
    pub virtual_category: String,
    #[serde(default)]
    pub proxy_recipe: String,
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
    /// Legacy snapshot compatibility. Active playback input is resolved through media gateway.
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
    pub note: String,
    #[serde(default)]
    pub timeline_start_sec: f64,
    #[serde(default)]
    pub timeline_end_sec: f64,
    #[serde(default)]
    pub timeline_start_frame: i64,
    #[serde(default)]
    pub timeline_end_frame: i64,
    #[serde(default)]
    pub source_in_frame: i64,
    #[serde(default)]
    pub source_out_frame: i64,
    #[serde(default)]
    pub source_fps: f64,
    #[serde(default)]
    pub source_timebase: EditorialSourceTimebase,
    #[serde(default)]
    pub virtual_category: String,
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
    pub start_frame: i64,
    #[serde(default)]
    pub end_frame: i64,
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
    pub timeline_frame: i64,
    #[serde(default)]
    pub part_id: String,
    #[serde(default)]
    pub label: String,
}
