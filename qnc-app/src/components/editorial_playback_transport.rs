//! Editorial playback transport component.
//!
//! The form supplies a snapshot of UI state and editorial data. This component
//! decides which broadcast-player transport intent should be emitted and
//! delegates playlist-input request construction to the editorial playback
//! component.

use super::{EditorialProgramPlaybackComponent, EditorialProgramPlaybackInput};
use crate::api::EditorialPlaylist;
use crate::editorial::segment_program::SegmentProgramModel;
use crate::editorial::types::{MarkerSlot, StoryCover, StoryMarker, StoryPart, StoryShot};
use crate::playback_routing::PlaybackTransportIntent;
use crate::player_remote::BroadcastProgramOpenRequest;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditorialPlaybackView {
    Source,
    Wrap,
}

pub(crate) struct EditorialTogglePlayInput<'a> {
    pub source_dock_keyboard_focus: bool,
    pub view_mode: EditorialPlaybackView,
    pub story_playing: bool,
    pub playlist_input_active: bool,
    pub playlist_input_playing: bool,
    pub playlist_program: EditorialPlaylistProgramInput<'a>,
}

pub(crate) struct EditorialPlaylistProgramInput<'a> {
    pub project_id: &'a str,
    pub program_id: &'a str,
    pub start_program_frame: i64,
    pub playlist: Option<&'a EditorialPlaylist>,
    pub marker_slots: &'a [MarkerSlot],
    pub covers: &'a [StoryCover],
    pub markers: &'a [StoryMarker],
    pub all_clips: &'a [StoryShot],
    pub virtual_shots: &'a [StoryShot],
    pub cover_shots: &'a [StoryShot],
    pub playback_inputs: &'a HashMap<String, String>,
}

pub(crate) struct EditorialTogglePlayOutcome {
    pub intent: PlaybackTransportIntent,
    pub view_mode: Option<EditorialPlaybackView>,
    pub playing: Option<bool>,
    pub status: Option<String>,
    pub selected_part_id: Option<String>,
}

pub(crate) struct EditorialWrapSessionInput<'a> {
    pub selected_part_id: Option<&'a str>,
    pub current_wrap_playhead_frame: i64,
    pub playlist: Option<&'a EditorialPlaylist>,
}

pub(crate) struct EditorialWrapSessionOutcome {
    pub selected_part_id: Option<String>,
    pub wrap_playhead_frame: i64,
}

pub(crate) struct EditorialPendingWrapScrubInput<'a> {
    pub preferred_part_id: Option<&'a str>,
    pub selected_part_id: &'a str,
    pub parts: &'a [StoryPart],
}

pub(crate) struct EditorialWrapRefreshInput<'a> {
    pub meta_ready: bool,
    pub pending_part_id: Option<&'a str>,
    pub was_wrap: bool,
    pub initial_selection_done: bool,
    pub current_wrap_playhead_frame: i64,
    pub selected_part_id: &'a str,
    pub playlist: Option<&'a EditorialPlaylist>,
}

pub(crate) struct EditorialWrapRefreshOutcome {
    pub session: Option<EditorialWrapSessionOutcome>,
    pub preserve_current_playhead: bool,
    pub clear_pending_part_id: bool,
}

pub(crate) struct EditorialPlaybackTransportComponent;

impl EditorialPlaybackTransportComponent {
    pub(crate) fn build_program_request(
        input: EditorialPlaylistProgramInput<'_>,
    ) -> Result<BroadcastProgramOpenRequest, String> {
        let program = Self::segment_program_model(&input);
        Self::build_program_request_from_model(input, &program)
    }

    pub(crate) fn playlist_input_available(playlist: Option<&EditorialPlaylist>) -> bool {
        playlist.is_some_and(|playlist| {
            playlist.timeline_fps.is_finite()
                && playlist.timeline_fps > 0.0
                && playlist.duration_frames > 0
                && playlist.segments.iter().any(|segment| {
                    segment.streamable || segment.covers.iter().any(|cover| cover.streamable)
                })
        })
    }

