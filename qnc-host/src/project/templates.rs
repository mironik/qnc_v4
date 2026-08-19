use std::collections::HashSet;
use std::fs;

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use super::db::{
    deep_merge, ensure_project_dirs_at, export_dir_from_settings, now_str, open_project,
    project_dir_in_root, project_root_from_settings, slug_id, ProjectPaths,
};
use super::kv::{load_object, load_string_list, replace_object, replace_string_list};
use super::store::{
    project_row, record_project_opened, set_active_project_id, upsert_project_meta,
};

#[derive(serde::Deserialize)]
struct SeedFile {
    source_templates: Vec<Value>,
    project_templates: Vec<Value>,
}

fn load_seed(seed_path: &std::path::Path) -> SeedFile {
    let fallback = SeedFile {
        source_templates: vec![],
        project_templates: vec![],
    };
    let Ok(raw) = fs::read_to_string(seed_path) else {
        return fallback;
    };
    serde_json::from_str(&raw).unwrap_or(fallback)
}

pub fn ensure_templates_seeded(
    conn: &Connection,
    seed_path: &std::path::Path,
) -> rusqlite::Result<()> {
    let seed = load_seed(seed_path);
    let now = now_str();
    for src in &seed.source_templates {
        let id = src
            .get("source_template_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if id.is_empty() {
            continue;
        }
        let config = src.get("config").cloned().unwrap_or_else(|| json!({}));
        conn.execute(
            "INSERT INTO source_templates
                (source_template_id, name, description, source_kind, system, config_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 1, '{}', ?5, ?5)
             ON CONFLICT(source_template_id) DO NOTHING",
            params![
                id,
                src.get("name").and_then(|v| v.as_str()).unwrap_or(id),
                src.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                src.get("source_kind").and_then(|v| v.as_str()).unwrap_or("local"),
                now,
            ],
        )?;
        if conn.changes() > 0 {
            replace_object(
                conn,
                "source_template_kv",
                "source_template_id",
                id,
                &config,
            )?;
        }
    }
    for tpl in &seed.project_templates {
        let id = tpl
            .get("template_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if id.is_empty() {
            continue;
        }
        let settings = tpl.get("settings").cloned().unwrap_or_else(|| json!({}));
        let source_ids = tpl
            .get("source_template_ids")
            .cloned()
            .unwrap_or_else(|| json!([]));
        conn.execute(
            "INSERT INTO project_templates
                (template_id, name, description, system, settings_json, source_template_ids_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, 1, '{}', '[]', ?4, ?4)
             ON CONFLICT(template_id) DO NOTHING",
            params![
                id,
                tpl.get("name").and_then(|v| v.as_str()).unwrap_or(id),
                tpl.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                now,
            ],
        )?;
        if conn.changes() > 0 {
            replace_object(conn, "project_template_kv", "template_id", id, &settings)?;
            let source_list: Vec<String> = source_ids
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
                id,
                "source_template_id",
                &source_list,
            )?;
        }
    }
    repair_system_template_workspaces(conn, &seed)?;
    Ok(())
}

pub fn list_source_templates(conn: &Connection) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare("SELECT * FROM source_templates ORDER BY name")?;
    let ids: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    let mut out = Vec::new();
    for source_template_id in ids {
        let mut stmt =
            conn.prepare("SELECT * FROM source_templates WHERE source_template_id = ?1")?;
        let mut row = stmt.query_row(params![source_template_id], |row| {
            Ok(json!({
                "source_template_id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "description": row.get::<_, String>(2)?,
                "source_kind": row.get::<_, String>(3)?,
                "system": row.get::<_, i64>(4)? != 0,
            }))
        })?;
        let config = load_object(
            conn,
            "source_template_kv",
            "source_template_id",
            &source_template_id,
        )?;
        if let Some(obj) = row.as_object_mut() {
            obj.insert("config".into(), config);
        }
        out.push(row);
    }
    Ok(out)
}

pub fn list_project_templates(conn: &Connection) -> rusqlite::Result<Vec<Value>> {
    let mut stmt =
        conn.prepare("SELECT template_id FROM project_templates ORDER BY system DESC, name")?;
    let ids: Vec<String> = stmt
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    let mut out = Vec::new();
    for template_id in ids {
        if let Some(item) = get_project_template(conn, &template_id)? {
            out.push(item);
        }
    }
    Ok(out)
}

pub fn get_project_template(
    conn: &Connection,
    template_id: &str,
) -> rusqlite::Result<Option<Value>> {
    let tid = template_id.trim();
    let row = conn.query_row(
        "SELECT template_id, name, description, system, created_by, updated_by, created_at, updated_at
         FROM project_templates WHERE template_id = ?1",
        params![tid],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)? != 0,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        },
    );
    let Ok((
        template_id,
        name,
        description,
        system,
        created_by,
        updated_by,
        created_at,
        updated_at,
    )) = row
    else {
        return Ok(None);
    };
    let settings = normalize_settings_workspace(&load_object(
        conn,
        "project_template_kv",
        "template_id",
        &template_id,
    )?);
    let source_ids = load_string_list(
        conn,
        "project_template_sources",
        "template_id",
        &template_id,
        "source_template_id",
    )?;
    Ok(Some(json!({
        "template_id": template_id,
        "name": name,
        "description": description,
        "system": system,
        "settings": settings,
        "source_template_ids": source_ids,
        "created_by": created_by,
        "updated_by": updated_by,
        "created_at": created_at,
        "updated_at": updated_at,
    })))
}

pub fn get_project_settings(paths: &ProjectPaths, project_id: &str) -> rusqlite::Result<Value> {
    let pid = project_id.trim();
    let conn = open_project(paths, pid)?;
    project_settings_from_conn(&conn, pid)
}

fn project_settings_from_conn(conn: &Connection, project_id: &str) -> rusqlite::Result<Value> {
    let pid = project_id.trim();
    let row: Option<String> = conn
        .query_row(
            "SELECT template_id FROM project_settings WHERE project_id = ?1",
            params![pid],
            |r| r.get(0),
        )
        .ok();
    let Some(template_id) = row else {
        return Ok(json!({}));
    };
    let settings = load_object(&conn, "project_settings_kv", "project_id", pid)?;
    Ok(json!({
        "project_id": pid,
        "template_id": template_id,
        "settings": settings,
    }))
}

