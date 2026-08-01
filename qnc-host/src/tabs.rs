use std::fs;
use std::path::Path;

use serde::Serialize;
use serde_json::{json, Value};
use tracing::warn;

#[derive(Clone, Debug, Serialize)]
pub struct PluginManifestError {
    pub path: String,
    pub error: String,
}

pub struct PluginScanResult {
    pub manifests: Vec<Value>,
    pub errors: Vec<PluginManifestError>,
}

pub fn list_tab_manifests(plugins_root: &Path) -> Vec<Value> {
    let mut scan = scan_plugin_manifests(plugins_root);
    scan.manifests.sort_by_key(sort_key);
    scan.manifests
}

pub fn scan_plugin_manifests(plugins_root: &Path) -> PluginScanResult {
    let mut manifests = Vec::new();
    let mut errors = Vec::new();

    let Ok(entries) = fs::read_dir(plugins_root) else {
        let msg = format!("cannot read plugins directory: {}", plugins_root.display());
        warn!(path = %plugins_root.display(), "Cannot read plugins directory; no tab manifests loaded");
        errors.push(PluginManifestError {
            path: plugins_root.display().to_string(),
            error: msg,
        });
        return PluginScanResult { manifests, errors };
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let manifest_path = path.join("plugin.json");
        if !manifest_path.is_file() {
            continue;
        }
        let raw = match fs::read_to_string(&manifest_path) {
            Ok(raw) => raw,
            Err(e) => {
                record_manifest_error(&mut errors, &manifest_path, format!("read failed: {e}"));
                continue;
            }
        };
        let mut data = match serde_json::from_str::<Value>(&raw) {
            Ok(v) => v,
            Err(e) => {
                record_manifest_error(&mut errors, &manifest_path, format!("invalid JSON: {e}"));
                continue;
            }
        };
        if !data.is_object() {
            record_manifest_error(
                &mut errors,
                &manifest_path,
                "root must be a JSON object".into(),
            );
            continue;
        }
        normalize_manifest(&path, &mut data);
        manifests.push(data);
    }

    PluginScanResult { manifests, errors }
}

fn record_manifest_error(errors: &mut Vec<PluginManifestError>, path: &Path, error: String) {
    warn!(path = %path.display(), %error, "Skipping plugin manifest");
    errors.push(PluginManifestError {
        path: path.display().to_string(),
        error,
    });
}

fn normalize_manifest(plugin_dir: &Path, data: &mut Value) {
    let default_id = plugin_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("plugin")
        .to_string();
    if let Some(obj) = data.as_object_mut() {
        if !obj.contains_key("plugin_id") {
            obj.insert("plugin_id".into(), json!(default_id));
        }
        if !obj.contains_key("tab_id") {
            let pid = obj
                .get("plugin_id")
                .and_then(|v| v.as_str())
                .unwrap_or(&default_id);
            obj.insert("tab_id".into(), json!(pid));
        }
        if !obj.contains_key("enabled") {
            obj.insert("enabled".into(), json!(true));
        }
    }
}

fn sort_key(item: &Value) -> (i32, i32, String) {
    let tab_id = item.get("tab_id").and_then(|v| v.as_str()).unwrap_or("");
    let position = item
        .get("position")
        .and_then(|v| v.as_str())
        .unwrap_or("normal");
    let bucket = if position == "first" || tab_id == "project" {
        -1
    } else if position == "last" || tab_id == "preview" || tab_id == "export" {
        1
    } else {
        0
    };
    let priority = item.get("priority").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    let label = item
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or(tab_id)
        .to_string();
    (bucket, priority, label)
}
