use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing::{info, warn};

use crate::ingest::thumb_process::copy_thumbs_from_card;
use crate::ingest_posters::PosterWorker;
use crate::project::db::{open_global, ProjectPaths};
use crate::project::list_project_ids;

/// Samostalan worker — kopija THM/JPG s kartice u ingest poster (bez ffmpeg).
#[derive(Clone)]
pub struct CardThumbWorker {
    paths: ProjectPaths,
    posters: Arc<PosterWorker>,
    pending: Arc<Mutex<HashSet<String>>>,
    blocked: Arc<Mutex<HashSet<String>>>,
}

impl CardThumbWorker {
    pub fn new(paths: ProjectPaths, posters: Arc<PosterWorker>) -> Self {
        Self {
            paths,
            posters,
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
            .expect("card thumb block")
            .insert(pid.to_string());
        self.pending.lock().expect("card thumb queue").remove(pid);
    }

    fn is_blocked(&self, project_id: &str) -> bool {
        self.blocked
            .lock()
            .expect("card thumb block")
            .contains(project_id)
    }

    pub fn enqueue(&self, project_id: &str) {
        let pid = project_id.trim();
        if pid.is_empty() || self.is_blocked(pid) {
            return;
        }
        self.pending
            .lock()
            .expect("card thumb queue")
            .insert(pid.to_string());
    }

    pub fn enqueue_recoverable_projects(&self) -> Result<usize, String> {
        let global = open_global(&self.paths).map_err(|e| e.to_string())?;
        let mut queued = 0usize;
        for project_id in list_project_ids(&global).map_err(|e| e.to_string())? {
            if self.is_blocked(&project_id) {
                continue;
            }
            let conn = crate::ingest::db::open_ingest(&self.paths, &project_id)
                .map_err(|e| e.to_string())?;
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM ingest_assets
                     WHERE thumb_status NOT IN ('ready')",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if count > 0 {
                self.enqueue(&project_id);
                queued += 1;
            }
        }
        Ok(queued)
    }

    pub fn spawn(self: Arc<Self>) {
        tokio::spawn(async move {
            loop {
                let batch: Vec<String> = {
                    let mut set = self.pending.lock().expect("card thumb queue");
                    set.drain().collect()
                };
                if batch.is_empty() {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
                for project_id in batch {
                    let worker = self.clone();
                    let pid_log = project_id.clone();
                    let result =
                        tokio::task::spawn_blocking(move || worker.process_project(&project_id))
                            .await;
                    match result {
                        Ok(Ok(copied)) if copied > 0 => {
                            info!("ingest card thumbs: project={} copied={}", pid_log, copied);
                        }
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => warn!("ingest card thumbs: project={} err={}", pid_log, e),
                        Err(e) => warn!("ingest card thumbs: project={} task err={}", pid_log, e),
                    }
                }
            }
        });
    }

    fn process_project(&self, project_id: &str) -> Result<usize, String> {
        if self.is_blocked(project_id) {
            return Ok(0);
        }
        let result = copy_thumbs_from_card(&self.paths, project_id)?;
        if !result.no_thumb_clip_ids.is_empty() {
            self.posters
                .enqueue_proxy_generate(project_id, &result.no_thumb_clip_ids);
        }
        Ok(result.copied)
    }
}
