use rusqlite::Connection;
use serde_json::{json, Value};

const UI_STATE_KEY: &str = "project_tab_ui_state";

pub fn default_ui_state() -> Value {
    json!({
        "selected_template_id": "tpl_breaking_news",
        "selected_project_id": "",
        "project_name": "",
        "template_create_open": false,
        "template_draft_name": "",
        "template_draft_description": "",
        "settings_override": {},
        "collab_session_id": "",
    })
}

pub fn get_ui_state(conn: &Connection) -> rusqlite::Result<Value> {
    let raw = super::db::get_setting(conn, UI_STATE_KEY, "")?;
    if raw.trim().is_empty() {
        return Ok(default_ui_state());
    }
    let mut state = super::db::parse_json(&raw, default_ui_state());
    if !state.is_object() {
        state = default_ui_state();
    }
    Ok(normalize_ui_state(state))
}

/// UI snapshot za API: samo SQLite + server-side merge (bez JS deepMerge).
pub fn get_ui_state_for_api(conn: &Connection) -> rusqlite::Result<Value> {
    let ui = get_ui_state(conn)?;
    attach_effective_settings(conn, ui)
}

pub fn save_ui_state(conn: &Connection, patch: &Value) -> rusqlite::Result<Value> {
    let mut current = get_ui_state(conn)?;
    merge_ui_patch(&mut current, patch);
    normalize_ui_state_in_place(&mut current);
    super::db::set_setting(conn, UI_STATE_KEY, &super::db::json_string(&current))?;
    attach_effective_settings(conn, current)
}

pub fn effective_template_settings(conn: &Connection, ui: &Value) -> rusqlite::Result<Value> {
    let template_id = ui
        .get("selected_template_id")
        .and_then(|v| v.as_str())
        .unwrap_or("tpl_breaking_news");
    let base = super::templates::get_project_template(conn, template_id)?
        .and_then(|t| t.get("settings").cloned())
        .unwrap_or_else(|| json!({}));
    let ov = ui
        .get("settings_override")
        .cloned()
        .unwrap_or_else(|| json!({}));
    Ok(super::db::deep_merge(&base, &ov))
}

fn attach_effective_settings(conn: &Connection, mut ui: Value) -> rusqlite::Result<Value> {
    let effective = effective_template_settings(conn, &ui)?;
    if let Some(obj) = ui.as_object_mut() {
        obj.insert("effective_settings".into(), effective);
    }
    Ok(ui)
}

fn normalize_ui_state(mut state: Value) -> Value {
    normalize_ui_state_in_place(&mut state);
    state
}

fn normalize_ui_state_in_place(state: &mut Value) {
    let defaults = default_ui_state();
    if let Some(obj) = state.as_object_mut() {
        if let Some(def) = defaults.as_object() {
            for (key, val) in def {
                if !obj.contains_key(key) {
                    obj.insert(key.clone(), val.clone());
                }
            }
        }
        if !obj
            .get("settings_override")
            .map(|v| v.is_object())
            .unwrap_or(false)
        {
            obj.insert("settings_override".into(), json!({}));
        }
    }
}

fn merge_ui_patch(target: &mut Value, patch: &Value) {
    let Some(patch_obj) = patch.as_object() else {
        return;
    };
    let Some(target_obj) = target.as_object_mut() else {
        return;
    };

    if patch_obj
        .get("reset_settings_override")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        target_obj.insert("settings_override".into(), json!({}));
    }

    for (key, val) in patch_obj {
        match key.as_str() {
            "settings_override" => {
                if let Some(ov) = target_obj.get_mut("settings_override") {
                    *ov = super::db::deep_merge(ov, val);
                } else {
                    target_obj.insert(key.clone(), val.clone());
                }
            }
            "settings_path" => {
                if let Some(obj) = val.as_object() {
                    let path = obj.get("path").and_then(|v| v.as_str()).unwrap_or("");
                    let value = obj.get("value").cloned().unwrap_or(Value::Null);
                    if !path.is_empty() {
                        set_nested_path(target_obj, "settings_override", path, value);
                    }
                }
            }
            "reset_settings_override" => {}
            _ => {
                target_obj.insert(key.clone(), val.clone());
            }
        }
    }
}

fn set_nested_path(
    root: &mut serde_json::Map<String, Value>,
    top_key: &str,
    path: &str,
    value: Value,
) {
    let parts: Vec<&str> = path.split('.').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return;
    }
    if !root.contains_key(top_key) || !root.get(top_key).map(|v| v.is_object()).unwrap_or(false) {
        root.insert(top_key.into(), json!({}));
    }
    let Some(top) = root.get_mut(top_key) else {
        return;
    };
    let Some(obj) = top.as_object_mut() else {
        return;
    };
    let mut cur = obj;
    for part in &parts[..parts.len() - 1] {
        if !cur.contains_key(*part) || !cur.get(*part).map(|v| v.is_object()).unwrap_or(false) {
            cur.insert(part.to_string(), json!({}));
        }
        let next = cur.get_mut(*part).unwrap();
        cur = next.as_object_mut().unwrap();
    }
    cur.insert(parts[parts.len() - 1].to_string(), value);
}

pub fn touch_collab_session(conn: &Connection, session_id: &str) -> rusqlite::Result<Value> {
    save_ui_state(conn, &json!({ "collab_session_id": session_id }))
}
