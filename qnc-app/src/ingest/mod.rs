//! Native Ingest — composes Story `qnc_ui` shell; domain = dir browser + clip grid.
//!
//! **Components:** [`dir_list`] (left media body).
//! Posters: snapshot `thumb_url` (DB) → async fetch → texture. UI never writes DB.

mod dir_list;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use eframe::egui::{self, TextureHandle, TextureOptions};
use serde_json::Value;

use crate::api::{FsEntry, FsList, HostClient, IngestClip, IngestState};
use crate::editorial::media_pool::{self, MediaPoolAction};
use crate::editorial::types::LibraryTab;
use crate::frame_time::{frame_to_seconds, seconds_to_frame};
use crate::media_assets::{self, AsyncImageAssetLoader, ImageAssetKey};
use crate::playback_stack::PlaybackStack;
use crate::player_contract::BroadcastHostSourceRef;
use crate::qnc_filmstrip_background::FilmFrame;
use crate::qnc_location_browser::clean_location_path;
use crate::qnc_source_dock::{self, SourceDockAction, SourceDockInput};
use crate::qnc_timeline::{ExpandedAudio, TimelineFocusPaint};
use crate::qnc_ui;
use crate::shortcuts::{StoryBindings, STORYBOARD_SHORTCUT_SCOPE};
use crate::story::playback_controls::{self, PlaybackAction};

use dir_list::{DirBrowserKind, DirListAction, DirListInput};

const POLL_INTERVAL: Duration = Duration::from_millis(1500);

pub struct IngestScreen {
    pub path_edit: String,
    pub state: Option<IngestState>,
    pub busy: bool,
    pub last_poll: Option<Instant>,
    pub loaded_for_project: String,
    pub message: Option<String>,
    poster_textures: HashMap<String, TextureHandle>,
    image_asset_loader: AsyncImageAssetLoader,
    archive_local: bool,
    ai_mining: bool,
    pub(crate) preview_clip_id: String,
    dir_loaded: bool,
    dir_roots: bool,
    dir_path: String,
    dir_parent: Option<String>,
    dir_entries: Vec<FsEntry>,
    dir_error: Option<String>,
    dir_busy: bool,
    dir_browser: DirBrowserKind,
    // Broadcast player projection (PlayerRemote via app).
    pub(crate) selected_play_path: String,
    pub(crate) selected_source_ref: Option<BroadcastHostSourceRef>,
    pub(crate) selected_source_fps: f64,
    pub(crate) selected_source_has_audio: bool,
    pub(crate) selected_source_audio_channels: u8,
    pub(crate) virtual_frame: i64,
    pub(crate) playing: bool,
    pub(crate) player_status: String,
    project_id: String,
    /// Web kodak: click A1/A2 label expands that wave lane.
    expanded_audio: ExpandedAudio,
    /// Same pool-head tabs as Story / Media Assist (`media_pool::show_head`).
    library_tab: LibraryTab,
    /// QNC keyboard-shortcuts (storyboard scope) — no hardcoded chords.
    bindings: StoryBindings,
    shortcuts_loading: bool,
    shortcuts_pending: usize,
    shortcuts_loaded: bool,
    shortcut_catalog: Option<Value>,
    shortcut_user: Option<Value>,
}

pub enum IngestAction {
    None,
    /// Progress-bar scrub — app routes to [`PlaybackStack`].
    CueFrame(i64),
    TogglePlay,
    RequestState(String),
    RequestDirList(String),
    #[allow(dead_code)]
    PickFolder,
    #[allow(dead_code)]
    BrowsePath,
    #[allow(dead_code)]
    Discover,
    Reload,
    SelectAll,
    ClearSelection,
    ImportSelected,
    Toggle(String),
    #[allow(dead_code)]
    FocusPreview(String),
    SetArchive(bool),
    /// Confirm dir-tree path → ingest browse/discover.
    ConfirmDir,
    CancelDir,
}

