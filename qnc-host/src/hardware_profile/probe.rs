use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use cpal::traits::{DeviceTrait, HostTrait};

use crate::ingest::proxy_encoder_kind::{
    parse_forced_encoder, platform_encoder_priority, ProxyVideoEncoder,
};
use crate::ingest::thumb::{ffmpeg_available, ffprobe_available, resolve_ffmpeg};

use super::{
    AudioOutputProfile, HardwareFingerprint, HardwareProfile, MediaDecodeProfile, SCHEMA_VERSION,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProxyEncoderChoice {
    pub encoder: ProxyVideoEncoder,
    pub verified: bool,
}

pub fn current_fingerprint() -> HardwareFingerprint {
    let ffmpeg_path = resolve_ffmpeg()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    HardwareFingerprint {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        ffmpeg_path: ffmpeg_path.clone(),
        ffmpeg_version: ffmpeg_version_line(&ffmpeg_path),
    }
}

pub fn run_probe() -> HardwareProfile {
    let fingerprint = current_fingerprint();
    let ffmpeg_ok = ffmpeg_available();
    let ffprobe_ok = ffprobe_available();
    let h264_encoders = if ffmpeg_ok {
        list_h264_encoders()
    } else {
        Vec::new()
    };
    let mut warnings = Vec::new();
    if !ffmpeg_ok {
        warnings.push("ffmpeg nije dostupan — ingest proxy/filmstrip neće raditi".into());
    }
    if !ffprobe_ok {
        warnings.push("ffprobe nije dostupan — trajanje/metadata ograničeni".into());
    }

    let choice = pick_proxy_encoder(&h264_encoders);
    let media_decode = probe_media_decode(ffmpeg_ok);
    let audio_output = probe_audio_output();
    let gpu_accel = choice.encoder.uses_gpu() && choice.verified;
    let mut hints = build_hints(&h264_encoders, choice.encoder, choice.verified);
    if media_decode.recommended_backend != "software" {
        hints.push(format!("decode:{}", media_decode.recommended_backend));
    }
    if audio_output.available {
        hints.push("audio:output".into());
    }
    hints.sort();
    hints.dedup();
    let vaapi_device = if choice.encoder == ProxyVideoEncoder::Vaapi {
        Some(detect_vaapi_device())
    } else {
        None
    };
    let cpu_logical_cores = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1)
        .max(1);
    let recommended_proxy_parallel = if gpu_accel && choice.verified {
        match choice.encoder {
            ProxyVideoEncoder::Nvenc => (cpu_logical_cores / 4).clamp(2, 3),
            ProxyVideoEncoder::Qsv | ProxyVideoEncoder::Amf => 2,
            ProxyVideoEncoder::Vaapi | ProxyVideoEncoder::VideoToolbox => 2,
            ProxyVideoEncoder::Libx264 => 1,
        }
    } else {
        1
    };
    let ingest_stable = ffmpeg_ok && (!h264_encoders.is_empty()) && choice.verified;
    let media_runtime_stable = ffmpeg_ok && ffprobe_ok && audio_output.available;

    if !choice.verified && ffmpeg_ok {
        warnings.push(format!(
            "proxy enkoder {} nije prošao smoke test — koristi se fallback",
            choice.encoder.label()
        ));
    }
    warnings.extend(media_decode.warnings.iter().cloned());
    warnings.extend(audio_output.warnings.iter().cloned());

    HardwareProfile {
        schema_version: SCHEMA_VERSION,
        probed_at: now_rfc3339(),
        fingerprint,
        ffmpeg_available: ffmpeg_ok,
        ffprobe_available: ffprobe_ok,
        h264_encoders,
        proxy_encoder: choice.encoder.label().to_string(),
        proxy_encoder_verified: choice.verified,
        gpu_accel,
        hints,
        vaapi_device,
        cpu_logical_cores,
        recommended_proxy_parallel,
        ingest_stable,
        media_decode,
        audio_output,
        media_runtime_stable,
        warnings,
    }
}

fn probe_media_decode(ffmpeg_ok: bool) -> MediaDecodeProfile {
    let mut warnings = Vec::new();
    let available_backends = if ffmpeg_ok {
        list_hardware_decode_backends()
    } else {
        Vec::new()
    };
    let forced_backend = forced_decode_backend();
    let (recommended_backend, selection_reason) = select_decode_backend(
        &available_backends,
        forced_backend.as_deref(),
        &mut warnings,
    );
    if ffmpeg_ok && available_backends.is_empty() {
        warnings.push("ffmpeg ne prijavljuje hardware decode backend; koristi se software".into());
    }

    MediaDecodeProfile {
        available_backends,
        recommended_backend,
        forced_backend,
        probe_method: "ffmpeg -hide_banner -hwaccels".into(),
        verified: false,
        selection_reason,
        warnings,
    }
}

