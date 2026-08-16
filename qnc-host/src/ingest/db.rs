use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};
use serde_json::Value;

use crate::project::db::{now_str, open_project, ProjectPaths};

pub fn ingest_dir(paths: &ProjectPaths, project_id: &str) -> PathBuf {
    paths.project_dir(project_id).join("ingest")
}

pub fn ensure_ingest_dirs(paths: &ProjectPaths, project_id: &str) -> std::io::Result<()> {
    // Never create ingest/ under an unknown/deleted project.
    if !paths
        .project_dir(project_id)
        .join("qnc_project.db")
        .exists()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "project db missing",
        ));
    }
    let base = ingest_dir(paths, project_id);
    fs::create_dir_all(base.join("thumbnails"))?;
    Ok(())
}

pub fn open_ingest(paths: &ProjectPaths, project_id: &str) -> rusqlite::Result<Connection> {
    // Validate / open project DB first — never mkdir for unknown project_id.
    let conn = open_project(paths, project_id)?;
    ensure_ingest_dirs(paths, project_id).ok();
    init_schema(&conn)?;
    Ok(conn)
}

fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS ingest_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS ingest_assets (
            source_id TEXT NOT NULL,
            clip_id TEXT NOT NULL,
            name TEXT NOT NULL DEFAULT '',
            media_id TEXT NOT NULL DEFAULT '',
            duration_sec REAL NOT NULL DEFAULT 0,
            resolution TEXT NOT NULL DEFAULT '',
            codec TEXT NOT NULL DEFAULT '',
            fps REAL NOT NULL DEFAULT 0,
            field_order TEXT NOT NULL DEFAULT '',
            interlaced INTEGER NOT NULL DEFAULT 0,
            source_class TEXT NOT NULL DEFAULT '',
            proxy_recipe TEXT NOT NULL DEFAULT '',
            has_audio INTEGER NOT NULL DEFAULT 0,
            audio_channels INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL DEFAULT '',
            import_status TEXT NOT NULL DEFAULT '',
            selected INTEGER NOT NULL DEFAULT 0,
            thumb_color_a TEXT NOT NULL DEFAULT '',
            thumb_color_b TEXT NOT NULL DEFAULT '',
            thumb_status TEXT NOT NULL DEFAULT 'pending',
            thumb_error TEXT NOT NULL DEFAULT '',
            source_path TEXT NOT NULL DEFAULT '',
            original_path TEXT NOT NULL DEFAULT '',
            proxy_path TEXT NOT NULL DEFAULT '',
            project_proxy_path TEXT NOT NULL DEFAULT '',
            thumb_path TEXT NOT NULL DEFAULT '',
            card_thumb_path TEXT NOT NULL DEFAULT '',
            metadata_json TEXT NOT NULL DEFAULT '{}',
            virtual_name TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (source_id, clip_id)
        );
        CREATE TABLE IF NOT EXISTS ingest_jobs (
            job_id TEXT PRIMARY KEY,
            job_type TEXT NOT NULL,
            source_id TEXT NOT NULL DEFAULT '',
            clip_id TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'queued',
            error TEXT NOT NULL DEFAULT '',
            attempts INTEGER NOT NULL DEFAULT 0,
            queued_at TEXT,
            started_at TEXT,
            finished_at TEXT,
            updated_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_ingest_jobs_status ON ingest_jobs(job_type, status);
        CREATE INDEX IF NOT EXISTS idx_ingest_jobs_clip ON ingest_jobs(job_type, source_id, clip_id);
        ",
    )?;
    migrate_thumb_columns(conn)?;
    migrate_ingest_metadata_columns(conn)?;
    Ok(())
}

pub fn ingest_job_id(job_type: &str, source_id: &str, clip_id: &str) -> String {
    format!(
        "{}:{}:{}",
        safe_job_part(job_type),
        safe_job_part(source_id),
        safe_job_part(clip_id)
    )
}

fn safe_job_part(raw: &str) -> String {
    let mut out: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        out = "_".into();
    }
    out
}

pub fn queue_ingest_job(
    conn: &Connection,
    job_type: &str,
    source_id: &str,
    clip_id: &str,
) -> rusqlite::Result<()> {
    let now = now_str();
    let job_id = ingest_job_id(job_type, source_id, clip_id);
    conn.execute(
        "INSERT INTO ingest_jobs
            (job_id, job_type, source_id, clip_id, status, error, attempts, queued_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'queued', '', 0, ?5, ?5)
         ON CONFLICT(job_id) DO UPDATE SET
            status = 'queued',
            error = '',
            queued_at = excluded.queued_at,
            started_at = NULL,
            finished_at = NULL,
            updated_at = excluded.updated_at",
        params![
            job_id,
            job_type.trim(),
            source_id.trim(),
            clip_id.trim(),
            now
        ],
    )?;
    Ok(())
}

