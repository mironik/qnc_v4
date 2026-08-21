use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::params;
use tracing::{info, warn};

use crate::background_work::BackgroundWorkGate;
use crate::filmstrip::{FilmstripWorker, DEFAULT_FILMSTRIP_FRAMES};
use crate::ingest::asset_row::IngestAssetRow;
use crate::ingest::db::{
    ingest_asset_meta, mark_ingest_job_done, mark_ingest_job_error, mark_ingest_job_processing,
    open_ingest, queue_ingest_job,
};
use crate::ingest::import_finish::complete_imported_clip;
use crate::ingest::proxy_generate::{generate_field_proxy, proxy_dest_for_source};
use crate::ingest::store::row_import_error;
use crate::ingest_posters::PosterWorker;
use crate::media::resolve_import_plan;
use crate::project::db::{open_global, project_settings_snapshot, ProjectPaths};
use crate::project::list_project_ids;
use serde_json::json;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct ProxyClipJob {
    project_id: String,
    source_id: String,
    clip_id: String,
}

/// Samostalan worker — ffmpeg proxy generate (GPU/CPU). Ne dijeli proces s importom.
#[derive(Clone)]
pub struct ProxyGenerateWorker {
    paths: ProjectPaths,
    filmstrip: Arc<FilmstripWorker>,
    posters: Arc<PosterWorker>,
    background: BackgroundWorkGate,
    pending: Arc<Mutex<HashSet<ProxyClipJob>>>,
    blocked: Arc<Mutex<HashSet<String>>>,
}

impl ProxyGenerateWorker {
    pub fn new(
        paths: ProjectPaths,
        filmstrip: Arc<FilmstripWorker>,
        posters: Arc<PosterWorker>,
        background: BackgroundWorkGate,
    ) -> Self {
        Self {
            paths,
            filmstrip,
            posters,
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
            .expect("proxy block")
            .insert(pid.to_string());
        let mut q = self.pending.lock().expect("proxy queue");
        q.retain(|j| j.project_id != pid);
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
        self.pending
            .lock()
            .expect("proxy queue")
            .insert(ProxyClipJob {
                project_id: pid.to_string(),
                source_id: sid.to_string(),
                clip_id: cid.to_string(),
            });
    }

    pub fn enqueue_recoverable_projects(&self) -> Result<usize, String> {
        let global = open_global(&self.paths).map_err(|e| e.to_string())?;
        let mut queued = 0usize;
        for project_id in list_project_ids(&global).map_err(|e| e.to_string())? {
            if self.is_blocked(&project_id) {
                continue;
            }
            let conn = open_ingest(&self.paths, &project_id).map_err(|e| e.to_string())?;
            let mut stmt = conn
                .prepare(
                    "SELECT source_id, clip_id FROM ingest_assets
                     WHERE import_status = 'generating_proxy'
                     ORDER BY clip_id",
                )
                .map_err(|e| e.to_string())?;
            let rows: Vec<(String, String)> = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| e.to_string())?
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            for (source_id, clip_id) in rows {
                self.enqueue_clip(&project_id, &source_id, &clip_id);
                queued += 1;
            }
        }
        Ok(queued)
    }

