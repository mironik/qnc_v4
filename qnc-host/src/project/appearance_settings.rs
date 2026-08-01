//! User appearance prefs (theme) — host SQLite, not per-project settings_override.

use rusqlite::Connection;
use serde_json::{json, Value};

use super::db::{get_setting, json_string, parse_json, set_setting};

const KEY: &str = "ui_appearance_user";

pub fn load_appearance_user(conn: &Connection) -> rusqlite::Result<Value> {
    let raw = get_setting(conn, KEY, "")?;
    if raw.trim().is_empty() {
        return Ok(json!({ "theme_id": "dark" }));
    }
    let mut v = parse_json(&raw, json!({ "theme_id": "dark" }));
    if !v.is_object() {
        v = json!({ "theme_id": "dark" });
    }
    if v.get("theme_id").and_then(|x| x.as_str()).is_none() {
        if let Some(obj) = v.as_object_mut() {
            obj.insert("theme_id".into(), json!("dark"));
        }
    }
    Ok(v)
}

pub fn save_appearance_user(conn: &Connection, user: &Value) -> rusqlite::Result<Value> {
    let theme_id = user
        .get("theme_id")
        .and_then(|v| v.as_str())
        .unwrap_or("dark");
    let theme_id = match theme_id {
        "soft" => "soft",
        "high_contrast" | "high-contrast" | "contrast" => "high_contrast",
        _ => "dark",
    };
    let payload = json!({ "theme_id": theme_id });
    set_setting(conn, KEY, &json_string(&payload))?;
    Ok(payload)
}
