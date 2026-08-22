use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use qnc_service_contracts::{
    FilmstripJobFrame, FilmstripJobPayload, FilmstripJobResult, JobAck, JobClaimRequest,
    JobClaimResponse, JobCompleteRequest, JobFailRequest, JobHeartbeatRequest,
    JobHeartbeatResponse, JobLease, ProxyGenerateJobPayload, ProxyGenerateJobResult,
    JOB_SOURCE_FILMSTRIP, JOB_TYPE_FILMSTRIP,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app_state::AppState;
use crate::ingest::asset_row::IngestAssetRow;
use crate::ingest::db::{
    ingest_asset_meta, mark_ingest_job_done, mark_ingest_job_error,
    migrate_ingest_job_lease_columns, open_ingest,
};
use crate::ingest::import_finish::complete_imported_clip;
use crate::ingest::proxy_generate::proxy_dest_for_source;
use crate::ingest::store::{ingest_probe_from_service, row_import_error};
use crate::media::{
    find_card_proxy_for_media_path, is_proxy_media_path, resolve_import_plan, ImportMediaMode,
};
use crate::project::db::{now_str, project_settings_snapshot, ProjectPaths};
use crate::project::{list_project_ids, ProjectDbBroker};

const DEFAULT_MAX_JOBS: usize = 1;
const MAX_CLAIM_JOBS: usize = 8;
const DEFAULT_LEASE_MS: u64 = 30_000;
const MIN_LEASE_MS: u64 = 5_000;
const MAX_LEASE_MS: u64 = 300_000;
const SMOKE_JOB_TYPE: &str = "qnc_worker_smoke";
const PROXY_GENERATE_JOB_TYPE: &str = "proxy_generate";
const FILMSTRIP_JOB_TYPE: &str = JOB_TYPE_FILMSTRIP;
const FILMSTRIP_SOURCE_ID: &str = JOB_SOURCE_FILMSTRIP;
const EXTERNAL_CLAIMABLE_JOB_TYPES: &[&str] =
    &[SMOKE_JOB_TYPE, PROXY_GENERATE_JOB_TYPE, FILMSTRIP_JOB_TYPE];

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/jobs/claim", post(api_jobs_claim))
        .route("/api/jobs/heartbeat", post(api_jobs_heartbeat))
        .route("/api/jobs/complete", post(api_jobs_complete))
        .route("/api/jobs/fail", post(api_jobs_fail))
}

async fn api_jobs_claim(
    State(state): State<AppState>,
    Json(body): Json<JobClaimRequest>,
) -> Result<Json<JobClaimResponse>, (StatusCode, String)> {
    let claim = NormalizedClaim::from_request(body)?;
    let playback_active = state.background_work.playback_active();
    if playback_active {
        return Ok(Json(JobClaimResponse {
            jobs: Vec::new(),
            playback_active: true,
            message: Some("playback_active".into()),
        }));
    }
    if claim.capabilities.is_empty() {
        return Ok(Json(JobClaimResponse {
            jobs: Vec::new(),
            playback_active: false,
            message: Some("no_external_claimable_capabilities".into()),
        }));
    }

    let paths = state.project.paths.clone();
    let project_db = state.project_db.clone();
    let jobs = tokio::task::spawn_blocking(move || claim_jobs(paths, project_db, claim))
        .await
        .map_err(internal_join_error)?
        .map_err(internal_error)?;

    Ok(Json(JobClaimResponse {
        jobs,
        playback_active: false,
        message: None,
    }))
}

async fn api_jobs_heartbeat(
    State(state): State<AppState>,
    Json(body): Json<JobHeartbeatRequest>,
) -> Result<Json<JobHeartbeatResponse>, (StatusCode, String)> {
    let paths = state.project.paths.clone();
    let project_db = state.project_db.clone();
    let response = tokio::task::spawn_blocking(move || heartbeat_jobs(paths, project_db, body))
        .await
        .map_err(internal_join_error)?
        .map_err(request_error)?;
    Ok(Json(response))
}

async fn api_jobs_complete(
    State(state): State<AppState>,
    Json(body): Json<JobCompleteRequest>,
) -> Result<Json<JobAck>, (StatusCode, String)> {
    let paths = state.project.paths.clone();
    let project_db = state.project_db.clone();
    let response = tokio::task::spawn_blocking(move || complete_job(paths, project_db, body))
        .await
        .map_err(internal_join_error)?
        .map_err(request_error)?;
    Ok(Json(response))
}

async fn api_jobs_fail(
    State(state): State<AppState>,
    Json(body): Json<JobFailRequest>,
) -> Result<Json<JobAck>, (StatusCode, String)> {
    let paths = state.project.paths.clone();
    let project_db = state.project_db.clone();
    let response = tokio::task::spawn_blocking(move || fail_job(paths, project_db, body))
        .await
        .map_err(internal_join_error)?
        .map_err(request_error)?;
    Ok(Json(response))
}

#[derive(Debug, Clone)]
struct NormalizedClaim {
    worker_id: String,
    project_id: Option<String>,
    capabilities: Vec<String>,
    max_jobs: usize,
    lease_ms: u64,
    lease_id: String,
}

impl NormalizedClaim {
    fn from_request(request: JobClaimRequest) -> Result<Self, (StatusCode, String)> {
        let worker_id = required_id("worker_id", &request.worker_id)?;
        let capabilities = normalize_capabilities(request.capabilities);
        let max_jobs = request
            .max_jobs
            .unwrap_or(DEFAULT_MAX_JOBS)
            .clamp(1, MAX_CLAIM_JOBS);
        let lease_ms = normalize_lease_ms(request.lease_ms);
        let project_id = request
            .project_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        Ok(Self {
            worker_id,
            project_id,
            capabilities,
            max_jobs,
            lease_ms,
            lease_id: format!("lease_{}", Uuid::new_v4().simple()),
        })
    }
}

fn claim_jobs(
    paths: ProjectPaths,
    project_db: ProjectDbBroker,
    claim: NormalizedClaim,
) -> Result<Vec<JobLease>, String> {
    let project_ids = if let Some(project_id) = claim.project_id.as_ref() {
        vec![project_id.clone()]
    } else {
        project_db.with_global(|conn| list_project_ids(conn).map_err(|e| e.to_string()))?
    };

    let mut out = Vec::new();
    for project_id in project_ids {
        if out.len() >= claim.max_jobs {
            break;
        }
        let remaining = claim.max_jobs - out.len();
        let mut leases =
            claim_jobs_for_project(&paths, &project_db, &project_id, &claim, remaining)?;
        out.append(&mut leases);
    }
    Ok(out)
}

