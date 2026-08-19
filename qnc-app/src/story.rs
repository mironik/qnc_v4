//! Native editorial form (Story / Media Assist) — one screen, role attributes.
//!
//! UI blocks (`qnc_ui`, `editorial::*`, `qnc_source_dock`, cards) are shared
//! components. This form only chooses composition via `EditorialRole`.

mod focus;
pub(crate) mod playback_controls;
mod playback_transport;
mod source_editor;
mod story_edit;
mod story_selection;
mod story_state;
mod story_timeline;

pub(super) use crate::editorial::types::{
    LibraryTab, MarkerSlot, StoryCover, StoryMarker, StoryPart, StoryShot,
};

use std::collections::HashMap;
use std::time::{Duration, Instant};

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions, Vec2};
use serde_json::Value;

use crate::api::{EditorialPlaylist, EditorialPlaylistSegment, HostClient, TimelineModel};
use crate::component_runtime::ComponentBackendCommand;
use crate::components::{EditorialEditComponent, EditorialEditData, EditorialEditKind};
use crate::composition::EditorialRole;
use crate::editorial::common::{shot_id, truncate};
use crate::editorial::segment_program::SegmentProgramModel;
use crate::editorial::{marker_cover_panel, media_pool, segment_panel};
use crate::frame_time::{frame_to_seconds, normalize_fps, seconds_to_frame, seconds_to_timecode};
use crate::media_assets::{
    self, AsyncImageAssetLoader, AsyncSourceMediaAssetLoader, ImageAssetKey,
};
use crate::playback_routing::PlaybackTransportIntent;
use crate::playback_stack::PlaybackStack;
use crate::player_contract::BroadcastHostSourceRef;
use crate::player_contract::FrameNumber;
use crate::qnc_filmstrip_background::FilmFrame;
use crate::qnc_timeline::{ExpandedAudio, TimelineFocusPaint};
use crate::shortcuts::{StoryBindings, STORYBOARD_SHORTCUT_SCOPE as STORY_SHORTCUT_SCOPE};

use self::focus::{FocusTarget, TimelineFocus};
use self::playback_transport::{StoryPlaybackView, StoryTogglePlayInput, StoryTogglePlayOutcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Wrap,
    Source,
}

/// Shared Story / Media Assist form — configure with [`EditorialRole`].
pub struct StoryScreen {
    /// Composition attributes (head tabs, card facets, …) — not a code fork.
    role: EditorialRole,
    project_id: String,
    loaded_project_id: String,
    meta_loading: bool,
    meta_pending: usize,
    state_loaded: bool,
    timeline_loaded: bool,
    playlist_loaded: bool,
    initial_selection_done: bool,
    timeline: Option<TimelineModel>,
    playlist: Option<EditorialPlaylist>,
    story_state_snapshot: Option<Value>,
    parts: Vec<StoryPart>,
    all_clips: Vec<StoryShot>,
    virtual_shots: Vec<StoryShot>,
    covers: Vec<StoryCover>,
    markers: Vec<StoryMarker>,
    marker_slots: Vec<MarkerSlot>,
    selected_part_id: String,
    selected_shot_id: String,
    selected_clip_id: String,
    selected_cover_id: String,
    selected_slot_id: String,
    selected_marker_id: String,
    library_tab: LibraryTab,
    view_mode: ViewMode,
    source_in_frame: i64,
    source_out_frame: i64,
    source_playhead_frame: i64,
    wrap_playhead_frame: i64,
    selected_shot_in_frame: i64,
    selected_shot_out_frame: i64,
    /// True after user pressed Mark IN on this clip — required for select_mark_in focus.
    mark_in_set: bool,
    /// True after user pressed Mark OUT on this clip — required for select_mark_out focus.
    mark_out_set: bool,
    selected_source_fps: f64,
    selected_source_has_audio: bool,
    selected_source_audio_channels: u8,
    selected_source_ref: Option<BroadcastHostSourceRef>,
    /// Absolute local proxy path for broadcast player (from host play-media).
    selected_play_path: String,
    story_summary: String,
    draft_status: String,
    pending_wrap_scrub_part_id: Option<String>,
    virtual_frame: i64,
    /// Frame playhead — synced from player / carrier (timeline authority).
    playhead_frame: i64,
    playing: bool,
    status: String,
    /// True while broadcast PlayerRemote owns the monitor.
    broadcast_preview_active: bool,
    thumb_textures: HashMap<String, TextureHandle>,
    thumbs_queued: Vec<String>,
    a1_peaks: Vec<f32>,
    a2_peaks: Vec<f32>,
    film_frames: Vec<FilmFrame>,
    waveform_clip_id: String,
    image_loader: AsyncImageAssetLoader,
    source_media_loader: AsyncSourceMediaAssetLoader,
    repaint_ctx: Option<egui::Context>,
    source_media_retry_at: Option<Instant>,
    /// Pending frame target while the player cue catches up.
    source_timebase_ready: bool,
    /// Sticky keyboard owner for the bottom source timeline.
    /// Cleared only by explicit Wrap/segment entry.
    source_dock_keyboard_focus: bool,
    /// Keyboard chords from host catalog + DB (or local file / builtin).
    bindings: StoryBindings,
    shortcuts_loading: bool,
    shortcuts_pending: usize,
    shortcuts_loaded: bool,
    shortcut_catalog: Option<Value>,
    shortcut_user: Option<Value>,
    pending_backend_commands: Vec<ComponentBackendCommand>,
    next_backend_request_id: u64,
    focus: TimelineFocus,
    /// Web kodak: click A1/A2 label expands that wave lane.
    expanded_audio: ExpandedAudio,
}

impl Default for StoryScreen {
    fn default() -> Self {
        Self::with_role(EditorialRole::Story)
    }
}

impl StoryScreen {
    pub fn story() -> Self {
        Self::with_role(EditorialRole::Story)
    }

    pub fn media_assist() -> Self {
        Self::with_role(EditorialRole::MediaAssist)
    }

    fn with_role(role: EditorialRole) -> Self {
        Self {
            role,
            project_id: String::new(),
            loaded_project_id: String::new(),
            meta_loading: false,
            meta_pending: 0,
            state_loaded: false,
            timeline_loaded: false,
            playlist_loaded: false,
            initial_selection_done: false,
            timeline: None,
            playlist: None,
            story_state_snapshot: None,
            parts: Vec::new(),
            all_clips: Vec::new(),
            virtual_shots: Vec::new(),
            covers: Vec::new(),
            markers: Vec::new(),
            marker_slots: Vec::new(),
            selected_part_id: String::new(),
            selected_shot_id: String::new(),
            selected_clip_id: String::new(),
            selected_cover_id: String::new(),
            selected_slot_id: String::new(),
            selected_marker_id: String::new(),
            library_tab: LibraryTab::All,
            view_mode: ViewMode::Source,
            source_in_frame: 0,
            source_out_frame: 0,
            source_playhead_frame: 0,
            wrap_playhead_frame: 0,
            selected_shot_in_frame: 0,
            selected_shot_out_frame: 0,
            mark_in_set: false,
            mark_out_set: false,
            selected_source_fps: 0.0,
            selected_source_has_audio: false,
            selected_source_audio_channels: 0,
            selected_source_ref: None,
            selected_play_path: String::new(),
            story_summary: String::new(),
            draft_status: "draft".into(),
            pending_wrap_scrub_part_id: None,
            virtual_frame: 0,
            playhead_frame: 0,
            playing: false,
            status: role.idle_status().into(),
            broadcast_preview_active: false,
            thumb_textures: HashMap::new(),
            thumbs_queued: Vec::new(),
            a1_peaks: Vec::new(),
            a2_peaks: Vec::new(),
            film_frames: Vec::new(),
            waveform_clip_id: String::new(),
            image_loader: AsyncImageAssetLoader::new(),
            source_media_loader: AsyncSourceMediaAssetLoader::new(),
            repaint_ctx: None,
            source_media_retry_at: None,
            source_timebase_ready: false,
            source_dock_keyboard_focus: false,
            bindings: StoryBindings::empty(),
            shortcuts_loading: false,
            shortcuts_pending: 0,
            shortcuts_loaded: false,
            shortcut_catalog: None,
            shortcut_user: None,
            pending_backend_commands: Vec::new(),
            next_backend_request_id: 1,
            focus: TimelineFocus::default(),
            expanded_audio: ExpandedAudio::None,
        }
    }

    fn timeline_fps(&self) -> Option<f64> {
        self.playlist
            .as_ref()
            .map(|p| p.timeline_fps)
            .filter(|fps| fps.is_finite() && *fps > 0.0)
            .or_else(|| {
                self.timeline
                    .as_ref()
                    .map(|t| t.timeline_fps)
                    .filter(|fps| fps.is_finite() && *fps > 0.0)
            })
    }

    fn source_timebase_fps(&self) -> Option<f64> {
        if self.source_timebase_ready
            && self.selected_source_fps.is_finite()
            && self.selected_source_fps > 0.0
        {
            Some(normalize_fps(self.selected_source_fps))
        } else {
            None
        }
    }

    fn active_playback_source_ref(&self) -> Option<&BroadcastHostSourceRef> {
        match self.view_mode {
            ViewMode::Source => self.selected_source_ref.as_ref(),
            ViewMode::Wrap => None,
        }
    }

    fn playlist_input_available(&self) -> bool {
        self.playlist.as_ref().is_some_and(|playlist| {
            playlist.timeline_fps.is_finite()
                && playlist.timeline_fps > 0.0
                && playlist.duration_frames > 0
                && playlist.segments.iter().any(|segment| {
                    segment.streamable || segment.covers.iter().any(|cover| cover.streamable)
                })
        })
    }

    fn active_playback_media_path(&self) -> Option<&str> {
        let path = match self.view_mode {
            ViewMode::Source => self.selected_play_path.trim(),
            ViewMode::Wrap => "",
        };
        (!path.is_empty()).then_some(path)
    }

    fn active_playback_fps(&self) -> f64 {
        match self.view_mode {
            ViewMode::Source => self.selected_source_fps,
            ViewMode::Wrap => 0.0,
        }
    }

    fn active_playback_has_audio(&self) -> bool {
        match self.view_mode {
            ViewMode::Source => self.selected_source_has_audio,
            ViewMode::Wrap => false,
        }
    }

    fn active_playback_audio_channels(&self) -> u8 {
        match self.view_mode {
            ViewMode::Source => self.selected_source_audio_channels,
            ViewMode::Wrap => 0,
        }
    }

    fn source_sec_from_frame(&self, frame: i64) -> Option<f64> {
        self.source_timebase_fps()
            .map(|fps| frame_to_seconds(frame.max(0), fps))
    }

    fn timeline_sec_from_frame(&self, frame: i64) -> Option<f64> {
        let frame = frame.max(0);
        if let Some(playlist) = self.playlist.as_ref() {
            if let Some(segment) = playlist.segments.iter().find(|segment| {
                let start = segment.global_start_frame.max(0);
                let end = segment
                    .global_end_frame
                    .max(start + segment.duration_frames.max(0))
                    .max(start + 1);
                frame >= start && frame < end
            }) {
                let start = segment.global_start_frame.max(0);
                let end = segment
                    .global_end_frame
                    .max(start + segment.duration_frames.max(0))
                    .max(start + 1);
                let local = (frame - start).clamp(0, end - start);
                let start_sec = segment.global_start_sec.max(0.0);
                let end_sec = segment.global_end_sec.max(start_sec);
                if end_sec > start_sec {
                    return Some(
                        start_sec + (end_sec - start_sec) * local as f64 / (end - start) as f64,
                    );
                }
                let fps = if segment.source_fps.is_finite() && segment.source_fps > 0.0 {
                    segment.source_fps
                } else {
                    playlist.timeline_fps
                };
                if fps.is_finite() && fps > 0.0 {
                    return Some(start_sec + frame_to_seconds(local, normalize_fps(fps)));
                }
            }
            if playlist.duration_frames > 0 && frame >= playlist.duration_frames {
                return Some(playlist.duration_sec.max(0.0));
            }
            if frame == 0 {
                return Some(0.0);
            }
        }
        if let Some(timeline) = self.timeline.as_ref() {
            if let Some(segment) = story_timeline::active_segment_frame(Some(timeline), frame) {
                let start = segment.global_start_frame.max(0);
                let end = segment.global_end_frame.max(start + 1);
                let local = (frame - start).clamp(0, end - start);
                let span_sec = (segment.global_end_sec - segment.global_start_sec).max(0.0);
                return Some(
                    segment.global_start_sec + span_sec * local as f64 / (end - start) as f64,
                );
            }
            if timeline.duration_frames > 0 && frame >= timeline.duration_frames {
                return Some(timeline.duration_sec.max(0.0));
            }
            if frame == 0 {
                return Some(0.0);
            }
        }
        self.timeline_fps()
            .map(|fps| frame_to_seconds(frame.max(0), fps))
    }

    fn virtual_sec(&self) -> f64 {
        match self.view_mode {
            ViewMode::Source => self
                .source_sec_from_frame(self.source_playhead_frame)
                .unwrap_or(0.0),
            ViewMode::Wrap => self
                .timeline_sec_from_frame(self.wrap_playhead_frame)
                .unwrap_or(0.0),
        }
    }

    fn part_source_fps(&self, part: &StoryPart) -> Option<f64> {
        if part.fps.is_finite() && part.fps > 0.0 {
            Some(normalize_fps(part.fps))
        } else {
            self.clip_source_meta(&part.clip_id)
                .map(|(fps, _, _, _, _)| fps)
                .filter(|fps| fps.is_finite() && *fps > 0.0)
                .map(normalize_fps)
        }
    }

    fn part_local_tc(&self, part_id: &str, local_frame: i64) -> String {
        self.parts
            .iter()
            .find(|part| part.part_id == part_id)
            .and_then(|part| self.part_source_fps(part))
            .map(|fps| seconds_to_timecode(frame_to_seconds(local_frame.max(0), fps), fps))
            .unwrap_or_else(|| "--:--:--:--".into())
    }

    fn set_source_playhead_frame(&mut self, frame: i64) {
        let frame = frame.max(0);
        self.source_playhead_frame = frame;
        if self.view_mode == ViewMode::Source {
            self.virtual_frame = frame;
            self.playhead_frame = frame;
        }
    }

    fn set_wrap_playhead_frame(&mut self, frame: i64) {
        let frame = frame.max(0);
        self.wrap_playhead_frame = frame;
        if self.view_mode == ViewMode::Wrap {
            self.virtual_frame = frame;
            self.playhead_frame = frame;
        }
    }

    fn source_dock_uses_live_carrier(&self, playback: &PlaybackStack) -> bool {
        if self.view_mode != ViewMode::Source || !playback.carrier().is_active() {
            return false;
        }
        self.selected_source_ref
            .as_ref()
            .is_some_and(|source_ref| playback.active_source_matches(source_ref))
    }

