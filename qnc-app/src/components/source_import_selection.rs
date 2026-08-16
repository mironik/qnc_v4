use crate::api::{HostRequestTimeout, IngestState};
use crate::component_runtime::{ComponentBackendCommand, ComponentBackendEvent};

const COMPONENT_ID: &str = "source.import.selection";
const PORT_SELECTION: &str = "selection";
const OP_TOGGLE: &str = "toggle";

pub(crate) struct SourceImportSelectionComponent;

impl SourceImportSelectionComponent {
    pub fn toggle(project_id: &str, clip_id: &str) -> ComponentBackendCommand {
        ComponentBackendCommand::post(
            COMPONENT_ID,
            PORT_SELECTION,
            OP_TOGGLE,
            project_id,
            "/api/ingest/selection/toggle",
            serde_json::json!({ "project_id": project_id, "clip_id": clip_id }),
        )
        .with_timeout(HostRequestTimeout::Default)
    }

    pub fn accepts_event(event: &ComponentBackendEvent) -> bool {
        event.component_id == COMPONENT_ID
            && event.port_id == PORT_SELECTION
            && event.operation_id == OP_TOGGLE
    }

    pub fn into_state(event: ComponentBackendEvent) -> Option<Result<IngestState, String>> {
        if !Self::accepts_event(&event) {
            return None;
        }
        Some(
            event.result.and_then(|value| {
                serde_json::from_value(value).map_err(|e| format!("selection: {e}"))
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::HostRequestMethod;
    use crate::component_runtime::ComponentResultPolicy;

    #[test]
    fn toggle_command_is_project_scoped_latest_state() {
        let command = SourceImportSelectionComponent::toggle("p1", "clip-a");
        assert_eq!(command.component_id, COMPONENT_ID);
        assert_eq!(command.port_id, PORT_SELECTION);
        assert_eq!(command.operation_id, OP_TOGGLE);
        assert_eq!(command.request_key, "p1");
        assert_eq!(command.method, HostRequestMethod::Post);
        assert_eq!(command.result_policy, ComponentResultPolicy::LatestWins);
        assert_eq!(command.timeout, HostRequestTimeout::Default);
    }
}
