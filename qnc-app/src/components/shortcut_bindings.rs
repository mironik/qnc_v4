use serde_json::Value;

use crate::component_runtime::{ComponentBackendCommand, ComponentBackendEvent};

const COMPONENT_ID: &str = "shortcut.bindings";
const OP_LOAD: &str = "load";
const PORT_CATALOG: &str = "catalog";
const PORT_USER: &str = "user";
const REQUEST_SEP: char = '\u{1f}';

#[derive(Debug, Clone)]
pub(crate) enum ShortcutBindingsData {
    Catalog {
        instance_id: String,
        scope: String,
        catalog: Value,
    },
    User {
        instance_id: String,
        scope: String,
        user: Value,
    },
}

pub(crate) struct ShortcutBindingsComponent;

impl ShortcutBindingsComponent {
    pub fn load_catalog(instance_id: &str, scope: &str) -> ComponentBackendCommand {
        ComponentBackendCommand::get(
            COMPONENT_ID,
            PORT_CATALOG,
            OP_LOAD,
            request_key(instance_id, scope),
            "/api/shell/keyboard-shortcuts",
        )
    }

    pub fn load_user(instance_id: &str, scope: &str) -> ComponentBackendCommand {
        ComponentBackendCommand::get(
            COMPONENT_ID,
            PORT_USER,
            OP_LOAD,
            request_key(instance_id, scope),
            "/api/settings/keyboard-shortcuts",
        )
    }

    pub fn load_all(instance_id: &str, scope: &str) -> [ComponentBackendCommand; 2] {
        [
            Self::load_catalog(instance_id, scope),
            Self::load_user(instance_id, scope),
        ]
    }

    pub fn accepts_event(event: &ComponentBackendEvent) -> bool {
        event.component_id == COMPONENT_ID
            && event.operation_id == OP_LOAD
            && matches!(event.port_id.as_str(), PORT_CATALOG | PORT_USER)
    }

    pub fn into_data(
        event: ComponentBackendEvent,
    ) -> Option<(
        String,
        String,
        &'static str,
        Result<ShortcutBindingsData, String>,
    )> {
        if !Self::accepts_event(&event) {
            return None;
        }
        let (instance_id, scope) = split_request_key(&event.request_key)
            .unwrap_or_else(|| (String::new(), event.request_key.clone()));
        let port = match event.port_id.as_str() {
            PORT_CATALOG => PORT_CATALOG,
            PORT_USER => PORT_USER,
            _ => return None,
        };
        let result = event
            .result
            .map(|value| parse_data(port, &instance_id, &scope, value));
        Some((instance_id, scope, port, result))
    }
}

fn parse_data(port_id: &str, instance_id: &str, scope: &str, value: Value) -> ShortcutBindingsData {
    match port_id {
        PORT_CATALOG => ShortcutBindingsData::Catalog {
            instance_id: instance_id.to_string(),
            scope: scope.to_string(),
            catalog: value,
        },
        PORT_USER => ShortcutBindingsData::User {
            instance_id: instance_id.to_string(),
            scope: scope.to_string(),
            user: value,
        },
        _ => unreachable!("accepts_event filters shortcut binding ports"),
    }
}

fn request_key(instance_id: &str, scope: &str) -> String {
    format!("{instance_id}{REQUEST_SEP}{scope}")
}

fn split_request_key(value: &str) -> Option<(String, String)> {
    let (instance_id, scope) = value.split_once(REQUEST_SEP)?;
    Some((instance_id.to_string(), scope.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::HostRequestMethod;

    #[test]
    fn load_all_uses_independent_ports_for_latest_wins() {
        let commands = ShortcutBindingsComponent::load_all("story", "storyboard");
        let ports: std::collections::BTreeSet<_> = commands
            .iter()
            .map(|command| command.port_id.as_str())
            .collect();
        assert_eq!(commands.len(), 2);
        assert_eq!(ports.len(), 2);
        assert!(commands.iter().all(|command| {
            command.component_id == COMPONENT_ID
                && command.operation_id == OP_LOAD
                && command.request_key == request_key("story", "storyboard")
                && command.method == HostRequestMethod::Get
        }));
    }

    #[test]
    fn request_key_keeps_instance_and_scope_separate() {
        let key = request_key("media_assist", "storyboard");
        assert_eq!(
            split_request_key(&key),
            Some(("media_assist".to_string(), "storyboard".to_string()))
        );
    }
}
