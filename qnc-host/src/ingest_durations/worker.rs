use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::{info, warn};

use crate::background_work::BackgroundWorkGate;
use crate::ingest::db::{open_ingest, set_meta};
use crate::ingest::store::{needs_duration_probe, probe_missing_durations};
use crate::project::db::{open_global, ProjectPaths};
use crate::project::list_project_ids;
use qnc_service_contracts::MediaProcessor;

/// Samostalan worker — ffprobe trajanja u pozadini (ne blokira discover/import).
#[derive(Clone)]
pub struct DurationWorker {
    paths: ProjectPaths,
    background: BackgroundWorkGate,
    media_processor: Arc<dyn MediaProcessor>,
    pending: Arc<Mutex<HashSet<String>>>,
    blocked: Arc<Mutex<HashSet<String>>>,
}

impl DurationWorker {
    pub fn new(
        paths: ProjectPaths,
        background: BackgroundWorkGate,
        media_processor: Arc<dyn MediaProcessor>,
    ) -> Self {
        Self {
            paths,
            background,
            media_processor,
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
        if let Ok(conn) = open_ingest(&self.paths, pid) {
            let _ = set_meta(&conn, "durations_probe", "processing");
        }
        self.pending
            .lock()
            .expect("duration queue")
            .insert(pid.to_string());
    }

    pub fn enqueue_recoverable_projects(&self) -> Result<usize, String> {
        let global = open_global(&self.paths).map_err(|e| e.to_string())?;
        let mut queued = 0usize;
        for project_id in list_project_ids(&global).map_err(|e| e.to_string())? {
            if self.is_blocked(&project_id) {
                continue;
            }
            if needs_duration_probe(&self.paths, &project_id).unwrap_or(false) {
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
        probe_missing_durations(&self.paths, self.media_processor.clone(), project_id)
            .await
            .map_err(|e| e.to_string())
    }
}
