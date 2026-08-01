//! Native Project — orchestrator (state + host I/O).
//!
//! Board: [`super::empty_story_layout::project_board`]
//! → `project_list` (left) | `setting_panel` (right).

use eframe::egui;
use serde_json::Value;

use crate::api::{FsEntry, HostClient, KeyboardPresetRow, ModuleRow, ProjectRow, TemplateRow};
use crate::project_pts;
use crate::qnc_location_browser::{clean_location_path, LocationSourceKind};

use super::empty_story_layout;
use super::project_list;
use super::project_list::{ProjectListAction, ProjectListInput};
use super::settings;

#[derive(Debug)]
pub struct ProjectScreen {
    pub new_name: String,
    pub templates: Vec<TemplateRow>,
    pub modules: Vec<ModuleRow>,
    pub keyboard_presets: Vec<KeyboardPresetRow>,
    pub selected_template_id: String,
    pub ui_state: Option<Value>,
    pub confirm_delete: bool,
    pub confirm_delete_template: Option<String>,
    pub message: Option<String>,
    pub template_create_open: bool,
    pub template_draft_name: String,
    pub template_draft_description: String,
    pub advanced_open: bool,
    pub picker_open: bool,
    pub default_projects_root: String,
    pub projects_root_draft: String,
    pub projects_root_browser_open: bool,
    pub projects_root_browser_kind: LocationSourceKind,
    pub projects_root_browser_roots: bool,
    pub projects_root_browser_path: String,
    pub projects_root_browser_parent: Option<String>,
    pub projects_root_browser_entries: Vec<FsEntry>,
    pub projects_root_browser_error: Option<String>,
    pub export_dir_draft: String,
    pub export_dir_browser_open: bool,
    pub export_dir_browser_kind: LocationSourceKind,
    pub export_dir_browser_roots: bool,
    pub export_dir_browser_path: String,
    pub export_dir_browser_parent: Option<String>,
    pub export_dir_browser_entries: Vec<FsEntry>,
    pub export_dir_browser_error: Option<String>,
    pub export_preset_draft_name: String,
    loaded: bool,
}

