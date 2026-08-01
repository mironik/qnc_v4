//! HTTP client for qnc-host (projects, ingest, story playback).

use std::io::Read;
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

fn default_keyboard_presets() -> Vec<KeyboardPresetRow> {
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

#[derive(Debug, Clone, Deserialize)]
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
    pub entry_step_id: Option<String>,
    #[serde(default)]
    pub active_step_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct WorkspaceEnvelope {
    status: String,
    workspace: Workspace,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenEnvelope {
    status: String,
    #[serde(default)]
    active_project_id: String,
    #[serde(default)]
    project: Option<ProjectRow>,
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

    pub fn health(&self) -> Result<Health, String> {
        let url = format!("{}/api/health", self.base_url);
        self.agent
            .get(&url)
            .call()
            .map_err(|e| format!("health: {e}"))?
            .into_json()
            .map_err(|e| format!("health json: {e}"))
    }

    pub fn runtime(&self) -> Result<Value, String> {
        let url = format!("{}/api/shell/runtime", self.base_url);
        self.agent
            .get(&url)
            .call()
            .map_err(|e| format!("runtime: {e}"))?
            .into_json()
            .map_err(|e| format!("runtime json: {e}"))
    }

    /// Full keyboard catalog (host seed via API).
    pub fn keyboard_catalog(&self) -> Result<Value, String> {
        let url = format!("{}/api/shell/keyboard-shortcuts", self.base_url);
        self.agent
            .get(&url)
            .call()
            .map_err(|e| format!("keyboard catalog: {e}"))?
            .into_json()
            .map_err(|e| format!("keyboard catalog json: {e}"))
    }

    /// User keyboard overrides from SQLite (`keyboard_shortcuts_user`).
    pub fn keyboard_user(&self) -> Result<Value, String> {
        let url = format!("{}/api/settings/keyboard-shortcuts", self.base_url);
        self.agent
            .get(&url)
            .call()
            .map_err(|e| format!("keyboard user: {e}"))?
            .into_json()
            .map_err(|e| format!("keyboard user json: {e}"))
    }

    /// User appearance prefs from SQLite (`ui_appearance_user`).
    pub fn appearance_user(&self) -> Result<Value, String> {
        let url = format!("{}/api/settings/appearance", self.base_url);
        self.agent
            .get(&url)
            .call()
            .map_err(|e| format!("appearance: {e}"))?
            .into_json()
            .map_err(|e| format!("appearance json: {e}"))
    }

    pub fn save_appearance_user(&self, theme_id: &str) -> Result<Value, String> {
        let url = format!("{}/api/settings/appearance", self.base_url);
        let body = serde_json::json!({ "theme_id": theme_id });
        self.agent
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("appearance save: {e}"))?
            .into_json()
            .map_err(|e| format!("appearance save json: {e}"))
    }

    pub fn list_projects(&self) -> Result<ProjectsList, String> {
        let url = format!("{}/api/projects", self.base_url);
        self.agent
            .get(&url)
            .call()
            .map_err(|e| format!("projects: {e}"))?
            .into_json()
            .map_err(|e| format!("projects json: {e}"))
    }

    pub fn open_project(&self, project_id: &str) -> Result<(ProjectRow, String), String> {
        let url = format!("{}/api/projects/open", self.base_url);
        let body = serde_json::json!({ "project_id": project_id });
        let resp: OpenEnvelope = self
            .agent
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("open: {e}"))?
            .into_json()
            .map_err(|e| format!("open json: {e}"))?;
        if resp.status != "ok" {
            return Err(format!("open status={}", resp.status));
        }
        let project = resp
            .project
            .ok_or_else(|| "open: missing project".to_string())?;
        Ok((project, resp.active_project_id))
    }

    pub fn delete_projects(&self, project_ids: &[String]) -> Result<ProjectsList, String> {
        let url = format!("{}/api/projects/delete", self.base_url);
        let body = serde_json::json!({ "project_ids": project_ids });
        let v: Value = long_agent()
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("delete: {e}"))?
            .into_json()
            .map_err(|e| format!("delete json: {e}"))?;
        if v.get("status").and_then(|s| s.as_str()) != Some("ok") {
            return Err(format!("delete status={v}"));
        }
        Ok(ProjectsList {
            status: "ok".into(),
            active_project_id: v
                .get("active_project_id")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            projects: v
                .get("projects")
                .cloned()
                .and_then(|p| serde_json::from_value(p).ok())
                .unwrap_or_default(),
        })
    }

    pub fn create_user_template(
        &self,
        name: &str,
        description: &str,
        base_template_id: &str,
        settings: Option<Value>,
        source_template_ids: &[String],
    ) -> Result<TemplateRow, String> {
        let url = format!("{}/api/project-templates", self.base_url);
        let body = serde_json::json!({
            "name": name,
            "description": description,
            "base_template_id": base_template_id,
            "settings": settings,
            "source_template_ids": source_template_ids,
        });
        let v: Value = self
            .agent
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("create template: {e}"))?
            .into_json()
            .map_err(|e| format!("create template json: {e}"))?;
        if v.get("status").and_then(|s| s.as_str()) != Some("ok") {
            return Err(format!("create template: {v}"));
        }
        serde_json::from_value(
            v.get("template")
                .cloned()
                .ok_or_else(|| "create template: missing template".to_string())?,
        )
        .map_err(|e| format!("create template parse: {e}"))
    }

    pub fn delete_user_template(
        &self,
        template_id: &str,
    ) -> Result<(Vec<TemplateRow>, Value), String> {
        let url = format!("{}/api/project-templates/delete", self.base_url);
        let body = serde_json::json!({ "template_id": template_id });
        let v: Value = self
            .agent
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("delete template: {e}"))?
            .into_json()
            .map_err(|e| format!("delete template json: {e}"))?;
        if v.get("status").and_then(|s| s.as_str()) != Some("ok") {
            return Err(format!(
                "delete template: {}",
                v.get("detail")
                    .or_else(|| v.get("message"))
                    .cloned()
                    .unwrap_or(v)
            ));
        }
        let templates = v
            .get("templates")
            .cloned()
            .and_then(|t| serde_json::from_value(t).ok())
            .unwrap_or_default();
        let ui = v.get("ui_state").cloned().unwrap_or(Value::Null);
        Ok((templates, ui))
    }

    pub fn list_templates(&self) -> Result<Vec<TemplateRow>, String> {
        let url = format!("{}/api/project-templates", self.base_url);
        let v: Value = self
            .agent
            .get(&url)
            .call()
            .map_err(|e| format!("templates: {e}"))?
            .into_json()
            .map_err(|e| format!("templates json: {e}"))?;
        let templates = v
            .get("templates")
            .cloned()
            .and_then(|t| serde_json::from_value(t).ok())
            .unwrap_or_default();
        Ok(templates)
    }

    pub fn list_modules(&self) -> Result<Vec<ModuleRow>, String> {
        let url = format!("{}/api/modules", self.base_url);
        let v: Value = self
            .agent
            .get(&url)
            .call()
            .map_err(|e| format!("modules: {e}"))?
            .into_json()
            .map_err(|e| format!("modules json: {e}"))?;
        let modules = v
            .get("modules")
            .cloned()
            .and_then(|m| serde_json::from_value(m).ok())
            .unwrap_or_default();
        Ok(modules)
    }

    pub fn keyboard_presets(&self) -> Result<Vec<KeyboardPresetRow>, String> {
        let url = format!("{}/api/settings/keyboard-shortcuts/presets", self.base_url);
        match self.agent.get(&url).call() {
            Ok(resp) => {
                let v: Value = resp
                    .into_json()
                    .map_err(|e| format!("kbd presets json: {e}"))?;
                let presets = v
                    .get("presets")
                    .cloned()
                    .and_then(|p| serde_json::from_value(p).ok())
                    .unwrap_or_else(default_keyboard_presets);
                Ok(if presets.is_empty() {
                    default_keyboard_presets()
                } else {
                    presets
                })
            }
            Err(_) => Ok(default_keyboard_presets()),
        }
    }

    pub fn default_projects_root(&self) -> Result<String, String> {
        let url = format!("{}/api/shell/projects-root", self.base_url);
        let v: Value = self
            .agent
            .get(&url)
            .call()
            .map_err(|e| format!("projects-root: {e}"))?
            .into_json()
            .map_err(|e| format!("projects-root json: {e}"))?;
        Ok(v.get("projects_root")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string())
    }

    pub fn ui_state(&self) -> Result<Value, String> {
        let url = format!("{}/api/projects/ui-state", self.base_url);
        let v: Value = self
            .agent
            .get(&url)
            .call()
            .map_err(|e| format!("ui-state: {e}"))?
            .into_json()
            .map_err(|e| format!("ui-state json: {e}"))?;
        v.get("ui_state")
            .cloned()
            .ok_or_else(|| "ui-state missing".into())
    }

    pub fn save_ui_state(&self, patch: Value) -> Result<Value, String> {
        let url = format!("{}/api/projects/ui-state", self.base_url);
        let v: Value = self
            .agent
            .post(&url)
            .send_json(patch)
            .map_err(|e| format!("ui-state save: {e}"))?
            .into_json()
            .map_err(|e| format!("ui-state save json: {e}"))?;
        v.get("ui_state")
            .cloned()
            .ok_or_else(|| "ui-state save missing".into())
    }

    pub fn save_settings_path(&self, path: &str, value: Value) -> Result<Value, String> {
        self.save_ui_state(serde_json::json!({
            "settings_path": { "path": path, "value": value }
        }))
    }

    /// Deep-merge into `settings_override` (web `settings_override: { export: … }`).
    pub fn merge_settings_override(&self, patch: Value) -> Result<Value, String> {
        self.save_ui_state(serde_json::json!({ "settings_override": patch }))
    }

    pub fn create_from_template(
        &self,
        name: &str,
        template_id: &str,
    ) -> Result<(ProjectRow, String), String> {
        let url = format!("{}/api/projects/from-template", self.base_url);
        let body = serde_json::json!({
            "name": name,
            "template_id": template_id,
        });
        let v: Value = long_agent()
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("from-template: {e}"))?
            .into_json()
            .map_err(|e| format!("from-template json: {e}"))?;
        if v.get("status").and_then(|s| s.as_str()) != Some("ok") {
            return Err(format!("from-template status={v}"));
        }
        let project: ProjectRow = serde_json::from_value(
            v.get("project")
                .cloned()
                .ok_or_else(|| "from-template: missing project".to_string())?,
        )
        .map_err(|e| format!("from-template project: {e}"))?;
        let active = v
            .get("active_project_id")
            .and_then(|x| x.as_str())
            .unwrap_or(&project.project_id)
            .to_string();
        Ok((project, active))
    }

    pub fn workspace(&self, project_id: &str) -> Result<Workspace, String> {
        let url = format!("{}/api/projects/{project_id}/workspace", self.base_url);
        let resp: WorkspaceEnvelope = self
            .agent
            .get(&url)
            .call()
            .map_err(|e| format!("workspace: {e}"))?
            .into_json()
            .map_err(|e| format!("workspace json: {e}"))?;
        if resp.status != "ok" {
            return Err(format!("workspace status={}", resp.status));
        }
        Ok(resp.workspace)
    }

    /// Host-side native folder dialog (same machine as qnc-host). Cancel → Ok(None).
    pub fn pick_directory(&self, initial_dir: &str) -> Result<Option<String>, String> {
        let url = format!("{}/api/shell/pick-directory", self.base_url);
        let body = serde_json::json!({ "initial_dir": initial_dir });
        match self.agent.post(&url).send_json(body) {
            Ok(resp) => {
                let v: Value = resp
                    .into_json()
                    .map_err(|e| format!("pick-directory json: {e}"))?;
                Ok(v.get("path")
                    .and_then(|p| p.as_str())
                    .map(str::to_string)
                    .filter(|s| !s.is_empty()))
            }
            Err(ureq::Error::Status(409, _)) => Ok(None),
            Err(e) => Err(format!("pick-directory: {e}")),
        }
    }

    /// In-app folder browser (same as web ingest-dir-tree). Empty path → drive roots.
    pub fn fs_list(&self, path: &str) -> Result<FsList, String> {
        let url = if path.trim().is_empty() {
            format!("{}/api/shell/fs-list", self.base_url)
        } else {
            format!(
                "{}/api/shell/fs-list?path={}",
                self.base_url,
                urlencoding_project(path)
            )
        };
        self.agent
            .get(&url)
            .call()
            .map_err(|e| format!("fs-list: {e}"))?
            .into_json()
            .map_err(|e| format!("fs-list json: {e}"))
    }

    pub fn ingest_state(&self, project_id: &str) -> Result<IngestState, String> {
        let url = format!(
            "{}/api/ingest/state?project_id={}",
            self.base_url,
            urlencoding_project(project_id)
        );
        self.agent
            .get(&url)
            .call()
            .map_err(|e| format!("ingest state: {e}"))?
            .into_json()
            .map_err(|e| format!("ingest state json: {e}"))
    }

    pub fn ingest_browse(&self, project_id: &str, path: &str) -> Result<IngestState, String> {
        let url = format!("{}/api/ingest/browse", self.base_url);
        let body = serde_json::json!({ "project_id": project_id, "path": path });
        long_agent()
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("ingest browse: {e}"))?
            .into_json()
            .map_err(|e| format!("ingest browse json: {e}"))
    }

    pub fn ingest_discover(&self, project_id: &str) -> Result<IngestState, String> {
        let url = format!("{}/api/ingest/discover", self.base_url);
        let body = serde_json::json!({ "project_id": project_id, "source_id": "" });
        long_agent()
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("ingest discover: {e}"))?
            .into_json()
            .map_err(|e| format!("ingest discover json: {e}"))
    }

    pub fn ingest_toggle(&self, project_id: &str, clip_id: &str) -> Result<IngestState, String> {
        let url = format!("{}/api/ingest/selection/toggle", self.base_url);
        let body = serde_json::json!({ "project_id": project_id, "clip_id": clip_id });
        self.agent
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("ingest toggle: {e}"))?
            .into_json()
            .map_err(|e| format!("ingest toggle json: {e}"))
    }

    pub fn ingest_select_all(&self, project_id: &str) -> Result<IngestState, String> {
        let url = format!("{}/api/ingest/selection/select-all", self.base_url);
        let body = serde_json::json!({ "project_id": project_id });
        self.agent
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("ingest select-all: {e}"))?
            .into_json()
            .map_err(|e| format!("ingest select-all json: {e}"))
    }

    pub fn ingest_import(
        &self,
        project_id: &str,
        clip_ids: &[String],
    ) -> Result<IngestState, String> {
        let url = format!("{}/api/ingest/import", self.base_url);
        let body = serde_json::json!({ "project_id": project_id, "clip_ids": clip_ids });
        long_agent()
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("ingest import: {e}"))?
            .into_json()
            .map_err(|e| format!("ingest import json: {e}"))
    }

    pub fn ingest_set_selection(
        &self,
        project_id: &str,
        selected_clip_ids: &[String],
    ) -> Result<IngestState, String> {
        let url = format!("{}/api/ingest/selection", self.base_url);
        let body = serde_json::json!({
            "project_id": project_id,
            "selected_clip_ids": selected_clip_ids,
        });
        self.agent
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("ingest selection: {e}"))?
            .into_json()
            .map_err(|e| format!("ingest selection json: {e}"))
    }

    pub fn ingest_set_archive_original(
        &self,
        project_id: &str,
        archive_original: bool,
    ) -> Result<IngestState, String> {
        let url = format!("{}/api/ingest/options", self.base_url);
        let body = serde_json::json!({
            "project_id": project_id,
            "archive_original": archive_original,
        });
        self.agent
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("ingest options: {e}"))?
            .into_json()
            .map_err(|e| format!("ingest options json: {e}"))
    }

    pub fn ingest_thumbnail_url(&self, project_id: &str, clip_id: &str) -> String {
        self.absolute(&format!(
            "/api/ingest/thumbnail?project_id={}&clip_id={}",
            urlencoding_project(project_id),
            urlencoding_project(clip_id)
        ))
    }

    pub fn story_state(&self, project_id: &str) -> Result<Value, String> {
        let url = format!(
            "{}/api/story/state?project_id={}",
            self.base_url,
            urlencoding_project(project_id)
        );
        self.agent
            .get(&url)
            .call()
            .map_err(|e| format!("story state: {e}"))?
            .into_json()
            .map_err(|e| format!("story state json: {e}"))
    }

    pub fn timeline_model(&self, project_id: &str) -> Result<TimelineModel, String> {
        let url = format!(
            "{}/api/story/timeline-model?project_id={}",
            self.base_url,
            urlencoding_project(project_id)
        );
        self.agent
            .get(&url)
            .call()
            .map_err(|e| format!("timeline: {e}"))?
            .into_json()
            .map_err(|e| format!("timeline json: {e}"))
    }

    pub fn playback_start(&self, project_id: &str) -> Result<PlaybackState, String> {
        let url = format!("{}/api/story/playback/start", self.base_url);
        let body = serde_json::json!({ "project_id": project_id });
        long_agent()
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("playback start: {e}"))?
            .into_json()
            .map_err(|e| format!("playback start json: {e}"))
    }

    pub fn story_part_create(
        &self,
        project_id: &str,
        kind: &str,
        virtual_shot_id: Option<&str>,
    ) -> Result<Value, String> {
        let url = format!("{}/api/story/part/create", self.base_url);
        let body = serde_json::json!({
            "project_id": project_id,
            "kind": kind,
            "virtual_shot_id": virtual_shot_id,
        });
        self.agent
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("part create: {e}"))?
            .into_json()
            .map_err(|e| format!("part create json: {e}"))
    }

    pub fn story_part_select(&self, project_id: &str, part_id: &str) -> Result<Value, String> {
        let url = format!("{}/api/story/part/select", self.base_url);
        let body = serde_json::json!({ "project_id": project_id, "part_id": part_id });
        self.agent
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("part select: {e}"))?
            .into_json()
            .map_err(|e| format!("part select json: {e}"))
    }

    pub fn story_part_delete(&self, project_id: &str, part_id: &str) -> Result<Value, String> {
        let url = format!("{}/api/story/part/delete", self.base_url);
        let body = serde_json::json!({ "project_id": project_id, "part_id": part_id });
        self.agent
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("part delete: {e}"))?
            .into_json()
            .map_err(|e| format!("part delete json: {e}"))
    }

    pub fn story_part_mark_in(
        &self,
        project_id: &str,
        part_id: &str,
        local_sec: f64,
    ) -> Result<Value, String> {
        let url = format!("{}/api/story/part/mark_in", self.base_url);
        let body = serde_json::json!({
            "project_id": project_id,
            "part_id": part_id,
            "local_sec": local_sec,
        });
        self.agent
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("mark in: {e}"))?
            .into_json()
            .map_err(|e| format!("mark in json: {e}"))
    }

    pub fn story_part_mark_out(
        &self,
        project_id: &str,
        part_id: &str,
        local_sec: f64,
    ) -> Result<Value, String> {
        let url = format!("{}/api/story/part/mark_out", self.base_url);
        let body = serde_json::json!({
            "project_id": project_id,
            "part_id": part_id,
            "local_sec": local_sec,
        });
        self.agent
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("mark out: {e}"))?
            .into_json()
            .map_err(|e| format!("mark out json: {e}"))
    }

    pub fn story_marker_create(
        &self,
        project_id: &str,
        timeline_sec: f64,
        part_id: &str,
    ) -> Result<Value, String> {
        let url = format!("{}/api/story/marker/create", self.base_url);
        let body = serde_json::json!({
            "project_id": project_id,
            "timeline_sec": timeline_sec,
            "part_id": part_id,
        });
        self.agent
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("marker create: {e}"))?
            .into_json()
            .map_err(|e| format!("marker create json: {e}"))
    }

    pub fn story_marker_slot_select(
        &self,
        project_id: &str,
        slot_id: &str,
    ) -> Result<Value, String> {
        let url = format!("{}/api/story/marker_slot/select", self.base_url);
        let body = serde_json::json!({ "project_id": project_id, "slot_id": slot_id });
        self.agent
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("marker slot select: {e}"))?
            .into_json()
            .map_err(|e| format!("marker slot select json: {e}"))
    }

    pub fn story_thumbnail_url(&self, project_id: &str, clip_id: &str, seek: f64) -> String {
        self.absolute(&format!(
            "/api/story/thumbnail?project_id={}&clip_id={}&seek={seek:.3}",
            urlencoding_project(project_id),
            urlencoding_project(clip_id)
        ))
    }

    pub fn story_virtual_shot_create(
        &self,
        project_id: &str,
        clip_id: &str,
        in_seconds: f64,
        out_seconds: f64,
    ) -> Result<Value, String> {
        let url = format!("{}/api/story/virtual-shot", self.base_url);
        let body = serde_json::json!({
            "project_id": project_id,
            "clip_id": clip_id,
            "in_seconds": in_seconds,
            "out_seconds": out_seconds,
        });
        self.agent
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("virtual-shot: {e}"))?
            .into_json()
            .map_err(|e| format!("virtual-shot json: {e}"))
    }

    pub fn story_cover_create(
        &self,
        project_id: &str,
        slot_id: &str,
        clip_id: Option<&str>,
        virtual_shot_id: Option<&str>,
    ) -> Result<Value, String> {
        let url = format!("{}/api/story/cover/create", self.base_url);
        let body = serde_json::json!({
            "project_id": project_id,
            "slot_id": slot_id,
            "clip_id": clip_id,
            "virtual_shot_id": virtual_shot_id,
        });
        self.agent
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("cover create: {e}"))?
            .into_json()
            .map_err(|e| format!("cover create json: {e}"))
    }

    pub fn story_cover_select(&self, project_id: &str, cover_id: &str) -> Result<Value, String> {
        let url = format!("{}/api/story/cover/select", self.base_url);
        let body = serde_json::json!({ "project_id": project_id, "cover_id": cover_id });
        self.agent
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("cover select: {e}"))?
            .into_json()
            .map_err(|e| format!("cover select json: {e}"))
    }

    pub fn story_commit(&self, project_id: &str) -> Result<Value, String> {
        let url = format!("{}/api/story/commit", self.base_url);
        let body = serde_json::json!({ "project_id": project_id });
        self.agent
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("story commit: {e}"))?
            .into_json()
            .map_err(|e| format!("story commit json: {e}"))
    }

    /// Frames for `qnc-timeline` V filmstrip (`GET /api/story/filmstrip`).
    pub fn story_filmstrip_frames(
        &self,
        project_id: &str,
        clip_id: &str,
    ) -> Result<Vec<(i64, f64, String)>, String> {
        let url = format!(
            "{}/api/story/filmstrip?project_id={}&clip_id={}",
            self.base_url,
            urlencoding_project(project_id),
            urlencoding_project(clip_id)
        );
        let v: Value = self
            .agent
            .get(&url)
            .call()
            .map_err(|e| format!("filmstrip: {e}"))?
            .into_json()
            .map_err(|e| format!("filmstrip json: {e}"))?;
        let frames = v
            .get("frames")
            .and_then(|f| f.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(frames
            .into_iter()
            .filter_map(|f| {
                let index = f.get("index").and_then(|x| x.as_i64()).unwrap_or(0);
                let seek = f.get("seek_sec").and_then(|x| x.as_f64()).unwrap_or(0.0);
                let rel = f.get("url").and_then(|x| x.as_str())?.to_string();
                Some((index, seek, self.absolute(&rel)))
            })
            .collect())
    }

    pub fn waveform_peaks(
        &self,
        project_id: &str,
        clip_id: &str,
        channel: u8,
    ) -> Result<Vec<f32>, String> {
        let url = format!(
            "{}/api/ingest/waveform/peaks?project_id={}&clip_id={}&channel={channel}",
            self.base_url,
            urlencoding_project(project_id),
            urlencoding_project(clip_id)
        );
        let v: Value = self
            .agent
            .get(&url)
            .call()
            .map_err(|e| format!("waveform peaks: {e}"))?
            .into_json()
            .map_err(|e| format!("waveform peaks json: {e}"))?;
        Ok(v.get("peaks")
            .and_then(|p| p.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_f64().map(|f| f as f32))
                    .collect()
            })
            .unwrap_or_default())
    }

    pub fn story_timeline_build(&self, project_id: &str, clip_id: &str) -> Result<Value, String> {
        let url = format!("{}/api/story/timeline/build", self.base_url);
        let body = serde_json::json!({
            "project_id": project_id,
            "clip_id": clip_id,
            "frames": 13
        });
        self.agent
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("story timeline build: {e}"))?
            .into_json()
            .map_err(|e| format!("story timeline build json: {e}"))
    }

    pub fn playback_stop(&self, session_id: &str) -> Result<(), String> {
        let url = format!("{}/api/story/playback/stop", self.base_url);
        let body = serde_json::json!({ "session_id": session_id });
        let _: Value = self
            .agent
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("playback stop: {e}"))?
            .into_json()
            .unwrap_or(Value::Null);
        Ok(())
    }

    pub fn playback_seek(&self, session_id: &str, virtual_sec: f64) -> Result<(), String> {
        let url = format!("{}/api/story/playback/seek", self.base_url);
        let body = serde_json::json!({
            "session_id": session_id,
            "virtual_sec": virtual_sec.max(0.0)
        });
        let _: Value = self
            .agent
            .post(&url)
            .send_json(body)
            .map_err(|e| format!("playback seek: {e}"))?
            .into_json()
            .unwrap_or(Value::Null);
        Ok(())
    }

    pub fn playback_state(&self, session_id: &str) -> Result<PlaybackState, String> {
        let url = format!(
            "{}/api/story/playback/state?session_id={}",
            self.base_url,
            urlencoding_project(session_id)
        );
        self.agent
            .get(&url)
            .call()
            .map_err(|e| format!("playback state: {e}"))?
            .into_json()
            .map_err(|e| format!("playback state json: {e}"))
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

    pub fn story_play_media(
        &self,
        project_id: &str,
        clip_id: &str,
    ) -> Result<StoryPlayMedia, String> {
        let url = format!(
            "{}/api/story/play-media?project_id={}&clip_id={}",
            self.base_url,
            urlencoding_project(project_id),
            urlencoding_project(clip_id)
        );
        self.agent
            .get(&url)
            .call()
            .map_err(|e| format!("play-media: {e}"))?
            .into_json()
            .map_err(|e| format!("play-media json: {e}"))
    }

    /// Legacy HTTP virtual-stream — web / remote only.
    /// Native qnc-app uses [`Self::story_play_media`] (local disk path).
    pub fn story_source_stream_url(
        &self,
        project_id: &str,
        clip_id: &str,
        in_seconds: Option<f64>,
        out_seconds: Option<f64>,
    ) -> String {
        let mut url = format!(
            "{}/api/story/virtual-stream?project_id={}&clip_id={}",
            self.base_url,
            urlencoding_project(project_id),
            urlencoding_project(clip_id)
        );
        if let (Some(in_sec), Some(out_sec)) = (in_seconds, out_seconds) {
            url.push_str(&format!(
                "&in_seconds={:.6}&out_seconds={:.6}",
                in_sec.max(0.0),
                out_sec.max(in_sec.max(0.0) + 0.000_001)
            ));
        }
        url
    }

    pub fn frame_url(&self, session_id: &str, virtual_sec: f64) -> String {
        self.absolute(&format!(
            "/api/story/playback/frame?session_id={}&virtual_sec={virtual_sec:.3}",
            urlencoding_project(session_id)
        ))
    }

    pub fn download_bytes(&self, url: &str) -> Result<Vec<u8>, String> {
        let mut reader = long_agent()
            .get(url)
            .call()
            .map_err(|e| format!("download: {e}"))?
            .into_reader();
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).map_err(|e| e.to_string())?;
        if buf.len() < 32 {
            return Err(format!("download too small ({} bytes)", buf.len()));
        }
        Ok(buf)
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
    pub status: String,
    #[serde(default)]
    pub roots: bool,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub entries: Vec<FsEntry>,
}

