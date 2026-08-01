//! Native shell — Project + Ingest + Media Assist + Story.

use eframe::egui::{self, Color32, RichText};
use serde_json::Value;

use crate::api::{self, HostClient, ProjectRow, Workspace};
use crate::composition::{ScreenComposition, WorkflowScreen};
use crate::ingest::IngestScreen;
use crate::media_assist::MediaAssistScreen;
use crate::player_bridge;
use crate::project::{ProjectAction, ProjectScreen};
use crate::qnc_broadcast_player::{BroadcastPlayerRx, QncBroadcastPlayer};
use crate::qnc_theme::{self, ThemeId};
use crate::story::StoryScreen;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    HostGate,
    ProjectOnly,
    Workspace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Screen {
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
    /// First workflow tab after open (`workspace.tabs` entry).
    Entry,
    /// Next tab after `from` in `workspace.tabs`.
    Next { from: &'a str },
}

pub struct QncApp {
    host: HostClient,
    host_url_edit: String,
    phase: Phase,
    screen: Screen,
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
    ingest: IngestScreen,
    media_assist: MediaAssistScreen,
    story: StoryScreen,
    /// Standalone broadcast player (own TX/RX channel — not form-owned).
    player: QncBroadcastPlayer,
    story_player_rx: BroadcastPlayerRx,
    ingest_player_rx: BroadcastPlayerRx,
    media_assist_player_rx: BroadcastPlayerRx,
    /// User-selected UI theme (host SQLite `ui_appearance_user`).
    theme_id: ThemeId,
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
        let player = QncBroadcastPlayer::new();
        let story_player_rx = player.subscribe();
        let ingest_player_rx = player.subscribe();
        let media_assist_player_rx = player.subscribe();
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
            player,
            story_player_rx,
            ingest_player_rx,
            media_assist_player_rx,
            theme_id: ThemeId::Dark,
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

    fn connect_and_load(&mut self) {
        self.error = None;
        self.host.set_base_url(&self.host_url_edit);
        match self.host.health() {
            Ok(h) if h.status == "ok" || !h.status.is_empty() => {
                self.health_ok = true;
                self.status = format!("Host OK ({})", h.status);
                if let Ok(rt) = self.host.runtime() {
                    let port = rt
                        .get("api_port")
                        .and_then(|v| v.as_u64())
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| "?".into());
                    let plugins = rt
                        .get("plugins_loaded_count")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    self.player.configure_runtime_profile(&rt);
                    self.runtime_summary = format!("port={port}  plugins={plugins}");
                } else {
                    self.runtime_summary.clear();
                }
                self.reload_projects();
                self.project_ui.reload_meta(&self.host);
                self.load_appearance();
                if self.phase == Phase::HostGate {
                    self.phase = Phase::ProjectOnly;
                    self.screen = Screen::Project;
                }
            }
            Ok(h) => {
                self.health_ok = false;
                self.phase = Phase::HostGate;
                self.error = Some(format!("Unexpected health status: {}", h.status));
                self.status = "HostDisconnected".into();
            }
            Err(e) => {
                self.health_ok = false;
                self.phase = Phase::HostGate;
                self.error = Some(e);
                self.status = "HostDisconnected".into();
                self.projects.clear();
            }
        }
    }

    fn load_appearance(&mut self) {
        match self.host.appearance_user() {
            Ok(v) => {
                let id = v
                    .get("user")
                    .and_then(|u| u.get("theme_id"))
                    .or_else(|| v.get("theme_id"))
                    .and_then(|x| x.as_str())
                    .and_then(ThemeId::parse)
                    .unwrap_or_default();
                self.theme_id = id;
            }
            Err(_e) => {
                // Keep current theme; host may be older without this route.
            }
        }
    }

    fn apply_theme(&self, ctx: &egui::Context) {
        let tokens = self.theme_id.tokens();
        qnc_theme::set_active(ctx, self.theme_id);
        qnc_theme::apply_egui_visuals(ctx, &tokens);
    }

    fn reload_projects(&mut self) {
        match self.host.list_projects() {
            Ok(list) => {
                self.projects = list.projects;
                self.active_project_id = list.active_project_id;
                if self.selected_index.is_none() && !self.projects.is_empty() {
                    let prefer = self
                        .projects
                        .iter()
                        .position(|p| p.project_id == self.active_project_id);
                    self.selected_index = Some(prefer.unwrap_or(0));
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
            Err(e) => {
                self.error = Some(e);
            }
        }
    }

    fn open_project_id(&mut self, project_id: &str) {
        self.error = None;
        match self.host.open_project(project_id) {
            Ok((project, active)) => {
                self.active_project_id = active;
                self.open_project = Some(project.clone());
                self.ingest = IngestScreen::default();
                self.story.reset_session(&self.host);
                self.story = StoryScreen::story();
                self.media_assist.reset_session(&self.host);
                self.media_assist = StoryScreen::media_assist();
                self.player.stop();
                match self.host.workspace(&project.project_id) {
                    Ok(ws) => {
                        self.workspace = Some(ws);
                        self.phase = Phase::Workspace;
                        if let Some(entry) = self.go_workflow(WorkflowGo::Entry) {
                            self.status = format!("Opened {} → entry `{entry}`", project.name);
                        }
                    }
                    Err(e) => {
                        self.error = Some(e);
                        self.phase = Phase::ProjectOnly;
                        self.screen = Screen::Project;
                    }
                }
            }
            Err(e) => {
                self.error = Some(e);
            }
        }
    }

    fn open_selected(&mut self) {
        let Some(idx) = self.selected_index else {
            self.error = Some("Select a project first.".into());
            return;
        };
        let Some(row) = self.projects.get(idx).cloned() else {
            self.error = Some("Invalid selection.".into());
            return;
        };
        self.open_project_id(&row.project_id);
    }

    fn close_project(&mut self) {
        self.story.reset_session(&self.host);
        self.media_assist.reset_session(&self.host);
        self.player.stop();
        self.open_project = None;
        self.workspace = None;
        self.ingest = IngestScreen::default();
        self.story = StoryScreen::story();
        self.media_assist = StoryScreen::media_assist();
        self.phase = if self.health_ok {
            Phase::ProjectOnly
        } else {
            Phase::HostGate
        };
        self.screen = Screen::Project;
        self.status = "Project closed (local UI; host active id unchanged)".into();
        self.project_ui.ensure_loaded(&self.host);
    }

    /// Single workflow navigation — Project open, footer, Uvezi all use this.
    fn go_workflow(&mut self, go: WorkflowGo<'_>) -> Option<String> {
        match go {
            WorkflowGo::Tab(_) => {}
            WorkflowGo::Entry | WorkflowGo::Next { .. } => self.reload_workspace(),
        }
        let tabs = self
            .workspace
            .as_ref()
            .map(|w| w.tabs.clone())
            .unwrap_or_default();
        let tab = match go {
            WorkflowGo::Tab(t) => t.to_string(),
            WorkflowGo::Entry => api::workflow_entry_tab(&tabs),
            WorkflowGo::Next { from } => {
                let Some(next) = api::workflow_next_tab(&tabs, from) else {
                    self.status = "Nema sljedećeg workflow taba.".into();
                    return None;
                };
                next
            }
        };
        self.activate_screen(&tab);
        Some(tab)
    }

    fn reload_workspace(&mut self) {
        let Some(pid) = self.open_project.as_ref().map(|p| p.project_id.clone()) else {
            return;
        };
        if let Ok(ws) = self.host.workspace(&pid) {
            self.workspace = Some(ws);
        }
    }

    fn activate_screen(&mut self, tab: &str) {
        if self.open_project.is_none() && tab != "project" {
            self.error = Some("Otvori projekt prije workflow taba.".into());
            self.screen = Screen::Project;
            return;
        }
        let next = Self::screen_from_tab(tab);
        if self.screen == Screen::Story && next != Screen::Story {
            self.story.reset_session(&self.host);
            self.story = StoryScreen::story();
            self.player.stop();
        }
        if self.screen == Screen::MediaAssist && next != Screen::MediaAssist {
            self.media_assist.reset_session(&self.host);
            self.media_assist = StoryScreen::media_assist();
            self.player.stop();
        }
        if self.screen == Screen::Ingest && next != Screen::Ingest {
            self.ingest.reset_player_session();
            self.player.stop();
        }
        // Inactive form RXs fill while another tab plays — drain so Story/MA
        // do not apply a backlog of foreign frames after switch.
        let _ = self.story_player_rx.try_recv_all();
        let _ = self.ingest_player_rx.try_recv_all();
        let _ = self.media_assist_player_rx.try_recv_all();
        self.screen = next;
        self.on_screen_entered();
    }

    fn dispatch_ingest(&mut self, pid: &str, action: crate::ingest::IngestAction) {
        let advance = matches!(action, crate::ingest::IngestAction::ImportSelected);
        if let Err(e) = self.ingest.dispatch(&self.host, pid, action) {
            self.error = Some(e);
        } else if advance {
            if let Some(next) = self.go_workflow(WorkflowGo::Next { from: "ingest" }) {
                self.status = format!("Uvoz pokrenut → {next}");
            }
        }
    }

    fn on_screen_entered(&mut self) {
        if matches!(self.screen, Screen::Project) {
            self.project_ui.ensure_loaded(&self.host);
        }
        let Some(p) = self.open_project.clone() else {
            return;
        };
        match self.screen {
            Screen::Ingest => self.ingest.ensure_loaded(&self.host, &p.project_id),
            Screen::MediaAssist => self.media_assist.ensure_loaded(&self.host, &p.project_id),
            Screen::Story => self.story.ensure_loaded(&self.host, &p.project_id),
            _ => {}
        }
    }

    fn load_project_root_browser(&mut self, path: &str) {
        match self.host.fs_list(path) {
            Ok(list) => self.project_ui.apply_projects_root_listing(
                list.roots,
                list.path,
                list.parent,
                list.entries,
            ),
            Err(e) => self.project_ui.set_projects_root_browser_error(e),
        }
    }

    fn load_export_dir_browser(&mut self, path: &str) {
        match self.host.fs_list(path) {
            Ok(list) => self.project_ui.apply_export_dir_listing(
                list.roots,
                list.path,
                list.parent,
                list.entries,
            ),
            Err(e) => self.project_ui.set_export_dir_browser_error(e),
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

    fn dispatch_project(&mut self, action: ProjectAction) {
        match action {
            ProjectAction::None => {}
            ProjectAction::Reload => {
                self.reload_projects();
                self.project_ui.reload_meta(&self.host);
            }
            ProjectAction::OpenSelected => self.open_selected(),
            ProjectAction::Create => {
                let name = self.project_ui.new_name.trim().to_string();
                let tpl = self.project_ui.selected_template_id.clone();
                if name.is_empty() {
                    self.error = Some("Unesi ime projekta.".into());
                    return;
                }
                let _ = self.host.save_ui_state(serde_json::json!({
                    "selected_template_id": tpl,
                    "project_name": name,
                }));
                match self.host.create_from_template(&name, &tpl) {
                    Ok((project, _)) => {
                        self.project_ui.new_name.clear();
                        self.reload_projects();
                        if let Some(i) = self
                            .projects
                            .iter()
                            .position(|p| p.project_id == project.project_id)
                        {
                            self.selected_index = Some(i);
                        }
                        self.open_project_id(&project.project_id);
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            ProjectAction::DeleteSelected => {
                let Some(idx) = self.selected_index else {
                    return;
                };
                let Some(row) = self.projects.get(idx).cloned() else {
                    return;
                };
                match self.host.delete_projects(&[row.project_id.clone()]) {
                    Ok(list) => {
                        if self
                            .open_project
                            .as_ref()
                            .map(|p| p.project_id == row.project_id)
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
                        self.status = format!("Deleted {}", row.name);
                        self.project_ui.message = Some(format!("Obrisan {}", row.name));
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            ProjectAction::ToggleProjectsRootBrowser => {
                let opening = !self.project_ui.projects_root_browser_open;
                self.project_ui.projects_root_browser_open = opening;
                if opening
                    && self.project_ui.projects_root_browser_kind
                        == crate::qnc_location_browser::LocationSourceKind::Local
                {
                    let start = Self::browser_start_path(&self.project_ui.projects_root_draft);
                    self.load_project_root_browser(&start);
                    if self.project_ui.projects_root_browser_error.is_some() {
                        self.load_project_root_browser("");
                    }
                }
            }
            ProjectAction::SelectProjectsRootKind(kind) => {
                self.project_ui.projects_root_browser_kind = kind;
                if kind == crate::qnc_location_browser::LocationSourceKind::Local
                    && self.project_ui.projects_root_browser_entries.is_empty()
                    && self.project_ui.projects_root_browser_path.trim().is_empty()
                {
                    self.load_project_root_browser("");
                }
            }
            ProjectAction::OpenProjectsRootPath(path) => {
                if self.project_ui.projects_root_browser_kind
                    == crate::qnc_location_browser::LocationSourceKind::Local
                {
                    self.load_project_root_browser(&path);
                }
            }
            ProjectAction::ConfirmProjectsRootBrowser => {
                let path = self
                    .project_ui
                    .projects_root_browser_path
                    .trim()
                    .to_string();
                if !path.is_empty() {
                    match self
                        .host
                        .save_settings_path("storage.projects_root", Value::String(path.clone()))
                    {
                        Ok(ui) => {
                            self.project_ui.apply_ui_state(ui);
                            self.project_ui.projects_root_browser_open = false;
                            self.project_ui.message = Some(format!("Projects root → {path}"));
                        }
                        Err(e) => self.error = Some(e),
                    }
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
                    self.load_export_dir_browser(&start);
                    if self.project_ui.export_dir_browser_error.is_some() {
                        self.load_export_dir_browser("");
                    }
                }
            }
            ProjectAction::SelectExportDirKind(kind) => {
                self.project_ui.export_dir_browser_kind = kind;
                if kind == crate::qnc_location_browser::LocationSourceKind::Local
                    && self.project_ui.export_dir_browser_entries.is_empty()
                    && self.project_ui.export_dir_browser_path.trim().is_empty()
                {
                    self.load_export_dir_browser("");
                }
            }
            ProjectAction::OpenExportDirPath(path) => {
                if self.project_ui.export_dir_browser_kind
                    == crate::qnc_location_browser::LocationSourceKind::Local
                {
                    self.load_export_dir_browser(&path);
                }
            }
            ProjectAction::ConfirmExportDirBrowser => {
                let path = self.project_ui.export_dir_browser_path.trim().to_string();
                if !path.is_empty() {
                    match self
                        .host
                        .save_settings_path("export.directory", Value::String(path.clone()))
                    {
                        Ok(ui) => {
                            self.project_ui.apply_ui_state(ui);
                            self.project_ui.export_dir_browser_open = false;
                            self.project_ui.message = Some(format!("Export dir → {path}"));
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
            }
            ProjectAction::CancelExportDirBrowser => {
                self.project_ui.export_dir_browser_open = false;
            }
            ProjectAction::SelectTemplate(id) => {
                self.project_ui.selected_template_id = id.clone();
                match self.host.save_ui_state(serde_json::json!({
                    "selected_template_id": id,
                    "reset_settings_override": true,
                })) {
                    Ok(ui) => {
                        self.project_ui.apply_ui_state(ui);
                        self.project_ui.message = None;
                    }
                    Err(e) => self.error = Some(e),
                }
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
                match self.host.save_settings_path("workspace.tabs", arr) {
                    Ok(ui) => {
                        self.project_ui.apply_ui_state(ui);
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            ProjectAction::SetSettingsPath(path, value) => {
                match self.host.save_settings_path(&path, value) {
                    Ok(ui) => {
                        self.project_ui.apply_ui_state(ui);
                        self.project_ui.message = None;
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            ProjectAction::MergeSettingsOverride(patch) => {
                match self.host.merge_settings_override(patch) {
                    Ok(ui) => {
                        self.project_ui.apply_ui_state(ui);
                        self.project_ui.message = None;
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            ProjectAction::ApplyExportPreset(preset_id) => {
                let eff = self
                    .project_ui
                    .ui_state
                    .as_ref()
                    .and_then(|u| u.get("effective_settings"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let patch = crate::project_pts::export_preset_override_patch(&eff, &preset_id);
                match self.host.merge_settings_override(patch) {
                    Ok(ui) => {
                        self.project_ui.apply_ui_state(ui);
                        self.project_ui.message = None;
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            ProjectAction::SaveExportPreset => {
                let name = self.project_ui.export_preset_draft_name.trim().to_string();
                if name.is_empty() {
                    self.error = Some("Unesi naziv preseta.".into());
                    return;
                }
                let eff = self
                    .project_ui
                    .ui_state
                    .as_ref()
                    .and_then(|u| u.get("effective_settings"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let cur = eff.get("export").cloned().unwrap_or(Value::Null);
                let id = crate::project_pts::slug_preset_id(&name);
                let preset = serde_json::json!({
                    "id": id,
                    "name": name,
                    "values": {
                        "format": cur.get("format").and_then(|v| v.as_str()).unwrap_or("HD 1080i50"),
                        "fps": crate::project_pts::path_num(&eff, &["export", "fps"]).unwrap_or(25.0),
                        "width": crate::project_pts::path_i64(&eff, &["export", "width"], 1920),
                        "height": crate::project_pts::path_i64(&eff, &["export", "height"], 1080),
                        "field_order": cur.get("field_order").and_then(|v| v.as_str()).unwrap_or("upper_first"),
                        "color_space": cur.get("color_space").and_then(|v| v.as_str()).unwrap_or("rec709"),
                        "container": cur.get("container").and_then(|v| v.as_str()).unwrap_or("mxf_op1a"),
                        "video_codec": cur.get("video_codec").and_then(|v| v.as_str()).unwrap_or("mpeg2_422_50mbit"),
                        "audio_sample_rate": crate::project_pts::path_i64(&eff, &["export", "audio_sample_rate"], 48000),
                        "audio_channels": crate::project_pts::path_i64(&eff, &["export", "audio_channels"], 2),
                    }
                });
                let mut existing: Vec<Value> = cur
                    .get("custom_presets")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let preset_id = preset
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Some(slot) = existing
                    .iter_mut()
                    .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(preset_id.as_str()))
                {
                    *slot = preset.clone();
                } else {
                    existing.push(preset.clone());
                }
                let values = preset.get("values").cloned().unwrap_or(Value::Null);
                let mut export = serde_json::json!({
                    "custom_presets": existing,
                    "preset": preset_id,
                });
                if let (Some(obj), Some(vals)) = (export.as_object_mut(), values.as_object()) {
                    for (k, v) in vals {
                        obj.insert(k.clone(), v.clone());
                    }
                }
                match self
                    .host
                    .merge_settings_override(serde_json::json!({ "export": export }))
                {
                    Ok(ui) => {
                        self.project_ui.export_preset_draft_name.clear();
                        self.project_ui.apply_ui_state(ui);
                        self.project_ui.message =
                            Some("Export preset spremljen u template draft.".into());
                        self.error = None;
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            ProjectAction::DeleteTemplate(template_id) => {
                match self.host.delete_user_template(&template_id) {
                    Ok((templates, ui)) => {
                        self.project_ui.templates = templates;
                        if !ui.is_null() {
                            self.project_ui.apply_ui_state(ui);
                        } else {
                            self.project_ui.reload_meta(&self.host);
                        }
                        if self.project_ui.selected_template_id == template_id {
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
                    Err(e) => self.error = Some(e),
                }
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
                let _ = self.host.save_ui_state(serde_json::json!({
                    "template_create_open": true,
                    "template_draft_name": name,
                    "template_draft_description": description,
                    "selected_template_id": base,
                }));
                match self
                    .host
                    .create_user_template(&name, &description, &base, None, &[])
                {
                    Ok(tpl) => {
                        self.project_ui.template_create_open = false;
                        self.project_ui.template_draft_name.clear();
                        self.project_ui.template_draft_description.clear();
                        self.project_ui.selected_template_id = tpl.template_id.clone();
                        let _ = self.host.save_ui_state(serde_json::json!({
                            "selected_template_id": tpl.template_id,
                            "template_create_open": false,
                            "template_draft_name": "",
                            "template_draft_description": "",
                            "reset_settings_override": true,
                        }));
                        self.project_ui.reload_meta(&self.host);
                        self.project_ui.message =
                            Some(format!("Novi template spremljen: {}", tpl.name));
                        self.status = format!("Template: {}", tpl.name);
                        self.error = None;
                    }
                    Err(e) => self.error = Some(e),
                }
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
                match self.host.save_ui_state(serde_json::json!({
                    "template_create_open": open,
                    "template_draft_name": self.project_ui.template_draft_name,
                    "template_draft_description": self.project_ui.template_draft_description,
                })) {
                    Ok(ui) => {
                        self.project_ui.apply_ui_state(ui);
                        self.project_ui.template_create_open = open;
                        self.project_ui.advanced_open = open;
                        self.project_ui.message = if open {
                            Some("Novi template — upiši naziv i Spremi.".into())
                        } else {
                            None
                        };
                    }
                    Err(e) => self.error = Some(e),
                }
            }
        }
    }

    fn set_theme(&mut self, theme: ThemeId) {
        match self.host.save_appearance_user(theme.as_str()) {
            Ok(_) => {
                self.theme_id = theme;
                self.status = format!("Tema: {}", theme.label());
                self.error = None;
            }
            Err(e) => {
                // Still apply locally if host is older / offline write failed.
                self.theme_id = theme;
                self.error = Some(format!("Tema lokalno ({e})"));
            }
        }
    }

    /// Shell footer — theme picker far left (global, not project settings).
    fn ui_shell_theme_picker(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Tema").weak());
        let mut selected = self.theme_id;
        egui::ComboBox::from_id_salt("shell_theme_picker")
            .selected_text(selected.label())
            .width(110.0)
            .show_ui(ui, |ui| {
                for id in ThemeId::ALL {
                    ui.selectable_value(&mut selected, id, id.label());
                }
            });
        if selected != self.theme_id {
            self.set_theme(selected);
        }
    }

    fn handle_story_playback_commands(&mut self, ctx: &egui::Context) {
        player_bridge::handle_playback_commands(&mut self.story, &self.player, ctx);
    }

    fn poll_story_player_remote(&mut self) {
        player_bridge::poll_player_remote(&mut self.story, &self.story_player_rx, &self.player);
    }

    fn handle_ingest_playback_commands(&mut self, ctx: &egui::Context) {
        player_bridge::handle_playback_commands(&mut self.ingest, &self.player, ctx);
    }

    fn poll_ingest_player_remote(&mut self) {
        player_bridge::poll_player_remote(&mut self.ingest, &self.ingest_player_rx, &self.player);
    }

    fn handle_media_assist_playback_commands(&mut self, ctx: &egui::Context) {
        player_bridge::handle_playback_commands(&mut self.media_assist, &self.player, ctx);
    }

    fn poll_media_assist_player_remote(&mut self) {
        player_bridge::poll_player_remote(
            &mut self.media_assist,
            &self.media_assist_player_rx,
            &self.player,
        );
    }

    /// Form → TX, then player pump (TX→decode→RX fan-out), then screen RX → form.
    fn tick_active_player(&mut self, ctx: &egui::Context) {
        match self.screen {
            Screen::Ingest if self.phase == Phase::Workspace => {
                self.handle_ingest_playback_commands(ctx);
                self.player.pump(ctx);
                self.poll_ingest_player_remote();
            }
            Screen::Story if self.phase == Phase::Workspace => {
                self.handle_story_playback_commands(ctx);
                self.player.pump(ctx);
                self.poll_story_player_remote();
            }
            Screen::MediaAssist if self.phase == Phase::Workspace => {
                self.handle_media_assist_playback_commands(ctx);
                self.player.pump(ctx);
                self.poll_media_assist_player_remote();
            }
            _ => {
                self.player.pump(ctx);
            }
        }
    }

    /// Clock only — no command drain (avoids double-open / double-seek in one frame).
    fn pump_active_player(&mut self, ctx: &egui::Context) {
        self.player.pump(ctx);
        match self.screen {
            Screen::Ingest if self.phase == Phase::Workspace => self.poll_ingest_player_remote(),
            Screen::Story if self.phase == Phase::Workspace => self.poll_story_player_remote(),
            Screen::MediaAssist if self.phase == Phase::Workspace => {
                self.poll_media_assist_player_remote()
            }
            _ => {}
        }
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
            self.connect_and_load();
        }

        self.apply_theme(ctx);

        if self.phase == Phase::Workspace && self.screen == Screen::Ingest {
            if let Some(p) = self.open_project.clone() {
                self.ingest.maybe_poll(&self.host, &p.project_id);
                if self.ingest.needs_poll() {
                    ctx.request_repaint_after(std::time::Duration::from_millis(500));
                }
            }
        }

        let story_active = self.phase == Phase::Workspace && matches!(self.screen, Screen::Story);
        let media_assist_active =
            self.phase == Phase::Workspace && matches!(self.screen, Screen::MediaAssist);
        let comp = self.composition();

        // Pre-UI: keep decode clock + apply RX (no command drain — UI may queue seeks).
        self.pump_active_player(ctx);

        if story_active {
            self.story.handle_shortcuts(ctx, &self.host);
            self.story.prepare_frame(&self.host, ctx);
            self.story.tick(&self.host, ctx);
        } else if media_assist_active {
            self.media_assist.handle_shortcuts(ctx, &self.host);
            self.media_assist.prepare_frame(&self.host, ctx);
            self.media_assist.tick(&self.host, ctx);
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
                        self.connect_and_load();
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
                if let Some(err) = &self.error {
                    ui.colored_label(Color32::from_rgb(220, 90, 90), err);
                }
            });
        }

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
                        cols[2].allocate_exact_size(
                            egui::vec2(cols[2].available_width(), h),
                            egui::Sense::hover(),
                        );
                    });
                });
        }

        // Source editor dock — composition.dock decides which screens get it.
        let panel_bg = qnc_theme::current_ctx(ctx).bg;
        if comp.dock.show && story_active {
            let h = self.story.source_dock_height();
            egui::TopBottomPanel::bottom("story_source_dock")
                .exact_height(h)
                .frame(egui::Frame::NONE.fill(panel_bg).inner_margin(0.0))
                .show(ctx, |ui| {
                    self.story.ui_source_dock(ui, &self.host);
                });
        } else if comp.dock.show && media_assist_active {
            let h = self.media_assist.source_dock_height();
            egui::TopBottomPanel::bottom("media_assist_source_dock")
                .exact_height(h)
                .frame(egui::Frame::NONE.fill(panel_bg).inner_margin(0.0))
                .show(ctx, |ui| {
                    self.media_assist.ui_source_dock(ui, &self.host);
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
                    let action = self.ingest.ui_timeline_dock(ui);
                    if !pid.is_empty() {
                        self.dispatch_ingest(&pid, action);
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
                            self.connect_and_load();
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
                                let action = self.ingest.ui(ui, &name, &pid, &self.host, ctx);
                                self.dispatch_ingest(&pid, action);
                            }
                            Screen::MediaAssist => {
                                self.media_assist.ui_main(ui, &self.host, ctx);
                            }
                            Screen::Story => {
                                self.story.ui_main(ui, &self.host, ctx);
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
                        self.project_ui.ensure_loaded(&self.host);
                        let action = self.project_ui.ui(
                            ui,
                            &self.projects,
                            &mut self.selected_index,
                            &self.active_project_id,
                        );
                        self.dispatch_project(action);
                        if ui.input(|i| i.key_pressed(egui::Key::Enter))
                            && self.selected_index.is_some()
                            && self.phase == Phase::ProjectOnly
                        {
                            self.open_selected();
                        }
                    }
                }
            }
        });

        // Post-UI: commands queued during paint → TX/RX.
        self.tick_active_player(ctx);
    }
}
