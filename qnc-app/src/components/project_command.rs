use serde_json::{json, Value};

use crate::api::{HostRequestTimeout, ProjectRow, ProjectsList, TemplateRow};
use crate::component_runtime::{ComponentBackendCommand, ComponentBackendEvent};

const COMPONENT_ID: &str = "project.command";
const PORT_MUTATION: &str = "mutation";
const REQUEST_ACTIVE_PROJECT: &str = "active-project";
const REQUEST_SEP: char = '\u{1f}';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectCommandKind {
    OpenProject,
    CreateFromTemplate,
    DeleteProjects,
    SaveUiState,
    DeleteUserTemplate,
    CreateUserTemplate,
}

impl ProjectCommandKind {
    fn operation_id(self) -> &'static str {
        match self {
            Self::OpenProject => "project.open",
            Self::CreateFromTemplate => "project.create_from_template",
            Self::DeleteProjects => "project.delete",
            Self::SaveUiState => "ui_state.save",
            Self::DeleteUserTemplate => "template.delete",
            Self::CreateUserTemplate => "template.create",
        }
    }

    fn from_operation_id(operation_id: &str) -> Option<Self> {
        Some(match operation_id {
            "project.open" => Self::OpenProject,
            "project.create_from_template" => Self::CreateFromTemplate,
            "project.delete" => Self::DeleteProjects,
            "ui_state.save" => Self::SaveUiState,
            "template.delete" => Self::DeleteUserTemplate,
            "template.create" => Self::CreateUserTemplate,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ProjectCommandData {
    OpenProject {
        project: ProjectRow,
        active_project_id: String,
    },
    CreatedProject {
        project: ProjectRow,
        active_project_id: String,
    },
    ProjectsDeleted(ProjectsList),
    UiState(Value),
    TemplateDeleted {
        templates: Vec<TemplateRow>,
        ui_state: Value,
    },
    TemplateCreated(TemplateRow),
}

pub(crate) struct ProjectCommandComponent;

impl ProjectCommandComponent {
    pub fn open_project(_request_id: u64, project_id: &str) -> ComponentBackendCommand {
        ComponentBackendCommand::post(
            COMPONENT_ID,
            PORT_MUTATION,
            ProjectCommandKind::OpenProject.operation_id(),
            REQUEST_ACTIVE_PROJECT,
            "/api/projects/open",
            json!({ "project_id": project_id }),
        )
    }

    pub fn create_from_template(
        request_id: u64,
        name: &str,
        template_id: &str,
    ) -> ComponentBackendCommand {
        Self::post(
            request_id,
            ProjectCommandKind::CreateFromTemplate,
            name,
            "/api/projects/from-template",
            json!({
                "name": name,
                "template_id": template_id,
            }),
        )
        .with_timeout(HostRequestTimeout::Long)
    }

    pub fn delete_projects(
        request_id: u64,
        project_ids: &[String],
        detail: &str,
    ) -> ComponentBackendCommand {
        Self::post(
            request_id,
            ProjectCommandKind::DeleteProjects,
            detail,
            "/api/projects/delete",
            json!({ "project_ids": project_ids }),
        )
        .with_timeout(HostRequestTimeout::Long)
    }

    pub fn save_ui_state(request_id: u64, detail: &str, patch: Value) -> ComponentBackendCommand {
        Self::post(
            request_id,
            ProjectCommandKind::SaveUiState,
            detail,
            "/api/projects/ui-state",
            patch,
        )
    }

    pub fn save_settings_path(
        request_id: u64,
        detail: &str,
        path: &str,
        value: Value,
    ) -> ComponentBackendCommand {
        Self::save_ui_state(
            request_id,
            detail,
            json!({ "settings_path": { "path": path, "value": value } }),
        )
    }

    pub fn merge_settings_override(
        request_id: u64,
        detail: &str,
        patch: Value,
    ) -> ComponentBackendCommand {
        Self::save_ui_state(request_id, detail, json!({ "settings_override": patch }))
    }

    pub fn delete_user_template(request_id: u64, template_id: &str) -> ComponentBackendCommand {
        Self::post(
            request_id,
            ProjectCommandKind::DeleteUserTemplate,
            template_id,
            "/api/project-templates/delete",
            json!({ "template_id": template_id }),
        )
    }

    pub fn create_user_template(
        request_id: u64,
        name: &str,
        description: &str,
        base_template_id: &str,
        settings: Option<Value>,
        source_template_ids: &[String],
    ) -> ComponentBackendCommand {
        Self::post(
            request_id,
            ProjectCommandKind::CreateUserTemplate,
            name,
            "/api/project-templates",
            json!({
                "name": name,
                "description": description,
                "base_template_id": base_template_id,
                "settings": settings,
                "source_template_ids": source_template_ids,
            }),
        )
    }

    pub fn accepts_event(event: &ComponentBackendEvent) -> bool {
        event.component_id == COMPONENT_ID
            && event.port_id == PORT_MUTATION
            && ProjectCommandKind::from_operation_id(&event.operation_id).is_some()
    }

    pub fn into_data(
        event: ComponentBackendEvent,
    ) -> Option<(
        ProjectCommandKind,
        String,
        Result<ProjectCommandData, String>,
    )> {
        if !Self::accepts_event(&event) {
            return None;
        }
        let kind = ProjectCommandKind::from_operation_id(&event.operation_id)?;
        let (_request_id, detail) = split_request_key(&event.request_key);
        Some((
            kind,
            detail.to_string(),
            event.result.and_then(|value| parse_data(kind, value)),
        ))
    }

    fn post(
        request_id: u64,
        kind: ProjectCommandKind,
        detail: &str,
        path: &str,
        payload: Value,
    ) -> ComponentBackendCommand {
        ComponentBackendCommand::post(
            COMPONENT_ID,
            PORT_MUTATION,
            kind.operation_id(),
            join_request_key(request_id, detail),
            path,
            payload,
        )
    }
}

fn parse_data(kind: ProjectCommandKind, value: Value) -> Result<ProjectCommandData, String> {
    match kind {
        ProjectCommandKind::OpenProject => {
            ensure_ok(&value, "open")?;
            let project = parse_field(&value, "project", "open project")?;
            let active_project_id = value
                .get("active_project_id")
                .and_then(|x| x.as_str())
                .unwrap_or_else(|| project_id_fallback(&project))
                .to_string();
            Ok(ProjectCommandData::OpenProject {
                project,
                active_project_id,
            })
        }
        ProjectCommandKind::CreateFromTemplate => {
            ensure_ok(&value, "from-template")?;
            let project: ProjectRow = parse_field(&value, "project", "from-template project")?;
            let active_project_id = value
                .get("active_project_id")
                .and_then(|x| x.as_str())
                .unwrap_or(&project.project_id)
                .to_string();
            Ok(ProjectCommandData::CreatedProject {
                project,
                active_project_id,
            })
        }
        ProjectCommandKind::DeleteProjects => {
            ensure_ok(&value, "delete")?;
            Ok(ProjectCommandData::ProjectsDeleted(ProjectsList {
                status: "ok".into(),
                active_project_id: value
                    .get("active_project_id")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                projects: value
                    .get("projects")
                    .cloned()
                    .and_then(|p| serde_json::from_value(p).ok())
                    .unwrap_or_default(),
            }))
        }
        ProjectCommandKind::SaveUiState => Ok(ProjectCommandData::UiState(
            value
                .get("ui_state")
                .cloned()
                .ok_or_else(|| "ui-state save missing".to_string())?,
        )),
        ProjectCommandKind::DeleteUserTemplate => {
            ensure_ok(&value, "delete template")?;
            Ok(ProjectCommandData::TemplateDeleted {
                templates: value
                    .get("templates")
                    .cloned()
                    .and_then(|t| serde_json::from_value(t).ok())
                    .unwrap_or_default(),
                ui_state: value.get("ui_state").cloned().unwrap_or(Value::Null),
            })
        }
        ProjectCommandKind::CreateUserTemplate => {
            ensure_ok(&value, "create template")?;
            Ok(ProjectCommandData::TemplateCreated(parse_field(
                &value,
                "template",
                "create template",
            )?))
        }
    }
}

fn ensure_ok(value: &Value, label: &str) -> Result<(), String> {
    if value.get("status").and_then(|s| s.as_str()) == Some("ok") {
        return Ok(());
    }
    Err(format!(
        "{label}: {}",
        value
            .get("detail")
            .or_else(|| value.get("message"))
            .cloned()
            .unwrap_or_else(|| value.clone())
    ))
}

fn parse_field<T: serde::de::DeserializeOwned>(
    value: &Value,
    key: &str,
    label: &str,
) -> Result<T, String> {
    let field = value
        .get(key)
        .cloned()
        .ok_or_else(|| format!("{label}: missing {key}"))?;
    serde_json::from_value(field).map_err(|e| format!("{label}: {e}"))
}

fn project_id_fallback(project: &ProjectRow) -> &str {
    &project.project_id
}

fn join_request_key(request_id: u64, detail: &str) -> String {
    format!("{request_id}{REQUEST_SEP}{detail}")
}

fn split_request_key(request_key: &str) -> (&str, &str) {
    request_key
        .split_once(REQUEST_SEP)
        .unwrap_or((request_key, ""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::HostRequestMethod;

    #[test]
    fn open_project_uses_active_project_latest_wins_scope() {
        let a = ProjectCommandComponent::open_project(1, "project-a");
        let b = ProjectCommandComponent::open_project(2, "project-b");
        assert_eq!(a.request_key, REQUEST_ACTIVE_PROJECT);
        assert_eq!(b.request_key, REQUEST_ACTIVE_PROJECT);
        assert_eq!(
            a.operation_id,
            ProjectCommandKind::OpenProject.operation_id()
        );
    }

    #[test]
    fn create_from_template_is_long_project_command() {
        let command = ProjectCommandComponent::create_from_template(7, "Demo", "default");
        assert_eq!(command.component_id, COMPONENT_ID);
        assert_eq!(command.port_id, PORT_MUTATION);
        assert_eq!(
            command.operation_id,
            ProjectCommandKind::CreateFromTemplate.operation_id()
        );
        assert_eq!(command.method, HostRequestMethod::Post);
        assert_eq!(command.timeout, HostRequestTimeout::Long);
    }

    #[test]
    fn save_settings_path_wraps_payload() {
        let command = ProjectCommandComponent::save_settings_path(
            8,
            "export_dir:C:/out",
            "export.directory",
            Value::String("C:/out".into()),
        );
        assert_eq!(command.request_key, "8\u{1f}export_dir:C:/out");
        assert_eq!(
            command.payload,
            Some(json!({
                "settings_path": {
                    "path": "export.directory",
                    "value": "C:/out"
                }
            }))
        );
    }
}
