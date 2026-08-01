//! Ingest native playback — same [`PlayerClient`] bridge as Story / Media Assist.

use std::time::{Duration, Instant};

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};

use crate::api::HostClient;
use crate::ingest::IngestScreen;
use crate::player_bridge::{PlaybackCommand, PlayerClient};
use crate::player_contract::BroadcastHostSourceRef;

pub use crate::player_bridge::PlaybackCommand as IngestPlaybackCommand;

impl IngestScreen {
    pub fn drain_playback_commands(&mut self) -> Vec<IngestPlaybackCommand> {
        PlayerClient::drain_playback_commands(self)
    }

    pub fn clear_pending_seeks(&mut self) {
        PlayerClient::clear_pending_seeks(self)
    }

    pub fn playback_source_ref(&self) -> Option<&BroadcastHostSourceRef> {
        PlayerClient::playback_source_ref(self)
    }

    pub fn playback_source_fps(&self) -> f64 {
        PlayerClient::playback_source_fps(self)
    }

    pub fn playback_source_has_audio(&self) -> bool {
        PlayerClient::playback_source_has_audio(self)
    }

    pub fn playback_source_audio_channels(&self) -> u8 {
        PlayerClient::playback_source_audio_channels(self)
    }

    pub fn playback_source_sec(&self) -> f64 {
        PlayerClient::playback_source_sec(self)
    }

    pub fn playback_media_path(&self) -> Option<String> {
        PlayerClient::playback_media_path(self)
    }

    pub fn playback_source_range_sec(&self) -> (f64, f64) {
        PlayerClient::playback_source_range_sec(self)
    }

    pub fn set_player_preview_active(&mut self, active: bool) {
        PlayerClient::set_player_preview_active(self, active)
    }

    pub fn apply_playback_command_state(&mut self, playing: bool, status: impl Into<String>) {
        PlayerClient::apply_playback_command_state(self, playing, status)
    }

    pub fn apply_player_error(&mut self, status: impl Into<String>) {
        PlayerClient::apply_player_error(self, status)
    }

    pub fn apply_player_frame(&mut self, image: ColorImage, source_sec: f64, playing: bool) {
        PlayerClient::apply_player_frame(self, image, source_sec, playing)
    }

    pub fn apply_player_state(
        &mut self,
        source_sec: f64,
        playing: bool,
        status: impl Into<String>,
    ) {
        PlayerClient::apply_player_state(self, source_sec, playing, status)
    }

    pub fn prepare_player_frame(&mut self, ctx: &egui::Context) {
        if let Some(img) = self.pending_player_image.take() {
            let tex = ctx.load_texture("ingest_player_preview", img, TextureOptions::LINEAR);
            self.player_texture = Some(tex);
        }
    }

    pub fn queue_seek_to_playhead(&mut self) {
        self.pending_playback_commands
            .push(IngestPlaybackCommand::SeekToPlayhead);
    }

    pub fn queue_pause_and_seek(&mut self) {
        self.lock_playhead_ui();
        self.pending_playback_commands
            .push(IngestPlaybackCommand::PauseAndSeek);
    }

    pub fn queue_toggle_play(&mut self) {
        self.pending_playback_commands
            .push(IngestPlaybackCommand::TogglePlay);
    }

    pub fn reset_player_session(&mut self) {
        self.selected_play_path.clear();
        self.selected_source_ref = None;
        self.player_texture = None;
        self.pending_player_image = None;
        self.player_preview_active = false;
        self.playing = false;
        self.virtual_sec = 0.0;
        self.pending_playback_commands.clear();
        self.clear_pending_seeks();
    }

