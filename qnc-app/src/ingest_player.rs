//! Ingest **form** adapter for the shared broadcast player ([`PlayerClient`]).
//! Not a separate player — only open metadata + player event projection.

use eframe::egui::ColorImage;

use crate::components::{PlaybackMediaResolution, PlaybackMediaResolverComponent};
use crate::frame_time::{frame_to_seconds, seconds_to_frame};
use crate::ingest::{IngestAction, IngestScreen};
use crate::playback_routing::PlaybackTransportIntent;
use crate::player_bridge::PlayerClient;
use crate::player_contract::{BroadcastHostSourceRef, BroadcastSourceTimebase, FrameNumber};

const INGEST_PLAYBACK_INSTANCE_ID: &str = "ingest";

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
        self.playback_media_path()?;
        Some(self.virtual_frame.max(0))
    }

    pub fn reset_player_session(&mut self) {
        self.selected_play_path.clear();
        self.selected_play_input_clip_id.clear();
        self.pending_play_input_clip_id.clear();
        self.pending_play_after_resolve = false;
        self.pending_backend_commands.clear();
        self.selected_source_ref = None;
        self.selected_source_timebase = BroadcastSourceTimebase::default();
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
        self.pending_play_after_resolve = false;

        let Some((source_timebase, has_audio, channels, duration)) = self
            .state
            .as_ref()
            .and_then(|st| st.clips.iter().find(|c| c.clip_id == clip_id))
            .and_then(|c| {
                let source_timebase = BroadcastSourceTimebase::from_i64(
                    c.source_timebase.fps_num,
                    c.source_timebase.fps_den,
                )
                .or_else(|| timebase_from_probe_fps(c.fps))?;
                Some((
                    source_timebase,
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
            self.selected_source_timebase = BroadcastSourceTimebase::default();
            self.selected_source_has_audio = false;
            self.selected_source_audio_channels = 0;
            self.selected_source_ref = None;
            self.player_status = format!("Klip '{clip_id}' nema potvrđen source timebase");
            self.request_playback_input(project_id, clip_id);
            return;
        };

        let fps = source_timebase.fps().unwrap_or(0.0);
        self.selected_source_fps = fps;
        self.selected_source_timebase = source_timebase;
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

        self.request_playback_input(project_id, clip_id);
    }

    pub(crate) fn ensure_preview_playback_ready(&mut self, project_id: &str) {
        if self.playing {
            return;
        }
        let clip_id = self.preview_clip_id.trim().to_string();
        if clip_id.is_empty() {
            return;
        }
        let source_ready = self.selected_source_ref.as_ref().is_some_and(|source_ref| {
            source_ref.project_id == project_id
                && source_ref.clip_id == clip_id
                && self.selected_source_timebase.is_valid()
                && self.selected_source_fps.is_finite()
                && self.selected_source_fps > 0.0
        });
        let path_ready = {
            let p = self.selected_play_path.trim();
            self.selected_play_input_clip_id == clip_id && !p.is_empty()
        };
        if source_ready && path_ready {
            return;
        }
        self.activate_preview_clip(project_id, &clip_id);
    }

    pub(crate) fn request_play_after_resolve(&mut self) {
        if self.preview_clip_id.trim().is_empty() {
            return;
        }
        self.pending_play_after_resolve = true;
    }

    fn request_playback_input(&mut self, project_id: &str, clip_id: &str) {
        let project_id = project_id.trim();
        let clip_id = clip_id.trim();
        if project_id.is_empty() || clip_id.is_empty() {
            self.selected_play_path.clear();
            self.selected_play_input_clip_id.clear();
            self.pending_play_input_clip_id.clear();
            return;
        }
        if self.selected_play_input_clip_id == clip_id && !self.selected_play_path.trim().is_empty()
        {
            self.player_status = format!("Play · {clip_id}");
            return;
        }
        if self.pending_play_input_clip_id == clip_id {
            self.player_status = format!("Play · {clip_id} (media resolve)");
            return;
        }
        self.selected_play_path.clear();
        self.selected_play_input_clip_id.clear();
        self.pending_play_input_clip_id = clip_id.to_string();
        self.pending_backend_commands
            .push(PlaybackMediaResolverComponent::resolve_playback_proxy(
                INGEST_PLAYBACK_INSTANCE_ID,
                project_id,
                clip_id,
            ));
        self.player_status = format!("Play · {clip_id} (media resolve)");
    }

    pub fn apply_playback_media_resolution(
        &mut self,
        project_id: &str,
        clip_id: &str,
        resolution: PlaybackMediaResolution,
    ) -> PlaybackTransportIntent {
        if !self.playback_project_matches(project_id) || self.preview_clip_id != clip_id {
            return PlaybackTransportIntent::None;
        }
        self.apply_resolved_playback_metadata(project_id, clip_id, &resolution);
        if self.pending_play_input_clip_id == clip_id {
            self.pending_play_input_clip_id.clear();
        }
        let play_after_resolve = self.pending_play_after_resolve;
        self.pending_play_after_resolve = false;
        self.selected_play_path = resolution.media_input.trim().to_string();
        self.selected_play_input_clip_id = clip_id.to_string();
        self.player_status = format!("Play · {clip_id} ({})", resolution.locator_kind);
        if self.transport_cue_frame().is_none() {
            return PlaybackTransportIntent::None;
        }
        if play_after_resolve {
            PlaybackTransportIntent::TogglePlay
        } else {
            PlaybackTransportIntent::CueFrame(self.virtual_frame.max(0))
        }
    }

    fn apply_resolved_playback_metadata(
        &mut self,
        project_id: &str,
        clip_id: &str,
        resolution: &PlaybackMediaResolution,
    ) {
        let Some(source_timebase) = resolution
            .source_timebase
            .filter(|timebase| timebase.is_valid())
        else {
            return;
        };
        let Some(source_fps) = source_timebase.fps().filter(|fps| *fps > 0.0) else {
            return;
        };
        let duration_frames = resolution
            .duration_frames
            .or_else(|| {
                resolution
                    .duration_sec
                    .filter(|duration| duration.is_finite() && *duration > 0.0)
                    .map(|duration| seconds_to_frame(duration, source_fps).max(1))
            })
            .filter(|frames| *frames > 0);
        let Some(duration_frames) = duration_frames else {
            return;
        };
        let duration_sec = resolution
            .duration_sec
            .filter(|duration| duration.is_finite() && *duration > 0.0)
            .unwrap_or_else(|| frame_to_seconds(duration_frames, source_fps));
        self.selected_source_fps = source_fps;
        self.selected_source_timebase = source_timebase;
        self.selected_source_has_audio = resolution.has_audio.unwrap_or(false);
        self.selected_source_audio_channels = resolution.audio_channels.unwrap_or_else(|| {
            if self.selected_source_has_audio {
                2
            } else {
                0
            }
        });
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
        self.merge_resolved_playback_metadata_into_state(
            clip_id,
            source_timebase,
            source_fps,
            duration_sec,
            resolution.has_audio,
            resolution.audio_channels,
        );
    }

    fn merge_resolved_playback_metadata_into_state(
        &mut self,
        clip_id: &str,
        source_timebase: BroadcastSourceTimebase,
        source_fps: f64,
        duration_sec: f64,
        has_audio: Option<bool>,
        audio_channels: Option<u8>,
    ) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let Some(clip) = state.clips.iter_mut().find(|clip| clip.clip_id == clip_id) else {
            return;
        };
        clip.fps = source_fps;
        clip.source_timebase = crate::api::EditorialSourceTimebase {
            fps_num: i64::from(source_timebase.fps_num),
            fps_den: i64::from(source_timebase.fps_den),
        };
        if duration_sec.is_finite() && duration_sec > 0.0 {
            clip.duration_sec = duration_sec;
        }
        if let Some(has_audio) = has_audio {
            clip.has_audio = has_audio;
        }
        if let Some(audio_channels) = audio_channels.filter(|channels| *channels > 0) {
            clip.audio_channels = audio_channels;
        }
    }

    pub fn set_playback_media_resolution_error(
        &mut self,
        project_id: &str,
        clip_id: &str,
        error: impl Into<String>,
    ) {
        if !self.playback_project_matches(project_id) || self.preview_clip_id != clip_id {
            return;
        }
        if self.pending_play_input_clip_id == clip_id {
            self.pending_play_input_clip_id.clear();
        }
        self.pending_play_after_resolve = false;
        self.selected_play_path.clear();
        self.selected_play_input_clip_id.clear();
        self.player_status = error.into();
    }

    fn playback_project_matches(&self, project_id: &str) -> bool {
        let project_id = project_id.trim();
        !project_id.is_empty()
            && (self.loaded_for_project == project_id || self.project_id == project_id)
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

fn timebase_from_probe_fps(fps: f64) -> Option<BroadcastSourceTimebase> {
    if !fps.is_finite() || fps <= 0.0 {
        return None;
    }
    let common = [
        (24000.0 / 1001.0, 24000, 1001),
        (30000.0 / 1001.0, 30000, 1001),
        (60000.0 / 1001.0, 60000, 1001),
    ];
    for (value, num, den) in common {
        if (fps - value).abs() < 0.01 {
            return Some(BroadcastSourceTimebase {
                fps_num: num,
                fps_den: den,
            });
        }
    }
    let rounded = fps.round();
    if (fps - rounded).abs() < 0.001 {
        return BroadcastSourceTimebase::from_i64(rounded as i64, 1);
    }
    let den = 1000i64;
    let num = (fps * den as f64).round() as i64;
    if num <= 0 {
        return None;
    }
    let gcd = gcd_i64(num, den);
    BroadcastSourceTimebase::from_i64(num / gcd, den / gcd)
}

fn gcd_i64(mut a: i64, mut b: i64) -> i64 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a.max(1)
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

    fn playback_source_timebase(&self) -> Option<BroadcastSourceTimebase> {
        self.selected_source_timebase
            .is_valid()
            .then_some(self.selected_source_timebase)
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
        "Ingest play input nije spreman — čekam media resolver".into()
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
