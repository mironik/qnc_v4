use std::collections::HashSet;

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::frame_time::{frame_to_seconds, frame_to_timecode, normalize_fps, seconds_to_frame};
use crate::project::db::{now_str, open_project, ProjectPaths};

use super::ingest_db::read_imported_clips;

pub(crate) fn open_db(paths: &ProjectPaths, project_id: &str) -> Result<Connection, String> {
    let conn = open_project(paths, project_id).map_err(|e| e.to_string())?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS pool_clips (
            clip_id TEXT PRIMARY KEY,
            status TEXT NOT NULL DEFAULT 'active',
            added_at TEXT,
            updated_at TEXT
        );
        CREATE TABLE IF NOT EXISTS media_pool_displayed_clips (
            clip_id TEXT PRIMARY KEY,
            snapshot_signature TEXT NOT NULL DEFAULT '',
            displayed_at TEXT,
            updated_at TEXT
        );
        CREATE TABLE IF NOT EXISTS media_pool_workflow (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            current_clip_id TEXT NOT NULL DEFAULT '',
            playhead_frame INTEGER NOT NULL DEFAULT 0,
            playhead_sec REAL NOT NULL DEFAULT 0,
            playhead_tc TEXT NOT NULL DEFAULT '',
            playhead_fps REAL NOT NULL DEFAULT 25,
            mark_in_sec REAL,
            mark_out_sec REAL,
            active_virtual_shot_id TEXT NOT NULL DEFAULT '',
            state_json TEXT NOT NULL DEFAULT '{}',
            updated_at TEXT
        );
        CREATE TABLE IF NOT EXISTS media_pool_workflow_selection (
            clip_id TEXT PRIMARY KEY,
            added_at TEXT
        );
        CREATE TABLE IF NOT EXISTS clip_transcripts (
            clip_id TEXT PRIMARY KEY,
            status TEXT NOT NULL DEFAULT 'none',
            text_body TEXT NOT NULL DEFAULT '',
            transcript_json TEXT NOT NULL DEFAULT '{}',
            updated_at TEXT
        );
        CREATE TABLE IF NOT EXISTS clip_transcript_segments (
            clip_id TEXT NOT NULL,
            segment_index INTEGER NOT NULL,
            start_sec REAL NOT NULL DEFAULT 0,
            end_sec REAL NOT NULL DEFAULT 0,
            text TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (clip_id, segment_index)
        );",
    )
    .map_err(|e| e.to_string())?;
    migrate_media_pool_schema(&conn)?;
    // virtual_shots schema/migrations are owned by the neutral editorial domain.
    crate::virtual_shots::db::ensure(paths, project_id, &conn)?;
    super::transcripts::migrate_transcript_files(&conn, paths, project_id)?;
    Ok(conn)
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    conn.prepare(&format!("PRAGMA table_info({table})"))
        .and_then(|mut stmt| {
            stmt.query_map([], |row| row.get::<_, String>(1))
                .map(|rows| rows.filter_map(Result::ok).any(|name| name == column))
        })
        .unwrap_or(false)
}

fn migrate_media_pool_schema(conn: &Connection) -> Result<(), String> {
    if !column_exists(conn, "clip_transcripts", "transcript_json") {
        conn.execute(
            "ALTER TABLE clip_transcripts ADD COLUMN transcript_json TEXT NOT NULL DEFAULT '{}'",
            [],
        )
        .map_err(|error| error.to_string())?;
    }
    if !column_exists(conn, "media_pool_workflow", "current_clip_id") {
        let _ = conn.execute(
            "ALTER TABLE media_pool_workflow ADD COLUMN current_clip_id TEXT NOT NULL DEFAULT ''",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE media_pool_workflow ADD COLUMN mark_in_sec REAL",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE media_pool_workflow ADD COLUMN mark_out_sec REAL",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE media_pool_workflow ADD COLUMN active_virtual_shot_id TEXT NOT NULL DEFAULT ''",
            [],
        );
    }
    for (column, sql_type) in [
        ("playhead_frame", "INTEGER NOT NULL DEFAULT 0"),
        ("playhead_sec", "REAL NOT NULL DEFAULT 0"),
        ("playhead_tc", "TEXT NOT NULL DEFAULT ''"),
        ("playhead_fps", "REAL NOT NULL DEFAULT 25"),
    ] {
        if !column_exists(conn, "media_pool_workflow", column) {
            let _ = conn.execute(
                &format!("ALTER TABLE media_pool_workflow ADD COLUMN {column} {sql_type}"),
                [],
            );
        }
    }
    migrate_workflow_json(conn)?;
    Ok(())
}

