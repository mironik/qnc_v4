use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use qnc_service_contracts::{
    MediaLocator, MediaProbe as ServiceMediaProbe, MediaProcessor, MediaRef, ScanMode,
};
use rusqlite::params;
use serde_json::{json, Value};

use crate::media::{
    find_card_poster_copy, find_card_proxy_for_media_path, group_media_files, import_display_label,
    is_breaking_news, is_media_file, is_proxy_media_path, use_house_media,
    virtual_name_for_root_clip, CardPosterKind,
};
use crate::project::db::{
    bump_project_data_revision, project_display_name, project_settings_snapshot, ProjectPaths,
};
use crate::project::ProjectDbBroker;

use super::db::{
    backfill_virtual_names_for_selected, ensure_ingest_dirs, get_meta, mark_ingest_job_done,
    mark_ingest_job_error, mark_ingest_job_processing, open_ingest, poster_exists,
    queue_ingest_job, set_meta, set_thumb_status, thumbnail_path, thumbnail_url,
};
use super::proxy_source::{classify_tv_source, recipe_for_source};
use super::scanner;

const META_ARCHIVE_ORIGINAL: &str = "archive_original";
const META_SELECTION_REVISION: &str = "selection_revision";

type DurationProbeRow = (String, String, String, String, String);
type DurationProbeResult = (String, super::thumb::MediaProbe, PathBuf);

pub fn ingest_archive_original_default(_project: &Value) -> bool {
    // Default: isključeno — korisnik ručno uključuje „Kopiraj original u projekt”.
    false
}

pub fn ingest_archive_original_enabled(
    conn: &rusqlite::Connection,
    project: &Value,
) -> rusqlite::Result<bool> {
    let raw = get_meta(conn, META_ARCHIVE_ORIGINAL, "")?;
    if raw == "1" {
        return Ok(true);
    }
    if raw == "0" {
        return Ok(false);
    }
    Ok(ingest_archive_original_default(project))
}

pub fn ingest_archive_original_available(project: &Value) -> bool {
    !use_house_media(project)
}

pub fn set_ingest_archive_original(
    paths: &ProjectPaths,
    project_id: &str,
    enabled: bool,
) -> rusqlite::Result<Value> {
    let conn = open_ingest(paths, project_id)?;
    set_meta(
        &conn,
        META_ARCHIVE_ORIGINAL,
        if enabled { "1" } else { "0" },
    )?;
    load_state(paths, project_id)
}

fn sync_selected_virtual_identity(
    paths: &ProjectPaths,
    project_id: &str,
    conn: &rusqlite::Connection,
) {
    let active_source =
        get_meta(conn, "active_source_id", "local").unwrap_or_else(|_| "local".into());
    let _ = backfill_virtual_names_for_selected(conn);
    // Select i deselect — sync rezerviranih import_root virtuala (prije uvoza).
    if let Err(err) = crate::virtual_shots::sync_root_virtual_shots_with_selection(
        paths,
        project_id,
        &active_source,
    ) {
        tracing::warn!("sync root virtual shots on selection: {err}");
    }
}

