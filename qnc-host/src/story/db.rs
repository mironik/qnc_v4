use std::collections::{BTreeSet, HashMap};

use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};

use crate::filmstrip::clip_filmstrip_snapshot;

use crate::frame_time::{
    duration_color_key_from_frames, duration_frames, frame_to_seconds, is_valid_fps, normalize_fps,
    seconds_frames_label_from_frames, seconds_to_frame, seconds_to_timecode, snap_seconds_to_frame,
};
use crate::ingest::db::{open_ingest, resolve_ingest_poster_path, thumbnail_url};
use crate::ingest::store::ingest_archive_original_enabled;
use crate::media::root_shot_id as root_shot_id_for_clip;
use crate::project::db::{now_str, open_project, project_settings_snapshot, ProjectPaths};

use super::covers::{
    covers_snapshot, create_cover as create_cover_row, delete_cover as delete_cover_row,
    ensure_cover_schema, select_cover as select_cover_row, update_cover as update_cover_row,
};
use super::markers::{
    create_marker as create_marker_row, create_marker_frame as create_marker_row_frame,
    delete_marker as delete_marker_row, delete_markers_for_part, ensure_marker_schema,
    ensure_materialized_slots, finalize_story_mutation, marker_slots_snapshot, markers_snapshot,
    move_marker as move_marker_row, resolve_marker_timeline_frame, resolve_marker_timeline_sec,
    select_marker_slot as select_marker_slot_row, timeline_duration_from_parts,
    update_marker as update_marker_row, update_marker_frame as update_marker_frame_row,
};

#[derive(Default)]
pub(crate) struct StoryRow {
    selected_part_id: String,
    selected_shot_id: String,
    pub(crate) selected_slot_id: String,
    pub(crate) selected_cover_id: String,
    draft_updated_at: String,
    committed_at: String,
    _updated_at: String,
}

#[derive(Clone)]
pub(crate) struct StoryPartRow {
    pub(crate) part_id: String,
    pub(crate) kind: String,
    sort_index: i64,
    pub(crate) title: String,
    text: String,
    pub(crate) clip_id: String,
    pub(crate) virtual_shot_id: String,
    in_tc: String,
    out_tc: String,
    pub(crate) in_seconds: Option<f64>,
    pub(crate) out_seconds: Option<f64>,
    pub(crate) fps: f64,
    pub(crate) in_frame: i64,
    pub(crate) out_frame: i64,
    pub(crate) duration_frames: i64,
    duration_label: String,
    duration_color_key: String,
    created_at: String,
    updated_at: String,
}

fn new_part_id() -> String {
    format!("part_{}", uuid::Uuid::new_v4().simple())
}

pub(crate) fn ensure_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS story_state (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            selected_part_id TEXT NOT NULL DEFAULT '',
            selected_shot_id TEXT NOT NULL DEFAULT '',
            draft_updated_at TEXT,
            committed_at TEXT,
            updated_at TEXT
        );
        CREATE TABLE IF NOT EXISTS story_parts (
            part_id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            sort_index INTEGER NOT NULL,
            title TEXT NOT NULL DEFAULT '',
            text TEXT NOT NULL DEFAULT '',
            clip_id TEXT NOT NULL DEFAULT '',
            virtual_shot_id TEXT NOT NULL DEFAULT '',
            in_tc TEXT NOT NULL DEFAULT '',
            out_tc TEXT NOT NULL DEFAULT '',
            in_seconds REAL,
            out_seconds REAL,
            fps REAL NOT NULL DEFAULT 0,
            in_frame INTEGER NOT NULL DEFAULT 0,
            out_frame INTEGER NOT NULL DEFAULT 0,
            duration_frames INTEGER NOT NULL DEFAULT 0,
            duration_label TEXT NOT NULL DEFAULT '',
            duration_color_key TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_story_parts_sort ON story_parts(sort_index);",
    )?;
    ensure_marker_schema(conn)?;
    ensure_cover_schema(conn)?;
    migrate_story_part_frame_columns(conn)?;
    Ok(())
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for name in rows {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn migrate_story_part_frame_columns(conn: &Connection) -> rusqlite::Result<()> {
    for (column, sql_type) in [
        ("fps", "REAL NOT NULL DEFAULT 0"),
        ("in_frame", "INTEGER NOT NULL DEFAULT 0"),
        ("out_frame", "INTEGER NOT NULL DEFAULT 0"),
        ("duration_frames", "INTEGER NOT NULL DEFAULT 0"),
        ("duration_label", "TEXT NOT NULL DEFAULT ''"),
        ("duration_color_key", "TEXT NOT NULL DEFAULT ''"),
    ] {
        if !column_exists(conn, "story_parts", column)? {
            conn.execute(
                &format!("ALTER TABLE story_parts ADD COLUMN {column} {sql_type}"),
                [],
            )?;
        }
    }
    backfill_story_part_duration_colors(conn)?;
    Ok(())
}

fn backfill_story_part_duration_colors(conn: &Connection) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT part_id, in_seconds, out_seconds, fps, duration_frames
         FROM story_parts
         WHERE duration_color_key = ''",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<f64>>(1)?,
                r.get::<_, Option<f64>>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (part_id, in_seconds, out_seconds, fps, stored_frames) in rows {
        if !is_valid_fps(fps) {
            continue;
        }
        let frames = if stored_frames > 0 {
            stored_frames
        } else {
            duration_frames(in_seconds.unwrap_or(0.0), out_seconds.unwrap_or(0.0), fps)
        };
        let color_key = duration_color_key_from_frames(frames, fps);
        conn.execute(
            "UPDATE story_parts SET duration_color_key = ?1 WHERE part_id = ?2",
            params![color_key, part_id],
        )?;
    }
    Ok(())
}

pub(crate) fn sync_story_part_source_fps(
    paths: &ProjectPaths,
    project_id: &str,
    conn: &Connection,
) -> Result<(), String> {
    ensure_schema(conn).map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT part_id, clip_id, in_frame, out_frame, fps
             FROM story_parts
             WHERE TRIM(clip_id) != ''",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, f64>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())?;
    for (part_id, clip_id, in_frame, out_frame, stored_fps) in rows {
        let Ok(fps) = crate::media_pool::resolve_clip_fps(paths, project_id, &clip_id) else {
            continue;
        };
        if !is_valid_fps(fps) || (stored_fps - fps).abs() <= 0.01 {
            continue;
        }
        let in_frame = in_frame.max(0);
        let out_frame = out_frame.max(in_frame + 1);
        let duration_frames = (out_frame - in_frame).max(0);
        let in_sec = round3(frame_to_seconds(in_frame, fps));
        let out_sec = round3(frame_to_seconds(out_frame, fps));
        let in_tc = seconds_to_timecode(in_sec, fps);
        let out_tc = seconds_to_timecode(out_sec, fps);
        let duration_label = seconds_frames_label_from_frames(duration_frames, fps);
        let duration_color_key = duration_color_key_from_frames(duration_frames, fps).to_string();
        conn.execute(
            "UPDATE story_parts
             SET fps = ?2, in_seconds = ?3, out_seconds = ?4,
                 in_tc = ?5, out_tc = ?6,
                 out_frame = ?7, duration_frames = ?8,
                 duration_label = ?9, duration_color_key = ?10,
                 updated_at = ?11
             WHERE part_id = ?1",
            params![
                part_id,
                fps,
                in_sec,
                out_sec,
                in_tc,
                out_tc,
                out_frame,
                duration_frames,
                duration_label,
                duration_color_key,
                now_str(),
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn story_program_source_fps(parts: &[StoryPartRow]) -> Option<f64> {
    parts
        .iter()
        .map(|part| part.fps)
        .find(|fps| is_valid_fps(*fps))
}

fn require_story_program_source_fps(parts: &[StoryPartRow]) -> Result<f64, String> {
    story_program_source_fps(parts)
        .ok_or_else(|| "story program nema valjan source FPS iz probe/DB".to_string())
}

fn require_current_story_program_source_fps(conn: &Connection) -> Result<f64, String> {
    let parts = list_parts(conn).map_err(|e| e.to_string())?;
    require_story_program_source_fps(&parts)
}

fn finalize_current_story_mutation(conn: &Connection) -> rusqlite::Result<()> {
    let parts = list_parts(conn)?;
    if let Some(timeline_fps) = story_program_source_fps(&parts) {
        finalize_story_mutation(conn, timeline_fps)
    } else {
        touch_draft(conn)
    }
}

fn ensure_row(conn: &Connection) -> rusqlite::Result<()> {
    ensure_schema(conn)?;
    conn.execute(
        "INSERT INTO story_state (id) VALUES (1) ON CONFLICT(id) DO NOTHING",
        [],
    )?;
    Ok(())
}

pub(crate) fn read_row(conn: &Connection) -> rusqlite::Result<StoryRow> {
    ensure_row(conn)?;
    conn.query_row(
        "SELECT selected_part_id, selected_shot_id,
                COALESCE(selected_slot_id, ''), COALESCE(selected_cover_id, ''),
                COALESCE(draft_updated_at, ''), COALESCE(committed_at, ''),
                COALESCE(updated_at, '')
         FROM story_state WHERE id = 1",
        [],
        |r| {
            Ok(StoryRow {
                selected_part_id: r.get(0)?,
                selected_shot_id: r.get(1)?,
                selected_slot_id: r.get(2)?,
                selected_cover_id: r.get(3)?,
                draft_updated_at: r.get(4)?,
                committed_at: r.get(5)?,
                _updated_at: r.get(6)?,
            })
        },
    )
}

pub(crate) fn touch_draft(conn: &Connection) -> rusqlite::Result<()> {
    let now = now_str();
    conn.execute(
        "UPDATE story_state SET draft_updated_at = ?1, updated_at = ?1 WHERE id = 1",
        params![now],
    )?;
    Ok(())
}

fn optional_text(value: &str) -> Value {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Value::Null
    } else {
        Value::String(trimmed.to_string())
    }
}

fn optional_f64(value: Option<f64>) -> Value {
    match value {
        Some(v) => json!(v),
        None => Value::Null,
    }
}

fn validate_kind(kind: &str) -> Result<&str, String> {
    match kind.trim().to_lowercase().as_str() {
        "tonovi" => Ok("tonovi"),
        "offovi" => Ok("offovi"),
        _ => Err(format!("invalid kind: {kind}")),
    }
}

fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

pub(crate) fn list_parts(conn: &Connection) -> rusqlite::Result<Vec<StoryPartRow>> {
    ensure_schema(conn)?;
    let mut stmt = conn.prepare(
        "SELECT part_id, kind, sort_index, title, text, clip_id, virtual_shot_id,
                in_tc, out_tc, in_seconds, out_seconds,
                fps, in_frame, out_frame, duration_frames, duration_label, duration_color_key,
                created_at, updated_at
         FROM story_parts
         ORDER BY sort_index ASC, part_id ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(StoryPartRow {
            part_id: r.get(0)?,
            kind: r.get(1)?,
            sort_index: r.get(2)?,
            title: r.get(3)?,
            text: r.get(4)?,
            clip_id: r.get(5)?,
            virtual_shot_id: r.get(6)?,
            in_tc: r.get(7)?,
            out_tc: r.get(8)?,
            in_seconds: r.get(9)?,
            out_seconds: r.get(10)?,
            fps: r.get(11)?,
            in_frame: r.get(12)?,
            out_frame: r.get(13)?,
            duration_frames: r.get(14)?,
            duration_label: r.get(15)?,
            duration_color_key: r.get(16)?,
            created_at: r.get(17)?,
            updated_at: r.get(18)?,
        })
    })?;
    rows.collect()
}

fn part_json(row: &StoryPartRow) -> Value {
    let source_class = "segment";
    let root_shot_id = if !row.virtual_shot_id.trim().is_empty() {
        row.virtual_shot_id.trim().to_string()
    } else if !row.clip_id.trim().is_empty() {
        root_shot_id_for_clip(&row.clip_id)
    } else {
        String::new()
    };
    let duration_sec = match (row.in_seconds, row.out_seconds) {
        (Some(inn), Some(out)) => (out - inn).max(0.0),
        _ => 0.0,
    };
    json!({
        "id": row.part_id,
        "shot_id": row.part_id,
        "root_shot_id": root_shot_id,
        "source_class": source_class,
        "part_id": row.part_id,
        "kind": row.kind,
        "sort_index": row.sort_index,
        "title": row.title,
        "name": row.title,
        "virtual_name": row.title,
        "text": row.text,
        "clip_id": row.clip_id,
        "virtual_shot_id": row.virtual_shot_id,
        "in_tc": row.in_tc,
        "out_tc": row.out_tc,
        "in_seconds": optional_f64(row.in_seconds),
        "out_seconds": optional_f64(row.out_seconds),
        "duration_sec": duration_sec,
        "duration_seconds": duration_sec,
        "fps": row.fps,
        "in_frame": row.in_frame,
        "out_frame": row.out_frame,
        "duration_frames": row.duration_frames,
        "duration_label": row.duration_label,
        "duration_color_key": row.duration_color_key,
        "created_at": row.created_at,
        "updated_at": row.updated_at,
    })
}

#[derive(Debug, Clone)]
struct ClipMediaMeta {
    duration_sec: f64,
    fps: f64,
    has_audio: bool,
    audio_channels: u8,
    field_order: String,
    interlaced: bool,
    source_class: String,
    proxy_recipe: String,
}

fn import_status_dots(
    paths: &ProjectPaths,
    project_id: &str,
    import_status: &str,
    original_path: &str,
    archive_original: bool,
) -> (String, String, bool) {
    let original_in_project = {
        let orig = std::path::PathBuf::from(original_path.trim());
        let original_dir = paths.project_dir(project_id).join("original");
        !original_path.trim().is_empty()
            && (orig.starts_with(&original_dir)
                || orig
                    .canonicalize()
                    .ok()
                    .zip(original_dir.canonicalize().ok())
                    .is_some_and(|(a, b)| a.starts_with(b)))
    };
    let status_original = match import_status {
        "error" => "error",
        "original_ready" | "generating_proxy" => "ready",
        "queued" | "processing" if archive_original => "pending",
        "imported" | "done" if original_in_project => "ready",
        _ => "idle",
    };
    let status_proxy = match import_status {
        "error" => "error",
        "imported" | "done" => "ready",
        "queued" | "processing" | "original_ready" | "generating_proxy" => "pending",
        _ => "idle",
    };
    (
        status_proxy.to_string(),
        status_original.to_string(),
        original_in_project,
    )
}

fn root_shot_by_clip(virtual_shots: &[Value]) -> HashMap<String, Value> {
    virtual_shots
        .iter()
        .filter(|shot| shot.get("kind").and_then(Value::as_str) == Some("import_root"))
        .filter_map(|shot| {
            let clip_id = shot.get("clip_id").and_then(Value::as_str)?.trim();
            if clip_id.is_empty() {
                return None;
            }
            Some((clip_id.to_string(), shot.clone()))
        })
        .collect()
}

fn ingest_clip_media_meta_map(
    paths: &ProjectPaths,
    project_id: &str,
) -> rusqlite::Result<HashMap<String, ClipMediaMeta>> {
    let conn = open_ingest(paths, project_id)?;
    let mut stmt = conn.prepare(
        "SELECT clip_id, duration_sec, fps, COALESCE(has_audio, 0),
                COALESCE(audio_channels, 0), COALESCE(field_order, ''),
                COALESCE(interlaced, 0), COALESCE(source_class, ''),
                COALESCE(proxy_recipe, '')
         FROM ingest_assets
         WHERE import_status = 'imported'
         ORDER BY CASE WHEN TRIM(COALESCE(project_proxy_path, '')) != '' THEN 0 ELSE 1 END,
                  CASE WHEN source_id = 'project_proxy_repair' THEN 1 ELSE 0 END,
                  clip_id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, f64>(1)?,
            row.get::<_, f64>(2)?,
            row.get::<_, i64>(3)? != 0,
            row.get::<_, i64>(4)?.clamp(0, u8::MAX as i64) as u8,
            row.get::<_, String>(5)?,
            row.get::<_, i64>(6)? != 0,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
        ))
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let (
            clip_id,
            duration_sec,
            fps,
            has_audio,
            audio_channels,
            field_order,
            interlaced,
            source_class,
            proxy_recipe,
        ) = row?;
        let clip_id = clip_id.trim().to_string();
        if !clip_id.is_empty() && !map.contains_key(&clip_id) {
            map.insert(
                clip_id,
                ClipMediaMeta {
                    duration_sec,
                    fps,
                    has_audio,
                    audio_channels,
                    field_order,
                    interlaced,
                    source_class,
                    proxy_recipe,
                },
            );
        }
    }
    Ok(map)
}

