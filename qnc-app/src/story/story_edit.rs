//! Native Story edit action helpers.
//!
//! Small host-edit wrapper layer plus target selection helpers. StoryScreen
//! remains responsible for applying returned state and restarting playback.

use serde_json::Value;

use crate::api::HostClient;

use super::{MarkerSlot, StoryShot};
use crate::editorial::common::shot_id;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CoverTarget {
    pub slot_id: String,
    pub clip_id: Option<String>,
    pub virtual_shot_id: Option<String>,
}

pub(super) fn mark_part_in(
    host: &HostClient,
    project_id: &str,
    part_id: &str,
    local_sec: f64,
) -> Result<Value, String> {
    host.story_part_mark_in(project_id, part_id, local_sec)
}

pub(super) fn mark_part_out(
    host: &HostClient,
    project_id: &str,
    part_id: &str,
    local_sec: f64,
) -> Result<Value, String> {
    host.story_part_mark_out(project_id, part_id, local_sec)
}

pub(super) fn save_virtual_shot(
    host: &HostClient,
    project_id: &str,
    clip_id: &str,
    source_in: f64,
    source_out: f64,
) -> Result<Value, String> {
    if clip_id.trim().is_empty() {
        return Err("Odaberi klip u All".into());
    }
    host.story_virtual_shot_create(
        project_id,
        clip_id,
        source_in,
        source_out.max(source_in + 0.04),
    )
}

pub(super) fn create_part(
    host: &HostClient,
    project_id: &str,
    kind: &str,
    selected_shot_id: &str,
    virtual_shots: &[StoryShot],
) -> Result<Value, String> {
    let shot = part_source_shot_id(selected_shot_id, virtual_shots)
        .ok_or_else(|| "Najprije Spremi virtualni kadar (Virtual tab)".to_string())?;
    host.story_part_create(project_id, kind, Some(&shot))
}

pub(super) fn commit(host: &HostClient, project_id: &str) -> Result<Value, String> {
    host.story_commit(project_id)
}

pub(super) fn create_marker(
    host: &HostClient,
    project_id: &str,
    timeline_sec: f64,
    part_id: &str,
) -> Result<Value, String> {
    host.story_marker_create(project_id, timeline_sec, part_id)
}

pub(super) fn select_marker_slot(
    host: &HostClient,
    project_id: &str,
    slot_id: &str,
) -> Result<Value, String> {
    if slot_id.trim().is_empty() {
        return Err(String::new());
    }
    host.story_marker_slot_select(project_id, slot_id)
}

pub(super) fn select_cover(
    host: &HostClient,
    project_id: &str,
    cover_id: &str,
) -> Result<Value, String> {
    if cover_id.trim().is_empty() {
        return Err(String::new());
    }
    host.story_cover_select(project_id, cover_id)
}

pub(super) fn quick_cover_target(
    selected_slot_id: &str,
    marker_slots: &[MarkerSlot],
    selected_clip_id: &str,
    selected_shot_id: &str,
) -> Result<CoverTarget, String> {
    let slot_id = if !selected_slot_id.trim().is_empty() {
        selected_slot_id.to_string()
    } else {
        marker_slots
            .first()
            .map(|slot| slot.slot_id.clone())
            .unwrap_or_default()
    };

    if slot_id.trim().is_empty() {
        return Err("Nema marker slotova za cover".into());
    }

    Ok(CoverTarget {
        slot_id,
        clip_id: non_empty_string(selected_clip_id),
        virtual_shot_id: non_empty_string(selected_shot_id),
    })
}

pub(super) fn create_cover(
    host: &HostClient,
    project_id: &str,
    target: &CoverTarget,
) -> Result<Value, String> {
    host.story_cover_create(
        project_id,
        &target.slot_id,
        target.clip_id.as_deref(),
        target.virtual_shot_id.as_deref(),
    )
}

fn part_source_shot_id(selected_shot_id: &str, virtual_shots: &[StoryShot]) -> Option<String> {
    if !selected_shot_id.trim().is_empty() {
        return Some(selected_shot_id.to_string());
    }
    virtual_shots
        .first()
        .map(shot_id)
        .filter(|id| !id.is_empty())
}

fn non_empty_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