    fn source_dock_playhead_frame(
        &self,
        playback: &PlaybackStack,
        clip_duration_frames: i64,
    ) -> i64 {
        let frame = if self.source_dock_uses_live_carrier(playback) {
            playback.carrier().display_frame().0
        } else {
            self.source_playhead_frame
        };
        frame.clamp(0, clip_duration_frames.max(1))
    }

    fn source_dock_timeline_model(
        &self,
        playback: &PlaybackStack,
        fps: f64,
        clip_duration_frames: i64,
        shot_in_frame: i64,
        shot_out_frame: i64,
        draft_in_frame: i64,
        draft_out_frame: i64,
    ) -> crate::qnc_timeline_progress::TimelineProgressModel {
        let fallback_frame = self.source_dock_playhead_frame(playback, clip_duration_frames);
        let live_source_ref = if self.view_mode == ViewMode::Source {
            self.selected_source_ref.as_ref()
        } else {
            None
        };
        playback.timeline_model_for_source_ref(
            live_source_ref,
            fps,
            clip_duration_frames,
            shot_in_frame,
            shot_out_frame,
            draft_in_frame,
            draft_out_frame,
            fallback_frame,
        )
    }

    fn clip_source_meta(&self, clip_id: &str) -> Option<(f64, bool, u8, i64, String)> {
        let clip_id = clip_id.trim();
        if clip_id.is_empty() {
            return None;
        }
        self.all_clips
            .iter()
            .chain(self.virtual_shots.iter())
            .find(|shot| shot.clip_id == clip_id)
            .map(|shot| {
                (
                    shot.fps,
                    shot.has_audio,
                    shot.audio_channels,
                    shot.duration_frames,
                    shot.play_path.trim().to_string(),
                )
            })
    }

    fn playlist_segment_by_id(&self, part_id: &str) -> Option<&EditorialPlaylistSegment> {
        let part_id = part_id.trim();
        if part_id.is_empty() {
            return None;
        }
        self.playlist
            .as_ref()?
            .segments
            .iter()
            .find(|segment| segment.part_id == part_id)
    }

    fn playlist_segment_at_frame(&self, frame: i64) -> Option<&EditorialPlaylistSegment> {
        let frame = frame.max(0);
        self.playlist.as_ref()?.segments.iter().find(|segment| {
            let start = segment.global_start_frame.max(0);
            let end = segment
                .global_end_frame
                .max(start + segment.duration_frames.max(0))
                .max(start + 1);
            frame >= start && frame < end
        })
    }

    fn segment_program_model(&self) -> SegmentProgramModel {
        SegmentProgramModel::from_playlist(
            self.playlist.as_ref(),
            &self.marker_slots,
            &self.covers,
            &self.markers,
        )
    }

    fn source_tc_frame(&self, frame: i64) -> String {
        self.source_timebase_fps()
            .map(|fps| seconds_to_timecode(frame_to_seconds(frame.max(0), fps), fps))
            .unwrap_or_else(|| "--:--:--:--".into())
    }

    fn tc(&self, sec: f64) -> String {
        self.timeline_fps()
            .map(|fps| seconds_to_timecode(sec, fps))
            .unwrap_or_else(|| "--:--:--:--".into())
    }

    pub fn needs_meta_load(&self, project_id: &str) -> bool {
        let project_id = project_id.trim();
        !project_id.is_empty() && self.loaded_project_id != project_id && !self.meta_loading
    }

    pub fn begin_meta_load(&mut self, project_id: &str, expected_results: usize) {
        let role = self.role;
        *self = Self::with_role(role);
        self.project_id = project_id.to_string();
        self.loaded_project_id = project_id.to_string();
        self.meta_loading = expected_results > 0;
        self.meta_pending = expected_results;
        if self.meta_loading {
            self.status = "Učitavam editorial snapshot...".into();
        }
    }

    pub fn apply_editorial_story_state(&mut self, project_id: &str, state: Value) {
        if self.loaded_project_id != project_id {
            return;
        }
        self.story_state_snapshot = Some(state.clone());
        self.apply_story_state(&state);
        self.state_loaded = true;
        self.finish_meta_result();
    }

    pub fn apply_editorial_timeline_model(
        &mut self,
        project_id: &str,
        timeline: TimelineModel,
    ) -> PlaybackTransportIntent {
        if self.loaded_project_id != project_id {
            return PlaybackTransportIntent::None;
        }
        let was_wrap = self.view_mode == ViewMode::Wrap;
        self.timeline = Some(timeline);
        self.timeline_loaded = true;
        if let Some(state) = self.story_state_snapshot.clone() {
            self.apply_story_state(&state);
        } else {
            self.refresh_story_summary();
        }
        self.finish_meta_result();
        self.wrap_projection_after_program_refresh(was_wrap)
    }

    pub fn apply_editorial_playlist(
        &mut self,
        project_id: &str,
        playlist: EditorialPlaylist,
    ) -> PlaybackTransportIntent {
        if self.loaded_project_id != project_id {
            return PlaybackTransportIntent::None;
        }
        let was_wrap = self.view_mode == ViewMode::Wrap;
        self.playlist = Some(playlist);
        self.playlist_loaded = true;
        self.finish_meta_result();
        self.wrap_projection_after_program_refresh(was_wrap)
    }

    fn wrap_projection_after_program_refresh(&mut self, was_wrap: bool) -> PlaybackTransportIntent {
        if !self.meta_ready() {
            return PlaybackTransportIntent::None;
        }
        if let Some(part_id) = self.pending_wrap_scrub_part_id.clone() {
            self.start_wrap_session_from_snapshot(Some(part_id));
            self.pending_wrap_scrub_part_id = None;
            PlaybackTransportIntent::None
        } else if was_wrap && self.initial_selection_done {
            let active_at_head = self
                .playlist_segment_at_frame(self.wrap_playhead_frame)
                .map(|segment| segment.part_id.clone());
            let selected = active_at_head.or_else(|| {
                (!self.selected_part_id.trim().is_empty()).then(|| self.selected_part_id.clone())
            });
            if selected.is_some() {
                let current_frame = self.wrap_playhead_frame;
                self.start_wrap_session_from_snapshot(selected);
                self.set_wrap_playhead_frame(current_frame);
            }
            PlaybackTransportIntent::None
        } else {
            PlaybackTransportIntent::None
        }
    }

    pub fn set_editorial_meta_error(&mut self, project_id: &str, error: impl Into<String>) {
        if self.loaded_project_id != project_id {
            return;
        }
        self.meta_loading = false;
        self.meta_pending = 0;
        self.pending_wrap_scrub_part_id = None;
        self.status = format!("editorial snapshot: {}", error.into());
    }

    pub fn meta_ready(&self) -> bool {
        !self.project_id.is_empty()
            && self.state_loaded
            && self.timeline_loaded
            && self.playlist_loaded
    }

    fn finish_meta_result(&mut self) {
        self.meta_pending = self.meta_pending.saturating_sub(1);
        if self.meta_pending == 0 {
            self.meta_loading = false;
        }
        self.apply_initial_projection_if_ready();
    }

    fn apply_initial_projection_if_ready(&mut self) {
        if !self.meta_ready() || self.initial_selection_done {
            return;
        }
        self.initial_selection_done = true;
        // Classic Story opens on source/All — not empty wrap.
        if let Some(first) = self.all_clips.first().cloned() {
            self.select_shot_from_snapshot(&first);
        } else {
            self.view_mode = ViewMode::Wrap;
            self.playing = false;
            self.status = "Wrap · broadcast".into();
        }
    }

    pub fn reset_session(&mut self, _host: &HostClient) {
        *self = Self::with_role(self.role);
    }

    pub fn drain_backend_commands(&mut self) -> Vec<ComponentBackendCommand> {
        std::mem::take(&mut self.pending_backend_commands)
    }