    pub(crate) fn activate_preview_clip(
        &mut self,
        _host: &HostClient,
        project_id: &str,
        clip_id: &str,
    ) {
        if clip_id.trim().is_empty() {
            return;
        }
        self.preview_clip_id = clip_id.to_string();
        self.virtual_sec = 0.0;
        self.playing = false;
        self.player_preview_active = false;
        self.player_texture = None;
        self.pending_player_image = None;

        let (fps, has_audio, channels, duration) = self
            .state
            .as_ref()
            .and_then(|st| st.clips.iter().find(|c| c.clip_id == clip_id))
            .map(|c| {
                (
                    if c.fps.is_finite() && c.fps > 0.0 {
                        c.fps
                    } else {
                        25.0
                    },
                    c.has_audio,
                    if c.audio_channels > 0 {
                        c.audio_channels
                    } else if c.has_audio {
                        2
                    } else {
                        0
                    },
                    if c.duration_sec.is_finite() && c.duration_sec > 0.0 {
                        c.duration_sec
                    } else {
                        1.0
                    },
                )
            })
            .unwrap_or((25.0, true, 2, 1.0));

        self.selected_source_fps = fps;
        self.selected_source_has_audio = has_audio;
        self.selected_source_audio_channels = channels;
        self.selected_source_ref = BroadcastHostSourceRef::from_story_fields(
            project_id,
            clip_id,
            clip_id,
            clip_id,
            Some(0.0),
            Some(duration),
            duration,
        )
        .ok();

        self.resolve_play_path(clip_id);
        self.queue_seek_to_playhead();
    }

    /// Ingest play source = kartica / folder (snapshot paths). Not Story project-proxy API.
    fn resolve_play_path(&mut self, clip_id: &str) {
        self.selected_play_path.clear();
        let Some(clip) = self
            .state
            .as_ref()
            .and_then(|st| st.clips.iter().find(|c| c.clip_id == clip_id))
        else {
            self.player_status = format!("Nema klipa · {clip_id}");
            return;
        };
        // Source-first: camera proxy on card → original/source on card → project proxy only if already copied.
        for (candidate, label) in [
            (clip.proxy_path.as_str(), "card proxy"),
            (clip.original_path.as_str(), "card original"),
            (clip.source_path.as_str(), "card source"),
            (clip.project_proxy_path.as_str(), "project proxy"),
        ] {
            let p = candidate.trim();
            if p.is_empty() {
                continue;
            }
            let path = std::path::Path::new(p);
            if path.is_file() {
                self.selected_play_path = p.to_string();
                self.player_status = format!("Play · {clip_id} ({label})");
                return;
            }
        }
        self.player_status = format!("Nema play path na sourceu · {clip_id}");
    }

    fn frame_eps_sec(&self) -> f64 {
        let fps = if self.selected_source_fps.is_finite() && self.selected_source_fps > 0.0 {
            self.selected_source_fps
        } else {
            25.0
        };
        (0.5 / fps).max(1e-4)
    }

    fn snap_source_sec(&self, sec: f64) -> f64 {
        let fps = if self.selected_source_fps.is_finite() && self.selected_source_fps > 0.0 {
            self.selected_source_fps
        } else {
            25.0
        };
        let frame = (sec * fps).round();
        (frame / fps).max(0.0)
    }

    fn lock_playhead_ui(&mut self) {
        self.playhead_ui_target = Some(self.virtual_sec);
        self.playhead_ui_lock_until = Some(Instant::now() + Duration::from_millis(120));
    }

    pub(crate) fn scrub_to(&mut self, sec: f64) {
        self.virtual_sec = self.snap_source_sec(sec.max(0.0));
        self.lock_playhead_ui();
        self.queue_pause_and_seek();
    }

    pub(crate) fn nudge_frames(&mut self, frames: i64) {
        let fps = if self.selected_source_fps.is_finite() && self.selected_source_fps > 0.0 {
            self.selected_source_fps
        } else {
            25.0
        };
        let next = self.virtual_sec + (frames as f64) / fps;
        self.scrub_to(next);
    }

    pub(crate) fn player_texture(&self) -> Option<&TextureHandle> {
        self.player_texture.as_ref()
    }
}

