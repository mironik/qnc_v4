use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::{info, warn};

use crate::background_work::BackgroundWorkGate;
use crate::ingest::db::{
    open_ingest, queue_ingest_job, set_poster_proxy_generation_approved, set_thumb_status,
};
use crate::project::db::ProjectPaths;
use crate::project::ProjectDbBroker;

#[derive(Clone)]
pub struct PosterWorker {
    paths: ProjectPaths,
    project_db: ProjectDbBroker,
    background: BackgroundWorkGate,
    blocked: Arc<Mutex<HashSet<String>>>,
}

impl PosterWorker {
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
            .expect("thumb block")
            .insert(pid.to_string());
    }

    fn is_blocked(&self, project_id: &str) -> bool {
        self.blocked
            .lock()
            .expect("thumb block")
            .contains(project_id)
    }

    /// Proces 2: eksplicitno odobreno generiranje postera iz proxya.
    pub fn enqueue_proxy_generate(&self, project_id: &str, clip_ids: &[String]) {
        let pid = project_id.trim();
        if pid.is_empty() || self.is_blocked(pid) {
            return;
        }
        let ids: Vec<String> = clip_ids
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if let Err(err) = self
            .project_db
            .serialize_project_write(pid, || self.queue_thumb_jobs(pid, &ids))
        {
            warn!(
                "ingest proxy thumbs: project={} queue jobs err={}",
                pid, err
            );
        }
    }

    pub fn enqueue_recoverable_projects(&self) -> Result<usize, String> {
        Ok(0)
    }

    pub fn spawn(self: Arc<Self>) {
        info!("ingest proxy thumbs: scheduler only; external JobService owner");
        tokio::spawn(async move {
            let mut last_recover = Instant::now();
            loop {
                if self.background.playback_active() {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
                if last_recover.elapsed() >= Duration::from_secs(8) {
                    let _ = self.enqueue_recoverable_projects();
                    last_recover = Instant::now();
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        });
    }

    fn queue_thumb_jobs(&self, project_id: &str, clip_ids: &[String]) -> Result<(), String> {
        let conn = open_ingest(&self.paths, project_id).map_err(|e| e.to_string())?;
        if clip_ids.is_empty() {
            let mut stmt = conn
                .prepare(
                    "SELECT source_id, clip_id FROM ingest_assets
                     WHERE thumb_status IN ('no_card_thumb', 'error')
                       AND (
                            import_status IN ('imported', 'done')
                            OR project_proxy_path != ''
                            OR proxy_path != ''
                            OR card_thumb_path != ''
                       )
                     ORDER BY clip_id",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| e.to_string())?;
            for row in rows {
                let (source_id, clip_id) = row.map_err(|e| e.to_string())?;
                set_poster_proxy_generation_approved(&conn, &source_id, &clip_id, true)
                    .map_err(|e| e.to_string())?;
                set_thumb_status(&conn, &source_id, &clip_id, "pending", "")
                    .map_err(|e| e.to_string())?;
                queue_ingest_job(&conn, "thumb_proxy", &source_id, &clip_id)
                    .map_err(|e| e.to_string())?;
            }
            return Ok(());
        }
        for clip_id in clip_ids {
            let source_id: String = conn
                .query_row(
                    "SELECT source_id FROM ingest_assets
                     WHERE clip_id = ?1 AND thumb_status != 'ready'
                     ORDER BY source_id LIMIT 1",
                    rusqlite::params![clip_id],
                    |r| r.get(0),
                )
                .unwrap_or_default();
            if !source_id.trim().is_empty() {
                set_poster_proxy_generation_approved(&conn, &source_id, clip_id, true)
                    .map_err(|e| e.to_string())?;
                set_thumb_status(&conn, &source_id, clip_id, "pending", "")
                    .map_err(|e| e.to_string())?;
                queue_ingest_job(&conn, "thumb_proxy", &source_id, clip_id)
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::db::{ingest_job_id, poster_proxy_generation_approved_for_asset};
    use rusqlite::params;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_paths(label: &str) -> ProjectPaths {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let base = std::env::temp_dir().join(format!(
            "qnc_ingest_posters_{label}_{}_{}",
            std::process::id(),
            nanos
        ));
        let _ = std::fs::remove_dir_all(&base);
        ProjectPaths {
            data_dir: base.join("data"),
            projects_root: base.join("projects"),
            seed_path: base.join("seed.json"),
        }
    }

    #[test]
    fn enqueue_proxy_generate_records_user_approval_and_pending_job() {
        let paths = test_paths("approve_proxy");
        let conn = open_ingest(&paths, "project_a").unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, import_status, status,
                 thumb_status, thumb_error, project_proxy_path)
             VALUES ('card', 'clip_a', 'Clip A', 'clip_a', 'imported', 'imported',
                     'no_card_thumb', 'missing card poster', 'proxy/clip_a.mp4')",
            [],
        )
        .unwrap();
        drop(conn);

        let worker = PosterWorker::new(
            paths.clone(),
            ProjectDbBroker::new(paths.clone()),
            BackgroundWorkGate::new(),
        );
        worker.enqueue_proxy_generate("project_a", &[String::from("clip_a")]);

        let conn = open_ingest(&paths, "project_a").unwrap();
        assert!(
            poster_proxy_generation_approved_for_asset(&conn, "card", "clip_a").unwrap(),
            "proxy poster generation must be persisted before worker claim"
        );
        let row: (String, String, String) = conn
            .query_row(
                "SELECT thumb_status, thumb_error, status
                 FROM ingest_assets
                 WHERE source_id = 'card' AND clip_id = 'clip_a'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(row, ("pending".into(), "".into(), "imported".into()));

        let job_id = ingest_job_id("thumb_proxy", "card", "clip_a");
        let status: String = conn
            .query_row(
                "SELECT status FROM ingest_jobs WHERE job_id = ?1",
                params![job_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "queued");
    }
}
