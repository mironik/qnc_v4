//! Native Story selection/loading helpers.
//!
//! Source range calculation for selected shots. No UI.

use super::StoryShot;
use crate::editorial::common::shot_id;
use crate::player_contract::{BroadcastHostSourceError, BroadcastHostSourceRef};

pub(super) struct ShotSelection {
    pub shot_id: String,
    pub clip_id: String,
    pub source_in: f64,
    pub source_out: f64,
    pub source_ref: BroadcastHostSourceRef,
}

pub(super) fn shot_selection(
    project_id: &str,
    shot: &StoryShot,
) -> Result<ShotSelection, BroadcastHostSourceError> {
    let source_in = shot.in_seconds.unwrap_or(0.0).max(0.0);
    let duration = shot.duration_sec.max(0.0);
    let source_out = shot
        .out_seconds
        .unwrap_or(if duration > 0.0 {
            duration
        } else {
            source_in + 10.0
        })
        .max(source_in + 0.04);
    let source_ref = BroadcastHostSourceRef::from_story_fields(
        project_id,
        &shot.shot_id,
        &shot.root_shot_id,
        &shot.clip_id,
        shot.in_seconds,
        shot.out_seconds,
        shot.duration_sec,
    )?;

    Ok(ShotSelection {
        shot_id: shot_id(shot),
        clip_id: shot.clip_id.clone(),
        source_in,
        source_out,
        source_ref,
    })
}
