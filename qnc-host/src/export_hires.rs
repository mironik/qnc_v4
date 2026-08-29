use std::fs;
use std::path::PathBuf;

use qnc_service_contracts::{
    ExportHiResJobPayload, ExportHiResSubmitResponse, ExportJobState, JOB_SOURCE_EXPORT_HIRES,
    JOB_TYPE_EXPORT_HIRES,
};
use rusqlite::params;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::ingest::db::{ingest_job_id, open_ingest, queue_ingest_job_payload};
use crate::media::ProjectMediaGateway;
use crate::project::db::{export_dir_from_settings, ProjectPaths};
use crate::project::ProjectDbBroker;

use crate::editorial_playlist::build_editorial_playlist_with_broker;
use crate::export_playlist::materialize_export_flat_payload;

pub(crate) fn submit_hires_export_job(
    paths: &ProjectPaths,
    project_db: &ProjectDbBroker,
    media_gateway: &ProjectMediaGateway,
    project_id: &str,
    project_settings: &Value,
) -> Result<ExportHiResSubmitResponse, String> {
    let pid = project_id.trim();
    if pid.is_empty() {
        return Err("project_id required".into());
    }

    let export_id = format!("hires_{}", Uuid::new_v4().simple());
    let output_dir = output_dir_for_hires_export(paths, pid, project_settings)?;
    let output_path = output_dir.join(format!("{}_{}", pid, export_id));
    let payload = prepare_hires_flat_payload(
        paths,
        project_db,
        media_gateway,
        pid,
        output_path,
        &export_id,
    )?;
    let output_path = payload.output_path.clone();
    let timeline_timebase = payload.timeline_timebase;
    let duration_frames = payload.duration_frames;
    let job_id = queue_hires_render_job(
        paths,
        project_db,
        pid,
        JOB_TYPE_EXPORT_HIRES,
        JOB_SOURCE_EXPORT_HIRES,
        &export_id,
        &payload,
        "Export HI-res",
    )?;

    Ok(ExportHiResSubmitResponse {
        project_id: pid.to_string(),
        job_id,
        export_id,
        state: ExportJobState::Queued,
        output_path,
        timeline_timebase,
        duration_frames,
        message: Some("Export HI-res job je dodan u worker red".into()),
    })
}

pub(crate) fn hires_export_status(
    paths: &ProjectPaths,
    project_db: &ProjectDbBroker,
    project_id: &str,
    job_id: &str,
) -> Result<Value, String> {
    let pid = project_id.trim();
    if pid.is_empty() {
        return Err("project_id required".into());
    }
    let job_id = job_id.trim();
    if job_id.is_empty() {
        return Err("job_id required".into());
    }
    project_db.serialize_project_write(pid, || {
        let conn = open_ingest(paths, pid).map_err(|e| e.to_string())?;
        let row = conn
            .query_row(
                "SELECT job_id, job_type, source_id, clip_id, status, error, COALESCE(payload_json, '{}')
                 FROM ingest_jobs WHERE job_id = ?1",
                params![job_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .map_err(|error| format!("Export HI-res job nije pronaden: {error}"))?;
        let (job_id, job_type, source_id, export_id, status, error, payload_json) = row;
        if job_type != JOB_TYPE_EXPORT_HIRES || source_id != JOB_SOURCE_EXPORT_HIRES {
            return Err(format!("Job '{job_id}' nije Export HI-res job."));
        }
        let payload = serde_json::from_str::<ExportHiResJobPayload>(&payload_json).ok();
        Ok(json!({
            "project_id": pid,
            "job_id": job_id,
            "export_id": export_id,
            "status": status,
            "error": error,
            "output_path": payload.as_ref().map(|payload| payload.output_path.clone()),
            "timeline_timebase": payload.as_ref().map(|payload| payload.timeline_timebase),
            "duration_frames": payload.as_ref().map(|payload| payload.duration_frames),
        }))
    })
}

fn prepare_hires_flat_payload(
    paths: &ProjectPaths,
    project_db: &ProjectDbBroker,
    media_gateway: &ProjectMediaGateway,
    project_id: &str,
    output_path: PathBuf,
    render_id: &str,
) -> Result<ExportHiResJobPayload, String> {
    let playlist = build_editorial_playlist_with_broker(paths, project_db, project_id)?;
    materialize_export_flat_payload(media_gateway, &playlist, output_path, render_id)
}

fn queue_hires_render_job(
    paths: &ProjectPaths,
    project_db: &ProjectDbBroker,
    project_id: &str,
    job_type: &str,
    job_source: &str,
    render_id: &str,
    payload: &ExportHiResJobPayload,
    label: &str,
) -> Result<String, String> {
    let payload_json = serde_json::to_value(payload)
        .map_err(|error| format!("{label} payload nije valjan: {error}"))?;
    let job_id = ingest_job_id(job_type, job_source, render_id);

    project_db.serialize_project_write(project_id, || {
        let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
        queue_ingest_job_payload(&conn, job_type, job_source, render_id, &payload_json)
            .map_err(|e| e.to_string())
    })?;

    Ok(job_id)
}

fn output_dir_for_hires_export(
    paths: &ProjectPaths,
    project_id: &str,
    settings: &Value,
) -> Result<PathBuf, String> {
    let output_dir = export_dir_from_settings(settings)
        .unwrap_or_else(|| paths.project_dir(project_id).join("exports"));
    fs::create_dir_all(&output_dir)
        .map_err(|error| format!("Export direktorij nije dostupan: {error}"))?;
    Ok(output_dir)
}
