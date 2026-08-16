//! Native Story edit target helpers.

use crate::editorial::common::shot_id;

use super::{MarkerSlot, StoryCover, StoryShot};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CoverTarget {
    pub slot_id: String,
    pub clip_id: Option<String>,
    pub virtual_shot_id: Option<String>,
}

pub(super) fn quick_cover_target(
    marker_slots: &[MarkerSlot],
    selected_shot_id: &str,
    virtual_shots: &[StoryShot],
) -> Result<CoverTarget, String> {
    let slot_id = first_empty_marker_slot(marker_slots)
        .map(|slot| slot.slot_id.clone())
        .unwrap_or_default();

    if slot_id.trim().is_empty() {
        return Err("Nema praznog marker slota; odaberi slot za overwrite".into());
    }

    cover_target(slot_id, selected_shot_id, virtual_shots)
}

pub(super) fn overwrite_cover_target(
    selected_slot_id: &str,
    selected_cover_id: &str,
    marker_slots: &[MarkerSlot],
    covers: &[StoryCover],
    selected_shot_id: &str,
    virtual_shots: &[StoryShot],
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
        return Err("Odaberi marker slot ili cover za overwrite".into());
    }

    cover_target(slot_id, selected_shot_id, virtual_shots)
}

fn cover_target(
    slot_id: String,
    selected_shot_id: &str,
    virtual_shots: &[StoryShot],
) -> Result<CoverTarget, String> {
    let selected_shot_id = selected_shot_id.trim();
    if selected_shot_id.is_empty()
        || !virtual_shots
            .iter()
            .any(|shot| shot_id(shot) == selected_shot_id)
    {
        return Err("Odaberi derived virtualni kadar u Virtual tabu za cover".into());
    }

    Ok(CoverTarget {
        slot_id,
        clip_id: None,
        virtual_shot_id: Some(selected_shot_id.to_string()),
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

    fn slot() -> MarkerSlot {
        MarkerSlot {
            slot_id: "slot_a".into(),
            ..MarkerSlot::default()
        }
    }

    fn virtual_shot() -> StoryShot {
        StoryShot {
            shot_id: "shot_virtual_a".into(),
            clip_id: "clip_a".into(),
            ..StoryShot::default()
        }
    }

    #[test]
    fn quick_cover_requires_selected_virtual_tab_shot() {
        let slots = vec![slot()];
        let shots = vec![virtual_shot()];

        let err = quick_cover_target(&slots, "root_clip_a", &shots)
            .expect_err("All/root selection must not create cover");
        assert!(err.contains("Virtual tabu"));
    }

    #[test]
    fn quick_cover_uses_virtual_shot_without_all_clip_fallback() {
        let slots = vec![slot()];
        let shots = vec![virtual_shot()];

        let target = quick_cover_target(&slots, "shot_virtual_a", &shots).unwrap();
        assert_eq!(target.slot_id, "slot_a");
        assert_eq!(target.clip_id, None);
        assert_eq!(target.virtual_shot_id.as_deref(), Some("shot_virtual_a"));
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
        let shots = vec![virtual_shot()];

        let target = quick_cover_target(&slots, "shot_virtual_a", &shots).unwrap();

        assert_eq!(target.slot_id, "slot_b");
    }

    #[test]
    fn quick_cover_requires_empty_slot_when_none_selected() {
        let slots = vec![MarkerSlot {
            slot_id: "slot_a".into(),
            start_frame: 10,
            end_frame: 20,
            has_cover: true,
            ..MarkerSlot::default()
        }];
        let shots = vec![virtual_shot()];

        let err = quick_cover_target(&slots, "shot_virtual_a", &shots).unwrap_err();

        assert!(err.contains("praznog marker slota"));
    }

    #[test]
    fn overwrite_cover_uses_selected_slot_even_when_it_has_cover() {
        let slots = vec![MarkerSlot {
            slot_id: "slot_a".into(),
            has_cover: true,
            ..MarkerSlot::default()
        }];
        let shots = vec![virtual_shot()];

        let target =
            overwrite_cover_target("slot_a", "", &slots, &[], "shot_virtual_a", &shots).unwrap();

        assert_eq!(target.slot_id, "slot_a");
    }

    #[test]
    fn overwrite_cover_uses_selected_cover_slot_when_slot_not_selected() {
        let covers = vec![StoryCover {
            cover_id: "cover_a".into(),
            slot_id: "slot_a".into(),
            ..StoryCover::default()
        }];
        let shots = vec![virtual_shot()];

        let target =
            overwrite_cover_target("", "cover_a", &[], &covers, "shot_virtual_a", &shots).unwrap();

        assert_eq!(target.slot_id, "slot_a");
    }
}
