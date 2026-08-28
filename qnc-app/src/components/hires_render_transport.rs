use qnc_service_contracts::{ExportHiResSubmitResponse, PreviewHiResInputResponse};
use serde::Deserialize;
use serde_json::json;

use crate::api::HostRequestTimeout;
use crate::component_runtime::{ComponentBackendCommand, ComponentBackendEvent};

const COMPONENT_ID: &str = "hires.render_transport";
const PORT_SUBMIT: &str = "submit";
const PORT_STATUS: &str = "status";
const OP_SUBMIT: &str = "export_hires.submit";
const OP_STATUS: &str = "export_hires.status";
const OP_PREVIEW_SUBMIT: &str = "preview_hires_input.build";
const REQUEST_SEP: char = '\u{1f}';

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExportHiResStatus {
    pub job_id: String,
    pub status: String,
    pub error: String,
}

pub(crate) struct HiResRenderTransportComponent;

impl HiResRenderTransportComponent {
    pub fn submit(instance_id: &str, request_id: u64, project_id: &str) -> ComponentBackendCommand {
        ComponentBackendCommand::post(
            COMPONENT_ID,
            PORT_SUBMIT,
            OP_SUBMIT,
            request_key(instance_id, project_id, request_id),
            "/api/render/hires/submit",
            json!({ "project_id": project_id }),
        )
        .with_timeout(HostRequestTimeout::Long)
    }

    pub fn status(
        instance_id: &str,
        request_id: u64,
        project_id: &str,
        job_id: &str,
    ) -> ComponentBackendCommand {
        ComponentBackendCommand::get(
            COMPONENT_ID,
            PORT_STATUS,
            OP_STATUS,
            status_request_key(instance_id, project_id, job_id, request_id),
            format!(
                "/api/render/hires/status?project_id={}&job_id={}",
                crate::api::encode_query_value(project_id),
                crate::api::encode_query_value(job_id)
            ),
        )
        .with_timeout(HostRequestTimeout::Default)
    }

    pub fn submit_preview(
        instance_id: &str,
        request_id: u64,
        project_id: &str,
    ) -> ComponentBackendCommand {
        ComponentBackendCommand::post(
            COMPONENT_ID,
            PORT_SUBMIT,
            OP_PREVIEW_SUBMIT,
            request_key(instance_id, project_id, request_id),
            "/api/preview/hires/input/build",
            json!({ "project_id": project_id }),
        )
        .with_timeout(HostRequestTimeout::Long)
    }

    pub fn accepts_event(event: &ComponentBackendEvent) -> bool {
        event.component_id == COMPONENT_ID
            && event.port_id == PORT_SUBMIT
            && event.operation_id == OP_SUBMIT
    }

    pub fn accepts_status_event(event: &ComponentBackendEvent) -> bool {
        event.component_id == COMPONENT_ID
            && event.port_id == PORT_STATUS
            && event.operation_id == OP_STATUS
    }

    pub fn accepts_preview_event(event: &ComponentBackendEvent) -> bool {
        event.component_id == COMPONENT_ID
            && event.port_id == PORT_SUBMIT
            && event.operation_id == OP_PREVIEW_SUBMIT
    }

    pub fn into_submit(
        event: ComponentBackendEvent,
    ) -> Option<(String, String, Result<ExportHiResSubmitResponse, String>)> {
        if !Self::accepts_event(&event) {
            return None;
        }
        let (instance_id, project_id, _request_id) = split_request_key(&event.request_key)
            .unwrap_or_else(|| (String::new(), event.request_key.clone(), 0));
        let result = event
            .result
            .and_then(|value| serde_json::from_value(value).map_err(|error| error.to_string()));
        Some((instance_id, project_id, result))
    }

    pub fn into_preview_submit(
        event: ComponentBackendEvent,
    ) -> Option<(String, String, Result<PreviewHiResInputResponse, String>)> {
        if !Self::accepts_preview_event(&event) {
            return None;
        }
        let (instance_id, project_id, _request_id) = split_request_key(&event.request_key)
            .unwrap_or_else(|| (String::new(), event.request_key.clone(), 0));
        let result = event
            .result
            .and_then(|value| serde_json::from_value(value).map_err(|error| error.to_string()));
        Some((instance_id, project_id, result))
    }

    pub fn into_status(
        event: ComponentBackendEvent,
    ) -> Option<(String, String, Result<ExportHiResStatus, String>)> {
        if !Self::accepts_status_event(&event) {
            return None;
        }
        let (instance_id, project_id, job_id, _request_id) =
            split_status_request_key(&event.request_key)
                .unwrap_or_else(|| (String::new(), event.request_key.clone(), String::new(), 0));
        let result = event
            .result
            .and_then(|value| status_from_value(value, &job_id));
        Some((instance_id, project_id, result))
    }
}