fn row_to_clip(
    project_id: &str,
    paths: &ProjectPaths,
    row: &rusqlite::Row<'_>,
    project: &Value,
    archive_original: bool,
) -> rusqlite::Result<Value> {
    let clip_id: String = row.get("clip_id")?;
    let ext: String = row
        .get::<_, Option<String>>("file_extension")
        .unwrap_or_default()
        .unwrap_or_default();
    let read_from_card = row.get::<_, i64>("read_from_card").unwrap_or(0) != 0;
    let card_locked = row.get::<_, i64>("card_locked").unwrap_or(0) != 0;
    let poster_source: String = row
        .get::<_, Option<String>>("poster_source")
        .unwrap_or_default()
        .unwrap_or_default();
    let thumb_status: String = row
        .get::<_, Option<String>>("thumb_status")?
        .unwrap_or_else(|| "pending".into());
    let thumb_error: String = row
        .get::<_, Option<String>>("thumb_error")?
        .unwrap_or_default();
    let import_status: String = row.get("import_status")?;
    let mut clip = json!({
        "clip_id": clip_id,
        "name": row.get::<_, String>("name")?,
        "media_id": row.get::<_, String>("media_id")?,
        "duration_sec": row.get::<_, f64>("duration_sec")?,
        "resolution": row.get::<_, String>("resolution")?,
        "codec": row.get::<_, String>("codec")?,
        "fps": row.get::<_, f64>("fps")?,
        "has_audio": row.get::<_, i64>("has_audio").unwrap_or(0) != 0,
        "audio_channels": row.get::<_, i64>("audio_channels").unwrap_or(0),
        "field_order": row.get::<_, String>("field_order").unwrap_or_default(),
        "interlaced": row.get::<_, i64>("interlaced").unwrap_or(0) != 0,
        "source_class": row.get::<_, String>("source_class").unwrap_or_default(),
        "proxy_recipe": row.get::<_, String>("proxy_recipe").unwrap_or_default(),
        "proxy_status": row.get::<_, String>("status")?,
        "import_status": import_status,
        "selected": row.get::<_, i64>("selected")? != 0,
        "virtual_name": row.get::<_, String>("virtual_name").unwrap_or_default(),
        "thumb_color_a": row.get::<_, String>("thumb_color_a")?,
        "thumb_color_b": row.get::<_, String>("thumb_color_b")?,
        "thumb_status": thumb_status,
        "thumb_error": thumb_error,
        "extension": ext,
    });
    let stored_thumb_path: String = row.get("thumb_path").unwrap_or_default();
    let card_thumb_path_early: String = row.get("card_thumb_path").unwrap_or_default();
    let poster = if stored_thumb_path.trim().is_empty() {
        thumbnail_path(paths, project_id, &clip_id)
    } else {
        PathBuf::from(stored_thumb_path.trim())
    };
    // Servable as soon as DB knows a file (local poster OR card THM/JPG).
    // Local copy → `ready` happens later in CardThumbWorker (after first paint).
    let has_local_poster = poster_exists(&poster);
    let has_card_poster = {
        let p = card_thumb_path_early.trim();
        !p.is_empty() && poster_exists(&PathBuf::from(p))
    };
    if (thumb_status == "ready" && has_local_poster) || has_card_poster {
        if let Some(obj) = clip.as_object_mut() {
            obj.insert(
                "thumb_url".into(),
                json!(thumbnail_url(project_id, &clip_id)),
            );
        }
    }
    if let Some(obj) = clip.as_object_mut() {
        let source_path: String = row.get("source_path").unwrap_or_default();
        let original_path: String = row.get("original_path").unwrap_or_default();
        let proxy_path: String = row.get("proxy_path").unwrap_or_default();
        let project_proxy_path: String = row.get("project_proxy_path").unwrap_or_default();
        let thumb_path: String = row.get("thumb_path").unwrap_or_default();
        let card_thumb_path = card_thumb_path_early;
        if !source_path.trim().is_empty() {
            obj.insert("source_path".into(), json!(source_path));
        }
        if !original_path.trim().is_empty() {
            obj.insert("original_path".into(), json!(original_path));
        }
        if !proxy_path.trim().is_empty() {
            obj.insert("proxy_path".into(), json!(proxy_path));
        }
        if !project_proxy_path.trim().is_empty() {
            obj.insert("project_proxy_path".into(), json!(project_proxy_path));
        }
        if !thumb_path.trim().is_empty() {
            obj.insert("thumb_path".into(), json!(thumb_path));
        }
        if !card_thumb_path.trim().is_empty() {
            obj.insert("card_thumb_path".into(), json!(card_thumb_path));
        }
        if !poster_source.is_empty() {
            obj.insert("poster_source".into(), json!(poster_source));
        }
        if read_from_card {
            obj.insert("read_from_card".into(), json!(true));
        }
        if card_locked {
            obj.insert("card_locked".into(), json!(true));
        }
        let import_label = match import_status.as_str() {
            "original_ready" => "Original u projektu — možeš vratiti karticu".into(),
            "generating_proxy" => "Generiram proxy…".into(),
            "processing" => "Uvoz u tijeku…".into(),
            "queued" => "Čeka uvoz…".into(),
            _ => import_display_label(
                &json!({
                    "source_path": source_path,
                    "original_path": original_path,
                    "proxy_path": proxy_path,
                    "card_thumb_path": card_thumb_path,
                    "extension": ext,
                    "read_from_card": read_from_card,
                    "card_locked": card_locked,
                    "poster_source": poster_source,
                }),
                project,
                archive_original,
            ),
        };
        obj.insert("import_label".into(), json!(import_label));
        let original_in_project = {
            let orig = PathBuf::from(original_path.trim());
            let original_dir = paths.project_dir(project_id).join("original");
            !original_path.trim().is_empty()
                && (orig.starts_with(&original_dir)
                    || orig
                        .canonicalize()
                        .ok()
                        .zip(original_dir.canonicalize().ok())
                        .is_some_and(|(a, b)| a.starts_with(b)))
        };
        let status_original = match import_status.as_str() {
            "error" => "error",
            "original_ready" | "generating_proxy" => "ready",
            "queued" | "processing" if archive_original => "pending",
            "imported" | "done" if original_in_project => "ready",
            _ => "idle",
        };
        let status_proxy = match import_status.as_str() {
            "error" => "error",
            "imported" | "done" => "ready",
            "queued" | "processing" | "original_ready" | "generating_proxy" => "pending",
            _ => "idle",
        };
        obj.insert("status_original".into(), json!(status_original));
        obj.insert("status_proxy".into(), json!(status_proxy));
        obj.insert("original_in_project".into(), json!(original_in_project));
        if let Some(p) = crate::media::import_source_path(
            &json!({
                "source_path": source_path,
                "original_path": original_path,
                "proxy_path": proxy_path,
            }),
            project,
        ) {
            obj.insert("import_path".into(), json!(p.to_string_lossy()));
        }
    }
    Ok(clip)
}

pub fn row_import_error(
    conn: &rusqlite::Connection,
    source_id: &str,
    clip_id: &str,
    error: &str,
) -> rusqlite::Result<()> {
    let msg = if error.len() > 240 {
        format!("{}…", error.chars().take(240).collect::<String>())
    } else {
        error.to_string()
    };
    conn.execute(
        "UPDATE ingest_assets SET import_status = 'error', status = 'error', thumb_error = ?3
         WHERE source_id = ?1 AND clip_id = ?2",
        params![source_id, clip_id, msg],
    )?;
    Ok(())
}

fn scan_root_for_active_source(
    paths: &ProjectPaths,
    project_id: &str,
    conn: &rusqlite::Connection,
) -> PathBuf {
    let browse = get_meta(conn, "browse_path", "").unwrap_or_default();
    if !browse.trim().is_empty() {
        PathBuf::from(browse.trim())
    } else {
        paths.project_dir(project_id).join("incoming")
    }
}

pub fn discover(
    paths: &ProjectPaths,
    project_id: &str,
    source_id: &str,
) -> rusqlite::Result<Value> {
    let conn = open_ingest(paths, project_id)?;
    let sid = if source_id.trim().is_empty() {
        get_meta(&conn, "active_source_id", "local")?
    } else {
        source_id.trim().to_string()
    };
    queue_ingest_job(&conn, "discover", &sid, "_")?;
    mark_ingest_job_processing(&conn, "discover", &sid, "_")?;
    let browse_root = scan_root_for_active_source(paths, project_id, &conn);
    let inventory = scanner::scan_inventory(&browse_root);
    set_meta(
        &conn,
        "source_scan_root",
        inventory.browse_root.to_string_lossy().as_ref(),
    )?;
    let project = project_settings_snapshot(paths, project_id).unwrap_or_else(|_| json!({}));
    let breaking = is_breaking_news(&project);
    set_meta(
        &conn,
        "card_root",
        inventory.card_root.to_string_lossy().as_ref(),
    )?;
    if breaking {
        set_meta(&conn, "card_locked", "1")?;
    } else {
        set_meta(&conn, "card_locked", "0")?;
    }
    let count = register_media_paths(
        paths,
        project_id,
        &sid,
        &inventory.media_files,
        &inventory.thumb_files,
    )
    .inspect_err(|e| {
        let _ = mark_ingest_job_error(&conn, "discover", &sid, "_", &e.to_string());
    })?;
    purge_non_video_clips(&conn, &sid).inspect_err(|e| {
        let _ = mark_ingest_job_error(&conn, "discover", &sid, "_", &e.to_string());
    })?;
    // Discover = samo registracija u DB za grid. Thumb copy + duration idle u pozadini.
    // Ništa nije selected; virtual source tek na select/deselect.
    conn.execute(
        "UPDATE ingest_assets SET selected = 0 WHERE source_id = ?1",
        params![sid],
    )?;
    set_meta(&conn, "durations_probe", "processing")?;
    mark_ingest_job_done(&conn, "discover", &sid, "_")?;

    Ok(json!({
        "status": "ok",
        "discovered": count,
        "scan_root": inventory.browse_root.to_string_lossy(),
        "card_root": inventory.card_root.to_string_lossy(),
        "thumbs_pending": true,
        "durations_pending": true,
    }))
}