    pub(crate) fn playlist_input_intent(
        input: EditorialPlaylistProgramInput<'_>,
        preload: bool,
    ) -> PlaybackTransportIntent {
        if !Self::playlist_input_available(input.playlist) {
            return PlaybackTransportIntent::None;
        }
        match Self::build_program_request(input) {
            Ok(request) => {
                if preload {
                    PlaybackTransportIntent::PreloadProgram(request)
                } else {
                    PlaybackTransportIntent::OpenProgram(request)
                }
            }
            Err(_) => PlaybackTransportIntent::None,
        }
    }

    pub(crate) fn required_playlist_playback_clip_ids(
        input: EditorialPlaylistProgramInput<'_>,
    ) -> Vec<String> {
        let program = Self::segment_program_model(&input);
        EditorialProgramPlaybackComponent::required_playback_clip_ids(&program)
    }

    fn build_program_request_from_model(
        input: EditorialPlaylistProgramInput<'_>,
        program: &SegmentProgramModel,
    ) -> Result<BroadcastProgramOpenRequest, String> {
        let clips = input
            .all_clips
            .iter()
            .chain(input.virtual_shots.iter())
            .chain(input.cover_shots.iter())
            .cloned()
            .collect::<Vec<_>>();
        EditorialProgramPlaybackComponent::build_program(EditorialProgramPlaybackInput {
            project_id: input.project_id,
            program_id: input.program_id,
            start_program_frame: input.start_program_frame,
            program,
            covers: input.covers,
            clips: &clips,
            playback_inputs: input.playback_inputs,
        })
    }

    fn segment_program_model(input: &EditorialPlaylistProgramInput<'_>) -> SegmentProgramModel {
        SegmentProgramModel::from_playlist(
            input.playlist,
            input.marker_slots,
            input.covers,
            input.markers,
        )
    }

    pub(crate) fn start_wrap_session(
        input: EditorialWrapSessionInput<'_>,
    ) -> EditorialWrapSessionOutcome {
        let selected_part_id = input
            .selected_part_id
            .and_then(non_empty_str)
            .map(str::to_string);
        let wrap_playhead_frame = selected_part_id
            .as_deref()
            .and_then(|part_id| playlist_segment_by_id(input.playlist, part_id))
            .map(|segment| segment.global_start_frame.max(0))
            .unwrap_or_else(|| input.current_wrap_playhead_frame.max(0));
        EditorialWrapSessionOutcome {
            selected_part_id,
            wrap_playhead_frame,
        }
    }

    pub(crate) fn pending_wrap_scrub_part_id(
        input: EditorialPendingWrapScrubInput<'_>,
    ) -> Option<String> {
        input
            .preferred_part_id
            .and_then(non_empty_str)
            .filter(|part_id| has_part(input.parts, part_id))
            .or_else(|| {
                non_empty_str(input.selected_part_id)
                    .filter(|part_id| has_part(input.parts, part_id))
            })
            .map(str::to_string)
            .or_else(|| {
                input
                    .parts
                    .iter()
                    .find(|part| part.active)
                    .map(|part| part.part_id.clone())
            })
    }

    pub(crate) fn wrap_projection_after_program_refresh(
        input: EditorialWrapRefreshInput<'_>,
    ) -> EditorialWrapRefreshOutcome {
        if !input.meta_ready {
            return EditorialWrapRefreshOutcome::none();
        }
        if let Some(part_id) = input.pending_part_id.and_then(non_empty_str) {
            return EditorialWrapRefreshOutcome {
                session: Some(Self::start_wrap_session(EditorialWrapSessionInput {
                    selected_part_id: Some(part_id),
                    current_wrap_playhead_frame: input.current_wrap_playhead_frame,
                    playlist: input.playlist,
                })),
                preserve_current_playhead: false,
                clear_pending_part_id: true,
            };
        }
        if input.was_wrap && input.initial_selection_done {
            let active_at_head =
                playlist_segment_at_frame(input.playlist, input.current_wrap_playhead_frame)
                    .map(|segment| segment.part_id.as_str());
            let selected = active_at_head.or_else(|| non_empty_str(input.selected_part_id));
            if selected.is_some() {
                return EditorialWrapRefreshOutcome {
                    session: Some(Self::start_wrap_session(EditorialWrapSessionInput {
                        selected_part_id: selected,
                        current_wrap_playhead_frame: input.current_wrap_playhead_frame,
                        playlist: input.playlist,
                    })),
                    preserve_current_playhead: true,
                    clear_pending_part_id: false,
                };
            }
        }
        EditorialWrapRefreshOutcome::none()
    }

