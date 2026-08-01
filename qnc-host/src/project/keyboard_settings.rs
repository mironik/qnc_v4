use std::fs;
use std::path::Path;

use rusqlite::Connection;
use serde_json::{json, Value};

use super::db::{get_setting, json_string, parse_json, set_setting};

const KEY: &str = "keyboard_shortcuts_user";
const PRESET_ORDER: &[&str] = &[
    "default", "resolve", "premiere", "finalcut", "edius", "avid",
];

pub fn load_keyboard_user(conn: &Connection) -> rusqlite::Result<Value> {
    let raw = get_setting(conn, KEY, "")?;
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    Ok(parse_json(&raw, json!({})))
}

pub fn save_keyboard_user(conn: &Connection, user: &Value) -> rusqlite::Result<Value> {
    let payload = if user.is_object() {
        user.clone()
    } else {
        json!({})
    };
    set_setting(conn, KEY, &json_string(&payload))?;
    Ok(payload)
}

/// Katalog NLE preseta — host čita manifest; klijent ide samo preko API-ja.
pub fn list_keyboard_presets(root: &Path) -> Value {
    let path = crate::workspace_paths::keyboard_shortcuts(root);
    let fallback = || {
        json!(PRESET_ORDER
            .iter()
            .map(|id| json!({ "id": id, "name": id }))
            .collect::<Vec<_>>())
    };
    let Ok(raw) = fs::read_to_string(&path) else {
        return json!({ "presets": fallback() });
    };
    let Ok(data) = serde_json::from_str::<Value>(&raw) else {
        return json!({ "presets": fallback() });
    };
    let presets_map = data.get("presets").and_then(|v| v.as_object());
    let Some(presets_map) = presets_map else {
        return json!({ "presets": fallback() });
    };
    let presets: Vec<Value> = PRESET_ORDER
        .iter()
        .filter_map(|id| {
            presets_map.get(*id).map(|p| {
                json!({
                    "id": id,
                    "name": p.get("name").and_then(|v| v.as_str()).unwrap_or(id),
                })
            })
        })
        .collect();
    json!({
        "presets": if presets.is_empty() { fallback() } else { json!(presets) }
    })
}
