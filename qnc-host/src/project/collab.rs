use rusqlite::{params, Connection};
use serde_json::{json, Value};

use super::db::{now_str, open_project, ProjectPaths};

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

pub fn start_session(
    conn: &Connection,
    paths: &ProjectPaths,
    display_name: &str,
    role: &str,
    station_id: &str,
    client_label: &str,
    project_id: &str,
) -> rusqlite::Result<Value> {
    let now = now_str();
    let display_name = if display_name.trim().is_empty() {
        "QNC korisnik"
    } else {
        display_name.trim()
    };
    let role = if role.trim().is_empty() {
        "editor"
    } else {
        role.trim()
    };
    let station_id = if station_id.trim().is_empty() {
        "unknown-station"
    } else {
        station_id.trim()
    };
    let user_id = new_id("usr");
    let session_id = new_id("ses");
    conn.execute(
        "INSERT INTO users (user_id, display_name, role, active, created_at, updated_at)
         VALUES (?1, ?2, ?3, 1, ?4, ?4)",
        params![user_id, display_name, role, now],
    )?;
    conn.execute(
        "INSERT INTO sessions (session_id, user_id, station_id, client_label, created_at, last_seen_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
        params![session_id, user_id, station_id, client_label.trim(), now],
    )?;
    if !project_id.trim().is_empty() {
        let pid = project_id.trim();
        let pconn = open_project(paths, pid)?;
        pconn.execute(
            "INSERT INTO project_members (project_id, user_id, role, joined_at, last_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(project_id, user_id) DO UPDATE SET
                role = excluded.role,
                last_seen_at = excluded.last_seen_at",
            params![pid, user_id, role, now],
        )?;
    }
    get_session(conn, &session_id)
}

pub fn get_session(conn: &Connection, session_id: &str) -> rusqlite::Result<Value> {
    let sid = session_id.trim();
    if sid.is_empty() {
        return Ok(json!({}));
    }
    conn.query_row(
        "SELECT s.session_id, s.user_id, s.station_id, s.client_label, s.created_at, s.last_seen_at,
                u.display_name, u.role
         FROM sessions s
         JOIN users u ON u.user_id = s.user_id
         WHERE s.session_id = ?1",
        params![sid],
        |r| {
            Ok(json!({
                "session_id": r.get::<_, String>(0)?,
                "user_id": r.get::<_, String>(1)?,
                "station_id": r.get::<_, String>(2)?,
                "client_label": r.get::<_, String>(3)?,
                "created_at": r.get::<_, Option<String>>(4)?,
                "last_seen_at": r.get::<_, Option<String>>(5)?,
                "display_name": r.get::<_, String>(6)?,
                "role": r.get::<_, String>(7)?,
            }))
        },
    )
}

pub fn touch_session(
    conn: &Connection,
    paths: &ProjectPaths,
    session_id: &str,
    project_id: &str,
) -> rusqlite::Result<()> {
    let sid = session_id.trim();
    if sid.is_empty() {
        return Ok(());
    }
    let now = now_str();
    conn.execute(
        "UPDATE sessions SET last_seen_at = ?1 WHERE session_id = ?2",
        params![now, sid],
    )?;
    let pid = project_id.trim();
    if pid.is_empty() {
        return Ok(());
    }
    let user_id: Option<String> = conn
        .query_row(
            "SELECT user_id FROM sessions WHERE session_id = ?1",
            params![sid],
            |r| r.get(0),
        )
        .ok();
    let Some(uid) = user_id else {
        return Ok(());
    };
    let pconn = open_project(paths, pid)?;
    pconn.execute(
        "INSERT INTO project_members (project_id, user_id, role, joined_at, last_seen_at)
         VALUES (?1, ?2, 'editor', ?3, ?3)
         ON CONFLICT(project_id, user_id) DO UPDATE SET last_seen_at = excluded.last_seen_at",
        params![pid, uid, now],
    )?;
    Ok(())
}
