//! Global component error boundary.
//!
//! This records component backend errors by neutral envelope key. It does not
//! decide which form should display the error; forms still own their narrow UI
//! projection. The boundary gives the app one place to inspect the latest
//! component failure and clear it when the same component key succeeds.

use std::collections::HashMap;

use crate::component_runtime::{ComponentBackendCommand, ComponentBackendEvent};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ComponentErrorKey {
    component_id: String,
    port_id: String,
    operation_id: String,
    request_key: String,
}

impl ComponentErrorKey {
    pub fn from_command(command: &ComponentBackendCommand) -> Self {
        Self {
            component_id: command.component_id.clone(),
            port_id: command.port_id.clone(),
            operation_id: command.operation_id.clone(),
            request_key: command.request_key.clone(),
        }
    }

    pub fn from_event(event: &ComponentBackendEvent) -> Self {
        Self {
            component_id: event.component_id.clone(),
            port_id: event.port_id.clone(),
            operation_id: event.operation_id.clone(),
            request_key: event.request_key.clone(),
        }
    }

    #[cfg(test)]
    fn label(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.component_id, self.port_id, self.operation_id, self.request_key
        )
    }
}

#[derive(Debug, Clone)]
struct ComponentErrorRecord {
    key: ComponentErrorKey,
    message: String,
}

#[derive(Debug, Default)]
pub(crate) struct ComponentErrorBoundary {
    active: HashMap<ComponentErrorKey, String>,
    last: Option<ComponentErrorRecord>,
}

impl ComponentErrorBoundary {
    pub fn record(&mut self, key: ComponentErrorKey, message: impl Into<String>) {
        let message = message.into();
        self.active.insert(key.clone(), message.clone());
        self.last = Some(ComponentErrorRecord { key, message });
    }

    pub fn clear(&mut self, key: &ComponentErrorKey) {
        self.active.remove(key);
        if self
            .last
            .as_ref()
            .map(|record| &record.key == key)
            .unwrap_or(false)
        {
            self.last = None;
        }
    }

    pub fn last_message(&self) -> Option<String> {
        let record = self.last.as_ref()?;
        Some(record.message.clone())
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.active.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component_runtime::ComponentBackendCommand;

    #[test]
    fn boundary_records_and_clears_by_component_key() {
        let command = ComponentBackendCommand::get("catalog", "items", "load", "global", "/x");
        let key = ComponentErrorKey::from_command(&command);
        let mut boundary = ComponentErrorBoundary::default();
        boundary.record(key.clone(), "failed");
        assert_eq!(boundary.len(), 1);
        assert_eq!(boundary.last_message().unwrap(), "failed");
        boundary.clear(&key);
        assert_eq!(boundary.len(), 0);
        assert!(boundary.last_message().is_none());
    }

    #[test]
    fn user_message_does_not_expose_internal_request_key() {
        let command = ComponentBackendCommand::get(
            "editorial.edit",
            "mutation",
            "part.delete",
            "story\u{1f}project\u{1f}1\u{1f}part_abc",
            "/x",
        );
        let key = ComponentErrorKey::from_command(&command);
        let mut boundary = ComponentErrorBoundary::default();
        boundary.record(key.clone(), "delete failed");

        assert!(key.label().contains("editorial.edit:mutation:part.delete"));
        assert_eq!(boundary.last_message().unwrap(), "delete failed");
    }
}
