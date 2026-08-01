use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::config::read_json;
use crate::project::db::{get_setting, json_string, set_setting};

const MIGRATED_KEY: &str = "design.files_migrated";
pub const ACTIVE_THEME_KEY: &str = "design.active_theme_id";
pub const DEFAULT_TOKENS_KEY: &str = "design.tokens.default";
const CUSTOM_THEME_IDS_KEY: &str = "design.custom_theme_ids";

pub fn theme_doc_key(theme_id: &str) -> String {
    format!("design.theme.{theme_id}")
}

pub fn lab_key(name: &str) -> String {
    format!("design.lab.{name}")
}

pub fn ensure_files_migrated(conn: &Connection, root: &Path) -> Result<(), String> {
    if get_setting(conn, MIGRATED_KEY, "0").map_err(|e| e.to_string())? == "1" {
        return Ok(());
    }
    migrate_active_theme(conn, root)?;
    migrate_default_tokens(conn, root)?;
    migrate_custom_themes(conn, root)?;
    migrate_lab_file(conn, root, "timeline-lab.json", "timeline")?;
    migrate_lab_file(conn, root, "project-list-lab.json", "project_list")?;
    migrate_lab_file(
        conn,
        root,
        "project-template-settings-lab.json",
        "project_template_settings",
    )?;
    migrate_lab_file(conn, root, "ingest-clip-grid-lab.json", "ingest_clip_grid")?;
    migrate_lab_file(
        conn,
        root,
        "ingest-proxy-clip-grid-lab.json",
        "ingest_clip_grid",
    )?;
    set_setting(conn, MIGRATED_KEY, "1").map_err(|e| e.to_string())?;
    Ok(())
}

fn overrides_dir(root: &Path) -> PathBuf {
    root.join("data").join("design_overrides")
}

fn mark_migrated(path: &Path) -> Result<(), String> {
    if !path.is_file() {
        return Ok(());
    }
    let backup = path.with_extension("json.migrated");
    if backup.is_file() {
        let _ = fs::remove_file(path);
        return Ok(());
    }
    fs::rename(path, backup).map_err(|e| e.to_string())
}

fn migrate_active_theme(conn: &Connection, root: &Path) -> Result<(), String> {
    let path = overrides_dir(root).join("active_theme.json");
    if !path.is_file() {
        return Ok(());
    }
    let doc = read_json(&path).unwrap_or(json!({}));
    let theme_id = doc
        .get("theme_id")
        .and_then(|v| v.as_str())
        .unwrap_or("default")
        .trim()
        .to_string();
    set_setting(conn, ACTIVE_THEME_KEY, &theme_id).map_err(|e| e.to_string())?;
    mark_migrated(&path)
}

fn migrate_default_tokens(conn: &Connection, root: &Path) -> Result<(), String> {
    let path = overrides_dir(root).join("tokens.json");
    if !path.is_file() {
        return Ok(());
    }
    let doc = read_json(&path).unwrap_or(json!({}));
    set_setting(conn, DEFAULT_TOKENS_KEY, &json_string(&doc)).map_err(|e| e.to_string())?;
    mark_migrated(&path)
}

fn migrate_custom_themes(conn: &Connection, root: &Path) -> Result<(), String> {
    let dir = overrides_dir(root).join("themes");
    if !dir.is_dir() {
        return Ok(());
    }
    let mut ids = load_custom_theme_ids(conn)?;
    for entry in fs::read_dir(&dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let doc = read_json(&path).unwrap_or(json!({}));
        let theme_id = doc
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("tema")
                    .to_string()
            });
        set_setting(conn, &theme_doc_key(&theme_id), &json_string(&doc))
            .map_err(|e| e.to_string())?;
        if !ids.iter().any(|id| id == &theme_id) {
            ids.push(theme_id);
        }
        mark_migrated(&path)?;
    }
    save_custom_theme_ids(conn, &ids)
}

fn migrate_lab_file(
    conn: &Connection,
    root: &Path,
    file_name: &str,
    lab_name: &str,
) -> Result<(), String> {
    let path = overrides_dir(root).join(file_name);
    if !path.is_file() {
        return Ok(());
    }
    let key = lab_key(lab_name);
    if get_setting(conn, &key, "")
        .map_err(|e| e.to_string())?
        .trim()
        .is_empty()
    {
        let doc = read_json(&path).unwrap_or(json!({}));
        set_setting(conn, &key, &json_string(&doc)).map_err(|e| e.to_string())?;
    }
    mark_migrated(&path)
}

pub fn active_theme_id(conn: &Connection) -> Result<String, String> {
    let id = get_setting(conn, ACTIVE_THEME_KEY, "default").map_err(|e| e.to_string())?;
    let id = id.trim();
    if id.is_empty() {
        Ok("default".into())
    } else {
        Ok(id.to_string())
    }
}

pub fn set_active_theme_id(conn: &Connection, theme_id: &str) -> Result<(), String> {
    set_setting(conn, ACTIVE_THEME_KEY, theme_id.trim()).map_err(|e| e.to_string())
}

pub fn load_json_setting(conn: &Connection, key: &str) -> Result<Option<Value>, String> {
    let raw = get_setting(conn, key, "").map_err(|e| e.to_string())?;
    if raw.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&raw).unwrap_or(json!({}))))
}

pub fn save_json_setting(conn: &Connection, key: &str, value: &Value) -> Result<(), String> {
    set_setting(conn, key, &json_string(value)).map_err(|e| e.to_string())
}

pub fn load_custom_theme_ids(conn: &Connection) -> Result<Vec<String>, String> {
    Ok(load_json_setting(conn, CUSTOM_THEME_IDS_KEY)?
        .and_then(|v| {
            v.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
        })
        .unwrap_or_default())
}

pub fn save_custom_theme_ids(conn: &Connection, ids: &[String]) -> Result<(), String> {
    save_json_setting(conn, CUSTOM_THEME_IDS_KEY, &json!(ids))
}

pub fn theme_exists(conn: &Connection, theme_id: &str) -> Result<bool, String> {
    if theme_id.trim() == "default" {
        return Ok(true);
    }
    Ok(load_json_setting(conn, &theme_doc_key(theme_id))?.is_some())
}
