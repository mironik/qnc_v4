use rusqlite::{params, Connection};
use serde_json::Value;

use super::covers::{
    cover_snapshot_by_id, delete_cover, restore_cover_from_snapshot, set_selected_cover_id,
};

const STATE_UNDONE: &str = "undone";
const STATE_ACTIVE: &str = "active";

pub(crate) fn ensure_object_history_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS story_object_history (
            object_type TEXT NOT NULL,
            object_id TEXT NOT NULL,
            state TEXT NOT NULL DEFAULT '',
            snapshot_json TEXT NOT NULL DEFAULT '{}',
            updated_at TEXT NOT NULL,
            PRIMARY KEY (object_type, object_id)
        );",
    )
}

fn object_key(object_type: &str, object_id: &str) -> Result<(String, String), String> {
    let object_type = object_type.trim();
    let object_id = object_id.trim();
    if object_type.is_empty() {
        return Err("object_type required".into());
    }
    if object_id.is_empty() {
        return Err("object_id required".into());
    }
    match object_type {
        "cover" => Ok((object_type.to_string(), object_id.to_string())),
        _ => Err(format!(
            "Story object undo nije podržan za tip: {object_type}"
        )),
    }
}

fn store_snapshot(
    conn: &Connection,
    object_type: &str,
    object_id: &str,
    state: &str,
    snapshot: &Value,
) -> Result<(), String> {
    ensure_object_history_schema(conn).map_err(|e| e.to_string())?;
    let now = crate::project::db::now_str();
    conn.execute(
        "INSERT INTO story_object_history
            (object_type, object_id, state, snapshot_json, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(object_type, object_id) DO UPDATE SET
            state = excluded.state,
            snapshot_json = excluded.snapshot_json,
            updated_at = excluded.updated_at",
        params![object_type, object_id, state, snapshot.to_string(), now],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn load_snapshot(
    conn: &Connection,
    object_type: &str,
    object_id: &str,
    required_state: &str,
) -> Result<Value, String> {
    ensure_object_history_schema(conn).map_err(|e| e.to_string())?;
    let (state, raw): (String, String) = conn
        .query_row(
            "SELECT state, snapshot_json FROM story_object_history
             WHERE object_type = ?1 AND object_id = ?2",
            params![object_type, object_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|_| format!("Nema redo zapisa za {object_type}:{object_id}"))?;
    if state != required_state {
        return Err(format!(
            "Redo nije dostupan za {object_type}:{object_id} (state={state})"
        ));
    }
    serde_json::from_str(&raw).map_err(|e| e.to_string())
}

pub(crate) fn undo_object(
    conn: &Connection,
    object_type: &str,
    object_id: &str,
) -> Result<(), String> {
    let (object_type, object_id) = object_key(object_type, object_id)?;
    match object_type.as_str() {
        "cover" => {
            let snapshot = cover_snapshot_by_id(conn, &object_id)?;
            store_snapshot(conn, &object_type, &object_id, STATE_UNDONE, &snapshot)?;
            delete_cover(conn, &object_id)
        }
        _ => unreachable!(),
    }
}

pub(crate) fn redo_object(
    conn: &Connection,
    object_type: &str,
    object_id: &str,
) -> Result<(), String> {
    let (object_type, object_id) = object_key(object_type, object_id)?;
    match object_type.as_str() {
        "cover" => {
            let snapshot = load_snapshot(conn, &object_type, &object_id, STATE_UNDONE)?;
            restore_cover_from_snapshot(conn, &snapshot)?;
            store_snapshot(conn, &object_type, &object_id, STATE_ACTIVE, &snapshot)?;
            set_selected_cover_id(conn, &object_id).map_err(|e| e.to_string())
        }
        _ => unreachable!(),
    }
}