pub fn mark_ingest_job_processing(
    conn: &Connection,
    job_type: &str,
    source_id: &str,
    clip_id: &str,
) -> rusqlite::Result<()> {
    let now = now_str();
    let job_id = ingest_job_id(job_type, source_id, clip_id);
    conn.execute(
        "INSERT INTO ingest_jobs
            (job_id, job_type, source_id, clip_id, status, error, attempts, queued_at, started_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'processing', '', 1, ?5, ?5, ?5)
         ON CONFLICT(job_id) DO UPDATE SET
            status = 'processing',
            error = '',
            attempts = attempts + 1,
            started_at = excluded.started_at,
            updated_at = excluded.updated_at",
        params![job_id, job_type.trim(), source_id.trim(), clip_id.trim(), now],
    )?;
    Ok(())
}

pub fn mark_ingest_job_done(
    conn: &Connection,
    job_type: &str,
    source_id: &str,
    clip_id: &str,
) -> rusqlite::Result<()> {
    let now = now_str();
    let job_id = ingest_job_id(job_type, source_id, clip_id);
    conn.execute(
        "INSERT INTO ingest_jobs
            (job_id, job_type, source_id, clip_id, status, error, attempts, queued_at, finished_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'done', '', 0, ?5, ?5, ?5)
         ON CONFLICT(job_id) DO UPDATE SET
            status = 'done',
            error = '',
            finished_at = excluded.finished_at,
            updated_at = excluded.updated_at",
        params![job_id, job_type.trim(), source_id.trim(), clip_id.trim(), now],
    )?;
    Ok(())
}

pub fn mark_ingest_job_error(
    conn: &Connection,
    job_type: &str,
    source_id: &str,
    clip_id: &str,
    error: &str,
) -> rusqlite::Result<()> {
    let now = now_str();
    let job_id = ingest_job_id(job_type, source_id, clip_id);
    let msg = if error.len() > 240 {
        format!("{}...", error.chars().take(240).collect::<String>())
    } else {
        error.to_string()
    };
    conn.execute(
        "INSERT INTO ingest_jobs
            (job_id, job_type, source_id, clip_id, status, error, attempts, queued_at, finished_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'error', ?5, 0, ?6, ?6, ?6)
         ON CONFLICT(job_id) DO UPDATE SET
            status = 'error',
            error = excluded.error,
            finished_at = excluded.finished_at,
            updated_at = excluded.updated_at",
        params![job_id, job_type.trim(), source_id.trim(), clip_id.trim(), msg, now],
    )?;
    Ok(())
}

pub fn reset_processing_ingest_jobs(conn: &Connection) -> rusqlite::Result<usize> {
    let changed = conn.execute(
        "UPDATE ingest_jobs SET status = 'queued', updated_at = ?1 WHERE status = 'processing'",
        params![now_str()],
    )?;
    Ok(changed)
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    conn.prepare(&format!("PRAGMA table_info({table})"))
        .and_then(|mut stmt| {
            stmt.query_map([], |row| row.get::<_, String>(1))
                .map(|rows| rows.filter_map(Result::ok).any(|name| name == column))
        })
        .unwrap_or(false)
}