    fn edit_instance_id(&self) -> &'static str {
        match self.role {
            EditorialRole::Story => "story",
            EditorialRole::MediaAssist => "media_assist",
        }
    }

    fn next_backend_request_id(&mut self) -> u64 {
        let request_id = self.next_backend_request_id;
        self.next_backend_request_id = self.next_backend_request_id.saturating_add(1);
        request_id
    }

    fn enqueue_backend_command(&mut self, command: ComponentBackendCommand) {
        self.pending_backend_commands.push(command);
    }

    fn enqueue_edit_command<F>(&mut self, build: F)
    where
        F: FnOnce(&str, u64, &str) -> ComponentBackendCommand,
    {
        if self.project_id.trim().is_empty() {
            self.status = "Nema otvorenog projekta".into();
            return;
        }
        let instance_id = self.edit_instance_id();
        let request_id = self.next_backend_request_id();
        let project_id = self.project_id.clone();
        self.enqueue_backend_command(build(instance_id, request_id, &project_id));
    }

    pub fn needs_shortcuts_load(&self) -> bool {
        !self.shortcuts_loaded && !self.shortcuts_loading
    }

    pub fn begin_shortcuts_load(&mut self, expected_results: usize) {
        self.shortcuts_loading = expected_results > 0;
        self.shortcuts_pending = expected_results;
        self.shortcut_catalog = None;
        self.shortcut_user = None;
    }

    pub fn apply_shortcut_catalog(&mut self, scope: &str, catalog: Value) {
        if scope != STORY_SHORTCUT_SCOPE {
            return;
        }
        self.shortcut_catalog = Some(catalog);
        self.finish_shortcut_result();
    }

    pub fn apply_shortcut_user(&mut self, scope: &str, user: Value) {
        if scope != STORY_SHORTCUT_SCOPE {
            return;
        }
        self.shortcut_user = Some(user);
        self.finish_shortcut_result();
    }

    pub fn set_shortcuts_error(&mut self, scope: &str, port_id: &str, error: impl Into<String>) {
        if scope != STORY_SHORTCUT_SCOPE {
            return;
        }
        if port_id == "user" {
            self.shortcut_user = Some(Value::Null);
            self.finish_shortcut_result();
            return;
        }
        self.shortcuts_loading = false;
        self.shortcuts_pending = 0;
        self.shortcuts_loaded = true;
        self.status = format!("shortcut catalog: {}", error.into());
    }

    pub fn shortcuts_ready(&self) -> bool {
        self.shortcuts_loaded
    }

    fn finish_shortcut_result(&mut self) {
        self.shortcuts_pending = self.shortcuts_pending.saturating_sub(1);
        if self.shortcuts_pending == 0 {
            self.shortcuts_loading = false;
        }
        self.apply_shortcuts_if_ready();
    }

    fn apply_shortcuts_if_ready(&mut self) {
        if self.shortcuts_loaded {
            return;
        }
        let Some(catalog) = self.shortcut_catalog.as_ref() else {
            return;
        };
        let Some(user) = self.shortcut_user.as_ref() else {
            return;
        };
        self.bindings = StoryBindings::from_catalog(catalog, user, STORY_SHORTCUT_SCOPE);
        self.shortcuts_loaded = true;
        self.shortcuts_loading = false;
        self.shortcuts_pending = 0;
    }

    fn start_wrap_session(&mut self, host: &HostClient) -> PlaybackTransportIntent {
        let _ = host;
        self.start_wrap_session_from_snapshot(None);
        PlaybackTransportIntent::None
    }

    fn start_wrap_session_from_snapshot(&mut self, selected_part_id: Option<String>) {
        self.source_dock_keyboard_focus = false;
        self.view_mode = ViewMode::Wrap;
        self.virtual_frame = self.wrap_playhead_frame;
        self.playhead_frame = self.wrap_playhead_frame;
        self.playing = false;
        if let Some(part_id) = selected_part_id.filter(|id| !id.trim().is_empty()) {
            self.selected_part_id = part_id;
            if let Some(segment) = self.playlist_segment_by_id(&self.selected_part_id).cloned() {
                self.set_wrap_playhead_frame(segment.global_start_frame.max(0));
            }
        }
        self.status = "Playlist input · timeline".into();
    }

    /// Enter Wrap UI — editorial only. Preview stays on broadcast PlayerRemote.
    fn start_wrap_session_for_part(
        &mut self,
        host: &HostClient,
        selected_part_id: Option<String>,
    ) -> PlaybackTransportIntent {
        let _ = host;
        self.start_wrap_session_from_snapshot(selected_part_id);
        PlaybackTransportIntent::None
    }

    fn defer_wrap_scrub_after_timeline(&mut self, preferred_part_id: Option<String>) {
        let preferred = preferred_part_id
            .filter(|id| !id.trim().is_empty())
            .filter(|id| self.parts.iter().any(|part| part.part_id == *id));
        let selected = (!self.selected_part_id.trim().is_empty())
            .then(|| self.selected_part_id.clone())
            .filter(|id| self.parts.iter().any(|part| part.part_id == *id));
        let fallback = self.parts.first().map(|part| part.part_id.clone());
        self.pending_wrap_scrub_part_id = preferred.or(selected).or(fallback);
    }

    fn cue_current_playhead_intent(&self) -> PlaybackTransportIntent {
        match self.view_mode {
            ViewMode::Source if self.active_playback_source_ref().is_some() => {
                PlaybackTransportIntent::CueFrame(self.source_playhead_frame.max(0))
            }
            ViewMode::Wrap if self.playlist_input_available() => {
                PlaybackTransportIntent::CueFrame(self.wrap_playhead_frame.max(0))
            }
            _ => PlaybackTransportIntent::None,
        }
    }

    fn scrub_current_playhead_intent(&self) -> PlaybackTransportIntent {
        match self.view_mode {
            ViewMode::Source if self.active_playback_source_ref().is_some() => {
                PlaybackTransportIntent::ScrubFrame(self.source_playhead_frame.max(0))
            }
            ViewMode::Wrap if self.playlist_input_available() => {
                PlaybackTransportIntent::ScrubFrame(self.wrap_playhead_frame.max(0))
            }
            _ => PlaybackTransportIntent::None,
        }
    }

    fn apply_story_state(&mut self, state: &Value) {
        let update = story_state::parse_state(state, self.timeline.as_ref());
        let thumbnail_queue = story_state::thumbnail_queue_delta(
            &update.all_clips,
            |clip_id| self.thumb_textures.contains_key(clip_id),
            |clip_id| self.thumbs_queued.iter().any(|queued| queued == clip_id),
        );

        self.selected_part_id = update.selected_part_id;
        self.selected_shot_id = update.selected_shot_id;
        self.parts = update.parts;
        self.all_clips = update.all_clips;
        self.virtual_shots = update.virtual_shots;
        self.covers = update.covers;
        self.markers = update.markers;
        self.marker_slots = update.marker_slots;
        self.selected_cover_id = update.selected_cover_id;
        self.selected_slot_id = update.selected_slot_id;
        self.draft_status = update.draft_status;
        self.story_summary = update.story_summary;
        self.thumbs_queued.extend(thumbnail_queue);
    }

    fn refresh_story_summary(&mut self) {
        self.story_summary = story_state::summary(
            self.timeline.as_ref(),
            &self.parts,
            &self.all_clips,
            &self.virtual_shots,
            &self.covers,
            &self.markers,
        );
    }

    pub fn apply_editorial_edit_data(
        &mut self,
        data: EditorialEditData,
    ) -> PlaybackTransportIntent {
        if self.loaded_project_id != data.project_id {
            return PlaybackTransportIntent::None;
        }
        self.story_state_snapshot = Some(data.state.clone());
        self.apply_story_state(&data.state);
        self.pending_wrap_scrub_part_id = None;
        self.timeline_loaded = false;
        self.playlist_loaded = false;
        match data.kind {
            EditorialEditKind::SaveVirtualShot => {
                self.library_tab = LibraryTab::Virtual;
                self.status = "Virtual clip spremljen u Virtual tab".into();
                PlaybackTransportIntent::None
            }
            EditorialEditKind::CreatePartFromMarks => {
                self.status = format!("Dodan {}", data.detail);
                self.defer_wrap_scrub_after_timeline(None);
                PlaybackTransportIntent::None
            }
            EditorialEditKind::Commit => {
                self.status = "Commit OK — Export XML datoteka čeka host API (isto kao web)".into();
                PlaybackTransportIntent::None
            }
            EditorialEditKind::DeletePart => {
                self.status = format!("Segment obrisan {}", truncate(&data.detail, 24));
                self.defer_wrap_scrub_after_timeline(None);
                PlaybackTransportIntent::None
            }
            EditorialEditKind::ReorderPart => {
                self.selected_part_id = data.detail.clone();
                self.status = format!("Segment {}", truncate(&data.detail, 24));
                self.defer_wrap_scrub_after_timeline(Some(data.detail));
                PlaybackTransportIntent::None
            }
            EditorialEditKind::MarkPartIn => {
                self.focus.clear();
                self.status = "Mark IN spremljen".into();
                self.defer_wrap_scrub_after_timeline(Some(data.detail));
                PlaybackTransportIntent::None
            }
            EditorialEditKind::MarkPartOut => {
                self.focus.clear();
                self.status = "Mark OUT spremljen".into();
                self.defer_wrap_scrub_after_timeline(Some(data.detail));
                PlaybackTransportIntent::None
            }
            EditorialEditKind::CreateMarker => {
                self.status = format!("Marker @ {}", self.tc(self.virtual_sec()));
                PlaybackTransportIntent::None
            }
            EditorialEditKind::DeleteMarker => {
                self.status = format!("Marker obrisan {}", truncate(&data.detail, 24));
                PlaybackTransportIntent::None
            }
            EditorialEditKind::MoveMarker => {
                self.status = format!("Marker {}", truncate(&data.detail, 24));
                PlaybackTransportIntent::None
            }
            EditorialEditKind::SelectMarkerSlot => {
                self.selected_slot_id = data.detail.clone();
                self.status = format!("Slot {}", truncate(&data.detail, 24));
                PlaybackTransportIntent::None
            }
            EditorialEditKind::SelectCover => {
                self.selected_cover_id = data.detail.clone();
                self.status = format!("Cover {}", truncate(&data.detail, 24));
                PlaybackTransportIntent::None
            }
            EditorialEditKind::CreateCover => {
                self.status = "Cover kreiran".into();
                PlaybackTransportIntent::None
            }
            EditorialEditKind::DeleteCover => {
                self.status = format!("Cover obrisan {}", truncate(&data.detail, 24));
                PlaybackTransportIntent::None
            }
        }
    }

    pub fn set_editorial_edit_error(
        &mut self,
        project_id: &str,
        _kind: EditorialEditKind,
        error: impl Into<String>,
    ) {
        if self.loaded_project_id != project_id {
            return;
        }
        self.status = error.into();
    }

    fn select_shot_from_snapshot(&mut self, shot: &StoryShot) {
        let source_duration_frames = self
            .all_clips
            .iter()
            .find(|clip| clip.clip_id == shot.clip_id && clip.duration_frames > 0)
            .map(|clip| clip.duration_frames);
        let selection =
            match story_selection::shot_selection(&self.project_id, shot, source_duration_frames) {
                Ok(selection) => selection,
                Err(err) => {
                    self.status = err.message;
                    return;
                }
            };
        self.view_mode = ViewMode::Source;
        self.source_dock_keyboard_focus = true;
        self.selected_shot_id = selection.shot_id.clone();
        self.selected_clip_id = selection.clip_id.clone();
        self.source_in_frame = selection.shot_in_frame;
        self.source_out_frame = selection.shot_out_frame;
        self.selected_shot_in_frame = selection.shot_in_frame;
        self.selected_shot_out_frame = selection.shot_out_frame;
        self.mark_in_set = false;
        self.mark_out_set = false;
        self.focus.clear();
        self.selected_source_ref = Some(selection.source_ref);
        self.selected_source_fps = shot.fps;
        self.selected_source_has_audio = shot.has_audio;
        self.selected_source_audio_channels = shot.audio_channels;
        self.source_timebase_ready = shot.fps.is_finite() && shot.fps > 0.0;
        self.set_source_playhead_frame(selection.shot_in_frame);
        if self.waveform_clip_id != selection.clip_id {
            self.a1_peaks.clear();
            self.a2_peaks.clear();
            self.film_frames.clear();
            self.waveform_clip_id.clear();
            self.image_loader.cancel_pending();
        }

        let mut play_path = shot.play_path.trim().to_string();
        if play_path.is_empty() {
            play_path = self
                .all_clips
                .iter()
                .chain(self.virtual_shots.iter())
                .find(|candidate| candidate.clip_id == selection.clip_id)
                .map(|candidate| candidate.play_path.trim().to_string())
                .unwrap_or_default();
        }
        if play_path.is_empty() {
            self.selected_play_path.clear();
            self.status = format!("Proxy path nije spreman · {}", selection.clip_id);
        } else {
            self.selected_play_path = play_path;
            self.status = format!("Source · {} (snapshot path)", selection.clip_id);
        }
    }

    fn select_shot(&mut self, host: &HostClient, shot: &StoryShot) -> PlaybackTransportIntent {
        self.select_shot_from_snapshot(shot);
        if !self.selected_clip_id.trim().is_empty() {
            self.request_source_media(host);
            return self.cue_current_playhead_intent();
        }
        PlaybackTransportIntent::None
    }

    fn request_source_media(&mut self, host: &HostClient) {
        let clip = self.selected_clip_id.clone();
        let clip = clip.trim();
        if clip.is_empty() {
            self.a1_peaks.clear();
            self.a2_peaks.clear();
            self.film_frames.clear();
            self.waveform_clip_id.clear();
            return;
        }
        if clip == self.waveform_clip_id && !self.a1_peaks.is_empty() {
            return;
        }
        if self.source_media_loader.request(
            host,
            self.project_id.clone(),
            clip.to_string(),
            self.repaint_ctx.clone(),
        ) {
            self.source_media_retry_at = None;
            self.status = format!("Učitavam source · {clip}");
        }
    }

    fn poll_media_assets(&mut self, _host: &HostClient, ctx: &egui::Context) {
        for result in self.image_loader.poll() {
            match result.key.scope.as_str() {
                "editorial.thumb" => {
                    let clip_id = result.key.item_id;
                    if let Ok(color) = result.image {
                        let tex = ctx.load_texture(
                            format!("thumb_{clip_id}"),
                            color,
                            TextureOptions::LINEAR,
                        );
                        self.thumb_textures.insert(clip_id, tex);
                    }
                }
                "editorial.film" => {
                    let clip_id = result.key.item_id;
                    if clip_id != self.selected_clip_id {
                        continue;
                    }
                    let index = result.key.variant.parse::<i64>().unwrap_or(0);
                    if let Ok(color) = result.image {
                        if let Some(frame) = self
                            .film_frames
                            .iter_mut()
                            .find(|frame| frame.index == index)
                        {
                            frame.texture = Some(ctx.load_texture(
                                format!("qnc_tl_frame_{clip_id}_{index}"),
                                color,
                                TextureOptions::LINEAR,
                            ));
                        }
                    }
                }
                _ => {}
            }
        }

        for result in self.source_media_loader.poll() {
            if result.clip_id != self.selected_clip_id {
                continue;
            }
            match result.media {
                Ok(media) => {
                    self.waveform_clip_id = media.clip_id;
                    self.a1_peaks = media.a1_peaks;
                    self.a2_peaks = media.a2_peaks;
                    let frames = media.film_frames.into_iter().map(|frame| FilmFrame {
                        index: frame.index,
                        seek_sec: frame.seek_sec,
                        url: frame.url,
                        texture: None,
                    });
                    crate::qnc_filmstrip_background::merge_frames(&mut self.film_frames, frames);
                    if self.a1_peaks.is_empty() && self.a2_peaks.is_empty() {
                        self.source_media_retry_at = Some(Instant::now() + Duration::from_secs(1));
                        self.status = format!("Waveform se gradi · {}", self.selected_clip_id);
                    } else {
                        self.source_media_retry_at = None;
                        self.status = format!("Source spreman · {}", self.selected_clip_id);
                    }
                    ctx.request_repaint();
                }
                Err(e) => self.status = e,
            }
        }
    }

    fn pump_film_frames(&mut self, ctx: &egui::Context) {
        for frame in &mut self.film_frames {
            if frame.texture.is_some() || frame.url.is_empty() {
                continue;
            }
            let _ = self.image_loader.request(
                ImageAssetKey::new(
                    "editorial.film",
                    self.selected_clip_id.clone(),
                    frame.index.to_string(),
                ),
                frame.url.clone(),
                Some(ctx.clone()),
            );
            break; // one per tick
        }
    }

    fn pump_thumbs(&mut self, host: &HostClient, ctx: &egui::Context) {
        let Some(clip_id) = self.thumbs_queued.pop() else {
            return;
        };
        if self.thumb_textures.contains_key(&clip_id) {
            return;
        }
        let url = media_assets::story_thumbnail_url(host, &self.project_id, &clip_id, 0.0);
        let _ = self.image_loader.request(
            ImageAssetKey::new("editorial.thumb", clip_id, "poster"),
            url,
            Some(ctx.clone()),
        );
    }

    pub fn tick(&mut self, host: &HostClient, ctx: &egui::Context) {
        self.repaint_ctx = Some(ctx.clone());
        if self.project_id.is_empty() {
            return;
        }
        self.pump_thumbs(host, ctx);
        self.pump_film_frames(ctx);
        let _ = host;
        if self
            .film_frames
            .iter()
            .any(|f| f.texture.is_none() && !f.url.is_empty())
        {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        if self.playing {
            ctx.request_repaint();
        }
    }

    pub fn handle_shortcuts(
        &mut self,
        ctx: &egui::Context,
        host: &HostClient,
        playback: &crate::playback_stack::PlaybackStack,
    ) -> Vec<PlaybackTransportIntent> {
        if !self.shortcuts_ready() {
            return Vec::new();
        }
        let mut intents = Vec::new();
        // Keys come only from seed/DB keyboard-shortcuts (storyboard scope).
        for action in playback_controls::shortcut_actions(ctx, &self.bindings) {
            let intent = match action {
                playback_controls::PlaybackAction::SeekFrames(frames) => {
                    self.step_focus(host, playback, frames)
                }
                other => self.dispatch_playback_action(host, other),
            };
            if intent != PlaybackTransportIntent::None {
                intents.push(intent);
            }
        }
        intents
    }

    #[allow(dead_code)]
    pub fn playback_is_playing(&self) -> bool {
        self.playing
    }

    pub fn playback_source_ref(&self) -> Option<&BroadcastHostSourceRef> {
        self.active_playback_source_ref()
    }

    pub(crate) fn uses_playlist_input_transport(&self) -> bool {
        self.view_mode == ViewMode::Wrap
    }

    pub(crate) fn playback_transport_available(&self) -> bool {
        match self.view_mode {
            ViewMode::Source => self.active_playback_source_ref().is_some(),
            ViewMode::Wrap => self.playlist_input_available(),
        }
    }

    pub(crate) fn playback_transport_toggle_intent(
        &mut self,
        playlist_input_active: bool,
        playlist_input_playing: bool,
    ) -> PlaybackTransportIntent {
        self.toggle_play_intent_for_input(playlist_input_active, playlist_input_playing)
    }

    #[allow(dead_code)]
    pub fn playback_source_fps(&self) -> f64 {
        <Self as crate::player_bridge::PlayerClient>::playback_source_fps(self)
    }

    #[allow(dead_code)]
    pub fn playback_source_has_audio(&self) -> bool {
        <Self as crate::player_bridge::PlayerClient>::playback_source_has_audio(self)
    }

    #[allow(dead_code)]
    pub fn playback_source_audio_channels(&self) -> u8 {
        <Self as crate::player_bridge::PlayerClient>::playback_source_audio_channels(self)
    }

    #[allow(dead_code)]
    pub fn playback_source_sec(&self) -> f64 {
        // Player clock only — timelines never map to each other.
        self.virtual_sec().max(0.0)
    }

    /// Project broadcast-player clock onto the active UI playhead.
    /// Timelines do not talk to each other; they only mirror the player.
    fn sync_playhead_from_player_frame(&mut self, source_frame: FrameNumber) {
        let frame = source_frame.0.max(0);
        match self.view_mode {
            ViewMode::Source => self.set_source_playhead_frame(frame),
            ViewMode::Wrap => {
                if let Some(segment) = self.playlist_segment_at_frame(frame) {
                    self.selected_part_id = segment.part_id.clone();
                }
                self.set_wrap_playhead_frame(frame);
            }
        }
    }

    /// Local disk path for native ffmpeg (never an HTTP virtual-stream URL).
    #[allow(dead_code)]
    pub fn playback_media_path(&self) -> Option<String> {
        <Self as crate::player_bridge::PlayerClient>::playback_media_path(self)
    }

    #[allow(dead_code)]
    pub fn set_player_preview_active(&mut self, active: bool) {
        <Self as crate::player_bridge::PlayerClient>::set_player_preview_active(self, active)
    }

    #[allow(dead_code)]
    pub fn apply_player_frame(
        &mut self,
        image: ColorImage,
        source_frame: FrameNumber,
        playing: bool,
    ) {
        <Self as crate::player_bridge::PlayerClient>::apply_player_frame(
            self,
            image,
            source_frame,
            playing,
        )
    }

    #[allow(dead_code)]
    pub fn apply_player_state(
        &mut self,
        source_frame: FrameNumber,
        playing: bool,
        status: impl Into<String>,
    ) {
        <Self as crate::player_bridge::PlayerClient>::apply_player_state(
            self,
            source_frame,
            playing,
            status,
        )
    }

    pub fn apply_player_error(&mut self, status: impl Into<String>) {
        <Self as crate::player_bridge::PlayerClient>::apply_player_error(self, status)
    }

    #[allow(dead_code)]
    pub fn apply_playback_command_state(&mut self, playing: bool, status: impl Into<String>) {
        <Self as crate::player_bridge::PlayerClient>::apply_playback_command_state(
            self, playing, status,
        )
    }

    pub fn prepare_frame(&mut self, host: &HostClient, ctx: &egui::Context) {
        self.repaint_ctx = Some(ctx.clone());
        self.poll_media_assets(host, ctx);
        if let Some(retry_at) = self.source_media_retry_at {
            let now = Instant::now();
            if now >= retry_at {
                self.source_media_retry_at = None;
                self.request_source_media(host);
            } else {
                ctx.request_repaint_after(retry_at - now);
            }
        }
        if self.waveform_clip_id != self.selected_clip_id {
            self.request_source_media(host);
        }
    }

    /// Docked bottom bar — web `story-source-editor-col` + `qnc-timeline` source.
    pub fn source_dock_height(&self) -> f32 {
        source_editor::dock_height(self.expanded_audio)
    }

    pub fn ui_source_dock(
        &mut self,
        ui: &mut egui::Ui,
        host: &HostClient,
        playback: &PlaybackStack,
    ) -> PlaybackTransportIntent {
        let dock_rect = ui.max_rect();
        let dock_hot = ui.input(|i| {
            i.pointer
                .hover_pos()
                .is_some_and(|pos| dock_rect.contains(pos))
        });
        let action = self.ui_source_editor(ui, host, self.source_dock_height(), playback);
        if dock_hot || !matches!(action, source_editor::SourceEditorAction::None) {
            self.source_dock_keyboard_focus = true;
        }
        match action {
            source_editor::SourceEditorAction::None => PlaybackTransportIntent::None,
            source_editor::SourceEditorAction::CueFrame(frame) => {
                if self.view_mode != ViewMode::Source {
                    self.view_mode = ViewMode::Source;
                }
                self.set_source_playhead_frame(frame);
                PlaybackTransportIntent::CueFrame(frame)
            }
            source_editor::SourceEditorAction::SaveVirtualShot => {
                self.save_virtual_shot(host);
                PlaybackTransportIntent::None
            }
            source_editor::SourceEditorAction::CreatePart(kind) => self.create_part(host, kind),
            source_editor::SourceEditorAction::CreateCover => self.quick_cover(host),
            source_editor::SourceEditorAction::ToggleAudioExpand(lane) => {
                self.expanded_audio = self.expanded_audio.toggle(lane);
                PlaybackTransportIntent::None
            }
        }
    }

    /// Central workspace — composed from `qnc_ui` (Story is the reference form).
    pub fn ui_main(
        &mut self,
        ui: &mut egui::Ui,
        host: &HostClient,
        _ctx: &egui::Context,
        playback: &PlaybackStack,
    ) -> Vec<PlaybackTransportIntent> {
        let mut intents = Vec::new();
        let monitor_empty_label = if self.view_mode == ViewMode::Wrap {
            "Playlist input"
        } else {
            "Odaberi klip"
        };
        crate::qnc_ui::editorial_shell(ui, |ui, m, side| match side {
            crate::qnc_ui::ShellSide::Left => {
                crate::qnc_ui::media_column_monitor(
                    ui,
                    m,
                    |ui, preview_h| {
                        playback.show_monitor(ui, preview_h, monitor_empty_label);
                    },
                    |ui, _rest| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.allocate_ui(Vec2::new(m.left_w, crate::qnc_ui::space::CHROME_H), |ui| {
                            let intent = self.ui_pool_head(ui, host);
                            if intent != PlaybackTransportIntent::None {
                                intents.push(intent);
                            }
                        });
                        let body = ui.available_height().max(0.0);
                        let intent = self.ui_filmstrip_web(ui, host, body);
                        if intent != PlaybackTransportIntent::None {
                            intents.push(intent);
                        }
                    },
                );
            }
            crate::qnc_ui::ShellSide::Right => match self.role.composition().right {
                crate::composition::RightPanelKind::SegmentPanel => {
                    let intent = self.ui_segmenti_panel(ui, host, playback, m.height);
                    if intent != PlaybackTransportIntent::None {
                        intents.push(intent);
                    }
                }
                crate::composition::RightPanelKind::None
                | crate::composition::RightPanelKind::ClipGrid
                | crate::composition::RightPanelKind::TemplateSettings => {}
            },
        });
        intents
    }

    fn selected_clip_label(&self) -> String {
        self.all_clips
            .iter()
            .chain(self.virtual_shots.iter())
            .find(|c| shot_id(c) == self.selected_shot_id || c.clip_id == self.selected_clip_id)
            .map(|c| {
                if !c.name.is_empty() {
                    c.name.clone()
                } else if !c.virtual_name.is_empty() {
                    c.virtual_name.clone()
                } else {
                    c.clip_id.clone()
                }
            })
            .unwrap_or_else(|| "Odaberi klip".into())
    }

    fn dispatch_media_pool(
        &mut self,
        host: &HostClient,
        action: media_pool::MediaPoolAction,
    ) -> PlaybackTransportIntent {
        match action {
            media_pool::MediaPoolAction::None => PlaybackTransportIntent::None,
            media_pool::MediaPoolAction::SwitchTab(tab) => {
                // Honor composition: no Segment tab → clamp away.
                let tab = if tab == LibraryTab::Segment && !self.role.head().show_segment_tab {
                    LibraryTab::All
                } else {
                    tab
                };
                self.library_tab = tab;
                if tab == LibraryTab::Segment {
                    self.start_wrap_session(host)
                } else {
                    PlaybackTransportIntent::None
                }
            }
            media_pool::MediaPoolAction::SelectShot(shot) => self.select_shot(host, &shot),
            media_pool::MediaPoolAction::SelectPart(part_id) => {
                self.selected_part_id = part_id.clone();
                self.start_wrap_session_for_part(host, Some(part_id))
            }
            media_pool::MediaPoolAction::DeletePart(part_id) => {
                self.delete_part(host, &part_id);
                PlaybackTransportIntent::None
            }
            media_pool::MediaPoolAction::ReorderPart { part_id, direction } => {
                self.reorder_part(host, &part_id, &direction);
                PlaybackTransportIntent::None
            }
            media_pool::MediaPoolAction::TogglePlay => PlaybackTransportIntent::TogglePlay,
            media_pool::MediaPoolAction::MarkIn => {
                self.dispatch_playback_action(host, playback_controls::PlaybackAction::MarkIn)
            }
            media_pool::MediaPoolAction::MarkOut => {
                self.dispatch_playback_action(host, playback_controls::PlaybackAction::MarkOut)
            }
            media_pool::MediaPoolAction::QuickCover => {
                self.dispatch_playback_action(host, playback_controls::PlaybackAction::QuickCover)
            }
            media_pool::MediaPoolAction::ExportCommit => {
                self.export_commit(host);
                PlaybackTransportIntent::None
            }
            media_pool::MediaPoolAction::SelectClipId(_) => PlaybackTransportIntent::None,
        }
    }

    fn ui_pool_head(&mut self, ui: &mut egui::Ui, host: &HostClient) -> PlaybackTransportIntent {
        let action = media_pool::show_head(
            ui,
            self.role
                .head()
                .to_pool_head(self.library_tab, self.playing),
        );
        self.dispatch_media_pool(host, action)
    }

    fn ui_filmstrip_web(
        &mut self,
        ui: &mut egui::Ui,
        host: &HostClient,
        height: f32,
    ) -> PlaybackTransportIntent {
        let timeline_fps = self.timeline_fps();
        let tc = move |sec| {
            timeline_fps
                .map(|fps| seconds_to_timecode(sec, fps))
                .unwrap_or_else(|| format!("{sec:.2}s"))
        };
        let action = media_pool::show_strip(
            ui,
            media_pool::MediaPoolStripInput {
                library_tab: self.library_tab,
                height,
                selected_shot_id: &self.selected_shot_id,
                selected_clip_id: &self.selected_clip_id,
                all_clips: &self.all_clips,
                virtual_shots: &self.virtual_shots,
                parts: &self.parts,
                selected_part_id: &self.selected_part_id,
                thumb_textures: &self.thumb_textures,
                tc: &tc,
                card_features: self.role.card_features(),
            },
        );
        self.dispatch_media_pool(host, action)
    }

    fn ui_segmenti_panel(
        &mut self,
        ui: &mut egui::Ui,
        host: &HostClient,
        playback: &PlaybackStack,
        height: f32,
    ) -> PlaybackTransportIntent {
        let program = self.segment_program_model();
        let display_frame =
            playback.playlist_display_frame(self.wrap_playhead_frame, program.duration_frames());
        let tc = |sec| self.tc(sec);
        let tc_frame = |frame| {
            self.timeline_sec_from_frame(frame)
                .map(|sec| self.tc(sec))
                .unwrap_or_else(|| "--:--:--:--".into())
        };
        let playhead_sec = self.timeline_sec_from_frame(display_frame).unwrap_or(0.0);
        let action = segment_panel::show(
            ui,
            segment_panel::SegmentPanelInput {
                height,
                virtual_frame: display_frame,
                playhead_sec,
                program: &program,
                marker_slots: &self.marker_slots,
                covers: &self.covers,
                markers: &self.markers,
                selected_slot_id: &self.selected_slot_id,
                selected_cover_id: &self.selected_cover_id,
                tc: &tc,
                tc_frame: &tc_frame,
            },
        );
        self.dispatch_segment_panel(host, action)
    }

    fn dispatch_marker_cover_action(
        &mut self,
        host: &HostClient,
        action: marker_cover_panel::MarkerCoverAction,
    ) -> PlaybackTransportIntent {
        match action {
            marker_cover_panel::MarkerCoverAction::None => PlaybackTransportIntent::None,
            marker_cover_panel::MarkerCoverAction::AddMarker => {
                self.marker_at_head(host);
                PlaybackTransportIntent::None
            }
            marker_cover_panel::MarkerCoverAction::CreateCover => self.quick_cover(host),
            marker_cover_panel::MarkerCoverAction::OverwriteCover => self.overwrite_cover(host),
            marker_cover_panel::MarkerCoverAction::SelectSlot(id) => {
                self.select_marker_slot(host, &id);
                PlaybackTransportIntent::None
            }
            marker_cover_panel::MarkerCoverAction::SelectCover(id) => {
                self.select_cover(host, &id);
                PlaybackTransportIntent::None
            }
            marker_cover_panel::MarkerCoverAction::DeleteCover(id) => {
                self.delete_cover(host, &id);
                PlaybackTransportIntent::None
            }
            marker_cover_panel::MarkerCoverAction::SeekMarkerFrame(frame) => {
                self.selected_marker_id.clear();
                self.set_wrap_playhead_frame(frame);
                self.ensure_wrap_or_scrub(host)
            }
            marker_cover_panel::MarkerCoverAction::MoveMarker {
                marker_id,
                direction,
            } => {
                self.move_marker(host, &marker_id, &direction);
                PlaybackTransportIntent::None
            }
            marker_cover_panel::MarkerCoverAction::DeleteMarker(id) => {
                self.delete_marker(host, &id);
                PlaybackTransportIntent::None
            }
        }
    }

    fn dispatch_segment_panel(
        &mut self,
        host: &HostClient,
        action: segment_panel::SegmentPanelAction,
    ) -> PlaybackTransportIntent {
        match action {
            segment_panel::SegmentPanelAction::None => PlaybackTransportIntent::None,
            segment_panel::SegmentPanelAction::SeekTimelineFrame(frame) => {
                self.set_wrap_playhead_frame(frame);
                self.ensure_wrap_or_scrub(host)
            }
            segment_panel::SegmentPanelAction::MarkerCover(action) => {
                self.dispatch_marker_cover_action(host, action)
            }
            segment_panel::SegmentPanelAction::SelectMarkerSlot(id) => {
                self.select_marker_slot(host, &id);
                PlaybackTransportIntent::None
            }
            segment_panel::SegmentPanelAction::SelectCover(id) => {
                self.select_cover(host, &id);
                PlaybackTransportIntent::None
            }
            segment_panel::SegmentPanelAction::SelectMarker { marker_id, frame } => {
                self.select_marker(host, &marker_id, frame)
            }
        }
    }

    fn ensure_wrap_or_scrub(&mut self, host: &HostClient) -> PlaybackTransportIntent {
        if self.view_mode != ViewMode::Wrap {
            self.start_wrap_session(host)
        } else {
            self.scrub_soft(host)
        }
    }

    fn story_playback_view(&self) -> StoryPlaybackView {
        match self.view_mode {
            ViewMode::Source => StoryPlaybackView::Source,
            ViewMode::Wrap => StoryPlaybackView::Wrap,
        }
    }

    fn apply_story_toggle_play_outcome(
        &mut self,
        outcome: StoryTogglePlayOutcome,
    ) -> PlaybackTransportIntent {
        if let Some(view_mode) = outcome.view_mode {
            self.view_mode = match view_mode {
                StoryPlaybackView::Source => ViewMode::Source,
                StoryPlaybackView::Wrap => ViewMode::Wrap,
            };
        }
        if let Some(playing) = outcome.playing {
            self.playing = playing;
        }
        if let Some(status) = outcome.status {
            self.status = status;
        }
        if let Some(part_id) = outcome.selected_part_id {
            self.selected_part_id = part_id;
        }
        outcome.intent
    }

    fn toggle_play_intent_for_input(
        &mut self,
        playlist_input_active: bool,
        playlist_input_playing: bool,
    ) -> PlaybackTransportIntent {
        let program = self.segment_program_model();
        let outcome = playback_transport::toggle_play(StoryTogglePlayInput {
            source_dock_keyboard_focus: self.source_dock_keyboard_focus,
            view_mode: self.story_playback_view(),
            story_playing: self.playing,
            playlist_input_active,
            playlist_input_playing,
            project_id: &self.project_id,
            program_id: self.edit_instance_id(),
            start_program_frame: self.wrap_playhead_frame,
            program: &program,
            covers: &self.covers,
            all_clips: &self.all_clips,
            virtual_shots: &self.virtual_shots,
        });
        self.apply_story_toggle_play_outcome(outcome)
    }

    fn ui_source_editor(
        &mut self,
        ui: &mut egui::Ui,
        host: &HostClient,
        _height: f32,
        playback: &PlaybackStack,
    ) -> source_editor::SourceEditorAction {
        let _ = host;
        let clip_label = self.selected_clip_label();
        let Some(timebase_fps) = self.source_timebase_fps() else {
            self.status = "Source FPS još nije potvrđen — source timeline nije spreman".into();
            return source_editor::SourceEditorAction::None;
        };
        let clip = self
            .all_clips
            .iter()
            .find(|c| c.clip_id == self.selected_clip_id);
        let clip_duration_frames = clip
            .and_then(|c| (c.duration_frames > 0).then_some(c.duration_frames))
            .unwrap_or_else(|| {
                let duration_sec = clip.map(|c| c.duration_sec).unwrap_or(0.0).max(0.0);
                seconds_to_frame(duration_sec, timebase_fps)
            })
            .max(self.source_out_frame.max(1));
        if self.source_dock_uses_live_carrier(playback) {
            let frame = playback.carrier().display_frame().0;
            self.sync_playhead_from_player_frame(FrameNumber(frame));
        }
        let tc_frame = |frame: i64| {
            seconds_to_timecode(frame_to_seconds(frame.max(0), timebase_fps), timebase_fps)
        };
        let focus_paint = match self.focus.target {
            FocusTarget::Playhead => TimelineFocusPaint::Playhead,
            FocusTarget::In => TimelineFocusPaint::In,
            FocusTarget::Out => TimelineFocusPaint::Out,
        };
        let shot_in_frame = self.selected_shot_in_frame.clamp(0, clip_duration_frames);
        let shot_out_frame = self
            .selected_shot_out_frame
            .max(shot_in_frame + 1)
            .clamp(0, clip_duration_frames);
        let draft_in_frame = self.source_in_frame.max(0);
        let draft_out_frame = self.source_out_frame.max(self.source_in_frame + 1);
        let timeline_model = self.source_dock_timeline_model(
            playback,
            timebase_fps,
            clip_duration_frames,
            shot_in_frame,
            shot_out_frame,
            draft_in_frame,
            draft_out_frame,
        );
        source_editor::show(
            ui,
            source_editor::SourceEditorInput {
                clip_label: &clip_label,
                source_in_frame: self.source_in_frame,
                source_out_frame: self.source_out_frame,
                timeline_model,
                focus: focus_paint,
                a1_peaks: &self.a1_peaks,
                a2_peaks: &self.a2_peaks,
                frames: &self.film_frames,
                tc_frame: &tc_frame,
                expanded_audio: self.expanded_audio,
            },
        )
    }
}

