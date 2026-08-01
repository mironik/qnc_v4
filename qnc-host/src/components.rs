use std::path::Path;

use serde_json::{json, Map, Value};

use crate::config::read_json;

pub fn list_global(components_root: &Path) -> Value {
    let registry_path = components_root.join("registry.json");
    let reg =
        read_json(&registry_path).unwrap_or_else(|| json!({ "version": 1, "components": {} }));
    let version = reg.get("version").cloned().unwrap_or(json!(1));
    let components = reg
        .get("components")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let enriched = enrich_components(components);
    json!({ "version": version, "components": enriched })
}

fn enrich_components(components: Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (global_id, mut item) in components {
        if let Some(obj) = item.as_object_mut() {
            if !obj.contains_key("global_id") {
                obj.insert("global_id".into(), json!(global_id));
            }
        }
        out.insert(global_id, item);
    }
    out
}
