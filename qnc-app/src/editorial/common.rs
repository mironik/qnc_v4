//! Common helpers for shared editorial modules — re-exports shared theme helpers.

use crate::editorial::types::StoryShot;

pub(crate) use crate::qnc_theme::{action_btn, truncate};

pub(crate) fn shot_id(shot: &StoryShot) -> String {
    if !shot.shot_id.trim().is_empty() {
        shot.shot_id.clone()
    } else if !shot.clip_id.trim().is_empty() {
        shot.clip_id.clone()
    } else {
        String::new()
    }
}