fn claim_jobs_for_project(
    paths: &ProjectPaths,
    project_db: &ProjectDbBroker,
    project_id: &str,
    claim: &NormalizedClaim,
    limit: usize,
) -> Result<Vec<JobLease>, String> {
    let pid = project_id.trim().to_string();
    if pid.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    project_db.serialize_project_write(&pid, || {
        let conn = open_ingest(paths, &pid).map_err(|e| e.to_string())?;
        ensure_job_service_schema(&conn).map_err(|e| e.to_string())?;
        let now_ms = now_unix_ms();
        requeue_expired_leases(&conn, now_ms).map_err(|e| e.to_string())?;

        let mut claimed = Vec::new();
        for job_type in &claim.capabilities {
            if claimed.len() >= limit {
                break;
            }
            if !is_external_claimable_job_type(job_type) {
                continue;
            }
            let remaining = limit - claimed.len();
            let rows = queued_jobs_for_type(&conn, job_type, remaining)?;
            for row in rows {
                if claimed.len() >= limit {
                    break;
                }
                let Some(payload) = payload_for_job_claim(paths, &pid, &conn, &row)? else {
                    continue;
                };
                let lease_until = now_ms.saturating_add(claim.lease_ms);
                let now_text = now_str();
                let changed = conn
                    .execute(
                        "UPDATE ingest_jobs
                         SET status = 'processing',
                             error = '',
                             attempts = attempts + 1,
                             started_at = COALESCE(started_at, ?2),
                             updated_at = ?2,
                             worker_id = ?3,
                             lease_id = ?4,
                             lease_until_ms = ?5,
                             heartbeat_ms = ?6
                         WHERE job_id = ?1 AND status = 'queued'",
                        params![
                            row.job_id,
                            now_text,
                            claim.worker_id,
                            claim.lease_id,
                            lease_until as i64,
                            now_ms as i64
                        ],
                    )
                    .map_err(|e| e.to_string())?;
                if changed == 1 {
                    claimed.push(JobLease {
                        job_id: row.job_id,
                        project_id: pid.clone(),
                        job_type: row.job_type,
                        source_id: row.source_id,
                        clip_id: row.clip_id,
                        worker_id: claim.worker_id.clone(),
                        lease_id: claim.lease_id.clone(),
                        lease_until_unix_ms: lease_until,
                        attempts: row.attempts + 1,
                        queued_at: row.queued_at,
                        payload,
                    });
                }
            }
        }
        Ok(claimed)
    })
}

fn payload_for_job_claim(
    paths: &ProjectPaths,
    project_id: &str,
    conn: &Connection,
    row: &QueuedJobRow,
) -> Result<Option<Value>, String> {
    match row.job_type.as_str() {
        SMOKE_JOB_TYPE => Ok(Some(json!({}))),
        PROXY_GENERATE_JOB_TYPE => payload_for_proxy_generate_claim(paths, project_id, conn, row),
        FILMSTRIP_JOB_TYPE => payload_for_filmstrip_claim(paths, project_id, conn, row),
        _ => Ok(None),
    }
}

fn payload_for_filmstrip_claim(
    paths: &ProjectPaths,
    project_id: &str,
    conn: &Connection,
    row: &QueuedJobRow,
) -> Result<Option<Value>, String> {
    let clip_id = row.clip_id.trim();
    if clip_id.is_empty() || row.source_id.trim() != FILMSTRIP_SOURCE_ID {
        return Ok(None);
    }
    let Some(media) = crate::media::resolve_filmstrip_media(paths, project_id, clip_id, None)
    else {
        let _ = mark_ingest_job_error(
            conn,
            FILMSTRIP_JOB_TYPE,
            FILMSTRIP_SOURCE_ID,
            clip_id,
            "filmstrip media missing",
        );
        return Ok(None);
    };
    let duration = stored_clip_duration_sec(conn, clip_id)
        .or_else(|| {
            qnc_media_ffmpeg::proxy::probe_media(&media)
                .ok()
                .and_then(|probe| probe.duration_sec)
        })
        .filter(|value| value.is_finite() && *value > 0.0);
    let Some(duration) = duration else {
        let _ = mark_ingest_job_error(
            conn,
            FILMSTRIP_JOB_TYPE,
            FILMSTRIP_SOURCE_ID,
            clip_id,
            "filmstrip duration missing",
        );
        return Ok(None);
    };
    let seeks = crate::ingest::thumb::timeline_seek_seconds(
        duration,
        crate::filmstrip::DEFAULT_FILMSTRIP_FRAMES,
    );
    let out_dir = crate::filmstrip::filmstrip_clip_dir(paths, project_id, clip_id);
    let frames: Vec<FilmstripJobFrame> = seeks
        .iter()
        .enumerate()
        .map(|(index, seek_sec)| FilmstripJobFrame {
            index,
            seek_sec: *seek_sec,
            output_path: crate::ingest::thumb::filmstrip_frame_path(&out_dir, index, *seek_sec),
        })
        .collect();
    if frames.len() < 2 {
        return Ok(None);
    }
    serde_json::to_value(FilmstripJobPayload {
        media_path: media,
        duration_sec: duration,
        frames,
    })
    .map(Some)
    .map_err(|error| error.to_string())
}

fn stored_clip_duration_sec(conn: &Connection, clip_id: &str) -> Option<f64> {
    conn.query_row(
        "SELECT duration_sec
         FROM ingest_assets
         WHERE clip_id = ?1
           AND import_status IN ('imported', 'done')
         ORDER BY CASE import_status WHEN 'imported' THEN 0 WHEN 'done' THEN 1 ELSE 2 END
         LIMIT 1",
        params![clip_id],
        |row| row.get::<_, f64>(0),
    )
    .ok()
    .filter(|value| value.is_finite() && *value > 0.0)
}

