use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use super::db::{
    ensure_project_dirs, ensure_project_store, get_setting, now_str, project_dir_in_root,
    set_setting, slug_id, ProjectPaths,
};

pub fn get_active_project_id(conn: &Connection) -> rusqlite::Result<String> {
    ensure_project_store(conn)?;
    let active = get_setting(conn, "active_project_id", "")?;
    let active = active.trim();
    if active.is_empty() {
        return Ok(String::new());
    }
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM projects WHERE project_id = ?1",
        params![active],
        |r| r.get(0),
    )?;
    if exists > 0 {
        Ok(active.to_string())
    } else {
        Ok(String::new())
    }
}

pub fn set_active_project_id(conn: &Connection, project_id: &str) -> rusqlite::Result<()> {
    let pid = project_id.trim();
    if pid.is_empty() {
        set_setting(conn, "active_project_id", "")?;
        return Ok(());
    }
    set_setting(conn, "active_project_id", pid)?;
    Ok(())
}

pub fn record_project_opened(conn: &Connection, project_id: &str) -> rusqlite::Result<()> {
    let pid = project_id.trim();
    if pid.is_empty() {
        return Ok(());
    }
    let now = now_str();
    conn.execute(
        "UPDATE projects SET last_opened_at = ?2, updated_at = ?2 WHERE project_id = ?1",
        params![pid, now],
    )?;
    Ok(())
}

pub fn list_projects(conn: &Connection) -> rusqlite::Result<Vec<Value>> {
    ensure_project_store(conn)?;
    let mut stmt = conn.prepare(
        "SELECT project_id, name, project_dir, created_at, last_opened_at FROM projects ORDER BY project_id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(json!({
            "project_id": r.get::<_, String>(0)?,
            "name": r.get::<_, String>(1)?,
            "project_dir": r.get::<_, Option<String>>(2)?,
            "created_at": r.get::<_, Option<String>>(3)?,
            "last_opened_at": r.get::<_, Option<String>>(4)?,
        }))
    })?;
    rows.collect()
}

