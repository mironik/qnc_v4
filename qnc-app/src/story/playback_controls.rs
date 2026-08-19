//! Native Story playback controls.
//!
//! Input-only helper: maps loaded keyboard bindings to typed playback actions.
//! StoryScreen emits commands; app/player orchestration executes them.
//!
//! Dedup rules (avoid double-fire):
//! - ignore egui key-repeat except for SeekFrames (hold ←/→ to nudge)
//! - at most one non-seek command per frame
//! - at most one SeekFrames per frame

use eframe::egui;

use crate::shortcuts::StoryBindings;

/// Frame delta for catalog actions `step_back_frame` / `step_forward_frame`.
pub(crate) const SEEK_STEP_FRAMES: i64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaybackAction {
    TogglePlay,
    MarkIn,
    MarkOut,
    SelectMarkIn,
    SelectMarkOut,
    FocusNext,
    FocusPrev,
    ClearFocus,
    QuickCover,
    OverwriteCover,
    StepPrevPart,
    StepNextPart,
    StepPrevMarkerSlot,
    StepNextMarkerSlot,
    FocusEmptySlot,
    MarkInFitDuration,
    DeleteSelection,
    /// Catalog `add_marker` / `add_marker_continue`.
    AddMarker,
    /// Catalog `add_ton_segment` (Talking Head / Shift+T).
    AddTonSegment,
    /// Catalog `add_off_segment` (Voice over / Shift+V).
    AddOffSegment,
    /// Frame step / nudge — StoryScreen routes by timeline focus.
    SeekFrames(i64),
}

/// Resolve pressed keys → actions **only** from loaded QNC keyboard-shortcuts bindings.
pub(crate) fn shortcut_actions(
    ctx: &egui::Context,
    bindings: &StoryBindings,
) -> Vec<PlaybackAction> {
    let mut actions = Vec::new();
    ctx.input(|i| {
        let mut took_command = false;
        let mut took_seek = false;
        for event in &i.events {
            let egui::Event::Key {
                key,
                pressed: true,
                repeat,
                modifiers,
                ..
            } = event
            else {
                continue;
            };
            let matches = bindings.matching_actions(*key, *modifiers);
            let Some(action) = resolve_playback_action(&matches) else {
                continue;
            };
            let is_seek = matches!(action, PlaybackAction::SeekFrames(_));
            if *repeat && !is_seek {
                continue;
            }
            if is_seek {
                if took_seek {
                    continue;
                }
                actions.push(action);
                took_seek = true;
            } else if !took_command {
                actions.push(action);
                took_command = true;
            }
        }
    });
    actions
}

fn resolve_playback_action(matches: &[&str]) -> Option<PlaybackAction> {
    if matches.is_empty() {
        return None;
    }
    // Prefer more specific catalog actions when several match the same chord.
    const ORDER: &[&str] = &[
        "select_mark_in",
        "select_mark_out",
        "mark_in",
        "mark_out",
        "focus_next",
        "focus_prev",
        "clear_focus",
        "step_prev_part",
        "step_next_part",
        "step_prev_marker_slot",
        "step_next_marker_slot",
        "focus_empty_slot",
        "mark_in_fit_duration",
        "delete_marker",
        "delete_part",
        "play_pause",
        "add_marker_continue",
        "add_marker",
        "overwrite_cover",
        "quick_overwrite_cover",
        "add_ton_segment",
        "add_off_segment",
        "step_back_frame",
        "step_forward_frame",
    ];
    for id in ORDER {
        if matches.iter().any(|m| *m == *id) {
            return playback_action_from_id(id);
        }
    }
    None
}