fn migrate_workflow_json(conn: &Connection) -> Result<(), String> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT state_json FROM media_pool_workflow WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .ok();
    let Some(raw) = raw else {
        return Ok(());
    };
    if raw.trim().is_empty() || raw.trim() == "{}" {
        return Ok(());
    }
    let state = serde_json::from_str::<Value>(&raw).unwrap_or(json!({}));
    let Some(obj) = state.as_object() else {
        conn.execute(
            "UPDATE media_pool_workflow SET state_json = '{}' WHERE id = 1",
            [],
        )
        .map_err(|e| e.to_string())?;
        return Ok(());
    };
    let current = obj
        .get("current_clip_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let active = obj
        .get("active_virtual_shot_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let playhead_fps = normalize_fps(
        obj.get("playhead_fps")
            .and_then(|v| v.as_f64())
            .unwrap_or(25.0),
    );
    let playhead_frame = obj
        .get("playhead_frame")
        .and_then(|v| v.as_i64())
        .or_else(|| {
            obj.get("playhead_sec")
                .and_then(|v| v.as_f64())
                .map(|sec| seconds_to_frame(sec, playhead_fps))
        })
        .unwrap_or(0)
        .max(0);
    let playhead_sec = frame_to_seconds(playhead_frame, playhead_fps);
    let playhead_tc = obj
        .get("playhead_tc")
        .and_then(|v| v.as_str())
        .filter(|v| !v.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| frame_to_timecode(playhead_frame, playhead_fps));
    let mark_in = obj.get("mark_in_sec").and_then(|v| v.as_f64());
    let mark_out = obj.get("mark_out_sec").and_then(|v| v.as_f64());
    let now = now_str();
    conn.execute(
        "INSERT INTO media_pool_workflow (
             id, current_clip_id, playhead_frame, playhead_sec, playhead_tc, playhead_fps,
             mark_in_sec, mark_out_sec, active_virtual_shot_id, updated_at
         )
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(id) DO UPDATE SET
           current_clip_id = excluded.current_clip_id,
           playhead_frame = excluded.playhead_frame,
           playhead_sec = excluded.playhead_sec,
           playhead_tc = excluded.playhead_tc,
           playhead_fps = excluded.playhead_fps,
           mark_in_sec = excluded.mark_in_sec,
           mark_out_sec = excluded.mark_out_sec,
           active_virtual_shot_id = excluded.active_virtual_shot_id,
           updated_at = excluded.updated_at",
        params![
            current,
            playhead_frame,
            playhead_sec,
            playhead_tc,
            playhead_fps,
            mark_in,
            mark_out,
            active,
            now
        ],
    )
    .map_err(|e| e.to_string())?;
    if let Some(ids) = obj.get("selected_clip_ids").and_then(|v| v.as_array()) {
        conn.execute("DELETE FROM media_pool_workflow_selection", [])
            .map_err(|e| e.to_string())?;
        for id in ids.iter().filter_map(|v| v.as_str()) {
            let trimmed = id.trim();
            if trimmed.is_empty() {
                continue;
            }
            conn.execute(
                "INSERT OR IGNORE INTO media_pool_workflow_selection (clip_id, added_at) VALUES (?1, ?2)",
                params![trimmed, now],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    conn.execute(
        "UPDATE media_pool_workflow SET state_json = '{}' WHERE id = 1",
        [],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Uskladi `pool_clips` s redovima `import_status=imported` u projektnoj bazi.
pub fn sync_pool_from_ingest_db(paths: &ProjectPaths, project_id: &str) -> Result<(), String> {
    let imported = read_imported_clips(paths, project_id)?;
    let ids: HashSet<String> = imported
        .iter()
        .filter_map(|c| {
            c.get("clip_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .collect();
    let conn = open_db(paths, project_id)?;
    let now = now_str();
    if ids.is_empty() {
        conn.execute("DELETE FROM pool_clips", [])
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    let existing: Vec<String> = conn
        .prepare("SELECT clip_id FROM pool_clips")
        .map_err(|e| e.to_string())?
        .query_map([], |r| r.get(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<String>, _>>()
        .map_err(|e| e.to_string())?;
    for old in existing {
        if !ids.contains(&old) {
            conn.execute("DELETE FROM pool_clips WHERE clip_id = ?1", params![old])
                .map_err(|e| e.to_string())?;
        }
    }
    for clip_id in &ids {
        conn.execute(
            "INSERT INTO pool_clips (clip_id, status, added_at, updated_at)
             VALUES (?1, 'active', ?2, ?2)
             ON CONFLICT(clip_id) DO UPDATE SET status = 'active', updated_at = excluded.updated_at",
            params![clip_id, now],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn pool_summary(clips: &[Value]) -> Value {
    json!({
        "total": clips.len(),
        "discovered": clips.iter().filter(|c| c.get("discovered").and_then(|v| v.as_bool()).unwrap_or(false)).count(),
        "validated": clips.iter().filter(|c| c.get("validated").and_then(|v| v.as_bool()).unwrap_or(false)).count(),
        "transferred": clips.iter().filter(|c| c.get("transferred").and_then(|v| v.as_bool()).unwrap_or(false)).count(),
        "transcribed": clips.iter().filter(|c| c.get("has_transcript").and_then(|v| v.as_bool()).unwrap_or(false)).count(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::db::{open_project, ProjectPaths};

    fn test_paths(base: &std::path::Path) -> ProjectPaths {
        ProjectPaths {
            data_dir: base.join("data"),
            projects_root: base.join("projects"),
            seed_path: base.join("seed.json"),
        }
    }

    #[test]
    fn workflow_state_json_migrates_to_typed_tables_and_clears() {
        let base = std::env::temp_dir().join(format!(
            "qnc_pool_workflow_json_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "pool_proj";
        let conn = open_project(&paths, project_id).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS media_pool_workflow (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                current_clip_id TEXT NOT NULL DEFAULT '',
                mark_in_sec REAL,
                mark_out_sec REAL,
                active_virtual_shot_id TEXT NOT NULL DEFAULT '',
                state_json TEXT NOT NULL DEFAULT '{}',
                updated_at TEXT
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO media_pool_workflow (id, state_json) VALUES (1, ?1)",
            params![json!({
                "selected_clip_ids": ["clip_a", "clip_b"],
                "current_clip_id": "clip_a",
                "playhead_frame": 50,
                "playhead_fps": 25.0,
                "mark_in_sec": 1.25,
                "mark_out_sec": 3.5,
                "active_virtual_shot_id": "shot_a"
            })
            .to_string()],
        )
        .unwrap();
        drop(conn);

        let conn = open_db(&paths, project_id).unwrap();
        let row: (
            String,
            i64,
            f64,
            String,
            f64,
            Option<f64>,
            Option<f64>,
            String,
            String,
        ) = conn
            .query_row(
                "SELECT current_clip_id, playhead_frame, playhead_sec, playhead_tc, playhead_fps,
                        mark_in_sec, mark_out_sec, active_virtual_shot_id, state_json
                 FROM media_pool_workflow WHERE id = 1",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(row.0, "clip_a");
        assert_eq!(row.1, 50);
        assert!((row.2 - 2.0).abs() < 0.001);
        assert_eq!(row.3, "00:00:02:00");
        assert!((row.4 - 25.0).abs() < 0.001);
        assert_eq!(row.5, Some(1.25));
        assert_eq!(row.6, Some(3.5));
        assert_eq!(row.7, "shot_a");
        assert_eq!(row.8, "{}");
        let selected: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM media_pool_workflow_selection",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(selected, 2);

        let _ = std::fs::remove_dir_all(&base);
    }
}