fn migrate_ingest_metadata_columns(conn: &Connection) -> rusqlite::Result<()> {
    if !column_exists(conn, "ingest_assets", "file_extension") {
        let _ = conn.execute(
            "ALTER TABLE ingest_assets ADD COLUMN file_extension TEXT NOT NULL DEFAULT ''",
            [],
        );
    }
    if !column_exists(conn, "ingest_assets", "poster_source") {
        let _ = conn.execute(
            "ALTER TABLE ingest_assets ADD COLUMN poster_source TEXT NOT NULL DEFAULT ''",
            [],
        );
    }
    if !column_exists(conn, "ingest_assets", "read_from_card") {
        let _ = conn.execute(
            "ALTER TABLE ingest_assets ADD COLUMN read_from_card INTEGER NOT NULL DEFAULT 0",
            [],
        );
    }
    if !column_exists(conn, "ingest_assets", "card_locked") {
        let _ = conn.execute(
            "ALTER TABLE ingest_assets ADD COLUMN card_locked INTEGER NOT NULL DEFAULT 0",
            [],
        );
    }
    let mut stmt = conn.prepare(
        "SELECT source_id, clip_id, metadata_json FROM ingest_assets WHERE metadata_json != '{}'",
    )?;
    let rows: Vec<(String, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    for (source_id, clip_id, raw) in rows {
        let meta = parse_json(&raw, serde_json::json!({}));
        let ext = meta.get("extension").and_then(|v| v.as_str()).unwrap_or("");
        let poster = meta
            .get("poster_source")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let read_from_card = meta
            .get("read_from_card")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let card_locked = meta
            .get("card_locked")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        conn.execute(
            "UPDATE ingest_assets SET
                file_extension = CASE WHEN file_extension = '' THEN ?3 ELSE file_extension END,
                poster_source = CASE WHEN poster_source = '' THEN ?4 ELSE poster_source END,
                read_from_card = CASE WHEN read_from_card = 0 AND ?5 = 1 THEN 1 ELSE read_from_card END,
                card_locked = CASE WHEN card_locked = 0 AND ?6 = 1 THEN 1 ELSE card_locked END,
                source_path = CASE WHEN source_path = '' THEN COALESCE(?7, '') ELSE source_path END,
                original_path = CASE WHEN original_path = '' THEN COALESCE(?8, '') ELSE original_path END,
                proxy_path = CASE WHEN proxy_path = '' THEN COALESCE(?9, '') ELSE proxy_path END,
                card_thumb_path = CASE WHEN card_thumb_path = '' THEN COALESCE(?10, '') ELSE card_thumb_path END,
                metadata_json = '{}'
             WHERE source_id = ?1 AND clip_id = ?2",
            params![
                source_id,
                clip_id,
                ext,
                poster,
                if read_from_card { 1 } else { 0 },
                if card_locked { 1 } else { 0 },
                meta.get("source_path").and_then(|v| v.as_str()).unwrap_or(""),
                meta.get("original_path").and_then(|v| v.as_str()).unwrap_or(""),
                meta.get("proxy_path").and_then(|v| v.as_str()).unwrap_or(""),
                meta.get("card_thumb_path").and_then(|v| v.as_str()).unwrap_or(""),
            ],
        )?;
    }
    if !column_exists(conn, "ingest_assets", "virtual_name") {
        let _ = conn.execute(
            "ALTER TABLE ingest_assets ADD COLUMN virtual_name TEXT NOT NULL DEFAULT ''",
            [],
        );
        backfill_virtual_names_for_selected(conn)?;
    }
    if !column_exists(conn, "ingest_assets", "has_audio") {
        let _ = conn.execute(
            "ALTER TABLE ingest_assets ADD COLUMN has_audio INTEGER NOT NULL DEFAULT 0",
            [],
        );
    }
    if !column_exists(conn, "ingest_assets", "audio_channels") {
        let _ = conn.execute(
            "ALTER TABLE ingest_assets ADD COLUMN audio_channels INTEGER NOT NULL DEFAULT 0",
            [],
        );
    }
    if !column_exists(conn, "ingest_assets", "field_order") {
        let _ = conn.execute(
            "ALTER TABLE ingest_assets ADD COLUMN field_order TEXT NOT NULL DEFAULT ''",
            [],
        );
    }
    if !column_exists(conn, "ingest_assets", "interlaced") {
        let _ = conn.execute(
            "ALTER TABLE ingest_assets ADD COLUMN interlaced INTEGER NOT NULL DEFAULT 0",
            [],
        );
    }
    if !column_exists(conn, "ingest_assets", "source_class") {
        let _ = conn.execute(
            "ALTER TABLE ingest_assets ADD COLUMN source_class TEXT NOT NULL DEFAULT ''",
            [],
        );
    }
    if !column_exists(conn, "ingest_assets", "proxy_recipe") {
        let _ = conn.execute(
            "ALTER TABLE ingest_assets ADD COLUMN proxy_recipe TEXT NOT NULL DEFAULT ''",
            [],
        );
    }
    Ok(())
}

/// Assign editorial virtual file names for selected ingest rows (idempotent).
pub fn backfill_virtual_names_for_selected(conn: &Connection) -> rusqlite::Result<usize> {
    use crate::media::virtual_name_for_root_clip;

    let mut stmt = conn.prepare(
        "SELECT source_id, clip_id, file_extension FROM ingest_assets
         WHERE selected != 0 AND TRIM(COALESCE(virtual_name, '')) = ''",
    )?;
    let rows: Vec<(String, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    let mut updated = 0usize;
    for (source_id, clip_id, ext) in rows {
        let name = virtual_name_for_root_clip(&clip_id, &ext);
        if name.is_empty() {
            continue;
        }
        updated += conn.execute(
            "UPDATE ingest_assets SET virtual_name = ?3 WHERE source_id = ?1 AND clip_id = ?2",
            params![source_id, clip_id, name],
        )?;
    }
    Ok(updated)
}

fn migrate_thumb_columns(conn: &Connection) -> rusqlite::Result<()> {
    let _ = conn.execute(
        "ALTER TABLE ingest_assets ADD COLUMN thumb_status TEXT NOT NULL DEFAULT 'pending'",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE ingest_assets ADD COLUMN thumb_error TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE ingest_assets ADD COLUMN source_path TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE ingest_assets ADD COLUMN original_path TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE ingest_assets ADD COLUMN proxy_path TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE ingest_assets ADD COLUMN project_proxy_path TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE ingest_assets ADD COLUMN thumb_path TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE ingest_assets ADD COLUMN card_thumb_path TEXT NOT NULL DEFAULT ''",
        [],
    );
    Ok(())
}

pub fn set_thumb_status(
    conn: &Connection,
    source_id: &str,
    clip_id: &str,
    status: &str,
    error: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE ingest_assets SET thumb_status = ?3, thumb_error = ?4
         WHERE source_id = ?1 AND clip_id = ?2",
        params![source_id, clip_id, status, error],
    )?;
    Ok(())
}

pub fn set_thumb_ready_path(
    conn: &Connection,
    source_id: &str,
    clip_id: &str,
    path: &Path,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE ingest_assets SET thumb_status = 'ready', thumb_error = '', thumb_path = ?3
         WHERE source_id = ?1 AND clip_id = ?2",
        params![source_id, clip_id, path.to_string_lossy()],
    )?;
    Ok(())
}