/// Pozadinski ffprobe trajanje: broker serializira SQLite snapshot/finalni upis,
/// a media probe radi izvan DB locka.
pub async fn probe_missing_durations_with_broker(
    paths: &ProjectPaths,
    project_db: &ProjectDbBroker,
    media_processor: Arc<dyn MediaProcessor>,
    project_id: &str,
) -> Result<usize, String> {
    let pid = project_id.trim();
    if pid.is_empty() {
        return Err("project_id required".into());
    }
    let source_id = project_db.serialize_project_write(pid, || {
        let conn = open_ingest(paths, pid).map_err(|e| e.to_string())?;
        let sid = get_meta(&conn, "active_source_id", "local").map_err(|e| e.to_string())?;
        set_meta(&conn, "durations_probe", "processing").map_err(|e| e.to_string())?;
        Ok(sid)
    })?;
    let rows = project_db.serialize_project_write(pid, || {
        let conn = open_ingest(paths, pid).map_err(|e| e.to_string())?;
        load_missing_media_probe_rows(&conn, &source_id).map_err(|e| e.to_string())
    })?;
    let results = run_media_probe_batch(media_processor, pid, rows).await;
    project_db.serialize_project_write(pid, || {
        let conn = open_ingest(paths, pid).map_err(|e| e.to_string())?;
        let filled =
            write_media_probe_results(&conn, &source_id, results).map_err(|e| e.to_string())?;
        set_meta(&conn, "durations_probe", "done").map_err(|e| e.to_string())?;
        bump_project_data_revision(&conn, "ingest").map_err(|e| e.to_string())?;
        Ok(filled)
    })
}

/// UI pending samo dok worker radi — inače poll nikad ne završi (failed probe = forever).
fn durations_still_pending(
    conn: &rusqlite::Connection,
    _source_id: &str,
) -> rusqlite::Result<bool> {
    Ok(get_meta(conn, "durations_probe", "")? == "processing")
}

fn count_missing_durations(conn: &rusqlite::Connection, source_id: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM ingest_assets
         WHERE source_id = ?1 AND (duration_sec IS NULL OR duration_sec <= 0)",
        params![source_id],
        |r| r.get(0),
    )
}

pub fn needs_duration_probe_conn(conn: &rusqlite::Connection) -> rusqlite::Result<bool> {
    let source_id = get_meta(&conn, "active_source_id", "local")?;
    let flag = get_meta(&conn, "durations_probe", "")?;
    if flag == "processing" || flag == "done" {
        return Ok(false);
    }
    Ok(count_missing_durations(&conn, &source_id)? > 0)
}

fn load_missing_media_probe_rows(
    conn: &rusqlite::Connection,
    source_id: &str,
) -> rusqlite::Result<Vec<DurationProbeRow>> {
    let mut stmt = conn.prepare(
        "SELECT clip_id, source_path, original_path, proxy_path, project_proxy_path
         FROM ingest_assets
         WHERE source_id = ?1 AND (duration_sec IS NULL OR duration_sec <= 0)
         ORDER BY clip_id",
    )?;
    let mapped = stmt.query_map(params![source_id], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    })?;
    mapped.collect::<Result<_, _>>()
}

async fn run_media_probe_batch(
    media_processor: Arc<dyn MediaProcessor>,
    project_id: &str,
    rows: Vec<DurationProbeRow>,
) -> Vec<DurationProbeResult> {
    use super::thumb::MediaProbe;

    // Prefer proxy (field default import) so fps/duration match playable media, not MXF.
    let work: Vec<(String, PathBuf)> = rows
        .into_iter()
        .filter_map(
            |(clip_id, source_path, original_path, proxy_path, project_proxy_path)| {
                [project_proxy_path, proxy_path, original_path, source_path]
                    .into_iter()
                    .map(|s| s.trim().to_string())
                    .find(|s| !s.is_empty())
                    .map(PathBuf::from)
                    .filter(|p| p.is_file())
                    .map(|media| (clip_id, media))
            },
        )
        .collect();

    // Parallel media probe (2-4) through the configured adapter. DB writes stay serial below.
    let parallel = std::thread::available_parallelism()
        .map(|n| n.get().clamp(2, 4))
        .unwrap_or(2);
    if work.is_empty() {
        return Vec::new();
    }
    let semaphore = Arc::new(tokio::sync::Semaphore::new(parallel));
    let mut handles = Vec::with_capacity(work.len());
    for (clip_id, media) in work {
        let processor = media_processor.clone();
        let semaphore = semaphore.clone();
        let project_id = project_id.to_string();
        handles.push(tokio::spawn(async move {
            let _permit = semaphore.acquire_owned().await.ok()?;
            let input = media_ref(&clip_id, &media);
            match processor.probe(&input).await {
                Ok(probe) => match ingest_probe_from_service(probe) {
                    Some(probe) if probe.duration_sec > 0.0 => Some((clip_id, probe, media)),
                    _ => {
                        tracing::warn!(
                            "ingest duration probe failed: project={} clip={} path={}",
                            project_id,
                            clip_id,
                            media.display()
                        );
                        None
                    }
                },
                Err(error) => {
                    tracing::warn!(
                        "ingest duration probe failed: project={} clip={} path={} err={}: {}",
                        project_id,
                        clip_id,
                        media.display(),
                        error.code,
                        error.message
                    );
                    None
                }
            }
        }));
    }
    let mut results: Vec<(String, MediaProbe, PathBuf)> = Vec::new();
    for handle in handles {
        match handle.await {
            Ok(Some(result)) => results.push(result),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    "ingest duration probe task failed: project={} err={}",
                    project_id,
                    error
                );
            }
        }
    }
    results
}