impl Default for ProjectScreen {
    fn default() -> Self {
        Self {
            new_name: String::new(),
            templates: Vec::new(),
            modules: Vec::new(),
            keyboard_presets: Vec::new(),
            selected_template_id: String::new(),
            ui_state: None,
            confirm_delete: false,
            confirm_delete_template: None,
            message: None,
            template_create_open: false,
            template_draft_name: String::new(),
            template_draft_description: String::new(),
            advanced_open: false,
            picker_open: false,
            default_projects_root: String::new(),
            projects_root_draft: String::new(),
            projects_root_browser_open: false,
            projects_root_browser_kind: LocationSourceKind::Local,
            projects_root_browser_roots: true,
            projects_root_browser_path: String::new(),
            projects_root_browser_parent: None,
            projects_root_browser_entries: Vec::new(),
            projects_root_browser_error: None,
            export_dir_draft: String::new(),
            export_dir_browser_open: false,
            export_dir_browser_kind: LocationSourceKind::Local,
            export_dir_browser_roots: true,
            export_dir_browser_path: String::new(),
            export_dir_browser_parent: None,
            export_dir_browser_entries: Vec::new(),
            export_dir_browser_error: None,
            export_preset_draft_name: String::new(),
            loaded: false,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ProjectAction {
    None,
    Reload,
    OpenSelected,
    Create,
    DeleteSelected,
    DeleteTemplate(String),
    SelectTemplate(String),
    ToggleProjectsRootBrowser,
    SelectProjectsRootKind(LocationSourceKind),
    OpenProjectsRootPath(String),
    ConfirmProjectsRootBrowser,
    CancelProjectsRootBrowser,
    ToggleExportDirBrowser,
    SelectExportDirKind(LocationSourceKind),
    OpenExportDirPath(String),
    ConfirmExportDirBrowser,
    CancelExportDirBrowser,
    ToggleWorkflowTab(String, bool),
    SetSettingsPath(String, Value),
    MergeSettingsOverride(Value),
    ApplyExportPreset(String),
    SaveExportPreset,
    SaveCustomTemplate,
    /// Web `template.create-panel.open/close`
    SetTemplateCreateOpen(bool),
}

impl ProjectScreen {
    pub fn ensure_loaded(&mut self, host: &HostClient) {
        if self.loaded && self.ui_state.is_some() && !self.templates.is_empty() {
            return;
        }
        self.reload_meta(host);
    }

    pub fn reload_meta(&mut self, host: &HostClient) {
        match host.list_templates() {
            Ok(t) => {
                self.templates = t;
                if self.selected_template_id.is_empty() {
                    if let Some(first) = self.templates.first() {
                        self.selected_template_id = first.template_id.clone();
                    }
                }
            }
            Err(e) => self.message = Some(e),
        }
        match host.list_modules() {
            Ok(mut m) => {
                m.sort_by_key(|row| project_pts::module_sort_key(row));
                self.modules = m;
            }
            Err(e) => {
                if self.message.is_none() {
                    self.message = Some(e);
                }
            }
        }
        match host.keyboard_presets() {
            Ok(p) => self.keyboard_presets = p,
            Err(_) => {}
        }
        if let Ok(root) = host.default_projects_root() {
            self.default_projects_root = root;
        }
        match host.ui_state() {
            Ok(ui) => {
                if let Some(sel) = ui.get("selected_template_id").and_then(|v| v.as_str()) {
                    if !sel.is_empty() {
                        self.selected_template_id = sel.to_string();
                    }
                }
                if let Some(name) = ui.get("project_name").and_then(|v| v.as_str()) {
                    if self.new_name.is_empty() {
                        self.new_name = name.to_string();
                    }
                }
                if let Some(open) = ui.get("template_create_open").and_then(|v| v.as_bool()) {
                    self.template_create_open = open;
                }
                if let Some(n) = ui.get("template_draft_name").and_then(|v| v.as_str()) {
                    if self.template_draft_name.is_empty() && !n.is_empty() {
                        self.template_draft_name = n.to_string();
                    }
                }
                self.ui_state = Some(ui);
                self.sync_path_drafts();
                self.message = None;
            }
            Err(e) => self.message = Some(e),
        }
        self.loaded = true;
    }

    pub fn apply_ui_state(&mut self, ui: Value) {
        if let Some(sel) = ui.get("selected_template_id").and_then(|v| v.as_str()) {
            if !sel.is_empty() {
                self.selected_template_id = sel.to_string();
            }
        }
        self.ui_state = Some(ui);
        self.sync_path_drafts();
    }

    fn sync_path_drafts(&mut self) {
        let eff = self.effective().cloned().unwrap_or(Value::Null);
        let configured = Self::path_str(&eff, &["storage", "projects_root"]);
        let projects_root = if configured.is_empty() {
            self.default_projects_root.clone()
        } else {
            configured
        };
        self.projects_root_draft = clean_location_path(&projects_root);
        let export = Self::path_str(&eff, &["export", "directory"]);
        let export_dir = if export.is_empty() {
            Self::path_str(&eff, &["export", "output_directory"])
        } else {
            export
        };
        self.export_dir_draft = clean_location_path(&export_dir);
        if self.export_dir_draft.is_empty() {
            self.export_dir_draft = "exports/projekti".into();
        }
    }

    pub(super) fn effective(&self) -> Option<&Value> {
        self.ui_state
            .as_ref()
            .and_then(|u| u.get("effective_settings"))
    }

    pub(super) fn path_str(effective: &Value, path: &[&str]) -> String {
        let mut cur = effective;
        for p in path {
            cur = match cur.get(*p) {
                Some(v) => v,
                None => return String::new(),
            };
        }
        cur.as_str().unwrap_or("").to_string()
    }

    pub(super) fn path_bool(effective: &Value, path: &[&str], default: bool) -> bool {
        let mut cur = effective;
        for p in path {
            cur = match cur.get(*p) {
                Some(v) => v,
                None => return default,
            };
        }
        cur.as_bool().unwrap_or(default)
    }

    pub fn apply_projects_root_listing(
        &mut self,
        roots: bool,
        path: String,
        parent: Option<String>,
        entries: Vec<FsEntry>,
    ) {
        let path = clean_location_path(&path);
        let parent = parent.map(|p| clean_location_path(&p));
        let entries = entries
            .into_iter()
            .map(|mut entry| {
                entry.path = clean_location_path(&entry.path);
                entry.name = clean_location_path(&entry.name);
                entry
            })
            .collect();
        self.projects_root_browser_roots = roots;
        self.projects_root_browser_path = path;
        self.projects_root_browser_parent = parent;
        self.projects_root_browser_entries = entries;
        self.projects_root_browser_error = None;
        if !self.projects_root_browser_path.trim().is_empty() {
            self.projects_root_draft = self.projects_root_browser_path.clone();
        }
    }

    pub fn set_projects_root_browser_error(&mut self, error: impl Into<String>) {
        self.projects_root_browser_error = Some(error.into());
        self.projects_root_browser_entries.clear();
    }

    pub fn apply_export_dir_listing(
        &mut self,
        roots: bool,
        path: String,
        parent: Option<String>,
        entries: Vec<FsEntry>,
    ) {
        let path = clean_location_path(&path);
        let parent = parent.map(|p| clean_location_path(&p));
        let entries = entries
            .into_iter()
            .map(|mut entry| {
                entry.path = clean_location_path(&entry.path);
                entry.name = clean_location_path(&entry.name);
                entry
            })
            .collect();
        self.export_dir_browser_roots = roots;
        self.export_dir_browser_path = path;
        self.export_dir_browser_parent = parent;
        self.export_dir_browser_entries = entries;
        self.export_dir_browser_error = None;
        if !self.export_dir_browser_path.trim().is_empty() {
            self.export_dir_draft = self.export_dir_browser_path.clone();
        }
    }

    pub fn set_export_dir_browser_error(&mut self, error: impl Into<String>) {
        self.export_dir_browser_error = Some(error.into());
        self.export_dir_browser_entries.clear();
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        projects: &[ProjectRow],
        selected_index: &mut Option<usize>,
        active_project_id: &str,
    ) -> ProjectAction {
        let mut action = ProjectAction::None;

        empty_story_layout::project_board(ui, |ui, side, w, h| match side {
            crate::qnc_ui::ShellSide::Left => {
                // Component: project_list
                let status = if let Some(msg) = &self.message {
                    msg.as_str()
                } else if projects.is_empty() {
                    "Nema projekata."
                } else {
                    "Spreman."
                };
                let input = ProjectListInput {
                    width: w,
                    height: h,
                    projects,
                    selected_index: *selected_index,
                    active_project_id,
                    confirm_delete: self.confirm_delete,
                    status,
                };
                match project_list::show(ui, input) {
                    ProjectListAction::None => {}
                    ProjectListAction::Select(i) => *selected_index = Some(i),
                    ProjectListAction::Open(i) => {
                        *selected_index = Some(i);
                        action = ProjectAction::OpenSelected;
                    }
                    ProjectListAction::RequestDelete(i) => {
                        *selected_index = Some(i);
                        self.confirm_delete = true;
                    }
                    ProjectListAction::ConfirmDelete => {
                        action = ProjectAction::DeleteSelected;
                        self.confirm_delete = false;
                    }
                    ProjectListAction::CancelDelete => {
                        self.confirm_delete = false;
                    }
                }
            }
            crate::qnc_ui::ShellSide::Right => {
                // setting_panel
                let a = settings::show(ui, w, h, self);
                if !matches!(a, ProjectAction::None) {
                    action = a;
                }
            }
        });

        action
    }
}
