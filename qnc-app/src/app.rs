//! Native shell — Project + Ingest + Media Assist + Story.

use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};
use serde_json::Value;

use crate::api::{self, EditorialPlaylist, HostClient, ProjectRow, TimelineModel, Workspace};
use crate::component_errors::{ComponentErrorBoundary, ComponentErrorKey};
use crate::component_runtime::{ComponentBackendCommand, ComponentBackendRuntime};
use crate::components::{
    EditorialEditComponent, EditorialEditData, EditorialEditKind, EditorialStateComponent,
    EditorialStateData, ExportHiResStatus, FilesystemListComponent, HiResPreviewOpen,
    HiResPreviewPlayerAction, HiResPreviewPlayerComponent, HiResPreviewPlayerState,
    HiResRenderTransportComponent, PlaybackMediaResolution, PlaybackMediaResolverComponent,
    ProjectCatalogComponent, ProjectCommandComponent, ProjectCommandData, ProjectCommandKind,
    ProjectExportProfileComponent, ProjectRegistryComponent, ShellStateComponent, ShellStateData,
    ShortcutBindingsComponent, ShortcutBindingsData, SourceImportCommandComponent,
    SourceImportCommandKind, SourceImportSelectionComponent, SourceImportStateComponent,
    SourceImportStateKind, SourceImportStatusComponent, ThemePickerComponent,
};
use crate::composition::{ScreenComposition, WorkflowScreen};
use crate::ingest::IngestScreen;
use crate::media_assist::MediaAssistScreen;
use crate::playback_routing::PlaybackTransportIntent;
use crate::playback_stack::PlaybackStack;
use crate::player_bridge;
use crate::project::{ProjectAction, ProjectScreen};
use crate::qnc_broadcast_player::BroadcastPlayerRx;
use crate::qnc_theme;
use crate::shortcuts::{PROJECT_SHORTCUT_SCOPE, STORYBOARD_SHORTCUT_SCOPE};
use crate::story::StoryScreen;

