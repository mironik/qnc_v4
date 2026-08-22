use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::params;
use serde_json::json;
use tracing::{info, warn};

use crate::background_work::BackgroundWorkGate;
use crate::filmstrip::{FilmstripWorker, DEFAULT_FILMSTRIP_FRAMES};
use crate::ingest::asset_row::IngestAssetRow;
use crate::ingest::audio_wrap::{
    audio_copy_dest, audio_project_dir, complete_imported_audio_clip,
    probe_audio_import_media_blocking,
};
use crate::ingest::db::{
    ingest_asset_meta, mark_ingest_job_error, mark_ingest_job_processing, open_ingest,
    queue_ingest_job, reset_processing_ingest_jobs_for_type,
};
use crate::ingest::import_finish::{complete_imported_clip, probe_import_media_blocking};
use crate::ingest::store::{ingest_archive_original_enabled, row_import_error};
use crate::ingest_audio_wrap::AudioWrapWorker;
use crate::ingest_posters::PosterWorker;
use crate::ingest_proxy::ProxyGenerateWorker;
use crate::media::{
    card_original_on_card, import_source_path, is_audio_media_file, is_breaking_news,
    proxy_policy_copy, resolve_filmstrip_media, resolve_import_plan, use_house_media,
    ImportMediaMode,
};
use crate::project::db::{
    bump_project_data_revision, ensure_project_dirs, project_settings_snapshot, ProjectPaths,
};
use crate::project::{list_project_ids, ProjectDbBroker};
use qnc_service_contracts::MediaProcessor;

/// Samostalan uvoz — copy/link/archive original. Generate proxy delegira na `ingest_proxy` worker.
#[derive(Clone)]
pub struct ImportWorker {
    paths: ProjectPaths,
    project_db: ProjectDbBroker,
    proxy: Arc<ProxyGenerateWorker>,
    filmstrip: Arc<FilmstripWorker>,
    posters: Arc<PosterWorker>,
    audio_wrap: Arc<AudioWrapWorker>,
    background: BackgroundWorkGate,
    media_processor: Arc<dyn MediaProcessor>,
    pending: Arc<Mutex<HashSet<String>>>,
    blocked: Arc<Mutex<HashSet<String>>>,
}

struct ImportBatch {
    project: serde_json::Value,
    breaking: bool,
    copy_proxy: bool,
    house_ingest: bool,
    archive_original: bool,
    rows: Vec<IngestAssetRow>,
}

impl ImportWorker {
    pub fn new(
        paths: ProjectPaths,
        project_db: ProjectDbBroker,
        proxy: Arc<ProxyGenerateWorker>,
        filmstrip: Arc<FilmstripWorker>,
        posters: Arc<PosterWorker>,
        audio_wrap: Arc<AudioWrapWorker>,
        background: BackgroundWorkGate,
        media_processor: Arc<dyn MediaProcessor>,
    ) -> Self {
        Self {
            paths,
            project_db,
            proxy,
            filmstrip,
            posters,
            audio_wrap,
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
            .expect("import block")
            .insert(pid.to_string());
        self.pending.lock().expect("import queue").remove(pid);
    }

    fn is_blocked(&self, project_id: &str) -> bool {
        self.blocked
            .lock()
            .expect("import block")
            .contains(project_id)
    }

    pub fn enqueue(&self, project_id: &str) {
        let pid = project_id.trim();
        if pid.is_empty() || self.is_blocked(pid) {
            return;
        }
        self.pending
            .lock()
            .expect("import queue")
            .insert(pid.to_string());
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
            let count = self.project_db.serialize_project_write(&project_id, || {
                let conn = open_ingest(&self.paths, &project_id).map_err(|e| e.to_string())?;
                reset_processing_ingest_jobs_for_type(&conn, "import")
                    .map_err(|e| e.to_string())?;
                let count: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM ingest_assets
                         WHERE import_status IN ('queued', 'processing', 'original_ready')",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                Ok(count)
            })?;
            if count > 0 {
                self.enqueue(&project_id);
                queued += 1;
            }
        }
        Ok(queued)
    }