fn duration_label_from_sec(duration_sec: f64, fps: f64) -> String {
    if duration_sec <= 0.0 || !is_valid_fps(fps) {
        return String::new();
    }
    let frames = seconds_to_frame(duration_sec, fps);
    seconds_frames_label_from_frames(frames, fps)
}

fn json_text(row: &Value, key: &str) -> String {
    row.get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

fn json_f64(row: &Value, key: &str) -> f64 {
    row.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

fn json_u8(row: &Value, key: &str) -> u8 {
    row.get(key)
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(u8::MAX as u64) as u8
}

fn existing_project_proxy_path(path: &str) -> Option<String> {
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    std::path::Path::new(path)
        .is_file()
        .then(|| path.to_string())
}

fn source_catalog_play_path(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    import_complete: bool,
    project_proxy_path: &str,
) -> Option<String> {
    let direct = project_proxy_path.trim();
    if !direct.is_empty() && (import_complete || std::path::Path::new(direct).is_file()) {
        return Some(direct.to_string());
    }
    crate::media_pool::proxy_path_for_clip(paths, project_id, clip_id)
        .map(|path| path.to_string_lossy().to_string())
        .or_else(|| existing_project_proxy_path(project_proxy_path))
}

fn source_catalog_ingest_rows(
    paths: &ProjectPaths,
    project_id: &str,
) -> rusqlite::Result<Vec<Value>> {
    let conn = open_ingest(paths, project_id)?;
    let mut stmt = conn.prepare(
        "SELECT clip_id, name, duration_sec, import_status, status,
                COALESCE(project_proxy_path, ''), COALESCE(proxy_path, ''),
                COALESCE(thumb_path, ''), COALESCE(source_path, ''),
                COALESCE(original_path, ''), COALESCE(card_thumb_path, ''),
                COALESCE(file_extension, ''), COALESCE(read_from_card, 0),
                COALESCE(card_locked, 0), COALESCE(poster_source, ''),
                fps, COALESCE(resolution, ''), COALESCE(codec, ''),
                COALESCE(virtual_name, ''), COALESCE(has_audio, 0),
                COALESCE(audio_channels, 0), COALESCE(field_order, ''),
                COALESCE(interlaced, 0), COALESCE(source_class, ''),
                COALESCE(proxy_recipe, ''), COALESCE(selected, 0)
         FROM ingest_assets
         WHERE selected != 0
            OR import_status IN ('queued', 'processing', 'original_ready',
                                 'generating_proxy', 'imported', 'done')
         ORDER BY clip_id,
                  CASE
                    WHEN import_status IN ('queued', 'processing', 'original_ready',
                                           'generating_proxy') THEN 0
                    WHEN selected != 0 THEN 1
                    WHEN import_status IN ('imported', 'done') THEN 2
                    ELSE 2
                  END,
                  CASE WHEN TRIM(COALESCE(project_proxy_path, '')) != '' THEN 0 ELSE 1 END,
                  source_id",
    )?;
    let rows = stmt.query_map([], |row| {
        let project_proxy_path = row.get::<_, String>(5).unwrap_or_default();
        let proxy_path = row.get::<_, String>(6).unwrap_or_default();
        let thumb_path = row.get::<_, String>(7).unwrap_or_default();
        let source_path = row.get::<_, String>(8).unwrap_or_default();
        let original_path = row.get::<_, String>(9).unwrap_or_default();
        let card_thumb_path = row.get::<_, String>(10).unwrap_or_default();
        let file_extension = row.get::<_, String>(11).unwrap_or_default();
        let read_from_card = row.get::<_, i64>(12).unwrap_or(0) != 0;
        let card_locked = row.get::<_, i64>(13).unwrap_or(0) != 0;
        let poster_source = row.get::<_, String>(14).unwrap_or_default();
        let metadata =
            crate::ingest::db::ingest_asset_meta(&crate::ingest::db::IngestAssetMetaInput {
                source_path: source_path.clone(),
                original_path: original_path.clone(),
                proxy_path: proxy_path.clone(),
                project_proxy_path: project_proxy_path.clone(),
                card_thumb_path: card_thumb_path.clone(),
                file_extension: file_extension.clone(),
                read_from_card,
                card_locked,
                poster_source: poster_source.clone(),
            });
        Ok(json!({
            "clip_id": row.get::<_, String>(0)?,
            "name": row.get::<_, String>(1)?,
            "duration_sec": row.get::<_, f64>(2)?,
            "import_status": row.get::<_, String>(3)?,
            "status": row.get::<_, String>(4)?,
            "project_proxy_path": optional_text(&project_proxy_path),
            "proxy_path": optional_text(&proxy_path),
            "ingest_proxy_path": optional_text(&proxy_path),
            "thumb_path": optional_text(&thumb_path),
            "source_path": optional_text(&source_path),
            "original_path": optional_text(&original_path),
            "card_thumb_path": optional_text(&card_thumb_path),
            "file_extension": file_extension,
            "metadata": metadata,
            "fps": row.get::<_, f64>(15)?,
            "resolution": row.get::<_, String>(16)?,
            "codec": row.get::<_, String>(17)?,
            "virtual_name": row.get::<_, String>(18)?,
            "has_audio": row.get::<_, i64>(19)? != 0,
            "audio_channels": row.get::<_, i64>(20)?.clamp(0, u8::MAX as i64) as u8,
            "field_order": row.get::<_, String>(21)?,
            "interlaced": row.get::<_, i64>(22)? != 0,
            "source_class": row.get::<_, String>(23)?,
            "proxy_recipe": row.get::<_, String>(24)?,
            "selected": row.get::<_, i64>(25)? != 0,
        }))
    })?;
    rows.collect()
}

/// All/source catalog: imported media plus selected/pending source rows for immediate thumbs.
fn build_all_clips_snapshot(
    paths: &ProjectPaths,
    project_id: &str,
    virtual_shots: &[Value],
    archive_original: bool,
) -> rusqlite::Result<Vec<Value>> {
    let roots = root_shot_by_clip(virtual_shots);
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for row in source_catalog_ingest_rows(paths, project_id)? {
        let clip_id = json_text(&row, "clip_id");
        if clip_id.is_empty() || !seen.insert(clip_id.clone()) {
            continue;
        }
        let name = json_text(&row, "name");
        let duration_sec = json_f64(&row, "duration_sec");
        let fps = json_f64(&row, "fps");
        let import_status = json_text(&row, "import_status");
        let original_path = json_text(&row, "original_path");
        let project_proxy_path = json_text(&row, "project_proxy_path");
        let proxy_path = json_text(&row, "proxy_path");
        let thumb_path = json_text(&row, "thumb_path");
        let source_path = json_text(&row, "source_path");
        let card_thumb_path = json_text(&row, "card_thumb_path");
        let has_audio = row
            .get("has_audio")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let audio_channels = json_u8(&row, "audio_channels");
        let field_order = json_text(&row, "field_order");
        let interlaced = row
            .get("interlaced")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let source_class = json_text(&row, "source_class");
        let proxy_recipe = json_text(&row, "proxy_recipe");
        let root_shot = roots.get(&clip_id);
        let has_poster = !thumb_path.trim().is_empty() || !card_thumb_path.trim().is_empty();
        let thumb_url = if has_poster {
            thumbnail_url(project_id, &clip_id)
        } else {
            String::new()
        };
        let root_shot_id = root_shot
            .and_then(|s| s.get("shot_id").and_then(Value::as_str))
            .map(str::to_string)
            .unwrap_or_else(|| root_shot_id_for_clip(&clip_id));
        let virtual_name = root_shot
            .and_then(|s| s.get("virtual_name").and_then(Value::as_str))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| json_text(&row, "virtual_name"));
        let import_complete = matches!(import_status.as_str(), "imported" | "done");
        let play_path = source_catalog_play_path(
            paths,
            project_id,
            &clip_id,
            import_complete,
            &project_proxy_path,
        )
        .unwrap_or_default();
        let materialized = !play_path.is_empty();
        let selected = row
            .get("selected")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let (mut status_proxy, status_original, original_in_project) = import_status_dots(
            paths,
            project_id,
            &import_status,
            &original_path,
            archive_original,
        );
        if materialized {
            status_proxy = "ready".to_string();
        } else if !import_complete
            && (selected
                || matches!(
                    import_status.as_str(),
                    "queued" | "processing" | "original_ready" | "generating_proxy"
                ))
        {
            status_proxy = "pending".to_string();
        }
        // Trajanje: isključivo iz ingest_assets (probe). Label iz baze ili izračun u Rustu.
        let root_label = root_shot
            .and_then(|s| s.get("duration_label").and_then(Value::as_str))
            .unwrap_or("")
            .trim();
        let duration_label = if !root_label.is_empty() && duration_sec <= 0.0 {
            root_label.to_string()
        } else {
            duration_label_from_sec(duration_sec, fps)
        };
        let duration_frames = if duration_sec > 0.0 && is_valid_fps(fps) {
            seconds_to_frame(duration_sec, fps).max(1)
        } else {
            root_shot
                .and_then(|s| s.get("duration_frames").and_then(Value::as_i64))
                .unwrap_or(0)
        };
        let duration_color_key = if duration_sec > 0.0 && is_valid_fps(fps) {
            let use_fps = fps;
            duration_color_key_from_frames(duration_frames, use_fps).to_string()
        } else {
            root_shot
                .and_then(|s| s.get("duration_color_key").and_then(Value::as_str))
                .unwrap_or("")
                .to_string()
        };
        let effective_import_status = if materialized && !import_complete {
            "imported".to_string()
        } else {
            import_status.clone()
        };
        out.push(json!({
            "clip_id": clip_id,
            "root_shot_id": root_shot_id,
            "name": name,
            "virtual_name": virtual_name,
            "duration_sec": duration_sec,
            "duration_seconds": duration_sec,
            "duration_frames": duration_frames,
            "in_frame": 0,
            "out_frame": duration_frames,
            "duration_label": duration_label,
            "duration_color_key": duration_color_key,
            "fps": fps,
            "has_audio": has_audio,
            "audio_channels": audio_channels,
            "field_order": field_order,
            "interlaced": interlaced,
            "source_class": source_class,
            "proxy_recipe": proxy_recipe,
            "import_status": effective_import_status,
            "status_proxy": status_proxy,
            "status_original": status_original,
            "original_in_project": original_in_project,
            "has_poster": has_poster,
            "thumb_url": thumb_url,
            "play_path": play_path,
            "selected": selected,
            "materialized": materialized,
            "proxy_path": optional_text(&proxy_path),
            "project_proxy_path": optional_text(if project_proxy_path.trim().is_empty() {
                &play_path
            } else {
                &project_proxy_path
            }),
            "thumb_path": optional_text(&thumb_path),
            "source_path": optional_text(&source_path),
            "original_path": optional_text(&original_path),
            "card_thumb_path": optional_text(&card_thumb_path),
        }));
    }
    Ok(out)
}

fn enrich_virtual_shots_from_ingest(
    paths: &ProjectPaths,
    project_id: &str,
    shots: &mut [Value],
) -> rusqlite::Result<()> {
    let media_meta = ingest_clip_media_meta_map(paths, project_id)?;
    for shot in shots.iter_mut() {
        let Some(obj) = shot.as_object_mut() else {
            continue;
        };
        let clip_id = obj
            .get("clip_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let shot_id = obj
            .get("shot_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let has_poster = !clip_id.is_empty()
            && resolve_ingest_poster_path(paths, project_id, &clip_id).is_some();
        let thumb_url = if has_poster {
            thumbnail_url(project_id, &clip_id)
        } else if !shot_id.is_empty() {
            format!(
                "/api/story/virtual-shot/{}/thumb?project_id={}&kind=in",
                shot_id, project_id
            )
        } else {
            String::new()
        };
        obj.insert("thumb_url".into(), json!(thumb_url));
        obj.insert("has_poster".into(), json!(has_poster));
        let play_path = if clip_id.is_empty() {
            String::new()
        } else {
            crate::media::resolve_play_media(paths, project_id, &clip_id)
                .ok()
                .map(|m| m.path.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        obj.insert("play_path".into(), json!(play_path));

        let shot_dur = obj
            .get("duration_seconds")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        if let Some(meta) = media_meta
            .get(&clip_id)
            .filter(|meta| is_valid_fps(meta.fps))
        {
            let duration_sec = meta.duration_sec;
            let use_fps = meta.fps;
            obj.insert("fps".into(), json!(use_fps));
            obj.insert("source_fps".into(), json!(use_fps));
            obj.insert("has_audio".into(), json!(meta.has_audio));
            obj.insert("audio_channels".into(), json!(meta.audio_channels));
            obj.insert("field_order".into(), json!(meta.field_order));
            obj.insert("interlaced".into(), json!(meta.interlaced));
            obj.insert("source_class".into(), json!(meta.source_class));
            obj.insert("proxy_recipe".into(), json!(meta.proxy_recipe));

            let in_seconds = obj
                .get("in_seconds")
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
                .max(0.0);
            let out_seconds = obj
                .get("out_seconds")
                .and_then(Value::as_f64)
                .unwrap_or(duration_sec)
                .max(in_seconds);
            let in_frame = seconds_to_frame(in_seconds, use_fps).max(0);
            let out_frame = seconds_to_frame(out_seconds, use_fps).max(in_frame + 1);
            let frames = out_frame - in_frame;
            obj.insert("in_frame".into(), json!(in_frame));
            obj.insert("out_frame".into(), json!(out_frame));
            obj.insert("duration_frames".into(), json!(frames));
            obj.insert(
                "duration_label".into(),
                json!(seconds_frames_label_from_frames(frames, use_fps)),
            );
            obj.insert(
                "duration_color_key".into(),
                json!(duration_color_key_from_frames(frames, use_fps)),
            );

            if shot_dur <= 0.0 {
                if duration_sec > 0.0 {
                    obj.insert("duration_seconds".into(), json!(duration_sec));
                    obj.insert("duration_sec".into(), json!(duration_sec));
                }
            } else {
                obj.insert("duration_sec".into(), json!(shot_dur));
            }
        } else if shot_dur > 0.0 {
            obj.insert("duration_sec".into(), json!(shot_dur));
        }
    }
    Ok(())
}

fn snapshot_json(
    paths: &ProjectPaths,
    conn: &Connection,
    project_id: &str,
    timeline_fps: f64,
    row: &StoryRow,
    parts: &[StoryPartRow],
) -> rusqlite::Result<Value> {
    let part_values: Vec<Value> = parts.iter().map(part_json).collect();
    let part_count = parts.len();
    let markers = if parts.is_empty() || !is_valid_fps(timeline_fps) {
        Vec::new()
    } else {
        markers_snapshot(conn, timeline_fps)?
    };
    let marker_slots = if parts.is_empty() {
        Vec::new()
    } else {
        marker_slots_snapshot(conn)?
    };
    let covers = covers_snapshot(conn)?;
    let mut all_virtual_shots = list_virtual_shots(conn)?;
    enrich_virtual_shots_from_ingest(paths, project_id, &mut all_virtual_shots)?;
    let project = project_settings_snapshot(paths, project_id).unwrap_or_else(|_| json!({}));
    let ingest_conn = open_ingest(paths, project_id)?;
    let archive_original = ingest_archive_original_enabled(&ingest_conn, &project)?;
    // All/source catalog = imported project media enriched with import_root identity.
    let all_clips =
        build_all_clips_snapshot(paths, project_id, &all_virtual_shots, archive_original)?;
    // Virtual tab = virtual cuts (everything except import_root source identities).
    let virtual_shots: Vec<Value> = all_virtual_shots
        .into_iter()
        .filter(|shot| shot.get("kind").and_then(Value::as_str) != Some("import_root"))
        .collect();

    // Samo fokusirani klipovi (parts/covers + selektirani ALL). Ostali stripovi
    // idu lazy preko GET /api/story/filmstrip — ALL tab može imati desetke klipova.
    let mut clip_ids = BTreeSet::new();
    for part in parts {
        let id = part.clip_id.trim();
        if !id.is_empty() {
            clip_ids.insert(id.to_string());
        }
    }
    for cover in &covers {
        if let Some(id) = cover.get("clip_id").and_then(|v| v.as_str()) {
            let id = id.trim();
            if !id.is_empty() {
                clip_ids.insert(id.to_string());
            }
        }
    }
    let selected_shot = row.selected_shot_id.trim();
    if !selected_shot.is_empty() {
        if let Some(clip_id) = all_clips
            .iter()
            .find(|c| c.get("root_shot_id").and_then(Value::as_str) == Some(selected_shot))
            .and_then(|c| c.get("clip_id").and_then(Value::as_str))
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            clip_ids.insert(clip_id.to_string());
        } else if let Some(rest) = selected_shot
            .strip_suffix("_root")
            .or_else(|| selected_shot.strip_prefix("root_"))
        {
            let id = rest.trim();
            if !id.is_empty() {
                clip_ids.insert(id.to_string());
            }
        } else if let Some(clip_id) = virtual_shots
            .iter()
            .find(|s| s.get("shot_id").and_then(Value::as_str) == Some(selected_shot))
            .and_then(|s| s.get("clip_id").and_then(Value::as_str))
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            clip_ids.insert(clip_id.to_string());
        }
    }
    let mut filmstrip_clips = Map::new();
    for clip_id in clip_ids {
        filmstrip_clips.insert(
            clip_id.clone(),
            clip_filmstrip_snapshot(paths, project_id, &clip_id),
        );
    }

    Ok(json!({
        "project_id": project_id,
        "selected_part_id": row.selected_part_id,
        "selected_shot_id": row.selected_shot_id,
        "selected_slot_id": row.selected_slot_id,
        "selected_cover_id": row.selected_cover_id,
        "parts": part_values,
        "markers": markers,
        "marker_slots": marker_slots,
        "covers": covers,
        "virtual_shots": virtual_shots,
        "all_clips": all_clips,
        "archive_original": archive_original,
        "filmstrip_clips": filmstrip_clips,
        "draft_updated_at": optional_text(&row.draft_updated_at),
        "committed_at": optional_text(&row.committed_at),
        "summary": {
            "part_count": part_count,
            "duration_sec": timeline_duration_from_parts(parts),
        },
    }))
}

fn table_exists(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![table],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}

fn list_virtual_shots(conn: &Connection) -> rusqlite::Result<Vec<Value>> {
    if !table_exists(conn, "virtual_shots")? {
        return Ok(Vec::new());
    }
    let has_kind = column_exists(conn, "virtual_shots", "kind")?;
    let has_virtual_name = column_exists(conn, "virtual_shots", "virtual_name")?;
    let kind_col = if has_kind { "kind" } else { "'' AS kind" };
    let virtual_name_col = if has_virtual_name {
        "virtual_name"
    } else {
        "'' AS virtual_name"
    };
    if !column_exists(conn, "virtual_shots", "duration_frames")? {
        let sql = format!(
            "SELECT shot_id, clip_id, source, quality, duration_seconds, in_seconds, out_seconds,
                    cover_path, out_cover_path, in_tc, out_tc, description, category_key, created_at,
                    {kind_col}, {virtual_name_col}
             FROM virtual_shots
             ORDER BY created_at, shot_id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            let in_seconds = row.get::<_, f64>(5)?;
            let out_seconds = row.get::<_, f64>(6)?;
            let fps = 0.0;
            let in_frame = 0;
            let out_frame = 0;
            let frames = 0;
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "shot_id": row.get::<_, String>(0)?,
                "clip_id": row.get::<_, String>(1)?,
                "source": row.get::<_, String>(2)?,
                "quality": row.get::<_, String>(3)?,
                "duration_seconds": row.get::<_, f64>(4)?,
                "in_seconds": in_seconds,
                "out_seconds": out_seconds,
                "cover_path": row.get::<_, String>(7)?,
                "out_cover_path": row.get::<_, String>(8)?,
                "in_tc": row.get::<_, String>(9)?,
                "out_tc": row.get::<_, String>(10)?,
                "description": row.get::<_, String>(11)?,
                "category_key": row.get::<_, String>(12)?,
                "fps": fps,
                "in_frame": in_frame,
                "out_frame": out_frame,
                "duration_frames": frames,
                "duration_label": "",
                "duration_color_key": "",
                "created_at": row.get::<_, Option<String>>(13)?,
                "kind": row.get::<_, String>(14)?,
                "virtual_name": row.get::<_, String>(15)?,
            }))
        })?;
        return rows.collect();
    }
    let sql = format!(
        "SELECT shot_id, clip_id, source, quality, duration_seconds, in_seconds, out_seconds,
                cover_path, out_cover_path, in_tc, out_tc, description, category_key,
                fps, in_frame, out_frame, duration_frames, duration_label, duration_color_key,
                created_at, {kind_col}, {virtual_name_col}
         FROM virtual_shots
         ORDER BY created_at, shot_id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(json!({
            "id": row.get::<_, String>(0)?,
            "shot_id": row.get::<_, String>(0)?,
            "clip_id": row.get::<_, String>(1)?,
            "source": row.get::<_, String>(2)?,
            "quality": row.get::<_, String>(3)?,
            "duration_seconds": row.get::<_, f64>(4)?,
            "in_seconds": row.get::<_, f64>(5)?,
            "out_seconds": row.get::<_, f64>(6)?,
            "cover_path": row.get::<_, String>(7)?,
            "out_cover_path": row.get::<_, String>(8)?,
            "in_tc": row.get::<_, String>(9)?,
            "out_tc": row.get::<_, String>(10)?,
            "description": row.get::<_, String>(11)?,
            "category_key": row.get::<_, String>(12)?,
            "fps": row.get::<_, f64>(13)?,
            "in_frame": row.get::<_, i64>(14)?,
            "out_frame": row.get::<_, i64>(15)?,
            "duration_frames": row.get::<_, i64>(16)?,
            "duration_label": row.get::<_, String>(17)?,
            "duration_color_key": row.get::<_, String>(18)?,
            "created_at": row.get::<_, Option<String>>(19)?,
            "kind": row.get::<_, String>(20)?,
            "virtual_name": row.get::<_, String>(21)?,
        }))
    })?;
    rows.collect()
}

