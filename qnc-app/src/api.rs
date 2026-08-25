//! HTTP client for qnc-host (projects, ingest, story playback).

use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

const TIMEOUT: Duration = Duration::from_secs(8);
const LONG_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone)]
pub struct HostClient {
    base_url: String,
    agent: ureq::Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostRequestMethod {
    Get,
    Post,
}

impl HostRequestMethod {
    fn label(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostRequestTimeout {
    Default,
    Long,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Health {
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct ProjectRow {
    pub project_id: String,
    pub name: String,
    #[serde(default)]
    pub project_dir: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub last_opened_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct ProjectsList {
    pub status: String,
    #[serde(default)]
    pub active_project_id: String,
    #[serde(default)]
    pub projects: Vec<ProjectRow>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct TemplateRow {
    pub template_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub system: Value,
}

impl TemplateRow {
    pub fn is_system(&self) -> bool {
        self.system.as_bool().unwrap_or(false)
            || self.system.as_i64() == Some(1)
            || self.system.as_u64() == Some(1)
            || self.system.as_str() == Some("1")
            || self.system.as_str() == Some("true")
    }
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct ModuleRow {
    #[serde(default)]
    pub module_id: String,
    #[serde(default)]
    pub tab_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub position: String,
    #[serde(default)]
    pub priority: i64,
}

impl ModuleRow {
    pub fn tab_key(&self) -> &str {
        if !self.tab_id.is_empty() {
            &self.tab_id
        } else {
            &self.module_id
        }
    }

    pub fn display_label(&self) -> &str {
        if !self.label.is_empty() {
            &self.label
        } else {
            self.tab_key()
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct KeyboardPresetRow {
    pub id: String,
    #[serde(default)]
    pub name: String,
}

pub(crate) fn default_keyboard_presets() -> Vec<KeyboardPresetRow> {
    [
        ("default", "QNC"),
        ("resolve", "DaVinci Resolve"),
        ("premiere", "Adobe Premiere Pro"),
        ("finalcut", "Final Cut Pro 11"),
        ("edius", "Grass Valley EDIUS"),
        ("avid", "Avid Media Composer"),
    ]
    .into_iter()
    .map(|(id, name)| KeyboardPresetRow {
        id: id.into(),
        name: name.into(),
    })
    .collect()
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct WorkflowStep {
    #[serde(default)]
    pub step_id: String,
    #[serde(default)]
    pub tab_id: String,
    #[serde(default)]
    pub next_step_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct Workspace {
    pub project_id: String,
    #[serde(default)]
    pub template_id: Value,
    #[serde(default)]
    pub tabs: Vec<String>,
    #[serde(default)]
    pub tab_labels: Value,
    #[serde(default)]
    pub steps: Vec<WorkflowStep>,
    #[serde(default)]
    pub entry_step_id: Option<String>,
    #[serde(default)]
    pub active_step_id: Option<String>,
}

impl HostClient {
    pub fn new(base_url: &str) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(TIMEOUT)
            .timeout_read(TIMEOUT)
            .build();
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            agent,
        }
    }

    pub fn set_base_url(&mut self, base_url: &str) {
        self.base_url = base_url.trim_end_matches('/').to_string();
    }

    pub fn absolute(&self, rel: &str) -> String {
        if rel.starts_with("http://") || rel.starts_with("https://") {
            rel.to_string()
        } else if rel.starts_with('/') {
            format!("{}{rel}", self.base_url)
        } else {
            format!("{}/{rel}", self.base_url)
        }
    }

    pub fn request_json(
        &self,
        method: HostRequestMethod,
        rel: &str,
        payload: Option<Value>,
        timeout: HostRequestTimeout,
    ) -> Result<Value, String> {
        let url = self.absolute(rel);
        let response = match method {
            HostRequestMethod::Get => match timeout {
                HostRequestTimeout::Default => self.agent.get(&url).call(),
                HostRequestTimeout::Long => long_agent().get(&url).call(),
            },
            HostRequestMethod::Post => {
                let body = payload.unwrap_or(Value::Null);
                match timeout {
                    HostRequestTimeout::Default => self.agent.post(&url).send_json(body),
                    HostRequestTimeout::Long => long_agent().post(&url).send_json(body),
                }
            }
        };
        response
            .map_err(|e| format!("{} {rel}: {e}", method.label()))?
            .into_json()
            .map_err(|e| format!("{} {rel} json: {e}", method.label()))
    }
}

fn long_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(TIMEOUT)
        .timeout_read(LONG_TIMEOUT)
        .build()
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FsEntry {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub path: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FsList {
    #[serde(default)]
    pub roots: bool,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub entries: Vec<FsEntry>,
}

pub(crate) fn encode_query_value(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct IngestClip {
    pub clip_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub duration_sec: f64,
    #[serde(default)]
    pub resolution: String,
    #[serde(default)]
    pub codec: String,
    #[serde(default)]
    pub fps: f64,
    #[serde(default)]
    pub source_timebase: EditorialSourceTimebase,
    #[serde(default)]
    pub has_audio: bool,
    #[serde(default)]
    pub audio_channels: u8,
    #[serde(default)]
    pub field_order: String,
    #[serde(default)]
    pub interlaced: bool,
    #[serde(default)]
    pub source_class: String,
    #[serde(default)]
    pub proxy_recipe: String,
    #[serde(default)]
    pub import_status: String,
    #[serde(default)]
    pub proxy_status: String,
    #[serde(default)]
    pub selected: bool,
    #[serde(default)]
    pub import_label: String,
    #[serde(default)]
    pub status_original: String,
    #[serde(default)]
    pub status_proxy: String,
    #[serde(default)]
    pub extension: String,
    #[serde(default)]
    pub source_path: String,
    #[serde(default)]
    pub original_path: String,
    #[serde(default)]
    pub proxy_path: String,
    #[serde(default)]
    pub project_proxy_path: String,
    #[serde(default)]
    pub thumb_url: String,
    #[serde(default)]
    pub thumb_status: String,
    #[serde(default)]
    pub thumb_error: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct IngestState {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub browse_path: String,
    #[serde(default)]
    pub active_source_id: String,
    #[serde(default)]
    pub clips: Vec<IngestClip>,
    #[serde(default)]
    pub selected_clip_ids: Vec<String>,
    #[serde(default)]
    pub selection_revision: u64,
    #[serde(default)]
    pub archive_original: bool,
    #[serde(default)]
    pub durations_pending: bool,
    #[serde(default)]
    pub import_queued: bool,
    #[serde(default)]
    pub queued: Option<u64>,
}

/// First workflow tab after Project. The project template/DB step graph owns this.
pub fn workflow_entry_tab(workspace: &Workspace) -> String {
    workspace
        .entry_step_id
        .as_deref()
        .and_then(|step_id| workflow_tab_for_step(workspace, step_id))
        .or_else(|| first_non_project_step_tab(workspace))
        .or_else(|| first_non_project_tab(&workspace.tabs))
        .unwrap_or_else(|| "project".into())
}

/// Next workflow tab after `from`. Prefer explicit DB `next_step_id`; tabs are legacy fallback.
pub fn workflow_next_tab(workspace: &Workspace, from: &str) -> Option<String> {
    if let Some(current) = workspace
        .steps
        .iter()
        .find(|step| step.tab_id.as_str() == from)
    {
        return current
            .next_step_id
            .as_deref()
            .and_then(|step_id| workflow_tab_for_step(workspace, step_id))
            .filter(|tab| tab.as_str() != "project");
    }
    workflow_next_tab_from_tabs(&workspace.tabs, from)
}

fn workflow_tab_for_step(workspace: &Workspace, step_id: &str) -> Option<String> {
    workspace
        .steps
        .iter()
        .find(|step| step.step_id == step_id)
        .map(|step| step.tab_id.trim())
        .filter(|tab| !tab.is_empty())
        .map(str::to_string)
}

fn first_non_project_step_tab(workspace: &Workspace) -> Option<String> {
    workspace
        .steps
        .iter()
        .map(|step| step.tab_id.trim())
        .find(|tab| !tab.is_empty() && *tab != "project")
        .map(str::to_string)
}

fn first_non_project_tab(tabs: &[String]) -> Option<String> {
    tabs.iter().find(|t| t.as_str() != "project").cloned()
}

fn workflow_next_tab_from_tabs(tabs: &[String], from: &str) -> Option<String> {
    let i = tabs.iter().position(|t| t == from)?;
    tabs.get(i + 1).filter(|t| t.as_str() != "project").cloned()
}

pub fn tab_label(workspace: &Workspace, tab_id: &str) -> String {
    workspace
        .tab_labels
        .get(tab_id)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| match tab_id {
            "ingest" => "Ingest".into(),
            "storyboard" | "story" => "Story".into(),
            "media_assist" => "Media Assist".into(),
            "project" => "Project".into(),
            other => other.to_string(),
        })
}

pub fn is_story_tab(tab_id: &str) -> bool {
    matches!(tab_id, "storyboard" | "story")
}

pub fn is_media_assist_tab(tab_id: &str) -> bool {
    tab_id == "media_assist"
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct TimelineSegment {
    pub part_id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub schema: String,
    #[serde(default)]
    pub clip_id: String,
    #[serde(default)]
    pub global_start_frame: i64,
    #[serde(default)]
    pub global_end_frame: i64,
    #[serde(default)]
    pub duration_frames: i64,
    #[serde(default)]
    pub global_start_sec: f64,
    #[serde(default)]
    pub global_end_sec: f64,
    #[serde(default)]
    pub duration_sec: f64,
    #[serde(default)]
    pub streamable: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct TimelineModel {
    pub project_id: String,
    #[serde(default)]
    pub application: String,
    #[serde(default)]
    pub timeline_fps: f64,
    #[serde(default)]
    pub duration_frames: i64,
    #[serde(default)]
    pub duration_sec: f64,
    #[serde(default)]
    pub rows: Vec<String>,
    #[serde(default)]
    pub segments: Vec<TimelineSegment>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct EditorialPlaylistSource {
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub part_id: String,
    #[serde(default)]
    pub cover_id: String,
    #[serde(default)]
    pub virtual_shot_id: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
#[allow(dead_code)]
pub struct EditorialSourceTimebase {
    #[serde(default)]
    pub fps_num: i64,
    #[serde(default)]
    pub fps_den: i64,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct EditorialPlaylistCover {
    pub cover_id: String,
    #[serde(default)]
    pub clip_id: String,
    #[serde(default)]
    pub virtual_shot_id: String,
    #[serde(default)]
    pub timeline_start_frame: i64,
    #[serde(default)]
    pub timeline_end_frame: i64,
    #[serde(default)]
    pub source_in_frame: i64,
    #[serde(default)]
    pub source_out_frame: i64,
    #[serde(default)]
    pub source_fps: f64,
    #[serde(default)]
    pub source_timebase: EditorialSourceTimebase,
    #[serde(default)]
    pub streamable: bool,
    #[serde(default)]
    pub source: EditorialPlaylistSource,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct EditorialPlaylistSegment {
    pub part_id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub clip_id: String,
    #[serde(default)]
    pub virtual_shot_id: String,
    #[serde(default)]
    pub global_start_frame: i64,
    #[serde(default)]
    pub global_end_frame: i64,
    #[serde(default)]
    pub duration_frames: i64,
    #[serde(default)]
    pub global_start_sec: f64,
    #[serde(default)]
    pub global_end_sec: f64,
    #[serde(default)]
    pub duration_sec: f64,
    #[serde(default)]
    pub source_in_frame: i64,
    #[serde(default)]
    pub source_out_frame: i64,
    #[serde(default)]
    pub source_fps: f64,
    #[serde(default)]
    pub source_timebase: EditorialSourceTimebase,
    #[serde(default)]
    pub streamable: bool,
    #[serde(default)]
    pub source: EditorialPlaylistSource,
    #[serde(default)]
    pub covers: Vec<EditorialPlaylistCover>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct EditorialPlaylist {
    pub project_id: String,
    #[serde(default)]
    pub timeline_fps: f64,
    #[serde(default)]
    pub duration_frames: i64,
    #[serde(default)]
    pub duration_sec: f64,
    #[serde(default)]
    pub segments: Vec<EditorialPlaylistSegment>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(step_id: &str, tab_id: &str, next_step_id: Option<&str>) -> WorkflowStep {
        WorkflowStep {
            step_id: step_id.to_string(),
            tab_id: tab_id.to_string(),
            next_step_id: next_step_id.map(str::to_string),
        }
    }

    #[test]
    fn workflow_entry_uses_template_entry_step_not_ingest_preference() {
        let workspace = Workspace {
            project_id: "p".into(),
            tabs: vec!["project".into(), "ingest".into(), "media_assist".into()],
            steps: vec![
                step("step_project", "project", Some("step_media_assist")),
                step("step_ingest", "ingest", None),
                step("step_media_assist", "media_assist", None),
            ],
            entry_step_id: Some("step_media_assist".into()),
            ..Workspace::default()
        };

        assert_eq!(workflow_entry_tab(&workspace), "media_assist");
    }

    #[test]
    fn workflow_next_uses_next_step_id_not_tab_order() {
        let workspace = Workspace {
            project_id: "p".into(),
            tabs: vec![
                "project".into(),
                "ingest".into(),
                "media_assist".into(),
                "story".into(),
            ],
            steps: vec![
                step("step_project", "project", Some("step_ingest")),
                step("step_ingest", "ingest", Some("step_story")),
                step("step_media_assist", "media_assist", None),
                step("step_story", "story", None),
            ],
            entry_step_id: Some("step_ingest".into()),
            ..Workspace::default()
        };

        assert_eq!(
            workflow_next_tab(&workspace, "ingest"),
            Some("story".into())
        );
    }

    #[test]
    fn workflow_entry_legacy_tabs_keep_template_order() {
        let workspace = Workspace {
            project_id: "p".into(),
            tabs: vec!["project".into(), "media_assist".into(), "ingest".into()],
            ..Workspace::default()
        };

        assert_eq!(workflow_entry_tab(&workspace), "media_assist");
    }

    #[test]
    fn workflow_entry_empty_workspace_returns_project() {
        let workspace = Workspace::default();

        assert_eq!(workflow_entry_tab(&workspace), "project");
    }
}
