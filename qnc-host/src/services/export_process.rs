use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use async_trait::async_trait;
use qnc_service_contracts::{ExportEngine, ExportJob, ExportRequest, ServiceError, ServiceResult};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

const PROTOCOL_VERSION: &str = "qnc_export_process_v1";

#[derive(Debug, Clone)]
pub struct ExternalProcessExportEngine {
    command: String,
    request_root: PathBuf,
}

impl ExternalProcessExportEngine {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            request_root: std::env::temp_dir().join("qnc_export_jobs"),
        }
    }

    #[cfg(test)]
    fn with_request_root(command: impl Into<String>, request_root: PathBuf) -> Self {
        Self {
            command: command.into(),
            request_root,
        }
    }
}

#[async_trait]
impl ExportEngine for ExternalProcessExportEngine {
    async fn submit(&self, request: ExportRequest) -> ServiceResult<ExportJob> {
        let command = self.command.clone();
        let request_root = self.request_root.clone();
        let job_id = format!("export_{}", Uuid::new_v4().simple());
        tokio::task::spawn_blocking(move || {
            submit_export_process(&command, &request_root, &job_id, request)
        })
        .await
        .map_err(join_error)?
    }

    async fn status(&self, job_id: &str) -> ServiceResult<ExportJob> {
        let command = self.command.clone();
        let job_id = require_job_id(job_id)?;
        tokio::task::spawn_blocking(move || status_export_process(&command, &job_id))
            .await
            .map_err(join_error)?
    }

    async fn cancel(&self, job_id: &str) -> ServiceResult<()> {
        let command = self.command.clone();
        let job_id = require_job_id(job_id)?;
        tokio::task::spawn_blocking(move || cancel_export_process(&command, &job_id))
            .await
            .map_err(join_error)?
    }
}

#[derive(Serialize)]
struct ExportSubmitEnvelope {
    protocol_version: &'static str,
    job_id: String,
    request: ExportRequest,
}

fn submit_export_process(
    command: &str,
    request_root: &Path,
    job_id: &str,
    request: ExportRequest,
) -> ServiceResult<ExportJob> {
    let job_dir = request_root.join(job_id);
    fs::create_dir_all(&job_dir).map_err(|error| {
        service_error(
            "export_request_write_failed",
            format!("Cannot create export request directory: {error}"),
        )
    })?;
    let request_path = job_dir.join("request.json");
    let envelope = ExportSubmitEnvelope {
        protocol_version: PROTOCOL_VERSION,
        job_id: job_id.to_string(),
        request,
    };
    let raw = serde_json::to_vec_pretty(&envelope).map_err(|error| {
        service_error(
            "export_request_encode_failed",
            format!("Cannot encode export request: {error}"),
        )
    })?;
    fs::write(&request_path, raw).map_err(|error| {
        service_error(
            "export_request_write_failed",
            format!("Cannot write export request: {error}"),
        )
    })?;

    let output = run_export_command(
        command,
        vec![
            OsString::from("submit"),
            OsString::from("--request"),
            request_path.clone().into_os_string(),
            OsString::from("--job-id"),
            OsString::from(job_id),
        ],
        vec![
            ("QNC_EXPORT_PROTOCOL", OsString::from(PROTOCOL_VERSION)),
            (
                "QNC_EXPORT_REQUEST_PATH",
                request_path.clone().into_os_string(),
            ),
            ("QNC_EXPORT_JOB_ID", OsString::from(job_id)),
        ],
    )?;
    let job = parse_job_response("submit", &output.stdout)?;
    ensure_job_id(job_id, job)
}

fn status_export_process(command: &str, job_id: &str) -> ServiceResult<ExportJob> {
    let output = run_export_command(
        command,
        vec![
            OsString::from("status"),
            OsString::from("--job-id"),
            OsString::from(job_id),
        ],
        vec![
            ("QNC_EXPORT_PROTOCOL", OsString::from(PROTOCOL_VERSION)),
            ("QNC_EXPORT_JOB_ID", OsString::from(job_id)),
        ],
    )?;
    let job = parse_job_response("status", &output.stdout)?;
    ensure_job_id(job_id, job)
}

fn cancel_export_process(command: &str, job_id: &str) -> ServiceResult<()> {
    run_export_command(
        command,
        vec![
            OsString::from("cancel"),
            OsString::from("--job-id"),
            OsString::from(job_id),
        ],
        vec![
            ("QNC_EXPORT_PROTOCOL", OsString::from(PROTOCOL_VERSION)),
            ("QNC_EXPORT_JOB_ID", OsString::from(job_id)),
        ],
    )?;
    Ok(())
}