fn payload_for_proxy_generate_claim(
    paths: &ProjectPaths,
    project_id: &str,
    conn: &Connection,
    row: &QueuedJobRow,
) -> Result<Option<Value>, String> {
    let asset = match read_ingest_asset_row(conn, &row.source_id, &row.clip_id) {
        Ok(asset) => asset,
        Err(_) => return Ok(None),
    };
    let project = project_settings_snapshot(paths, project_id).unwrap_or_else(|_| json!({}));
    match proxy_generate_preflight_from_row(paths, project_id, &asset, &project)? {
        ProxyGeneratePreflight::Generate(payload) => serde_json::to_value(payload)
            .map(Some)
            .map_err(|error| error.to_string()),
        ProxyGeneratePreflight::ExistingProxy { source_path } => {
            let _ = source_path;
            Ok(None)
        }
        ProxyGeneratePreflight::Skip { reason } => {
            let _ = reason;
            Ok(None)
        }
    }
}

#[derive(Debug, Clone)]
enum ProxyGeneratePreflight {
    Generate(ProxyGenerateJobPayload),
    ExistingProxy { source_path: PathBuf },
    Skip { reason: String },
}

#[allow(dead_code)]
fn proxy_generate_preflight(
    paths: &ProjectPaths,
    project_db: &ProjectDbBroker,
    project_id: &str,
    source_id: &str,
    clip_id: &str,
) -> Result<ProxyGeneratePreflight, String> {
    let pid = required_runtime_id("project_id", project_id)?;
    let sid = required_runtime_id("source_id", source_id)?;
    let cid = required_runtime_id("clip_id", clip_id)?;
    project_db.serialize_project_write(&pid, || {
        let conn = open_ingest(paths, &pid).map_err(|e| e.to_string())?;
        let row = read_ingest_asset_row(&conn, &sid, &cid)?;
        let project = project_settings_snapshot(paths, &pid).unwrap_or_else(|_| json!({}));
        proxy_generate_preflight_from_row(paths, &pid, &row, &project)
    })
}

#[allow(dead_code)]
fn proxy_generate_preflight_from_row(
    paths: &ProjectPaths,
    project_id: &str,
    row: &IngestAssetRow,
    project: &serde_json::Value,
) -> Result<ProxyGeneratePreflight, String> {
    if row.status != "generating_proxy" {
        return Ok(ProxyGeneratePreflight::Skip {
            reason: format!("clip is not waiting for proxy generation: {}", row.status),
        });
    }

    if let Some(proxy) = existing_or_discovered_proxy(row) {
        return Ok(ProxyGeneratePreflight::ExistingProxy { source_path: proxy });
    }

    let meta = ingest_asset_meta(&row.meta_input_without_project_proxy());
    let plan = resolve_import_plan(&meta, project)?;
    if plan.mode != ImportMediaMode::GenerateProxy {
        return if is_proxy_media_path(&plan.source) {
            Ok(ProxyGeneratePreflight::ExistingProxy {
                source_path: plan.source,
            })
        } else {
            Ok(ProxyGeneratePreflight::Skip {
                reason: format!(
                    "resolved import mode is not proxy generation: {:?}",
                    plan.mode
                ),
            })
        };
    }

    let proxy_dir = paths.project_dir(project_id).join("proxy");
    let source = existing_original_or_plan_source(row, plan.source);
    let output_path = proxy_dest_for_source(&proxy_dir, &row.clip_id, &source)?;
    let original_path = existing_text_path(&row.original_path).or_else(|| Some(source.clone()));
    Ok(ProxyGeneratePreflight::Generate(ProxyGenerateJobPayload {
        source_path: source,
        output_path,
        asset_status: plan.asset_status.to_string(),
        card_locked: plan.card_locked,
        original_path,
    }))
}

#[allow(dead_code)]
fn apply_proxy_generate_result(
    paths: &ProjectPaths,
    project_db: &ProjectDbBroker,
    project_id: &str,
    source_id: &str,
    clip_id: &str,
    result: ProxyGenerateJobResult,
) -> Result<(), String> {
    let pid = required_runtime_id("project_id", project_id)?;
    let sid = required_runtime_id("source_id", source_id)?;
    let cid = required_runtime_id("clip_id", clip_id)?;
    if !result.output_path.is_file() {
        return Err(format!(
            "generated proxy is missing: {}",
            result.output_path.display()
        ));
    }

    project_db.serialize_project_write(&pid, || {
        let conn = open_ingest(paths, &pid).map_err(|e| e.to_string())?;
        let row = read_ingest_asset_row(&conn, &sid, &cid)?;
        let project = project_settings_snapshot(paths, &pid).unwrap_or_else(|_| json!({}));
        let meta = ingest_asset_meta(&row.meta_input_without_project_proxy());
        let plan = resolve_import_plan(&meta, &project)?;
        let original_path = result
            .output_path
            .parent()
            .and_then(|_| existing_text_path(&row.original_path))
            .or_else(|| existing_text_path(&row.source_path))
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let probe = result.probe.and_then(ingest_probe_from_service);

        complete_imported_clip(
            paths,
            &pid,
            &sid,
            &cid,
            &result.output_path,
            plan.asset_status,
            false,
            plan.card_locked,
            &original_path,
            probe.as_ref(),
        )?;
        let conn = open_ingest(paths, &pid).map_err(|e| e.to_string())?;
        mark_ingest_job_done(&conn, "proxy_generate", &sid, &cid).map_err(|e| e.to_string())?;
        Ok(())
    })
}

fn apply_filmstrip_job_result(
    paths: &ProjectPaths,
    project_db: &ProjectDbBroker,
    project_id: &str,
    clip_id: &str,
    result: FilmstripJobResult,
) -> Result<(), String> {
    let duration = result
        .duration_sec
        .is_finite()
        .then_some(result.duration_sec)
        .filter(|value| *value > 0.0)
        .ok_or_else(|| format!("filmstrip duration missing for clip_id={clip_id}"))?;
    let mut frames: Vec<crate::filmstrip::FilmstripFrame> = result
        .frames
        .into_iter()
        .filter_map(|frame| {
            let path = frame.artifact.path;
            (path.is_file() && path.metadata().map(|m| m.len()).unwrap_or(0) > 0).then_some(
                crate::filmstrip::FilmstripFrame {
                    index: frame.index,
                    seek_sec: frame.seek_sec,
                    path,
                },
            )
        })
        .collect();
    frames.sort_by_key(|frame| frame.index);

    let seeks = crate::ingest::thumb::timeline_seek_seconds(
        duration,
        crate::filmstrip::DEFAULT_FILMSTRIP_FRAMES,
    );
    crate::filmstrip::save_built_filmstrip_frames(
        paths, project_db, project_id, clip_id, duration, &frames, &seeks,
    )
}

