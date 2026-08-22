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
use crate::ingest::import_finish::{complete_imported_clip, probe_import_media};
use crate::ingest::proxy_generate::proxy_dest_for_source;
use crate::ingest::store::row_import_error;
use crate::ingest::thumb::MediaProbe;
use crate::ingest_posters::PosterWorker;
use crate::media::resolve_import_plan;
use crate::project::db::{project_settings_snapshot, ProjectPaths};
use crate::project::{list_project_ids, ProjectDbBroker};
use qnc_service_contracts::{
    MediaLocator, MediaProcessor, MediaRef, ProxyBuildRequest, ServiceError,
};
use serde_json::json;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct ProxyClipJob {
    project_id: String,
    source_id: String,
    clip_id: String,
}

#[derive(Clone, Debug)]
struct PreparedProxyClip {
    source: PathBuf,
    dest: PathBuf,
    asset_status: String,
    card_locked: bool,
    original_path: String,
}

/// Samostalan worker — ffmpeg proxy generate (GPU/CPU). Ne dijeli proces s importom.
#[derive(Clone)]
pub struct ProxyGenerateWorker {
    paths: ProjectPaths,
    project_db: ProjectDbBroker,
    filmstrip: Arc<FilmstripWorker>,
    posters: Arc<PosterWorker>,
    background: BackgroundWorkGate,
    media_processor: Arc<dyn MediaProcessor>,
    pending: Arc<Mutex<HashSet<ProxyClipJob>>>,
    blocked: Arc<Mutex<HashSet<String>>>,
}

