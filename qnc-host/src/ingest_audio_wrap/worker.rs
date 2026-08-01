//! Background worker — AV wrap for imported audio, driven by SQLite fps
//! (same deferred pattern as waveform peaks after video import).

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::{info, warn};

use crate::ingest::audio_wrap::process_project_audio_wraps;
use crate::ingest::db::open_ingest;
use crate::project::db::{open_global, ProjectPaths};
use crate::project::list_project_ids;

#[derive(Clone)]
pub struct AudioWrapWorker {
    paths: ProjectPaths,
    pending: Arc<Mutex<HashSet<String>>>,
    blocked: Arc<Mutex<HashSet<String>>>,
}

impl AudioWrapWorker {
    pub fn new(paths: ProjectPaths) -> Self {
        Self {
            paths,
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
            .expect("audio wrap block")
            .insert(pid.to_string());
        self.pending.lock().expect("audio wrap queue").remove(pid);
    }

    fn is_blocked(&self, project_id: &str) -> bool {
        self.blocked
            .lock()
            .expect("audio wrap block")
            .contains(project_id)
    }

    pub fn enqueue(&self, project_id: &str) {
        let pid = project_id.trim();
        if pid.is_empty() || self.is_blocked(pid) {
            return;
        }
        self.pending
            .lock()
            .expect("audio wrap queue")
            .insert(pid.to_string());
    }

    /// Projects that have imported audio waiting for wraps (metadata in SQLite).
    pub fn enqueue_recoverable_projects(&self) -> Result<usize, String> {
        let global = open_global(&self.paths).map_err(|e| e.to_string())?;
        let mut queued = 0usize;
        for project_id in list_project_ids(&global).map_err(|e| e.to_string())? {
            if self.is_blocked(&project_id) {
                continue;
            }
            if !project_has_pending_audio_wrap(&self.paths, &project_id) {
                continue;
            }
            self.enqueue(&project_id);
            queued += 1;
        }
        Ok(queued)
    }

    pub fn spawn(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut last_recover = Instant::now();
            loop {
                let batch: Vec<String> = {
                    let mut set = self.pending.lock().expect("audio wrap queue");
                    set.drain().collect()
                };
                if batch.is_empty() {
                    // Like waveform recovery — pick up when video fps lands in DB.
                    if last_recover.elapsed() >= Duration::from_secs(4) {
                        let _ = self.enqueue_recoverable_projects();
                        last_recover = Instant::now();
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
                for project_id in batch {
                    if self.is_blocked(&project_id) {
                        continue;
                    }
                    let worker = self.clone();
                    let pid = project_id.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        process_project_audio_wraps(&worker.paths, &pid)
                    })
                    .await;
                    match result {
                        Ok(Ok(n)) if n > 0 => {
                            info!("ingest audio wrap: project={} built={}", project_id, n)
                        }
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => warn!("ingest audio wrap: project={} err={}", project_id, e),
                        Err(e) => {
                            warn!("ingest audio wrap: project={} join err={}", project_id, e)
                        }
                    }
                }
            }
        });
    }
}

fn project_has_pending_audio_wrap(paths: &ProjectPaths, project_id: &str) -> bool {
    let Ok(conn) = open_ingest(paths, project_id) else {
        return false;
    };
    conn.query_row(
        "SELECT 1 FROM ingest_assets
         WHERE import_status IN ('imported', 'done')
           AND metadata_json LIKE '%audio_project_path%'
         LIMIT 1",
        [],
        |_| Ok(1i64),
    )
    .is_ok()
}