fn request_key(instance_id: &str, project_id: &str, request_id: u64) -> String {
    format!("{instance_id}{REQUEST_SEP}{project_id}{REQUEST_SEP}{request_id}")
}

fn status_request_key(
    instance_id: &str,
    project_id: &str,
    job_id: &str,
    request_id: u64,
) -> String {
    format!("{instance_id}{REQUEST_SEP}{project_id}{REQUEST_SEP}{job_id}{REQUEST_SEP}{request_id}")
}

fn split_request_key(value: &str) -> Option<(String, String, u64)> {
    let mut parts = value.splitn(3, REQUEST_SEP);
    let instance_id = parts.next()?.to_string();
    let project_id = parts.next()?.to_string();
    let request_id = parts.next()?.parse().ok()?;
    Some((instance_id, project_id, request_id))
}

fn split_status_request_key(value: &str) -> Option<(String, String, String, u64)> {
    let mut parts = value.splitn(4, REQUEST_SEP);
    let instance_id = parts.next()?.to_string();
    let project_id = parts.next()?.to_string();
    let job_id = parts.next()?.to_string();
    let request_id = parts.next()?.parse().ok()?;
    Some((instance_id, project_id, job_id, request_id))
}

#[derive(Debug, Deserialize)]
struct IngestJobsState {
    #[serde(default)]
    jobs: Vec<IngestJobRow>,
}

#[derive(Debug, Deserialize)]
struct IngestJobRow {
    #[serde(default)]
    job_id: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    error: String,
}

fn status_from_value(value: serde_json::Value, job_id: &str) -> Result<ExportHiResStatus, String> {
    if value.get("jobs").is_none() {
        let row: IngestJobRow = serde_json::from_value(value).map_err(|error| error.to_string())?;
        if row.job_id != job_id {
            return Err(format!("Export HI-res job nije pronađen: {job_id}"));
        }
        return Ok(ExportHiResStatus {
            job_id: row.job_id,
            status: row.status,
            error: row.error,
        });
    }
    let state: IngestJobsState =
        serde_json::from_value(value).map_err(|error| error.to_string())?;
    let row = state
        .jobs
        .into_iter()
        .find(|row| row.job_id == job_id)
        .ok_or_else(|| format!("Export HI-res job nije pronađen: {job_id}"))?;
    Ok(ExportHiResStatus {
        job_id: row.job_id,
        status: row.status,
        error: row.error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::HostRequestMethod;

    #[test]
    fn submit_uses_hires_export_endpoint() {
        let command = HiResRenderTransportComponent::submit("story", 7, "p1");
        assert_eq!(command.component_id, COMPONENT_ID);
        assert_eq!(command.port_id, PORT_SUBMIT);
        assert_eq!(command.method, HostRequestMethod::Post);
        assert_eq!(command.path, "/api/render/hires/submit");
        assert_eq!(command.timeout, HostRequestTimeout::Long);
    }

    #[test]
    fn submit_preview_uses_hires_preview_endpoint() {
        let command = HiResRenderTransportComponent::submit_preview("story", 7, "p1");
        assert_eq!(command.component_id, COMPONENT_ID);
        assert_eq!(command.port_id, PORT_SUBMIT);
        assert_eq!(command.operation_id, OP_PREVIEW_SUBMIT);
        assert_eq!(command.method, HostRequestMethod::Post);
        assert_eq!(command.path, "/api/preview/hires/input/build");
        assert_eq!(command.timeout, HostRequestTimeout::Long);
    }

    #[test]
    fn status_uses_hires_render_status_endpoint() {
        let command = HiResRenderTransportComponent::status("story", 8, "p 1", "job:1");

        assert_eq!(command.component_id, COMPONENT_ID);
        assert_eq!(command.port_id, PORT_STATUS);
        assert_eq!(command.operation_id, OP_STATUS);
        assert!(command
            .path
            .contains("/api/render/hires/status?project_id=p%201&job_id=job%3A1"));
        assert_eq!(
            split_status_request_key(&command.request_key),
            Some(("story".into(), "p 1".into(), "job:1".into(), 8))
        );
    }

    #[test]
    fn status_extracts_matching_export_job() {
        assert_eq!(
            status_from_value(
                json!({
                    "job_id": "job:1",
                    "status": "processing",
                    "error": ""
                }),
                "job:1"
            )
            .unwrap(),
            ExportHiResStatus {
                job_id: "job:1".into(),
                status: "processing".into(),
                error: String::new(),
            }
        );
    }
}
