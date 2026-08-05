//! Native editorial form (Story / Media Assist) — one screen, role attributes.
//!
//! UI blocks (`qnc_ui`, `editorial::*`, `qnc_source_dock`, cards) are shared
//! components. This form only chooses composition via `EditorialRole`.

mod async_media;
mod focus;
pub(crate) mod playback_controls;
mod playback_runtime;
mod preview_monitor;
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

use crate::api::{HostClient, TimelineModel};
use crate::composition::EditorialRole;
use crate::editorial::common::{shot_id, truncate};
use crate::editorial::{marker_cover_panel, media_pool, segment_panel};
use crate::frame_time::{
    frame_to_seconds, normalize_fps, seconds_to_frame, seconds_to_timecode,
    snap_seconds_to_frame, DEFAULT_FPS,
};
use crate::player_contract::FrameNumber;
use crate::playback_stack::PlaybackStack;
use crate::player_contract::BroadcastHostSourceRef;
use crate::qnc_filmstrip_background::FilmFrame;
use crate::qnc_timeline::{ExpandedAudio, TimelineFocusPaint};
use crate::shortcuts::{load_story_bindings, StoryBindings};

use self::focus::{FocusTarget, TimelineFocus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Wrap,
    Source,
}

pub use crate::player_bridge::PlaybackCommand as StoryPlaybackCommand;

/// Shared Story / Media Assist form — configure with [`EditorialRole`].
pub struct StoryScreen {
    /// Composition attributes (head tabs, card facets, …) — not a code fork.
    role: EditorialRole,
    project_id: String,
    loaded_project_id: String,
    timeline: Option<TimelineModel>,
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
    library_tab: LibraryTab,
    view_mode: ViewMode,
    source_in: f64,
    source_out: f64,
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
    virtual_sec: f64,
    /// Frame playhead — synced from player / carrier (timeline authority).
    playhead_frame: i64,
    playing: bool,
    layer: String,
    status: String,
    /// True while broadcast PlayerRemote owns the monitor.
    broadcast_preview_active: bool,
    thumb_textures: HashMap<String, TextureHandle>,
    thumbs_queued: Vec<String>,
    a1_peaks: Vec<f32>,
    a2_peaks: Vec<f32>,
    film_frames: Vec<FilmFrame>,
    waveform_clip_id: String,
    image_loader: async_media::AsyncImageLoader,
    source_media_loader: async_media::AsyncSourceMediaLoader,
    repaint_ctx: Option<egui::Context>,
    source_media_retry_at: Option<Instant>,
    /// Pending frame target while the player cue catches up.
    source_timebase_ready: bool,
    pending_playback_commands: Vec<StoryPlaybackCommand>,
    /// Keyboard chords from host catalog + DB (or local file / builtin).
    bindings: StoryBindings,
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

    pub fn role(&self) -> EditorialRole {
        self.role
    }

    fn with_role(role: EditorialRole) -> Self {
        Self {
            role,
            project_id: String::new(),
            loaded_project_id: String::new(),
            timeline: None,
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
            library_tab: LibraryTab::All,
            view_mode: ViewMode::Source,
            source_in: 0.0,
            source_out: 0.0,
            mark_in_set: false,
            mark_out_set: false,
            selected_source_fps: 0.0,
            selected_source_has_audio: false,
            selected_source_audio_channels: 0,
            selected_source_ref: None,
            selected_play_path: String::new(),
            story_summary: String::new(),
            draft_status: "draft".into(),
            virtual_sec: 0.0,
            playhead_frame: 0,
            playing: false,
            layer: String::new(),
            status: role.idle_status().into(),
            broadcast_preview_active: false,
            thumb_textures: HashMap::new(),
            thumbs_queued: Vec::new(),
            a1_peaks: Vec::new(),
            a2_peaks: Vec::new(),
            film_frames: Vec::new(),
            waveform_clip_id: String::new(),
            image_loader: async_media::AsyncImageLoader::new(),
            source_media_loader: async_media::AsyncSourceMediaLoader::new(),
            repaint_ctx: None,
            source_media_retry_at: None,
            source_timebase_ready: false,
            pending_playback_commands: Vec::new(),
            bindings: StoryBindings::empty(),
            focus: TimelineFocus::default(),
            expanded_audio: ExpandedAudio::None,
        }
    }

