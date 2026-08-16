use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::config::{configured_projects_root, AppConfig};
use crate::frame_time::{normalize_fps, DEFAULT_FPS};

/// Shared SQLite tuning: WAL + busy wait so UI/API readers are not stalled by workers.
pub fn configure_connection(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(Duration::from_millis(5_000))?;
    conn.pragma_update(None, "foreign_keys", &true)?;
    // journal_mode returns the mode string; ignore if already WAL.
    let _ = conn.pragma_update(None, "journal_mode", &"WAL");
    conn.pragma_update(None, "synchronous", &"NORMAL")?;
    Ok(())
}

#[derive(Clone)]
pub struct ProjectPaths {
    pub data_dir: PathBuf,
    pub projects_root: PathBuf,
    pub seed_path: PathBuf,
}

impl ProjectPaths {
    pub fn from_root(root: &Path, config: &AppConfig) -> Self {
        let data_dir = root.join("data");
        let projects_root = configured_projects_root(config);
        let seed_path = crate::workspace_paths::system_seed(root);
        Self {
            data_dir,
            projects_root,
            seed_path,
        }
    }

    pub fn global_db(&self) -> PathBuf {
        self.data_dir.join("project_store.db")
    }

    #[allow(dead_code)]
    pub fn project_db(&self, project_id: &str) -> PathBuf {
        self.project_dir(project_id).join("qnc_project.db")
    }

    pub fn project_dir(&self, project_id: &str) -> PathBuf {
        if let Ok(conn) = Connection::open(self.global_db()) {
            let _ = configure_connection(&conn);
            return project_dir_from_conn(&conn, self, project_id);
        }
        project_dir_in_root(&self.projects_root, project_id)
    }
}

pub fn now_str() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("epoch_{secs}")
}