pub fn prepare_project_workspace(
    global: &Connection,
    paths: &ProjectPaths,
    project_id: &str,
) -> rusqlite::Result<Value> {
    let pid = project_id.trim();
    let conn = open_project(paths, pid)?;
    migrate_legacy_ingest_workflow(&conn, pid)?;
    let item = project_settings_from_conn(&conn, pid)?;
    let template_id = item
        .get("template_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let settings_raw = item.get("settings").cloned().unwrap_or_else(|| json!({}));
    let settings = normalize_settings_workspace(&hydrate_project_settings(
        global,
        &conn,
        pid,
        &template_id,
        settings_raw.clone(),
    )?);
    let settings = materialize_workspace_tabs(&settings);
    let mut snapshot_template_id = item.get("template_id").cloned().unwrap_or(Value::Null);
    if settings != settings_raw && settings_have_workflow_tabs(&settings) {
        let tid = resolve_persist_template_id(global, &conn, pid, &template_id)?;
        let _ = persist_project_settings_kv(&conn, pid, &tid, &settings);
        if !tid.trim().is_empty() {
            snapshot_template_id = Value::String(tid);
        }
    }
    ensure_project_workflow(&conn, pid, &settings)?;
    workspace_snapshot_from_db(&conn, pid, snapshot_template_id)
}

pub fn get_project_workspace(
    _global: &Connection,
    paths: &ProjectPaths,
    project_id: &str,
) -> rusqlite::Result<Value> {
    let pid = project_id.trim();
    let conn = open_project(paths, pid)?;
    migrate_legacy_ingest_workflow(&conn, pid)?;
    let item = project_settings_from_conn(&conn, pid)?;
    let steps = list_workflow_steps(&conn, pid)?;
    if steps.is_empty() {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "Projekt '{pid}' nema DB workspace workflow; otvori projekt kroz Project Registry repair/open tok."
        )));
    }
    workspace_snapshot_from_parts(
        pid,
        item.get("template_id").cloned().unwrap_or(Value::Null),
        steps,
        workflow_state(&conn, pid)?,
    )
}

fn workspace_snapshot_from_db(
    conn: &Connection,
    project_id: &str,
    template_id: Value,
) -> rusqlite::Result<Value> {
    let steps = list_workflow_steps(conn, project_id)?;
    let state = workflow_state(conn, project_id)?;
    workspace_snapshot_from_parts(project_id, template_id, steps, state)
}

fn workspace_snapshot_from_parts(
    project_id: &str,
    template_id: Value,
    steps: Vec<Value>,
    state: Value,
) -> rusqlite::Result<Value> {
    let tabs: Vec<String> = steps
        .iter()
        .filter_map(|s| {
            s.get("tab_id")
                .and_then(|v| v.as_str())
                .map(normalize_workspace_tab_id)
        })
        .collect();
    let mut labels = serde_json::Map::new();
    for step in &steps {
        if let (Some(tab_id), Some(label)) = (
            step.get("tab_id").and_then(|v| v.as_str()),
            step.get("label").and_then(|v| v.as_str()),
        ) {
            labels.insert(tab_id.to_string(), json!(label));
        }
    }
    Ok(json!({
        "project_id": project_id,
        "template_id": template_id,
        "tabs": tabs,
        "tab_labels": Value::Object(labels),
        "steps": steps,
        "active_step_id": state.get("active_step_id").cloned().unwrap_or(Value::Null),
        "entry_step_id": state.get("entry_step_id").cloned().unwrap_or(Value::Null),
        "modules": [],
    }))
}

pub fn save_project_settings(
    paths: &ProjectPaths,
    project_id: &str,
    settings: &Value,
    template_id: &str,
    user_id: &str,
) -> rusqlite::Result<Value> {
    let pid = project_id.trim();
    validate_project_export_settings(settings).map_err(rusqlite::Error::InvalidParameterName)?;
    let conn = open_project(paths, pid)?;
    let now = now_str();
    let existing: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT created_at, created_by FROM project_settings WHERE project_id = ?1",
            params![pid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let (created_at, created_by) = existing.unwrap_or((None, None));
    let created_by_val = created_by.unwrap_or_else(|| user_id.to_string());
    conn.execute(
        "INSERT INTO project_settings
            (project_id, template_id, settings_json, created_by, updated_by, created_at, updated_at)
         VALUES (?1, ?2, '{}', ?3, ?4, ?5, ?6)
         ON CONFLICT(project_id) DO UPDATE SET
            template_id = excluded.template_id,
            settings_json = '{}',
            updated_by = excluded.updated_by,
            updated_at = excluded.updated_at",
        params![
            pid,
            template_id,
            if created_by_val.is_empty() {
                user_id
            } else {
                &created_by_val
            },
            user_id,
            created_at.unwrap_or_else(|| now.clone()),
            now,
        ],
    )?;
    replace_object(&conn, "project_settings_kv", "project_id", pid, settings)?;
    write_project_workflow(&conn, pid, settings)?;
    get_project_settings(paths, pid)
}

pub fn create_user_template(
    conn: &Connection,
    name: &str,
    description: &str,
    settings: Option<&Value>,
    source_template_ids: Option<&Value>,
    user_id: &str,
    base_template_id: &str,
) -> rusqlite::Result<Value> {
    let base = if base_template_id.is_empty() {
        None
    } else {
        get_project_template(conn, base_template_id)?
    };
    let payload = match (settings, &base) {
        (Some(s), Some(b)) => deep_merge(b.get("settings").unwrap_or(&json!({})), s),
        (Some(s), None) => s.clone(),
        (None, Some(b)) => b.get("settings").cloned().unwrap_or_else(|| json!({})),
        (None, None) => json!({}),
    };
    validate_project_export_settings(&payload).map_err(rusqlite::Error::InvalidParameterName)?;
    let source_ids = source_template_ids
        .cloned()
        .or_else(|| {
            base.as_ref()
                .and_then(|b| b.get("source_template_ids").cloned())
        })
        .unwrap_or_else(|| json!([]));
    let template_id = format!("tpl_user_{}_{}", slug_id(name), now_str());
    let now = now_str();
    conn.execute(
        "INSERT INTO project_templates
            (template_id, name, description, system, settings_json, source_template_ids_json,
             created_by, updated_by, created_at, updated_at)
         VALUES (?1, ?2, ?3, 0, '{}', '[]', ?4, ?4, ?5, ?5)",
        params![template_id, name.trim(), description.trim(), user_id, now],
    )?;
    replace_object(
        conn,
        "project_template_kv",
        "template_id",
        &template_id,
        &payload,
    )?;
    let source_list: Vec<String> = source_ids
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
        &source_list,
    )?;
    get_project_template(conn, &template_id)?
        .ok_or_else(|| rusqlite::Error::InvalidParameterName("template insert failed".into()))
}

