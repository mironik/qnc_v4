//! Native editorial form (Story / Media Assist) — one screen, role attributes.
//!
//! UI blocks (`qnc_ui`, `editorial::*`, `qnc_source_dock`, cards) are shared
//! components. This form only chooses composition via `EditorialRole`.

mod focus;
pub(crate) mod playback_controls;
mod source_editor;
mod story_edit;
mod story_selection;
mod story_state;
mod story_timeline;

pub(super) use crate::editorial::types::{
    LibraryTab, MarkerSlot, StoryCover, StoryMarker, StoryPart, StoryShot,
};

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions, Vec2};
use serde_json::Value;

use crate::api::{
    EditorialPlaylist, EditorialPlaylistSegment, EditorialSourceTimebase, HostClient, TimelineModel,
};
use crate::component_runtime::ComponentBackendCommand;
use crate::components::{
    EditorialEditComponent, EditorialEditData, EditorialEditKind, EditorialPendingWrapScrubInput,
    EditorialPlaybackTransportComponent, EditorialPlaybackView, EditorialPlaylistProgramInput,
    EditorialTogglePlayInput, EditorialTogglePlayOutcome, EditorialWrapRefreshInput,
    EditorialWrapRefreshOutcome, EditorialWrapSessionInput, EditorialWrapSessionOutcome,
    PlaybackMediaResolution, PlaybackMediaResolverComponent, SyncCoverCaptureComponent,
    SyncCoverCaptureState, SyncCoverPreviewInput, SyncCoverSpaceContext,
};
use crate::composition::EditorialRole;
use crate::editorial::common::{shot_id, truncate};
use crate::editorial::program_waveform::{self, ProgramWaveformAssets};
use crate::editorial::segment_program::{SegmentProgramModel, SegmentProgramNavigationKind};
use crate::editorial::{marker_cover_panel, media_pool, segment_panel};
use crate::frame_time::{frame_to_seconds, normalize_fps, seconds_to_frame, seconds_to_timecode};
use crate::media_assets::{
    self, AsyncImageAssetLoader, AsyncSourceMediaAssetLoader, ImageAssetKey,
};
use crate::playback_routing::PlaybackTransportIntent;
use crate::playback_stack::PlaybackStack;
use crate::player_contract::{BroadcastHostSourceRef, BroadcastSourceTimebase, FrameNumber};
use crate::qnc_filmstrip_background::FilmFrame;
use crate::qnc_timeline::{ExpandedAudio, TimelineFocusPaint};
use crate::shortcuts::{StoryBindings, STORYBOARD_SHORTCUT_SCOPE as STORY_SHORTCUT_SCOPE};

use self::focus::{
    adjacent_source_navigation_target, next_panel_focus, FocusTarget, PanelFocus,
    SourceNavigationTarget, TimelineFocus,
};
const SOURCE_MEDIA_RETRY_DELAY: Duration = Duration::from_secs(1);
const SOURCE_MEDIA_MAX_RETRIES: u8 = 30;
const FILM_FRAME_MAX_LOAD_ATTEMPTS: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Wrap,
    Source,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoryObjectRef {
    object_type: String,
    object_id: String,
}

