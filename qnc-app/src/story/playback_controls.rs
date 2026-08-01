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

pub(super) const SEEK_STEP_FRAMES: i64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlaybackAction {
    TogglePlay,
    MarkIn,
    MarkOut,
    SelectMarkIn,
    SelectMarkOut,
    FocusNext,
    FocusPrev,
    ClearFocus,
    QuickCover,
    /// Frame step / nudge — StoryScreen routes by timeline focus.
    SeekFrames(i64),
}

pub(super) fn shortcut_actions(
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
        "play_pause",
        "quick_overwrite_cover",
        "overwrite_cover",
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
        "quick_overwrite_cover" | "overwrite_cover" => Some(PlaybackAction::QuickCover),
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
}
