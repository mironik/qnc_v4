use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use crate::api::{self, HostRequestTimeout, IngestClip, IngestState};
use crate::component_runtime::{ComponentBackendCommand, ComponentBackendEvent};

const COMPONENT_ID: &str = "source.import.status";
const PORT_STATUS: &str = "status";
const OP_POLL: &str = "poll";
const POLL_INTERVAL: Duration = Duration::from_millis(900);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceImportStatusUpdate {
    pub project_id: String,
    pub changed: bool,
    pub pending_count: usize,
    pub imported_count: usize,
    pub was_pending: bool,
}

impl SourceImportStatusUpdate {
    pub fn completed(&self) -> bool {
        self.was_pending && self.pending_count == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceImportStatusSnapshot {
    project_id: String,
    fingerprint: u64,
    pending_count: usize,
    imported_count: usize,
}

impl SourceImportStatusSnapshot {
    fn from_state(state: &IngestState) -> Self {
        let mut rows: Vec<_> = state.clips.iter().collect();
        rows.sort_by(|a, b| a.clip_id.cmp(&b.clip_id));

        let mut hasher = DefaultHasher::new();
        state.project_id.hash(&mut hasher);
        for clip in &rows {
            clip.clip_id.hash(&mut hasher);
            clip.import_status.hash(&mut hasher);
            clip.status_proxy.hash(&mut hasher);
            clip.status_original.hash(&mut hasher);
            clip.proxy_status.hash(&mut hasher);
            clip.proxy_path.hash(&mut hasher);
            clip.project_proxy_path.hash(&mut hasher);
        }

        Self {
            project_id: state.project_id.clone(),
            fingerprint: hasher.finish(),
            pending_count: rows.iter().filter(|clip| import_pending(clip)).count(),
            imported_count: rows.iter().filter(|clip| import_ready(clip)).count(),
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct SourceImportStatusComponent {
    project_id: String,
    busy: bool,
    last_poll: Option<Instant>,
    last_fingerprint: Option<u64>,
    pending_count: usize,
}

impl SourceImportStatusComponent {
    pub fn poll(project_id: &str) -> ComponentBackendCommand {
        ComponentBackendCommand::get(
            COMPONENT_ID,
            PORT_STATUS,
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
            && event.port_id == PORT_STATUS
            && event.operation_id == OP_POLL
    }

    pub fn into_state(event: ComponentBackendEvent) -> Option<Result<IngestState, String>> {
        if !Self::accepts_event(&event) {
            return None;
        }
        Some(event.result.and_then(|value| {
            serde_json::from_value(value).map_err(|e| format!("source import status: {e}"))
        }))
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn watch_project(&mut self, project_id: &str) {
        let project_id = project_id.trim();
        if project_id.is_empty() {
            self.reset();
            return;
        }
        if self.project_id != project_id {
            self.project_id = project_id.to_string();
            self.busy = false;
            self.last_poll = None;
            self.last_fingerprint = None;
            self.pending_count = 0;
        }
    }

    pub fn mark_possible_work(&mut self, project_id: &str) {
        self.watch_project(project_id);
        if !self.project_id.is_empty() {
            self.pending_count = self.pending_count.max(1);
            self.last_poll = None;
        }
    }

    pub fn begin_poll(&mut self, project_id: &str) {
        self.watch_project(project_id);
        if self.project_id.is_empty() {
            return;
        }
        self.busy = true;
        self.last_poll = Some(Instant::now());
    }

    pub fn set_error(&mut self) {
        self.busy = false;
        self.last_poll = Some(Instant::now());
    }

    pub fn should_request_poll(&self) -> bool {
        if self.busy || self.project_id.trim().is_empty() {
            return false;
        }
        let due = self
            .last_poll
            .map(|last| last.elapsed() >= POLL_INTERVAL)
            .unwrap_or(true);
        due && (self.pending_count > 0 || self.last_fingerprint.is_none())
    }

    pub fn needs_repaint(&self) -> bool {
        self.busy || self.pending_count > 0
    }

    pub fn apply_state(&mut self, state: &IngestState) -> SourceImportStatusUpdate {
        let snapshot = SourceImportStatusSnapshot::from_state(state);
        self.watch_project(&snapshot.project_id);
        let was_pending = self.pending_count > 0;
        let changed = self
            .last_fingerprint
            .map(|fingerprint| fingerprint != snapshot.fingerprint)
            .unwrap_or(true);
        self.busy = false;
        self.last_poll = Some(Instant::now());
        self.last_fingerprint = Some(snapshot.fingerprint);
        self.pending_count = snapshot.pending_count;
        SourceImportStatusUpdate {
            project_id: snapshot.project_id,
            changed,
            pending_count: snapshot.pending_count,
            imported_count: snapshot.imported_count,
            was_pending,
        }
    }
}

fn import_pending(clip: &IngestClip) -> bool {
    matches!(
        clip.import_status.as_str(),
        "queued" | "processing" | "generating_proxy" | "original_ready"
    )
}

fn import_ready(clip: &IngestClip) -> bool {
    matches!(
        clip.import_status.as_str(),
        "imported" | "done" | "ready" | "proxy_ready"
    ) || clip.status_proxy == "ready"
        || clip.proxy_status == "ready"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_status(project_id: &str, statuses: &[(&str, &str, &str)]) -> IngestState {
        IngestState {
            project_id: project_id.to_string(),
            clips: statuses
                .iter()
                .map(|(clip_id, import_status, status_proxy)| IngestClip {
                    clip_id: (*clip_id).to_string(),
                    import_status: (*import_status).to_string(),
                    status_proxy: (*status_proxy).to_string(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn poll_command_is_status_component_scoped() {
        let command = SourceImportStatusComponent::poll("p1");

        assert_eq!(command.component_id, COMPONENT_ID);
        assert_eq!(command.port_id, PORT_STATUS);
        assert_eq!(command.operation_id, OP_POLL);
        assert_eq!(command.request_key, "p1");
        assert!(command.path.contains("/api/ingest/state?project_id=p1"));
    }

    #[test]
    fn watcher_polls_initially_and_while_import_is_pending() {
        let mut watcher = SourceImportStatusComponent::default();
        watcher.watch_project("p1");

        assert!(watcher.should_request_poll());
        watcher.begin_poll("p1");
        assert!(!watcher.should_request_poll());

        let update = watcher.apply_state(&state_with_status(
            "p1",
            &[("a", "queued", "pending"), ("b", "imported", "ready")],
        ));

        assert!(update.changed);
        assert_eq!(update.pending_count, 1);
        assert_eq!(update.imported_count, 1);
        assert!(!watcher.should_request_poll());
    }

    #[test]
    fn completed_update_is_reported_when_pending_reaches_zero() {
        let mut watcher = SourceImportStatusComponent::default();
        watcher.mark_possible_work("p1");

        let update = watcher.apply_state(&state_with_status("p1", &[("a", "imported", "ready")]));

        assert!(update.completed());
        assert_eq!(update.pending_count, 0);
    }

    #[test]
    fn fingerprint_changes_when_dot_status_changes() {
        let a = SourceImportStatusSnapshot::from_state(&state_with_status(
            "p1",
            &[("a", "queued", "pending")],
        ));
        let b = SourceImportStatusSnapshot::from_state(&state_with_status(
            "p1",
            &[("a", "imported", "ready")],
        ));

        assert_ne!(a.fingerprint, b.fingerprint);
    }
}
