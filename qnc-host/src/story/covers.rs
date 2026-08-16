use rusqlite::{params, Connection};
use serde_json::{json, Value};

use super::db::{read_row, touch_draft};
use super::markers::{get_slot_by_id, TIMELINE_EPS};

#[derive(Clone)]
pub struct StoryCoverRow {
    pub cover_id: String,
    pub timeline_start_frame: i64,
    pub timeline_end_frame: i64,
    pub timeline_start_sec: f64,
    pub timeline_end_sec: f64,
    pub slot_signature: String,
    pub slot_index: i64,
    pub clip_id: String,
    pub virtual_shot_id: String,
    pub title: String,
    pub note: String,
    pub in_tc: String,
    pub out_tc: String,
    pub in_seconds: Option<f64>,
    pub out_seconds: Option<f64>,
    pub sort_index: i64,
    pub created_at: String,
    pub updated_at: String,
}

pub fn ensure_cover_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS story_covers (
            cover_id TEXT PRIMARY KEY,
            timeline_start_frame INTEGER NOT NULL DEFAULT 0,
            timeline_end_frame INTEGER NOT NULL DEFAULT 0,
            timeline_start_sec REAL NOT NULL DEFAULT 0,
            timeline_end_sec REAL NOT NULL DEFAULT 0,
            slot_signature TEXT NOT NULL DEFAULT '',
            slot_index INTEGER NOT NULL DEFAULT 0,
            clip_id TEXT NOT NULL DEFAULT '',
            virtual_shot_id TEXT NOT NULL DEFAULT '',
            title TEXT NOT NULL DEFAULT '',
            note TEXT NOT NULL DEFAULT '',
            in_tc TEXT NOT NULL DEFAULT '',
            out_tc TEXT NOT NULL DEFAULT '',
            in_seconds REAL,
            out_seconds REAL,
            sort_index INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_story_covers_signature ON story_covers(slot_signature);",
    )?;
    let _ = conn.execute(
        "ALTER TABLE story_state ADD COLUMN selected_cover_id TEXT NOT NULL DEFAULT ''",
        [],
    );
    migrate_cover_frame_columns(conn)?;
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

fn migrate_cover_frame_columns(conn: &Connection) -> rusqlite::Result<()> {
    for column in ["timeline_start_frame", "timeline_end_frame"] {
        if !column_exists(conn, "story_covers", column)? {
            conn.execute(
                &format!("ALTER TABLE story_covers ADD COLUMN {column} INTEGER NOT NULL DEFAULT 0"),
                [],
            )?;
        }
    }
    Ok(())
}

fn new_cover_id() -> String {
    format!("cover_{}", uuid::Uuid::new_v4().simple())
}

fn validate_id_field(value: &str, field: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.contains('{') || trimmed.contains('[') {
        return Err(format!("{field} must be a string id, not an object"));
    }
    Ok(trimmed.to_string())
}

fn cover_exists(conn: &Connection, cover_id: &str) -> Result<bool, String> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM story_covers WHERE cover_id = ?1",
            params![cover_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok(n > 0)
}

pub fn list_covers(conn: &Connection) -> rusqlite::Result<Vec<StoryCoverRow>> {
    ensure_cover_schema(conn)?;
    let mut stmt = conn.prepare(
        "SELECT cover_id, timeline_start_frame, timeline_end_frame,
                timeline_start_sec, timeline_end_sec, slot_signature, slot_index,
                clip_id, virtual_shot_id, title, note,
                in_tc, out_tc, in_seconds, out_seconds, sort_index, created_at, updated_at
         FROM story_covers
         ORDER BY timeline_start_frame ASC, sort_index ASC, cover_id ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(StoryCoverRow {
            cover_id: r.get(0)?,
            timeline_start_frame: r.get(1)?,
            timeline_end_frame: r.get(2)?,
            timeline_start_sec: r.get(3)?,
            timeline_end_sec: r.get(4)?,
            slot_signature: r.get(5)?,
            slot_index: r.get(6)?,
            clip_id: r.get(7)?,
            virtual_shot_id: r.get(8)?,
            title: r.get(9)?,
            note: r.get(10)?,
            in_tc: r.get(11)?,
            out_tc: r.get(12)?,
            in_seconds: r.get(13)?,
            out_seconds: r.get(14)?,
            sort_index: r.get(15)?,
            created_at: r.get(16)?,
            updated_at: r.get(17)?,
        })
    })?;
    rows.collect()
}