fn read_ingest_asset_row(
    conn: &Connection,
    source_id: &str,
    clip_id: &str,
) -> Result<IngestAssetRow, String> {
    conn.query_row(
        "SELECT source_id, clip_id, source_path, original_path, proxy_path,
                project_proxy_path, card_thumb_path, file_extension,
                read_from_card, card_locked, poster_source, import_status
         FROM ingest_assets
         WHERE source_id = ?1 AND clip_id = ?2",
        params![source_id, clip_id],
        IngestAssetRow::from_row,
    )
    .map_err(|e| e.to_string())
}

fn existing_or_discovered_proxy(row: &IngestAssetRow) -> Option<PathBuf> {
    existing_text_path(&row.project_proxy_path)
        .or_else(|| existing_text_path(&row.proxy_path))
        .or_else(|| {
            existing_text_path(&row.original_path)
                .or_else(|| existing_text_path(&row.source_path))
                .and_then(|source| find_card_proxy_for_media_path(&source))
        })
}

fn existing_original_or_plan_source(row: &IngestAssetRow, plan_source: PathBuf) -> PathBuf {
    existing_text_path(&row.original_path).unwrap_or(plan_source)
}

fn existing_text_path(raw: &str) -> Option<PathBuf> {
    let path = PathBuf::from(raw.trim());
    path.is_file().then_some(path)
}

fn heartbeat_jobs(
    paths: ProjectPaths,
    project_db: ProjectDbBroker,
    request: JobHeartbeatRequest,
) -> Result<JobHeartbeatResponse, String> {
    let worker_id = required_runtime_id("worker_id", &request.worker_id)?;
    let project_id = required_runtime_id("project_id", &request.project_id)?;
    let lease_id = required_runtime_id("lease_id", &request.lease_id)?;
    let lease_until = now_unix_ms().saturating_add(normalize_lease_ms(request.lease_ms));
    let now_ms = now_unix_ms();
    let now_text = now_str();
    let job_ids: Vec<String> = request
        .job_ids
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();

    project_db.serialize_project_write(&project_id, || {
        let conn = open_ingest(&paths, &project_id).map_err(|e| e.to_string())?;
        ensure_job_service_schema(&conn).map_err(|e| e.to_string())?;
        for job_id in job_ids {
            let changed = conn
                .execute(
                    "UPDATE ingest_jobs
                     SET heartbeat_ms = ?4,
                         lease_until_ms = ?5,
                         updated_at = ?6
                     WHERE job_id = ?1
                   AND worker_id = ?2
                   AND lease_id = ?3
                   AND status = 'processing'
                   AND job_type IN ('qnc_worker_smoke', 'proxy_generate', 'filmstrip')",
                    params![
                        job_id,
                        worker_id,
                        lease_id,
                        now_ms as i64,
                        lease_until as i64,
                        now_text
                    ],
                )
                .map_err(|e| e.to_string())?;
            if changed == 1 {
                accepted.push(job_id);
            } else {
                rejected.push(job_id);
            }
        }
        Ok(())
    })?;

    Ok(JobHeartbeatResponse {
        accepted,
        rejected,
        lease_until_unix_ms: lease_until,
    })
}

fn complete_job(
    paths: ProjectPaths,
    project_db: ProjectDbBroker,
    request: JobCompleteRequest,
) -> Result<JobAck, String> {
    let worker_id = required_runtime_id("worker_id", &request.worker_id)?;
    let project_id = required_runtime_id("project_id", &request.project_id)?;
    let lease_id = required_runtime_id("lease_id", &request.lease_id)?;
    let job_id = required_runtime_id("job_id", &request.job_id)?;
    let result_json = serde_json::to_string(&request.result).unwrap_or_else(|_| "{}".into());
    let Some(active) = active_lease_job(
        &paths,
        &project_db,
        &project_id,
        &job_id,
        &worker_id,
        &lease_id,
    )?
    else {
        return Ok(JobAck {
            accepted: false,
            job_id,
            message: Some("lease_not_active".into()),
        });
    };

    match active.job_type.as_str() {
        SMOKE_JOB_TYPE => {}
        PROXY_GENERATE_JOB_TYPE => {
            let result: ProxyGenerateJobResult =
                serde_json::from_value(request.result).map_err(|error| {
                    format!("invalid proxy_generate result for job_id={job_id}: {error}")
                })?;
            apply_proxy_generate_result(
                &paths,
                &project_db,
                &project_id,
                &active.source_id,
                &active.clip_id,
                result,
            )?;
        }
        FILMSTRIP_JOB_TYPE => {
            let result: FilmstripJobResult =
                serde_json::from_value(request.result).map_err(|error| {
                    format!("invalid filmstrip result for job_id={job_id}: {error}")
                })?;
            apply_filmstrip_job_result(&paths, &project_db, &project_id, &active.clip_id, result)?;
        }
        _ => {
            return Ok(JobAck {
                accepted: false,
                job_id,
                message: Some("unsupported_job_type".into()),
            });
        }
    }

    let changed = finish_active_job_done(
        &paths,
        &project_db,
        &project_id,
        &job_id,
        &worker_id,
        &lease_id,
        &active.job_type,
        &result_json,
    )?;

    Ok(JobAck {
        accepted: changed == 1,
        job_id,
        message: if changed == 1 {
            None
        } else {
            Some("lease_not_active".into())
        },
    })
}

#[derive(Debug, Clone)]
struct ActiveLeaseJob {
    job_type: String,
    source_id: String,
    clip_id: String,
}

