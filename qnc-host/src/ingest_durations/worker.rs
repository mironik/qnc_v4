use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::{info, warn};

use crate::background_work::BackgroundWorkGate;
use crate::ingest::db::{open_ingest, set_meta};
use crate::ingest::store::{needs_duration_probe_conn, queue_missing_duration_probe_jobs};
use crate::project::db::ProjectPaths;
use crate::project::{list_project_ids, ProjectDbBroker};

/// Samostalan worker — ffprobe trajanja u pozadini (ne blokira discover/import).
#[derive(Clone)]
pub struct DurationWorker {
    paths: ProjectPaths,
    project_db: ProjectDbBroker,
    background: BackgroundWorkGate,
    pending: Arc<Mutex<HashSet<String>>>,
    blocked: Arc<Mutex<HashSet<String>>>,
}

impl DurationWorker {
    pub fn new(
        paths: ProjectPaths,
        project_db: ProjectDbBroker,
        background: BackgroundWorkGate,
    ) -> Self {
        Self {
            paths,
            project_db,
            background,
            pending: Arc::new(Mutex::new(HashSet::new())),
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
            .expect("duration block")
            .insert(pid.to_string());
        self.pending.lock().expect("duration queue").remove(pid);
    }

    fn is_blocked(&self, project_id: &str) -> bool {
        self.blocked
            .lock()
            .expect("duration block")
            .contains(project_id)
    }

    pub fn enqueue(&self, project_id: &str) {
        let pid = project_id.trim();
        if pid.is_empty() || self.is_blocked(pid) {
            return;
        }
        let _ = self.project_db.serialize_project_write(pid, || {
            if let Ok(conn) = open_ingest(&self.paths, pid) {
                let _ = set_meta(&conn, "durations_probe", "processing");
            }
            Ok(())
        });
        self.pending
            .lock()
            .expect("duration queue")
            .insert(pid.to_string());
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
            let needs = self
                .project_db
                .serialize_project_write(&project_id, || {
                    let conn = open_ingest(&self.paths, &project_id).map_err(|e| e.to_string())?;
                    needs_duration_probe_conn(&conn).map_err(|e| e.to_string())
                })
                .unwrap_or(false);
            if needs {
                self.enqueue(&project_id);
                queued += 1;
            }
        }
        Ok(queued)
    }

    pub fn spawn(self: Arc<Self>) {
        tokio::spawn(async move {
            // Probe missing durations from SQLite — memory queue is only a wake hint.
            let mut last_recover = Instant::now();
            loop {
                if self.background.playback_active() {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
                let batch: Vec<String> = {
                    let mut set = self.pending.lock().expect("duration queue");
                    set.drain().collect()
                };
                if batch.is_empty() {
                    if last_recover.elapsed() >= Duration::from_secs(4) {
                        let _ = self.enqueue_recoverable_projects();
                        last_recover = Instant::now();
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
                for project_id in batch {
                    let worker = self.clone();
                    let pid_log = project_id.clone();
                    let result = worker.process_project(&project_id).await;
                    match result {
                        Ok(n) if n > 0 => {
                            info!("ingest durations: project={} filled={}", pid_log, n);
                        }
                        Ok(_) => {}
                        Err(e) => warn!("ingest durations: project={} err={}", pid_log, e),
                    }
                }
            }
        });
    }

    async fn process_project(&self, project_id: &str) -> Result<usize, String> {
        if self.is_blocked(project_id) {
            return Ok(0);
        }
        queue_missing_duration_probe_jobs(&self.paths, &self.project_db, project_id)
    }
}
