//! Profil hardvera — **jednom** pri prvom pokretanju shella na ovom računalu.
//! Spremljeno u `data/shell.db` (`shell_settings`), ne u project bazi.
//! Sljedeća pokretanja samo učitavaju profil ako postoji; ponovni probe samo uz `QNC_FORCE_HW_PROBE=1`.

mod probe;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::shell_store;

pub const SCHEMA_VERSION: u32 = 2;
const SETTING_KEY: &str = "hardware.profile";
const LEGACY_JSON_FILE: &str = "hardware_profile.json";
const LEGACY_PROJECT_STORE_KEY: &str = "hardware.profile";

static PROFILE: OnceLock<HardwareProfile> = OnceLock::new();

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HardwareFingerprint {
    pub os: String,
    pub arch: String,
    pub ffmpeg_path: String,
    pub ffmpeg_version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HardwareProfile {
    pub schema_version: u32,
    pub probed_at: String,
    pub fingerprint: HardwareFingerprint,
    pub ffmpeg_available: bool,
    pub ffprobe_available: bool,
    /// Svi H.264 enkoderi koje ffmpeg prijavljuje.
    pub h264_encoders: Vec<String>,
    /// Odabrani enkoder za terenski proxy (npr. nvenc, libx264).
    pub proxy_encoder: String,
    /// Smoke test odabranog enkodera prošao.
    pub proxy_encoder_verified: bool,
    pub gpu_accel: bool,
    pub hints: Vec<String>,
    #[serde(default)]
    pub vaapi_device: Option<String>,
    pub cpu_logical_cores: u32,
    /// Preporuka za paralelne proxy jobove (1 = GPU ili konzervativno).
    pub recommended_proxy_parallel: u32,
    /// Minimalni uvjeti za ingest (ffmpeg + barem jedan H.264 enkoder).
    pub ingest_stable: bool,
    #[serde(default)]
    pub media_decode: MediaDecodeProfile,
    #[serde(default)]
    pub audio_output: AudioOutputProfile,
    #[serde(default)]
    pub media_runtime_stable: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MediaDecodeProfile {
    pub available_backends: Vec<String>,
    pub recommended_backend: String,
    #[serde(default)]
    pub forced_backend: Option<String>,
    pub probe_method: String,
    pub verified: bool,
    pub selection_reason: String,
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl Default for MediaDecodeProfile {
    fn default() -> Self {
        Self {
            available_backends: Vec::new(),
            recommended_backend: "software".into(),
            forced_backend: None,
            probe_method: "not_probed".into(),
            verified: false,
            selection_reason: "software fallback".into(),
            warnings: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AudioOutputProfile {
    pub available: bool,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub default_device: Option<String>,
    #[serde(default)]
    pub default_config: Option<String>,
    pub probe_method: String,
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl Default for AudioOutputProfile {
    fn default() -> Self {
        Self {
            available: false,
            host: None,
            default_device: None,
            default_config: None,
            probe_method: "not_probed".into(),
            warnings: Vec::new(),
        }
    }
}

impl HardwareProfile {
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}))
    }
}

fn legacy_json_path(root: &Path) -> PathBuf {
    root.join("data").join(LEGACY_JSON_FILE)
}

fn force_reprobe() -> bool {
    matches!(
        std::env::var("QNC_FORCE_HW_PROBE").as_deref(),
        Ok(v) if v == "1" || v.eq_ignore_ascii_case("true")
    )
}

/// Učitaj spremljeni profil ili pokreni probe samo ako profil još ne postoji.
pub fn ensure(root: &Path) -> &'static HardwareProfile {
    PROFILE.get_or_init(|| load_or_probe(root))
}

pub fn get() -> Option<&'static HardwareProfile> {
    PROFILE.get()
}

pub fn recommended_proxy_parallel() -> u32 {
    // Prefer probed value; cap so Shell stays responsive, but allow 2–3 for GPU.
    let n = get()
        .map(|p| p.recommended_proxy_parallel)
        .unwrap_or(2)
        .max(1);
    n.min(3)
}

pub fn proxy_encoder_label() -> String {
    get()
        .map(|p| p.proxy_encoder.clone())
        .unwrap_or_else(|| "libx264".into())
}

fn load_or_probe(root: &Path) -> HardwareProfile {
    let data_dir = root.join("data");
    let conn = match shell_store::open(&data_dir) {
        Ok(c) => c,
        Err(e) => {
            warn!("hardware profile: shell.db nedostupna ({e}), probe bez spremanja");
            return probe::run_probe();
        }
    };

    if !force_reprobe() {
        if let Some(cached) = load_from_shell_db(&conn) {
            info!(
                "hardware profile: učitano iz shell.db proxy_encoder={} gpu={} stable={}",
                cached.proxy_encoder, cached.gpu_accel, cached.ingest_stable
            );
            return cached;
        }
        if let Some(cached) = migrate_legacy(root, &conn) {
            return cached;
        }
    } else {
        info!("hardware profile: QNC_FORCE_HW_PROBE=1 — ponovni probe");
    }

    info!("hardware profile: prvi pokretaj shella — testiranje hardvera…");
    let profile = probe::run_probe();
    if let Err(e) = save_to_shell_db(&conn, &profile) {
        warn!("hardware profile: spremanje u shell.db nije uspjelo: {e}");
    } else {
        info!(
            "hardware profile: spremljeno u shell.db proxy_encoder={} verified={} gpu={} stable={}",
            profile.proxy_encoder,
            profile.proxy_encoder_verified,
            profile.gpu_accel,
            profile.ingest_stable
        );
    }
    profile
}

fn migrate_legacy(root: &Path, conn: &Connection) -> Option<HardwareProfile> {
    if let Some(cached) = load_legacy_json(root) {
        info!("hardware profile: migrirano iz legacy JSON u shell.db");
        if let Err(e) = save_to_shell_db(conn, &cached) {
            warn!("hardware profile: migracija u shell.db nije uspjela: {e}");
        }
        return Some(cached);
    }
    if let Some(cached) = load_from_project_store_legacy(root) {
        info!("hardware profile: migrirano iz project_store.db u shell.db");
        if let Err(e) = save_to_shell_db(conn, &cached) {
            warn!("hardware profile: migracija u shell.db nije uspjela: {e}");
        }
        return Some(cached);
    }
    None
}

fn load_from_shell_db(conn: &Connection) -> Option<HardwareProfile> {
    let raw = shell_store::get_setting(conn, SETTING_KEY).ok()??;
    if raw.trim().is_empty() {
        return None;
    }
    let profile: HardwareProfile = serde_json::from_str(&raw).ok()?;
    current_schema(profile, "shell.db")
}

fn load_from_project_store_legacy(root: &Path) -> Option<HardwareProfile> {
    let db_path = root.join("data").join("project_store.db");
    let conn = Connection::open(&db_path).ok()?;
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            rusqlite::params![LEGACY_PROJECT_STORE_KEY],
            |r| r.get(0),
        )
        .ok();
    let raw = raw?;
    let profile: HardwareProfile = serde_json::from_str(&raw).ok()?;
    current_schema(profile, "project_store.db")
}

