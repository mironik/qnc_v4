use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::config::{read_json, AppConfig};
use crate::design_db::{
    self, active_theme_id as db_active_theme_id, lab_key, load_custom_theme_ids, load_json_setting,
    save_custom_theme_ids, save_json_setting, set_active_theme_id, theme_doc_key, theme_exists,
    DEFAULT_TOKENS_KEY,
};
use crate::project::db::{open_global, ProjectPaths};

const VALID_MODES: [&str; 3] = ["open", "password", "off"];
const DEFAULT_THEME_ID: &str = "default";

fn with_design_store<T>(
    root: &Path,
    config: &AppConfig,
    f: impl FnOnce(&Connection, &Path) -> Result<T, String>,
) -> Result<T, String> {
    let paths = ProjectPaths::from_root(root, config);
    let conn = open_global(&paths).map_err(|e| e.to_string())?;
    design_db::ensure_files_migrated(&conn, root)?;
    f(&conn, root)
}

pub fn design_mode(root: &Path) -> String {
    if let Ok(env) = std::env::var("QNC_DESIGN_MODE") {
        let mode = env.trim().to_lowercase();
        if VALID_MODES.contains(&mode.as_str()) {
            return mode;
        }
    }
    let cfg = read_json(&root.join("data").join("shell_config.json")).unwrap_or(json!({}));
    if let Some(mode) = cfg
        .get("design_editor")
        .and_then(|v| v.get("mode"))
        .and_then(|v| v.as_str())
    {
        let mode = mode.trim().to_lowercase();
        if VALID_MODES.contains(&mode.as_str()) {
            return mode;
        }
    }
    "open".to_string()
}

pub fn design_default_enabled(root: &Path, mode: &str) -> bool {
    let cfg = read_json(&root.join("data").join("shell_config.json")).unwrap_or(json!({}));
    if let Some(block) = cfg.get("design_editor") {
        if let Some(v) = block.get("default_enabled") {
            return v.as_bool().unwrap_or(false);
        }
    }
    mode == "open"
}

pub fn design_editor_capability(root: &Path) -> Value {
    let mode = design_mode(root);
    let available = mode != "off";
    let authenticated = available && mode == "open";
    json!({
        "available": available,
        "mode": mode,
        "authenticated": authenticated,
        "default_enabled": design_default_enabled(root, &mode),
    })
}

pub fn design_status(root: &Path, config: &AppConfig) -> Value {
    let cap = design_editor_capability(root);
    let _ = with_design_store(root, config, |_, _| Ok(()));
    json!({
        "status": "ok",
        "available": cap.get("available").cloned().unwrap_or(json!(false)),
        "mode": cap.get("mode").cloned().unwrap_or(json!("off")),
        "authenticated": cap.get("authenticated").cloned().unwrap_or(json!(false)),
        "default_enabled": cap.get("default_enabled").cloned().unwrap_or(json!(false)),
        "storage": "app_settings",
        "paths": {
            "tokens": "/api/design-tools/tokens",
            "overrides": "project_store.db/app_settings (design.*)",
            "themes": "project_store.db/app_settings (design.theme.*)",
            "active_theme": "project_store.db/app_settings (design.active_theme_id)",
            "timeline_lab": "project_store.db/app_settings (design.lab.timeline)",
            "project_list_lab": "project_store.db/app_settings (design.lab.project_list)",
            "project_template_settings_lab": "project_store.db/app_settings (design.lab.project_template_settings)",
            "ingest_clip_grid_lab": "project_store.db/app_settings (design.lab.ingest_clip_grid)",
        }
    })
}

fn slug_theme_id(label: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in label.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            prev_dash = false;
        } else if !prev_dash && !slug.is_empty() {
            slug.push('-');
            prev_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "tema".to_string()
    } else {
        slug
    }
}

fn base_tokens_path(root: &Path) -> PathBuf {
    crate::workspace_paths::design_tokens(root)
}

fn load_token_overrides(conn: &Connection) -> HashMap<String, String> {
    let doc = load_json_setting(conn, DEFAULT_TOKENS_KEY)
        .ok()
        .flatten()
        .unwrap_or(json!({}));
    token_map_from_doc(&doc)
}

