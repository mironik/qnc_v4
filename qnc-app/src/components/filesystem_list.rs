use crate::api::{self, FsList, HostRequestTimeout};
use crate::component_runtime::{ComponentBackendCommand, ComponentBackendEvent};

const COMPONENT_ID: &str = "filesystem.list";
const PORT_LISTING: &str = "listing";
const OP_LOAD: &str = "load";

pub(crate) struct FilesystemListComponent;

impl FilesystemListComponent {
    pub fn load(instance_id: impl Into<String>, path: &str) -> ComponentBackendCommand {
        ComponentBackendCommand::get(
            COMPONENT_ID,
            PORT_LISTING,
            OP_LOAD,
            instance_id,
            fs_list_path(path),
        )
        .with_timeout(HostRequestTimeout::Long)
    }

    pub fn event_instance(event: &ComponentBackendEvent) -> Option<&str> {
        if event.component_id == COMPONENT_ID
            && event.port_id == PORT_LISTING
            && event.operation_id == OP_LOAD
        {
            Some(event.request_key.as_str())
        } else {
            None
        }
    }

    pub fn into_listing(event: ComponentBackendEvent) -> Option<(String, Result<FsList, String>)> {
        let instance_id = Self::event_instance(&event)?.to_string();
        let result = event
            .result
            .and_then(|value| serde_json::from_value(value).map_err(|e| format!("fs-list: {e}")));
        Some((instance_id, result))
    }
}

fn fs_list_path(path: &str) -> String {
    if path.trim().is_empty() {
        "/api/shell/fs-list".into()
    } else {
        format!("/api/shell/fs-list?path={}", api::encode_query_value(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::HostRequestMethod;

    #[test]
    fn fs_list_command_is_neutral_and_long_running() {
        let command = FilesystemListComponent::load("settings.storage.projects_root", "C:\\QNC");
        assert_eq!(command.component_id, COMPONENT_ID);
        assert_eq!(command.port_id, PORT_LISTING);
        assert_eq!(command.operation_id, OP_LOAD);
        assert_eq!(command.request_key, "settings.storage.projects_root");
        assert_eq!(command.method, HostRequestMethod::Get);
        assert_eq!(command.timeout, HostRequestTimeout::Long);
        assert!(command.path.starts_with("/api/shell/fs-list?path="));
        assert!(command.payload.is_none());
    }
}