    pub fn spawn(self: Arc<Self>) {
        tokio::spawn(async move {
            // Memory queue is only a wake hint — SQLite import_status is the truth.
            let mut last_recover = Instant::now();
            loop {
                if self.background.playback_active() {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
                let batch: Vec<String> = {
                    let mut set = self.pending.lock().expect("import queue");
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
                    let result =
                        tokio::task::spawn_blocking(move || worker.process_project(&project_id))
                            .await;
                    match result {
                        Ok(Ok(count)) if count > 0 => {
                            info!("ingest import: project={} processed={}", pid_log, count);
                        }
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => warn!("ingest import: project={} err={}", pid_log, e),
                        Err(e) => warn!("ingest import: project={} task err={}", pid_log, e),
                    }
                }
            }
        });
    }

    fn process_project(&self, project_id: &str) -> Result<usize, String> {
        if self.is_blocked(project_id) {
            return Ok(0);
        }
        let batch = self.load_import_batch(project_id)?;
        let had_rows = !batch.rows.is_empty();

        let proxy_dir = self.paths.project_dir(project_id).join("proxy");
        let original_dir = self.paths.project_dir(project_id).join("original");
        if !batch.house_ingest {
            fs::create_dir_all(&proxy_dir).map_err(|e| e.to_string())?;
            fs::create_dir_all(&original_dir).map_err(|e| e.to_string())?;
        }

        let project_base = self.paths.project_dir(project_id);
        let mut done = 0usize;

        for row in batch.rows {
            if self.is_blocked(project_id) {
                break;
            }
            // DB-first: već uvezeno — nikad ponovo.
            if row.status == "imported" || row.status == "done" {
                continue;
            }
            if !self.claim_import_row(project_id, &row)? {
                continue;
            }

            let meta = ingest_asset_meta(&row.meta_input_without_project_proxy());
            // Već kopiran original (status original_ready) — ne kopiraj ponovo.
            let archived_original = if row.status == "original_ready"
                && !row.original_path.trim().is_empty()
                && PathBuf::from(row.original_path.trim()).is_file()
            {
                Some(PathBuf::from(row.original_path.trim()))
            } else if batch.archive_original {
                card_original_on_card(&meta, &project_base)
                    .map(|src: PathBuf| {
                        crate::ingest::project_media::copy_into_project_dir(
                            &original_dir,
                            &row.clip_id,
                            &src,
                        )
                    })
                    .transpose()?
            } else {
                None
            };
            if let Some(ref path) = archived_original {
                if row.status != "original_ready" {
                    info!(
                        "ingest import: clip={} archived original to {}",
                        row.clip_id,
                        path.display()
                    );
                    let (duration_sec, fps, resolution, codec) = probe_import_media_blocking(
                        self.media_processor.clone(),
                        &row.clip_id,
                        path,
                    )
                    .map(|p| (p.duration_sec, p.fps, p.resolution, p.codec))
                    .unwrap_or((0.0, 0.0, String::new(), String::new()));
                    self.mark_original_ready(
                        project_id,
                        &row,
                        path,
                        duration_sec,
                        fps,
                        &resolution,
                        &codec,
                    )?;
                }
            }

            let result = if batch.breaking {
                import_breaking_clip(
                    self,
                    project_id,
                    &row,
                    &meta,
                    &batch.project,
                    batch.copy_proxy,
                    &proxy_dir,
                    &archived_original,
                )
            } else {
                import_field_clip(
                    self,
                    project_id,
                    &row,
                    &meta,
                    &batch.project,
                    &proxy_dir,
                    &archived_original,
                )
            };

            match result {
                Ok(()) => done += 1,
                Err(err) => {
                    self.mark_import_error(project_id, &row, &err)?;
                }
            }
        }

        if had_rows {
            self.bump_ingest_revision(project_id)?;
        }
        if done > 0 {
            self.ensure_root_virtual_shots(project_id);
        }
        Ok(done)
    }

    fn load_import_batch(&self, project_id: &str) -> Result<ImportBatch, String> {
        ensure_project_dirs(&self.paths, project_id).map_err(|e| e.to_string())?;
        let project =
            project_settings_snapshot(&self.paths, project_id).unwrap_or_else(|_| json!({}));
        let breaking = is_breaking_news(&project);
        let copy_proxy = proxy_policy_copy(&project);
        let house_ingest = use_house_media(&project);
        let (rows, archive_original) =
            self.project_db.serialize_project_write(project_id, || {
                let conn = open_ingest(&self.paths, project_id).map_err(|e| e.to_string())?;
                let archive_original =
                    ingest_archive_original_enabled(&conn, &project).map_err(|e| e.to_string())?;
                let mut stmt = conn
                    .prepare(
                        "SELECT source_id, clip_id, source_path, original_path, proxy_path,
                            project_proxy_path, card_thumb_path, file_extension,
                            read_from_card, card_locked, poster_source, import_status
                     FROM ingest_assets
                     WHERE import_status IN ('queued', 'processing', 'original_ready')
                     ORDER BY clip_id",
                    )
                    .map_err(|e| e.to_string())?;
                let rows: Vec<IngestAssetRow> = stmt
                    .query_map([], IngestAssetRow::from_row)
                    .map_err(|e| e.to_string())?
                    .collect::<Result<_, _>>()
                    .map_err(|e| e.to_string())?;
                Ok((rows, archive_original))
            })?;
        Ok(ImportBatch {
            project,
            breaking,
            copy_proxy,
            house_ingest,
            archive_original,
            rows,
        })
    }

    fn claim_import_row(&self, project_id: &str, row: &IngestAssetRow) -> Result<bool, String> {
        self.project_db.serialize_project_write(project_id, || {
            let conn = open_ingest(&self.paths, project_id).map_err(|e| e.to_string())?;
            let changed = conn
                .execute(
                    "UPDATE ingest_assets
                     SET import_status = 'processing'
                     WHERE source_id = ?1
                       AND clip_id = ?2
                       AND import_status IN ('queued', 'processing', 'original_ready')",
                    params![row.source_id, row.clip_id],
                )
                .map_err(|e| e.to_string())?;
            if changed == 0 {
                return Ok(false);
            }
            queue_ingest_job(&conn, "import", &row.source_id, &row.clip_id)
                .map_err(|e| e.to_string())?;
            mark_ingest_job_processing(&conn, "import", &row.source_id, &row.clip_id)
                .map_err(|e| e.to_string())?;
            Ok(true)
        })
    }

    fn mark_original_ready(
        &self,
        project_id: &str,
        row: &IngestAssetRow,
        path: &Path,
        duration_sec: f64,
        fps: f64,
        resolution: &str,
        codec: &str,
    ) -> Result<(), String> {
        self.project_db.serialize_project_write(project_id, || {
            let conn = open_ingest(&self.paths, project_id).map_err(|e| e.to_string())?;
            conn.execute(
                "UPDATE ingest_assets SET
                    import_status = 'original_ready',
                    original_path = ?3,
                    read_from_card = 0,
                    duration_sec = CASE WHEN ?4 > 0 THEN ?4 ELSE duration_sec END,
                    fps = CASE WHEN ?5 > 0 THEN ?5 ELSE fps END,
                    resolution = CASE WHEN ?6 = '' THEN resolution ELSE ?6 END,
                    codec = CASE WHEN ?7 = '' THEN codec ELSE ?7 END
                 WHERE source_id = ?1 AND clip_id = ?2",
                params![
                    row.source_id,
                    row.clip_id,
                    path.to_string_lossy().as_ref(),
                    duration_sec,
                    fps,
                    resolution,
                    codec,
                ],
            )
            .map_err(|e| e.to_string())?;
            bump_project_data_revision(&conn, "ingest").map_err(|e| e.to_string())?;
            Ok(())
        })
    }

    fn mark_generating_proxy(
        &self,
        project_id: &str,
        row: &IngestAssetRow,
        archived_original: &Option<PathBuf>,
    ) -> Result<(), String> {
        self.project_db.serialize_project_write(project_id, || {
            let conn = open_ingest(&self.paths, project_id).map_err(|e| e.to_string())?;
            if let Some(path) = archived_original {
                conn.execute(
                    "UPDATE ingest_assets SET original_path = ?3 WHERE source_id = ?1 AND clip_id = ?2",
                    params![
                        row.source_id,
                        row.clip_id,
                        path.to_string_lossy().as_ref(),
                    ],
                )
                .map_err(|e| e.to_string())?;
            }
            conn.execute(
                "UPDATE ingest_assets SET import_status = 'generating_proxy'
                 WHERE source_id = ?1 AND clip_id = ?2",
                params![row.source_id, row.clip_id],
            )
            .map_err(|e| e.to_string())?;
            bump_project_data_revision(&conn, "ingest").map_err(|e| e.to_string())?;
            Ok(())
        })
    }

    fn mark_import_error(
        &self,
        project_id: &str,
        row: &IngestAssetRow,
        err: &str,
    ) -> Result<(), String> {
        self.project_db.serialize_project_write(project_id, || {
            let conn = open_ingest(&self.paths, project_id).map_err(|e| e.to_string())?;
            row_import_error(&conn, &row.source_id, &row.clip_id, err)
                .map_err(|e| e.to_string())?;
            mark_ingest_job_error(&conn, "import", &row.source_id, &row.clip_id, err)
                .map_err(|e| e.to_string())?;
            Ok(())
        })
    }

    fn bump_ingest_revision(&self, project_id: &str) -> Result<(), String> {
        self.project_db.serialize_project_write(project_id, || {
            let conn = open_ingest(&self.paths, project_id).map_err(|e| e.to_string())?;
            bump_project_data_revision(&conn, "ingest").map_err(|e| e.to_string())?;
            Ok(())
        })
    }

    fn ensure_root_virtual_shots(&self, project_id: &str) {
        let _ = self.project_db.serialize_project_write(project_id, || {
            crate::virtual_shots::ensure_root_virtual_shots(&self.paths, project_id)
        });
    }

    fn clip_needs_poster(&self, project_id: &str, row: &IngestAssetRow) -> bool {
        self.project_db
            .serialize_project_write(project_id, || {
                let conn = open_ingest(&self.paths, project_id).map_err(|e| e.to_string())?;
                let needs = conn
                    .query_row(
                        "SELECT thumb_status FROM ingest_assets
                         WHERE source_id = ?1 AND clip_id = ?2",
                        params![row.source_id, row.clip_id],
                        |r| r.get::<_, String>(0),
                    )
                    .map(|s| matches!(s.as_str(), "no_card_thumb" | "pending" | "error"))
                    .unwrap_or(false);
                Ok(needs)
            })
            .unwrap_or(false)
    }
}

fn import_breaking_clip(
    worker: &ImportWorker,
    project_id: &str,
    row: &IngestAssetRow,
    meta: &serde_json::Value,
    project: &serde_json::Value,
    copy_proxy: bool,
    proxy_dir: &Path,
    archived_original: &Option<PathBuf>,
) -> Result<(), String> {
    let (dest_or_link, asset_status, read_from_card) =
        import_breaking_card(meta, project, copy_proxy, proxy_dir, &row.clip_id)?;
    let original_path = archived_original
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| row.original_path.clone());
    let probe =
        probe_import_media_blocking(worker.media_processor.clone(), &row.clip_id, &dest_or_link);
    worker.project_db.serialize_project_write(project_id, || {
        complete_imported_clip(
            &worker.paths,
            project_id,
            &row.source_id,
            &row.clip_id,
            &dest_or_link,
            &asset_status,
            read_from_card,
            read_from_card,
            &original_path,
            probe.as_ref(),
        )
    })?;
    let filmstrip_media = resolve_filmstrip_media(
        &worker.paths,
        project_id,
        &row.clip_id,
        Some(dest_or_link.as_path()),
    );
    if let Some(media) = filmstrip_media {
        worker
            .filmstrip
            .enqueue(project_id, &row.clip_id, &media, DEFAULT_FILMSTRIP_FRAMES);
    }
    Ok(())
}