fn optional_f64(value: Option<f64>) -> Value {
    match value {
        Some(v) => json!(v),
        None => Value::Null,
    }
}

pub fn cover_json(row: &StoryCoverRow) -> Value {
    json!({
        "cover_id": row.cover_id,
        "timeline_start_frame": row.timeline_start_frame,
        "timeline_end_frame": row.timeline_end_frame,
        "timeline_start_sec": row.timeline_start_sec,
        "timeline_end_sec": row.timeline_end_sec,
        "slot_signature": row.slot_signature,
        "slot_index": row.slot_index,
        "clip_id": row.clip_id,
        "virtual_shot_id": row.virtual_shot_id,
        "title": row.title,
        "note": row.note,
        "in_tc": row.in_tc,
        "out_tc": row.out_tc,
        "in_seconds": optional_f64(row.in_seconds),
        "out_seconds": optional_f64(row.out_seconds),
        "sort_index": row.sort_index,
        "created_at": row.created_at,
        "updated_at": row.updated_at,
    })
}

pub fn covers_snapshot(conn: &Connection) -> rusqlite::Result<Vec<Value>> {
    Ok(list_covers(conn)?.iter().map(cover_json).collect())
}

fn normalize_selected_cover_id_value(
    conn: &Connection,
    selected: &str,
) -> rusqlite::Result<String> {
    let selected = selected.trim();
    if selected.is_empty() {
        return Ok(String::new());
    }
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM story_covers WHERE cover_id = ?1",
        params![selected],
        |r| r.get(0),
    )?;
    if exists > 0 {
        Ok(selected.to_string())
    } else {
        Ok(String::new())
    }
}

fn set_selected_cover_id(conn: &Connection, cover_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE story_state SET selected_cover_id = ?1 WHERE id = 1",
        params![cover_id],
    )?;
    Ok(())
}

fn cover_matches_slot(
    cover: &StoryCoverRow,
    start_frame: i64,
    end_frame: i64,
    start: f64,
    end: f64,
    sig: &str,
) -> bool {
    if cover.slot_signature == sig {
        return true;
    }
    if cover.timeline_start_frame == start_frame && cover.timeline_end_frame == end_frame {
        return true;
    }
    (cover.timeline_start_sec - start).abs() < TIMELINE_EPS
        && (cover.timeline_end_sec - end).abs() < TIMELINE_EPS
}

/// Drop covers whose timeline interval no longer matches any materialized slot.
pub fn normalize_covers_for_slots(
    conn: &Connection,
    new_slots: &[(i64, i64, f64, f64, String)],
) -> rusqlite::Result<()> {
    ensure_cover_schema(conn)?;
    let covers = list_covers(conn)?;
    for cover in covers {
        let matched = new_slots
            .iter()
            .find(|(start_frame, end_frame, start, end, sig)| {
                cover_matches_slot(&cover, *start_frame, *end_frame, *start, *end, sig)
            });
        if let Some((start_frame, end_frame, _, _, _)) = matched {
            if cover.timeline_start_frame != *start_frame || cover.timeline_end_frame != *end_frame
            {
                conn.execute(
                    "UPDATE story_covers
                     SET timeline_start_frame = ?1, timeline_end_frame = ?2
                     WHERE cover_id = ?3",
                    params![start_frame, end_frame, cover.cover_id],
                )?;
            }
        } else {
            conn.execute(
                "DELETE FROM story_covers WHERE cover_id = ?1",
                params![cover.cover_id],
            )?;
        }
    }
    Ok(())
}

pub fn normalize_selected_cover_id(conn: &Connection) -> rusqlite::Result<()> {
    ensure_cover_schema(conn)?;
    let row = read_row(conn)?;
    let normalized = normalize_selected_cover_id_value(conn, &row.selected_cover_id)?;
    if normalized != row.selected_cover_id {
        set_selected_cover_id(conn, &normalized)?;
    }
    Ok(())
}

