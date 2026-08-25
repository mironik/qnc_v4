use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::project::db::{now_str, open_project_strict, ProjectPaths};

#[derive(Clone, Debug)]
pub struct FilmstripFrame {
    pub index: usize,
    pub seek_sec: f64,
    pub path: PathBuf,
}

fn safe_name(value: &str) -> String {
    let mut out: String = value
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
    if out.len() > 120 {
        out.truncate(120);
    }
    if out.is_empty() {
        "clip".into()
    } else {
        out
    }
}

pub fn filmstrip_root(paths: &ProjectPaths, project_id: &str) -> PathBuf {
    paths.project_dir(project_id).join("filmstrip")
}

fn open_db(paths: &ProjectPaths, project_id: &str) -> Result<Connection, String> {
    // Validate / open project DB first — never mkdir for unknown project_id.
    let conn = open_project_strict(paths, project_id).map_err(|e| e.to_string())?;
    let root = filmstrip_root(paths, project_id);
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS filmstrips (
            clip_id TEXT PRIMARY KEY,
            status TEXT NOT NULL DEFAULT 'missing',
            duration_sec REAL NOT NULL DEFAULT 0,
            frame_count INTEGER NOT NULL DEFAULT 0,
            error TEXT NOT NULL DEFAULT '',
            built_at TEXT,
            updated_at TEXT
        );
        CREATE TABLE IF NOT EXISTS filmstrip_frames (
            clip_id TEXT NOT NULL,
            frame_index INTEGER NOT NULL,
            seek_sec REAL NOT NULL,
            path TEXT NOT NULL,
            updated_at TEXT,
            PRIMARY KEY (clip_id, frame_index)
        );
        CREATE INDEX IF NOT EXISTS idx_filmstrip_frames_clip ON filmstrip_frames(clip_id);",
    )
    .map_err(|e| e.to_string())?;
    Ok(conn)
}

#[allow(dead_code)]
pub fn bootstrap_schema(paths: &ProjectPaths, project_id: &str) -> Result<(), String> {
    open_db(paths, project_id).map(|_| ())
}

pub fn filmstrip_clip_dir(paths: &ProjectPaths, project_id: &str, clip_id: &str) -> PathBuf {
    let dir = filmstrip_root(paths, project_id).join(safe_name(clip_id));
    // Never create under an unknown/deleted project.
    if paths
        .project_dir(project_id)
        .join("qnc_project.db")
        .exists()
    {
        fs::create_dir_all(&dir).ok();
    }
    dir
}

fn filmstrip_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "clip_id": row.get::<_, String>("clip_id")?,
        "status": row.get::<_, String>("status")?,
        "duration_sec": row.get::<_, f64>("duration_sec")?,
        "frame_count": row.get::<_, i64>("frame_count")?,
        "error": row.get::<_, String>("error")?,
        "built_at": row.get::<_, Option<String>>("built_at")?,
        "updated_at": row.get::<_, Option<String>>("updated_at")?,
    }))
}

