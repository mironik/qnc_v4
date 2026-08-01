use std::path::Path;

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::project::db::ProjectPaths;
use crate::tabs;

const CONTRACT: &str = "db-first-v1";
const CHECKPOINT: &str = "db-first-clean-2026-06-18";

pub fn diagnostics(root: &Path, paths: &ProjectPaths) -> Value {
    let mut checks = Vec::new();
    let mut violations = Vec::new();
    let mut plugins_sdk = Vec::new();

    let scan = tabs::scan_plugin_manifests(&crate::workspace_paths::tabs_dir(root));
    for err in &scan.errors {
        violations.push(format!("plugin manifest: {err:?}"));
    }

    for manifest in &scan.manifests {
        let plugin_id = manifest
            .get("plugin_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if plugin_id.is_empty() {
            continue;
        }
        let sdk_version = manifest.get("sdk_version").and_then(|v| v.as_i64());
        let persistence = manifest
            .get("backend")
            .and_then(|b| b.get("store"))
            .and_then(|s| s.get("persistence"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let snapshot_keys: Vec<String> = manifest
            .get("state")
            .and_then(|s| s.get("snapshots"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| item.get("key").and_then(|k| k.as_str()).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        if plugin_id == "sdk_demo" {
            if persistence == "in_memory_demo" || persistence == "in_memory" {
                violations.push(format!(
                    "sdk_demo backend.store.persistence must be project DB, got '{persistence}'"
                ));
            }
            if manifest
                .get("description")
                .and_then(|v| v.as_str())
                .is_some_and(|d| d.to_ascii_lowercase().contains("in-memory rust state"))
            {
                violations
                    .push("sdk_demo description still references in-memory Rust state".into());
            }
        }

        if sdk_version == Some(1) {
            plugins_sdk.push(json!({
                "plugin_id": plugin_id,
                "sdk_version": 1,
                "persistence": persistence,
                "snapshot_keys": snapshot_keys,
            }));
        }
    }

    match inspect_global_db(paths) {
        Ok(db_checks) => checks.extend(db_checks),
        Err(msg) => violations.push(msg),
    }

    for note in runtime_legacy_notes(root) {
        checks.push(note);
    }

    let status = if violations.is_empty() { "ok" } else { "warn" };

    json!({
        "status": status,
        "contract": CONTRACT,
        "checkpoint": CHECKPOINT,
        "checks": checks,
        "violations": violations,
        "plugins_sdk": plugins_sdk,
    })
}

fn inspect_global_db(paths: &ProjectPaths) -> Result<Vec<Value>, String> {
    let conn = crate::project::db::open_global(paths).map_err(|e| e.to_string())?;
    let mut out = Vec::new();

    for table in [
        "module_state",
        "app_settings",
        "projects",
        "project_template_kv",
    ] {
        let ok = table_exists(&conn, table);
        out.push(json!({
            "id": format!("global_db.{table}"),
            "ok": ok,
        }));
        if !ok {
            return Err(format!("project_store.db missing table '{table}'"));
        }
    }

    let migrated = crate::project::db::get_setting(&conn, "design.files_migrated", "0")
        .unwrap_or_else(|_| "0".into());
    out.push(json!({
        "id": "design.files_migrated",
        "ok": true,
        "value": migrated,
    }));

    Ok(out)
}

fn table_exists(conn: &Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1 LIMIT 1",
        rusqlite::params![table],
        |_| Ok(()),
    )
    .is_ok()
}

fn runtime_legacy_notes(root: &Path) -> Vec<Value> {
    let mut notes = Vec::new();
    for (id, rel) in [
        (
            "legacy.shell_module_state_json",
            "data/shell_module_state.json",
        ),
        (
            "legacy.design_overrides_tokens",
            "data/design_overrides/tokens.json",
        ),
    ] {
        let path = root.join(rel);
        if path.is_file() {
            notes.push(json!({
                "id": id,
                "ok": true,
                "present": true,
                "note": "file on disk; import/migration expected on host boot, not workflow truth",
            }));
        }
    }
    notes
}