    pub(crate) fn toggle_play(input: EditorialTogglePlayInput<'_>) -> EditorialTogglePlayOutcome {
        if input.source_dock_keyboard_focus {
            return EditorialTogglePlayOutcome {
                intent: PlaybackTransportIntent::TogglePlay,
                view_mode: (input.view_mode != EditorialPlaybackView::Source)
                    .then_some(EditorialPlaybackView::Source),
                playing: None,
                status: None,
                selected_part_id: None,
            };
        }

        if input.view_mode != EditorialPlaybackView::Wrap {
            return EditorialTogglePlayOutcome::intent(PlaybackTransportIntent::TogglePlay);
        }

        if input.story_playing || input.playlist_input_playing {
            return EditorialTogglePlayOutcome {
                intent: if input.playlist_input_active {
                    PlaybackTransportIntent::Pause
                } else {
                    PlaybackTransportIntent::None
                },
                view_mode: None,
                playing: Some(false),
                status: Some("Pauza playlist inputa".into()),
                selected_part_id: None,
            };
        }

        if input.playlist_input_active {
            return EditorialTogglePlayOutcome::playlist(
                PlaybackTransportIntent::PlayLoadedInput,
                "Playlist input play",
            );
        }

        let program = Self::segment_program_model(&input.playlist_program);
        let selected_part_id = program
            .active_part_at_program_frame(input.playlist_program.start_program_frame)
            .map(|segment| segment.part_id.clone());
        let request = Self::build_program_request_from_model(input.playlist_program, &program);

        match request {
            Ok(request) => EditorialTogglePlayOutcome {
                intent: PlaybackTransportIntent::PlayProgram(request),
                view_mode: None,
                playing: Some(true),
                status: Some("Playlist input play".into()),
                selected_part_id,
            },
            Err(error) => EditorialTogglePlayOutcome {
                intent: PlaybackTransportIntent::None,
                view_mode: None,
                playing: Some(false),
                status: Some(error),
                selected_part_id: None,
            },
        }
    }
}

impl EditorialWrapRefreshOutcome {
    fn none() -> Self {
        Self {
            session: None,
            preserve_current_playhead: false,
            clear_pending_part_id: false,
        }
    }
}

impl EditorialTogglePlayOutcome {
    fn intent(intent: PlaybackTransportIntent) -> Self {
        Self {
            intent,
            view_mode: None,
            playing: None,
            status: None,
            selected_part_id: None,
        }
    }

    fn playlist(intent: PlaybackTransportIntent, status: &'static str) -> Self {
        Self {
            intent,
            view_mode: None,
            playing: Some(true),
            status: Some(status.into()),
            selected_part_id: None,
        }
    }
}

fn non_empty_str(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

fn has_part(parts: &[StoryPart], part_id: &str) -> bool {
    parts
        .iter()
        .any(|part| part.active && part.part_id == part_id)
}

fn playlist_segment_by_id<'a>(
    playlist: Option<&'a EditorialPlaylist>,
    part_id: &str,
) -> Option<&'a crate::api::EditorialPlaylistSegment> {
    let part_id = non_empty_str(part_id)?;
    playlist?
        .segments
        .iter()
        .find(|segment| segment.part_id == part_id)
}

fn playlist_segment_at_frame(
    playlist: Option<&EditorialPlaylist>,
    frame: i64,
) -> Option<&crate::api::EditorialPlaylistSegment> {
    let frame = frame.max(0);
    playlist?.segments.iter().find(|segment| {
        let start = segment.global_start_frame.max(0);
        let end = segment
            .global_end_frame
            .max(start + segment.duration_frames.max(0))
            .max(start + 1);
        frame >= start && frame < end
    })
}