fn url_encode_component(raw: &str) -> String {
    raw.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Snapshot jednog klipa za API — `filmstrip_frames` iz SQLite, ne generirano u JS-u.
///
/// `api_prefix` = namespace za thumbnail URL (npr. `/api/story`, `/api/qstory`).
pub fn clip_filmstrip_snapshot(paths: &ProjectPaths, project_id: &str, clip_id: &str) -> Value {
    clip_filmstrip_snapshot_for_api(paths, project_id, clip_id, "/api/story")
}

pub fn manifest_for_api(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    api_prefix: &str,
) -> Value {
    let pid = project_id.trim();
    let clip = clip_id.trim();
    let snapshot = clip_filmstrip_snapshot_for_api(paths, pid, clip, api_prefix);
    let filmstrip = get_filmstrip(paths, pid, clip).unwrap_or_else(|| {
        json!({
            "clip_id": clip,
            "status": "missing",
            "duration_sec": 0,
            "frame_count": 0,
            "error": "",
        })
    });
    json!({
        "project_id": pid,
        "clip_id": clip,
        "filmstrip": filmstrip,
        "frames": snapshot
            .get("filmstrip_frames")
            .cloned()
            .unwrap_or_else(|| json!(crate::filmstrip::pad_frames_to_default_with_placeholder(
                Vec::new(),
                &crate::filmstrip::placeholder_url_for_api(api_prefix.trim().trim_end_matches('/'))
            ))),
    })
}

pub fn manifest_from_frames_for_api(
    project_id: &str,
    clip_id: &str,
    filmstrip: Value,
    frames: &[FilmstripFrame],
    api_prefix: &str,
) -> Value {
    let pid = project_id.trim();
    let clip = clip_id.trim();
    let api = api_prefix.trim().trim_end_matches('/');
    let api = if api.is_empty() { "/api/story" } else { api };
    let ph = crate::filmstrip::placeholder_url_for_api(api);
    let version = filmstrip
        .get("updated_at")
        .or_else(|| filmstrip.get("built_at"))
        .and_then(Value::as_str)
        .map(url_encode_component)
        .unwrap_or_default();
    let mut out = json!({
        "filmstrip_status": filmstrip
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("missing"),
        "filmstrip_frames": crate::filmstrip::pad_frames_to_default_with_placeholder(Vec::new(), &ph),
    });
    if let Some(err) = filmstrip
        .get("error")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        out["filmstrip_error"] = json!(err);
    }
    let filmstrip_frames: Vec<Value> = frames
        .iter()
        .filter(|frame| frame_file_valid(&frame.path))
        .map(|frame| {
            json!({
                "index": frame.index,
                "frame_index": frame.index,
                "seek_sec": frame.seek_sec,
                "url": format!(
                    "{api}/thumbnail?clip_id={}&frame_index={}&project_id={}&v={}",
                    url_encode_component(clip),
                    frame.index,
                    url_encode_component(pid),
                    version,
                ),
            })
        })
        .collect();
    if !filmstrip_frames.is_empty() {
        out["filmstrip_frames"] = json!(crate::filmstrip::pad_frames_to_default_with_placeholder(
            filmstrip_frames,
            &ph
        ));
        out["timeline_seeks"] = json!(frames
            .iter()
            .filter(|frame| frame_file_valid(&frame.path))
            .map(|frame| frame.seek_sec)
            .collect::<Vec<_>>());
    }
    if let Some(duration) = filmstrip.get("duration_sec") {
        out["timeline_duration_sec"] = duration.clone();
    }
    json!({
        "project_id": pid,
        "clip_id": clip,
        "filmstrip": filmstrip,
        "frames": out["filmstrip_frames"].clone(),
    })
}

pub fn manifest_from_status_for_api(
    project_id: &str,
    clip_id: &str,
    status: &str,
    error: &str,
    api_prefix: &str,
) -> Value {
    manifest_from_frames_for_api(
        project_id,
        clip_id,
        json!({
            "clip_id": clip_id.trim(),
            "status": status,
            "duration_sec": 0,
            "frame_count": 0,
            "error": error,
            "built_at": Value::Null,
            "updated_at": Value::Null,
        }),
        &[],
        api_prefix,
    )
}

pub fn manifest_cache_key(clip_id: &str, api_prefix: &str) -> String {
    format!(
        "filmstrip_manifest:{}:{}",
        api_prefix.trim().trim_end_matches('/'),
        clip_id.trim()
    )
}

pub fn clip_filmstrip_snapshot_for_api(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    api_prefix: &str,
) -> Value {
    let clip_id = clip_id.trim();
    let api = api_prefix.trim().trim_end_matches('/');
    let api = if api.is_empty() { "/api/story" } else { api };
    let ph = crate::filmstrip::placeholder_url_for_api(api);
    let ph_frames = || crate::filmstrip::pad_frames_to_default_with_placeholder(Vec::new(), &ph);
    if clip_id.is_empty() {
        return json!({
            "filmstrip_status": "missing",
            "filmstrip_frames": ph_frames(),
        });
    }
    let Some(fs) = get_filmstrip(paths, project_id, clip_id) else {
        return json!({
            "filmstrip_status": "missing",
            "filmstrip_frames": ph_frames(),
        });
    };
    let status = fs
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("missing");
    let version = fs
        .get("updated_at")
        .or_else(|| fs.get("built_at"))
        .and_then(Value::as_str)
        .map(url_encode_component)
        .unwrap_or_default();
    let mut out = json!({
        "filmstrip_status": status,
        "filmstrip_frames": ph_frames(),
    });
    if let Some(err) = fs
        .get("error")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        out["filmstrip_error"] = json!(err);
    }
    if let Ok(frames) = list_frames_for_clip(paths, project_id, clip_id) {
        if !frames.is_empty() {
            let filmstrip_frames: Vec<Value> = frames
                .iter()
                .map(|f| {
                    let index = f.get("index").and_then(|v| v.as_i64()).unwrap_or(0);
                    json!({
                        "index": index,
                        "frame_index": index,
                        "seek_sec": f.get("seek_sec").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        "url": format!(
                            "{api}/thumbnail?clip_id={}&frame_index={}&project_id={}&v={}",
                            url_encode_component(clip_id),
                            index,
                            url_encode_component(project_id),
                            version,
                        ),
                    })
                })
                .collect();
            out["filmstrip_frames"] = json!(
                crate::filmstrip::pad_frames_to_default_with_placeholder(filmstrip_frames, &ph)
            );
            let seeks: Vec<Value> = frames
                .iter()
                .map(|f| json!(f.get("seek_sec").and_then(|v| v.as_f64()).unwrap_or(0.0)))
                .collect();
            out["timeline_seeks"] = json!(seeks);
        }
    }
    if let Some(dur) = fs.get("duration_sec") {
        out["timeline_duration_sec"] = dur.clone();
    }
    out
}

