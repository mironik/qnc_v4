use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::{info, warn};

use crate::background_work::BackgroundWorkGate;
use crate::ingest::db::{ingest_job_has_active_external_lease, open_ingest, queue_ingest_job};
use crate::ingest::thumb::timeline_seek_seconds;
use crate::project::db::ProjectPaths;
use crate::project::ProjectDbBroker;
use qnc_service_contracts::{JOB_SOURCE_FILMSTRIP, JOB_TYPE_FILMSTRIP};

use super::store::{get_filmstrip, list_frames_for_clip};

#[derive(Clone)]
pub struct FilmstripWorker {
    paths: ProjectPaths,
    project_db: ProjectDbBroker,
    _background: BackgroundWorkGate,
    blocked: Arc<Mutex<HashSet<String>>>,
}

impl FilmstripWorker {
    pub fn new(
        paths: ProjectPaths,
        project_db: ProjectDbBroker,
        background: BackgroundWorkGate,
    ) -> Self {
        Self {
            paths,
            project_db,
            _background: background,
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
            .expect("filmstrip block")
            .insert(pid.to_string());
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
        let _ = (media_path, frames, priority);
        let pid = project_id.trim();
        let cid = clip_id.trim();
        if pid.is_empty() || cid.is_empty() {
            return;
        }
        if self.is_blocked(pid) {
            return;
        }
        self.queue_filmstrip_job(pid, cid);
    }

    pub fn spawn(self: Arc<Self>) {
        let _ = self;
        info!("filmstrip: scheduler only; external JobService owner");
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
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
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
