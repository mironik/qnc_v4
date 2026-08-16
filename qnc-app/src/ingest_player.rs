//! Ingest **form** adapter for the shared broadcast player ([`PlayerClient`]).
//! Not a separate player — only open metadata + player event projection.

use eframe::egui::ColorImage;

use crate::frame_time::{frame_to_seconds, seconds_to_frame};
use crate::ingest::{IngestAction, IngestScreen};
use crate::player_bridge::PlayerClient;
use crate::player_contract::{BroadcastHostSourceRef, FrameNumber};

impl IngestScreen {
    pub fn playback_source_ref(&self) -> Option<&BroadcastHostSourceRef> {
        self.selected_source_ref.as_ref()
    }

    #[allow(dead_code)]
    pub fn playback_source_fps(&self) -> f64 {
        PlayerClient::playback_source_fps(self)
    }

    #[allow(dead_code)]
    pub fn playback_source_has_audio(&self) -> bool {
        PlayerClient::playback_source_has_audio(self)
    }

    #[allow(dead_code)]
    pub fn playback_source_audio_channels(&self) -> u8 {
        PlayerClient::playback_source_audio_channels(self)
    }

    #[allow(dead_code)]
    pub fn playback_source_sec(&self) -> f64 {
        if self.selected_source_fps.is_finite() && self.selected_source_fps > 0.0 {
            frame_to_seconds(self.virtual_frame, self.selected_source_fps)
        } else {
            0.0
        }
    }

    #[allow(dead_code)]
    pub fn playback_media_path(&self) -> Option<String> {
        PlayerClient::playback_media_path(self)
    }

    #[allow(dead_code)]
    pub fn set_player_preview_active(&mut self, active: bool) {
        PlayerClient::set_player_preview_active(self, active)
    }

    #[allow(dead_code)]
    pub fn apply_playback_command_state(&mut self, playing: bool, status: impl Into<String>) {
        PlayerClient::apply_playback_command_state(self, playing, status)
    }

    pub fn apply_player_error(&mut self, status: impl Into<String>) {
        PlayerClient::apply_player_error(self, status)
    }

    #[allow(dead_code)]
    pub fn apply_player_frame(
        &mut self,
        image: ColorImage,
        source_frame: FrameNumber,
        playing: bool,
    ) {
        PlayerClient::apply_player_frame(self, image, source_frame, playing)
    }

    #[allow(dead_code)]
    pub fn apply_player_state(
        &mut self,
        source_frame: FrameNumber,
        playing: bool,
        status: impl Into<String>,
    ) {
        PlayerClient::apply_player_state(self, source_frame, playing, status)
    }

    pub(crate) fn transport_cue_frame(&self) -> Option<i64> {
        self.selected_source_ref.as_ref()?;
        Some(self.virtual_frame.max(0))
    }

    pub fn reset_player_session(&mut self) {
        self.selected_play_path.clear();
        self.selected_source_ref = None;
        self.playing = false;
        self.virtual_frame = 0;
    }

    pub(crate) fn activate_preview_clip(&mut self, project_id: &str, clip_id: &str) {
        if clip_id.trim().is_empty() {
            return;
        }
        self.preview_clip_id = clip_id.to_string();
        self.virtual_frame = 0;
        self.playing = false;

        let Some((fps, has_audio, channels, duration)) = self
            .state
            .as_ref()
            .and_then(|st| st.clips.iter().find(|c| c.clip_id == clip_id))
            .and_then(|c| {
                (c.fps.is_finite() && c.fps > 0.0).then_some((
                    c.fps,
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
                ))
            })
        else {
            self.selected_source_fps = 0.0;
            self.selected_source_has_audio = false;
            self.selected_source_audio_channels = 0;
            self.selected_source_ref = None;
            self.player_status = format!("Klip '{clip_id}' nema potvrđen source FPS");
            self.resolve_play_path(clip_id);
            return;
        };

        self.selected_source_fps = fps;
        self.selected_source_has_audio = has_audio;
        self.selected_source_audio_channels = channels;
        let duration_frames = seconds_to_frame(duration, fps).max(1);
        self.selected_source_ref = BroadcastHostSourceRef::from_frame_fields(
            project_id,
            clip_id,
            clip_id,
            clip_id,
            Some(FrameNumber(0)),
            Some(FrameNumber(duration_frames)),
            FrameNumber(duration_frames),
        )
        .ok();

        self.resolve_play_path(clip_id);
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

    #[allow(dead_code)]
    pub(crate) fn scrub_to(&mut self, sec: f64) {
        if self.selected_source_fps.is_finite() && self.selected_source_fps > 0.0 {
            self.virtual_frame = seconds_to_frame(sec.max(0.0), self.selected_source_fps).max(0);
        }
    }

    #[allow(dead_code)]
    pub(crate) fn scrub_to_frame(&mut self, frame: i64) {
        self.virtual_frame = frame.max(0);
    }

    pub(crate) fn nudge_frames(&mut self, frames: i64) -> IngestAction {
        self.virtual_frame = self.virtual_frame.saturating_add(frames).max(0);
        IngestAction::CueFrame(self.virtual_frame.max(0))
    }
}

impl PlayerClient for IngestScreen {
    fn playback_source_ref(&self) -> Option<BroadcastHostSourceRef> {
        self.selected_source_ref.clone()
    }

    fn playback_media_path(&self) -> Option<String> {
        let path = self.selected_play_path.trim();
        if path.is_empty() {
            None
        } else {
            Some(path.to_string())
        }
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

    fn apply_player_frame(&mut self, _image: ColorImage, source_frame: FrameNumber, playing: bool) {
        self.playing = playing;
        self.player_status = "Broadcast player".into();
        // Editorial mirror — timeline paint uses PlaybackStack carrier, not this field.
        self.virtual_frame = source_frame.0.max(0);
    }

    fn apply_player_state(
        &mut self,
        source_frame: FrameNumber,
        playing: bool,
        status: impl Into<String>,
    ) {
        self.playing = playing;
        self.player_status = status.into();
        self.virtual_frame = source_frame.0.max(0);
    }
}
