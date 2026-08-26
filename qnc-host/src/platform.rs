use std::path::Path;

use serde_json::{json, Value};

use crate::config::AppConfig;

const SHELL_API_VERSION: i32 = 1;

pub fn runtime_info(root: &Path, config: &AppConfig) -> Value {
    let deployment = config.runtime.deployment.as_str();

    json!({
        "status": "ok",
        "shell_api_version": SHELL_API_VERSION,
        "app_version": std::env::var("QNC_APP_VERSION").unwrap_or_else(|_| "0.5.1".into()),
        "deployment": deployment,
        "hardware_hints": hardware_hints(),
        "hardware_profile": crate::hardware_profile::get().map(|p| p.snapshot()),
        "platform": {
            "system": std::env::consts::OS,
            "machine": std::env::consts::ARCH,
            "family": std::env::consts::FAMILY,
        },
        "host": {
            "hostname": hostname::get()
                .ok()
                .and_then(|h| h.into_string().ok())
                .unwrap_or_else(|| "localhost".into()),
        },
        "api_port": config.api_port,
        "bind_host": config.bind_host,
        "api_url": format!(
            "http://{}:{}",
            crate::config::app_url_host(&config.bind_host),
            config.api_port
        ),
        "ui": "qnc-app",
        "projects_root": crate::config::configured_projects_root(config).to_string_lossy(),
        "runtime": &config.runtime,
        "runtime_effective": crate::services::describe_runtime(&config.runtime),
        "capabilities": capabilities(root, deployment),
        "network_presets": config.network_presets,
        "labels": {
            "server": config.server_label,
        },
    })
}

fn flag(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => {
            let v = v.to_lowercase();
            v == "1" || v == "true" || v == "yes"
        }
        Err(_) => false,
    }
}

fn capabilities(root: &Path, deployment: &str) -> Value {
    json!({
        "core": true,
        "ingest_local": true,
        "ingest_remote_client": flag("QNC_INGEST_REMOTE"),
        "ai_asr": false,
        "ai_asr_status": { "available": false, "reason": "asr module not in product path" },
        "ai_gpu_encode": flag("QNC_HW_ENCODE"),
        "proxy_encoder": crate::ingest::proxy_generate::active_proxy_encoder_label(),
        "hardware_profile": crate::hardware_profile::get().map(|p| p.snapshot()),
        "deployment": deployment,
        "design_editor": crate::design::design_editor_capability(root),
    })
}

fn hardware_hints() -> Vec<String> {
    if let Some(p) = crate::hardware_profile::get() {
        return p.hints.clone();
    }
    let mut hints = Vec::new();
    if Path::new("/etc/nv_tegra_release").exists() {
        hints.push("nvidia_tegra".into());
    }
    hints
}
