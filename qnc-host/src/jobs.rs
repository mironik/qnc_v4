use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use qnc_service_contracts::{
    JobAck, JobClaimRequest, JobClaimResponse, JobCompleteRequest, JobFailRequest,
    JobHeartbeatRequest, JobHeartbeatResponse, JobLease,
};
use rusqlite::{params, Connection};
use serde_json::json;
use uuid::Uuid;

use crate::app_state::AppState;
use crate::ingest::db::open_ingest;
use crate::project::db::{now_str, ProjectPaths};
use crate::project::{list_project_ids, ProjectDbBroker};

const DEFAULT_MAX_JOBS: usize = 1;
const MAX_CLAIM_JOBS: usize = 8;
const DEFAULT_LEASE_MS: u64 = 30_000;
const MIN_LEASE_MS: u64 = 5_000;
const MAX_LEASE_MS: u64 = 300_000;
const EXTERNAL_CLAIMABLE_JOB_TYPES: &[&str] = &[
    // Protocol canary only. Product artifact jobs are added here only after
    // they have both an external worker handler and a host-side result applier.
    "qnc_worker_smoke",
];

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
            let remaining = limit - claimed.len();
            let rows = queued_jobs_for_type(&conn, job_type, remaining)?;
            for row in rows {
                if claimed.len() >= limit {
                    break;
                }
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
                        payload: json!({}),
                    });
                }
            }
        }
        Ok(claimed)
    })
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
                       AND job_type = 'qnc_worker_smoke'",
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
    let now = now_str();

    let changed = project_db.serialize_project_write(&project_id, || {
        let conn = open_ingest(&paths, &project_id).map_err(|e| e.to_string())?;
        ensure_job_service_schema(&conn).map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE ingest_jobs
             SET status = 'done',
                 error = '',
                 finished_at = ?4,
                 updated_at = ?4,
                 worker_id = '',
                 lease_id = '',
                 lease_until_ms = 0,
                 heartbeat_ms = 0,
                 result_json = ?5
             WHERE job_id = ?1
               AND worker_id = ?2
               AND lease_id = ?3
               AND status = 'processing'
               AND job_type = 'qnc_worker_smoke'",
            params![job_id, worker_id, lease_id, now, result_json],
        )
        .map_err(|e| e.to_string())
    })?;

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
                   AND job_type = 'qnc_worker_smoke'",
                params![job_id, worker_id, lease_id, error, now],
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
                   AND job_type = 'qnc_worker_smoke'",
                params![job_id, worker_id, lease_id, error, now],
            )
        }
        .map_err(|e| e.to_string())
    })?;

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
    ensure_column(
        conn,
        "ingest_jobs",
        "worker_id",
        "ALTER TABLE ingest_jobs ADD COLUMN worker_id TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        conn,
        "ingest_jobs",
        "lease_id",
        "ALTER TABLE ingest_jobs ADD COLUMN lease_id TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        conn,
        "ingest_jobs",
        "lease_until_ms",
        "ALTER TABLE ingest_jobs ADD COLUMN lease_until_ms INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "ingest_jobs",
        "heartbeat_ms",
        "ALTER TABLE ingest_jobs ADD COLUMN heartbeat_ms INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "ingest_jobs",
        "result_json",
        "ALTER TABLE ingest_jobs ADD COLUMN result_json TEXT NOT NULL DEFAULT '{}'",
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ingest_jobs_external_lease
         ON ingest_jobs(status, lease_until_ms, worker_id)",
        [],
    )?;
    Ok(())
}

fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    alter_sql: &str,
) -> rusqlite::Result<()> {
    if !column_exists(conn, table, column)? {
        conn.execute_batch(alter_sql)?;
    }
    Ok(())
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for row in rows {
        if row? == column {
            return Ok(true);
        }
    }
    Ok(false)
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

    #[test]
    fn claim_uses_existing_ingest_jobs_and_skips_internal_jobs() {
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