/// Briše samo custom (non-system) project template iz SQLite.
pub fn delete_user_template(conn: &Connection, template_id: &str) -> Result<String, String> {
    let tid = template_id.trim();
    if tid.is_empty() {
        return Err("Nedostaje template_id.".into());
    }
    let Some(tpl) = get_project_template(conn, tid).map_err(|e| e.to_string())? else {
        return Err(format!("Template '{tid}' ne postoji."));
    };
    if tpl.get("system").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Err("System template se ne može obrisati.".into());
    }
    conn.execute(
        "DELETE FROM project_template_kv WHERE template_id = ?1",
        params![tid],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "DELETE FROM project_template_sources WHERE template_id = ?1",
        params![tid],
    )
    .map_err(|e| e.to_string())?;
    let n = conn
        .execute(
            "DELETE FROM project_templates WHERE template_id = ?1 AND system = 0",
            params![tid],
        )
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err(format!("Template '{tid}' nije obrisan."));
    }
    Ok(tid.to_string())
}

pub fn create_project_from_template(
    global: &Connection,
    paths: &ProjectPaths,
    name: &str,
    template_id: &str,
    settings_override: Option<&Value>,
    user_id: &str,
) -> rusqlite::Result<Value> {
    let template = get_project_template(global, template_id)?
        .ok_or_else(|| rusqlite::Error::QueryReturnedNoRows)?;
    let label = name.trim();
    let label = if label.is_empty() {
        "QNC projekt"
    } else {
        label
    };
    let project_id = slug_id(label);
    let mut settings = template
        .get("settings")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if let Some(ov) = settings_override {
        settings = deep_merge(&settings, ov);
    }
    settings = materialize_workspace_tabs(&settings);
    validate_project_export_settings(&settings).map_err(rusqlite::Error::InvalidParameterName)?;
    if let Value::Object(ref mut map) = settings {
        map.insert(
            "template".into(),
            json!({
                "template_id": template.get("template_id"),
                "name": template.get("name"),
                "system": template.get("system"),
            }),
        );
        map.insert(
            "source_template_ids".into(),
            template
                .get("source_template_ids")
                .cloned()
                .unwrap_or_else(|| json!([])),
        );
    }
    let projects_root =
        project_root_from_settings(&settings).unwrap_or_else(|| paths.projects_root.clone());
    let project_dir = project_dir_in_root(&projects_root, &project_id);
    upsert_project_meta(global, &project_id, label, None, Some(&project_dir))?;
    ensure_project_dirs_at(&project_dir)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    if let Some(export_dir) = export_dir_from_settings(&settings) {
        fs::create_dir_all(export_dir)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    }
    let tpl_id = template
        .get("template_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let saved = save_project_settings(paths, &project_id, &settings, tpl_id, user_id)?;
    let project_conn = open_project(paths, &project_id)?;
    save_template_snapshot(&project_conn, &project_id, &template)?;
    write_project_workflow(&project_conn, &project_id, &settings)?;
    set_active_project_id(global, &project_id)?;
    record_project_opened(global, &project_id)?;
    let project = project_row(global, &project_id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
    Ok(json!({
        "project": project,
        "settings": saved,
    }))
}

pub(crate) fn validate_project_export_settings(settings: &Value) -> Result<(), String> {
    let Some(export) = settings.get("export") else {
        return Ok(());
    };
    if export_is_forbidden_pal_progressive(export) {
        return Err(
            "PAL single-rate progressive export nije dozvoljen; koristi p50 ili i50.".into(),
        );
    }
    Ok(())
}

fn export_str<'a>(export: &'a Value, key: &str) -> &'a str {
    export.get(key).and_then(Value::as_str).unwrap_or("")
}

fn export_fps(export: &Value) -> f64 {
    export
        .get("fps")
        .and_then(Value::as_f64)
        .or_else(|| {
            export
                .get("fps")
                .and_then(Value::as_i64)
                .map(|value| value as f64)
        })
        .or_else(|| export.get("fps").and_then(Value::as_str)?.parse().ok())
        .unwrap_or(0.0)
}

fn contains_forbidden_pal_progressive_marker(format: &str) -> bool {
    let bytes = format.as_bytes();
    bytes.windows(3).any(|window| {
        (window[0].eq_ignore_ascii_case(&b'p') && window[1] == b'2' && window[2] == b'5')
            || (window[0] == b'2' && window[1] == b'5' && window[2].eq_ignore_ascii_case(&b'p'))
    })
}

fn export_is_forbidden_pal_progressive(export: &Value) -> bool {
    let fps = export_fps(export);
    let field_order = export_str(export, "field_order").to_ascii_lowercase();
    let format = export_str(export, "format").to_ascii_lowercase();
    let single_rate_pal = (fps - 25.0).abs() < 0.01;
    let explicit_forbidden = contains_forbidden_pal_progressive_marker(&format);
    let explicit_i50 = format.contains("i50") || field_order.contains("upper");
    explicit_forbidden || (single_rate_pal && field_order == "progressive" && !explicit_i50)
}

/// Legacy v1 workspace tab id → registered plugin tab id.
fn normalize_workspace_tab_id(raw: &str) -> String {
    let id = raw.trim();
    if id.is_empty() {
        return String::new();
    }
    if id == "ingest_proxy" {
        "ingest".to_string()
    } else {
        id.to_string()
    }
}

/// Map legacy ids, preserve order, drop empty/duplicate tab ids.
fn normalize_workspace_tab_list(tabs: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for raw in tabs {
        let id = normalize_workspace_tab_id(&raw);
        if id.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        out.push(id);
    }
    out
}

fn normalized_tab_labels(labels: &Value) -> Value {
    let mut map = labels.as_object().cloned().unwrap_or_default();
    if !map.contains_key("ingest") {
        if let Some(label) = map.get("ingest_proxy").cloned() {
            map.insert("ingest".to_string(), label);
        }
    }
    map.remove("ingest_proxy");
    Value::Object(map)
}

