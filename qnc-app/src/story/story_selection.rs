//! Native Story selection/loading helpers.
//!
//! Source range calculation for selected shots. No UI.

use super::StoryShot;
use crate::editorial::common::shot_id;
use crate::player_contract::{BroadcastHostSourceError, BroadcastHostSourceRef, FrameNumber};

pub(super) struct ShotSelection {
    pub shot_id: String,
    pub clip_id: String,
    pub shot_in_frame: i64,
    pub shot_out_frame: i64,
    pub source_ref: BroadcastHostSourceRef,
}

pub(super) fn shot_selection(
    project_id: &str,
    shot: &StoryShot,
    source_duration_frames: Option<i64>,
) -> Result<ShotSelection, BroadcastHostSourceError> {
    if !shot.fps.is_finite() || shot.fps <= 0.0 {
        return Err(BroadcastHostSourceError::new(
            "source selection requires valid FPS",
        ));
    }
    let shot_in_frame = shot.in_frame.max(0);
    let shot_out_frame = if shot.out_frame > shot_in_frame {
        shot.out_frame
    } else if shot.duration_frames > shot_in_frame {
        shot.duration_frames
    } else {
        return Err(BroadcastHostSourceError::new(
            "source selection requires frame range",
        ));
    };
    let duration_frames = source_duration_frames
        .unwrap_or(0)
        .max(shot.duration_frames)
        .max(shot.out_frame)
        .max(shot_out_frame)
        .max(1);
    let source_ref = BroadcastHostSourceRef::from_frame_fields(
        project_id,
        &shot.shot_id,
        &shot.root_shot_id,
        &shot.clip_id,
        Some(FrameNumber(0)),
        Some(FrameNumber(duration_frames)),
        FrameNumber(duration_frames),
    )?;

    Ok(ShotSelection {
        shot_id: shot_id(shot),
        clip_id: shot.clip_id.clone(),
        shot_in_frame,
        shot_out_frame,
        source_ref,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_keeps_db_shot_range_separate_from_player_source_range() {
        let shot = StoryShot {
            shot_id: "derived_a".into(),
            root_shot_id: "clip_a_root".into(),
            clip_id: "clip_a".into(),
            duration_sec: 10.0,
            fps: 25.0,
            in_seconds: Some(2.0),
            out_seconds: Some(4.0),
            in_frame: 50,
            out_frame: 100,
            duration_frames: 50,
            ..StoryShot::default()
        };

        let selection = shot_selection("project_a", &shot, Some(250)).unwrap();

        assert_eq!(selection.shot_in_frame, 50);
        assert_eq!(selection.shot_out_frame, 100);
        assert_eq!(selection.source_ref.in_frame, Some(FrameNumber(0)));
        assert_eq!(selection.source_ref.out_frame, Some(FrameNumber(250)));
    }
}
