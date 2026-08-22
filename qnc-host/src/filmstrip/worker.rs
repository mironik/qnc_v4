use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::{info, warn};

use crate::background_work::BackgroundWorkGate;
use crate::ingest::thumb::timeline_seek_seconds;
use crate::media::imported_clip_media_rows;
use crate::project::db::{open_global, ProjectPaths};
use crate::project::list_project_ids;
use qnc_service_contracts::MediaProcessor;

use super::build::build_for_clip;
use super::store::{get_filmstrip, list_frames_for_clip};

#[derive(Clone)]
struct FilmstripJob {
    project_id: String,
    clip_id: String,
    media_path: PathBuf,
    frames: u32,
}

#[derive(Clone)]
pub struct FilmstripWorker {
    paths: ProjectPaths,
    background: BackgroundWorkGate,
    media_processor: Arc<dyn MediaProcessor>,
    pending: Arc<Mutex<Vec<FilmstripJob>>>,
    blocked: Arc<Mutex<HashSet<String>>>,
    in_flight: Arc<AtomicUsize>,
}

impl FilmstripWorker {
    pub fn new(
        paths: ProjectPaths,
        background: BackgroundWorkGate,
        media_processor: Arc<dyn MediaProcessor>,
    ) -> Self {
        Self {
            paths,
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
            .expect("filmstrip block")
            .insert(pid.to_string());
        let mut q = self.pending.lock().expect("filmstrip queue");
        q.retain(|j| j.project_id != pid);
    }

    fn is_blocked(&self, project_id: &str) -> bool {
        self.blocked
            .lock()
            .expect("filmstrip block")
            .contains(project_id)
    }

    pub fn enqueue(&self, project_id: &str, clip_id: &str, media_path: &Path, frames: u32) {
        let pid = project_id.trim();
        let cid = clip_id.trim();
        if pid.is_empty() || cid.is_empty() || !media_path.is_file() {
            return;
        }
        if self.is_blocked(pid) {
            return;
        }
        let frames = frames.max(super::DEFAULT_FILMSTRIP_FRAMES);
        let mut pending = self.pending.lock().expect("filmstrip queue");
        if pending
            .iter()
            .any(|job| job.project_id == pid && job.clip_id == cid)
        {
            return;
        }
        pending.push(FilmstripJob {
            project_id: pid.to_string(),
            clip_id: cid.to_string(),
            media_path: media_path.to_path_buf(),
            frames,
        });
    }

    pub fn enqueue_recoverable_projects(&self, frames: u32) -> Result<usize, String> {
        let global = open_global(&self.paths).map_err(|e| e.to_string())?;
        let mut queued = 0usize;
        for project_id in list_project_ids(&global).map_err(|e| e.to_string())? {
            if self.is_blocked(&project_id) {
                continue;
            }
            for (clip_id, media) in imported_clip_media_rows(&self.paths, &project_id)? {
                if filmstrip_ready(&self.paths, &project_id, &clip_id) {
                    continue;
                }
                self.enqueue(&project_id, &clip_id, &media, frames);
                queued += 1;
            }
        }
        Ok(queued)
    }

    pub fn spawn(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut last_recover = Instant::now();
            let frames = super::DEFAULT_FILMSTRIP_FRAMES;
            loop {
                if self.background.playback_active() {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    continue;
                }
                let job: Option<FilmstripJob> = {
                    let mut q = self.pending.lock().expect("filmstrip queue");
                    if q.is_empty() {
                        None
                    } else {
                        Some(q.remove(0))
                    }
                };
                if job.is_none() {
                    if last_recover.elapsed() >= Duration::from_secs(8) {
                        let _ = self.enqueue_recoverable_projects(frames);
                        last_recover = Instant::now();
                    }
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    continue;
                }
                let job = job.unwrap();
                // Defer only if THIS clip's media is not ready yet (other clips may still encode).
                if !job.media_path.is_file() {
                    self.pending.lock().expect("filmstrip queue").push(job);
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
                let frames = job.frames;
                in_flight.fetch_add(1, Ordering::AcqRel);
                let result = build_for_clip(
                    &worker.paths,
                    worker.media_processor.clone(),
                    &pid,
                    &cid,
                    &media,
                    frames,
                )
                .await;
                in_flight.fetch_sub(1, Ordering::AcqRel);
                match result {
                    Ok(()) => info!("filmstrip: project={} clip={}", pid_log, cid_log),
                    Err(e) => {
                        warn!("filmstrip: project={} clip={} err={}", pid_log, cid_log, e)
                    }
                }
            }
        });
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