pub fn project_row(conn: &Connection, project_id: &str) -> rusqlite::Result<Option<Value>> {
    ensure_project_store(conn)?;
    let pid = project_id.trim();
    if pid.is_empty() {
        return Ok(None);
    }
    let row = conn.query_row(
        "SELECT project_id, name, project_dir, created_at, last_opened_at
         FROM projects
         WHERE project_id = ?1",
        params![pid],
        |r| {
            Ok(json!({
                "project_id": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "project_dir": r.get::<_, Option<String>>(2)?,
                "created_at": r.get::<_, Option<String>>(3)?,
                "last_opened_at": r.get::<_, Option<String>>(4)?,
            }))
        },
    );
    match row {
        Ok(value) => Ok(Some(value)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn list_project_ids(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    ensure_project_store(conn)?;
    let mut stmt = conn.prepare("SELECT project_id FROM projects ORDER BY project_id")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.collect()
}

pub fn upsert_project_meta(
    conn: &Connection,
    project_id: &str,
    name: &str,
    created_at: Option<&str>,
    project_dir: Option<&Path>,
) -> rusqlite::Result<()> {
    let now = now_str();
    let existing: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT created_at, project_dir FROM projects WHERE project_id = ?1",
            params![project_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let existing_created = existing.as_ref().and_then(|v| v.0.clone());
    let existing_dir = existing.and_then(|v| v.1);
    let created = created_at
        .map(str::to_string)
        .or(existing_created)
        .unwrap_or_else(|| now.clone());
    let dir = project_dir
        .map(|p| p.to_string_lossy().to_string())
        .or(existing_dir)
        .unwrap_or_default();
    conn.execute(
        "INSERT INTO projects (project_id, name, project_dir, created_at, updated_at, last_opened_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5)
         ON CONFLICT(project_id) DO UPDATE SET
            name = excluded.name,
            project_dir = CASE
                WHEN excluded.project_dir IS NULL OR excluded.project_dir = '' THEN projects.project_dir
                ELSE excluded.project_dir
            END,
            updated_at = excluded.updated_at",
        params![project_id, name, dir, created, now],
    )?;
    Ok(())
}

pub fn delete_project_row(conn: &Connection, project_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM projects WHERE project_id = ?",
        params![project_id],
    )?;
    Ok(())
}

pub fn create_project(
    conn: &Connection,
    paths: &ProjectPaths,
    name: Option<&str>,
) -> rusqlite::Result<Value> {
    let list = list_projects(conn)?;
    let label = name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("Projekt {}", list.len() + 1));
    let project_id = slug_id(&label);
    let created_at = now_str();
    let project_dir = project_dir_in_root(&paths.projects_root, &project_id);
    upsert_project_meta(
        conn,
        &project_id,
        &label,
        Some(&created_at),
        Some(&project_dir),
    )?;
    ensure_project_dirs(paths, &project_id).ok();
    record_project_opened(conn, &project_id)?;
    set_active_project_id(conn, &project_id)?;
    project_row(conn, &project_id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
}

pub fn open_project(
    conn: &Connection,
    _paths: &ProjectPaths,
    project_id: &str,
) -> rusqlite::Result<Option<Value>> {
    let pid = project_id.trim();
    for p in list_projects(conn)? {
        if p.get("project_id").and_then(|v| v.as_str()) == Some(pid) {
            set_active_project_id(conn, pid)?;
            record_project_opened(conn, pid)?;
            return project_row(conn, pid);
        }
    }
    Ok(None)
}

/// Uklanja projekte: red u globalnoj bazi + cijeli projektni direktorij
/// (isti folder koji kreiranje stvara — ne ostavlja prazan naziv).
#[cfg(test)]
mod delete_tests {
    use super::*;
    use crate::project::db::{
        ensure_project_dirs_at, open_global, project_dir_from_conn, project_dir_in_root,
        ProjectPaths,
    };
    use std::path::PathBuf;

    #[test]
    fn delete_projects_removes_folder_and_name() {
        let base = std::env::temp_dir().join(format!(
            "qnc_delete_proj_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let paths = ProjectPaths {
            data_dir: base.join("data"),
            projects_root: base.join("projects"),
            seed_path: PathBuf::from("nonexistent"),
        };
        let conn = open_global(&paths).unwrap();
        let project_id = "brisanje_test_1";
        let project_dir = project_dir_in_root(&paths.projects_root, project_id);
        upsert_project_meta(&conn, project_id, "Brisanje Test", None, Some(&project_dir)).unwrap();
        ensure_project_dirs_at(&project_dir).unwrap();
        fs::write(project_dir.join("marker.txt"), b"x").unwrap();
        assert!(project_dir.exists());

        let (removed, _) = delete_projects(&conn, &paths, &[project_id.to_string()]).unwrap();
        assert_eq!(removed, vec![project_id.to_string()]);
        assert!(!project_dir.exists(), "project folder must be gone");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM projects WHERE project_id = ?1",
                params![project_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
        let _ = project_dir_from_conn(&conn, &paths, project_id);
        let _ = fs::remove_dir_all(&base);
    }
}

/// Uklanja projekte: red u globalnoj bazi + cijeli projektni direktorij
/// (isti folder koji kreiranje stvara — ne ostavlja prazan naziv).
pub fn delete_projects(
    conn: &Connection,
    paths: &ProjectPaths,
    project_ids: &[String],
) -> rusqlite::Result<(Vec<String>, Vec<String>)> {
    use super::db::project_dir_from_conn;

    let remove: Vec<String> = project_ids
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if remove.is_empty() {
        return Ok((vec![], vec![]));
    }
    let before = list_projects(conn)?;
    let after: Vec<Value> = before
        .iter()
        .filter(|p| {
            let id = p.get("project_id").and_then(|v| v.as_str()).unwrap_or("");
            !remove.iter().any(|r| r == id)
        })
        .cloned()
        .collect();

    // Clear active first so workers / open_project stop targeting deleted ids.
    let mut active = get_active_project_id(conn)?;
    if !active.is_empty() && remove.iter().any(|r| r == &active) {
        let next = after
            .first()
            .and_then(|p| p.get("project_id").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();
        set_active_project_id(conn, &next)?;
        active = next;
    }

    let mut leftovers = Vec::new();
    let mut removed = Vec::new();
    for pid in &remove {
        let dir = project_dir_from_conn(conn, paths, pid);
        let fallback = project_dir_in_root(&paths.projects_root, pid);

        // Drop DB row first so open_project / ensure_project_dirs cannot recreate the folder.
        delete_project_row(conn, pid)?;
        removed.push(pid.clone());

        for target in unique_existing_dirs(&[dir, fallback]) {
            match remove_project_dir(&target) {
                Ok(()) => {}
                Err(e) => {
                    if target.exists() {
                        leftovers.push(format!("{} ({})", target.display(), e));
                    }
                }
            }
        }
    }

    if active.is_empty() || remove.iter().any(|r| r == &active) {
        let next = after
            .first()
            .and_then(|p| p.get("project_id").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();
        set_active_project_id(conn, &next)?;
    }

    if !leftovers.is_empty() {
        let msg = format!(
            "Projekt obrisan iz baze, ali folder još postoji (datoteke zaključane): {}. Zatvori QNC host i pokreni cleanup orphans ili obriši ručno.",
            leftovers.join("; ")
        );
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
            Some(msg),
        ));
    }
    Ok((removed, vec![]))
}

fn unique_existing_dirs(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in dirs {
        if !dir.exists() {
            continue;
        }
        if out.iter().any(|d: &PathBuf| d == dir) {
            continue;
        }
        out.push(dir.clone());
    }
    out
}

fn remove_project_dir(dir: &Path) -> Result<(), String> {
    if !dir.exists() {
        return Ok(());
    }
    // Rename first so the project name disappears from Projekti/ even if a locked
    // file delays full remove_dir_all.
    let parent = dir.parent().unwrap_or(dir);
    let trash_name = format!(
        ".deleting_{}_{}",
        dir.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "project".into()),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    );
    let trash = parent.join(&trash_name);
    let remove_target = if fs::rename(dir, &trash).is_ok() {
        trash
    } else {
        dir.to_path_buf()
    };

    const ATTEMPTS: usize = 8;
    let mut last_err = String::new();
    for attempt in 0..ATTEMPTS {
        clear_readonly_recursive(&remove_target);
        match fs::remove_dir_all(&remove_target) {
            Ok(()) => {
                if !remove_target.exists() && !dir.exists() {
                    return Ok(());
                }
                last_err = format!(
                    "Folder još postoji nakon brisanja: {}",
                    remove_target.display()
                );
            }
            Err(e) => {
                last_err = format!("Ne mogu obrisati {}: {}", remove_target.display(), e);
            }
        }
        if attempt + 1 < ATTEMPTS {
            thread::sleep(Duration::from_millis(250 * (attempt as u64 + 1)));
        }
    }
    Err(last_err)
}

#[cfg(windows)]
fn clear_readonly_recursive(root: &Path) {
    fn walk(path: &Path) {
        let Ok(meta) = fs::metadata(path) else {
            return;
        };
        let mut perms = meta.permissions();
        if perms.readonly() {
            perms.set_readonly(false);
            let _ = fs::set_permissions(path, perms);
        }
        if meta.is_dir() {
            if let Ok(entries) = fs::read_dir(path) {
                for entry in entries.flatten() {
                    walk(&entry.path());
                }
            }
        }
    }
    walk(root);
}

#[cfg(not(windows))]
fn clear_readonly_recursive(_root: &Path) {}

/// Direktoriji u `Projekti/` koji nemaju red u globalnoj bazi.
pub fn orphan_project_dir_names(
    conn: &Connection,
    paths: &ProjectPaths,
) -> rusqlite::Result<Vec<String>> {
    let projects = list_projects(conn)?;
    let known: HashSet<String> = projects
        .iter()
        .filter_map(|p| p.get("project_id").and_then(|v| v.as_str()))
        .map(|id| paths.project_dir(id))
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .collect();
    let mut orphans = Vec::new();
    if let Ok(entries) = fs::read_dir(&paths.projects_root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if !known.contains(&name) {
                orphans.push(name);
            }
        }
    }
    Ok(orphans)
}

/// Briše orphan direktorije (nema reda u bazi). Vraća uklonjene nazive foldera.
pub fn cleanup_orphan_project_dirs(
    conn: &Connection,
    paths: &ProjectPaths,
) -> rusqlite::Result<(Vec<String>, Vec<String>)> {
    let orphans = orphan_project_dir_names(conn, paths)?;
    let mut removed = Vec::new();
    let mut leftovers = Vec::new();
    for name in orphans {
        let dir = paths.projects_root.join(&name);
        if remove_project_dir(&dir).is_ok() && !dir.exists() {
            removed.push(name);
        } else if dir.exists() {
            leftovers.push(name);
        }
    }
    Ok((removed, leftovers))
}