/// Popuni duration/fps/resolution/codec u ingest_assets — istina samo u SQLite.
fn write_media_probe_results(
    conn: &rusqlite::Connection,
    source_id: &str,
    results: Vec<DurationProbeResult>,
) -> rusqlite::Result<usize> {
    let mut filled = 0usize;
    for (clip_id, probe, _media) in results {
        let source_class = classify_tv_source(&probe);
        let proxy_recipe = recipe_for_source(source_class);
        conn.execute(
            "UPDATE ingest_assets SET
                duration_sec = ?3,
                fps = CASE WHEN ?4 > 0 THEN ?4 ELSE fps END,
                resolution = CASE WHEN TRIM(?5) = '' THEN resolution ELSE ?5 END,
                codec = CASE WHEN TRIM(?6) = '' THEN codec ELSE ?6 END,
                has_audio = ?7,
                audio_channels = ?8,
                field_order = ?9,
                interlaced = ?10,
                source_class = ?11,
                proxy_recipe = ?12
             WHERE source_id = ?1 AND clip_id = ?2",
            params![
                source_id,
                clip_id,
                probe.duration_sec,
                probe.fps,
                probe.resolution,
                probe.codec,
                if probe.has_audio { 1 } else { 0 },
                probe.audio_channels,
                probe.field_order,
                if probe.interlaced { 1 } else { 0 },
                source_class.label(),
                proxy_recipe.id(),
            ],
        )?;
        filled += 1;
    }
    Ok(filled)
}

fn media_ref(clip_id: &str, media: &std::path::Path) -> MediaRef {
    MediaRef {
        clip_id: clip_id.to_string(),
        locator: MediaLocator::LocalPath {
            path: media.to_path_buf(),
        },
    }
}

pub(crate) fn ingest_probe_from_service(
    probe: ServiceMediaProbe,
) -> Option<super::thumb::MediaProbe> {
    let fps = fps_from_service_probe(&probe)?;
    let duration_sec = duration_from_service_probe(&probe, fps)?;
    let resolution = if probe.width > 0 && probe.height > 0 {
        format!("{}x{}", probe.width, probe.height)
    } else {
        String::new()
    };
    let field_order = field_order_from_service_probe(&probe);
    let interlaced = matches!(
        probe.scan_mode,
        ScanMode::InterlacedTopFieldFirst | ScanMode::InterlacedBottomFieldFirst
    );

    Some(super::thumb::MediaProbe {
        duration_sec,
        fps,
        resolution,
        codec: probe.codec,
        has_audio: probe.has_audio,
        audio_channels: probe.audio_channels.min(4) as u8,
        field_order,
        interlaced,
    })
}

fn fps_from_service_probe(probe: &ServiceMediaProbe) -> Option<f64> {
    let fps = probe.timebase.fps_num as f64 / probe.timebase.fps_den as f64;
    fps.is_finite().then_some(fps).filter(|value| *value > 0.0)
}

fn duration_from_service_probe(probe: &ServiceMediaProbe, fps: f64) -> Option<f64> {
    probe
        .duration_sec
        .filter(|value| value.is_finite() && *value > 0.0)
        .or_else(|| {
            let frames = probe.duration_frames.or(probe.frame_count)?;
            (frames > 0).then_some(frames as f64 / fps)
        })
}

fn field_order_from_service_probe(probe: &ServiceMediaProbe) -> String {
    let raw = probe.field_order.trim();
    if !raw.is_empty() {
        return raw.to_ascii_lowercase();
    }
    match probe.scan_mode {
        ScanMode::Progressive => "progressive",
        ScanMode::InterlacedTopFieldFirst => "tt",
        ScanMode::InterlacedBottomFieldFirst => "bb",
        ScanMode::Unknown => "unknown",
    }
    .into()
}

/// True if any path is a recognised media file on disk (video or audio).
fn metadata_has_media(source_path: &str, original_path: &str, proxy_path: &str) -> bool {
    for s in [source_path, original_path, proxy_path] {
        if s.trim().is_empty() {
            continue;
        }
        let p = PathBuf::from(s.trim());
        if p.is_file() && (is_media_file(&p) || is_proxy_media_path(&p)) {
            return true;
        }
    }
    false
}