fn save_to_shell_db(conn: &Connection, profile: &HardwareProfile) -> Result<(), String> {
    let raw = serde_json::to_string(profile).map_err(|e| e.to_string())?;
    shell_store::set_setting(conn, SETTING_KEY, &raw).map_err(|e| e.to_string())
}

fn load_legacy_json(root: &Path) -> Option<HardwareProfile> {
    let path = legacy_json_path(root);
    let raw = std::fs::read_to_string(&path).ok()?;
    let profile: HardwareProfile = serde_json::from_str(&raw).ok()?;
    current_schema(profile, "legacy JSON")
}

fn current_schema(profile: HardwareProfile, source: &str) -> Option<HardwareProfile> {
    if profile.schema_version == SCHEMA_VERSION {
        return Some(profile);
    }
    info!(
        "hardware profile: {} schema {} zastarjela, ponovni probe",
        source, profile.schema_version
    );
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_roundtrip_json() {
        let p = HardwareProfile {
            schema_version: SCHEMA_VERSION,
            probed_at: "2026-01-01T00:00:00Z".into(),
            fingerprint: HardwareFingerprint {
                os: "windows".into(),
                arch: "x86_64".into(),
                ffmpeg_path: "/ffmpeg".into(),
                ffmpeg_version: "ffmpeg version 6.0".into(),
            },
            ffmpeg_available: true,
            ffprobe_available: true,
            h264_encoders: vec!["h264_nvenc".into(), "libx264".into()],
            proxy_encoder: "nvenc".into(),
            proxy_encoder_verified: true,
            gpu_accel: true,
            hints: vec!["nvenc".into()],
            vaapi_device: None,
            cpu_logical_cores: 8,
            recommended_proxy_parallel: 1,
            ingest_stable: true,
            media_decode: MediaDecodeProfile {
                available_backends: vec!["d3d11va".into()],
                recommended_backend: "d3d11va".into(),
                forced_backend: None,
                probe_method: "ffmpeg -hwaccels".into(),
                verified: false,
                selection_reason: "platform priority".into(),
                warnings: vec![],
            },
            audio_output: AudioOutputProfile {
                available: true,
                host: Some("windows".into()),
                default_device: Some("default".into()),
                default_config: Some("2 ch 48000 Hz F32".into()),
                probe_method: "cpal default_output_device".into(),
                warnings: vec![],
            },
            media_runtime_stable: true,
            warnings: vec![],
        };
        let raw = serde_json::to_string(&p).unwrap();
        let back: HardwareProfile = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.proxy_encoder, "nvenc");
        assert_eq!(back.media_decode.recommended_backend, "d3d11va");
        assert!(back.audio_output.available);
    }
}