pub fn slug_id(name: &str) -> String {
    let mut base: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    while base.contains("__") {
        base = base.replace("__", "_");
    }
    base = base.trim_matches('_').chars().take(40).collect();
    if base.is_empty() {
        base = "projekt".into();
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{base}_{ts}")
}

pub fn safe_dir_name(project_id: &str) -> String {
    let mut pid: String = project_id
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if pid.len() > 80 {
        pid.truncate(80);
    }
    if pid.is_empty() {
        "_invalid_project_id".into()
    } else {
        pid
    }
}

pub fn project_dir_in_root(root: &Path, project_id: &str) -> PathBuf {
    root.join(safe_dir_name(project_id))
}

/// Resolve on-disk project folder from an already-open global DB connection.
pub fn project_dir_from_conn(conn: &Connection, paths: &ProjectPaths, project_id: &str) -> PathBuf {
    let pid = project_id.trim();
    let row: Option<Option<String>> = conn
        .query_row(
            "SELECT project_dir FROM projects WHERE project_id = ?1",
            params![pid],
            |r| r.get(0),
        )
        .ok();
    if let Some(Some(dir)) = row.as_ref() {
        let dir = dir.trim();
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    let fallback = project_dir_in_root(&paths.projects_root, pid);
    if row.is_some() {
        let stored = fallback.to_string_lossy().to_string();
        let _ = conn.execute(
            "UPDATE projects
             SET project_dir = ?2
             WHERE project_id = ?1 AND TRIM(COALESCE(project_dir, '')) = ''",
            params![pid, stored],
        );
    }
    fallback
}

pub fn project_is_registered(paths: &ProjectPaths, project_id: &str) -> bool {
    let pid = project_id.trim();
    if pid.is_empty() {
        return false;
    }
    let Ok(conn) = Connection::open(paths.global_db()) else {
        return false;
    };
    let _ = configure_connection(&conn);
    conn.query_row(
        "SELECT 1 FROM projects WHERE project_id = ?1",
        params![pid],
        |_| Ok(()),
    )
    .is_ok()
}

pub fn project_root_from_settings(settings: &Value) -> Option<PathBuf> {
    settings
        .get("storage")
        .and_then(|v| v.get("projects_root"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

pub fn export_dir_from_settings(settings: &Value) -> Option<PathBuf> {
    settings
        .get("export")
        .and_then(|v| v.get("directory").or_else(|| v.get("output_directory")))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// Merged project settings from SQLite KV (`project_settings_kv`).
pub fn project_effective_settings(paths: &ProjectPaths, project_id: &str) -> Value {
    let pid = project_id.trim();
    open_project(paths, pid)
        .ok()
        .and_then(|conn| load_project_settings_object(&conn, pid).ok())
        .filter(|settings| settings.is_object())
        .unwrap_or_else(|| json!({}))
}

/// Export/sequence FPS from project settings (`settings.video.fps`).
/// Do not use this for source, Story, Segment timeline, marker, or playback math.
#[allow(dead_code)]
pub fn project_timeline_fps(paths: &ProjectPaths, project_id: &str) -> f64 {
    project_effective_settings(paths, project_id)
        .get("video")
        .and_then(|video| video.get("fps"))
        .and_then(|fps| fps.as_f64())
        .filter(|fps| fps.is_finite() && *fps > 0.0)
        .map(normalize_fps)
        .unwrap_or(DEFAULT_FPS)
}

pub fn project_settings_snapshot(
    paths: &ProjectPaths,
    project_id: &str,
) -> rusqlite::Result<Value> {
    let pid = project_id.trim();
    let conn = open_project(paths, pid)?;
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT template_id, settings_json FROM project_settings WHERE project_id = ?1",
            params![pid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let Some((template_id, _settings_raw)) = row else {
        return Ok(json!({}));
    };
    let settings = load_project_settings_object(&conn, pid)?;
    Ok(json!({
        "project_id": pid,
        "template_id": template_id,
        "settings": settings,
    }))
}

pub fn project_display_name(paths: &ProjectPaths, project_id: &str) -> String {
    let pid = project_id.trim();
    if pid.is_empty() {
        return String::new();
    }
    let Ok(conn) = Connection::open(paths.global_db()) else {
        return pid.to_string();
    };
    let _ = configure_connection(&conn);
    let name = conn
        .query_row(
            "SELECT name FROM projects WHERE project_id = ?1",
            params![pid],
            |r| r.get::<_, String>(0),
        )
        .unwrap_or_default();
    if name.trim().is_empty() {
        pid.to_string()
    } else {
        name
    }
}

pub fn open_global(paths: &ProjectPaths) -> rusqlite::Result<Connection> {
    fs::create_dir_all(&paths.data_dir).ok();
    let conn = Connection::open(paths.global_db())?;
    configure_connection(&conn)?;
    init_global_schema(&conn)?;
    backfill_project_dirs(&conn, &paths.projects_root)?;
    Ok(conn)
}

pub fn open_project(paths: &ProjectPaths, project_id: &str) -> rusqlite::Result<Connection> {
    let pid = project_id.trim();
    if pid.is_empty() {
        return Err(rusqlite::Error::InvalidParameterName(
            "project_id".to_string(),
        ));
    }
    let registered = project_is_registered(paths, pid);
    if !registered {
        #[cfg(not(test))]
        {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
    }
    let dir = paths.project_dir(pid);
    let db_path = dir.join("qnc_project.db");
    // Never recreate folders for deleted / unknown projects in production.
    // (Unit tests still open ephemeral DBs without a global projects row.)
    if !db_path.exists() {
        fs::create_dir_all(&dir).map_err(|e| {
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
                Some(format!("Ne mogu kreirati projektni folder: {e}")),
            )
        })?;
    }
    let conn = Connection::open(db_path)?;
    configure_connection(&conn)?;
    init_project_schema(&conn)?;
    Ok(conn)
}

/// API / worker path: reject unknown `project_id` even under `cfg(test)`.
/// Use this from ingest/filmstrip so mkdir never runs before validation.
pub fn open_project_strict(paths: &ProjectPaths, project_id: &str) -> rusqlite::Result<Connection> {
    let pid = project_id.trim();
    if pid.is_empty() {
        return Err(rusqlite::Error::InvalidParameterName(
            "project_id".to_string(),
        ));
    }
    if !project_is_registered(paths, pid) {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    open_project(paths, project_id)
}

fn backfill_project_dirs(conn: &Connection, projects_root: &Path) -> rusqlite::Result<()> {
    let mut stmt =
        conn.prepare("SELECT project_id FROM projects WHERE TRIM(COALESCE(project_dir, '')) = ''")?;
    let ids: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    for project_id in ids {
        let project_dir = project_dir_in_root(projects_root, &project_id);
        conn.execute(
            "UPDATE projects SET project_dir = ?2 WHERE project_id = ?1",
            params![project_id, project_dir.to_string_lossy().to_string()],
        )?;
    }
    Ok(())
}

fn init_global_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS projects (
            project_id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            project_dir TEXT,
            created_at TEXT,
            updated_at TEXT,
            created_by TEXT,
            updated_by TEXT
        );
        CREATE TABLE IF NOT EXISTS app_settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS users (
            user_id TEXT PRIMARY KEY,
            display_name TEXT NOT NULL,
            role TEXT NOT NULL DEFAULT 'editor',
            active INTEGER NOT NULL DEFAULT 1,
            created_at TEXT,
            updated_at TEXT
        );
        CREATE TABLE IF NOT EXISTS sessions (
            session_id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            station_id TEXT NOT NULL,
            client_label TEXT NOT NULL DEFAULT '',
            created_at TEXT,
            last_seen_at TEXT,
            FOREIGN KEY(user_id) REFERENCES users(user_id)
        );
        CREATE TABLE IF NOT EXISTS source_templates (
            source_template_id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            source_kind TEXT NOT NULL DEFAULT 'local',
            system INTEGER NOT NULL DEFAULT 0,
            config_json TEXT NOT NULL DEFAULT '{}',
            created_by TEXT,
            updated_by TEXT,
            created_at TEXT,
            updated_at TEXT
        );
        CREATE TABLE IF NOT EXISTS project_templates (
            template_id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            system INTEGER NOT NULL DEFAULT 0,
            settings_json TEXT NOT NULL DEFAULT '{}',
            source_template_ids_json TEXT NOT NULL DEFAULT '[]',
            created_by TEXT,
            updated_by TEXT,
            created_at TEXT,
            updated_at TEXT
        );
        CREATE TABLE IF NOT EXISTS module_state (
            module_id TEXT PRIMARY KEY,
            enabled INTEGER NOT NULL DEFAULT 1,
            updated_at TEXT
        );
        CREATE TABLE IF NOT EXISTS project_template_kv (
            template_id TEXT NOT NULL,
            setting_key TEXT NOT NULL,
            setting_value TEXT NOT NULL,
            PRIMARY KEY (template_id, setting_key)
        );
        CREATE TABLE IF NOT EXISTS project_template_sources (
            template_id TEXT NOT NULL,
            source_template_id TEXT NOT NULL,
            PRIMARY KEY (template_id, source_template_id)
        );
        CREATE TABLE IF NOT EXISTS source_template_kv (
            source_template_id TEXT NOT NULL,
            setting_key TEXT NOT NULL,
            setting_value TEXT NOT NULL,
            PRIMARY KEY (source_template_id, setting_key)
        );
        ",
    )?;
    migrate_projects_columns(conn)?;
    migrate_global_json_blobs_to_kv(conn)?;
    Ok(())
}

fn migrate_global_json_blobs_to_kv(conn: &Connection) -> rusqlite::Result<()> {
    use super::kv::{kv_count, migrate_json_column_to_kv, replace_string_list};
    migrate_json_column_to_kv(
        conn,
        "project_templates",
        "template_id",
        "settings_json",
        "project_template_kv",
    )?;
    migrate_json_column_to_kv(
        conn,
        "source_templates",
        "source_template_id",
        "config_json",
        "source_template_kv",
    )?;
    let mut stmt = conn.prepare(
        "SELECT template_id, source_template_ids_json FROM project_templates
         WHERE source_template_ids_json != '[]'",
    )?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    for (template_id, raw) in rows {
        if kv_count(
            conn,
            "project_template_sources",
            "template_id",
            &template_id,
        )? > 0
        {
            continue;
        }
        let ids = parse_json(&raw, json!([]));
        let list: Vec<String> = ids
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        replace_string_list(
            conn,
            "project_template_sources",
            "template_id",
            &template_id,
            "source_template_id",
            &list,
        )?;
        conn.execute(
            "UPDATE project_templates SET source_template_ids_json = '[]' WHERE template_id = ?1",
            params![template_id],
        )?;
    }
    Ok(())
}

fn migrate_project_db_kv(conn: &Connection) -> rusqlite::Result<()> {
    use super::kv::migrate_json_column_to_kv;
    migrate_json_column_to_kv(
        conn,
        "project_settings",
        "project_id",
        "settings_json",
        "project_settings_kv",
    )?;
    migrate_json_column_to_kv(
        conn,
        "project_template_snapshot",
        "project_id",
        "snapshot_json",
        "project_snapshot_kv",
    )?;
    migrate_json_column_to_kv(
        conn,
        "project_workflow_steps",
        "step_id",
        "settings_json",
        "project_workflow_step_kv",
    )?;
    Ok(())
}

fn load_project_settings_object(conn: &Connection, project_id: &str) -> rusqlite::Result<Value> {
    use super::kv::load_object;
    load_object(conn, "project_settings_kv", "project_id", project_id)
}

fn migrate_projects_columns(conn: &Connection) -> rusqlite::Result<()> {
    let _ = conn.execute("ALTER TABLE projects ADD COLUMN last_opened_at TEXT", []);
    let _ = conn.execute("ALTER TABLE projects ADD COLUMN project_dir TEXT", []);
    Ok(())
}

fn init_project_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS project_settings (
            project_id TEXT PRIMARY KEY,
            template_id TEXT,
            settings_json TEXT NOT NULL DEFAULT '{}',
            created_by TEXT,
            updated_by TEXT,
            created_at TEXT,
            updated_at TEXT
        );
        CREATE TABLE IF NOT EXISTS project_members (
            project_id TEXT NOT NULL,
            user_id TEXT NOT NULL,
            role TEXT NOT NULL DEFAULT 'editor',
            joined_at TEXT,
            last_seen_at TEXT,
            PRIMARY KEY(project_id, user_id)
        );
        CREATE TABLE IF NOT EXISTS project_template_snapshot (
            project_id TEXT PRIMARY KEY,
            template_id TEXT,
            template_name TEXT NOT NULL DEFAULT '',
            template_version TEXT NOT NULL DEFAULT '',
            snapshot_json TEXT NOT NULL DEFAULT '{}',
            created_at TEXT
        );
        CREATE TABLE IF NOT EXISTS project_workflow_steps (
            step_id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            plugin_id TEXT NOT NULL,
            tab_id TEXT NOT NULL,
            label TEXT NOT NULL,
            position INTEGER NOT NULL,
            status TEXT NOT NULL DEFAULT 'locked',
            next_step_id TEXT,
            settings_json TEXT NOT NULL DEFAULT '{}'
        );
        CREATE TABLE IF NOT EXISTS project_workflow_state (
            project_id TEXT PRIMARY KEY,
            active_step_id TEXT,
            entry_step_id TEXT,
            updated_at TEXT
        );
        CREATE TABLE IF NOT EXISTS project_data_revisions (
            scope TEXT PRIMARY KEY,
            revision INTEGER NOT NULL DEFAULT 0,
            updated_at TEXT NOT NULL DEFAULT ''
        );
        CREATE TABLE IF NOT EXISTS project_settings_kv (
            project_id TEXT NOT NULL,
            setting_key TEXT NOT NULL,
            setting_value TEXT NOT NULL,
            PRIMARY KEY (project_id, setting_key)
        );
        CREATE TABLE IF NOT EXISTS project_snapshot_kv (
            project_id TEXT NOT NULL,
            setting_key TEXT NOT NULL,
            setting_value TEXT NOT NULL,
            PRIMARY KEY (project_id, setting_key)
        );
        CREATE TABLE IF NOT EXISTS project_workflow_step_kv (
            step_id TEXT NOT NULL,
            setting_key TEXT NOT NULL,
            setting_value TEXT NOT NULL,
            PRIMARY KEY (step_id, setting_key)
        );
        ",
    )?;
    migrate_project_db_kv(conn)?;
    Ok(())
}

pub fn bump_project_data_revision(conn: &Connection, scope: &str) -> rusqlite::Result<i64> {
    let scope = scope.trim();
    let now = now_str();
    conn.execute(
        "INSERT INTO project_data_revisions (scope, revision, updated_at)
         VALUES (?1, 1, ?2)
         ON CONFLICT(scope) DO UPDATE SET
            revision = revision + 1,
            updated_at = excluded.updated_at",
        params![scope, now],
    )?;
    conn.query_row(
        "SELECT revision FROM project_data_revisions WHERE scope = ?1",
        params![scope],
        |row| row.get(0),
    )
}

pub fn project_data_revision_snapshot(
    paths: &ProjectPaths,
    project_id: &str,
) -> rusqlite::Result<Value> {
    let conn = open_project(paths, project_id)?;
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT revision, updated_at
             FROM project_data_revisions WHERE scope = 'ingest'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok();
    let pending_imports: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ingest_assets
             WHERE import_status IN ('queued', 'processing')",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    let (revision, updated_at) = row.unwrap_or((0, String::new()));
    Ok(json!({
        "project_id": project_id,
        "ingest": {
            "revision": revision,
            "pending_count": pending_imports,
            "updated_at": updated_at,
        }
    }))
}

pub fn get_setting(conn: &Connection, key: &str, default: &str) -> rusqlite::Result<String> {
    let row: Option<String> = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            params![key],
            |r| r.get(0),
        )
        .ok();
    Ok(row.unwrap_or_else(|| default.to_string()))
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO app_settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn open_shell_db(data_dir: &Path) -> rusqlite::Result<Connection> {
    fs::create_dir_all(data_dir).ok();
    let conn = Connection::open(data_dir.join("project_store.db"))?;
    configure_connection(&conn)?;
    init_global_schema(&conn)?;
    migrate_from_shell_module_state_json(&conn, data_dir)?;
    Ok(conn)
}

pub fn load_module_enabled(conn: &Connection) -> rusqlite::Result<HashMap<String, bool>> {
    let mut stmt = conn.prepare("SELECT module_id, enabled FROM module_state")?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (module_id, enabled) = row?;
        out.insert(module_id, enabled);
    }
    Ok(out)
}

pub fn upsert_module_enabled(
    conn: &Connection,
    module_id: &str,
    enabled: bool,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO module_state (module_id, enabled, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(module_id) DO UPDATE SET
            enabled = excluded.enabled,
            updated_at = excluded.updated_at",
        params![module_id, if enabled { 1 } else { 0 }, now_str()],
    )?;
    Ok(())
}

fn migrate_from_shell_module_state_json(
    conn: &Connection,
    data_dir: &Path,
) -> rusqlite::Result<()> {
    let path = data_dir.join("shell_module_state.json");
    let Ok(raw) = fs::read_to_string(&path) else {
        return Ok(());
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
        return Ok(());
    };
    let Some(enabled_obj) = parsed.get("enabled").and_then(|v| v.as_object()) else {
        return Ok(());
    };
    for (module_id, value) in enabled_obj {
        let Some(enabled) = value.as_bool() else {
            continue;
        };
        upsert_module_enabled(conn, module_id, enabled)?;
    }
    let backup = data_dir.join("shell_module_state.json.migrated");
    let _ = fs::rename(&path, &backup);
    Ok(())
}

pub fn ensure_project_store(conn: &Connection) -> rusqlite::Result<()> {
    // Projekt se ne seeda automatski — korisnik kreira projekte ručno.
    let _ = conn;
    Ok(())
}

pub fn deep_merge(base: &Value, override_val: &Value) -> Value {
    match (base, override_val) {
        (Value::Object(a), Value::Object(b)) => {
            let mut out = a.clone();
            for (k, v) in b {
                if let Some(existing) = a.get(k) {
                    out.insert(k.clone(), deep_merge(existing, v));
                } else {
                    out.insert(k.clone(), v.clone());
                }
            }
            Value::Object(out)
        }
        (_, b) => b.clone(),
    }
}

pub fn json_string(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".into())
}

pub fn parse_json(raw: &str, fallback: Value) -> Value {
    serde_json::from_str(raw).unwrap_or(fallback)
}

pub fn ensure_project_dirs(paths: &ProjectPaths, project_id: &str) -> std::io::Result<()> {
    let pid = project_id.trim();
    if pid.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "project_id je prazan",
        ));
    }
    if !project_is_registered(paths, pid) {
        #[cfg(not(test))]
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Projekt '{pid}' ne postoji — folder se ne kreira."),
            ));
        }
    }
    let base = paths.project_dir(pid);
    ensure_project_dirs_at(&base)
}

