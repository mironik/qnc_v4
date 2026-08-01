//! Shell / stroj — SQLite u `data/shell.db` (nije vezano uz projekt).
//! Profil hardvera, buduće postavke računala.

use std::fs;
use std::path::Path;

use rusqlite::{params, Connection};

use crate::project::db::configure_connection;

const DB_FILE: &str = "shell.db";

pub fn open(data_dir: &Path) -> rusqlite::Result<Connection> {
    fs::create_dir_all(data_dir).ok();
    let conn = Connection::open(data_dir.join(DB_FILE))?;
    configure_connection(&conn)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS shell_settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )?;
    Ok(conn)
}

pub fn get_setting(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    match conn.query_row(
        "SELECT value FROM shell_settings WHERE key = ?1",
        params![key],
        |r| r.get(0),
    ) {
        Ok(value) => Ok(Some(value)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO shell_settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}
