use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::{info, warn};

use crate::background_work::BackgroundWorkGate;
use crate::media::imported_clip_media_rows;
use crate::project::db::ProjectPaths;
use crate::project::{list_project_ids, ProjectDbBroker};
use qnc_service_contracts::MediaProcessor;

use super::store::{build_for_clip, ready as waveform_ready};

#[derive(Clone)]
struct WaveformJob {
    project_id: String,
    clip_id: String,
    media_path: PathBuf,
}

#[derive(Clone)]
pub struct WaveformWorker {
    paths: ProjectPaths,
    project_db: ProjectDbBroker,
    background: BackgroundWorkGate,
    media_processor: Arc<dyn MediaProcessor>,
    pending: Arc<Mutex<Vec<WaveformJob>>>,
    blocked: Arc<Mutex<HashSet<String>>>,
    in_flight: Arc<AtomicUsize>,
}

impl WaveformWorker {
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
            .expect("waveform block")
            .insert(pid.to_string());
        self.pending
            .lock()
            .expect("waveform queue")
            .retain(|job| job.project_id != pid);
    }

    fn is_blocked(&self, project_id: &str) -> bool {
        self.blocked
            .lock()
            .expect("waveform block")
            .contains(project_id)
    }

    pub fn enqueue(&self, project_id: &str, clip_id: &str, media_path: &Path) {
        let pid = project_id.trim();
        let cid = clip_id.trim();
        if pid.is_empty() || cid.is_empty() || !media_path.is_file() {
            return;
        }
        if self.is_blocked(pid) || waveform_ready(&self.paths, pid, cid) {
            return;
        }
        let mut pending = self.pending.lock().expect("waveform queue");
        if pending
            .iter()
            .any(|job| job.project_id == pid && job.clip_id == cid)
        {
            return;
        }
        pending.push(WaveformJob {
            project_id: pid.to_string(),
            clip_id: cid.to_string(),
            media_path: media_path.to_path_buf(),
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
            for (clip_id, media) in imported_clip_media_rows(&self.paths, &project_id)? {
                if waveform_ready(&self.paths, &project_id, &clip_id) {
                    continue;
                }
                self.enqueue(&project_id, &clip_id, &media);
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
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    continue;
                }
                let job: Option<WaveformJob> = {
                    let mut q = self.pending.lock().expect("waveform queue");
                    if q.is_empty() {
                        None
                    } else {
                        Some(q.remove(0))
                    }
                };
                if job.is_none() {
                    if last_recover.elapsed() >= Duration::from_secs(30) {
                        let _ = self.enqueue_recoverable_projects();
                        last_recover = Instant::now();
                    }
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    continue;
                }
                let job = job.unwrap();
                // Defer only until this clip's media exists (don't wait on other encodes).
                if !job.media_path.is_file() {
                    self.pending.lock().expect("waveform queue").push(job);
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    continue;
                }
                if self.is_blocked(&job.project_id) {
                    continue;
                }
                let worker = self.clone();
                let in_flight = worker.in_flight.clone();
                let pid_log = job.project_id.clone();
                let cid_log = job.clip_id.clone();
                let pid = job.project_id;
                let cid = job.clip_id;
                let media = job.media_path;
                in_flight.fetch_add(1, Ordering::AcqRel);
                let result = build_for_clip(
                    &worker.paths,
                    &worker.project_db,
                    worker.media_processor.clone(),
                    &pid,
                    &cid,
                    &media,
                )
                .await;
                in_flight.fetch_sub(1, Ordering::AcqRel);
                match result {
                    Ok(()) => info!("waveform: project={} clip={}", pid_log, cid_log),
                    Err(e) => {
                        warn!("waveform: project={} clip={} err={}", pid_log, cid_log, e)
                    }
                }
            }
        });
    }
}