impl Default for IngestScreen {
    fn default() -> Self {
        Self {
            path_edit: String::new(),
            state: None,
            busy: false,
            last_poll: None,
            loaded_for_project: String::new(),
            message: None,
            poster_textures: HashMap::new(),
            image_asset_loader: AsyncImageAssetLoader::new(),
            archive_local: false,
            ai_mining: false,
            preview_clip_id: String::new(),
            dir_loaded: false,
            dir_roots: false,
            dir_path: String::new(),
            dir_parent: None,
            dir_entries: Vec::new(),
            dir_error: None,
            dir_busy: false,
            dir_browser: DirBrowserKind::default(),
            selected_play_path: String::new(),
            selected_source_ref: None,
            selected_source_fps: 0.0,
            selected_source_has_audio: false,
            selected_source_audio_channels: 0,
            virtual_frame: 0,
            playing: false,
            player_status: String::new(),
            project_id: String::new(),
            expanded_audio: ExpandedAudio::default(),
            library_tab: LibraryTab::default(),
            bindings: StoryBindings::empty(),
            shortcuts_loading: false,
            shortcuts_pending: 0,
            shortcuts_loaded: false,
            shortcut_catalog: None,
            shortcut_user: None,
        }
    }
}

impl IngestScreen {
    pub fn handle_shortcuts(&mut self, ctx: &egui::Context) -> Vec<IngestAction> {
        if !self.shortcuts_ready() {
            return Vec::new();
        }
        if self.preview_clip_id.is_empty() {
            return Vec::new();
        }
        let mut actions = Vec::new();
        // play_pause / step_back_frame / step_forward_frame from catalog only.
        for action in playback_controls::shortcut_actions(ctx, &self.bindings) {
            match action {
                PlaybackAction::TogglePlay => actions.push(IngestAction::TogglePlay),
                PlaybackAction::SeekFrames(frames) => actions.push(self.nudge_frames(frames)),
                _ => {}
            }
        }
        actions
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
        if scope != STORYBOARD_SHORTCUT_SCOPE {
            return;
        }
        self.shortcut_catalog = Some(catalog);
        self.finish_shortcut_result();
    }

    pub fn apply_shortcut_user(&mut self, scope: &str, user: Value) {
        if scope != STORYBOARD_SHORTCUT_SCOPE {
            return;
        }
        self.shortcut_user = Some(user);
        self.finish_shortcut_result();
    }