fn import_field_clip(
    worker: &ImportWorker,
    project_id: &str,
    row: &IngestAssetRow,
    meta: &serde_json::Value,
    project: &serde_json::Value,
    proxy_dir: &Path,
    archived_original: &Option<PathBuf>,
) -> Result<(), String> {
    let plan = resolve_import_plan(meta, project)?;
    info!(
        "ingest import: clip={} mode={:?} source={}",
        row.clip_id,
        plan.mode,
        plan.source.display()
    );
    match plan.mode {
        ImportMediaMode::GenerateProxy => {
            worker.mark_generating_proxy(project_id, row, archived_original)?;
            // Generate proxy: sva snaga na GPU worker — filmstrip tek nakon proxyja.
            worker
                .proxy
                .enqueue_clip(project_id, &row.source_id, &row.clip_id);
            Ok(())
        }
        ImportMediaMode::CopyToProject => {
            if is_audio_media_file(&plan.source) {
                import_audio_copy_only(worker, project_id, row, &plan.source, archived_original)
            } else {
                let media_path = crate::ingest::project_media::copy_into_project_dir(
                    proxy_dir,
                    &row.clip_id,
                    &plan.source,
                )?;
                finish_copy_import(
                    worker,
                    project_id,
                    row,
                    &media_path,
                    plan.asset_status,
                    archived_original.is_none() && plan.read_from_card,
                    plan.card_locked,
                    archived_original,
                    false,
                )
            }
        }
        ImportMediaMode::LinkInPlace => {
            if is_audio_media_file(&plan.source) {
                // Still copy into project/audio — wrap worker needs a local file.
                import_audio_copy_only(worker, project_id, row, &plan.source, archived_original)
            } else {
                finish_copy_import(
                    worker,
                    project_id,
                    row,
                    &plan.source,
                    plan.asset_status,
                    archived_original.is_none() && plan.read_from_card,
                    plan.card_locked,
                    archived_original,
                    false,
                )
            }
        }
    }
}

