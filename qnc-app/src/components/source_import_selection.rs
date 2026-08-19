use crate::api::{HostRequestTimeout, IngestState};
use crate::component_runtime::{ComponentBackendCommand, ComponentBackendEvent};

const COMPONENT_ID: &str = "source.import.selection";
const PORT_SELECTION: &str = "selection";
const OP_SET: &str = "set";

pub(crate) struct SourceImportSelectionComponent;

impl SourceImportSelectionComponent {
    pub fn set(project_id: &str, selected_clip_ids: &[String]) -> ComponentBackendCommand {
        ComponentBackendCommand::post(
            COMPONENT_ID,
            PORT_SELECTION,
            OP_SET,
            project_id,
            "/api/ingest/selection",
            serde_json::json!({
                "project_id": project_id,
                "selected_clip_ids": selected_clip_ids,
            }),
        )
        .with_timeout(HostRequestTimeout::Long)
    }

    pub fn accepts_event(event: &ComponentBackendEvent) -> bool {
        event.component_id == COMPONENT_ID
            && event.port_id == PORT_SELECTION
            && event.operation_id == OP_SET
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
    fn set_command_is_project_scoped_latest_state() {
        let command = SourceImportSelectionComponent::set(
            "p1",
            &["clip-a".to_string(), "clip-b".to_string()],
        );
        assert_eq!(command.component_id, COMPONENT_ID);
        assert_eq!(command.port_id, PORT_SELECTION);
        assert_eq!(command.operation_id, OP_SET);
        assert_eq!(command.request_key, "p1");
        assert_eq!(command.method, HostRequestMethod::Post);
        assert_eq!(command.path, "/api/ingest/selection");
        let payload = command.payload.as_ref().expect("payload");
        let ids = payload["selected_clip_ids"]
            .as_array()
            .expect("selected_clip_ids");
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0], "clip-a");
        assert_eq!(ids[1], "clip-b");
        assert_eq!(command.result_policy, ComponentResultPolicy::LatestWins);
        assert_eq!(command.timeout, HostRequestTimeout::Long);
    }
}