impl ProxyGenerateWorker {
    pub fn new(
        paths: ProjectPaths,
        project_db: ProjectDbBroker,
        filmstrip: Arc<FilmstripWorker>,
        posters: Arc<PosterWorker>,
        background: BackgroundWorkGate,
        media_processor: Arc<dyn MediaProcessor>,
    ) -> Self {
        Self {
            paths,
            project_db,
            filmstrip,
            posters,
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
        let project_ids = self
            .project_db
            .with_global(|global| list_project_ids(global).map_err(|e| e.to_string()))?;
        let mut queued = 0usize;
        for project_id in project_ids {
            if self.is_blocked(&project_id) {
                continue;
            }
            let rows = self.recoverable_jobs_for_project(&project_id)?;
            for (source_id, clip_id) in rows {
                self.enqueue_clip(&project_id, &source_id, &clip_id);
                queued += 1;
            }
        }
        Ok(queued)
    }

    fn recoverable_jobs_for_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<(String, String)>, String> {
        self.project_db.serialize_project_write(project_id, || {
            let conn = open_ingest(&self.paths, project_id).map_err(|e| e.to_string())?;
            let mut stmt = conn
                .prepare(
                    "SELECT source_id, clip_id FROM ingest_assets
                     WHERE import_status = 'generating_proxy'
                     ORDER BY clip_id",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| e.to_string())?
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            Ok(rows)
        })
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
                        handles.push(tokio::spawn(async move {
                            crate::ingest_proxy::proxy_job_begin();
                            let result = worker.process_clip(job).await;
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

    async fn process_clip(self: Arc<Self>, job: ProxyClipJob) -> Result<(), String> {
        let prepared = {
            let worker = self.clone();
            let job = job.clone();
            tokio::task::spawn_blocking(move || worker.prepare_clip(&job))
                .await
                .map_err(|error| format!("proxy prepare join err={error}"))??
        };
        let Some(prepared) = prepared else {
            return Ok(());
        };

        let media_result = self
            .media_processor
            .build_proxy(ProxyBuildRequest {
                input: media_ref(&job.clip_id, &prepared.source),
                output_path: prepared.dest.clone(),
            })
            .await
            .map(|_| ());

        match media_result {
            Ok(()) => {
                let probe =
                    probe_import_media(self.media_processor.clone(), &job.clip_id, &prepared.dest)
                        .await;
                let worker = self.clone();
                let job = job.clone();
                tokio::task::spawn_blocking(move || {
                    worker.finish_clip(&job, &prepared, probe.as_ref())
                })
                .await
                .map_err(|error| format!("proxy finish join err={error}"))?
            }
            Err(error) => {
                let err = service_error_message(error);
                let worker = self.clone();
                let job = job.clone();
                let mark_err = err.clone();
                let mark_result =
                    tokio::task::spawn_blocking(move || worker.mark_clip_error(&job, &mark_err))
                        .await;
                match mark_result {
                    Ok(Ok(())) => Err(err),
                    Ok(Err(mark_err)) => Err(mark_err),
                    Err(join_err) => Err(format!("proxy error mark join err={join_err}")),
                }
            }
        }
    }

    fn prepare_clip(&self, job: &ProxyClipJob) -> Result<Option<PreparedProxyClip>, String> {
        if self.is_blocked(&job.project_id) {
            return Ok(None);
        }
        let Some(row) = self.claim_proxy_job(job)? else {
            return Ok(None);
        };

        let project = self.project_settings(&job.project_id);
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
        let original_path = if row.original_path.trim().is_empty() {
            source.to_string_lossy().to_string()
        } else {
            row.original_path.clone()
        };

        Ok(Some(PreparedProxyClip {
            source,
            dest,
            asset_status: plan.asset_status.to_string(),
            card_locked: plan.card_locked,
            original_path,
        }))
    }

    fn claim_proxy_job(&self, job: &ProxyClipJob) -> Result<Option<IngestAssetRow>, String> {
        self.project_db
            .serialize_project_write(&job.project_id, || {
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
                    return Ok(None);
                }
                if row.status != "generating_proxy" {
                    return Ok(None);
                }
                queue_ingest_job(&conn, "proxy_generate", &job.source_id, &job.clip_id)
                    .map_err(|e| e.to_string())?;
                mark_ingest_job_processing(&conn, "proxy_generate", &job.source_id, &job.clip_id)
                    .map_err(|e| e.to_string())?;
                Ok(Some(row))
            })
    }

    fn project_settings(&self, project_id: &str) -> serde_json::Value {
        self.project_db
            .serialize_project_write(project_id, || {
                project_settings_snapshot(&self.paths, project_id).map_err(|e| e.to_string())
            })
            .unwrap_or_else(|_| json!({}))
    }

    fn finish_clip(
        &self,
        job: &ProxyClipJob,
        prepared: &PreparedProxyClip,
        probe: Option<&MediaProbe>,
    ) -> Result<(), String> {
        if self.is_blocked(&job.project_id) {
            return Ok(());
        }
        self.project_db
            .serialize_project_write(&job.project_id, || {
                complete_imported_clip(
                    &self.paths,
                    &job.project_id,
                    &job.source_id,
                    &job.clip_id,
                    &prepared.dest,
                    &prepared.asset_status,
                    false,
                    prepared.card_locked,
                    &prepared.original_path,
                    probe,
                )?;
                let conn = open_ingest(&self.paths, &job.project_id).map_err(|e| e.to_string())?;
                mark_ingest_job_done(&conn, "proxy_generate", &job.source_id, &job.clip_id)
                    .map_err(|e| e.to_string())?;
                Ok(())
            })?;
        // Fallback: ako CPU filmstrip nije krenuo/gotov — dodaj s project proxyja.
        if prepared.dest.is_file() && !self.filmstrip_ready(&job.project_id, &job.clip_id) {
            self.filmstrip.enqueue(
                &job.project_id,
                &job.clip_id,
                &prepared.dest,
                DEFAULT_FILMSTRIP_FRAMES,
            );
        }
        // No card THM/JPG → generate poster from project proxy now that it exists.
        if self.clip_needs_poster(&job.project_id, job) {
            self.posters
                .enqueue_proxy_generate(&job.project_id, &[job.clip_id.clone()]);
        }
        self.ensure_root_virtual_shots(&job.project_id);
        Ok(())
    }

    fn mark_clip_error(&self, job: &ProxyClipJob, err: &str) -> Result<(), String> {
        self.project_db
            .serialize_project_write(&job.project_id, || {
                let conn = open_ingest(&self.paths, &job.project_id).map_err(|e| e.to_string())?;
                row_import_error(&conn, &job.source_id, &job.clip_id, err)
                    .map_err(|e| e.to_string())?;
                mark_ingest_job_error(&conn, "proxy_generate", &job.source_id, &job.clip_id, err)
                    .map_err(|e| e.to_string())
            })
    }

    fn filmstrip_ready(&self, project_id: &str, clip_id: &str) -> bool {
        self.project_db
            .serialize_project_write(project_id, || {
                Ok(crate::filmstrip::filmstrip_ready(
                    &self.paths,
                    project_id,
                    clip_id,
                ))
            })
            .unwrap_or(false)
    }

    fn clip_needs_poster(&self, project_id: &str, job: &ProxyClipJob) -> bool {
        self.project_db
            .serialize_project_write(project_id, || {
                let conn = open_ingest(&self.paths, project_id).map_err(|e| e.to_string())?;
                let needs = conn
                    .query_row(
                        "SELECT thumb_status FROM ingest_assets
                         WHERE source_id = ?1 AND clip_id = ?2",
                        params![job.source_id, job.clip_id],
                        |r| r.get::<_, String>(0),
                    )
                    .map(|s| matches!(s.as_str(), "no_card_thumb" | "pending" | "error"))
                    .unwrap_or(false);
                Ok(needs)
            })
            .unwrap_or(false)
    }

    fn ensure_root_virtual_shots(&self, project_id: &str) {
        let _ = self.project_db.serialize_project_write(project_id, || {
            crate::virtual_shots::ensure_root_virtual_shots(&self.paths, project_id)
        });
    }
}

fn media_ref(clip_id: &str, source: &std::path::Path) -> MediaRef {
    MediaRef {
        clip_id: clip_id.to_string(),
        locator: MediaLocator::LocalPath {
            path: source.to_path_buf(),
        },
    }
}

fn service_error_message(error: ServiceError) -> String {
    format!("{}: {}", error.code, error.message)
}
