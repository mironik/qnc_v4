//! The only broadcast player in the app (+ carrier link for timeline projection).
//!
//! ```text
//! CueFrame / TogglePlay / open metadata
//!        │
//!        ▼
//! PlaybackStack ──CarrierSync──► SeekFrame / toggle_play
//!        │              │
//!        │              └──► TimelineProgressModel
//!        └── show_monitor ──► monitor paint
//! ```
//!
//! Timeline and monitor are projections/remotes of this stack — not of any workflow screen.

use eframe::egui;

use crate::carrier_sync::CarrierSync;
use crate::player_contract::BroadcastHostSourceRef;
use crate::player_remote::{
    BroadcastPlayerOpenRequest, BroadcastProgramOpenRequest, PlayerCommand, PlayerEvent,
};
use crate::qnc_broadcast_player::{BroadcastPlayerRx, BroadcastPlayerTx, QncBroadcastPlayer};
use crate::qnc_timeline::ExpandedAudio;
use crate::qnc_timeline_progress::{TimelineProgressIntent, TimelineProgressModel};

/// Broadcast player + carrier → timeline model + monitor.
pub struct PlaybackStack {
    player: QncBroadcastPlayer,
    carrier: CarrierSync,
    active_source_ref: Option<BroadcastHostSourceRef>,
    playlist_input_active: bool,
    pending_seek: Option<PendingSeek>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingSeek {
    frame: i64,
    coalesce: bool,
}

impl PlaybackStack {
    pub fn new() -> Self {
        Self {
            player: QncBroadcastPlayer::new(),
            carrier: CarrierSync::new(),
            active_source_ref: None,
            playlist_input_active: false,
            pending_seek: None,
        }
    }

    pub fn player(&self) -> &QncBroadcastPlayer {
        &self.player
    }

    pub fn player_mut(&mut self) -> &mut QncBroadcastPlayer {
        &mut self.player
    }

    #[allow(dead_code)]
    pub fn tx(&self) -> BroadcastPlayerTx {
        self.player.tx()
    }

    pub fn subscribe(&self) -> BroadcastPlayerRx {
        self.player.subscribe()
    }

    pub fn carrier(&self) -> &CarrierSync {
        &self.carrier
    }

    #[allow(dead_code)]
    pub fn carrier_mut(&mut self) -> &mut CarrierSync {
        &mut self.carrier
    }

    #[allow(dead_code)]
    pub fn pump_player(&mut self, ctx: &egui::Context) {
        self.player.pump(ctx);
    }

    pub fn ingest_events(&mut self, events: &[PlayerEvent]) {
        if events
            .iter()
            .any(|event| matches!(event, PlayerEvent::Error(_) | PlayerEvent::Stopped))
        {
            self.active_source_ref = None;
            self.playlist_input_active = false;
            self.pending_seek = None;
        }
        self.carrier.ingest_player_events(events);
        self.flush_pending_seek();
    }

    #[allow(dead_code)]
    pub fn pump(&mut self, ctx: &egui::Context, rx: &BroadcastPlayerRx) {
        self.pump_player(ctx);
        let events = rx.try_recv_all();
        self.ingest_events(&events);
    }

    pub fn stop(&mut self) {
        self.player.stop();
        self.carrier.clear();
        self.active_source_ref = None;
        self.playlist_input_active = false;
        self.pending_seek = None;
    }

    #[allow(dead_code)]
    pub fn timeline_model(&self) -> Option<TimelineProgressModel> {
        self.carrier.timeline_model()
    }

    pub fn active_source_matches(&self, source_ref: &BroadcastHostSourceRef) -> bool {
        self.active_source_ref
            .as_ref()
            .is_some_and(|active| active == source_ref)
    }

    pub fn playlist_input_active(&self) -> bool {
        self.playlist_input_active
    }

    pub fn playlist_input_playing(&self) -> bool {
        self.playlist_input_active && (self.player.snapshot().playing || self.carrier.playing())
    }

