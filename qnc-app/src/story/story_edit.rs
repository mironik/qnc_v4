//! Native Story edit target helpers.

use super::{MarkerSlot, StoryCover};

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
}

pub(super) fn quick_cover_target(
    selected_slot_id: &str,
    marker_slots: &[MarkerSlot],
) -> Result<CoverTarget, String> {
    let selected_slot_id = selected_slot_id.trim();
    let selected_empty_slot = marker_slots.iter().find(|slot| {
        slot.slot_id == selected_slot_id && !slot.has_cover && !slot.slot_id.trim().is_empty()
    });
    let slot_id = if let Some(slot) = selected_empty_slot {
        slot.slot_id.clone()
    } else {
        first_empty_marker_slot(marker_slots)
            .map(|slot| slot.slot_id.clone())
            .unwrap_or_default()
    };

    if slot_id.trim().is_empty() {
        return Err("Nema marker slota za pokrivalicu".into());
    }

    Ok(CoverTarget { slot_id })
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
        first_empty_marker_slot(marker_slots)
            .map(|slot| slot.slot_id.clone())
            .unwrap_or_default()
    };

    if slot_id.trim().is_empty() {
        return Err("Odaberi marker slot za pokrivalicu".into());
    }

    Ok(CoverTarget { slot_id })
}

pub(super) fn cover_source_range(
    selected_clip_id: &str,
    mark_in_set: bool,
    mark_out_set: bool,
    source_in_frame: i64,
    source_out_frame: i64,
    source_fps: Option<f64>,
) -> Result<CoverSourceRange, String> {
    let clip_id = selected_clip_id.trim();
    if clip_id.is_empty() {
        return Err("Odaberi source klip za pokrivalicu".into());
    }
    let fps = source_fps.ok_or_else(|| "Source FPS još nije potvrđen".to_string())?;
    if !mark_in_set || !mark_out_set {
        return Err("Pokrivalica treba IN/OUT na source klipu".into());
    }
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
    })
}

fn first_empty_marker_slot(marker_slots: &[MarkerSlot]) -> Option<&MarkerSlot> {
    marker_slots
        .iter()
        .find(|slot| !slot.has_cover && !slot.slot_id.trim().is_empty())
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
    fn quick_cover_skips_selected_filled_slot() {
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

        let target = quick_cover_target("slot_a", &slots).unwrap();

        assert_eq!(target.slot_id, "slot_b");
    }

    #[test]
    fn quick_cover_uses_first_empty_slot_when_none_selected() {
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

        let target = quick_cover_target("", &slots).unwrap();

        assert_eq!(target.slot_id, "slot_b");
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

        assert!(err.contains("marker slota"));
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
    fn cover_source_range_requires_source_marks() {
        let err = cover_source_range("clip_a", true, false, 10, 20, Some(50.0)).unwrap_err();

        assert!(err.contains("IN/OUT"));
    }

    #[test]
    fn cover_source_range_keeps_source_fps_frames() {
        let range = cover_source_range("clip_a", true, true, 100, 160, Some(50.0)).unwrap();

        assert_eq!(range.clip_id, "clip_a");
        assert_eq!(range.in_frame, 100);
        assert_eq!(range.out_frame, 160);
        assert_eq!(range.fps, 50.0);
    }
}