fn virtual_shot_exists(conn: &Connection, shot_id: &str) -> Result<bool, String> {
    let shot_id = shot_id.trim();
    if shot_id.is_empty() {
        return Ok(false);
    }
    if !table_exists(conn, "virtual_shots").map_err(|e| e.to_string())? {
        return Ok(false);
    }
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM virtual_shots WHERE shot_id = ?1",
            params![shot_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok(exists > 0)
}

fn validate_virtual_shot_id(conn: &Connection, shot_id: &str) -> Result<String, String> {
    let shot_id = shot_id.trim();
    if shot_id.is_empty() {
        return Ok(String::new());
    }
    if !virtual_shot_exists(conn, shot_id)? {
        return Err(format!("virtual shot not found: {shot_id}"));
    }
    Ok(shot_id.to_string())
}

struct StoryShotForPart {
    shot_id: String,
    clip_id: String,
    in_tc: String,
    out_tc: String,
    in_seconds: f64,
    out_seconds: f64,
    fps: f64,
    in_frame: i64,
    out_frame: i64,
    duration_frames: i64,
    duration_label: String,
    duration_color_key: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SegmentRangeInput {
    pub in_frame: Option<i64>,
    pub out_frame: Option<i64>,
    pub in_seconds: Option<f64>,
    pub out_seconds: Option<f64>,
}

fn get_virtual_shot_for_part(
    conn: &Connection,
    virtual_shot_id: Option<&str>,
) -> Result<Option<StoryShotForPart>, String> {
    let explicit = virtual_shot_id.map(str::trim).filter(|s| !s.is_empty());
    let shot_id = if let Some(id) = explicit {
        validate_virtual_shot_id(conn, id)?
    } else {
        let row = read_row(conn).map_err(|e| e.to_string())?;
        let selected = row.selected_shot_id.trim();
        if selected.is_empty() {
            return Ok(None);
        }
        validate_virtual_shot_id(conn, selected)?
    };
    if !column_exists(conn, "virtual_shots", "duration_frames").map_err(|e| e.to_string())? {
        return Err(format!(
            "virtual shot '{shot_id}' nema FPS/frame metapodatke; osvježi source iz probe"
        ));
    }
    let shot = conn
        .query_row(
            "SELECT shot_id, clip_id, in_tc, out_tc, in_seconds, out_seconds,
                    fps, in_frame, out_frame, duration_frames, duration_label, duration_color_key
             FROM virtual_shots WHERE shot_id = ?1",
            params![shot_id],
            |r| {
                Ok(StoryShotForPart {
                    shot_id: r.get(0)?,
                    clip_id: r.get(1)?,
                    in_tc: r.get(2)?,
                    out_tc: r.get(3)?,
                    in_seconds: r.get(4)?,
                    out_seconds: r.get(5)?,
                    fps: r.get(6)?,
                    in_frame: r.get(7)?,
                    out_frame: r.get(8)?,
                    duration_frames: r.get(9)?,
                    duration_label: r.get(10)?,
                    duration_color_key: r.get(11)?,
                })
            },
        )
        .map_err(|e| e.to_string())?;
    if !is_valid_fps(shot.fps) {
        return Err(format!(
            "virtual shot '{}' nema valjan source FPS",
            shot.shot_id
        ));
    }
    Ok(Some(shot))
}

fn resolve_cover_virtual_shot_id(
    conn: &Connection,
    virtual_shot_id: Option<&str>,
) -> Result<Option<String>, String> {
    let explicit = virtual_shot_id.map(str::trim);
    if let Some(shot_id) = explicit {
        return Ok(Some(validate_virtual_shot_id(conn, shot_id)?));
    }
    let row = read_row(conn).map_err(|e| e.to_string())?;
    let selected = row.selected_shot_id.trim();
    if selected.is_empty() {
        return Ok(None);
    }
    Ok(Some(validate_virtual_shot_id(conn, selected)?))
}

fn get_virtual_shot_for_cover(
    conn: &Connection,
    virtual_shot_id: Option<&str>,
) -> Result<StoryShotForPart, String> {
    get_virtual_shot_for_part(conn, virtual_shot_id)?
        .ok_or_else(|| "odaberi virtualni kadar za pokrivanje".to_string())
}

struct CoverSourceTrim {
    in_frame: i64,
    out_frame: i64,
    fps: f64,
    in_seconds: f64,
    out_seconds: f64,
    in_tc: String,
    out_tc: String,
}

fn trim_cover_source_to_slot(shot: &StoryShotForPart) -> Result<CoverSourceTrim, String> {
    let fps = crate::frame_time::require_fps(shot.fps, "cover source")?;
    let in_frame = shot.in_frame.max(0);
    let out_frame = if shot.out_frame > in_frame {
        shot.out_frame
    } else if shot.duration_frames > 0 {
        in_frame + shot.duration_frames
    } else {
        in_frame + 1
    };
    let out_frame = out_frame.max(in_frame + 1);
    let in_seconds = frame_to_seconds(in_frame, fps);
    let out_seconds = frame_to_seconds(out_frame, fps);
    let in_tc = if shot.in_tc.trim().is_empty() {
        seconds_to_timecode(in_seconds, fps)
    } else {
        shot.in_tc.clone()
    };
    let out_tc = if shot.out_tc.trim().is_empty() {
        seconds_to_timecode(out_seconds, fps)
    } else {
        shot.out_tc.clone()
    };
    Ok(CoverSourceTrim {
        in_frame,
        out_frame,
        fps,
        in_seconds,
        out_seconds,
        in_tc,
        out_tc,
    })
}

fn load_snapshot(
    paths: &ProjectPaths,
    conn: &Connection,
    project_id: &str,
) -> rusqlite::Result<Value> {
    let row = read_row(conn)?;
    let _ = sync_story_part_source_fps(paths, project_id, conn);
    let parts = list_parts(conn)?;
    let timeline_fps = story_program_source_fps(&parts).unwrap_or(0.0);
    if !parts.is_empty() && is_valid_fps(timeline_fps) {
        ensure_materialized_slots(conn, timeline_fps)?;
    }
    snapshot_json(paths, conn, project_id, timeline_fps, &row, &parts)
}

pub fn load_state(paths: &ProjectPaths, project_id: &str) -> Result<Value, String> {
    let pid = project_id.trim();
    // Back-compat: pending selected source rows may have reserved import_root identity.
    let _ = crate::virtual_shots::ensure_reserved_root_shots_for_project(paths, pid);
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    load_snapshot(paths, &conn, pid).map_err(|e| e.to_string())
}

fn next_sort_index(conn: &Connection) -> rusqlite::Result<i64> {
    let max_idx: i64 = conn.query_row(
        "SELECT COALESCE(MAX(sort_index), -1) FROM story_parts",
        [],
        |r| r.get(0),
    )?;
    Ok(max_idx + 1)
}

fn set_selected_part_id(conn: &Connection, part_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE story_state SET selected_part_id = ?1 WHERE id = 1",
        params![part_id],
    )?;
    Ok(())
}

fn part_exists(conn: &Connection, part_id: &str) -> rusqlite::Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM story_parts WHERE part_id = ?1",
        params![part_id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

fn resolve_selection_after_delete(
    conn: &Connection,
    deleted_id: &str,
    deleted_sort: i64,
) -> rusqlite::Result<String> {
    let row = read_row(conn)?;
    if row.selected_part_id != deleted_id {
        return Ok(row.selected_part_id);
    }
    let neighbor: Option<String> = conn
        .query_row(
            "SELECT part_id FROM story_parts
             WHERE part_id != ?1
             ORDER BY ABS(sort_index - ?2) ASC, sort_index ASC
             LIMIT 1",
            params![deleted_id, deleted_sort],
            |r| r.get(0),
        )
        .ok();
    Ok(neighbor.unwrap_or_default())
}

/// Create a Segment-tab virtual shot (`story_parts`).
///
/// - `virtual_shot_id`: copy range from an existing Virtual-tab shot (no new Virtual row).
/// - or `clip_id` + IN/OUT: write segment directly from source marks (Talking Head /
///   Voice over) — stored as Segment-tab virtual identity, not in the Virtual/All tab list.
pub fn create_part(
    paths: &ProjectPaths,
    project_id: &str,
    kind: &str,
    virtual_shot_id: Option<&str>,
    clip_id: Option<&str>,
    in_seconds: Option<f64>,
    out_seconds: Option<f64>,
) -> Result<Value, String> {
    create_part_with_range(
        paths,
        project_id,
        kind,
        virtual_shot_id,
        clip_id,
        SegmentRangeInput {
            in_seconds,
            out_seconds,
            ..SegmentRangeInput::default()
        },
    )
}

pub fn create_part_with_range(
    paths: &ProjectPaths,
    project_id: &str,
    kind: &str,
    virtual_shot_id: Option<&str>,
    clip_id: Option<&str>,
    range: SegmentRangeInput,
) -> Result<Value, String> {
    let pid = project_id.trim();
    let kind = validate_kind(kind)?;
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    ensure_row(&conn).map_err(|e| e.to_string())?;
    let shot = resolve_segment_source(paths, pid, &conn, virtual_shot_id, clip_id, range)?;
    let part_id = new_part_id();
    let now = now_str();
    let sort_index = next_sort_index(&conn).map_err(|e| e.to_string())?;
    let shot_id = shot.as_ref().map(|s| s.shot_id.as_str()).unwrap_or("");
    let clip_id = shot.as_ref().map(|s| s.clip_id.as_str()).unwrap_or("");
    let in_tc = shot.as_ref().map(|s| s.in_tc.as_str()).unwrap_or("");
    let out_tc = shot.as_ref().map(|s| s.out_tc.as_str()).unwrap_or("");
    let in_seconds = shot.as_ref().map(|s| s.in_seconds);
    let out_seconds = shot.as_ref().map(|s| s.out_seconds);
    let fps = shot.as_ref().map(|s| s.fps).unwrap_or(0.0);
    let in_frame = shot.as_ref().map(|s| s.in_frame).unwrap_or(0);
    let out_frame = shot.as_ref().map(|s| s.out_frame).unwrap_or(0);
    let duration_frames = shot.as_ref().map(|s| s.duration_frames).unwrap_or(0);
    let duration_label = shot
        .as_ref()
        .map(|s| s.duration_label.clone())
        .unwrap_or_default();
    let duration_color_key = shot
        .as_ref()
        .map(|s| s.duration_color_key.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    conn.execute(
        "INSERT INTO story_parts
            (part_id, kind, sort_index, title, text, clip_id, virtual_shot_id,
             in_tc, out_tc, in_seconds, out_seconds,
             fps, in_frame, out_frame, duration_frames, duration_label, duration_color_key,
             created_at, updated_at)
         VALUES (?1, ?2, ?3, '', '', ?4, ?5, ?6, ?7, ?8, ?9,
                 ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?16)",
        params![
            part_id,
            kind,
            sort_index,
            clip_id,
            shot_id,
            in_tc,
            out_tc,
            in_seconds,
            out_seconds,
            fps,
            in_frame,
            out_frame,
            duration_frames,
            duration_label,
            duration_color_key,
            now,
        ],
    )
    .map_err(|e| e.to_string())?;
    set_selected_part_id(&conn, &part_id).map_err(|e| e.to_string())?;
    finalize_current_story_mutation(&conn).map_err(|e| e.to_string())?;
    load_snapshot(paths, &conn, pid).map_err(|e| e.to_string())
}

fn resolve_segment_source(
    paths: &ProjectPaths,
    project_id: &str,
    conn: &Connection,
    virtual_shot_id: Option<&str>,
    clip_id: Option<&str>,
    range: SegmentRangeInput,
) -> Result<Option<StoryShotForPart>, String> {
    if virtual_shot_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_some()
    {
        return get_virtual_shot_for_part(conn, virtual_shot_id);
    }
    if let Some(clip_id) = clip_id.map(str::trim).filter(|s| !s.is_empty()) {
        if range.in_frame.is_some() || range.out_frame.is_some() {
            let inn = range
                .in_frame
                .ok_or_else(|| "Segment frame IN nedostaje".to_string())?;
            let out = range
                .out_frame
                .ok_or_else(|| "Segment frame OUT nedostaje".to_string())?;
            // Segment-only path: story_parts trim, no virtual_shots insert.
            return Ok(Some(segment_source_from_clip_frames(
                paths, project_id, clip_id, inn, out,
            )?));
        }
        if range.in_seconds.is_some() || range.out_seconds.is_some() {
            let inn = range
                .in_seconds
                .ok_or_else(|| "Segment legacy seconds IN nedostaje".to_string())?;
            let out = range
                .out_seconds
                .ok_or_else(|| "Segment legacy seconds OUT nedostaje".to_string())?;
            // Compatibility fallback for older clients. New callers must send frames.
            return Ok(Some(segment_source_from_clip_seconds(
                paths, project_id, clip_id, inn, out,
            )?));
        }
        return Err("Segment source range nedostaje".into());
    }
    get_virtual_shot_for_part(conn, None)
}

/// Build Segment-tab virtual shot trim from source-file frame IN/OUT.
fn segment_source_from_clip_frames(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    in_frame: i64,
    out_frame: i64,
) -> Result<StoryShotForPart, String> {
    let fps = crate::media_pool::resolve_clip_fps(paths, project_id, clip_id)?;
    if !is_valid_fps(fps) {
        return Err(format!("clip '{clip_id}' nema valjan source FPS"));
    }
    let in_frame = in_frame.max(0);
    if out_frame <= in_frame {
        return Err("OUT mora biti poslije IN".into());
    }
    let out_frame = out_frame.max(in_frame + 1);
    let duration_frames = (out_frame - in_frame).max(0);
    let in_sec = frame_to_seconds(in_frame, fps);
    let out_sec = frame_to_seconds(out_frame, fps);
    Ok(StoryShotForPart {
        shot_id: String::new(),
        clip_id: clip_id.to_string(),
        in_tc: seconds_to_timecode(in_sec, fps),
        out_tc: seconds_to_timecode(out_sec, fps),
        in_seconds: in_sec,
        out_seconds: out_sec,
        fps,
        in_frame,
        out_frame,
        duration_frames,
        duration_label: seconds_frames_label_from_frames(duration_frames, fps),
        duration_color_key: duration_color_key_from_frames(duration_frames, fps).to_string(),
    })
}

fn segment_source_from_clip_seconds(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    in_seconds: f64,
    out_seconds: f64,
) -> Result<StoryShotForPart, String> {
    let fps = crate::media_pool::resolve_clip_fps(paths, project_id, clip_id)?;
    if !is_valid_fps(fps) {
        return Err(format!("clip '{clip_id}' nema valjan source FPS"));
    }
    let in_frame = seconds_to_frame(snap_seconds_to_frame(in_seconds.max(0.0), fps), fps);
    let out_frame = seconds_to_frame(snap_seconds_to_frame(out_seconds.max(0.0), fps), fps);
    segment_source_from_clip_frames(paths, project_id, clip_id, in_frame, out_frame)
}

pub fn update_part(
    paths: &ProjectPaths,
    project_id: &str,
    part_id: &str,
    title: Option<&str>,
    text: Option<&str>,
    kind: Option<&str>,
) -> Result<Value, String> {
    let pid = project_id.trim();
    let part_id = part_id.trim();
    if part_id.is_empty() {
        return Err("part_id required".into());
    }
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    if !part_exists(&conn, part_id).map_err(|e| e.to_string())? {
        return Err(format!("part not found: {part_id}"));
    }
    let now = now_str();
    if let Some(k) = kind {
        let k = validate_kind(k)?;
        conn.execute(
            "UPDATE story_parts SET kind = ?1, updated_at = ?2 WHERE part_id = ?3",
            params![k, now, part_id],
        )
        .map_err(|e| e.to_string())?;
    }
    if title.is_some() || text.is_some() {
        let title = title.unwrap_or("");
        let text = text.unwrap_or("");
        conn.execute(
            "UPDATE story_parts SET title = ?1, text = ?2, updated_at = ?3 WHERE part_id = ?4",
            params![title, text, now, part_id],
        )
        .map_err(|e| e.to_string())?;
    }
    finalize_current_story_mutation(&conn).map_err(|e| e.to_string())?;
    load_snapshot(paths, &conn, pid).map_err(|e| e.to_string())
}

fn get_part_row(conn: &Connection, part_id: &str) -> Result<StoryPartRow, String> {
    list_parts(conn)
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|p| p.part_id == part_id)
        .ok_or_else(|| format!("part not found: {part_id}"))
}

fn apply_part_source_trim_frames(
    conn: &Connection,
    part_id: &str,
    in_frame: i64,
    out_frame: i64,
) -> Result<(), String> {
    let part = get_part_row(conn, part_id)?;
    let fps = crate::frame_time::require_fps(part.fps, "story part source trim")?;
    let in_frame = in_frame.max(0);
    if out_frame <= in_frame {
        return Err("OUT mora biti poslije IN".into());
    }
    let out_frame = out_frame.max(in_frame + 1);
    let duration_frames = (out_frame - in_frame).max(0);
    let in_sec = frame_to_seconds(in_frame, fps);
    let out_sec = frame_to_seconds(out_frame, fps);
    let in_tc = seconds_to_timecode(in_sec, fps);
    let out_tc = seconds_to_timecode(out_sec, fps);
    let duration_label = seconds_frames_label_from_frames(duration_frames, fps);
    let duration_color_key = duration_color_key_from_frames(duration_frames, fps).to_string();
    let now = now_str();
    conn.execute(
        "UPDATE story_parts SET in_seconds = ?1, out_seconds = ?2, in_tc = ?3, out_tc = ?4,
         in_frame = ?5, out_frame = ?6, duration_frames = ?7, duration_label = ?8,
         duration_color_key = ?9, updated_at = ?10 WHERE part_id = ?11",
        params![
            in_sec,
            out_sec,
            in_tc,
            out_tc,
            in_frame,
            out_frame,
            duration_frames,
            duration_label,
            duration_color_key,
            now,
            part_id,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn set_part_mark_in(
    paths: &ProjectPaths,
    project_id: &str,
    part_id: &str,
    local_sec: f64,
) -> Result<Value, String> {
    let pid = project_id.trim();
    let part_id = part_id.trim();
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    let part = get_part_row(&conn, part_id)?;
    let fps = crate::frame_time::require_fps(part.fps, "story part mark in")?;
    let local_frame = seconds_to_frame(snap_seconds_to_frame(local_sec.max(0.0), fps), fps);
    drop(conn);
    set_part_mark_in_frame(paths, pid, part_id, local_frame)
}

pub fn set_part_mark_in_frame(
    paths: &ProjectPaths,
    project_id: &str,
    part_id: &str,
    local_frame: i64,
) -> Result<Value, String> {
    let pid = project_id.trim();
    let part_id = part_id.trim();
    if part_id.is_empty() {
        return Err("part_id required".into());
    }
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    let timeline_fps = require_current_story_program_source_fps(&conn)?;
    let part = get_part_row(&conn, part_id)?;
    let current_in = part.in_frame.max(0);
    let current_out = part.out_frame.max(current_in + 1);
    let span = (current_out - current_in).max(0);
    let local = if span > 0 {
        local_frame.max(0).min(span)
    } else {
        0
    };
    let new_in = current_in + local;
    apply_part_source_trim_frames(&conn, part_id, new_in, current_out)?;
    finalize_story_mutation(&conn, timeline_fps).map_err(|e| e.to_string())?;
    load_snapshot(paths, &conn, pid).map_err(|e| e.to_string())
}

pub fn set_part_mark_out(
    paths: &ProjectPaths,
    project_id: &str,
    part_id: &str,
    local_sec: f64,
) -> Result<Value, String> {
    let pid = project_id.trim();
    let part_id = part_id.trim();
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    let part = get_part_row(&conn, part_id)?;
    let fps = crate::frame_time::require_fps(part.fps, "story part mark out")?;
    let local_frame = seconds_to_frame(snap_seconds_to_frame(local_sec.max(0.0), fps), fps);
    drop(conn);
    set_part_mark_out_frame(paths, pid, part_id, local_frame)
}

pub fn set_part_mark_out_frame(
    paths: &ProjectPaths,
    project_id: &str,
    part_id: &str,
    local_frame: i64,
) -> Result<Value, String> {
    let pid = project_id.trim();
    let part_id = part_id.trim();
    if part_id.is_empty() {
        return Err("part_id required".into());
    }
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    let timeline_fps = require_current_story_program_source_fps(&conn)?;
    let part = get_part_row(&conn, part_id)?;
    let current_in = part.in_frame.max(0);
    let current_out = part.out_frame.max(current_in + 1);
    let span = (current_out - current_in).max(0);
    let local = if span > 0 {
        local_frame.max(0).min(span)
    } else {
        0
    };
    let new_out = current_in + local;
    apply_part_source_trim_frames(&conn, part_id, current_in, new_out)?;
    finalize_story_mutation(&conn, timeline_fps).map_err(|e| e.to_string())?;
    load_snapshot(paths, &conn, pid).map_err(|e| e.to_string())
}

pub fn delete_part(paths: &ProjectPaths, project_id: &str, part_id: &str) -> Result<Value, String> {
    let pid = project_id.trim();
    let part_id = part_id.trim();
    if part_id.is_empty() {
        return Err("part_id required".into());
    }
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    let deleted_sort: i64 = conn
        .query_row(
            "SELECT sort_index FROM story_parts WHERE part_id = ?1",
            params![part_id],
            |r| r.get(0),
        )
        .map_err(|_| format!("part not found: {part_id}"))?;
    let parts_before_delete = list_parts(&conn).map_err(|e| e.to_string())?;
    let timeline_fps = story_program_source_fps(&parts_before_delete);
    conn.execute(
        "DELETE FROM story_parts WHERE part_id = ?1",
        params![part_id],
    )
    .map_err(|e| e.to_string())?;
    if let Some(timeline_fps) = timeline_fps {
        delete_markers_for_part(&conn, part_id, timeline_fps, &parts_before_delete)
            .map_err(|e| e.to_string())?;
    }
    let next_selected =
        resolve_selection_after_delete(&conn, part_id, deleted_sort).map_err(|e| e.to_string())?;
    set_selected_part_id(&conn, &next_selected).map_err(|e| e.to_string())?;
    let parts = list_parts(&conn).map_err(|e| e.to_string())?;
    for (idx, part) in parts.iter().enumerate() {
        conn.execute(
            "UPDATE story_parts SET sort_index = ?1 WHERE part_id = ?2",
            params![idx as i64, part.part_id],
        )
        .map_err(|e| e.to_string())?;
    }
    if let Some(timeline_fps) = timeline_fps {
        finalize_story_mutation(&conn, timeline_fps).map_err(|e| e.to_string())?;
    } else {
        touch_draft(&conn).map_err(|e| e.to_string())?;
    }
    load_snapshot(paths, &conn, pid).map_err(|e| e.to_string())
}

pub fn reorder_part(
    paths: &ProjectPaths,
    project_id: &str,
    part_id: &str,
    direction: &str,
) -> Result<Value, String> {
    let pid = project_id.trim();
    let part_id = part_id.trim();
    if part_id.is_empty() {
        return Err("part_id required".into());
    }
    let dir = direction.trim().to_lowercase();
    if dir != "up" && dir != "down" {
        return Err(format!("invalid direction: {direction}"));
    }
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    let mut parts = list_parts(&conn).map_err(|e| e.to_string())?;
    let idx = parts
        .iter()
        .position(|p| p.part_id == part_id)
        .ok_or_else(|| format!("part not found: {part_id}"))?;
    let swap_with = if dir == "up" {
        if idx == 0 {
            return load_snapshot(paths, &conn, pid).map_err(|e| e.to_string());
        }
        idx - 1
    } else if idx + 1 >= parts.len() {
        return load_snapshot(paths, &conn, pid).map_err(|e| e.to_string());
    } else {
        idx + 1
    };
    parts.swap(idx, swap_with);
    for (i, part) in parts.iter().enumerate() {
        conn.execute(
            "UPDATE story_parts SET sort_index = ?1, updated_at = ?2 WHERE part_id = ?3",
            params![i as i64, now_str(), part.part_id],
        )
        .map_err(|e| e.to_string())?;
    }
    finalize_current_story_mutation(&conn).map_err(|e| e.to_string())?;
    load_snapshot(paths, &conn, pid).map_err(|e| e.to_string())
}

pub fn select_part(paths: &ProjectPaths, project_id: &str, part_id: &str) -> Result<Value, String> {
    let pid = project_id.trim();
    let part_id = part_id.trim();
    if part_id.is_empty() {
        return Err("part_id required".into());
    }
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    if !part_exists(&conn, part_id).map_err(|e| e.to_string())? {
        return Err(format!("part not found: {part_id}"));
    }
    set_selected_part_id(&conn, part_id).map_err(|e| e.to_string())?;
    touch_draft(&conn).map_err(|e| e.to_string())?;
    load_snapshot(paths, &conn, pid).map_err(|e| e.to_string())
}

pub fn select_shot(
    paths: &ProjectPaths,
    project_id: &str,
    virtual_shot_id: &str,
) -> Result<Value, String> {
    let pid = project_id.trim();
    let shot_id = virtual_shot_id.trim();
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    if shot_id.is_empty() {
        conn.execute(
            "UPDATE story_state SET selected_shot_id = '' WHERE id = 1",
            [],
        )
        .map_err(|e| e.to_string())?;
        touch_draft(&conn).map_err(|e| e.to_string())?;
        return load_snapshot(paths, &conn, pid).map_err(|e| e.to_string());
    }
    if !virtual_shot_exists(&conn, shot_id)? {
        return Err(format!("virtual shot not found: {shot_id}"));
    }
    conn.execute(
        "UPDATE story_state SET selected_shot_id = ?1 WHERE id = 1",
        params![shot_id],
    )
    .map_err(|e| e.to_string())?;
    touch_draft(&conn).map_err(|e| e.to_string())?;
    load_snapshot(paths, &conn, pid).map_err(|e| e.to_string())
}

pub fn create_marker(
    paths: &ProjectPaths,
    project_id: &str,
    timeline_sec: Option<f64>,
    part_id: Option<&str>,
    label: Option<&str>,
    local_sec: Option<f64>,
) -> Result<Value, String> {
    let pid = project_id.trim();
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    let timeline_fps = require_current_story_program_source_fps(&conn)?;
    let parts = list_parts(&conn).map_err(|e| e.to_string())?;
    let (resolved_sec, origin_part_id, origin_local_sec) =
        resolve_marker_timeline_sec(&parts, timeline_sec, part_id, local_sec)?;
    let origin_part = if origin_part_id.is_empty() {
        None
    } else {
        Some(origin_part_id.as_str())
    };
    create_marker_row(
        &conn,
        resolved_sec,
        label,
        origin_part,
        origin_local_sec,
        timeline_fps,
    )?;
    load_snapshot(paths, &conn, pid).map_err(|e| e.to_string())
}

pub fn create_marker_from_frame(
    paths: &ProjectPaths,
    project_id: &str,
    timeline_frame: i64,
    part_id: Option<&str>,
    label: Option<&str>,
    local_frame: Option<i64>,
) -> Result<Value, String> {
    let pid = project_id.trim();
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    let timeline_fps = require_current_story_program_source_fps(&conn)?;
    let parts = list_parts(&conn).map_err(|e| e.to_string())?;
    let (resolved_frame, origin_part_id, origin_local_frame) = resolve_marker_timeline_frame(
        &parts,
        Some(timeline_frame),
        part_id,
        local_frame,
        timeline_fps,
    )?;
    let origin_part = if origin_part_id.is_empty() {
        None
    } else {
        Some(origin_part_id.as_str())
    };
    create_marker_row_frame(
        &conn,
        resolved_frame,
        label,
        origin_part,
        origin_local_frame,
        timeline_fps,
    )?;
    load_snapshot(paths, &conn, pid).map_err(|e| e.to_string())
}

pub fn create_marker_from_part_frame(
    paths: &ProjectPaths,
    project_id: &str,
    part_id: &str,
    label: Option<&str>,
    local_frame: i64,
) -> Result<Value, String> {
    let pid = project_id.trim();
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    let timeline_fps = require_current_story_program_source_fps(&conn)?;
    let parts = list_parts(&conn).map_err(|e| e.to_string())?;
    let (resolved_frame, origin_part_id, origin_local_frame) = resolve_marker_timeline_frame(
        &parts,
        None,
        Some(part_id),
        Some(local_frame),
        timeline_fps,
    )?;
    let origin_part = if origin_part_id.is_empty() {
        None
    } else {
        Some(origin_part_id.as_str())
    };
    create_marker_row_frame(
        &conn,
        resolved_frame,
        label,
        origin_part,
        origin_local_frame,
        timeline_fps,
    )?;
    load_snapshot(paths, &conn, pid).map_err(|e| e.to_string())
}

pub fn delete_marker(
    paths: &ProjectPaths,
    project_id: &str,
    marker_id: &str,
) -> Result<Value, String> {
    let pid = project_id.trim();
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    let timeline_fps = require_current_story_program_source_fps(&conn)?;
    delete_marker_row(&conn, marker_id, timeline_fps)?;
    load_snapshot(paths, &conn, pid).map_err(|e| e.to_string())
}

pub fn move_marker(
    paths: &ProjectPaths,
    project_id: &str,
    marker_id: &str,
    direction: &str,
) -> Result<Value, String> {
    let pid = project_id.trim();
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    let timeline_fps = require_current_story_program_source_fps(&conn)?;
    move_marker_row(&conn, marker_id, direction, timeline_fps)?;
    load_snapshot(paths, &conn, pid).map_err(|e| e.to_string())
}

pub fn update_marker(
    paths: &ProjectPaths,
    project_id: &str,
    marker_id: &str,
    timeline_sec: f64,
    label: Option<&str>,
) -> Result<Value, String> {
    let pid = project_id.trim();
    let conn = open_project(paths, pid).map_err(|error| error.to_string())?;
    let timeline_fps = require_current_story_program_source_fps(&conn)?;
    update_marker_row(&conn, marker_id, timeline_sec, label, timeline_fps)?;
    load_snapshot(paths, &conn, pid).map_err(|error| error.to_string())
}

pub fn update_marker_frame(
    paths: &ProjectPaths,
    project_id: &str,
    marker_id: &str,
    timeline_frame: i64,
    label: Option<&str>,
) -> Result<Value, String> {
    let pid = project_id.trim();
    let conn = open_project(paths, pid).map_err(|error| error.to_string())?;
    let timeline_fps = require_current_story_program_source_fps(&conn)?;
    update_marker_frame_row(&conn, marker_id, timeline_frame, label, timeline_fps)?;
    load_snapshot(paths, &conn, pid).map_err(|error| error.to_string())
}

pub fn select_marker_slot(
    paths: &ProjectPaths,
    project_id: &str,
    slot_id: &str,
) -> Result<Value, String> {
    let pid = project_id.trim();
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    select_marker_slot_row(&conn, slot_id)?;
    load_snapshot(paths, &conn, pid).map_err(|e| e.to_string())
}

pub fn create_cover(
    paths: &ProjectPaths,
    project_id: &str,
    slot_id: &str,
    clip_id: Option<&str>,
    virtual_shot_id: Option<&str>,
    title: Option<&str>,
    note: Option<&str>,
    range: SegmentRangeInput,
) -> Result<Value, String> {
    let pid = project_id.trim();
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    let timeline_fps = require_current_story_program_source_fps(&conn)?;
    ensure_materialized_slots(&conn, timeline_fps).map_err(|e| e.to_string())?;
    let _slot = super::markers::get_slot_by_id(&conn, slot_id)?;
    let shot = if virtual_shot_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .is_some()
        || (range.in_frame.is_none() && range.out_frame.is_none())
    {
        get_virtual_shot_for_cover(&conn, virtual_shot_id)?
    } else {
        let clip_id = clip_id
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| "Pokrivalica iz source frameova treba clip_id".to_string())?;
        let in_frame = range
            .in_frame
            .ok_or_else(|| "Pokrivalica frame IN nedostaje".to_string())?;
        let out_frame = range
            .out_frame
            .ok_or_else(|| "Pokrivalica frame OUT nedostaje".to_string())?;
        let created = crate::virtual_shots::add_virtual_shot_from_frames(
            paths, pid, clip_id, in_frame, out_frame,
        )?;
        let shot_id = created
            .get("shot_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "virtual-shot bez shot_id".to_string())?
            .to_string();
        ensure_row(&conn).map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE story_state SET selected_shot_id = ?1 WHERE id = 1",
            params![shot_id],
        )
        .map_err(|e| e.to_string())?;
        get_virtual_shot_for_cover(&conn, Some(shot_id.as_str()))?
    };
    let cover_trim = trim_cover_source_to_slot(&shot)?;
    let clip_id = clip_id
        .filter(|v| !v.trim().is_empty())
        .unwrap_or(shot.clip_id.as_str());
    create_cover_row(
        &conn,
        slot_id,
        Some(clip_id),
        Some(shot.shot_id.as_str()),
        Some(cover_trim.in_tc.as_str()),
        Some(cover_trim.out_tc.as_str()),
        Some(cover_trim.in_seconds),
        Some(cover_trim.out_seconds),
        Some(cover_trim.in_frame),
        Some(cover_trim.out_frame),
        Some(cover_trim.fps),
        title,
        note,
    )?;
    load_snapshot(paths, &conn, pid).map_err(|e| e.to_string())
}

pub fn update_cover(
    paths: &ProjectPaths,
    project_id: &str,
    cover_id: &str,
    title: Option<&str>,
    note: Option<&str>,
    clip_id: Option<&str>,
    virtual_shot_id: Option<&str>,
) -> Result<Value, String> {
    let pid = project_id.trim();
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    let resolved_virtual_shot_id = if virtual_shot_id.is_some() {
        resolve_cover_virtual_shot_id(&conn, virtual_shot_id)?
    } else {
        None
    };
    update_cover_row(
        &conn,
        cover_id,
        title,
        note,
        clip_id,
        resolved_virtual_shot_id.as_deref(),
    )?;
    load_snapshot(paths, &conn, pid).map_err(|e| e.to_string())
}

pub fn delete_cover(
    paths: &ProjectPaths,
    project_id: &str,
    cover_id: &str,
) -> Result<Value, String> {
    let pid = project_id.trim();
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    delete_cover_row(&conn, cover_id)?;
    load_snapshot(paths, &conn, pid).map_err(|e| e.to_string())
}

pub fn select_cover(
    paths: &ProjectPaths,
    project_id: &str,
    cover_id: &str,
) -> Result<Value, String> {
    let pid = project_id.trim();
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    select_cover_row(&conn, cover_id)?;
    load_snapshot(paths, &conn, pid).map_err(|e| e.to_string())
}

pub fn commit_story(paths: &ProjectPaths, project_id: &str) -> Result<Value, String> {
    let pid = project_id.trim();
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    ensure_row(&conn).map_err(|e| e.to_string())?;
    let now = now_str();
    conn.execute(
        "UPDATE story_state SET committed_at = ?1, updated_at = ?1 WHERE id = 1",
        params![now],
    )
    .map_err(|e| e.to_string())?;
    load_snapshot(paths, &conn, pid).map_err(|e| e.to_string())
}

/// Resolve a Story segment to its source cut for virtual-stream playback.
/// Returns `(clip_id, in_frame, out_frame, source_fps)`.
pub fn part_stream_frames(
    paths: &ProjectPaths,
    project_id: &str,
    part_id: &str,
) -> Result<(String, i64, i64, f64), String> {
    let pid = project_id.trim();
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    ensure_schema(&conn).map_err(|e| e.to_string())?;
    let (clip_id, in_seconds, out_seconds, source_fps, mut in_frame, mut out_frame): (
        String,
        Option<f64>,
        Option<f64>,
        f64,
        i64,
        i64,
    ) = conn
        .query_row(
            "SELECT clip_id, in_seconds, out_seconds, fps, in_frame, out_frame
             FROM story_parts WHERE part_id = ?1",
            params![part_id.trim()],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .map_err(|_| format!("Segment '{}' nije pronađen", part_id.trim()))?;
    let clip_id = clip_id.trim().to_string();
    if clip_id.is_empty() {
        return Err(format!("Segment '{}' nema izvorni klip", part_id.trim()));
    }
    let fps = if source_fps.is_finite() && source_fps > 0.0 {
        normalize_fps(source_fps)
    } else {
        crate::media_pool::resolve_clip_fps(paths, pid, &clip_id)?
    };
    if out_frame <= in_frame {
        let in_sec = in_seconds.unwrap_or(0.0).max(0.0);
        let out_sec = out_seconds.unwrap_or(0.0).max(0.0);
        in_frame = (in_sec * fps).round() as i64;
        out_frame = (out_sec * fps).round() as i64;
    }
    let in_frame = in_frame.max(0);
    let out_frame = out_frame.max(in_frame + 1);
    Ok((clip_id, in_frame, out_frame, fps))
}

/// Resolve a Story cover to its slot-bounded source cut for virtual-stream playback.
/// Returns `(clip_id, in_frame, out_frame, source_fps)`.
pub fn cover_stream_frames(
    paths: &ProjectPaths,
    project_id: &str,
    cover_id: &str,
) -> Result<(String, i64, i64, f64), String> {
    let pid = project_id.trim();
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    ensure_cover_schema(&conn).map_err(|e| e.to_string())?;
    let (clip_id, virtual_shot_id, in_seconds, out_seconds, source_in_frame, source_out_frame): (
        String,
        String,
        Option<f64>,
        Option<f64>,
        i64,
        i64,
    ) = conn
        .query_row(
            "SELECT clip_id, virtual_shot_id, in_seconds, out_seconds,
                    source_in_frame, source_out_frame
             FROM story_covers WHERE cover_id = ?1",
            params![cover_id.trim()],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .map_err(|_| format!("Pokrivanje '{}' nije pronađeno", cover_id.trim()))?;
    let shot_id = virtual_shot_id.trim();
    let (clip_id, fps) = if !shot_id.is_empty() {
        let (shot_clip, _shot_in, _shot_out, fps) =
            crate::virtual_shots::virtual_shot_frames(paths, pid, shot_id)?;
        if !clip_id.trim().is_empty() && clip_id.trim() != shot_clip {
            return Err(format!(
                "Pokrivanje '{}': clip_id '{}' ne odgovara virtual shot clip_id '{}'",
                cover_id.trim(),
                clip_id.trim(),
                shot_clip
            ));
        }
        (shot_clip, fps)
    } else {
        let clip_id = clip_id.trim().to_string();
        if clip_id.is_empty() {
            return Err(format!(
                "Pokrivanje '{}' nema virtualni kadar ni klip",
                cover_id.trim()
            ));
        }
        let fps = crate::media_pool::resolve_clip_fps(paths, pid, &clip_id)?;
        (clip_id, fps)
    };
    let in_frame = source_in_frame.max(0);
    let out_frame = source_out_frame.max(in_frame + 1);
    let (in_frame, out_frame) = if source_out_frame > source_in_frame {
        (in_frame, out_frame)
    } else {
        let in_sec = in_seconds.unwrap_or(0.0).max(0.0);
        let out_sec = out_seconds.unwrap_or(0.0).max(0.0);
        let in_frame = seconds_to_frame(in_sec, fps).max(0);
        let out_frame = seconds_to_frame(out_sec, fps).max(in_frame + 1);
        (in_frame, out_frame)
    };
    Ok((clip_id, in_frame, out_frame, fps))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_paths(base: &std::path::Path) -> ProjectPaths {
        ProjectPaths {
            data_dir: base.join("data"),
            projects_root: base.join("projects"),
            seed_path: base.join("seed.json"),
        }
    }

    fn seed_imported_clip(paths: &ProjectPaths, project_id: &str, clip_id: &str, fps: f64) {
        let conn = crate::ingest::db::open_ingest(paths, project_id).unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, duration_sec, fps, import_status)
             VALUES ('src_a', ?1, ?1, 10.0, ?2, 'imported')",
            params![clip_id, fps],
        )
        .unwrap();
    }

    fn seed_materialized_clip(paths: &ProjectPaths, project_id: &str, clip_id: &str, fps: f64) {
        let proxy_path = paths
            .project_dir(project_id)
            .join("proxy")
            .join(format!("{clip_id}.mp4"));
        std::fs::create_dir_all(proxy_path.parent().unwrap()).unwrap();
        let ffmpeg =
            crate::ingest::thumb::resolve_ffmpeg().expect("ffmpeg required for video test");
        let status = std::process::Command::new(ffmpeg)
            .args(["-y", "-v", "error", "-f", "lavfi"])
            .args(["-i", &format!("color=c=black:s=64x64:d=4:r={fps}")])
            .args(["-an", "-pix_fmt", "yuv420p"])
            .arg(&proxy_path)
            .status()
            .expect("ffmpeg test video");
        assert!(status.success(), "ffmpeg test video failed");
        let proxy_path = proxy_path.to_string_lossy().to_string();
        let conn = crate::ingest::db::open_ingest(paths, project_id).unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, duration_sec, fps, status, import_status,
                 project_proxy_path)
             VALUES (?1, ?1, ?1, 10.0, ?2, 'active', 'imported', ?3)",
            params![clip_id, fps, proxy_path],
        )
        .unwrap();
    }

    #[test]
    fn story_all_catalog_keeps_selected_thumbs_and_imported_ready_status() {
        let base =
            std::env::temp_dir().join(format!("qnc_story_all_catalog_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "story_all_catalog";
        let conn = open_project(&paths, project_id).unwrap();
        ensure_schema(&conn).unwrap();
        drop(conn);

        let proxy_path = paths
            .project_dir(project_id)
            .join("proxy")
            .join("imported_clip.mp4")
            .to_string_lossy()
            .to_string();
        let thumb_path = paths
            .project_dir(project_id)
            .join("ingest")
            .join("thumbnails")
            .join("imported_clip")
            .join("poster.jpg")
            .to_string_lossy()
            .to_string();
        let selected_thumb_path = paths
            .project_dir(project_id)
            .join("ingest")
            .join("thumbnails")
            .join("selected_clip")
            .join("poster.jpg")
            .to_string_lossy()
            .to_string();
        let conn = crate::ingest::db::open_ingest(&paths, project_id).unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, duration_sec, fps, status, import_status, selected)
             VALUES ('card_a', 'card_only_clip', 'Card only', 8.0, 50.0,
                     'on_source', 'detected', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, duration_sec, fps, status, import_status, selected,
                 source_path, proxy_path, thumb_path, virtual_name)
             VALUES ('card_a', 'selected_clip', 'Selected clip', 9.0, 50.0,
                     'on_source', 'detected', 1, 'G:/DCIM/selected_clip.MP4',
                     'G:/SUB/selected_clip_proxy.MP4', ?1, 'selected_clip_root.mp4')",
            params![selected_thumb_path],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, duration_sec, fps, has_audio, audio_channels,
                 status, import_status, selected, source_path, project_proxy_path, thumb_path,
                 virtual_name)
             VALUES ('card_a', 'imported_clip', 'Imported clip', 10.0, 50.0, 1, 2,
                     'active', 'imported', 0, 'G:/DCIM/imported_clip.MP4', ?1, ?2, '')",
            params![proxy_path, thumb_path],
        )
        .unwrap();
        drop(conn);

        let state = load_state(&paths, project_id).unwrap();
        let clips = state.get("all_clips").and_then(Value::as_array).unwrap();
        assert_eq!(
            clips
                .iter()
                .map(|clip| clip.get("clip_id").and_then(Value::as_str).unwrap_or(""))
                .collect::<Vec<_>>(),
            vec!["imported_clip", "selected_clip"],
            "source catalog projects selected thumbnails immediately and imported media as ready"
        );
        let imported = clips
            .iter()
            .find(|clip| clip.get("clip_id").and_then(Value::as_str) == Some("imported_clip"))
            .unwrap();
        assert_eq!(imported.get("fps").and_then(Value::as_f64), Some(50.0));
        assert_eq!(
            imported.get("has_audio").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            imported.get("audio_channels").and_then(Value::as_u64),
            Some(2)
        );
        assert_eq!(
            imported.get("status_proxy").and_then(Value::as_str),
            Some("ready")
        );
        assert_eq!(
            imported.get("project_proxy_path").and_then(Value::as_str),
            Some(proxy_path.as_str())
        );
        assert_eq!(
            imported.get("play_path").and_then(Value::as_str),
            Some(proxy_path.as_str())
        );
        assert_eq!(
            imported.get("materialized").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            imported.get("has_poster").and_then(Value::as_bool),
            Some(true)
        );
        assert!(
            imported
                .get("thumb_url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .contains("/api/ingest/thumbnail"),
            "thumbnail URL must be projected from DB thumbnail metadata"
        );

        let selected = clips
            .iter()
            .find(|clip| clip.get("clip_id").and_then(Value::as_str) == Some("selected_clip"))
            .unwrap();
        assert_eq!(
            selected.get("status_proxy").and_then(Value::as_str),
            Some("pending"),
            "selected source row keeps the yellow/pending proxy status until project proxy exists"
        );
        assert_eq!(
            selected.get("selected").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            selected.get("materialized").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(selected.get("play_path").and_then(Value::as_str), Some(""));
        assert_eq!(
            selected.get("has_poster").and_then(Value::as_bool),
            Some(true),
            "selected clip thumbnail must be visible immediately while import is pending"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn story_all_catalog_prioritizes_pending_selected_row_over_stale_ready_path() {
        let base = std::env::temp_dir().join(format!(
            "qnc_story_pending_status_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "story_pending_status";
        let conn = open_project(&paths, project_id).unwrap();
        ensure_schema(&conn).unwrap();
        drop(conn);

        let stale_proxy_path = paths
            .project_dir(project_id)
            .join("proxy")
            .join("clip_a.mp4")
            .to_string_lossy()
            .to_string();
        let selected_thumb_path = paths
            .project_dir(project_id)
            .join("ingest")
            .join("thumbnails")
            .join("clip_a")
            .join("poster.jpg")
            .to_string_lossy()
            .to_string();

        let conn = crate::ingest::db::open_ingest(&paths, project_id).unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, duration_sec, fps, status, import_status, selected,
                 source_path, project_proxy_path, thumb_path)
             VALUES ('card_a', 'clip_a', 'Clip A', 10.0, 50.0,
                     'on_source', 'queued', 1, 'G:/DCIM/clip_a.MP4', ?1, ?2)",
            params![stale_proxy_path, selected_thumb_path],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, duration_sec, fps, status, import_status, selected,
                 project_proxy_path)
             VALUES ('project_proxy_repair', 'clip_a', 'Clip A stale', 10.0, 50.0,
                     'active', 'imported', 0, ?1)",
            params![stale_proxy_path],
        )
        .unwrap();
        drop(conn);

        let state = load_state(&paths, project_id).unwrap();
        let clips = state.get("all_clips").and_then(Value::as_array).unwrap();
        assert_eq!(clips.len(), 1);
        let clip = &clips[0];
        assert_eq!(clip.get("clip_id").and_then(Value::as_str), Some("clip_a"));
        assert_eq!(
            clip.get("import_status").and_then(Value::as_str),
            Some("queued"),
            "pending selected source row is the active DB state for this clip"
        );
        assert_eq!(
            clip.get("status_proxy").and_then(Value::as_str),
            Some("pending"),
            "queued clip must keep the yellow dot even if a stale proxy path exists"
        );
        assert_eq!(
            clip.get("materialized").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(clip.get("play_path").and_then(Value::as_str), Some(""));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn story_all_catalog_refreshes_materialized_pending_proxy_for_play() {
        let base = std::env::temp_dir().join(format!(
            "qnc_story_materialized_pending_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "story_materialized_pending";
        let conn = open_project(&paths, project_id).unwrap();
        ensure_schema(&conn).unwrap();
        drop(conn);

        let proxy_path = paths
            .project_dir(project_id)
            .join("proxy")
            .join("clip_ready.mp4");
        std::fs::create_dir_all(proxy_path.parent().unwrap()).unwrap();
        std::fs::write(&proxy_path, b"proxy").unwrap();
        let proxy_path = proxy_path.to_string_lossy().to_string();
        let selected_thumb_path = paths
            .project_dir(project_id)
            .join("ingest")
            .join("thumbnails")
            .join("clip_ready")
            .join("poster.jpg")
            .to_string_lossy()
            .to_string();

        let conn = crate::ingest::db::open_ingest(&paths, project_id).unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, duration_sec, fps, status, import_status, selected,
                 source_path, project_proxy_path, thumb_path)
             VALUES ('card_a', 'clip_ready', 'Clip Ready', 10.0, 50.0,
                     'on_source', 'queued', 1, 'G:/DCIM/clip_ready.MP4', ?1, ?2)",
            params![proxy_path, selected_thumb_path],
        )
        .unwrap();
        drop(conn);

        let state = load_state(&paths, project_id).unwrap();
        let clips = state.get("all_clips").and_then(Value::as_array).unwrap();
        assert_eq!(clips.len(), 1);
        let clip = &clips[0];
        assert_eq!(
            clip.get("import_status").and_then(Value::as_str),
            Some("imported"),
            "effective source snapshot must refresh stale pending import status when project proxy exists"
        );
        assert_eq!(
            clip.get("status_proxy").and_then(Value::as_str),
            Some("ready"),
            "green proxy dot follows materialized project proxy, not stale queued status"
        );
        assert_eq!(
            clip.get("materialized").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            clip.get("play_path").and_then(Value::as_str),
            Some(proxy_path.as_str()),
            "playback gate must receive path when project proxy is ready"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn segment_from_source_frames_is_segment_tab_virtual_shot_not_virtual_tab_row() {
        let base = std::env::temp_dir().join(format!(
            "qnc_story_segment_frames_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "story_segment_frames";
        let conn = open_project(&paths, project_id).unwrap();
        ensure_schema(&conn).unwrap();
        drop(conn);
        seed_imported_clip(&paths, project_id, "clip_a", 25.0);

        let state = create_part_with_range(
            &paths,
            project_id,
            "tonovi",
            None,
            Some("clip_a"),
            SegmentRangeInput {
                in_frame: Some(25),
                out_frame: Some(75),
                ..SegmentRangeInput::default()
            },
        )
        .unwrap();
        let part = state
            .get("parts")
            .and_then(Value::as_array)
            .and_then(|parts| parts.first())
            .unwrap();
        let part_id = part.get("part_id").and_then(Value::as_str).unwrap();
        assert_eq!(part.get("shot_id").and_then(Value::as_str), Some(part_id));
        assert_eq!(
            part.get("root_shot_id").and_then(Value::as_str),
            Some("clip_a_root")
        );
        assert_eq!(
            part.get("source_class").and_then(Value::as_str),
            Some("segment")
        );
        assert_eq!(part.get("clip_id").and_then(Value::as_str), Some("clip_a"));
        assert_eq!(
            part.get("virtual_shot_id").and_then(Value::as_str),
            Some("")
        );
        assert_eq!(part.get("in_frame").and_then(Value::as_i64), Some(25));
        assert_eq!(part.get("out_frame").and_then(Value::as_i64), Some(75));
        assert_eq!(part.get("in_seconds").and_then(Value::as_f64), Some(1.0));
        assert_eq!(part.get("out_seconds").and_then(Value::as_f64), Some(3.0));

        assert_eq!(
            state
                .get("virtual_shots")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(0),
            "Segment-tab virtual segment must not create a Virtual-tab shot"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn cover_from_source_frames_creates_virtual_shot_then_cover() {
        let base = std::env::temp_dir().join(format!(
            "qnc_story_cover_source_frames_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "story_cover_source_frames";
        let conn = open_project(&paths, project_id).unwrap();
        ensure_schema(&conn).unwrap();
        drop(conn);
        seed_materialized_clip(&paths, project_id, "clip_a", 25.0);

        create_part_with_range(
            &paths,
            project_id,
            "tonovi",
            None,
            Some("clip_a"),
            SegmentRangeInput {
                in_frame: Some(0),
                out_frame: Some(100),
                ..SegmentRangeInput::default()
            },
        )
        .unwrap();
        let marked =
            create_marker(&paths, project_id, Some(2.0), None, Some("slot-end"), None).unwrap();
        let slot_id = marked
            .get("marker_slots")
            .and_then(Value::as_array)
            .and_then(|slots| slots.first())
            .and_then(|slot| slot.get("slot_id"))
            .and_then(Value::as_str)
            .unwrap()
            .to_string();

        let state = create_cover(
            &paths,
            project_id,
            &slot_id,
            Some("clip_a"),
            None,
            None,
            None,
            SegmentRangeInput {
                in_frame: Some(25),
                out_frame: Some(75),
                ..SegmentRangeInput::default()
            },
        )
        .unwrap();
        let cover = state
            .get("covers")
            .and_then(Value::as_array)
            .and_then(|covers| covers.first())
            .unwrap();
        let virtual_shot_id = cover
            .get("virtual_shot_id")
            .and_then(Value::as_str)
            .unwrap();

        assert!(!virtual_shot_id.is_empty());
        assert_eq!(cover.get("clip_id").and_then(Value::as_str), Some("clip_a"));
        assert_eq!(
            cover.get("source_in_frame").and_then(Value::as_i64),
            Some(25)
        );
        assert_eq!(
            cover.get("source_out_frame").and_then(Value::as_i64),
            Some(75)
        );
        let virtual_shot = state
            .get("virtual_shots")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|shot| shot.get("shot_id").and_then(Value::as_str) == Some(virtual_shot_id))
            .expect("cover must reference a materialized virtual shot");
        assert_eq!(
            virtual_shot.get("kind").and_then(Value::as_str),
            Some("virtual")
        );
        assert_eq!(
            state.get("selected_shot_id").and_then(Value::as_str),
            Some(virtual_shot_id)
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn cover_from_source_frames_accepts_multiple_empty_slots() {
        let base = std::env::temp_dir().join(format!(
            "qnc_story_cover_multiple_slots_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "story_cover_multiple_slots";
        let conn = open_project(&paths, project_id).unwrap();
        ensure_schema(&conn).unwrap();
        drop(conn);
        seed_materialized_clip(&paths, project_id, "clip_a", 25.0);

        create_part_with_range(
            &paths,
            project_id,
            "tonovi",
            None,
            Some("clip_a"),
            SegmentRangeInput {
                in_frame: Some(0),
                out_frame: Some(100),
                ..SegmentRangeInput::default()
            },
        )
        .unwrap();
        create_marker(&paths, project_id, Some(1.0), None, Some("m1"), None).unwrap();
        let marked = create_marker(&paths, project_id, Some(2.0), None, Some("m2"), None).unwrap();
        let slots = marked
            .get("marker_slots")
            .and_then(Value::as_array)
            .expect("marker slots");
        assert!(slots.len() >= 3);
        let first_slot = slots[0]
            .get("slot_id")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        let second_slot = slots[1]
            .get("slot_id")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();

        create_cover(
            &paths,
            project_id,
            &first_slot,
            Some("clip_a"),
            None,
            None,
            None,
            SegmentRangeInput {
                in_frame: Some(0),
                out_frame: Some(25),
                ..SegmentRangeInput::default()
            },
        )
        .unwrap();
        let state = create_cover(
            &paths,
            project_id,
            &second_slot,
            Some("clip_a"),
            None,
            None,
            None,
            SegmentRangeInput {
                in_frame: Some(25),
                out_frame: Some(50),
                ..SegmentRangeInput::default()
            },
        )
        .unwrap();

        let covers = state
            .get("covers")
            .and_then(Value::as_array)
            .expect("covers");
        assert_eq!(covers.len(), 2);
        let marker_slots = state
            .get("marker_slots")
            .and_then(Value::as_array)
            .expect("marker slots");
        assert_eq!(
            marker_slots[0].get("has_cover").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            marker_slots[1].get("has_cover").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            marker_slots[2].get("has_cover").and_then(Value::as_bool),
            Some(false)
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn source_less_segment_keeps_basic_part_mutations_available() {
        let base = std::env::temp_dir().join(format!(
            "qnc_story_source_less_segment_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "story_source_less_segment";
        let conn = open_project(&paths, project_id).unwrap();
        ensure_schema(&conn).unwrap();
        drop(conn);

        let state = create_part(&paths, project_id, "tonovi", None, None, None, None).unwrap();
        let part_id = state
            .get("selected_part_id")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        let parts = state.get("parts").and_then(Value::as_array).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].get("clip_id").and_then(Value::as_str), Some(""));
        assert_eq!(parts[0].get("fps").and_then(Value::as_f64), Some(0.0));
        assert_eq!(
            state
                .get("marker_slots")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(0)
        );

        let updated = update_part(&paths, project_id, &part_id, Some("Draft"), None, None).unwrap();
        assert_eq!(
            updated
                .get("parts")
                .and_then(Value::as_array)
                .and_then(|parts| parts.first())
                .and_then(|part| part.get("title"))
                .and_then(Value::as_str),
            Some("Draft")
        );

        create_part(&paths, project_id, "offovi", None, None, None, None).unwrap();
        reorder_part(&paths, project_id, &part_id, "down").unwrap();
        let deleted = delete_part(&paths, project_id, &part_id).unwrap();
        assert_eq!(
            deleted.get("parts").and_then(Value::as_array).map(Vec::len),
            Some(1)
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn story_snapshot_reads_and_selects_virtual_shots_from_project_db() {
        let base = std::env::temp_dir().join(format!(
            "qnc_story_virtual_shots_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "story_proj";
        let conn = open_project(&paths, project_id).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS virtual_shots (
                shot_id TEXT PRIMARY KEY,
                clip_id TEXT NOT NULL DEFAULT '',
                source TEXT NOT NULL DEFAULT '',
                quality TEXT NOT NULL DEFAULT '',
                duration_seconds REAL NOT NULL DEFAULT 0,
                in_seconds REAL NOT NULL DEFAULT 0,
                out_seconds REAL NOT NULL DEFAULT 0,
                fps REAL NOT NULL DEFAULT 0,
                in_frame INTEGER NOT NULL DEFAULT 0,
                out_frame INTEGER NOT NULL DEFAULT 0,
                duration_frames INTEGER NOT NULL DEFAULT 0,
                duration_label TEXT NOT NULL DEFAULT '',
                duration_color_key TEXT NOT NULL DEFAULT '',
                cover_path TEXT NOT NULL DEFAULT '',
                out_cover_path TEXT NOT NULL DEFAULT '',
                in_tc TEXT NOT NULL DEFAULT '',
                out_tc TEXT NOT NULL DEFAULT '',
                description TEXT NOT NULL DEFAULT '',
                category_key TEXT NOT NULL DEFAULT '',
                data_json TEXT NOT NULL DEFAULT '{}',
                created_at TEXT,
                updated_at TEXT
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO virtual_shots
                (shot_id, clip_id, source, quality, duration_seconds, in_seconds, out_seconds,
                 fps, in_frame, out_frame, duration_frames, duration_label, duration_color_key,
                 in_tc, out_tc, description, category_key, created_at)
             VALUES ('shot_a', 'clip_a', 'manual', 'ok', 2.0, 1.0, 3.0,
                     25.0, 25, 75, 50, '2:00', 'under_3',
                     '00:00:01:00', '00:00:03:00', 'Opis', 'manual_cut', 'epoch_1')",
            [],
        )
        .unwrap();
        drop(conn);

        let state = load_state(&paths, project_id).unwrap();
        let shots = state
            .get("virtual_shots")
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(shots.len(), 1);
        assert_eq!(
            shots[0].get("shot_id").and_then(|v| v.as_str()),
            Some("shot_a")
        );

        let selected = select_shot(&paths, project_id, "shot_a").unwrap();
        assert_eq!(
            selected.get("selected_shot_id").and_then(|v| v.as_str()),
            Some("shot_a")
        );

        let with_part = create_part(&paths, project_id, "tonovi", None, None, None, None).unwrap();
        let part_id = with_part
            .get("selected_part_id")
            .and_then(|v| v.as_str())
            .unwrap()
            .to_string();
        let part = with_part
            .get("parts")
            .and_then(|v| v.as_array())
            .and_then(|parts| parts.first())
            .unwrap();
        assert_eq!(
            part.get("virtual_shot_id").and_then(|v| v.as_str()),
            Some("shot_a")
        );
        assert_eq!(part.get("clip_id").and_then(|v| v.as_str()), Some("clip_a"));
        assert_eq!(part.get("in_seconds").and_then(|v| v.as_f64()), Some(1.0));
        assert_eq!(part.get("out_seconds").and_then(|v| v.as_f64()), Some(3.0));
        assert_eq!(part.get("in_frame").and_then(|v| v.as_i64()), Some(25));
        assert_eq!(part.get("out_frame").and_then(|v| v.as_i64()), Some(75));
        assert_eq!(
            part.get("duration_frames").and_then(|v| v.as_i64()),
            Some(50)
        );
        assert_eq!(
            part.get("duration_label").and_then(|v| v.as_str()),
            Some("2:00")
        );
        assert_eq!(
            part.get("duration_color_key").and_then(|v| v.as_str()),
            Some("under_3")
        );
        let with_marker =
            create_marker(&paths, project_id, None, Some(&part_id), None, Some(1.0)).unwrap();
        let marker_id = with_marker
            .get("markers")
            .and_then(|v| v.as_array())
            .and_then(|markers| {
                markers
                    .iter()
                    .find(|marker| marker.get("timeline_sec").and_then(Value::as_f64) == Some(1.0))
            })
            .and_then(|marker| marker.get("marker_id"))
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        let updated_marker =
            update_marker_frame(&paths, project_id, &marker_id, 38, Some("Prijelaz")).unwrap();
        let marker = updated_marker
            .get("markers")
            .and_then(Value::as_array)
            .and_then(|markers| {
                markers.iter().find(|marker| {
                    marker.get("marker_id").and_then(Value::as_str) == Some(marker_id.as_str())
                })
            })
            .unwrap();
        assert_eq!(
            marker.get("timeline_frame").and_then(Value::as_i64),
            Some(38)
        );
        assert_eq!(
            marker.get("label").and_then(Value::as_str),
            Some("Prijelaz")
        );
        let start_marker_id = updated_marker
            .get("markers")
            .and_then(Value::as_array)
            .and_then(|markers| {
                markers
                    .iter()
                    .find(|marker| marker.get("timeline_sec").and_then(Value::as_f64) == Some(0.0))
            })
            .and_then(|marker| marker.get("marker_id"))
            .and_then(Value::as_str)
            .unwrap();
        assert!(update_marker(
            &paths,
            project_id,
            start_marker_id,
            0.5,
            Some("Nedopušteno")
        )
        .is_err());
        let with_second_marker =
            create_marker(&paths, project_id, Some(1.8), None, Some("Drugi"), None).unwrap();
        assert!(update_marker(&paths, project_id, &marker_id, 1.8, Some("Kolizija")).is_err());
        let second_marker_id = with_second_marker
            .get("markers")
            .and_then(Value::as_array)
            .and_then(|markers| {
                markers
                    .iter()
                    .find(|marker| marker.get("timeline_sec").and_then(Value::as_f64) == Some(1.8))
            })
            .and_then(|marker| marker.get("marker_id"))
            .and_then(Value::as_str)
            .unwrap();
        delete_marker(&paths, project_id, second_marker_id).unwrap();
        let restored_marker = delete_marker(&paths, project_id, &marker_id).unwrap();
        let slot_id = restored_marker
            .get("marker_slots")
            .and_then(|v| v.as_array())
            .and_then(|slots| slots.first())
            .and_then(|slot| slot.get("slot_id"))
            .and_then(|v| v.as_str())
            .unwrap()
            .to_string();
        let with_cover = create_cover(
            &paths,
            project_id,
            &slot_id,
            None,
            None,
            None,
            None,
            SegmentRangeInput::default(),
        )
        .unwrap();
        let cover = with_cover
            .get("covers")
            .and_then(|v| v.as_array())
            .and_then(|covers| covers.first())
            .unwrap();
        assert_eq!(
            cover.get("virtual_shot_id").and_then(|v| v.as_str()),
            Some("shot_a")
        );
        assert_eq!(
            cover.get("clip_id").and_then(|v| v.as_str()),
            Some("clip_a")
        );
        assert_eq!(cover.get("in_seconds").and_then(|v| v.as_f64()), Some(1.0));
        assert_eq!(cover.get("out_seconds").and_then(|v| v.as_f64()), Some(3.0));
        assert_eq!(
            cover.get("source_in_frame").and_then(|v| v.as_i64()),
            Some(25)
        );
        assert_eq!(
            cover.get("source_out_frame").and_then(|v| v.as_i64()),
            Some(75)
        );
        assert_eq!(cover.get("source_fps").and_then(|v| v.as_f64()), Some(25.0));
        assert_eq!(
            cover.get("in_tc").and_then(|v| v.as_str()),
            Some("00:00:01:00")
        );
        assert_eq!(
            cover.get("out_tc").and_then(|v| v.as_str()),
            Some("00:00:03:00")
        );
        assert!(update_cover(
            &paths,
            project_id,
            cover.get("cover_id").and_then(|v| v.as_str()).unwrap(),
            None,
            None,
            None,
            Some("missing_shot")
        )
        .is_err());

        let committed = commit_story(&paths, project_id).unwrap();
        assert!(committed
            .get("committed_at")
            .and_then(|v| v.as_str())
            .is_some());

        let _ = std::fs::remove_dir_all(&base);
    }
}
