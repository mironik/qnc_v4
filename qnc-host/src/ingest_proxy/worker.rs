use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tracing::{info, warn};

use crate::background_work::BackgroundWorkGate;
use crate::ingest::db::{ingest_job_has_active_external_lease, open_ingest, queue_ingest_job};
use crate::project::db::ProjectPaths;
use crate::project::{list_project_ids, ProjectDbBroker};

/// Scheduler-only proxy worker. Actual FFmpeg proxy generation is owned by
/// qnc-worker through JobService.
#[derive(Clone)]
pub struct ProxyGenerateWorker {
    paths: ProjectPaths,
    project_db: ProjectDbBroker,
    background: BackgroundWorkGate,
    blocked: Arc<Mutex<HashSet<String>>>,
}

impl ProxyGenerateWorker {
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

    pub fn block_project(&self, project_id: &str) {
        let pid = project_id.trim();
        if pid.is_empty() {
            return;
        }
        self.blocked
            .lock()
            .expect("proxy block")
            .insert(pid.to_string());
    }

    fn is_blocked(&self, project_id: &str) -> bool {
        self.blocked
            .lock()
            .expect("proxy block")
            .contains(project_id)
    }

    pub fn enqueue_clip(&self, project_id: &str, source_id: &str, clip_id: &str) {
        let pid = project_id.trim();
        let sid = source_id.trim();
        let cid = clip_id.trim();
        if pid.is_empty() || sid.is_empty() || cid.is_empty() || self.is_blocked(pid) {
            return;
        }
        self.queue_proxy_job(pid, sid, cid);
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
            let rows = self.recoverable_jobs_for_project(&project_id)?;
            for (source_id, clip_id) in rows {
                self.enqueue_clip(&project_id, &source_id, &clip_id);
                queued += 1;
            }
        }
        Ok(queued)
    }

    fn recoverable_jobs_for_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<(String, String)>, String> {
        self.project_db.serialize_project_write(project_id, || {
            let conn = open_ingest(&self.paths, project_id).map_err(|e| e.to_string())?;
            let mut stmt = conn
                .prepare(
                    "SELECT source_id, clip_id FROM ingest_assets
                     WHERE import_status = 'generating_proxy'
                     ORDER BY clip_id",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| e.to_string())?
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            Ok(rows)
        })
    }

    pub fn spawn(self: Arc<Self>) {
        info!("ingest proxy generate: scheduler only; external JobService owner");
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

    fn queue_proxy_job(&self, project_id: &str, source_id: &str, clip_id: &str) {
        if let Err(error) = self.project_db.serialize_project_write(project_id, || {
            let conn = open_ingest(&self.paths, project_id).map_err(|e| e.to_string())?;
            if ingest_job_has_active_external_lease(
                &conn,
                "proxy_generate",
                source_id,
                clip_id,
                now_unix_ms() as i64,
            )
            .map_err(|e| e.to_string())?
            {
                return Ok(());
            }
            queue_ingest_job(&conn, "proxy_generate", source_id, clip_id).map_err(|e| e.to_string())
        }) {
            warn!(
                "proxy queue: project={} source={} clip={} err={}",
                project_id, source_id, clip_id, error
            );
        }
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}