fn token_map_from_doc(doc: &Value) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Some(obj) = doc.get("tokens").and_then(|v| v.as_object()) {
        for (k, v) in obj {
            if k.starts_with("--") {
                if let Some(s) = v.as_str() {
                    out.insert(k.clone(), s.to_string());
                }
            }
        }
    }
    out
}

fn load_theme_doc(conn: &Connection, root: &Path, theme_id: &str) -> Value {
    if theme_id == DEFAULT_THEME_ID {
        let base = read_json(&base_tokens_path(root)).unwrap_or(json!({}));
        let overrides = load_token_overrides(conn);
        return json!({
            "id": DEFAULT_THEME_ID,
            "label": base.get("label").cloned().unwrap_or(json!("QNC Default")),
            "built_in": true,
            "tokens": overrides,
        });
    }
    load_json_setting(conn, &theme_doc_key(theme_id))
        .ok()
        .flatten()
        .unwrap_or(json!({}))
}

fn theme_override_tokens(
    conn: &Connection,
    root: &Path,
    theme_id: &str,
) -> HashMap<String, String> {
    if theme_id == DEFAULT_THEME_ID {
        return load_token_overrides(conn);
    }
    let doc = load_theme_doc(conn, root, theme_id);
    token_map_from_doc(&json!({ "tokens": doc.get("tokens").cloned().unwrap_or(json!({})) }))
}

pub fn merged_tokens(root: &Path, config: &AppConfig) -> Value {
    with_design_store(root, config, |conn, root| {
        let base =
            read_json(&base_tokens_path(root)).unwrap_or(json!({ "version": 1, "tokens": {} }));
        let theme_id = db_active_theme_id(conn)?;
        let overrides_map = theme_override_tokens(conn, root, &theme_id);
        let mut tokens = HashMap::new();
        if let Some(obj) = base.get("tokens").and_then(|v| v.as_object()) {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    tokens.insert(k.clone(), s.to_string());
                }
            }
        }
        for (k, v) in &overrides_map {
            tokens.insert(k.clone(), v.clone());
        }
        let theme_doc = load_theme_doc(conn, root, &theme_id);
        let label = theme_doc
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or("QNC Default");
        let overrides: Value = serde_json::to_value(&overrides_map).unwrap_or(json!({}));
        Ok(json!({
            "status": "ok",
            "version": base.get("version").cloned().unwrap_or(json!(1)),
            "label": label,
            "theme_id": theme_id,
            "tokens": tokens,
            "overrides": overrides,
        }))
    })
    .unwrap_or_else(|_| json!({ "status": "error" }))
}

pub fn list_themes(root: &Path, config: &AppConfig) -> Value {
    with_design_store(root, config, |conn, root| {
        let base = read_json(&base_tokens_path(root)).unwrap_or(json!({}));
        let mut themes = vec![json!({
            "id": DEFAULT_THEME_ID,
            "label": base.get("label").cloned().unwrap_or(json!("QNC Default")),
            "built_in": true,
        })];
        for theme_id in load_custom_theme_ids(conn)? {
            if theme_id == DEFAULT_THEME_ID {
                continue;
            }
            let doc = load_theme_doc(conn, root, &theme_id);
            themes.push(json!({
                "id": theme_id,
                "label": doc.get("label").cloned().unwrap_or(json!(theme_id)),
                "built_in": false,
            }));
        }
        Ok(json!({
            "status": "ok",
            "active_id": db_active_theme_id(conn)?,
            "themes": themes,
        }))
    })
    .unwrap_or_else(|_| json!({ "status": "error", "themes": [] }))
}

pub fn activate_theme(root: &Path, config: &AppConfig, theme_id: &str) -> Result<Value, String> {
    let target = if theme_id.trim().is_empty() {
        DEFAULT_THEME_ID.to_string()
    } else {
        theme_id.trim().to_string()
    };
    with_design_store(root, config, |conn, _| {
        if target != DEFAULT_THEME_ID && !theme_exists(conn, &target)? {
            return Err(format!("Tema '{target}' ne postoji."));
        }
        set_active_theme_id(conn, &target)?;
        Ok(json!({ "status": "ok", "active_id": target }))
    })
}

