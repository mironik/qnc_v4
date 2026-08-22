use std::fs;
use std::path::{Path, PathBuf};

use qnc_service_contracts::RuntimeProfile;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize)]
pub struct AppConfig {
    #[serde(default = "default_port")]
    pub api_port: u16,
    #[serde(default = "default_bind_host")]
    pub bind_host: String,
    #[serde(default = "default_server_label")]
    pub server_label: String,
    #[serde(default)]
    pub projects_root: Option<String>,
    #[serde(default)]
    pub network_presets: Vec<NetworkPreset>,
    #[serde(default)]
    pub runtime: RuntimeConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NetworkPreset {
    pub label: String,
    pub host: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RuntimeConfig {
    #[serde(default = "default_runtime_profile")]
    pub profile: RuntimeProfile,
    #[serde(default = "default_media_service_config")]
    pub media: ServiceBackendConfig,
    #[serde(default = "default_disabled_service_config")]
    pub transcription: ServiceBackendConfig,
    #[serde(default = "default_disabled_service_config")]
    pub search: ServiceBackendConfig,
    #[serde(default = "default_disabled_service_config")]
    pub ai: ServiceBackendConfig,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ServiceBackendConfig {
    #[serde(default)]
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_path: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ConfigTomlFile {
    #[serde(default)]
    profile: Option<RuntimeProfile>,
    #[serde(default)]
    runtime: Option<RuntimeTomlSection>,
    #[serde(default)]
    media: Option<ServiceBackendConfig>,
    #[serde(default)]
    transcription: Option<ServiceBackendConfig>,
    #[serde(default)]
    search: Option<ServiceBackendConfig>,
    #[serde(default)]
    ai: Option<ServiceBackendConfig>,
}

#[derive(Debug, Default, Deserialize)]
struct RuntimeTomlSection {
    #[serde(default)]
    profile: Option<RuntimeProfile>,
}

fn default_port() -> u16 {
    8001
}

fn default_bind_host() -> String {
    "127.0.0.1".into()
}

fn default_server_label() -> String {
    "QNC server".into()
}

fn default_runtime_profile() -> RuntimeProfile {
    RuntimeProfile::Light
}

fn default_media_service_config() -> ServiceBackendConfig {
    service_config_or("local_ffmpeg", None)
}

fn default_disabled_service_config() -> ServiceBackendConfig {
    service_config_or("disabled", None)
}

fn service_config_or(
    default_backend: &str,
    configured: Option<ServiceBackendConfig>,
) -> ServiceBackendConfig {
    let mut config = configured.unwrap_or_default();
    if config.backend.trim().is_empty() {
        config.backend = default_backend.to_string();
    }
    config
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            profile: default_runtime_profile(),
            media: default_media_service_config(),
            transcription: default_disabled_service_config(),
            search: default_disabled_service_config(),
            ai: default_disabled_service_config(),
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            api_port: default_port(),
            bind_host: default_bind_host(),
            server_label: default_server_label(),
            projects_root: None,
            network_presets: vec![],
            runtime: RuntimeConfig::default(),
        }
    }
}

impl AppConfig {
    pub fn load(root: &Path) -> Self {
        let path = root.join("data").join("shell_config.json");
        let mut cfg = match fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str::<AppConfig>(&raw).unwrap_or_default(),
            Err(_) => AppConfig::default(),
        };
        cfg.runtime = load_runtime_config(root);
        if let Ok(raw) = std::env::var("QNC_API_PORT") {
            if let Ok(p) = raw.parse::<u16>() {
                cfg.api_port = p;
            }
        }
        if let Ok(raw) =
            std::env::var("QNC_PROJECTS_ROOT").or_else(|_| std::env::var("QNC_PROJEKTI_ROOT"))
        {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                cfg.projects_root = Some(trimmed.to_string());
            }
        }
        cfg.bind_host = configured_bind_host();
        cfg
    }
}

pub fn load_runtime_config(root: &Path) -> RuntimeConfig {
    let path = root.join("config.toml");
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return RuntimeConfig::default();
        }
        Err(error) => {
            tracing::warn!("config.toml nije procitan: {error}");
            return RuntimeConfig::default();
        }
    };

    match parse_runtime_config_toml(&raw) {
        Ok(config) => config,
        Err(error) => {
            tracing::warn!("config.toml nije valjan: {error}");
            RuntimeConfig::default()
        }
    }
}

fn parse_runtime_config_toml(raw: &str) -> Result<RuntimeConfig, String> {
    let file: ConfigTomlFile = toml::from_str(raw).map_err(|error| error.to_string())?;
    let profile = file
        .runtime
        .as_ref()
        .and_then(|runtime| runtime.profile)
        .or(file.profile)
        .unwrap_or_else(default_runtime_profile);

    Ok(RuntimeConfig {
        profile,
        media: service_config_or("local_ffmpeg", file.media),
        transcription: service_config_or("disabled", file.transcription),
        search: service_config_or("disabled", file.search),
        ai: service_config_or("disabled", file.ai),
    })
}

/// HTTP bind address. Default `127.0.0.1`; set `QNC_BIND_HOST=0.0.0.0` for LAN
/// (requires `QNC_TRUSTED_LAN=1` — see [`require_trusted_lan_for_bind`]).
pub fn configured_bind_host() -> String {
    match std::env::var("QNC_BIND_HOST") {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return default_bind_host();
            }
            if trimmed.parse::<std::net::IpAddr>().is_ok() {
                trimmed.to_string()
            } else {
                tracing::warn!("Invalid QNC_BIND_HOST '{trimmed}', using 127.0.0.1");
                default_bind_host()
            }
        }
        Err(_) => default_bind_host(),
    }
}