#[allow(dead_code)]
pub fn stored_thumbnail_path(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
) -> rusqlite::Result<Option<PathBuf>> {
    Ok(resolve_ingest_poster_path(paths, project_id, clip_id))
}

/// First existing poster file for a clip (ingest thumb, default path, or card THM/JPG).
pub fn resolve_ingest_poster_path(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
) -> Option<PathBuf> {
    let conn = open_ingest(paths, project_id).ok()?;
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT thumb_path, card_thumb_path FROM ingest_assets
             WHERE clip_id = ?1
             ORDER BY source_id
             LIMIT 1",
            params![clip_id.trim()],
            |r| {
                Ok((
                    r.get::<_, String>(0).unwrap_or_default(),
                    r.get::<_, String>(1).unwrap_or_default(),
                ))
            },
        )
        .ok();
    let Some((thumb_path, card_thumb_path)) = row else {
        let default = thumbnail_path(paths, project_id, clip_id);
        return poster_exists(&default).then_some(default);
    };
    let stored = thumb_path.trim();
    if !stored.is_empty() {
        let path = PathBuf::from(stored);
        if poster_exists(&path) {
            return Some(path);
        }
    }
    let default = thumbnail_path(paths, project_id, clip_id);
    if poster_exists(&default) {
        return Some(default);
    }
    let card = card_thumb_path.trim();
    if !card.is_empty() {
        let path = PathBuf::from(card);
        if poster_exists(&path) {
            return Some(path);
        }
    }
    None
}