const WORKFLOW_TAB_ORDER: &[&str] = &[
    "project",
    "ingest",
    "pool",
    "search",
    "library",
    "off",
    "media_assist",
    "storyboard",
    "qstoryboard",
    "audio",
    "preview",
];

fn workflow_tabs_from_label_keys(label_keys: impl IntoIterator<Item = String>) -> Vec<String> {
    let keys: HashSet<String> = label_keys
        .into_iter()
        .map(|k| k.trim().to_string())
        .collect();
    let mut tabs: Vec<String> = WORKFLOW_TAB_ORDER
        .iter()
        .filter(|id| *id != &"project" && keys.contains(**id))
        .map(|s| s.to_string())
        .collect();
    if tabs.is_empty() {
        for id in WORKFLOW_TAB_ORDER.iter().skip(1) {
            if keys.contains(*id) {
                tabs.push((*id).to_string());
            }
        }
    }
    tabs.insert(0, "project".to_string());
    normalize_workspace_tab_list(tabs)
}

fn explicit_workspace_tab_ids(settings: &Value) -> Vec<String> {
    settings
        .get("workspace")
        .and_then(|w| w.get("tabs"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn materialize_workspace_tabs(settings: &Value) -> Value {
    let explicit = explicit_workspace_tab_ids(settings);
    if explicit.iter().any(|t| t != "project") {
        return normalize_settings_workspace(settings);
    }
    let normalized = normalize_settings_workspace(settings);
    let (tabs, _) = workflow_tabs_from_settings(&normalized);
    if tabs.len() <= 1 {
        return normalized;
    }
    let mut out = normalized;
    if let Some(root) = out.as_object_mut() {
        if let Some(ws) = root.get_mut("workspace").and_then(|v| v.as_object_mut()) {
            ws.insert("tabs".into(), json!(tabs));
        }
    }
    out
}

fn repair_system_template_workspaces(conn: &Connection, seed: &SeedFile) -> rusqlite::Result<()> {
    let now = now_str();
    for tpl in &seed.project_templates {
        let id = tpl
            .get("template_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if id.is_empty() {
            continue;
        }
        let seed_settings = tpl.get("settings").cloned().unwrap_or_else(|| json!({}));
        let seed_materialized = materialize_workspace_tabs(&seed_settings);
        if !settings_have_workflow_tabs(&seed_materialized) {
            continue;
        }
        let seed_workspace = seed_materialized.get("workspace").cloned();
        let current = load_object(conn, "project_template_kv", "template_id", id)?;
        let mut merged = if current.as_object().is_some_and(|o| !o.is_empty()) {
            current
        } else {
            seed_materialized.clone()
        };
        // System templates: workspace tabs/labels always follow system_seed.json.
        if let (Some(ws), Some(root)) = (seed_workspace, merged.as_object_mut()) {
            if let Some(obj) = root.get_mut("workspace").and_then(|v| v.as_object_mut()) {
                if let Some(tabs) = ws.get("tabs") {
                    obj.insert("tabs".into(), tabs.clone());
                }
                if let Some(labels) = ws.get("tab_labels") {
                    obj.insert("tab_labels".into(), labels.clone());
                }
            } else {
                root.insert("workspace".into(), ws);
            }
        }
        let merged = materialize_workspace_tabs(&merged);
        replace_object(conn, "project_template_kv", "template_id", id, &merged)?;
        conn.execute(
            "UPDATE project_templates SET updated_at = ?2 WHERE template_id = ?1",
            params![id, now],
        )?;
    }
    Ok(())
}

fn normalize_settings_workspace(settings: &Value) -> Value {
    let mut out = settings.clone();
    let Some(root) = out.as_object_mut() else {
        return out;
    };
    let workspace = root.get("workspace").cloned().unwrap_or_else(|| json!({}));
    let mut ws = workspace.as_object().cloned().unwrap_or_default();
    if let Some(arr) = ws.get("tabs").and_then(|v| v.as_array()) {
        let raw = arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        ws.insert("tabs".into(), json!(normalize_workspace_tab_list(raw)));
    }
    if let Some(labels) = ws.get("tab_labels") {
        ws.insert("tab_labels".into(), normalized_tab_labels(labels));
    }
    root.insert("workspace".into(), Value::Object(ws));
    out
}

fn workflow_tabs_from_settings(settings: &Value) -> (Vec<String>, Value) {
    let normalized = normalize_settings_workspace(settings);
    let workspace = normalized
        .get("workspace")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let mut raw_tabs = workspace
        .get("tabs")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    if raw_tabs.is_empty() {
        if let Some(labels) = workspace.get("tab_labels").and_then(|v| v.as_object()) {
            raw_tabs = workflow_tabs_from_label_keys(labels.keys().cloned());
            raw_tabs.retain(|id| id != "project");
        }
    }
    let mut tabs = normalize_workspace_tab_list(raw_tabs);
    if !tabs.iter().any(|t| t == "project") {
        tabs.insert(0, "project".to_string());
    }
    let labels = workspace
        .get("tab_labels")
        .map(normalized_tab_labels)
        .unwrap_or_else(|| json!({}));
    (tabs, labels)
}

fn label_for_tab(tab_id: &str, labels: &Value) -> String {
    labels
        .get(tab_id)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| {
            if tab_id == "project" {
                "Project".to_string()
            } else {
                tab_id.replace('_', " ")
            }
        })
}

fn plugin_id_for_tab(tab_id: &str) -> String {
    match tab_id {
        "pool" => "media_pool".to_string(),
        "storyboard" => "story".to_string(),
        "qstoryboard" => "qstory".to_string(),
        "media_assist" => "media_assist".to_string(),
        other => other.to_string(),
    }
}

fn step_id_for_tab(tab_id: &str) -> String {
    format!("step_{tab_id}")
}

fn save_template_snapshot(
    conn: &Connection,
    project_id: &str,
    template: &Value,
) -> rusqlite::Result<()> {
    let now = now_str();
    let template_id = template
        .get("template_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let template_name = template.get("name").and_then(|v| v.as_str()).unwrap_or("");
    conn.execute(
        "INSERT INTO project_template_snapshot
            (project_id, template_id, template_name, template_version, snapshot_json, created_at)
         VALUES (?1, ?2, ?3, '', '{}', ?4)
         ON CONFLICT(project_id) DO UPDATE SET
            template_id = excluded.template_id,
            template_name = excluded.template_name,
            snapshot_json = '{}'",
        params![project_id, template_id, template_name, now],
    )?;
    replace_object(
        conn,
        "project_snapshot_kv",
        "project_id",
        project_id,
        template,
    )?;
    Ok(())
}

fn write_project_workflow(
    conn: &Connection,
    project_id: &str,
    settings: &Value,
) -> rusqlite::Result<()> {
    let (tabs, labels) = workflow_tabs_from_settings(settings);
    conn.execute(
        "DELETE FROM project_workflow_steps WHERE project_id = ?1",
        params![project_id],
    )?;
    let mut entry_step_id = String::new();
    let mut previous_step_id = String::new();
    for (idx, tab_id) in tabs.iter().enumerate() {
        let step_id = step_id_for_tab(tab_id);
        if tab_id != "project" && entry_step_id.is_empty() {
            entry_step_id = step_id.clone();
        }
        if !previous_step_id.is_empty() {
            conn.execute(
                "UPDATE project_workflow_steps SET next_step_id = ?2 WHERE project_id = ?1 AND step_id = ?3",
                params![project_id, step_id, previous_step_id],
            )?;
        }
        let status = if tab_id == "project" {
            "complete"
        } else if step_id == entry_step_id {
            "active"
        } else {
            "locked"
        };
        conn.execute(
            "INSERT INTO project_workflow_steps
                (step_id, project_id, plugin_id, tab_id, label, position, status, next_step_id, settings_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, '{}')",
            params![
                step_id,
                project_id,
                plugin_id_for_tab(tab_id),
                tab_id,
                label_for_tab(tab_id, &labels),
                idx as i64,
                status,
            ],
        )?;
        previous_step_id = step_id;
    }
    let active_step_id = if entry_step_id.is_empty() {
        step_id_for_tab("project")
    } else {
        entry_step_id.clone()
    };
    let now = now_str();
    conn.execute(
        "INSERT INTO project_workflow_state (project_id, active_step_id, entry_step_id, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(project_id) DO UPDATE SET
            active_step_id = excluded.active_step_id,
            entry_step_id = excluded.entry_step_id,
            updated_at = excluded.updated_at",
        params![project_id, active_step_id, entry_step_id, now],
    )?;
    Ok(())
}

fn settings_have_workflow_tabs(settings: &Value) -> bool {
    let (tabs, _) = workflow_tabs_from_settings(settings);
    tabs.iter().any(|t| t != "project")
}

fn workflow_tab_ids_in_db(conn: &Connection, project_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT tab_id FROM project_workflow_steps WHERE project_id = ?1 ORDER BY position, step_id",
    )?;
    let rows = stmt.query_map(params![project_id], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(normalize_workspace_tab_id(&row?));
    }
    Ok(normalize_workspace_tab_list(out))
}

fn hydrate_project_settings(
    global: &Connection,
    project_conn: &Connection,
    project_id: &str,
    template_id: &str,
    settings: Value,
) -> rusqlite::Result<Value> {
    if settings_have_workflow_tabs(&settings) {
        return Ok(settings);
    }
    let mut merged = settings;
    let snapshot = load_object(
        project_conn,
        "project_snapshot_kv",
        "project_id",
        project_id,
    )?;
    if snapshot.is_object() && !snapshot.as_object().is_some_and(|o| o.is_empty()) {
        let snap_settings = snapshot
            .get("settings")
            .cloned()
            .unwrap_or_else(|| snapshot.clone());
        merged = deep_merge(&snap_settings, &merged);
        if settings_have_workflow_tabs(&merged) {
            return Ok(merged);
        }
    }
    let mut tid = template_id.to_string();
    if tid.is_empty() {
        tid = snapshot
            .get("template_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
    }
    if !tid.is_empty() {
        if let Some(tpl) = get_project_template(global, &tid)? {
            if let Some(tpl_settings) = tpl.get("settings") {
                merged = deep_merge(tpl_settings, &merged);
            }
        }
    }
    if !settings_have_workflow_tabs(&merged) {
        if let Some(tpl) = get_project_template(global, "tpl_breaking_news")? {
            if let Some(tpl_settings) = tpl.get("settings") {
                merged = deep_merge(tpl_settings, &merged);
            }
        }
    }
    Ok(merged)
}

fn resolve_persist_template_id(
    global: &Connection,
    project_conn: &Connection,
    project_id: &str,
    template_id: &str,
) -> rusqlite::Result<String> {
    if !template_id.trim().is_empty() {
        return Ok(template_id.trim().to_string());
    }
    let snapshot = load_object(
        project_conn,
        "project_snapshot_kv",
        "project_id",
        project_id,
    )?;
    if let Some(tid) = snapshot.get("template_id").and_then(|v| v.as_str()) {
        if !tid.trim().is_empty() {
            return Ok(tid.trim().to_string());
        }
    }
    if get_project_template(global, "tpl_breaking_news")?.is_some() {
        return Ok("tpl_breaking_news".to_string());
    }
    Ok(String::new())
}

fn persist_project_settings_kv(
    conn: &Connection,
    project_id: &str,
    template_id: &str,
    settings: &Value,
) -> rusqlite::Result<()> {
    replace_object(
        conn,
        "project_settings_kv",
        "project_id",
        project_id,
        settings,
    )?;
    let now = now_str();
    conn.execute(
        "INSERT INTO project_settings (project_id, template_id, settings_json, created_at, updated_at)
         VALUES (?1, ?2, '{}', ?3, ?3)
         ON CONFLICT(project_id) DO UPDATE SET
            template_id = CASE
                WHEN excluded.template_id != '' THEN excluded.template_id
                ELSE project_settings.template_id
            END,
            updated_at = excluded.updated_at",
        params![project_id, template_id, now],
    )?;
    Ok(())
}

fn ensure_project_workflow(
    conn: &Connection,
    project_id: &str,
    settings: &Value,
) -> rusqlite::Result<()> {
    let (expected, _) = workflow_tabs_from_settings(settings);
    let current = workflow_tab_ids_in_db(conn, project_id)?;
    let state = workflow_state(conn, project_id)?;
    let entry = state
        .get("entry_step_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let needs_repair =
        current.is_empty() || current != expected || (expected.len() > 1 && entry.is_empty());
    if needs_repair {
        write_project_workflow(conn, project_id, settings)?;
    }
    Ok(())
}

fn list_workflow_steps(conn: &Connection, project_id: &str) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT step_id, plugin_id, tab_id, label, position, status, next_step_id
         FROM project_workflow_steps
         WHERE project_id = ?1
         ORDER BY position, step_id",
    )?;
    let rows = stmt.query_map(params![project_id], |r| {
        let step_id: String = r.get(0)?;
        Ok((
            step_id,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, Option<String>>(6)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (step_id, plugin_id, tab_id, label, position, status, next_step_id) = row?;
        let settings = load_object(conn, "project_workflow_step_kv", "step_id", &step_id)?;
        out.push(json!({
            "step_id": step_id,
            "plugin_id": plugin_id,
            "tab_id": tab_id,
            "label": label,
            "position": position,
            "status": status,
            "next_step_id": next_step_id,
            "settings": settings,
        }));
    }
    Ok(out)
}

fn workflow_state(conn: &Connection, project_id: &str) -> rusqlite::Result<Value> {
    let row: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT active_step_id, entry_step_id FROM project_workflow_state WHERE project_id = ?1",
            params![project_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let Some((active_step_id, entry_step_id)) = row else {
        return Ok(json!({}));
    };
    Ok(json!({
        "active_step_id": active_step_id,
        "entry_step_id": entry_step_id,
    }))
}

/// Idempotent SQLite migration for legacy `ingest_proxy` workspace tab ids.
pub(crate) fn migrate_legacy_ingest_workflow(
    conn: &Connection,
    project_id: &str,
) -> rusqlite::Result<()> {
    let pid = project_id.trim();
    if pid.is_empty() || !project_needs_ingest_migration(conn, pid)? {
        return Ok(());
    }

    let settings = load_object(conn, "project_settings_kv", "project_id", pid)?;
    let normalized = normalize_settings_workspace(&settings);
    if normalized != settings {
        replace_object(conn, "project_settings_kv", "project_id", pid, &normalized)?;
    }

    let has_ingest: i64 = conn.query_row(
        "SELECT COUNT(*) FROM project_workflow_steps
         WHERE project_id = ?1 AND (step_id = 'step_ingest' OR tab_id = 'ingest')",
        params![pid],
        |r| r.get(0),
    )?;
    let has_proxy: i64 = conn.query_row(
        "SELECT COUNT(*) FROM project_workflow_steps
         WHERE project_id = ?1 AND (step_id = 'step_ingest_proxy' OR tab_id = 'ingest_proxy')",
        params![pid],
        |r| r.get(0),
    )?;

    if has_proxy > 0 && has_ingest > 0 {
        let proxy_next: Option<String> = conn
            .query_row(
                "SELECT next_step_id FROM project_workflow_steps
                 WHERE project_id = ?1 AND step_id = 'step_ingest_proxy'",
                params![pid],
                |r| r.get(0),
            )
            .ok()
            .flatten();
        let proxy_status: Option<String> = conn
            .query_row(
                "SELECT status FROM project_workflow_steps
                 WHERE project_id = ?1 AND step_id = 'step_ingest_proxy'",
                params![pid],
                |r| r.get(0),
            )
            .ok();
        conn.execute(
            "UPDATE project_workflow_steps SET next_step_id = ?2
             WHERE project_id = ?1 AND next_step_id = 'step_ingest_proxy'",
            params![pid, proxy_next],
        )?;
        if proxy_status.as_deref() == Some("active") {
            conn.execute(
                "UPDATE project_workflow_steps SET status = 'locked'
                 WHERE project_id = ?1 AND status = 'active' AND step_id != 'step_ingest'",
                params![pid],
            )?;
            conn.execute(
                "UPDATE project_workflow_steps SET status = 'active'
                 WHERE project_id = ?1 AND step_id = 'step_ingest'",
                params![pid],
            )?;
        }
        conn.execute(
            "DELETE FROM project_workflow_step_kv WHERE step_id = 'step_ingest_proxy'",
            [],
        )?;
        conn.execute(
            "DELETE FROM project_workflow_steps
             WHERE project_id = ?1 AND step_id = 'step_ingest_proxy'",
            params![pid],
        )?;
    } else if has_proxy > 0 {
        migrate_workflow_step_kv(conn, "step_ingest_proxy", "step_ingest")?;
        conn.execute(
            "UPDATE project_workflow_steps SET next_step_id = 'step_ingest'
             WHERE project_id = ?1 AND next_step_id = 'step_ingest_proxy'",
            params![pid],
        )?;
        conn.execute(
            "UPDATE project_workflow_steps
             SET step_id = 'step_ingest', tab_id = 'ingest', plugin_id = 'ingest'
             WHERE project_id = ?1 AND step_id = 'step_ingest_proxy'",
            params![pid],
        )?;
    }

    conn.execute(
        "UPDATE project_workflow_steps SET tab_id = 'ingest', plugin_id = 'ingest'
         WHERE project_id = ?1 AND tab_id = 'ingest_proxy'",
        params![pid],
    )?;
    conn.execute(
        "UPDATE project_workflow_state SET active_step_id = 'step_ingest'
         WHERE project_id = ?1 AND active_step_id = 'step_ingest_proxy'",
        params![pid],
    )?;
    conn.execute(
        "UPDATE project_workflow_state SET entry_step_id = 'step_ingest'
         WHERE project_id = ?1 AND entry_step_id = 'step_ingest_proxy'",
        params![pid],
    )?;
    let now = now_str();
    conn.execute(
        "UPDATE project_workflow_state SET updated_at = ?2 WHERE project_id = ?1",
        params![pid, now],
    )?;
    Ok(())
}

fn project_needs_ingest_migration(conn: &Connection, project_id: &str) -> rusqlite::Result<bool> {
    let pid = project_id.trim();
    let legacy_steps: i64 = conn.query_row(
        "SELECT COUNT(*) FROM project_workflow_steps
         WHERE project_id = ?1 AND (tab_id = 'ingest_proxy' OR step_id = 'step_ingest_proxy')",
        params![pid],
        |r| r.get(0),
    )?;
    if legacy_steps > 0 {
        return Ok(true);
    }
    let state_legacy: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM project_workflow_state
             WHERE project_id = ?1
               AND (active_step_id = 'step_ingest_proxy' OR entry_step_id = 'step_ingest_proxy')",
            params![pid],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if state_legacy > 0 {
        return Ok(true);
    }
    let settings = load_object(conn, "project_settings_kv", "project_id", pid)?;
    Ok(settings_needs_ingest_migration(&settings))
}

fn settings_needs_ingest_migration(settings: &Value) -> bool {
    if settings
        .get("workspace")
        .and_then(|w| w.get("tabs"))
        .and_then(|t| t.as_array())
        .is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some("ingest_proxy")))
    {
        return true;
    }
    settings
        .get("workspace")
        .and_then(|w| w.get("tab_labels"))
        .and_then(|l| l.as_object())
        .is_some_and(|map| map.contains_key("ingest_proxy"))
}

fn migrate_workflow_step_kv(
    conn: &Connection,
    from_step: &str,
    to_step: &str,
) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT setting_key, setting_value FROM project_workflow_step_kv WHERE step_id = ?1",
    )?;
    let rows: Vec<(String, String)> = stmt
        .query_map(params![from_step], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    for (key, value) in rows {
        conn.execute(
            "INSERT INTO project_workflow_step_kv (step_id, setting_key, setting_value)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(step_id, setting_key) DO NOTHING",
            params![to_step, key, value],
        )?;
    }
    conn.execute(
        "DELETE FROM project_workflow_step_kv WHERE step_id = ?1",
        params![from_step],
    )?;
    Ok(())
}

#[cfg(test)]
mod legacy_ingest_tests {
    use super::*;
    use crate::project::db::{
        ensure_project_dirs_at, open_global, open_project, project_dir_in_root, ProjectPaths,
    };
    use crate::project::store::upsert_project_meta;
    use std::path::PathBuf;

    fn test_paths(base: &std::path::Path) -> ProjectPaths {
        ProjectPaths {
            data_dir: base.join("data"),
            projects_root: base.join("projects"),
            seed_path: PathBuf::from("nonexistent"),
        }
    }

    fn open_registered_project(
        paths: &ProjectPaths,
        project_id: &str,
    ) -> rusqlite::Result<Connection> {
        let global = open_global(paths)?;
        let project_dir = project_dir_in_root(&paths.projects_root, project_id);
        upsert_project_meta(&global, project_id, project_id, None, Some(&project_dir))?;
        ensure_project_dirs_at(&project_dir)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        open_project(paths, project_id)
    }

    fn seed_project(conn: &Connection, project_id: &str, settings: &Value) -> rusqlite::Result<()> {
        conn.execute(
            "INSERT INTO project_settings (project_id, template_id, settings_json)
             VALUES (?1, 'tpl_breaking_news', '{}')",
            params![project_id],
        )?;
        replace_object(
            conn,
            "project_settings_kv",
            "project_id",
            project_id,
            settings,
        )?;
        write_project_workflow(conn, project_id, settings)
    }

    fn corrupt_to_legacy(conn: &Connection, project_id: &str) -> rusqlite::Result<()> {
        conn.execute(
            "UPDATE project_workflow_steps
             SET step_id = 'step_ingest_proxy', tab_id = 'ingest_proxy', plugin_id = 'ingest_proxy'
             WHERE project_id = ?1 AND step_id = 'step_ingest'",
            params![project_id],
        )?;
        conn.execute(
            "UPDATE project_workflow_steps SET next_step_id = 'step_ingest_proxy'
             WHERE project_id = ?1 AND next_step_id = 'step_ingest'",
            params![project_id],
        )?;
        conn.execute(
            "UPDATE project_workflow_state
             SET active_step_id = 'step_ingest_proxy', entry_step_id = 'step_ingest_proxy'
             WHERE project_id = ?1",
            params![project_id],
        )?;
        conn.execute(
            "UPDATE project_settings_kv SET setting_value = 'ingest_proxy'
             WHERE project_id = ?1 AND setting_key = 'workspace.tabs[1]'",
            params![project_id],
        )?;
        let proxy_steps: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM project_workflow_steps
                 WHERE project_id = ?1 AND step_id = 'step_ingest_proxy'",
                params![project_id],
                |r| r.get(0),
            )
            .expect("count legacy ingest_proxy steps");
        assert!(
            proxy_steps > 0,
            "legacy ingest corruption fixture did not update workflow steps"
        );
        Ok(())
    }

    fn breaking_news_settings() -> Value {
        json!({
            "workspace": {
                "tabs": ["project", "ingest", "pool"],
                "tab_labels": { "ingest": "Ingest", "pool": "Media" }
            }
        })
    }

    #[test]
    fn workspace_get_reads_existing_db_workflow_and_prepare_owns_repair() {
        let base = std::env::temp_dir().join(format!(
            "qnc_workspace_db_first_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&base);
        let paths = test_paths(&base);
        let project_id = "qa_workspace_db_first";
        let global = open_global(&paths).unwrap();
        let project_dir = project_dir_in_root(&paths.projects_root, project_id);
        upsert_project_meta(&global, project_id, project_id, None, Some(&project_dir)).unwrap();
        ensure_project_dirs_at(&project_dir).unwrap();
        let conn = open_project(&paths, project_id).unwrap();
        let settings = breaking_news_settings();
        conn.execute(
            "INSERT INTO project_settings (project_id, template_id, settings_json)
             VALUES (?1, 'tpl_breaking_news', '{}')",
            params![project_id],
        )
        .unwrap();
        replace_object(
            &conn,
            "project_settings_kv",
            "project_id",
            project_id,
            &settings,
        )
        .unwrap();

        assert!(
            get_project_workspace(&global, &paths, project_id).is_err(),
            "workspace GET must not derive workflow from settings/template on read"
        );

        let repaired = prepare_project_workspace(&global, &paths, project_id).unwrap();
        assert_eq!(
            repaired
                .get("entry_step_id")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            "step_ingest"
        );
        let readonly = get_project_workspace(&global, &paths, project_id).unwrap();
        assert_eq!(
            readonly
                .get("entry_step_id")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            "step_ingest"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn legacy_ingest_renames_proxy_step_and_settings() {
        let base = std::env::temp_dir().join(format!(
            "qnc_legacy_ingest_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&base);
        let paths = test_paths(&base);
        let project_id = "qa_legacy_ingest";
        let conn = open_registered_project(&paths, project_id).unwrap();
        seed_project(&conn, project_id, &breaking_news_settings()).unwrap();
        corrupt_to_legacy(&conn, project_id).unwrap();

        migrate_legacy_ingest_workflow(&conn, project_id).unwrap();
        migrate_legacy_ingest_workflow(&conn, project_id).unwrap();

        let settings = load_object(&conn, "project_settings_kv", "project_id", project_id).unwrap();
        let tabs = settings["workspace"]["tabs"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(!tabs.iter().any(|v| v.as_str() == Some("ingest_proxy")));
        assert_eq!(
            tabs.iter().filter(|v| v.as_str() == Some("ingest")).count(),
            1
        );

        let proxy_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM project_workflow_steps
                 WHERE project_id = ?1 AND (tab_id = 'ingest_proxy' OR step_id = 'step_ingest_proxy')",
                params![project_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(proxy_count, 0);

        let ingest_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM project_workflow_steps
                 WHERE project_id = ?1 AND tab_id = 'ingest'",
                params![project_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ingest_count, 1);

        let (active, entry): (String, String) = conn
            .query_row(
                "SELECT active_step_id, entry_step_id FROM project_workflow_state WHERE project_id = ?1",
                params![project_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(active, "step_ingest");
        assert_eq!(entry, "step_ingest");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn workspace_get_migrates_legacy_ingest_proxy_state() {
        let base = std::env::temp_dir().join(format!(
            "qnc_legacy_ingest_get_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&base);
        let paths = test_paths(&base);
        let project_id = "qa_legacy_ingest_get";
        let global = open_global(&paths).unwrap();
        let conn = open_registered_project(&paths, project_id).unwrap();
        seed_project(&conn, project_id, &breaking_news_settings()).unwrap();
        corrupt_to_legacy(&conn, project_id).unwrap();
        drop(conn);

        let workspace = get_project_workspace(&global, &paths, project_id).unwrap();
        assert_eq!(
            workspace
                .get("active_step_id")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            "step_ingest"
        );
        assert_eq!(
            workspace
                .get("entry_step_id")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            "step_ingest"
        );
        let tabs = workspace
            .get("tabs")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(!tabs.iter().any(|tab| tab.as_str() == Some("ingest_proxy")));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn legacy_ingest_dedupes_duplicate_proxy_and_ingest_steps() {
        let base = std::env::temp_dir().join(format!(
            "qnc_legacy_ingest_dup_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&base);
        let paths = test_paths(&base);
        let project_id = "qa_legacy_dup";
        let settings = json!({
            "workspace": {
                "tabs": ["project", "ingest", "ingest_proxy", "pool"],
                "tab_labels": { "ingest": "Ingest", "ingest_proxy": "Proxy", "pool": "Media" }
            }
        });
        let conn = open_registered_project(&paths, project_id).unwrap();
        seed_project(&conn, project_id, &settings).unwrap();
        conn.execute(
            "INSERT INTO project_workflow_steps
                (step_id, project_id, plugin_id, tab_id, label, position, status, next_step_id, settings_json)
             VALUES ('step_ingest_proxy', ?1, 'ingest_proxy', 'ingest_proxy', 'Proxy', 2, 'locked', 'step_pool', '{}')",
            params![project_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE project_workflow_steps SET next_step_id = 'step_ingest_proxy'
             WHERE project_id = ?1 AND step_id = 'step_ingest'",
            params![project_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE project_workflow_steps SET position = 3 WHERE project_id = ?1 AND step_id = 'step_pool'",
            params![project_id],
        )
        .unwrap();

        migrate_legacy_ingest_workflow(&conn, project_id).unwrap();

        let ingest_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM project_workflow_steps
                 WHERE project_id = ?1 AND tab_id = 'ingest'",
                params![project_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ingest_count, 1);
        let proxy_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM project_workflow_steps
                 WHERE project_id = ?1 AND tab_id = 'ingest_proxy'",
                params![project_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(proxy_count, 0);
        let ingest_next: Option<String> = conn
            .query_row(
                "SELECT next_step_id FROM project_workflow_steps
                 WHERE project_id = ?1 AND step_id = 'step_ingest'",
                params![project_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ingest_next.as_deref(), Some("step_pool"));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn legacy_ingest_corrupt_and_migrate() {
        let db_path = match std::env::var("QNC_TEST_PROJECT_DB") {
            Ok(v) if !v.trim().is_empty() => v,
            _ => return,
        };
        let project_id = match std::env::var("QNC_TEST_PROJECT_ID") {
            Ok(v) if !v.trim().is_empty() => v,
            _ => return,
        };
        let conn = Connection::open(&db_path).expect("open project db for legacy ingest test");
        corrupt_to_legacy(&conn, &project_id).expect("corrupt project db to legacy ingest_proxy");
    }

    #[test]
    fn workspace_tabs_derived_from_tab_labels_when_tabs_missing() {
        let settings = json!({
            "workspace": {
                "tab_labels": {
                    "ingest": "Ingest",
                    "pool": "Media",
                    "qstoryboard": "QStory",
                    "preview": "Export"
                }
            }
        });
        let (tabs, _) = workflow_tabs_from_settings(&settings);
        assert!(tabs.iter().any(|t| t == "ingest"));
        assert!(tabs.iter().any(|t| t == "qstoryboard"));
        assert_eq!(tabs.first().map(String::as_str), Some("project"));

        let materialized = materialize_workspace_tabs(&settings);
        let stored = materialized["workspace"]["tabs"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(stored.iter().any(|v| v.as_str() == Some("ingest")));
        assert!(stored.iter().any(|v| v.as_str() == Some("qstoryboard")));
    }

    #[test]
    fn export_settings_reject_forbidden_pal_progressive() {
        let settings = json!({
            "export": {
                "preset": "single_rate_pal",
                "format": "PAL single-rate progressive",
                "fps": 25,
                "field_order": "progressive"
            }
        });

        assert!(validate_project_export_settings(&settings).is_err());
    }

    #[test]
    fn export_settings_allow_i50() {
        let i50 = json!({
            "export": {
                "preset": "xdcam_hd422_50i",
                "format": "HD 1080i50",
                "fps": 25,
                "field_order": "upper_first"
            }
        });

        assert!(validate_project_export_settings(&i50).is_ok());
    }
}
