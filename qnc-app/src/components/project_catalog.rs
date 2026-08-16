use serde_json::Value;

use crate::api::{default_keyboard_presets, KeyboardPresetRow, ModuleRow, TemplateRow};
use crate::component_runtime::{ComponentBackendCommand, ComponentBackendEvent};
use crate::project_pts;

const COMPONENT_ID: &str = "project.catalog";
const REQUEST_GLOBAL: &str = "global";
const OP_LOAD: &str = "load";
const PORT_TEMPLATES: &str = "templates";
const PORT_MODULES: &str = "modules";
const PORT_KEYBOARD_PRESETS: &str = "keyboard.presets";
const PORT_DEFAULT_ROOT: &str = "projects.default_root";
const PORT_UI_STATE: &str = "ui.state";

#[derive(Debug, Clone)]
pub(crate) enum ProjectCatalogData {
    Templates(Vec<TemplateRow>),
    Modules(Vec<ModuleRow>),
    KeyboardPresets(Vec<KeyboardPresetRow>),
    DefaultProjectsRoot(String),
    UiState(Value),
}

pub(crate) struct ProjectCatalogComponent;

impl ProjectCatalogComponent {
    pub fn load_templates() -> ComponentBackendCommand {
        Self::load(PORT_TEMPLATES, "/api/project-templates")
    }

    pub fn load_modules() -> ComponentBackendCommand {
        Self::load(PORT_MODULES, "/api/modules")
    }

    pub fn load_keyboard_presets() -> ComponentBackendCommand {
        Self::load(
            PORT_KEYBOARD_PRESETS,
            "/api/settings/keyboard-shortcuts/presets",
        )
    }

    pub fn load_default_projects_root() -> ComponentBackendCommand {
        Self::load(PORT_DEFAULT_ROOT, "/api/shell/projects-root")
    }

    pub fn load_ui_state() -> ComponentBackendCommand {
        Self::load(PORT_UI_STATE, "/api/projects/ui-state")
    }

    pub fn load_all() -> [ComponentBackendCommand; 5] {
        [
            Self::load_templates(),
            Self::load_modules(),
            Self::load_keyboard_presets(),
            Self::load_default_projects_root(),
            Self::load_ui_state(),
        ]
    }

    pub fn accepts_event(event: &ComponentBackendEvent) -> bool {
        event.component_id == COMPONENT_ID
            && event.operation_id == OP_LOAD
            && matches!(
                event.port_id.as_str(),
                PORT_TEMPLATES
                    | PORT_MODULES
                    | PORT_KEYBOARD_PRESETS
                    | PORT_DEFAULT_ROOT
                    | PORT_UI_STATE
            )
    }

    pub fn into_data(event: ComponentBackendEvent) -> Option<Result<ProjectCatalogData, String>> {
        if !Self::accepts_event(&event) {
            return None;
        }
        let port_id = event.port_id.clone();
        Some(event.result.and_then(|value| parse_data(&port_id, value)))
    }

    fn load(port_id: &'static str, path: &'static str) -> ComponentBackendCommand {
        ComponentBackendCommand::get(COMPONENT_ID, port_id, OP_LOAD, REQUEST_GLOBAL, path)
    }
}

fn parse_data(port_id: &str, value: Value) -> Result<ProjectCatalogData, String> {
    match port_id {
        PORT_TEMPLATES => {
            let templates = value
                .get("templates")
                .cloned()
                .and_then(|t| serde_json::from_value(t).ok())
                .unwrap_or_default();
            Ok(ProjectCatalogData::Templates(templates))
        }
        PORT_MODULES => {
            let mut modules: Vec<ModuleRow> = value
                .get("modules")
                .cloned()
                .and_then(|m| serde_json::from_value(m).ok())
                .unwrap_or_default();
            modules.sort_by_key(project_pts::module_sort_key);
            Ok(ProjectCatalogData::Modules(modules))
        }
        PORT_KEYBOARD_PRESETS => {
            let presets: Vec<KeyboardPresetRow> = value
                .get("presets")
                .cloned()
                .and_then(|p| serde_json::from_value(p).ok())
                .unwrap_or_else(default_keyboard_presets);
            Ok(ProjectCatalogData::KeyboardPresets(if presets.is_empty() {
                default_keyboard_presets()
            } else {
                presets
            }))
        }
        PORT_DEFAULT_ROOT => Ok(ProjectCatalogData::DefaultProjectsRoot(
            value
                .get("projects_root")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
        )),
        PORT_UI_STATE => Ok(ProjectCatalogData::UiState(
            value
                .get("ui_state")
                .cloned()
                .ok_or_else(|| "ui-state missing".to_string())?,
        )),
        _ => Err(format!("unknown project catalog port: {port_id}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::HostRequestMethod;

    #[test]
    fn load_all_uses_independent_ports_for_latest_wins() {
        let commands = ProjectCatalogComponent::load_all();
        let ports: std::collections::BTreeSet<_> = commands
            .iter()
            .map(|command| command.port_id.as_str())
            .collect();
        assert_eq!(commands.len(), 5);
        assert_eq!(ports.len(), 5);
        assert!(commands.iter().all(|command| {
            command.component_id == COMPONENT_ID
                && command.operation_id == OP_LOAD
                && command.request_key == REQUEST_GLOBAL
                && command.method == HostRequestMethod::Get
        }));
    }

    #[test]
    fn keyboard_presets_fall_back_when_host_returns_empty() {
        let data = parse_data(PORT_KEYBOARD_PRESETS, serde_json::json!({ "presets": [] }))
            .expect("keyboard presets");
        let ProjectCatalogData::KeyboardPresets(presets) = data else {
            panic!("unexpected data");
        };
        assert!(!presets.is_empty());
        assert_eq!(presets[0].id, "default");
    }
}
