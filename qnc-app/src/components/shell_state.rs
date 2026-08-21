use serde_json::{json, Value};

use crate::api::{self, Health, Workspace};
use crate::component_runtime::{ComponentBackendCommand, ComponentBackendEvent};
use crate::qnc_theme::ThemeId;

const COMPONENT_ID: &str = "shell.state";
const OP_LOAD: &str = "load";
const OP_SET: &str = "set";
const PORT_HEALTH: &str = "health";
const PORT_RUNTIME: &str = "runtime";
const PORT_APPEARANCE: &str = "appearance";
const PORT_WORKSPACE: &str = "workspace";
const PORT_BACKGROUND_PLAYBACK: &str = "background_playback";
const REQUEST_GLOBAL: &str = "global";

#[derive(Debug, Clone)]
pub(crate) enum ShellStateData {
    Health(Health),
    Runtime(Value),
    Appearance(ThemeId),
    Workspace {
        project_id: String,
        workspace: Workspace,
    },
}

pub(crate) struct ShellStateComponent;

impl ShellStateComponent {
    pub fn health() -> ComponentBackendCommand {
        ComponentBackendCommand::get(
            COMPONENT_ID,
            PORT_HEALTH,
            OP_LOAD,
            REQUEST_GLOBAL,
            "/api/health",
        )
    }

    pub fn runtime() -> ComponentBackendCommand {
        ComponentBackendCommand::get(
            COMPONENT_ID,
            PORT_RUNTIME,
            OP_LOAD,
            REQUEST_GLOBAL,
            "/api/shell/runtime",
        )
    }

    pub fn appearance() -> ComponentBackendCommand {
        ComponentBackendCommand::get(
            COMPONENT_ID,
            PORT_APPEARANCE,
            OP_LOAD,
            REQUEST_GLOBAL,
            "/api/settings/appearance",
        )
    }

    pub fn workspace(project_id: &str) -> ComponentBackendCommand {
        ComponentBackendCommand::get(
            COMPONENT_ID,
            PORT_WORKSPACE,
            OP_LOAD,
            project_id,
            format!(
                "/api/projects/{}/workspace",
                api::encode_query_value(project_id)
            ),
        )
    }

    pub fn background_playback(active: bool) -> ComponentBackendCommand {
        ComponentBackendCommand::post(
            COMPONENT_ID,
            PORT_BACKGROUND_PLAYBACK,
            OP_SET,
            REQUEST_GLOBAL,
            "/api/shell/background/playback",
            json!({ "active": active }),
        )
    }

    pub fn accepts_event(event: &ComponentBackendEvent) -> bool {
        event.component_id == COMPONENT_ID
            && event.operation_id == OP_LOAD
            && matches!(
                event.port_id.as_str(),
                PORT_HEALTH | PORT_RUNTIME | PORT_APPEARANCE | PORT_WORKSPACE
            )
    }

    pub fn accepts_background_event(event: &ComponentBackendEvent) -> bool {
        event.component_id == COMPONENT_ID
            && event.operation_id == OP_SET
            && event.port_id == PORT_BACKGROUND_PLAYBACK
    }

    pub fn into_background_result(event: ComponentBackendEvent) -> Option<Result<(), String>> {
        if !Self::accepts_background_event(&event) {
            return None;
        }
        Some(event.result.map(|_| ()))
    }

    pub fn into_data(event: ComponentBackendEvent) -> Option<Result<ShellStateData, String>> {
        if !Self::accepts_event(&event) {
            return None;
        }
        let port_id = event.port_id.clone();
        let request_key = event.request_key.clone();
        Some(
            event
                .result
                .and_then(|value| parse_data(&port_id, &request_key, value)),
        )
    }
}

fn parse_data(port_id: &str, request_key: &str, value: Value) -> Result<ShellStateData, String> {
    match port_id {
        PORT_HEALTH => serde_json::from_value(value)
            .map(ShellStateData::Health)
            .map_err(|e| format!("health: {e}")),
        PORT_RUNTIME => Ok(ShellStateData::Runtime(value)),
        PORT_APPEARANCE => {
            let id = value
                .get("user")
                .and_then(|u| u.get("theme_id"))
                .or_else(|| value.get("theme_id"))
                .and_then(|x| x.as_str())
                .and_then(ThemeId::parse)
                .unwrap_or_default();
            Ok(ShellStateData::Appearance(id))
        }
        PORT_WORKSPACE => {
            if value.get("status").and_then(|s| s.as_str()) != Some("ok") {
                return Err(format!(
                    "workspace status={}",
                    value
                        .get("status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("unknown")
                ));
            }
            let workspace = value
                .get("workspace")
                .cloned()
                .ok_or_else(|| "workspace missing".to_string())
                .and_then(|v| serde_json::from_value(v).map_err(|e| format!("workspace: {e}")))?;
            Ok(ShellStateData::Workspace {
                project_id: request_key.to_string(),
                workspace,
            })
        }
        _ => Err(format!("unknown shell state port: {port_id}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::HostRequestMethod;

    #[test]
    fn workspace_request_is_project_scoped() {
        let command = ShellStateComponent::workspace("p1");
        assert_eq!(command.component_id, COMPONENT_ID);
        assert_eq!(command.port_id, PORT_WORKSPACE);
        assert_eq!(command.operation_id, OP_LOAD);
        assert_eq!(command.request_key, "p1");
        assert_eq!(command.method, HostRequestMethod::Get);
    }
}
