use crate::api::{self, HostRequestTimeout, IngestState};
use crate::component_runtime::{ComponentBackendCommand, ComponentBackendEvent};

const COMPONENT_ID: &str = "source.import.state";
const PORT_STATE: &str = "state";
const OP_LOAD: &str = "load";
const OP_POLL: &str = "poll";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceImportStateKind {
    Load,
    Poll,
}

impl SourceImportStateKind {
    fn from_operation(operation_id: &str) -> Option<Self> {
        match operation_id {
            OP_LOAD => Some(Self::Load),
            OP_POLL => Some(Self::Poll),
            _ => None,
        }
    }
}

pub(crate) struct SourceImportStateComponent;

impl SourceImportStateComponent {
    pub fn load(project_id: &str) -> ComponentBackendCommand {
        ComponentBackendCommand::get(
            COMPONENT_ID,
            PORT_STATE,
            OP_LOAD,
            project_id,
            format!(
                "/api/ingest/state?project_id={}",
                api::encode_query_value(project_id)
            ),
        )
        .with_timeout(HostRequestTimeout::Default)
    }

    pub fn poll(project_id: &str) -> ComponentBackendCommand {
        ComponentBackendCommand::get(
            COMPONENT_ID,
            PORT_STATE,
            OP_POLL,
            project_id,
            format!(
                "/api/ingest/state?project_id={}",
                api::encode_query_value(project_id)
            ),
        )
        .with_timeout(HostRequestTimeout::Default)
    }

    pub fn accepts_event(event: &ComponentBackendEvent) -> bool {
        event.component_id == COMPONENT_ID
            && event.port_id == PORT_STATE
            && SourceImportStateKind::from_operation(&event.operation_id).is_some()
    }

    pub fn into_state(
        event: ComponentBackendEvent,
    ) -> Option<(SourceImportStateKind, Result<IngestState, String>)> {
        if !Self::accepts_event(&event) {
            return None;
        }
        let kind = SourceImportStateKind::from_operation(&event.operation_id)?;
        let result = event.result.and_then(|value| {
            serde_json::from_value(value).map_err(|e| format!("source import state: {e}"))
        });
        Some((kind, result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::HostRequestMethod;

    #[test]
    fn state_load_command_is_project_scoped_query() {
        let command = SourceImportStateComponent::load("p1");
        assert_eq!(command.component_id, COMPONENT_ID);
        assert_eq!(command.port_id, PORT_STATE);
        assert_eq!(command.operation_id, OP_LOAD);
        assert_eq!(command.request_key, "p1");
        assert_eq!(command.method, HostRequestMethod::Get);
        assert!(command.path.contains("/api/ingest/state?project_id=p1"));
    }

    #[test]
    fn state_poll_uses_same_query_with_distinct_operation() {
        let command = SourceImportStateComponent::poll("p1");
        assert_eq!(command.component_id, COMPONENT_ID);
        assert_eq!(command.port_id, PORT_STATE);
        assert_eq!(command.operation_id, OP_POLL);
        assert_eq!(command.request_key, "p1");
        assert_eq!(command.method, HostRequestMethod::Get);
        assert!(command.path.contains("/api/ingest/state?project_id=p1"));
    }
}