fn run_export_command(
    command_line: &str,
    args: Vec<OsString>,
    envs: Vec<(&'static str, OsString)>,
) -> ServiceResult<Output> {
    let mut parts = split_command_line(command_line).map_err(|message| {
        service_error(
            "export_command_invalid",
            format!("Invalid export command: {message}"),
        )
    })?;
    if parts.is_empty() {
        return Err(service_error(
            "export_command_required",
            "Export backend external_process requires [export].command.",
        ));
    }
    let program = parts.remove(0);
    let output = Command::new(program)
        .args(parts)
        .args(args)
        .envs(envs)
        .output()
        .map_err(|error| {
            service_error(
                "export_process_spawn_failed",
                format!("Cannot start export process: {error}"),
            )
        })?;
    if !output.status.success() {
        return Err(service_error(
            "export_process_failed",
            format!(
                "Export process failed with status {}. stderr: {} stdout: {}",
                output.status,
                output_text(&output.stderr),
                output_text(&output.stdout)
            ),
        ));
    }
    Ok(output)
}

fn parse_job_response(action: &str, stdout: &[u8]) -> ServiceResult<ExportJob> {
    let raw = output_text(stdout);
    if raw.trim().is_empty() {
        return Err(service_error(
            "export_process_invalid_response",
            format!("Export process {action} did not return ExportJob JSON on stdout."),
        ));
    }
    let value: Value = serde_json::from_str(raw.trim()).map_err(|error| {
        service_error(
            "export_process_invalid_response",
            format!("Export process {action} returned invalid JSON: {error}"),
        )
    })?;
    let job_value = value.get("job").cloned().unwrap_or(value);
    serde_json::from_value::<ExportJob>(job_value).map_err(|error| {
        service_error(
            "export_process_invalid_response",
            format!("Export process {action} JSON is not an ExportJob: {error}"),
        )
    })
}

fn ensure_job_id(expected: &str, mut job: ExportJob) -> ServiceResult<ExportJob> {
    if job.job_id.trim().is_empty() {
        job.job_id = expected.to_string();
    }
    if job.job_id == expected {
        Ok(job)
    } else {
        Err(service_error(
            "export_process_job_id_mismatch",
            format!(
                "Export process returned job_id '{}' but host requested '{}'.",
                job.job_id, expected
            ),
        ))
    }
}

fn require_job_id(job_id: &str) -> ServiceResult<String> {
    let job_id = job_id.trim();
    if job_id.is_empty() {
        Err(service_error("export_job_id_required", "job_id required"))
    } else {
        Ok(job_id.to_string())
    }
}

fn split_command_line(raw: &str) -> Result<Vec<OsString>, String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;

    for ch in raw.trim().chars() {
        match ch {
            '"' | '\'' if quote == Some(ch) => quote = None,
            '"' | '\'' if quote.is_none() => quote = Some(ch),
            ch if ch.is_whitespace() && quote.is_none() => {
                if !current.is_empty() {
                    out.push(OsString::from(std::mem::take(&mut current)));
                }
            }
            _ => current.push(ch),
        }
    }

    if quote.is_some() {
        return Err("unterminated quote".into());
    }
    if !current.is_empty() {
        out.push(OsString::from(current));
    }
    Ok(out)
}

fn output_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .take(4_000)
        .collect::<String>()
}

fn join_error(error: tokio::task::JoinError) -> ServiceError {
    service_error(
        "export_process_join_failed",
        format!("Export process worker failed: {error}"),
    )
}

fn service_error(code: &'static str, message: impl Into<String>) -> ServiceError {
    ServiceError::new(code, message)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use qnc_service_contracts::{ExportJobState, ExportRequest};
    use serde_json::json;

    use super::*;

    fn sample_request() -> ExportRequest {
        ExportRequest {
            project_id: "p1".into(),
            playlist: json!({"segments": []}),
            project_settings: json!({"export": {"format": "xml"}}),
            export_settings: json!({"format": "xml"}),
            output_dir: None,
        }
    }

    #[test]
    fn split_command_line_keeps_quoted_paths() {
        let parts = split_command_line(r#""C:\Program Files\QNC Export\export.exe" --fast"#)
            .unwrap()
            .into_iter()
            .map(|part| part.to_string_lossy().to_string())
            .collect::<Vec<_>>();

        assert_eq!(parts[0], r#"C:\Program Files\QNC Export\export.exe"#);
        assert_eq!(parts[1], "--fast");
    }

    #[tokio::test]
    async fn external_process_submit_delegates_request_to_command() {
        let root = std::env::temp_dir().join(format!(
            "qnc_export_process_test_{}_{}",
            std::process::id(),
            Uuid::new_v4().simple()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let command = write_test_exporter(&root);
        let engine = ExternalProcessExportEngine::with_request_root(command, root.clone());

        let job = engine.submit(sample_request()).await.unwrap();

        assert_eq!(job.state, ExportJobState::Completed);
        assert_eq!(job.message.as_deref(), Some("ok"));
        let request_path = root.join(&job.job_id).join("request.json");
        let envelope: Value = serde_json::from_str(&fs::read_to_string(request_path).unwrap())
            .expect("request envelope json");
        assert_eq!(envelope["protocol_version"], PROTOCOL_VERSION);
        assert_eq!(envelope["request"]["project_id"], "p1");

        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    fn write_test_exporter(root: &Path) -> String {
        let script = root.join("exporter.ps1");
        fs::write(
            &script,
            r#"
$jobId = $env:QNC_EXPORT_JOB_ID
@{
  job_id = $jobId
  state = "completed"
  artifacts = @()
  message = "ok"
} | ConvertTo-Json -Compress -Depth 5
"#,
        )
        .unwrap();
        format!(
            r#"powershell -NoProfile -ExecutionPolicy Bypass -File "{}""#,
            script.display()
        )
    }

    #[cfg(not(windows))]
    fn write_test_exporter(root: &Path) -> String {
        let script = root.join("exporter.sh");
        fs::write(
            &script,
            r#"printf '{"job_id":"%s","state":"completed","artifacts":[],"message":"ok"}\n' "$QNC_EXPORT_JOB_ID""#,
        )
        .unwrap();
        format!(r#"sh "{}""#, script.display())
    }
}
