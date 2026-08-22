use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::{info, warn};

use crate::background_work::BackgroundWorkGate;
use crate::ingest::db::{open_ingest, queue_ingest_job, reset_processing_ingest_jobs_for_type};
use crate::ingest::thumb_process::generate_thumbs_from_proxy;
use crate::project::db::ProjectPaths;
use crate::project::{list_project_ids, ProjectDbBroker};
use qnc_service_contracts::MediaProcessor;

#[derive(Clone)]
struct ProxyThumbJob {
    project_id: String,
    clip_ids: Vec<String>,
}

#[derive(Clone)]
pub struct PosterWorker {
    paths: ProjectPaths,
    project_db: ProjectDbBroker,
    background: BackgroundWorkGate,
    media_processor: Arc<dyn MediaProcessor>,
    pending: Arc<Mutex<Vec<ProxyThumbJob>>>,
    blocked: Arc<Mutex<HashSet<String>>>,
    in_flight: Arc<AtomicUsize>,
}

impl PosterWorker {
    pub fn new(
        paths: ProjectPaths,
        project_db: ProjectDbBroker,
        background: BackgroundWorkGate,
        media_processor: Arc<dyn MediaProcessor>,
    ) -> Self {
        Self {
            paths,
            project_db,
            background,
            media_processor,
            pending: Arc::new(Mutex::new(Vec::new())),
            blocked: Arc::new(Mutex::new(HashSet::new())),
            in_flight: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub async fn wait_drained(&self, max_ms: u64) {
        let deadline = Instant::now() + Duration::from_millis(max_ms);
        while self.in_flight.load(Ordering::Acquire) > 0 {
            if Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub fn block_project(&self, project_id: &str) {
        let pid = project_id.trim();
        if pid.is_empty() {
            return;
        }
        self.blocked
            .lock()
            .expect("thumb block")
            .insert(pid.to_string());
        let mut q = self.pending.lock().expect("proxy thumb queue");
        q.retain(|j| j.project_id != pid);
    }

    fn is_blocked(&self, project_id: &str) -> bool {
        self.blocked
            .lock()
            .expect("thumb block")
            .contains(project_id)
    }

    /// Proces 2: generiranje postera iz proxya (orchestrator nakon copy-card).
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
        self.pending
            .lock()
            .expect("proxy thumb queue")
            .push(ProxyThumbJob {
                project_id: pid.to_string(),
                clip_ids: ids,
            });
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
            let count: i64 = self.project_db.serialize_project_write(&project_id, || {
                let conn = open_ingest(&self.paths, &project_id).map_err(|e| e.to_string())?;
                reset_processing_ingest_jobs_for_type(&conn, "thumb_proxy")
                    .map_err(|e| e.to_string())?;
                let count = conn
                    .query_row(
                        "SELECT COUNT(*) FROM ingest_assets
                         WHERE thumb_status IN ('pending', 'processing', 'no_card_thumb', 'error')
                           AND (
                                import_status IN ('imported', 'done')
                                OR project_proxy_path != ''
                                OR proxy_path != ''
                                OR card_thumb_path != ''
                           )",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                Ok(count)
            })?;
            if count > 0 {
                self.enqueue_proxy_generate(&project_id, &[]);
                queued += 1;
            }
        }
        Ok(queued)
    }

    pub fn spawn(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut last_recover = Instant::now();
            loop {
                if self.background.playback_active() {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
                let batch: Vec<ProxyThumbJob> = {
                    let mut q = self.pending.lock().expect("proxy thumb queue");
                    if q.is_empty() {
                        Vec::new()
                    } else {
                        q.drain(..).collect()
                    }
                };
                if batch.is_empty() {
                    if last_recover.elapsed() >= Duration::from_secs(8) {
                        let _ = self.enqueue_recoverable_projects();
                        last_recover = Instant::now();
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
                for job in batch {
                    if self.is_blocked(&job.project_id) {
                        continue;
                    }
                    let worker = self.clone();
                    let in_flight = worker.in_flight.clone();
                    let pid_log = job.project_id.clone();
                    let clip_ids = job.clip_ids.clone();
                    in_flight.fetch_add(1, Ordering::AcqRel);
                    let result = worker
                        .process_proxy_generate(&job.project_id, &clip_ids)
                        .await;
                    in_flight.fetch_sub(1, Ordering::AcqRel);
                    match result {
                        Ok(count) if count > 0 => {
                            info!(
                                "ingest proxy thumbs: project={} processed={}",
                                pid_log, count
                            );
                        }
                        Ok(_) => {}
                        Err(e) => warn!("ingest proxy thumbs: project={} err={}", pid_log, e),
                    }
                }
            }
        });
    }

    async fn process_proxy_generate(
        &self,
        project_id: &str,
        clip_ids: &[String],
    ) -> Result<usize, String> {
        if self.is_blocked(project_id) {
            return Ok(0);
        }
        generate_thumbs_from_proxy(
            &self.paths,
            &self.project_db,
            self.media_processor.clone(),
            project_id,
            clip_ids,
        )
        .await
    }

    fn queue_thumb_jobs(&self, project_id: &str, clip_ids: &[String]) -> Result<(), String> {
        let conn = open_ingest(&self.paths, project_id).map_err(|e| e.to_string())?;
        if clip_ids.is_empty() {
            let mut stmt = conn
                .prepare(
                    "SELECT source_id, clip_id FROM ingest_assets
                     WHERE thumb_status IN ('pending', 'processing', 'no_card_thumb', 'error')
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
                queue_ingest_job(&conn, "thumb_proxy", &source_id, &clip_id)
                    .map_err(|e| e.to_string())?;
            }
            return Ok(());
        }
        for clip_id in clip_ids {
            let source_id: String = conn
                .query_row(
                    "SELECT source_id FROM ingest_assets WHERE clip_id = ?1 ORDER BY source_id LIMIT 1",
                    rusqlite::params![clip_id],
                    |r| r.get(0),
                )
                .unwrap_or_default();
            if !source_id.trim().is_empty() {
                queue_ingest_job(&conn, "thumb_proxy", &source_id, clip_id)
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }
}
