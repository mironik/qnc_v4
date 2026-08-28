use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use qnc_service_contracts::{
    AudioWrapJobItem, AudioWrapJobPayload, AudioWrapJobResult, ExportHiResJobPayload,
    ExportHiResJobResult, FilmstripJobFrame, FilmstripJobPayload, FilmstripJobResult, JobAck,
    JobClaimRequest, JobClaimResponse, JobCompleteRequest, JobFailRequest, JobHeartbeatRequest,
    JobHeartbeatResponse, JobLease, MediaAccessKind, MediaLocator, MediaProbeJobPayload,
    MediaProbeJobResult, MediaResolveRequest, PosterJobPayload, PosterJobResult,
    ProxyGenerateJobPayload, ProxyGenerateJobResult, WaveformJobPayload, WaveformJobResult,
    JOB_SOURCE_EXPORT_HIRES, JOB_SOURCE_FILMSTRIP, JOB_SOURCE_WAVEFORM, JOB_TYPE_AUDIO_WRAP,
    JOB_TYPE_EXPORT_HIRES, JOB_TYPE_FILMSTRIP, JOB_TYPE_MEDIA_PROBE, JOB_TYPE_THUMB_PROXY,
    JOB_TYPE_WAVEFORM,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app_state::AppState;
use crate::ingest::asset_row::IngestAssetRow;
use crate::ingest::db::{
    get_meta, ingest_asset_meta, mark_ingest_job_done, mark_ingest_job_error,
    migrate_ingest_job_lease_columns, open_ingest, poster_exists,
    poster_proxy_generation_approved_for_asset, queue_ingest_artifact_job_once,
    set_thumb_ready_path, set_thumb_status, thumbnail_path,
};
use crate::ingest::import_finish::complete_imported_clip;
use crate::ingest::proxy_generate::proxy_dest_for_source;
use crate::ingest::store::{ingest_probe_from_service, row_import_error};
use crate::ingest::thumb_process::apply_card_poster_copy;
use crate::media::{
    find_card_proxy_for_media_path, is_proxy_media_path, resolve_import_plan, ImportMediaMode,
    ProjectMediaGateway,
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
const WAVEFORM_JOB_TYPE: &str = JOB_TYPE_WAVEFORM;
const WAVEFORM_SOURCE_ID: &str = JOB_SOURCE_WAVEFORM;
const THUMB_PROXY_JOB_TYPE: &str = JOB_TYPE_THUMB_PROXY;
const AUDIO_WRAP_JOB_TYPE: &str = JOB_TYPE_AUDIO_WRAP;
const MEDIA_PROBE_JOB_TYPE: &str = JOB_TYPE_MEDIA_PROBE;
const EXPORT_HIRES_JOB_TYPE: &str = JOB_TYPE_EXPORT_HIRES;
const EXPORT_HIRES_SOURCE_ID: &str = JOB_SOURCE_EXPORT_HIRES;
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/jobs/status", get(api_jobs_status))
        .route("/api/jobs/claim", post(api_jobs_claim))
        .route("/api/jobs/heartbeat", post(api_jobs_heartbeat))
        .route("/api/jobs/complete", post(api_jobs_complete))
        .route("/api/jobs/fail", post(api_jobs_fail))
}

async fn api_jobs_status(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let paths = state.project.paths.clone();
    let project_db = state.project_db.clone();
    let playback_active = state.background_work.playback_active();

    let response = tokio::task::spawn_blocking(move || {
        artifact_worker_status(paths, project_db, playback_active)
    })
    .await
    .map_err(internal_join_error)?
    .map_err(internal_error)?;
    Ok(Json(response))
}

async fn api_jobs_claim(
    State(state): State<AppState>,
    Json(body): Json<JobClaimRequest>,
) -> Result<Json<JobClaimResponse>, (StatusCode, String)> {
    let mut claim = NormalizedClaim::from_request(body)?;
    let playback_active = state.background_work.playback_active();
    if playback_active {
        claim
            .capabilities
            .retain(|job_type| is_claimable_during_playback(job_type));
        if claim.capabilities.is_empty() {
            return Ok(Json(JobClaimResponse {
                jobs: Vec::new(),
                playback_active: true,
                message: Some("playback_active".into()),
            }));
        }
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
    let media_gateway = state.media_gateway.clone();
    let jobs =
        tokio::task::spawn_blocking(move || claim_jobs(paths, project_db, media_gateway, claim))
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

#[derive(Debug, Default)]
struct ArtifactJobTypeCounts {
    queued: i64,
    processing: i64,
    active_leases: i64,
    expired_leases: i64,
    done: i64,
    error: i64,
}

impl ArtifactJobTypeCounts {
    fn add(&mut self, other: &Self) {
        self.queued += other.queued;
        self.processing += other.processing;
        self.active_leases += other.active_leases;
        self.expired_leases += other.expired_leases;
        self.done += other.done;
        self.error += other.error;
    }

    fn to_json(&self) -> Value {
        json!({
            "queued": self.queued,
            "processing": self.processing,
            "active_leases": self.active_leases,
            "expired_leases": self.expired_leases,
            "done": self.done,
            "error": self.error,
        })
    }
}

#[derive(Debug, Default)]
struct ArtifactWorkerSnapshot {
    playback_active: bool,
    projects_scanned: usize,
    proxy_generate: ArtifactJobTypeCounts,
    filmstrip: ArtifactJobTypeCounts,
    waveform: ArtifactJobTypeCounts,
    thumb_proxy: ArtifactJobTypeCounts,
    audio_wrap: ArtifactJobTypeCounts,
    media_probe: ArtifactJobTypeCounts,
    export_hires: ArtifactJobTypeCounts,
}

impl ArtifactWorkerSnapshot {
    fn queued(&self) -> i64 {
        self.proxy_generate.queued
            + self.filmstrip.queued
            + self.waveform.queued
            + self.thumb_proxy.queued
            + self.audio_wrap.queued
            + self.media_probe.queued
            + self.export_hires.queued
    }

    fn processing(&self) -> i64 {
        self.proxy_generate.processing
            + self.filmstrip.processing
            + self.waveform.processing
            + self.thumb_proxy.processing
            + self.audio_wrap.processing
            + self.media_probe.processing
            + self.export_hires.processing
    }

    fn active_leases(&self) -> i64 {
        self.proxy_generate.active_leases
            + self.filmstrip.active_leases
            + self.waveform.active_leases
            + self.thumb_proxy.active_leases
            + self.audio_wrap.active_leases
            + self.media_probe.active_leases
            + self.export_hires.active_leases
    }

    fn expired_leases(&self) -> i64 {
        self.proxy_generate.expired_leases
            + self.filmstrip.expired_leases
            + self.waveform.expired_leases
            + self.thumb_proxy.expired_leases
            + self.audio_wrap.expired_leases
            + self.media_probe.expired_leases
            + self.export_hires.expired_leases
    }

    fn missing_external_worker(&self) -> bool {
        !self.playback_active && self.queued() > 0 && self.active_leases() == 0
    }

    fn message(&self) -> &'static str {
        if self.playback_active {
            "playback_active"
        } else if self.missing_external_worker() {
            "external_artifact_worker_missing"
        } else if self.active_leases() > 0 {
            "external_artifact_worker_active"
        } else {
            "external_artifact_worker_idle"
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "status": if self.missing_external_worker() { "warn" } else { "ok" },
            "message": self.message(),
            "artifact_owner": "jobservice",
            "worker_required": true,
            "external_worker_missing": self.missing_external_worker(),
            "playback_active": self.playback_active,
            "projects_scanned": self.projects_scanned,
            "artifact_jobs": {
                "queued": self.queued(),
                "processing": self.processing(),
                "active_leases": self.active_leases(),
                "expired_leases": self.expired_leases(),
                "by_type": {
                    PROXY_GENERATE_JOB_TYPE: self.proxy_generate.to_json(),
                    FILMSTRIP_JOB_TYPE: self.filmstrip.to_json(),
                    WAVEFORM_JOB_TYPE: self.waveform.to_json(),
                    THUMB_PROXY_JOB_TYPE: self.thumb_proxy.to_json(),
                    AUDIO_WRAP_JOB_TYPE: self.audio_wrap.to_json(),
                    MEDIA_PROBE_JOB_TYPE: self.media_probe.to_json(),
                    EXPORT_HIRES_JOB_TYPE: self.export_hires.to_json(),
                },
            },
        })
    }
}

fn artifact_worker_status(
    paths: ProjectPaths,
    project_db: ProjectDbBroker,
    playback_active: bool,
) -> Result<Value, String> {
    let project_ids =
        project_db.with_global(|conn| list_project_ids(conn).map_err(|e| e.to_string()))?;
    let mut snapshot = ArtifactWorkerSnapshot {
        playback_active,
        ..Default::default()
    };

    for project_id in project_ids {
        let project_id = project_id.trim().to_string();
        if project_id.is_empty() {
            continue;
        }
        project_db.serialize_project_write(&project_id, || {
            let conn = open_ingest(&paths, &project_id).map_err(|e| e.to_string())?;
            ensure_job_service_schema(&conn).map_err(|e| e.to_string())?;
            let now_ms = now_unix_ms();
            let proxy = artifact_counts_for_type(&conn, PROXY_GENERATE_JOB_TYPE, now_ms)?;
            let filmstrip = artifact_counts_for_type(&conn, FILMSTRIP_JOB_TYPE, now_ms)?;
            let waveform = artifact_counts_for_type(&conn, WAVEFORM_JOB_TYPE, now_ms)?;
            let thumb_proxy = artifact_counts_for_type(&conn, THUMB_PROXY_JOB_TYPE, now_ms)?;
            let audio_wrap = artifact_counts_for_type(&conn, AUDIO_WRAP_JOB_TYPE, now_ms)?;
            let media_probe = artifact_counts_for_type(&conn, MEDIA_PROBE_JOB_TYPE, now_ms)?;
            let export_hires = artifact_counts_for_type(&conn, EXPORT_HIRES_JOB_TYPE, now_ms)?;
            snapshot.proxy_generate.add(&proxy);
            snapshot.filmstrip.add(&filmstrip);
            snapshot.waveform.add(&waveform);
            snapshot.thumb_proxy.add(&thumb_proxy);
            snapshot.audio_wrap.add(&audio_wrap);
            snapshot.media_probe.add(&media_probe);
            snapshot.export_hires.add(&export_hires);
            snapshot.projects_scanned += 1;
            Ok(())
        })?;
    }

    Ok(snapshot.to_json())
}

fn artifact_counts_for_type(
    conn: &Connection,
    job_type: &str,
    now_ms: u64,
) -> Result<ArtifactJobTypeCounts, String> {
    let queued = artifact_count(conn, job_type, "status = 'queued'", CountTimeParam::None)?;
    let processing = artifact_count(
        conn,
        job_type,
        "status = 'processing'",
        CountTimeParam::None,
    )?;
    let active_leases = artifact_count(
        conn,
        job_type,
        "status = 'processing' AND COALESCE(lease_until_ms, 0) > ?2",
        CountTimeParam::Now(now_ms),
    )?;
    let expired_leases = artifact_count(
        conn,
        job_type,
        "status = 'processing' AND COALESCE(lease_until_ms, 0) > 0 AND COALESCE(lease_until_ms, 0) <= ?2",
        CountTimeParam::Now(now_ms),
    )?;
    let done = artifact_count(conn, job_type, "status = 'done'", CountTimeParam::None)?;
    let error = artifact_count(conn, job_type, "status = 'error'", CountTimeParam::None)?;
    Ok(ArtifactJobTypeCounts {
        queued,
        processing,
        active_leases,
        expired_leases,
        done,
        error,
    })
}

enum CountTimeParam {
    None,
    Now(u64),
}

fn artifact_count(
    conn: &Connection,
    job_type: &str,
    status_filter: &str,
    time_param: CountTimeParam,
) -> Result<i64, String> {
    let sql = format!(
        "SELECT COUNT(*)
         FROM ingest_jobs
         WHERE job_type = ?1 AND {status_filter}"
    );
    match time_param {
        CountTimeParam::None => conn
            .query_row(&sql, params![job_type], |row| row.get(0))
            .map_err(|e| e.to_string()),
        CountTimeParam::Now(now_ms) => conn
            .query_row(&sql, params![job_type, now_ms as i64], |row| row.get(0))
            .map_err(|e| e.to_string()),
    }
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
    media_gateway: ProjectMediaGateway,
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
        let mut leases = claim_jobs_for_project(
            &paths,
            &project_db,
            &media_gateway,
            &project_id,
            &claim,
            remaining,
        )?;
        out.append(&mut leases);
    }
    Ok(out)
}