pub fn create_theme(root: &Path, config: &AppConfig, label: &str) -> Result<Value, String> {
    let clean = label.trim();
    if clean.is_empty() {
        return Err("Naziv teme je obavezan.".into());
    }
    with_design_store(root, config, |conn, root| {
        let mut theme_id = slug_theme_id(clean);
        let mut suffix = 2;
        while theme_exists(conn, &theme_id)? {
            theme_id = format!("{}-{}", slug_theme_id(clean), suffix);
            suffix += 1;
        }
        let merged = merged_tokens(root, config);
        let base = read_json(&base_tokens_path(root)).unwrap_or(json!({}));
        let base_tokens = base.get("tokens").and_then(|v| v.as_object());
        let mut overrides = HashMap::new();
        if let Some(obj) = merged.get("tokens").and_then(|v| v.as_object()) {
            for (k, v) in obj {
                if !k.starts_with("--") {
                    continue;
                }
                let val = v.as_str().unwrap_or("");
                let base_val = base_tokens
                    .and_then(|b| b.get(k))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if val != base_val {
                    overrides.insert(k.clone(), val.to_string());
                }
            }
        }
        let doc = json!({
            "version": 1,
            "id": theme_id,
            "label": clean,
            "tokens": overrides,
        });
        save_json_setting(conn, &theme_doc_key(&theme_id), &doc)?;
        let mut ids = load_custom_theme_ids(conn)?;
        if !ids.iter().any(|id| id == &theme_id) {
            ids.push(theme_id.clone());
            save_custom_theme_ids(conn, &ids)?;
        }
        set_active_theme_id(conn, &theme_id)?;
        Ok(json!({
            "status": "ok",
            "id": theme_id.clone(),
            "label": clean,
            "active_id": theme_id,
        }))
    })
}

