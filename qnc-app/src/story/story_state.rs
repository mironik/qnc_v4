//! Native Story data-state helpers.
//!
//! Pure parsing/formatting for host Story JSON. No UI, playback or host calls.

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::api::TimelineModel;

use super::{MarkerSlot, StoryCover, StoryMarker, StoryPart, StoryShot};

#[derive(Debug, Clone)]
pub(super) struct StoryStateUpdate {
    pub selected_part_id: String,
    pub selected_shot_id: String,
    pub parts: Vec<StoryPart>,
    pub all_clips: Vec<StoryShot>,
    pub virtual_shots: Vec<StoryShot>,
    pub covers: Vec<StoryCover>,
    pub markers: Vec<StoryMarker>,
    pub marker_slots: Vec<MarkerSlot>,
    pub selected_cover_id: String,
    pub selected_slot_id: String,
    pub draft_status: String,
    pub story_summary: String,
}

pub(super) fn parse_state(state: &Value, timeline: Option<&TimelineModel>) -> StoryStateUpdate {
    let parts = array(state, "parts");
    let all_clips = array(state, "all_clips");
    let virtual_shots = array(state, "virtual_shots");
    let covers = array(state, "covers");
    let markers = array(state, "markers");
    let marker_slots: Vec<MarkerSlot> = array(state, "marker_slots");

    let selected_cover_id = string_field(state, "selected_cover_id");
    let mut selected_slot_id = string_field(state, "selected_slot_id");
    if selected_slot_id.is_empty() {
        if let Some(slot) = marker_slots.first() {
            selected_slot_id = slot.slot_id.clone();
        }
    }

    let draft_status = if state
        .get("committed_at")
        .and_then(|v| v.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false)
    {
        "committed"
    } else {
        "draft"
    }
    .to_string();

    let story_summary = summary(
        timeline,
        &parts,
        &all_clips,
        &virtual_shots,
        &covers,
        &markers,
    );

    StoryStateUpdate {
        selected_part_id: string_field(state, "selected_part_id"),
        selected_shot_id: string_field(state, "selected_shot_id"),
        parts,
        all_clips,
        virtual_shots,
        covers,
        markers,
        marker_slots,
        selected_cover_id,
        selected_slot_id,
        draft_status,
        story_summary,
    }
}

pub(super) fn thumbnail_queue_delta(
    clips: &[StoryShot],
    has_thumb: impl Fn(&str) -> bool,
    is_queued: impl Fn(&str) -> bool,
) -> Vec<String> {
    clips
        .iter()
        .filter_map(|clip| {
            let id = clip.clip_id.trim();
            if id.is_empty() || has_thumb(id) || is_queued(id) {
                None
            } else {
                Some(id.to_string())
            }
        })
        .collect()
}

pub(super) fn summary(
    timeline: Option<&TimelineModel>,
    parts: &[StoryPart],
    all_clips: &[StoryShot],
    virtual_shots: &[StoryShot],
    covers: &[StoryCover],
    markers: &[StoryMarker],
) -> String {
    let segs = timeline.map(|t| t.segments.len()).unwrap_or(0);
    let dur = timeline.map(|t| t.duration_sec).unwrap_or(0.0);
    format!(
        "{segs} seg · {dur:.1}s · parts={} · clips={} · virtual={} · covers={} · markers={}",
        parts.len(),
        all_clips.len(),
        virtual_shots.len(),
        covers.len(),
        markers.len()
    )
}

fn string_field(state: &Value, key: &str) -> String {
    state
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn array<T>(state: &Value, key: &str) -> Vec<T>
where
    T: DeserializeOwned,
{
    state
        .get(key)
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}