    /// Live playlist/program frame for passive Program/Segment UI projections.
    ///
    /// The playlist input is loaded as one broadcast-player input, so the
    /// carrier display frame is already the program-frame authority.
    pub fn playlist_display_frame(
        &self,
        fallback_playhead_frame: i64,
        duration_frames: i64,
    ) -> i64 {
        let duration_frames = duration_frames.max(1);
        let fallback = fallback_playhead_frame.clamp(0, duration_frames);
        if !(self.playlist_input_active && self.carrier.is_active()) {
            return fallback;
        }
        self.carrier.display_frame().0.clamp(0, duration_frames)
    }

    pub fn timeline_model_for_source_ref(
        &self,
        source_ref: Option<&BroadcastHostSourceRef>,
        fps: f64,
        duration_frames: i64,
        shot_in_frame: i64,
        shot_out_frame: i64,
        draft_in_frame: i64,
        draft_out_frame: i64,
        fallback_playhead_frame: i64,
    ) -> TimelineProgressModel {
        if source_ref.is_some_and(|source_ref| self.active_source_matches(source_ref)) {
            if let Some(model) = self.carrier.timeline_model_with_ranges(
                shot_in_frame,
                shot_out_frame,
                draft_in_frame,
                draft_out_frame,
            ) {
                return model;
            }
        }
        Self::fallback_timeline_model(
            fps,
            duration_frames,
            shot_in_frame,
            shot_out_frame,
            draft_in_frame,
            draft_out_frame,
            fallback_playhead_frame,
        )
    }

    #[allow(dead_code)]
    pub fn timeline_model_for_clip(
        &self,
        fps: f64,
        duration_frames: i64,
        shot_in_frame: i64,
        shot_out_frame: i64,
        draft_in_frame: i64,
        draft_out_frame: i64,
        fallback_playhead_frame: i64,
    ) -> TimelineProgressModel {
        Self::fallback_timeline_model(
            fps,
            duration_frames,
            shot_in_frame,
            shot_out_frame,
            draft_in_frame,
            draft_out_frame,
            fallback_playhead_frame,
        )
    }

    /// Static projection only. Live carrier projection must go through
    /// `timeline_model_for_source_ref` with an explicit source identity match.
    fn fallback_timeline_model(
        fps: f64,
        duration_frames: i64,
        shot_in_frame: i64,
        shot_out_frame: i64,
        draft_in_frame: i64,
        draft_out_frame: i64,
        fallback_playhead_frame: i64,
    ) -> TimelineProgressModel {
        let fps = if fps.is_finite() && fps > 0.0 {
            fps
        } else {
            0.0
        };
        let duration_frames = duration_frames.max(1);
        let clamp = |frame: i64| frame.clamp(0, duration_frames);
        TimelineProgressModel::from_ranges(
            fps,
            duration_frames,
            clamp(fallback_playhead_frame),
            clamp(shot_in_frame),
            clamp(shot_out_frame.max(shot_in_frame)),
            clamp(draft_in_frame),
            clamp(draft_out_frame.max(draft_in_frame)),
        )
    }

    pub fn show_monitor(&self, ui: &mut egui::Ui, height: f32, empty_label: &str) {
        self.player.show_monitor(ui, height, empty_label);
    }

    #[allow(dead_code)]
    pub fn monitor_texture(&self) -> Option<&eframe::egui::TextureHandle> {
        self.player.texture()
    }

    /// Idempotent open — same source keeps decode position (demo / monolith).
    pub fn ensure_open(&mut self, request: BroadcastPlayerOpenRequest) -> Result<(), String> {
        if self.player.matches_source(&request) && self.player.snapshot().has_source {
            self.active_source_ref = Some(request.source_ref);
            self.playlist_input_active = false;
            return Ok(());
        }
        let source_ref = request.source_ref.clone();
        self.carrier.clear();
        self.active_source_ref = None;
        self.playlist_input_active = false;
        self.pending_seek = None;
        let _ = self.player.tx().stop();
        self.player.tx().open(request)?;
        self.active_source_ref = Some(source_ref);
        Ok(())
    }