pub fn save_token_overrides(
    root: &Path,
    config: &AppConfig,
    tokens: &HashMap<String, String>,
) -> Result<Value, String> {
    let mode = design_mode(root);
    if mode == "off" {
        return Err("Design editor je isključen.".into());
    }
    if mode == "password" {
        return Err("Spremanje zahtijeva admin autentifikaciju (još nije implementirano).".into());
    }
    with_design_store(root, config, |conn, root| {
        let target = db_active_theme_id(conn)?;
        let clean: HashMap<String, String> = tokens
            .iter()
            .filter(|(k, _)| k.starts_with("--"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if target == DEFAULT_THEME_ID {
            let doc = json!({ "version": 1, "tokens": clean });
            save_json_setting(conn, DEFAULT_TOKENS_KEY, &doc)?;
            return Ok(json!({ "status": "ok", "theme_id": target, "tokens": clean }));
        }
        let theme_doc = load_theme_doc(conn, root, &target);
        if theme_doc.get("id").is_none() {
            return Err(format!("Tema '{target}' ne postoji."));
        }
        let label = theme_doc
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or(&target);
        let doc = json!({
            "version": 1,
            "id": target,
            "label": label,
            "tokens": clean,
        });
        save_json_setting(conn, &theme_doc_key(&target), &doc)?;
        Ok(json!({ "status": "ok", "theme_id": target, "tokens": clean }))
    })
}

fn default_timeline_lab_prefs() -> Value {
    json!({
        "version": 1,
        "active_profile_id": "design-lab",
        "label_display": "full",
        "video_presentation": "filmstrip",
        "playhead_pct": 35,
        "track_states": {},
    })
}

pub fn load_timeline_lab_prefs(root: &Path, config: &AppConfig) -> Value {
    with_design_store(root, config, |conn, _| {
        let doc = load_json_setting(conn, &lab_key("timeline"))?
            .unwrap_or_else(default_timeline_lab_prefs);
        Ok(json!({
            "status": "ok",
            "prefs": doc,
        }))
    })
    .unwrap_or_else(|_| json!({ "status": "error" }))
}

pub fn save_timeline_lab_prefs(
    root: &Path,
    config: &AppConfig,
    prefs: Value,
) -> Result<Value, String> {
    let mode = design_mode(root);
    if mode == "off" {
        return Err("Design editor je isključen.".into());
    }
    if mode == "password" {
        return Err("Spremanje zahtijeva admin autentifikaciju (još nije implementirano).".into());
    }
    with_design_store(root, config, |conn, _| {
        save_json_setting(conn, &lab_key("timeline"), &prefs)?;
        Ok(json!({ "status": "ok", "prefs": prefs }))
    })
}

fn default_project_list_lab_prefs() -> Value {
    json!({
        "version": 1,
        "component": "project-list",
        "active_profile_id": "design-lab",
        "feature_states": {},
    })
}

pub fn load_project_list_lab_prefs(root: &Path, config: &AppConfig) -> Value {
    with_design_store(root, config, |conn, _| {
        let doc = load_json_setting(conn, &lab_key("project_list"))?
            .unwrap_or_else(default_project_list_lab_prefs);
        Ok(json!({
            "status": "ok",
            "prefs": doc,
        }))
    })
    .unwrap_or_else(|_| json!({ "status": "error" }))
}

pub fn save_project_list_lab_prefs(
    root: &Path,
    config: &AppConfig,
    prefs: Value,
) -> Result<Value, String> {
    let mode = design_mode(root);
    if mode == "off" {
        return Err("Design editor je isključen.".into());
    }
    if mode == "password" {
        return Err("Spremanje zahtijeva admin autentifikaciju (još nije implementirano).".into());
    }
    with_design_store(root, config, |conn, _| {
        save_json_setting(conn, &lab_key("project_list"), &prefs)?;
        Ok(json!({ "status": "ok", "prefs": prefs }))
    })
}

fn default_project_template_settings_lab_prefs() -> Value {
    json!({
        "version": 1,
        "component": "project-template-settings",
        "active_profile_id": "design-lab",
        "section_states": {},
    })
}

pub fn load_project_template_settings_lab_prefs(root: &Path, config: &AppConfig) -> Value {
    with_design_store(root, config, |conn, _| {
        let doc = load_json_setting(conn, &lab_key("project_template_settings"))?
            .unwrap_or_else(default_project_template_settings_lab_prefs);
        Ok(json!({
            "status": "ok",
            "prefs": doc,
        }))
    })
    .unwrap_or_else(|_| json!({ "status": "error" }))
}

pub fn save_project_template_settings_lab_prefs(
    root: &Path,
    config: &AppConfig,
    prefs: Value,
) -> Result<Value, String> {
    let mode = design_mode(root);
    if mode == "off" {
        return Err("Design editor je isključen.".into());
    }
    if mode == "password" {
        return Err("Spremanje zahtijeva admin autentifikaciju (još nije implementirano).".into());
    }
    with_design_store(root, config, |conn, _| {
        save_json_setting(conn, &lab_key("project_template_settings"), &prefs)?;
        Ok(json!({ "status": "ok", "prefs": prefs }))
    })
}

fn default_ingest_clip_grid_lab_prefs() -> Value {
    json!({
        "version": 1,
        "component": "ingest-clip-grid",
        "active_profile_id": "fcp11-browser",
        "feature_states": {},
    })
}

pub fn load_ingest_clip_grid_lab_prefs(root: &Path, config: &AppConfig) -> Value {
    with_design_store(root, config, |conn, _| {
        let doc = load_json_setting(conn, &lab_key("ingest_clip_grid"))?
            .unwrap_or_else(default_ingest_clip_grid_lab_prefs);
        Ok(json!({
            "status": "ok",
            "prefs": doc,
        }))
    })
    .unwrap_or_else(|_| json!({ "status": "error" }))
}

pub fn save_ingest_clip_grid_lab_prefs(
    root: &Path,
    config: &AppConfig,
    prefs: Value,
) -> Result<Value, String> {
    let mode = design_mode(root);
    if mode == "off" {
        return Err("Design editor je isključen.".into());
    }
    if mode == "password" {
        return Err("Spremanje zahtijeva admin autentifikaciju (još nije implementirano).".into());
    }
    with_design_store(root, config, |conn, _| {
        save_json_setting(conn, &lab_key("ingest_clip_grid"), &prefs)?;
        Ok(json!({ "status": "ok", "prefs": prefs }))
    })
}
