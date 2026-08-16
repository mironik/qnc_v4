//! Neutral editorial domain: virtual shots (root + derived).
//!
//! This module OWNS the `virtual_shots` table (schema, migrations, CRUD) and the
//! per-shot cover artifacts. Ingest, QStory and Media Pool all depend on this
//! module — never the reverse ownership. Clip metadata (fps/duration/proxy) is
//! still read from `crate::media_pool` (ingest-backed reads); moving those reads
//! into `crate::ingest` is a separate follow-up.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::frame_time::{
    dual_fps_snapshot, duration_color_key_from_frames, duration_frames, frame_to_seconds,
    is_valid_fps, normalize_fps, rational_fps, seconds_frames_label_from_frames, seconds_to_frame,
    seconds_to_timecode, snap_seconds_to_frame, DualFpsSnapshot,
};
use crate::ingest::thumb::{extract_poster_jpeg_at_seek, media_duration_sec};
use crate::media::{
    clip_id_token, derived_shot_id, root_shot_id, virtual_name_for_derived_shot,
    virtual_name_for_root_clip,
};
use crate::media_pool::{proxy_path_for_clip, read_imported_clips, resolve_clip_fps};
use crate::project::db::{now_str, open_project, ProjectPaths};

/// Open the project DB and guarantee the virtual_shots schema is present.
pub(crate) fn open(paths: &ProjectPaths, project_id: &str) -> Result<Connection, String> {
    let conn = open_project(paths, project_id).map_err(|e| e.to_string())?;
    ensure(paths, project_id, &conn)?;
    Ok(conn)
}

/// Ensure the virtual_shots schema, migrations and backfills on an existing
/// connection. Safe to call repeatedly; other modules (Media Pool) call this
/// while opening their own tables.
pub(crate) fn ensure(
    paths: &ProjectPaths,
    project_id: &str,
    conn: &Connection,
) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS virtual_shots (
            shot_id TEXT PRIMARY KEY,
            clip_id TEXT NOT NULL DEFAULT '',
            kind TEXT NOT NULL DEFAULT 'derived',
            source_shot_id TEXT NOT NULL DEFAULT '',
            locked INTEGER NOT NULL DEFAULT 0,
            display_name TEXT NOT NULL DEFAULT '',
            source TEXT NOT NULL DEFAULT '',
            quality TEXT NOT NULL DEFAULT '',
            duration_seconds REAL NOT NULL DEFAULT 0,
            in_seconds REAL NOT NULL DEFAULT 0,
            out_seconds REAL NOT NULL DEFAULT 0,
            fps REAL NOT NULL DEFAULT 0,
            source_fps REAL NOT NULL DEFAULT 0,
            timeline_fps REAL NOT NULL DEFAULT 0,
            field_order TEXT NOT NULL DEFAULT '',
            interlaced INTEGER NOT NULL DEFAULT 0,
            source_class TEXT NOT NULL DEFAULT '',
            proxy_recipe TEXT NOT NULL DEFAULT '',
            source_fps_num INTEGER NOT NULL DEFAULT 0,
            source_fps_den INTEGER NOT NULL DEFAULT 1,
            timeline_fps_num INTEGER NOT NULL DEFAULT 0,
            timeline_fps_den INTEGER NOT NULL DEFAULT 1,
            in_frame INTEGER NOT NULL DEFAULT 0,
            out_frame INTEGER NOT NULL DEFAULT 0,
            duration_frames INTEGER NOT NULL DEFAULT 0,
            timeline_duration_frames INTEGER NOT NULL DEFAULT 0,
            duration_label TEXT NOT NULL DEFAULT '',
            duration_color_key TEXT NOT NULL DEFAULT '',
            cover_path TEXT NOT NULL DEFAULT '',
            out_cover_path TEXT NOT NULL DEFAULT '',
            in_tc TEXT NOT NULL DEFAULT '',
            out_tc TEXT NOT NULL DEFAULT '',
            description TEXT NOT NULL DEFAULT '',
            category_key TEXT NOT NULL DEFAULT '',
            virtual_name TEXT NOT NULL DEFAULT '',
            data_json TEXT NOT NULL DEFAULT '{}',
            created_at TEXT,
            updated_at TEXT
        );",
    )
    .map_err(|e| e.to_string())?;
    migrate_columns(conn)?;
    migrate_virtual_shots_json(paths, project_id, conn)?;
    migrate_shot_identity_standard(conn)?;
    backfill_frame_fields(paths, project_id, conn)?;
    backfill_dual_fps(paths, project_id, conn)?;
    sync_virtual_shot_probe_meta(conn)?;
    Ok(())
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    conn.prepare(&format!("PRAGMA table_info({table})"))
        .and_then(|mut stmt| {
            stmt.query_map([], |row| row.get::<_, String>(1))
                .map(|rows| rows.filter_map(Result::ok).any(|name| name == column))
        })
        .unwrap_or(false)
}

fn migrate_columns(conn: &Connection) -> Result<(), String> {
    if !column_exists(conn, "virtual_shots", "in_seconds") {
        let _ = conn.execute(
            "ALTER TABLE virtual_shots ADD COLUMN in_seconds REAL NOT NULL DEFAULT 0",
            [],
        );
    }
    if !column_exists(conn, "virtual_shots", "out_seconds") {
        let _ = conn.execute(
            "ALTER TABLE virtual_shots ADD COLUMN out_seconds REAL NOT NULL DEFAULT 0",
            [],
        );
    }
    for (column, sql_type) in [
        ("kind", "TEXT NOT NULL DEFAULT 'derived'"),
        ("source_shot_id", "TEXT NOT NULL DEFAULT ''"),
        ("locked", "INTEGER NOT NULL DEFAULT 0"),
        ("display_name", "TEXT NOT NULL DEFAULT ''"),
        ("virtual_name", "TEXT NOT NULL DEFAULT ''"),
        ("cover_path", "TEXT NOT NULL DEFAULT ''"),
        ("out_cover_path", "TEXT NOT NULL DEFAULT ''"),
        ("in_tc", "TEXT NOT NULL DEFAULT ''"),
        ("out_tc", "TEXT NOT NULL DEFAULT ''"),
        ("fps", "REAL NOT NULL DEFAULT 0"),
        ("source_fps", "REAL NOT NULL DEFAULT 0"),
        ("timeline_fps", "REAL NOT NULL DEFAULT 0"),
        ("field_order", "TEXT NOT NULL DEFAULT ''"),
        ("interlaced", "INTEGER NOT NULL DEFAULT 0"),
        ("source_class", "TEXT NOT NULL DEFAULT ''"),
        ("proxy_recipe", "TEXT NOT NULL DEFAULT ''"),
        ("source_fps_num", "INTEGER NOT NULL DEFAULT 0"),
        ("source_fps_den", "INTEGER NOT NULL DEFAULT 1"),
        ("timeline_fps_num", "INTEGER NOT NULL DEFAULT 0"),
        ("timeline_fps_den", "INTEGER NOT NULL DEFAULT 1"),
        ("in_frame", "INTEGER NOT NULL DEFAULT 0"),
        ("out_frame", "INTEGER NOT NULL DEFAULT 0"),
        ("duration_frames", "INTEGER NOT NULL DEFAULT 0"),
        ("timeline_duration_frames", "INTEGER NOT NULL DEFAULT 0"),
        ("duration_label", "TEXT NOT NULL DEFAULT ''"),
        ("duration_color_key", "TEXT NOT NULL DEFAULT ''"),
        ("description", "TEXT NOT NULL DEFAULT ''"),
        ("category_key", "TEXT NOT NULL DEFAULT ''"),
    ] {
        if !column_exists(conn, "virtual_shots", column) {
            let _ = conn.execute(
                &format!("ALTER TABLE virtual_shots ADD COLUMN {column} {sql_type}"),
                [],
            );
        }
    }
    Ok(())
}