pub fn get_filmstrip(paths: &ProjectPaths, project_id: &str, clip_id: &str) -> Option<Value> {
    let conn = open_db(paths, project_id).ok()?;
    conn.query_row(
        "SELECT clip_id, status, duration_sec, frame_count, error, built_at, updated_at
         FROM filmstrips WHERE clip_id = ?1",
        params![clip_id],
        filmstrip_row,
    )
    .ok()
}

pub fn list_frames_for_clip(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
) -> Result<Vec<Value>, String> {
    let conn = open_db(paths, project_id)?;
    let rows = {
        let mut stmt = conn
            .prepare(
                "SELECT frame_index, seek_sec, path FROM filmstrip_frames
                 WHERE clip_id = ?1 ORDER BY frame_index",
            )
            .map_err(|e| e.to_string())?;
        let mapped = stmt
            .query_map(params![clip_id], |row| {
                Ok(json!({
                    "index": row.get::<_, i64>(0)?,
                    "seek_sec": row.get::<_, f64>(1)?,
                    "path": row.get::<_, String>(2)?,
                }))
            })
            .map_err(|e| e.to_string())?;
        mapped
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };
    Ok(rows)
}

fn frame_file_valid(path: &Path) -> bool {
    path.is_file() && path.metadata().map(|m| m.len()).unwrap_or(0) > 0
}

