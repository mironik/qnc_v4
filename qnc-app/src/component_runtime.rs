//! Neutral component command runtime.
//!
//! A form is only a layout of components. Components emit backend commands with
//! IDs and ports; this runtime executes the operation and returns an event to
//! the same component key. It does not know any form or future layout.

use std::collections::HashMap;
use std::sync::{
    mpsc::{self, Receiver, Sender},
    Arc, Mutex,
};
use std::thread;

use eframe::egui;
use serde_json::Value;

use crate::api::{HostClient, HostRequestMethod, HostRequestTimeout};

const DEFAULT_WORKER_COUNT: usize = 4;
type LatestMap = Arc<Mutex<HashMap<String, u64>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComponentResultPolicy {
    LatestWins,
}

#[derive(Debug, Clone)]
pub(crate) struct ComponentBackendCommand {
    pub component_id: String,
    pub port_id: String,
    pub operation_id: String,
    pub request_key: String,
    pub method: HostRequestMethod,
    pub path: String,
    pub payload: Option<Value>,
    pub timeout: HostRequestTimeout,
    pub result_policy: ComponentResultPolicy,
}

impl ComponentBackendCommand {
    pub fn get(
        component_id: impl Into<String>,
        port_id: impl Into<String>,
        operation_id: impl Into<String>,
        request_key: impl Into<String>,
        path: impl Into<String>,
    ) -> Self {
        Self {
            component_id: component_id.into(),
            port_id: port_id.into(),
            operation_id: operation_id.into(),
            request_key: request_key.into(),
            method: HostRequestMethod::Get,
            path: path.into(),
            payload: None,
            timeout: HostRequestTimeout::Default,
            result_policy: ComponentResultPolicy::LatestWins,
        }
    }

    pub fn post(
        component_id: impl Into<String>,
        port_id: impl Into<String>,
        operation_id: impl Into<String>,
        request_key: impl Into<String>,
        path: impl Into<String>,
        payload: Value,
    ) -> Self {
        Self {
            component_id: component_id.into(),
            port_id: port_id.into(),
            operation_id: operation_id.into(),
            request_key: request_key.into(),
            method: HostRequestMethod::Post,
            path: path.into(),
            payload: Some(payload),
            timeout: HostRequestTimeout::Default,
            result_policy: ComponentResultPolicy::LatestWins,
        }
    }

    pub fn with_timeout(mut self, timeout: HostRequestTimeout) -> Self {
        self.timeout = timeout;
        self
    }

    fn correlation_key(&self) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{}",
            self.component_id, self.port_id, self.request_key
        )
    }
}

#[derive(Debug)]
pub(crate) struct ComponentBackendEvent {
    pub sequence: u64,
    pub component_id: String,
    pub port_id: String,
    pub operation_id: String,
    pub request_key: String,
    pub result: Result<Value, String>,
    correlation_key: String,
}

struct ComponentBackendJob {
    host: HostClient,
    command: ComponentBackendCommand,
    event: ComponentBackendEvent,
    repaint: Option<egui::Context>,
}

pub(crate) struct ComponentBackendRuntime {
    job_tx: Sender<ComponentBackendJob>,
    rx: Receiver<ComponentBackendEvent>,
    latest: LatestMap,
    next_sequence: u64,
}

impl ComponentBackendRuntime {
    pub fn new() -> Self {
        let (event_tx, rx) = mpsc::channel();
        let (job_tx, job_rx) = mpsc::channel();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let latest = Arc::new(Mutex::new(HashMap::new()));
        for index in 0..DEFAULT_WORKER_COUNT {
            spawn_component_worker(
                index,
                Arc::clone(&job_rx),
                event_tx.clone(),
                Arc::clone(&latest),
            );
        }
        Self {
            job_tx,
            rx,
            latest,
            next_sequence: 1,
        }
    }

    pub fn submit(
        &mut self,
        host: &HostClient,
        command: ComponentBackendCommand,
        repaint: Option<egui::Context>,
    ) -> Result<u64, String> {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);

        let correlation_key = command.correlation_key();
        if command.result_policy == ComponentResultPolicy::LatestWins {
            self.latest
                .lock()
                .map_err(|_| "component backend latest state unavailable".to_string())?
                .insert(correlation_key.clone(), sequence);
        }

