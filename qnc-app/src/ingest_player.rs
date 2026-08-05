//! Ingest **form** adapter for the shared broadcast player ([`PlayerClient`]).
//! Not a separate player — only open metadata + command queue into [`PlaybackStack`].

use eframe::egui::ColorImage;

use crate::api::HostClient;
use crate::frame_time::{frame_to_seconds, seconds_to_frame};
use crate::ingest::IngestScreen;
use crate::player_bridge::{PlaybackCommand, PlayerClient};
use crate::player_contract::{BroadcastHostSourceRef, FrameNumber};

impl IngestScreen {
    pub fn drain_playback_commands(&mut self) -> Vec<PlaybackCommand> {
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
        self.virtual_sec
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

    fn source_frame_now(&self) -> FrameNumber {
        FrameNumber(seconds_to_frame(self.virtual_sec, self.selected_source_fps))
    }

    pub fn queue_seek_to_playhead(&mut self) {
        self.pending_playback_commands
            .push(PlaybackCommand::CueFrame(self.source_frame_now()));
    }

    pub fn queue_pause_and_seek(&mut self) {
        self.pending_playback_commands
            .push(PlaybackCommand::ScrubFrame(self.source_frame_now()));
    }

    pub fn queue_toggle_play(&mut self) {
        self.pending_playback_commands
            .push(PlaybackCommand::TogglePlay);
    }

    pub fn reset_player_session(&mut self) {
        self.selected_play_path.clear();
        self.selected_source_ref = None;
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

    pub(crate) fn scrub_to(&mut self, sec: f64) {
        self.virtual_sec = frame_to_seconds(
            seconds_to_frame(sec.max(0.0), self.selected_source_fps),
            self.selected_source_fps,
        );
        self.queue_pause_and_seek();
    }

    pub(crate) fn scrub_to_frame(&mut self, frame: i64) {
        self.virtual_sec = frame_to_seconds(frame.max(0), self.selected_source_fps);
        self.queue_pause_and_seek();
    }

    pub(crate) fn nudge_frames(&mut self, frames: i64) {
        let fps = if self.selected_source_fps.is_finite() && self.selected_source_fps > 0.0 {
            self.selected_source_fps
        } else {
            25.0
        };
        let next = self.virtual_sec + (frames as f64) / fps;
        self.virtual_sec = frame_to_seconds(
            seconds_to_frame(next.max(0.0), fps),
            fps,
        );
        // CueFrame (not scrub debounce) — same as Story ←/→ for exact IN/OUT.
        self.queue_seek_to_playhead();
    }
}

impl PlayerClient for IngestScreen {
    fn drain_playback_commands(&mut self) -> Vec<PlaybackCommand> {
        crate::player_bridge::compact_playback_commands(
            self.pending_playback_commands.drain(..),
            self.playing,
        )
    }

    fn clear_pending_seeks(&mut self) {}

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

    fn playback_is_playing(&self) -> bool {
        self.playing
    }

    fn missing_source_message(&self) -> String {
        "Odaberi klip prije play".into()
    }

    fn missing_path_message(&self) -> String {
        "Nema play path na sourceu (kartica / folder) — odaberi klip s medijem".into()
    }

    fn set_player_preview_active(&mut self, _active: bool) {}

    fn apply_playback_command_state(&mut self, playing: bool, status: impl Into<String>) {
        self.playing = playing;
        self.player_status = status.into();
    }

    fn apply_player_error(&mut self, status: impl Into<String>) {
        self.playing = false;
        self.player_status = status.into();
    }

    fn apply_player_frame(&mut self, _image: ColorImage, source_sec: f64, playing: bool) {
        self.playing = playing;
        self.player_status = "Broadcast player".into();
        // Editorial mirror — timeline paint uses PlaybackStack carrier, not this field.
        self.virtual_sec = frame_to_seconds(
            seconds_to_frame(source_sec.max(0.0), self.selected_source_fps),
            self.selected_source_fps,
        );
    }

    fn apply_player_state(&mut self, source_sec: f64, playing: bool, status: impl Into<String>) {
        self.playing = playing;
        self.player_status = status.into();
        self.virtual_sec = frame_to_seconds(
            seconds_to_frame(source_sec.max(0.0), self.selected_source_fps),
            self.selected_source_fps,
        );
    }
}