fn probe_audio_output() -> AudioOutputProfile {
    let host = cpal::default_host();
    let mut profile = AudioOutputProfile {
        host: Some(std::env::consts::OS.to_string()),
        probe_method: "cpal default_output_device".into(),
        ..AudioOutputProfile::default()
    };

    let Some(device) = host.default_output_device() else {
        profile
            .warnings
            .push("audio output device nije dostupan".into());
        return profile;
    };

    profile.available = true;
    profile.default_device = device.name().ok();
    match device.default_output_config() {
        Ok(config) => {
            profile.default_config = Some(format!(
                "{} ch {} Hz {:?}",
                config.channels(),
                config.sample_rate().0,
                config.sample_format()
            ));
        }
        Err(err) => {
            profile
                .warnings
                .push(format!("audio output config nije dostupan: {err}"));
        }
    }
    profile
}

fn forced_decode_backend() -> Option<String> {
    std::env::var("QNC_PLAYER_HWACCEL")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
}

fn select_decode_backend(
    available: &[String],
    forced: Option<&str>,
    warnings: &mut Vec<String>,
) -> (String, String) {
    if let Some(raw) = forced {
        if matches!(raw, "none" | "software" | "off" | "sw") {
            return (
                "software".into(),
                "QNC_PLAYER_HWACCEL software override".into(),
            );
        }
        if raw != "auto" {
            if contains_backend(available, raw) {
                return (
                    raw.to_string(),
                    "QNC_PLAYER_HWACCEL backend override".into(),
                );
            }
            warnings.push(format!(
                "QNC_PLAYER_HWACCEL={raw} nije prijavljen u ffmpeg -hwaccels; koristi se software"
            ));
            return ("software".into(), "forced backend unavailable".into());
        }
    }

    for backend in decode_backend_priority() {
        if contains_backend(available, backend) {
            let reason = if forced == Some("auto") {
                "QNC_PLAYER_HWACCEL auto platform priority"
            } else {
                "platform priority"
            };
            return ((*backend).into(), reason.into());
        }
    }

    ("software".into(), "software fallback".into())
}

fn contains_backend(available: &[String], backend: &str) -> bool {
    available
        .iter()
        .any(|item| item.eq_ignore_ascii_case(backend))
}

fn decode_backend_priority() -> &'static [&'static str] {
    if cfg!(target_os = "windows") {
        &["d3d11va", "dxva2", "d3d12va", "qsv", "cuda", "vulkan"]
    } else if cfg!(target_os = "macos") {
        &["videotoolbox"]
    } else if cfg!(target_os = "linux") {
        &["vaapi", "cuda", "qsv", "vulkan"]
    } else {
        &["vaapi", "cuda", "qsv", "videotoolbox", "d3d11va", "dxva2"]
    }
}

fn pick_proxy_encoder(listed: &[String]) -> ProxyEncoderChoice {
    if let Ok(raw) = std::env::var("QNC_PROXY_ENCODER") {
        if let Some(enc) = parse_forced_encoder(raw.trim()) {
            let verified = smoke_test_encoder(enc);
            return ProxyEncoderChoice {
                encoder: enc,
                verified,
            };
        }
    }
    if matches!(
        std::env::var("QNC_HW_ENCODE").as_deref(),
        Ok(v) if v == "0" || v.eq_ignore_ascii_case("false")
    ) {
        let enc = ProxyVideoEncoder::Libx264;
        return ProxyEncoderChoice {
            encoder: enc,
            verified: smoke_test_encoder(enc),
        };
    }

    let set: HashSet<&str> = listed.iter().map(|s| s.as_str()).collect();
    for &enc in platform_encoder_priority() {
        if set.contains(enc.id()) && smoke_test_encoder(enc) {
            return ProxyEncoderChoice {
                encoder: enc,
                verified: true,
            };
        }
    }
    if set.contains("libx264") {
        let enc = ProxyVideoEncoder::Libx264;
        return ProxyEncoderChoice {
            encoder: enc,
            verified: smoke_test_encoder(enc),
        };
    }
    ProxyEncoderChoice {
        encoder: ProxyVideoEncoder::Libx264,
        verified: false,
    }
}

fn build_hints(listed: &[String], chosen: ProxyVideoEncoder, verified: bool) -> Vec<String> {
    let mut hints = Vec::new();
    for id in listed {
        if id.contains("nvenc") {
            hints.push("nvenc".into());
        } else if id.contains("amf") {
            hints.push("amf".into());
        } else if id.contains("qsv") {
            hints.push("qsv".into());
        } else if id.contains("videotoolbox") {
            hints.push("videotoolbox".into());
        } else if id.contains("vaapi") {
            hints.push("vaapi".into());
        }
    }
    hints.sort();
    hints.dedup();
    if verified && chosen.uses_gpu() {
        hints.push(format!("proxy:{}", chosen.label()));
    }
    if Path::new("/etc/nv_tegra_release").exists() {
        hints.push("nvidia_tegra".into());
    }
    hints
}

fn list_h264_encoders() -> Vec<String> {
    let Some(ffmpeg) = resolve_ffmpeg() else {
        return Vec::new();
    };
    let output = Command::new(&ffmpeg)
        .args(["-hide_banner", "-encoders"])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    parse_encoder_list(&String::from_utf8_lossy(&output.stdout))
}