        let host = host.clone();
        let event = ComponentBackendEvent {
            sequence,
            component_id: command.component_id.clone(),
            port_id: command.port_id.clone(),
            operation_id: command.operation_id.clone(),
            request_key: command.request_key.clone(),
            result: Ok(Value::Null),
            correlation_key: correlation_key.clone(),
        };
        self.job_tx
            .send(ComponentBackendJob {
                host,
                command,
                event,
                repaint,
            })
            .map_err(|err| {
                if let Ok(mut latest) = self.latest.lock() {
                    if latest.get(&correlation_key) == Some(&sequence) {
                        latest.remove(&correlation_key);
                    }
                }
                format!("component backend queue: {err}")
            })?;

        Ok(sequence)
    }

    pub fn poll(&mut self) -> Vec<ComponentBackendEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.rx.try_recv() {
            let is_latest = self
                .latest
                .lock()
                .map(|mut latest| {
                    if latest.get(&event.correlation_key) == Some(&event.sequence) {
                        latest.remove(&event.correlation_key);
                        true
                    } else {
                        false
                    }
                })
                .unwrap_or(false);
            if is_latest {
                events.push(event);
            }
        }
        events
    }
}

fn spawn_component_worker(
    index: usize,
    job_rx: Arc<Mutex<Receiver<ComponentBackendJob>>>,
    event_tx: Sender<ComponentBackendEvent>,
    latest: LatestMap,
) {
    if let Err(err) = thread::Builder::new()
        .name(format!("qnc-component-backend-worker-{index}"))
        .spawn(move || loop {
            let job = {
                let Ok(rx) = job_rx.lock() else {
                    break;
                };
                rx.recv()
            };
            match job {
                Ok(job) => execute_component_job(job, &event_tx, &latest),
                Err(_) => break,
            }
        })
    {
        eprintln!("qnc-app: component backend worker spawn failed: {err}");
    }
}

fn execute_component_job(
    job: ComponentBackendJob,
    event_tx: &Sender<ComponentBackendEvent>,
    latest: &LatestMap,
) {
    let ComponentBackendJob {
        host,
        command,
        event,
        repaint,
    } = job;
    if is_stale_event(latest, &event) {
        return;
    }
    let result = host.request_json(
        command.method,
        &command.path,
        command.payload,
        command.timeout,
    );
    if is_stale_event(latest, &event) {
        return;
    }
    let _ = event_tx.send(ComponentBackendEvent { result, ..event });
    if let Some(ctx) = repaint {
        ctx.request_repaint();
    }
}

fn is_stale_event(latest: &LatestMap, event: &ComponentBackendEvent) -> bool {
    latest
        .lock()
        .map(|latest| latest.get(&event.correlation_key) != Some(&event.sequence))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correlation_key_is_component_port_request_scoped() {
        let a = ComponentBackendCommand::get("media.card", "state", "load", "clip-1", "/x");
        let b = ComponentBackendCommand::get("media.card", "state", "load", "clip-2", "/x");
        let c = ComponentBackendCommand::get("media.card", "thumb", "load", "clip-1", "/x");
        assert_ne!(a.correlation_key(), b.correlation_key());
        assert_ne!(a.correlation_key(), c.correlation_key());
    }

    #[test]
    fn post_command_keeps_payload_and_defaults_to_latest_wins() {
        let command = ComponentBackendCommand::post(
            "theme.picker",
            "settings",
            "appearance.save",
            "active-theme",
            "/api/settings/appearance",
            serde_json::json!({ "theme_id": "dark" }),
        );
        assert_eq!(command.method, HostRequestMethod::Post);
        assert_eq!(command.result_policy, ComponentResultPolicy::LatestWins);
        assert!(command.payload.is_some());
    }

    #[test]
    fn stale_event_detection_rejects_older_sequence() {
        let command = ComponentBackendCommand::get("catalog", "items", "load", "global", "/x");
        let correlation_key = command.correlation_key();
        let latest = Arc::new(Mutex::new(HashMap::from([(correlation_key.clone(), 2)])));
        let stale = ComponentBackendEvent {
            sequence: 1,
            component_id: command.component_id.clone(),
            port_id: command.port_id.clone(),
            operation_id: command.operation_id.clone(),
            request_key: command.request_key.clone(),
            result: Ok(Value::Null),
            correlation_key: correlation_key.clone(),
        };
        let current = ComponentBackendEvent {
            sequence: 2,
            component_id: command.component_id,
            port_id: command.port_id,
            operation_id: command.operation_id,
            request_key: command.request_key,
            result: Ok(Value::Null),
            correlation_key,
        };
        assert!(is_stale_event(&latest, &stale));
        assert!(!is_stale_event(&latest, &current));
    }
}