impl StoryScreen {
    fn dispatch_playback_action(
        &mut self,
        host: &HostClient,
        action: playback_controls::PlaybackAction,
    ) -> PlaybackTransportIntent {
        match action {
            playback_controls::PlaybackAction::TogglePlay => PlaybackTransportIntent::TogglePlay,
            playback_controls::PlaybackAction::MarkIn => self.mark_in_action(host),
            playback_controls::PlaybackAction::MarkOut => self.mark_out_action(host),
            playback_controls::PlaybackAction::SelectMarkIn => self.select_mark_in(host),
            playback_controls::PlaybackAction::SelectMarkOut => self.select_mark_out(host),
            playback_controls::PlaybackAction::FocusNext => {
                let chain = self.edit_focus_chain();
                self.focus.focus_next(&chain);
                self.after_focus_changed()
            }
            playback_controls::PlaybackAction::FocusPrev => {
                let chain = self.edit_focus_chain();
                self.focus.focus_prev(&chain);
                self.after_focus_changed()
            }
            playback_controls::PlaybackAction::ClearFocus => {
                if !self.focus.is_playhead() {
                    self.focus.clear();
                    self.status = "Fokus → playhead".into();
                }
                PlaybackTransportIntent::None
            }
            playback_controls::PlaybackAction::QuickCover => self.quick_cover(host),
            playback_controls::PlaybackAction::OverwriteCover => self.overwrite_cover(host),
            playback_controls::PlaybackAction::StepPrevPart => self.select_adjacent_part(host, -1),
            playback_controls::PlaybackAction::StepNextPart => self.select_adjacent_part(host, 1),
            playback_controls::PlaybackAction::StepPrevMarkerSlot => {
                self.select_adjacent_marker_slot(host, -1)
            }
            playback_controls::PlaybackAction::StepNextMarkerSlot => {
                self.select_adjacent_marker_slot(host, 1)
            }
            playback_controls::PlaybackAction::FocusEmptySlot => self.focus_empty_marker_slot(host),
            playback_controls::PlaybackAction::MarkInFitDuration => self.mark_in_fit_duration(host),
            playback_controls::PlaybackAction::DeleteSelection => {
                self.delete_selected_timeline_item(host);
                PlaybackTransportIntent::None
            }
            playback_controls::PlaybackAction::AddMarker => {
                self.marker_at_head(host);
                PlaybackTransportIntent::None
            }
            playback_controls::PlaybackAction::AddTonSegment => self.create_part(host, "tonovi"),
            playback_controls::PlaybackAction::AddOffSegment => self.create_part(host, "offovi"),
            // SeekFrames is handled in handle_shortcuts.
            playback_controls::PlaybackAction::SeekFrames(_) => PlaybackTransportIntent::None,
        }
    }

