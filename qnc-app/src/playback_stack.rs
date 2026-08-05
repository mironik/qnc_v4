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
use crate::player_remote::{BroadcastPlayerOpenRequest, PlayerCommand};
use crate::qnc_broadcast_player::{BroadcastPlayerRx, BroadcastPlayerTx, QncBroadcastPlayer};
use crate::qnc_timeline::ExpandedAudio;
use crate::qnc_timeline_progress::{TimelineProgressIntent, TimelineProgressModel};

/// Broadcast player + carrier → timeline model + monitor.
pub struct PlaybackStack {
    player: QncBroadcastPlayer,
    carrier: CarrierSync,
}

impl PlaybackStack {
    pub fn new() -> Self {
        Self {
            player: QncBroadcastPlayer::new(),
            carrier: CarrierSync::new(),
        }
    }

    pub fn player(&self) -> &QncBroadcastPlayer {
        &self.player
    }

    pub fn player_mut(&mut self) -> &mut QncBroadcastPlayer {
        &mut self.player
    }

    pub fn tx(&self) -> BroadcastPlayerTx {
        self.player.tx()
    }

    pub fn subscribe(&self) -> BroadcastPlayerRx {
        self.player.subscribe()
    }

    pub fn carrier(&self) -> &CarrierSync {
        &self.carrier
    }

    pub fn carrier_mut(&mut self) -> &mut CarrierSync {
        &mut self.carrier
    }

    pub fn pump_player(&mut self, ctx: &egui::Context) {
        self.player.pump(ctx);
    }

    pub fn ingest_events(&mut self, events: &[crate::player_remote::PlayerEvent]) {
        self.carrier.ingest_player_events(events);
    }

    pub fn pump(&mut self, ctx: &egui::Context, rx: &BroadcastPlayerRx) {
        self.pump_player(ctx);
        let events = rx.try_recv_all();
        self.ingest_events(&events);
    }

    pub fn stop(&mut self) {
        self.player.stop();
        self.carrier.clear();
    }

    pub fn timeline_model(&self) -> Option<TimelineProgressModel> {
        self.carrier.timeline_model()
    }

    pub fn timeline_model_for_clip(
        &self,
        fps: f64,
        duration_sec: f64,
        in_frame: i64,
        out_frame: i64,
        fallback_playhead_frame: i64,
    ) -> TimelineProgressModel {
        if let Some(model) = self.carrier.timeline_model_with_marks(in_frame, out_frame) {
            return model;
        }
        use crate::frame_time::{normalize_fps, seconds_to_frame};
        let fps = normalize_fps(fps);
        let duration_frames = seconds_to_frame(duration_sec.max(0.0), fps).max(1);
        let clamp = |frame: i64| frame.clamp(0, duration_frames);
        TimelineProgressModel::from_carrier(
            fps,
            duration_frames,
            clamp(fallback_playhead_frame),
            clamp(in_frame),
            clamp(out_frame.max(in_frame)),
        )
    }

    pub fn monitor_texture(&self) -> Option<&eframe::egui::TextureHandle> {
        self.player.texture()
    }

    pub fn show_monitor(&self, ui: &mut egui::Ui, height: f32, empty_label: &str) {
        self.player.show_monitor(ui, height, empty_label);
    }

    /// Idempotent open — same source keeps decode position (demo / monolith).
    pub fn ensure_open(&self, request: BroadcastPlayerOpenRequest) -> Result<(), String> {
        if self.player.matches_source(&request) && self.player.snapshot().has_source {
            return Ok(());
        }
        let _ = self.player.tx().stop();
        self.player.tx().open(request)
    }

    /// Progress-bar click — set playhead (CueFrame). Space will play from here.
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

    pub fn cue_frame(&mut self, frame: i64) -> bool {
        self.send_seek(frame, false)
    }

    pub fn scrub_frame(&mut self, frame: i64) -> bool {
        self.send_seek(frame, true)
    }

    fn send_seek(&mut self, frame: i64, coalesce: bool) -> bool {
        let Some(cmd) = self.carrier.dispatch_seek_frame(frame, coalesce) else {
            return false;
        };
        let _ = self.player.tx().send(cmd);
        true
    }

    pub fn dispatch_timeline_intent(
        &mut self,
        intent: TimelineProgressIntent,
    ) -> Option<PlayerCommand> {
        let cmd = self.carrier.dispatch_timeline_intent(intent)?;
        let _ = self.player.tx().send(cmd.clone());
        Some(cmd)
    }

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
    use crate::player_contract::FrameNumber;
    use crate::player_remote::PlayerEvent;

    #[test]
    fn stack_projects_player_frame_to_timeline_model() {
        let mut stack = PlaybackStack::new();
        let rx = stack.subscribe();
        stack.carrier_mut().ingest_player_events(&[
            PlayerEvent::SourceReady {
                fps: 25.0,
                duration_frames: 250,
                in_frame: 0,
                out_frame: 250,
                field_mode: qnc_player_core::FieldMode::Progressive,
            },
            PlayerEvent::State {
                source_frame: FrameNumber(42),
                source_sec: 42.0 / 25.0,
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
}
