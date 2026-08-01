//! Decode hwaccel selection for the broadcast FFmpeg backend.
//!
//! Encode hints from the host profile (e.g. `proxy_encoder=qsv`) are remapped to
//! a decode method compatible with CPU `scale/format=rgba` preview filters.

use std::process::Command;
use std::sync::OnceLock;

/// Decode hwaccel for local ffmpeg (player runs on the client machine).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeHwaccel {
    None,
    D3d11va,
    Dxva2,
    Qsv,
    Cuda,
    Vaapi,
    VideoToolbox,
}

impl DecodeHwaccel {
    pub fn as_ffmpeg(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::D3d11va => Some("d3d11va"),
            Self::Dxva2 => Some("dxva2"),
            Self::Qsv => Some("qsv"),
            Self::Cuda => Some("cuda"),
            Self::Vaapi => Some("vaapi"),
            Self::VideoToolbox => Some("videotoolbox"),
        }
    }

    pub fn label(self) -> &'static str {
        self.as_ffmpeg().unwrap_or("software")
    }

    pub fn from_token(raw: &str) -> Option<Self> {
        let t = raw.trim().to_ascii_lowercase();
        match t.as_str() {
            "" | "none" | "off" | "software" | "sw" | "libx264" => Some(Self::None),
            "d3d11va" => Some(Self::D3d11va),
            "dxva2" => Some(Self::Dxva2),
            "qsv" | "h264_qsv" | "quicksync" => Some(Self::Qsv),
            "cuda" | "nvenc" | "h264_nvenc" | "cuvid" => Some(Self::Cuda),
            "vaapi" | "h264_vaapi" => Some(Self::Vaapi),
            "videotoolbox" | "h264_videotoolbox" => Some(Self::VideoToolbox),
            "amf" | "h264_amf" => Some(Self::D3d11va),
            _ => None,
        }
    }
}

static PLAYER_HWACCEL: OnceLock<DecodeHwaccel> = OnceLock::new();

/// Prefer host `hardware.profile`, then local `ffmpeg -hwaccels`.
/// Override: `QNC_PLAYER_HWACCEL=qsv|cuda|d3d11va|none`.
pub fn configure_player_hwaccel_from_host_profile(profile: &serde_json::Value) {
    let _ = PLAYER_HWACCEL.get_or_init(|| resolve_player_hwaccel(Some(profile)));
}

pub fn player_hwaccel() -> DecodeHwaccel {
    *PLAYER_HWACCEL.get_or_init(|| resolve_player_hwaccel(None))
}

fn resolve_player_hwaccel(host_profile: Option<&serde_json::Value>) -> DecodeHwaccel {
    if let Ok(raw) = std::env::var("QNC_PLAYER_HWACCEL") {
        if let Some(forced) = DecodeHwaccel::from_token(&raw) {
            eprintln!("qnc-app player hwaccel: env={}", forced.label());
            return forced;
        }
    }

    let available = probe_ffmpeg_hwaccels();
    if let Some(profile) = host_profile {
        if let Some(chosen) = prefer_from_host_profile(profile, &available) {
            eprintln!(
                "qnc-app player hwaccel: host_profile={} (ffmpeg ok)",
                chosen.label()
            );
            return chosen;
        }
    }

    let chosen = prefer_from_available(&available);
    eprintln!("qnc-app player hwaccel: {}", chosen.label());
    chosen
}

/// Map host *encode* profile tokens onto a *decode* hwaccel that works with
/// software `-vf scale…,format=rgba` used by the broadcast player preview.
fn decode_hwaccel_from_profile_token(raw: &str) -> Option<DecodeHwaccel> {
    let t = raw.trim().to_ascii_lowercase();
    let t = t.strip_prefix("proxy:").unwrap_or(&t);
    match t {
        "" | "none" | "off" | "software" | "sw" | "libx264" => Some(DecodeHwaccel::None),
        "qsv" | "h264_qsv" | "hevc_qsv" | "quicksync" => {
            if cfg!(target_os = "windows") {
                Some(DecodeHwaccel::D3d11va)
            } else {
                Some(DecodeHwaccel::Qsv)
            }
        }
        "amf" | "h264_amf" | "hevc_amf" => Some(DecodeHwaccel::D3d11va),
        "nvenc" | "h264_nvenc" | "hevc_nvenc" | "cuda" | "cuvid" => Some(DecodeHwaccel::Cuda),
        "d3d11va" => Some(DecodeHwaccel::D3d11va),
        "dxva2" => Some(DecodeHwaccel::Dxva2),
        "vaapi" | "h264_vaapi" => Some(DecodeHwaccel::Vaapi),
        "videotoolbox" | "h264_videotoolbox" => Some(DecodeHwaccel::VideoToolbox),
        _ => DecodeHwaccel::from_token(raw),
    }
}

fn prefer_from_host_profile(
    profile: &serde_json::Value,
    available: &[String],
) -> Option<DecodeHwaccel> {
    let mut candidates = Vec::new();
    if let Some(enc) = profile.get("proxy_encoder").and_then(|v| v.as_str()) {
        candidates.push(enc.to_string());
    }
    if let Some(hints) = profile.get("hints").and_then(|v| v.as_array()) {
        for h in hints {
            if let Some(s) = h.as_str() {
                candidates.push(s.to_string());
            }
        }
    }
    for c in candidates {
        let Some(hw) = decode_hwaccel_from_profile_token(&c) else {
            continue;
        };
        if hw == DecodeHwaccel::None {
            continue;
        }
        if let Some(name) = hw.as_ffmpeg() {
            if available.iter().any(|a| a.eq_ignore_ascii_case(name)) {
                return Some(hw);
            }
        }
    }
    None
}

fn prefer_from_available(available: &[String]) -> DecodeHwaccel {
    let priority: &[&str] = if cfg!(target_os = "windows") {
        &["d3d11va", "cuda", "qsv", "dxva2"]
    } else if cfg!(target_os = "macos") {
        &["videotoolbox"]
    } else {
        &["vaapi", "cuda", "qsv"]
    };
    for name in priority {
        if available.iter().any(|a| a.eq_ignore_ascii_case(name)) {
            if let Some(hw) = DecodeHwaccel::from_token(name) {
                return hw;
            }
        }
    }
    DecodeHwaccel::None
}

fn probe_ffmpeg_hwaccels() -> Vec<String> {
    let output = Command::new(ffmpeg_program())
        .args(["-hide_banner", "-hwaccels"])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !line.eq_ignore_ascii_case("Hardware acceleration methods:")
                && !line.starts_with("ffmpeg")
        })
        .map(|line| line.to_ascii_lowercase())
        .collect()
}

pub fn ffmpeg_program() -> String {
    std::env::var("QNC_FFMPEG")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "ffmpeg".into())
}

pub fn push_hwaccel_args(args: &mut Vec<String>, hwaccel: DecodeHwaccel) {
    if let Some(name) = hwaccel.as_ffmpeg() {
        args.push("-hwaccel".into());
        args.push(name.into());
    }
}