    fn edit_focus_chain(&self) -> Vec<FocusTarget> {
        let mut chain = vec![FocusTarget::Playhead];
        if self.mark_in_set {
            chain.push(FocusTarget::In);
        }
        if self.mark_out_set {
            chain.push(FocusTarget::Out);
        }
        chain
    }

    fn select_mark_in(&mut self, host: &HostClient) -> PlaybackTransportIntent {
        let _ = host;
        if self.view_mode != ViewMode::Source {
            self.status = format!(
                "{}: source mode",
                self.chord_or("select_mark_in", "select_mark_in")
            );
            return PlaybackTransportIntent::None;
        }
        if !self.mark_in_set {
            self.status = format!("Prvo stavi IN ({})", self.chord_or("mark_in", "mark_in"));
            return PlaybackTransportIntent::None;
        }
        self.focus.select_in();
        self.set_source_playhead_frame(self.source_in_frame);
        self.status = format!(
            "Fokus IN · {} · {} pomak 1 frame · {} playhead",
            self.source_tc_frame(self.source_in_frame),
            self.chord_or("step_forward_frame", "←/→"),
            self.chord_or("clear_focus", "Esc")
        );
        self.cue_current_playhead_intent()
    }

    fn after_focus_changed(&mut self) -> PlaybackTransportIntent {
        match self.focus.target {
            FocusTarget::In => {
                if !self.mark_in_set {
                    self.focus.clear();
                    self.status = format!(
                        "IN još nije označen ({})",
                        self.chord_or("mark_in", "mark_in")
                    );
                    return PlaybackTransportIntent::None;
                }
                self.set_source_playhead_frame(self.source_in_frame);
                self.status = format!(
                    "Fokus IN · {} · {} 1f",
                    self.source_tc_frame(self.source_in_frame),
                    self.chord_or("step_forward_frame", "step")
                );
                self.cue_current_playhead_intent()
            }
            FocusTarget::Out => {
                if !self.mark_out_set {
                    self.focus.clear();
                    self.status = format!(
                        "OUT još nije označen ({})",
                        self.chord_or("mark_out", "mark_out")
                    );
                    return PlaybackTransportIntent::None;
                }
                self.set_source_playhead_frame(self.source_out_frame);
                self.status = format!(
                    "Fokus OUT · {} · {} 1f",
                    self.source_tc_frame(self.source_out_frame),
                    self.chord_or("step_forward_frame", "step")
                );
                self.cue_current_playhead_intent()
            }
            FocusTarget::Playhead => {
                self.status = "Fokus → playhead".into();
                PlaybackTransportIntent::None
            }
        }
    }

    fn select_mark_out(&mut self, host: &HostClient) -> PlaybackTransportIntent {
        let _ = host;
        if self.view_mode != ViewMode::Source {
            self.status = format!(
                "{}: source mode",
                self.chord_or("select_mark_out", "select_mark_out")
            );
            return PlaybackTransportIntent::None;
        }
        if !self.mark_out_set {
            self.status = format!("Prvo stavi OUT ({})", self.chord_or("mark_out", "mark_out"));
            return PlaybackTransportIntent::None;
        }
        self.focus.select_out();
        self.set_source_playhead_frame(self.source_out_frame);
        self.status = format!(
            "Fokus OUT · {} · {} pomak 1 frame · {} playhead",
            self.source_tc_frame(self.source_out_frame),
            self.chord_or("step_forward_frame", "←/→"),
            self.chord_or("clear_focus", "Esc")
        );
        self.cue_current_playhead_intent()
    }

    fn chord_or(&self, action_id: &str, fallback: &str) -> String {
        let hint = self.bindings.chord_hint(action_id);
        if hint.is_empty() {
            fallback.to_string()
        } else {
            hint
        }
    }

    fn selected_clip_duration_frames(&self) -> i64 {
        self.all_clips
            .iter()
            .find(|c| c.clip_id == self.selected_clip_id)
            .map(|c| c.duration_frames)
            .unwrap_or(0)
            .max(self.selected_shot_out_frame)
            .max(self.source_out_frame)
            .max(self.source_playhead_frame + 1)
            .max(1)
    }

    fn mark_in_action(&mut self, _host: &HostClient) -> PlaybackTransportIntent {
        match self.view_mode {
            ViewMode::Source => {
                if self.source_timebase_fps().is_none() {
                    self.status = "Source FPS još nije potvrđen — IN nije upisan".into();
                    return PlaybackTransportIntent::None;
                }
                self.source_in_frame = self.source_playhead_frame.max(0);
                // Do not collapse OUT to IN+1s — that traps playhead/player in a 1s window.
                if !self.mark_out_set || self.source_out_frame <= self.source_in_frame {
                    let clip_end = self.selected_clip_duration_frames();
                    self.source_out_frame = clip_end.max(self.source_in_frame + 1);
                }
                self.mark_in_set = true;
                // Stay on playhead — select_mark_in later for frame edit focus.
                self.focus.clear();
                self.status = format!(
                    "IN {} · {} za fokus / korekcija",
                    self.source_tc_frame(self.source_in_frame),
                    self.chord_or("select_mark_in", "select_mark_in")
                );
                PlaybackTransportIntent::None
            }
            ViewMode::Wrap => {
                if let Some(local_frame) = story_timeline::local_frame_in_part(
                    self.timeline.as_ref(),
                    &self.selected_part_id,
                    self.wrap_playhead_frame,
                ) {
                    let part = self.selected_part_id.clone();
                    self.enqueue_edit_command(|instance, request, project| {
                        EditorialEditComponent::mark_part_in(
                            instance,
                            request,
                            project,
                            &part,
                            local_frame,
                        )
                    });
                    self.status = format!(
                        "Spremam Mark IN @ {}",
                        self.part_local_tc(&part, local_frame)
                    );
                    PlaybackTransportIntent::None
                } else {
                    self.status = "Odaberi segment za Mark IN".into();
                    PlaybackTransportIntent::None
                }
            }
        }
    }

    fn mark_out_action(&mut self, _host: &HostClient) -> PlaybackTransportIntent {
        match self.view_mode {
            ViewMode::Source => {
                if self.source_timebase_fps().is_none() {
                    self.status = "Source FPS još nije potvrđen — OUT nije upisan".into();
                    return PlaybackTransportIntent::None;
                }
                self.source_out_frame = self.source_playhead_frame.max(self.source_in_frame + 1);
                self.mark_out_set = true;
                self.focus.clear();
                self.status = format!(
                    "OUT {} · {} za fokus / korekcija",
                    self.source_tc_frame(self.source_out_frame),
                    self.chord_or("select_mark_out", "select_mark_out")
                );
                PlaybackTransportIntent::None
            }
            ViewMode::Wrap => {
                if let Some(local_frame) = story_timeline::local_frame_in_part(
                    self.timeline.as_ref(),
                    &self.selected_part_id,
                    self.wrap_playhead_frame,
                ) {
                    let part = self.selected_part_id.clone();
                    self.enqueue_edit_command(|instance, request, project| {
                        EditorialEditComponent::mark_part_out(
                            instance,
                            request,
                            project,
                            &part,
                            local_frame,
                        )
                    });
                    self.status = format!(
                        "Spremam Mark OUT @ {}",
                        self.part_local_tc(&part, local_frame)
                    );
                    PlaybackTransportIntent::None
                } else {
                    self.status = "Odaberi segment za Mark OUT".into();
                    PlaybackTransportIntent::None
                }
            }
        }
    }

    fn save_virtual_shot(&mut self, _host: &HostClient) {
        if self.view_mode == ViewMode::Source && self.source_timebase_fps().is_none() {
            self.status = "Source FPS još nije potvrđen — virtualni kadar nije spremljen".into();
            return;
        }
        let clip = self.selected_clip_id.clone();
        if clip.trim().is_empty() {
            self.status = "Odaberi klip u All".into();
            return;
        }
        if self.source_out_frame <= self.source_in_frame {
            self.status = "OUT mora biti nakon IN".into();
            return;
        }
        let in_frame = self.source_in_frame;
        let out_frame = self.source_out_frame;
        self.enqueue_edit_command(|instance, request, project| {
            EditorialEditComponent::save_virtual_shot(
                instance, request, project, &clip, in_frame, out_frame,
            )
        });
        self.status = "Spremam virtual clip...".into();
    }

    fn create_part(&mut self, _host: &HostClient, kind: &str) -> PlaybackTransportIntent {
        // TON/OFF from Source IN/OUT creates a virtual segment in story_parts.
        // Add virtual clip is a separate action that writes virtual_shots.
        match self.source_range_for_segment() {
            Ok((clip_id, in_frame, out_frame)) => {
                let kind = kind.to_string();
                self.enqueue_edit_command(|instance, request, project| {
                    EditorialEditComponent::create_part_from_marks(
                        instance, request, project, &kind, &clip_id, in_frame, out_frame,
                    )
                });
                self.status = format!("Spremam {kind} segment...");
                PlaybackTransportIntent::None
            }
            Err(e) => {
                self.status = e;
                PlaybackTransportIntent::None
            }
        }
    }

    /// Mark range from Source dock — copied into `story_parts` at create time only.
    fn source_range_for_segment(&self) -> Result<(String, i64, i64), String> {
        let clip_id = self.selected_clip_id.trim();
        if clip_id.is_empty() {
            return Err("Odaberi source klip".into());
        }
        let _ = self
            .source_timebase_fps()
            .ok_or_else(|| "Source FPS još nije potvrđen".to_string())?;
        if !self.mark_in_set || !self.mark_out_set {
            return Err("Označi IN i OUT na source klipu".into());
        }
        let in_frame = self.source_in_frame.max(0);
        let out_frame = self.source_out_frame.max(self.source_in_frame + 1);
        if out_frame <= in_frame {
            return Err("OUT mora biti poslije IN".into());
        }
        Ok((clip_id.to_string(), in_frame, out_frame))
    }

    fn export_commit(&mut self, _host: &HostClient) {
        self.enqueue_edit_command(EditorialEditComponent::commit);
        self.status = "Commit u tijeku...".into();
    }

    fn delete_part(&mut self, _host: &HostClient, part_id: &str) {
        if part_id.trim().is_empty() {
            return;
        }
        let part_id = part_id.to_string();
        self.enqueue_edit_command(|instance, request, project| {
            EditorialEditComponent::delete_part(instance, request, project, &part_id)
        });
        self.status = format!("Brišem segment {}", truncate(&part_id, 24));
    }

    fn reorder_part(&mut self, _host: &HostClient, part_id: &str, direction: &str) {
        if part_id.trim().is_empty() {
            return;
        }
        let part_id = part_id.to_string();
        let direction = direction.to_string();
        self.enqueue_edit_command(|instance, request, project| {
            EditorialEditComponent::reorder_part(instance, request, project, &part_id, &direction)
        });
        self.status = format!("Pomičem segment {}", truncate(&part_id, 24));
    }

