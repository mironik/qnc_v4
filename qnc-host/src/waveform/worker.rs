use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::{info, warn};

use crate::background_work::BackgroundWorkGate;
use crate::ingest::db::{open_ingest, queue_ingest_artifact_job_once};
use crate::media::imported_clip_ids;
use crate::project::db::ProjectPaths;
use crate::project::{list_project_ids, ProjectDbBroker};
use qnc_service_contracts::{JOB_SOURCE_WAVEFORM, JOB_TYPE_WAVEFORM};

use super::store::ready as waveform_ready;

#[derive(Clone)]
pub struct WaveformWorker {
    paths: ProjectPaths,
    project_db: ProjectDbBroker,
    background: BackgroundWorkGate,
    blocked: Arc<Mutex<HashSet<String>>>,
}

impl WaveformWorker {
    pub fn new(
        paths: ProjectPaths,
        project_db: ProjectDbBroker,
        background: BackgroundWorkGate,
    ) -> Self {
        Self {
            paths,
            project_db,
            background,
            blocked: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub async fn wait_drained(&self, _max_ms: u64) {}

    pub fn block_project(&self, project_id: &str) {
        let pid = project_id.trim();
        if pid.is_empty() {
            return;
        }
        self.blocked
            .lock()
            .expect("waveform block")
            .insert(pid.to_string());
    }

    fn is_blocked(&self, project_id: &str) -> bool {
        self.blocked
            .lock()
            .expect("waveform block")
            .contains(project_id)
    }

    pub fn enqueue_job(&self, project_id: &str, clip_id: &str) {
        let pid = project_id.trim();
        let cid = clip_id.trim();
        if pid.is_empty() || cid.is_empty() {
            return;
        }
        if self.is_blocked(pid) || waveform_ready(&self.paths, pid, cid) {
            return;
        }
        self.queue_waveform_job(pid, cid);
    }

    pub fn enqueue_recoverable_projects(&self) -> Result<usize, String> {
        let project_ids = self
            .project_db
            .with_global(|global| list_project_ids(global).map_err(|e| e.to_string()))?;
        let mut queued = 0usize;
        for project_id in project_ids {
            if self.is_blocked(&project_id) {
                continue;
            }
            if self.proxy_pressure_active(&project_id) {
                continue;
            }
            for clip_id in imported_clip_ids(&self.paths, &project_id)? {
                if waveform_ready(&self.paths, &project_id, &clip_id) {
                    continue;
                }
                if self.queue_waveform_job(&project_id, &clip_id) {
                    queued += 1;
                }
            }
        }
        Ok(queued)
    }

    pub fn spawn(self: Arc<Self>) {
        info!("waveform: scheduler only; external JobService owner");
        tokio::spawn(async move {
            let mut last_recover = Instant::now();
            loop {
                if self.background.playback_active() {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    continue;
                }
                if last_recover.elapsed() >= Duration::from_secs(8) {
                    let _ = self.enqueue_recoverable_projects();
                    last_recover = Instant::now();
                }
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
        });
    }

    fn proxy_pressure_active(&self, project_id: &str) -> bool {
        proxy_pressure_active(&self.paths, &self.project_db, project_id).unwrap_or(false)
    }

    fn queue_waveform_job(&self, project_id: &str, clip_id: &str) -> bool {
        match self.project_db.serialize_project_write(project_id, || {
            let conn = open_ingest(&self.paths, project_id).map_err(|e| e.to_string())?;
            queue_ingest_artifact_job_once(&conn, JOB_TYPE_WAVEFORM, JOB_SOURCE_WAVEFORM, clip_id)
                .map_err(|e| e.to_string())
        }) {
            Ok(queued) => queued,
            Err(error) => {
                warn!(
                    "waveform queue: project={} clip={} err={}",
                    project_id, clip_id, error
                );
                false
            }
        }
    }
}

fn proxy_pressure_active(
    paths: &ProjectPaths,
    project_db: &ProjectDbBroker,
    project_id: &str,
) -> Result<bool, String> {
    if crate::ingest_proxy::proxy_generate_busy(paths) {
        return Ok(true);
    }
    project_db.serialize_project_write(project_id, || {
        let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
        let active_assets: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ingest_assets
                 WHERE import_status = 'generating_proxy'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if active_assets > 0 {
            return Ok(true);
        }
        let active_jobs: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ingest_jobs
                 WHERE job_type = 'proxy_generate'
                   AND status IN ('queued', 'processing')",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(active_jobs > 0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::db::{mark_ingest_job_done, open_ingest, queue_ingest_job};
    use crate::project::db::{
        ensure_project_dirs_at, open_global, open_project, project_dir_in_root,
    };
    use rusqlite::params;
    use std::fs;
    use std::path::Path;

    fn test_paths(base: &Path) -> ProjectPaths {
        ProjectPaths {
            data_dir: base.join("data"),
            projects_root: base.join("projects"),
            seed_path: base.join("seed.json"),
        }
    }

    fn register_project(paths: &ProjectPaths, project_id: &str) {
        let global = open_global(paths).unwrap();
        let project_dir = project_dir_in_root(&paths.projects_root, project_id);
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
        ensure_project_dirs_at(&project_dir).unwrap();
        let _ = open_project(paths, project_id).unwrap();
    }

    #[test]
    fn enqueue_job_records_waveform_ingest_job_without_media_path() {
        let base = std::env::temp_dir().join(format!(
            "qnc_waveform_queue_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "project_a";
        register_project(&paths, project_id);
        let project_db = ProjectDbBroker::new(paths.clone());
        let worker = WaveformWorker::new(paths.clone(), project_db, BackgroundWorkGate::new());

        worker.enqueue_job(project_id, "clip_a");

        let conn = open_ingest(&paths, project_id).unwrap();
        let status: String = conn
            .query_row(
                "SELECT status FROM ingest_jobs
                 WHERE job_type = ?1 AND source_id = ?2 AND clip_id = 'clip_a'",
                params![JOB_TYPE_WAVEFORM, JOB_SOURCE_WAVEFORM],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "queued");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn enqueue_job_keeps_active_waveform_ingest_job() {
        let base = std::env::temp_dir().join(format!(
            "qnc_waveform_active_queue_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "project_a";
        register_project(&paths, project_id);
        let project_db = ProjectDbBroker::new(paths.clone());
        let worker = WaveformWorker::new(paths.clone(), project_db, BackgroundWorkGate::new());

        worker.enqueue_job(project_id, "clip_a");

        let conn = open_ingest(&paths, project_id).unwrap();
        conn.execute(
            "UPDATE ingest_jobs
             SET status = 'processing',
                 queued_at = 'original-queued',
                 started_at = 'original-started',
                 updated_at = 'original-updated',
                 worker_id = 'worker-a',
                 lease_id = '',
                 lease_until_ms = 0,
                 heartbeat_ms = 0
             WHERE job_type = ?1 AND source_id = ?2 AND clip_id = 'clip_a'",
            params![JOB_TYPE_WAVEFORM, JOB_SOURCE_WAVEFORM],
        )
        .unwrap();

        worker.enqueue_job(project_id, "clip_a");

        let row: (String, String, String, String) = conn
            .query_row(
                "SELECT status, queued_at, started_at, worker_id
                 FROM ingest_jobs
                 WHERE job_type = ?1 AND source_id = ?2 AND clip_id = 'clip_a'",
                params![JOB_TYPE_WAVEFORM, JOB_SOURCE_WAVEFORM],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (
                "processing".into(),
                "original-queued".into(),
                "original-started".into(),
                "worker-a".into()
            )
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn recoverable_projects_do_not_requeue_error_waveform() {
        let base = std::env::temp_dir().join(format!(
            "qnc_waveform_error_recoverable_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "project_a";
        register_project(&paths, project_id);
        let project_db = ProjectDbBroker::new(paths.clone());
        let worker = WaveformWorker::new(paths.clone(), project_db, BackgroundWorkGate::new());
        let conn = open_ingest(&paths, project_id).unwrap();
        conn.execute(
            "INSERT INTO ingest_assets (source_id, clip_id, name, import_status)
             VALUES ('card', 'clip_a', 'Clip A', 'imported')",
            [],
        )
        .unwrap();
        worker.enqueue_job(project_id, "clip_a");
        conn.execute(
            "UPDATE ingest_jobs
             SET status = 'error',
                 error = 'waveform media missing',
                 queued_at = 'original-queued',
                 started_at = 'original-started',
                 worker_id = 'worker-a'
             WHERE job_type = ?1 AND source_id = ?2 AND clip_id = 'clip_a'",
            params![JOB_TYPE_WAVEFORM, JOB_SOURCE_WAVEFORM],
        )
        .unwrap();

        assert_eq!(worker.enqueue_recoverable_projects().unwrap(), 0);
        let row: (String, String, String, String) = conn
            .query_row(
                "SELECT status, error, queued_at, worker_id
                 FROM ingest_jobs
                 WHERE job_type = ?1 AND source_id = ?2 AND clip_id = 'clip_a'",
                params![JOB_TYPE_WAVEFORM, JOB_SOURCE_WAVEFORM],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (
                "error".into(),
                "waveform media missing".into(),
                "original-queued".into(),
                "worker-a".into()
            )
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn proxy_pressure_gates_waveform_recovery() {
        let base = std::env::temp_dir().join(format!(
            "qnc_waveform_gate_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "project_a";
        register_project(&paths, project_id);
        let project_db = ProjectDbBroker::new(paths.clone());
        let conn = open_ingest(&paths, project_id).unwrap();

        assert!(!proxy_pressure_active(&paths, &project_db, project_id).unwrap());

        queue_ingest_job(&conn, "filmstrip", "filmstrip", "clip_a").unwrap();
        assert!(!proxy_pressure_active(&paths, &project_db, project_id).unwrap());

        mark_ingest_job_done(&conn, "filmstrip", "filmstrip", "clip_a").unwrap();
        assert!(!proxy_pressure_active(&paths, &project_db, project_id).unwrap());

        queue_ingest_job(&conn, "proxy_generate", "card", "clip_proxy").unwrap();
        assert!(proxy_pressure_active(&paths, &project_db, project_id).unwrap());

        mark_ingest_job_done(&conn, "proxy_generate", "card", "clip_proxy").unwrap();
        assert!(!proxy_pressure_active(&paths, &project_db, project_id).unwrap());

        conn.execute(
            "INSERT INTO ingest_assets (source_id, clip_id, name, import_status)
             VALUES ('card', 'clip_b', 'Clip B', 'generating_proxy')",
            [],
        )
        .unwrap();
        assert!(proxy_pressure_active(&paths, &project_db, project_id).unwrap());

        conn.execute(
            "UPDATE ingest_assets SET import_status = 'imported' WHERE clip_id = 'clip_b'",
            [],
        )
        .unwrap();
        assert!(!proxy_pressure_active(&paths, &project_db, project_id).unwrap());

        let _ = fs::remove_dir_all(&base);
    }
}