pub fn get_meta(conn: &Connection, key: &str, default: &str) -> rusqlite::Result<String> {
    let row: Option<String> = conn
        .query_row(
            "SELECT value FROM ingest_meta WHERE key = ?1",
            params![key],
            |r| r.get(0),
        )
        .ok();
    Ok(row.unwrap_or_else(|| default.to_string()))
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO ingest_meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn parse_json(raw: &str, fallback: Value) -> Value {
    serde_json::from_str(raw).unwrap_or(fallback)
}

pub struct IngestAssetMetaInput {
    pub source_path: String,
    pub original_path: String,
    pub proxy_path: String,
    pub project_proxy_path: String,
    pub card_thumb_path: String,
    pub file_extension: String,
    pub read_from_card: bool,
    pub card_locked: bool,
    pub poster_source: String,
}

pub fn ingest_asset_meta(input: &IngestAssetMetaInput) -> Value {
    let mut obj = serde_json::Map::new();
    for (key, val) in [
        ("source_path", input.source_path.as_str()),
        ("original_path", input.original_path.as_str()),
        ("proxy_path", input.proxy_path.as_str()),
        ("project_proxy_path", input.project_proxy_path.as_str()),
        ("card_thumb_path", input.card_thumb_path.as_str()),
        ("extension", input.file_extension.as_str()),
        ("poster_source", input.poster_source.as_str()),
    ] {
        if !val.trim().is_empty() {
            obj.insert(key.into(), Value::String(val.to_string()));
        }
    }
    if input.read_from_card {
        obj.insert("read_from_card".into(), Value::Bool(true));
    }
    if input.card_locked {
        obj.insert("card_locked".into(), Value::Bool(true));
    }
    Value::Object(obj)
}

pub fn thumbnail_path(paths: &ProjectPaths, project_id: &str, clip_id: &str) -> PathBuf {
    ingest_dir(paths, project_id)
        .join("thumbnails")
        .join(sanitize_name(clip_id))
        .join("poster.jpg")
}

pub fn thumbnail_url(project_id: &str, clip_id: &str) -> String {
    format!(
        "/api/ingest/thumbnail?project_id={}&clip_id={}",
        urlencoding(project_id),
        urlencoding(clip_id)
    )
}

fn urlencoding(raw: &str) -> String {
    raw.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            _ => format!("%{:02X}", c as u8),
        })
        .collect()
}

fn sanitize_name(raw: &str) -> String {
    let mut out: String = raw
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
    if out.is_empty() {
        out = "clip".into();
    }
    out
}

pub fn poster_exists(path: &Path) -> bool {
    path.is_file() && path.metadata().map(|m| m.len() > 0).unwrap_or(false)
}

/// Kopija THM/JPG s kartice u ingest poster (bez ffmpeg).
pub fn copy_card_image_to_poster(src: &Path, dest: &Path) -> std::io::Result<()> {
    if !src.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("THM/JPG ne postoji: {}", src.display()),
        ));
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(src, dest)?;
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

    #[test]
    fn unknown_project_does_not_create_orphan_ingest_dir() {
        let base = std::env::temp_dir().join(format!(
            "qnc_ingest_orphan_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        fs::create_dir_all(&paths.projects_root).unwrap();
        fs::create_dir_all(&paths.data_dir).unwrap();

        let pid = "ghost_deleted_project";
        // Strict gate (production reject) — mkdir helpers must not create orphans.
        assert!(crate::project::db::open_project_strict(&paths, pid).is_err());
        assert!(ensure_ingest_dirs(&paths, pid).is_err());
        assert!(
            !ingest_dir(&paths, pid).exists(),
            "orphan ingest/ must not be created for unknown project_id"
        );
        assert!(
            !paths.project_dir(pid).exists(),
            "orphan project dir must not be created for unknown project_id"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn ingest_job_requeue_resets_done_status() {
        let base =
            std::env::temp_dir().join(format!("qnc_ingest_jobs_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let conn = open_ingest(&paths, "job_proj").unwrap();

        queue_ingest_job(&conn, "import", "local", "clip_a").unwrap();
        mark_ingest_job_processing(&conn, "import", "local", "clip_a").unwrap();
        mark_ingest_job_done(&conn, "import", "local", "clip_a").unwrap();
        queue_ingest_job(&conn, "import", "local", "clip_a").unwrap();

        let status: String = conn
            .query_row(
                "SELECT status FROM ingest_jobs WHERE job_id = ?1",
                params![ingest_job_id("import", "local", "clip_a")],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "queued");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn backfill_virtual_names_for_selected_assigns_root_names() {
        let base = std::env::temp_dir().join(format!(
            "qnc_ingest_virtual_name_test_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let conn = open_ingest(&paths, "virt_proj").unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, file_extension, selected, import_status, status)
             VALUES ('local', 'mironik_1483', 'mironik_1483', 'mironik_1483', 'mxf', 1, 'detected', 'on_source')",
            [],
        )
        .unwrap();
        let updated = backfill_virtual_names_for_selected(&conn).unwrap();
        assert_eq!(updated, 1);
        let name: String = conn
            .query_row(
                "SELECT virtual_name FROM ingest_assets WHERE clip_id = 'mironik_1483'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(name, "mironik_1483_root.mxf");
        let _ = fs::remove_dir_all(&base);
    }
}