    fn select_adjacent_part(
        &mut self,
        host: &HostClient,
        direction: i32,
    ) -> PlaybackTransportIntent {
        let program = self.segment_program_model();
        let Some(part) =
            program.adjacent_part(&self.selected_part_id, self.wrap_playhead_frame, direction)
        else {
            self.status = if direction < 0 {
                "Nema prethodnog segmenta".into()
            } else {
                "Nema sljedećeg segmenta".into()
            };
            return PlaybackTransportIntent::None;
        };
        let part_id = part.part_id.clone();
        let start_frame = part.global_start_frame;
        drop(program);

        self.selected_part_id = part_id.clone();
        self.set_wrap_playhead_frame(start_frame);
        if self.view_mode == ViewMode::Wrap {
            self.scrub_soft(host)
        } else {
            self.start_wrap_session_for_part(host, Some(part_id))
        }
    }

    fn delete_selected_timeline_item(&mut self, host: &HostClient) {
        if !self.selected_marker_id.trim().is_empty() {
            let marker_id = self.selected_marker_id.clone();
            self.delete_marker(host, &marker_id);
            return;
        }
        let program = self.segment_program_model();
        let part_id = if !self.selected_part_id.trim().is_empty() {
            self.selected_part_id.clone()
        } else {
            program
                .active_part_at_program_frame(self.wrap_playhead_frame)
                .map(|part| part.part_id.clone())
                .unwrap_or_default()
        };
        drop(program);
        if part_id.trim().is_empty() {
            self.status = "Nema odabranog segmenta za brisanje".into();
            return;
        }
        self.delete_part(host, &part_id);
    }

    fn marker_at_head(&mut self, _host: &HostClient) {
        let part = self.selected_part_id.clone();
        let timeline_frame = self.wrap_playhead_frame;
        self.enqueue_edit_command(|instance, request, project| {
            EditorialEditComponent::create_marker(instance, request, project, timeline_frame, &part)
        });
        self.status = format!("Spremam marker @ {}", self.tc(self.virtual_sec()));
    }

    fn delete_marker(&mut self, _host: &HostClient, marker_id: &str) {
        if marker_id.trim().is_empty() {
            return;
        }
        if self.selected_marker_id == marker_id {
            self.selected_marker_id.clear();
        }
        let marker_id = marker_id.to_string();
        self.enqueue_edit_command(|instance, request, project| {
            EditorialEditComponent::delete_marker(instance, request, project, &marker_id)
        });
        self.status = format!("Brišem marker {}", truncate(&marker_id, 24));
    }

    fn move_marker(&mut self, _host: &HostClient, marker_id: &str, direction: &str) {
        if marker_id.trim().is_empty() {
            return;
        }
        let marker_id = marker_id.to_string();
        let direction = direction.to_string();
        self.enqueue_edit_command(|instance, request, project| {
            EditorialEditComponent::move_marker(instance, request, project, &marker_id, &direction)
        });
        self.status = format!("Pomičem marker {}", truncate(&marker_id, 24));
    }

    fn select_marker_slot(&mut self, _host: &HostClient, slot_id: &str) {
        if slot_id.trim().is_empty() {
            return;
        }
        self.selected_marker_id.clear();
        self.selected_slot_id = slot_id.to_string();
        let slot_id = slot_id.to_string();
        self.enqueue_edit_command(|instance, request, project| {
            EditorialEditComponent::select_marker_slot(instance, request, project, &slot_id)
        });
        self.status = format!("Biranje slota {}", truncate(&slot_id, 24));
    }

    fn select_adjacent_marker_slot(
        &mut self,
        host: &HostClient,
        direction: i32,
    ) -> PlaybackTransportIntent {
        let program = self.segment_program_model();
        let Some(slot) = program.adjacent_marker_slot(
            &self.selected_slot_id,
            self.wrap_playhead_frame,
            direction,
        ) else {
            self.status = if direction < 0 {
                "Nema prethodnog M-M slota".into()
            } else {
                "Nema sljedećeg M-M slota".into()
            };
            return PlaybackTransportIntent::None;
        };
        let slot_id = slot.slot_id.clone();
        let start_frame = slot.start_frame;
        drop(program);

        self.set_wrap_playhead_frame(start_frame);
        self.select_marker_slot(host, &slot_id);
        if self.view_mode == ViewMode::Wrap {
            self.scrub_soft(host)
        } else {
            PlaybackTransportIntent::None
        }
    }

    fn focus_empty_marker_slot(&mut self, host: &HostClient) -> PlaybackTransportIntent {
        let program = self.segment_program_model();
        let Some(slot) = program.first_empty_marker_slot() else {
            self.status = "Nema praznog M-M slota".into();
            return PlaybackTransportIntent::None;
        };
        let slot_id = slot.slot_id.clone();
        let start_frame = slot.start_frame;
        drop(program);

        self.set_wrap_playhead_frame(start_frame);
        self.select_marker_slot(host, &slot_id);
        if self.view_mode == ViewMode::Wrap {
            self.scrub_soft(host)
        } else {
            PlaybackTransportIntent::None
        }
    }

    fn select_cover(&mut self, _host: &HostClient, cover_id: &str) {
        if cover_id.trim().is_empty() {
            return;
        }
        self.selected_marker_id.clear();
        let cover_id = cover_id.to_string();
        self.enqueue_edit_command(|instance, request, project| {
            EditorialEditComponent::select_cover(instance, request, project, &cover_id)
        });
        self.status = format!("Biranje covera {}", truncate(&cover_id, 24));
    }

    fn delete_cover(&mut self, _host: &HostClient, cover_id: &str) {
        if cover_id.trim().is_empty() {
            return;
        }
        let cover_id = cover_id.to_string();
        self.enqueue_edit_command(|instance, request, project| {
            EditorialEditComponent::delete_cover(instance, request, project, &cover_id)
        });
        self.status = format!("Brišem cover {}", truncate(&cover_id, 24));
    }

    fn select_marker(
        &mut self,
        host: &HostClient,
        marker_id: &str,
        frame: i64,
    ) -> PlaybackTransportIntent {
        let marker_id = marker_id.trim();
        if marker_id.is_empty() {
            return PlaybackTransportIntent::None;
        }
        let frame = self
            .markers
            .iter()
            .find(|marker| marker.marker_id == marker_id)
            .map(|marker| marker.timeline_frame.max(0))
            .unwrap_or(frame.max(0));
        self.set_wrap_playhead_frame(frame);
        if frame == 0 {
            self.selected_marker_id.clear();
            self.status = "Početni M marker je zaključan.".into();
        } else {
            self.selected_marker_id = marker_id.to_string();
            self.status = format!("M marker odabran @ {}", self.tc(self.virtual_sec()));
        }
        self.ensure_wrap_or_scrub(host)
    }

    fn quick_cover(&mut self, _host: &HostClient) -> PlaybackTransportIntent {
        let target =
            match story_edit::quick_cover_target(&self.selected_slot_id, &self.marker_slots) {
                Ok(target) => target,
                Err(e) => {
                    self.status = e;
                    return PlaybackTransportIntent::None;
                }
            };
        let source = match story_edit::cover_source_range(
            &self.selected_clip_id,
            self.mark_in_set,
            self.mark_out_set,
            self.source_in_frame,
            self.source_out_frame,
            self.source_timebase_fps(),
        ) {
            Ok(source) => source,
            Err(e) => {
                self.status = e;
                return PlaybackTransportIntent::None;
            }
        };
        let slot_id = target.slot_id;
        let clip_id = source.clip_id;
        let in_frame = source.in_frame;
        let out_frame = source.out_frame;
        self.enqueue_edit_command(|instance, request, project| {
            EditorialEditComponent::create_cover_from_source(
                instance, request, project, &slot_id, &clip_id, in_frame, out_frame,
            )
        });
        self.status = "Spremam virtualni kadar za pokrivalicu...".into();
        PlaybackTransportIntent::None
    }

    fn overwrite_cover(&mut self, _host: &HostClient) -> PlaybackTransportIntent {
        let target = match story_edit::overwrite_cover_target(
            &self.selected_slot_id,
            &self.selected_cover_id,
            &self.marker_slots,
            &self.covers,
        ) {
            Ok(target) => target,
            Err(e) => {
                self.status = e;
                return PlaybackTransportIntent::None;
            }
        };
        let source = match story_edit::cover_source_range(
            &self.selected_clip_id,
            self.mark_in_set,
            self.mark_out_set,
            self.source_in_frame,
            self.source_out_frame,
            self.source_timebase_fps(),
        ) {
            Ok(source) => source,
            Err(e) => {
                self.status = e;
                return PlaybackTransportIntent::None;
            }
        };
        let slot_id = target.slot_id;
        let clip_id = source.clip_id;
        let in_frame = source.in_frame;
        let out_frame = source.out_frame;
        self.enqueue_edit_command(|instance, request, project| {
            EditorialEditComponent::create_cover_from_source(
                instance, request, project, &slot_id, &clip_id, in_frame, out_frame,
            )
        });
        self.status = "Spremam pokrivalicu...".into();
        PlaybackTransportIntent::None
    }

    fn mark_in_fit_duration(&mut self, _host: &HostClient) -> PlaybackTransportIntent {
        if self.view_mode != ViewMode::Source {
            self.status = "Mark IN + trajanje slota radi na source timelineu".into();
            return PlaybackTransportIntent::None;
        }
        let Some(_source_fps) = self.source_timebase_fps() else {
            self.status = "Source FPS još nije potvrđen — slot fit nije moguć".into();
            return PlaybackTransportIntent::None;
        };
        let program = self.segment_program_model();
        let slot = program
            .marker_slot_by_id(&self.selected_slot_id)
            .or_else(|| program.first_empty_marker_slot());
        let Some(slot) = slot else {
            self.status = "Nema M-M slota za trajanje".into();
            return PlaybackTransportIntent::None;
        };
        let slot_id = slot.slot_id.clone();
        let slot_duration_frames = (slot.end_frame - slot.start_frame).max(1);
        drop(program);

        let source_duration_frames = slot_duration_frames.max(1);
        let clip_end = self.selected_clip_duration_frames().max(1);
        let in_frame = self
            .source_playhead_frame
            .clamp(0, clip_end.saturating_sub(1));
        let out_frame = (in_frame + source_duration_frames).clamp(in_frame + 1, clip_end);
        self.source_in_frame = in_frame;
        self.source_out_frame = out_frame;
        self.mark_in_set = true;
        self.mark_out_set = true;
        self.select_marker_slot(_host, &slot_id);
        self.status = format!(
            "IN/OUT prema slotu {} · {}–{}",
            truncate(&slot_id, 18),
            self.source_tc_frame(in_frame),
            self.source_tc_frame(out_frame)
        );
        self.cue_current_playhead_intent()
    }

    fn scrub_soft(&mut self, host: &HostClient) -> PlaybackTransportIntent {
        if self.view_mode == ViewMode::Wrap {
            let _ = host;
            self.playing = false;
            self.status = "Playlist input cue".into();
            return self.scrub_current_playhead_intent();
        }
        self.scrub_current_playhead_intent()
    }

    /// ←/→: nudge focused IN/OUT, otherwise seek playhead by frames.
    fn step_focus(
        &mut self,
        host: &HostClient,
        playback: &crate::playback_stack::PlaybackStack,
        frames: i64,
    ) -> PlaybackTransportIntent {
        match self.focus.target {
            FocusTarget::In => self.nudge_in(host, frames),
            FocusTarget::Out => self.nudge_out(host, frames),
            FocusTarget::Playhead => self.seek_playhead(host, playback, frames),
        }
    }

    /// Playhead step via carrier + CueFrame (same path as progress-bar click).
    fn seek_playhead(
        &mut self,
        host: &HostClient,
        playback: &crate::playback_stack::PlaybackStack,
        frames: i64,
    ) -> PlaybackTransportIntent {
        if self.view_mode != ViewMode::Source {
            return self.seek_by_frames(host, frames);
        }
        if self.source_timebase_fps().is_none() {
            self.status = "Source FPS još nije potvrđen — frame seek nije moguć".into();
            return PlaybackTransportIntent::None;
        }
        let clip_end = self.selected_clip_duration_frames();
        let current = if playback.carrier().is_active() {
            playback.carrier().display_frame().0
        } else {
            self.source_playhead_frame
        };
        let next = (current + frames).clamp(0, clip_end);
        self.set_source_playhead_frame(next);
        self.status = format!("Playhead → {} (1f)", self.source_tc_frame(next));
        self.cue_current_playhead_intent()
    }

    fn nudge_in(&mut self, _host: &HostClient, frames: i64) -> PlaybackTransportIntent {
        match self.view_mode {
            ViewMode::Source => {
                let next = (self.source_in_frame + frames).max(0);
                if next >= self.source_out_frame {
                    self.status = "IN ne smije prijeći OUT".into();
                    return PlaybackTransportIntent::None;
                }
                self.source_in_frame = next;
                self.set_source_playhead_frame(next);
                self.status = format!("IN → {} (1f)", self.source_tc_frame(self.source_in_frame));
                self.cue_current_playhead_intent()
            }
            ViewMode::Wrap => {
                let Some(part) = self
                    .parts
                    .iter()
                    .find(|p| p.part_id == self.selected_part_id)
                else {
                    self.status = "IN nudge: odaberi segment".into();
                    return PlaybackTransportIntent::None;
                };
                let cur = part.in_frame.max(0);
                let next = (cur + frames).max(0);
                if next >= part.out_frame.max(cur + 1) {
                    self.status = "IN ne smije prijeći OUT".into();
                    return PlaybackTransportIntent::None;
                }
                let part_id = part.part_id.clone();
                let local_frame = (next - cur).max(0);
                self.enqueue_edit_command(|instance, request, project| {
                    EditorialEditComponent::mark_part_in(
                        instance,
                        request,
                        project,
                        &part_id,
                        local_frame,
                    )
                });
                self.status = format!("Spremam IN → {} (1f)", self.source_tc_frame(next));
                PlaybackTransportIntent::None
            }
        }
    }