impl PlayerClient for IngestScreen {
    fn drain_playback_commands(&mut self) -> Vec<PlaybackCommand> {
        let mut seek_to = false;
        let mut pause_and_seek = false;
        let mut toggle = false;
        for c in self.pending_playback_commands.drain(..) {
            match c {
                PlaybackCommand::SeekToPlayhead => seek_to = true,
                PlaybackCommand::PauseAndSeek => pause_and_seek = true,
                PlaybackCommand::TogglePlay => toggle = true,
            }
        }
        // Toggle alone — never Play+Seek in the same drain (that pauses immediately).
        if toggle {
            return vec![PlaybackCommand::TogglePlay];
        }
        if pause_and_seek {
            vec![PlaybackCommand::PauseAndSeek]
        } else if seek_to && !self.playing {
            vec![PlaybackCommand::SeekToPlayhead]
        } else {
            Vec::new()
        }
    }

    fn clear_pending_seeks(&mut self) {
        self.playhead_ui_lock_until = None;
        self.playhead_ui_target = None;
    }

    fn playback_source_ref(&self) -> Option<&BroadcastHostSourceRef> {
        self.selected_source_ref.as_ref()
    }

    fn playback_media_path(&self) -> Option<String> {
        let path = self.selected_play_path.trim();
        if path.is_empty() {
            None
        } else {
            Some(path.to_string())
        }
    }

    fn playback_source_range_sec(&self) -> (f64, f64) {
        let end = self
            .state
            .as_ref()
            .and_then(|st| {
                st.clips
                    .iter()
                    .find(|c| c.clip_id == self.preview_clip_id)
                    .map(|c| c.duration_sec)
            })
            .unwrap_or(1.0)
            .max(0.04);
        (0.0, end)
    }

    fn playback_source_fps(&self) -> f64 {
        self.selected_source_fps
    }

    fn playback_source_has_audio(&self) -> bool {
        self.selected_source_has_audio
    }

    fn playback_source_audio_channels(&self) -> u8 {
        if self.selected_source_has_audio {
            self.selected_source_audio_channels.max(2).min(4)
        } else {
            2
        }
    }

    fn playback_source_sec(&self) -> f64 {
        self.virtual_sec.max(0.0)
    }

    fn missing_source_message(&self) -> String {
        "Odaberi klip prije play".into()
    }

    fn missing_path_message(&self) -> String {
        "Nema play path na sourceu (kartica / folder) — odaberi klip s medijem".into()
    }

    fn set_player_preview_active(&mut self, active: bool) {
        self.player_preview_active = active;
    }

    fn apply_playback_command_state(&mut self, playing: bool, status: impl Into<String>) {
        self.playing = playing;
        self.player_status = status.into();
    }

    fn apply_player_error(&mut self, status: impl Into<String>) {
        self.player_preview_active = false;
        self.playing = false;
        self.player_status = status.into();
    }

    fn apply_player_frame(&mut self, image: ColorImage, source_sec: f64, playing: bool) {
        let snapped = self.snap_source_sec(source_sec.max(0.0));
        let eps = self.frame_eps_sec();
        let locked = self
            .playhead_ui_lock_until
            .is_some_and(|until| Instant::now() < until);
        let near_ui = self
            .playhead_ui_target
            .map(|t| (snapped - t).abs() <= eps)
            .unwrap_or(false);
        self.pending_player_image = Some(image);
        self.player_preview_active = true;
        self.playing = playing;
        if locked && !near_ui && !playing {
            return;
        }
        if near_ui || !locked {
            self.playhead_ui_lock_until = None;
            self.playhead_ui_target = None;
            self.virtual_sec = snapped;
        }
        self.player_status = "Broadcast player".into();
    }

    fn apply_player_state(&mut self, source_sec: f64, playing: bool, status: impl Into<String>) {
        let snapped = self.snap_source_sec(source_sec.max(0.0));
        let eps = self.frame_eps_sec();
        let locked = self
            .playhead_ui_lock_until
            .is_some_and(|until| Instant::now() < until);
        let near_ui = self
            .playhead_ui_target
            .map(|t| (snapped - t).abs() <= eps)
            .unwrap_or(false);
        self.playing = playing;
        self.player_status = status.into();
        if playing {
            self.playhead_ui_lock_until = None;
            self.playhead_ui_target = None;
            self.virtual_sec = snapped;
            return;
        }
        if locked && !near_ui {
            return;
        }
        self.playhead_ui_lock_until = None;
        self.playhead_ui_target = None;
        self.virtual_sec = snapped;
    }
}