fn claim_jobs_for_project(
    paths: &ProjectPaths,
    project_db: &ProjectDbBroker,
    media_gateway: &ProjectMediaGateway,
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
            if !is_claimable_job_type(job_type) {
                continue;
            }
            let remaining = limit - claimed.len();
            let rows = queued_jobs_for_type(&conn, job_type, remaining)?;
            for row in rows {
                if claimed.len() >= limit {
                    break;
                }
                let Some(payload) = payload_for_job_claim(paths, media_gateway, &pid, &conn, &row)?
                else {
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
    media_gateway: &ProjectMediaGateway,
    project_id: &str,
    conn: &Connection,
    row: &QueuedJobRow,
) -> Result<Option<Value>, String> {
    match row.job_type.as_str() {
        SMOKE_JOB_TYPE => Ok(Some(json!({}))),
        PROXY_GENERATE_JOB_TYPE => payload_for_proxy_generate_claim(paths, project_id, conn, row),
        FILMSTRIP_JOB_TYPE => {
            payload_for_filmstrip_claim(paths, media_gateway, project_id, conn, row)
        }
        WAVEFORM_JOB_TYPE => {
            payload_for_waveform_claim(paths, media_gateway, project_id, conn, row)
        }
        THUMB_PROXY_JOB_TYPE => {
            payload_for_thumb_proxy_claim(paths, media_gateway, project_id, conn, row)
        }
        AUDIO_WRAP_JOB_TYPE => payload_for_audio_wrap_claim(paths, project_id, conn, row),
        MEDIA_PROBE_JOB_TYPE => payload_for_media_probe_claim(project_id, conn, row),
        EXPORT_HIRES_JOB_TYPE => payload_for_hires_render_claim(
            conn,
            row,
            EXPORT_HIRES_JOB_TYPE,
            EXPORT_HIRES_SOURCE_ID,
            "export_hires",
        ),
        _ => Ok(None),
    }
}

fn payload_for_hires_render_claim(
    conn: &Connection,
    row: &QueuedJobRow,
    expected_job_type: &str,
    expected_source_id: &str,
    label: &str,
) -> Result<Option<Value>, String> {
    let source_id = row.source_id.trim();
    if source_id != expected_source_id {
        let _ = mark_ingest_job_error(
            conn,
            expected_job_type,
            source_id,
            &row.clip_id,
            &format!("{label} job source nije {expected_source_id}"),
        );
        return Ok(None);
    }
    let payload: ExportHiResJobPayload = match serde_json::from_str(&row.payload_json) {
        Ok(payload) => payload,
        Err(error) => {
            let _ = mark_ingest_job_error(
                conn,
                expected_job_type,
                source_id,
                &row.clip_id,
                &format!("invalid {label} payload: {error}"),
            );
            return Ok(None);
        }
    };
    if payload.items.is_empty() {
        let _ = mark_ingest_job_error(
            conn,
            expected_job_type,
            source_id,
            &row.clip_id,
            &format!("{label} payload nema snapshot flat playlist iteme"),
        );
        return Ok(None);
    }
    serde_json::to_value(payload)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn payload_for_thumb_proxy_claim(
    paths: &ProjectPaths,
    media_gateway: &ProjectMediaGateway,
    project_id: &str,
    conn: &Connection,
    row: &QueuedJobRow,
) -> Result<Option<Value>, String> {
    let source_id = row.source_id.trim();
    let clip_id = row.clip_id.trim();
    if source_id.is_empty() || clip_id.is_empty() {
        return Ok(None);
    }
    let asset = match read_thumb_asset_row(conn, source_id, clip_id) {
        Ok(asset) => asset,
        Err(error) => {
            let _ = mark_ingest_job_error(conn, THUMB_PROXY_JOB_TYPE, source_id, clip_id, &error);
            return Ok(None);
        }
    };
    if !matches!(
        asset.status.as_str(),
        "pending" | "processing" | "no_card_thumb" | "error"
    ) {
        return Ok(None);
    }

    let poster = thumbnail_path(paths, project_id, clip_id);
    if poster_exists(&poster) {
        set_thumb_ready_path(conn, source_id, clip_id, &poster).map_err(|e| e.to_string())?;
        let _ = mark_ingest_job_done(conn, THUMB_PROXY_JOB_TYPE, source_id, clip_id);
        return Ok(None);
    }
    if complete_card_poster_copy_if_available(paths, project_id, conn, &asset)? {
        let _ = mark_ingest_job_done(conn, THUMB_PROXY_JOB_TYPE, source_id, clip_id);
        return Ok(None);
    }
    if !poster_proxy_generation_approved_for_asset(conn, source_id, clip_id)
        .map_err(|e| e.to_string())?
    {
        let message = "proxy poster generation requires user approval";
        set_thumb_status(conn, source_id, clip_id, "no_card_thumb", message)
            .map_err(|e| e.to_string())?;
        let _ = mark_ingest_job_done(conn, THUMB_PROXY_JOB_TYPE, source_id, clip_id);
        return Ok(None);
    }

    let media = match media_gateway.resolve_sync(MediaResolveRequest {
        project_id: project_id.to_string(),
        clip_id: clip_id.to_string(),
        access: MediaAccessKind::PosterSource,
        fallback: None,
    }) {
        Ok(response) => match response.media.locator {
            MediaLocator::LocalPath { path } => path,
            MediaLocator::IntranetPath { .. } | MediaLocator::ManagedAsset { .. } => {
                let message = "poster media is not local to this worker";
                set_thumb_status(conn, source_id, clip_id, "error", message)
                    .map_err(|e| e.to_string())?;
                let _ =
                    mark_ingest_job_error(conn, THUMB_PROXY_JOB_TYPE, source_id, clip_id, message);
                return Ok(None);
            }
        },
        Err(error) => {
            set_thumb_status(conn, source_id, clip_id, "error", &error.message)
                .map_err(|e| e.to_string())?;
            let _ = mark_ingest_job_error(
                conn,
                THUMB_PROXY_JOB_TYPE,
                source_id,
                clip_id,
                &error.message,
            );
            return Ok(None);
        }
    };

    set_thumb_status(conn, source_id, clip_id, "processing", "").map_err(|e| e.to_string())?;
    serde_json::to_value(PosterJobPayload {
        media_path: media,
        output_path: poster,
        seek_sec: 0.0,
    })
    .map(Some)
    .map_err(|error| error.to_string())
}

fn payload_for_audio_wrap_claim(
    paths: &ProjectPaths,
    project_id: &str,
    conn: &Connection,
    row: &QueuedJobRow,
) -> Result<Option<Value>, String> {
    let source_id = row.source_id.trim();
    let clip_id = row.clip_id.trim();
    if source_id.is_empty() || clip_id.is_empty() {
        return Ok(None);
    }
    let settings = crate::project::db::project_effective_settings(paths, project_id);
    let region = crate::ingest::audio_wrap::broadcast_region_from_settings(&settings);
    let needed = crate::ingest::audio_wrap::needed_wrap_rates_from_conn(conn, region)?;
    if needed.is_empty() {
        let _ = mark_ingest_job_done(conn, AUDIO_WRAP_JOB_TYPE, source_id, clip_id);
        return Ok(None);
    }

    let row_data = conn
        .query_row(
            "SELECT original_path, source_path, COALESCE(metadata_json, '{}')
             FROM ingest_assets
             WHERE source_id = ?1 AND clip_id = ?2
               AND import_status IN ('imported', 'done')",
            params![source_id, clip_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((original_path, source_path, meta_raw)) = row_data else {
        let _ = mark_ingest_job_error(
            conn,
            AUDIO_WRAP_JOB_TYPE,
            source_id,
            clip_id,
            "audio wrap asset row missing",
        );
        return Ok(None);
    };
    let meta: Value = serde_json::from_str(&meta_raw).unwrap_or_else(|_| json!({}));
    let has_audio_meta = meta
        .get("audio_project_path")
        .and_then(|v| v.as_str())
        .is_some();
    let audio_dir = crate::ingest::audio_wrap::audio_project_dir(paths, project_id);
    let Some(media_path) = crate::ingest::audio_wrap::resolve_audio_source_for_asset(
        &audio_dir,
        clip_id,
        &original_path,
        &source_path,
        &meta,
    ) else {
        let _ = mark_ingest_job_error(
            conn,
            AUDIO_WRAP_JOB_TYPE,
            source_id,
            clip_id,
            "audio wrap source missing",
        );
        return Ok(None);
    };
    if !has_audio_meta && !crate::media::is_audio_media_file(&media_path) {
        let _ = mark_ingest_job_error(
            conn,
            AUDIO_WRAP_JOB_TYPE,
            source_id,
            clip_id,
            "audio wrap source is not audio media",
        );
        return Ok(None);
    }

    let proxy_dir = paths.project_dir(project_id).join("proxy");
    let existing = crate::ingest::audio_wrap::audio_wraps_from_meta(&meta);
    let mut wraps = Vec::new();
    for rate in needed {
        let tag = crate::ingest::audio_wrap::fps_path_tag(rate);
        if let Some(path) = existing.get(&tag) {
            if PathBuf::from(path).is_file() {
                continue;
            }
        }
        wraps.push(AudioWrapJobItem {
            fps: rate,
            output_path: crate::ingest::audio_wrap::audio_wrap_dest_for_fps(
                &proxy_dir, clip_id, rate,
            ),
        });
    }
    if wraps.is_empty() {
        let _ = mark_ingest_job_done(conn, AUDIO_WRAP_JOB_TYPE, source_id, clip_id);
        return Ok(None);
    }

    serde_json::to_value(AudioWrapJobPayload { media_path, wraps })
        .map(Some)
        .map_err(|error| error.to_string())
}

fn payload_for_media_probe_claim(
    _project_id: &str,
    conn: &Connection,
    row: &QueuedJobRow,
) -> Result<Option<Value>, String> {
    let source_id = row.source_id.trim();
    let clip_id = row.clip_id.trim();
    if source_id.is_empty() || clip_id.is_empty() {
        return Ok(None);
    }
    let data = conn
        .query_row(
            "SELECT source_path, original_path, proxy_path, project_proxy_path
             FROM ingest_assets
             WHERE source_id = ?1 AND clip_id = ?2
               AND (duration_sec IS NULL OR duration_sec <= 0)",
            params![source_id, clip_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((source_path, original_path, proxy_path, project_proxy_path)) = data else {
        let _ = mark_ingest_job_done(conn, MEDIA_PROBE_JOB_TYPE, source_id, clip_id);
        return Ok(None);
    };
    let media_path = [project_proxy_path, proxy_path, original_path, source_path]
        .into_iter()
        .map(|s| s.trim().to_string())
        .find(|s| !s.is_empty() && PathBuf::from(s).is_file())
        .map(PathBuf::from);
    let Some(media_path) = media_path else {
        let _ = mark_ingest_job_error(
            conn,
            MEDIA_PROBE_JOB_TYPE,
            source_id,
            clip_id,
            "media probe source missing",
        );
        let _ = crate::ingest::store::finish_duration_probe_if_idle_conn(conn, source_id);
        return Ok(None);
    };

    serde_json::to_value(MediaProbeJobPayload { media_path })
        .map(Some)
        .map_err(|error| error.to_string())
}

fn read_thumb_asset_row(
    conn: &Connection,
    source_id: &str,
    clip_id: &str,
) -> Result<IngestAssetRow, String> {
    conn.query_row(
        "SELECT source_id, clip_id, source_path, original_path, proxy_path,
                project_proxy_path, card_thumb_path, file_extension,
                read_from_card, card_locked, poster_source, thumb_status
         FROM ingest_assets
         WHERE source_id = ?1 AND clip_id = ?2",
        params![source_id, clip_id],
        IngestAssetRow::from_row,
    )
    .map_err(|e| e.to_string())
}

fn complete_card_poster_copy_if_available(
    paths: &ProjectPaths,
    project_id: &str,
    conn: &Connection,
    row: &IngestAssetRow,
) -> Result<bool, String> {
    let card_root_raw = get_meta(conn, "card_root", "").unwrap_or_default();
    let card_root = if card_root_raw.trim().is_empty() {
        None
    } else {
        Some(PathBuf::from(card_root_raw.trim()))
    };
    let mut meta = ingest_asset_meta(&row.meta_input());
    if !apply_card_poster_copy(
        paths,
        project_id,
        &row.clip_id,
        &mut meta,
        card_root.as_deref(),
    ) {
        return Ok(false);
    }
    let card_thumb = meta
        .get("card_thumb_path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let poster_src = meta
        .get("poster_source")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let thumb_path = thumbnail_path(paths, project_id, &row.clip_id)
        .to_string_lossy()
        .to_string();
    conn.execute(
        "UPDATE ingest_assets SET
            thumb_status = 'ready',
            thumb_error = '',
            thumb_path = ?3,
            card_thumb_path = ?4,
            poster_source = ?5,
            metadata_json = '{}'
         WHERE source_id = ?1 AND clip_id = ?2",
        params![
            row.source_id,
            row.clip_id,
            thumb_path,
            card_thumb,
            poster_src
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(true)
}

fn payload_for_waveform_claim(
    paths: &ProjectPaths,
    media_gateway: &ProjectMediaGateway,
    project_id: &str,
    conn: &Connection,
    row: &QueuedJobRow,
) -> Result<Option<Value>, String> {
    let clip_id = row.clip_id.trim();
    if clip_id.is_empty() || row.source_id.trim() != WAVEFORM_SOURCE_ID {
        return Ok(None);
    }
    if crate::waveform::ready(paths, project_id, clip_id) {
        let _ = mark_ingest_job_done(conn, WAVEFORM_JOB_TYPE, WAVEFORM_SOURCE_ID, clip_id);
        return Ok(None);
    }
    let media = match media_gateway.resolve_sync(MediaResolveRequest {
        project_id: project_id.to_string(),
        clip_id: clip_id.to_string(),
        access: MediaAccessKind::WaveformSource,
        fallback: None,
    }) {
        Ok(response) => match response.media.locator {
            MediaLocator::LocalPath { path } => path,
            MediaLocator::IntranetPath { .. } | MediaLocator::ManagedAsset { .. } => {
                let _ = mark_ingest_job_error(
                    conn,
                    WAVEFORM_JOB_TYPE,
                    WAVEFORM_SOURCE_ID,
                    clip_id,
                    "waveform media is not local to this worker",
                );
                return Ok(None);
            }
        },
        Err(error) => {
            let _ = mark_ingest_job_error(
                conn,
                WAVEFORM_JOB_TYPE,
                WAVEFORM_SOURCE_ID,
                clip_id,
                &error.message,
            );
            return Ok(None);
        }
    };

    serde_json::to_value(WaveformJobPayload {
        media_path: media,
        peak_buckets: crate::waveform::PEAK_BUCKETS,
        sample_rate_hz: crate::waveform::WAVEFORM_SAMPLE_RATE_HZ,
    })
    .map(Some)
    .map_err(|error| error.to_string())
}

fn payload_for_filmstrip_claim(
    paths: &ProjectPaths,
    media_gateway: &ProjectMediaGateway,
    project_id: &str,
    conn: &Connection,
    row: &QueuedJobRow,
) -> Result<Option<Value>, String> {
    let clip_id = row.clip_id.trim();
    if clip_id.is_empty() || row.source_id.trim() != FILMSTRIP_SOURCE_ID {
        return Ok(None);
    }
    let media = match media_gateway.resolve_sync(MediaResolveRequest {
        project_id: project_id.to_string(),
        clip_id: clip_id.to_string(),
        access: MediaAccessKind::FilmstripSource,
        fallback: None,
    }) {
        Ok(response) => match response.media.locator {
            MediaLocator::LocalPath { path } => path,
            MediaLocator::IntranetPath { .. } | MediaLocator::ManagedAsset { .. } => {
                let _ = mark_ingest_job_error(
                    conn,
                    FILMSTRIP_JOB_TYPE,
                    FILMSTRIP_SOURCE_ID,
                    clip_id,
                    "filmstrip media is not local to this worker",
                );
                return Ok(None);
            }
        },
        Err(error) => {
            let _ = mark_ingest_job_error(
                conn,
                FILMSTRIP_JOB_TYPE,
                FILMSTRIP_SOURCE_ID,
                clip_id,
                &error.message,
            );
            return Ok(None);
        }
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
        if !crate::filmstrip::filmstrip_ready(paths, &pid, &cid) {
            queue_ingest_artifact_job_once(&conn, FILMSTRIP_JOB_TYPE, JOB_SOURCE_FILMSTRIP, &cid)
                .map_err(|e| e.to_string())?;
        }
        if !crate::waveform::ready(paths, &pid, &cid) {
            queue_ingest_artifact_job_once(&conn, WAVEFORM_JOB_TYPE, JOB_SOURCE_WAVEFORM, &cid)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    })?;
    let _ = project_db.serialize_project_write(&pid, || {
        crate::virtual_shots::ensure_root_virtual_shots(paths, &pid)
    });
    let _ = crate::ingest::audio_wrap::queue_project_audio_wrap_jobs(paths, project_db, &pid);
    Ok(())
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

fn apply_thumb_proxy_job_result(
    paths: &ProjectPaths,
    project_db: &ProjectDbBroker,
    project_id: &str,
    source_id: &str,
    clip_id: &str,
    result: PosterJobResult,
) -> Result<(), String> {
    if !result.output_path.is_file() {
        return Err(format!(
            "generated poster is missing: {}",
            result.output_path.display()
        ));
    }
    let expected = thumbnail_path(paths, project_id, clip_id);
    let poster = if result.output_path == expected {
        result.output_path
    } else {
        expected
    };
    if !poster.is_file() {
        return Err(format!("poster is missing: {}", poster.display()));
    }
    let thumb_path = poster.to_string_lossy().to_string();
    project_db.serialize_project_write(project_id, || {
        let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE ingest_assets SET
                thumb_status = 'ready',
                thumb_error = '',
                thumb_path = ?3,
                poster_source = 'proxy_ffmpeg',
                metadata_json = '{}'
             WHERE source_id = ?1 AND clip_id = ?2",
            params![source_id, clip_id, thumb_path],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    })
}

fn apply_audio_wrap_job_result(
    paths: &ProjectPaths,
    project_db: &ProjectDbBroker,
    project_id: &str,
    source_id: &str,
    clip_id: &str,
    result: AudioWrapJobResult,
) -> Result<(), String> {
    let settings = crate::project::db::project_effective_settings(paths, project_id);
    let region = crate::ingest::audio_wrap::broadcast_region_from_settings(&settings);
    if result.wraps.is_empty() {
        return Err("audio_wrap result has no wraps".into());
    }
    for wrap in result.wraps {
        if !wrap.output_path.is_file()
            || wrap.output_path.metadata().map(|m| m.len()).unwrap_or(0) == 0
        {
            return Err(format!(
                "audio wrap is missing: {}",
                wrap.output_path.display()
            ));
        }
        let probe = wrap.probe.and_then(ingest_probe_from_service);
        crate::ingest::audio_wrap::record_audio_wrap(
            paths,
            project_db,
            project_id,
            source_id,
            clip_id,
            wrap.fps,
            &wrap.output_path,
            region,
            probe.as_ref(),
        )?;
    }
    Ok(())
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
                   AND status = 'processing'",
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
        WAVEFORM_JOB_TYPE => {
            let result: WaveformJobResult = serde_json::from_value(request.result)
                .map_err(|error| format!("invalid waveform result for job_id={job_id}: {error}"))?;
            crate::waveform::save_waveform_job_result(
                &paths,
                &project_db,
                &project_id,
                &active.clip_id,
                result,
            )?;
        }
        THUMB_PROXY_JOB_TYPE => {
            let result: PosterJobResult =
                serde_json::from_value(request.result).map_err(|error| {
                    format!("invalid thumb_proxy result for job_id={job_id}: {error}")
                })?;
            apply_thumb_proxy_job_result(
                &paths,
                &project_db,
                &project_id,
                &active.source_id,
                &active.clip_id,
                result,
            )?;
        }
        AUDIO_WRAP_JOB_TYPE => {
            let result: AudioWrapJobResult =
                serde_json::from_value(request.result).map_err(|error| {
                    format!("invalid audio_wrap result for job_id={job_id}: {error}")
                })?;
            apply_audio_wrap_job_result(
                &paths,
                &project_db,
                &project_id,
                &active.source_id,
                &active.clip_id,
                result,
            )?;
        }
        MEDIA_PROBE_JOB_TYPE => {
            let result: MediaProbeJobResult =
                serde_json::from_value(request.result).map_err(|error| {
                    format!("invalid media_probe result for job_id={job_id}: {error}")
                })?;
            crate::ingest::store::record_media_probe_result(
                &paths,
                &project_db,
                &project_id,
                &active.source_id,
                &active.clip_id,
                result.probe,
            )?;
        }
        EXPORT_HIRES_JOB_TYPE => {
            let result: ExportHiResJobResult =
                serde_json::from_value(request.result).map_err(|error| {
                    format!("invalid export_hires result for job_id={job_id}: {error}")
                })?;
            if !result.output_path.is_file() {
                return Err(format!(
                    "export_hires output missing for job_id={job_id}: {}",
                    result.output_path.display()
                ));
            }
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
    if changed == 1 && active.job_type == MEDIA_PROBE_JOB_TYPE {
        crate::ingest::store::finish_duration_probe_if_idle(
            &paths,
            &project_db,
            &project_id,
            &active.source_id,
        )?;
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
    if changed == 1 && !request.retryable && active.job_type == WAVEFORM_JOB_TYPE {
        crate::waveform::mark_waveform_error(
            &paths,
            &project_db,
            &project_id,
            &active.clip_id,
            &error,
        )?;
    }
    if changed == 1 && !request.retryable && active.job_type == THUMB_PROXY_JOB_TYPE {
        project_db.serialize_project_write(&project_id, || {
            let conn = open_ingest(&paths, &project_id).map_err(|e| e.to_string())?;
            set_thumb_status(&conn, &active.source_id, &active.clip_id, "error", &error)
                .map_err(|e| e.to_string())
        })?;
    }
    if changed == 1 && !request.retryable && active.job_type == AUDIO_WRAP_JOB_TYPE {
        crate::ingest::audio_wrap::mark_audio_wrap_error(
            &paths,
            &project_db,
            &project_id,
            &active.source_id,
            &active.clip_id,
            &error,
        )?;
    }
    if changed == 1 && !request.retryable && active.job_type == MEDIA_PROBE_JOB_TYPE {
        crate::ingest::store::finish_duration_probe_if_idle(
            &paths,
            &project_db,
            &project_id,
            &active.source_id,
        )?;
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
    payload_json: String,
}

fn queued_jobs_for_type(
    conn: &Connection,
    job_type: &str,
    limit: usize,
) -> Result<Vec<QueuedJobRow>, String> {
    let limit = limit.max(1) as i64;
    let sql = format!(
        "SELECT job_id, job_type, source_id, clip_id, attempts, queued_at, COALESCE(payload_json, '{{}}')
         FROM ingest_jobs
         WHERE job_type = ?1 AND status = 'queued'
         ORDER BY queued_at ASC, updated_at ASC, job_id ASC
         LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![job_type, limit], |row| {
            Ok(QueuedJobRow {
                job_id: row.get(0)?,
                job_type: row.get(1)?,
                source_id: row.get(2)?,
                clip_id: row.get(3)?,
                attempts: row.get(4)?,
                queued_at: row.get(5)?,
                payload_json: row.get(6)?,
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
        .filter(|value| is_claimable_job_type(value))
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

fn is_claimable_job_type(job_type: &str) -> bool {
    matches!(job_type, SMOKE_JOB_TYPE)
        || matches!(
            job_type,
            PROXY_GENERATE_JOB_TYPE
                | FILMSTRIP_JOB_TYPE
                | WAVEFORM_JOB_TYPE
                | THUMB_PROXY_JOB_TYPE
                | AUDIO_WRAP_JOB_TYPE
                | MEDIA_PROBE_JOB_TYPE
                | EXPORT_HIRES_JOB_TYPE
        )
}

fn is_claimable_during_playback(job_type: &str) -> bool {
    matches!(job_type, EXPORT_HIRES_JOB_TYPE)
}

fn is_lease_managed_job_type(job_type: &str) -> bool {
    matches!(
        job_type,
        SMOKE_JOB_TYPE
            | PROXY_GENERATE_JOB_TYPE
            | FILMSTRIP_JOB_TYPE
            | WAVEFORM_JOB_TYPE
            | THUMB_PROXY_JOB_TYPE
            | AUDIO_WRAP_JOB_TYPE
            | MEDIA_PROBE_JOB_TYPE
            | EXPORT_HIRES_JOB_TYPE
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
    use qnc_service_contracts::{
        ExportHiResPlaylistItem, ExportHiResPlaylistSource, FrameTimebase, MediaProbe, ScanMode,
    };
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

    fn test_media_gateway(paths: &ProjectPaths) -> ProjectMediaGateway {
        ProjectMediaGateway::new(
            paths.clone(),
            qnc_service_contracts::IntegrationGatewayKind::LocalFs,
            true,
        )
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
        queue_ingest_job(&conn, WAVEFORM_JOB_TYPE, WAVEFORM_SOURCE_ID, "clip0002").unwrap();
        conn.execute(
            "UPDATE ingest_jobs
             SET status = 'processing',
                 queued_at = 'waveform-original-queued',
                 started_at = 'waveform-original-started',
                 worker_id = 'waveform-worker'
             WHERE job_type = ?1 AND source_id = ?2 AND clip_id = 'clip0002'",
            params![WAVEFORM_JOB_TYPE, WAVEFORM_SOURCE_ID],
        )
        .unwrap();
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
        let waveform_job: (String, String, String) = conn
            .query_row(
                "SELECT status, queued_at, worker_id
                 FROM ingest_jobs
                 WHERE job_type = ?1 AND source_id = ?2 AND clip_id = 'clip0002'",
                params![WAVEFORM_JOB_TYPE, WAVEFORM_SOURCE_ID],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            waveform_job,
            (
                "processing".into(),
                "waveform-original-queued".into(),
                "waveform-worker".into()
            )
        );
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
    fn proxy_generate_is_claimable_by_jobservice() {
        assert!(is_claimable_job_type(PROXY_GENERATE_JOB_TYPE));
    }

    #[test]
    fn filmstrip_is_claimable_by_jobservice() {
        assert!(is_claimable_job_type(FILMSTRIP_JOB_TYPE));
    }

    #[test]
    fn waveform_is_claimable_by_jobservice() {
        assert!(is_claimable_job_type(WAVEFORM_JOB_TYPE));
    }

    #[test]
    fn audio_wrap_is_claimable_by_jobservice() {
        assert!(is_claimable_job_type(AUDIO_WRAP_JOB_TYPE));
    }

    #[test]
    fn media_probe_is_claimable_by_jobservice() {
        assert!(is_claimable_job_type(MEDIA_PROBE_JOB_TYPE));
    }

    #[test]
    fn playback_gate_allows_user_initiated_hires_render_only() {
        assert!(is_claimable_during_playback(EXPORT_HIRES_JOB_TYPE));
        assert!(!is_claimable_during_playback(FILMSTRIP_JOB_TYPE));
        assert!(!is_claimable_during_playback(WAVEFORM_JOB_TYPE));
        assert!(!is_claimable_during_playback(PROXY_GENERATE_JOB_TYPE));
    }

    #[test]
    fn export_hires_claim_uses_stored_flat_payload() {
        let paths = test_paths("export_hires_claim");
        let broker = ProjectDbBroker::new(paths.clone());
        register_project(&paths, "project_a");
        let conn = open_ingest(&paths, "project_a").unwrap();
        let payload = ExportHiResJobPayload {
            project_id: "project_a".into(),
            export_id: "hires_a".into(),
            output_path: paths
                .project_dir("project_a")
                .join("exports")
                .join("master.mov"),
            timeline_timebase: FrameTimebase {
                fps_num: 50,
                fps_den: 1,
            },
            duration_frames: 50,
            items: vec![ExportHiResPlaylistItem {
                item_id: "item:0-50".into(),
                record_in_frame: 0,
                record_out_frame: 50,
                sources: vec![ExportHiResPlaylistSource {
                    source_id: "part:p1:base_video".into(),
                    source_kind: "base_video".into(),
                    clip_id: "clip_a".into(),
                    virtual_shot_id: "shot_a".into(),
                    original_path: paths.project_dir("project_a").join("original.mxf"),
                    source_in_frame: 10,
                    source_out_frame: 60,
                    source_timebase: FrameTimebase {
                        fps_num: 50,
                        fps_den: 1,
                    },
                    has_video: true,
                    has_audio: false,
                    audio_output_channel: None,
                }],
            }],
        };
        crate::ingest::db::queue_ingest_job_payload(
            &conn,
            EXPORT_HIRES_JOB_TYPE,
            EXPORT_HIRES_SOURCE_ID,
            "hires_a",
            &serde_json::to_value(&payload).unwrap(),
        )
        .unwrap();
        drop(conn);

        let claim = NormalizedClaim::from_request(JobClaimRequest {
            worker_id: "worker_a".into(),
            placement: None,
            project_id: Some("project_a".into()),
            capabilities: vec![EXPORT_HIRES_JOB_TYPE.into()],
            max_jobs: Some(1),
            lease_ms: Some(10_000),
        })
        .unwrap();
        let gateway = test_media_gateway(&paths);
        let jobs = claim_jobs(paths, broker, gateway, claim).unwrap();

        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].job_type, EXPORT_HIRES_JOB_TYPE);
        let claimed: ExportHiResJobPayload =
            serde_json::from_value(jobs[0].payload.clone()).unwrap();
        assert_eq!(claimed.export_id, "hires_a");
        assert_eq!(claimed.items[0].sources[0].source_in_frame, 10);
    }

    #[test]
    fn export_hires_claim_rejects_payload_without_flat_snapshot() {
        let paths = test_paths("export_hires_claim_missing_snapshot");
        let broker = ProjectDbBroker::new(paths.clone());
        register_project(&paths, "project_a");
        let conn = open_ingest(&paths, "project_a").unwrap();
        let payload = ExportHiResJobPayload {
            project_id: "project_a".into(),
            export_id: "hires_empty".into(),
            output_path: paths
                .project_dir("project_a")
                .join("exports")
                .join("master.mxf"),
            timeline_timebase: FrameTimebase {
                fps_num: 50,
                fps_den: 1,
            },
            duration_frames: 50,
            items: Vec::new(),
        };
        crate::ingest::db::queue_ingest_job_payload(
            &conn,
            EXPORT_HIRES_JOB_TYPE,
            EXPORT_HIRES_SOURCE_ID,
            "hires_empty",
            &serde_json::to_value(&payload).unwrap(),
        )
        .unwrap();
        drop(conn);

        let claim = NormalizedClaim::from_request(JobClaimRequest {
            worker_id: "worker_a".into(),
            placement: None,
            project_id: Some("project_a".into()),
            capabilities: vec![EXPORT_HIRES_JOB_TYPE.into()],
            max_jobs: Some(1),
            lease_ms: Some(10_000),
        })
        .unwrap();
        let gateway = test_media_gateway(&paths);
        let jobs = claim_jobs(paths.clone(), broker, gateway, claim).unwrap();

        assert!(jobs.is_empty());
        let conn = open_ingest(&paths, "project_a").unwrap();
        let (status, error): (String, String) = conn
            .query_row(
                "SELECT status, error FROM ingest_jobs
                 WHERE job_type = ?1 AND source_id = ?2 AND clip_id = ?3",
                params![EXPORT_HIRES_JOB_TYPE, EXPORT_HIRES_SOURCE_ID, "hires_empty"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "error");
        assert!(error.contains("snapshot flat playlist"));
    }

    #[test]
    fn artifact_worker_status_warns_when_external_jobs_wait_without_worker() {
        let paths = test_paths("artifact_status_missing_worker");
        let broker = ProjectDbBroker::new(paths.clone());
        register_project(&paths, "project_a");
        let conn = open_ingest(&paths, "project_a").unwrap();
        queue_ingest_job(&conn, PROXY_GENERATE_JOB_TYPE, "card", "clip_a").unwrap();
        queue_ingest_job(&conn, FILMSTRIP_JOB_TYPE, FILMSTRIP_SOURCE_ID, "clip_a").unwrap();
        drop(conn);

        let status = artifact_worker_status(paths, broker, false).unwrap();

        assert_eq!(status.get("status").and_then(Value::as_str), Some("warn"));
        assert_eq!(
            status.get("message").and_then(Value::as_str),
            Some("external_artifact_worker_missing")
        );
        assert_eq!(
            status
                .get("artifact_jobs")
                .and_then(|v| v.get("queued"))
                .and_then(Value::as_i64),
            Some(2)
        );
        assert_eq!(
            status
                .get("artifact_jobs")
                .and_then(|v| v.get("active_leases"))
                .and_then(Value::as_i64),
            Some(0)
        );
    }

    #[test]
    fn non_preview_claim_keeps_fifo_order() {
        let paths = test_paths("non_preview_claim_fifo");
        register_project(&paths, "project_a");
        let conn = open_ingest(&paths, "project_a").unwrap();
        queue_ingest_job(&conn, FILMSTRIP_JOB_TYPE, FILMSTRIP_SOURCE_ID, "old").unwrap();
        queue_ingest_job(&conn, FILMSTRIP_JOB_TYPE, FILMSTRIP_SOURCE_ID, "new").unwrap();
        conn.execute(
            "UPDATE ingest_jobs SET queued_at = '2026-08-27T10:00:00Z', updated_at = '2026-08-27T10:00:00Z'
             WHERE clip_id = 'old'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE ingest_jobs SET queued_at = '2026-08-27T10:01:00Z', updated_at = '2026-08-27T10:01:00Z'
             WHERE clip_id = 'new'",
            [],
        )
        .unwrap();

        let rows = queued_jobs_for_type(&conn, FILMSTRIP_JOB_TYPE, 2).unwrap();

        assert_eq!(
            rows.iter()
                .map(|row| row.clip_id.as_str())
                .collect::<Vec<_>>(),
            vec!["old", "new"]
        );
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
            placement: None,
            project_id: Some("project_a".into()),
            capabilities: vec![FILMSTRIP_JOB_TYPE.into()],
            max_jobs: Some(1),
            lease_ms: Some(10_000),
        })
        .unwrap();
        let gateway = test_media_gateway(&paths);
        let jobs = claim_jobs(paths, broker, gateway, claim).unwrap();

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
    fn waveform_claim_uses_gateway_media() {
        let paths = test_paths("waveform_claim");
        let broker = ProjectDbBroker::new(paths.clone());
        register_project(&paths, "project_a");
        let project_dir = paths.project_dir("project_a");
        let source = touch(project_dir.join("original").join("clip_a.mxf"));
        let proxy = touch(project_dir.join("proxy").join("clip_a.mp4"));
        let conn = open_ingest(&paths, "project_a").unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, source_path, original_path, proxy_path,
                 import_status, status, duration_sec, file_extension)
             VALUES ('card', 'clip_a', 'Clip A', 'clip_a', ?1, ?1, ?2,
                     'imported', 'imported', 26.0, 'mxf')",
            params![
                source.to_string_lossy().as_ref(),
                proxy.to_string_lossy().as_ref()
            ],
        )
        .unwrap();
        queue_ingest_job(&conn, WAVEFORM_JOB_TYPE, WAVEFORM_SOURCE_ID, "clip_a").unwrap();
        drop(conn);

        let claim = NormalizedClaim::from_request(JobClaimRequest {
            worker_id: "worker_a".into(),
            placement: None,
            project_id: Some("project_a".into()),
            capabilities: vec![WAVEFORM_JOB_TYPE.into()],
            max_jobs: Some(1),
            lease_ms: Some(10_000),
        })
        .unwrap();
        let gateway = test_media_gateway(&paths);
        let jobs = claim_jobs(paths, broker, gateway, claim).unwrap();

        assert_eq!(jobs.len(), 1);
        let payload: WaveformJobPayload = serde_json::from_value(jobs[0].payload.clone()).unwrap();
        assert_eq!(payload.media_path, proxy);
        assert_eq!(payload.peak_buckets, crate::waveform::PEAK_BUCKETS);
        assert_eq!(
            payload.sample_rate_hz,
            crate::waveform::WAVEFORM_SAMPLE_RATE_HZ
        );
    }

    #[test]
    fn media_probe_claim_and_complete_record_sqlite() {
        let paths = test_paths("media_probe_claim_complete");
        let broker = ProjectDbBroker::new(paths.clone());
        register_project(&paths, "project_a");
        let project_dir = paths.project_dir("project_a");
        let source = touch(project_dir.join("card").join("clip_a.mp4"));
        let proxy = touch(project_dir.join("proxy").join("clip_a.mp4"));
        let conn = open_ingest(&paths, "project_a").unwrap();
        crate::ingest::db::set_meta(&conn, "active_source_id", "card").unwrap();
        crate::ingest::db::set_meta(&conn, "durations_probe", "processing").unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, source_path, proxy_path,
                 import_status, status, duration_sec, fps, file_extension)
             VALUES ('card', 'clip_a', 'Clip A', 'clip_a', ?1, ?2,
                     'detected', 'detected', 0.0, 0.0, 'mp4')",
            params![
                source.to_string_lossy().as_ref(),
                proxy.to_string_lossy().as_ref()
            ],
        )
        .unwrap();
        queue_ingest_job(&conn, MEDIA_PROBE_JOB_TYPE, "card", "clip_a").unwrap();
        drop(conn);

        let claim = NormalizedClaim::from_request(JobClaimRequest {
            worker_id: "worker_a".into(),
            placement: None,
            project_id: Some("project_a".into()),
            capabilities: vec![MEDIA_PROBE_JOB_TYPE.into()],
            max_jobs: Some(1),
            lease_ms: Some(10_000),
        })
        .unwrap();
        let gateway = test_media_gateway(&paths);
        let jobs = claim_jobs(paths.clone(), broker.clone(), gateway, claim).unwrap();

        assert_eq!(jobs.len(), 1);
        let payload: MediaProbeJobPayload =
            serde_json::from_value(jobs[0].payload.clone()).unwrap();
        assert_eq!(payload.media_path, proxy);

        let ack = complete_job(
            paths.clone(),
            broker,
            JobCompleteRequest {
                worker_id: "worker_a".into(),
                project_id: "project_a".into(),
                lease_id: jobs[0].lease_id.clone(),
                job_id: jobs[0].job_id.clone(),
                result: serde_json::to_value(MediaProbeJobResult {
                    probe: test_probe(),
                })
                .unwrap(),
            },
        )
        .unwrap();

        assert!(ack.accepted);
        let conn = open_ingest(&paths, "project_a").unwrap();
        let row: (f64, f64, i64, i64, String, String) = conn
            .query_row(
                "SELECT duration_sec, fps, source_fps_num, source_fps_den, source_class, proxy_recipe
                 FROM ingest_assets
                 WHERE source_id = 'card' AND clip_id = 'clip_a'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        let job: (String, String, String) = conn
            .query_row(
                "SELECT status, worker_id, lease_id
                 FROM ingest_jobs WHERE job_id = ?1",
                params![jobs[0].job_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(row.0, 1.0);
        assert_eq!(row.1, 50.0);
        assert_eq!(row.2, 50);
        assert_eq!(row.3, 1);
        assert_eq!(row.4, "pal_50p");
        assert_eq!(row.5, "h264_native");
        assert_eq!(
            crate::ingest::db::get_meta(&conn, "durations_probe", "").unwrap(),
            "done"
        );
        assert_eq!(job.0, "done");
        assert!(job.1.is_empty());
        assert!(job.2.is_empty());
    }

    #[test]
    fn waveform_complete_route_stores_peaks_and_clears_lease() {
        let paths = test_paths("waveform_complete");
        let broker = ProjectDbBroker::new(paths.clone());
        register_project(&paths, "project_a");
        let conn = open_ingest(&paths, "project_a").unwrap();
        lease_job(
            &conn,
            WAVEFORM_JOB_TYPE,
            WAVEFORM_SOURCE_ID,
            "clip_a",
            "worker_a",
            "lease_a",
        );
        drop(conn);

        let job_id =
            crate::ingest::db::ingest_job_id(WAVEFORM_JOB_TYPE, WAVEFORM_SOURCE_ID, "clip_a");
        let ack = complete_job(
            paths.clone(),
            broker,
            JobCompleteRequest {
                worker_id: "worker_a".into(),
                project_id: "project_a".into(),
                lease_id: "lease_a".into(),
                job_id: job_id.clone(),
                result: serde_json::to_value(WaveformJobResult {
                    a1_peaks: vec![0.1, 0.5, 1.0],
                    a2_peaks: vec![0.2, 0.4],
                    warning: None,
                })
                .unwrap(),
            },
        )
        .unwrap();

        assert!(ack.accepted);
        assert_eq!(
            crate::waveform::peaks_for_channel(&paths, "project_a", "clip_a", 1).unwrap(),
            vec![0.1, 0.5, 1.0]
        );
        assert_eq!(
            crate::waveform::peaks_for_channel(&paths, "project_a", "clip_a", 2).unwrap(),
            vec![0.2, 0.4]
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
    fn audio_wrap_claim_and_complete_record_sqlite() {
        let paths = test_paths("audio_wrap_claim_complete");
        let broker = ProjectDbBroker::new(paths.clone());
        register_project(&paths, "project_a");
        let project_dir = paths.project_dir("project_a");
        let video = touch(project_dir.join("proxy").join("clip_video.mp4"));
        let audio = touch(project_dir.join("audio").join("vo_a.wav"));
        let conn = open_ingest(&paths, "project_a").unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, source_path, original_path,
                 project_proxy_path, import_status, status, duration_sec, fps, file_extension)
             VALUES ('card', 'clip_video', 'Clip Video', 'clip_video', ?1, ?1, ?1,
                     'imported', 'imported', 2.0, 50.0, 'mp4')",
            params![video.to_string_lossy().as_ref()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, source_path, original_path,
                 import_status, status, file_extension, metadata_json)
             VALUES ('voice', 'vo_a', 'Voice A', 'vo_a', ?1, ?1,
                     'imported', 'imported', 'wav', ?2)",
            params![
                audio.to_string_lossy().as_ref(),
                json!({
                    "audio_project_path": audio.to_string_lossy().to_string(),
                    "audio_wrap_status": "pending",
                    "audio_wraps": {}
                })
                .to_string()
            ],
        )
        .unwrap();
        queue_ingest_job(&conn, AUDIO_WRAP_JOB_TYPE, "voice", "vo_a").unwrap();
        drop(conn);

        let claim = NormalizedClaim::from_request(JobClaimRequest {
            worker_id: "worker_a".into(),
            placement: None,
            project_id: Some("project_a".into()),
            capabilities: vec![AUDIO_WRAP_JOB_TYPE.into()],
            max_jobs: Some(1),
            lease_ms: Some(10_000),
        })
        .unwrap();
        let gateway = test_media_gateway(&paths);
        let jobs = claim_jobs(paths.clone(), broker.clone(), gateway, claim).unwrap();

        assert_eq!(jobs.len(), 1);
        let payload: AudioWrapJobPayload = serde_json::from_value(jobs[0].payload.clone()).unwrap();
        assert_eq!(payload.media_path, audio);
        assert_eq!(payload.wraps.len(), 1);
        assert_eq!(payload.wraps[0].fps, 50.0);
        let wrap_path = payload.wraps[0].output_path.clone();
        touch(wrap_path.clone());

        let ack = complete_job(
            paths.clone(),
            broker,
            JobCompleteRequest {
                worker_id: "worker_a".into(),
                project_id: "project_a".into(),
                lease_id: jobs[0].lease_id.clone(),
                job_id: jobs[0].job_id.clone(),
                result: serde_json::to_value(AudioWrapJobResult {
                    wraps: vec![qnc_service_contracts::AudioWrapJobArtifact {
                        fps: 50.0,
                        output_path: wrap_path.clone(),
                        probe: Some(test_probe()),
                    }],
                })
                .unwrap(),
            },
        )
        .unwrap();

        assert!(ack.accepted);
        let conn = open_ingest(&paths, "project_a").unwrap();
        let asset: (String, f64, String) = conn
            .query_row(
                "SELECT project_proxy_path, fps, metadata_json
                 FROM ingest_assets
                 WHERE source_id = 'voice' AND clip_id = 'vo_a'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let meta: Value = serde_json::from_str(&asset.2).unwrap();
        let job: (String, String, String) = conn
            .query_row(
                "SELECT status, worker_id, lease_id
                 FROM ingest_jobs WHERE job_id = ?1",
                params![jobs[0].job_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let wrap_path_s = wrap_path.to_string_lossy().to_string();
        assert_eq!(asset.0, wrap_path_s);
        assert_eq!(asset.1, 50.0);
        assert_eq!(
            meta.get("audio_wrap_status").and_then(Value::as_str),
            Some("ready")
        );
        assert_eq!(
            meta.get("audio_wraps")
                .and_then(|v| v.get("50"))
                .and_then(Value::as_str),
            Some(wrap_path_s.as_str())
        );
        assert_eq!(job.0, "done");
        assert!(job.1.is_empty());
        assert!(job.2.is_empty());
    }

    #[test]
    fn thumb_proxy_claim_prefers_card_poster_without_worker_payload() {
        let paths = test_paths("thumb_proxy_card");
        let broker = ProjectDbBroker::new(paths.clone());
        register_project(&paths, "project_a");
        let project_dir = paths.project_dir("project_a");
        let source = touch(project_dir.join("card").join("clip_a.mxf"));
        let card_thumb = touch(project_dir.join("card").join("clip_a.thm"));
        let conn = open_ingest(&paths, "project_a").unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, source_path, original_path,
                 card_thumb_path, file_extension, thumb_status)
             VALUES ('card', 'clip_a', 'Clip A', 'clip_a', ?1, ?1, ?2, 'mxf', 'pending')",
            params![
                source.to_string_lossy().as_ref(),
                card_thumb.to_string_lossy().as_ref()
            ],
        )
        .unwrap();
        queue_ingest_job(&conn, THUMB_PROXY_JOB_TYPE, "card", "clip_a").unwrap();
        drop(conn);

        let claim = NormalizedClaim::from_request(JobClaimRequest {
            worker_id: "worker_a".into(),
            placement: None,
            project_id: Some("project_a".into()),
            capabilities: vec![THUMB_PROXY_JOB_TYPE.into()],
            max_jobs: Some(1),
            lease_ms: Some(10_000),
        })
        .unwrap();
        let gateway = test_media_gateway(&paths);
        let jobs = claim_jobs(paths.clone(), broker, gateway, claim).unwrap();

        assert!(jobs.is_empty());
        let poster = thumbnail_path(&paths, "project_a", "clip_a");
        assert!(poster_exists(&poster));
        assert_eq!(fs::read(&poster).unwrap(), b"media");
        let conn = open_ingest(&paths, "project_a").unwrap();
        let row: (String, String, String) = conn
            .query_row(
                "SELECT thumb_status, poster_source, thumb_path
                 FROM ingest_assets
                 WHERE source_id = 'card' AND clip_id = 'clip_a'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let job_status: String = conn
            .query_row(
                "SELECT status FROM ingest_jobs WHERE job_id = ?1",
                params![crate::ingest::db::ingest_job_id(
                    THUMB_PROXY_JOB_TYPE,
                    "card",
                    "clip_a"
                )],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(row.0, "ready");
        assert_eq!(row.1, "card_thm");
        assert_eq!(row.2, poster.to_string_lossy());
        assert_eq!(job_status, "done");
    }

    #[test]
    fn thumb_proxy_claim_requires_proxy_generation_approval_when_card_poster_missing() {
        let paths = test_paths("thumb_proxy_requires_approval");
        let broker = ProjectDbBroker::new(paths.clone());
        register_project(&paths, "project_a");
        let project_dir = paths.project_dir("project_a");
        let source = touch(project_dir.join("card").join("clip_b.mxf"));
        let proxy = touch(project_dir.join("proxy").join("clip_b.mp4"));
        let conn = open_ingest(&paths, "project_a").unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, source_path, original_path, proxy_path,
                 import_status, status, file_extension, thumb_status)
             VALUES ('card', 'clip_b', 'Clip B', 'clip_b', ?1, ?1, ?2,
                     'imported', 'imported', 'mxf', 'no_card_thumb')",
            params![
                source.to_string_lossy().as_ref(),
                proxy.to_string_lossy().as_ref()
            ],
        )
        .unwrap();
        queue_ingest_job(&conn, THUMB_PROXY_JOB_TYPE, "card", "clip_b").unwrap();
        drop(conn);

        let claim = NormalizedClaim::from_request(JobClaimRequest {
            worker_id: "worker_a".into(),
            placement: None,
            project_id: Some("project_a".into()),
            capabilities: vec![THUMB_PROXY_JOB_TYPE.into()],
            max_jobs: Some(1),
            lease_ms: Some(10_000),
        })
        .unwrap();
        let gateway = test_media_gateway(&paths);
        let jobs = claim_jobs(paths.clone(), broker, gateway, claim).unwrap();

        assert!(jobs.is_empty());
        let conn = open_ingest(&paths, "project_a").unwrap();
        let row: (String, String) = conn
            .query_row(
                "SELECT thumb_status, thumb_error FROM ingest_assets WHERE clip_id = 'clip_b'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let job_status: String = conn
            .query_row(
                "SELECT status FROM ingest_jobs WHERE job_id = ?1",
                params![crate::ingest::db::ingest_job_id(
                    THUMB_PROXY_JOB_TYPE,
                    "card",
                    "clip_b"
                )],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(row.0, "no_card_thumb");
        assert_eq!(row.1, "proxy poster generation requires user approval");
        assert_eq!(job_status, "done");
    }

    #[test]
    fn thumb_proxy_claim_uses_proxy_media_after_proxy_generation_approval() {
        let paths = test_paths("thumb_proxy_claim");
        let broker = ProjectDbBroker::new(paths.clone());
        register_project(&paths, "project_a");
        let project_dir = paths.project_dir("project_a");
        let source = touch(project_dir.join("card").join("clip_b.mxf"));
        let proxy = touch(project_dir.join("proxy").join("clip_b.mp4"));
        let conn = open_ingest(&paths, "project_a").unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, source_path, original_path, proxy_path,
                 import_status, status, file_extension, thumb_status)
             VALUES ('card', 'clip_b', 'Clip B', 'clip_b', ?1, ?1, ?2,
                     'imported', 'imported', 'mxf', 'no_card_thumb')",
            params![
                source.to_string_lossy().as_ref(),
                proxy.to_string_lossy().as_ref()
            ],
        )
        .unwrap();
        crate::ingest::db::set_poster_proxy_generation_approved(&conn, "card", "clip_b", true)
            .unwrap();
        queue_ingest_job(&conn, THUMB_PROXY_JOB_TYPE, "card", "clip_b").unwrap();
        drop(conn);

        let claim = NormalizedClaim::from_request(JobClaimRequest {
            worker_id: "worker_a".into(),
            placement: None,
            project_id: Some("project_a".into()),
            capabilities: vec![THUMB_PROXY_JOB_TYPE.into()],
            max_jobs: Some(1),
            lease_ms: Some(10_000),
        })
        .unwrap();
        let gateway = test_media_gateway(&paths);
        let jobs = claim_jobs(paths.clone(), broker, gateway, claim).unwrap();

        assert_eq!(jobs.len(), 1);
        let payload: PosterJobPayload = serde_json::from_value(jobs[0].payload.clone()).unwrap();
        assert_eq!(payload.media_path, proxy);
        assert_eq!(
            payload.output_path,
            thumbnail_path(&paths, "project_a", "clip_b")
        );
        let conn = open_ingest(&paths, "project_a").unwrap();
        let thumb_status: String = conn
            .query_row(
                "SELECT thumb_status FROM ingest_assets WHERE clip_id = 'clip_b'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(thumb_status, "processing");
    }

    #[test]
    fn thumb_proxy_complete_route_marks_poster_ready_and_clears_lease() {
        let paths = test_paths("thumb_proxy_complete");
        let broker = ProjectDbBroker::new(paths.clone());
        register_project(&paths, "project_a");
        let source = touch(
            paths
                .project_dir("project_a")
                .join("card")
                .join("clip_c.mxf"),
        );
        let poster = thumbnail_path(&paths, "project_a", "clip_c");
        touch(poster.clone());
        let conn = open_ingest(&paths, "project_a").unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, source_path, original_path,
                 import_status, status, file_extension, thumb_status)
             VALUES ('card', 'clip_c', 'Clip C', 'clip_c', ?1, ?1,
                     'imported', 'imported', 'mxf', 'processing')",
            params![source.to_string_lossy().as_ref()],
        )
        .unwrap();
        lease_job(
            &conn,
            THUMB_PROXY_JOB_TYPE,
            "card",
            "clip_c",
            "worker_a",
            "lease_a",
        );
        drop(conn);

        let job_id = crate::ingest::db::ingest_job_id(THUMB_PROXY_JOB_TYPE, "card", "clip_c");
        let ack = complete_job(
            paths.clone(),
            broker,
            JobCompleteRequest {
                worker_id: "worker_a".into(),
                project_id: "project_a".into(),
                lease_id: "lease_a".into(),
                job_id: job_id.clone(),
                result: serde_json::to_value(PosterJobResult {
                    output_path: poster.clone(),
                })
                .unwrap(),
            },
        )
        .unwrap();

        assert!(ack.accepted);
        let conn = open_ingest(&paths, "project_a").unwrap();
        let row: (String, String, String) = conn
            .query_row(
                "SELECT thumb_status, poster_source, thumb_path
                 FROM ingest_assets
                 WHERE source_id = 'card' AND clip_id = 'clip_c'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let job: (String, String, String) = conn
            .query_row(
                "SELECT status, worker_id, lease_id
                 FROM ingest_jobs WHERE job_id = ?1",
                params![job_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(row.0, "ready");
        assert_eq!(row.1, "proxy_ffmpeg");
        assert_eq!(row.2, poster.to_string_lossy());
        assert_eq!(job.0, "done");
        assert!(job.1.is_empty());
        assert!(job.2.is_empty());
    }

    #[test]
    fn artifact_capabilities_are_claimable_in_jobservice() {
        let claim = NormalizedClaim::from_request(JobClaimRequest {
            worker_id: "worker_a".into(),
            placement: None,
            project_id: None,
            capabilities: vec![
                PROXY_GENERATE_JOB_TYPE.into(),
                FILMSTRIP_JOB_TYPE.into(),
                WAVEFORM_JOB_TYPE.into(),
                THUMB_PROXY_JOB_TYPE.into(),
                AUDIO_WRAP_JOB_TYPE.into(),
                MEDIA_PROBE_JOB_TYPE.into(),
                EXPORT_HIRES_JOB_TYPE.into(),
                SMOKE_JOB_TYPE.into(),
            ],
            max_jobs: Some(1),
            lease_ms: Some(10_000),
        })
        .unwrap();
        assert_eq!(
            claim.capabilities,
            vec![
                PROXY_GENERATE_JOB_TYPE.to_string(),
                FILMSTRIP_JOB_TYPE.to_string(),
                WAVEFORM_JOB_TYPE.to_string(),
                THUMB_PROXY_JOB_TYPE.to_string(),
                AUDIO_WRAP_JOB_TYPE.to_string(),
                MEDIA_PROBE_JOB_TYPE.to_string(),
                EXPORT_HIRES_JOB_TYPE.to_string(),
                SMOKE_JOB_TYPE.to_string(),
            ]
        );
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
            placement: None,
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

        let gateway = test_media_gateway(&paths);
        let jobs = claim_jobs(paths.clone(), broker.clone(), gateway, claim).unwrap();
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
            placement: None,
            project_id: Some("project_a".into()),
            capabilities: vec!["qnc_worker_smoke".into()],
            max_jobs: Some(1),
            lease_ms: Some(10_000),
        })
        .unwrap();
        let gateway = test_media_gateway(&paths);
        let job = claim_jobs(paths.clone(), broker.clone(), gateway, claim)
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

    #[test]
    fn heartbeat_accepts_every_lease_managed_job_type() {
        let paths = test_paths("heartbeat_all_lease_jobs");
        let broker = ProjectDbBroker::new(paths.clone());
        let conn = open_ingest(&paths, "project_a").unwrap();
        let job_types = [
            SMOKE_JOB_TYPE,
            PROXY_GENERATE_JOB_TYPE,
            FILMSTRIP_JOB_TYPE,
            WAVEFORM_JOB_TYPE,
            THUMB_PROXY_JOB_TYPE,
            AUDIO_WRAP_JOB_TYPE,
            MEDIA_PROBE_JOB_TYPE,
            EXPORT_HIRES_JOB_TYPE,
        ];
        let mut job_ids = Vec::new();
        for (index, job_type) in job_types.iter().enumerate() {
            let clip_id = format!("clip_{index}");
            lease_job(&conn, job_type, "worker", &clip_id, "worker_a", "lease_a");
            job_ids.push(crate::ingest::db::ingest_job_id(
                job_type, "worker", &clip_id,
            ));
        }
        drop(conn);

        let heartbeat = heartbeat_jobs(
            paths,
            broker,
            JobHeartbeatRequest {
                worker_id: "worker_a".into(),
                project_id: "project_a".into(),
                lease_id: "lease_a".into(),
                job_ids: job_ids.clone(),
                lease_ms: Some(10_000),
            },
        )
        .unwrap();

        assert_eq!(heartbeat.accepted, job_ids);
        assert!(heartbeat.rejected.is_empty());
    }
}
