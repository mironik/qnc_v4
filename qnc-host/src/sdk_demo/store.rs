use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::project::db::{now_str, open_project, ProjectPaths};

fn ensure_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sdk_demo_state (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            counter INTEGER NOT NULL DEFAULT 0,
            updated_at TEXT
        );",
    )?;
    Ok(())
}

fn read_row(conn: &Connection) -> rusqlite::Result<(u32, String)> {
    ensure_schema(conn)?;
    match conn.query_row(
        "SELECT counter, COALESCE(updated_at, '') FROM sdk_demo_state WHERE id = 1",
        [],
        |r| Ok((r.get::<_, i64>(0)? as u32, r.get::<_, String>(1)?)),
    ) {
        Ok(row) => Ok(row),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok((0, String::new())),
        Err(e) => Err(e),
    }
}

fn write_row(conn: &Connection, counter: u32, updated_at: &str) -> rusqlite::Result<()> {
    ensure_schema(conn)?;
    conn.execute(
        "INSERT INTO sdk_demo_state (id, counter, updated_at) VALUES (1, ?1, ?2)
         ON CONFLICT(id) DO UPDATE SET counter = excluded.counter, updated_at = excluded.updated_at",
        params![counter as i64, updated_at],
    )?;
    Ok(())
}

fn snapshot_json(project_id: &str, counter: u32, updated_at: &str) -> Value {
    json!({
        "project_id": project_id,
        "counter": counter,
        "persistence": "project_db_demo",
        "updated_at": updated_at,
    })
}

pub fn load_state(paths: &ProjectPaths, project_id: &str) -> Result<Value, String> {
    let pid = project_id.trim();
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    let (counter, updated_at) = read_row(&conn).map_err(|e| e.to_string())?;
    Ok(snapshot_json(pid, counter, &updated_at))
}

pub fn increment(paths: &ProjectPaths, project_id: &str, step: u32) -> Result<Value, String> {
    let pid = project_id.trim();
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    let (counter, _) = read_row(&conn).map_err(|e| e.to_string())?;
    let next = counter.saturating_add(step);
    let updated_at = now_str();
    write_row(&conn, next, &updated_at).map_err(|e| e.to_string())?;
    Ok(snapshot_json(pid, next, &updated_at))
}

pub fn reset(paths: &ProjectPaths, project_id: &str) -> Result<Value, String> {
    let pid = project_id.trim();
    let conn = open_project(paths, pid).map_err(|e| e.to_string())?;
    let updated_at = now_str();
    write_row(&conn, 0, &updated_at).map_err(|e| e.to_string())?;
    Ok(snapshot_json(pid, 0, &updated_at))
}

pub fn clamp_step(step: Option<u32>) -> u32 {
    match step.unwrap_or(1) {
        0 => 1,
        n if n > 10 => 10,
        n => n,
    }
}