    pub fn spawn(self: Arc<Self>) {
        tokio::spawn(async move {
            // Wake hint in memory; recoverable work comes from SQLite generating_proxy.
            let mut last_recover = Instant::now();
            loop {
                if self.background.playback_active() {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
                let batch: Vec<ProxyClipJob> = {
                    let mut set = self.pending.lock().expect("proxy queue");
                    set.drain().collect()
                };
                if batch.is_empty() {
                    if last_recover.elapsed() >= Duration::from_secs(4) {
                        let _ = self.enqueue_recoverable_projects();
                        last_recover = Instant::now();
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
                crate::ingest_proxy::proxy_job_queued(batch.len() as isize);
                let parallel =
                    crate::hardware_profile::recommended_proxy_parallel().max(1) as usize;
                info!(
                    "ingest proxy generate: batch={} parallel={} encoder={}",
                    batch.len(),
                    parallel,
                    crate::hardware_profile::proxy_encoder_label()
                );
                for chunk in batch.chunks(parallel) {
                    let mut handles = Vec::with_capacity(chunk.len());
                    for job in chunk {
                        let worker = self.clone();
                        let job = job.clone();
                        handles.push(tokio::task::spawn_blocking(move || {
                            crate::ingest_proxy::proxy_job_begin();
                            let result = worker.process_clip(&job);
                            crate::ingest_proxy::proxy_job_end();
                            crate::ingest_proxy::proxy_job_queued(-1);
                            result
                        }));
                    }
                    for handle in handles {
                        match handle.await {
                            Ok(Ok(())) => {}
                            Ok(Err(e)) => warn!("ingest proxy generate: {e}"),
                            Err(e) => warn!("ingest proxy generate: join err={e}"),
                        }
                    }
                }
            }
        });
    }

    fn process_clip(&self, job: &ProxyClipJob) -> Result<(), String> {
        if self.is_blocked(&job.project_id) {
            return Ok(());
        }
        let conn = open_ingest(&self.paths, &job.project_id).map_err(|e| e.to_string())?;
        let row: IngestAssetRow = conn
            .query_row(
                "SELECT source_id, clip_id, source_path, original_path, proxy_path,
                        project_proxy_path, card_thumb_path, file_extension,
                        read_from_card, card_locked, poster_source, import_status
                 FROM ingest_assets
                 WHERE source_id = ?1 AND clip_id = ?2",
                params![job.source_id, job.clip_id],
                IngestAssetRow::from_row,
            )
            .map_err(|e| e.to_string())?;
        if row.status == "imported" || row.status == "done" {
            return Ok(());
        }
        if row.status != "generating_proxy" {
            return Ok(());
        }
        queue_ingest_job(&conn, "proxy_generate", &job.source_id, &job.clip_id)
            .map_err(|e| e.to_string())?;
        mark_ingest_job_processing(&conn, "proxy_generate", &job.source_id, &job.clip_id)
            .map_err(|e| e.to_string())?;

        let project =
            project_settings_snapshot(&self.paths, &job.project_id).unwrap_or_else(|_| json!({}));
        let meta = ingest_asset_meta(&row.meta_input_without_project_proxy());
        let plan = resolve_import_plan(&meta, &project).map_err(|e| e.to_string())?;
        let proxy_dir = self.paths.project_dir(&job.project_id).join("proxy");
        std::fs::create_dir_all(&proxy_dir).map_err(|e| e.to_string())?;
        let source = if row.original_path.trim().is_empty() {
            plan.source
        } else {
            let p = PathBuf::from(row.original_path.trim());
            if p.is_file() {
                p
            } else {
                plan.source
            }
        };
        let dest = proxy_dest_for_source(&proxy_dir, &job.clip_id, &source)?;
        // Proxy generate prema tipu izvora (PAL/NTSC broadcast klase).
        let result = generate_field_proxy(&source, &dest);
        match result {
            Ok(()) => {
                let original_path = if row.original_path.trim().is_empty() {
                    source.to_string_lossy().to_string()
                } else {
                    row.original_path.clone()
                };
                complete_imported_clip(
                    &self.paths,
                    &job.project_id,
                    &job.source_id,
                    &job.clip_id,
                    &dest,
                    plan.asset_status,
                    false,
                    plan.card_locked,
                    &original_path,
                )?;
                mark_ingest_job_done(&conn, "proxy_generate", &job.source_id, &job.clip_id)
                    .map_err(|e| e.to_string())?;
                // Fallback: ako CPU filmstrip nije krenuo/gotov — dodaj s project proxyja.
                if dest.is_file()
                    && !crate::filmstrip::filmstrip_ready(
                        &self.paths,
                        &job.project_id,
                        &job.clip_id,
                    )
                {
                    self.filmstrip.enqueue(
                        &job.project_id,
                        &job.clip_id,
                        &dest,
                        DEFAULT_FILMSTRIP_FRAMES,
                    );
                }
                // No card THM/JPG → generate poster from project proxy now that it exists.
                let needs_poster = conn
                    .query_row(
                        "SELECT thumb_status FROM ingest_assets
                         WHERE source_id = ?1 AND clip_id = ?2",
                        params![job.source_id, job.clip_id],
                        |r| r.get::<_, String>(0),
                    )
                    .map(|s| matches!(s.as_str(), "no_card_thumb" | "pending" | "error"))
                    .unwrap_or(false);
                if needs_poster {
                    self.posters
                        .enqueue_proxy_generate(&job.project_id, &[job.clip_id.clone()]);
                }
                let _ =
                    crate::virtual_shots::ensure_root_virtual_shots(&self.paths, &job.project_id);
                Ok(())
            }
            Err(err) => {
                row_import_error(&conn, &job.source_id, &job.clip_id, &err)
                    .map_err(|e| e.to_string())?;
                mark_ingest_job_error(&conn, "proxy_generate", &job.source_id, &job.clip_id, &err)
                    .map_err(|e| e.to_string())?;
                Err(err)
            }
        }
    }
}