fn active_lease_job(
    paths: &ProjectPaths,
    project_db: &ProjectDbBroker,
    project_id: &str,
    job_id: &str,
    worker_id: &str,
    lease_id: &str,
) -> Result<Option<ActiveLeaseJob>, String> {
    project_db.serialize_project_write(project_id, || {
        let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
        ensure_job_service_schema(&conn).map_err(|e| e.to_string())?;
        let row = conn
            .query_row(
                "SELECT job_type, source_id, clip_id
                 FROM ingest_jobs
                 WHERE job_id = ?1
                   AND worker_id = ?2
                   AND lease_id = ?3
                   AND status = 'processing'",
                params![job_id, worker_id, lease_id],
                |row| {
                    Ok(ActiveLeaseJob {
                        job_type: row.get(0)?,
                        source_id: row.get(1)?,
                        clip_id: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(|e| e.to_string())?;
        Ok(row.filter(|job| is_lease_managed_job_type(&job.job_type)))
    })
}

fn finish_active_job_done(
    paths: &ProjectPaths,
    project_db: &ProjectDbBroker,
    project_id: &str,
    job_id: &str,
    worker_id: &str,
    lease_id: &str,
    job_type: &str,
    result_json: &str,
) -> Result<usize, String> {
    let now = now_str();
    project_db.serialize_project_write(project_id, || {
        let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
        ensure_job_service_schema(&conn).map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE ingest_jobs
             SET status = 'done',
                 error = '',
                 finished_at = ?5,
                 updated_at = ?5,
                 worker_id = '',
                 lease_id = '',
                 lease_until_ms = 0,
                 heartbeat_ms = 0,
                 result_json = ?6
             WHERE job_id = ?1
               AND worker_id = ?2
               AND lease_id = ?3
               AND job_type = ?4
               AND status IN ('processing', 'done')",
            params![job_id, worker_id, lease_id, job_type, now, result_json],
        )
        .map_err(|e| e.to_string())
    })
}

fn fail_job(
    paths: ProjectPaths,
    project_db: ProjectDbBroker,
    request: JobFailRequest,
) -> Result<JobAck, String> {
    let worker_id = required_runtime_id("worker_id", &request.worker_id)?;
    let project_id = required_runtime_id("project_id", &request.project_id)?;
    let lease_id = required_runtime_id("lease_id", &request.lease_id)?;
    let job_id = required_runtime_id("job_id", &request.job_id)?;
    let error = truncate_error(&request.error);
    let now = now_str();
    let active = active_lease_job(
        &paths,
        &project_db,
        &project_id,
        &job_id,
        &worker_id,
        &lease_id,
    )?;
    let Some(active) = active else {
        return Ok(JobAck {
            accepted: false,
            job_id,
            message: Some("lease_not_active".into()),
        });
    };

    let changed = project_db.serialize_project_write(&project_id, || {
        let conn = open_ingest(&paths, &project_id).map_err(|e| e.to_string())?;
        ensure_job_service_schema(&conn).map_err(|e| e.to_string())?;
        if request.retryable {
            conn.execute(
                "UPDATE ingest_jobs
                 SET status = 'queued',
                     error = ?4,
                     updated_at = ?5,
                     worker_id = '',
                     lease_id = '',
                     lease_until_ms = 0,
                     heartbeat_ms = 0
                 WHERE job_id = ?1
                   AND worker_id = ?2
                   AND lease_id = ?3
                   AND status = 'processing'
                   AND job_type = ?6",
                params![job_id, worker_id, lease_id, error, now, &active.job_type],
            )
        } else {
            conn.execute(
                "UPDATE ingest_jobs
                 SET status = 'error',
                     error = ?4,
                     finished_at = ?5,
                     updated_at = ?5,
                     worker_id = '',
                     lease_id = '',
                     lease_until_ms = 0,
                     heartbeat_ms = 0
                 WHERE job_id = ?1
                   AND worker_id = ?2
                   AND lease_id = ?3
                   AND status = 'processing'
                   AND job_type = ?6",
                params![job_id, worker_id, lease_id, error, now, &active.job_type],
            )
        }
        .map_err(|e| e.to_string())
    })?;

    if changed == 1 && !request.retryable && active.job_type == PROXY_GENERATE_JOB_TYPE {
        project_db.serialize_project_write(&project_id, || {
            let conn = open_ingest(&paths, &project_id).map_err(|e| e.to_string())?;
            row_import_error(&conn, &active.source_id, &active.clip_id, &error)
                .map_err(|e| e.to_string())
        })?;
    }
    if changed == 1 && !request.retryable && active.job_type == FILMSTRIP_JOB_TYPE {
        project_db.serialize_project_write(&project_id, || {
            crate::filmstrip::mark_filmstrip(&paths, &project_id, &active.clip_id, "error", &error)
        })?;
    }

    Ok(JobAck {
        accepted: changed == 1,
        job_id,
        message: if changed == 1 {
            None
        } else {
            Some("lease_not_active".into())
        },
    })
}

#[derive(Debug)]
struct QueuedJobRow {
    job_id: String,
    job_type: String,
    source_id: String,
    clip_id: String,
    attempts: i64,
    queued_at: Option<String>,
}

fn queued_jobs_for_type(
    conn: &Connection,
    job_type: &str,
    limit: usize,
) -> Result<Vec<QueuedJobRow>, String> {
    let limit = limit.max(1) as i64;
    let mut stmt = conn
        .prepare(
            "SELECT job_id, job_type, source_id, clip_id, attempts, queued_at
             FROM ingest_jobs
             WHERE job_type = ?1 AND status = 'queued'
             ORDER BY queued_at ASC, updated_at ASC, job_id ASC
             LIMIT ?2",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![job_type, limit], |row| {
            Ok(QueuedJobRow {
                job_id: row.get(0)?,
                job_type: row.get(1)?,
                source_id: row.get(2)?,
                clip_id: row.get(3)?,
                attempts: row.get(4)?,
                queued_at: row.get(5)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())
}

fn ensure_job_service_schema(conn: &Connection) -> rusqlite::Result<()> {
    migrate_ingest_job_lease_columns(conn)
}

fn requeue_expired_leases(conn: &Connection, now_ms: u64) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE ingest_jobs
         SET status = 'queued',
             worker_id = '',
             lease_id = '',
             lease_until_ms = 0,
             heartbeat_ms = 0,
             updated_at = ?1
         WHERE status = 'processing'
           AND TRIM(COALESCE(lease_id, '')) <> ''
           AND lease_until_ms > 0
           AND lease_until_ms <= ?2",
        params![now_str(), now_ms as i64],
    )
}

fn normalize_capabilities(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| is_external_claimable_job_type(value))
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

fn is_external_claimable_job_type(job_type: &str) -> bool {
    EXTERNAL_CLAIMABLE_JOB_TYPES.contains(&job_type)
}

fn is_lease_managed_job_type(job_type: &str) -> bool {
    matches!(
        job_type,
        SMOKE_JOB_TYPE | PROXY_GENERATE_JOB_TYPE | FILMSTRIP_JOB_TYPE
    )
}

fn normalize_lease_ms(value: Option<u64>) -> u64 {
    value
        .unwrap_or(DEFAULT_LEASE_MS)
        .clamp(MIN_LEASE_MS, MAX_LEASE_MS)
}

fn required_id(name: &str, value: &str) -> Result<String, (StatusCode, String)> {
    let value = value.trim();
    if value.is_empty() {
        return Err((StatusCode::BAD_REQUEST, format!("{name} je prazan.")));
    }
    Ok(value.to_string())
}

fn required_runtime_id(name: &str, value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{name} je prazan."));
    }
    Ok(value.to_string())
}

fn truncate_error(value: &str) -> String {
    let value = value.trim();
    if value.len() > 240 {
        format!("{}...", value.chars().take(240).collect::<String>())
    } else {
        value.to_string()
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn internal_join_error(error: tokio::task::JoinError) -> (StatusCode, String) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("job service task failed: {error}"),
    )
}

fn internal_error(error: String) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, error)
}

