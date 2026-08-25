//! Read-only pogled na ingest tablice u projektnoj bazi.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::frame_time::normalize_fps;
use crate::ingest::db::{ingest_asset_meta, open_ingest, IngestAssetMetaInput};
use crate::ingest::proxy_source::{classify_tv_source, recipe_for_source};
use crate::ingest::thumb::probe_media;
use crate::project::db::ProjectPaths;

const VIDEO_EXT: &[&str] = &["mp4", "mov", "m4v", "mxf", "mts", "mkv", "avi", "webm"];

fn open_ingest_readonly(paths: &ProjectPaths, project_id: &str) -> Result<Connection, String> {
    open_ingest(paths, project_id).map_err(|e| e.to_string())
}

fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|s| VIDEO_EXT.contains(&s.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Klipovi s `import_status = imported` (rezultat ingest uvoza).
pub fn read_imported_clips(paths: &ProjectPaths, project_id: &str) -> Result<Vec<Value>, String> {
    let conn = match open_ingest_readonly(paths, project_id) {
        Ok(c) => c,
        Err(_) => return Ok(vec![]),
    };
    if !table_exists(&conn, "ingest_assets")? {
        return Ok(vec![]);
    }
    let mut stmt = conn
        .prepare(
            "SELECT clip_id, name, duration_sec, import_status, status,
                    project_proxy_path, proxy_path, thumb_path, source_path, original_path,
                    card_thumb_path, file_extension, read_from_card, card_locked, poster_source,
                    fps, resolution, codec, virtual_name,
                    COALESCE(has_audio, 0), COALESCE(audio_channels, 0),
                    COALESCE(field_order, ''), COALESCE(interlaced, 0),
                    COALESCE(source_class, ''), COALESCE(proxy_recipe, ''),
                    COALESCE(source_fps_num, 0), COALESCE(source_fps_den, 1)
             FROM ingest_assets
             WHERE import_status = 'imported'
             ORDER BY clip_id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            let clip_id: String = row.get(0)?;
            let project_proxy_path = row.get::<_, String>(5).unwrap_or_default();
            let ingest_proxy_path = row.get::<_, String>(6).unwrap_or_default();
            let thumb_path = row.get::<_, String>(7).unwrap_or_default();
            let source_path = row.get::<_, String>(8).unwrap_or_default();
            let original_path = row.get::<_, String>(9).unwrap_or_default();
            let card_thumb_path = row.get::<_, String>(10).unwrap_or_default();
            let file_extension = row.get::<_, String>(11).unwrap_or_default();
            let read_from_card = row.get::<_, i64>(12).unwrap_or(0) != 0;
            let card_locked = row.get::<_, i64>(13).unwrap_or(0) != 0;
            let poster_source = row.get::<_, String>(14).unwrap_or_default();
            let fps = row.get::<_, f64>(15).unwrap_or(0.0);
            let resolution = row.get::<_, String>(16).unwrap_or_default();
            let codec = row.get::<_, String>(17).unwrap_or_default();
            let virtual_name = row.get::<_, String>(18).unwrap_or_default();
            let has_audio = row.get::<_, i64>(19).unwrap_or(0) != 0;
            let audio_channels = row.get::<_, i64>(20).unwrap_or(0).clamp(0, u8::MAX as i64) as u8;
            let field_order = row.get::<_, String>(21).unwrap_or_default();
            let interlaced = row.get::<_, i64>(22).unwrap_or(0) != 0;
            let source_class = row.get::<_, String>(23).unwrap_or_default();
            let proxy_recipe = row.get::<_, String>(24).unwrap_or_default();
            let source_fps_num = row.get::<_, i64>(25).unwrap_or(0);
            let source_fps_den = row.get::<_, i64>(26).unwrap_or(1);
            let meta = ingest_asset_meta(&IngestAssetMetaInput {
                source_path: source_path.clone(),
                original_path: original_path.clone(),
                proxy_path: ingest_proxy_path.clone(),
                project_proxy_path: project_proxy_path.clone(),
                card_thumb_path: card_thumb_path.clone(),
                file_extension: file_extension.clone(),
                read_from_card,
                card_locked,
                poster_source: poster_source.clone(),
            });
            let proxy_path = Some(project_proxy_path.as_str())
                .filter(|s| !s.trim().is_empty())
                .or_else(|| Some(ingest_proxy_path.as_str()).filter(|s| !s.trim().is_empty()))
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .filter(|p| p.is_file());
            Ok(json!({
                "clip_id": clip_id,
                "name": row.get::<_, String>(1)?,
                "virtual_name": virtual_name,
                "file_extension": file_extension,
                "duration_sec": row.get::<_, f64>(2)?,
                "fps": fps,
                "source_fps_num": source_fps_num,
                "source_fps_den": source_fps_den,
                "source_timebase": {
                    "fps_num": source_fps_num,
                    "fps_den": source_fps_den,
                },
                "metadata_ready": fps.is_finite() && fps > 0.0,
                "resolution": resolution,
                "codec": codec,
                "import_status": row.get::<_, String>(3)?,
                "status": row.get::<_, String>(4)?,
                "proxy_status": row.get::<_, String>(4)?,
                "project_proxy_path": empty_to_null(&project_proxy_path),
                "ingest_proxy_path": empty_to_null(&ingest_proxy_path),
                "metadata": meta,
                "proxy_path": proxy_path.as_ref().map(|p| p.to_string_lossy().to_string()),
                "thumb_path": empty_to_null(&thumb_path),
                "source_path": empty_to_null(&source_path),
                "original_path": empty_to_null(&original_path),
                "card_thumb_path": empty_to_null(&card_thumb_path),
                "has_audio": has_audio,
                "audio_channels": audio_channels,
                "field_order": field_order,
                "interlaced": interlaced,
                "source_class": source_class,
                "proxy_recipe": proxy_recipe,
            }))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

pub fn pending_import_count(paths: &ProjectPaths, project_id: &str) -> Result<i64, String> {
    let conn = match open_ingest_readonly(paths, project_id) {
        Ok(conn) => conn,
        Err(_) => return Ok(0),
    };
    if !table_exists(&conn, "ingest_assets")? {
        return Ok(0);
    }
    conn.query_row(
        "SELECT COUNT(*) FROM ingest_assets
         WHERE import_status IN ('queued', 'processing', 'original_ready', 'generating_proxy')",
        [],
        |row| row.get(0),
    )
    .map_err(|error| error.to_string())
}

fn empty_to_null(value: &str) -> Value {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Value::Null
    } else {
        json!(trimmed)
    }
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool, String> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1 LIMIT 1",
            params![table],
            |_| Ok(()),
        )
        .is_ok())
}

