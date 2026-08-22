use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tracing::{info, warn};

use crate::background_work::BackgroundWorkGate;
use crate::ingest::db::{
    ingest_job_has_active_external_lease, mark_ingest_job_done, mark_ingest_job_error,
    mark_ingest_job_processing, open_ingest, queue_ingest_job,
};
use crate::ingest::thumb::timeline_seek_seconds;
use crate::media::imported_filmstrip_media_rows;
use crate::project::db::ProjectPaths;
use crate::project::{list_project_ids, ProjectDbBroker};
use qnc_service_contracts::{MediaProcessor, JOB_SOURCE_FILMSTRIP, JOB_TYPE_FILMSTRIP};

use super::build::build_for_clip;
use super::store::{get_filmstrip, list_frames_for_clip};

#[derive(Clone, Debug, PartialEq, Eq)]
struct FilmstripJob {
    project_id: String,
    clip_id: String,
    media_path: PathBuf,
    frames: u32,
    priority: bool,
}

#[derive(Clone)]
pub struct FilmstripWorker {
    paths: ProjectPaths,
    project_db: ProjectDbBroker,
    background: BackgroundWorkGate,
    media_processor: Arc<dyn MediaProcessor>,
    pending: Arc<Mutex<Vec<FilmstripJob>>>,
    blocked: Arc<Mutex<HashSet<String>>>,
    in_flight: Arc<AtomicUsize>,
}

