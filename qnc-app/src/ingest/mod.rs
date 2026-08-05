//! Native Ingest — composes Story `qnc_ui` shell; domain = dir browser + clip grid.
//!
//! **Components:** [`dir_list`] (left media body).
//! Posters: snapshot `thumb_url` (DB) → async fetch → texture. UI never writes DB.

mod dir_list;
mod poster_loader;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use eframe::egui::{self, TextureHandle, TextureOptions};

use crate::api::{FsEntry, HostClient, IngestClip, IngestState};
use crate::editorial::media_pool::{self, MediaPoolAction};
use crate::editorial::types::LibraryTab;
use crate::frame_time::{frame_to_seconds, seconds_to_frame};
use crate::playback_stack::PlaybackStack;
use crate::player_bridge::PlaybackCommand;
use crate::player_contract::BroadcastHostSourceRef;
use crate::qnc_filmstrip_background::FilmFrame;
use crate::qnc_source_dock::{self, SourceDockAction, SourceDockInput};
use crate::qnc_timeline::{ExpandedAudio, TimelineFocusPaint};
use crate::qnc_ui;
use crate::shortcuts::{load_story_bindings, StoryBindings};
use crate::story::playback_controls::{self, PlaybackAction};

use dir_list::{DirBrowserKind, DirListAction, DirListInput};
use poster_loader::AsyncPosterLoader;

const POLL_INTERVAL: Duration = Duration::from_millis(1500);

pub struct IngestScreen {
    pub path_edit: String,
    pub state: Option<IngestState>,
    pub busy: bool,
    pub last_poll: Option<Instant>,
    pub loaded_for_project: String,
    pub message: Option<String>,
    poster_textures: HashMap<String, TextureHandle>,
    poster_loader: AsyncPosterLoader,
    archive_local: bool,
    ai_mining: bool,
    pub(crate) preview_clip_id: String,
    dir_loaded: bool,
    dir_roots: bool,
    dir_path: String,
    dir_parent: Option<String>,
    dir_entries: Vec<FsEntry>,
    dir_error: Option<String>,
    dir_browser: DirBrowserKind,
    // Broadcast player projection (PlayerRemote via app).
    pub(crate) selected_play_path: String,
    pub(crate) selected_source_ref: Option<BroadcastHostSourceRef>,
    pub(crate) selected_source_fps: f64,
    pub(crate) selected_source_has_audio: bool,
    pub(crate) selected_source_audio_channels: u8,
    pub(crate) virtual_sec: f64,
    pub(crate) playing: bool,
    pub(crate) player_status: String,
    pub(crate) pending_playback_commands: Vec<PlaybackCommand>,
    project_id: String,
    /// Web kodak: click A1/A2 label expands that wave lane.
    expanded_audio: ExpandedAudio,
    /// Same pool-head tabs as Story / Media Assist (`media_pool::show_head`).
    library_tab: LibraryTab,
    /// QNC keyboard-shortcuts (storyboard scope) — no hardcoded chords.
    bindings: StoryBindings,
}

pub enum IngestAction {
    None,
    PickFolder,
    BrowsePath,
    Discover,
    Reload,
    SelectAll,
    ClearSelection,
    ImportSelected,
    Toggle(String),
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
            poster_loader: AsyncPosterLoader::new(),
            archive_local: false,
            ai_mining: false,
            preview_clip_id: String::new(),
            dir_loaded: false,
            dir_roots: false,
            dir_path: String::new(),
            dir_parent: None,
            dir_entries: Vec::new(),
            dir_error: None,
            dir_browser: DirBrowserKind::default(),
            selected_play_path: String::new(),
            selected_source_ref: None,
            selected_source_fps: 25.0,
            selected_source_has_audio: true,
            selected_source_audio_channels: 2,
            virtual_sec: 0.0,
            playing: false,
            player_status: String::new(),
            pending_playback_commands: Vec::new(),
            project_id: String::new(),
            expanded_audio: ExpandedAudio::default(),
            library_tab: LibraryTab::default(),
            bindings: StoryBindings::empty(),
        }
    }
}

impl IngestScreen {
    pub fn handle_shortcuts(&mut self, ctx: &egui::Context, host: &HostClient) {
        if self.bindings.by_action.is_empty() {
            self.bindings = load_story_bindings(host, "storyboard");
        }
        if self.preview_clip_id.is_empty() {
            return;
        }
        // play_pause / step_back_frame / step_forward_frame from catalog only.
        for action in playback_controls::shortcut_actions(ctx, &self.bindings) {
            match action {
                PlaybackAction::TogglePlay => self.queue_toggle_play(),
                PlaybackAction::SeekFrames(frames) => self.nudge_frames(frames),
                _ => {}
            }
        }
    }

