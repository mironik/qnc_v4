//! Story playback transport policy.
//!
//! The Story form supplies a snapshot of UI state and editorial data. This
//! module decides which broadcast-player transport intent should be emitted.

use crate::components::{EditorialProgramPlaybackComponent, EditorialProgramPlaybackInput};
use crate::editorial::segment_program::SegmentProgramModel;
use crate::editorial::types::{StoryCover, StoryShot};
use crate::playback_routing::PlaybackTransportIntent;
use crate::player_remote::BroadcastProgramOpenRequest;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoryPlaybackView {
    Source,
    Wrap,
}

pub(crate) struct StoryTogglePlayInput<'a> {
    pub source_dock_keyboard_focus: bool,
    pub view_mode: StoryPlaybackView,
    pub story_playing: bool,
    pub playlist_input_active: bool,
    pub playlist_input_playing: bool,
    pub playlist_program: StoryPlaylistProgramInput<'a>,
}

pub(crate) struct StoryPlaylistProgramInput<'a> {
    pub project_id: &'a str,
    pub program_id: &'a str,
    pub start_program_frame: i64,
    pub program: &'a SegmentProgramModel,
    pub covers: &'a [StoryCover],
    pub all_clips: &'a [StoryShot],
    pub virtual_shots: &'a [StoryShot],
}

pub(crate) fn build_program_request(
    input: StoryPlaylistProgramInput<'_>,
) -> Result<BroadcastProgramOpenRequest, String> {
    let clips = input
        .all_clips
        .iter()
        .chain(input.virtual_shots.iter())
        .cloned()
        .collect::<Vec<_>>();
    EditorialProgramPlaybackComponent::build_program(EditorialProgramPlaybackInput {
        project_id: input.project_id,
        program_id: input.program_id,
        start_program_frame: input.start_program_frame,
        program: input.program,
        covers: input.covers,
        clips: &clips,
    })
}

pub(crate) struct StoryTogglePlayOutcome {
    pub intent: PlaybackTransportIntent,
    pub view_mode: Option<StoryPlaybackView>,
    pub playing: Option<bool>,
    pub status: Option<String>,
    pub selected_part_id: Option<String>,
}

impl StoryTogglePlayOutcome {
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

pub(crate) fn toggle_play(input: StoryTogglePlayInput<'_>) -> StoryTogglePlayOutcome {
    if input.source_dock_keyboard_focus {
        return StoryTogglePlayOutcome {
            intent: PlaybackTransportIntent::TogglePlay,
            view_mode: (input.view_mode != StoryPlaybackView::Source)
                .then_some(StoryPlaybackView::Source),
            playing: None,
            status: None,
            selected_part_id: None,
        };
    }

    if input.view_mode != StoryPlaybackView::Wrap {
        return StoryTogglePlayOutcome::intent(PlaybackTransportIntent::TogglePlay);
    }

    if input.story_playing || input.playlist_input_playing {
        return StoryTogglePlayOutcome {
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
        return StoryTogglePlayOutcome::playlist(
            PlaybackTransportIntent::PlayLoadedInput,
            "Playlist input play",
        );
    }

    let selected_part_id = input
        .playlist_program
        .program
        .active_part_at_program_frame(input.playlist_program.start_program_frame)
        .map(|segment| segment.part_id.clone());
    let request = build_program_request(input.playlist_program);

    match request {
        Ok(request) => StoryTogglePlayOutcome {
            intent: PlaybackTransportIntent::PlayProgram(request),
            view_mode: None,
            playing: Some(true),
            status: Some("Playlist input play".into()),
            selected_part_id,
        },
        Err(error) => StoryTogglePlayOutcome {
            intent: PlaybackTransportIntent::None,
            view_mode: None,
            playing: Some(false),
            status: Some(error),
            selected_part_id: None,
        },
    }
}
