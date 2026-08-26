//! Native Story edit target helpers.

use super::{MarkerSlot, StoryCover};
use crate::player_contract::BroadcastSourceTimebase;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CoverTarget {
    pub slot_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct CoverSourceRange {
    pub clip_id: String,
    pub in_frame: i64,
    pub out_frame: i64,
    pub fps: f64,
    pub source_timebase: BroadcastSourceTimebase,
}

pub(super) fn quick_cover_target(
    selected_slot_id: &str,
    marker_slots: &[MarkerSlot],
) -> Result<CoverTarget, String> {
    let selected_slot_id = selected_slot_id.trim();
    if selected_slot_id.is_empty() {
        return Err("Odaberi marker slot za pokrivalicu".into());
    }
    let Some(slot) = marker_slots
        .iter()
        .find(|slot| slot.slot_id == selected_slot_id && !slot.slot_id.trim().is_empty())
    else {
        return Err("Odabrani marker slot nije dostupan".into());
    };
    if slot.has_cover {
        return Err("Odabrani marker slot već ima pokrivalicu".into());
    }

    Ok(CoverTarget {
        slot_id: slot.slot_id.clone(),
    })
}

pub(super) fn overwrite_cover_target(
    selected_slot_id: &str,
    selected_cover_id: &str,
    marker_slots: &[MarkerSlot],
    covers: &[StoryCover],
) -> Result<CoverTarget, String> {
    let slot_id = if !selected_slot_id.trim().is_empty() {
        selected_slot_id.to_string()
    } else if let Some(slot_id) = selected_cover_slot_id(selected_cover_id, covers) {
        slot_id.to_string()
    } else {
        String::new()
    };

    if slot_id.trim().is_empty() {
        return Err("Odaberi marker slot za pokrivalicu".into());
    }
    if !marker_slots.is_empty() && marker_slots.iter().all(|slot| slot.slot_id != slot_id) {
        return Err("Odabrani marker slot nije dostupan".into());
    }

    Ok(CoverTarget { slot_id })
}

pub(super) fn cover_source_range(
    selected_clip_id: &str,
    _mark_in_set: bool,
    _mark_out_set: bool,
    source_in_frame: i64,
    source_out_frame: i64,
    source_fps: Option<f64>,
    source_timebase: Option<BroadcastSourceTimebase>,
) -> Result<CoverSourceRange, String> {
    let clip_id = selected_clip_id.trim();
    if clip_id.is_empty() {
        return Err("Odaberi source klip za pokrivalicu".into());
    }
    let fps = source_fps.ok_or_else(|| "Source FPS još nije potvrđen".to_string())?;
    let source_timebase = source_timebase
        .filter(|timebase| timebase.is_valid())
        .ok_or_else(|| "Source timebase još nije potvrđen".to_string())?;
    let in_frame = source_in_frame.max(0);
    let out_frame = source_out_frame.max(in_frame + 1);
    if out_frame <= in_frame {
        return Err("OUT mora biti poslije IN".into());
    }

    Ok(CoverSourceRange {
        clip_id: clip_id.to_string(),
        in_frame,
        out_frame,
        fps,
        source_timebase,
    })
}

fn selected_cover_slot_id<'a>(
    selected_cover_id: &str,
    covers: &'a [StoryCover],
) -> Option<&'a str> {
    let selected_cover_id = selected_cover_id.trim();
    if selected_cover_id.is_empty() {
        return None;
    }
    covers
        .iter()
        .find(|cover| cover.cover_id == selected_cover_id)
        .map(|cover| cover.slot_id.as_str())
        .filter(|slot_id| !slot_id.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_cover_uses_selected_empty_slot() {
        let slots = vec![
            MarkerSlot {
                slot_id: "slot_a".into(),
                has_cover: false,
                ..MarkerSlot::default()
            },
            MarkerSlot {
                slot_id: "slot_b".into(),
                has_cover: false,
                ..MarkerSlot::default()
            },
        ];

        let target = quick_cover_target("slot_a", &slots).unwrap();

        assert_eq!(target.slot_id, "slot_a");
    }

    #[test]
    fn quick_cover_rejects_selected_filled_slot() {
        let slots = vec![
            MarkerSlot {
                slot_id: "slot_a".into(),
                has_cover: true,
                ..MarkerSlot::default()
            },
            MarkerSlot {
                slot_id: "slot_b".into(),
                has_cover: false,
                ..MarkerSlot::default()
            },
        ];

        let err = quick_cover_target("slot_a", &slots).unwrap_err();

        assert!(err.contains("već ima pokrivalicu"));
    }

    #[test]
    fn quick_cover_requires_selected_slot_when_empty_slot_exists() {
        let slots = vec![
            MarkerSlot {
                slot_id: "slot_a".into(),
                start_frame: 0,
                end_frame: 50,
                has_cover: true,
                ..MarkerSlot::default()
            },
            MarkerSlot {
                slot_id: "slot_b".into(),
                start_frame: 50,
                end_frame: 100,
                has_cover: false,
                ..MarkerSlot::default()
            },
        ];

        let err = quick_cover_target("", &slots).unwrap_err();

        assert!(err.contains("Odaberi marker slot"));
    }

    #[test]
    fn quick_cover_requires_slot() {
        let slots = vec![MarkerSlot {
            slot_id: "slot_a".into(),
            start_frame: 10,
            end_frame: 20,
            has_cover: true,
            ..MarkerSlot::default()
        }];

        let err = quick_cover_target("", &slots).unwrap_err();

        assert!(err.contains("Odaberi marker slot"));
    }

    #[test]
    fn overwrite_cover_uses_selected_cover_slot_when_slot_not_selected() {
        let covers = vec![StoryCover {
            cover_id: "cover_a".into(),
            slot_id: "slot_a".into(),
            ..StoryCover::default()
        }];

        let target = overwrite_cover_target("", "cover_a", &[], &covers).unwrap();

        assert_eq!(target.slot_id, "slot_a");
    }

    #[test]
    fn cover_source_range_uses_selected_source_range_without_manual_marks() {
        let range = cover_source_range(
            "clip_a",
            false,
            false,
            10,
            20,
            Some(50.0),
            BroadcastSourceTimebase::from_i64(50, 1),
        )
        .unwrap();

        assert_eq!(range.in_frame, 10);
        assert_eq!(range.out_frame, 20);
    }

    #[test]
    fn cover_source_range_accepts_out_only_override() {
        let range = cover_source_range(
            "clip_a",
            false,
            true,
            10,
            40,
            Some(50.0),
            BroadcastSourceTimebase::from_i64(50, 1),
        )
        .unwrap();

        assert_eq!(range.in_frame, 10);
        assert_eq!(range.out_frame, 40);
    }

    #[test]
    fn cover_source_range_keeps_source_fps_frames() {
        let range = cover_source_range(
            "clip_a",
            true,
            true,
            100,
            160,
            Some(50.0),
            BroadcastSourceTimebase::from_i64(50, 1),
        )
        .unwrap();

        assert_eq!(range.clip_id, "clip_a");
        assert_eq!(range.in_frame, 100);
        assert_eq!(range.out_frame, 160);
        assert_eq!(range.fps, 50.0);
        assert_eq!(
            range.source_timebase,
            BroadcastSourceTimebase {
                fps_num: 50,
                fps_den: 1,
            }
        );
    }
}