    pub fn ensure_loaded(&mut self, host: &HostClient, project_id: &str) {
        if self.loaded_for_project == project_id && self.state.is_some() {
            return;
        }
        self.loaded_for_project = project_id.to_string();
        self.project_id = project_id.to_string();
        self.poster_textures.clear();
        self.poster_loader.clear();
        self.preview_clip_id.clear();
        self.reset_player_session();
        self.reload(host, project_id);
        self.load_dir(host, "");
    }

    pub fn reload(&mut self, host: &HostClient, project_id: &str) {
        self.busy = true;
        match host.ingest_state(project_id) {
            Ok(st) => {
                self.apply(st);
                self.message = None;
            }
            Err(e) => {
                self.message = Some(e);
            }
        }
        self.busy = false;
        self.last_poll = Some(Instant::now());
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

    pub fn maybe_poll(&mut self, host: &HostClient, project_id: &str) {
        if self.busy {
            return;
        }
        let due = self
            .last_poll
            .map(|t| t.elapsed() >= POLL_INTERVAL)
            .unwrap_or(true);
        if due && self.needs_poll() {
            if let Ok(st) = host.ingest_state(project_id) {
                let path_keep = self.path_edit.clone();
                self.apply(st);
                self.path_edit = path_keep;
            }
        }
    }

    fn load_dir(&mut self, host: &HostClient, path: &str) {
        match host.fs_list(path) {
            Ok(list) => {
                self.dir_roots = list.roots;
                self.dir_path = list.path;
                self.dir_parent = list.parent;
                self.dir_entries = list.entries;
                self.dir_error = None;
                self.dir_loaded = true;
            }
            Err(e) => {
                self.dir_error = Some(e);
                self.dir_entries.clear();
                self.dir_loaded = true;
            }
        }
    }

    fn pump_posters(&mut self, host: &HostClient, project_id: &str, ctx: &egui::Context) {
        for result in self.poster_loader.poll() {
            if let Ok(color) = result.image {
                let tex = ctx.load_texture(
                    format!("ingest_poster_{}", result.clip_id),
                    color,
                    TextureOptions::LINEAR,
                );
                self.poster_textures.insert(result.clip_id, tex);
            }
        }
        let Some(st) = &self.state else {
            return;
        };
        for c in &st.clips {
            // Snapshot (DB) must expose thumb_url before we fetch — no 404 probing.
            if c.thumb_url.trim().is_empty() || self.poster_textures.contains_key(&c.clip_id) {
                continue;
            }
            let url = host.ingest_thumbnail_url(project_id, &c.clip_id);
            self.poster_loader
                .request(c.clip_id.clone(), url, Some(ctx.clone()));
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
        if !self.dir_loaded {
            self.load_dir(host, "");
        }
        if self.project_id != project_id {
            self.project_id = project_id.to_string();
        }

        let mut action = IngestAction::None;
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
                        self.dispatch_pool_head(head);
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
                                busy: self.busy,
                            };
                            match dir_list::show(ui, input) {
                                DirListAction::None => {}
                                DirListAction::SelectKind(kind) => {
                                    self.dir_browser = kind;
                                    self.dir_error = None;
                                    match kind {
                                        DirBrowserKind::Local => self.load_dir(host, ""),
                                        DirBrowserKind::Lan | DirBrowserKind::Internet => {
                                            self.dir_roots = true;
                                            self.dir_path.clear();
                                            self.dir_parent = None;
                                            self.dir_entries.clear();
                                        }
                                    }
                                }
                                DirListAction::OpenPath(path) => self.load_dir(host, &path),
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
                        self.preview_clip_id = id.clone();
                        action = IngestAction::Toggle(id);
                    }
                    MediaPoolAction::None
                    | MediaPoolAction::SwitchTab(_)
                    | MediaPoolAction::SelectShot(_)
                    | MediaPoolAction::SelectPart(_)
                    | MediaPoolAction::DeletePart(_)
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
    fn dispatch_pool_head(&mut self, action: MediaPoolAction) {
        let enabled = !self.preview_clip_id.is_empty();
        match action {
            MediaPoolAction::None => {}
            MediaPoolAction::SwitchTab(tab) => {
                // Ingest never uses Segment — clamp away if somehow selected.
                self.library_tab = match tab {
                    LibraryTab::Segment => LibraryTab::All,
                    other => other,
                };
            }
            MediaPoolAction::TogglePlay if enabled => self.queue_toggle_play(),
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
            | MediaPoolAction::DeletePart(_) => {}
        }
    }

    pub fn timeline_dock_height(&self) -> f32 {
        let dur = self
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
        qnc_source_dock::dock_height(self.expanded_audio, true, dur)
    }

    pub fn ui_timeline_dock(
        &mut self,
        ui: &mut egui::Ui,
        playback: &mut PlaybackStack,
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
        let tc = |s: f64| format_tc(s, self.selected_source_fps);
        let fps = if self.selected_source_fps.is_finite() && self.selected_source_fps > 0.0 {
            self.selected_source_fps
        } else {
            25.0
        };
        if playback.carrier().is_active() {
            let frame = playback.carrier().display_frame().0;
            self.virtual_sec = frame_to_seconds(frame, fps).clamp(0.0, dur);
        }
        let fallback_frame = seconds_to_frame(self.virtual_sec.clamp(0.0, dur), fps);
        let timeline_model =
            playback.timeline_model_for_clip(fps, dur, 0, seconds_to_frame(dur, fps), fallback_frame);
        let dock = qnc_source_dock::show(
            ui,
            SourceDockInput {
                clip_label: label,
                source_in: 0.0,
                source_out: dur,
                timeline_model,
                focus: TimelineFocusPaint::Playhead,
                a1_peaks: empty,
                a2_peaks: empty,
                frames: &frames,
                tc: &tc,
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
            SourceDockAction::CueFrame(frame) => {
                // Progress bar → CueFrame; Space plays from this position.
                if let Err(err) = crate::player_bridge::build_open_request(self)
                    .and_then(|request| playback.cue_timeline_click(request, frame))
                {
                    self.player_status = err;
                }
                IngestAction::None
            }
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
            SourceDockAction::SaveVirtualShot | SourceDockAction::CreatePart(_) => {
                IngestAction::None
            }
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
        match action {
            IngestAction::None => Ok(()),
            IngestAction::Reload => {
                self.reload(host, project_id);
                Ok(())
            }
            IngestAction::PickFolder => {
                let initial = self.path_edit.clone();
                match host.pick_directory(&initial)? {
                    Some(path) => {
                        self.path_edit = path.clone();
                        self.load_dir(host, &path);
                        self.busy = true;
                        let st = host.ingest_browse(project_id, &path)?;
                        self.apply(st);
                        self.busy = false;
                        self.message = Some(format!("Discovered from {path}"));
                        Ok(())
                    }
                    None => {
                        self.message = Some("Odustano.".into());
                        Ok(())
                    }
                }
            }
            IngestAction::ConfirmDir => {
                let path = self.dir_path.trim().to_string();
                if path.is_empty() || self.dir_roots {
                    return Err("Odaberi mapu u stablu.".into());
                }
                self.path_edit = path.clone();
                self.busy = true;
                let st = host.ingest_browse(project_id, &path)?;
                self.apply(st);
                self.busy = false;
                self.message = Some(format!("Otkrij: {path}"));
                Ok(())
            }
            IngestAction::CancelDir => {
                self.dir_browser = DirBrowserKind::Local;
                self.load_dir(host, "");
                self.message = Some("Stablo: računalo.".into());
                Ok(())
            }
            IngestAction::BrowsePath => {
                let path = if !self.dir_path.is_empty() && !self.dir_roots {
                    self.dir_path.clone()
                } else {
                    self.path_edit.trim().to_string()
                };
                if path.is_empty() {
                    return Err("Odaberi mapu (U redu) ili upiši putanju.".into());
                }
                self.path_edit = path.clone();
                self.busy = true;
                let st = host.ingest_browse(project_id, &path)?;
                self.apply(st);
                self.busy = false;
                self.message = Some("Otkrij gotov.".into());
                Ok(())
            }
            IngestAction::Discover => {
                self.busy = true;
                let st = host.ingest_discover(project_id)?;
                self.apply(st);
                self.busy = false;
                self.message = Some("Ponovo otkrij gotov.".into());
                Ok(())
            }
            IngestAction::SelectAll => {
                let st = host.ingest_select_all(project_id)?;
                self.apply(st);
                Ok(())
            }
            IngestAction::ClearSelection => {
                let st = host.ingest_set_selection(project_id, &[])?;
                self.apply(st);
                Ok(())
            }
            IngestAction::Toggle(clip_id) => {
                self.activate_preview_clip(host, project_id, &clip_id);
                let st = host.ingest_toggle(project_id, &clip_id)?;
                self.apply(st);
                Ok(())
            }
            IngestAction::FocusPreview(clip_id) => {
                self.activate_preview_clip(host, project_id, &clip_id);
                Ok(())
            }
            IngestAction::SetArchive(v) => {
                let st = host.ingest_set_archive_original(project_id, v)?;
                self.apply(st);
                Ok(())
            }
            IngestAction::ImportSelected => {
                let selected_n = self
                    .state
                    .as_ref()
                    .map(|s| s.selected_clip_ids.len())
                    .unwrap_or(0);
                if selected_n == 0 {
                    return Err("Nema odabranih klipova.".into());
                }
                self.busy = true;
                // Empty clip_ids → host uvozi sve `selected=1` iz SQLite (video + audio).
                let st = host.ingest_import(project_id, &[])?;
                let queued = st.queued.unwrap_or(selected_n as u64);
                self.apply(st);
                self.busy = false;
                self.message = Some(format!("Uvoz u bazi: {queued} klip(ova)."));
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
    let fps = if fps.is_finite() && fps > 0.0 {
        fps
    } else {
        25.0
    };
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
