use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use tracing::info;

use super::proxy_encoder_kind::{
    parse_forced_encoder, platform_encoder_priority, ProxyVideoEncoder,
};

static ENCODER_CACHE: OnceLock<ProxyVideoEncoder> = OnceLock::new();

/// Najbrži dostupan enkoder — iz shell profila (`data/shell.db` → `hardware.profile`).
pub fn resolve_proxy_encoder(ffmpeg: &Path) -> ProxyVideoEncoder {
    *ENCODER_CACHE.get_or_init(|| {
        if let Some(profile) = crate::hardware_profile::get() {
            if profile.proxy_encoder_verified || !profile.h264_encoders.is_empty() {
                let enc = ProxyVideoEncoder::from_label(&profile.proxy_encoder);
                info!(
                    "ingest proxy encoder: profile={} gpu={}",
                    enc.label(),
                    profile.gpu_accel
                );
                return enc;
            }
        }
        detect_proxy_encoder(ffmpeg)
    })
}

fn detect_proxy_encoder(ffmpeg: &Path) -> ProxyVideoEncoder {
    if let Ok(raw) = std::env::var("QNC_PROXY_ENCODER") {
        if let Some(enc) = parse_forced_encoder(raw.trim()) {
            info!("ingest proxy encoder: forced={}", enc.label());
            return enc;
        }
    }
    if matches!(
        std::env::var("QNC_HW_ENCODE").as_deref(),
        Ok(v) if v == "0" || v.eq_ignore_ascii_case("false")
    ) {
        info!("ingest proxy encoder: QNC_HW_ENCODE=0 → libx264");
        return ProxyVideoEncoder::Libx264;
    }
    let available = h264_encoders_from_ffmpeg(ffmpeg);
    for &enc in platform_encoder_priority() {
        if available.contains(enc.id()) {
            info!("ingest proxy encoder: auto={}", enc.label());
            return enc;
        }
    }
    info!("ingest proxy encoder: auto=libx264 (fallback)");
    ProxyVideoEncoder::Libx264
}

fn h264_encoders_from_ffmpeg(ffmpeg: &Path) -> std::collections::HashSet<String> {
    use std::collections::HashSet;
    let output = Command::new(ffmpeg)
        .args(["-hide_banner", "-encoders"])
        .output();
    let Ok(output) = output else {
        return HashSet::new();
    };
    parse_encoder_list(&String::from_utf8_lossy(&output.stdout))
}

fn parse_encoder_list(text: &str) -> std::collections::HashSet<String> {
    use std::collections::HashSet;
    let ids = [
        "h264_nvenc",
        "h264_amf",
        "h264_qsv",
        "h264_videotoolbox",
        "h264_vaapi",
        "libx264",
    ];
    let mut set = HashSet::new();
    for line in text.lines() {
        for id in ids {
            if line.contains(id) {
                set.insert(id.to_string());
            }
        }
    }
    set
}

/// GPU scale (brzo) — ostaje na uređaju; SW scale samo kao fallback.
pub fn proxy_scale_filter_hw(encoder: ProxyVideoEncoder, height: u32) -> Option<String> {
    match encoder {
        ProxyVideoEncoder::Qsv => Some(format!("scale_qsv=w=-1:h={height}")),
        ProxyVideoEncoder::Nvenc => Some(format!("scale_cuda=w=-2:h={height}")),
        ProxyVideoEncoder::Vaapi => Some(format!("scale_vaapi=w=-2:h={height}")),
        _ => None,
    }
}

/// Softverski scale — sporiji put (CPU download/upload).
pub fn proxy_scale_filter_sw(encoder: ProxyVideoEncoder, height: u32) -> String {
    let scale = format!("scale=-2:{height}");
    match encoder {
        ProxyVideoEncoder::Qsv => format!("format=nv12,{scale}"),
        ProxyVideoEncoder::Vaapi => format!("format=nv12,hwupload,{scale}"),
        _ => scale,
    }
}

#[allow(dead_code)]
pub fn proxy_scale_filter(encoder: ProxyVideoEncoder, height: u32) -> String {
    proxy_scale_filter_hw(encoder, height).unwrap_or_else(|| proxy_scale_filter_sw(encoder, height))
}

pub fn append_vaapi_hwaccel(cmd: &mut Command, encoder: ProxyVideoEncoder) {
    if encoder != ProxyVideoEncoder::Vaapi {
        return;
    }
    let device = crate::hardware_profile::get()
        .and_then(|p| p.vaapi_device.clone())
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("QNC_VAAPI_DEVICE").ok())
        .unwrap_or_else(|| "/dev/dri/renderD128".to_string());
    cmd.args(["-hwaccel", "vaapi", "-hwaccel_device", device.trim()]);
}

/// Decode hwaccel — default: download u system memory (SW filteri / poster).
/// Za puni GPU proxy put koristi `append_decode_hwaccel_mode(..., true)`.
pub fn append_decode_hwaccel(cmd: &mut Command, encoder: ProxyVideoEncoder) {
    append_decode_hwaccel_mode(cmd, encoder, false);
}

pub fn append_decode_hwaccel_mode(
    cmd: &mut Command,
    encoder: ProxyVideoEncoder,
    keep_on_gpu: bool,
) {
    match encoder {
        ProxyVideoEncoder::Qsv => {
            cmd.args(["-hwaccel", "qsv"]);
            if keep_on_gpu {
                cmd.args(["-hwaccel_output_format", "qsv"]);
            }
        }
        ProxyVideoEncoder::Nvenc => {
            cmd.args(["-hwaccel", "cuda"]);
            if keep_on_gpu {
                cmd.args(["-hwaccel_output_format", "cuda"]);
            }
        }
        ProxyVideoEncoder::Amf => {
            cmd.args(["-hwaccel", "d3d11va"]);
        }
        ProxyVideoEncoder::Vaapi => {
            append_vaapi_hwaccel(cmd, encoder);
            if keep_on_gpu {
                cmd.args(["-hwaccel_output_format", "vaapi"]);
            }
        }
        ProxyVideoEncoder::VideoToolbox => {
            cmd.args(["-hwaccel", "videotoolbox"]);
        }
        ProxyVideoEncoder::Libx264 => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_encoder_list_finds_nvenc_and_x264() {
        let text = " V..... h264_nvenc           NVIDIA NVENC H.264 encoder\n V..... libx264              libx264 H.264";
        let set = parse_encoder_list(text);
        assert!(set.contains("h264_nvenc"));
        assert!(set.contains("libx264"));
    }

    #[test]
    fn forced_encoder_env_parses() {
        assert_eq!(
            parse_forced_encoder("nvenc"),
            Some(ProxyVideoEncoder::Nvenc)
        );
        assert_eq!(parse_forced_encoder("auto"), None);
    }
}
