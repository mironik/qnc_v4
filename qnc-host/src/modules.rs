use std::collections::HashMap;
use std::path::Path;

use serde_json::{json, Value};

use crate::project::db::{load_module_enabled, open_shell_db, upsert_module_enabled};

#[derive(Clone)]
pub struct ModuleStore {
    enabled: HashMap<String, bool>,
}

pub enum ModuleError {
    NotFound,
    NotRemovable,
}

impl ModuleStore {
    pub fn load(data_dir: &Path) -> Self {
        let enabled = open_shell_db(data_dir)
            .and_then(|conn| load_module_enabled(&conn))
            .unwrap_or_default();
        Self { enabled }
    }

    fn persist_enabled(data_dir: &Path, module_id: &str, enabled: bool) {
        if let Ok(conn) = open_shell_db(data_dir) {
            upsert_module_enabled(&conn, module_id, enabled).ok();
        }
    }

    pub fn apply_enabled(&self, root: &Path, manifests: Vec<Value>) -> Vec<Value> {
        let design_cap = crate::design::design_editor_capability(root);
        let design_available = design_cap
            .get("available")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let design_default = design_cap
            .get("default_enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        manifests
            .into_iter()
            .filter_map(|mut m| {
                let tab_id = m
                    .get("tab_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if tab_id.is_empty() {
                    return None;
                }
                let requires = m
                    .get("requires_capability")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if requires == "design_editor" && !design_available {
                    return None;
                }
                let default_enabled = m.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                let mut enabled = self
                    .enabled
                    .get(&tab_id)
                    .copied()
                    .unwrap_or(default_enabled);
                if tab_id == "design-tools"
                    && design_available
                    && design_default
                    && !self.enabled.contains_key(&tab_id)
                {
                    enabled = true;
                }
                if !enabled {
                    return None;
                }
                if let Some(obj) = m.as_object_mut() {
                    obj.insert("enabled".into(), json!(enabled));
                    obj.insert("module_id".into(), json!(tab_id));
                }
                Some(m)
            })
            .collect()
    }

    pub fn as_module_list(&self, manifests: Vec<Value>) -> Vec<Value> {
        manifests
            .into_iter()
            .filter_map(|mut m| {
                let tab_id = m
                    .get("tab_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if tab_id.is_empty() {
                    return None;
                }
                let default_enabled = m.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                let enabled = self
                    .enabled
                    .get(&tab_id)
                    .copied()
                    .unwrap_or(default_enabled);
                if let Some(obj) = m.as_object_mut() {
                    obj.insert("module_id".into(), json!(tab_id));
                    obj.insert("enabled".into(), json!(enabled));
                }
                Some(m)
            })
            .collect()
    }

    pub fn set_enabled(
        &mut self,
        data_dir: &Path,
        manifests: &[Value],
        module_id: &str,
        enabled: bool,
    ) -> Result<Value, ModuleError> {
        let manifest = manifests
            .iter()
            .find(|m| m.get("tab_id").and_then(|v| v.as_str()) == Some(module_id))
            .ok_or(ModuleError::NotFound)?;

        if !enabled && manifest.get("removable").and_then(|v| v.as_bool()) == Some(false) {
            return Err(ModuleError::NotRemovable);
        }

        self.enabled.insert(module_id.to_string(), enabled);
        Self::persist_enabled(data_dir, module_id, enabled);

        let mut out = manifest.clone();
        if let Some(obj) = out.as_object_mut() {
            obj.insert("module_id".into(), json!(module_id));
            obj.insert("enabled".into(), json!(enabled));
        }
        Ok(out)
    }
}
