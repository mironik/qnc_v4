use crate::api::{HostRequestTimeout, IngestState};
use crate::component_runtime::{ComponentBackendCommand, ComponentBackendEvent};

const COMPONENT_ID: &str = "source.import.command";
const PORT_BROWSE: &str = "browse";
const PORT_DISCOVER: &str = "discover";
const PORT_SELECTION: &str = "selection";
const PORT_OPTIONS: &str = "options";
const PORT_IMPORT: &str = "import";
const OP_BROWSE: &str = "browse";
const OP_DISCOVER: &str = "discover";
const OP_SELECT_ALL: &str = "selection.select_all";
const OP_CLEAR_SELECTION: &str = "selection.clear";
const OP_SET_ARCHIVE: &str = "options.archive_original";
const OP_IMPORT_SELECTED: &str = "import.selected";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceImportCommandKind {
    Browse,
    Discover,
    SelectAll,
    ClearSelection,
    SetArchive,
    ImportSelected,
}

impl SourceImportCommandKind {
    fn from_operation(operation_id: &str) -> Option<Self> {
        match operation_id {
            OP_BROWSE => Some(Self::Browse),
            OP_DISCOVER => Some(Self::Discover),
            OP_SELECT_ALL => Some(Self::SelectAll),
            OP_CLEAR_SELECTION => Some(Self::ClearSelection),
            OP_SET_ARCHIVE => Some(Self::SetArchive),
            OP_IMPORT_SELECTED => Some(Self::ImportSelected),
            _ => None,
        }
    }
}

pub(crate) struct SourceImportCommandComponent;

impl SourceImportCommandComponent {
    pub fn browse(project_id: &str, path: &str) -> ComponentBackendCommand {
        ComponentBackendCommand::post(
            COMPONENT_ID,
            PORT_BROWSE,
            OP_BROWSE,
            project_id,
            "/api/ingest/browse",
            serde_json::json!({ "project_id": project_id, "path": path }),
        )
        .with_timeout(HostRequestTimeout::Long)
    }

    pub fn discover(project_id: &str) -> ComponentBackendCommand {
        ComponentBackendCommand::post(
            COMPONENT_ID,
            PORT_DISCOVER,
            OP_DISCOVER,
            project_id,
            "/api/ingest/discover",
            serde_json::json!({ "project_id": project_id, "source_id": "" }),
        )
        .with_timeout(HostRequestTimeout::Long)
    }

    pub fn select_all(project_id: &str) -> ComponentBackendCommand {
        ComponentBackendCommand::post(
            COMPONENT_ID,
            PORT_SELECTION,
            OP_SELECT_ALL,
            project_id,
            "/api/ingest/selection/select-all",
            serde_json::json!({ "project_id": project_id }),
        )
    }

    pub fn clear_selection(project_id: &str) -> ComponentBackendCommand {
        ComponentBackendCommand::post(
            COMPONENT_ID,
            PORT_SELECTION,
            OP_CLEAR_SELECTION,
            project_id,
            "/api/ingest/selection",
            serde_json::json!({ "project_id": project_id, "selected_clip_ids": [] }),
        )
    }

    pub fn set_archive_original(
        project_id: &str,
        archive_original: bool,
    ) -> ComponentBackendCommand {
        ComponentBackendCommand::post(
            COMPONENT_ID,
            PORT_OPTIONS,
            OP_SET_ARCHIVE,
            project_id,
            "/api/ingest/options",
            serde_json::json!({
                "project_id": project_id,
                "archive_original": archive_original,
            }),
        )
    }

    pub fn import_selected(project_id: &str, clip_ids: &[String]) -> ComponentBackendCommand {
        ComponentBackendCommand::post(
            COMPONENT_ID,
            PORT_IMPORT,
            OP_IMPORT_SELECTED,
            project_id,
            "/api/ingest/import",
            serde_json::json!({ "project_id": project_id, "clip_ids": clip_ids }),
        )
        .with_timeout(HostRequestTimeout::Long)
    }

    pub fn accepts_event(event: &ComponentBackendEvent) -> bool {
        event.component_id == COMPONENT_ID
            && matches!(
                event.port_id.as_str(),
                PORT_BROWSE | PORT_DISCOVER | PORT_SELECTION | PORT_OPTIONS | PORT_IMPORT
            )
            && SourceImportCommandKind::from_operation(&event.operation_id).is_some()
    }

    pub fn into_state(
        event: ComponentBackendEvent,
    ) -> Option<(SourceImportCommandKind, Result<IngestState, String>)> {
        if !Self::accepts_event(&event) {
            return None;
        }
        let kind = SourceImportCommandKind::from_operation(&event.operation_id)?;
        let result = event.result.and_then(|value| {
            serde_json::from_value(value).map_err(|e| format!("source import command: {e}"))
        });
        Some((kind, result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::HostRequestMethod;
    use crate::component_runtime::ComponentResultPolicy;

    #[test]
    fn browse_command_is_neutral_long_operation() {
        let command = SourceImportCommandComponent::browse("p1", "C:\\card");
        assert_eq!(command.component_id, COMPONENT_ID);
        assert_eq!(command.port_id, PORT_BROWSE);
        assert_eq!(command.operation_id, OP_BROWSE);
        assert_eq!(command.request_key, "p1");
        assert_eq!(command.method, HostRequestMethod::Post);
        assert_eq!(command.timeout, HostRequestTimeout::Long);
    }

    #[test]
    fn import_selected_supersedes_older_project_commands() {
        let command = SourceImportCommandComponent::import_selected("p1", &[]);
        assert_eq!(command.operation_id, OP_IMPORT_SELECTED);
        assert_eq!(command.timeout, HostRequestTimeout::Long);
        assert_eq!(command.result_policy, ComponentResultPolicy::LatestWins);
    }

    #[test]
    fn archive_and_import_use_independent_ports() {
        let archive = SourceImportCommandComponent::set_archive_original("p1", true);
        let import = SourceImportCommandComponent::import_selected("p1", &[]);
        assert_eq!(archive.port_id, PORT_OPTIONS);
        assert_eq!(import.port_id, PORT_IMPORT);
        assert_ne!(archive.port_id, import.port_id);
    }

    #[test]
    fn import_selected_sends_explicit_clip_intent() {
        let command = SourceImportCommandComponent::import_selected(
            "p1",
            &["clip_a".into(), "clip_b".into()],
        );
        let payload = command.payload.expect("payload");
        let ids = payload["clip_ids"].as_array().expect("clip_ids");
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0], "clip_a");
        assert_eq!(ids[1], "clip_b");
    }
}