    fn nudge_out(&mut self, _host: &HostClient, frames: i64) -> PlaybackTransportIntent {
        match self.view_mode {
            ViewMode::Source => {
                let next = (self.source_out_frame + frames).max(0);
                if next <= self.source_in_frame {
                    self.status = "OUT ne smije prijeći ispred IN".into();
                    return PlaybackTransportIntent::None;
                }
                self.source_out_frame = next;
                self.set_source_playhead_frame(next);
                self.status = format!("OUT → {} (1f)", self.source_tc_frame(self.source_out_frame));
                self.cue_current_playhead_intent()
            }
            ViewMode::Wrap => {
                let Some(part) = self
                    .parts
                    .iter()
                    .find(|p| p.part_id == self.selected_part_id)
                else {
                    self.status = "OUT nudge: odaberi segment".into();
                    return PlaybackTransportIntent::None;
                };
                let inn = part.in_frame.max(0);
                let cur = part.out_frame.max(inn + 1);
                let next = (cur + frames).max(0);
                if next <= inn {
                    self.status = "OUT ne smije prijeći ispred IN".into();
                    return PlaybackTransportIntent::None;
                }
                let part_id = part.part_id.clone();
                let local_frame = (next - inn).max(0);
                self.enqueue_edit_command(|instance, request, project| {
                    EditorialEditComponent::mark_part_out(
                        instance,
                        request,
                        project,
                        &part_id,
                        local_frame,
                    )
                });
                self.status = format!("Spremam OUT → {} (1f)", self.source_tc_frame(next));
                PlaybackTransportIntent::None
            }
        }
    }

    fn seek_by_frames(&mut self, host: &HostClient, frames: i64) -> PlaybackTransportIntent {
        if self.view_mode == ViewMode::Source {
            if self.source_timebase_fps().is_none() {
                self.status = "Source FPS još nije potvrđen — frame seek nije moguć".into();
                return PlaybackTransportIntent::None;
            }
            let clip_end = self.selected_clip_duration_frames();
            self.set_source_playhead_frame(
                (self.source_playhead_frame + frames).clamp(0, clip_end),
            );
            return self.cue_current_playhead_intent();
        }
        self.set_wrap_playhead_frame((self.wrap_playhead_frame + frames).max(0));
        let dur = self.segment_program_model().duration_frames();
        if dur > 0 {
            self.set_wrap_playhead_frame(self.wrap_playhead_frame.min(dur));
        }
        self.scrub_soft(host)
    }
}

impl crate::player_bridge::PlayerClient for StoryScreen {
    fn playback_source_ref(&self) -> Option<BroadcastHostSourceRef> {
        self.active_playback_source_ref().cloned()
    }

    fn playback_media_path(&self) -> Option<String> {
        self.active_playback_media_path().map(str::to_string)
    }

    fn playback_source_fps(&self) -> f64 {
        self.active_playback_fps()
    }

    fn playback_source_has_audio(&self) -> bool {
        self.active_playback_has_audio()
    }

    fn playback_source_audio_channels(&self) -> u8 {
        if self.active_playback_has_audio() {
            self.active_playback_audio_channels().max(2).min(4)
        } else {
            2
        }
    }

    fn missing_source_message(&self) -> String {
        "Odaberi source kadar prije play".into()
    }

    fn missing_path_message(&self) -> String {
        "Proxy path nije spreman — pričekaj proxy ili odaberi clip ponovo".into()
    }

    fn set_player_preview_active(&mut self, active: bool) {
        self.broadcast_preview_active = active;
    }

    fn apply_playback_command_state(&mut self, playing: bool, status: impl Into<String>) {
        self.playing = playing;
        self.status = status.into();
    }

    fn apply_player_frame(&mut self, _image: ColorImage, source_frame: FrameNumber, playing: bool) {
        self.playing = playing;
        self.status = if self.view_mode == ViewMode::Wrap && playing {
            "Playlist input play".into()
        } else {
            "Broadcast player".into()
        };
        self.sync_playhead_from_player_frame(source_frame);
    }

    fn apply_player_state(
        &mut self,
        source_frame: FrameNumber,
        playing: bool,
        status: impl Into<String>,
    ) {
        self.playing = playing;
        let status = status.into();
        self.status = if self.view_mode == ViewMode::Wrap && playing {
            "Playlist input play".into()
        } else {
            status
        };
        self.sync_playhead_from_player_frame(source_frame);
    }

    fn apply_player_error(&mut self, status: impl Into<String>) {
        self.broadcast_preview_active = false;
        self.playing = false;
        self.status = status.into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::TimelineSegment;
    use crate::player_remote::{PlayerEvent, PROGRAM_AUDIO_OUTPUT_CH1};
    use qnc_player_core::FieldMode;
    use serde_json::json;

    fn story_state_with_selected_part(part_id: &str) -> Value {
        json!({
            "selected_part_id": part_id,
            "parts": [{
                "part_id": part_id,
                "kind": "tonovi",
                "clip_id": "clip_a",
                "in_frame": 100,
                "out_frame": 150,
                "fps": 50.0,
                "duration_frames": 50
            }],
            "all_clips": [{
                "shot_id": "clip_a",
                "root_shot_id": "clip_a",
                "clip_id": "clip_a",
                "fps": 50.0,
                "duration_frames": 250,
                "play_path": "C:/qnc/proxy/clip_a.mp4",
                "has_audio": true,
                "audio_channels": 2
            }],
            "virtual_shots": [],
            "covers": [],
            "markers": [],
            "marker_slots": []
        })
    }

    fn timeline_with_part(part_id: &str) -> TimelineModel {
        TimelineModel {
            project_id: "p".into(),
            application: "wrap".into(),
            timeline_fps: 50.0,
            duration_frames: 50,
            duration_sec: 2.0,
            rows: Vec::new(),
            segments: vec![TimelineSegment {
                part_id: part_id.into(),
                clip_id: "clip_a".into(),
                global_start_frame: 10,
                global_end_frame: 60,
                duration_frames: 50,
                streamable: true,
                ..TimelineSegment::default()
            }],
        }
    }

    fn two_part_timeline() -> TimelineModel {
        TimelineModel {
            project_id: "p".into(),
            application: "wrap".into(),
            timeline_fps: 50.0,
            duration_frames: 100,
            duration_sec: 4.0,
            rows: Vec::new(),
            segments: vec![
                TimelineSegment {
                    part_id: "part_a".into(),
                    clip_id: "clip_a".into(),
                    global_start_frame: 0,
                    global_end_frame: 50,
                    duration_frames: 50,
                    streamable: true,
                    ..TimelineSegment::default()
                },
                TimelineSegment {
                    part_id: "part_b".into(),
                    clip_id: "clip_a".into(),
                    global_start_frame: 50,
                    global_end_frame: 100,
                    duration_frames: 50,
                    streamable: true,
                    ..TimelineSegment::default()
                },
            ],
        }
    }

    fn playlist_with_part(part_id: &str) -> EditorialPlaylist {
        EditorialPlaylist {
            project_id: "p".into(),
            timeline_fps: 50.0,
            duration_frames: 50,
            duration_sec: 2.0,
            segments: vec![EditorialPlaylistSegment {
                part_id: part_id.into(),
                kind: "tonovi".into(),
                clip_id: "clip_a".into(),
                global_start_frame: 10,
                global_end_frame: 60,
                duration_frames: 50,
                source_in_frame: 100,
                source_out_frame: 150,
                source_fps: 50.0,
                streamable: true,
                ..EditorialPlaylistSegment::default()
            }],
        }
    }

    fn two_part_playlist() -> EditorialPlaylist {
        EditorialPlaylist {
            project_id: "p".into(),
            timeline_fps: 50.0,
            duration_frames: 100,
            duration_sec: 4.0,
            segments: vec![
                EditorialPlaylistSegment {
                    part_id: "part_a".into(),
                    kind: "tonovi".into(),
                    clip_id: "clip_a".into(),
                    global_start_frame: 0,
                    global_end_frame: 50,
                    duration_frames: 50,
                    source_in_frame: 100,
                    source_out_frame: 150,
                    source_fps: 50.0,
                    streamable: true,
                    ..EditorialPlaylistSegment::default()
                },
                EditorialPlaylistSegment {
                    part_id: "part_b".into(),
                    kind: "tonovi".into(),
                    clip_id: "clip_a".into(),
                    global_start_frame: 50,
                    global_end_frame: 100,
                    duration_frames: 50,
                    source_in_frame: 200,
                    source_out_frame: 250,
                    source_fps: 50.0,
                    streamable: true,
                    ..EditorialPlaylistSegment::default()
                },
            ],
        }
    }

    #[test]
    fn source_dock_ignores_segment_carrier_when_wrap_is_active() {
        let mut playback = PlaybackStack::new();
        let source_ref = BroadcastHostSourceRef::from_frame_fields(
            "p",
            "clip_a",
            "",
            "clip_a",
            Some(FrameNumber(0)),
            Some(FrameNumber(300)),
            FrameNumber(300),
        )
        .unwrap();
        playback
            .ensure_open(crate::player_remote::BroadcastPlayerOpenRequest {
                source_ref: source_ref.clone(),
                media_input: "C:/qnc/proxy/clip_a.mp4".into(),
                source_fps: 50.0,
                has_audio: true,
                audio_channels: 2,
                start_source_frame: FrameNumber(0),
            })
            .unwrap();
        playback.ingest_events(&[
            PlayerEvent::SourceReady {
                fps: 50.0,
                duration_frames: 300,
                in_frame: 100,
                out_frame: 150,
                field_mode: FieldMode::Progressive,
            },
            PlayerEvent::State {
                source_frame: FrameNumber(130),
                source_sec: 5.2,
                playing: true,
                status: "playing".into(),
            },
        ]);

        let mut screen = StoryScreen::story();
        screen.selected_source_ref = Some(source_ref);
        screen.view_mode = ViewMode::Wrap;
        screen.source_playhead_frame = 12;
        screen.wrap_playhead_frame = 70;

        let fallback_source_model =
            screen.source_dock_timeline_model(&playback, 50.0, 300, 0, 300, 10, 40);
        assert_eq!(fallback_source_model.playhead_frame(), 12);

        screen.view_mode = ViewMode::Source;
        let live_source_model =
            screen.source_dock_timeline_model(&playback, 50.0, 300, 0, 300, 10, 40);
        assert_eq!(live_source_model.playhead_frame(), 130);
    }

    #[test]
    fn program_request_does_not_replace_source_dock_selection() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        screen.loaded_project_id = "p".into();
        let source_shot = StoryShot {
            shot_id: "source_a".into(),
            root_shot_id: "source_a".into(),
            clip_id: "clip_source".into(),
            fps: 50.0,
            in_frame: 0,
            out_frame: 250,
            duration_frames: 250,
            play_path: "C:/qnc/proxy/source.mp4".into(),
            has_audio: true,
            audio_channels: 2,
            ..StoryShot::default()
        };
        screen.all_clips = vec![
            source_shot.clone(),
            StoryShot {
                shot_id: "segment_src".into(),
                root_shot_id: "segment_src".into(),
                clip_id: "clip_segment".into(),
                fps: 50.0,
                in_frame: 0,
                out_frame: 300,
                duration_frames: 300,
                play_path: "C:/qnc/proxy/segment.mp4".into(),
                has_audio: true,
                audio_channels: 2,
                ..StoryShot::default()
            },
        ];
        screen.select_shot_from_snapshot(&source_shot);
        let source_clip_id = screen.selected_clip_id.clone();
        let source_ref = screen.selected_source_ref.clone();
        let source_path = screen.selected_play_path.clone();

        screen.timeline = Some(TimelineModel {
            project_id: "p".into(),
            application: "wrap".into(),
            timeline_fps: 50.0,
            duration_frames: 50,
            duration_sec: 2.0,
            rows: Vec::new(),
            segments: vec![TimelineSegment {
                part_id: "part_segment".into(),
                clip_id: "clip_segment".into(),
                global_start_frame: 0,
                global_end_frame: 50,
                duration_frames: 50,
                streamable: true,
                ..TimelineSegment::default()
            }],
        });
        screen.playlist = Some(EditorialPlaylist {
            project_id: "p".into(),
            timeline_fps: 50.0,
            duration_frames: 50,
            duration_sec: 2.0,
            segments: vec![EditorialPlaylistSegment {
                part_id: "part_segment".into(),
                kind: "tonovi".into(),
                clip_id: "clip_segment".into(),
                global_start_frame: 0,
                global_end_frame: 50,
                duration_frames: 50,
                source_in_frame: 100,
                source_out_frame: 150,
                source_fps: 50.0,
                streamable: true,
                ..EditorialPlaylistSegment::default()
            }],
        });
        screen.parts = vec![StoryPart {
            part_id: "part_segment".into(),
            clip_id: "clip_segment".into(),
            in_frame: 100,
            out_frame: 150,
            fps: 50.0,
            duration_frames: 50,
            ..StoryPart::default()
        }];
        screen.start_wrap_session_from_snapshot(Some("part_segment".into()));

        let intent = screen.playback_transport_toggle_intent(false, false);

        assert!(matches!(intent, PlaybackTransportIntent::PlayProgram(_)));
        assert_eq!(screen.selected_clip_id, source_clip_id);
        assert_eq!(screen.selected_source_ref, source_ref);
        assert_eq!(screen.selected_play_path, source_path);
    }

    #[test]
    fn source_player_frame_does_not_advance_segment_playhead() {
        let mut screen = StoryScreen::story();
        screen.view_mode = ViewMode::Source;
        screen.source_playhead_frame = 12;
        screen.wrap_playhead_frame = 80;

        screen.sync_playhead_from_player_frame(FrameNumber(140));

        assert_eq!(screen.source_playhead_frame, 140);
        assert_eq!(screen.wrap_playhead_frame, 80);
    }

    #[test]
    fn wrap_timeline_refresh_keeps_wrap_on_timeline_without_native_scrub() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        screen.loaded_project_id = "p".into();
        screen.view_mode = ViewMode::Wrap;
        screen.state_loaded = true;
        screen.initial_selection_done = true;
        screen.selected_part_id = "part_new".into();
        screen.wrap_playhead_frame = 10;
        screen.apply_story_state(&story_state_with_selected_part("part_new"));
        screen.playlist = Some(playlist_with_part("part_new"));
        screen.playlist_loaded = true;

        let timeline_intent =
            screen.apply_editorial_timeline_model("p", timeline_with_part("part_new"));