impl FilmstripWorker {
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
        let pid = project_id.trim();
        let cid = clip_id.trim();
        if pid.is_empty() || cid.is_empty() || !media_path.is_file() {
            return;
        }
        if self.is_blocked(pid) {
            return;
        }
        let frames = frames.max(super::DEFAULT_FILMSTRIP_FRAMES);
        self.queue_filmstrip_job(pid, cid);
        let mut pending = self.pending.lock().expect("filmstrip queue");
        push_filmstrip_job(
            &mut pending,
            FilmstripJob {
                project_id: pid.to_string(),
                clip_id: cid.to_string(),
                media_path: media_path.to_path_buf(),
                frames,
                priority,
            },
        );
    }

    pub fn enqueue_recoverable_projects(&self, frames: u32) -> Result<usize, String> {
        let project_ids = self
            .project_db
            .with_global(|global| list_project_ids(global).map_err(|e| e.to_string()))?;
        let mut queued = 0usize;
        for project_id in project_ids {
            if self.is_blocked(&project_id) {
                continue;
            }
            for (clip_id, media) in imported_filmstrip_media_rows(&self.paths, &project_id)? {
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
                match worker.claim_local_filmstrip_job(&pid, &cid) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(error) => {
                        warn!(
                            "filmstrip claim: project={} clip={} err={}",
                            pid_log, cid_log, error
                        );
                        continue;
                    }
                }
                in_flight.fetch_add(1, Ordering::AcqRel);
                let result = build_for_clip(
                    &worker.paths,
                    &worker.project_db,
                    worker.media_processor.clone(),
                    &pid,
                    &cid,
                    &media,
                    frames,
                )
                .await;
                in_flight.fetch_sub(1, Ordering::AcqRel);
                match result {
                    Ok(()) => {
                        let _ = worker.finish_local_filmstrip_job(&pid_log, &cid_log, None);
                        info!("filmstrip: project={} clip={}", pid_log, cid_log)
                    }
                    Err(e) => {
                        let _ = worker.finish_local_filmstrip_job(&pid_log, &cid_log, Some(&e));
                        warn!("filmstrip: project={} clip={} err={}", pid_log, cid_log, e)
                    }
                }
            }
        });
    }

    fn queue_filmstrip_job(&self, project_id: &str, clip_id: &str) {
        if let Err(error) = self.project_db.serialize_project_write(project_id, || {
            let conn = open_ingest(&self.paths, project_id).map_err(|e| e.to_string())?;
            if ingest_job_has_active_external_lease(
                &conn,
                JOB_TYPE_FILMSTRIP,
                JOB_SOURCE_FILMSTRIP,
                clip_id,
                now_unix_ms() as i64,
            )
            .map_err(|e| e.to_string())?
            {
                return Ok(());
            }
            queue_ingest_job(&conn, JOB_TYPE_FILMSTRIP, JOB_SOURCE_FILMSTRIP, clip_id)
                .map_err(|e| e.to_string())
        }) {
            warn!(
                "filmstrip queue: project={} clip={} err={}",
                project_id, clip_id, error
            );
        }
    }

    fn claim_local_filmstrip_job(&self, project_id: &str, clip_id: &str) -> Result<bool, String> {
        self.project_db.serialize_project_write(project_id, || {
            let conn = open_ingest(&self.paths, project_id).map_err(|e| e.to_string())?;
            if ingest_job_has_active_external_lease(
                &conn,
                JOB_TYPE_FILMSTRIP,
                JOB_SOURCE_FILMSTRIP,
                clip_id,
                now_unix_ms() as i64,
            )
            .map_err(|e| e.to_string())?
            {
                return Ok(false);
            }
            mark_ingest_job_processing(&conn, JOB_TYPE_FILMSTRIP, JOB_SOURCE_FILMSTRIP, clip_id)
                .map_err(|e| e.to_string())?;
            Ok(true)
        })
    }

    fn finish_local_filmstrip_job(
        &self,
        project_id: &str,
        clip_id: &str,
        error: Option<&str>,
    ) -> Result<(), String> {
        self.project_db.serialize_project_write(project_id, || {
            let conn = open_ingest(&self.paths, project_id).map_err(|e| e.to_string())?;
            match error {
                Some(message) => mark_ingest_job_error(
                    &conn,
                    JOB_TYPE_FILMSTRIP,
                    JOB_SOURCE_FILMSTRIP,
                    clip_id,
                    message,
                ),
                None => {
                    mark_ingest_job_done(&conn, JOB_TYPE_FILMSTRIP, JOB_SOURCE_FILMSTRIP, clip_id)
                }
            }
            .map_err(|e| e.to_string())
        })
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn push_filmstrip_job(pending: &mut Vec<FilmstripJob>, job: FilmstripJob) {
    if let Some(pos) = pending.iter().position(|existing| {
        existing.project_id == job.project_id && existing.clip_id == job.clip_id
    }) {
        let mut existing = pending.remove(pos);
        existing.media_path = job.media_path;
        existing.frames = job.frames;
        existing.priority |= job.priority;
        insert_filmstrip_job(pending, existing);
        return;
    }
    insert_filmstrip_job(pending, job);
}

fn insert_filmstrip_job(pending: &mut Vec<FilmstripJob>, job: FilmstripJob) {
    if job.priority {
        let pos = pending
            .iter()
            .position(|existing| !existing.priority)
            .unwrap_or(pending.len());
        pending.insert(pos, job);
    } else {
        pending.push(job);
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

    fn job(clip_id: &str, priority: bool) -> FilmstripJob {
        FilmstripJob {
            project_id: "p".into(),
            clip_id: clip_id.into(),
            media_path: PathBuf::from(format!("{clip_id}.mp4")),
            frames: 13,
            priority,
        }
    }

    #[test]
    fn priority_jobs_run_before_normal_backlog() {
        let mut pending = vec![job("a", false), job("b", false)];

        push_filmstrip_job(&mut pending, job("selected", true));

        assert_eq!(
            pending
                .iter()
                .map(|job| job.clip_id.as_str())
                .collect::<Vec<_>>(),
            vec!["selected", "a", "b"]
        );
    }

    #[test]
    fn priority_duplicate_moves_existing_job_forward() {
        let mut pending = vec![job("a", false), job("selected", false), job("b", false)];

        push_filmstrip_job(&mut pending, job("selected", true));

        assert_eq!(
            pending
                .iter()
                .map(|job| (job.clip_id.as_str(), job.priority))
                .collect::<Vec<_>>(),
            vec![("selected", true), ("a", false), ("b", false)]
        );
    }

    #[test]
    fn normal_duplicate_does_not_demote_priority_job() {
        let mut pending = vec![job("selected", true), job("a", false)];

        push_filmstrip_job(&mut pending, job("selected", false));

        assert_eq!(
            pending
                .iter()
                .map(|job| (job.clip_id.as_str(), job.priority))
                .collect::<Vec<_>>(),
            vec![("selected", true), ("a", false)]
        );
    }
}