fn playback_action_from_id(id: &str) -> Option<PlaybackAction> {
    match id {
        "play_pause" => Some(PlaybackAction::TogglePlay),
        "mark_in" => Some(PlaybackAction::MarkIn),
        "mark_out" => Some(PlaybackAction::MarkOut),
        "select_mark_in" => Some(PlaybackAction::SelectMarkIn),
        "select_mark_out" => Some(PlaybackAction::SelectMarkOut),
        "focus_next" => Some(PlaybackAction::FocusNext),
        "focus_prev" => Some(PlaybackAction::FocusPrev),
        "clear_focus" => Some(PlaybackAction::ClearFocus),
        "step_prev_part" => Some(PlaybackAction::StepPrevPart),
        "step_next_part" => Some(PlaybackAction::StepNextPart),
        "step_prev_marker_slot" => Some(PlaybackAction::StepPrevMarkerSlot),
        "step_next_marker_slot" => Some(PlaybackAction::StepNextMarkerSlot),
        "focus_empty_slot" => Some(PlaybackAction::FocusEmptySlot),
        "mark_in_fit_duration" => Some(PlaybackAction::MarkInFitDuration),
        "delete_marker" | "delete_part" => Some(PlaybackAction::DeleteSelection),
        "add_marker" | "add_marker_continue" => Some(PlaybackAction::AddMarker),
        "quick_overwrite_cover" => Some(PlaybackAction::QuickCover),
        "overwrite_cover" => Some(PlaybackAction::OverwriteCover),
        "add_ton_segment" => Some(PlaybackAction::AddTonSegment),
        "add_off_segment" => Some(PlaybackAction::AddOffSegment),
        "step_back_frame" => Some(PlaybackAction::SeekFrames(-SEEK_STEP_FRAMES)),
        "step_forward_frame" => Some(PlaybackAction::SeekFrames(SEEK_STEP_FRAMES)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shortcuts::StoryBindings;
    use serde_json::json;

    fn storyboard_bindings() -> StoryBindings {
        let catalog = json!({
            "active_preset": "default",
            "presets": {
                "default": {
                    "storyboard": {
                        "mark_in": [{ "code": "KeyI", "shift": false }],
                        "mark_out": [{ "code": "KeyO", "shift": false }],
                        "play_pause": [{ "code": "Space" }],
                        "select_mark_in": [{ "code": "KeyI", "ctrl": true, "shift": false }],
                        "select_mark_out": [{ "code": "KeyO", "ctrl": true, "shift": false }],
                        "focus_next": [{ "key": "Tab", "shift": false }],
                        "focus_prev": [{ "key": "Tab", "shift": true }],
                        "clear_focus": [{ "key": "Escape" }],
                        "add_marker": [{ "key": "m" }],
                        "quick_overwrite_cover": [{ "code": "KeyB", "shift": false, "ctrl": false }],
                        "overwrite_cover": [{ "code": "KeyB", "shift": true, "ctrl": false }],
                        "step_prev_part": [{ "key": "ArrowUp" }],
                        "step_next_part": [{ "key": "ArrowDown" }],
                        "step_prev_marker_slot": [{ "code": "BracketLeft" }],
                        "step_next_marker_slot": [{ "code": "BracketRight" }],
                        "focus_empty_slot": [{ "code": "KeyS", "shift": true, "ctrl": false }],
                        "mark_in_fit_duration": [{ "code": "KeyI", "shift": true, "ctrl": false }],
                        "delete_part": [{ "key": "Delete" }],
                        "add_ton_segment": [{ "code": "KeyT", "shift": true, "ctrl": false }],
                        "add_off_segment": [{ "code": "KeyV", "shift": true, "ctrl": false }],
                        "step_forward_frame": [{ "key": "ArrowRight" }]
                    }
                }
            }
        });
        StoryBindings::from_catalog(&catalog, &serde_json::Value::Null, "storyboard")
    }

    #[test]
    fn resolves_mark_in_from_catalog_action() {
        let bindings = storyboard_bindings();
        let matches = bindings.matching_actions(egui::Key::I, egui::Modifiers::NONE);
        assert_eq!(
            resolve_playback_action(&matches),
            Some(PlaybackAction::MarkIn)
        );
        let matches = bindings.matching_actions(egui::Key::O, egui::Modifiers::NONE);
        assert_eq!(
            resolve_playback_action(&matches),
            Some(PlaybackAction::MarkOut)
        );
        let matches = bindings.matching_actions(egui::Key::Space, egui::Modifiers::NONE);
        assert_eq!(
            resolve_playback_action(&matches),
            Some(PlaybackAction::TogglePlay)
        );
    }

    #[test]
    fn resolves_select_mark_and_focus() {
        let bindings = storyboard_bindings();
        let mut ctrl = egui::Modifiers::NONE;
        ctrl.ctrl = true;
        assert_eq!(
            resolve_playback_action(&bindings.matching_actions(egui::Key::I, ctrl)),
            Some(PlaybackAction::SelectMarkIn)
        );
        assert_eq!(
            resolve_playback_action(&bindings.matching_actions(egui::Key::O, ctrl)),
            Some(PlaybackAction::SelectMarkOut)
        );
        assert_eq!(
            resolve_playback_action(
                &bindings.matching_actions(egui::Key::Tab, egui::Modifiers::NONE)
            ),
            Some(PlaybackAction::FocusNext)
        );
        let mut shift = egui::Modifiers::NONE;
        shift.shift = true;
        assert_eq!(
            resolve_playback_action(&bindings.matching_actions(egui::Key::Tab, shift)),
            Some(PlaybackAction::FocusPrev)
        );
        assert_eq!(
            resolve_playback_action(
                &bindings.matching_actions(egui::Key::Escape, egui::Modifiers::NONE)
            ),
            Some(PlaybackAction::ClearFocus)
        );
        assert_eq!(
            resolve_playback_action(
                &bindings.matching_actions(egui::Key::ArrowRight, egui::Modifiers::NONE)
            ),
            Some(PlaybackAction::SeekFrames(1))
        );
    }

    #[test]
    fn select_mark_preferred_over_mark_when_ctrl() {
        let bindings = storyboard_bindings();
        let mut ctrl = egui::Modifiers::NONE;
        ctrl.ctrl = true;
        // Only select_mark_in should match; mark_in requires shift:false ctrl:false.
        let matches = bindings.matching_actions(egui::Key::I, ctrl);
        assert_eq!(matches, vec!["select_mark_in"]);
        assert_eq!(
            resolve_playback_action(&matches),
            Some(PlaybackAction::SelectMarkIn)
        );
    }

    #[test]
    fn resolves_add_marker_from_catalog_action() {
        let bindings = storyboard_bindings();
        assert_eq!(
            resolve_playback_action(
                &bindings.matching_actions(egui::Key::M, egui::Modifiers::NONE)
            ),
            Some(PlaybackAction::AddMarker)
        );
    }

    #[test]
    fn resolves_quick_and_overwrite_cover_from_catalog_actions() {
        let bindings = storyboard_bindings();
        assert_eq!(
            resolve_playback_action(
                &bindings.matching_actions(egui::Key::B, egui::Modifiers::NONE)
            ),
            Some(PlaybackAction::QuickCover)
        );

        let mut shift = egui::Modifiers::NONE;
        shift.shift = true;
        assert_eq!(
            resolve_playback_action(&bindings.matching_actions(egui::Key::B, shift)),
            Some(PlaybackAction::OverwriteCover)
        );
    }

    #[test]
    fn resolves_add_ton_and_off_segments() {
        let bindings = storyboard_bindings();
        let mut shift = egui::Modifiers::NONE;
        shift.shift = true;
        assert_eq!(
            resolve_playback_action(&bindings.matching_actions(egui::Key::T, shift)),
            Some(PlaybackAction::AddTonSegment)
        );
        assert_eq!(
            resolve_playback_action(&bindings.matching_actions(egui::Key::V, shift)),
            Some(PlaybackAction::AddOffSegment)
        );
    }

    #[test]
    fn resolves_core_story_shortcuts_from_seed_presets() {
        let catalog: serde_json::Value =
            serde_json::from_str(include_str!("../../../seed/keyboard-shortcuts.json"))
                .expect("valid keyboard shortcut seed");
        let presets = catalog
            .get("presets")
            .and_then(|v| v.as_object())
            .expect("seed presets");
        let mut shift = egui::Modifiers::NONE;
        shift.shift = true;

        for preset_id in presets.keys() {
            let user = json!({ "active_preset": preset_id });
            let bindings = StoryBindings::from_catalog(&catalog, &user, "storyboard");

            assert_eq!(
                resolve_playback_action(
                    &bindings.matching_actions(egui::Key::Space, egui::Modifiers::NONE)
                ),
                Some(PlaybackAction::TogglePlay),
                "play_pause missing for preset {preset_id}"
            );
            assert_eq!(
                resolve_playback_action(&bindings.matching_actions(egui::Key::B, shift)),
                Some(PlaybackAction::OverwriteCover),
                "overwrite_cover missing for preset {preset_id}"
            );
            assert_eq!(
                resolve_playback_action(&bindings.matching_actions(egui::Key::T, shift)),
                Some(PlaybackAction::AddTonSegment),
                "add_ton_segment missing for preset {preset_id}"
            );
            assert_eq!(
                resolve_playback_action(&bindings.matching_actions(egui::Key::V, shift)),
                Some(PlaybackAction::AddOffSegment),
                "add_off_segment missing for preset {preset_id}"
            );
        }
    }

    #[test]
    fn resolves_story_playlist_input_actions_from_catalog() {
        let bindings = storyboard_bindings();

        assert_eq!(
            resolve_playback_action(
                &bindings.matching_actions(egui::Key::ArrowUp, egui::Modifiers::NONE)
            ),
            Some(PlaybackAction::StepPrevPart)
        );
        assert_eq!(
            resolve_playback_action(
                &bindings.matching_actions(egui::Key::ArrowDown, egui::Modifiers::NONE)
            ),
            Some(PlaybackAction::StepNextPart)
        );
        assert_eq!(
            resolve_playback_action(
                &bindings.matching_actions(egui::Key::OpenBracket, egui::Modifiers::NONE)
            ),
            Some(PlaybackAction::StepPrevMarkerSlot)
        );
        assert_eq!(
            resolve_playback_action(
                &bindings.matching_actions(egui::Key::CloseBracket, egui::Modifiers::NONE)
            ),
            Some(PlaybackAction::StepNextMarkerSlot)
        );

        let mut shift = egui::Modifiers::NONE;
        shift.shift = true;
        assert_eq!(
            resolve_playback_action(&bindings.matching_actions(egui::Key::S, shift)),
            Some(PlaybackAction::FocusEmptySlot)
        );
        assert_eq!(
            resolve_playback_action(&bindings.matching_actions(egui::Key::I, shift)),
            Some(PlaybackAction::MarkInFitDuration)
        );
        assert_eq!(
            resolve_playback_action(
                &bindings.matching_actions(egui::Key::Delete, egui::Modifiers::NONE)
            ),
            Some(PlaybackAction::DeleteSelection)
        );
    }
}