impl StoryObjectRef {
    fn cover(cover_id: &str) -> Option<Self> {
        let cover_id = cover_id.trim();
        (!cover_id.is_empty()).then(|| Self {
            object_type: "cover".into(),
            object_id: cover_id.to_string(),
        })
    }
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
    cover_shots: Vec<StoryShot>,
    covers: Vec<StoryCover>,
    pending_cover_projections: Vec<StoryCover>,
    pending_cover_undo_slots: HashSet<String>,
    active_redo_object: Option<StoryObjectRef>,
    markers: Vec<StoryMarker>,
    marker_slots: Vec<MarkerSlot>,
    selected_part_id: String,
    selected_shot_id: String,
    focused_media_shot_id: String,
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
    selected_source_timebase: BroadcastSourceTimebase,
    selected_source_has_audio: bool,
    selected_source_audio_channels: u8,
    selected_source_ref: Option<BroadcastHostSourceRef>,
    /// Resolved playback input for broadcast player (local proxy path or routed URI).
    selected_play_path: String,
    selected_play_input_clip_id: String,
    pending_play_input_clip_id: String,
    /// Resolved playlist-input media inputs by clip_id. The playlist builder uses
    /// only this map, never snapshot play_path.
    playlist_playback_inputs: HashMap<String, String>,
    pending_playlist_playback_input_clip_ids: HashSet<String>,
    sync_cover: SyncCoverCaptureState,
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
    filmstrip_manifest_ready: bool,
    waveform_clip_id: String,
    image_loader: AsyncImageAssetLoader,
    filmstrip_image_loader: AsyncImageAssetLoader,
    source_media_loader: AsyncSourceMediaAssetLoader,
    program_waveforms: ProgramWaveformAssets,
    repaint_ctx: Option<egui::Context>,
    source_media_retry_at: Option<Instant>,
    source_media_retry_clip_id: String,
    source_media_retry_attempts: u8,
    /// Pending frame target while the player cue catches up.
    source_timebase_ready: bool,
    /// Sticky keyboard owner for the bottom source timeline.
    /// Cleared only by explicit Wrap/segment entry.
    source_dock_keyboard_focus: bool,
    /// Active keyboard panel. This is separate from selection so the app can be
    /// operated without a mouse.
    panel_focus: PanelFocus,
    media_pool_grid_cols: usize,
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
            cover_shots: Vec::new(),
            covers: Vec::new(),
            pending_cover_projections: Vec::new(),
            pending_cover_undo_slots: HashSet::new(),
            active_redo_object: None,
            markers: Vec::new(),
            marker_slots: Vec::new(),
            selected_part_id: String::new(),
            selected_shot_id: String::new(),
            focused_media_shot_id: String::new(),
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
            selected_source_timebase: BroadcastSourceTimebase::default(),
            selected_source_has_audio: false,
            selected_source_audio_channels: 0,
            selected_source_ref: None,
            selected_play_path: String::new(),
            selected_play_input_clip_id: String::new(),
            pending_play_input_clip_id: String::new(),
            playlist_playback_inputs: HashMap::new(),
            pending_playlist_playback_input_clip_ids: HashSet::new(),
            sync_cover: SyncCoverCaptureState::default(),
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
            filmstrip_manifest_ready: false,
            waveform_clip_id: String::new(),
            image_loader: AsyncImageAssetLoader::new(),
            filmstrip_image_loader: AsyncImageAssetLoader::new(),
            source_media_loader: AsyncSourceMediaAssetLoader::new(),
            program_waveforms: ProgramWaveformAssets::new(),
            repaint_ctx: None,
            source_media_retry_at: None,
            source_media_retry_clip_id: String::new(),
            source_media_retry_attempts: 0,
            source_timebase_ready: false,
            source_dock_keyboard_focus: false,
            panel_focus: PanelFocus::MediaPool,
            media_pool_grid_cols: 1,
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
        self.source_timebase_ready
            .then_some(self.selected_source_timebase)
            .and_then(BroadcastSourceTimebase::fps)
    }

    fn active_playback_source_ref(&self) -> Option<&BroadcastHostSourceRef> {
        match self.view_mode {
            ViewMode::Source => self.selected_source_ref.as_ref(),
            ViewMode::Wrap => None,
        }
    }

    fn playlist_input_available(&self) -> bool {
        EditorialPlaybackTransportComponent::playlist_input_available(self.playlist.as_ref())
    }

    fn active_playback_media_path(&self) -> Option<&str> {
        let path = match self.view_mode {
            ViewMode::Source if self.selected_play_input_clip_id == self.selected_clip_id => {
                self.selected_play_path.trim()
            }
            ViewMode::Source => "",
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
                .map(|(fps, _, _, _)| fps)
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

    fn clip_source_meta(&self, clip_id: &str) -> Option<(f64, bool, u8, i64)> {
        let clip_id = clip_id.trim();
        if clip_id.is_empty() {
            return None;
        }
        self.all_clips
            .iter()
            .chain(self.virtual_shots.iter())
            .chain(self.cover_shots.iter())
            .find(|shot| shot.clip_id == clip_id)
            .map(|shot| {
                (
                    shot.fps,
                    shot.has_audio,
                    shot.audio_channels,
                    shot.duration_frames,
                )
            })
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

    pub fn has_editorial_project(&self, project_id: &str) -> bool {
        !project_id.trim().is_empty() && self.loaded_project_id == project_id
    }

    pub fn begin_cached_meta_load(&mut self, project_id: &str) {
        self.begin_meta_load(project_id, 3);
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
        self.request_playlist_playback_inputs();
        self.finish_meta_result();
        let projection = self.wrap_projection_after_program_refresh(was_wrap);
        if projection != PlaybackTransportIntent::None {
            projection
        } else {
            self.playlist_input_preload_intent()
        }
    }

    fn wrap_projection_after_program_refresh(&mut self, was_wrap: bool) -> PlaybackTransportIntent {
        let current_frame = self.wrap_playhead_frame;
        let outcome = EditorialPlaybackTransportComponent::wrap_projection_after_program_refresh(
            EditorialWrapRefreshInput {
                meta_ready: self.meta_ready(),
                pending_part_id: self.pending_wrap_scrub_part_id.as_deref(),
                was_wrap,
                initial_selection_done: self.initial_selection_done,
                current_wrap_playhead_frame: current_frame,
                selected_part_id: &self.selected_part_id,
                playlist: self.playlist.as_ref(),
            },
        );
        self.apply_wrap_refresh_outcome(outcome, current_frame)
    }

    fn apply_wrap_refresh_outcome(
        &mut self,
        outcome: EditorialWrapRefreshOutcome,
        previous_wrap_playhead_frame: i64,
    ) -> PlaybackTransportIntent {
        if let Some(session) = outcome.session {
            self.apply_wrap_session_outcome(session);
            if outcome.preserve_current_playhead {
                self.set_wrap_playhead_frame(previous_wrap_playhead_frame);
            }
        }
        if outcome.clear_pending_part_id {
            self.pending_wrap_scrub_part_id = None;
        }
        PlaybackTransportIntent::None
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
        if self.initial_selection_done {
            return;
        }
        // Classic Story opens on source/All — not empty wrap.
        if let Some(first) = self.all_clips.first().cloned() {
            self.initial_selection_done = true;
            self.select_shot_from_snapshot(&first);
            return;
        }
        if self.meta_ready() {
            self.initial_selection_done = true;
            self.view_mode = ViewMode::Wrap;
            self.playing = false;
            self.status = "Wrap · broadcast".into();
        }
    }

    pub fn reset_session(&mut self, _host: &HostClient) {
        *self = Self::with_role(self.role);
    }

    pub fn suspend_playback_session(&mut self) {
        self.broadcast_preview_active = false;
        self.playing = false;
        self.source_media_retry_at = None;
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
        self.playlist_input_preload_intent()
    }

    fn start_wrap_session_from_snapshot(&mut self, selected_part_id: Option<String>) {
        let session =
            EditorialPlaybackTransportComponent::start_wrap_session(EditorialWrapSessionInput {
                selected_part_id: selected_part_id.as_deref(),
                current_wrap_playhead_frame: self.wrap_playhead_frame,
                playlist: self.playlist.as_ref(),
            });
        self.apply_wrap_session_outcome(session);
    }

    fn apply_wrap_session_outcome(&mut self, outcome: EditorialWrapSessionOutcome) {
        self.source_dock_keyboard_focus = false;
        self.panel_focus = PanelFocus::SegmentPanel;
        self.view_mode = ViewMode::Wrap;
        self.playing = false;
        if let Some(part_id) = outcome.selected_part_id {
            self.selected_part_id = part_id;
        }
        self.set_wrap_playhead_frame(outcome.wrap_playhead_frame);
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
        self.playlist_input_open_intent(self.wrap_playhead_frame)
    }

    fn defer_wrap_scrub_after_timeline(&mut self, preferred_part_id: Option<String>) {
        self.pending_wrap_scrub_part_id =
            EditorialPlaybackTransportComponent::pending_wrap_scrub_part_id(
                EditorialPendingWrapScrubInput {
                    preferred_part_id: preferred_part_id.as_deref(),
                    selected_part_id: &self.selected_part_id,
                    parts: &self.parts,
                },
            );
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
        let sync_ready_slot_id = self
            .sync_cover
            .ready_cover()
            .map(|ready| ready.slot_id.clone());
        let sync_holds_slot_selection = self.sync_cover.active().is_some()
            || self.sync_cover.pending_slot().is_some()
            || sync_ready_slot_id.is_some();
        let update = story_state::parse_state(state, self.timeline.as_ref());
        let active_thumb_ids = story_thumb_ids(
            &update.all_clips,
            &update.virtual_shots,
            &update.cover_shots,
        );
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
        self.cover_shots = update.cover_shots;
        self.ensure_media_pool_focus();
        self.covers = update.covers;
        self.markers = update.markers;
        self.marker_slots = update.marker_slots;
        if sync_holds_slot_selection {
            self.selected_cover_id.clear();
            self.selected_slot_id.clear();
            self.selected_marker_id.clear();
            if let Some(slot_id) = sync_ready_slot_id.filter(|slot_id| {
                self.marker_slots
                    .iter()
                    .any(|slot| slot.slot_id == *slot_id && !slot.has_cover)
            }) {
                self.selected_slot_id = slot_id;
            }
        } else {
            self.selected_cover_id = update.selected_cover_id;
            self.selected_slot_id = update.selected_slot_id;
        }
        self.draft_status = update.draft_status;
        self.story_summary = update.story_summary;
        self.thumb_textures
            .retain(|clip_id, _| active_thumb_ids.contains(clip_id.as_str()));
        self.thumbs_queued
            .retain(|clip_id| active_thumb_ids.contains(clip_id.as_str()));
        self.thumbs_queued.extend(thumbnail_queue);
        self.apply_pending_cover_projections();
    }

    fn add_pending_cover_projection(
        &mut self,
        slot_id: &str,
        source: &story_edit::CoverSourceRange,
    ) -> PlaybackTransportIntent {
        let Some(slot) = self
            .marker_slots
            .iter()
            .find(|slot| slot.slot_id == slot_id)
            .cloned()
        else {
            return PlaybackTransportIntent::None;
        };
        let timeline_start_frame = slot.start_frame.max(0);
        let timeline_end_frame = slot.end_frame.max(timeline_start_frame + 1);
        let timeline_fps = self.timeline_fps().unwrap_or(0.0);
        let timeline_start_sec = if slot.start_sec.is_finite() && slot.start_sec > 0.0 {
            slot.start_sec
        } else {
            frame_to_seconds(timeline_start_frame, timeline_fps)
        };
        let timeline_end_sec = if slot.end_sec.is_finite() && slot.end_sec > timeline_start_sec {
            slot.end_sec
        } else {
            frame_to_seconds(timeline_end_frame, timeline_fps)
        };
        let cover = StoryCover {
            cover_id: pending_cover_id(slot_id, &source.clip_id, source.in_frame, source.out_frame),
            slot_id: slot_id.to_string(),
            clip_id: source.clip_id.clone(),
            title: source.clip_id.clone(),
            timeline_start_sec,
            timeline_end_sec,
            timeline_start_frame,
            timeline_end_frame,
            source_in_frame: source.in_frame,
            source_out_frame: source.out_frame,
            source_fps: source.fps,
            source_timebase: EditorialSourceTimebase {
                fps_num: i64::from(source.source_timebase.fps_num),
                fps_den: i64::from(source.source_timebase.fps_den),
            },
            ..StoryCover::default()
        };
        self.pending_cover_projections
            .retain(|pending| pending.slot_id != slot_id);
        self.pending_cover_projections.push(cover);
        self.apply_pending_cover_projections();
        self.playlist_loaded = false;
        self.playlist_input_preload_intent()
    }

    fn apply_pending_cover_projections(&mut self) {
        if self.pending_cover_projections.is_empty() {
            return;
        }
        let pending = self.pending_cover_projections.clone();
        for cover in &pending {
            self.covers
                .retain(|existing| existing.slot_id != cover.slot_id);
            self.covers.push(cover.clone());
            for slot in &mut self.marker_slots {
                if slot.slot_id == cover.slot_id {
                    slot.has_cover = true;
                }
            }
        }
        if let Some(cover) = pending.last() {
            self.selected_cover_id = cover.cover_id.clone();
            self.selected_slot_id = cover.slot_id.clone();
        }
        self.refresh_story_summary();
    }

    fn remove_pending_cover_projection(&mut self, slot_id: &str) {
        let slot_id = slot_id.trim();
        if slot_id.is_empty() {
            self.pending_cover_projections.clear();
        } else {
            self.pending_cover_projections
                .retain(|pending| pending.slot_id != slot_id);
        }
    }

    fn clear_pending_cover_projections(&mut self) {
        self.pending_cover_projections.clear();
    }

    fn persisted_cover_id_for_slot(&self, slot_id: &str) -> Option<String> {
        let selected = self.selected_cover_id.trim();
        if !selected.is_empty() && !is_pending_cover_id(selected) {
            return Some(selected.to_string());
        }
        let slot_id = slot_id.trim();
        if slot_id.is_empty() {
            return None;
        }
        self.covers
            .iter()
            .find(|cover| cover.slot_id == slot_id && !is_pending_cover_id(&cover.cover_id))
            .map(|cover| cover.cover_id.clone())
    }

    fn remove_visible_cover(&mut self, cover_id: &str) {
        let cover_id = cover_id.trim();
        if cover_id.is_empty() {
            return;
        }
        let slot_id = self
            .covers
            .iter()
            .find(|cover| cover.cover_id == cover_id)
            .map(|cover| cover.slot_id.clone())
            .unwrap_or_default();
        self.covers.retain(|cover| cover.cover_id != cover_id);
        if self.selected_cover_id == cover_id {
            self.selected_cover_id.clear();
        }
        if !slot_id.trim().is_empty() {
            let has_cover = self.covers.iter().any(|cover| cover.slot_id == slot_id);
            for slot in &mut self.marker_slots {
                if slot.slot_id == slot_id {
                    slot.has_cover = has_cover;
                }
            }
        }
        self.refresh_story_summary();
    }

    fn restore_state_without_pending_cover(&mut self, slot_id: &str) {
        self.remove_pending_cover_projection(slot_id);
        if let Some(state) = self.story_state_snapshot.clone() {
            self.apply_story_state(&state);
            return;
        }
        let slot_id = slot_id.trim();
        if slot_id.is_empty() {
            return;
        }
        self.covers
            .retain(|cover| cover.slot_id != slot_id || !is_pending_cover_id(&cover.cover_id));
        let has_cover = self.covers.iter().any(|cover| cover.slot_id == slot_id);
        for slot in &mut self.marker_slots {
            if slot.slot_id == slot_id {
                slot.has_cover = has_cover;
            }
        }
        if is_pending_cover_id(&self.selected_cover_id) {
            self.selected_cover_id.clear();
        }
        self.refresh_story_summary();
    }

    fn enqueue_delete_cover(&mut self, cover_id: &str) {
        let cover_id = cover_id.trim();
        if cover_id.is_empty() {
            return;
        }
        let cover_id = cover_id.to_string();
        self.enqueue_edit_command(|instance, request, project| {
            EditorialEditComponent::delete_cover(instance, request, project, &cover_id)
        });
    }

    fn enqueue_object_undo(&mut self, object: &StoryObjectRef) {
        let object_type = object.object_type.clone();
        let object_id = object.object_id.clone();
        self.enqueue_edit_command(|instance, request, project| {
            EditorialEditComponent::undo_object(
                instance,
                request,
                project,
                &object_type,
                &object_id,
            )
        });
    }

    fn enqueue_object_redo(&mut self, object: &StoryObjectRef) {
        let object_type = object.object_type.clone();
        let object_id = object.object_id.clone();
        self.enqueue_edit_command(|instance, request, project| {
            EditorialEditComponent::redo_object(
                instance,
                request,
                project,
                &object_type,
                &object_id,
            )
        });
    }

    fn undo_selected_story_object(&mut self, _host: &HostClient) -> PlaybackTransportIntent {
        let selected_cover_id = self.selected_cover_id.trim().to_string();
        if is_pending_cover_id(&selected_cover_id) {
            let Some(pending) = self
                .pending_cover_projections
                .iter()
                .find(|pending| pending.cover_id == selected_cover_id)
                .cloned()
            else {
                self.status = "Pending pokrivalica više nije dostupna".into();
                return PlaybackTransportIntent::None;
            };
            self.pending_cover_undo_slots
                .insert(pending.slot_id.clone());
            self.restore_state_without_pending_cover(&pending.slot_id);
            self.playlist_loaded = false;
            self.status = "Undo pokrivalice · čekam potvrdu zapisa".into();
            return PlaybackTransportIntent::None;
        }

        if let Some(object) = StoryObjectRef::cover(&selected_cover_id) {
            self.enqueue_object_undo(&object);
            self.remove_visible_cover(&selected_cover_id);
            self.active_redo_object = Some(object);
            self.playlist_loaded = false;
            self.status = "Undo pokrivalice".into();
            return PlaybackTransportIntent::None;
        }

        self.status = "Odaberi Story objekt za undo".into();
        PlaybackTransportIntent::None
    }

    fn redo_selected_story_object(&mut self, _host: &HostClient) -> PlaybackTransportIntent {
        let object = self
            .active_redo_object
            .clone()
            .or_else(|| StoryObjectRef::cover(&self.selected_cover_id));
        let Some(object) = object else {
            self.status = "Odaberi Story objekt za redo".into();
            return PlaybackTransportIntent::None;
        };
        self.enqueue_object_redo(&object);
        self.status = "Redo Story objekta".into();
        PlaybackTransportIntent::None
    }

    fn refresh_story_summary(&mut self) {
        self.story_summary = story_state::summary(
            self.timeline.as_ref(),
            &self.parts,
            &self.all_clips,
            &self.virtual_shots,
            &self.cover_shots,
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
        let undo_created_cover = data.kind == EditorialEditKind::CreateCover
            && self.pending_cover_undo_slots.remove(data.detail.trim());
        if data.kind == EditorialEditKind::CreateCover {
            self.remove_pending_cover_projection(&data.detail);
        }
        self.story_state_snapshot = Some(data.state.clone());
        self.apply_story_state(&data.state);
        self.state_loaded = true;
        self.finish_meta_result();
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
                if self.sync_cover.pending_slot().is_some() {
                    return self
                        .try_select_pending_sync_slot()
                        .unwrap_or(PlaybackTransportIntent::None);
                }
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
                let cover_id = self.persisted_cover_id_for_slot(&data.detail);
                if undo_created_cover {
                    if let Some(cover_id) = cover_id {
                        if let Some(object) = StoryObjectRef::cover(&cover_id) {
                            self.enqueue_object_undo(&object);
                            self.active_redo_object = Some(object);
                        }
                        self.remove_visible_cover(&cover_id);
                        self.status = "Pokrivalica poništena".into();
                    } else {
                        self.status = "Pokrivalica poništena · cover_id nije vraćen".into();
                    }
                    return PlaybackTransportIntent::None;
                }
                self.status = "Cover kreiran".into();
                PlaybackTransportIntent::None
            }
            EditorialEditKind::DeleteCover => {
                self.status = format!("Cover obrisan {}", truncate(&data.detail, 24));
                PlaybackTransportIntent::None
            }
            EditorialEditKind::UndoObject => {
                self.status = format!("Undo {}", truncate(&data.detail, 24));
                PlaybackTransportIntent::None
            }
            EditorialEditKind::RedoObject => {
                self.active_redo_object = None;
                self.status = format!("Redo {}", truncate(&data.detail, 24));
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
        if _kind == EditorialEditKind::CreateCover {
            self.pending_cover_undo_slots.clear();
            self.clear_pending_cover_projections();
            if let Some(state) = self.story_state_snapshot.clone() {
                self.apply_story_state(&state);
            }
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
                    if self.select_shot_pending_resolved_metadata(shot) {
                        return;
                    }
                    self.status = err.message;
                    return;
                }
            };
        self.view_mode = ViewMode::Source;
        self.source_dock_keyboard_focus = true;
        self.panel_focus = PanelFocus::SourceTimeline;
        self.selected_shot_id = selection.shot_id.clone();
        self.focused_media_shot_id = selection.shot_id.clone();
        self.selected_clip_id = selection.clip_id.clone();
        self.source_in_frame = selection.shot_in_frame;
        self.source_out_frame = selection.shot_out_frame;
        self.selected_shot_in_frame = selection.shot_in_frame;
        self.selected_shot_out_frame = selection.shot_out_frame;
        self.mark_in_set = false;
        self.mark_out_set = false;
        let sync_source_end_frame = source_shot_end_frame(shot);
        let sync_selected_from_virtual_tab =
            matches!(self.library_tab, LibraryTab::Virtual | LibraryTab::Cover);
        self.focus.clear();
        self.selected_source_ref = Some(selection.source_ref);
        self.selected_source_timebase = BroadcastSourceTimebase::from_i64(
            shot.source_timebase.fps_num,
            shot.source_timebase.fps_den,
        )
        .unwrap_or_default();
        self.selected_source_fps = self.selected_source_timebase.fps().unwrap_or(0.0);
        self.selected_source_has_audio = shot.has_audio;
        self.selected_source_audio_channels = shot.audio_channels;
        self.source_timebase_ready = self.selected_source_timebase.is_valid();
        self.set_source_playhead_frame(selection.shot_in_frame);
        SyncCoverCaptureComponent::auto_arm_source_selection(
            &mut self.sync_cover,
            &selection.clip_id,
            selection.shot_in_frame,
            sync_source_end_frame,
            sync_selected_from_virtual_tab,
        );
        if self.waveform_clip_id != selection.clip_id {
            self.a1_peaks.clear();
            self.a2_peaks.clear();
            self.film_frames.clear();
            self.filmstrip_manifest_ready = false;
            self.waveform_clip_id.clear();
            self.clear_source_media_retry();
            self.filmstrip_image_loader.cancel_pending();
        }

        if self.selected_play_input_clip_id != selection.clip_id {
            self.selected_play_path.clear();
            self.selected_play_input_clip_id.clear();
        }
        self.request_playback_input_for_selected_clip();
    }

    fn select_shot_pending_resolved_metadata(&mut self, shot: &StoryShot) -> bool {
        let clip_id = shot.clip_id.trim();
        if clip_id.is_empty() {
            return false;
        }
        self.view_mode = ViewMode::Source;
        self.source_dock_keyboard_focus = true;
        self.panel_focus = PanelFocus::SourceTimeline;
        self.selected_shot_id = shot_id(shot);
        self.focused_media_shot_id = self.selected_shot_id.clone();
        self.selected_clip_id = clip_id.to_string();
        let in_frame = shot.in_frame.max(0);
        let out_frame = source_shot_end_frame(shot).max(in_frame + 1);
        self.source_in_frame = in_frame;
        self.source_out_frame = out_frame;
        self.selected_shot_in_frame = in_frame;
        self.selected_shot_out_frame = out_frame;
        self.mark_in_set = false;
        self.mark_out_set = false;
        let sync_selected_from_virtual_tab =
            matches!(self.library_tab, LibraryTab::Virtual | LibraryTab::Cover);
        self.focus.clear();
        self.selected_source_ref = None;
        self.selected_source_timebase = BroadcastSourceTimebase::default();
        self.selected_source_fps = 0.0;
        self.selected_source_has_audio = shot.has_audio;
        self.selected_source_audio_channels = shot.audio_channels;
        self.source_timebase_ready = false;
        self.set_source_playhead_frame(in_frame);
        SyncCoverCaptureComponent::auto_arm_source_selection(
            &mut self.sync_cover,
            clip_id,
            in_frame,
            out_frame,
            sync_selected_from_virtual_tab,
        );
        if self.waveform_clip_id != clip_id {
            self.a1_peaks.clear();
            self.a2_peaks.clear();
            self.film_frames.clear();
            self.filmstrip_manifest_ready = false;
            self.waveform_clip_id.clear();
            self.clear_source_media_retry();
            self.filmstrip_image_loader.cancel_pending();
        }
        if self.selected_play_input_clip_id != clip_id {
            self.selected_play_path.clear();
            self.selected_play_input_clip_id.clear();
        }
        self.request_playback_input_for_selected_clip();
        true
    }

    fn select_shot(&mut self, host: &HostClient, shot: &StoryShot) -> PlaybackTransportIntent {
        self.select_shot_from_snapshot(shot);
        if !self.selected_clip_id.trim().is_empty() {
            self.request_source_media(host);
        }
        self.cue_current_playhead_intent()
    }

    fn request_playback_input_for_selected_clip(&mut self) {
        let project_id = self.project_id.trim().to_string();
        let clip_id = self.selected_clip_id.trim().to_string();
        if project_id.is_empty() || clip_id.is_empty() {
            self.selected_play_path.clear();
            self.selected_play_input_clip_id.clear();
            self.pending_play_input_clip_id.clear();
            self.status = "Nema odabranog source klipa".into();
            return;
        }
        if self.selected_play_input_clip_id == clip_id && !self.selected_play_path.trim().is_empty()
        {
            self.status = format!("Source · {clip_id}");
            return;
        }
        if self.pending_play_input_clip_id == clip_id {
            self.status = format!("Source · {clip_id} (media resolve)");
            return;
        }
        self.pending_play_input_clip_id = clip_id.clone();
        let instance_id = self.edit_instance_id();
        self.enqueue_backend_command(PlaybackMediaResolverComponent::resolve_playback_proxy(
            instance_id,
            &project_id,
            &clip_id,
        ));
        self.status = format!("Source · {clip_id} (media resolve)");
    }

    fn request_playlist_playback_inputs(&mut self) -> bool {
        let project_id = self.project_id.trim().to_string();
        if project_id.is_empty() || !self.playlist_input_available() {
            return false;
        }
        let clip_ids = {
            let input = self.playlist_program_input(self.wrap_playhead_frame);
            EditorialPlaybackTransportComponent::required_playlist_playback_clip_ids(input)
        };
        if clip_ids.is_empty() {
            return false;
        }

        let mut all_ready = true;
        for clip_id in clip_ids {
            if self
                .playlist_playback_inputs
                .get(&clip_id)
                .is_some_and(|value| !value.trim().is_empty())
            {
                continue;
            }
            all_ready = false;
            if self
                .pending_playlist_playback_input_clip_ids
                .insert(clip_id.clone())
            {
                let instance_id = self.edit_instance_id();
                self.enqueue_backend_command(
                    PlaybackMediaResolverComponent::resolve_playback_proxy(
                        instance_id,
                        &project_id,
                        &clip_id,
                    ),
                );
            }
        }

        if !all_ready {
            self.status = "Playlist input · čekam media resolve".into();
        }
        all_ready
    }

    pub fn apply_playback_media_resolution(
        &mut self,
        project_id: &str,
        clip_id: &str,
        resolution: PlaybackMediaResolution,
    ) -> PlaybackTransportIntent {
        if self.loaded_project_id != project_id {
            return PlaybackTransportIntent::None;
        }
        let media_input = resolution.media_input.trim().to_string();
        if !media_input.is_empty() {
            self.playlist_playback_inputs
                .insert(clip_id.to_string(), media_input.clone());
        }
        self.pending_playlist_playback_input_clip_ids
            .remove(clip_id);
        if self.selected_clip_id != clip_id {
            return PlaybackTransportIntent::None;
        }
        self.merge_resolved_playback_metadata_into_source(clip_id, &resolution);
        if self.pending_play_input_clip_id == clip_id {
            self.pending_play_input_clip_id.clear();
        }
        self.selected_play_path = media_input;
        self.selected_play_input_clip_id = clip_id.to_string();
        self.status = format!("Source · {clip_id} ({})", resolution.locator_kind);
        self.cue_current_playhead_intent()
    }

    fn merge_resolved_playback_metadata_into_source(
        &mut self,
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
        let editorial_timebase = EditorialSourceTimebase {
            fps_num: i64::from(source_timebase.fps_num),
            fps_den: i64::from(source_timebase.fps_den),
        };
        let has_audio = resolution.has_audio;
        let audio_channels = resolution.audio_channels;

        for shot in &mut self.all_clips {
            if shot.clip_id == clip_id {
                apply_resolved_media_to_story_shot(
                    shot,
                    source_fps,
                    editorial_timebase,
                    Some(duration_frames),
                    Some(duration_sec),
                    has_audio,
                    audio_channels,
                );
            }
        }
        for shot in &mut self.virtual_shots {
            if shot.clip_id == clip_id {
                apply_resolved_media_to_story_shot(
                    shot,
                    source_fps,
                    editorial_timebase,
                    None,
                    None,
                    has_audio,
                    audio_channels,
                );
            }
        }

        self.selected_source_fps = source_fps;
        self.selected_source_timebase = source_timebase;
        self.source_timebase_ready = true;
        if let Some(has_audio) = has_audio {
            self.selected_source_has_audio = has_audio;
        }
        if let Some(audio_channels) = audio_channels.filter(|channels| *channels > 0) {
            self.selected_source_audio_channels = audio_channels;
        }

        let selected = self
            .all_clips
            .iter()
            .chain(self.virtual_shots.iter())
            .chain(self.cover_shots.iter())
            .find(|shot| shot.clip_id == clip_id && shot_id(shot) == self.selected_shot_id)
            .or_else(|| {
                self.all_clips
                    .iter()
                    .chain(self.virtual_shots.iter())
                    .chain(self.cover_shots.iter())
                    .find(|shot| shot.clip_id == clip_id)
            });
        let source_id = selected
            .map(|shot| shot_id(shot))
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| clip_id.to_string());
        let root_id = selected
            .map(|shot| shot.root_shot_id.trim().to_string())
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| source_id.clone());
        let in_frame = selected
            .map(|shot| shot.in_frame.max(0))
            .unwrap_or_else(|| self.selected_shot_in_frame.max(0));
        let out_frame = selected
            .map(source_shot_end_frame)
            .unwrap_or(self.selected_shot_out_frame)
            .max(in_frame + 1)
            .min(duration_frames);
        self.source_in_frame = in_frame;
        self.source_out_frame = out_frame;
        self.selected_shot_in_frame = in_frame;
        self.selected_shot_out_frame = out_frame;
        self.source_playhead_frame = self.source_playhead_frame.clamp(in_frame, out_frame);
        self.selected_source_ref = BroadcastHostSourceRef::from_frame_fields(
            &self.project_id,
            &source_id,
            &root_id,
            clip_id,
            Some(FrameNumber(0)),
            Some(FrameNumber(duration_frames)),
            FrameNumber(duration_frames),
        )
        .ok();
    }

    pub fn set_playback_media_resolution_error(
        &mut self,
        project_id: &str,
        clip_id: &str,
        error: impl Into<String>,
    ) {
        if self.loaded_project_id != project_id {
            return;
        }
        self.pending_playlist_playback_input_clip_ids
            .remove(clip_id);
        if self.selected_clip_id != clip_id {
            self.status = error.into();
            return;
        }
        if self.pending_play_input_clip_id == clip_id {
            self.pending_play_input_clip_id.clear();
        }
        self.selected_play_path.clear();
        self.selected_play_input_clip_id.clear();
        self.status = error.into();
    }

    fn request_source_media(&mut self, host: &HostClient) {
        let clip = self.selected_clip_id.clone();
        let clip = clip.trim();
        if clip.is_empty() {
            self.a1_peaks.clear();
            self.a2_peaks.clear();
            self.film_frames.clear();
            self.filmstrip_manifest_ready = false;
            self.waveform_clip_id.clear();
            self.clear_source_media_retry();
            return;
        }
        if self.source_media_request_blocked(clip) {
            return;
        }
        if self.source_media_loader.request(
            host,
            self.project_id.clone(),
            clip.to_string(),
            self.source_filmstrip_ready(),
            self.repaint_ctx.clone(),
        ) {
            self.source_media_retry_at = None;
            self.status = format!("Učitavam source · {clip}");
        }
    }

    fn source_media_request_blocked(&self, clip: &str) -> bool {
        if self.source_media_retry_at.is_some() {
            return true;
        }
        if self.source_media_retry_clip_id == clip
            && self.source_media_retry_attempts >= SOURCE_MEDIA_MAX_RETRIES
        {
            return true;
        }
        self.source_media_ready_for_clip(clip)
    }

    fn source_media_ready_for_clip(&self, clip: &str) -> bool {
        if clip != self.waveform_clip_id {
            return false;
        }
        let waveform_ready = if self.selected_source_has_audio {
            !self.a1_peaks.is_empty() || !self.a2_peaks.is_empty()
        } else {
            true
        };
        waveform_ready && self.source_filmstrip_ready()
    }

    fn source_filmstrip_ready(&self) -> bool {
        self.filmstrip_manifest_ready
            && !self.film_frames.is_empty()
            && self
                .film_frames
                .iter()
                .all(|frame| filmstrip_frame_url_ready(&frame.url))
    }

    fn clear_source_media_retry(&mut self) {
        self.source_media_retry_at = None;
        self.source_media_retry_clip_id.clear();
        self.source_media_retry_attempts = 0;
    }

    fn schedule_source_media_retry(&mut self, clip_id: &str, status: String) {
        if self.source_media_retry_clip_id != clip_id {
            self.source_media_retry_clip_id = clip_id.to_string();
            self.source_media_retry_attempts = 0;
        }
        self.source_media_retry_attempts = self.source_media_retry_attempts.saturating_add(1);
        if self.source_media_retry_attempts >= SOURCE_MEDIA_MAX_RETRIES {
            self.source_media_retry_at = None;
            self.status = format!(
                "{status} · pokušaji {}/{}",
                self.source_media_retry_attempts, SOURCE_MEDIA_MAX_RETRIES
            );
        } else {
            self.source_media_retry_at = Some(Instant::now() + SOURCE_MEDIA_RETRY_DELAY);
            self.status = format!(
                "{status} · pokušaj {}/{}",
                self.source_media_retry_attempts, SOURCE_MEDIA_MAX_RETRIES
            );
        }
    }

    fn poll_media_assets(&mut self, _host: &HostClient, ctx: &egui::Context) {
        self.program_waveforms.poll(&self.loaded_project_id, ctx);

        for result in self.filmstrip_image_loader.poll() {
            match result.key.scope.as_str() {
                "editorial.film" => {
                    let clip_id = result.key.item_id;
                    if clip_id != self.selected_clip_id {
                        continue;
                    }
                    let index = result.key.variant.parse::<i64>().unwrap_or(0);
                    if let Some(frame) = self
                        .film_frames
                        .iter_mut()
                        .find(|frame| frame.index == index)
                    {
                        match result.image {
                            Ok(color) => {
                                frame.texture = Some(ctx.load_texture(
                                    format!("qnc_tl_frame_{clip_id}_{index}"),
                                    color,
                                    TextureOptions::LINEAR,
                                ));
                                frame.load_attempts = 0;
                            }
                            Err(_) => {
                                frame.load_attempts = frame.load_attempts.saturating_add(1);
                            }
                        }
                    }
                }
                _ => {}
            }
        }

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
                _ => {}
            }
        }

        for result in self.source_media_loader.poll() {
            let result_clip_id = result.clip_id;
            if result_clip_id != self.selected_clip_id {
                continue;
            }
            match result.media {
                Ok(media) => {
                    self.waveform_clip_id = media.clip_id;
                    if media.waveform_loaded {
                        self.a1_peaks = media.a1_peaks;
                        self.a2_peaks = media.a2_peaks;
                    }
                    self.filmstrip_manifest_ready = media.filmstrip_ready;
                    let media_clip_id = self.waveform_clip_id.clone();
                    let frames = media.film_frames.into_iter().map(|frame| FilmFrame {
                        index: frame.index,
                        seek_sec: frame.seek_sec,
                        url: frame.url,
                        texture: None,
                        load_attempts: 0,
                    });
                    crate::qnc_filmstrip_background::merge_frames(&mut self.film_frames, frames);
                    if !self.source_filmstrip_ready() {
                        self.schedule_source_media_retry(
                            &media_clip_id,
                            format!("Filmstrip se gradi · {}", self.selected_clip_id),
                        );
                    } else if self.selected_source_has_audio
                        && self.a1_peaks.is_empty()
                        && self.a2_peaks.is_empty()
                    {
                        self.schedule_source_media_retry(
                            &media_clip_id,
                            format!("Waveform se gradi · {}", self.selected_clip_id),
                        );
                    } else {
                        self.clear_source_media_retry();
                        self.status = format!("Source spreman · {}", self.selected_clip_id);
                    }
                    ctx.request_repaint();
                }
                Err(e) => {
                    self.schedule_source_media_retry(&result_clip_id, e);
                    ctx.request_repaint();
                }
            }
        }
    }

    fn pump_film_frames(&mut self, ctx: &egui::Context) {
        for frame in &mut self.film_frames {
            if self.filmstrip_image_loader.is_saturated() {
                break;
            }
            if frame.texture.is_some()
                || frame.url.is_empty()
                || !filmstrip_frame_url_ready(&frame.url)
                || frame.load_attempts >= FILM_FRAME_MAX_LOAD_ATTEMPTS
            {
                continue;
            }
            let _ = self.filmstrip_image_loader.request(
                ImageAssetKey::new(
                    "editorial.film",
                    self.selected_clip_id.clone(),
                    frame.index.to_string(),
                ),
                frame.url.clone(),
                Some(ctx.clone()),
            );
        }
    }

    fn pump_thumbs(&mut self, host: &HostClient, ctx: &egui::Context) {
        if self.image_loader.is_saturated() {
            return;
        }
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
        self.pump_film_frames(ctx);
        self.pump_thumbs(host, ctx);
        let _ = host;
        if self.film_frames.iter().any(|f| {
            f.texture.is_none()
                && !f.url.is_empty()
                && f.load_attempts < FILM_FRAME_MAX_LOAD_ATTEMPTS
        }) && !self.filmstrip_image_loader.is_saturated()
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
        if self.sync_cover.active().is_some() || self.sync_cover_should_start_on_space() {
            return self
                .sync_cover_toggle_play_intent(playlist_input_active, playlist_input_playing);
        }
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
                if let Some(session) = self.sync_cover.active() {
                    let source_frame =
                        SyncCoverCaptureComponent::source_frame_at_program_frame(session, frame);
                    self.set_source_playhead_frame(source_frame);
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
        let selected_clip_id = self.selected_clip_id.clone();
        if self.source_media_retry_at.is_none()
            && !selected_clip_id.trim().is_empty()
            && !self.source_media_ready_for_clip(&selected_clip_id)
        {
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
            self.panel_focus = PanelFocus::SourceTimeline;
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
            .chain(self.cover_shots.iter())
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
                let head = self.role.head();
                // Honor composition: no Segment/Cover tab → clamp away.
                let tab = if (tab == LibraryTab::Segment && !head.show_segment_tab)
                    || (tab == LibraryTab::Cover && !head.show_cover_tab)
                {
                    LibraryTab::All
                } else {
                    tab
                };
                self.library_tab = tab;
                self.panel_focus = PanelFocus::MediaPool;
                self.ensure_media_pool_focus();
                if tab == LibraryTab::Segment {
                    self.start_wrap_session(host)
                } else {
                    PlaybackTransportIntent::None
                }
            }
            media_pool::MediaPoolAction::SelectShot(shot) => self.select_shot(host, &shot),
            media_pool::MediaPoolAction::ToggleShotSelection(shot) => self.select_shot(host, &shot),
            media_pool::MediaPoolAction::SelectPart(part_id) => {
                self.panel_focus = PanelFocus::SegmentPanel;
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
            media_pool::MediaPoolAction::SelectClipId(_)
            | media_pool::MediaPoolAction::ToggleClipSelection(_) => PlaybackTransportIntent::None,
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
        self.ensure_media_pool_focus();
        self.media_pool_grid_cols = crate::qnc_media_card::grid_metrics(
            ui.available_width().max(crate::qnc_media_card::MIN_CARD_W),
            self.current_media_pool_shots().len(),
        )
        .cols
        .max(1);
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
                focused_shot_id: &self.focused_media_shot_id,
                panel_focused: self.panel_focus == PanelFocus::MediaPool,
                selected_clip_id: &self.selected_clip_id,
                all_clips: &self.all_clips,
                virtual_shots: &self.virtual_shots,
                cover_shots: &self.cover_shots,
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
        self.program_waveforms.request_for_program(
            host,
            &self.loaded_project_id,
            &program,
            ui.ctx(),
        );
        let source_durations = program_waveform::source_duration_frames(
            &self.all_clips,
            &self.virtual_shots,
            &self.cover_shots,
        );
        let program_waveform = self.program_waveforms.compose(&program, &source_durations);
        let display_frame =
            playback.playlist_display_frame(self.wrap_playhead_frame, program.duration_frames());
        let tc = |sec| self.tc(sec);
        let playhead_sec = self.timeline_sec_from_frame(display_frame).unwrap_or(0.0);
        let action = segment_panel::show(
            ui,
            segment_panel::SegmentPanelInput {
                height,
                virtual_frame: display_frame,
                playhead_sec,
                program: &program,
                covers: &self.covers,
                markers: &self.markers,
                a1_peaks: &program_waveform.a1_peaks,
                a2_peaks: &program_waveform.a2_peaks,
                selected_slot_id: &self.selected_slot_id,
                selected_cover_id: &self.selected_cover_id,
                sync_cover_enabled: self.sync_cover.enabled(),
                tc: &tc,
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
            marker_cover_panel::MarkerCoverAction::PlaylistStart => self.jump_playlist_start(host),
            marker_cover_panel::MarkerCoverAction::PreviousSegment => {
                self.select_adjacent_part(host, -1)
            }
            marker_cover_panel::MarkerCoverAction::PreviousMarkerSlot => {
                self.select_adjacent_marker_slot(host, -1)
            }
            marker_cover_panel::MarkerCoverAction::PreviousMarker => {
                self.select_adjacent_marker(host, -1)
            }
            marker_cover_panel::MarkerCoverAction::NextMarker => {
                self.select_adjacent_marker(host, 1)
            }
            marker_cover_panel::MarkerCoverAction::NextMarkerSlot => {
                self.select_adjacent_marker_slot(host, 1)
            }
            marker_cover_panel::MarkerCoverAction::NextSegment => {
                self.select_adjacent_part(host, 1)
            }
            marker_cover_panel::MarkerCoverAction::AddMarker => {
                self.marker_at_head(host);
                PlaybackTransportIntent::None
            }
            marker_cover_panel::MarkerCoverAction::CreateCover => self.quick_cover(host),
            marker_cover_panel::MarkerCoverAction::OverwriteCover => self.overwrite_cover(host),
            marker_cover_panel::MarkerCoverAction::ToggleSyncCover => {
                self.toggle_sync_cover_capture()
            }
        }
    }

    fn dispatch_segment_panel(
        &mut self,
        host: &HostClient,
        action: segment_panel::SegmentPanelAction,
    ) -> PlaybackTransportIntent {
        if !matches!(action, segment_panel::SegmentPanelAction::None) {
            self.panel_focus = PanelFocus::SegmentPanel;
            self.source_dock_keyboard_focus = false;
        }
        match action {
            segment_panel::SegmentPanelAction::None => PlaybackTransportIntent::None,
            segment_panel::SegmentPanelAction::SeekTimelineFrame(frame) => {
                self.set_wrap_playhead_frame(frame);
                self.ensure_wrap_or_scrub(host)
            }
            segment_panel::SegmentPanelAction::MarkerCover(action) => {
                self.dispatch_marker_cover_action(host, action)
            }
            segment_panel::SegmentPanelAction::SelectMarkerSlot { slot_id, frame } => {
                self.set_wrap_playhead_frame(frame);
                self.select_marker_slot(host, &slot_id);
                self.ensure_wrap_or_scrub(host)
            }
            segment_panel::SegmentPanelAction::SelectCover { cover_id, frame } => {
                self.set_wrap_playhead_frame(frame);
                self.select_cover(host, &cover_id);
                self.ensure_wrap_or_scrub(host)
            }
            segment_panel::SegmentPanelAction::SelectMarker { marker_id, frame } => {
                self.select_marker(host, &marker_id, frame)
            }
        }
    }

    fn ensure_wrap_or_scrub(&mut self, _host: &HostClient) -> PlaybackTransportIntent {
        if self.view_mode != ViewMode::Wrap {
            self.start_wrap_session_from_snapshot(None);
            self.playlist_input_open_intent(self.wrap_playhead_frame)
        } else {
            self.scrub_soft(_host)
        }
    }

    fn editorial_playback_view(&self) -> EditorialPlaybackView {
        match self.view_mode {
            ViewMode::Source => EditorialPlaybackView::Source,
            ViewMode::Wrap => EditorialPlaybackView::Wrap,
        }
    }

    fn apply_editorial_toggle_play_outcome(
        &mut self,
        outcome: EditorialTogglePlayOutcome,
    ) -> PlaybackTransportIntent {
        if let Some(view_mode) = outcome.view_mode {
            self.view_mode = match view_mode {
                EditorialPlaybackView::Source => ViewMode::Source,
                EditorialPlaybackView::Wrap => ViewMode::Wrap,
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
        if !self.source_dock_keyboard_focus
            && self.view_mode == ViewMode::Wrap
            && !self.playing
            && !playlist_input_active
            && !playlist_input_playing
            && self.playlist_input_available()
            && !self.request_playlist_playback_inputs()
        {
            return PlaybackTransportIntent::None;
        }
        let playlist_program = self.playlist_program_input(self.wrap_playhead_frame);
        let outcome = EditorialPlaybackTransportComponent::toggle_play(EditorialTogglePlayInput {
            source_dock_keyboard_focus: self.source_dock_keyboard_focus,
            view_mode: self.editorial_playback_view(),
            story_playing: self.playing,
            playlist_input_active,
            playlist_input_playing,
            playlist_program,
        });
        self.apply_editorial_toggle_play_outcome(outcome)
    }

    pub(crate) fn playlist_input_preload_intent(&mut self) -> PlaybackTransportIntent {
        if self.sync_cover.active().is_some() || self.sync_cover.pending_slot().is_some() {
            return PlaybackTransportIntent::None;
        }
        if !self.playlist_input_available() {
            return PlaybackTransportIntent::None;
        }
        if !self.request_playlist_playback_inputs() {
            return PlaybackTransportIntent::None;
        }
        self.playlist_input_request_intent(self.wrap_playhead_frame, true)
    }

    pub(crate) fn playlist_input_open_intent(
        &mut self,
        start_program_frame: i64,
    ) -> PlaybackTransportIntent {
        if !self.playlist_input_available() {
            return PlaybackTransportIntent::None;
        }
        if !self.request_playlist_playback_inputs() {
            return PlaybackTransportIntent::None;
        }
        self.playlist_input_request_intent(start_program_frame, false)
    }

    fn playlist_input_request_intent(
        &self,
        start_program_frame: i64,
        preload: bool,
    ) -> PlaybackTransportIntent {
        EditorialPlaybackTransportComponent::playlist_input_intent(
            self.playlist_program_input(start_program_frame),
            preload,
        )
    }

    fn playlist_program_input(
        &self,
        start_program_frame: i64,
    ) -> EditorialPlaylistProgramInput<'_> {
        EditorialPlaylistProgramInput {
            project_id: &self.project_id,
            program_id: self.edit_instance_id(),
            start_program_frame,
            playlist: self.playlist.as_ref(),
            marker_slots: &self.marker_slots,
            covers: &self.covers,
            markers: &self.markers,
            all_clips: &self.all_clips,
            virtual_shots: &self.virtual_shots,
            cover_shots: &self.cover_shots,
            playback_inputs: &self.playlist_playback_inputs,
        }
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

fn story_thumb_ids(
    all_clips: &[StoryShot],
    virtual_shots: &[StoryShot],
    cover_shots: &[StoryShot],
) -> HashSet<String> {
    all_clips
        .iter()
        .chain(virtual_shots.iter())
        .chain(cover_shots.iter())
        .filter_map(|shot| {
            let clip_id = shot.clip_id.trim();
            (!clip_id.is_empty()).then(|| clip_id.to_string())
        })
        .collect()
}

fn filmstrip_frame_url_ready(url: &str) -> bool {
    let url = url.trim();
    !url.is_empty() && !url.contains("/filmstrip/placeholder")
}

fn pending_cover_id(slot_id: &str, clip_id: &str, in_frame: i64, out_frame: i64) -> String {
    format!(
        "pending-cover:{}:{}:{}:{}",
        slot_id.trim(),
        clip_id.trim(),
        in_frame.max(0),
        out_frame.max(in_frame + 1)
    )
}

fn is_pending_cover_id(cover_id: &str) -> bool {
    cover_id.trim().starts_with("pending-cover:")
}

fn source_shot_end_frame(shot: &StoryShot) -> i64 {
    let start = shot.in_frame.max(0);
    if shot.out_frame > start {
        shot.out_frame
    } else if shot.duration_frames > 0 {
        start + shot.duration_frames
    } else {
        start + 1
    }
}

fn apply_resolved_media_to_story_shot(
    shot: &mut StoryShot,
    source_fps: f64,
    source_timebase: EditorialSourceTimebase,
    source_duration_frames: Option<i64>,
    source_duration_sec: Option<f64>,
    has_audio: Option<bool>,
    audio_channels: Option<u8>,
) {
    shot.fps = source_fps;
    shot.source_timebase = source_timebase;
    if let Some(duration_frames) = source_duration_frames.filter(|frames| *frames > 0) {
        shot.duration_frames = duration_frames;
        if shot.out_frame <= shot.in_frame.max(0) {
            shot.out_frame = duration_frames;
        }
    }
    if let Some(duration_sec) =
        source_duration_sec.filter(|duration| duration.is_finite() && *duration > 0.0)
    {
        shot.duration_sec = duration_sec;
    }
    if let Some(has_audio) = has_audio {
        shot.has_audio = has_audio;
    }
    if let Some(audio_channels) = audio_channels.filter(|channels| *channels > 0) {
        shot.audio_channels = audio_channels;
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
            playback_controls::PlaybackAction::MarkOut => self.mark_out_or_sync_end(host),
            playback_controls::PlaybackAction::SelectMarkIn => self.select_mark_in(host),
            playback_controls::PlaybackAction::SelectMarkOut => self.select_mark_out(host),
            playback_controls::PlaybackAction::FocusNext => self.focus_adjacent_panel(host, 1),
            playback_controls::PlaybackAction::FocusPrev => self.focus_adjacent_panel(host, -1),
            playback_controls::PlaybackAction::ActivateFocusedItem => {
                self.sync_cover_enter_or_activate_focused_item(host)
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
            playback_controls::PlaybackAction::NavigatePrevObject => {
                self.navigate_adjacent_object(host, -1)
            }
            playback_controls::PlaybackAction::NavigateNextObject => {
                self.navigate_adjacent_object(host, 1)
            }
            playback_controls::PlaybackAction::StepPrevPart => self.step_adjacent_segment(host, -1),
            playback_controls::PlaybackAction::StepNextPart => self.step_adjacent_segment(host, 1),
            playback_controls::PlaybackAction::StepPrevMarkerSlot => {
                self.select_adjacent_marker_slot(host, -1)
            }
            playback_controls::PlaybackAction::StepNextMarkerSlot => {
                self.select_adjacent_marker_slot(host, 1)
            }
            playback_controls::PlaybackAction::SelectCurrentMarkerSlot => {
                self.select_current_marker_slot(host)
            }
            playback_controls::PlaybackAction::FocusEmptySlot => self.focus_empty_marker_slot(host),
            playback_controls::PlaybackAction::MarkInFitDuration => self.mark_in_fit_duration(host),
            playback_controls::PlaybackAction::DeleteSelection => {
                self.delete_selected_timeline_item(host);
                PlaybackTransportIntent::None
            }
            playback_controls::PlaybackAction::UndoObject => self.undo_selected_story_object(host),
            playback_controls::PlaybackAction::RedoObject => self.redo_selected_story_object(host),
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

    fn has_segment_panel(&self) -> bool {
        matches!(
            self.role.composition().right,
            crate::composition::RightPanelKind::SegmentPanel
        )
    }

    fn focus_adjacent_panel(
        &mut self,
        host: &HostClient,
        direction: i32,
    ) -> PlaybackTransportIntent {
        let next = next_panel_focus(self.panel_focus, self.has_segment_panel(), direction);
        self.activate_panel_focus(host, next)
    }

    fn activate_panel_focus(
        &mut self,
        _host: &HostClient,
        panel: PanelFocus,
    ) -> PlaybackTransportIntent {
        self.panel_focus = panel;
        match panel {
            PanelFocus::MediaPool => {
                self.source_dock_keyboard_focus = false;
                self.ensure_media_pool_focus();
                self.status = format!(
                    "Fokus panel: {} · {}",
                    self.media_pool_focus_label(),
                    self.chord_or("activate_focused_item", "Enter")
                );
                PlaybackTransportIntent::None
            }
            PanelFocus::SegmentPanel => {
                self.source_dock_keyboard_focus = false;
                self.view_mode = ViewMode::Wrap;
                self.status = format!("Fokus panel: {}", panel.label());
                self.playlist_input_preload_intent()
            }
            PanelFocus::SourceTimeline => {
                self.source_dock_keyboard_focus = true;
                self.view_mode = ViewMode::Source;
                self.status = format!("Fokus panel: {}", panel.label());
                PlaybackTransportIntent::None
            }
        }
    }

    fn current_media_pool_shots(&self) -> &[StoryShot] {
        match self.library_tab {
            LibraryTab::All => &self.all_clips,
            LibraryTab::Virtual => &self.virtual_shots,
            LibraryTab::Cover => &self.cover_shots,
            LibraryTab::Segment => &[],
        }
    }

    fn ensure_media_pool_focus(&mut self) {
        if self.library_tab == LibraryTab::Segment {
            self.focused_media_shot_id.clear();
            return;
        }
        let next_focus = {
            let shots = self.current_media_pool_shots();
            if shots.is_empty() {
                String::new()
            } else if shots
                .iter()
                .any(|shot| shot_id(shot) == self.focused_media_shot_id)
            {
                self.focused_media_shot_id.clone()
            } else {
                let selected = self.selected_shot_id.trim();
                if !selected.is_empty() && shots.iter().any(|shot| shot_id(shot) == selected) {
                    selected.to_string()
                } else {
                    shot_id(&shots[0])
                }
            }
        };
        self.focused_media_shot_id = next_focus;
    }

    fn media_pool_focus_label(&self) -> &'static str {
        match self.library_tab {
            LibraryTab::All => "All",
            LibraryTab::Virtual => "Virtual",
            LibraryTab::Cover => "B-roll",
            LibraryTab::Segment => "Segment",
        }
    }

    fn move_media_pool_focus(&mut self, host: &HostClient, delta: i32) -> PlaybackTransportIntent {
        self.panel_focus = PanelFocus::MediaPool;
        self.source_dock_keyboard_focus = false;
        if self.library_tab == LibraryTab::Segment {
            return self.select_adjacent_part(host, delta.signum());
        }
        self.ensure_media_pool_focus();
        let (next_id, title) = {
            let shots = self.current_media_pool_shots();
            if shots.is_empty() {
                (String::new(), String::new())
            } else {
                let current = shots
                    .iter()
                    .position(|shot| shot_id(shot) == self.focused_media_shot_id)
                    .unwrap_or(0);
                let last = shots.len().saturating_sub(1);
                let next = (current as i32 + delta).clamp(0, last as i32) as usize;
                let shot = &shots[next];
                let title = if !shot.name.is_empty() {
                    shot.name.clone()
                } else if !shot.virtual_name.is_empty() {
                    shot.virtual_name.clone()
                } else if !shot.clip_id.is_empty() {
                    shot.clip_id.clone()
                } else {
                    shot_id(shot)
                };
                (shot_id(shot), title)
            }
        };
        if next_id.is_empty() {
            self.status = format!("{} panel je prazan", self.media_pool_focus_label());
            return PlaybackTransportIntent::None;
        }
        self.focused_media_shot_id = next_id;
        self.status = format!(
            "{} fokus: {} · {} otvori",
            self.media_pool_focus_label(),
            title,
            self.chord_or("activate_focused_item", "Enter")
        );
        PlaybackTransportIntent::None
    }

    #[allow(dead_code)]
    fn media_pool_focused_title(&self) -> String {
        self.current_media_pool_shots()
            .iter()
            .find(|shot| shot_id(shot) == self.focused_media_shot_id)
            .map(|shot| {
                if !shot.name.is_empty() {
                    shot.name.clone()
                } else if !shot.virtual_name.is_empty() {
                    shot.virtual_name.clone()
                } else if !shot.clip_id.is_empty() {
                    shot.clip_id.clone()
                } else {
                    shot_id(shot)
                }
            })
            .unwrap_or_else(|| "nema odabira".into())
    }

    fn activate_focused_panel_item(&mut self, host: &HostClient) -> PlaybackTransportIntent {
        match self.panel_focus {
            PanelFocus::MediaPool => {
                if self.library_tab == LibraryTab::Segment {
                    if self.selected_part_id.trim().is_empty() {
                        self.status = "Odaberi segment".into();
                        return PlaybackTransportIntent::None;
                    }
                    return self
                        .start_wrap_session_for_part(host, Some(self.selected_part_id.clone()));
                }
                self.ensure_media_pool_focus();
                let focused_id = self.focused_media_shot_id.clone();
                let Some(shot) = self
                    .current_media_pool_shots()
                    .iter()
                    .find(|shot| shot_id(shot) == focused_id)
                    .cloned()
                else {
                    self.status = "Nema fokusiranog klipa".into();
                    return PlaybackTransportIntent::None;
                };
                self.select_shot(host, &shot)
            }
            PanelFocus::SegmentPanel => self.ensure_wrap_or_scrub(host),
            PanelFocus::SourceTimeline => self.cue_current_playhead_intent(),
        }
    }

    fn sync_cover_enter_or_activate_focused_item(
        &mut self,
        host: &HostClient,
    ) -> PlaybackTransportIntent {
        if self.sync_cover.active().is_some() {
            self.status = "Sync play je aktivan · OUT završava slot".into();
            return PlaybackTransportIntent::None;
        }
        if self.sync_cover.pending_slot().is_some() {
            if self.try_select_pending_sync_slot().is_none() {
                return PlaybackTransportIntent::None;
            }
        }
        if self.sync_cover.ready_cover().is_some() {
            return self.commit_sync_cover_with_enter(host);
        }
        self.activate_focused_panel_item(host)
    }

    #[allow(dead_code)]
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

    #[allow(dead_code)]
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

    #[allow(dead_code)]
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
                if self.sync_cover.enabled() {
                    self.sync_cover
                        .arm_source_in(&self.selected_clip_id, self.source_in_frame);
                }
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

    fn mark_out_or_sync_end(&mut self, host: &HostClient) -> PlaybackTransportIntent {
        if self.sync_cover.active().is_some() {
            return self.finish_sync_cover_with_out(host);
        }
        self.mark_out_action(host)
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
        self.status = format!("Isključujem segment {}", truncate(&part_id, 24));
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

    fn navigate_adjacent_object(
        &mut self,
        host: &HostClient,
        direction: i32,
    ) -> PlaybackTransportIntent {
        if self.active_keyboard_panel_is_source() && self.panel_focus != PanelFocus::MediaPool {
            return self.navigate_adjacent_source_object(host, direction);
        }
        match self.panel_focus {
            PanelFocus::MediaPool => self.move_media_pool_focus(host, direction.signum()),
            PanelFocus::SourceTimeline => self.navigate_adjacent_source_object(host, direction),
            PanelFocus::SegmentPanel => self.navigate_adjacent_program_object(host, direction),
        }
    }

    fn active_keyboard_panel_is_source(&self) -> bool {
        self.source_dock_keyboard_focus || self.panel_focus == PanelFocus::SourceTimeline
    }

    fn activate_source_keyboard_panel(&mut self) {
        self.view_mode = ViewMode::Source;
        self.source_dock_keyboard_focus = true;
        self.panel_focus = PanelFocus::SourceTimeline;
    }

    fn step_adjacent_segment(
        &mut self,
        host: &HostClient,
        direction: i32,
    ) -> PlaybackTransportIntent {
        if self.active_keyboard_panel_is_source() && self.panel_focus != PanelFocus::MediaPool {
            return self.select_adjacent_source_segment(host, direction);
        }
        if self.panel_focus == PanelFocus::MediaPool {
            let delta = direction.signum() * self.media_pool_grid_cols.max(1) as i32;
            return self.move_media_pool_focus(host, delta);
        }
        self.select_adjacent_part(host, direction)
    }

    fn select_adjacent_source_segment(
        &mut self,
        host: &HostClient,
        direction: i32,
    ) -> PlaybackTransportIntent {
        self.activate_source_keyboard_panel();
        let Some(shot) = self.adjacent_source_segment(direction).cloned() else {
            self.status = if direction < 0 {
                "Nema prethodnog source segmenta".into()
            } else {
                "Nema sljedećeg source segmenta".into()
            };
            return PlaybackTransportIntent::None;
        };
        self.select_shot(host, &shot)
    }

    fn adjacent_source_segment(&self, direction: i32) -> Option<&StoryShot> {
        if direction == 0 {
            return None;
        }
        let clip_id = self.selected_clip_id.trim();
        if clip_id.is_empty() {
            return None;
        }
        let mut shots: Vec<&StoryShot> = self
            .virtual_shots
            .iter()
            .filter(|shot| {
                shot.clip_id == clip_id
                    && !shot_id(shot).trim().is_empty()
                    && source_shot_end_frame(shot) > shot.in_frame.max(0)
            })
            .collect();
        if shots.is_empty() {
            return None;
        }
        shots.sort_by(|left, right| {
            left.in_frame
                .max(0)
                .cmp(&right.in_frame.max(0))
                .then(source_shot_end_frame(left).cmp(&source_shot_end_frame(right)))
                .then(shot_id(left).cmp(&shot_id(right)))
        });

        let selected_shot_id = self.selected_shot_id.trim();
        let current_index = shots
            .iter()
            .position(|shot| !selected_shot_id.is_empty() && shot_id(shot) == selected_shot_id)
            .or_else(|| {
                let frame = self.source_playhead_frame.max(0);
                shots.iter().position(|shot| {
                    frame >= shot.in_frame.max(0) && frame < source_shot_end_frame(shot)
                })
            });
        if let Some(current_index) = current_index {
            let next = if direction < 0 {
                current_index.checked_sub(1)?
            } else {
                current_index
                    .checked_add(1)
                    .filter(|next| *next < shots.len())?
            };
            return shots.get(next).copied();
        }

        let frame = self.source_playhead_frame.max(0);
        if direction < 0 {
            shots
                .iter()
                .rfind(|shot| shot.in_frame.max(0) < frame)
                .copied()
        } else {
            shots
                .iter()
                .find(|shot| shot.in_frame.max(0) > frame)
                .copied()
        }
    }

    fn navigate_adjacent_source_object(
        &mut self,
        host: &HostClient,
        direction: i32,
    ) -> PlaybackTransportIntent {
        self.activate_source_keyboard_panel();
        let Some(target) = self.adjacent_source_navigation_target(direction) else {
            self.status = if direction < 0 {
                "Nema prethodnog source objekta".into()
            } else {
                "Nema sljedećeg source objekta".into()
            };
            return PlaybackTransportIntent::None;
        };
        match target {
            SourceNavigationTarget::Start => {
                self.focus.clear();
                self.set_source_playhead_frame(0);
                self.status = "Source početak".into();
                self.cue_current_playhead_intent()
            }
            SourceNavigationTarget::MarkIn => self.select_mark_in(host),
            SourceNavigationTarget::MarkOut => self.select_mark_out(host),
        }
    }

    fn adjacent_source_navigation_target(&self, direction: i32) -> Option<SourceNavigationTarget> {
        adjacent_source_navigation_target(
            &self.focus,
            self.source_playhead_frame,
            self.mark_in_set.then_some(self.source_in_frame),
            self.mark_out_set.then_some(self.source_out_frame),
            direction,
        )
    }

    fn navigate_adjacent_program_object(
        &mut self,
        host: &HostClient,
        direction: i32,
    ) -> PlaybackTransportIntent {
        let program = self.segment_program_model();
        let Some(target) = program.adjacent_navigation_target(
            &self.selected_part_id,
            &self.selected_slot_id,
            &self.selected_marker_id,
            self.wrap_playhead_frame,
            direction,
        ) else {
            self.status = if direction < 0 {
                "Nema prethodnog timeline objekta".into()
            } else {
                "Nema sljedećeg timeline objekta".into()
            };
            return PlaybackTransportIntent::None;
        };
        drop(program);

        match target.kind {
            SegmentProgramNavigationKind::Segment => {
                self.selected_marker_id.clear();
                self.selected_slot_id.clear();
                self.selected_cover_id.clear();
                self.selected_part_id = target.id;
                self.set_wrap_playhead_frame(target.frame);
                self.ensure_wrap_or_scrub(host)
            }
            SegmentProgramNavigationKind::MarkerSlot => {
                self.set_wrap_playhead_frame(target.frame);
                self.select_marker_slot(host, &target.id);
                self.ensure_wrap_or_scrub(host)
            }
            SegmentProgramNavigationKind::Marker => {
                self.selected_slot_id.clear();
                self.selected_cover_id.clear();
                self.select_marker(host, &target.id, target.frame)
            }
        }
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

    fn jump_playlist_start(&mut self, host: &HostClient) -> PlaybackTransportIntent {
        let first_part_id = self
            .segment_program_model()
            .segments()
            .first()
            .map(|segment| segment.part_id.clone())
            .unwrap_or_default();
        self.selected_part_id = first_part_id;
        self.set_wrap_playhead_frame(0);
        self.ensure_wrap_or_scrub(host)
    }

    fn delete_selected_timeline_item(&mut self, host: &HostClient) {
        if !self.selected_marker_id.trim().is_empty() {
            let marker_id = self.selected_marker_id.clone();
            self.delete_marker(host, &marker_id);
            return;
        }
        if !self.selected_cover_id.trim().is_empty()
            && !is_pending_cover_id(&self.selected_cover_id)
        {
            let cover_id = self.selected_cover_id.clone();
            self.delete_cover(host, &cover_id);
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

    fn toggle_sync_cover_capture(&mut self) -> PlaybackTransportIntent {
        let enabled = !self.sync_cover.enabled();
        let was_active = self.sync_cover.set_enabled(enabled);
        if enabled {
            self.status = "Sync uključen · Source IN armira Sync play".into();
            return PlaybackTransportIntent::None;
        }
        if was_active {
            self.playing = false;
            self.status = "Sync prekinut".into();
            return PlaybackTransportIntent::Pause;
        }
        self.status = "Sync isključen".into();
        PlaybackTransportIntent::None
    }

    fn sync_cover_should_start_on_space(&self) -> bool {
        SyncCoverCaptureComponent::should_start_on_space(
            &self.sync_cover,
            SyncCoverSpaceContext {
                view_is_source: self.view_mode == ViewMode::Source,
                source_dock_keyboard_focus: self.source_dock_keyboard_focus,
                source_clip_id: &self.selected_clip_id,
            },
        )
    }

    fn sync_cover_toggle_play_intent(
        &mut self,
        playlist_input_active: bool,
        playlist_input_playing: bool,
    ) -> PlaybackTransportIntent {
        if self.sync_cover.active().is_some() && (playlist_input_playing || self.playing) {
            self.playing = false;
            self.status = "Sync pauza".into();
            return PlaybackTransportIntent::Pause;
        }
        if self.sync_cover.active().is_some() && playlist_input_active {
            self.playing = true;
            self.status = "Sync play".into();
            return PlaybackTransportIntent::PlayLoadedInput;
        }
        let Some(source_in_frame) = self
            .sync_cover
            .armed_source_in_frame(&self.selected_clip_id)
        else {
            return self
                .toggle_play_intent_for_input(playlist_input_active, playlist_input_playing);
        };
        if !self.sync_cover_should_start_on_space() {
            return self
                .toggle_play_intent_for_input(playlist_input_active, playlist_input_playing);
        }
        self.selected_slot_id.clear();
        self.selected_cover_id.clear();
        self.selected_marker_id.clear();
        if !self.request_sync_cover_playback_inputs() {
            return PlaybackTransportIntent::None;
        }
        let input = SyncCoverPreviewInput {
            project_id: &self.project_id,
            program_id: self.edit_instance_id(),
            start_program_frame: self.wrap_playhead_frame,
            playlist: self.playlist.as_ref(),
            marker_slots: &self.marker_slots,
            covers: &self.covers,
            markers: &self.markers,
            all_clips: &self.all_clips,
            virtual_shots: &self.virtual_shots,
            cover_shots: &self.cover_shots,
            playback_inputs: &self.playlist_playback_inputs,
            source_clip_id: &self.selected_clip_id,
            source_in_frame,
            source_duration_frames: self.selected_clip_duration_frames(),
            source_timebase: self.selected_source_timebase,
        };
        let outcome = match SyncCoverCaptureComponent::build_preview(input) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.status = error;
                return PlaybackTransportIntent::None;
            }
        };
        let anchor_program_frame = outcome.session.anchor_program_frame;
        let source_in_frame = outcome.session.source_in_frame;
        if let Some(part_id) = self
            .segment_program_model()
            .active_part_at_program_frame(anchor_program_frame)
            .map(|segment| segment.part_id.clone())
        {
            self.selected_part_id = part_id;
        }
        self.view_mode = ViewMode::Wrap;
        self.source_dock_keyboard_focus = false;
        self.panel_focus = PanelFocus::SegmentPanel;
        self.set_wrap_playhead_frame(anchor_program_frame);
        self.set_source_playhead_frame(source_in_frame);
        self.playing = true;
        self.sync_cover.set_active(outcome.session);
        self.status = "Sync play · OUT završava slot".into();
        PlaybackTransportIntent::PlayProgram(outcome.request)
    }

    fn request_sync_cover_playback_inputs(&mut self) -> bool {
        let playlist_ready = self.request_playlist_playback_inputs();
        let clip_id = self.selected_clip_id.trim().to_string();
        if clip_id.is_empty() {
            self.status = "Odaberi source klip za Sync".into();
            return false;
        }
        if self
            .playlist_playback_inputs
            .get(&clip_id)
            .is_some_and(|input| !input.trim().is_empty())
        {
            return playlist_ready;
        }
        if self.selected_play_input_clip_id == clip_id && !self.selected_play_path.trim().is_empty()
        {
            self.playlist_playback_inputs
                .insert(clip_id, self.selected_play_path.trim().to_string());
            return playlist_ready;
        }
        self.request_playback_input_for_selected_clip();
        self.status = "Sync · čekam source media resolve".into();
        false
    }

    fn finish_sync_cover_with_out(&mut self, host: &HostClient) -> PlaybackTransportIntent {
        let Some(session) = self.sync_cover.active().cloned() else {
            return self.mark_out_action(host);
        };
        let duration_frames = self
            .playlist
            .as_ref()
            .map(|playlist| playlist.duration_frames)
            .unwrap_or_else(|| self.segment_program_model().duration_frames())
            .max(session.anchor_program_frame + 1);
        let pending = match SyncCoverCaptureComponent::pending_slot(
            &session,
            self.wrap_playhead_frame,
            duration_frames,
        ) {
            Ok(pending) => pending,
            Err(error) => {
                self.status = error;
                return PlaybackTransportIntent::None;
            }
        };
        self.set_wrap_playhead_frame(pending.timeline_end_frame);
        self.source_in_frame = pending.source_in_frame;
        self.source_out_frame = pending.source_out_frame;
        self.set_source_playhead_frame(pending.source_out_frame);
        self.mark_in_set = true;
        self.mark_out_set = true;
        self.focus.clear();
        self.sync_cover.set_pending_slot(pending.clone());
        self.playing = false;
        if self.try_select_pending_sync_slot().is_some() {
            self.status = "Sync slot odabran · Enter dodaje pokrivalicu".into();
            return PlaybackTransportIntent::Pause;
        }
        if !self
            .markers
            .iter()
            .any(|marker| marker.timeline_frame == pending.timeline_end_frame)
        {
            self.marker_at_head(host);
        }
        self.status = "Sync OUT spremljen · čekam novi M-M slot".into();
        PlaybackTransportIntent::Pause
    }

    fn try_select_pending_sync_slot(&mut self) -> Option<PlaybackTransportIntent> {
        let pending = self.sync_cover.take_pending_slot()?;
        let plan = match SyncCoverCaptureComponent::slot_plan(&pending, &self.marker_slots) {
            Ok(plan) => plan,
            Err(error) => {
                self.sync_cover.restore_pending_slot(pending);
                self.status = error;
                return None;
            }
        };
        let ready = SyncCoverCaptureComponent::ready_cover(&pending, &plan);
        self.selected_slot_id = plan.slot_id;
        self.selected_cover_id.clear();
        self.source_in_frame = pending.source_in_frame;
        self.source_out_frame = pending.source_out_frame;
        self.set_source_playhead_frame(pending.source_out_frame);
        self.mark_in_set = true;
        self.mark_out_set = true;
        self.sync_cover.set_ready_cover(ready);
        self.status = "Sync slot odabran · Enter dodaje pokrivalicu".into();
        Some(self.playlist_input_preload_intent())
    }

    fn commit_sync_cover_with_enter(&mut self, _host: &HostClient) -> PlaybackTransportIntent {
        let Some(ready) = self.sync_cover.take_ready_cover() else {
            return PlaybackTransportIntent::None;
        };
        let Some(slot) = self
            .marker_slots
            .iter()
            .find(|slot| slot.slot_id == ready.slot_id && !slot.slot_id.trim().is_empty())
        else {
            self.sync_cover.set_ready_cover(ready);
            self.status = "Sync slot još nije dostupan".into();
            return PlaybackTransportIntent::None;
        };
        if slot.has_cover {
            self.status = "Sync slot već ima pokrivalicu".into();
            return PlaybackTransportIntent::None;
        }
        let Some(source_fps) = ready.source_timebase.fps() else {
            self.sync_cover.set_ready_cover(ready);
            self.status = "Source FPS još nije potvrđen".into();
            return PlaybackTransportIntent::None;
        };
        let slot_id = ready.slot_id.clone();
        let clip_id = ready.source_clip_id.clone();
        let source = story_edit::CoverSourceRange {
            clip_id: clip_id.clone(),
            in_frame: ready.source_in_frame,
            out_frame: ready.source_out_frame,
            fps: source_fps,
            source_timebase: ready.source_timebase,
        };
        self.selected_slot_id = slot_id.clone();
        self.selected_cover_id.clear();
        self.selected_marker_id.clear();
        self.selected_clip_id = clip_id.clone();
        self.source_in_frame = ready.source_in_frame;
        self.source_out_frame = ready.source_out_frame;
        self.set_source_playhead_frame(ready.source_out_frame);
        self.mark_in_set = true;
        self.mark_out_set = true;
        self.enqueue_edit_command(|instance, request, project| {
            EditorialEditComponent::create_cover_from_source(
                instance,
                request,
                project,
                &slot_id,
                &clip_id,
                source.in_frame,
                source.out_frame,
            )
        });
        let intent = self.add_pending_cover_projection(&slot_id, &source);
        self.status = "Sync pokrivalica dodana u slot · spremam...".into();
        intent
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

    fn delete_cover(&mut self, _host: &HostClient, cover_id: &str) {
        let cover_id = cover_id.trim();
        if cover_id.is_empty() {
            return;
        }
        self.enqueue_delete_cover(cover_id);
        self.remove_visible_cover(cover_id);
        self.playlist_loaded = false;
        self.status = format!("Brišem pokrivalicu {}", truncate(cover_id, 24));
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

    fn select_adjacent_marker(
        &mut self,
        host: &HostClient,
        direction: i32,
    ) -> PlaybackTransportIntent {
        if direction == 0 {
            return PlaybackTransportIntent::None;
        }
        let current_frame = self.wrap_playhead_frame.max(0);
        let target = if direction < 0 {
            self.markers
                .iter()
                .filter(|marker| {
                    !marker.marker_id.trim().is_empty()
                        && marker.timeline_frame > 0
                        && marker.timeline_frame < current_frame
                })
                .max_by_key(|marker| marker.timeline_frame)
        } else {
            self.markers
                .iter()
                .filter(|marker| {
                    !marker.marker_id.trim().is_empty()
                        && marker.timeline_frame > 0
                        && marker.timeline_frame > current_frame
                })
                .min_by_key(|marker| marker.timeline_frame)
        };
        let Some(marker) = target else {
            self.status = if direction < 0 {
                "Nema prethodnog M markera".into()
            } else {
                "Nema sljedećeg M markera".into()
            };
            return PlaybackTransportIntent::None;
        };
        let marker_id = marker.marker_id.clone();
        let frame = marker.timeline_frame;
        self.select_marker(host, &marker_id, frame)
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

    fn select_current_marker_slot(&mut self, host: &HostClient) -> PlaybackTransportIntent {
        let program = self.segment_program_model();
        let Some(slot) = program.marker_slot_at_program_frame(self.wrap_playhead_frame) else {
            self.status = "Nema M-M slota pod playheadom".into();
            return PlaybackTransportIntent::None;
        };
        let slot_id = slot.slot_id.clone();
        drop(program);

        self.select_marker_slot(host, &slot_id);
        PlaybackTransportIntent::None
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
        if self.project_id.trim().is_empty() {
            self.status = "Nema otvorenog projekta".into();
            return PlaybackTransportIntent::None;
        }
        if self.sync_cover.ready_cover().is_some() {
            self.status = "Sync slot čeka Enter".into();
            return PlaybackTransportIntent::None;
        }
        if self.sync_cover.pending_slot().is_some() && self.try_select_pending_sync_slot().is_none()
        {
            return PlaybackTransportIntent::None;
        }
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
            self.selected_source_timebase
                .is_valid()
                .then_some(self.selected_source_timebase),
        ) {
            Ok(source) => source,
            Err(e) => {
                self.status = e;
                return PlaybackTransportIntent::None;
            }
        };
        let slot_id = target.slot_id;
        let clip_id = source.clip_id.clone();
        let in_frame = source.in_frame;
        let out_frame = source.out_frame;
        self.enqueue_edit_command(|instance, request, project| {
            EditorialEditComponent::create_cover_from_source(
                instance, request, project, &slot_id, &clip_id, in_frame, out_frame,
            )
        });
        let intent = self.add_pending_cover_projection(&slot_id, &source);
        self.status = "Pokrivalica dodana u slot · spremam...".into();
        intent
    }

    fn overwrite_cover(&mut self, _host: &HostClient) -> PlaybackTransportIntent {
        if self.project_id.trim().is_empty() {
            self.status = "Nema otvorenog projekta".into();
            return PlaybackTransportIntent::None;
        }
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
            self.selected_source_timebase
                .is_valid()
                .then_some(self.selected_source_timebase),
        ) {
            Ok(source) => source,
            Err(e) => {
                self.status = e;
                return PlaybackTransportIntent::None;
            }
        };
        let slot_id = target.slot_id;
        let clip_id = source.clip_id.clone();
        let in_frame = source.in_frame;
        let out_frame = source.out_frame;
        self.enqueue_edit_command(|instance, request, project| {
            EditorialEditComponent::create_cover_from_source(
                instance, request, project, &slot_id, &clip_id, in_frame, out_frame,
            )
        });
        let intent = self.add_pending_cover_projection(&slot_id, &source);
        self.status = "Pokrivalica zamijenjena u slotu · spremam...".into();
        intent
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
        if self.panel_focus == PanelFocus::MediaPool && !self.source_dock_keyboard_focus {
            return self.move_media_pool_focus(host, frames.signum() as i32);
        }
        if self.source_dock_keyboard_focus {
            self.activate_source_keyboard_panel();
        }
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

    fn playback_source_timebase(&self) -> Option<BroadcastSourceTimebase> {
        self.selected_source_timebase
            .is_valid()
            .then_some(self.selected_source_timebase)
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
    use crate::api::{EditorialSourceTimebase, TimelineSegment};
    use crate::player_remote::{PlayerEvent, PROGRAM_AUDIO_OUTPUT_CH1, PROGRAM_AUDIO_OUTPUT_CH2};
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

    fn story_state_with_cover(slot_id: &str, cover_id: &str) -> Value {
        json!({
            "selected_part_id": "part_a",
            "selected_cover_id": cover_id,
            "selected_slot_id": slot_id,
            "parts": [{
                "part_id": "part_a",
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
            "covers": [{
                "cover_id": cover_id,
                "slot_id": slot_id,
                "clip_id": "clip_a",
                "title": "clip_a",
                "timeline_start_frame": 10,
                "timeline_end_frame": 40,
                "source_in_frame": 100,
                "source_out_frame": 150,
                "source_fps": 50.0,
                "source_timebase": { "fps_num": 50, "fps_den": 1 }
            }],
            "markers": [],
            "marker_slots": [{
                "slot_id": slot_id,
                "start_frame": 10,
                "end_frame": 40,
                "has_cover": true
            }]
        })
    }

    fn tb50() -> EditorialSourceTimebase {
        EditorialSourceTimebase {
            fps_num: 50,
            fps_den: 1,
        }
    }

    fn source_tb50() -> BroadcastSourceTimebase {
        BroadcastSourceTimebase {
            fps_num: 50,
            fps_den: 1,
        }
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
                source_timebase: tb50(),
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
                    source_timebase: tb50(),
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
                    source_timebase: tb50(),
                    streamable: true,
                    ..EditorialPlaylistSegment::default()
                },
            ],
        }
    }

    fn seed_playlist_playback_inputs(screen: &mut StoryScreen, clip_ids: &[&str]) {
        for clip_id in clip_ids {
            screen.playlist_playback_inputs.insert(
                (*clip_id).to_string(),
                format!("C:/qnc/resolved/{clip_id}.mp4"),
            );
        }
    }

    fn resolved_story_probe_input(path: &str) -> PlaybackMediaResolution {
        PlaybackMediaResolution {
            media_input: path.into(),
            locator_kind: "local",
            source_timebase: Some(source_tb50()),
            duration_sec: Some(2.0),
            duration_frames: Some(100),
            has_audio: Some(true),
            audio_channels: Some(2),
        }
    }

    fn sync_story_screen() -> StoryScreen {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        screen.loaded_project_id = "p".into();
        screen.state_loaded = true;
        screen.initial_selection_done = true;
        screen.view_mode = ViewMode::Source;
        screen.source_dock_keyboard_focus = true;
        screen.panel_focus = PanelFocus::SourceTimeline;
        screen.timeline = Some(two_part_timeline());
        screen.playlist = Some(two_part_playlist());
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
        screen.marker_slots = vec![
            MarkerSlot {
                slot_id: "slot_a".into(),
                start_frame: 0,
                end_frame: 50,
                has_cover: false,
                ..MarkerSlot::default()
            },
            MarkerSlot {
                slot_id: "slot_b".into(),
                start_frame: 50,
                end_frame: 100,
                has_cover: false,
                ..MarkerSlot::default()
            },
        ];
        screen.markers = vec![
            StoryMarker {
                marker_id: "m0".into(),
                timeline_frame: 0,
                part_id: "part_a".into(),
                ..StoryMarker::default()
            },
            StoryMarker {
                marker_id: "m50".into(),
                timeline_frame: 50,
                part_id: "part_b".into(),
                ..StoryMarker::default()
            },
        ];
        screen.all_clips = vec![
            StoryShot {
                shot_id: "clip_a".into(),
                root_shot_id: "clip_a".into(),
                clip_id: "clip_a".into(),
                fps: 50.0,
                source_timebase: tb50(),
                in_frame: 0,
                out_frame: 300,
                duration_frames: 300,
                play_path: "C:/qnc/proxy/clip_a.mp4".into(),
                has_audio: true,
                audio_channels: 2,
                ..StoryShot::default()
            },
            StoryShot {
                shot_id: "cover_a".into(),
                root_shot_id: "cover_a".into(),
                clip_id: "cover_a".into(),
                fps: 50.0,
                source_timebase: tb50(),
                in_frame: 0,
                out_frame: 300,
                duration_frames: 300,
                play_path: "C:/qnc/proxy/cover_a.mp4".into(),
                has_audio: true,
                audio_channels: 2,
                ..StoryShot::default()
            },
        ];
        screen.selected_clip_id = "cover_a".into();
        screen.selected_shot_id = "cover_a".into();
        screen.selected_source_fps = 50.0;
        screen.selected_source_timebase = source_tb50();
        screen.source_timebase_ready = true;
        screen.selected_source_has_audio = true;
        screen.selected_source_audio_channels = 2;
        screen.source_in_frame = 0;
        screen.source_out_frame = 300;
        screen.source_playhead_frame = 24;
        screen.wrap_playhead_frame = 30;
        screen.selected_source_ref = BroadcastHostSourceRef::from_frame_fields(
            "p",
            "cover_a",
            "",
            "cover_a",
            Some(FrameNumber(0)),
            Some(FrameNumber(300)),
            FrameNumber(300),
        )
        .ok();
        screen.selected_play_path = "C:/qnc/proxy/cover_a.mp4".into();
        seed_playlist_playback_inputs(&mut screen, &["clip_a", "cover_a"]);
        screen
    }

    fn sync_marker_state_with_slot(end_frame: i64) -> Value {
        json!({
            "selected_part_id": "part_a",
            "selected_slot_id": "",
            "parts": [{
                "part_id": "part_a",
                "kind": "tonovi",
                "clip_id": "clip_a",
                "in_frame": 100,
                "out_frame": 150,
                "fps": 50.0,
                "duration_frames": 50
            }, {
                "part_id": "part_b",
                "kind": "tonovi",
                "clip_id": "clip_a",
                "in_frame": 200,
                "out_frame": 250,
                "fps": 50.0,
                "duration_frames": 50
            }],
            "all_clips": [{
                "shot_id": "clip_a",
                "root_shot_id": "clip_a",
                "clip_id": "clip_a",
                "fps": 50.0,
                "source_timebase": { "fps_num": 50, "fps_den": 1 },
                "duration_frames": 300,
                "play_path": "C:/qnc/proxy/clip_a.mp4",
                "has_audio": true,
                "audio_channels": 2
            }, {
                "shot_id": "cover_a",
                "root_shot_id": "cover_a",
                "clip_id": "cover_a",
                "fps": 50.0,
                "source_timebase": { "fps_num": 50, "fps_den": 1 },
                "duration_frames": 300,
                "play_path": "C:/qnc/proxy/cover_a.mp4",
                "has_audio": true,
                "audio_channels": 2
            }],
            "virtual_shots": [],
            "covers": [],
            "markers": [{
                "marker_id": "m0",
                "timeline_frame": 0,
                "part_id": "part_a"
            }, {
                "marker_id": "m_end",
                "timeline_frame": end_frame,
                "part_id": "part_a"
            }],
            "marker_slots": [{
                "slot_id": format!("slot_0_{end_frame}"),
                "start_frame": 0,
                "end_frame": end_frame,
                "has_cover": false
            }]
        })
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
                source_timebase: BroadcastSourceTimebase {
                    fps_num: 50,
                    fps_den: 1,
                },
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
                source_timebase: tb50(),
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
        seed_playlist_playback_inputs(&mut screen, &["clip_segment"]);
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
    fn story_state_selects_initial_source_before_timeline_and_playlist_load() {
        let mut screen = StoryScreen::story();
        screen.begin_meta_load("p", 3);

        screen.apply_editorial_story_state("p", story_state_with_selected_part("part_a"));

        assert!(screen.state_loaded);
        assert!(!screen.meta_ready());
        assert!(screen.initial_selection_done);
        assert_eq!(screen.view_mode, ViewMode::Source);
        assert_eq!(screen.selected_clip_id, "clip_a");
        assert_eq!(screen.selected_play_path, "");
        let commands = screen.drain_backend_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].path, "/api/media/resolve");
        assert_eq!(
            commands[0]
                .payload
                .as_ref()
                .and_then(|payload| payload.get("access"))
                .and_then(Value::as_str),
            Some("playback_proxy")
        );
        let intent = screen.apply_playback_media_resolution(
            "p",
            "clip_a",
            PlaybackMediaResolution {
                media_input: "C:/qnc/proxy/clip_a.mp4".into(),
                locator_kind: "local",
                source_timebase: None,
                duration_sec: None,
                duration_frames: None,
                has_audio: None,
                audio_channels: None,
            },
        );
        assert_eq!(screen.selected_play_path, "C:/qnc/proxy/clip_a.mp4");
        assert_eq!(intent, PlaybackTransportIntent::CueFrame(0));
    }

    #[test]
    fn story_selection_waits_for_resolver_when_snapshot_probe_is_missing() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        screen.loaded_project_id = "p".into();
        let shot = StoryShot {
            shot_id: "clip_a".into(),
            root_shot_id: "clip_a".into(),
            clip_id: "clip_a".into(),
            duration_sec: 0.0,
            fps: 0.0,
            duration_frames: 0,
            has_audio: false,
            audio_channels: 0,
            ..StoryShot::default()
        };
        screen.all_clips = vec![shot.clone()];

        let intent = screen.select_shot(&HostClient::new("http://127.0.0.1:1"), &shot);

        assert_eq!(intent, PlaybackTransportIntent::None);
        assert_eq!(screen.selected_clip_id, "clip_a");
        assert!(screen.selected_source_ref.is_none());
        let commands = screen.drain_backend_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].path, "/api/media/resolve");

        let intent = screen.apply_playback_media_resolution(
            "p",
            "clip_a",
            resolved_story_probe_input("C:/qnc/proxy/clip_a.mp4"),
        );

        assert_eq!(intent, PlaybackTransportIntent::CueFrame(0));
        assert_eq!(screen.selected_play_path, "C:/qnc/proxy/clip_a.mp4");
        assert_eq!(screen.selected_source_fps, 50.0);
        assert!(screen.source_timebase_ready);
        assert!(screen.selected_source_ref.is_some());
        assert_eq!(screen.source_out_frame, 100);
        let updated = screen
            .all_clips
            .iter()
            .find(|shot| shot.clip_id == "clip_a")
            .expect("updated shot");
        assert_eq!(updated.fps, 50.0);
        assert_eq!(updated.duration_frames, 100);
        assert_eq!(updated.duration_sec, 2.0);
    }

    #[test]
    fn suspend_playback_session_keeps_loaded_story_components() {
        let mut screen = StoryScreen::story();
        screen.begin_meta_load("p", 3);
        screen.apply_editorial_story_state("p", story_state_with_selected_part("part_a"));
        screen.playing = true;
        screen.broadcast_preview_active = true;

        screen.suspend_playback_session();

        assert!(!screen.playing);
        assert!(!screen.broadcast_preview_active);
        assert_eq!(screen.loaded_project_id, "p");
        assert!(screen.state_loaded);
        assert_eq!(screen.all_clips.len(), 1);
        assert_eq!(screen.selected_clip_id, "clip_a");
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
        seed_playlist_playback_inputs(&mut screen, &["clip_a"]);

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
        assert_eq!(item.sources.len(), 2);
        let video = item
            .sources
            .iter()
            .find(|source| source.has_video)
            .expect("video source");
        assert_eq!(video.source_ref.in_frame, Some(FrameNumber(200)));
        assert_eq!(video.source_ref.out_frame, Some(FrameNumber(250)));
        assert!(video.has_video);
        assert!(!video.has_audio);
        assert_eq!(video.audio_output_channel, None);
        let audio = item
            .sources
            .iter()
            .find(|source| {
                source.has_audio && source.audio_output_channel == Some(PROGRAM_AUDIO_OUTPUT_CH1)
            })
            .expect("audio source");
        assert_eq!(audio.source_ref.in_frame, Some(FrameNumber(200)));
        assert_eq!(audio.source_ref.out_frame, Some(FrameNumber(250)));
        assert!(!audio.has_video);
        assert!(audio.has_audio);
        assert_eq!(audio.audio_channels, 2);
        assert_eq!(audio.audio_output_channel, Some(PROGRAM_AUDIO_OUTPUT_CH1));
    }

    #[test]
    fn sync_enabled_without_source_in_keeps_source_space_normal() {
        let mut screen = sync_story_screen();
        screen.toggle_sync_cover_capture();

        let intent = screen.playback_transport_toggle_intent(false, false);

        assert_eq!(intent, PlaybackTransportIntent::TogglePlay);
        assert_eq!(screen.view_mode, ViewMode::Source);
        assert_eq!(screen.source_playhead_frame, 24);
        assert!(screen.sync_cover.active().is_none());
    }

    #[test]
    fn source_mark_in_arms_sync_and_space_builds_single_program_overlay() {
        let mut screen = sync_story_screen();
        screen.toggle_sync_cover_capture();
        screen.source_playhead_frame = 24;

        let mark_in_intent = screen.dispatch_playback_action(
            &HostClient::new("http://127.0.0.1:1"),
            playback_controls::PlaybackAction::MarkIn,
        );
        assert!(matches!(
            mark_in_intent,
            PlaybackTransportIntent::None | PlaybackTransportIntent::CueFrame(24)
        ));
        assert_eq!(screen.sync_cover.armed_source_in_frame("cover_a"), Some(24));

        let intent = screen.playback_transport_toggle_intent(false, false);
        let request = match intent {
            PlaybackTransportIntent::PlayProgram(request) => request,
            other => panic!("expected PlayProgram, got {other:?}"),
        };

        assert_eq!(screen.view_mode, ViewMode::Wrap);
        assert_eq!(screen.wrap_playhead_frame, 0);
        assert_eq!(screen.source_playhead_frame, 24);
        assert!(screen.playing);
        assert_eq!(request.start_program_frame, FrameNumber(0));
        assert_eq!(request.items.len(), 2);
        let preview_item = request
            .items
            .iter()
            .find(|item| item.record_in_frame == FrameNumber(0))
            .expect("preview item");
        assert!(preview_item.sources.iter().any(|source| {
            source.source_ref.clip_id == "clip_a"
                && !source.has_video
                && source.has_audio
                && source.audio_output_channel == Some(PROGRAM_AUDIO_OUTPUT_CH1)
        }));
        let cover_video = preview_item
            .sources
            .iter()
            .find(|source| {
                source.source_ref.clip_id == "cover_a"
                    && source.has_video
                    && source.has_audio
                    && source.audio_output_channel == Some(PROGRAM_AUDIO_OUTPUT_CH2)
            })
            .expect("cover video");
        assert_eq!(cover_video.source_ref.in_frame, Some(FrameNumber(24)));
        assert_eq!(cover_video.source_ref.out_frame, Some(FrameNumber(74)));
    }

    #[test]
    fn sync_space_clears_stale_selected_slot_before_choosing_anchor_marker() {
        let mut screen = sync_story_screen();
        screen.marker_slots = vec![MarkerSlot {
            slot_id: "old_slot_0_35".into(),
            start_frame: 0,
            end_frame: 35,
            has_cover: false,
            ..MarkerSlot::default()
        }];
        screen.markers = vec![
            StoryMarker {
                marker_id: "m0".into(),
                timeline_frame: 0,
                part_id: "part_a".into(),
                ..StoryMarker::default()
            },
            StoryMarker {
                marker_id: "m35".into(),
                timeline_frame: 35,
                part_id: "part_a".into(),
                ..StoryMarker::default()
            },
        ];
        screen.selected_slot_id = "old_slot_0_35".into();
        screen.selected_cover_id = "old_cover".into();
        screen.selected_marker_id = "old_marker".into();
        screen.wrap_playhead_frame = 35;
        screen.toggle_sync_cover_capture();
        screen.source_playhead_frame = 24;
        let _ = screen.dispatch_playback_action(
            &HostClient::new("http://127.0.0.1:1"),
            playback_controls::PlaybackAction::MarkIn,
        );

        let intent = screen.playback_transport_toggle_intent(false, false);
        let request = match intent {
            PlaybackTransportIntent::PlayProgram(request) => request,
            other => panic!("expected PlayProgram, got {other:?}"),
        };

        assert_eq!(screen.selected_slot_id, "");
        assert_eq!(screen.selected_cover_id, "");
        assert_eq!(screen.selected_marker_id, "");
        assert_eq!(screen.wrap_playhead_frame, 35);
        assert_eq!(request.start_program_frame, FrameNumber(35));
        assert_eq!(
            screen
                .sync_cover
                .active()
                .map(|session| session.anchor_program_frame),
            Some(35)
        );

        screen.apply_story_state(&json!({
            "selected_part_id": "part_a",
            "selected_slot_id": "old_slot_0_35",
            "selected_cover_id": "old_cover",
            "parts": [],
            "all_clips": [],
            "virtual_shots": [],
            "covers": [],
            "markers": [{
                "marker_id": "m0",
                "timeline_frame": 0,
                "part_id": "part_a"
            }, {
                "marker_id": "m35",
                "timeline_frame": 35,
                "part_id": "part_a"
            }],
            "marker_slots": [{
                "slot_id": "old_slot_0_35",
                "start_frame": 0,
                "end_frame": 35,
                "has_cover": false
            }]
        }));
        assert_eq!(screen.selected_slot_id, "");
        assert_eq!(screen.selected_cover_id, "");
    }

    #[test]
    fn sync_play_blocks_normal_playlist_preload_from_replacing_pending_open() {
        let mut screen = sync_story_screen();
        screen.toggle_sync_cover_capture();
        screen.source_playhead_frame = 24;
        let _ = screen.dispatch_playback_action(
            &HostClient::new("http://127.0.0.1:1"),
            playback_controls::PlaybackAction::MarkIn,
        );
        let intent = screen.playback_transport_toggle_intent(false, false);
        assert!(matches!(intent, PlaybackTransportIntent::PlayProgram(_)));

        assert_eq!(
            screen.playlist_input_preload_intent(),
            PlaybackTransportIntent::None
        );
    }

    #[test]
    fn sync_pending_slot_blocks_quick_cover_fallback_until_materialized() {
        let host = HostClient::new("http://127.0.0.1:1");
        let mut screen = sync_story_screen();
        screen.toggle_sync_cover_capture();
        screen.source_playhead_frame = 24;
        let _ = screen.dispatch_playback_action(&host, playback_controls::PlaybackAction::MarkIn);
        let _ = screen.playback_transport_toggle_intent(false, false);
        screen.sync_playhead_from_player_frame(FrameNumber(35));
        let _ = screen.dispatch_playback_action(&host, playback_controls::PlaybackAction::MarkOut);
        let _ = screen.drain_backend_commands();

        let cover_intent =
            screen.dispatch_playback_action(&host, playback_controls::PlaybackAction::QuickCover);

        assert_eq!(cover_intent, PlaybackTransportIntent::None);
        assert!(screen.drain_backend_commands().is_empty());
        assert_eq!(screen.selected_slot_id, "");
        assert_eq!(screen.status, "Sync slot još nije materijaliziran");
    }

    #[test]
    fn sync_out_creates_marker_only_then_enter_fills_new_slot() {
        let host = HostClient::new("http://127.0.0.1:1");
        let mut screen = sync_story_screen();
        screen.toggle_sync_cover_capture();
        screen.source_playhead_frame = 24;
        let _ = screen.dispatch_playback_action(&host, playback_controls::PlaybackAction::MarkIn);
        let _ = screen.playback_transport_toggle_intent(false, false);
        screen.sync_playhead_from_player_frame(FrameNumber(35));

        let out_intent =
            screen.dispatch_playback_action(&host, playback_controls::PlaybackAction::MarkOut);

        assert_eq!(out_intent, PlaybackTransportIntent::Pause);
        assert_eq!(screen.source_in_frame, 24);
        assert_eq!(screen.source_out_frame, 59);
        let commands = screen.drain_backend_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].path, "/api/story/marker/create");
        let marker_payload = commands[0].payload.as_ref().expect("marker payload");
        assert_eq!(
            marker_payload.get("timeline_frame").and_then(Value::as_i64),
            Some(35)
        );

        let preload = screen.apply_editorial_edit_data(EditorialEditData {
            instance_id: "story".into(),
            project_id: "p".into(),
            kind: EditorialEditKind::CreateMarker,
            detail: "part_a".into(),
            state: sync_marker_state_with_slot(35),
        });
        assert!(matches!(
            preload,
            PlaybackTransportIntent::PreloadProgram(_)
        ));
        assert_eq!(screen.selected_slot_id, "slot_0_35");
        assert_eq!(screen.source_in_frame, 24);
        assert_eq!(screen.source_out_frame, 59);

        let shift_b_intent =
            screen.dispatch_playback_action(&host, playback_controls::PlaybackAction::QuickCover);
        assert_eq!(shift_b_intent, PlaybackTransportIntent::None);
        assert!(screen.drain_backend_commands().is_empty());
        assert_eq!(screen.status, "Sync slot čeka Enter");

        let cover_intent = screen.dispatch_playback_action(
            &host,
            playback_controls::PlaybackAction::ActivateFocusedItem,
        );

        assert!(matches!(
            cover_intent,
            PlaybackTransportIntent::PreloadProgram(_)
        ));
        let commands = screen.drain_backend_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].path, "/api/story/cover/create");
        let cover_payload = commands[0].payload.as_ref().expect("cover payload");
        assert_eq!(
            cover_payload.get("slot_id").and_then(Value::as_str),
            Some("slot_0_35")
        );
        assert_eq!(
            cover_payload.get("clip_id").and_then(Value::as_str),
            Some("cover_a")
        );
        assert_eq!(
            cover_payload.get("in_frame").and_then(Value::as_i64),
            Some(24)
        );
        assert_eq!(
            cover_payload.get("out_frame").and_then(Value::as_i64),
            Some(59)
        );
    }

    #[test]
    fn sync_auto_arms_virtual_tab_selection_but_not_all_tab_root_selection() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        screen.loaded_project_id = "p".into();
        screen.toggle_sync_cover_capture();
        let root_shot = StoryShot {
            shot_id: "root_a".into(),
            root_shot_id: "root_a".into(),
            clip_id: "clip_a".into(),
            kind: "virtual".into(),
            fps: 50.0,
            source_timebase: tb50(),
            in_frame: 0,
            out_frame: 300,
            duration_frames: 300,
            play_path: "C:/qnc/proxy/clip_a.mp4".into(),
            has_audio: true,
            audio_channels: 2,
            ..StoryShot::default()
        };
        let short_shot = StoryShot {
            shot_id: "short_a".into(),
            root_shot_id: "clip_a".into(),
            clip_id: "clip_a".into(),
            kind: "virtual".into(),
            fps: 50.0,
            source_timebase: tb50(),
            in_frame: 80,
            out_frame: 130,
            duration_frames: 50,
            play_path: "C:/qnc/proxy/clip_a.mp4".into(),
            has_audio: true,
            audio_channels: 2,
            ..StoryShot::default()
        };
        screen.all_clips = vec![root_shot.clone()];
        screen.virtual_shots = vec![short_shot.clone()];

        screen.library_tab = LibraryTab::All;
        screen.select_shot_from_snapshot(&root_shot);
        assert_eq!(
            screen.sync_cover.armed_source_in_frame("clip_a"),
            None,
            "All/source-root selection must not arm sync just because it is internally virtual"
        );

        screen.library_tab = LibraryTab::Virtual;
        screen.select_shot_from_snapshot(&short_shot);
        assert_eq!(screen.sync_cover.armed_source_in_frame("clip_a"), Some(80));

        screen.source_playhead_frame = 92;
        let intent = screen.dispatch_playback_action(
            &HostClient::new("http://127.0.0.1:1"),
            playback_controls::PlaybackAction::MarkIn,
        );
        assert!(matches!(
            intent,
            PlaybackTransportIntent::None | PlaybackTransportIntent::CueFrame(92)
        ));
        assert_eq!(screen.sync_cover.armed_source_in_frame("clip_a"), Some(92));
    }

    #[test]
    fn wrap_entry_preloads_playlist_input_without_starting_playback() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        screen.loaded_project_id = "p".into();
        screen.playlist = Some(two_part_playlist());
        screen.all_clips = vec![StoryShot {
            clip_id: "clip_a".into(),
            fps: 50.0,
            duration_frames: 300,
            play_path: "C:/qnc/proxy/clip_a.mp4".into(),
            has_audio: true,
            audio_channels: 2,
            ..StoryShot::default()
        }];
        screen.wrap_playhead_frame = 50;
        seed_playlist_playback_inputs(&mut screen, &["clip_a"]);

        let intent = screen.start_wrap_session(&HostClient::new("http://127.0.0.1:1"));

        let request = match intent {
            PlaybackTransportIntent::PreloadProgram(request) => request,
            other => panic!("expected PreloadProgram, got {other:?}"),
        };
        assert_eq!(screen.view_mode, ViewMode::Wrap);
        assert!(!screen.playing);
        assert_eq!(request.start_program_frame, FrameNumber(50));
        assert_eq!(request.items.len(), 2);
    }

    #[test]
    fn playlist_snapshot_preloads_before_wrap_mode() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        screen.loaded_project_id = "p".into();
        screen.all_clips = vec![StoryShot {
            clip_id: "clip_a".into(),
            fps: 50.0,
            duration_frames: 300,
            play_path: "C:/qnc/proxy/clip_a.mp4".into(),
            has_audio: true,
            audio_channels: 2,
            ..StoryShot::default()
        }];
        seed_playlist_playback_inputs(&mut screen, &["clip_a"]);

        let intent = screen.apply_editorial_playlist("p", two_part_playlist());

        assert!(matches!(intent, PlaybackTransportIntent::PreloadProgram(_)));
        assert_eq!(screen.view_mode, ViewMode::Source);
    }

    #[test]
    fn home_from_segment_panel_opens_playlist_input_at_start() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        screen.loaded_project_id = "p".into();
        screen.playlist = Some(two_part_playlist());
        screen.all_clips = vec![StoryShot {
            clip_id: "clip_a".into(),
            fps: 50.0,
            duration_frames: 300,
            play_path: "C:/qnc/proxy/clip_a.mp4".into(),
            has_audio: true,
            audio_channels: 2,
            ..StoryShot::default()
        }];
        seed_playlist_playback_inputs(&mut screen, &["clip_a"]);

        let intent = screen.dispatch_marker_cover_action(
            &HostClient::new("http://127.0.0.1:1"),
            marker_cover_panel::MarkerCoverAction::PlaylistStart,
        );

        let request = match intent {
            PlaybackTransportIntent::OpenProgram(request) => request,
            other => panic!("expected OpenProgram, got {other:?}"),
        };
        assert_eq!(screen.view_mode, ViewMode::Wrap);
        assert_eq!(screen.wrap_playhead_frame, 0);
        assert_eq!(request.start_program_frame, FrameNumber(0));
    }

    #[test]
    fn source_media_request_blocker_treats_a2_only_waveform_as_loaded() {
        let mut screen = StoryScreen::story();
        screen.selected_clip_id = "clip_a".into();
        screen.waveform_clip_id = "clip_a".into();
        screen.selected_source_has_audio = true;
        screen.filmstrip_manifest_ready = true;
        screen.a2_peaks = vec![0.2, 0.4];
        screen.film_frames = vec![FilmFrame {
            index: 0,
            seek_sec: 0.0,
            url: "/api/story/thumbnail?clip_id=clip_a&frame_index=0".into(),
            texture: None,
            load_attempts: 0,
        }];

        assert!(screen.source_media_request_blocked("clip_a"));
    }

    #[test]
    fn source_media_request_blocker_does_not_retry_no_audio_sources() {
        let mut screen = StoryScreen::story();
        screen.selected_clip_id = "clip_a".into();
        screen.waveform_clip_id = "clip_a".into();
        screen.selected_source_has_audio = false;
        screen.filmstrip_manifest_ready = true;
        screen.film_frames = vec![FilmFrame {
            index: 0,
            seek_sec: 0.0,
            url: "/api/story/thumbnail?clip_id=clip_a&frame_index=0".into(),
            texture: None,
            load_attempts: 0,
        }];

        assert!(screen.source_media_request_blocked("clip_a"));
    }

    #[test]
    fn source_media_request_blocker_retries_placeholder_filmstrip() {
        let mut screen = StoryScreen::story();
        screen.selected_clip_id = "clip_a".into();
        screen.waveform_clip_id = "clip_a".into();
        screen.selected_source_has_audio = true;
        screen.a1_peaks = vec![0.2, 0.4];
        screen.film_frames = vec![FilmFrame {
            index: 0,
            seek_sec: 0.0,
            url: "/api/story/filmstrip/placeholder".into(),
            texture: None,
            load_attempts: 0,
        }];

        assert!(!screen.source_media_request_blocked("clip_a"));
    }

    #[test]
    fn source_media_request_blocker_retries_until_filmstrip_manifest_is_ready() {
        let mut screen = StoryScreen::story();
        screen.selected_clip_id = "clip_a".into();
        screen.waveform_clip_id = "clip_a".into();
        screen.selected_source_has_audio = true;
        screen.a1_peaks = vec![0.2, 0.4];
        screen.filmstrip_manifest_ready = false;
        screen.film_frames = vec![FilmFrame {
            index: 0,
            seek_sec: 0.0,
            url: "/api/story/thumbnail?clip_id=clip_a&frame_index=0".into(),
            texture: None,
            load_attempts: 0,
        }];

        assert!(!screen.source_media_request_blocked("clip_a"));
    }

    #[test]
    fn source_media_request_blocker_honors_retry_timer_and_retry_cap() {
        let mut screen = StoryScreen::story();
        screen.selected_clip_id = "clip_a".into();
        screen.selected_source_has_audio = true;
        assert!(!screen.source_media_request_blocked("clip_a"));

        screen.source_media_retry_at = Some(Instant::now() + SOURCE_MEDIA_RETRY_DELAY);
        assert!(screen.source_media_request_blocked("clip_a"));

        screen.source_media_retry_at = None;
        screen.source_media_retry_clip_id = "clip_a".into();
        screen.source_media_retry_attempts = SOURCE_MEDIA_MAX_RETRIES;
        assert!(screen.source_media_request_blocked("clip_a"));
    }

    #[test]
    fn story_state_prunes_stale_thumbnail_queue_entries() {
        let mut screen = StoryScreen::story();
        screen.thumbs_queued = vec!["old_clip".into(), "clip_a".into()];

        screen.apply_story_state(&story_state_with_selected_part("part_a"));

        assert_eq!(screen.thumbs_queued, vec!["clip_a"]);
    }

    #[test]
    fn thumbnail_queue_waits_when_image_loader_is_saturated() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        for i in 0..media_assets::IMAGE_ASSET_MAX_IN_FLIGHT {
            assert!(screen.image_loader.request(
                ImageAssetKey::new("stress", format!("clip_{i}"), "poster"),
                format!("http://127.0.0.1:9/{i}.jpg"),
                None,
            ));
        }
        screen.thumbs_queued = vec!["clip_a".into()];

        screen.pump_thumbs(
            &HostClient::new("http://127.0.0.1:8001"),
            &egui::Context::default(),
        );

        assert_eq!(screen.thumbs_queued, vec!["clip_a"]);
        assert_eq!(
            screen.image_loader.in_flight_len(),
            media_assets::IMAGE_ASSET_MAX_IN_FLIGHT
        );
    }

    #[test]
    fn filmstrip_queue_does_not_wait_for_thumbnail_loader_saturation() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        screen.selected_clip_id = "clip_a".into();
        for i in 0..media_assets::IMAGE_ASSET_MAX_IN_FLIGHT {
            assert!(screen.image_loader.request(
                ImageAssetKey::new("stress", format!("clip_{i}"), "poster"),
                format!("http://127.0.0.1:9/{i}.jpg"),
                None,
            ));
        }
        screen.film_frames = vec![FilmFrame {
            index: 0,
            seek_sec: 0.0,
            url: "http://127.0.0.1:9/film_0.jpg".into(),
            texture: None,
            load_attempts: 0,
        }];

        screen.pump_film_frames(&egui::Context::default());

        assert_eq!(
            screen.image_loader.in_flight_len(),
            media_assets::IMAGE_ASSET_MAX_IN_FLIGHT
        );
        assert_eq!(screen.filmstrip_image_loader.in_flight_len(), 1);
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
    fn playlist_header_transport_home_jumps_to_start() {
        let mut screen = StoryScreen::story();
        screen.view_mode = ViewMode::Wrap;
        screen.playlist = Some(two_part_playlist());
        screen.selected_part_id = "part_b".into();
        screen.wrap_playhead_frame = 60;
        let host = HostClient::new("http://127.0.0.1:1");

        let intent = screen.dispatch_marker_cover_action(
            &host,
            marker_cover_panel::MarkerCoverAction::PlaylistStart,
        );

        assert_eq!(screen.wrap_playhead_frame, 0);
        assert_eq!(screen.selected_part_id, "part_a");
        assert_eq!(intent, PlaybackTransportIntent::ScrubFrame(0));
    }

    #[test]
    fn playlist_header_transport_steps_slots_and_markers() {
        let mut screen = StoryScreen::story();
        screen.view_mode = ViewMode::Wrap;
        screen.playlist = Some(two_part_playlist());
        screen.marker_slots = vec![
            MarkerSlot {
                slot_id: "slot_a".into(),
                start_frame: 0,
                end_frame: 40,
                ..MarkerSlot::default()
            },
            MarkerSlot {
                slot_id: "slot_b".into(),
                start_frame: 40,
                end_frame: 80,
                ..MarkerSlot::default()
            },
            MarkerSlot {
                slot_id: "slot_c".into(),
                start_frame: 80,
                end_frame: 100,
                ..MarkerSlot::default()
            },
        ];
        screen.markers = vec![
            StoryMarker {
                marker_id: "locked_start".into(),
                timeline_frame: 0,
                ..StoryMarker::default()
            },
            StoryMarker {
                marker_id: "m_a".into(),
                timeline_frame: 25,
                ..StoryMarker::default()
            },
            StoryMarker {
                marker_id: "m_b".into(),
                timeline_frame: 75,
                ..StoryMarker::default()
            },
        ];
        screen.selected_slot_id = "slot_b".into();
        screen.wrap_playhead_frame = 60;
        let host = HostClient::new("http://127.0.0.1:1");

        let intent = screen.dispatch_marker_cover_action(
            &host,
            marker_cover_panel::MarkerCoverAction::PreviousMarkerSlot,
        );
        assert_eq!(screen.selected_slot_id, "slot_a");
        assert_eq!(screen.wrap_playhead_frame, 0);
        assert_eq!(intent, PlaybackTransportIntent::ScrubFrame(0));

        screen.selected_slot_id = "slot_b".into();
        screen.wrap_playhead_frame = 60;
        let intent = screen.dispatch_marker_cover_action(
            &host,
            marker_cover_panel::MarkerCoverAction::NextMarkerSlot,
        );
        assert_eq!(screen.selected_slot_id, "slot_c");
        assert_eq!(screen.wrap_playhead_frame, 80);
        assert_eq!(intent, PlaybackTransportIntent::ScrubFrame(80));

        screen.wrap_playhead_frame = 60;
        let intent = screen.dispatch_marker_cover_action(
            &host,
            marker_cover_panel::MarkerCoverAction::PreviousMarker,
        );
        assert_eq!(screen.selected_marker_id, "m_a");
        assert_eq!(screen.wrap_playhead_frame, 25);
        assert_eq!(intent, PlaybackTransportIntent::ScrubFrame(25));

        let intent = screen
            .dispatch_marker_cover_action(&host, marker_cover_panel::MarkerCoverAction::NextMarker);
        assert_eq!(screen.selected_marker_id, "m_b");
        assert_eq!(screen.wrap_playhead_frame, 75);
        assert_eq!(intent, PlaybackTransportIntent::ScrubFrame(75));
    }

    #[test]
    fn shortcut_selects_current_marker_slot_under_playhead() {
        let mut screen = StoryScreen::story();
        screen.view_mode = ViewMode::Wrap;
        screen.playlist = Some(two_part_playlist());
        screen.marker_slots = vec![
            MarkerSlot {
                slot_id: "slot_a".into(),
                start_frame: 0,
                end_frame: 40,
                ..MarkerSlot::default()
            },
            MarkerSlot {
                slot_id: "slot_b".into(),
                start_frame: 40,
                end_frame: 80,
                ..MarkerSlot::default()
            },
        ];
        screen.selected_slot_id.clear();
        screen.wrap_playhead_frame = 65;
        let host = HostClient::new("http://127.0.0.1:1");

        let intent = screen.dispatch_playback_action(
            &host,
            playback_controls::PlaybackAction::SelectCurrentMarkerSlot,
        );

        assert_eq!(screen.selected_slot_id, "slot_b");
        assert_eq!(screen.wrap_playhead_frame, 65);
        assert_eq!(intent, PlaybackTransportIntent::None);
    }

    #[test]
    fn shortcut_navigates_active_program_object_kind() {
        let mut screen = StoryScreen::story();
        screen.view_mode = ViewMode::Wrap;
        screen.panel_focus = PanelFocus::SegmentPanel;
        screen.playlist = Some(two_part_playlist());
        screen.marker_slots = vec![
            MarkerSlot {
                slot_id: "slot_a".into(),
                start_frame: 0,
                end_frame: 50,
                ..MarkerSlot::default()
            },
            MarkerSlot {
                slot_id: "slot_b".into(),
                start_frame: 50,
                end_frame: 100,
                ..MarkerSlot::default()
            },
        ];
        screen.markers = vec![
            StoryMarker {
                marker_id: "marker_a".into(),
                timeline_frame: 25,
                ..StoryMarker::default()
            },
            StoryMarker {
                marker_id: "marker_b".into(),
                timeline_frame: 75,
                ..StoryMarker::default()
            },
        ];
        screen.selected_part_id.clear();
        screen.selected_slot_id.clear();
        screen.selected_marker_id.clear();
        screen.wrap_playhead_frame = 0;
        let host = HostClient::new("http://127.0.0.1:1");

        let intent = screen
            .dispatch_playback_action(&host, playback_controls::PlaybackAction::NavigateNextObject);

        assert_eq!(screen.selected_part_id, "part_b");
        assert_eq!(screen.selected_slot_id, "");
        assert_eq!(screen.wrap_playhead_frame, 50);
        assert_eq!(intent, PlaybackTransportIntent::ScrubFrame(50));

        screen.selected_part_id.clear();
        screen.selected_slot_id = "slot_a".into();
        screen.wrap_playhead_frame = 0;
        let intent = screen
            .dispatch_playback_action(&host, playback_controls::PlaybackAction::NavigateNextObject);

        assert_eq!(screen.selected_slot_id, "slot_b");
        assert_eq!(screen.wrap_playhead_frame, 50);
        assert_eq!(intent, PlaybackTransportIntent::ScrubFrame(50));

        screen.selected_slot_id.clear();
        screen.selected_marker_id = "marker_a".into();
        screen.wrap_playhead_frame = 25;
        let intent = screen
            .dispatch_playback_action(&host, playback_controls::PlaybackAction::NavigateNextObject);

        assert_eq!(screen.selected_marker_id, "marker_b");
        assert_eq!(screen.selected_slot_id, "");
        assert_eq!(screen.wrap_playhead_frame, 75);
        assert_eq!(intent, PlaybackTransportIntent::ScrubFrame(75));
    }

    #[test]
    fn quick_cover_does_not_fallback_to_empty_slot_when_selected_slot_is_filled() {
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
                start_frame: 10,
                end_frame: 40,
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
        screen.selected_source_timebase = source_tb50();
        screen.source_timebase_ready = true;

        let intent = screen.dispatch_playback_action(
            &HostClient::new("http://127.0.0.1:1"),
            playback_controls::PlaybackAction::QuickCover,
        );

        assert_eq!(intent, PlaybackTransportIntent::None);
        assert!(screen.drain_backend_commands().is_empty());
        assert_eq!(screen.selected_slot_id, "slot_a");
        assert!(screen.covers.is_empty());
        assert_eq!(screen.status, "Odabrani marker slot već ima pokrivalicu");
    }

    #[test]
    fn pending_cover_projection_survives_stale_state_until_backend_confirms() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        screen.loaded_project_id = "p".into();
        screen.selected_slot_id = "slot_a".into();
        screen.marker_slots = vec![MarkerSlot {
            slot_id: "slot_a".into(),
            start_frame: 10,
            end_frame: 40,
            has_cover: false,
            ..MarkerSlot::default()
        }];
        screen.selected_clip_id = "clip_a".into();
        screen.source_in_frame = 100;
        screen.source_out_frame = 150;
        screen.mark_in_set = true;
        screen.mark_out_set = true;
        screen.selected_source_fps = 50.0;
        screen.selected_source_timebase = source_tb50();
        screen.source_timebase_ready = true;

        let _ = screen.dispatch_playback_action(
            &HostClient::new("http://127.0.0.1:1"),
            playback_controls::PlaybackAction::QuickCover,
        );
        screen.apply_story_state(&story_state_with_selected_part("part_a"));

        assert_eq!(screen.covers.len(), 1);
        assert_eq!(screen.covers[0].slot_id, "slot_a");
        assert!(screen
            .selected_cover_id
            .starts_with("pending-cover:slot_a:clip_a"));
    }

    #[test]
    fn pending_cover_projection_rolls_back_on_backend_error() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        screen.loaded_project_id = "p".into();
        screen.story_state_snapshot = Some(story_state_with_selected_part("part_a"));
        screen.selected_slot_id = "slot_a".into();
        screen.marker_slots = vec![MarkerSlot {
            slot_id: "slot_a".into(),
            start_frame: 10,
            end_frame: 40,
            has_cover: false,
            ..MarkerSlot::default()
        }];
        screen.selected_clip_id = "clip_a".into();
        screen.source_in_frame = 100;
        screen.source_out_frame = 150;
        screen.mark_in_set = true;
        screen.mark_out_set = true;
        screen.selected_source_fps = 50.0;
        screen.selected_source_timebase = source_tb50();
        screen.source_timebase_ready = true;

        let _ = screen.dispatch_playback_action(
            &HostClient::new("http://127.0.0.1:1"),
            playback_controls::PlaybackAction::QuickCover,
        );
        screen.set_editorial_edit_error("p", EditorialEditKind::CreateCover, "cover failed");

        assert!(screen.covers.is_empty());
        assert!(screen.pending_cover_projections.is_empty());
        assert_eq!(screen.status, "cover failed");
    }

    #[test]
    fn ctrl_z_undo_deletes_selected_cover_object() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        screen.loaded_project_id = "p".into();
        screen.apply_editorial_edit_data(EditorialEditData {
            instance_id: "story".into(),
            project_id: "p".into(),
            kind: EditorialEditKind::SelectCover,
            detail: "cover_a".into(),
            state: story_state_with_cover("slot_a", "cover_a"),
        });
        let _ = screen.drain_backend_commands();

        let intent = screen.dispatch_playback_action(
            &HostClient::new("http://127.0.0.1:1"),
            playback_controls::PlaybackAction::UndoObject,
        );

        assert_eq!(intent, PlaybackTransportIntent::None);
        let commands = screen.drain_backend_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].path, "/api/story/object/undo");
        assert_eq!(
            commands[0]
                .payload
                .as_ref()
                .and_then(|payload| payload.get("object_type"))
                .and_then(Value::as_str),
            Some("cover")
        );
        assert_eq!(
            commands[0]
                .payload
                .as_ref()
                .and_then(|payload| payload.get("object_id"))
                .and_then(Value::as_str),
            Some("cover_a")
        );
        assert!(screen.covers.is_empty());
        assert_eq!(screen.selected_cover_id, "");

        let intent = screen.dispatch_playback_action(
            &HostClient::new("http://127.0.0.1:1"),
            playback_controls::PlaybackAction::RedoObject,
        );

        assert_eq!(intent, PlaybackTransportIntent::None);
        let commands = screen.drain_backend_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].path, "/api/story/object/redo");
        assert_eq!(
            commands[0]
                .payload
                .as_ref()
                .and_then(|payload| payload.get("object_type"))
                .and_then(Value::as_str),
            Some("cover")
        );
        assert_eq!(
            commands[0]
                .payload
                .as_ref()
                .and_then(|payload| payload.get("object_id"))
                .and_then(Value::as_str),
            Some("cover_a")
        );
    }

    #[test]
    fn ctrl_z_pending_cover_object_deletes_after_backend_confirmation() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        screen.loaded_project_id = "p".into();
        screen.selected_slot_id = "slot_a".into();
        screen.marker_slots = vec![MarkerSlot {
            slot_id: "slot_a".into(),
            start_frame: 10,
            end_frame: 40,
            has_cover: false,
            ..MarkerSlot::default()
        }];
        screen.selected_clip_id = "clip_a".into();
        screen.source_in_frame = 100;
        screen.source_out_frame = 150;
        screen.mark_in_set = true;
        screen.mark_out_set = true;
        screen.selected_source_fps = 50.0;
        screen.selected_source_timebase = source_tb50();
        screen.source_timebase_ready = true;

        let _ = screen.dispatch_playback_action(
            &HostClient::new("http://127.0.0.1:1"),
            playback_controls::PlaybackAction::QuickCover,
        );
        let _ = screen.drain_backend_commands();
        assert_eq!(screen.covers.len(), 1);
        assert!(is_pending_cover_id(&screen.selected_cover_id));

        let _ = screen.dispatch_playback_action(
            &HostClient::new("http://127.0.0.1:1"),
            playback_controls::PlaybackAction::UndoObject,
        );
        assert!(screen.covers.is_empty());
        assert!(screen.drain_backend_commands().is_empty());

        screen.apply_editorial_edit_data(EditorialEditData {
            instance_id: "story".into(),
            project_id: "p".into(),
            kind: EditorialEditKind::CreateCover,
            detail: "slot_a".into(),
            state: story_state_with_cover("slot_a", "cover_a"),
        });

        let commands = screen.drain_backend_commands();
        let undo = commands
            .iter()
            .find(|command| command.path == "/api/story/object/undo")
            .expect("undo command");
        assert_eq!(
            undo.payload
                .as_ref()
                .and_then(|payload| payload.get("object_id"))
                .and_then(Value::as_str),
            Some("cover_a")
        );
        assert!(screen.covers.is_empty());
    }

    #[test]
    fn source_selection_locks_source_timeline_until_wrap_entry() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        let source_shot = StoryShot {
            shot_id: "shot_a".into(),
            clip_id: "clip_a".into(),
            fps: 50.0,
            source_timebase: tb50(),
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
    fn source_dock_keyboard_focus_routes_frame_step_to_source_timeline() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        let source_shot = StoryShot {
            shot_id: "clip_a".into(),
            clip_id: "clip_a".into(),
            fps: 50.0,
            source_timebase: tb50(),
            duration_frames: 250,
            out_frame: 250,
            play_path: "C:/qnc/proxy/clip_a.mp4".into(),
            ..StoryShot::default()
        };
        screen.all_clips = vec![source_shot.clone()];
        screen.select_shot_from_snapshot(&source_shot);
        screen.view_mode = ViewMode::Wrap;
        screen.source_dock_keyboard_focus = true;
        screen.wrap_playhead_frame = 60;
        screen.source_playhead_frame = 10;
        let playback = PlaybackStack::new();

        let intent = screen.step_focus(&HostClient::new("http://127.0.0.1:1"), &playback, 1);

        assert_eq!(screen.view_mode, ViewMode::Source);
        assert_eq!(screen.source_playhead_frame, 11);
        assert_eq!(screen.wrap_playhead_frame, 60);
        assert_eq!(intent, PlaybackTransportIntent::CueFrame(11));
    }

    #[test]
    fn source_dock_keyboard_focus_routes_up_down_to_source_segments() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        screen.playlist = Some(two_part_playlist());
        let source_shot = StoryShot {
            shot_id: "clip_a".into(),
            clip_id: "clip_a".into(),
            fps: 50.0,
            source_timebase: tb50(),
            duration_frames: 250,
            out_frame: 250,
            play_path: "C:/qnc/proxy/clip_a.mp4".into(),
            ..StoryShot::default()
        };
        screen.all_clips = vec![source_shot.clone()];
        screen.virtual_shots = vec![
            StoryShot {
                shot_id: "virt_a".into(),
                clip_id: "clip_a".into(),
                fps: 50.0,
                in_frame: 0,
                out_frame: 40,
                duration_frames: 40,
                ..StoryShot::default()
            },
            StoryShot {
                shot_id: "virt_b".into(),
                clip_id: "clip_a".into(),
                fps: 50.0,
                in_frame: 40,
                out_frame: 80,
                duration_frames: 40,
                ..StoryShot::default()
            },
        ];
        screen.select_shot_from_snapshot(&source_shot);
        screen.view_mode = ViewMode::Wrap;
        screen.source_dock_keyboard_focus = true;
        screen.source_playhead_frame = 0;
        screen.wrap_playhead_frame = 60;

        let intent = screen.dispatch_playback_action(
            &HostClient::new("http://127.0.0.1:1"),
            playback_controls::PlaybackAction::StepNextPart,
        );

        assert_eq!(screen.view_mode, ViewMode::Source);
        assert_eq!(screen.selected_shot_id, "virt_b");
        assert_eq!(screen.source_in_frame, 40);
        assert_eq!(screen.source_playhead_frame, 40);
        assert_eq!(screen.wrap_playhead_frame, 60);
        assert_eq!(intent, PlaybackTransportIntent::CueFrame(40));
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
        assert!(screen.state_loaded);
        assert!(!screen.timeline_loaded);
        assert!(!screen.playlist_loaded);
        assert!(!screen.meta_ready());
        assert_eq!(screen.library_tab, LibraryTab::All);
        assert!(screen.selected_source_ref.is_none());

        let timeline_intent =
            screen.apply_editorial_timeline_model("p", timeline_with_part("part_new"));
        assert_eq!(timeline_intent, PlaybackTransportIntent::None);

        seed_playlist_playback_inputs(&mut screen, &["clip_a"]);
        let playlist_intent = screen.apply_editorial_playlist("p", playlist_with_part("part_new"));

        assert!(matches!(
            playlist_intent,
            PlaybackTransportIntent::PreloadProgram(_)
        ));
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
        assert!(screen.state_loaded);
        assert!(!screen.timeline_loaded);
        assert!(!screen.playlist_loaded);
        assert!(!screen.meta_ready());
        assert_eq!(screen.selected_part_id, "part_remaining");

        let timeline_intent =
            screen.apply_editorial_timeline_model("p", timeline_with_part("part_remaining"));
        assert_eq!(timeline_intent, PlaybackTransportIntent::None);

        seed_playlist_playback_inputs(&mut screen, &["clip_a"]);
        let playlist_intent =
            screen.apply_editorial_playlist("p", playlist_with_part("part_remaining"));

        assert!(matches!(
            playlist_intent,
            PlaybackTransportIntent::PreloadProgram(_)
        ));
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

    #[test]
    fn tab_cycles_keyboard_panel_focus() {
        let mut screen = StoryScreen::story();
        let host = HostClient::new("http://127.0.0.1:1");

        let intent = screen.focus_adjacent_panel(&host, 1);
        assert_eq!(screen.panel_focus, PanelFocus::SegmentPanel);
        assert_eq!(screen.view_mode, ViewMode::Wrap);
        assert_eq!(intent, PlaybackTransportIntent::None);

        let intent = screen.focus_adjacent_panel(&host, 1);
        assert_eq!(screen.panel_focus, PanelFocus::SourceTimeline);
        assert_eq!(screen.view_mode, ViewMode::Source);
        assert_eq!(intent, PlaybackTransportIntent::None);

        let intent = screen.focus_adjacent_panel(&host, 1);
        assert_eq!(screen.panel_focus, PanelFocus::MediaPool);
        assert_eq!(intent, PlaybackTransportIntent::None);
    }

    #[test]
    fn media_pool_focus_arrows_do_not_open_source_or_move_playhead() {
        let mut screen = StoryScreen::story();
        screen.all_clips = vec![
            StoryShot {
                shot_id: "shot_a".into(),
                clip_id: "clip_a".into(),
                name: "A".into(),
                ..StoryShot::default()
            },
            StoryShot {
                shot_id: "shot_b".into(),
                clip_id: "clip_b".into(),
                name: "B".into(),
                ..StoryShot::default()
            },
        ];
        screen.library_tab = LibraryTab::All;
        screen.panel_focus = PanelFocus::MediaPool;
        screen.focused_media_shot_id = "shot_a".into();
        screen.selected_shot_id = "shot_a".into();
        screen.selected_clip_id = "clip_a".into();
        screen.source_playhead_frame = 20;
        let playback = PlaybackStack::new();

        let intent = screen.step_focus(&HostClient::new("http://127.0.0.1:1"), &playback, 1);

        assert_eq!(intent, PlaybackTransportIntent::None);
        assert_eq!(screen.focused_media_shot_id, "shot_b");
        assert_eq!(screen.selected_shot_id, "shot_a");
        assert_eq!(screen.selected_clip_id, "clip_a");
        assert_eq!(screen.source_playhead_frame, 20);
    }

    #[test]
    fn enter_activates_focused_media_pool_clip() {
        let mut screen = StoryScreen::story();
        screen.project_id = "p".into();
        screen.all_clips = vec![
            StoryShot {
                shot_id: "shot_a".into(),
                clip_id: "clip_a".into(),
                fps: 50.0,
                duration_frames: 100,
                out_frame: 100,
                play_path: "C:/qnc/proxy/a.mp4".into(),
                ..StoryShot::default()
            },
            StoryShot {
                shot_id: "shot_b".into(),
                clip_id: "clip_b".into(),
                fps: 50.0,
                duration_frames: 200,
                out_frame: 200,
                play_path: "C:/qnc/proxy/b.mp4".into(),
                ..StoryShot::default()
            },
        ];
        screen.library_tab = LibraryTab::All;
        screen.panel_focus = PanelFocus::MediaPool;
        screen.focused_media_shot_id = "shot_b".into();

        let intent = screen.dispatch_playback_action(
            &HostClient::new("http://127.0.0.1:1"),
            playback_controls::PlaybackAction::ActivateFocusedItem,
        );

        assert_eq!(screen.panel_focus, PanelFocus::SourceTimeline);
        assert_eq!(screen.view_mode, ViewMode::Source);
        assert_eq!(screen.selected_shot_id, "shot_b");
        assert_eq!(screen.selected_clip_id, "clip_b");
        assert_eq!(screen.source_playhead_frame, 0);
        assert_eq!(intent, PlaybackTransportIntent::CueFrame(0));
    }
}