fn purge_non_video_clips(conn: &rusqlite::Connection, source_id: &str) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT clip_id, source_path, original_path, proxy_path
         FROM ingest_assets WHERE source_id = ?1",
    )?;
    let rows: Vec<(String, String, String, String)> = stmt
        .query_map(params![source_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<_, _>>()?;
    for (clip_id, source_path, original_path, proxy_path) in rows {
        if !metadata_has_media(&source_path, &original_path, &proxy_path) {
            conn.execute(
                "DELETE FROM ingest_assets WHERE source_id = ?1 AND clip_id = ?2",
                params![source_id, clip_id],
            )?;
        }
    }
    Ok(())
}

fn reconcile_source_assets(
    conn: &rusqlite::Connection,
    source_id: &str,
    valid_clip_ids: &[String],
) -> rusqlite::Result<()> {
    if valid_clip_ids.is_empty() {
        return Ok(());
    }
    let valid: HashSet<String> = valid_clip_ids.iter().cloned().collect();
    let mut stmt = conn.prepare("SELECT clip_id FROM ingest_assets WHERE source_id = ?1")?;
    let rows: Vec<String> = stmt
        .query_map(params![source_id], |r| r.get(0))?
        .collect::<Result<_, _>>()?;

    for clip_id in rows {
        if valid.contains(&clip_id) {
            continue;
        }
        conn.execute(
            "DELETE FROM ingest_assets WHERE source_id = ?1 AND clip_id = ?2",
            params![source_id, clip_id],
        )?;
    }
    Ok(())
}

fn codec_label(ext: &str) -> String {
    match ext.to_ascii_lowercase().as_str() {
        "mxf" => "MXF".into(),
        "mov" => "QuickTime".into(),
        "mp4" | "m4v" => "MP4".into(),
        "mts" | "m2ts" => "AVCHD".into(),
        _ => ext.to_ascii_uppercase(),
    }
}

fn thumb_colors(name: &str) -> (String, String) {
    let h = name
        .bytes()
        .fold(0u32, |acc, b| acc.wrapping_add(u32::from(b)));
    let a = format!(
        "#{:02x}{:02x}{:02x}",
        40 + (h % 40) as u8,
        40 + ((h >> 3) % 40) as u8,
        42 + ((h >> 6) % 40) as u8
    );
    let b = format!(
        "#{:02x}{:02x}{:02x}",
        24 + (h % 24) as u8,
        24 + ((h >> 4) % 24) as u8,
        26 + ((h >> 8) % 24) as u8
    );
    (a, b)
}

pub fn list_sources(paths: &ProjectPaths, project_id: &str) -> rusqlite::Result<Vec<Value>> {
    let settings = project_settings_snapshot(paths, project_id).unwrap_or_else(|_| json!({}));
    let inner = settings
        .get("settings")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let source_ids = inner
        .get("source_template_ids")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for id_val in source_ids {
        let Some(id) = id_val.as_str() else { continue };
        out.push(json!({
            "source_id": id,
            "name": id,
            "path": "incoming",
        }));
    }
    if out.is_empty() {
        out.push(json!({
            "source_id": "local",
            "name": "Lokalni incoming",
            "path": "incoming",
        }));
    }
    Ok(out)
}

pub fn load_state(paths: &ProjectPaths, project_id: &str) -> rusqlite::Result<Value> {
    let pid = if project_id.trim().is_empty() {
        "default".to_string()
    } else {
        project_id.trim().to_string()
    };
    ensure_ingest_dirs(paths, &pid).ok();
    let conn = open_ingest(paths, &pid)?;
    reconcile_thumbnail_rows(paths, &pid, &conn)?;
    let active_source = get_meta(&conn, "active_source_id", "local")?;
    purge_non_video_clips(&conn, &active_source)?;
    let browse_path = get_meta(&conn, "browse_path", "")?;
    let card_locked = get_meta(&conn, "card_locked", "")? == "1";
    let card_root = get_meta(&conn, "card_root", "")?;
    let project = project_settings_snapshot(paths, &pid).unwrap_or_else(|_| json!({}));
    let archive_original = ingest_archive_original_enabled(&conn, &project)?;
    // Virtual root shots se rezerviraju na selection write (toggle / select-all),
    // ne na svaki GET — ingest otvaranje mora ostati brzo (samo poster grid).
    let mut stmt =
        conn.prepare("SELECT * FROM ingest_assets WHERE source_id = ?1 ORDER BY clip_id")?;
    let clips: Vec<Value> = stmt
        .query_map(params![active_source], |row| {
            row_to_clip(&pid, paths, row, &project, archive_original)
        })?
        .collect::<Result<_, _>>()?;
    let selected: Vec<String> = clips
        .iter()
        .filter(|c| c.get("selected").and_then(|v| v.as_bool()).unwrap_or(false))
        .filter_map(|c| {
            c.get("clip_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .collect();
    let jobs = list_ingest_jobs(&conn)?;
    let durations_pending = durations_still_pending(&conn, &active_source)?;
    Ok(json!({
        "status": "ok",
        "project_id": pid,
        "project_name": project_display_name(paths, &pid),
        "active_source_id": active_source,
        "browse_path": browse_path,
        "card_locked": card_locked,
        "card_root": card_root,
        "sources": list_sources(paths, &pid)?,
        "clips": clips,
        "jobs": jobs,
        "selected_clip_ids": selected,
        "archive_original": archive_original,
        "archive_original_available": ingest_archive_original_available(&project),
        "durations_pending": durations_pending,
    }))
}

fn list_ingest_jobs(conn: &rusqlite::Connection) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT job_id, job_type, source_id, clip_id, status, error, attempts,
                queued_at, started_at, finished_at, updated_at
         FROM ingest_jobs
         ORDER BY updated_at DESC, queued_at DESC
         LIMIT 100",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(json!({
            "job_id": row.get::<_, String>(0)?,
            "job_type": row.get::<_, String>(1)?,
            "source_id": row.get::<_, String>(2)?,
            "clip_id": row.get::<_, String>(3)?,
            "status": row.get::<_, String>(4)?,
            "error": row.get::<_, String>(5)?,
            "attempts": row.get::<_, i64>(6)?,
            "queued_at": row.get::<_, Option<String>>(7)?,
            "started_at": row.get::<_, Option<String>>(8)?,
            "finished_at": row.get::<_, Option<String>>(9)?,
            "updated_at": row.get::<_, Option<String>>(10)?,
        }))
    })?;
    rows.collect()
}

pub fn set_browse_path(
    paths: &ProjectPaths,
    project_id: &str,
    path: &str,
) -> rusqlite::Result<Value> {
    let conn = open_ingest(paths, project_id)?;
    set_meta(&conn, "browse_path", path.trim())?;
    load_state(paths, project_id)
}

pub fn set_active_source(
    paths: &ProjectPaths,
    project_id: &str,
    source_id: &str,
) -> rusqlite::Result<Value> {
    let conn = open_ingest(paths, project_id)?;
    set_meta(&conn, "active_source_id", source_id.trim())?;
    let browse = get_meta(&conn, "browse_path", "")?;
    if !browse.trim().is_empty() {
        discover(paths, project_id, source_id)?;
    }
    load_state(paths, project_id)
}

pub fn save_selection(
    paths: &ProjectPaths,
    project_id: &str,
    selected_clip_ids: &[String],
    selection_revision: Option<u64>,
) -> rusqlite::Result<Value> {
    let mut conn = open_ingest(paths, project_id)?;
    let active_source = get_meta(&conn, "active_source_id", "local")?;
    if let Some(incoming_revision) = selection_revision {
        let current_revision = get_meta(&conn, META_SELECTION_REVISION, "0")?
            .trim()
            .parse::<u64>()
            .unwrap_or(0);
        if incoming_revision < current_revision {
            return load_state(paths, project_id);
        }
    }

    {
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE ingest_assets SET selected = 0 WHERE source_id = ?1",
            params![active_source],
        )?;
        for clip_id in selected_clip_ids {
            let clip_id = clip_id.trim();
            if clip_id.is_empty() {
                continue;
            }
            tx.execute(
                "UPDATE ingest_assets SET selected = 1 WHERE source_id = ?1 AND clip_id = ?2",
                params![active_source, clip_id],
            )?;
        }
        if let Some(incoming_revision) = selection_revision {
            tx.execute(
                "INSERT INTO ingest_meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![META_SELECTION_REVISION, incoming_revision.to_string()],
            )?;
        }
        tx.commit()?;
    }
    sync_selected_virtual_identity(paths, project_id, &conn);
    load_state(paths, project_id)
}