fn list_hardware_decode_backends() -> Vec<String> {
    let Some(ffmpeg) = resolve_ffmpeg() else {
        return Vec::new();
    };
    let output = Command::new(&ffmpeg)
        .args(["-hide_banner", "-hwaccels"])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_hwaccel_list(&String::from_utf8_lossy(&output.stdout))
}

fn parse_hwaccel_list(text: &str) -> Vec<String> {
    let mut out: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.ends_with(':'))
        .map(str::to_ascii_lowercase)
        .collect();
    out.sort();
    out.dedup();
    out
}

fn parse_encoder_list(text: &str) -> Vec<String> {
    let ids = [
        "h264_nvenc",
        "h264_amf",
        "h264_qsv",
        "h264_videotoolbox",
        "h264_vaapi",
        "libx264",
    ];
    let mut found = HashSet::new();
    for line in text.lines() {
        for id in ids {
            if line.contains(id) {
                found.insert(id.to_string());
            }
        }
    }
    let mut out: Vec<String> = found.into_iter().collect();
    out.sort();
    out
}

/// Kratki null-mux smoke test (~50 ms video).
fn smoke_test_encoder(encoder: ProxyVideoEncoder) -> bool {
    let Some(ffmpeg) = resolve_ffmpeg() else {
        return false;
    };
    let mut cmd = Command::new(&ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    if encoder == ProxyVideoEncoder::Vaapi {
        let device = detect_vaapi_device();
        cmd.args(["-hwaccel", "vaapi", "-hwaccel_device", device.trim()]);
    }
    cmd.args([
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=64x64:d=0.05:r=25",
        "-frames:v",
        "1",
    ]);
    append_smoke_encode_args(&mut cmd, encoder);
    cmd.args(["-an", "-f", "null", "-"]);
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

fn append_smoke_encode_args(cmd: &mut Command, encoder: ProxyVideoEncoder) {
    match encoder {
        ProxyVideoEncoder::Nvenc => {
            cmd.args(["-c:v", "h264_nvenc", "-preset", "p1", "-pix_fmt", "yuv420p"]);
        }
        ProxyVideoEncoder::Amf => {
            cmd.args([
                "-c:v", "h264_amf", "-quality", "speed", "-pix_fmt", "yuv420p",
            ]);
        }
        ProxyVideoEncoder::Qsv => {
            cmd.args([
                "-vf",
                "format=nv12",
                "-c:v",
                "h264_qsv",
                "-preset",
                "veryfast",
            ]);
        }
        ProxyVideoEncoder::VideoToolbox => {
            cmd.args(["-c:v", "h264_videotoolbox", "-b:v", "500k"]);
        }
        ProxyVideoEncoder::Vaapi => {
            cmd.args([
                "-vf",
                "format=nv12,hwupload",
                "-c:v",
                "h264_vaapi",
                "-qp",
                "26",
            ]);
        }
        ProxyVideoEncoder::Libx264 => {
            cmd.args([
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
            ]);
        }
    }
}

fn ffmpeg_version_line(ffmpeg_path: &str) -> String {
    if ffmpeg_path.is_empty() {
        return String::new();
    }
    Command::new(ffmpeg_path)
        .arg("-version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.lines().next().map(|l| l.trim().to_string()))
        .unwrap_or_default()
}

fn detect_vaapi_device() -> String {
    std::env::var("QNC_VAAPI_DEVICE").unwrap_or_else(|_| {
        if Path::new("/dev/dri/renderD128").exists() {
            "/dev/dri/renderD128".into()
        } else {
            String::new()
        }
    })
}

fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_encoder_list_sorted() {
        let text = " V..... h264_nvenc\n V..... libx264";
        let v = parse_encoder_list(text);
        assert!(v.contains(&"h264_nvenc".to_string()));
        assert!(v.contains(&"libx264".to_string()));
    }

    #[test]
    fn parse_hwaccel_list_skips_header_and_sorts() {
        let v = parse_hwaccel_list("Hardware acceleration methods:\r\ncuda\r\nd3d11va\r\n");
        assert_eq!(v, vec!["cuda".to_string(), "d3d11va".to_string()]);
    }

    #[test]
    fn select_decode_backend_respects_forced_software() {
        let mut warnings = Vec::new();
        let (backend, reason) =
            select_decode_backend(&["d3d11va".into()], Some("software"), &mut warnings);
        assert_eq!(backend, "software");
        assert!(reason.contains("override"));
        assert!(warnings.is_empty());
    }

    #[test]
    fn select_decode_backend_rejects_unlisted_forced_backend() {
        let mut warnings = Vec::new();
        let (backend, reason) =
            select_decode_backend(&["d3d11va".into()], Some("qsv"), &mut warnings);
        assert_eq!(backend, "software");
        assert_eq!(reason, "forced backend unavailable");
        assert_eq!(warnings.len(), 1);
    }
}