fn urlencoding_project(project_id: &str) -> String {
    // project ids are typically [A-Za-z0-9_]; encode minimally
    project_id
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
    pub has_audio: bool,
    #[serde(default)]
    pub audio_channels: u8,
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
    pub archive_original: bool,
    #[serde(default)]
    pub durations_pending: bool,
    #[serde(default)]
    pub import_queued: bool,
    #[serde(default)]
    pub queued: Option<u64>,
}

/// First workflow tab after Project (shell entry). Prefer ingest when present.
pub fn workflow_entry_tab(tabs: &[String]) -> String {
    if tabs.iter().any(|t| t == "ingest") {
        return "ingest".into();
    }
    tabs.iter()
        .find(|t| t.as_str() != "project")
        .cloned()
        .unwrap_or_else(|| "ingest".into())
}

/// Next tab in `workspace.tabs` after `from` (same list Project open / footer use).
pub fn workflow_next_tab(tabs: &[String], from: &str) -> Option<String> {
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
pub struct StoryPlayMedia {
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub clip_id: String,
    #[serde(default)]
    pub kind: String,
    /// Absolute local filesystem path for native ffmpeg (not an HTTP URL).
    #[serde(default)]
    pub path: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct PlaybackActive {
    #[serde(default)]
    pub layer: String,
    #[serde(default)]
    pub part_id: String,
    #[serde(default)]
    pub cover_id: String,
    #[serde(default)]
    pub clip_id: String,
    #[serde(default)]
    pub stream_url: String,
    #[serde(default)]
    pub a1_stream_url: String,
    #[serde(default)]
    pub mixed_audio_url: String,
    #[serde(default)]
    pub preview_frame_url: String,
    #[serde(default)]
    pub video_blank: bool,
    #[serde(default)]
    pub has_video: bool,
    #[serde(default)]
    pub has_audio: bool,
    #[serde(default)]
    pub audio_channels: u8,
    #[serde(default)]
    pub local_sec: f64,
    #[serde(default)]
    pub source_sec: f64,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct PlaybackState {
    pub session_id: String,
    #[serde(default)]
    pub virtual_sec: f64,
    #[serde(default)]
    pub playing: bool,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub timebase_fps: f64,
    #[serde(default)]
    pub active: PlaybackActive,
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
    pub duration_sec: f64,
    #[serde(default)]
    pub rows: Vec<String>,
    #[serde(default)]
    pub segments: Vec<TimelineSegment>,
}