fn env_flag_true(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => {
            let v = v.to_lowercase();
            v == "1" || v == "true" || v == "yes"
        }
        Err(_) => false,
    }
}

/// True when bind is loopback only (safe default without LAN flag).
pub fn bind_is_loopback(bind_host: &str) -> bool {
    match bind_host.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => bind_host == "localhost",
    }
}

/// Non-loopback bind (e.g. `0.0.0.0`) requires explicit `QNC_TRUSTED_LAN=1`.
/// Internet exposure is not supported without an external auth/proxy layer.
pub fn require_trusted_lan_for_bind(bind_host: &str) -> Result<(), String> {
    if bind_is_loopback(bind_host) {
        return Ok(());
    }
    if env_flag_true("QNC_TRUSTED_LAN") {
        return Ok(());
    }
    Err(format!(
        "QNC_BIND_HOST={bind_host} is not loopback. Set QNC_TRUSTED_LAN=1 to allow LAN bind \
         (filesystem browse/pick/delete have no auth). Internet exposure is not supported \
         without an external auth/proxy layer."
    ))
}

/// Hostname for logs and UI when bind is `0.0.0.0` / `::`.
pub fn app_url_host(bind_host: &str) -> String {
    match bind_host.parse::<std::net::IpAddr>() {
        Ok(ip) if ip.is_unspecified() => default_bind_host(),
        Ok(ip) => ip.to_string(),
        Err(_) => default_bind_host(),
    }
}

pub fn read_json(path: &Path) -> Option<Value> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn default_projects_root() -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(base) = std::env::var("LOCALAPPDATA") {
            return PathBuf::from(base).join("QNC").join("Projects");
        }
        if let Ok(base) = std::env::var("APPDATA") {
            return PathBuf::from(base).join("QNC").join("Projects");
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("QNC")
                .join("Projects");
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Ok(base) = std::env::var("XDG_DATA_HOME") {
            return PathBuf::from(base).join("qnc").join("projects");
        }
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("qnc")
                .join("projects");
        }
    }

    PathBuf::from("QNC").join("Projects")
}

pub fn configured_projects_root(config: &AppConfig) -> PathBuf {
    config
        .projects_root
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(default_projects_root)
}

pub fn save_projects_root(root: &Path, projects_root: &str) -> Result<Value, String> {
    let path = root.join("data").join("shell_config.json");
    let mut doc = fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !doc.is_object() {
        doc = serde_json::json!({});
    }
    let Some(obj) = doc.as_object_mut() else {
        return Err("Neispravan shell_config.json".into());
    };
    obj.insert(
        "projects_root".into(),
        Value::String(projects_root.trim().to_string()),
    );
    fs::create_dir_all(root.join("data")).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    fs::write(&path, text).map_err(|e| e.to_string())?;
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_bind_is_allowed_without_lan_flag() {
        assert!(require_trusted_lan_for_bind("127.0.0.1").is_ok());
        assert!(require_trusted_lan_for_bind("::1").is_ok());
        assert!(bind_is_loopback("127.0.0.1"));
        assert!(bind_is_loopback("::1"));
        assert!(!bind_is_loopback("0.0.0.0"));
        assert!(!bind_is_loopback("192.168.1.10"));
    }

    #[test]
    fn runtime_config_defaults_to_light_workstation_services() {
        let config = parse_runtime_config_toml("").unwrap();

        assert_eq!(config.profile, RuntimeProfile::Light);
        assert_eq!(config.media.backend, "local_ffmpeg");
        assert_eq!(config.transcription.backend, "disabled");
        assert_eq!(config.search.backend, "disabled");
        assert_eq!(config.ai.backend, "disabled");
    }

    #[test]
    fn runtime_config_reads_root_profile_and_backends() {
        let config = parse_runtime_config_toml(
            r#"
            profile = "local_ai"

            [media]
            backend = "local_ffmpeg"

            [transcription]
            backend = "whisper_cpp"
            model_path = "models/whisper.bin"

            [ai]
            backend = "ollama"
            model = "llama3.1"
            "#,
        )
        .unwrap();

        assert_eq!(config.profile, RuntimeProfile::LocalAi);
        assert_eq!(config.media.backend, "local_ffmpeg");
        assert_eq!(config.transcription.backend, "whisper_cpp");
        assert_eq!(
            config.transcription.model_path.as_deref(),
            Some("models/whisper.bin")
        );
        assert_eq!(config.ai.backend, "ollama");
        assert_eq!(config.ai.model.as_deref(), Some("llama3.1"));
    }

    #[test]
    fn runtime_section_profile_overrides_root_profile() {
        let config = parse_runtime_config_toml(
            r#"
            profile = "light"

            [runtime]
            profile = "enterprise"

            [media]
            backend = "remote_rest"
            endpoint = "http://qnc-media.local:9000"
            "#,
        )
        .unwrap();

        assert_eq!(config.profile, RuntimeProfile::Enterprise);
        assert_eq!(config.media.backend, "remote_rest");
        assert_eq!(
            config.media.endpoint.as_deref(),
            Some("http://qnc-media.local:9000")
        );
    }
}
