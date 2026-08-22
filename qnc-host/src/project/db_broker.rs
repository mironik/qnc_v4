use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use rusqlite::Connection;
use serde_json::{json, Value};

use super::db::{open_global, open_project, ProjectPaths};

#[derive(Clone)]
pub struct ProjectDbBroker {
    paths: ProjectPaths,
    global_gate: Arc<Mutex<()>>,
    project_gates: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    runtime_cache: Arc<RwLock<HashMap<String, Value>>>,
}

impl ProjectDbBroker {
    pub fn new(paths: ProjectPaths) -> Self {
        Self {
            paths,
            global_gate: Arc::new(Mutex::new(())),
            project_gates: Arc::new(Mutex::new(HashMap::new())),
            runtime_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn layout_snapshot(&self, active_project_id: Option<&str>) -> Value {
        let active_project_id = active_project_id
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .unwrap_or("");
        json!({
            "kind": "project_db_broker",
            "status": "active",
            "global_catalog_db": self.paths.global_db().to_string_lossy(),
            "projects_root": self.paths.projects_root.to_string_lossy(),
            "active_project_id": active_project_id,
            "active_project_db": if active_project_id.is_empty() {
                Value::Null
            } else {
                json!(self.paths.project_db(active_project_id).to_string_lossy())
            },
            "sqlite_role": {
                "global": "project_catalog",
                "per_project": "project_truth",
                "runtime_cache": "non_durable_fast_state"
            },
            "write_model": "single_writer_gate_per_project",
            "ui_db_access": "host_api_only",
            "worker_db_access": "broker_only_for_new_code",
        })
    }

    pub fn with_global<T, F>(&self, f: F) -> Result<T, String>
    where
        F: FnOnce(&Connection) -> Result<T, String>,
    {
        let _guard = self.global_gate.lock().map_err(|e| e.to_string())?;
        let conn = open_global(&self.paths).map_err(|e| e.to_string())?;
        f(&conn)
    }

    #[allow(dead_code)]
    pub fn with_project_read<T, F>(&self, project_id: &str, f: F) -> Result<T, String>
    where
        F: FnOnce(&Connection) -> Result<T, String>,
    {
        let conn = open_project(&self.paths, project_id).map_err(|e| e.to_string())?;
        f(&conn)
    }

    #[allow(dead_code)]
    pub fn with_project_write<T, F>(&self, project_id: &str, f: F) -> Result<T, String>
    where
        F: FnOnce(&Connection) -> Result<T, String>,
    {
        let gate = self.project_gate(project_id)?;
        let _guard = gate.lock().map_err(|e| e.to_string())?;
        let conn = open_project(&self.paths, project_id).map_err(|e| e.to_string())?;
        f(&conn)
    }

    pub fn serialize_project_write<T, F>(&self, project_id: &str, f: F) -> Result<T, String>
    where
        F: FnOnce() -> Result<T, String>,
    {
        let gate = self.project_gate(project_id)?;
        let _guard = gate.lock().map_err(|e| e.to_string())?;
        f()
    }

    pub fn put_runtime_cache(&self, project_id: &str, key: &str, value: Value) {
        let cache_key = runtime_key(project_id, key);
        if let Ok(mut cache) = self.runtime_cache.write() {
            cache.insert(cache_key, value);
        }
    }

    pub fn get_runtime_cache(&self, project_id: &str, key: &str) -> Option<Value> {
        let cache_key = runtime_key(project_id, key);
        self.runtime_cache
            .read()
            .ok()
            .and_then(|cache| cache.get(&cache_key).cloned())
    }

    #[allow(dead_code)]
    pub fn clear_project_runtime_cache(&self, project_id: &str) {
        let prefix = runtime_key(project_id, "");
        if let Ok(mut cache) = self.runtime_cache.write() {
            cache.retain(|key, _| !key.starts_with(&prefix));
        }
    }

    fn project_gate(&self, project_id: &str) -> Result<Arc<Mutex<()>>, String> {
        let pid = project_id.trim();
        if pid.is_empty() {
            return Err("project_id required".into());
        }
        let mut gates = self.project_gates.lock().map_err(|e| e.to_string())?;
        Ok(gates
            .entry(pid.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone())
    }
}

fn runtime_key(project_id: &str, key: &str) -> String {
    format!("{}\u{1f}{}", project_id.trim(), key.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::db::{ensure_project_dirs_at, project_dir_in_root};
    use crate::project::store::upsert_project_meta;

    fn test_paths(base: &std::path::Path) -> ProjectPaths {
        ProjectPaths {
            data_dir: base.join("data"),
            projects_root: base.join("projects"),
            seed_path: base.join("seed.json"),
        }
    }

    #[test]
    fn broker_reports_global_and_project_db_layout() {
        let base =
            std::env::temp_dir().join(format!("qnc_db_broker_layout_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let paths = test_paths(&base);
        let broker = ProjectDbBroker::new(paths.clone());
        let snapshot = broker.layout_snapshot(Some("project_a"));

        assert_eq!(snapshot["kind"], "project_db_broker");
        assert!(snapshot["global_catalog_db"]
            .as_str()
            .unwrap()
            .ends_with("project_store.db"));
        assert!(snapshot["active_project_db"]
            .as_str()
            .unwrap()
            .ends_with("qnc_project.db"));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn runtime_cache_is_scoped_per_project() {
        let base = std::env::temp_dir().join(format!("qnc_db_broker_cache_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let broker = ProjectDbBroker::new(test_paths(&base));

        broker.put_runtime_cache("p1", "asset:clip", json!({"status": "building"}));
        broker.put_runtime_cache("p2", "asset:clip", json!({"status": "ready"}));

        assert_eq!(
            broker.get_runtime_cache("p1", "asset:clip").unwrap()["status"],
            "building"
        );
        assert_eq!(
            broker.get_runtime_cache("p2", "asset:clip").unwrap()["status"],
            "ready"
        );
        broker.clear_project_runtime_cache("p1");
        assert!(broker.get_runtime_cache("p1", "asset:clip").is_none());
        assert!(broker.get_runtime_cache("p2", "asset:clip").is_some());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn broker_can_write_project_db_through_single_entrypoint() {
        let base = std::env::temp_dir().join(format!("qnc_db_broker_write_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let paths = test_paths(&base);
        let broker = ProjectDbBroker::new(paths.clone());
        broker
            .with_global(|conn| {
                let project_dir = project_dir_in_root(&paths.projects_root, "project_a");
                upsert_project_meta(conn, "project_a", "Project A", None, Some(&project_dir))
                    .map_err(|e| e.to_string())?;
                ensure_project_dirs_at(&project_dir).map_err(|e| e.to_string())?;
                Ok(())
            })
            .unwrap();
        broker
            .with_project_write("project_a", |conn| {
                conn.execute(
                    "CREATE TABLE IF NOT EXISTS broker_test (key TEXT PRIMARY KEY, value TEXT)",
                    [],
                )
                .map_err(|e| e.to_string())?;
                conn.execute(
                    "INSERT INTO broker_test (key, value) VALUES ('broker_test', 'ok')",
                    [],
                )
                .map_err(|e| e.to_string())?;
                Ok(())
            })
            .unwrap();
        let value = broker
            .with_project_read("project_a", |conn| {
                conn.query_row(
                    "SELECT value FROM broker_test WHERE key = 'broker_test'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .map_err(|e| e.to_string())
            })
            .unwrap();

        assert_eq!(value, "ok");
        let _ = std::fs::remove_dir_all(&base);
    }
}