pub fn toggle_clip_selection(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
) -> rusqlite::Result<Value> {
    let cid = clip_id.trim();
    if cid.is_empty() {
        return load_state(paths, project_id);
    }
    let conn = open_ingest(paths, project_id)?;
    let active_source = get_meta(&conn, "active_source_id", "local")?;
    let current: i64 = conn
        .query_row(
            "SELECT selected FROM ingest_assets WHERE source_id = ?1 AND clip_id = ?2",
            params![active_source, cid],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let new_sel = if current != 0 { 0i64 } else { 1i64 };
    conn.execute(
        "UPDATE ingest_assets SET selected = ?3 WHERE source_id = ?1 AND clip_id = ?2",
        params![active_source, cid, new_sel],
    )?;
    sync_selected_virtual_identity(paths, project_id, &conn);
    load_state(paths, project_id)
}

pub fn select_all_clips(paths: &ProjectPaths, project_id: &str) -> rusqlite::Result<Value> {
    let conn = open_ingest(paths, project_id)?;
    let active_source = get_meta(&conn, "active_source_id", "local")?;
    let total: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ingest_assets WHERE source_id = ?1",
            params![active_source],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let selected: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ingest_assets WHERE source_id = ?1 AND selected != 0",
            params![active_source],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let select_all = total > 0 && selected < total;
    if select_all {
        conn.execute(
            "UPDATE ingest_assets SET selected = 1 WHERE source_id = ?1",
            params![active_source],
        )?;
    } else {
        conn.execute(
            "UPDATE ingest_assets SET selected = 0 WHERE source_id = ?1",
            params![active_source],
        )?;
    }
    sync_selected_virtual_identity(paths, project_id, &conn);
    load_state(paths, project_id)
}

pub fn reconcile_thumbnail_rows(
    paths: &ProjectPaths,
    project_id: &str,
    conn: &rusqlite::Connection,
) -> rusqlite::Result<()> {
    let mut stmt = conn
        .prepare("SELECT source_id, clip_id, thumb_status FROM ingest_assets ORDER BY clip_id")?;
    let rows: Vec<(String, String, String)> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<_, _>>()?;

    for (source_id, clip_id, thumb_status) in rows {
        let poster = thumbnail_path(paths, project_id, &clip_id);
        let exists = poster_exists(&poster);
        let status = thumb_status.trim();

        if status == "processing" {
            set_thumb_status(conn, &source_id, &clip_id, "pending", "")?;
            continue;
        }

        if exists && status != "ready" {
            set_thumb_status(conn, &source_id, &clip_id, "ready", "")?;
        } else if !exists && status == "ready" {
            set_thumb_status(conn, &source_id, &clip_id, "pending", "")?;
        }
    }
    Ok(())
}

fn upsert_media_group(
    paths: &ProjectPaths,
    project_id: &str,
    source_id: &str,
    group: &crate::media::MediaGroup,
    breaking: bool,
) -> rusqlite::Result<usize> {
    if group.original.is_none() && group.proxy.is_none() {
        return Ok(0);
    }
    let conn = open_ingest(paths, project_id)?;
    let sid = if source_id.trim().is_empty() {
        get_meta(&conn, "active_source_id", "local")?
    } else {
        source_id.trim().to_string()
    };
    let clip_id = group.clip_id.clone();
    let ext = group
        .original
        .as_ref()
        .or(group.proxy.as_ref())
        .and_then(|p| p.extension().and_then(|e| e.to_str()))
        .unwrap_or("")
        .to_ascii_lowercase();
    let name = group.display_name.clone();
    let (color_a, color_b) = thumb_colors(&name);
    let poster = thumbnail_path(paths, project_id, &clip_id);
    let project_dir = paths.project_dir(project_id);
    let on_card = group.is_on_card(&project_dir);
    // Ne kopiraj THM/JPG ovdje — prvo upiši put s kartice (DB), UI prikaže, copy kasnije.
    let mut meta = group.build_metadata(breaking, breaking, on_card);
    let card_root_raw = get_meta(&conn, "card_root", "").unwrap_or_default();
    let card_root = {
        let t = card_root_raw.trim();
        if t.is_empty() {
            None
        } else {
            Some(PathBuf::from(t))
        }
    };
    if path_text_from_meta(&meta, "card_thumb_path").is_empty() {
        if let Some((img, kind)) = find_card_poster_copy(&meta, card_root.as_deref()) {
            if let Some(obj) = meta.as_object_mut() {
                obj.insert(
                    "card_thumb_path".into(),
                    json!(img.to_string_lossy().to_string()),
                );
                obj.insert(
                    "poster_source".into(),
                    json!(match kind {
                        CardPosterKind::Thm => "card_thm",
                        CardPosterKind::Jpg => "card_jpg",
                    }),
                );
            }
        }
    }
    // Ensure card proxy is in DB so Uvezi can CopyToProject instead of ffmpeg.
    if path_text_from_meta(&meta, "proxy_path").is_empty() {
        let origin = path_text_from_meta(&meta, "original_path");
        let origin = if origin.is_empty() {
            path_text_from_meta(&meta, "source_path")
        } else {
            origin
        };
        if !origin.is_empty() {
            if let Some(proxy) = find_card_proxy_for_media_path(PathBuf::from(&origin).as_path()) {
                if let Some(obj) = meta.as_object_mut() {
                    obj.insert(
                        "proxy_path".into(),
                        json!(proxy.to_string_lossy().to_string()),
                    );
                }
            }
        }
    }
    let thumb_status = if poster_exists(&poster) {
        "ready"
    } else {
        "pending"
    };
    let thumb_path = if thumb_status == "ready" {
        poster.to_string_lossy().to_string()
    } else {
        String::new()
    };
    let source_path = path_text_from_meta(&meta, "source_path");
    let original_path = path_text_from_meta(&meta, "original_path");
    let proxy_path = path_text_from_meta(&meta, "proxy_path");
    let card_thumb_path = path_text_from_meta(&meta, "card_thumb_path");
    let file_extension = path_text_from_meta(&meta, "extension");
    let read_from_card = meta
        .get("read_from_card")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let card_locked = meta
        .get("card_locked")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let poster_source = path_text_from_meta(&meta, "poster_source");
    let virtual_name = virtual_name_for_root_clip(&clip_id, &file_extension);
    conn.execute(
        "INSERT INTO ingest_assets
            (source_id, clip_id, name, media_id, duration_sec, resolution, codec, fps,
             status, import_status, selected, thumb_color_a, thumb_color_b,
             thumb_status, thumb_error, source_path, original_path, proxy_path,
             project_proxy_path, thumb_path, card_thumb_path, file_extension, poster_source,
             read_from_card, card_locked, metadata_json, virtual_name)
         VALUES (?1, ?2, ?3, ?4, 0, '', ?5, 0, 'on_source', 'detected', 0, ?6, ?7, ?8, '',
             ?9, ?10, ?11, '', ?12, ?13, ?14, ?15, ?16, ?17, '{}', ?18)
         ON CONFLICT(source_id, clip_id) DO UPDATE SET
            name = excluded.name,
            codec = excluded.codec,
            status = CASE
                WHEN ingest_assets.import_status IN ('imported', 'done') THEN ingest_assets.status
                ELSE excluded.status
            END,
            thumb_color_a = excluded.thumb_color_a,
            thumb_color_b = excluded.thumb_color_b,
            thumb_status = CASE
                WHEN ingest_assets.thumb_status = 'ready' THEN ingest_assets.thumb_status
                ELSE excluded.thumb_status
            END,
            thumb_error = CASE WHEN excluded.thumb_status = 'pending' THEN '' ELSE ingest_assets.thumb_error END,
            source_path = excluded.source_path,
            original_path = excluded.original_path,
            proxy_path = excluded.proxy_path,
            thumb_path = CASE WHEN excluded.thumb_path = '' THEN ingest_assets.thumb_path ELSE excluded.thumb_path END,
            card_thumb_path = CASE WHEN excluded.card_thumb_path = '' THEN ingest_assets.card_thumb_path ELSE excluded.card_thumb_path END,
            file_extension = excluded.file_extension,
            poster_source = excluded.poster_source,
            read_from_card = excluded.read_from_card,
            card_locked = excluded.card_locked,
            metadata_json = '{}',
            selected = ingest_assets.selected,
            import_status = ingest_assets.import_status,
            virtual_name = CASE
                WHEN TRIM(COALESCE(ingest_assets.virtual_name, '')) != '' THEN ingest_assets.virtual_name
                WHEN ingest_assets.selected != 0 THEN excluded.virtual_name
                ELSE ingest_assets.virtual_name
            END",
        params![
            sid,
            clip_id,
            name,
            clip_id,
            codec_label(&ext),
            color_a,
            color_b,
            thumb_status,
            source_path,
            original_path,
            proxy_path,
            thumb_path,
            card_thumb_path,
            file_extension,
            poster_source,
            if read_from_card { 1 } else { 0 },
            if card_locked { 1 } else { 0 },
            virtual_name,
        ],
    )?;
    Ok(1)
}

fn path_text_from_meta(meta: &Value, key: &str) -> String {
    meta.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("")
        .to_string()
}

pub fn register_media_paths(
    paths: &ProjectPaths,
    project_id: &str,
    source_id: &str,
    file_paths: &[PathBuf],
    thumb_paths: &[PathBuf],
) -> rusqlite::Result<usize> {
    let project = project_settings_snapshot(paths, project_id).unwrap_or_else(|_| json!({}));
    let breaking = is_breaking_news(&project);
    let groups = group_media_files(file_paths, thumb_paths);
    let mut count = 0usize;
    let mut clip_ids = Vec::new();
    for group in groups {
        count += upsert_media_group(paths, project_id, source_id, &group, breaking)?;
        clip_ids.push(group.clip_id.clone());
    }
    let conn = open_ingest(paths, project_id)?;
    let sid = if source_id.trim().is_empty() {
        get_meta(&conn, "active_source_id", "local")?
    } else {
        source_id.trim().to_string()
    };
    reconcile_source_assets(&conn, &sid, &clip_ids)?;
    purge_non_video_clips(&conn, &sid)?;
    Ok(count)
}

pub fn queue_import(
    paths: &ProjectPaths,
    project_id: &str,
    clip_ids: &[String],
) -> rusqlite::Result<Value> {
    let conn = open_ingest(paths, project_id)?;
    let active_source = get_meta(&conn, "active_source_id", "local")?;
    // DB-first: empty body → svi selected redovi (checkbox istina u SQLite).
    let ids: HashSet<String> = if clip_ids.iter().any(|s| !s.trim().is_empty()) {
        clip_ids
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        let mut stmt = conn.prepare(
            "SELECT clip_id FROM ingest_assets
             WHERE source_id = ?1 AND selected != 0
             ORDER BY clip_id",
        )?;
        let mapped = stmt.query_map(params![active_source], |r| r.get(0))?;
        mapped.collect::<Result<HashSet<_>, _>>()?
    };
    if ids.is_empty() {
        return Ok(json!({ "status": "ok", "queued": 0, "skipped_imported": 0 }));
    }
    let mut queued = 0usize;
    let mut skipped_imported = 0usize;
    for clip_id in &ids {
        // Već uvezeno u SQLite — ne radi ponovo (istina = import_status).
        let status: String = conn
            .query_row(
                "SELECT COALESCE(import_status, '') FROM ingest_assets
                 WHERE source_id = ?1 AND clip_id = ?2",
                params![active_source, clip_id],
                |r| r.get(0),
            )
            .unwrap_or_default();
        let st = status.trim().to_ascii_lowercase();
        if st == "imported" || st == "done" {
            skipped_imported += 1;
            continue;
        }
        let n = conn.execute(
            "UPDATE ingest_assets SET import_status = 'queued'
             WHERE source_id = ?1 AND clip_id = ?2
               AND import_status NOT IN ('imported', 'done')",
            params![active_source, clip_id],
        )?;
        if n == 0 {
            skipped_imported += 1;
            continue;
        }
        queue_ingest_job(&conn, "import", &active_source, clip_id)?;
        queued += 1;
    }
    Ok(json!({
        "status": "ok",
        "queued": queued,
        "skipped_imported": skipped_imported,
    }))
}

#[cfg(test)]
mod archive_original_tests {
    use super::*;
    use crate::ingest::db::{get_meta, open_ingest};
    use crate::project::db::ProjectPaths;
    use crate::project::ProjectDbBroker;
    use async_trait::async_trait;
    use qnc_service_contracts::{
        ArtifactRef, AudioProbe, AudioProbeRequest, AudioWrapRequest, ExtractRangeRequest,
        FilmstripFrameArtifact, FilmstripRequest, FrameExtractRequest, FrameTimebase,
        MediaProbe as ContractMediaProbe, PosterExtractRequest, ProxyBuildRequest, ServiceError,
        ServiceResult, WaveformPeaks, WaveformRequest,
    };
    use serde_json::json;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn test_paths(base: &Path) -> ProjectPaths {
        ProjectPaths {
            data_dir: base.join("data"),
            projects_root: base.join("projects"),
            seed_path: base.join("seed.json"),
        }
    }

    fn field_project() -> Value {
        json!({
            "settings": {
                "storage": { "ingest_profile": "field", "proxy_policy": "generate_if_missing" }
            }
        })
    }

    #[derive(Default)]
    struct FakeDurationProcessor {
        probe_calls: AtomicUsize,
    }

    #[async_trait]
    impl MediaProcessor for FakeDurationProcessor {
        async fn probe(&self, _input: &MediaRef) -> ServiceResult<ContractMediaProbe> {
            self.probe_calls.fetch_add(1, Ordering::AcqRel);
            Ok(ContractMediaProbe {
                width: 1920,
                height: 1080,
                duration_sec: Some(12.5),
                timebase: FrameTimebase::new(50, 1).unwrap(),
                scan_mode: ScanMode::Progressive,
                codec: "h264".into(),
                field_order: "progressive".into(),
                frame_count: Some(625),
                duration_frames: Some(625),
                has_video: true,
                has_audio: true,
                audio_channels: 2,
            })
        }

        async fn probe_audio(&self, _request: AudioProbeRequest) -> ServiceResult<AudioProbe> {
            Err(unused_service_error())
        }

        async fn extract_frame(&self, _request: FrameExtractRequest) -> ServiceResult<ArtifactRef> {
            Err(unused_service_error())
        }

        async fn extract_poster(
            &self,
            _request: PosterExtractRequest,
        ) -> ServiceResult<ArtifactRef> {
            Err(unused_service_error())
        }

        async fn build_filmstrip(
            &self,
            _request: FilmstripRequest,
        ) -> ServiceResult<Vec<FilmstripFrameArtifact>> {
            Err(unused_service_error())
        }

        async fn build_proxy(&self, _request: ProxyBuildRequest) -> ServiceResult<ArtifactRef> {
            Err(unused_service_error())
        }

        async fn build_audio_wrap(&self, _request: AudioWrapRequest) -> ServiceResult<ArtifactRef> {
            Err(unused_service_error())
        }

        async fn build_waveform(&self, _request: WaveformRequest) -> ServiceResult<WaveformPeaks> {
            Err(unused_service_error())
        }

        async fn extract_range(&self, _request: ExtractRangeRequest) -> ServiceResult<ArtifactRef> {
            Err(unused_service_error())
        }
    }

    fn unused_service_error() -> ServiceError {
        ServiceError::new("unused", "unused in this test")
    }

    #[test]
    fn ingest_archive_original_defaults_off() {
        let base = std::env::temp_dir().join(format!("qnc_archive_default_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "proj_archive_default";
        let conn = open_ingest(&paths, project_id).expect("ingest db");
        assert!(!ingest_archive_original_enabled(&conn, &field_project()).expect("enabled"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn ingest_archive_original_persists_in_meta() {
        let base = std::env::temp_dir().join(format!("qnc_archive_meta_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "proj_archive_meta";
        set_ingest_archive_original(&paths, project_id, false).expect("set off");
        let conn = open_ingest(&paths, project_id).expect("ingest db");
        assert!(!ingest_archive_original_enabled(&conn, &field_project()).expect("enabled"));
        set_ingest_archive_original(&paths, project_id, true).expect("set on");
        let conn = open_ingest(&paths, project_id).expect("ingest db");
        assert!(ingest_archive_original_enabled(&conn, &field_project()).expect("enabled"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn probe_missing_durations_uses_media_processor_and_updates_sqlite() {
        let base = std::env::temp_dir().join(format!(
            "qnc_duration_probe_adapter_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "proj_duration_probe";
        let media = base.join("clip_a.mp4");
        std::fs::write(&media, b"fake-video").unwrap();
        let conn = open_ingest(&paths, project_id).expect("ingest db");
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, duration_sec, source_path)
             VALUES ('local', 'clip_a', 'clip_a', 'clip_a', 0, ?1)",
            rusqlite::params![media.to_string_lossy().to_string()],
        )
        .unwrap();
        drop(conn);

        let processor = Arc::new(FakeDurationProcessor::default());
        let broker = ProjectDbBroker::new(paths.clone());
        let filled =
            probe_missing_durations_with_broker(&paths, &broker, processor.clone(), project_id)
                .await
                .unwrap();

        assert_eq!(filled, 1);
        assert_eq!(processor.probe_calls.load(Ordering::Acquire), 1);
        let conn = open_ingest(&paths, project_id).expect("ingest db");
        let row: (
            f64,
            f64,
            String,
            String,
            i64,
            i64,
            String,
            i64,
            String,
            String,
        ) = conn
            .query_row(
                "SELECT duration_sec, fps, resolution, codec, has_audio, audio_channels,
                        field_order, interlaced, source_class, proxy_recipe
                 FROM ingest_assets
                 WHERE source_id = 'local' AND clip_id = 'clip_a'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                    ))
                },
            )
            .unwrap();
        assert!((row.0 - 12.5).abs() < 0.001);
        assert!((row.1 - 50.0).abs() < 0.001);
        assert_eq!(row.2, "1920x1080");
        assert_eq!(row.3, "h264");
        assert_eq!(row.4, 1);
        assert_eq!(row.5, 2);
        assert_eq!(row.6, "progressive");
        assert_eq!(row.7, 0);
        assert_eq!(row.8, "pal_50p");
        assert_eq!(row.9, "h264_native");
        assert_eq!(get_meta(&conn, "durations_probe", "").unwrap(), "done");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn save_selection_revision_rejects_stale_async_write() {
        let base =
            std::env::temp_dir().join(format!("qnc_selection_revision_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "proj_selection_revision";
        let conn = open_ingest(&paths, project_id).expect("ingest db");
        for clip_id in ["clip_a", "clip_b"] {
            let proxy = base.join(format!("{clip_id}.MP4"));
            std::fs::write(&proxy, b"proxy").unwrap();
            conn.execute(
                "INSERT INTO ingest_assets
                    (source_id, clip_id, name, import_status, selected, source_path, proxy_path)
                 VALUES ('local', ?1, ?1, 'detected', 0, ?2, ?2)",
                rusqlite::params![clip_id, proxy.to_string_lossy().to_string()],
            )
            .unwrap();
        }
        drop(conn);

        save_selection(
            &paths,
            project_id,
            &["clip_a".to_string(), "clip_b".to_string()],
            Some(2),
        )
        .expect("newer selection");
        save_selection(&paths, project_id, &["clip_a".to_string()], Some(1))
            .expect("stale selection ignored");

        let conn = open_ingest(&paths, project_id).expect("ingest db");
        let selected: Vec<String> = conn
            .prepare(
                "SELECT clip_id FROM ingest_assets
                 WHERE source_id = 'local' AND selected != 0
                 ORDER BY clip_id",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(selected, vec!["clip_a".to_string(), "clip_b".to_string()]);
        assert_eq!(
            get_meta(&conn, META_SELECTION_REVISION, "0").unwrap(),
            "2".to_string()
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
