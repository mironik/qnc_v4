use crate::api::ProjectsList;
use crate::component_runtime::{ComponentBackendCommand, ComponentBackendEvent};

const COMPONENT_ID: &str = "project.registry";
const PORT_PROJECTS: &str = "projects";
const OP_LIST: &str = "list";
const REQUEST_GLOBAL: &str = "global";

pub(crate) struct ProjectRegistryComponent;

impl ProjectRegistryComponent {
    pub fn list_projects() -> ComponentBackendCommand {
        ComponentBackendCommand::get(
            COMPONENT_ID,
            PORT_PROJECTS,
            OP_LIST,
            REQUEST_GLOBAL,
            "/api/projects",
        )
    }

    pub fn accepts_event(event: &ComponentBackendEvent) -> bool {
        event.component_id == COMPONENT_ID
            && event.port_id == PORT_PROJECTS
            && event.operation_id == OP_LIST
            && event.request_key == REQUEST_GLOBAL
    }

    pub fn into_projects(event: ComponentBackendEvent) -> Option<Result<ProjectsList, String>> {
        if !Self::accepts_event(&event) {
            return None;
        }
        Some(event.result.and_then(|value| {
            serde_json::from_value(value).map_err(|e| format!("projects list: {e}"))
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::HostRequestMethod;

    #[test]
    fn list_projects_is_neutral_registry_query() {
        let command = ProjectRegistryComponent::list_projects();
        assert_eq!(command.component_id, COMPONENT_ID);
        assert_eq!(command.port_id, PORT_PROJECTS);
        assert_eq!(command.operation_id, OP_LIST);
        assert_eq!(command.request_key, REQUEST_GLOBAL);
        assert_eq!(command.method, HostRequestMethod::Get);
        assert_eq!(command.path, "/api/projects");
    }
}
