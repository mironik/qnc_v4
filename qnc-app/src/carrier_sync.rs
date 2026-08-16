//! Neutral link between [`QncBroadcastPlayer`] and [`qnc_timeline_progress`].
//!
//! Owns playhead projection only — no forms, DB, or decode.

use qnc_player_core::FieldMode;

use crate::player_contract::FrameNumber;
use crate::player_remote::{PlayerCommand, PlayerEvent};
use crate::qnc_timeline::ExpandedAudio;
use crate::qnc_timeline_progress::{TimelineProgressIntent, TimelineProgressModel};

/// Static program view for timeline paint (slow-changing).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProgramBounds {
    pub fps: f64,
    pub duration_frames: i64,
    pub in_frame: i64,
    pub out_frame: i64,
    /// Probed from media (`progressive` / tff / bff).
    pub field_mode: FieldMode,
}

impl ProgramBounds {
    #[allow(dead_code)]
    pub fn from_seconds(fps: f64, duration_sec: f64, in_sec: f64, out_sec: f64) -> Self {
        use crate::frame_time::{normalize_fps, seconds_to_frame};
        let fps = normalize_fps(fps);
        let duration_frames = seconds_to_frame(duration_sec.max(0.0), fps).max(1);
        let clamp = |frame: i64| frame.clamp(0, duration_frames);
        Self {
            fps,
            duration_frames,
            in_frame: clamp(seconds_to_frame(in_sec.max(0.0), fps)),
            out_frame: clamp(seconds_to_frame(out_sec.max(in_sec), fps)),
            field_mode: FieldMode::Progressive,
        }
    }
}

/// Frame authority between player transport and timeline progress bar.
#[derive(Debug, Clone, Copy)]
pub struct CarrierSync {
    program: Option<ProgramBounds>,
    player_frame: FrameNumber,
    playing: bool,
    active: bool,
    pending_cue: Option<FrameNumber>,
}

impl Default for CarrierSync {
    fn default() -> Self {
        Self {
            program: None,
            player_frame: FrameNumber(0),
            playing: false,
            active: false,
            pending_cue: None,
        }
    }
}

impl CarrierSync {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    #[allow(dead_code)]
    pub fn playing(&self) -> bool {
        self.playing
    }

    #[allow(dead_code)]
    pub fn player_frame(&self) -> FrameNumber {
        self.player_frame
    }

    #[allow(dead_code)]
    pub fn pending_cue(&self) -> Option<FrameNumber> {
        self.pending_cue
    }