    /// Progress-bar click — set playhead (CueFrame). Space will play from here.
    #[allow(dead_code)]
    pub fn cue_timeline_click(
        &mut self,
        request: BroadcastPlayerOpenRequest,
        frame: i64,
    ) -> Result<(), String> {
        crate::player_log::log_info("bridge", &format!("progress-bar cue frame {frame}"));
        self.ensure_open(request)?;
        if self.cue_frame(frame) {
            Ok(())
        } else {
            Err("Timeline nije spreman — pričekaj SourceReady".into())
        }
    }

    /// Space — same as QNC `playback_demo`: only toggle play/pause.
    /// Position comes solely from progress-bar SeekFrame (click), not a second goto.
    pub fn toggle_play(&self) -> Result<(), String> {
        crate::player_log::log_info("bridge", "TogglePlay / Space");
        self.player.tx().toggle_play()
    }

    /// Source Space — pause must not reopen the same active input.
    pub fn toggle_source_play(
        &mut self,
        request: BroadcastPlayerOpenRequest,
    ) -> Result<(), String> {
        if self.source_input_playing(&request.source_ref) {
            return self.pause();
        }
        self.ensure_open(request)?;
        self.toggle_play()
    }

    fn source_input_playing(&self, source_ref: &BroadcastHostSourceRef) -> bool {
        !self.playlist_input_active
            && self.active_source_matches(source_ref)
            && (self.player.snapshot().playing || self.carrier.playing())
    }

    /// Open one playlist input in the Broadcast Player.
    pub fn play_program(&mut self, request: BroadcastProgramOpenRequest) -> Result<(), String> {
        crate::player_log::log_info("bridge", "OpenProgram + Play");
        self.carrier.clear();
        self.active_source_ref = None;
        self.playlist_input_active = false;
        self.pending_seek = None;
        self.player.tx().open_program(request)?;
        self.playlist_input_active = true;
        self.player.tx().play()
    }

    /// Pause playback deterministically; used when program play reaches an unsupported layer.
    pub fn pause(&self) -> Result<(), String> {
        crate::player_log::log_info("bridge", "Pause");
        self.player.tx().pause()
    }

    /// Resume the already-open input without rebuilding source/program state.
    pub fn play_loaded_input(&self) -> Result<(), String> {
        crate::player_log::log_info("bridge", "PlayLoadedInput");
        self.player.tx().play()
    }

    pub fn cue_frame(&mut self, frame: i64) -> bool {
        self.send_seek(frame, false)
    }

    pub fn scrub_frame(&mut self, frame: i64) -> bool {
        self.send_seek(frame, true)
    }

    fn send_seek(&mut self, frame: i64, coalesce: bool) -> bool {
        let Some(cmd) = self.carrier.dispatch_seek_frame(frame, coalesce) else {
            self.pending_seek = Some(PendingSeek { frame, coalesce });
            return true;
        };
        self.pending_seek = None;
        let _ = self.player.tx().send(cmd);
        true
    }

    fn flush_pending_seek(&mut self) {
        let Some(pending) = self.pending_seek else {
            return;
        };
        let Some(cmd) = self
            .carrier
            .dispatch_seek_frame(pending.frame, pending.coalesce)
        else {
            return;
        };
        self.pending_seek = None;
        let _ = self.player.tx().send(cmd);
    }

    #[allow(dead_code)]
    pub fn dispatch_timeline_intent(
        &mut self,
        intent: TimelineProgressIntent,
    ) -> Option<PlayerCommand> {
        let cmd = self.carrier.dispatch_timeline_intent(intent)?;
        let _ = self.player.tx().send(cmd.clone());
        Some(cmd)
    }