const FS_INSTANCE_PROJECTS_ROOT: &str = "settings.storage.projects_root";
const FS_INSTANCE_EXPORT_DIR: &str = "settings.export.directory";
const FS_INSTANCE_IMPORT_SOURCE: &str = "source.import.location";
const EDITORIAL_INSTANCE_STORY: &str = "story";
const EDITORIAL_INSTANCE_MEDIA_ASSIST: &str = "media_assist";
const EDITORIAL_INSTANCE_IMPORT_STATUS: &str = "source_import_status";
const SHORTCUT_INSTANCE_INGEST: &str = "ingest";
const SHORTCUT_INSTANCE_PROJECT: &str = "project";
const BACKGROUND_PLAYBACK_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Phase {
    HostGate,
    ProjectOnly,
    Workspace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Screen {
    Project,
    Ingest,
    MediaAssist,
    Story,
    Unsupported(String),
}

/// One workflow navigation intent — Project open, footer, and Uvezi share `go_workflow`.
enum WorkflowGo<'a> {
    /// Explicit tab id (footer).
    Tab(&'a str),
    /// First workflow tab after open (DB workflow entry step).
    Entry,
    /// Next tab after `from` in DB workflow step graph.
    Next { from: &'a str },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WorkspaceGo {
    Entry,
}

pub struct QncApp {
    host: HostClient,
    host_url_edit: String,
    pub(crate) phase: Phase,
    pub(crate) screen: Screen,
    status: String,
    error: Option<String>,
    health_ok: bool,
    runtime_summary: String,
    projects: Vec<ProjectRow>,
    active_project_id: String,
    selected_index: Option<usize>,
    open_project: Option<ProjectRow>,
    workspace: Option<Workspace>,
    auto_connect_once: bool,
    /// One-shot maximize — belt-and-suspenders if ViewportBuilder missed it (any OS).
    maximize_once: bool,
    project_ui: ProjectScreen,
    pub(crate) ingest: IngestScreen,
    pub(crate) media_assist: MediaAssistScreen,
    pub(crate) story: StoryScreen,
    /// Sole broadcast player (+ carrier / monitor). Screens only host the UI slots.
    pub(crate) playback: PlaybackStack,
    playback_rx: BroadcastPlayerRx,
    hires_preview_player: HiResPreviewPlayerState,
    component_backend: ComponentBackendRuntime,
    component_errors: ComponentErrorBoundary,
    source_import_status: SourceImportStatusComponent,
    theme_picker: ThemePickerComponent,
    background_playback_active_sent: Option<bool>,
    background_playback_last_submit: Option<Instant>,
    next_project_request_id: u64,
    pending_workspace_go: Option<WorkspaceGo>,
}

impl QncApp {
    /// Orchestrator picks shared block composition for the active screen.
    fn composition(&self) -> ScreenComposition {
        let wf = if self.phase == Phase::HostGate {
            WorkflowScreen::HostGate
        } else {
            match &self.screen {
                Screen::Project => WorkflowScreen::Project,
                Screen::Ingest => WorkflowScreen::Ingest,
                Screen::MediaAssist => WorkflowScreen::MediaAssist,
                Screen::Story => WorkflowScreen::Story,
                Screen::Unsupported(_) => WorkflowScreen::Unsupported,
            }
        };
        ScreenComposition::resolve(wf)
    }

    pub fn new(base_url: String) -> Self {
        let playback = PlaybackStack::new();
        let playback_rx = playback.subscribe();
        Self {
            host: HostClient::new(&base_url),
            host_url_edit: base_url,
            phase: Phase::HostGate,
            screen: Screen::Project,
            status: "Connecting…".into(),
            error: None,
            health_ok: false,
            runtime_summary: String::new(),
            projects: Vec::new(),
            active_project_id: String::new(),
            selected_index: None,
            open_project: None,
            workspace: None,
            auto_connect_once: true,
            maximize_once: true,
            project_ui: ProjectScreen::default(),
            ingest: IngestScreen::default(),
            media_assist: StoryScreen::media_assist(),
            story: StoryScreen::story(),
            playback,
            playback_rx,
            hires_preview_player: HiResPreviewPlayerState::default(),
            component_backend: ComponentBackendRuntime::new(),
            component_errors: ComponentErrorBoundary::default(),
            source_import_status: SourceImportStatusComponent::default(),
            theme_picker: ThemePickerComponent::default(),
            background_playback_active_sent: None,
            background_playback_last_submit: None,
            next_project_request_id: 1,
            pending_workspace_go: None,
        }
    }

    fn screen_from_tab(tab: &str) -> Screen {
        if tab == "project" {
            Screen::Project
        } else if tab == "ingest" {
            Screen::Ingest
        } else if api::is_media_assist_tab(tab) {
            Screen::MediaAssist
        } else if api::is_story_tab(tab) {
            Screen::Story
        } else {
            Screen::Unsupported(tab.to_string())
        }
    }

    fn connect_and_load(&mut self, ctx: Option<egui::Context>) {
        self.error = None;
        self.host.set_base_url(&self.host_url_edit);
        self.background_playback_active_sent = None;
        self.background_playback_last_submit = None;
        self.status = "Connecting…".into();
        self.submit_shell_state_command(ShellStateComponent::health(), ctx);
    }

    fn load_appearance(&mut self, ctx: Option<egui::Context>) {
        self.submit_shell_state_command(ShellStateComponent::appearance(), ctx);
    }

    fn apply_theme(&self, ctx: &egui::Context) {
        let theme = self.theme_picker.active();
        let tokens = theme.tokens();
        qnc_theme::set_active(ctx, theme);
        qnc_theme::apply_egui_visuals(ctx, &tokens);
    }

    fn global_error_message(&self) -> Option<String> {
        self.error
            .clone()
            .or_else(|| self.component_errors.last_message())
    }

    fn short_error_label(message: &str) -> String {
        const MAX_CHARS: usize = 96;
        if message.chars().count() <= MAX_CHARS {
            return message.to_string();
        }
        let mut out: String = message.chars().take(MAX_CHARS.saturating_sub(3)).collect();
        out.push_str("...");
        out
    }

    fn load_project_list(&mut self, ctx: Option<egui::Context>) {
        let command = ProjectRegistryComponent::list_projects();
        if let Err(e) = self.submit_component_backend_command(command, ctx) {
            self.error = Some(e);
        }
    }

    fn apply_project_list(&mut self, list: crate::api::ProjectsList) {
        self.projects = list.projects;
        self.active_project_id = list.active_project_id;
        let active_index = self
            .projects
            .iter()
            .position(|p| p.project_id == self.active_project_id);
        if active_index.is_some() || (self.selected_index.is_none() && !self.projects.is_empty()) {
            self.selected_index = Some(active_index.unwrap_or(0));
        }
        if let Some(idx) = self.selected_index {
            if idx >= self.projects.len() {
                self.selected_index = if self.projects.is_empty() {
                    None
                } else {
                    Some(0)
                };
            }
        }
        self.status = format!("{} project(s)", self.projects.len());
    }

    fn next_project_request_id(&mut self) -> u64 {
        let id = self.next_project_request_id;
        self.next_project_request_id = self.next_project_request_id.saturating_add(1);
        id
    }

    fn submit_component_backend_command(
        &mut self,
        command: ComponentBackendCommand,
        ctx: Option<egui::Context>,
    ) -> Result<u64, String> {
        let key = ComponentErrorKey::from_command(&command);
        self.component_backend
            .submit(&self.host, command, ctx)
            .map_err(|e| self.record_component_error(key, e))
    }

    fn record_component_error(
        &mut self,
        key: ComponentErrorKey,
        error: impl Into<String>,
    ) -> String {
        let error = error.into();
        self.component_errors.record(key, error.clone());
        error
    }

    fn clear_component_error(&mut self, key: &ComponentErrorKey) {
        self.component_errors.clear(key);
    }

    fn submit_shell_state_command(
        &mut self,
        command: ComponentBackendCommand,
        ctx: Option<egui::Context>,
    ) {
        if let Err(e) = self.submit_component_backend_command(command, ctx) {
            self.error = Some(e);
        }
    }

    fn sync_background_playback_gate(&mut self, ctx: Option<egui::Context>) {
        if !self.health_ok {
            return;
        }
        let active = self.phase == Phase::Workspace && self.playback.blocks_background_work();
        let now = Instant::now();
        let heartbeat_due = active
            && self.background_playback_active_sent == Some(true)
            && self
                .background_playback_last_submit
                .map(|last| now.duration_since(last) >= BACKGROUND_PLAYBACK_HEARTBEAT_INTERVAL)
                .unwrap_or(true);
        if self.background_playback_active_sent == Some(active) && !heartbeat_due {
            return;
        }
        let command = ShellStateComponent::background_playback(active);
        let submitted_at = now;
        match self.submit_component_backend_command(command, ctx) {
            Ok(_) => {
                self.background_playback_active_sent = Some(active);
                self.background_playback_last_submit = Some(submitted_at);
            }
            Err(_) => {
                self.background_playback_active_sent = None;
                self.background_playback_last_submit = None;
            }
        }
    }

    fn submit_project_command(
        &mut self,
        command: ComponentBackendCommand,
        ctx: Option<egui::Context>,
    ) {
        if let Err(e) = self.submit_component_backend_command(command, ctx) {
            self.error = Some(e);
        }
    }

    fn request_workspace(
        &mut self,
        project_id: &str,
        followup: Option<WorkspaceGo>,
        ctx: Option<egui::Context>,
    ) {
        if project_id.trim().is_empty() {
            return;
        }
        if followup.is_some() {
            self.pending_workspace_go = followup;
        }
        self.submit_shell_state_command(ShellStateComponent::workspace(project_id), ctx);
    }

    fn apply_open_project(
        &mut self,
        project: ProjectRow,
        active_project_id: String,
        ctx: Option<egui::Context>,
    ) {
        let name = project.name.clone();
        let project_id = project.project_id.clone();
        self.active_project_id = active_project_id;
        self.open_project = Some(project);
        self.workspace = None;
        self.ingest = IngestScreen::default();
        self.source_import_status.reset();
        self.source_import_status.watch_project(&project_id);
        self.story.reset_session(&self.host);
        self.story = StoryScreen::story();
        self.media_assist.reset_session(&self.host);
        self.media_assist = StoryScreen::media_assist();
        self.playback.stop();
        self.status = format!("Opening {name}…");
        self.request_workspace(&project_id, Some(WorkspaceGo::Entry), ctx);
    }

    fn open_project_id(&mut self, project_id: &str, ctx: Option<egui::Context>) {
        self.error = None;
        let command =
            ProjectCommandComponent::open_project(self.next_project_request_id(), project_id);
        self.submit_project_command(command, ctx);
    }

    fn open_selected(&mut self, ctx: Option<egui::Context>) {
        let Some(idx) = self.selected_index else {
            self.error = Some("Select a project first.".into());
            return;
        };
        let Some(row) = self.projects.get(idx).cloned() else {
            self.error = Some("Invalid selection.".into());
            return;
        };
        self.open_project_id(&row.project_id, ctx);
    }

    fn close_project(&mut self) {
        self.story.reset_session(&self.host);
        self.media_assist.reset_session(&self.host);
        self.playback.stop();
        self.open_project = None;
        self.workspace = None;
        self.ingest = IngestScreen::default();
        self.source_import_status.reset();
        self.story = StoryScreen::story();
        self.media_assist = StoryScreen::media_assist();
        self.phase = if self.health_ok {
            Phase::ProjectOnly
        } else {
            Phase::HostGate
        };
        self.screen = Screen::Project;
        self.status = "Project closed (local UI; host active id unchanged)".into();
    }

    /// Single workflow navigation — Project open, footer, Uvezi all use this.
    fn go_workflow(&mut self, go: WorkflowGo<'_>) -> Option<String> {
        let tab = match go {
            WorkflowGo::Tab(t) => t.to_string(),
            WorkflowGo::Entry => self
                .workspace
                .as_ref()
                .map(api::workflow_entry_tab)
                .unwrap_or_else(|| "project".into()),
            WorkflowGo::Next { from } => {
                let Some(next) = self
                    .workspace
                    .as_ref()
                    .and_then(|workspace| api::workflow_next_tab(workspace, from))
                else {
                    self.status = "Nema sljedećeg workflow taba.".into();
                    return None;
                };
                next
            }
        };
        self.activate_screen(&tab);
        Some(tab)
    }

    fn reload_workspace(&mut self, ctx: Option<egui::Context>) {
        let Some(pid) = self.open_project.as_ref().map(|p| p.project_id.clone()) else {
            return;
        };
        self.request_workspace(&pid, None, ctx);
    }

    fn activate_screen(&mut self, tab: &str) {
        if self.open_project.is_none() && tab != "project" {
            self.error = Some("Otvori projekt prije workflow taba.".into());
            self.screen = Screen::Project;
            return;
        }
        let next = Self::screen_from_tab(tab);
        if self.screen == Screen::Story && next != Screen::Story {
            self.story.suspend_playback_session();
            self.playback.stop();
        }
        if self.screen == Screen::MediaAssist && next != Screen::MediaAssist {
            self.media_assist.suspend_playback_session();
            self.playback.stop();
        }
        if self.screen == Screen::Ingest && next != Screen::Ingest {
            self.ingest.reset_player_session();
            self.playback.stop();
        }
        // Drain stale RX before switching forms — one shared player, one subscriber.
        let _ = self.playback_rx.try_recv_all();
        self.screen = next;
        self.on_screen_entered();
    }

    fn dispatch_ingest(
        &mut self,
        pid: &str,
        action: crate::ingest::IngestAction,
        ctx: &egui::Context,
    ) {
        match action {
            crate::ingest::IngestAction::CueFrame(frame) => {
                if !pid.trim().is_empty() {
                    self.ingest.ensure_preview_playback_ready(pid);
                    self.submit_ingest_backend_commands(Some(ctx.clone()));
                }
                if !self.playback_transport_available() {
                    self.ingest
                        .apply_player_error(self.ingest_playback_not_ready_message());
                    return;
                }
                self.playback_transport_cue_frame(frame);
                return;
            }
            crate::ingest::IngestAction::TogglePlay => {
                if !pid.trim().is_empty() {
                    self.ingest.ensure_preview_playback_ready(pid);
                    self.submit_ingest_backend_commands(Some(ctx.clone()));
                }
                if !self.playback_transport_available() {
                    self.ingest.request_play_after_resolve();
                    self.ingest
                        .apply_player_error(self.ingest_playback_not_ready_message());
                    return;
                }
                self.playback_transport_toggle();
                return;
            }
            crate::ingest::IngestAction::RequestState(project_id) => {
                self.load_ingest_state(&project_id, Some(ctx.clone()));
                return;
            }
            crate::ingest::IngestAction::RequestDirList(path) => {
                self.load_ingest_dir_browser(&path, Some(ctx.clone()));
                return;
            }
            crate::ingest::IngestAction::Reload => {
                self.load_ingest_state(pid, Some(ctx.clone()));
                return;
            }
            crate::ingest::IngestAction::ConfirmDir => {
                match self.ingest.confirm_dir_path() {
                    Ok(path) => self.browse_import_source(pid, &path, Some(ctx.clone())),
                    Err(e) => self.error = Some(e),
                }
                return;
            }
            crate::ingest::IngestAction::BrowsePath => {
                match self.ingest.browse_path_candidate() {
                    Ok(path) => self.browse_import_source(pid, &path, Some(ctx.clone())),
                    Err(e) => self.error = Some(e),
                }
                return;
            }
            crate::ingest::IngestAction::Discover => {
                self.submit_source_import_command(
                    pid,
                    SourceImportCommandComponent::discover(pid),
                    Some(ctx.clone()),
                );
                return;
            }
            crate::ingest::IngestAction::SelectAll => {
                let previous = self.ingest.select_all_clip_selection_local();
                if previous.is_some() {
                    ctx.request_repaint();
                }
                if let Some(previous) = previous {
                    let selected_clip_ids = self.ingest.selected_clip_ids();
                    let revision = self.ingest.selection_revision();
                    if let Err(e) = self.save_import_selection(
                        pid,
                        &selected_clip_ids,
                        revision,
                        Some(ctx.clone()),
                    ) {
                        self.ingest.restore_clip_selection_local(previous);
                        self.error = Some(e);
                    }
                }
                return;
            }
            crate::ingest::IngestAction::ClearSelection => {
                let previous = self.ingest.clear_clip_selection_local();
                if previous.is_some() {
                    ctx.request_repaint();
                }
                if let Some(previous) = previous {
                    let selected_clip_ids = self.ingest.selected_clip_ids();
                    let revision = self.ingest.selection_revision();
                    if let Err(e) = self.save_import_selection(
                        pid,
                        &selected_clip_ids,
                        revision,
                        Some(ctx.clone()),
                    ) {
                        self.ingest.restore_clip_selection_local(previous);
                        self.error = Some(e);
                    }
                }
                return;
            }
            crate::ingest::IngestAction::SetArchive(archive_original) => {
                self.ingest.set_archive_draft(archive_original);
                self.submit_source_import_command(
                    pid,
                    SourceImportCommandComponent::set_archive_original(pid, archive_original),
                    Some(ctx.clone()),
                );
                return;
            }
            crate::ingest::IngestAction::ImportSelected => {
                let selected_clip_ids = self.ingest.selected_clip_ids();
                if selected_clip_ids.is_empty() {
                    self.error = Some("Nema odabranih klipova.".into());
                    return;
                }
                let revision = self.ingest.selection_revision();
                if let Err(e) =
                    self.save_import_selection(pid, &selected_clip_ids, revision, Some(ctx.clone()))
                {
                    self.error = Some(e);
                    return;
                }
                self.source_import_status.mark_possible_work(pid);
                self.submit_source_import_command(
                    pid,
                    SourceImportCommandComponent::import_selected(pid, &selected_clip_ids),
                    Some(ctx.clone()),
                );
                return;
            }
            crate::ingest::IngestAction::ApproveProxyPosters(clip_ids) => {
                if clip_ids.is_empty() {
                    self.error = Some("Nema klipova bez postera s kartice.".into());
                    return;
                }
                self.source_import_status.mark_possible_work(pid);
                self.submit_source_import_command(
                    pid,
                    SourceImportCommandComponent::approve_proxy_posters(pid, &clip_ids),
                    Some(ctx.clone()),
                );
                return;
            }
            crate::ingest::IngestAction::Toggle(clip_id) => {
                let previous = self.ingest.toggle_clip_selection_local(&clip_id);
                if previous.is_some() {
                    ctx.request_repaint();
                }
                if let Some(previous) = previous {
                    let selected_clip_ids = self.ingest.selected_clip_ids();
                    let revision = self.ingest.selection_revision();
                    if let Err(e) = self.save_import_selection(
                        pid,
                        &selected_clip_ids,
                        revision,
                        Some(ctx.clone()),
                    ) {
                        self.ingest.set_clip_selection_local(&clip_id, previous);
                        self.error = Some(e);
                    }
                }
                return;
            }
            _ => {}
        }
        let cue_after_dispatch = matches!(&action, crate::ingest::IngestAction::FocusPreview(_));
        if let Err(e) = self.ingest.dispatch(&self.host, pid, action) {
            self.error = Some(e);
        } else {
            self.submit_ingest_backend_commands(Some(ctx.clone()));
            if cue_after_dispatch {
                if let Some(frame) = self.ingest.transport_cue_frame() {
                    self.playback_transport_cue_frame(frame);
                }
            }
        }
    }

    fn ingest_playback_not_ready_message(&self) -> String {
        let status = self.ingest.player_status.trim();
        if !status.is_empty() {
            status.to_string()
        } else if self.ingest.preview_clip_id.trim().is_empty() {
            "Odaberi klip prije play".into()
        } else {
            "Ingest play input nije spreman".into()
        }
    }

    fn on_screen_entered(&mut self) {
        let Some(p) = self.open_project.clone() else {
            return;
        };
        match self.screen {
            Screen::Ingest => self.ingest.ensure_loaded(&self.host, &p.project_id),
            _ => {}
        }
    }

    fn request_editorial_state_if_needed(
        &mut self,
        instance_id: &str,
        project_id: &str,
        ctx: Option<egui::Context>,
    ) {
        let needs_load = match instance_id {
            EDITORIAL_INSTANCE_STORY => self.story.needs_meta_load(project_id),
            EDITORIAL_INSTANCE_MEDIA_ASSIST => self.media_assist.needs_meta_load(project_id),
            _ => false,
        };
        if needs_load {
            self.load_editorial_state(instance_id, project_id, ctx);
        }
    }

    fn load_editorial_state(
        &mut self,
        instance_id: &str,
        project_id: &str,
        ctx: Option<egui::Context>,
    ) {
        if project_id.trim().is_empty() {
            return;
        }
        let command = EditorialStateComponent::load_story_state(instance_id, project_id);
        match instance_id {
            EDITORIAL_INSTANCE_STORY => self.story.begin_meta_load(project_id, 3),
            EDITORIAL_INSTANCE_MEDIA_ASSIST => self.media_assist.begin_meta_load(project_id, 3),
            _ => return,
        }
        if let Err(e) = self.submit_component_backend_command(command, ctx) {
            self.set_editorial_state_error(instance_id, project_id, e);
        }
    }

    fn load_editorial_timeline_model(
        &mut self,
        instance_id: &str,
        project_id: &str,
        ctx: Option<egui::Context>,
    ) {
        if project_id.trim().is_empty() {
            return;
        }
        let commands = [
            EditorialStateComponent::load_timeline_model(instance_id, project_id),
            EditorialStateComponent::load_playlist(instance_id, project_id),
        ];
        for command in commands {
            if let Err(e) = self.submit_component_backend_command(command, ctx.clone()) {
                self.set_editorial_state_error(instance_id, project_id, e);
            }
        }
    }

    fn ensure_cached_editorial_project(screen: &mut StoryScreen, project_id: &str) {
        if !screen.has_editorial_project(project_id) {
            screen.begin_cached_meta_load(project_id);
        }
    }

    fn mirror_editorial_story_state_to_peer(
        &mut self,
        instance_id: &str,
        project_id: &str,
        state: Value,
    ) {
        match instance_id {
            EDITORIAL_INSTANCE_STORY => {
                Self::ensure_cached_editorial_project(&mut self.media_assist, project_id);
                self.media_assist
                    .apply_editorial_story_state(project_id, state);
            }
            EDITORIAL_INSTANCE_MEDIA_ASSIST => {
                Self::ensure_cached_editorial_project(&mut self.story, project_id);
                self.story.apply_editorial_story_state(project_id, state);
            }
            _ => {}
        }
    }

    fn mirror_editorial_timeline_model_to_peer(
        &mut self,
        instance_id: &str,
        project_id: &str,
        timeline: TimelineModel,
    ) {
        match instance_id {
            EDITORIAL_INSTANCE_STORY => {
                if self.media_assist.has_editorial_project(project_id) {
                    let _ = self
                        .media_assist
                        .apply_editorial_timeline_model(project_id, timeline);
                }
            }
            EDITORIAL_INSTANCE_MEDIA_ASSIST => {
                if self.story.has_editorial_project(project_id) {
                    let _ = self
                        .story
                        .apply_editorial_timeline_model(project_id, timeline);
                }
            }
            _ => {}
        }
    }

    fn mirror_editorial_playlist_to_peer(
        &mut self,
        instance_id: &str,
        project_id: &str,
        playlist: EditorialPlaylist,
    ) {
        match instance_id {
            EDITORIAL_INSTANCE_STORY => {
                if self.media_assist.has_editorial_project(project_id) {
                    let _ = self
                        .media_assist
                        .apply_editorial_playlist(project_id, playlist);
                }
            }
            EDITORIAL_INSTANCE_MEDIA_ASSIST => {
                if self.story.has_editorial_project(project_id) {
                    let _ = self.story.apply_editorial_playlist(project_id, playlist);
                }
            }
            _ => {}
        }
    }

    fn mirror_editorial_edit_data_to_peer(
        &mut self,
        instance_id: &str,
        project_id: &str,
        data: EditorialEditData,
    ) {
        match instance_id {
            EDITORIAL_INSTANCE_STORY => {
                Self::ensure_cached_editorial_project(&mut self.media_assist, project_id);
                let _ = self.media_assist.apply_editorial_edit_data(data);
            }
            EDITORIAL_INSTANCE_MEDIA_ASSIST => {
                Self::ensure_cached_editorial_project(&mut self.story, project_id);
                let _ = self.story.apply_editorial_edit_data(data);
            }
            _ => {}
        }
    }

    fn submit_editorial_backend_commands(&mut self, instance_id: &str, ctx: Option<egui::Context>) {
        let commands = match instance_id {
            EDITORIAL_INSTANCE_STORY => self.story.drain_backend_commands(),
            EDITORIAL_INSTANCE_MEDIA_ASSIST => self.media_assist.drain_backend_commands(),
            _ => Vec::new(),
        };
        for command in commands {
            if let Err(e) = self.submit_component_backend_command(command, ctx.clone()) {
                match instance_id {
                    EDITORIAL_INSTANCE_STORY => self.story.apply_player_error(e),
                    EDITORIAL_INSTANCE_MEDIA_ASSIST => self.media_assist.apply_player_error(e),
                    _ => self.error = Some(e),
                }
            }
        }
    }

    fn submit_ingest_backend_commands(&mut self, ctx: Option<egui::Context>) {
        for command in self.ingest.drain_backend_commands() {
            if let Err(e) = self.submit_component_backend_command(command, ctx.clone()) {
                self.ingest.apply_player_error(e);
            }
        }
    }

    fn set_editorial_state_error(
        &mut self,
        instance_id: &str,
        project_id: &str,
        error: impl Into<String>,
    ) {
        match instance_id {
            EDITORIAL_INSTANCE_STORY => self.story.set_editorial_meta_error(project_id, error),
            EDITORIAL_INSTANCE_MEDIA_ASSIST => self
                .media_assist
                .set_editorial_meta_error(project_id, error),
            _ => self.error = Some(error.into()),
        }
    }

    fn set_editorial_edit_error(
        &mut self,
        instance_id: &str,
        project_id: &str,
        kind: EditorialEditKind,
        error: impl Into<String>,
    ) {
        match instance_id {
            EDITORIAL_INSTANCE_STORY => {
                self.story.set_editorial_edit_error(project_id, kind, error)
            }
            EDITORIAL_INSTANCE_MEDIA_ASSIST => self
                .media_assist
                .set_editorial_edit_error(project_id, kind, error),
            _ => self.error = Some(error.into()),
        }
    }

    fn apply_export_hires_submit(
        &mut self,
        instance_id: &str,
        project_id: &str,
        response: qnc_service_contracts::ExportHiResSubmitResponse,
    ) {
        match instance_id {
            EDITORIAL_INSTANCE_STORY => {
                self.story.apply_export_hires_submit(project_id, response);
                self.status = self.story.status_text().to_owned();
            }
            EDITORIAL_INSTANCE_MEDIA_ASSIST => {
                self.media_assist
                    .apply_export_hires_submit(project_id, response);
                self.status = self.media_assist.status_text().to_owned();
            }
            _ => {}
        }
    }

    fn set_export_hires_error(
        &mut self,
        instance_id: &str,
        project_id: &str,
        error: impl Into<String>,
    ) {
        let error = error.into();
        match instance_id {
            EDITORIAL_INSTANCE_STORY => {
                self.story.set_export_hires_error(project_id, error);
                self.status = self.story.status_text().to_owned();
            }
            EDITORIAL_INSTANCE_MEDIA_ASSIST => {
                self.media_assist.set_export_hires_error(project_id, error);
                self.status = self.media_assist.status_text().to_owned();
            }
            _ => self.error = Some(error),
        }
    }

    fn apply_export_hires_status(
        &mut self,
        instance_id: &str,
        project_id: &str,
        status: ExportHiResStatus,
    ) {
        match instance_id {
            EDITORIAL_INSTANCE_STORY => {
                self.story.apply_export_hires_status(project_id, status);
                self.status = self.story.status_text().to_owned();
            }
            EDITORIAL_INSTANCE_MEDIA_ASSIST => {
                self.media_assist
                    .apply_export_hires_status(project_id, status);
                self.status = self.media_assist.status_text().to_owned();
            }
            _ => {}
        }
    }

    fn set_export_hires_status_error(
        &mut self,
        instance_id: &str,
        project_id: &str,
        error: impl Into<String>,
    ) {
        let error = error.into();
        match instance_id {
            EDITORIAL_INSTANCE_STORY => {
                self.story.set_export_hires_status_error(project_id, error);
                self.status = self.story.status_text().to_owned();
            }
            EDITORIAL_INSTANCE_MEDIA_ASSIST => {
                self.media_assist
                    .set_export_hires_status_error(project_id, error);
                self.status = self.media_assist.status_text().to_owned();
            }
            _ => self.error = Some(error),
        }
    }

    fn apply_preview_hires_submit(
        &mut self,
        instance_id: &str,
        project_id: &str,
        response: qnc_service_contracts::PreviewHiResInputResponse,
        ctx: Option<egui::Context>,
    ) {
        match instance_id {
            EDITORIAL_INSTANCE_STORY => {
                self.story
                    .apply_preview_hires_submit(project_id, response.clone());
                self.status = self.story.status_text().to_owned();
            }
            EDITORIAL_INSTANCE_MEDIA_ASSIST => {
                self.media_assist
                    .apply_preview_hires_submit(project_id, response.clone());
                self.status = self.media_assist.status_text().to_owned();
            }
            _ => {
                self.error = Some("Preview HI-res: nepoznata editorial instanca".into());
                return;
            }
        }
        let open = match HiResPreviewPlayerComponent::build_open(&response) {
            Ok(open) => open,
            Err(error) => {
                self.set_preview_hires_error(instance_id, project_id, error);
                return;
            }
        };
        if ctx.is_some() {
            self.playback.stop();
        }
        self.open_hires_preview(instance_id, project_id, open, ctx);
    }

    fn open_hires_preview(
        &mut self,
        instance_id: &str,
        project_id: &str,
        open: HiResPreviewOpen,
        ctx: Option<egui::Context>,
    ) {
        let intent = HiResPreviewPlayerComponent::build_play_intent(&open);
        if let Some(ctx) = ctx.as_ref() {
            HiResPreviewPlayerComponent::open(&mut self.hires_preview_player, ctx, &open);
        }
        self.playback_transport_intent(intent);
        self.status = format!("Preview HI-res play · {}", open.preview_id);
        let _ = (instance_id, project_id);
    }

    fn set_preview_hires_error(
        &mut self,
        instance_id: &str,
        project_id: &str,
        error: impl Into<String>,
    ) {
        let error = error.into();
        match instance_id {
            EDITORIAL_INSTANCE_STORY => {
                self.story.set_preview_hires_error(project_id, error);
                self.status = self.story.status_text().to_owned();
            }
            EDITORIAL_INSTANCE_MEDIA_ASSIST => {
                self.media_assist.set_preview_hires_error(project_id, error);
                self.status = self.media_assist.status_text().to_owned();
            }
            _ => self.error = Some(error),
        }
    }

    fn request_shortcuts_if_needed(&mut self, instance_id: &str, ctx: Option<egui::Context>) {
        let needs_load = match instance_id {
            SHORTCUT_INSTANCE_PROJECT => self.project_ui.needs_shortcuts_load(),
            EDITORIAL_INSTANCE_STORY => self.story.needs_shortcuts_load(),
            EDITORIAL_INSTANCE_MEDIA_ASSIST => self.media_assist.needs_shortcuts_load(),
            SHORTCUT_INSTANCE_INGEST => self.ingest.needs_shortcuts_load(),
            _ => false,
        };
        if needs_load {
            let scope = match instance_id {
                SHORTCUT_INSTANCE_PROJECT => PROJECT_SHORTCUT_SCOPE,
                _ => STORYBOARD_SHORTCUT_SCOPE,
            };
            self.load_shortcuts(instance_id, scope, ctx);
        }
    }

    fn load_shortcuts(&mut self, instance_id: &str, scope: &str, ctx: Option<egui::Context>) {
        let commands = ShortcutBindingsComponent::load_all(instance_id, scope);
        match instance_id {
            SHORTCUT_INSTANCE_PROJECT => self.project_ui.begin_shortcuts_load(commands.len()),
            EDITORIAL_INSTANCE_STORY => self.story.begin_shortcuts_load(commands.len()),
            EDITORIAL_INSTANCE_MEDIA_ASSIST => {
                self.media_assist.begin_shortcuts_load(commands.len())
            }
            SHORTCUT_INSTANCE_INGEST => self.ingest.begin_shortcuts_load(commands.len()),
            _ => return,
        }
        for command in commands {
            if let Err(e) = self.submit_component_backend_command(command, ctx.clone()) {
                self.set_shortcuts_error(instance_id, scope, "catalog", e);
            }
        }
    }

    fn set_shortcuts_error(
        &mut self,
        instance_id: &str,
        scope: &str,
        port_id: &str,
        error: impl Into<String>,
    ) {
        match instance_id {
            SHORTCUT_INSTANCE_PROJECT => self.project_ui.set_shortcuts_error(scope, port_id, error),
            EDITORIAL_INSTANCE_STORY => self.story.set_shortcuts_error(scope, port_id, error),
            EDITORIAL_INSTANCE_MEDIA_ASSIST => {
                self.media_assist.set_shortcuts_error(scope, port_id, error)
            }
            SHORTCUT_INSTANCE_INGEST => self.ingest.set_shortcuts_error(scope, port_id, error),
            _ => self.error = Some(error.into()),
        }
    }

    fn request_project_meta_if_needed(&mut self, ctx: Option<egui::Context>) {
        if self.project_ui.needs_meta_load() {
            self.load_project_meta(ctx);
        }
    }

    fn load_project_meta(&mut self, ctx: Option<egui::Context>) {
        let commands = ProjectCatalogComponent::load_all();
        self.project_ui.begin_meta_load(commands.len());
        for command in commands {
            if let Err(e) = self.submit_component_backend_command(command, ctx.clone()) {
                self.project_ui.set_meta_error(e);
            }
        }
    }

    fn load_project_root_browser(&mut self, path: &str, ctx: Option<egui::Context>) {
        self.project_ui.projects_root_browser_busy = true;
        self.project_ui.projects_root_browser_error = None;
        let command = FilesystemListComponent::load(FS_INSTANCE_PROJECTS_ROOT, path);
        if let Err(e) = self.submit_component_backend_command(command, ctx) {
            self.project_ui.set_projects_root_browser_error(e);
        }
    }

    fn load_export_dir_browser(&mut self, path: &str, ctx: Option<egui::Context>) {
        self.project_ui.export_dir_browser_busy = true;
        self.project_ui.export_dir_browser_error = None;
        let command = FilesystemListComponent::load(FS_INSTANCE_EXPORT_DIR, path);
        if let Err(e) = self.submit_component_backend_command(command, ctx) {
            self.project_ui.set_export_dir_browser_error(e);
        }
    }

    fn load_ingest_dir_browser(&mut self, path: &str, ctx: Option<egui::Context>) {
        self.ingest.begin_dir_listing(path);
        let command = FilesystemListComponent::load(FS_INSTANCE_IMPORT_SOURCE, path);
        if let Err(e) = self.submit_component_backend_command(command, ctx) {
            self.ingest.set_dir_listing_error(e);
        }
    }

    fn load_ingest_state(&mut self, project_id: &str, ctx: Option<egui::Context>) {
        if project_id.trim().is_empty() {
            return;
        }
        self.ingest.begin_state_load(project_id);
        let command = SourceImportStateComponent::load(project_id);
        if let Err(e) = self.submit_component_backend_command(command, ctx) {
            self.ingest.set_state_request_error(e);
        }
    }

    fn poll_ingest_state(&mut self, project_id: &str, ctx: Option<egui::Context>) {
        if project_id.trim().is_empty() {
            return;
        }
        self.ingest.begin_state_poll(project_id);
        let command = SourceImportStateComponent::poll(project_id);
        if let Err(e) = self.submit_component_backend_command(command, ctx) {
            self.ingest.set_state_request_error(e);
        }
    }

    fn poll_source_import_status(&mut self, project_id: &str, ctx: Option<egui::Context>) {
        if project_id.trim().is_empty() {
            return;
        }
        self.source_import_status.begin_poll(project_id);
        let command = SourceImportStatusComponent::poll(project_id);
        if let Err(e) = self.submit_component_backend_command(command, ctx) {
            self.source_import_status.set_error();
            self.error = Some(e);
        }
    }

    fn refresh_editorial_status_indicators(
        &mut self,
        project_id: &str,
        ctx: Option<egui::Context>,
    ) {
        let story_ready = self.story.meta_ready() && self.story.has_editorial_project(project_id);
        let media_assist_ready =
            self.media_assist.meta_ready() && self.media_assist.has_editorial_project(project_id);
        if !story_ready && !media_assist_ready {
            return;
        }
        let command = EditorialStateComponent::refresh_story_status(
            EDITORIAL_INSTANCE_IMPORT_STATUS,
            project_id,
        );
        if let Err(e) = self.submit_component_backend_command(command, ctx) {
            self.error = Some(e);
        }
    }

    fn apply_source_import_status_snapshot(
        &mut self,
        state: &crate::api::IngestState,
        ctx: Option<egui::Context>,
    ) {
        let update = self.source_import_status.apply_state(state);
        if update.changed {
            self.refresh_editorial_status_indicators(&update.project_id, ctx);
        }
        if update.completed() {
            self.status = format!("Uvoz gotov: {} klip(ova).", update.imported_count);
        }
    }

    fn apply_source_import_status_state(
        &mut self,
        state: crate::api::IngestState,
        ctx: Option<egui::Context>,
    ) {
        self.ingest.apply_status_state(state.clone());
        self.apply_source_import_status_snapshot(&state, ctx);
    }

    fn save_import_selection(
        &mut self,
        project_id: &str,
        selected_clip_ids: &[String],
        selection_revision: u64,
        ctx: Option<egui::Context>,
    ) -> Result<(), String> {
        if project_id.trim().is_empty() {
            return Ok(());
        }
        let command =
            SourceImportSelectionComponent::set(project_id, selected_clip_ids, selection_revision);
        self.submit_component_backend_command(command, ctx)
            .map(|_| ())
    }

    fn browse_import_source(&mut self, project_id: &str, path: &str, ctx: Option<egui::Context>) {
        self.submit_source_import_command(
            project_id,
            SourceImportCommandComponent::browse(project_id, path),
            ctx,
        );
    }

    fn submit_source_import_command(
        &mut self,
        project_id: &str,
        command: ComponentBackendCommand,
        ctx: Option<egui::Context>,
    ) {
        if project_id.trim().is_empty() {
            return;
        }
        self.ingest.begin_import_command(project_id);
        if let Err(e) = self.submit_component_backend_command(command, ctx) {
            self.ingest.set_import_command_error(e);
        }
    }

    fn browser_start_path(path: &str) -> String {
        let clean = crate::qnc_location_browser::clean_location_path(path);
        if std::path::Path::new(&clean).is_absolute() {
            clean
        } else {
            String::new()
        }
    }

    fn dispatch_project(&mut self, action: ProjectAction, ctx: &egui::Context) {
        match action {
            ProjectAction::None => {}
            ProjectAction::Reload => {
                self.load_project_list(Some(ctx.clone()));
                self.load_project_meta(Some(ctx.clone()));
            }
            ProjectAction::OpenSelected => self.open_selected(Some(ctx.clone())),
            ProjectAction::Create => {
                let name = self.project_ui.new_name.trim().to_string();
                let tpl = self.project_ui.selected_template_id.clone();
                if name.is_empty() {
                    self.error = Some("Unesi ime projekta.".into());
                    return;
                }
                let save_draft = ProjectCommandComponent::save_ui_state(
                    self.next_project_request_id(),
                    "project.create.draft",
                    serde_json::json!({
                        "selected_template_id": tpl.clone(),
                        "project_name": name.clone(),
                    }),
                );
                self.submit_project_command(save_draft, Some(ctx.clone()));
                let create = ProjectCommandComponent::create_from_template(
                    self.next_project_request_id(),
                    &name,
                    &tpl,
                );
                self.submit_project_command(create, Some(ctx.clone()));
                self.status = format!("Creating project {name}…");
            }
            ProjectAction::DeleteSelected => {
                let Some(idx) = self.selected_index else {
                    return;
                };
                let Some(row) = self.projects.get(idx).cloned() else {
                    return;
                };
                let command = ProjectCommandComponent::delete_projects(
                    self.next_project_request_id(),
                    &[row.project_id.clone()],
                    &row.project_id,
                );
                self.submit_project_command(command, Some(ctx.clone()));
                self.status = format!("Deleting {}…", row.name);
            }
            ProjectAction::ToggleProjectsRootBrowser => {
                let opening = !self.project_ui.projects_root_browser_open;
                self.project_ui.projects_root_browser_open = opening;
                if opening
                    && self.project_ui.projects_root_browser_kind
                        == crate::qnc_location_browser::LocationSourceKind::Local
                {
                    let start = Self::browser_start_path(&self.project_ui.projects_root_draft);
                    self.load_project_root_browser(&start, Some(ctx.clone()));
                }
            }
            ProjectAction::SelectProjectsRootKind(kind) => {
                self.project_ui.projects_root_browser_kind = kind;
                if kind == crate::qnc_location_browser::LocationSourceKind::Local
                    && self.project_ui.projects_root_browser_entries.is_empty()
                    && self.project_ui.projects_root_browser_path.trim().is_empty()
                {
                    self.load_project_root_browser("", Some(ctx.clone()));
                }
            }
            ProjectAction::OpenProjectsRootPath(path) => {
                if self.project_ui.projects_root_browser_kind
                    == crate::qnc_location_browser::LocationSourceKind::Local
                {
                    self.load_project_root_browser(&path, Some(ctx.clone()));
                }
            }
            ProjectAction::ConfirmProjectsRootBrowser => {
                let path = self
                    .project_ui
                    .projects_root_browser_path
                    .trim()
                    .to_string();
                if !path.is_empty() {
                    let command = ProjectCommandComponent::save_settings_path(
                        self.next_project_request_id(),
                        &format!("projects_root:{path}"),
                        "storage.projects_root",
                        Value::String(path.clone()),
                    );
                    self.submit_project_command(command, Some(ctx.clone()));
                }
            }
            ProjectAction::CancelProjectsRootBrowser => {
                self.project_ui.projects_root_browser_open = false;
            }
            ProjectAction::ToggleExportDirBrowser => {
                let opening = !self.project_ui.export_dir_browser_open;
                self.project_ui.export_dir_browser_open = opening;
                if opening
                    && self.project_ui.export_dir_browser_kind
                        == crate::qnc_location_browser::LocationSourceKind::Local
                {
                    let start = Self::browser_start_path(&self.project_ui.export_dir_draft);
                    self.load_export_dir_browser(&start, Some(ctx.clone()));
                }
            }
            ProjectAction::SelectExportDirKind(kind) => {
                self.project_ui.export_dir_browser_kind = kind;
                if kind == crate::qnc_location_browser::LocationSourceKind::Local
                    && self.project_ui.export_dir_browser_entries.is_empty()
                    && self.project_ui.export_dir_browser_path.trim().is_empty()
                {
                    self.load_export_dir_browser("", Some(ctx.clone()));
                }
            }
            ProjectAction::OpenExportDirPath(path) => {
                if self.project_ui.export_dir_browser_kind
                    == crate::qnc_location_browser::LocationSourceKind::Local
                {
                    self.load_export_dir_browser(&path, Some(ctx.clone()));
                }
            }
            ProjectAction::ConfirmExportDirBrowser => {
                let path = self.project_ui.export_dir_browser_path.trim().to_string();
                if !path.is_empty() {
                    let command = ProjectCommandComponent::save_settings_path(
                        self.next_project_request_id(),
                        &format!("export_dir:{path}"),
                        "export.directory",
                        Value::String(path.clone()),
                    );
                    self.submit_project_command(command, Some(ctx.clone()));
                }
            }
            ProjectAction::CancelExportDirBrowser => {
                self.project_ui.export_dir_browser_open = false;
            }
            ProjectAction::SelectTemplate(id) => {
                self.project_ui.selected_template_id = id.clone();
                let command = ProjectCommandComponent::save_ui_state(
                    self.next_project_request_id(),
                    "template.select",
                    serde_json::json!({
                        "selected_template_id": id,
                        "reset_settings_override": true,
                    }),
                );
                self.submit_project_command(command, Some(ctx.clone()));
            }
            ProjectAction::ToggleWorkflowTab(tab_id, on) => {
                let mut tabs: Vec<String> = self
                    .project_ui
                    .ui_state
                    .as_ref()
                    .and_then(|u| u.get("effective_settings"))
                    .and_then(|e| e.get("workspace"))
                    .and_then(|w| w.get("tabs"))
                    .and_then(|t| t.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_else(|| {
                        vec![
                            "project".into(),
                            "ingest".into(),
                            "media_assist".into(),
                            "storyboard".into(),
                        ]
                    });
                if !tabs.iter().any(|t| t == "project") {
                    tabs.insert(0, "project".into());
                }
                if on {
                    if !tabs.iter().any(|t| t == &tab_id) {
                        // Keep Media Assist before Story when enabling.
                        if tab_id == "media_assist" {
                            if let Some(pos) = tabs.iter().position(|t| t == "storyboard") {
                                tabs.insert(pos, tab_id);
                            } else {
                                tabs.push(tab_id);
                            }
                        } else {
                            tabs.push(tab_id);
                        }
                    }
                } else {
                    tabs.retain(|t| t != &tab_id);
                }
                let arr = Value::Array(tabs.into_iter().map(Value::String).collect());
                let command = ProjectCommandComponent::save_settings_path(
                    self.next_project_request_id(),
                    "workspace.tabs",
                    "workspace.tabs",
                    arr,
                );
                self.submit_project_command(command, Some(ctx.clone()));
            }
            ProjectAction::SetSettingsPath(path, value) => {
                let command = ProjectCommandComponent::save_settings_path(
                    self.next_project_request_id(),
                    "settings.path",
                    &path,
                    value,
                );
                self.submit_project_command(command, Some(ctx.clone()));
            }
            ProjectAction::MergeSettingsOverride(patch) => {
                let command = ProjectCommandComponent::merge_settings_override(
                    self.next_project_request_id(),
                    "settings.merge",
                    patch,
                );
                self.submit_project_command(command, Some(ctx.clone()));
            }
            ProjectAction::ApplyExportPreset(preset_id) => {
                let eff = self
                    .project_ui
                    .ui_state
                    .as_ref()
                    .and_then(|u| u.get("effective_settings"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let command = ProjectExportProfileComponent::apply_preset(
                    self.next_project_request_id(),
                    &eff,
                    &preset_id,
                );
                self.submit_project_command(command, Some(ctx.clone()));
            }
            ProjectAction::SaveExportPreset => {
                let name = self.project_ui.export_preset_draft_name.trim().to_string();
                let eff = self
                    .project_ui
                    .ui_state
                    .as_ref()
                    .and_then(|u| u.get("effective_settings"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let command = match ProjectExportProfileComponent::save_custom_preset(
                    self.next_project_request_id(),
                    &eff,
                    &name,
                ) {
                    Ok(command) => command,
                    Err(error) => {
                        self.error = Some(error);
                        return;
                    }
                };
                self.submit_project_command(command, Some(ctx.clone()));
            }
            ProjectAction::DeleteTemplate(template_id) => {
                let command = ProjectCommandComponent::delete_user_template(
                    self.next_project_request_id(),
                    &template_id,
                );
                self.submit_project_command(command, Some(ctx.clone()));
            }
            ProjectAction::SaveCustomTemplate => {
                let name = self.project_ui.template_draft_name.trim().to_string();
                let description = self
                    .project_ui
                    .template_draft_description
                    .trim()
                    .to_string();
                let base = self.project_ui.selected_template_id.clone();
                if name.is_empty() {
                    self.error = Some("Upiši naziv custom templatea.".into());
                    return;
                }
                if base.is_empty() {
                    self.error = Some("Odaberi bazni template.".into());
                    return;
                }
                // Host bakes effective_settings when settings is null.
                let save_draft = ProjectCommandComponent::save_ui_state(
                    self.next_project_request_id(),
                    "template.create.draft",
                    serde_json::json!({
                        "template_create_open": true,
                        "template_draft_name": name.clone(),
                        "template_draft_description": description.clone(),
                        "selected_template_id": base.clone(),
                    }),
                );
                self.submit_project_command(save_draft, Some(ctx.clone()));
                let create = ProjectCommandComponent::create_user_template(
                    self.next_project_request_id(),
                    &name,
                    &description,
                    &base,
                    None,
                    &[],
                );
                self.submit_project_command(create, Some(ctx.clone()));
            }
            ProjectAction::SetTemplateCreateOpen(open) => {
                self.project_ui.template_create_open = open;
                if open {
                    // Advanced is part of Novi template — open with the create form.
                    self.project_ui.advanced_open = true;
                    if self.project_ui.template_draft_name.is_empty() {
                        if let Some(t) = self
                            .project_ui
                            .templates
                            .iter()
                            .find(|t| t.template_id == self.project_ui.selected_template_id)
                        {
                            self.project_ui.template_draft_name = format!("{} (custom)", t.name);
                        }
                    }
                } else {
                    self.project_ui.advanced_open = false;
                    self.project_ui.template_draft_name.clear();
                    self.project_ui.template_draft_description.clear();
                }
                let command = ProjectCommandComponent::save_ui_state(
                    self.next_project_request_id(),
                    if open {
                        "template.create.open"
                    } else {
                        "template.create.close"
                    },
                    serde_json::json!({
                        "template_create_open": open,
                        "template_draft_name": self.project_ui.template_draft_name.clone(),
                        "template_draft_description": self.project_ui.template_draft_description.clone(),
                    }),
                );
                self.submit_project_command(command, Some(ctx.clone()));
            }
        }
    }

    fn apply_shell_state_data(&mut self, data: ShellStateData, ctx: Option<egui::Context>) {
        match data {
            ShellStateData::Health(h) if h.status == "ok" || !h.status.is_empty() => {
                self.health_ok = true;
                self.status = format!("Host OK ({})", h.status);
                self.submit_shell_state_command(ShellStateComponent::runtime(), ctx.clone());
                self.load_project_list(ctx.clone());
                self.load_project_meta(ctx.clone());
                self.load_appearance(ctx);
                if self.phase == Phase::HostGate {
                    self.phase = Phase::ProjectOnly;
                    self.screen = Screen::Project;
                }
            }
            ShellStateData::Health(h) => {
                self.health_ok = false;
                self.phase = Phase::HostGate;
                self.error = Some(format!("Unexpected health status: {}", h.status));
                self.status = "HostDisconnected".into();
            }
            ShellStateData::Runtime(rt) => {
                let port = rt
                    .get("api_port")
                    .and_then(|v| v.as_u64())
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "?".into());
                let plugins = rt
                    .get("plugins_loaded_count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                self.playback.player_mut().configure_runtime_profile(&rt);
                self.runtime_summary = format!("port={port}  plugins={plugins}");
            }
            ShellStateData::Appearance(theme_id) => {
                self.theme_picker.set_active(theme_id);
            }
            ShellStateData::Workspace {
                project_id,
                workspace,
            } => {
                let is_current = self
                    .open_project
                    .as_ref()
                    .map(|p| p.project_id == project_id)
                    .unwrap_or(false);
                if !is_current {
                    return;
                }
                self.workspace = Some(workspace);
                self.phase = Phase::Workspace;
                if self.pending_workspace_go.take() == Some(WorkspaceGo::Entry) {
                    let name = self
                        .open_project
                        .as_ref()
                        .map(|p| p.name.clone())
                        .unwrap_or_else(|| project_id.clone());
                    if let Some(entry) = self.go_workflow(WorkflowGo::Entry) {
                        self.status = format!("Opened {name} → entry `{entry}`");
                    }
                }
            }
        }
    }

    fn set_shell_state_error(&mut self, port_id: &str, error: impl Into<String>) {
        let error = error.into();
        match port_id {
            "health" => {
                self.health_ok = false;
                self.phase = Phase::HostGate;
                self.error = Some(error);
                self.status = "HostDisconnected".into();
                self.projects.clear();
            }
            "runtime" => {
                self.runtime_summary.clear();
            }
            "appearance" => {
                // Keep current theme; host may be older without this route.
            }
            "workspace" => {
                self.pending_workspace_go = None;
                self.error = Some(error);
                self.phase = Phase::ProjectOnly;
                self.screen = Screen::Project;
            }
            _ => self.error = Some(error),
        }
    }

    fn apply_project_command_data(
        &mut self,
        kind: ProjectCommandKind,
        detail: &str,
        data: ProjectCommandData,
        ctx: Option<egui::Context>,
    ) {
        match data {
            ProjectCommandData::OpenProject {
                project,
                active_project_id,
            } => {
                self.apply_open_project(project, active_project_id, ctx);
                self.error = None;
            }
            ProjectCommandData::CreatedProject {
                project,
                active_project_id,
            } => {
                self.project_ui.new_name.clear();
                self.load_project_list(ctx.clone());
                self.apply_open_project(project, active_project_id, ctx);
                self.error = None;
            }
            ProjectCommandData::ProjectsDeleted(list) => {
                let deleted_id = detail;
                let deleted_name = self
                    .projects
                    .iter()
                    .find(|p| p.project_id == deleted_id)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| deleted_id.to_string());
                if self
                    .open_project
                    .as_ref()
                    .map(|p| p.project_id == deleted_id)
                    .unwrap_or(false)
                {
                    self.close_project();
                }
                self.projects = list.projects;
                self.active_project_id = list.active_project_id;
                self.selected_index = if self.projects.is_empty() {
                    None
                } else {
                    Some(0)
                };
                self.status = format!("Deleted {deleted_name}");
                self.project_ui.message = Some(format!("Obrisan {deleted_name}"));
                self.error = None;
            }
            ProjectCommandData::UiState(ui_state) => {
                self.apply_project_ui_state(detail, ui_state, ctx);
                self.error = None;
            }
            ProjectCommandData::TemplateDeleted {
                templates,
                ui_state,
            } => {
                self.project_ui.templates = templates;
                if !ui_state.is_null() {
                    self.project_ui.apply_ui_state(ui_state);
                } else {
                    self.load_project_meta(ctx);
                }
                if self.project_ui.selected_template_id == detail {
                    if let Some(first) = self.project_ui.templates.first() {
                        self.project_ui.selected_template_id = first.template_id.clone();
                    } else {
                        self.project_ui.selected_template_id.clear();
                    }
                }
                self.project_ui.message = Some("Template obrisan.".into());
                self.status = "Template deleted".into();
                self.error = None;
            }
            ProjectCommandData::TemplateCreated(tpl) => {
                let template_name = tpl.name.clone();
                let template_id = tpl.template_id.clone();
                self.project_ui.template_create_open = false;
                self.project_ui.advanced_open = false;
                self.project_ui.template_draft_name.clear();
                self.project_ui.template_draft_description.clear();
                self.project_ui.selected_template_id = template_id.clone();
                let command = ProjectCommandComponent::save_ui_state(
                    self.next_project_request_id(),
                    "template.create.after",
                    serde_json::json!({
                        "selected_template_id": template_id,
                        "template_create_open": false,
                        "template_draft_name": "",
                        "template_draft_description": "",
                        "reset_settings_override": true,
                    }),
                );
                self.submit_project_command(command, ctx.clone());
                self.load_project_meta(ctx);
                self.project_ui.message = Some(format!("Novi template spremljen: {template_name}"));
                self.status = format!("Template: {template_name}");
                self.error = None;
            }
        }
        let _ = kind;
    }

    fn apply_project_ui_state(
        &mut self,
        detail: &str,
        ui_state: Value,
        ctx: Option<egui::Context>,
    ) {
        match detail {
            "project.create.draft" | "template.create.draft" => {}
            "workspace.tabs" => {
                self.project_ui.apply_ui_state(ui_state);
                self.project_ui.message = None;
                self.reload_workspace(ctx);
            }
            "export.preset.save" => {
                self.project_ui.export_preset_draft_name.clear();
                self.project_ui.apply_ui_state(ui_state);
                self.project_ui.message = Some("Export preset spremljen u template draft.".into());
            }
            "template.create.open" => {
                self.project_ui.apply_ui_state(ui_state);
                self.project_ui.template_create_open = true;
                self.project_ui.advanced_open = true;
                self.project_ui.message = Some("Novi template — upiši naziv i Spremi.".into());
            }
            "template.create.close" => {
                self.project_ui.apply_ui_state(ui_state);
                self.project_ui.template_create_open = false;
                self.project_ui.advanced_open = false;
                self.project_ui.message = None;
            }
            "template.create.after" => {
                self.project_ui.apply_ui_state(ui_state);
            }
            _ if detail.starts_with("projects_root:") => {
                let path = detail.trim_start_matches("projects_root:");
                self.project_ui.apply_ui_state(ui_state);
                self.project_ui.projects_root_browser_open = false;
                self.project_ui.message = Some(format!("Projects root → {path}"));
            }
            _ if detail.starts_with("export_dir:") => {
                let path = detail.trim_start_matches("export_dir:");
                self.project_ui.apply_ui_state(ui_state);
                self.project_ui.export_dir_browser_open = false;
                self.project_ui.message = Some(format!("Export dir → {path}"));
            }
            _ => {
                self.project_ui.apply_ui_state(ui_state);
                self.project_ui.message = None;
            }
        }
    }

    fn set_project_command_error(
        &mut self,
        kind: ProjectCommandKind,
        _detail: &str,
        error: impl Into<String>,
    ) {
        match kind {
            ProjectCommandKind::SaveUiState
            | ProjectCommandKind::OpenProject
            | ProjectCommandKind::CreateFromTemplate
            | ProjectCommandKind::DeleteProjects
            | ProjectCommandKind::DeleteUserTemplate
            | ProjectCommandKind::CreateUserTemplate => self.error = Some(error.into()),
        }
    }

    fn poll_component_backend(&mut self, ctx: Option<egui::Context>) {
        for event in self.component_backend.poll() {
            let error_key = ComponentErrorKey::from_event(&event);
            if ShellStateComponent::accepts_event(&event) {
                let port_id = event.port_id.clone();
                let Some(result) = ShellStateComponent::into_data(event) else {
                    continue;
                };
                match result {
                    Ok(data) => {
                        self.clear_component_error(&error_key);
                        self.apply_shell_state_data(data, ctx.clone());
                    }
                    Err(e) => {
                        let e = self.record_component_error(error_key, e);
                        self.set_shell_state_error(&port_id, e);
                    }
                }
            } else if ShellStateComponent::accepts_background_event(&event) {
                let Some(result) = ShellStateComponent::into_background_result(event) else {
                    continue;
                };
                match result {
                    Ok(()) => self.clear_component_error(&error_key),
                    Err(e) => {
                        let _ = self.record_component_error(error_key, e);
                        self.background_playback_active_sent = None;
                        self.background_playback_last_submit = None;
                    }
                }
            } else if ProjectCommandComponent::accepts_event(&event) {
                let Some((kind, detail, result)) = ProjectCommandComponent::into_data(event) else {
                    continue;
                };
                match result {
                    Ok(data) => {
                        self.clear_component_error(&error_key);
                        self.apply_project_command_data(kind, &detail, data, ctx.clone());
                    }
                    Err(e) => {
                        let e = self.record_component_error(error_key, e);
                        self.set_project_command_error(kind, &detail, e);
                    }
                }
            } else if self.theme_picker.accepts_event(&event) {
                if let Err(e) = self.theme_picker.handle_event(event) {
                    let e = self.record_component_error(error_key, e);
                    self.error = Some(e);
                } else {
                    self.clear_component_error(&error_key);
                    self.error = None;
                }
            } else if ProjectCatalogComponent::accepts_event(&event) {
                let Some(result) = ProjectCatalogComponent::into_data(event) else {
                    continue;
                };
                match result {
                    Ok(data) => {
                        self.clear_component_error(&error_key);
                        self.project_ui.apply_catalog_data(data);
                    }
                    Err(e) => {
                        let e = self.record_component_error(error_key, e);
                        self.project_ui.set_meta_error(e);
                    }
                }
            } else if ProjectRegistryComponent::accepts_event(&event) {
                let Some(result) = ProjectRegistryComponent::into_projects(event) else {
                    continue;
                };
                match result {
                    Ok(list) => {
                        self.clear_component_error(&error_key);
                        self.apply_project_list(list);
                    }
                    Err(e) => {
                        let e = self.record_component_error(error_key, e);
                        self.error = Some(e);
                    }
                }
            } else if EditorialEditComponent::accepts_event(&event) {
                let Some((instance_id, project_id, kind, _detail, result)) =
                    EditorialEditComponent::into_data(event)
                else {
                    continue;
                };
                match result {
                    Ok(data) => {
                        self.clear_component_error(&error_key);
                        self.apply_editorial_edit_data(data, ctx.clone());
                    }
                    Err(e) => {
                        let e = self.record_component_error(error_key, e);
                        self.set_editorial_edit_error(&instance_id, &project_id, kind, e);
                    }
                }
            } else if HiResRenderTransportComponent::accepts_event(&event) {
                let Some((instance_id, project_id, result)) =
                    HiResRenderTransportComponent::into_submit(event)
                else {
                    continue;
                };
                match result {
                    Ok(response) => {
                        self.clear_component_error(&error_key);
                        self.apply_export_hires_submit(&instance_id, &project_id, response);
                    }
                    Err(e) => {
                        let e = self.record_component_error(error_key, e);
                        self.set_export_hires_error(&instance_id, &project_id, e);
                    }
                }
            } else if HiResRenderTransportComponent::accepts_status_event(&event) {
                let Some((instance_id, project_id, result)) =
                    HiResRenderTransportComponent::into_status(event)
                else {
                    continue;
                };
                match result {
                    Ok(status) => {
                        self.clear_component_error(&error_key);
                        self.apply_export_hires_status(&instance_id, &project_id, status);
                    }
                    Err(e) => {
                        let e = self.record_component_error(error_key, e);
                        self.set_export_hires_status_error(&instance_id, &project_id, e);
                    }
                }
            } else if HiResRenderTransportComponent::accepts_preview_event(&event) {
                let Some((instance_id, project_id, result)) =
                    HiResRenderTransportComponent::into_preview_submit(event)
                else {
                    continue;
                };
                match result {
                    Ok(response) => {
                        self.clear_component_error(&error_key);
                        self.apply_preview_hires_submit(
                            &instance_id,
                            &project_id,
                            response,
                            ctx.clone(),
                        );
                    }
                    Err(e) => {
                        let e = self.record_component_error(error_key, e);
                        self.set_preview_hires_error(&instance_id, &project_id, e);
                    }
                }
            } else if EditorialStateComponent::accepts_event(&event) {
                let Some((instance_id, project_id, result)) =
                    EditorialStateComponent::into_data(event)
                else {
                    continue;
                };
                match result {
                    Ok(data) => {
                        self.clear_component_error(&error_key);
                        self.apply_editorial_state_data(data, ctx.clone());
                    }
                    Err(e) => {
                        let e = self.record_component_error(error_key, e);
                        self.set_editorial_state_error(&instance_id, &project_id, e);
                    }
                }
            } else if PlaybackMediaResolverComponent::accepts_event(&event) {
                let Some((instance_id, project_id, clip_id, result)) =
                    PlaybackMediaResolverComponent::into_media_resolution(event)
                else {
                    continue;
                };
                match result {
                    Ok(data) => {
                        self.clear_component_error(&error_key);
                        let intent = self.apply_playback_media_resolution(
                            &instance_id,
                            &project_id,
                            &clip_id,
                            data,
                        );
                        if intent != PlaybackTransportIntent::None {
                            self.playback_transport_intent(intent);
                        }
                    }
                    Err(e) => {
                        let e = self.record_component_error(error_key, e);
                        self.set_playback_media_error(&instance_id, &project_id, &clip_id, e);
                    }
                }
            } else if ShortcutBindingsComponent::accepts_event(&event) {
                let Some((instance_id, scope, port_id, result)) =
                    ShortcutBindingsComponent::into_data(event)
                else {
                    continue;
                };
                match result {
                    Ok(data) => {
                        self.clear_component_error(&error_key);
                        self.apply_shortcut_bindings_data(data);
                    }
                    Err(e) => {
                        let e = self.record_component_error(error_key, e);
                        self.set_shortcuts_error(&instance_id, &scope, port_id, e);
                    }
                }
            } else if FilesystemListComponent::event_instance(&event).is_some() {
                let Some((instance_id, result)) = FilesystemListComponent::into_listing(event)
                else {
                    continue;
                };
                match (instance_id.as_str(), result) {
                    (FS_INSTANCE_PROJECTS_ROOT, Ok(list)) => {
                        self.clear_component_error(&error_key);
                        self.project_ui.apply_projects_root_listing(
                            list.roots,
                            list.path,
                            list.parent,
                            list.entries,
                        );
                    }
                    (FS_INSTANCE_PROJECTS_ROOT, Err(e)) => {
                        let e = self.record_component_error(error_key, e);
                        self.project_ui.set_projects_root_browser_error(e);
                    }
                    (FS_INSTANCE_EXPORT_DIR, Ok(list)) => {
                        self.clear_component_error(&error_key);
                        self.project_ui.apply_export_dir_listing(
                            list.roots,
                            list.path,
                            list.parent,
                            list.entries,
                        );
                    }
                    (FS_INSTANCE_EXPORT_DIR, Err(e)) => {
                        let e = self.record_component_error(error_key, e);
                        self.project_ui.set_export_dir_browser_error(e);
                    }
                    (FS_INSTANCE_IMPORT_SOURCE, Ok(list)) => {
                        self.clear_component_error(&error_key);
                        self.ingest.apply_dir_listing(list);
                    }
                    (FS_INSTANCE_IMPORT_SOURCE, Err(e)) => {
                        let e = self.record_component_error(error_key, e);
                        self.ingest.set_dir_listing_error(e);
                    }
                    _ => {}
                }
            } else if SourceImportSelectionComponent::accepts_event(&event) {
                let Some(result) = SourceImportSelectionComponent::into_state(event) else {
                    continue;
                };
                match result {
                    Ok(state) => {
                        self.clear_component_error(&error_key);
                        self.ingest.apply_status_state(state);
                    }
                    Err(e) => {
                        let e = self.record_component_error(error_key, e);
                        self.error = Some(e);
                    }
                }
            } else if SourceImportCommandComponent::accepts_event(&event) {
                let Some((kind, result)) = SourceImportCommandComponent::into_state(event) else {
                    continue;
                };
                match result {
                    Ok(state) => {
                        self.clear_component_error(&error_key);
                        self.apply_source_import_command_result(kind, state, ctx.clone());
                    }
                    Err(e) => {
                        let e = self.record_component_error(error_key, e);
                        self.ingest.set_import_command_error(e);
                    }
                }
            } else if SourceImportStatusComponent::accepts_event(&event) {
                let Some(result) = SourceImportStatusComponent::into_state(event) else {
                    continue;
                };
                match result {
                    Ok(state) => {
                        self.clear_component_error(&error_key);
                        self.apply_source_import_status_state(state, ctx.clone());
                    }
                    Err(e) => {
                        let e = self.record_component_error(error_key, e);
                        self.source_import_status.set_error();
                        self.error = Some(e);
                    }
                }
            } else if SourceImportStateComponent::accepts_event(&event) {
                let Some((kind, result)) = SourceImportStateComponent::into_state(event) else {
                    continue;
                };
                match (kind, result) {
                    (SourceImportStateKind::Load, Ok(state)) => {
                        self.clear_component_error(&error_key);
                        self.apply_source_import_status_snapshot(&state, ctx.clone());
                        self.ingest.apply_loaded_state(state)
                    }
                    (SourceImportStateKind::Poll, Ok(state)) => {
                        self.clear_component_error(&error_key);
                        self.apply_source_import_status_snapshot(&state, ctx.clone());
                        self.ingest.apply_polled_state(state)
                    }
                    (_, Err(e)) => {
                        let e = self.record_component_error(error_key, e);
                        self.ingest.set_state_request_error(e);
                    }
                }
            }
        }
    }

    fn apply_editorial_state_data(&mut self, data: EditorialStateData, ctx: Option<egui::Context>) {
        match data {
            EditorialStateData::StoryState {
                instance_id,
                project_id,
                state,
                refresh_only,
            } => {
                if refresh_only {
                    self.apply_editorial_story_status_refresh(&project_id, state);
                    return;
                }
                let peer_state = state.clone();
                match instance_id.as_str() {
                    EDITORIAL_INSTANCE_STORY => {
                        self.story.apply_editorial_story_state(&project_id, state);
                        self.mirror_editorial_story_state_to_peer(
                            &instance_id,
                            &project_id,
                            peer_state,
                        );
                        self.load_editorial_timeline_model(&instance_id, &project_id, ctx);
                    }
                    EDITORIAL_INSTANCE_MEDIA_ASSIST => {
                        self.media_assist
                            .apply_editorial_story_state(&project_id, state);
                        self.mirror_editorial_story_state_to_peer(
                            &instance_id,
                            &project_id,
                            peer_state,
                        );
                        self.load_editorial_timeline_model(&instance_id, &project_id, ctx);
                    }
                    _ => {}
                }
            }
            EditorialStateData::TimelineModel {
                instance_id,
                project_id,
                timeline,
            } => {
                let peer_timeline = timeline.clone();
                match instance_id.as_str() {
                    EDITORIAL_INSTANCE_STORY => {
                        let intent = self
                            .story
                            .apply_editorial_timeline_model(&project_id, timeline);
                        self.playback_transport_intent(intent);
                        self.mirror_editorial_timeline_model_to_peer(
                            &instance_id,
                            &project_id,
                            peer_timeline,
                        );
                    }
                    EDITORIAL_INSTANCE_MEDIA_ASSIST => {
                        let intent = self
                            .media_assist
                            .apply_editorial_timeline_model(&project_id, timeline);
                        self.playback_transport_intent(intent);
                        self.mirror_editorial_timeline_model_to_peer(
                            &instance_id,
                            &project_id,
                            peer_timeline,
                        );
                    }
                    _ => {}
                }
            }
            EditorialStateData::Playlist {
                instance_id,
                project_id,
                playlist,
            } => {
                let peer_playlist = playlist.clone();
                match instance_id.as_str() {
                    EDITORIAL_INSTANCE_STORY => {
                        let intent = self.story.apply_editorial_playlist(&project_id, playlist);
                        self.playback_transport_intent(intent);
                        self.mirror_editorial_playlist_to_peer(
                            &instance_id,
                            &project_id,
                            peer_playlist,
                        );
                    }
                    EDITORIAL_INSTANCE_MEDIA_ASSIST => {
                        let intent = self
                            .media_assist
                            .apply_editorial_playlist(&project_id, playlist);
                        self.playback_transport_intent(intent);
                        self.mirror_editorial_playlist_to_peer(
                            &instance_id,
                            &project_id,
                            peer_playlist,
                        );
                    }
                    _ => {}
                }
            }
        }
    }

    fn apply_editorial_story_status_refresh(&mut self, project_id: &str, state: Value) {
        if self.story.meta_ready() && self.story.has_editorial_project(project_id) {
            self.story
                .apply_editorial_story_state(project_id, state.clone());
        }
        if self.media_assist.meta_ready() && self.media_assist.has_editorial_project(project_id) {
            self.media_assist
                .apply_editorial_story_state(project_id, state);
        }
    }

    fn apply_editorial_edit_data(&mut self, data: EditorialEditData, ctx: Option<egui::Context>) {
        let instance_id = data.instance_id.clone();
        let project_id = data.project_id.clone();
        let peer_data = data.clone();
        if matches!(
            instance_id.as_str(),
            EDITORIAL_INSTANCE_STORY | EDITORIAL_INSTANCE_MEDIA_ASSIST
        ) {
            self.playback.invalidate_playlist_input();
        }
        let intent = match instance_id.as_str() {
            EDITORIAL_INSTANCE_STORY => self.story.apply_editorial_edit_data(data),
            EDITORIAL_INSTANCE_MEDIA_ASSIST => self.media_assist.apply_editorial_edit_data(data),
            _ => PlaybackTransportIntent::None,
        };
        self.playback_transport_intent(intent);
        self.mirror_editorial_edit_data_to_peer(&instance_id, &project_id, peer_data);
        self.load_editorial_timeline_model(&instance_id, &project_id, ctx);
    }

    fn apply_playback_media_resolution(
        &mut self,
        instance_id: &str,
        project_id: &str,
        clip_id: &str,
        data: PlaybackMediaResolution,
    ) -> PlaybackTransportIntent {
        match instance_id {
            EDITORIAL_INSTANCE_STORY => self
                .story
                .apply_playback_media_resolution(project_id, clip_id, data),
            EDITORIAL_INSTANCE_MEDIA_ASSIST => self
                .media_assist
                .apply_playback_media_resolution(project_id, clip_id, data),
            SHORTCUT_INSTANCE_INGEST => {
                let intent = self
                    .ingest
                    .apply_playback_media_resolution(project_id, clip_id, data);
                if self.screen == Screen::Ingest {
                    intent
                } else {
                    PlaybackTransportIntent::None
                }
            }
            _ => PlaybackTransportIntent::None,
        }
    }

    fn set_playback_media_error(
        &mut self,
        instance_id: &str,
        project_id: &str,
        clip_id: &str,
        error: impl Into<String>,
    ) {
        match instance_id {
            EDITORIAL_INSTANCE_STORY => self
                .story
                .set_playback_media_resolution_error(project_id, clip_id, error),
            EDITORIAL_INSTANCE_MEDIA_ASSIST => self
                .media_assist
                .set_playback_media_resolution_error(project_id, clip_id, error),
            SHORTCUT_INSTANCE_INGEST => self
                .ingest
                .set_playback_media_resolution_error(project_id, clip_id, error),
            _ => self.error = Some(error.into()),
        }
    }

    fn apply_shortcut_bindings_data(&mut self, data: ShortcutBindingsData) {
        match data {
            ShortcutBindingsData::Catalog {
                instance_id,
                scope,
                catalog,
            } => match instance_id.as_str() {
                SHORTCUT_INSTANCE_PROJECT => {
                    self.project_ui.apply_shortcut_catalog(&scope, catalog)
                }
                EDITORIAL_INSTANCE_STORY => self.story.apply_shortcut_catalog(&scope, catalog),
                EDITORIAL_INSTANCE_MEDIA_ASSIST => {
                    self.media_assist.apply_shortcut_catalog(&scope, catalog)
                }
                SHORTCUT_INSTANCE_INGEST => self.ingest.apply_shortcut_catalog(&scope, catalog),
                _ => {}
            },
            ShortcutBindingsData::User {
                instance_id,
                scope,
                user,
            } => match instance_id.as_str() {
                SHORTCUT_INSTANCE_PROJECT => self.project_ui.apply_shortcut_user(&scope, user),
                EDITORIAL_INSTANCE_STORY => self.story.apply_shortcut_user(&scope, user),
                EDITORIAL_INSTANCE_MEDIA_ASSIST => {
                    self.media_assist.apply_shortcut_user(&scope, user)
                }
                SHORTCUT_INSTANCE_INGEST => self.ingest.apply_shortcut_user(&scope, user),
                _ => {}
            },
        }
    }

    fn apply_source_import_command_result(
        &mut self,
        kind: SourceImportCommandKind,
        state: crate::api::IngestState,
        ctx: Option<egui::Context>,
    ) {
        if kind == SourceImportCommandKind::SetArchive {
            self.apply_source_import_status_snapshot(&state, ctx);
            self.ingest
                .apply_archive_option_state(state, "Import opcije spremljene.");
            return;
        }
        let status_state = state.clone();
        let message = match kind {
            SourceImportCommandKind::Browse => {
                if state.browse_path.trim().is_empty() {
                    "Otkrij gotov.".into()
                } else {
                    format!("Otkrij: {}", state.browse_path)
                }
            }
            SourceImportCommandKind::Discover => "Ponovo otkrij gotov.".into(),
            SourceImportCommandKind::SetArchive => unreachable!("handled above"),
            SourceImportCommandKind::ImportSelected => {
                let queued = state
                    .queued
                    .unwrap_or_else(|| state.selected_clip_ids.len() as u64);
                format!("Uvoz u bazi: {queued} klip(ova).")
            }
            SourceImportCommandKind::ApproveProxyPosters => {
                let count = state
                    .clips
                    .iter()
                    .filter(|clip| matches!(clip.thumb_status.as_str(), "pending" | "processing"))
                    .count();
                format!("Posteri iz proxya odobreni: {count} klip(ova).")
            }
        };
        self.ingest.apply_import_command_state(state, message);
        self.apply_source_import_status_snapshot(&status_state, ctx);
        if kind == SourceImportCommandKind::ApproveProxyPosters {
            self.source_import_status
                .mark_possible_work(&status_state.project_id);
        }
        if kind == SourceImportCommandKind::ImportSelected {
            self.source_import_status
                .mark_possible_work(&status_state.project_id);
            if let Some(next) = self.go_workflow(WorkflowGo::Next { from: "ingest" }) {
                self.status = format!("Uvoz pokrenut → {next}");
            }
        }
    }

    /// Shell footer — theme picker far left (global, not project settings).
    fn ui_shell_theme_picker(&mut self, ui: &mut egui::Ui) {
        if let Some(command) = self.theme_picker.ui(ui) {
            self.status = format!("Tema: {}", self.theme_picker.active().label());
            if let Err(e) = self.submit_component_backend_command(command, Some(ui.ctx().clone())) {
                self.error = Some(e);
            }
        }
    }

    fn tick_playback(&mut self, ctx: &egui::Context) {
        self.playback.player_mut().pump(ctx);
        let events = self.playback_rx.try_recv_all();
        self.playback.ingest_events(&events);
        if self.phase == Phase::Workspace {
            match self.screen {
                Screen::Ingest => player_bridge::apply_player_events(
                    &mut self.ingest,
                    &events,
                    self.playback.player(),
                ),
                Screen::Story => player_bridge::apply_player_events(
                    &mut self.story,
                    &events,
                    self.playback.player(),
                ),
                Screen::MediaAssist => player_bridge::apply_player_events(
                    &mut self.media_assist,
                    &events,
                    self.playback.player(),
                ),
                _ => {}
            }
            self.flush_playback_transport(ctx);
        }
        self.sync_background_playback_gate(Some(ctx.clone()));
    }

    /// Process transport commands emitted during event application in the same tick.
    fn flush_playback_transport(&mut self, ctx: &egui::Context) {
        self.playback.player_mut().pump(ctx);
        let follow_up = self.playback_rx.try_recv_all();
        if follow_up.is_empty() {
            return;
        }
        self.playback.ingest_events(&follow_up);
        if self.phase != Phase::Workspace {
            return;
        }
        match self.screen {
            Screen::Ingest => player_bridge::apply_player_events(
                &mut self.ingest,
                &follow_up,
                self.playback.player(),
            ),
            Screen::Story => player_bridge::apply_player_events(
                &mut self.story,
                &follow_up,
                self.playback.player(),
            ),
            Screen::MediaAssist => player_bridge::apply_player_events(
                &mut self.media_assist,
                &follow_up,
                self.playback.player(),
            ),
            _ => {}
        }
    }

    /// Player pump (TX->decode->RX->carrier), then transport mirror.
    fn tick_active_player(&mut self, ctx: &egui::Context) {
        self.tick_playback(ctx);
    }

    /// Clock only before paint; commands are routed explicitly by app-owned transport intents.
    fn pump_active_player(&mut self, ctx: &egui::Context) {
        self.tick_playback(ctx);
    }
}

impl eframe::App for QncApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.maximize_once {
            self.maximize_once = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
        }

        if self.auto_connect_once {
            self.auto_connect_once = false;
            self.connect_and_load(Some(ctx.clone()));
        }

        self.poll_component_backend(Some(ctx.clone()));
        self.apply_theme(ctx);

        if self.phase == Phase::Workspace {
            if let Some(p) = self.open_project.clone() {
                self.source_import_status.watch_project(&p.project_id);
                if self.screen == Screen::Ingest {
                    if self.ingest.should_request_poll() {
                        self.poll_ingest_state(&p.project_id, Some(ctx.clone()));
                    }
                    if self.ingest.needs_poll() {
                        ctx.request_repaint_after(std::time::Duration::from_millis(500));
                    }
                } else {
                    if self.source_import_status.should_request_poll() {
                        self.poll_source_import_status(&p.project_id, Some(ctx.clone()));
                    }
                    if self.source_import_status.needs_repaint() {
                        ctx.request_repaint_after(std::time::Duration::from_millis(500));
                    }
                }
            }
        }

        let story_active = self.phase == Phase::Workspace && matches!(self.screen, Screen::Story);
        let media_assist_active =
            self.phase == Phase::Workspace && matches!(self.screen, Screen::MediaAssist);
        let ingest_active = self.phase == Phase::Workspace && matches!(self.screen, Screen::Ingest);
        let project_active = self.phase == Phase::ProjectOnly;
        if story_active {
            if let Some(p) = self.open_project.clone() {
                self.request_editorial_state_if_needed(
                    EDITORIAL_INSTANCE_STORY,
                    &p.project_id,
                    Some(ctx.clone()),
                );
                self.request_shortcuts_if_needed(EDITORIAL_INSTANCE_STORY, Some(ctx.clone()));
            }
        } else if media_assist_active {
            if let Some(p) = self.open_project.clone() {
                self.request_editorial_state_if_needed(
                    EDITORIAL_INSTANCE_MEDIA_ASSIST,
                    &p.project_id,
                    Some(ctx.clone()),
                );
                self.request_shortcuts_if_needed(
                    EDITORIAL_INSTANCE_MEDIA_ASSIST,
                    Some(ctx.clone()),
                );
            }
        } else if ingest_active {
            self.request_shortcuts_if_needed(SHORTCUT_INSTANCE_INGEST, Some(ctx.clone()));
        } else if project_active {
            self.request_shortcuts_if_needed(SHORTCUT_INSTANCE_PROJECT, Some(ctx.clone()));
        }
        let comp = self.composition();

        // Pre-UI: decode clock + RX. Transport commands are routed explicitly below.
        self.pump_active_player(ctx);

        if story_active {
            if self.story.meta_ready() {
                let intent = self.story.playlist_input_preload_intent();
                self.playback_transport_intent(intent);
            }
            if self.story.meta_ready() && self.story.shortcuts_ready() {
                let intents = self.story.handle_shortcuts(ctx, &self.host, &self.playback);
                self.playback_transport_intents(intents);
            }
            self.submit_editorial_backend_commands(EDITORIAL_INSTANCE_STORY, Some(ctx.clone()));
            // Apply catalog CueFrame before dock paint so playhead moves this frame.
            self.tick_playback(ctx);
            self.story.prepare_frame(&self.host, ctx);
            self.story.tick(&self.host, ctx);
        } else if media_assist_active {
            if self.media_assist.meta_ready() && self.media_assist.shortcuts_ready() {
                let intents = self
                    .media_assist
                    .handle_shortcuts(ctx, &self.host, &self.playback);
                self.playback_transport_intents(intents);
            }
            self.submit_editorial_backend_commands(
                EDITORIAL_INSTANCE_MEDIA_ASSIST,
                Some(ctx.clone()),
            );
            self.tick_playback(ctx);
            self.media_assist.prepare_frame(&self.host, ctx);
            self.media_assist.tick(&self.host, ctx);
        } else if ingest_active {
            let actions = self.ingest.handle_shortcuts(ctx);
            let pid = self
                .open_project
                .as_ref()
                .map(|project| project.project_id.clone())
                .unwrap_or_default();
            for action in actions {
                self.dispatch_ingest(&pid, action, ctx);
            }
            self.tick_playback(ctx);
        } else if project_active && self.project_ui.shortcuts_ready() {
            let action = self.project_ui.handle_shortcuts(ctx);
            if matches!(action, ProjectAction::OpenSelected) && self.selected_index.is_some() {
                self.dispatch_project(action, ctx);
            }
        }

        // Host chrome only on HostGate. Project / editorial / ingest = empty shell (web).
        let hide_top_chrome = !matches!(self.phase, Phase::HostGate);
        if !hide_top_chrome {
            egui::TopBottomPanel::top("top").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("HostDisconnected").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(&self.status);
                    });
                });
                ui.horizontal(|ui| {
                    ui.label("Host URL");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.host_url_edit)
                            .desired_width(280.0)
                            .hint_text("http://127.0.0.1:8001"),
                    );
                    if ui.button("Connect / Refresh").clicked() {
                        self.connect_and_load(Some(ctx.clone()));
                    }
                    if self.health_ok {
                        ui.colored_label(Color32::from_rgb(80, 180, 100), "connected");
                    } else {
                        ui.colored_label(Color32::from_rgb(220, 90, 90), "disconnected");
                    }
                    if !self.runtime_summary.is_empty() {
                        ui.label(RichText::new(&self.runtime_summary).weak());
                    }
                });
                if let Some(err) = self.global_error_message() {
                    ui.colored_label(Color32::from_rgb(220, 90, 90), err);
                }
            });
        }

        let panel_bg = qnc_theme::current_ctx(ctx).bg;

        // Footer first = outer bottom edge (web workflow tabs)
        if self.phase == Phase::Workspace {
            egui::TopBottomPanel::bottom("footer")
                .exact_height(36.0)
                .show(ctx, |ui| {
                    let tabs = {
                        let mut tabs = self
                            .workspace
                            .as_ref()
                            .map(|w| w.tabs.clone())
                            .unwrap_or_default();
                        if !tabs.iter().any(|t| t == "project") {
                            tabs.insert(0, "project".into());
                        }
                        tabs
                    };
                    let project_name = self
                        .open_project
                        .as_ref()
                        .map(|p| format!("Projekt {}", p.name));
                    let mut close = false;
                    let mut go_tab: Option<String> = None;

                    ui.columns(3, |cols| {
                        // Left — global theme (shell) + project label (link → Project)
                        cols[0].with_layout(
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.set_min_height(ui.available_height());
                                ui.spacing_mut().item_spacing.x = 8.0;
                                self.ui_shell_theme_picker(ui);
                                if let Some(name) = &project_name {
                                    ui.separator();
                                    if qnc_theme::text_link(ui, name, true).clicked() {
                                        go_tab = Some("project".into());
                                    }
                                }
                            },
                        );
                        // Center — tabs always in the middle of the footer
                        cols[1].with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                            ui.set_min_height(ui.available_height());
                            ui.horizontal_centered(|ui| {
                                ui.spacing_mut().item_spacing.x = 12.0;
                                for tab in &tabs {
                                    let label = self
                                        .workspace
                                        .as_ref()
                                        .map(|w| api::tab_label(w, tab))
                                        .unwrap_or_else(|| tab.clone());
                                    let selected = match &self.screen {
                                        Screen::Project => tab == "project",
                                        Screen::Ingest => tab == "ingest",
                                        Screen::MediaAssist => api::is_media_assist_tab(tab),
                                        Screen::Story => api::is_story_tab(tab),
                                        Screen::Unsupported(id) => id == tab,
                                    };
                                    if qnc_theme::link_tab(ui, &label, selected).clicked() {
                                        go_tab = Some(tab.clone());
                                    }
                                }
                            });
                        });
                        // Right — close
                        cols[2].with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                ui.set_min_height(ui.available_height());
                                if qnc_theme::action_btn(ui, "Close project").clicked() {
                                    close = true;
                                }
                                if let Some(err) = self.global_error_message() {
                                    ui.colored_label(
                                        Color32::from_rgb(220, 90, 90),
                                        Self::short_error_label(&err),
                                    );
                                }
                            },
                        );
                    });

                    if let Some(tab) = go_tab {
                        self.go_workflow(WorkflowGo::Tab(&tab));
                    }
                    if close {
                        self.close_project();
                    }
                });
        } else if self.phase == Phase::ProjectOnly {
            // Same footer height as workspace — Project tab only until a project is open.
            egui::TopBottomPanel::bottom("footer_project_only")
                .exact_height(36.0)
                .show(ctx, |ui| {
                    let h = ui.available_height();
                    ui.columns(3, |cols| {
                        cols[0].with_layout(
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.set_min_height(h);
                                ui.spacing_mut().item_spacing.x = 8.0;
                                self.ui_shell_theme_picker(ui);
                            },
                        );
                        cols[1].with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                            ui.set_min_height(h);
                            ui.horizontal_centered(|ui| {
                                let _ = qnc_theme::link_tab(ui, "Project", true);
                            });
                        });
                        cols[2].with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                ui.set_min_height(h);
                                if let Some(err) = self.global_error_message() {
                                    ui.colored_label(
                                        Color32::from_rgb(220, 90, 90),
                                        Self::short_error_label(&err),
                                    );
                                }
                            },
                        );
                    });
                });
        }

        if self.phase == Phase::Workspace {
            egui::TopBottomPanel::bottom("workspace_status")
                .exact_height(22.0)
                .frame(egui::Frame::NONE.fill(panel_bg).inner_margin(egui::Margin {
                    left: 8,
                    right: 8,
                    top: 2,
                    bottom: 2,
                }))
                .show(ctx, |ui| {
                    let t = qnc_theme::current_ctx(ctx);
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.set_min_height(ui.available_height());
                        let status = self.status.trim();
                        let status = if status.is_empty() { "Ready" } else { status };
                        ui.label(
                            RichText::new(status)
                                .size(qnc_theme::FONT_UI)
                                .color(t.muted),
                        );
                    });
                });
        }

        // Source editor dock — composition.dock decides which screens get it.
        if comp.dock.show && story_active {
            let h = self.story.source_dock_height();
            egui::TopBottomPanel::bottom("story_source_dock")
                .exact_height(h)
                .frame(egui::Frame::NONE.fill(panel_bg).inner_margin(0.0))
                .show(ctx, |ui| {
                    let intent = self.story.ui_source_dock(ui, &self.host, &self.playback);
                    self.playback_transport_intent(intent);
                    self.submit_editorial_backend_commands(
                        EDITORIAL_INSTANCE_STORY,
                        Some(ctx.clone()),
                    );
                });
        } else if comp.dock.show && media_assist_active {
            let h = self.media_assist.source_dock_height();
            egui::TopBottomPanel::bottom("media_assist_source_dock")
                .exact_height(h)
                .frame(egui::Frame::NONE.fill(panel_bg).inner_margin(0.0))
                .show(ctx, |ui| {
                    let intent = self
                        .media_assist
                        .ui_source_dock(ui, &self.host, &self.playback);
                    self.playback_transport_intent(intent);
                    self.submit_editorial_backend_commands(
                        EDITORIAL_INSTANCE_MEDIA_ASSIST,
                        Some(ctx.clone()),
                    );
                });
        } else if comp.dock.show
            && self.phase == Phase::Workspace
            && matches!(self.screen, Screen::Ingest)
        {
            let h = self.ingest.timeline_dock_height();
            egui::TopBottomPanel::bottom("ingest_source_dock")
                .exact_height(h)
                .frame(egui::Frame::NONE.fill(panel_bg).inner_margin(0.0))
                .show(ctx, |ui| {
                    let pid = self
                        .open_project
                        .as_ref()
                        .map(|p| p.project_id.clone())
                        .unwrap_or_default();
                    let action = self.ingest.ui_timeline_dock(ui, &self.playback);
                    if !pid.is_empty() {
                        self.dispatch_ingest(&pid, action, ctx);
                    }
                });
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(panel_bg))
            .show(ctx, |ui| {
            match self.phase {
                Phase::HostGate => {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.heading("HostDisconnected");
                        ui.label(
                            "Pokreni qnc-host (run_host.ps1), zatim Connect. Native prozor — ne browser.",
                        );
                        ui.add_space(12.0);
                        if ui.button("Retry connect").clicked() {
                            self.connect_and_load(Some(ctx.clone()));
                        }
                    });
                }
                Phase::ProjectOnly | Phase::Workspace => {
                    if self.phase == Phase::Workspace && !matches!(self.screen, Screen::Project) {
                        match self.screen.clone() {
                            Screen::Ingest => {
                                let (name, pid) = self
                                    .open_project
                                    .as_ref()
                                    .map(|p| (p.name.clone(), p.project_id.clone()))
                                    .unwrap_or_else(|| ("?".into(), String::new()));
                                if pid.is_empty() {
                                    ui.label("No open project.");
                                    return;
                                }
                                let action =
                                    self.ingest.ui(ui, &name, &pid, &self.host, ctx, &self.playback);
                                self.dispatch_ingest(&pid, action, ctx);
                            }
                            Screen::MediaAssist => {
                                let intents =
                                    self.media_assist.ui_main(ui, &self.host, ctx, &self.playback);
                                self.playback_transport_intents(intents);
                                self.submit_editorial_backend_commands(
                                    EDITORIAL_INSTANCE_MEDIA_ASSIST,
                                    Some(ctx.clone()),
                                );
                                self.status = self.media_assist.status_text().to_owned();
                            }
                            Screen::Story => {
                                let intents = self.story.ui_main(ui, &self.host, ctx, &self.playback);
                                self.playback_transport_intents(intents);
                                self.submit_editorial_backend_commands(
                                    EDITORIAL_INSTANCE_STORY,
                                    Some(ctx.clone()),
                                );
                                self.status = self.story.status_text().to_owned();
                            }
                            Screen::Unsupported(tab) => {
                                ui.vertical_centered(|ui| {
                                    ui.add_space(48.0);
                                    ui.heading(format!("Screen `{tab}`"));
                                    ui.label("Not implemented yet in native app.");
                                });
                            }
                            Screen::Project => {}
                        }
                    } else {
                        self.request_project_meta_if_needed(Some(ctx.clone()));
                        let action = self.project_ui.ui(
                            ui,
                            &self.projects,
                            &mut self.selected_index,
                            &self.active_project_id,
                        );
                        self.dispatch_project(action, ctx);
                    }
                }
            }
        });

        // Post-UI: process app-routed transport commands emitted during paint.
        self.tick_active_player(ctx);
        if self.hires_preview_player.active() {
            match HiResPreviewPlayerComponent::show(
                ctx,
                &self.playback,
                &mut self.hires_preview_player,
            ) {
                HiResPreviewPlayerAction::None => {}
                HiResPreviewPlayerAction::Close => {
                    self.playback.stop();
                    HiResPreviewPlayerComponent::close(&mut self.hires_preview_player, ctx);
                }
            }
        }
    }
}