    pub fn display_frame(&self) -> FrameNumber {
        if self.playing {
            return self.player_frame;
        }
        if let Some(pending) = self.pending_cue {
            return pending;
        }
        self.player_frame
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn ingest_player_events(&mut self, events: &[PlayerEvent]) {
        for event in events {
            self.ingest_player_event(event);
        }
    }

    pub fn ingest_player_event(&mut self, event: &PlayerEvent) {
        match event {
            PlayerEvent::SourceReady {
                fps,
                duration_frames,
                in_frame,
                out_frame,
                field_mode,
            } => {
                self.program = Some(ProgramBounds {
                    fps: *fps,
                    duration_frames: (*duration_frames).max(1),
                    in_frame: *in_frame,
                    out_frame: *out_frame,
                    field_mode: *field_mode,
                });
                self.active = true;
            }
            PlayerEvent::Frame {
                source_frame,
                playing,
                ..
            }
            | PlayerEvent::State {
                source_frame,
                playing,
                ..
            } => {
                self.player_frame = *source_frame;
                self.playing = *playing;
                self.active = true;
                if self.playing {
                    self.pending_cue = None;
                } else if self.pending_cue == Some(*source_frame) {
                    self.pending_cue = None;
                }
            }
            PlayerEvent::BoundaryReached { source_frame } => {
                self.player_frame = *source_frame;
                self.playing = false;
                self.active = true;
                if self.pending_cue == Some(*source_frame) {
                    self.pending_cue = None;
                }
            }
            PlayerEvent::Stopped => self.clear(),
            PlayerEvent::Error(_) => {}
        }
    }

    #[allow(dead_code)]
    pub fn timeline_model(&self) -> Option<TimelineProgressModel> {
        let program = self.program?;
        Some(TimelineProgressModel::from_carrier(
            program.fps,
            program.duration_frames,
            self.display_frame().0,
            program.in_frame,
            program.out_frame,
        ))
    }

    /// Carrier playhead + selected shot range + draft IN/OUT marks (all frames).
    pub fn timeline_model_with_ranges(
        &self,
        shot_in_frame: i64,
        shot_out_frame: i64,
        draft_in_frame: i64,
        draft_out_frame: i64,
    ) -> Option<TimelineProgressModel> {
        let program = self.program?;
        let duration_frames = program.duration_frames.max(1);
        let clamp = |frame: i64| frame.clamp(0, duration_frames);
        Some(TimelineProgressModel::from_ranges(
            program.fps,
            duration_frames,
            clamp(self.display_frame().0),
            clamp(shot_in_frame),
            clamp(shot_out_frame.max(shot_in_frame)),
            clamp(draft_in_frame),
            clamp(draft_out_frame.max(draft_in_frame)),
        ))
    }

    #[allow(dead_code)]
    pub fn dispatch_timeline_intent(
        &mut self,
        intent: TimelineProgressIntent,
    ) -> Option<PlayerCommand> {
        match intent {
            TimelineProgressIntent::CueFrame(frame) => self.dispatch_seek_frame(frame, false),
            TimelineProgressIntent::ToggleAudioExpand(_) => None,
            TimelineProgressIntent::None => None,
        }
    }

    /// Frame seek — shared by progress-bar scrub and neutral command input.
    pub fn dispatch_seek_frame(&mut self, frame: i64, coalesce: bool) -> Option<PlayerCommand> {
        let program = self.program?;
        let frame = FrameNumber(frame.clamp(0, program.duration_frames));
        self.pending_cue = Some(frame);
        Some(PlayerCommand::SeekFrame {
            frame,
            still: true,
            coalesce,
        })
    }

    #[allow(dead_code)]
    pub fn apply_audio_expand(
        &self,
        expanded: ExpandedAudio,
        intent: TimelineProgressIntent,
    ) -> ExpandedAudio {
        match intent {
            TimelineProgressIntent::ToggleAudioExpand(lane) => expanded.toggle(lane),
            _ => expanded,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds() -> ProgramBounds {
        ProgramBounds::from_seconds(25.0, 10.0, 0.0, 10.0)
    }

    fn state(frame: i64, playing: bool) -> PlayerEvent {
        PlayerEvent::State {
            source_frame: FrameNumber(frame),
            source_sec: frame as f64 / 25.0,
            playing,
            status: if playing {
                "Playing".into()
            } else {
                "Ready".into()
            },
        }
    }

    fn ready() -> PlayerEvent {
        let b = bounds();
        PlayerEvent::SourceReady {
            fps: b.fps,
            duration_frames: b.duration_frames,
            in_frame: b.in_frame,
            out_frame: b.out_frame,
            field_mode: b.field_mode,
        }
    }

    #[test]
    fn player_frame_projects_to_timeline_model() {
        let mut sync = CarrierSync::new();
        sync.ingest_player_events(&[ready(), state(100, false)]);
        let model = sync.timeline_model().expect("model");
        assert_eq!(model.playhead_frame(), 100);
    }

    #[test]
    fn scrub_emits_seek_frame() {
        let mut sync = CarrierSync::new();
        sync.ingest_player_event(&ready());
        sync.ingest_player_event(&state(0, false));
        let cmd = sync
            .dispatch_timeline_intent(TimelineProgressIntent::CueFrame(50))
            .expect("cmd");
        assert_eq!(sync.display_frame(), FrameNumber(50));
        assert_eq!(sync.pending_cue(), Some(FrameNumber(50)));
        assert!(matches!(
            cmd,
            PlayerCommand::SeekFrame {
                frame: FrameNumber(50),
                still: true,
                coalesce: false,
            }
        ));
    }

    #[test]
    fn player_confirms_pending_cue_clears_pending() {
        let mut sync = CarrierSync::new();
        sync.ingest_player_event(&ready());
        sync.ingest_player_event(&state(10, false));
        sync.dispatch_timeline_intent(TimelineProgressIntent::CueFrame(50));

        sync.ingest_player_event(&state(50, false));
        assert_eq!(sync.pending_cue(), None);
        assert_eq!(sync.display_frame(), FrameNumber(50));
    }

    #[test]
    fn playing_clears_pending_and_follows_player() {
        let mut sync = CarrierSync::new();
        sync.ingest_player_event(&ready());
        sync.ingest_player_events(&[state(10, false), state(20, true)]);
        sync.dispatch_timeline_intent(TimelineProgressIntent::CueFrame(99));

        sync.ingest_player_event(&state(22, true));
        assert_eq!(sync.pending_cue(), None);
        assert_eq!(sync.display_frame(), FrameNumber(22));
    }

    #[test]
    fn stopped_resets_sync() {
        let mut sync = CarrierSync::new();
        sync.ingest_player_events(&[ready(), state(40, true), PlayerEvent::Stopped]);
        assert!(!sync.is_active());
        assert!(sync.timeline_model().is_none());
    }

    #[test]
    fn roundtrip_seek_frame_command() {
        let mut sync = CarrierSync::new();
        sync.ingest_player_event(&ready());
        sync.ingest_player_event(&state(0, false));

        let cmd = sync
            .dispatch_timeline_intent(TimelineProgressIntent::CueFrame(125))
            .expect("command");

        sync.ingest_player_event(&state(125, false));
        let model = sync.timeline_model().expect("model");
        assert_eq!(model.playhead_frame(), 125);
        assert!(matches!(
            cmd,
            PlayerCommand::SeekFrame {
                frame: FrameNumber(125),
                ..
            }
        ));
    }

    #[test]
    fn source_ready_sets_program_bounds() {
        let mut sync = CarrierSync::new();
        sync.ingest_player_event(&ready());
        let model = sync.timeline_model().expect("model");
        assert_eq!(model.duration_frames(), 250);
        assert_eq!(model.shot_in_frame(), 0);
        assert_eq!(model.shot_out_frame(), 250);
        assert_eq!(model.draft_in_frame(), 0);
        assert_eq!(model.draft_out_frame(), 250);
    }

    #[test]
    fn source_timeline_keeps_selected_range_and_draft_marks_separate() {
        let mut sync = CarrierSync::new();
        sync.ingest_player_event(&ready());
        sync.ingest_player_event(&state(40, false));
        let model = sync
            .timeline_model_with_ranges(10, 200, 60, 120)
            .expect("model");

        assert_eq!(model.playhead_frame(), 40);
        assert_eq!(model.shot_in_frame(), 10);
        assert_eq!(model.shot_out_frame(), 200);
        assert_eq!(model.draft_in_frame(), 60);
        assert_eq!(model.draft_out_frame(), 120);
    }
}