    fn timeline_fps(&self) -> f64 {
        self.timeline
            .as_ref()
            .map(|t| t.timeline_fps)
            .filter(|fps| fps.is_finite() && *fps > 0.0)
            .unwrap_or(DEFAULT_FPS)
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

    fn source_tc(&self, sec: f64) -> String {
        self.source_timebase_fps()
            .map(|fps| seconds_to_timecode(sec, fps))
            .unwrap_or_else(|| "--:--:--:--".into())
    }

    fn snap_source_sec(&self, sec: f64) -> f64 {
        self.source_timebase_fps()
            .map(|fps| snap_seconds_to_frame(sec, fps))
            .unwrap_or_else(|| sec.max(0.0))
    }

    fn snap_sec(&self, sec: f64) -> f64 {
        snap_seconds_to_frame(sec, self.timeline_fps())
    }

    fn tc(&self, sec: f64) -> String {
        seconds_to_timecode(sec, self.timeline_fps())
    }

    fn frame_step(&self) -> f64 {
        1.0 / self.timeline_fps().max(1.0)
    }

    pub fn ensure_loaded(&mut self, host: &HostClient, project_id: &str) {
        if self.loaded_project_id == project_id {
            return;
        }
        self.reset_session(host);
        self.project_id = project_id.to_string();
        self.loaded_project_id = project_id.to_string();
        self.reload_shortcuts(host);
        self.reload_meta(host);
        // Classic Story opens on source/All — not empty wrap.
        if let Some(first) = self.all_clips.first().cloned() {
            self.select_shot(host, &first);
        } else {
            self.start_wrap_session(host);
        }
    }

    pub fn reset_session(&mut self, _host: &HostClient) {
        *self = Self::with_role(self.role);
    }

    fn reload_shortcuts(&mut self, host: &HostClient) {
        self.bindings = load_story_bindings(host, "storyboard");
    }

    fn start_wrap_session(&mut self, host: &HostClient) {
        self.start_wrap_session_for_part(host, None);
    }

    /// Enter Wrap UI — editorial only. Preview stays on broadcast PlayerRemote.
    fn start_wrap_session_for_part(&mut self, host: &HostClient, selected_part_id: Option<String>) {
        self.view_mode = ViewMode::Wrap;
        self.playing = false;
        if let Some(part_id) = selected_part_id.filter(|id| !id.trim().is_empty()) {
            match host.story_part_select(&self.project_id, &part_id) {
                Ok(state) => self.apply_story_state(&state),
                Err(e) => {
                    self.status = e;
                    return;
                }
            }
        }
        self.status = "Wrap · broadcast".into();
        if self.selected_source_ref.is_some() && !self.selected_play_path.trim().is_empty() {
            self.queue_pause_and_seek();
        }
    }

    fn activate_source_ui(&mut self, host: &HostClient, clip_id: &str) {
        self.activate_source_ui_for_shot(host, clip_id, None, None);
    }

    fn activate_source_ui_for_shot(
        &mut self,
        host: &HostClient,
        clip_id: &str,
        selected_shot_id: Option<String>,
        play_path_hint: Option<&str>,
    ) {
        if clip_id.trim().is_empty() {
            self.status = "Nema clip_id".into();
            return;
        }
        self.view_mode = ViewMode::Source;
        self.selected_clip_id = clip_id.to_string();
        if let Some(shot_id) = selected_shot_id {
            self.selected_shot_id = shot_id;
        }
        self.playing = false;

        // Prefer path from Story snapshot (no extra round-trip); refresh via play-media.
        let hint = play_path_hint.unwrap_or("").trim();
        if !hint.is_empty() {
            self.selected_play_path = hint.to_string();
        } else if let Some(path) = self
            .all_clips
            .iter()
            .chain(self.virtual_shots.iter())
            .find(|c| c.clip_id == clip_id)
            .map(|c| c.play_path.trim().to_string())
            .filter(|p| !p.is_empty())
        {
            self.selected_play_path = path;
        } else {
            self.selected_play_path.clear();
        }
        match host.story_play_media(&self.project_id, clip_id) {
            Ok(media) if !media.path.trim().is_empty() => {
                self.selected_play_path = media.path;
                self.status = format!("Source · {} ({})", clip_id, media.kind);
            }
            Ok(_) if !self.selected_play_path.is_empty() => {
                self.status = format!("Source · {clip_id} (snapshot path)");
            }
            Ok(_) => {
                self.selected_play_path.clear();
                self.status = format!("Proxy path prazan · {clip_id}");
            }
            Err(err) if !self.selected_play_path.is_empty() => {
                self.status = format!("Source · {clip_id}");
                let _ = err;
            }
            Err(err) => {
                self.selected_play_path.clear();
                self.status = err;
            }
        }
        self.source_timebase_ready =
            self.selected_source_fps.is_finite() && self.selected_source_fps > 0.0;
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

    fn reload_meta(&mut self, host: &HostClient) {
        match host.timeline_model(&self.project_id) {
            Ok(m) => self.timeline = Some(m),
            Err(e) => {
                self.timeline = None;
                self.status = format!("timeline: {e}");
            }
        }
        match host.story_state(&self.project_id) {
            Ok(state) => self.apply_story_state(&state),
            Err(e) => self.status = format!("story state: {e}"),
        }
    }

    fn after_edit(&mut self, host: &HostClient, state: Value) {
        self.apply_story_state(&state);
        if let Ok(m) = host.timeline_model(&self.project_id) {
            self.timeline = Some(m);
            self.refresh_story_summary();
        }
    }

    fn select_shot(&mut self, host: &HostClient, shot: &StoryShot) {
        let selection = match story_selection::shot_selection(&self.project_id, shot) {
            Ok(selection) => selection,
            Err(err) => {
                self.status = err.message;
                return;
            }
        };
        self.selected_shot_id = selection.shot_id.clone();
        self.selected_clip_id = selection.clip_id.clone();
        self.source_in = selection.source_in;
        self.source_out = selection.source_out;
        // Loaded shot range is not an explicit Mark I/O — require I/O keys first.
        self.mark_in_set = false;
        self.mark_out_set = false;
        self.focus.clear();
        self.selected_source_ref = Some(selection.source_ref);
        self.selected_source_fps = shot.fps;
        self.selected_source_has_audio = shot.has_audio;
        self.selected_source_audio_channels = shot.audio_channels;
        self.source_timebase_ready = shot.fps.is_finite() && shot.fps > 0.0;
        self.virtual_sec = self.snap_source_sec(selection.source_in);
        if self.waveform_clip_id != selection.clip_id {
            self.a1_peaks.clear();
            self.a2_peaks.clear();
            self.film_frames.clear();
            self.waveform_clip_id.clear();
        }
        if !selection.clip_id.is_empty() {
            let play_hint = shot.play_path.as_str();
            self.activate_source_ui_for_shot(
                host,
                &selection.clip_id,
                Some(selection.shot_id),
                Some(play_hint),
            );
            self.request_source_media(host);
            self.queue_seek_to_playhead();
        }
    }

    fn queue_seek_to_playhead(&mut self) {
        if self.selected_source_ref.is_some() {
            let frame = FrameNumber(seconds_to_frame(
                self.virtual_sec,
                self.source_timebase_fps().unwrap_or(DEFAULT_FPS),
            ));
            self.pending_playback_commands
                .push(StoryPlaybackCommand::CueFrame(frame));
        }
    }

    fn queue_pause_and_seek(&mut self) {
        if self.selected_source_ref.is_some() {
            let frame = FrameNumber(seconds_to_frame(
                self.virtual_sec,
                self.source_timebase_fps().unwrap_or(DEFAULT_FPS),
            ));
            self.pending_playback_commands
                .push(StoryPlaybackCommand::ScrubFrame(frame));
        }
    }

    fn schedule_native_seek(&mut self) {
        self.queue_pause_and_seek();
    }

    fn schedule_native_seek_io(&mut self) {
        self.queue_seek_to_playhead();
    }

    fn frame_eps_sec(&self) -> f64 {
        self.source_timebase_fps()
            .map(|fps| 0.5 / fps.max(1.0))
            .unwrap_or(0.02)
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

    fn poll_async_media(&mut self, _host: &HostClient, ctx: &egui::Context) {
        for result in self.image_loader.poll() {
            match result.key {
                async_media::ImageKey::Thumb { clip_id } => {
                    if let Ok(color) = result.image {
                        let tex = ctx.load_texture(
                            format!("thumb_{clip_id}"),
                            color,
                            TextureOptions::LINEAR,
                        );
                        self.thumb_textures.insert(clip_id, tex);
                    }
                }
                async_media::ImageKey::Film { clip_id, index } => {
                    if clip_id != self.selected_clip_id {
                        continue;
                    }
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
                    self.film_frames = media
                        .film_frames
                        .into_iter()
                        .map(|(index, seek_sec, url)| FilmFrame {
                            index,
                            seek_sec,
                            url,
                            texture: None,
                        })
                        .collect();
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
                async_media::ImageKey::Film {
                    clip_id: self.selected_clip_id.clone(),
                    index: frame.index,
                },
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
        let url = host.story_thumbnail_url(&self.project_id, &clip_id, 0.0);
        let _ = self.image_loader.request(
            async_media::ImageKey::Thumb { clip_id },
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
        playback: &mut crate::playback_stack::PlaybackStack,
    ) {
        if self.bindings.by_action.is_empty() {
            self.reload_shortcuts(host);
        }
        // Keys come only from seed/DB keyboard-shortcuts (storyboard scope).
        for action in playback_controls::shortcut_actions(ctx, &self.bindings) {
            match action {
                playback_controls::PlaybackAction::SeekFrames(frames) => {
                    self.step_focus(host, playback, frames);
                }
                other => self.dispatch_playback_action(host, other),
            }
        }
    }

    pub fn drain_playback_commands(&mut self) -> Vec<StoryPlaybackCommand> {
        <Self as crate::player_bridge::PlayerClient>::drain_playback_commands(self)
    }

    /// Drop UI playhead lock so play is not fighting a scrub target.
    pub fn clear_pending_seeks(&mut self) {
        <Self as crate::player_bridge::PlayerClient>::clear_pending_seeks(self)
    }

    pub fn playback_is_playing(&self) -> bool {
        self.playing
    }

    pub fn playback_source_ref(&self) -> Option<&BroadcastHostSourceRef> {
        <Self as crate::player_bridge::PlayerClient>::playback_source_ref(self)
    }

    pub fn playback_source_fps(&self) -> f64 {
        <Self as crate::player_bridge::PlayerClient>::playback_source_fps(self)
    }

    pub fn playback_source_has_audio(&self) -> bool {
        <Self as crate::player_bridge::PlayerClient>::playback_source_has_audio(self)
    }

    pub fn playback_source_audio_channels(&self) -> u8 {
        <Self as crate::player_bridge::PlayerClient>::playback_source_audio_channels(self)
    }

    pub fn playback_source_sec(&self) -> f64 {
        self.virtual_sec
    }

    /// Local disk path for native ffmpeg (never an HTTP virtual-stream URL).
    pub fn playback_media_path(&self) -> Option<String> {
        <Self as crate::player_bridge::PlayerClient>::playback_media_path(self)
    }

    pub fn playback_source_range_sec(&self) -> (f64, f64) {
        <Self as crate::player_bridge::PlayerClient>::playback_source_range_sec(self)
    }

    /// Marked IN/OUT for save / virtual shot (not the decode window).
    pub fn marked_range_sec(&self) -> (f64, f64) {
        (
            self.source_in.max(0.0),
            self.source_out.max(self.source_in + 0.04),
        )
    }

    pub fn set_player_preview_active(&mut self, active: bool) {
        <Self as crate::player_bridge::PlayerClient>::set_player_preview_active(self, active)
    }

    pub fn apply_player_frame(&mut self, image: ColorImage, source_sec: f64, playing: bool) {
        <Self as crate::player_bridge::PlayerClient>::apply_player_frame(
            self, image, source_sec, playing,
        )
    }

    pub fn apply_player_state(
        &mut self,
        source_sec: f64,
        playing: bool,
        status: impl Into<String>,
    ) {
        <Self as crate::player_bridge::PlayerClient>::apply_player_state(
            self, source_sec, playing, status,
        )
    }

    pub fn apply_player_error(&mut self, status: impl Into<String>) {
        <Self as crate::player_bridge::PlayerClient>::apply_player_error(self, status)
    }

    pub fn apply_playback_command_state(&mut self, playing: bool, status: impl Into<String>) {
        <Self as crate::player_bridge::PlayerClient>::apply_playback_command_state(
            self, playing, status,
        )
    }

    pub fn prepare_frame(&mut self, host: &HostClient, ctx: &egui::Context) {
        self.repaint_ctx = Some(ctx.clone());
        self.poll_async_media(host, ctx);
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
        let dur = self.selected_clip_duration().max(0.04);
        source_editor::dock_height(self.expanded_audio, dur)
    }

    pub fn ui_source_dock(
        &mut self,
        ui: &mut egui::Ui,
        host: &HostClient,
        playback: &mut PlaybackStack,
    ) {
        self.ui_source_editor(ui, host, self.source_dock_height(), playback);
    }

    /// Central workspace — composed from `qnc_ui` (Story is the reference form).
    pub fn ui_main(
        &mut self,
        ui: &mut egui::Ui,
        host: &HostClient,
        _ctx: &egui::Context,
        playback: &PlaybackStack,
    ) {
        crate::qnc_ui::editorial_shell(ui, |ui, m, side| match side {
            crate::qnc_ui::ShellSide::Left => {
                crate::qnc_ui::media_column_monitor(
                    ui,
                    m,
                    |ui, preview_h| {
                        playback.show_monitor(ui, preview_h, "Odaberi klip");
                    },
                    |ui, _rest| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.allocate_ui(Vec2::new(m.left_w, crate::qnc_ui::space::CHROME_H), |ui| {
                            self.ui_pool_head(ui, host);
                        });
                        let body = ui.available_height().max(0.0);
                        self.ui_filmstrip_web(ui, host, body);
                    },
                );
            }
            crate::qnc_ui::ShellSide::Right => match self.role.composition().right {
                crate::composition::RightPanelKind::SegmentPanel => {
                    self.ui_segmenti_panel(ui, host, m.height);
                }
                crate::composition::RightPanelKind::None
                | crate::composition::RightPanelKind::ClipGrid
                | crate::composition::RightPanelKind::TemplateSettings => {}
            },
        });
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

    fn dispatch_media_pool(&mut self, host: &HostClient, action: media_pool::MediaPoolAction) {
        match action {
            media_pool::MediaPoolAction::None => {}
            media_pool::MediaPoolAction::SwitchTab(tab) => {
                // Honor composition: no Segment tab → clamp away.
                let tab = if tab == LibraryTab::Segment && !self.role.head().show_segment_tab {
                    LibraryTab::All
                } else {
                    tab
                };
                self.library_tab = tab;
                if tab == LibraryTab::Segment {
                    self.start_wrap_session(host);
                }
            }
            media_pool::MediaPoolAction::SelectShot(shot) => self.select_shot(host, &shot),
            media_pool::MediaPoolAction::SelectPart(part_id) => {
                self.selected_part_id = part_id.clone();
                self.start_wrap_session_for_part(host, Some(part_id));
            }
            media_pool::MediaPoolAction::DeletePart(part_id) => {
                if let Ok(st) = host.story_part_delete(&self.project_id, &part_id) {
                    self.after_edit(host, st);
                    self.start_wrap_session(host);
                }
            }
            media_pool::MediaPoolAction::TogglePlay => {
                self.dispatch_playback_action(host, playback_controls::PlaybackAction::TogglePlay);
            }
            media_pool::MediaPoolAction::MarkIn => {
                self.dispatch_playback_action(host, playback_controls::PlaybackAction::MarkIn);
            }
            media_pool::MediaPoolAction::MarkOut => {
                self.dispatch_playback_action(host, playback_controls::PlaybackAction::MarkOut);
            }
            media_pool::MediaPoolAction::QuickCover => {
                self.dispatch_playback_action(host, playback_controls::PlaybackAction::QuickCover);
            }
            media_pool::MediaPoolAction::ExportCommit => self.export_commit(host),
            media_pool::MediaPoolAction::SelectClipId(_) => {}
        }
    }

    fn ui_pool_head(&mut self, ui: &mut egui::Ui, host: &HostClient) {
        let action = media_pool::show_head(
            ui,
            self.role
                .head()
                .to_pool_head(self.library_tab, self.playing),
        );
        self.dispatch_media_pool(host, action);
    }

    fn ui_filmstrip_web(&mut self, ui: &mut egui::Ui, host: &HostClient, height: f32) {
        let timeline_fps = self.timeline_fps();
        let tc = move |sec| seconds_to_timecode(sec, timeline_fps);
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
        self.dispatch_media_pool(host, action);
    }

    fn ui_segmenti_panel(&mut self, ui: &mut egui::Ui, host: &HostClient, height: f32) {
        let segs = self
            .timeline
            .as_ref()
            .map(|t| t.segments.clone())
            .unwrap_or_default();
        let duration = self
            .timeline
            .as_ref()
            .map(|t| t.duration_sec.max(0.1))
            .unwrap_or(0.1);
        let active_part_id =
            story_timeline::active_segment(self.timeline.as_ref(), self.virtual_sec)
                .map(|segment| segment.part_id.as_str());
        let tc = |sec| self.tc(sec);
        let action = segment_panel::show(
            ui,
            segment_panel::SegmentPanelInput {
                height,
                duration_sec: duration,
                fps: self.timeline_fps(),
                virtual_sec: self.virtual_sec,
                segments: &segs,
                active_part_id,
                marker_slots: &self.marker_slots,
                covers: &self.covers,
                markers: &self.markers,
                selected_slot_id: &self.selected_slot_id,
                selected_cover_id: &self.selected_cover_id,
                tc: &tc,
            },
        );
        self.dispatch_segment_panel(host, action);
    }

    fn dispatch_marker_cover_action(
        &mut self,
        host: &HostClient,
        action: marker_cover_panel::MarkerCoverAction,
    ) {
        match action {
            marker_cover_panel::MarkerCoverAction::None => {}
            marker_cover_panel::MarkerCoverAction::AddMarker => self.marker_at_head(host),
            marker_cover_panel::MarkerCoverAction::CreateCover => self.quick_cover(host),
            marker_cover_panel::MarkerCoverAction::SelectSlot(id) => {
                self.select_marker_slot(host, &id);
            }
            marker_cover_panel::MarkerCoverAction::SelectCover(id) => {
                self.select_cover(host, &id);
            }
            marker_cover_panel::MarkerCoverAction::SeekMarker(sec) => {
                self.virtual_sec = self.snap_sec(sec);
                self.ensure_wrap_or_scrub(host);
            }
        }
    }

    fn dispatch_segment_panel(
        &mut self,
        host: &HostClient,
        action: segment_panel::SegmentPanelAction,
    ) {
        match action {
            segment_panel::SegmentPanelAction::None => {}
            segment_panel::SegmentPanelAction::SeekTimeline(sec) => {
                self.virtual_sec = self.snap_sec(sec);
                self.ensure_wrap_or_scrub(host);
            }
            segment_panel::SegmentPanelAction::MarkerCover(action) => {
                self.dispatch_marker_cover_action(host, action);
            }
            segment_panel::SegmentPanelAction::SelectSegment { part_id, start_sec } => {
                self.virtual_sec = start_sec;
                self.selected_part_id = part_id.clone();
                if self.view_mode != ViewMode::Wrap {
                    self.start_wrap_session_for_part(host, Some(part_id));
                } else {
                    self.scrub_soft(host);
                }
            }
        }
    }

    fn ensure_wrap_or_scrub(&mut self, host: &HostClient) {
        if self.view_mode != ViewMode::Wrap {
            self.start_wrap_session(host);
        } else {
            self.scrub_soft(host);
        }
    }

    fn ui_source_editor(
        &mut self,
        ui: &mut egui::Ui,
        host: &HostClient,
        _height: f32,
        playback: &mut PlaybackStack,
    ) {
        let clip_label = self.selected_clip_label();
        let clip_dur = self
            .all_clips
            .iter()
            .find(|c| c.clip_id == self.selected_clip_id)
            .map(|c| c.duration_sec)
            .unwrap_or(0.0)
            .max((self.source_out).max(1.0));
        let timebase_fps = self.source_timebase_fps().unwrap_or(DEFAULT_FPS);
        // Editorial mirror from carrier projection (marks), not a local clock.
        if playback.carrier().is_active() {
            let frame = playback.carrier().display_frame().0;
            self.playhead_frame = frame;
            self.virtual_sec = self.snap_source_sec(frame_to_seconds(frame, timebase_fps));
        }
        let tc = |sec| self.source_tc(sec);
        let focus_paint = match self.focus.target {
            FocusTarget::Playhead => TimelineFocusPaint::Playhead,
            FocusTarget::In => TimelineFocusPaint::In,
            FocusTarget::Out => TimelineFocusPaint::Out,
        };
        let in_frame = seconds_to_frame(self.source_in.max(0.0), timebase_fps);
        let out_frame = seconds_to_frame(self.source_out.max(self.source_in), timebase_fps);
        let fallback_frame =
            seconds_to_frame(self.virtual_sec.clamp(0.0, clip_dur), timebase_fps);
        let timeline_model = playback.timeline_model_for_clip(
            timebase_fps,
            clip_dur,
            in_frame,
            out_frame,
            fallback_frame,
        );
        let action = source_editor::show(
            ui,
            source_editor::SourceEditorInput {
                clip_label: &clip_label,
                source_in: self.source_in,
                source_out: self.source_out,
                timeline_model,
                focus: focus_paint,
                a1_peaks: &self.a1_peaks,
                a2_peaks: &self.a2_peaks,
                frames: &self.film_frames,
                tc: &tc,
                expanded_audio: self.expanded_audio,
            },
        );

        match action {
            source_editor::SourceEditorAction::None => {}
            source_editor::SourceEditorAction::SaveVirtualShot => self.save_virtual_shot(host),
            source_editor::SourceEditorAction::CreatePart(kind) => self.create_part(host, kind),
            source_editor::SourceEditorAction::ToggleAudioExpand(lane) => {
                self.expanded_audio = self.expanded_audio.toggle(lane);
            }
            source_editor::SourceEditorAction::CueFrame(frame) => {
                // Progress bar → CueFrame; Space plays from this position.
                match crate::player_bridge::build_open_request(self)
                    .and_then(|request| playback.cue_timeline_click(request, frame))
                {
                    Ok(()) => {}
                    Err(err) => self.status = err,
                }
            }
        }
    }
}

impl StoryScreen {
    fn dispatch_playback_action(
        &mut self,
        host: &HostClient,
        action: playback_controls::PlaybackAction,
    ) {
        match action {
            playback_controls::PlaybackAction::TogglePlay => self
                .pending_playback_commands
                .push(StoryPlaybackCommand::TogglePlay),
            playback_controls::PlaybackAction::MarkIn => self.mark_in_action(host),
            playback_controls::PlaybackAction::MarkOut => self.mark_out_action(host),
            playback_controls::PlaybackAction::SelectMarkIn => self.select_mark_in(host),
            playback_controls::PlaybackAction::SelectMarkOut => self.select_mark_out(host),
            playback_controls::PlaybackAction::FocusNext => {
                let chain = self.edit_focus_chain();
                self.focus.focus_next(&chain);
                self.after_focus_changed();
            }
            playback_controls::PlaybackAction::FocusPrev => {
                let chain = self.edit_focus_chain();
                self.focus.focus_prev(&chain);
                self.after_focus_changed();
            }
            playback_controls::PlaybackAction::ClearFocus => {
                if !self.focus.is_playhead() {
                    self.focus.clear();
                    self.status = "Fokus → playhead".into();
                }
            }
            playback_controls::PlaybackAction::QuickCover => self.quick_cover(host),
            // SeekFrames is handled in handle_shortcuts (needs PlaybackStack).
            playback_controls::PlaybackAction::SeekFrames(_) => {}
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

    fn select_mark_in(&mut self, host: &HostClient) {
        let _ = host;
        if self.view_mode != ViewMode::Source {
            self.status = format!(
                "{}: source mode",
                self.chord_or("select_mark_in", "select_mark_in")
            );
            return;
        }
        if !self.mark_in_set {
            self.status = format!("Prvo stavi IN ({})", self.chord_or("mark_in", "mark_in"));
            return;
        }
        self.focus.select_in();
        self.virtual_sec = self.snap_source_sec(self.source_in);
        self.schedule_native_seek_io();
        self.status = format!(
            "Fokus IN · {} · {} pomak 1 frame · {} playhead",
            self.source_tc(self.source_in),
            self.chord_or("step_forward_frame", "←/→"),
            self.chord_or("clear_focus", "Esc")
        );
    }

    fn after_focus_changed(&mut self) {
        match self.focus.target {
            FocusTarget::In => {
                if !self.mark_in_set {
                    self.focus.clear();
                    self.status = format!(
                        "IN još nije označen ({})",
                        self.chord_or("mark_in", "mark_in")
                    );
                    return;
                }
                self.virtual_sec = self.snap_source_sec(self.source_in);
                self.schedule_native_seek_io();
                self.status = format!(
                    "Fokus IN · {} · {} 1f",
                    self.source_tc(self.source_in),
                    self.chord_or("step_forward_frame", "step")
                );
            }
            FocusTarget::Out => {
                if !self.mark_out_set {
                    self.focus.clear();
                    self.status = format!(
                        "OUT još nije označen ({})",
                        self.chord_or("mark_out", "mark_out")
                    );
                    return;
                }
                self.virtual_sec = self.snap_source_sec(self.source_out);
                self.schedule_native_seek_io();
                self.status = format!(
                    "Fokus OUT · {} · {} 1f",
                    self.source_tc(self.source_out),
                    self.chord_or("step_forward_frame", "step")
                );
            }
            FocusTarget::Playhead => {
                self.status = "Fokus → playhead".into();
            }
        }
    }

    fn select_mark_out(&mut self, host: &HostClient) {
        let _ = host;
        if self.view_mode != ViewMode::Source {
            self.status = format!(
                "{}: source mode",
                self.chord_or("select_mark_out", "select_mark_out")
            );
            return;
        }
        if !self.mark_out_set {
            self.status = format!("Prvo stavi OUT ({})", self.chord_or("mark_out", "mark_out"));
            return;
        }
        self.focus.select_out();
        self.virtual_sec = self.snap_source_sec(self.source_out);
        self.schedule_native_seek_io();
        self.status = format!(
            "Fokus OUT · {} · {} pomak 1 frame · {} playhead",
            self.source_tc(self.source_out),
            self.chord_or("step_forward_frame", "←/→"),
            self.chord_or("clear_focus", "Esc")
        );
    }

    fn chord_or(&self, action_id: &str, fallback: &str) -> String {
        let hint = self.bindings.chord_hint(action_id);
        if hint.is_empty() {
            fallback.to_string()
        } else {
            hint
        }
    }

    fn selected_clip_duration(&self) -> f64 {
        self.all_clips
            .iter()
            .find(|c| c.clip_id == self.selected_clip_id)
            .map(|c| c.duration_sec)
            .unwrap_or(0.0)
            .max(self.source_out)
            .max(self.virtual_sec + 0.04)
            .max(1.0)
    }

    fn mark_in_action(&mut self, host: &HostClient) {
        match self.view_mode {
            ViewMode::Source => {
                if self.source_timebase_fps().is_none() {
                    self.status = "Source FPS još nije potvrđen — IN nije upisan".into();
                    return;
                }
                self.source_in = self.snap_source_sec(self.virtual_sec.max(0.0));
                // Do not collapse OUT to IN+1s — that traps playhead/player in a 1s window.
                if !self.mark_out_set || self.source_out <= self.source_in {
                    let clip_end = self.snap_source_sec(self.selected_clip_duration());
                    self.source_out = clip_end.max(self.source_in + 0.04);
                }
                self.mark_in_set = true;
                // Stay on playhead — select_mark_in later for frame edit focus.
                self.focus.clear();
                self.status = format!(
                    "IN {} · {} za fokus / korekcija",
                    self.source_tc(self.source_in),
                    self.chord_or("select_mark_in", "select_mark_in")
                );
            }
            ViewMode::Wrap => {
                if let Some(local) = story_timeline::local_sec_in_part(
                    self.timeline.as_ref(),
                    &self.selected_part_id,
                    self.virtual_sec,
                ) {
                    let part = self.selected_part_id.clone();
                    match story_edit::mark_part_in(host, &self.project_id, &part, local) {
                        Ok(st) => {
                            self.after_edit(host, st);
                            self.focus.clear();
                            self.status = format!("Mark IN @ {local:.2}s");
                            self.start_wrap_session(host);
                        }
                        Err(e) => self.status = e,
                    }
                } else {
                    self.status = "Odaberi part za Mark IN".into();
                }
            }
        }
    }

    fn mark_out_action(&mut self, host: &HostClient) {
        match self.view_mode {
            ViewMode::Source => {
                if self.source_timebase_fps().is_none() {
                    self.status = "Source FPS još nije potvrđen — OUT nije upisan".into();
                    return;
                }
                self.source_out = self.snap_source_sec(self.virtual_sec.max(self.source_in + 0.04));
                self.mark_out_set = true;
                self.focus.clear();
                self.status = format!(
                    "OUT {} · {} za fokus / korekcija",
                    self.source_tc(self.source_out),
                    self.chord_or("select_mark_out", "select_mark_out")
                );
            }
            ViewMode::Wrap => {
                if let Some(local) = story_timeline::local_sec_in_part(
                    self.timeline.as_ref(),
                    &self.selected_part_id,
                    self.virtual_sec,
                ) {
                    let part = self.selected_part_id.clone();
                    match story_edit::mark_part_out(host, &self.project_id, &part, local) {
                        Ok(st) => {
                            self.after_edit(host, st);
                            self.focus.clear();
                            self.status = format!("Mark OUT @ {local:.2}s");
                            self.start_wrap_session(host);
                        }
                        Err(e) => self.status = e,
                    }
                } else {
                    self.status = "Odaberi part za Mark OUT".into();
                }
            }
        }
    }

    fn save_virtual_shot(&mut self, host: &HostClient) {
        if self.view_mode == ViewMode::Source && self.source_timebase_fps().is_none() {
            self.status = "Source FPS još nije potvrđen — virtualni kadar nije spremljen".into();
            return;
        }
        let clip = self.selected_clip_id.clone();
        match story_edit::save_virtual_shot(
            host,
            &self.project_id,
            &clip,
            self.source_in,
            self.source_out,
        ) {
            Ok(_) => {
                self.status = "Virtualni kadar spremljen".into();
                self.reload_meta(host);
                self.library_tab = LibraryTab::Virtual;
            }
            Err(e) => self.status = e,
        }
    }

    fn create_part(&mut self, host: &HostClient, kind: &str) {
        match story_edit::create_part(
            host,
            &self.project_id,
            kind,
            &self.selected_shot_id,
            &self.virtual_shots,
        ) {
            Ok(st) => {
                self.status = format!("Dodan {kind}");
                self.after_edit(host, st);
                self.library_tab = LibraryTab::Segment;
                self.start_wrap_session(host);
            }
            Err(e) => self.status = e,
        }
    }

    fn export_commit(&mut self, host: &HostClient) {
        match story_edit::commit(host, &self.project_id) {
            Ok(st) => {
                self.after_edit(host, st);
                self.status = "Commit OK — Export XML datoteka čeka host API (isto kao web)".into();
            }
            Err(e) => self.status = e,
        }
    }

    fn marker_at_head(&mut self, host: &HostClient) {
        let part = self.selected_part_id.clone();
        match story_edit::create_marker(host, &self.project_id, self.virtual_sec, &part) {
            Ok(st) => {
                self.after_edit(host, st);
                self.status = format!("Marker @ {}", self.tc(self.virtual_sec));
            }
            Err(e) => self.status = e,
        }
    }

    fn select_marker_slot(&mut self, host: &HostClient, slot_id: &str) {
        if slot_id.trim().is_empty() {
            return;
        }
        match story_edit::select_marker_slot(host, &self.project_id, slot_id) {
            Ok(st) => {
                self.after_edit(host, st);
                self.selected_slot_id = slot_id.to_string();
                self.status = format!("Slot {}", truncate(slot_id, 24));
            }
            Err(e) => self.status = e,
        }
    }

    fn select_cover(&mut self, host: &HostClient, cover_id: &str) {
        if cover_id.trim().is_empty() {
            return;
        }
        match story_edit::select_cover(host, &self.project_id, cover_id) {
            Ok(st) => {
                self.after_edit(host, st);
                self.selected_cover_id = cover_id.to_string();
                self.status = format!("Cover {}", truncate(cover_id, 24));
            }
            Err(e) => self.status = e,
        }
    }

    fn quick_cover(&mut self, host: &HostClient) {
        let target = match story_edit::quick_cover_target(
            &self.selected_slot_id,
            &self.marker_slots,
            &self.selected_clip_id,
            &self.selected_shot_id,
        ) {
            Ok(target) => target,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        match story_edit::create_cover(host, &self.project_id, &target) {
            Ok(st) => {
                self.after_edit(host, st);
                self.status = "Cover kreiran".into();
            }
            Err(e) => self.status = e,
        }
    }

    fn scrub_soft(&mut self, _host: &HostClient) {
        self.schedule_native_seek();
    }

    /// ←/→: nudge focused IN/OUT, otherwise seek playhead by frames.
    fn step_focus(
        &mut self,
        host: &HostClient,
        playback: &mut crate::playback_stack::PlaybackStack,
        frames: i64,
    ) {
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
        playback: &mut crate::playback_stack::PlaybackStack,
        frames: i64,
    ) {
        if self.view_mode != ViewMode::Source {
            self.seek_by_frames(host, frames);
            return;
        }
        let Some(fps) = self.source_timebase_fps() else {
            self.status = "Source FPS još nije potvrđen — frame seek nije moguć".into();
            return;
        };
        let clip_end = seconds_to_frame(self.selected_clip_duration(), fps);
        let current = if playback.carrier().is_active() {
            playback.carrier().display_frame().0
        } else {
            seconds_to_frame(self.virtual_sec, fps)
        };
        let next = (current + frames).clamp(0, clip_end);
        self.playhead_frame = next;
        self.virtual_sec = self.snap_source_sec(frame_to_seconds(next, fps));
        match crate::player_bridge::build_open_request(self)
            .and_then(|request| playback.cue_timeline_click(request, next))
        {
            Ok(()) => {
                self.status = format!("Playhead → {} (1f)", self.source_tc(self.virtual_sec));
            }
            Err(err) => self.status = err,
        }
    }

    fn frame_delta_sec(&self) -> Option<f64> {
        if self.view_mode == ViewMode::Source {
            let fps = self.source_timebase_fps()?;
            Some(1.0 / fps.max(1.0))
        } else {
            Some(self.frame_step())
        }
    }

    fn nudge_in(&mut self, host: &HostClient, frames: i64) {
        let Some(step) = self.frame_delta_sec() else {
            self.status = "Source FPS još nije potvrđen — IN nudge nije moguć".into();
            return;
        };
        let delta = step * frames as f64;
        match self.view_mode {
            ViewMode::Source => {
                let next = self.snap_source_sec((self.source_in + delta).max(0.0));
                if next >= self.source_out {
                    self.status = "IN ne smije prijeći OUT".into();
                    return;
                }
                self.source_in = next;
                self.virtual_sec = next;
                self.schedule_native_seek_io();
                self.status = format!("IN → {} (1f)", self.source_tc(self.source_in));
            }
            ViewMode::Wrap => {
                let Some(part) = self
                    .parts
                    .iter()
                    .find(|p| p.part_id == self.selected_part_id)
                else {
                    self.status = "IN nudge: odaberi part".into();
                    return;
                };
                let cur = part.in_seconds.unwrap_or(0.0).max(0.0);
                let next = (cur + delta).max(0.0);
                if part.out_seconds.is_some_and(|o| next >= o) {
                    self.status = "IN ne smije prijeći OUT".into();
                    return;
                }
                let part_id = part.part_id.clone();
                match story_edit::mark_part_in(host, &self.project_id, &part_id, next) {
                    Ok(st) => {
                        self.after_edit(host, st);
                        self.status = format!("IN → {next:.3}s (1f)");
                        self.start_wrap_session(host);
                    }
                    Err(e) => self.status = e,
                }
            }
        }
    }

    fn nudge_out(&mut self, host: &HostClient, frames: i64) {
        let Some(step) = self.frame_delta_sec() else {
            self.status = "Source FPS još nije potvrđen — OUT nudge nije moguć".into();
            return;
        };
        let delta = step * frames as f64;
        match self.view_mode {
            ViewMode::Source => {
                let next = self.snap_source_sec((self.source_out + delta).max(0.0));
                if next <= self.source_in {
                    self.status = "OUT ne smije prijeći ispred IN".into();
                    return;
                }
                self.source_out = next;
                self.virtual_sec = next;
                self.schedule_native_seek_io();
                self.status = format!("OUT → {} (1f)", self.source_tc(self.source_out));
            }
            ViewMode::Wrap => {
                let Some(part) = self
                    .parts
                    .iter()
                    .find(|p| p.part_id == self.selected_part_id)
                else {
                    self.status = "OUT nudge: odaberi part".into();
                    return;
                };
                let inn = part.in_seconds.unwrap_or(0.0).max(0.0);
                let cur = part.out_seconds.unwrap_or(inn + 1.0);
                let next = (cur + delta).max(0.0);
                if next <= inn {
                    self.status = "OUT ne smije prijeći ispred IN".into();
                    return;
                }
                let part_id = part.part_id.clone();
                match story_edit::mark_part_out(host, &self.project_id, &part_id, next) {
                    Ok(st) => {
                        self.after_edit(host, st);
                        self.status = format!("OUT → {next:.3}s (1f)");
                        self.start_wrap_session(host);
                    }
                    Err(e) => self.status = e,
                }
            }
        }
    }

    fn seek_by_frames(&mut self, host: &HostClient, frames: i64) {
        if self.view_mode == ViewMode::Source {
            let Some(fps) = self.source_timebase_fps() else {
                self.status = "Source FPS još nije potvrđen — frame seek nije moguć".into();
                return;
            };
            let delta = (1.0 / fps.max(1.0)) * frames as f64;
            let clip_end = self.selected_clip_duration();
            self.virtual_sec = self.snap_source_sec((self.virtual_sec + delta).max(0.0));
            self.virtual_sec = self.virtual_sec.clamp(0.0, clip_end);
            self.schedule_native_seek_io();
            return;
        }
        let delta = self.frame_step() * frames as f64;
        self.virtual_sec = self.snap_sec((self.virtual_sec + delta).max(0.0));
        let dur = story_timeline::duration(
            self.view_mode,
            self.timeline.as_ref(),
            self.source_in,
            self.source_out,
        );
        if dur > 0.0 {
            self.virtual_sec = self.virtual_sec.min(dur);
        }
        self.scrub_soft(host);
    }
}

impl crate::player_bridge::PlayerClient for StoryScreen {
    fn drain_playback_commands(&mut self) -> Vec<crate::player_bridge::PlaybackCommand> {
        let raw = std::mem::take(&mut self.pending_playback_commands);
        crate::player_bridge::compact_playback_commands(raw, self.playing)
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
        if self.view_mode == ViewMode::Wrap {
            if let Some(part) = self
                .parts
                .iter()
                .find(|p| p.part_id == self.selected_part_id)
            {
                let in_sec = part.in_seconds.unwrap_or(0.0).max(0.0);
                let out_sec = part
                    .out_seconds
                    .filter(|o| *o > in_sec)
                    .unwrap_or(in_sec + 1.0);
                return (in_sec, out_sec);
            }
        }
        let end = self.selected_clip_duration();
        (0.0, end.max(0.04))
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

    fn apply_player_frame(&mut self, _image: ColorImage, source_sec: f64, playing: bool) {
        self.playing = playing;
        self.status = "Broadcast player".into();
        // Editorial mirror of player clock (Source view). Timeline paint uses carrier.
        if self.view_mode == ViewMode::Source {
            let fps = self.source_timebase_fps().unwrap_or(DEFAULT_FPS);
            self.playhead_frame = seconds_to_frame(source_sec.max(0.0), fps);
            self.virtual_sec = self.snap_source_sec(source_sec.max(0.0));
        }
    }

    fn apply_player_state(&mut self, source_sec: f64, playing: bool, status: impl Into<String>) {
        self.playing = playing;
        self.status = status.into();
        if self.view_mode == ViewMode::Source {
            let fps = self.source_timebase_fps().unwrap_or(DEFAULT_FPS);
            self.playhead_frame = seconds_to_frame(source_sec.max(0.0), fps);
            self.virtual_sec = self.snap_source_sec(source_sec.max(0.0));
        }
    }

    fn apply_player_error(&mut self, status: impl Into<String>) {
        self.broadcast_preview_active = false;
        self.playing = false;
        self.status = status.into();
    }
}