pub fn create_cover(
    conn: &Connection,
    slot_id: &str,
    clip_id: Option<&str>,
    virtual_shot_id: Option<&str>,
    in_tc: Option<&str>,
    out_tc: Option<&str>,
    in_seconds: Option<f64>,
    out_seconds: Option<f64>,
    title: Option<&str>,
    note: Option<&str>,
) -> Result<(), String> {
    let slot_id = slot_id.trim();
    if slot_id.is_empty() {
        return Err("slot_id required".into());
    }
    let slot = get_slot_by_id(conn, slot_id)?;
    let clip_id = clip_id
        .map(|v| validate_id_field(v, "clip_id"))
        .transpose()?
        .unwrap_or_default();
    let virtual_shot_id = virtual_shot_id
        .map(|v| validate_id_field(v, "virtual_shot_id"))
        .transpose()?
        .unwrap_or_default();
    let in_tc = in_tc.unwrap_or("").trim();
    let out_tc = out_tc.unwrap_or("").trim();
    let title = title.unwrap_or("").trim();
    let note = note.unwrap_or("").trim();
    let cover_id = new_cover_id();
    let now = crate::project::db::now_str();
    let sig = slot.slot_signature.clone();
    conn.execute(
        "DELETE FROM story_covers WHERE slot_signature = ?1",
        params![sig],
    )
    .map_err(|e| e.to_string())?;
    let sort_index = 0;
    conn.execute(
        "INSERT INTO story_covers
            (cover_id, timeline_start_frame, timeline_end_frame,
             timeline_start_sec, timeline_end_sec, slot_signature, slot_index,
             clip_id, virtual_shot_id, title, note,
             in_tc, out_tc, in_seconds, out_seconds, sort_index, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?17)",
        params![
            cover_id,
            slot.start_frame,
            slot.end_frame,
            slot.start_sec,
            slot.end_sec,
            sig,
            slot.slot_index,
            clip_id,
            virtual_shot_id,
            title,
            note,
            in_tc,
            out_tc,
            in_seconds,
            out_seconds,
            sort_index,
            now,
        ],
    )
    .map_err(|e| e.to_string())?;
    set_selected_cover_id(conn, &cover_id).map_err(|e| e.to_string())?;
    touch_draft(conn).map_err(|e| e.to_string())
}

pub fn update_cover(
    conn: &Connection,
    cover_id: &str,
    title: Option<&str>,
    note: Option<&str>,
    clip_id: Option<&str>,
    virtual_shot_id: Option<&str>,
) -> Result<(), String> {
    let cover_id = cover_id.trim();
    if cover_id.is_empty() {
        return Err("cover_id required".into());
    }
    if !cover_exists(conn, cover_id)? {
        return Err(format!("cover not found: {cover_id}"));
    }
    let now = crate::project::db::now_str();
    if let Some(t) = title {
        conn.execute(
            "UPDATE story_covers SET title = ?1, updated_at = ?2 WHERE cover_id = ?3",
            params![t.trim(), now, cover_id],
        )
        .map_err(|e| e.to_string())?;
    }
    if let Some(n) = note {
        conn.execute(
            "UPDATE story_covers SET note = ?1, updated_at = ?2 WHERE cover_id = ?3",
            params![n.trim(), now, cover_id],
        )
        .map_err(|e| e.to_string())?;
    }
    if let Some(c) = clip_id {
        let c = validate_id_field(c, "clip_id")?;
        conn.execute(
            "UPDATE story_covers SET clip_id = ?1, updated_at = ?2 WHERE cover_id = ?3",
            params![c, now, cover_id],
        )
        .map_err(|e| e.to_string())?;
    }
    if let Some(v) = virtual_shot_id {
        let v = validate_id_field(v, "virtual_shot_id")?;
        conn.execute(
            "UPDATE story_covers SET virtual_shot_id = ?1, updated_at = ?2 WHERE cover_id = ?3",
            params![v, now, cover_id],
        )
        .map_err(|e| e.to_string())?;
    }
    touch_draft(conn).map_err(|e| e.to_string())
}

pub fn delete_cover(conn: &Connection, cover_id: &str) -> Result<(), String> {
    let cover_id = cover_id.trim();
    if cover_id.is_empty() {
        return Err("cover_id required".into());
    }
    let n = conn
        .execute(
            "DELETE FROM story_covers WHERE cover_id = ?1",
            params![cover_id],
        )
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err(format!("cover not found: {cover_id}"));
    }
    let row = read_row(conn).map_err(|e| e.to_string())?;
    if row.selected_cover_id == cover_id {
        set_selected_cover_id(conn, "").map_err(|e| e.to_string())?;
    }
    touch_draft(conn).map_err(|e| e.to_string())
}

pub fn select_cover(conn: &Connection, cover_id: &str) -> Result<(), String> {
    let cover_id = cover_id.trim();
    if cover_id.is_empty() {
        return Err("cover_id required".into());
    }
    if !cover_exists(conn, cover_id)? {
        return Err(format!("cover not found: {cover_id}"));
    }
    set_selected_cover_id(conn, cover_id).map_err(|e| e.to_string())?;
    touch_draft(conn).map_err(|e| e.to_string())
}