fn write_frames(
    conn: &Connection,
    clip_id: &str,
    frames: &[FilmstripFrame],
    now: &str,
) -> Result<(), String> {
    conn.execute(
        "DELETE FROM filmstrip_frames WHERE clip_id = ?1",
        params![clip_id],
    )
    .map_err(|e| e.to_string())?;
    for frame in frames {
        if !frame_file_valid(&frame.path) {
            continue;
        }
        let path = frame.path.to_string_lossy().into_owned();
        conn.execute(
            "INSERT INTO filmstrip_frames (clip_id, frame_index, seek_sec, path, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![clip_id, frame.index as i64, frame.seek_sec, path, now],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn mark_filmstrip(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    status: &str,
    error: &str,
) -> Result<(), String> {
    let conn = open_db(paths, project_id)?;
    let now = now_str();
    conn.execute(
        "INSERT INTO filmstrips (clip_id, status, error, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(clip_id) DO UPDATE SET
            status = excluded.status,
            error = excluded.error,
            updated_at = excluded.updated_at",
        params![clip_id, status, error, now],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn save_filmstrip(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    duration_sec: f64,
    frames: &[FilmstripFrame],
    error: &str,
) -> Result<Value, String> {
    let status = if frames.iter().any(|f| frame_file_valid(&f.path)) {
        "ready"
    } else {
        "error"
    };
    save_filmstrip_with_status(
        paths,
        project_id,
        clip_id,
        duration_sec,
        frames,
        error,
        status,
    )
}

pub(super) fn save_filmstrip_progress(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    duration_sec: f64,
    frames: &[FilmstripFrame],
    error: &str,
) -> Result<Value, String> {
    save_filmstrip_with_status(
        paths,
        project_id,
        clip_id,
        duration_sec,
        frames,
        error,
        "building",
    )
}

fn save_filmstrip_with_status(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    duration_sec: f64,
    frames: &[FilmstripFrame],
    error: &str,
    status: &str,
) -> Result<Value, String> {
    let valid: Vec<FilmstripFrame> = frames
        .iter()
        .filter(|f| frame_file_valid(&f.path))
        .cloned()
        .collect();
    let frame_count = valid.len() as i64;
    let status = if frame_count > 0 { status } else { "error" };
    let conn = open_db(paths, project_id)?;
    let now = now_str();
    let built_at = if status == "ready" {
        now.clone()
    } else {
        String::new()
    };
    conn.execute(
        "INSERT INTO filmstrips
            (clip_id, status, duration_sec, frame_count, error, built_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(clip_id) DO UPDATE SET
            status = excluded.status,
            duration_sec = excluded.duration_sec,
            frame_count = excluded.frame_count,
            error = excluded.error,
            built_at = excluded.built_at,
            updated_at = excluded.updated_at",
        params![
            clip_id,
            status,
            duration_sec,
            frame_count,
            error,
            built_at,
            now,
        ],
    )
    .map_err(|e| e.to_string())?;
    if frame_count > 0 {
        write_frames(&conn, clip_id, &valid, &now)?;
    }
    Ok(json!({
        "clip_id": clip_id,
        "status": status,
        "duration_sec": duration_sec,
        "frame_count": frame_count,
        "error": error,
        "built_at": if built_at.is_empty() { Value::Null } else { json!(built_at) },
        "updated_at": now,
    }))
}

pub fn frame_path_for_seek(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    seek: f64,
) -> Option<PathBuf> {
    let fs = get_filmstrip(paths, project_id, clip_id)?;
    if fs.get("status").and_then(|v| v.as_str()) != Some("ready") {
        return None;
    }
    let frames = list_frames_for_clip(paths, project_id, clip_id).ok()?;
    let mut best_path: Option<String> = None;
    let mut best_diff = f64::MAX;
    for fr in frames {
        let sec = fr.get("seek_sec").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let path = fr.get("path").and_then(|v| v.as_str()).unwrap_or("");
        if path.is_empty() {
            continue;
        }
        let diff = (sec - seek).abs();
        if diff < best_diff {
            best_diff = diff;
            best_path = Some(path.to_string());
        }
    }
    best_path.map(PathBuf::from).filter(|p| p.is_file())
}

pub fn frame_path_for_index(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    frame_index: i64,
) -> Option<PathBuf> {
    let conn = open_db(paths, project_id).ok()?;
    conn.query_row(
        "SELECT path FROM filmstrip_frames WHERE clip_id = ?1 AND frame_index = ?2",
        params![clip_id, frame_index],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .map(PathBuf::from)
    .filter(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::db::{
        ensure_project_dirs_at, open_global, open_project, project_dir_in_root,
    };
    use std::io::Write;

    fn test_paths(base: &Path) -> ProjectPaths {
        ProjectPaths {
            data_dir: base.join("data"),
            projects_root: base.join("projects"),
            seed_path: base.join("seed.json"),
        }
    }

    fn register_project(paths: &ProjectPaths, project_id: &str) {
        let global = open_global(paths).unwrap();
        let project_dir = project_dir_in_root(&paths.projects_root, project_id);
        global
            .execute(
                "INSERT INTO projects (project_id, name, project_dir)
                 VALUES (?1, ?2, ?3)",
                params![
                    project_id,
                    project_id,
                    project_dir.to_string_lossy().to_string()
                ],
            )
            .unwrap();
        ensure_project_dirs_at(&project_dir).unwrap();
        let _ = open_project(paths, project_id).unwrap();
    }

    #[test]
    fn unknown_project_does_not_create_orphan_filmstrip_dir() {
        let base = std::env::temp_dir().join(format!(
            "qnc_filmstrip_orphan_{}_{}",
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

        let pid = "ghost_filmstrip_project";
        assert!(open_project_strict(&paths, pid).is_err());
        let _ = filmstrip_clip_dir(&paths, pid, "clip");
        assert!(
            !filmstrip_root(&paths, pid).exists(),
            "orphan filmstrip/ must not be created for unknown project_id"
        );
        assert!(
            !paths.project_dir(pid).exists(),
            "orphan project dir must not be created for unknown project_id"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn save_filmstrip_writes_frames_to_db() {
        let base = std::env::temp_dir().join(format!("qnc_filmstrip_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "test_proj";
        let clip_id = "clip_a";
        register_project(&paths, project_id);
        let dir = filmstrip_clip_dir(&paths, project_id, clip_id);
        let frame_path = dir.join("000_1_50.jpg");
        {
            let mut f = fs::File::create(&frame_path).unwrap();
            f.write_all(b"fake-jpeg").unwrap();
        }
        let frames = vec![FilmstripFrame {
            index: 0,
            seek_sec: 1.5,
            path: frame_path.clone(),
        }];
        save_filmstrip(&paths, project_id, clip_id, 10.0, &frames, "").unwrap();
        let stored = list_frames_for_clip(&paths, project_id, clip_id).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(
            stored[0].get("path").and_then(|v| v.as_str()),
            Some(frame_path.to_string_lossy().as_ref())
        );
        let fs = get_filmstrip(&paths, project_id, clip_id).unwrap();
        assert_eq!(fs.get("status").and_then(|v| v.as_str()), Some("ready"));
        let _ = fs::remove_dir_all(&base);
    }
}