    #[allow(dead_code)]
    pub fn apply_audio_expand(
        &self,
        expanded: ExpandedAudio,
        intent: TimelineProgressIntent,
    ) -> ExpandedAudio {
        self.carrier.apply_audio_expand(expanded, intent)
    }
}

impl Default for PlaybackStack {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player_contract::{BroadcastHostSourceRef, FrameNumber};
    use crate::player_remote::{BroadcastPlayerOpenRequest, PlayerEvent};

    fn open_request() -> BroadcastPlayerOpenRequest {
        BroadcastPlayerOpenRequest {
            source_ref: BroadcastHostSourceRef::from_frame_fields(
                "project",
                "part_a",
                "",
                "clip_a",
                Some(FrameNumber(10)),
                Some(FrameNumber(40)),
                FrameNumber(250),
            )
            .unwrap(),
            media_input: "media.mov".into(),
            source_fps: 50.0,
            has_audio: true,
            audio_channels: 2,
            start_source_frame: FrameNumber(10),
        }
    }

    fn source_ref(id: &str, in_frame: i64, out_frame: i64) -> BroadcastHostSourceRef {
        BroadcastHostSourceRef::from_frame_fields(
            "project",
            id,
            "",
            "clip_a",
            Some(FrameNumber(in_frame)),
            Some(FrameNumber(out_frame)),
            FrameNumber(250),
        )
        .unwrap()
    }

    #[test]
    fn stack_projects_player_frame_to_timeline_model() {
        let mut stack = PlaybackStack::new();
        let rx = stack.subscribe();
        stack.carrier_mut().ingest_player_events(&[
            PlayerEvent::SourceReady {
                fps: 50.0,
                duration_frames: 250,
                in_frame: 0,
                out_frame: 250,
                field_mode: qnc_player_core::FieldMode::Progressive,
            },
            PlayerEvent::State {
                source_frame: FrameNumber(42),
                source_sec: 42.0 / 50.0,
                playing: false,
                status: "Ready".into(),
            },
        ]);
        let _ = rx.try_recv_all();
        let model = stack.timeline_model().expect("model");
        assert_eq!(model.playhead_frame(), 42);
    }

    #[test]
    fn paused_progress_bar_cue_updates_display_frame() {
        let mut stack = PlaybackStack::new();
        stack.carrier_mut().ingest_player_events(&[
            PlayerEvent::SourceReady {
                fps: 50.0,
                duration_frames: 500,
                in_frame: 0,
                out_frame: 500,
                field_mode: qnc_player_core::FieldMode::Progressive,
            },
            PlayerEvent::State {
                source_frame: FrameNumber(144),
                source_sec: 2.88,
                playing: false,
                status: "Paused".into(),
            },
        ]);
        stack
            .carrier_mut()
            .dispatch_seek_frame(70, false)
            .expect("seek");
        // Progress bar paint follows cue; Space must not re-goto — only Play.
        assert_eq!(stack.carrier().display_frame(), FrameNumber(70));
    }

    #[test]
    fn cold_scrub_waits_for_source_ready_before_seek() {
        let mut stack = PlaybackStack::new();

        assert!(stack.scrub_frame(88));
        assert_eq!(stack.carrier().display_frame(), FrameNumber(0));

        stack.ingest_events(&[PlayerEvent::SourceReady {
            fps: 50.0,
            duration_frames: 250,
            in_frame: 0,
            out_frame: 250,
            field_mode: qnc_player_core::FieldMode::Progressive,
        }]);

        assert_eq!(stack.carrier().display_frame(), FrameNumber(88));
    }

    #[test]
    fn ensure_open_resets_carrier_for_new_source() {
        let mut stack = PlaybackStack::new();
        stack.ingest_events(&[PlayerEvent::SourceReady {
            fps: 50.0,
            duration_frames: 250,
            in_frame: 0,
            out_frame: 250,
            field_mode: qnc_player_core::FieldMode::Progressive,
        }]);
        assert!(stack.carrier().is_active());

        stack.ensure_open(open_request()).unwrap();

        assert!(!stack.carrier().is_active());
    }