pub fn ensure_project_dirs_at(base: &Path) -> std::io::Result<()> {
    for sub in [
        "",
        "proxy",
        "original",
        "audio",
        "incoming/card",
        "incoming/ftp",
        "ingest/thumbnails",
        "filmstrip",
    ] {
        let dir = if sub.is_empty() {
            base.to_path_buf()
        } else {
            base.join(sub)
        };
        fs::create_dir_all(dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_paths(base: &Path) -> ProjectPaths {
        ProjectPaths {
            data_dir: base.join("data"),
            projects_root: base.join("projects"),
            seed_path: base.join("seed.json"),
        }
    }

    fn temp_base(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "qnc_project_db_first_{label}_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    #[test]
    fn strict_open_rejects_orphan_project_db_even_when_folder_exists() {
        let base = temp_base("orphan");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "ghost_project";
        let project_dir = project_dir_in_root(&paths.projects_root, project_id);
        fs::create_dir_all(&project_dir).unwrap();
        let orphan_db = project_dir.join("qnc_project.db");
        let conn = Connection::open(&orphan_db).unwrap();
        configure_connection(&conn).unwrap();
        drop(conn);

        let _global = open_global(&paths).unwrap();

        assert!(
            open_project_strict(&paths, project_id).is_err(),
            "strict open must treat the global projects registry as source of truth"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn project_dir_from_conn_backfills_known_project_dir_into_registry() {
        let base = temp_base("backfill");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let conn = open_global(&paths).unwrap();
        conn.execute(
            "INSERT INTO projects (project_id, name, project_dir) VALUES (?1, ?2, '')",
            params!["known_project", "Known Project"],
        )
        .unwrap();

        let resolved = project_dir_from_conn(&conn, &paths, "known_project");
        let stored: String = conn
            .query_row(
                "SELECT project_dir FROM projects WHERE project_id = 'known_project'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(resolved, PathBuf::from(&stored));
        assert!(!stored.trim().is_empty());
        let _ = fs::remove_dir_all(&base);
    }
}