    pub fn set_shortcuts_error(&mut self, scope: &str, port_id: &str, error: impl Into<String>) {
        if scope != STORYBOARD_SHORTCUT_SCOPE {
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
        self.message = Some(format!("shortcut catalog: {}", error.into()));
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
        self.bindings = StoryBindings::from_catalog(catalog, user, STORYBOARD_SHORTCUT_SCOPE);
        self.shortcuts_loaded = true;
        self.shortcuts_loading = false;
        self.shortcuts_pending = 0;
    }

    pub fn ensure_loaded(&mut self, host: &HostClient, project_id: &str) {
        if self.loaded_for_project == project_id && self.state.is_some() {
            return;
        }
        self.loaded_for_project = project_id.to_string();
        self.project_id = project_id.to_string();
        self.poster_textures.clear();
        self.image_asset_loader.clear();
        self.preview_clip_id.clear();
        self.reset_player_session();
        let _ = host;
        self.state = None;
        self.busy = false;
        self.last_poll = None;
        self.dir_path.clear();
        self.dir_entries.clear();
        self.dir_error = None;
        self.dir_busy = false;
        self.dir_loaded = false;
    }

    pub fn apply(&mut self, st: IngestState) {
        if !st.browse_path.is_empty() {
            self.path_edit = st.browse_path.clone();
        }
        self.archive_local = st.archive_original;
        if self.preview_clip_id.is_empty() {
            if let Some(first) = st.clips.first() {
                self.preview_clip_id = first.clip_id.clone();
            }
        }
        self.state = Some(st);
        self.last_poll = Some(Instant::now());
    }

    pub fn begin_state_load(&mut self, project_id: &str) {
        self.project_id = project_id.to_string();
        self.loaded_for_project = project_id.to_string();
        self.busy = true;
        self.message = None;
        self.last_poll = Some(Instant::now());
    }

    pub fn begin_state_poll(&mut self, project_id: &str) {
        self.project_id = project_id.to_string();
        self.loaded_for_project = project_id.to_string();
        self.busy = true;
        self.last_poll = Some(Instant::now());
    }

    pub fn apply_loaded_state(&mut self, st: IngestState) {
        self.apply(st);
        self.busy = false;
        self.message = None;
    }

    pub fn apply_polled_state(&mut self, st: IngestState) {
        let path_keep = self.path_edit.clone();
        self.apply(st);
        self.path_edit = path_keep;
        self.busy = false;
    }

    pub fn begin_import_command(&mut self, project_id: &str) {
        self.project_id = project_id.to_string();
        self.loaded_for_project = project_id.to_string();
        self.busy = true;
        self.message = None;
        self.last_poll = Some(Instant::now());
    }

    pub fn apply_import_command_state(&mut self, st: IngestState, message: impl Into<String>) {
        self.apply(st);
        self.busy = false;
        self.message = Some(message.into());
    }

    pub fn selected_clip_ids(&self) -> Vec<String> {
        self.state
            .as_ref()
            .map(|s| s.selected_clip_ids.clone())
            .unwrap_or_default()
    }

    pub fn apply_archive_option_state(&mut self, st: IngestState, message: impl Into<String>) {
        self.archive_local = st.archive_original;
        if let Some(current) = self.state.as_mut() {
            current.archive_original = st.archive_original;
        }
        self.busy = false;
        self.message = Some(message.into());
        self.last_poll = Some(Instant::now());
    }

    pub fn set_state_error(&mut self, error: impl Into<String>) {
        self.message = Some(error.into());
        self.busy = false;
        self.last_poll = Some(Instant::now());
    }

    pub fn needs_poll(&self) -> bool {
        let Some(st) = &self.state else {
            return false;
        };
        if st.durations_pending {
            return true;
        }
        st.clips.iter().any(|c| {
            matches!(
                c.import_status.as_str(),
                "queued" | "processing" | "generating_proxy" | "original_ready"
            ) || (matches!(
                c.thumb_status.as_str(),
                "pending" | "no_card_thumb" | "processing"
            ) && !self.poster_textures.contains_key(&c.clip_id))
        })
    }

    pub fn should_request_poll(&self) -> bool {
        if self.busy {
            return false;
        }
        let due = self
            .last_poll
            .map(|t| t.elapsed() >= POLL_INTERVAL)
            .unwrap_or(true);
        due && self.needs_poll()
    }

    pub fn selected_clip_count(&self) -> usize {
        self.selected_clip_ids().len()
    }

    pub fn confirm_dir_path(&mut self) -> Result<String, String> {
        let path = self.dir_path.trim().to_string();
        if path.is_empty() || self.dir_roots {
            return Err("Odaberi mapu u stablu.".into());
        }
        self.path_edit = path.clone();
        Ok(path)
    }

    pub fn browse_path_candidate(&mut self) -> Result<String, String> {
        let path = if !self.dir_path.is_empty() && !self.dir_roots {
            self.dir_path.clone()
        } else {
            self.path_edit.trim().to_string()
        };
        if path.is_empty() {
            return Err("Odaberi mapu (U redu) ili upiši putanju.".into());
        }
        self.path_edit = path.clone();
        Ok(path)
    }

    pub fn cancel_dir_browser(&mut self) {
        self.dir_browser = DirBrowserKind::Local;
        self.dir_roots = true;
        self.dir_path.clear();
        self.dir_parent = None;
        self.dir_entries.clear();
        self.dir_error = None;
        self.dir_loaded = false;
        self.dir_busy = false;
        self.message = Some("Stablo: računalo.".into());
    }

    pub fn set_archive_draft(&mut self, archive_original: bool) {
        self.archive_local = archive_original;
    }

    pub fn begin_dir_listing(&mut self, path: &str) {
        self.dir_busy = true;
        self.dir_error = None;
        self.dir_loaded = true;
        if path.trim().is_empty() {
            self.dir_roots = true;
            self.dir_path.clear();
            self.dir_parent = None;
        }
    }

    pub fn apply_dir_listing(&mut self, list: FsList) {
        self.dir_roots = list.roots;
        self.dir_path = clean_location_path(&list.path);
        self.dir_parent = list.parent.map(|p| clean_location_path(&p));
        self.dir_entries = list
            .entries
            .into_iter()
            .map(|mut entry| {
                entry.path = clean_location_path(&entry.path);
                entry.name = clean_location_path(&entry.name);
                entry
            })
            .collect();
        self.dir_error = None;
        self.dir_busy = false;
        self.dir_loaded = true;
    }

    pub fn set_dir_listing_error(&mut self, error: impl Into<String>) {
        self.dir_error = Some(error.into());
        self.dir_entries.clear();
        self.dir_busy = false;
        self.dir_loaded = true;
    }

    fn pump_posters(&mut self, host: &HostClient, project_id: &str, ctx: &egui::Context) {
        for result in self.image_asset_loader.poll() {
            if let Ok(color) = result.image {
                let clip_id = result.key.item_id;
                let tex = ctx.load_texture(
                    format!("ingest_poster_{clip_id}"),
                    color,
                    TextureOptions::LINEAR,
                );
                self.poster_textures.insert(clip_id, tex);
            }
        }
        let Some(st) = &self.state else {
            return;
        };
        let mut requested = false;
        if !self.preview_clip_id.trim().is_empty() {
            if let Some(c) = st.clips.iter().find(|c| c.clip_id == self.preview_clip_id) {
                if !c.thumb_url.trim().is_empty() && !self.poster_textures.contains_key(&c.clip_id)
                {
                    let url = media_assets::ingest_thumbnail_url(host, project_id, &c.clip_id);
                    requested = self.image_asset_loader.request(
                        ImageAssetKey::new("ingest.poster", c.clip_id.clone(), "thumb"),
                        url,
                        Some(ctx.clone()),
                    );
                }
            }
        }
        for c in &st.clips {
            // Snapshot (DB) must expose thumb_url before we fetch — no 404 probing.
            if requested
                || c.clip_id == self.preview_clip_id
                || c.thumb_url.trim().is_empty()
                || self.poster_textures.contains_key(&c.clip_id)
            {
                continue;
            }
            let url = media_assets::ingest_thumbnail_url(host, project_id, &c.clip_id);
            requested = self.image_asset_loader.request(
                ImageAssetKey::new("ingest.poster", c.clip_id.clone(), "thumb"),
                url,
                Some(ctx.clone()),
            );
            if requested {
                break;
            }
        }
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        project_name: &str,
        project_id: &str,
        host: &HostClient,
        ctx: &egui::Context,
        playback: &PlaybackStack,
    ) -> IngestAction {
        self.pump_posters(host, project_id, ctx);
        if self.playing {
            ctx.request_repaint();
        }
        if self.state.as_ref().is_some_and(|st| {
            st.clips.iter().any(|c| {
                !c.thumb_url.trim().is_empty() && !self.poster_textures.contains_key(&c.clip_id)
            })
        }) {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        if self.project_id != project_id {
            self.project_id = project_id.to_string();
        }

        let mut action = IngestAction::None;
        if self.state.is_none() && !self.busy && !project_id.trim().is_empty() {
            action = IngestAction::RequestState(project_id.to_string());
        } else if !self.dir_loaded
            && !self.dir_busy
            && matches!(self.dir_browser, DirBrowserKind::Local)
        {
            action = IngestAction::RequestDirList(self.dir_path.clone());
        }
        let _ = project_name;

        // Story shell (reference). Ingest only fills domain bodies — never own layout math.
        let clips: Vec<IngestClip> = self
            .state
            .as_ref()
            .map(|s| s.clips.clone())
            .unwrap_or_default();
        qnc_ui::editorial_shell(ui, |ui, m, side| match side {
            qnc_ui::ShellSide::Left => {
                qnc_ui::media_column_monitor(
                    ui,
                    m,
                    |ui, preview_h| {
                        playback.show_monitor(ui, preview_h, "Odaberi klip");
                    },
                    |ui, _rest| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let head = media_pool::show_head(
                            ui,
                            crate::composition::HeadFeatures::INGEST
                                .to_pool_head(self.library_tab, self.playing),
                        );
                        let head_action = self.dispatch_pool_head(head);
                        if !matches!(head_action, IngestAction::None) {
                            action = head_action;
                        }
                        let body = ui.available_height().max(0.0);
                        qnc_ui::content_panel(ui, body, |ui| {
                            // Component: dir_list
                            let input = DirListInput {
                                kind: self.dir_browser,
                                roots: self.dir_roots,
                                path: &self.dir_path,
                                parent: self.dir_parent.as_deref(),
                                entries: &self.dir_entries,
                                error: self.dir_error.as_deref(),
                                busy: self.busy || self.dir_busy,
                            };
                            match dir_list::show(ui, input) {
                                DirListAction::None => {}
                                DirListAction::SelectKind(kind) => {
                                    self.dir_browser = kind;
                                    self.dir_error = None;
                                    match kind {
                                        DirBrowserKind::Local => {
                                            action = IngestAction::RequestDirList(String::new());
                                        }
                                        DirBrowserKind::Lan | DirBrowserKind::Internet => {
                                            self.dir_roots = true;
                                            self.dir_path.clear();
                                            self.dir_parent = None;
                                            self.dir_entries.clear();
                                        }
                                    }
                                }
                                DirListAction::OpenPath(path) => {
                                    action = IngestAction::RequestDirList(path);
                                }
                                DirListAction::Confirm => action = IngestAction::ConfirmDir,
                                DirListAction::Cancel => action = IngestAction::CancelDir,
                            }
                        });
                    },
                );
            }
            qnc_ui::ShellSide::Right => {
                let tc = |s: f64| format_duration(s);
                let strip = media_pool::show_ingest_strip(
                    ui,
                    m.height,
                    &self.preview_clip_id,
                    &clips,
                    &self.poster_textures,
                    &tc,
                );
                match strip {
                    MediaPoolAction::SelectClipId(id) => {
                        if self.preview_clip_id != id {
                            self.image_asset_loader.cancel_pending();
                        }
                        self.preview_clip_id = id.clone();
                        action = IngestAction::Toggle(id);
                    }
                    MediaPoolAction::None
                    | MediaPoolAction::SwitchTab(_)
                    | MediaPoolAction::SelectShot(_)
                    | MediaPoolAction::SelectPart(_)
                    | MediaPoolAction::DeletePart(_)
                    | MediaPoolAction::ReorderPart { .. }
                    | MediaPoolAction::TogglePlay
                    | MediaPoolAction::MarkIn
                    | MediaPoolAction::MarkOut
                    | MediaPoolAction::QuickCover
                    | MediaPoolAction::ExportCommit => {}
                }
            }
        });

        action
    }

    /// Shared pool-head transport (Story / MA) — map to ingest playback only.
    fn dispatch_pool_head(&mut self, action: MediaPoolAction) -> IngestAction {
        let enabled = !self.preview_clip_id.is_empty();
        match action {
            MediaPoolAction::None => IngestAction::None,
            MediaPoolAction::SwitchTab(tab) => {
                // Ingest never uses Segment — clamp away if somehow selected.
                self.library_tab = match tab {
                    LibraryTab::Segment => LibraryTab::All,
                    other => other,
                };
                IngestAction::None
            }
            MediaPoolAction::TogglePlay if enabled => IngestAction::TogglePlay,
            MediaPoolAction::MarkIn if enabled => self.nudge_frames(-1),
            MediaPoolAction::MarkOut if enabled => self.nudge_frames(1),
            MediaPoolAction::QuickCover
            | MediaPoolAction::ExportCommit
            | MediaPoolAction::TogglePlay
            | MediaPoolAction::MarkIn
            | MediaPoolAction::MarkOut
            | MediaPoolAction::SelectShot(_)
            | MediaPoolAction::SelectClipId(_)
            | MediaPoolAction::SelectPart(_)
            | MediaPoolAction::DeletePart(_)
            | MediaPoolAction::ReorderPart { .. } => IngestAction::None,
        }
    }

    pub fn timeline_dock_height(&self) -> f32 {
        qnc_source_dock::dock_height(self.expanded_audio, true)
    }

    pub fn ui_timeline_dock(
        &mut self,
        ui: &mut egui::Ui,
        playback: &PlaybackStack,
    ) -> IngestAction {
        let clips = self
            .state
            .as_ref()
            .map(|s| s.clips.clone())
            .unwrap_or_default();
        let clip = clips.iter().find(|c| c.clip_id == self.preview_clip_id);
        let dur = clip.map(|c| c.duration_sec).unwrap_or(1.0).max(0.04);
        let label = clip.map(|c| c.name.as_str()).unwrap_or("—");
        let selected_n = self
            .state
            .as_ref()
            .map(|s| s.selected_clip_ids.len())
            .unwrap_or(0);
        let (sel, total, imp, _) = self.summary();
        let status = format!("{imp} uvezeno · {sel}/{total}");
        let mut frames: Vec<FilmFrame> = Vec::new();
        if let Some(tex) = self.poster_textures.get(&self.preview_clip_id) {
            let n = 12i64;
            for i in 0..n {
                let seek = if n > 1 {
                    (i as f64) * (dur / (n as f64 - 1.0))
                } else {
                    0.0
                };
                frames.push(FilmFrame {
                    index: i,
                    seek_sec: seek,
                    url: String::new(),
                    texture: Some(tex.clone()),
                });
            }
        }
        let empty: &[f32] = &[];
        let Some(fps) = clip
            .and_then(|clip| (clip.fps.is_finite() && clip.fps > 0.0).then_some(clip.fps))
            .or_else(|| {
                (self.selected_source_fps.is_finite() && self.selected_source_fps > 0.0)
                    .then_some(self.selected_source_fps)
            })
        else {
            ui.label("Source FPS nije potvrđen");
            return IngestAction::None;
        };
        let tc_frame = |frame: i64| format_tc(frame_to_seconds(frame.max(0), fps), fps);
        let duration_frames = seconds_to_frame(dur, fps).max(1);
        let live_source_ref = self.selected_source_ref.as_ref();
        if live_source_ref.is_some_and(|source_ref| playback.active_source_matches(source_ref))
            && playback.carrier().is_active()
        {
            self.virtual_frame = playback
                .carrier()
                .display_frame()
                .0
                .clamp(0, duration_frames);
        } else {
            self.virtual_frame = self.virtual_frame.clamp(0, duration_frames);
        }
        let timeline_model = playback.timeline_model_for_source_ref(
            live_source_ref,
            fps,
            duration_frames,
            0,
            duration_frames,
            0,
            duration_frames,
            self.virtual_frame,
        );
        let dock = qnc_source_dock::show(
            ui,
            SourceDockInput {
                clip_label: label,
                source_in_frame: 0,
                source_out_frame: duration_frames,
                timeline_model,
                focus: TimelineFocusPaint::Playhead,
                a1_peaks: empty,
                a2_peaks: empty,
                frames: &frames,
                tc_frame: &tc_frame,
                show_header: true,
                show_edit_actions: false,
                show_import_actions: true,
                archive_original: self.archive_local,
                ai_mining: self.ai_mining,
                import_enabled: !self.busy && selected_n > 0,
                ingest_status: &status,
                expanded_audio: self.expanded_audio,
            },
        );
        match dock {
            SourceDockAction::None => IngestAction::None,
            SourceDockAction::CueFrame(frame) => IngestAction::CueFrame(frame),
            SourceDockAction::ToggleAudioExpand(lane) => {
                self.expanded_audio = self.expanded_audio.toggle(lane);
                IngestAction::None
            }
            SourceDockAction::ImportSelected => IngestAction::ImportSelected,
            SourceDockAction::SelectAll => IngestAction::SelectAll,
            SourceDockAction::ClearSelection => IngestAction::ClearSelection,
            SourceDockAction::Reload => IngestAction::Reload,
            SourceDockAction::SetArchive(v) => IngestAction::SetArchive(v),
            SourceDockAction::SetAiMining(v) => {
                self.ai_mining = v;
                IngestAction::None
            }
            SourceDockAction::SaveVirtualShot
            | SourceDockAction::CreatePart(_)
            | SourceDockAction::CreateCover => IngestAction::None,
        }
    }

    fn summary(&self) -> (usize, usize, usize, usize) {
        let Some(st) = &self.state else {
            return (0, 0, 0, 0);
        };
        let selected = st.selected_clip_ids.len();
        let total = st.clips.len();
        let imported = st
            .clips
            .iter()
            .filter(|c| {
                matches!(
                    c.import_status.as_str(),
                    "imported" | "done" | "ready" | "proxy_ready"
                ) || c.status_proxy == "ready"
                    || c.proxy_status == "ready"
            })
            .count();
        let pending = st
            .clips
            .iter()
            .filter(|c| {
                matches!(
                    c.import_status.as_str(),
                    "queued" | "processing" | "generating_proxy" | "original_ready"
                )
            })
            .count();
        (selected, total, imported, pending)
    }

    pub fn dispatch(
        &mut self,
        host: &HostClient,
        project_id: &str,
        action: IngestAction,
    ) -> Result<(), String> {
        let _ = host;
        match action {
            IngestAction::None
            | IngestAction::CueFrame(_)
            | IngestAction::TogglePlay
            | IngestAction::RequestState(_)
            | IngestAction::RequestDirList(_)
            | IngestAction::Toggle(_)
            | IngestAction::Reload
            | IngestAction::PickFolder
            | IngestAction::ConfirmDir
            | IngestAction::BrowsePath
            | IngestAction::Discover
            | IngestAction::SelectAll
            | IngestAction::ClearSelection
            | IngestAction::SetArchive(_)
            | IngestAction::ImportSelected => Ok(()),
            IngestAction::CancelDir => {
                self.cancel_dir_browser();
                Ok(())
            }
            IngestAction::FocusPreview(clip_id) => {
                self.activate_preview_clip(project_id, &clip_id);
                Ok(())
            }
        }
    }
}

fn format_duration(sec: f64) -> String {
    if !sec.is_finite() || sec <= 0.0 {
        return "—".into();
    }
    let total = sec.round() as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Story-style timecode HH:MM:SS:FF for source dock.
fn format_tc(sec: f64, fps: f64) -> String {
    if !fps.is_finite() || fps <= 0.0 {
        return "--:--:--:--".into();
    }
    let sec = if sec.is_finite() && sec > 0.0 {
        sec
    } else {
        0.0
    };
    let total_frames = (sec * fps).round() as i64;
    let fps_i = fps.round().max(1.0) as i64;
    let ff = total_frames.rem_euclid(fps_i);
    let total_sec = total_frames / fps_i;
    let ss = total_sec.rem_euclid(60);
    let total_min = total_sec / 60;
    let mm = total_min.rem_euclid(60);
    let hh = total_min / 60;
    format!("{hh:02}:{mm:02}:{ss:02}:{ff:02}")
}