fn migrate_virtual_shots_json(
    paths: &ProjectPaths,
    project_id: &str,
    conn: &Connection,
) -> Result<(), String> {
    let mut stmt = conn
        .prepare("SELECT shot_id, clip_id, data_json FROM virtual_shots WHERE data_json != '{}'")
        .map_err(|e| e.to_string())?;
    let rows: Vec<(String, String, String)> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    for (shot_id, clip_id, raw) in rows {
        let data = serde_json::from_str::<Value>(&raw).unwrap_or(json!({}));
        let in_sec = data
            .get("in_seconds")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let out_sec = data
            .get("out_seconds")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let category_key = data
            .get("categories")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let fps = fps_for_clip(paths, project_id, &clip_id).unwrap_or(0.0);
        let (in_frame, out_frame, duration_frames, duration_label, duration_color_key) =
            if is_valid_fps(fps) {
                let in_frame = seconds_to_frame(in_sec, fps);
                let out_frame = seconds_to_frame(out_sec, fps);
                let duration_frames = duration_frames(in_sec, out_sec, fps);
                (
                    in_frame,
                    out_frame,
                    duration_frames,
                    seconds_frames_label_from_frames(duration_frames, fps),
                    duration_color_key_from_frames(duration_frames, fps).to_string(),
                )
            } else {
                (0, 0, 0, String::new(), String::new())
            };
        conn.execute(
            "UPDATE virtual_shots SET
                in_seconds = CASE WHEN in_seconds = 0 AND ?2 > 0 THEN ?2 ELSE in_seconds END,
                out_seconds = CASE WHEN out_seconds = 0 AND ?3 > 0 THEN ?3 ELSE out_seconds END,
                cover_path = CASE WHEN cover_path = '' THEN ?4 ELSE cover_path END,
                out_cover_path = CASE WHEN out_cover_path = '' THEN ?5 ELSE out_cover_path END,
                in_tc = CASE WHEN in_tc = '' THEN ?6 ELSE in_tc END,
                out_tc = CASE WHEN out_tc = '' THEN ?7 ELSE out_tc END,
                description = CASE WHEN description = '' THEN ?8 ELSE description END,
                category_key = CASE WHEN category_key = '' THEN ?9 ELSE category_key END,
                fps = CASE WHEN fps <= 0 THEN ?10 ELSE fps END,
                source_fps = CASE WHEN source_fps <= 0 THEN ?10 ELSE source_fps END,
                in_frame = CASE WHEN in_frame = 0 THEN ?11 ELSE in_frame END,
                out_frame = CASE WHEN out_frame = 0 THEN ?12 ELSE out_frame END,
                duration_frames = CASE WHEN duration_frames = 0 THEN ?13 ELSE duration_frames END,
                duration_label = CASE WHEN duration_label = '' THEN ?14 ELSE duration_label END,
                duration_color_key = CASE WHEN duration_color_key = '' THEN ?15 ELSE duration_color_key END,
                data_json = '{}'
             WHERE shot_id = ?1",
            params![
                shot_id,
                in_sec,
                out_sec,
                data.get("cover_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                data.get("out_cover_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                data.get("in_tc").and_then(|v| v.as_str()).unwrap_or(""),
                data.get("out_tc").and_then(|v| v.as_str()).unwrap_or(""),
                data.get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                category_key,
                fps,
                in_frame,
                out_frame,
                duration_frames,
                duration_label,
                duration_color_key,
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// One naming standard:
/// - root (full length): `{clip}_root` / `{clip}_root.ext`
/// - derived: `{clip}_shot_001` / `{clip}_shot_001.ext`
fn migrate_shot_identity_standard(conn: &Connection) -> Result<(), String> {
    // Legacy root ids: `root_{clip}` → `{clip}_root`
    let mut stmt = conn
        .prepare(
            "SELECT shot_id, clip_id, virtual_name FROM virtual_shots
             WHERE kind = 'import_root'",
        )
        .map_err(|e| e.to_string())?;
    let roots: Vec<(String, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    for (old_id, clip_id, virtual_name) in roots {
        let new_id = root_shot_id(&clip_id);
        if new_id.is_empty() {
            continue;
        }
        if old_id != new_id {
            rename_shot_id(conn, &old_id, &new_id)?;
        }
        let want = if virtual_name.trim().is_empty() {
            virtual_name_for_root_clip(&clip_id, "")
        } else if !virtual_name.contains("_root.")
            && !virtual_name.ends_with("_root")
            && !virtual_name.contains("_root.vclip")
        {
            // Old `mironik_1483.mxf` → `mironik_1483_root.mxf`
            let ext = virtual_name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
            virtual_name_for_root_clip(&clip_id, ext)
        } else {
            continue;
        };
        if !want.is_empty() && want != virtual_name {
            let _ = conn.execute(
                "UPDATE virtual_shots SET virtual_name = ?2, display_name = CASE
                    WHEN TRIM(display_name) = '' OR display_name = virtual_name THEN ?2
                    ELSE display_name END
                 WHERE shot_id = ?1",
                params![new_id, want],
            );
        }
    }

    // Legacy derived UUID ids (`…_shot_a1b2c3…`) → `{clip}_shot_001` ordered by created_at.
    // Do not touch already-standard `_shot_NNN` or unrelated test ids.
    let mut clip_stmt = conn
        .prepare(
            "SELECT DISTINCT clip_id FROM virtual_shots
             WHERE kind != 'import_root' AND shot_id LIKE '%_shot_%'",
        )
        .map_err(|e| e.to_string())?;
    let clip_ids: Vec<String> = clip_stmt
        .query_map([], |r| r.get(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    for clip_id in clip_ids {
        let mut q = conn
            .prepare(
                "SELECT shot_id, virtual_name FROM virtual_shots
                 WHERE clip_id = ?1 AND kind != 'import_root'
                 ORDER BY created_at, shot_id",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<(String, String)> = q
            .query_map(params![clip_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        if !rows.iter().any(|(id, _)| is_legacy_derived_shot_id(id)) {
            continue;
        }
        // Two-pass rename avoids PK collisions while renumbering.
        let mut parked: Vec<(String, u32, String)> = Vec::new();
        for (i, (old_id, old_vname)) in rows.into_iter().enumerate() {
            let index = (i as u32) + 1;
            let park = format!("__mig_{}_{index}", clip_id_token(&clip_id));
            if old_id != park {
                if shot_id_exists(conn, &park)? {
                    continue;
                }
                rename_shot_id(conn, &old_id, &park)?;
            }
            parked.push((park, index, old_vname));
        }
        let root_vname: String = conn
            .query_row(
                "SELECT virtual_name FROM virtual_shots
                 WHERE clip_id = ?1 AND kind = 'import_root'
                 LIMIT 1",
                params![clip_id],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| virtual_name_for_root_clip(&clip_id, ""));
        for (park, index, old_vname) in parked {
            let new_id = derived_shot_id(&clip_id, index);
            let new_vname = virtual_name_for_derived_shot(&root_vname, index);
            if park != new_id {
                rename_shot_id(conn, &park, &new_id)?;
            }
            if new_vname != old_vname && !new_vname.is_empty() {
                let _ = conn.execute(
                    "UPDATE virtual_shots SET virtual_name = ?2, display_name = CASE
                        WHEN TRIM(display_name) = '' OR display_name = virtual_name THEN ?2
                        ELSE display_name END
                     WHERE shot_id = ?1",
                    params![new_id, new_vname],
                );
            }
        }
    }
    Ok(())
}

fn is_legacy_derived_shot_id(shot_id: &str) -> bool {
    let Some((_, rest)) = shot_id.rsplit_once("_shot_") else {
        return false;
    };
    // Standard: `_shot_001` (digits only, short). Legacy UUID tokens are longer / hex.
    !(rest.chars().all(|c| c.is_ascii_digit()) && (1..=4).contains(&rest.len()))
}

fn shot_id_exists(conn: &Connection, shot_id: &str) -> Result<bool, String> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM virtual_shots WHERE shot_id = ?1",
            params![shot_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok(n > 0)
}

fn rename_shot_id(conn: &Connection, old_id: &str, new_id: &str) -> Result<(), String> {
    if old_id == new_id || old_id.is_empty() || new_id.is_empty() {
        return Ok(());
    }
    if shot_id_exists(conn, new_id)? {
        return Ok(());
    }
    conn.execute(
        "UPDATE virtual_shots SET shot_id = ?2 WHERE shot_id = ?1",
        params![old_id, new_id],
    )
    .map_err(|e| e.to_string())?;
    // Keep story / cover / state FKs in sync (same project DB).
    let _ = conn.execute(
        "UPDATE story_state SET selected_shot_id = ?2 WHERE selected_shot_id = ?1",
        params![old_id, new_id],
    );
    let _ = conn.execute(
        "UPDATE story_parts SET virtual_shot_id = ?2 WHERE virtual_shot_id = ?1",
        params![old_id, new_id],
    );
    let _ = conn.execute(
        "UPDATE story_covers SET virtual_shot_id = ?2 WHERE virtual_shot_id = ?1",
        params![old_id, new_id],
    );
    let _ = conn.execute(
        "UPDATE virtual_shots SET source_shot_id = ?2 WHERE source_shot_id = ?1",
        params![old_id, new_id],
    );
    if table_exists(conn, "media_pool_workflow") {
        let _ = conn.execute(
            "UPDATE media_pool_workflow SET active_virtual_shot_id = ?2
             WHERE active_virtual_shot_id = ?1",
            params![old_id, new_id],
        );
    }
    Ok(())
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        params![name],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .unwrap_or(false)
}

fn next_derived_shot_index(conn: &Connection, clip_id: &str) -> Result<u32, String> {
    let mut stmt = conn
        .prepare(
            "SELECT shot_id FROM virtual_shots
             WHERE clip_id = ?1 AND kind != 'import_root'",
        )
        .map_err(|e| e.to_string())?;
    let ids: Vec<String> = stmt
        .query_map(params![clip_id], |r| r.get(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let mut max = 0u32;
    for id in ids {
        if let Some(n) = parse_shot_index(&id) {
            max = max.max(n);
        }
    }
    Ok(max + 1)
}

fn parse_shot_index(shot_id: &str) -> Option<u32> {
    let rest = shot_id.rsplit_once("_shot_")?.1;
    rest.parse::<u32>().ok()
}

fn backfill_frame_fields(
    paths: &ProjectPaths,
    project_id: &str,
    conn: &Connection,
) -> Result<(), String> {
    let mut stmt = conn
        .prepare(
            "SELECT shot_id, clip_id, in_seconds, out_seconds, fps, source_fps
             FROM virtual_shots
             WHERE duration_frames = 0 OR duration_label = '' OR duration_color_key = '' OR out_frame = 0",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, f64>(4)?,
                r.get::<_, f64>(5)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    for (shot_id, clip_id, in_sec, out_sec, _fps_raw, source_fps_raw) in rows {
        let fps = if is_valid_fps(source_fps_raw) {
            source_fps_raw
        } else {
            fps_for_clip(paths, project_id, &clip_id)
                .ok()
                .filter(|fps| is_valid_fps(*fps))
                .unwrap_or(0.0)
        };
        if !is_valid_fps(fps) {
            continue;
        }
        let in_frame = seconds_to_frame(in_sec, fps);
        let out_frame = seconds_to_frame(out_sec, fps).max(in_frame);
        let frames = (out_frame - in_frame).max(0);
        let label = seconds_frames_label_from_frames(frames, fps);
        let color_key = duration_color_key_from_frames(frames, fps);
        conn.execute(
            "UPDATE virtual_shots
             SET fps = ?1, source_fps = ?1, in_frame = ?2, out_frame = ?3,
                 duration_frames = ?4, duration_label = ?5, duration_color_key = ?6
             WHERE shot_id = ?7",
            params![fps, in_frame, out_frame, frames, label, color_key, shot_id],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn backfill_dual_fps(
    paths: &ProjectPaths,
    project_id: &str,
    conn: &Connection,
) -> Result<(), String> {
    if !column_exists(conn, "virtual_shots", "timeline_duration_frames") {
        return Ok(());
    }
    let mut stmt = conn
        .prepare(
            "SELECT shot_id, clip_id, in_frame, out_frame, fps, source_fps, timeline_fps
             FROM virtual_shots
             WHERE timeline_duration_frames = 0 AND out_frame > in_frame",
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
                r.get::<_, f64>(5)?,
                r.get::<_, f64>(6)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    for (shot_id, clip_id, in_frame, out_frame, _fps, source_fps_raw, timeline_fps_raw) in rows {
        let source_fps = if is_valid_fps(source_fps_raw) {
            source_fps_raw
        } else {
            fps_for_clip(paths, project_id, &clip_id)
                .ok()
                .filter(|fps| is_valid_fps(*fps))
                .unwrap_or(0.0)
        };
        if !is_valid_fps(source_fps) {
            continue;
        }
        let stored_timeline_fps = if is_valid_fps(timeline_fps_raw) {
            timeline_fps_raw
        } else {
            source_fps
        };
        let dual = dual_fps_snapshot(in_frame, out_frame, source_fps, stored_timeline_fps);
        conn.execute(
            "UPDATE virtual_shots
             SET fps = ?1, source_fps = ?1, timeline_fps = ?2, timeline_duration_frames = ?3
             WHERE shot_id = ?4",
            params![
                dual.source_fps,
                dual.timeline_fps,
                dual.timeline_duration_frames,
                shot_id
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn dual_fps_for_virtual_shot(
    _paths: &ProjectPaths,
    _project_id: &str,
    in_frame: i64,
    out_frame: i64,
    source_fps: f64,
) -> DualFpsSnapshot {
    dual_fps_snapshot(in_frame, out_frame, source_fps, source_fps)
}

/// Persist rational source/timeline FPS (broadcast truth) for a shot.
fn set_shot_rational_fps(
    conn: &Connection,
    shot_id: &str,
    source_fps: f64,
    timeline_fps: f64,
) -> Result<(), String> {
    let (s_num, s_den) = rational_fps(source_fps);
    let (t_num, t_den) = rational_fps(timeline_fps);
    conn.execute(
        "UPDATE virtual_shots
         SET source_fps_num = ?1, source_fps_den = ?2,
             timeline_fps_num = ?3, timeline_fps_den = ?4
         WHERE shot_id = ?5",
        params![s_num, s_den, t_num, t_den, shot_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn fps_for_clip(paths: &ProjectPaths, project_id: &str, clip_id: &str) -> Result<f64, String> {
    resolve_clip_fps(paths, project_id, clip_id)
}

#[derive(Debug, Clone, Default)]
struct ClipProbeMeta {
    field_order: String,
    interlaced: bool,
    source_class: String,
    proxy_recipe: String,
}

fn probe_meta_for_clip(conn: &Connection, clip_id: &str) -> ClipProbeMeta {
    conn.query_row(
        "SELECT COALESCE(field_order, ''), COALESCE(interlaced, 0),
                COALESCE(source_class, ''), COALESCE(proxy_recipe, '')
         FROM ingest_assets
         WHERE clip_id = ?1
         ORDER BY CASE import_status WHEN 'imported' THEN 0 WHEN 'done' THEN 1 ELSE 2 END,
                  CASE WHEN TRIM(COALESCE(project_proxy_path, '')) != '' THEN 0 ELSE 1 END,
                  source_id
         LIMIT 1",
        params![clip_id],
        |row| {
            Ok(ClipProbeMeta {
                field_order: row.get(0)?,
                interlaced: row.get::<_, i64>(1)? != 0,
                source_class: row.get(2)?,
                proxy_recipe: row.get(3)?,
            })
        },
    )
    .unwrap_or_default()
}

fn sync_virtual_shot_probe_meta(conn: &Connection) -> Result<(), String> {
    if !table_exists(conn, "ingest_assets")
        || !column_exists(conn, "ingest_assets", "field_order")
        || !column_exists(conn, "ingest_assets", "interlaced")
        || !column_exists(conn, "ingest_assets", "source_class")
        || !column_exists(conn, "ingest_assets", "proxy_recipe")
    {
        return Ok(());
    }
    let mut stmt = conn
        .prepare(
            "SELECT clip_id, COALESCE(field_order, ''), COALESCE(interlaced, 0),
                    COALESCE(source_class, ''), COALESCE(proxy_recipe, '')
             FROM ingest_assets
             WHERE TRIM(COALESCE(clip_id, '')) != ''
             ORDER BY clip_id,
                      CASE import_status WHEN 'imported' THEN 0 WHEN 'done' THEN 1 ELSE 2 END,
                      CASE WHEN TRIM(COALESCE(project_proxy_path, '')) != '' THEN 0 ELSE 1 END,
                      source_id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                ClipProbeMeta {
                    field_order: row.get(1)?,
                    interlaced: row.get::<_, i64>(2)? != 0,
                    source_class: row.get(3)?,
                    proxy_recipe: row.get(4)?,
                },
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let mut by_clip = HashMap::new();
    for (clip_id, meta) in rows {
        by_clip.entry(clip_id.trim().to_string()).or_insert(meta);
    }
    for (clip_id, meta) in by_clip {
        if clip_id.is_empty() {
            continue;
        }
        conn.execute(
            "UPDATE virtual_shots SET
                field_order = CASE WHEN TRIM(?2) != '' THEN ?2 ELSE field_order END,
                interlaced = CASE WHEN TRIM(?2) != '' THEN ?3 ELSE interlaced END,
                source_class = CASE WHEN TRIM(?4) != '' THEN ?4 ELSE source_class END,
                proxy_recipe = CASE WHEN TRIM(?5) != '' THEN ?5 ELSE proxy_recipe END
             WHERE clip_id = ?1",
            params![
                clip_id,
                meta.field_order,
                if meta.interlaced { 1 } else { 0 },
                meta.source_class,
                meta.proxy_recipe,
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn list_virtual_shots(paths: &ProjectPaths, project_id: &str) -> Result<Vec<Value>, String> {
    let conn = open(paths, project_id)?;
    let mut stmt = conn
        .prepare(
            "SELECT shot_id, clip_id, source, quality, duration_seconds, in_seconds, out_seconds,
                    cover_path, out_cover_path, in_tc, out_tc, description, category_key,
                    fps, source_fps, timeline_fps, in_frame, out_frame, duration_frames,
                    timeline_duration_frames, duration_label, duration_color_key, created_at,
                    kind, source_shot_id, locked, display_name, virtual_name,
                    source_fps_num, source_fps_den, timeline_fps_num, timeline_fps_den,
                    COALESCE(field_order, ''), COALESCE(interlaced, 0),
                    COALESCE(source_class, ''), COALESCE(proxy_recipe, '')
             FROM virtual_shots ORDER BY created_at",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            let _fps_raw: f64 = row.get(13)?;
            let source_fps_raw: f64 = row.get(14)?;
            let timeline_fps_raw: f64 = row.get(15)?;
            let clip_id: String = row.get(1)?;
            let source_fps = if is_valid_fps(source_fps_raw) {
                source_fps_raw
            } else {
                fps_for_clip(paths, project_id, &clip_id)
                    .ok()
                    .filter(|fps| is_valid_fps(*fps))
                    .unwrap_or(0.0)
            };
            let timeline_fps = if is_valid_fps(timeline_fps_raw) {
                timeline_fps_raw
            } else {
                source_fps
            };
            let timeline_duration_frames: i64 = row.get(19)?;
            let in_frame: i64 = row.get(16)?;
            let out_frame: i64 = row.get(17)?;
            let timeline_duration_frames = if timeline_duration_frames > 0 {
                timeline_duration_frames
            } else if is_valid_fps(source_fps) && is_valid_fps(timeline_fps) {
                dual_fps_snapshot(in_frame, out_frame, source_fps, timeline_fps)
                    .timeline_duration_frames
            } else {
                0
            };
            let stored_probe = ClipProbeMeta {
                field_order: row.get(32)?,
                interlaced: row.get::<_, i64>(33)? != 0,
                source_class: row.get(34)?,
                proxy_recipe: row.get(35)?,
            };
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "shot_id": row.get::<_, String>(0)?,
                "clip_id": clip_id,
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
                "fps": source_fps,
                "source_fps": source_fps,
                "timeline_fps": timeline_fps,
                "in_frame": in_frame,
                "out_frame": out_frame,
                "duration_frames": row.get::<_, i64>(18)?,
                "timeline_duration_frames": timeline_duration_frames,
                "duration_label": row.get::<_, String>(20)?,
                "duration_color_key": row.get::<_, String>(21)?,
                "created_at": row.get::<_, Option<String>>(22)?,
                "kind": row.get::<_, String>(23)?,
                "source_shot_id": row.get::<_, String>(24)?,
                "locked": row.get::<_, i64>(25)? != 0,
                "display_name": row.get::<_, String>(26)?,
                "virtual_name": row.get::<_, String>(27)?,
                "source_in_frame": in_frame,
                "source_out_frame": out_frame,
                "source_fps_num": row.get::<_, i64>(28)?,
                "source_fps_den": row.get::<_, i64>(29)?,
                "timeline_fps_num": row.get::<_, i64>(30)?,
                "timeline_fps_den": row.get::<_, i64>(31)?,
                "field_order": stored_probe.field_order,
                "interlaced": stored_probe.interlaced,
                "source_class": stored_probe.source_class,
                "proxy_recipe": stored_probe.proxy_recipe,
            }))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

pub fn add_virtual_shot(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    in_seconds: f64,
    out_seconds: f64,
) -> Result<Value, String> {
    let fps = fps_for_clip(paths, project_id, clip_id)?;
    let in_frame = seconds_to_frame(snap_seconds_to_frame(in_seconds.max(0.0), fps), fps);
    let out_frame = seconds_to_frame(snap_seconds_to_frame(out_seconds.max(0.0), fps), fps);
    add_virtual_shot_from_frames(paths, project_id, clip_id, in_frame, out_frame)
}

pub fn add_virtual_shot_from_frames(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    in_frame: i64,
    out_frame: i64,
) -> Result<Value, String> {
    if proxy_path_for_clip(paths, project_id, clip_id).is_none() {
        return Err(format!("Klip '{clip_id}' nije uvezen u ingest"));
    }
    let fps = fps_for_clip(paths, project_id, clip_id)?;
    let in_frame = in_frame.max(0);
    if out_frame <= in_frame {
        return Err("OUT mora biti najmanje jedan frame nakon IN".into());
    }
    let out_frame = out_frame.max(in_frame + 1);
    let in_r = round3(frame_to_seconds(in_frame, fps));
    let out_r = round3(frame_to_seconds(out_frame, fps));
    let duration = round3(out_r - in_r);
    let duration_frames = (out_frame - in_frame).max(0);
    let dual = dual_fps_for_virtual_shot(paths, project_id, in_frame, out_frame, fps);
    let duration_label = seconds_frames_label_from_frames(duration_frames, fps);
    let duration_color_key = duration_color_key_from_frames(duration_frames, fps);
    let conn = open(paths, project_id)?;
    let index = next_derived_shot_index(&conn, clip_id)?;
    let shot_id = derived_shot_id(clip_id, index);
    let root_virtual_name: String = conn
        .query_row(
            "SELECT virtual_name FROM virtual_shots
             WHERE clip_id = ?1 AND kind = 'import_root' AND TRIM(COALESCE(virtual_name, '')) != ''
             LIMIT 1",
            params![clip_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .unwrap_or_else(|| {
            conn.query_row(
                "SELECT virtual_name FROM ingest_assets
                 WHERE clip_id = ?1 AND TRIM(COALESCE(virtual_name, '')) != ''
                 LIMIT 1",
                params![clip_id],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| virtual_name_for_root_clip(clip_id, ""))
        });
    let virtual_name = virtual_name_for_derived_shot(&root_virtual_name, index);
    let now = now_str();
    let (cover, out_cover) = write_shot_covers(paths, project_id, &shot_id, clip_id, in_r, out_r)?;
    let in_tc = seconds_to_timecode(in_r, fps);
    let out_tc = seconds_to_timecode(out_r, fps);
    let description = "Ručno označen virtualni kadar.";
    let category_key = "manual_cut";
    let probe = probe_meta_for_clip(&conn, clip_id);
    conn.execute(
        "INSERT INTO virtual_shots
            (shot_id, clip_id, source, quality, duration_seconds, in_seconds, out_seconds,
             fps, source_fps, timeline_fps, in_frame, out_frame, duration_frames,
             timeline_duration_frames, duration_label, duration_color_key,
             cover_path, out_cover_path, in_tc, out_tc, description, category_key,
             display_name, virtual_name, kind, field_order, interlaced, source_class,
             proxy_recipe, data_json, created_at, updated_at)
         VALUES (?1, ?2, 'manual', 'ok', ?3, ?4, ?5, ?6, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                 ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, 'derived', ?22, ?23, ?24, ?25,
                 '{}', ?26, ?26)",
        params![
            shot_id,
            clip_id,
            duration,
            in_r,
            out_r,
            dual.source_fps,
            dual.timeline_fps,
            in_frame,
            out_frame,
            duration_frames,
            dual.timeline_duration_frames,
            duration_label,
            duration_color_key,
            cover.to_string_lossy(),
            out_cover.to_string_lossy(),
            in_tc,
            out_tc,
            description,
            category_key,
            virtual_name.clone(),
            virtual_name,
            probe.field_order,
            if probe.interlaced { 1 } else { 0 },
            probe.source_class,
            probe.proxy_recipe,
            now,
        ],
    )
    .map_err(|e| e.to_string())?;
    set_shot_rational_fps(&conn, &shot_id, dual.source_fps, dual.timeline_fps)?;
    Ok(json!({
        "id": shot_id,
        "shot_id": shot_id,
        "clip_id": clip_id,
        "in_seconds": in_r,
        "out_seconds": out_r,
        "duration_seconds": duration,
        "fps": dual.source_fps,
        "source_fps": dual.source_fps,
        "timeline_fps": dual.timeline_fps,
        "in_frame": in_frame,
        "out_frame": out_frame,
        "duration_frames": duration_frames,
        "timeline_duration_frames": dual.timeline_duration_frames,
        "duration_label": duration_label,
        "duration_color_key": duration_color_key,
        "source": "manual",
        "quality": "ok",
        "cover_path": cover.to_string_lossy(),
        "out_cover_path": out_cover.to_string_lossy(),
        "in_tc": in_tc,
        "out_tc": out_tc,
        "virtual_name": virtual_name,
        "field_order": probe.field_order,
        "interlaced": probe.interlaced,
        "source_class": probe.source_class,
        "proxy_recipe": probe.proxy_recipe,
    }))
}

fn root_virtual_name_for_clip(conn: &Connection, clip_id: &str) -> String {
    let ext: String = conn
        .query_row(
            "SELECT file_extension FROM ingest_assets
             WHERE clip_id = ?1 LIMIT 1",
            params![clip_id],
            |r| r.get(0),
        )
        .unwrap_or_default();
    let name = virtual_name_for_root_clip(clip_id, &ext);
    if !name.is_empty() {
        return name;
    }
    virtual_name_for_root_clip(clip_id, "")
}

fn root_shot_is_finalized(conn: &Connection, shot_id: &str) -> Result<bool, String> {
    let row: Option<(f64, String)> = conn
        .query_row(
            "SELECT duration_seconds, quality FROM virtual_shots WHERE shot_id = ?1",
            params![shot_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(row
        .map(|(duration, quality)| duration > 0.0 && quality == "ok")
        .unwrap_or(false))
}

/// Reserve `import_root` identity at ingest selection — before import starts.
fn reserve_root_virtual_shot(
    conn: &Connection,
    clip_id: &str,
    virtual_name: &str,
    display_name: &str,
) -> Result<(), String> {
    let shot_id = root_shot_id(clip_id);
    let probe = probe_meta_for_clip(conn, clip_id);
    if root_shot_is_finalized(conn, &shot_id)? {
        if !virtual_name.trim().is_empty() {
            let now = now_str();
            let _ = conn.execute(
                "UPDATE virtual_shots SET virtual_name = ?2, updated_at = ?3
                 WHERE shot_id = ?1 AND TRIM(COALESCE(virtual_name, '')) = ''",
                params![shot_id, virtual_name.trim(), now],
            );
        }
        return Ok(());
    }
    let now = now_str();
    let vname = virtual_name.trim();
    let dname = display_name.trim();
    let dname = if dname.is_empty() { clip_id } else { dname };
    conn.execute(
        "INSERT INTO virtual_shots
            (shot_id, clip_id, kind, source_shot_id, locked, display_name, virtual_name,
             source, quality, description, category_key, field_order, interlaced,
             source_class, proxy_recipe, data_json, created_at, updated_at)
         VALUES (?1, ?2, 'import_root', '', 1, ?3, ?4,
                 'import', 'pending', 'Rezervirani root virtualni kadar (čeka uvoz).', 'import_root',
                 ?5, ?6, ?7, ?8, '{}', ?9, ?9)
         ON CONFLICT(shot_id) DO UPDATE SET
            display_name = CASE
                WHEN TRIM(excluded.display_name) != '' THEN excluded.display_name
                ELSE virtual_shots.display_name
            END,
            virtual_name = CASE
                WHEN TRIM(excluded.virtual_name) != '' THEN excluded.virtual_name
                ELSE virtual_shots.virtual_name
            END,
            kind = 'import_root',
            field_order = CASE WHEN TRIM(excluded.field_order) != '' THEN excluded.field_order ELSE virtual_shots.field_order END,
            interlaced = CASE WHEN excluded.interlaced != 0 THEN excluded.interlaced ELSE virtual_shots.interlaced END,
            source_class = CASE WHEN TRIM(excluded.source_class) != '' THEN excluded.source_class ELSE virtual_shots.source_class END,
            proxy_recipe = CASE WHEN TRIM(excluded.proxy_recipe) != '' THEN excluded.proxy_recipe ELSE virtual_shots.proxy_recipe END,
            updated_at = excluded.updated_at
         WHERE virtual_shots.duration_seconds <= 0 OR virtual_shots.quality = 'pending'",
        params![
            shot_id,
            clip_id,
            dname,
            vname,
            probe.field_order,
            if probe.interlaced { 1 } else { 0 },
            probe.source_class,
            probe.proxy_recipe,
            now
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Create or refresh reserved `import_root` rows for every selected ingest clip on one source.
pub fn ensure_reserved_root_shots_for_selected(
    paths: &ProjectPaths,
    project_id: &str,
    source_id: &str,
) -> Result<usize, String> {
    ensure_reserved_root_shots(paths, project_id, Some(source_id.trim()))
}

/// Select/deselect sync: rezerviraj za selected, ukloni pending root za deselected.
pub fn sync_root_virtual_shots_with_selection(
    paths: &ProjectPaths,
    project_id: &str,
    source_id: &str,
) -> Result<usize, String> {
    let reserved = ensure_reserved_root_shots_for_selected(paths, project_id, source_id)?;
    let released = release_deselected_pending_root_shots(paths, project_id, source_id)?;
    Ok(reserved.saturating_add(released))
}

/// Ukloni samo pending (nefinalizirane) import_root za klipove koji više nisu selected.
fn release_deselected_pending_root_shots(
    paths: &ProjectPaths,
    project_id: &str,
    source_id: &str,
) -> Result<usize, String> {
    let conn = open(paths, project_id)?;
    let sid = source_id.trim();
    let removed = if sid.is_empty() {
        conn.execute(
            "DELETE FROM virtual_shots
             WHERE kind = 'import_root'
               AND quality = 'pending'
               AND (duration_seconds IS NULL OR duration_seconds <= 0)
               AND clip_id NOT IN (
                    SELECT clip_id FROM ingest_assets WHERE selected != 0
               )",
            [],
        )
        .map_err(|e| e.to_string())?
    } else {
        conn.execute(
            "DELETE FROM virtual_shots
             WHERE kind = 'import_root'
               AND quality = 'pending'
               AND (duration_seconds IS NULL OR duration_seconds <= 0)
               AND clip_id NOT IN (
                    SELECT clip_id FROM ingest_assets
                    WHERE source_id = ?1 AND selected != 0
               )",
            params![sid],
        )
        .map_err(|e| e.to_string())?
    };
    Ok(removed)
}

/// Create or refresh reserved `import_root` rows for all selected ingest clips in the project.
pub fn ensure_reserved_root_shots_for_project(
    paths: &ProjectPaths,
    project_id: &str,
) -> Result<usize, String> {
    ensure_reserved_root_shots(paths, project_id, None)
}

fn ensure_reserved_root_shots(
    paths: &ProjectPaths,
    project_id: &str,
    source_id: Option<&str>,
) -> Result<usize, String> {
    let conn = open(paths, project_id)?;
    let (sql, bind_source): (&str, Option<&str>) = match source_id.filter(|s| !s.is_empty()) {
        Some(sid) => (
            "SELECT clip_id, name, virtual_name, file_extension FROM ingest_assets
             WHERE source_id = ?1 AND selected != 0",
            Some(sid),
        ),
        None => (
            "SELECT clip_id, name, virtual_name, file_extension FROM ingest_assets
             WHERE selected != 0",
            None,
        ),
    };
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let rows: Vec<(String, String, String, String)> = if let Some(sid) = bind_source {
        stmt.query_map(params![sid], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?
    } else {
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?
    };
    let mut count = 0usize;
    for (clip_id, name, _virtual_name, ext) in rows {
        let vname = virtual_name_for_root_clip(&clip_id, &ext);
        if vname.trim().is_empty() {
            continue;
        }
        reserve_root_virtual_shot(&conn, &clip_id, &vname, &name)?;
        count += 1;
    }
    Ok(count)
}

fn finalize_root_virtual_shot(
    conn: &Connection,
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    clip: &Value,
    virtual_name: &str,
) -> Result<Value, String> {
    let shot_id = root_shot_id(clip_id);
    let display_name = clip
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(clip_id)
        .to_string();
    let duration_raw = clip
        .get("duration_sec")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let fps = fps_for_clip(paths, project_id, clip_id)?;
    let out_r = round3(snap_seconds_to_frame(duration_raw.max(1.0 / fps), fps));
    let in_frame = 0;
    let out_frame = seconds_to_frame(out_r, fps);
    let duration_frames = (out_frame - in_frame).max(0);
    let dual = dual_fps_for_virtual_shot(paths, project_id, in_frame, out_frame, fps);
    let duration = round3(out_r);
    let duration_label = seconds_frames_label_from_frames(duration_frames, fps);
    let duration_color_key = duration_color_key_from_frames(duration_frames, fps);
    let in_tc = seconds_to_timecode(0.0, fps);
    let out_tc = seconds_to_timecode(out_r, fps);
    let probe = probe_meta_for_clip(conn, clip_id);
    let now = now_str();
    let exists = conn
        .query_row(
            "SELECT 1 FROM virtual_shots WHERE shot_id = ?1",
            params![shot_id],
            |_| Ok(()),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .is_some();
    if exists {
        conn.execute(
            "UPDATE virtual_shots SET
                clip_id = ?2, kind = 'import_root', locked = 1,
                display_name = ?3, virtual_name = ?4,
                source = 'import', quality = 'ok',
                duration_seconds = ?5, in_seconds = 0, out_seconds = ?6,
                fps = ?7, source_fps = ?7, timeline_fps = ?8,
                in_frame = 0, out_frame = ?9, duration_frames = ?10,
                timeline_duration_frames = ?11, duration_label = ?12, duration_color_key = ?13,
                in_tc = ?14, out_tc = ?15,
                description = 'Originalni uvezeni kadar.', category_key = 'import_root',
                field_order = ?16, interlaced = ?17, source_class = ?18, proxy_recipe = ?19,
                updated_at = ?20
             WHERE shot_id = ?1",
            params![
                shot_id,
                clip_id,
                display_name,
                virtual_name,
                duration,
                out_r,
                dual.source_fps,
                dual.timeline_fps,
                out_frame,
                duration_frames,
                dual.timeline_duration_frames,
                duration_label,
                duration_color_key,
                in_tc,
                out_tc,
                probe.field_order,
                if probe.interlaced { 1 } else { 0 },
                probe.source_class,
                probe.proxy_recipe,
                now,
            ],
        )
        .map_err(|e| e.to_string())?;
    } else {
        conn.execute(
            "INSERT INTO virtual_shots
                (shot_id, clip_id, kind, source_shot_id, locked, display_name, virtual_name,
                 source, quality, duration_seconds, in_seconds, out_seconds,
                 fps, source_fps, timeline_fps, in_frame, out_frame, duration_frames,
                 timeline_duration_frames, duration_label, duration_color_key,
                 cover_path, out_cover_path, in_tc, out_tc, description, category_key,
                 field_order, interlaced, source_class, proxy_recipe, data_json,
                 created_at, updated_at)
             VALUES (?1, ?2, 'import_root', '', 1, ?3, ?4,
                     'import', 'ok', ?5, 0, ?6,
                     ?7, ?7, ?8, 0, ?9, ?10,
                     ?11, ?12, ?13,
                     '', '', ?14, ?15, 'Originalni uvezeni kadar.', 'import_root',
                     ?16, ?17, ?18, ?19, '{}',
                     ?20, ?20)",
            params![
                shot_id,
                clip_id,
                display_name,
                virtual_name,
                duration,
                out_r,
                dual.source_fps,
                dual.timeline_fps,
                out_frame,
                duration_frames,
                dual.timeline_duration_frames,
                duration_label,
                duration_color_key,
                in_tc,
                out_tc,
                probe.field_order,
                if probe.interlaced { 1 } else { 0 },
                probe.source_class,
                probe.proxy_recipe,
                now,
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    set_shot_rational_fps(conn, &shot_id, dual.source_fps, dual.timeline_fps)?;
    Ok(json!({
        "shot_id": shot_id,
        "kind": "import_root",
        "clip_id": clip_id,
        "virtual_name": virtual_name,
        "field_order": probe.field_order,
        "interlaced": probe.interlaced,
        "source_class": probe.source_class,
        "proxy_recipe": probe.proxy_recipe,
    }))
}

/// Root virtual shot for one imported clip: full source range, read-only.
/// Idempotent — `{clip_id}_root`; finalizes a selection-time reservation after import.
pub fn create_root_virtual_shot(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
) -> Result<Value, String> {
    let shot_id = root_shot_id(clip_id);
    let conn = open(paths, project_id)?;
    let virtual_name = root_virtual_name_for_clip(&conn, clip_id);
    if root_shot_is_finalized(&conn, &shot_id)? {
        if !virtual_name.is_empty() {
            let _ = conn.execute(
                "UPDATE virtual_shots SET virtual_name = ?2
                 WHERE shot_id = ?1 AND TRIM(COALESCE(virtual_name, '')) = ''",
                params![shot_id, virtual_name],
            );
        }
        return Ok(json!({ "shot_id": shot_id, "kind": "import_root", "skipped": true }));
    }
    let clip = read_imported_clips(paths, project_id)?
        .into_iter()
        .find(|c| c.get("clip_id").and_then(Value::as_str) == Some(clip_id))
        .ok_or_else(|| format!("Klip '{clip_id}' nije uvezen u ingest"))?;
    finalize_root_virtual_shot(&conn, paths, project_id, clip_id, &clip, &virtual_name)
}

/// Ensure every imported clip has a root virtual shot. Idempotent; safe at project open.
pub fn ensure_root_virtual_shots(paths: &ProjectPaths, project_id: &str) -> Result<(), String> {
    // Guarantee the virtual_shots schema/migration is applied before QStory reads it,
    // even for projects with no imported clips yet.
    open(paths, project_id)?;
    for clip in read_imported_clips(paths, project_id)? {
        if let Some(clip_id) = clip.get("clip_id").and_then(Value::as_str) {
            // Skip clips without usable fps; they get a root once metadata is ready.
            if let Err(err) = create_root_virtual_shot(paths, project_id, clip_id) {
                tracing::warn!(
                    "root virtual shot skipped: project={project_id} clip={clip_id} err={err}"
                );
                continue;
            }
        }
    }
    Ok(())
}

/// Resolve a virtual shot's absolute SOURCE frame range (broadcast truth):
/// (clip_id, source_in_frame, source_out_frame, source_fps). Frames come straight
/// from the stored row — no seconds*fps recomputation.
pub fn virtual_shot_frames(
    paths: &ProjectPaths,
    project_id: &str,
    shot_id: &str,
) -> Result<(String, i64, i64, f64), String> {
    let conn = open(paths, project_id)?;
    let (clip_id, in_frame, out_frame, source_fps): (String, i64, i64, f64) = conn
        .query_row(
            "SELECT clip_id, in_frame, out_frame, source_fps
             FROM virtual_shots WHERE shot_id = ?1",
            params![shot_id.trim()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(|_| format!("Virtualni kadar '{}' nije pronađen", shot_id.trim()))?;
    let fps = if source_fps.is_finite() && source_fps > 0.0 {
        normalize_fps(source_fps)
    } else {
        fps_for_clip(paths, project_id, &clip_id)?
    };
    let out_frame = out_frame.max(in_frame + 1);
    Ok((clip_id, in_frame.max(0), out_frame, fps))
}

/// New editorial cut: a derived shot from an existing virtual shot.
/// `local_*_seconds` are relative to the source shot's own IN.
pub fn derive_virtual_shot(
    paths: &ProjectPaths,
    project_id: &str,
    source_shot_id: &str,
    local_in_seconds: f64,
    local_out_seconds: f64,
) -> Result<Value, String> {
    let (_, _, _, fps) = virtual_shot_frames(paths, project_id, source_shot_id)?;
    let local_in_frame =
        seconds_to_frame(snap_seconds_to_frame(local_in_seconds.max(0.0), fps), fps);
    let local_out_frame =
        seconds_to_frame(snap_seconds_to_frame(local_out_seconds.max(0.0), fps), fps);
    derive_virtual_shot_from_frames(
        paths,
        project_id,
        source_shot_id,
        local_in_frame,
        local_out_frame,
    )
}

pub fn derive_virtual_shot_from_frames(
    paths: &ProjectPaths,
    project_id: &str,
    source_shot_id: &str,
    local_in_frame: i64,
    local_out_frame: i64,
) -> Result<Value, String> {
    let source_shot_id = source_shot_id.trim();
    let conn = open(paths, project_id)?;
    let (clip_id, src_in_frame): (String, i64) = conn
        .query_row(
            "SELECT clip_id, in_frame FROM virtual_shots WHERE shot_id = ?1",
            params![source_shot_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|_| format!("Izvorni kadar '{source_shot_id}' nije pronađen"))?;
    let abs_in = src_in_frame.max(0) + local_in_frame.max(0);
    let abs_out = src_in_frame.max(0) + local_out_frame.max(0);
    let created = add_virtual_shot_from_frames(paths, project_id, &clip_id, abs_in, abs_out)?;
    let new_shot_id = created
        .get("shot_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    conn.execute(
        "UPDATE virtual_shots SET source_shot_id = ?1, kind = 'derived' WHERE shot_id = ?2",
        params![source_shot_id, new_shot_id],
    )
    .map_err(|e| e.to_string())?;
    let mut out = created;
    if let Value::Object(ref mut map) = out {
        map.insert("source_shot_id".into(), json!(source_shot_id));
        map.insert("kind".into(), json!("derived"));
    }
    Ok(out)
}

pub fn update_virtual_shot(
    paths: &ProjectPaths,
    project_id: &str,
    shot_id: &str,
    in_seconds: f64,
    out_seconds: f64,
) -> Result<Value, String> {
    let conn = open(paths, project_id)?;
    let clip_id: String = conn
        .query_row(
            "SELECT clip_id FROM virtual_shots WHERE shot_id = ?1",
            params![shot_id.trim()],
            |row| row.get(0),
        )
        .map_err(|_| format!("Virtualni kadar '{}' nije pronađen", shot_id.trim()))?;
    let fps = fps_for_clip(paths, project_id, &clip_id)?;
    let in_frame = seconds_to_frame(snap_seconds_to_frame(in_seconds.max(0.0), fps), fps);
    let out_frame = seconds_to_frame(snap_seconds_to_frame(out_seconds.max(0.0), fps), fps);
    update_virtual_shot_from_frames(paths, project_id, shot_id, in_frame, out_frame)
}

pub fn update_virtual_shot_from_frames(
    paths: &ProjectPaths,
    project_id: &str,
    shot_id: &str,
    in_frame: i64,
    out_frame: i64,
) -> Result<Value, String> {
    let conn = open(paths, project_id)?;
    let (clip_id, locked): (String, i64) = conn
        .query_row(
            "SELECT clip_id, locked FROM virtual_shots WHERE shot_id = ?1",
            params![shot_id.trim()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|_| format!("Virtualni kadar '{}' nije pronađen", shot_id.trim()))?;
    if locked != 0 {
        return Err("Originalni (root) kadar je read-only.".into());
    }
    let fps = fps_for_clip(paths, project_id, &clip_id)?;
    let in_frame = in_frame.max(0);
    if out_frame <= in_frame {
        return Err("OUT mora biti najmanje jedan frame nakon IN".into());
    }
    let out_frame = out_frame.max(in_frame + 1);
    let in_r = round3(frame_to_seconds(in_frame, fps));
    let out_r = round3(frame_to_seconds(out_frame, fps));
    let duration_frames = out_frame - in_frame;
    let dual = dual_fps_for_virtual_shot(paths, project_id, in_frame, out_frame, fps);
    let duration = round3(out_r - in_r);
    let duration_label = seconds_frames_label_from_frames(duration_frames, fps);
    let duration_color_key = duration_color_key_from_frames(duration_frames, fps);
    let (cover, out_cover) = write_shot_covers(paths, project_id, shot_id, &clip_id, in_r, out_r)?;
    let in_tc = seconds_to_timecode(in_r, fps);
    let out_tc = seconds_to_timecode(out_r, fps);
    let probe = probe_meta_for_clip(&conn, &clip_id);
    conn.execute(
        "UPDATE virtual_shots
         SET duration_seconds = ?1, in_seconds = ?2, out_seconds = ?3,
             fps = ?4, source_fps = ?4, timeline_fps = ?5,
             in_frame = ?6, out_frame = ?7, duration_frames = ?8,
             timeline_duration_frames = ?9,
             duration_label = ?10, duration_color_key = ?11,
             cover_path = ?12, out_cover_path = ?13, in_tc = ?14, out_tc = ?15,
             field_order = ?16, interlaced = ?17, source_class = ?18, proxy_recipe = ?19,
             updated_at = ?20
         WHERE shot_id = ?21",
        params![
            duration,
            in_r,
            out_r,
            dual.source_fps,
            dual.timeline_fps,
            in_frame,
            out_frame,
            duration_frames,
            dual.timeline_duration_frames,
            duration_label,
            duration_color_key,
            cover.to_string_lossy(),
            out_cover.to_string_lossy(),
            in_tc,
            out_tc,
            probe.field_order,
            if probe.interlaced { 1 } else { 0 },
            probe.source_class,
            probe.proxy_recipe,
            now_str(),
            shot_id.trim(),
        ],
    )
    .map_err(|e| e.to_string())?;
    set_shot_rational_fps(&conn, shot_id.trim(), dual.source_fps, dual.timeline_fps)?;
    list_virtual_shots(paths, project_id)?
        .into_iter()
        .find(|shot| shot.get("id").and_then(Value::as_str) == Some(shot_id.trim()))
        .ok_or_else(|| "Ažurirani virtualni kadar nije pronađen".into())
}

// --- Cover artifacts (per shot) -------------------------------------------------

fn virtual_shots_root(paths: &ProjectPaths, project_id: &str) -> PathBuf {
    paths.project_dir(project_id).join("virtual_shots")
}

fn shot_dir(paths: &ProjectPaths, project_id: &str, shot_id: &str) -> PathBuf {
    virtual_shots_root(paths, project_id).join(shot_id)
}

fn cover_file(shot_dir: &Path, kind: &str) -> PathBuf {
    if kind == "out" || kind == "out_cover" || kind == "end" {
        shot_dir.join("out_cover.jpg")
    } else {
        shot_dir.join("cover.jpg")
    }
}

fn bounded_seek(source: &Path, seek_sec: f64) -> f64 {
    let seek = seek_sec.max(0.0);
    if let Some(duration) = media_duration_sec(source) {
        if duration > 0.08 {
            return seek.min((duration - 0.04).max(0.0));
        }
    }
    seek
}

fn extract_cover_with_retries(
    source: &Path,
    dest: &Path,
    preferred_sec: f64,
    fallback_sec: f64,
) -> Result<(), String> {
    let mut last_err = String::new();
    let candidates = [
        bounded_seek(source, preferred_sec),
        bounded_seek(source, preferred_sec - 0.04),
        bounded_seek(source, preferred_sec - 0.12),
        bounded_seek(source, fallback_sec),
    ];
    let mut tried: Vec<i64> = Vec::new();
    for sec in candidates {
        let key = (sec * 1000.0).round() as i64;
        if tried.contains(&key) {
            continue;
        }
        tried.push(key);
        match extract_poster_jpeg_at_seek(source, dest, sec) {
            Ok(()) => return Ok(()),
            Err(err) => last_err = err,
        }
    }
    Err(last_err)
}

pub fn write_shot_covers(
    paths: &ProjectPaths,
    project_id: &str,
    shot_id: &str,
    clip_id: &str,
    in_sec: f64,
    out_sec: f64,
) -> Result<(PathBuf, PathBuf), String> {
    let proxy = proxy_path_for_clip(paths, project_id, clip_id)
        .filter(|p| p.is_file())
        .ok_or_else(|| format!("nema proxy za '{clip_id}'"))?;
    let dir = shot_dir(paths, project_id, shot_id);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let cover = cover_file(&dir, "in");
    let out_cover = cover_file(&dir, "out");
    if !cover.is_file() {
        extract_cover_with_retries(&proxy, &cover, in_sec, 0.0)?;
    }
    if !out_cover.is_file() {
        if let Err(out_err) = extract_cover_with_retries(&proxy, &out_cover, out_sec - 0.04, in_sec)
        {
            if cover.is_file() {
                fs::copy(&cover, &out_cover).map_err(|copy_err| {
                    format!("out cover ffmpeg: {out_err}; kopiranje IN covera: {copy_err}")
                })?;
            } else {
                return Err(out_err);
            }
        }
    }
    Ok((cover, out_cover))
}

pub fn cover_path_for_shot(
    paths: &ProjectPaths,
    project_id: &str,
    shot_id: &str,
    kind: &str,
) -> Result<Option<PathBuf>, String> {
    let conn = open(paths, project_id)?;
    let row: Option<(String, String, String, f64, f64)> = conn
        .query_row(
            "SELECT cover_path, out_cover_path, clip_id, in_seconds, out_seconds
             FROM virtual_shots WHERE shot_id = ?1",
            params![shot_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .ok();
    let Some((cover_raw, out_cover_raw, clip_id, in_sec, out_sec)) = row else {
        return Ok(None);
    };
    let want_out = kind == "out" || kind == "out_cover" || kind == "end";
    let stored = if want_out {
        out_cover_raw.trim()
    } else {
        cover_raw.trim()
    };
    if !stored.is_empty() {
        let path = PathBuf::from(stored);
        if path.is_file() {
            return Ok(Some(path));
        }
    }
    let fallback = cover_file(&shot_dir(paths, project_id, shot_id), kind);
    if fallback.is_file() {
        return Ok(Some(fallback));
    }

    // Reserved import_root često još nema cover JPEG — koristi ingest poster.
    if !clip_id.trim().is_empty() {
        if let Some(poster) =
            crate::ingest::db::resolve_ingest_poster_path(paths, project_id, clip_id.trim())
        {
            return Ok(Some(poster));
        }
    }

    if out_sec <= in_sec {
        return Ok(None);
    }
    let (cover, out_cover) =
        write_shot_covers(paths, project_id, shot_id, &clip_id, in_sec, out_sec)?;
    let cover_path = if want_out {
        out_cover.clone()
    } else {
        cover.clone()
    };
    conn.execute(
        "UPDATE virtual_shots
         SET cover_path = ?1, out_cover_path = ?2, updated_at = ?3
         WHERE shot_id = ?4",
        params![
            cover.to_string_lossy(),
            out_cover.to_string_lossy(),
            now_str(),
            shot_id,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(Some(cover_path))
}

#[allow(dead_code)]
pub fn delete_virtual_shot(
    paths: &ProjectPaths,
    project_id: &str,
    shot_id: &str,
) -> Result<bool, String> {
    let conn = open(paths, project_id)?;
    let deleted = conn
        .execute(
            "DELETE FROM virtual_shots WHERE shot_id = ?1",
            params![shot_id],
        )
        .map_err(|e| e.to_string())?;
    if deleted == 0 {
        return Ok(false);
    }
    let dir = shot_dir(paths, project_id, shot_id);
    if dir.is_dir() {
        let _ = fs::remove_dir_all(&dir);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn test_paths(base: &Path) -> ProjectPaths {
        ProjectPaths {
            data_dir: base.join("data"),
            projects_root: base.join("projects"),
            seed_path: base.join("seed.json"),
        }
    }

    #[test]
    fn ensure_reserved_root_shots_on_selected_before_import() {
        let base =
            std::env::temp_dir().join(format!("qnc_virtual_reserve_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let conn = crate::ingest::db::open_ingest(&paths, "reserve_proj").unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, file_extension, selected, import_status, status, virtual_name)
             VALUES ('local', 'mironik_1483', 'mironik_1483', 'mironik_1483', 'mxf', 1, 'detected', 'on_source', 'mironik_1483_root.mxf')",
            [],
        )
        .unwrap();
        drop(conn);

        let count =
            ensure_reserved_root_shots_for_selected(&paths, "reserve_proj", "local").unwrap();
        assert_eq!(count, 1);

        let conn = open(&paths, "reserve_proj").unwrap();
        let (kind, quality, virtual_name): (String, String, String) = conn
            .query_row(
                "SELECT kind, quality, virtual_name FROM virtual_shots WHERE shot_id = 'mironik_1483_root'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(kind, "import_root");
        assert_eq!(quality, "pending");
        assert_eq!(virtual_name, "mironik_1483_root.mxf");

        let duration: f64 = conn
            .query_row(
                "SELECT duration_seconds FROM virtual_shots WHERE shot_id = 'mironik_1483_root'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(duration, 0.0);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn sync_selection_releases_pending_root_on_deselect() {
        let base =
            std::env::temp_dir().join(format!("qnc_virtual_deselect_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let conn = crate::ingest::db::open_ingest(&paths, "deselect_proj").unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, file_extension, selected, import_status, status, virtual_name)
             VALUES ('local', 'clip_a', 'clip_a', 'clip_a', 'mxf', 1, 'detected', 'on_source', 'clip_a.mxf')",
            [],
        )
        .unwrap();
        drop(conn);

        sync_root_virtual_shots_with_selection(&paths, "deselect_proj", "local").unwrap();
        let conn = open(&paths, "deselect_proj").unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM virtual_shots WHERE shot_id = 'clip_a_root'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
        drop(conn);

        let conn = crate::ingest::db::open_ingest(&paths, "deselect_proj").unwrap();
        conn.execute(
            "UPDATE ingest_assets SET selected = 0 WHERE clip_id = 'clip_a'",
            [],
        )
        .unwrap();
        drop(conn);

        sync_root_virtual_shots_with_selection(&paths, "deselect_proj", "local").unwrap();
        let conn = open(&paths, "deselect_proj").unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM virtual_shots WHERE shot_id = 'clip_a_root'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);

        let _ = fs::remove_dir_all(&base);
    }
}