fn request_error(error: String) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::db::queue_ingest_job;
    use qnc_service_contracts::{FrameTimebase, MediaProbe, ScanMode};
    use std::fs;

    fn test_paths(label: &str) -> ProjectPaths {
        let base =
            std::env::temp_dir().join(format!("qnc_job_service_{label}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        ProjectPaths {
            data_dir: base.join("data"),
            projects_root: base.join("projects"),
            seed_path: base.join("seed.json"),
        }
    }

    fn touch(path: PathBuf) -> PathBuf {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"media").unwrap();
        path
    }

    fn register_project(paths: &ProjectPaths, project_id: &str) {
        let global = crate::project::db::open_global(paths).unwrap();
        let project_dir = crate::project::db::project_dir_in_root(&paths.projects_root, project_id);
        global
            .execute(
                "INSERT INTO projects (project_id, name, project_dir)
                 VALUES (?1, ?2, ?3)",
                params![
                    project_id,
                    project_id,
                    project_dir.to_string_lossy().to_string()
                ],
            )
            .unwrap();
        crate::project::db::ensure_project_dirs_at(&project_dir).unwrap();
        let _ = crate::project::db::open_project(paths, project_id).unwrap();
    }

    fn field_project() -> serde_json::Value {
        json!({
            "settings": {
                "storage": {
                    "ingest_profile": "field",
                    "proxy_policy": "generate_if_missing"
                }
            }
        })
    }

    fn test_probe() -> MediaProbe {
        MediaProbe {
            width: 1920,
            height: 1080,
            duration_sec: Some(1.0),
            timebase: FrameTimebase {
                fps_num: 50,
                fps_den: 1,
            },
            scan_mode: ScanMode::Progressive,
            codec: "h264".into(),
            field_order: "progressive".into(),
            frame_count: Some(50),
            duration_frames: Some(50),
            has_video: true,
            has_audio: true,
            audio_channels: 2,
        }
    }

    fn lease_job(
        conn: &Connection,
        job_type: &str,
        source_id: &str,
        clip_id: &str,
        worker_id: &str,
        lease_id: &str,
    ) {
        queue_ingest_job(conn, job_type, source_id, clip_id).unwrap();
        ensure_job_service_schema(conn).unwrap();
        let job_id = crate::ingest::db::ingest_job_id(job_type, source_id, clip_id);
        conn.execute(
            "UPDATE ingest_jobs
             SET status = 'processing',
                 worker_id = ?2,
                 lease_id = ?3,
                 lease_until_ms = 999999999,
                 heartbeat_ms = 123
             WHERE job_id = ?1",
            params![job_id, worker_id, lease_id],
        )
        .unwrap();
    }

    #[test]
    fn proxy_preflight_uses_camera_proxy_before_generate() {
        let paths = test_paths("proxy_preflight_existing");
        let project_dir = paths.project_dir("project_a");
        let original = touch(project_dir.join("card").join("Clip0001.MXF"));
        let proxy = touch(project_dir.join("card").join("Sub").join("Clip0001S03.MP4"));
        let row = IngestAssetRow {
            source_id: "card".into(),
            clip_id: "clip0001".into(),
            source_path: original.to_string_lossy().to_string(),
            original_path: original.to_string_lossy().to_string(),
            proxy_path: proxy.to_string_lossy().to_string(),
            project_proxy_path: String::new(),
            card_thumb_path: String::new(),
            file_extension: "mxf".into(),
            read_from_card: 0,
            card_locked: 0,
            poster_source: String::new(),
            status: "generating_proxy".into(),
        };

        let decision =
            proxy_generate_preflight_from_row(&paths, "project_a", &row, &field_project()).unwrap();
        match decision {
            ProxyGeneratePreflight::ExistingProxy { source_path } => assert_eq!(source_path, proxy),
            ProxyGeneratePreflight::Generate(payload) => {
                panic!("expected existing proxy, got generate payload: {payload:?}")
            }
            ProxyGeneratePreflight::Skip { reason } => panic!("expected existing proxy: {reason}"),
        }
    }

    #[test]
    fn proxy_result_applier_records_generated_proxy_in_sqlite() {
        let paths = test_paths("proxy_apply");
        let broker = ProjectDbBroker::new(paths.clone());
        let project_dir = paths.project_dir("project_a");
        let original = touch(project_dir.join("card").join("Clip0002.MXF"));
        let proxy = touch(project_dir.join("proxy").join("clip0002.mxf"));
        let conn = open_ingest(&paths, "project_a").unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, source_path, original_path,
                 import_status, status, file_extension)
             VALUES ('card', 'clip0002', 'Clip0002', 'clip0002', ?1, ?1,
                     'generating_proxy', 'generating_proxy', 'mxf')",
            params![original.to_string_lossy().as_ref()],
        )
        .unwrap();
        queue_ingest_job(&conn, "proxy_generate", "card", "clip0002").unwrap();
        drop(conn);

        apply_proxy_generate_result(
            &paths,
            &broker,
            "project_a",
            "card",
            "clip0002",
            ProxyGenerateJobResult {
                output_path: proxy.clone(),
                probe: Some(test_probe()),
            },
        )
        .unwrap();

        let conn = open_ingest(&paths, "project_a").unwrap();
        let row: (String, String, f64) = conn
            .query_row(
                "SELECT import_status, project_proxy_path, fps
                 FROM ingest_assets
                 WHERE source_id = 'card' AND clip_id = 'clip0002'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let proxy_status: String = conn
            .query_row(
                "SELECT status FROM ingest_jobs WHERE job_id = ?1",
                params![crate::ingest::db::ingest_job_id(
                    "proxy_generate",
                    "card",
                    "clip0002"
                )],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(row.0, "imported");
        assert_eq!(row.1, proxy.to_string_lossy());
        assert_eq!(row.2, 50.0);
        assert_eq!(proxy_status, "done");
    }

    #[test]
    fn proxy_generate_complete_route_applies_result_and_clears_lease() {
        let paths = test_paths("proxy_complete_route");
        let broker = ProjectDbBroker::new(paths.clone());
        let project_dir = paths.project_dir("project_a");
        let original = touch(project_dir.join("card").join("Clip0003.MXF"));
        let proxy = touch(project_dir.join("proxy").join("clip0003.mxf"));
        let conn = open_ingest(&paths, "project_a").unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, source_path, original_path,
                 import_status, status, file_extension)
             VALUES ('card', 'clip0003', 'Clip0003', 'clip0003', ?1, ?1,
                     'generating_proxy', 'generating_proxy', 'mxf')",
            params![original.to_string_lossy().as_ref()],
        )
        .unwrap();
        lease_job(
            &conn,
            PROXY_GENERATE_JOB_TYPE,
            "card",
            "clip0003",
            "worker_a",
            "lease_a",
        );
        drop(conn);

        let heartbeat = heartbeat_jobs(
            paths.clone(),
            broker.clone(),
            JobHeartbeatRequest {
                worker_id: "worker_a".into(),
                project_id: "project_a".into(),
                lease_id: "lease_a".into(),
                job_ids: vec![crate::ingest::db::ingest_job_id(
                    PROXY_GENERATE_JOB_TYPE,
                    "card",
                    "clip0003",
                )],
                lease_ms: Some(10_000),
            },
        )
        .unwrap();
        assert_eq!(heartbeat.accepted.len(), 1);
        assert!(heartbeat.rejected.is_empty());

        let result = ProxyGenerateJobResult {
            output_path: proxy.clone(),
            probe: Some(test_probe()),
        };
        let job_id = crate::ingest::db::ingest_job_id(PROXY_GENERATE_JOB_TYPE, "card", "clip0003");
        let ack = complete_job(
            paths.clone(),
            broker,
            JobCompleteRequest {
                worker_id: "worker_a".into(),
                project_id: "project_a".into(),
                lease_id: "lease_a".into(),
                job_id: job_id.clone(),
                result: serde_json::to_value(result).unwrap(),
            },
        )
        .unwrap();
        assert!(ack.accepted);

        let conn = open_ingest(&paths, "project_a").unwrap();
        let asset: (String, String, f64) = conn
            .query_row(
                "SELECT import_status, project_proxy_path, fps
                 FROM ingest_assets
                 WHERE source_id = 'card' AND clip_id = 'clip0003'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let job: (String, String, String, String) = conn
            .query_row(
                "SELECT status, worker_id, lease_id, result_json
                 FROM ingest_jobs WHERE job_id = ?1",
                params![job_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(asset.0, "imported");
        assert_eq!(asset.1, proxy.to_string_lossy());
        assert_eq!(asset.2, 50.0);
        assert_eq!(job.0, "done");
        assert!(job.1.is_empty());
        assert!(job.2.is_empty());
        assert!(job.3.contains("clip0003.mxf"));
    }

    #[test]
    fn proxy_generate_is_external_claimable_after_handler_and_applier() {
        assert!(is_external_claimable_job_type(PROXY_GENERATE_JOB_TYPE));
    }

    #[test]
    fn filmstrip_is_external_claimable_after_handler_and_applier() {
        assert!(is_external_claimable_job_type(FILMSTRIP_JOB_TYPE));
    }

    #[test]
    fn filmstrip_claim_uses_proxy_media_and_segment_start_frames() {
        let paths = test_paths("filmstrip_claim");
        let broker = ProjectDbBroker::new(paths.clone());
        register_project(&paths, "project_a");
        let project_dir = paths.project_dir("project_a");
        let original = touch(project_dir.join("original").join("clip_a.mxf"));
        let proxy = touch(project_dir.join("proxy").join("clip_a.mp4"));
        let conn = open_ingest(&paths, "project_a").unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, source_path, original_path, proxy_path,
                 import_status, status, duration_sec, file_extension)
             VALUES ('card', 'clip_a', 'Clip A', 'clip_a', ?1, ?1, ?2,
                     'imported', 'imported', 26.0, 'mxf')",
            params![
                original.to_string_lossy().as_ref(),
                proxy.to_string_lossy().as_ref()
            ],
        )
        .unwrap();
        queue_ingest_job(&conn, FILMSTRIP_JOB_TYPE, FILMSTRIP_SOURCE_ID, "clip_a").unwrap();
        drop(conn);

        let claim = NormalizedClaim::from_request(JobClaimRequest {
            worker_id: "worker_a".into(),
            project_id: Some("project_a".into()),
            capabilities: vec![FILMSTRIP_JOB_TYPE.into()],
            max_jobs: Some(1),
            lease_ms: Some(10_000),
        })
        .unwrap();
        let jobs = claim_jobs(paths, broker, claim).unwrap();

        assert_eq!(jobs.len(), 1);
        let payload: FilmstripJobPayload = serde_json::from_value(jobs[0].payload.clone()).unwrap();
        assert_eq!(payload.media_path, proxy);
        assert_eq!(payload.duration_sec, 26.0);
        assert_eq!(
            payload.frames.len(),
            crate::filmstrip::DEFAULT_FILMSTRIP_FRAMES as usize
        );
        assert_eq!(payload.frames[0].seek_sec, 0.0);
        assert_eq!(payload.frames[1].seek_sec, 2.0);
        assert_eq!(
            payload.frames[0]
                .output_path
                .file_name()
                .and_then(|v| v.to_str()),
            Some("000_0_00.jpg")
        );
    }

    #[test]
    fn filmstrip_complete_route_stores_frames_and_clears_lease() {
        let paths = test_paths("filmstrip_complete");
        let broker = ProjectDbBroker::new(paths.clone());
        register_project(&paths, "project_a");
        let conn = open_ingest(&paths, "project_a").unwrap();
        lease_job(
            &conn,
            FILMSTRIP_JOB_TYPE,
            FILMSTRIP_SOURCE_ID,
            "clip_a",
            "worker_a",
            "lease_a",
        );
        drop(conn);

        let duration = 26.0;
        let seeks = crate::ingest::thumb::timeline_seek_seconds(
            duration,
            crate::filmstrip::DEFAULT_FILMSTRIP_FRAMES,
        );
        let out_dir = crate::filmstrip::filmstrip_clip_dir(&paths, "project_a", "clip_a");
        let frames: Vec<qnc_service_contracts::FilmstripFrameArtifact> = seeks
            .iter()
            .enumerate()
            .map(|(index, seek_sec)| {
                let path = crate::ingest::thumb::filmstrip_frame_path(&out_dir, index, *seek_sec);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, b"jpeg").unwrap();
                qnc_service_contracts::FilmstripFrameArtifact {
                    index,
                    seek_sec: *seek_sec,
                    artifact: qnc_service_contracts::ArtifactRef {
                        path,
                        media_type: "image/jpeg".into(),
                        render_version: None,
                    },
                }
            })
            .collect();
        let job_id =
            crate::ingest::db::ingest_job_id(FILMSTRIP_JOB_TYPE, FILMSTRIP_SOURCE_ID, "clip_a");
        let ack = complete_job(
            paths.clone(),
            broker,
            JobCompleteRequest {
                worker_id: "worker_a".into(),
                project_id: "project_a".into(),
                lease_id: "lease_a".into(),
                job_id: job_id.clone(),
                result: serde_json::to_value(FilmstripJobResult {
                    duration_sec: duration,
                    frames,
                })
                .unwrap(),
            },
        )
        .unwrap();

        assert!(ack.accepted);
        let manifest = crate::filmstrip::get_filmstrip(&paths, "project_a", "clip_a").unwrap();
        assert_eq!(
            manifest.get("status").and_then(|v| v.as_str()),
            Some("ready")
        );
        assert_eq!(
            crate::filmstrip::list_frames_for_clip(&paths, "project_a", "clip_a")
                .unwrap()
                .len(),
            crate::filmstrip::DEFAULT_FILMSTRIP_FRAMES as usize
        );
        let conn = open_ingest(&paths, "project_a").unwrap();
        let job: (String, String, String) = conn
            .query_row(
                "SELECT status, worker_id, lease_id
                 FROM ingest_jobs WHERE job_id = ?1",
                params![job_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(job.0, "done");
        assert!(job.1.is_empty());
        assert!(job.2.is_empty());
    }

    #[test]
    fn claim_skips_unprepared_proxy_generate_and_continues() {
        let paths = test_paths("claim");
        let broker = ProjectDbBroker::new(paths.clone());
        let conn = open_ingest(&paths, "project_a").unwrap();
        queue_ingest_job(&conn, "import", "card", "clip_a").unwrap();
        queue_ingest_job(&conn, "proxy_generate", "card", "clip_a").unwrap();
        queue_ingest_job(&conn, "qnc_worker_smoke", "worker", "clip_a").unwrap();
        drop(conn);

        let claim = NormalizedClaim::from_request(JobClaimRequest {
            worker_id: "worker_a".into(),
            project_id: Some("project_a".into()),
            capabilities: vec![
                "import".into(),
                "proxy_generate".into(),
                "qnc_worker_smoke".into(),
            ],
            max_jobs: Some(4),
            lease_ms: Some(10_000),
        })
        .unwrap();

        let jobs = claim_jobs(paths.clone(), broker.clone(), claim).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].job_type, "qnc_worker_smoke");
        assert_eq!(jobs[0].project_id, "project_a");
        assert!(!jobs[0].lease_id.is_empty());

        let conn = open_ingest(&paths, "project_a").unwrap();
        let smoke_status: String = conn
            .query_row(
                "SELECT status FROM ingest_jobs WHERE job_id = ?1",
                params![jobs[0].job_id],
                |row| row.get(0),
            )
            .unwrap();
        let proxy_status: String = conn
            .query_row(
                "SELECT status FROM ingest_jobs WHERE job_id = ?1",
                params![crate::ingest::db::ingest_job_id(
                    "proxy_generate",
                    "card",
                    "clip_a"
                )],
                |row| row.get(0),
            )
            .unwrap();
        let import_status: String = conn
            .query_row(
                "SELECT status FROM ingest_jobs WHERE job_id = ?1",
                params![crate::ingest::db::ingest_job_id("import", "card", "clip_a")],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(smoke_status, "processing");
        assert_eq!(proxy_status, "queued");
        assert_eq!(import_status, "queued");
    }

    #[test]
    fn heartbeat_and_complete_require_active_lease() {
        let paths = test_paths("complete");
        let broker = ProjectDbBroker::new(paths.clone());
        let conn = open_ingest(&paths, "project_a").unwrap();
        queue_ingest_job(&conn, "qnc_worker_smoke", "worker", "clip_a").unwrap();
        drop(conn);

        let claim = NormalizedClaim::from_request(JobClaimRequest {
            worker_id: "worker_a".into(),
            project_id: Some("project_a".into()),
            capabilities: vec!["qnc_worker_smoke".into()],
            max_jobs: Some(1),
            lease_ms: Some(10_000),
        })
        .unwrap();
        let job = claim_jobs(paths.clone(), broker.clone(), claim)
            .unwrap()
            .pop()
            .unwrap();

        let heartbeat = heartbeat_jobs(
            paths.clone(),
            broker.clone(),
            JobHeartbeatRequest {
                worker_id: job.worker_id.clone(),
                project_id: job.project_id.clone(),
                lease_id: job.lease_id.clone(),
                job_ids: vec![job.job_id.clone()],
                lease_ms: Some(10_000),
            },
        )
        .unwrap();
        assert_eq!(heartbeat.accepted, vec![job.job_id.clone()]);
        assert!(heartbeat.rejected.is_empty());

        let ack = complete_job(
            paths.clone(),
            broker,
            JobCompleteRequest {
                worker_id: job.worker_id,
                project_id: job.project_id,
                lease_id: job.lease_id,
                job_id: job.job_id.clone(),
                result: json!({"artifact": "ok"}),
            },
        )
        .unwrap();
        assert!(ack.accepted);

        let conn = open_ingest(&paths, "project_a").unwrap();
        let row: (String, String) = conn
            .query_row(
                "SELECT status, result_json FROM ingest_jobs WHERE job_id = ?1",
                params![job.job_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(row.0, "done");
        assert!(row.1.contains("artifact"));
    }
}