        assert_eq!(timeline_intent, PlaybackTransportIntent::None);
        assert_eq!(screen.view_mode, ViewMode::Wrap);
        assert_eq!(screen.selected_part_id, "part_new");
        assert_eq!(screen.wrap_playhead_frame, 10);
    }

    #[test]
    fn wrap_timeline_refresh_selects_part_under_playhead_without_native_scrub() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        screen.loaded_project_id = "p".into();
        screen.view_mode = ViewMode::Wrap;
        screen.state_loaded = true;
        screen.initial_selection_done = true;
        screen.selected_part_id = "part_b".into();
        screen.wrap_playhead_frame = 10;
        screen.parts = vec![
            StoryPart {
                part_id: "part_a".into(),
                clip_id: "clip_a".into(),
                in_frame: 100,
                out_frame: 150,
                fps: 50.0,
                duration_frames: 50,
                ..StoryPart::default()
            },
            StoryPart {
                part_id: "part_b".into(),
                clip_id: "clip_a".into(),
                in_frame: 200,
                out_frame: 250,
                fps: 50.0,
                duration_frames: 50,
                ..StoryPart::default()
            },
        ];
        screen.all_clips = vec![StoryShot {
            clip_id: "clip_a".into(),
            fps: 50.0,
            duration_frames: 300,
            play_path: "C:/qnc/proxy/clip_a.mp4".into(),
            has_audio: true,
            audio_channels: 2,
            ..StoryShot::default()
        }];
        screen.playlist = Some(two_part_playlist());
        screen.playlist_loaded = true;

        let timeline_intent = screen.apply_editorial_timeline_model("p", two_part_timeline());

        assert_eq!(timeline_intent, PlaybackTransportIntent::None);
        assert_eq!(screen.selected_part_id, "part_a");
        assert_eq!(screen.wrap_playhead_frame, 10);
    }

    #[test]
    fn wrap_player_frame_sync_uses_program_frame_active_part() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        screen.loaded_project_id = "p".into();
        screen.view_mode = ViewMode::Wrap;
        screen.timeline = Some(two_part_timeline());
        screen.playlist = Some(two_part_playlist());
        screen.selected_part_id = "part_a".into();
        screen.parts = vec![
            StoryPart {
                part_id: "part_a".into(),
                clip_id: "clip_a".into(),
                in_frame: 100,
                out_frame: 150,
                fps: 50.0,
                duration_frames: 50,
                ..StoryPart::default()
            },
            StoryPart {
                part_id: "part_b".into(),
                clip_id: "clip_a".into(),
                in_frame: 200,
                out_frame: 250,
                fps: 50.0,
                duration_frames: 50,
                ..StoryPart::default()
            },
        ];
        screen.all_clips = vec![StoryShot {
            clip_id: "clip_a".into(),
            fps: 50.0,
            duration_frames: 300,
            play_path: "C:/qnc/proxy/clip_a.mp4".into(),
            has_audio: true,
            audio_channels: 2,
            ..StoryShot::default()
        }];

        screen.sync_playhead_from_player_frame(FrameNumber(60));

        assert_eq!(screen.selected_part_id, "part_b");
        assert_eq!(screen.wrap_playhead_frame, 60);
    }

    #[test]
    fn wrap_toggle_starts_broadcast_program_request_with_audio() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        screen.loaded_project_id = "p".into();
        screen.view_mode = ViewMode::Wrap;
        screen.timeline = Some(two_part_timeline());
        screen.playlist = Some(two_part_playlist());
        screen.wrap_playhead_frame = 60;
        screen.parts = vec![
            StoryPart {
                part_id: "part_a".into(),
                clip_id: "clip_a".into(),
                in_frame: 100,
                out_frame: 150,
                fps: 50.0,
                duration_frames: 50,
                ..StoryPart::default()
            },
            StoryPart {
                part_id: "part_b".into(),
                clip_id: "clip_a".into(),
                in_frame: 200,
                out_frame: 250,
                fps: 50.0,
                duration_frames: 50,
                ..StoryPart::default()
            },
        ];
        screen.all_clips = vec![StoryShot {
            clip_id: "clip_a".into(),
            fps: 50.0,
            duration_frames: 300,
            play_path: "C:/qnc/proxy/clip_a.mp4".into(),
            has_audio: true,
            audio_channels: 2,
            ..StoryShot::default()
        }];

        let intent = screen.playback_transport_toggle_intent(false, false);

        let request = match intent {
            PlaybackTransportIntent::PlayProgram(request) => request,
            other => panic!("expected PlayProgram, got {other:?}"),
        };
        assert_eq!(screen.view_mode, ViewMode::Wrap);
        assert_eq!(screen.wrap_playhead_frame, 60);
        assert_eq!(screen.selected_part_id, "part_b");
        assert!(screen.playing);
        assert!(screen.playback_source_ref().is_none());
        assert_eq!(request.start_program_frame, FrameNumber(60));
        assert_eq!(request.items.len(), 2);
        let item = &request.items[1];
        assert_eq!(item.record_in_frame, FrameNumber(50));
        assert_eq!(item.record_out_frame, FrameNumber(100));
        assert_eq!(item.sources.len(), 1);
        let source = &item.sources[0];
        assert_eq!(source.source_ref.in_frame, Some(FrameNumber(200)));
        assert_eq!(source.source_ref.out_frame, Some(FrameNumber(250)));
        assert!(source.has_video);
        assert!(source.has_audio);
        assert_eq!(source.audio_channels, 2);
        assert_eq!(source.audio_output_channel, Some(PROGRAM_AUDIO_OUTPUT_CH1));
    }

    #[test]
    fn playlist_input_request_requires_playlist_not_ui_timeline() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        screen.loaded_project_id = "p".into();
        screen.view_mode = ViewMode::Wrap;
        screen.timeline = Some(two_part_timeline());
        screen.selected_part_id = "part_a".into();
        screen.parts = vec![StoryPart {
            part_id: "part_a".into(),
            clip_id: "clip_a".into(),
            in_frame: 100,
            out_frame: 150,
            fps: 50.0,
            duration_frames: 50,
            ..StoryPart::default()
        }];

        let intent = screen.playback_transport_toggle_intent(false, false);

        assert_eq!(intent, PlaybackTransportIntent::None);
        assert_eq!(screen.status, "Playlist input nema valjan timeline FPS");
    }

    #[test]
    fn wrap_transport_available_uses_playlist_input_not_source_ref() {
        let mut screen = StoryScreen::story();
        screen.view_mode = ViewMode::Wrap;
        screen.playlist = Some(two_part_playlist());

        assert!(screen.playback_source_ref().is_none());
        assert!(screen.playback_transport_available());
    }

    #[test]
    fn source_toggle_intent_is_not_blocked_by_transport_availability_gate() {
        let mut screen = StoryScreen::story();
        screen.view_mode = ViewMode::Source;

        assert!(!screen.playback_transport_available());
        assert_eq!(
            screen.playback_transport_toggle_intent(false, false),
            PlaybackTransportIntent::TogglePlay
        );
    }

    #[test]
    fn source_dock_keyboard_lock_routes_space_to_source_timeline() {
        let mut screen = StoryScreen::story();
        screen.view_mode = ViewMode::Wrap;
        screen.source_dock_keyboard_focus = true;

        let intent = screen.playback_transport_toggle_intent(true, true);

        assert_eq!(intent, PlaybackTransportIntent::TogglePlay);
        assert_eq!(screen.view_mode, ViewMode::Source);
    }

    #[test]
    fn story_play_shortcut_defers_toggle_to_app_transport() {
        let mut screen = StoryScreen::story();
        screen.view_mode = ViewMode::Wrap;
        screen.playing = true;

        let intent = screen.dispatch_playback_action(
            &HostClient::new("http://127.0.0.1:1"),
            playback_controls::PlaybackAction::TogglePlay,
        );

        assert_eq!(intent, PlaybackTransportIntent::TogglePlay);
        assert!(screen.playing);
    }

    #[test]
    fn quick_cover_adds_to_empty_slot_when_another_slot_is_filled() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        screen.loaded_project_id = "p".into();
        screen.selected_slot_id = "slot_a".into();
        screen.marker_slots = vec![
            MarkerSlot {
                slot_id: "slot_a".into(),
                has_cover: true,
                ..MarkerSlot::default()
            },
            MarkerSlot {
                slot_id: "slot_b".into(),
                has_cover: false,
                ..MarkerSlot::default()
            },
        ];
        screen.selected_clip_id = "clip_a".into();
        screen.source_in_frame = 100;
        screen.source_out_frame = 150;
        screen.mark_in_set = true;
        screen.mark_out_set = true;
        screen.selected_source_fps = 50.0;
        screen.source_timebase_ready = true;

        let intent = screen.dispatch_playback_action(
            &HostClient::new("http://127.0.0.1:1"),
            playback_controls::PlaybackAction::QuickCover,
        );

        assert_eq!(intent, PlaybackTransportIntent::None);
        let commands = screen.drain_backend_commands();
        assert_eq!(commands.len(), 1);
        let payload = commands[0].payload.as_ref().expect("cover payload");
        assert_eq!(
            payload.get("slot_id").and_then(Value::as_str),
            Some("slot_b")
        );
        assert_eq!(
            payload.get("clip_id").and_then(Value::as_str),
            Some("clip_a")
        );
        assert_eq!(payload.get("in_frame").and_then(Value::as_i64), Some(100));
        assert_eq!(payload.get("out_frame").and_then(Value::as_i64), Some(150));
    }

    #[test]
    fn source_selection_locks_source_timeline_until_wrap_entry() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        let source_shot = StoryShot {
            shot_id: "shot_a".into(),
            clip_id: "clip_a".into(),
            fps: 50.0,
            duration_frames: 250,
            out_frame: 250,
            play_path: "C:/qnc/proxy/clip_a.mp4".into(),
            ..StoryShot::default()
        };
        screen.all_clips = vec![source_shot.clone()];

        screen.select_shot_from_snapshot(&source_shot);

        assert!(screen.source_dock_keyboard_focus);

        screen.start_wrap_session_from_snapshot(None);

        assert!(!screen.source_dock_keyboard_focus);
        assert_eq!(screen.view_mode, ViewMode::Wrap);
    }

    #[test]
    fn wrap_seek_emits_playlist_program_frame_without_source_ref() {
        let mut screen = StoryScreen::story();
        screen.view_mode = ViewMode::Wrap;
        screen.playlist = Some(two_part_playlist());
        screen.wrap_playhead_frame = 10;

        let intent = screen.seek_by_frames(&HostClient::new("http://127.0.0.1:1"), 15);

        assert_eq!(screen.wrap_playhead_frame, 25);
        assert!(screen.playback_source_ref().is_none());
        assert_eq!(intent, PlaybackTransportIntent::ScrubFrame(25));
    }

    #[test]
    fn wrap_resume_uses_loaded_playlist_input_without_reopening_program() {
        let mut screen = StoryScreen::story();
        screen.view_mode = ViewMode::Wrap;
        screen.playlist = Some(two_part_playlist());
        screen.playing = false;

        let intent = screen.playback_transport_toggle_intent(true, false);

        assert_eq!(intent, PlaybackTransportIntent::PlayLoadedInput);
        assert!(screen.playing);
        assert!(screen.playback_source_ref().is_none());
    }

    #[test]
    fn playlist_player_state_pauses_even_when_story_playing_flag_is_stale() {
        let mut screen = StoryScreen::story();
        screen.view_mode = ViewMode::Wrap;
        screen.playlist = Some(two_part_playlist());
        screen.playing = false;

        let intent = screen.playback_transport_toggle_intent(true, true);

        assert_eq!(intent, PlaybackTransportIntent::Pause);
        assert!(!screen.playing);
    }

    #[test]
    fn create_part_waits_for_playlist_before_timeline_projection() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        screen.loaded_project_id = "p".into();
        screen.state_loaded = true;
        screen.initial_selection_done = true;
        screen.wrap_playhead_frame = 10;

        let edit_intent = screen.apply_editorial_edit_data(EditorialEditData {
            instance_id: "story".into(),
            project_id: "p".into(),
            kind: EditorialEditKind::CreatePartFromMarks,
            detail: "tonovi".into(),
            state: story_state_with_selected_part("part_new"),
        });

        assert_eq!(edit_intent, PlaybackTransportIntent::None);
        assert_eq!(screen.library_tab, LibraryTab::All);
        assert!(screen.selected_source_ref.is_none());

        let timeline_intent =
            screen.apply_editorial_timeline_model("p", timeline_with_part("part_new"));
        assert_eq!(timeline_intent, PlaybackTransportIntent::None);

        let playlist_intent = screen.apply_editorial_playlist("p", playlist_with_part("part_new"));

        assert_eq!(playlist_intent, PlaybackTransportIntent::None);
        assert_eq!(screen.library_tab, LibraryTab::All);
        assert_eq!(screen.view_mode, ViewMode::Wrap);
        assert_eq!(screen.selected_part_id, "part_new");
        assert_eq!(screen.wrap_playhead_frame, 10);
    }

    #[test]
    fn delete_part_waits_for_playlist_before_timeline_projection() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        screen.loaded_project_id = "p".into();
        screen.state_loaded = true;
        screen.initial_selection_done = true;
        screen.wrap_playhead_frame = 10;
        screen.selected_part_id = "part_deleted".into();

        let edit_intent = screen.apply_editorial_edit_data(EditorialEditData {
            instance_id: "story".into(),
            project_id: "p".into(),
            kind: EditorialEditKind::DeletePart,
            detail: "part_deleted".into(),
            state: story_state_with_selected_part("part_remaining"),
        });

        assert_eq!(edit_intent, PlaybackTransportIntent::None);
        assert_eq!(screen.selected_part_id, "part_remaining");

        let timeline_intent =
            screen.apply_editorial_timeline_model("p", timeline_with_part("part_remaining"));
        assert_eq!(timeline_intent, PlaybackTransportIntent::None);

        let playlist_intent =
            screen.apply_editorial_playlist("p", playlist_with_part("part_remaining"));

        assert_eq!(playlist_intent, PlaybackTransportIntent::None);
        assert_eq!(screen.view_mode, ViewMode::Wrap);
        assert_eq!(screen.selected_part_id, "part_remaining");
        assert_eq!(screen.wrap_playhead_frame, 10);
    }

    #[test]
    fn marker_edit_clears_stale_pending_wrap_scrub() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        screen.loaded_project_id = "p".into();
        screen.wrap_playhead_frame = 10;

        let edit_intent = screen.apply_editorial_edit_data(EditorialEditData {
            instance_id: "story".into(),
            project_id: "p".into(),
            kind: EditorialEditKind::CreatePartFromMarks,
            detail: "tonovi".into(),
            state: story_state_with_selected_part("part_new"),
        });
        assert_eq!(edit_intent, PlaybackTransportIntent::None);

        let marker_intent = screen.apply_editorial_edit_data(EditorialEditData {
            instance_id: "story".into(),
            project_id: "p".into(),
            kind: EditorialEditKind::CreateMarker,
            detail: "marker_a".into(),
            state: story_state_with_selected_part("part_new"),
        });
        assert_eq!(marker_intent, PlaybackTransportIntent::None);

        let timeline_intent =
            screen.apply_editorial_timeline_model("p", timeline_with_part("part_new"));

        assert_eq!(timeline_intent, PlaybackTransportIntent::None);
    }

    #[test]
    fn timeline_error_clears_pending_wrap_scrub() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        screen.loaded_project_id = "p".into();
        screen.wrap_playhead_frame = 10;

        let edit_intent = screen.apply_editorial_edit_data(EditorialEditData {
            instance_id: "story".into(),
            project_id: "p".into(),
            kind: EditorialEditKind::CreatePartFromMarks,
            detail: "tonovi".into(),
            state: story_state_with_selected_part("part_new"),
        });
        assert_eq!(edit_intent, PlaybackTransportIntent::None);

        screen.set_editorial_meta_error("p", "timeline failed");
        let timeline_intent =
            screen.apply_editorial_timeline_model("p", timeline_with_part("part_new"));

        assert_eq!(timeline_intent, PlaybackTransportIntent::None);
    }
}
