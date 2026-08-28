use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::{info, warn};

use crate::background_work::BackgroundWorkGate;
use crate::ingest::db::{
    open_ingest, queue_ingest_artifact_job_once, requeue_terminal_ingest_artifact_job,
};
use crate::ingest::thumb::timeline_seek_seconds;
use crate::media::imported_clip_ids;
use crate::project::db::ProjectPaths;
use crate::project::{list_project_ids, ProjectDbBroker};
use qnc_service_contracts::{JOB_SOURCE_FILMSTRIP, JOB_TYPE_FILMSTRIP};

use super::store::{get_filmstrip, list_frames_for_clip};

#[derive(Clone)]
pub struct FilmstripWorker {
    paths: ProjectPaths,
    project_db: ProjectDbBroker,
    background: BackgroundWorkGate,
    blocked: Arc<Mutex<HashSet<String>>>,
}

impl FilmstripWorker {
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
            .expect("filmstrip block")
            .insert(pid.to_string());
    }

    fn is_blocked(&self, project_id: &str) -> bool {
        self.blocked
            .lock()
            .expect("filmstrip block")
            .contains(project_id)
    }

    pub fn enqueue(&self, project_id: &str, clip_id: &str, media_path: &Path, frames: u32) {
        self.enqueue_with_priority(project_id, clip_id, media_path, frames, false);
    }

    pub fn enqueue_priority(
        &self,
        project_id: &str,
        clip_id: &str,
        media_path: &Path,
        frames: u32,
    ) {
        self.enqueue_with_priority(project_id, clip_id, media_path, frames, true);
    }

    fn enqueue_with_priority(
        &self,
        project_id: &str,
        clip_id: &str,
        media_path: &Path,
        frames: u32,
        priority: bool,
    ) {
        let _ = (media_path, frames, priority);
        let pid = project_id.trim();
        let cid = clip_id.trim();
        if pid.is_empty() || cid.is_empty() {
            return;
        }
        if self.is_blocked(pid) {
            return;
        }
        self.queue_filmstrip_job(pid, cid);
    }

    pub fn retry_terminal_error(&self, project_id: &str, clip_id: &str) -> bool {
        let pid = project_id.trim();
        let cid = clip_id.trim();
        if pid.is_empty() || cid.is_empty() || self.is_blocked(pid) {
            return false;
        }
        match self.project_db.serialize_project_write(pid, || {
            let conn = open_ingest(&self.paths, pid).map_err(|e| e.to_string())?;
            requeue_terminal_ingest_artifact_job(
                &conn,
                JOB_TYPE_FILMSTRIP,
                JOB_SOURCE_FILMSTRIP,
                cid,
            )
            .map_err(|e| e.to_string())
        }) {
            Ok(queued) => queued,
            Err(error) => {
                warn!(
                    "filmstrip retry: project={} clip={} err={}",
                    project_id, clip_id, error
                );
                false
            }
        }
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
            for clip_id in imported_clip_ids(&self.paths, &project_id)? {
                if filmstrip_ready(&self.paths, &project_id, &clip_id) {
                    continue;
                }
                if self.queue_filmstrip_job(&project_id, &clip_id) {
                    queued += 1;
                }
            }
        }
        Ok(queued)
    }

    pub fn spawn(self: Arc<Self>) {
        info!("filmstrip: scheduler only; external JobService owner");
        tokio::spawn(async move {
            let mut last_recover = Instant::now();
            loop {
                if self.background.playback_active() {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
                if last_recover.elapsed() >= Duration::from_secs(4) {
                    let _ = self.enqueue_recoverable_projects();
                    last_recover = Instant::now();
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        });
    }

    fn queue_filmstrip_job(&self, project_id: &str, clip_id: &str) -> bool {
        match self.project_db.serialize_project_write(project_id, || {
            let conn = open_ingest(&self.paths, project_id).map_err(|e| e.to_string())?;
            queue_ingest_artifact_job_once(&conn, JOB_TYPE_FILMSTRIP, JOB_SOURCE_FILMSTRIP, clip_id)
                .map_err(|e| e.to_string())
        }) {
            Ok(queued) => queued,
            Err(error) => {
                warn!(
                    "filmstrip queue: project={} clip={} err={}",
                    project_id, clip_id, error
                );
                false
            }
        }
    }
}

pub(crate) fn filmstrip_ready(paths: &ProjectPaths, project_id: &str, clip_id: &str) -> bool {
    let Some(fs) = get_filmstrip(paths, project_id, clip_id) else {
        return false;
    };
    if fs.get("status").and_then(|v| v.as_str()) != Some("ready") {
        return false;
    }
    let Ok(frames) = list_frames_for_clip(paths, project_id, clip_id) else {
        return false;
    };
    let duration = fs
        .get("duration_sec")
        .and_then(|v| v.as_f64())
        .filter(|v| *v > 0.0)
        .unwrap_or(0.0);
    if duration <= 0.0 {
        return false;
    }
    let seeks = timeline_seek_seconds(duration, super::DEFAULT_FILMSTRIP_FRAMES);
    super::build::stored_frames_match_seeks(&frames, &seeks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::db::{
        ensure_project_dirs_at, open_global, open_project, project_dir_in_root,
    };
    use rusqlite::params;
    use std::fs;

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
    fn enqueue_keeps_active_filmstrip_job() {
        let base = std::env::temp_dir().join(format!(
            "qnc_filmstrip_active_queue_test_{}_{}",
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
        let worker = FilmstripWorker::new(
            paths.clone(),
            ProjectDbBroker::new(paths.clone()),
            BackgroundWorkGate::new(),
        );

        worker.enqueue(project_id, "clip_a", Path::new("dummy.mp4"), 13);

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
            params![JOB_TYPE_FILMSTRIP, JOB_SOURCE_FILMSTRIP],
        )
        .unwrap();

        worker.enqueue_priority(project_id, "clip_a", Path::new("dummy.mp4"), 13);

        let row: (String, String, String, String) = conn
            .query_row(
                "SELECT status, queued_at, started_at, worker_id
                 FROM ingest_jobs
                 WHERE job_type = ?1 AND source_id = ?2 AND clip_id = 'clip_a'",
                params![JOB_TYPE_FILMSTRIP, JOB_SOURCE_FILMSTRIP],
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
    fn recoverable_projects_enqueue_imported_missing_filmstrip() {
        let base = std::env::temp_dir().join(format!(
            "qnc_filmstrip_recoverable_test_{}_{}",
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
        let worker = FilmstripWorker::new(
            paths.clone(),
            ProjectDbBroker::new(paths.clone()),
            BackgroundWorkGate::new(),
        );
        let conn = open_ingest(&paths, project_id).unwrap();
        conn.execute(
            "INSERT INTO ingest_assets (source_id, clip_id, name, import_status)
             VALUES ('card', 'clip_a', 'Clip A', 'imported')",
            [],
        )
        .unwrap();

        assert_eq!(worker.enqueue_recoverable_projects().unwrap(), 1);
        let status: String = conn
            .query_row(
                "SELECT status FROM ingest_jobs
                 WHERE job_type = ?1 AND source_id = ?2 AND clip_id = 'clip_a'",
                params![JOB_TYPE_FILMSTRIP, JOB_SOURCE_FILMSTRIP],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "queued");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn recoverable_projects_do_not_requeue_error_filmstrip() {
        let base = std::env::temp_dir().join(format!(
            "qnc_filmstrip_error_recoverable_test_{}_{}",
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
        let worker = FilmstripWorker::new(
            paths.clone(),
            ProjectDbBroker::new(paths.clone()),
            BackgroundWorkGate::new(),
        );
        let conn = open_ingest(&paths, project_id).unwrap();
        conn.execute(
            "INSERT INTO ingest_assets (source_id, clip_id, name, import_status)
             VALUES ('card', 'clip_a', 'Clip A', 'imported')",
            [],
        )
        .unwrap();
        worker.enqueue(project_id, "clip_a", Path::new("dummy.mp4"), 13);
        conn.execute(
            "UPDATE ingest_jobs
             SET status = 'error',
                 error = 'filmstrip media missing',
                 queued_at = 'original-queued',
                 started_at = 'original-started',
                 worker_id = 'worker-a'
             WHERE job_type = ?1 AND source_id = ?2 AND clip_id = 'clip_a'",
            params![JOB_TYPE_FILMSTRIP, JOB_SOURCE_FILMSTRIP],
        )
        .unwrap();

        assert_eq!(worker.enqueue_recoverable_projects().unwrap(), 0);
        let row: (String, String, String, String) = conn
            .query_row(
                "SELECT status, error, queued_at, worker_id
                 FROM ingest_jobs
                 WHERE job_type = ?1 AND source_id = ?2 AND clip_id = 'clip_a'",
                params![JOB_TYPE_FILMSTRIP, JOB_SOURCE_FILMSTRIP],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (
                "error".into(),
                "filmstrip media missing".into(),
                "original-queued".into(),
                "worker-a".into()
            )
        );

        let _ = fs::remove_dir_all(&base);
    }
}