pub fn proxy_path_for_clip(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
) -> Option<PathBuf> {
    let find = || {
        read_imported_clips(paths, project_id)
            .ok()?
            .iter()
            .find(|c| c.get("clip_id").and_then(|v| v.as_str()) == Some(clip_id))
            .and_then(|c| c.get("proxy_path").and_then(|v| v.as_str()))
            .map(PathBuf::from)
            .filter(|p| p.is_file() && is_video(p))
    };
    find().or_else(|| {
        repair_proxy_asset_index(paths, project_id, clip_id).ok()?;
        find()
    })
}

fn repair_proxy_asset_index(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
) -> Result<(), String> {
    let proxy_dir = paths.project_dir(project_id).join("proxy");
    let media = std::fs::read_dir(&proxy_dir)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.is_file()
                && is_video(path)
                && path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .map(|stem| stem.eq_ignore_ascii_case(clip_id))
                    .unwrap_or(false)
        })
        .ok_or_else(|| format!("proxy nije pronađen za '{clip_id}'"))?;
    let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
    let (duration_sec, fps) = conn
        .query_row(
            "SELECT COALESCE(MAX(out_seconds), 0), COALESCE(MAX(fps), 0)
             FROM virtual_shots WHERE clip_id = ?1",
            params![clip_id],
            |row| Ok((row.get::<_, f64>(0)?, row.get::<_, f64>(1)?)),
        )
        .unwrap_or((0.0, 0.0));
    let path = media.to_string_lossy().to_string();
    let name = media
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(clip_id);
    let extension = media
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    conn.execute(
        "INSERT INTO ingest_assets
            (source_id, clip_id, name, media_id, duration_sec, fps, status, import_status,
             selected, proxy_path, project_proxy_path, file_extension)
         VALUES ('project_proxy_repair', ?1, ?2, ?1, ?3, ?4, 'active', 'imported',
                 0, ?5, ?5, ?6)
         ON CONFLICT(source_id, clip_id) DO UPDATE SET
             name = excluded.name,
             duration_sec = excluded.duration_sec,
             fps = excluded.fps,
             status = 'active',
             import_status = 'imported',
             proxy_path = excluded.proxy_path,
             project_proxy_path = excluded.project_proxy_path,
             file_extension = excluded.file_extension",
        params![clip_id, name, duration_sec, fps, path, extension],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

struct IngestClipProbeRow {
    source_id: String,
    fps: f64,
    source_fps_num: i64,
    source_fps_den: i64,
    has_audio: bool,
    audio_channels: u8,
    _duration_sec: f64,
    project_proxy_path: String,
    proxy_path: String,
    field_order: String,
    interlaced: bool,
    source_class: String,
    proxy_recipe: String,
}

fn load_ingest_clip_row(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
) -> Result<IngestClipProbeRow, String> {
    let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
    conn.query_row(
        "SELECT source_id, fps, COALESCE(has_audio, 0), COALESCE(audio_channels, 0),
                duration_sec, project_proxy_path, proxy_path,
                COALESCE(field_order, ''), COALESCE(interlaced, 0),
                COALESCE(source_class, ''), COALESCE(proxy_recipe, ''),
                COALESCE(source_fps_num, 0), COALESCE(source_fps_den, 1)
         FROM ingest_assets
         WHERE clip_id = ?1 AND import_status = 'imported'
         ORDER BY CASE WHEN source_id = 'project_proxy_repair' THEN 1 ELSE 0 END
         LIMIT 1",
        params![clip_id],
        |row| {
            Ok(IngestClipProbeRow {
                source_id: row.get(0)?,
                fps: row.get(1)?,
                has_audio: row.get::<_, i64>(2)? != 0,
                audio_channels: row.get::<_, i64>(3)?.clamp(0, 4) as u8,
                _duration_sec: row.get(4)?,
                project_proxy_path: row.get(5)?,
                proxy_path: row.get(6)?,
                field_order: row.get(7)?,
                interlaced: row.get::<_, i64>(8)? != 0,
                source_class: row.get(9)?,
                proxy_recipe: row.get(10)?,
                source_fps_num: row.get(11)?,
                source_fps_den: row.get(12)?,
            })
        },
    )
    .map_err(|_| format!("Uvezeni klip '{clip_id}' nije pronađen u projektnoj bazi."))
}

/// Original/probe timebase lookup: read the stored rational pair from SQLite.
pub fn resolve_stored_clip_timebase(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
) -> Result<(i64, i64), String> {
    let clip_id = clip_id.trim();
    if clip_id.is_empty() {
        return Err("clip_id je prazan".into());
    }
    let row = load_ingest_clip_row(paths, project_id, clip_id)?;
    if row.source_fps_num > 0 && row.source_fps_den > 0 {
        return Ok((row.source_fps_num, row.source_fps_den));
    }
    Err(format!(
        "Klip '{clip_id}' nema valjan source timebase u ingest bazi; pričekaj import/probe worker."
    ))
}

/// Hot-path source FPS lookup: read the already imported/probed value from SQLite only.
pub fn resolve_stored_clip_fps(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
) -> Result<f64, String> {
    let clip_id = clip_id.trim();
    if clip_id.is_empty() {
        return Err("clip_id je prazan".into());
    }
    let row = load_ingest_clip_row(paths, project_id, clip_id)?;
    if row.fps.is_finite() && row.fps > 0.0 {
        return Ok(normalize_fps(row.fps));
    }
    Err(format!(
        "Klip '{clip_id}' nema valjan fps u ingest bazi; pričekaj import/probe worker."
    ))
}

fn clip_probe_media_path(row: &IngestClipProbeRow) -> Option<std::path::PathBuf> {
    [&row.project_proxy_path, &row.proxy_path]
        .iter()
        .map(|value| value.trim())
        .find(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_file() && is_video(path))
}

fn persist_clip_probe(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    source_id: &str,
    probe: &crate::ingest::thumb::MediaProbe,
) -> Result<(), String> {
    let source_class = classify_tv_source(probe);
    let proxy_recipe = recipe_for_source(source_class);
    let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE ingest_assets SET
            duration_sec = CASE WHEN ?3 > 0.0 THEN ?3 ELSE duration_sec END,
            fps = ?4,
            resolution = CASE WHEN ?5 = '' THEN resolution ELSE ?5 END,
            codec = CASE WHEN ?6 = '' THEN codec ELSE ?6 END,
            has_audio = ?7,
            audio_channels = ?8,
            field_order = ?9,
            interlaced = ?10,
            source_class = ?11,
            proxy_recipe = ?12,
            source_fps_num = CASE WHEN ?13 > 0 THEN ?13 ELSE source_fps_num END,
            source_fps_den = CASE WHEN ?14 > 0 THEN ?14 ELSE source_fps_den END
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
            probe.fps_num,
            probe.fps_den,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Background/repair source FPS: may ffprobe proxy/master and update SQLite.
pub fn resolve_clip_fps(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
) -> Result<f64, String> {
    let clip_id = clip_id.trim();
    if clip_id.is_empty() {
        return Err("clip_id je prazan".into());
    }
    let row = load_ingest_clip_row(paths, project_id, clip_id)?;
    if let Some(media_path) = clip_probe_media_path(&row) {
        if let Some(probe) = probe_media(&media_path) {
            if probe.fps.is_finite() && probe.fps > 0.0 {
                let probed = normalize_fps(probe.fps);
                let stored = if row.fps.is_finite() && row.fps > 0.0 {
                    normalize_fps(row.fps)
                } else {
                    0.0
                };
                let source_class = classify_tv_source(&probe);
                let proxy_recipe = recipe_for_source(source_class);
                let audio_changed =
                    row.has_audio != probe.has_audio || row.audio_channels != probe.audio_channels;
                let clip_type_changed = row.field_order != probe.field_order
                    || row.interlaced != probe.interlaced
                    || row.source_class != source_class.label()
                    || row.proxy_recipe != proxy_recipe.id();
                if stored <= 0.0
                    || (stored - probed).abs() > 0.01
                    || audio_changed
                    || clip_type_changed
                {
                    persist_clip_probe(paths, project_id, clip_id, &row.source_id, &probe)?;
                }
                return Ok(probed);
            }
        }
    }
    if row.fps.is_finite() && row.fps > 0.0 {
        return Ok(normalize_fps(row.fps));
    }
    Err(format!(
        "Klip '{clip_id}' nema valjan fps u bazi i ffprobe nije dostupan."
    ))
}

/// Background maintenance: repair fps/metadata za sve projekte.
/// Vrti se izvan request patha (server boot / maintenance), nikad u form/state readu.
pub fn backfill_all_imported_metadata(paths: &ProjectPaths) {
    let Ok(global) = crate::project::db::open_global(paths) else {
        return;
    };
    let Ok(project_ids) = crate::project::list_project_ids(&global) else {
        return;
    };
    for project_id in project_ids {
        backfill_imported_clip_metadata(paths, &project_id);
    }
}

/// Jednokratni backfill za uvezene clipove bez fps/trajanja u bazi.
pub fn backfill_imported_clip_metadata(paths: &ProjectPaths, project_id: &str) {
    let Ok(conn) = open_ingest(paths, project_id) else {
        return;
    };
    let Ok(mut stmt) = conn.prepare(
        "SELECT DISTINCT clip_id FROM ingest_assets
         WHERE import_status = 'imported'
         ORDER BY clip_id",
    ) else {
        return;
    };
    let clip_ids = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .ok()
        .map(|rows| rows.filter_map(Result::ok).collect::<Vec<_>>())
        .unwrap_or_default();
    for clip_id in clip_ids {
        let _ = resolve_clip_fps(paths, project_id, &clip_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_import_count_reads_active_import_pipeline_assets() {
        let root = std::env::temp_dir().join(format!(
            "qnc_media_pool_pending_import_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let paths = ProjectPaths {
            data_dir: root.join("data"),
            projects_root: root.join("projects"),
            seed_path: root.join("seed.json"),
        };
        let project_id = "pending_import_project";
        let conn = open_ingest(&paths, project_id).unwrap();
        for (clip_id, status) in [
            ("clip_queued", "queued"),
            ("clip_processing", "processing"),
            ("clip_original_ready", "original_ready"),
            ("clip_generating_proxy", "generating_proxy"),
            ("clip_imported", "imported"),
            ("clip_error", "error"),
        ] {
            conn.execute(
                "INSERT INTO ingest_assets (source_id, clip_id, name, import_status)
                 VALUES ('source', ?1, ?1, ?2)",
                params![clip_id, status],
            )
            .unwrap();
        }
        drop(conn);

        assert_eq!(pending_import_count(&paths, project_id).unwrap(), 4);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolve_clip_fps_reads_stored_fps_without_probe() {
        let root =
            std::env::temp_dir().join(format!("qnc_resolve_clip_fps_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let paths = ProjectPaths {
            data_dir: root.join("data"),
            projects_root: root.join("projects"),
            seed_path: root.join("seed.json"),
        };
        let project_id = "resolve_fps_project";
        let conn = open_ingest(&paths, project_id).unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, duration_sec, fps, status, import_status, selected)
             VALUES ('src', 'clip_50p', 'Clip', 10.0, 50.0, 'ready', 'imported', 1)",
            [],
        )
        .unwrap();
        drop(conn);

        assert_eq!(
            resolve_clip_fps(&paths, project_id, "clip_50p").unwrap(),
            50.0
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