    #[test]
    fn ensure_open_marks_playlist_input_inactive() {
        let mut stack = PlaybackStack::new();
        stack.playlist_input_active = true;

        stack.ensure_open(open_request()).unwrap();

        assert!(!stack.playlist_input_active());
    }

    #[test]
    fn playlist_display_frame_uses_carrier_only_for_active_playlist_input() {
        let mut stack = PlaybackStack::new();
        stack.ingest_events(&[
            PlayerEvent::SourceReady {
                fps: 50.0,
                duration_frames: 250,
                in_frame: 0,
                out_frame: 250,
                field_mode: qnc_player_core::FieldMode::Progressive,
            },
            PlayerEvent::State {
                source_frame: FrameNumber(80),
                source_sec: 1.6,
                playing: true,
                status: "Playing".into(),
            },
        ]);

        assert_eq!(stack.playlist_display_frame(12, 250), 12);

        stack.playlist_input_active = true;
        assert_eq!(stack.playlist_display_frame(12, 250), 80);
        assert_eq!(stack.playlist_display_frame(12, 60), 60);
    }

    #[test]
    fn source_toggle_pauses_matching_active_playing_source() {
        let mut stack = PlaybackStack::new();
        let request = open_request();
        stack.active_source_ref = Some(request.source_ref.clone());
        stack.ingest_events(&[
            PlayerEvent::SourceReady {
                fps: 50.0,
                duration_frames: 250,
                in_frame: 10,
                out_frame: 40,
                field_mode: qnc_player_core::FieldMode::Progressive,
            },
            PlayerEvent::State {
                source_frame: FrameNumber(18),
                source_sec: 18.0 / 50.0,
                playing: true,
                status: "Playing".into(),
            },
        ]);

        assert!(stack.source_input_playing(&request.source_ref));
        assert!(!stack.source_input_playing(&source_ref("part_b", 50, 90)));
    }

    #[test]
    fn stopped_or_error_clears_playlist_input_state() {
        let mut stack = PlaybackStack::new();
        stack.playlist_input_active = true;
        stack.active_source_ref = Some(source_ref("part_a", 10, 40));

        stack.ingest_events(&[PlayerEvent::Stopped]);

        assert!(!stack.playlist_input_active());
        assert!(stack.active_source_ref.is_none());

        stack.playlist_input_active = true;
        stack.active_source_ref = Some(source_ref("part_a", 10, 40));

        stack.ingest_events(&[PlayerEvent::Error("failed".into())]);

        assert!(!stack.playlist_input_active());
        assert!(stack.active_source_ref.is_none());
    }

    #[test]
    fn source_projection_requires_matching_open_source_ref() {
        let mut stack = PlaybackStack::new();
        let active_ref = source_ref("part_a", 10, 40);
        stack.active_source_ref = Some(active_ref.clone());
        stack.carrier_mut().ingest_player_events(&[
            PlayerEvent::SourceReady {
                fps: 50.0,
                duration_frames: 250,
                in_frame: 10,
                out_frame: 40,
                field_mode: qnc_player_core::FieldMode::Progressive,
            },
            PlayerEvent::State {
                source_frame: FrameNumber(24),
                source_sec: 24.0 / 50.0,
                playing: true,
                status: "Playing".into(),
            },
        ]);

        let live =
            stack.timeline_model_for_source_ref(Some(&active_ref), 50.0, 250, 0, 250, 0, 250, 7);
        assert_eq!(live.playhead_frame(), 24);

        let stale = stack.timeline_model_for_source_ref(
            Some(&source_ref("part_b", 50, 90)),
            50.0,
            250,
            0,
            250,
            0,
            250,
            7,
        );
        assert_eq!(stale.playhead_frame(), 7);
    }
}