/// Copy audio into `project/audio/` only; AV wrap runs later from DB fps.
fn import_audio_copy_only(
    worker: &ImportWorker,
    project_id: &str,
    row: &IngestAssetRow,
    source: &Path,
    archived_original: &Option<PathBuf>,
) -> Result<(), String> {
    let audio_dir = audio_project_dir(&worker.paths, project_id);
    fs::create_dir_all(&audio_dir).map_err(|e| e.to_string())?;
    let dest = audio_copy_dest(&audio_dir, &row.clip_id, source);
    if source.canonicalize().map_err(|e| e.to_string())?
        != dest.canonicalize().unwrap_or(dest.clone())
    {
        fs::copy(source, &dest).map_err(|e| format!("audio copy: {e}"))?;
    }
    let original_path = archived_original
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if row.original_path.trim().is_empty() {
                source.to_string_lossy().to_string()
            } else {
                row.original_path.clone()
            }
        });
    info!(
        "ingest import: audio copy clip={} dest={}",
        row.clip_id,
        dest.display()
    );
    let probe =
        probe_audio_import_media_blocking(worker.media_processor.clone(), &row.clip_id, &dest);
    worker.project_db.serialize_project_write(project_id, || {
        complete_imported_audio_clip(
            &worker.paths,
            project_id,
            &row.source_id,
            &row.clip_id,
            &dest,
            "ready",
            false,
            false,
            &original_path,
            probe.as_ref(),
        )
    })?;
    worker.ensure_root_virtual_shots(project_id);
    // Same pattern as waveform peaks after video import: enqueue deferred AV wrap
    // from fps already stored on video rows in SQLite.
    worker.audio_wrap.enqueue(project_id);
    Ok(())
}

fn finish_copy_import(
    worker: &ImportWorker,
    project_id: &str,
    row: &IngestAssetRow,
    media_path: &Path,
    asset_status: &str,
    read_from_card: bool,
    card_locked: bool,
    archived_original: &Option<PathBuf>,
    source_was_audio: bool,
) -> Result<(), String> {
    let original_path = archived_original
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| row.original_path.clone());
    let probe =
        probe_import_media_blocking(worker.media_processor.clone(), &row.clip_id, media_path);
    worker.project_db.serialize_project_write(project_id, || {
        complete_imported_clip(
            &worker.paths,
            project_id,
            &row.source_id,
            &row.clip_id,
            media_path,
            asset_status,
            read_from_card,
            card_locked,
            &original_path,
            probe.as_ref(),
        )
    })?;
    // Skip filmstrip for audio wraps (black video only) — source_was_audio.
    let filmstrip_media =
        resolve_filmstrip_media(&worker.paths, project_id, &row.clip_id, Some(media_path));
    if !source_was_audio
        && !crate::filmstrip::filmstrip_ready(&worker.paths, project_id, &row.clip_id)
        && filmstrip_media.is_some()
    {
        let media = filmstrip_media.unwrap();
        worker
            .filmstrip
            .enqueue(project_id, &row.clip_id, &media, DEFAULT_FILMSTRIP_FRAMES);
    }
    // Card copy/link done — if no THM/JPG, generate poster from project media.
    let needs_poster = worker.clip_needs_poster(project_id, row);
    if needs_poster {
        worker
            .posters
            .enqueue_proxy_generate(project_id, &[row.clip_id.clone()]);
    }
    // Video fps landed in DB — wake audio wrap for any pending VO.
    if !source_was_audio {
        worker.audio_wrap.enqueue(project_id);
    }
    Ok(())
}

fn import_breaking_card(
    meta: &serde_json::Value,
    project: &serde_json::Value,
    copy_proxy: bool,
    proxy_dir: &Path,
    clip_id: &str,
) -> Result<(PathBuf, String, bool), String> {
    let proxy_on_card = meta
        .get("proxy_path")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .filter(|p| p.is_file());
    let original_on_card = meta
        .get("original_path")
        .or_else(|| meta.get("source_path"))
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .filter(|p| p.is_file());

    if let Some(proxy) = proxy_on_card {
        if copy_proxy {
            let dest =
                crate::ingest::project_media::copy_into_project_dir(proxy_dir, clip_id, &proxy)?;
            Ok((dest, "ready".to_string(), false))
        } else {
            Ok((proxy, "on_card".to_string(), true))
        }
    } else if let Some(path) = original_on_card {
        Ok((path, "on_card".to_string(), true))
    } else {
        let _ = import_source_path(meta, project);
        Err("nema proxy ni originala na kartici".into())
    }
}
