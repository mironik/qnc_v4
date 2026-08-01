use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use tracing::{info, warn};

use crate::frame_time::{rational_fps, seconds_to_frame};
use crate::ingest::thumb::{probe_media, resolve_ffmpeg, MediaProbe};

use super::proxy_encode::{
    active_encoder_from_profile, append_decode_hwaccel_mode, append_proxy_video_encode_args,
    resolve_proxy_encoder,
};
use super::proxy_encoder_kind::ProxyVideoEncoder;
use super::proxy_source::{classify_tv_source, recipe_for_source, ProxyCodec, ProxyRecipe};

fn safe_clip_stem(clip_id: &str) -> String {
    let safe: String = clip_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe.is_empty() {
        "clip".into()
    } else {
        safe
    }
}

/// Legacy helper — H.264 putanja. Preferiraj `proxy_path_for_recipe`.
#[allow(dead_code)]
pub fn proxy_mp4_path(proxy_dir: &Path, clip_id: &str) -> PathBuf {
    proxy_dir.join(format!("{}.mp4", safe_clip_stem(clip_id)))
}

pub fn proxy_path_for_recipe(proxy_dir: &Path, clip_id: &str, recipe: ProxyRecipe) -> PathBuf {
    proxy_dir.join(format!(
        "{}.{}",
        safe_clip_stem(clip_id),
        recipe.extension()
    ))
}

/// Odredi destinaciju prema tipu izvora (PAL/NTSC × p/i).
pub fn proxy_dest_for_source(
    proxy_dir: &Path,
    clip_id: &str,
    source: &Path,
) -> Result<PathBuf, String> {
    let probe = probe_media(source)
        .ok_or_else(|| format!("ffprobe ne može pročitati izvor: {}", source.display()))?;
    let class = classify_tv_source(&probe);
    let recipe = recipe_for_source(class);
    Ok(proxy_path_for_recipe(proxy_dir, clip_id, recipe))
}

/// Label aktivnog proxy enkodera (diagnostics / health).
pub fn active_proxy_encoder_label() -> Option<String> {
    let ffmpeg = resolve_ffmpeg()?;
    Some(active_encoder_from_profile(&ffmpeg).label().to_string())
}

/// Sažetak recepta po tipu izvora — za health / dijagnostiku.
pub fn proxy_recipe_policy_snapshot() -> serde_json::Value {
    use super::proxy_source::TvSourceClass;
    let classes = [
        TvSourceClass::Pal50p,
        TvSourceClass::Pal50i,
        TvSourceClass::Pal25p,
        TvSourceClass::Ntsc60p,
        TvSourceClass::Ntsc60i,
        TvSourceClass::Ntsc30p,
    ];
    let mut map = serde_json::Map::new();
    for class in classes {
        let recipe = recipe_for_source(class);
        map.insert(
            class.label().into(),
            serde_json::json!({
                "region": class.region_label(),
                "recipe": recipe.id(),
                "ext": recipe.extension(),
            }),
        );
    }
    serde_json::Value::Object(map)
}

/// Terenski proxy prema tipu izvora (50p/50i/25p + NTSC ekvivalenti).
pub fn generate_field_proxy(source: &Path, dest: &Path) -> Result<(), String> {
    if !source.is_file() {
        return Err(format!("izvor ne postoji: {}", source.display()));
    }
    if crate::media::is_audio_media_file(source) {
        return Err(format!(
            "audio-only se ne enkodira kao video proxy (copy umjesto generate): {}",
            source.display()
        ));
    }
    let probe = probe_media(source)
        .ok_or_else(|| format!("ffprobe ne može pročitati izvor: {}", source.display()))?;
    let class = classify_tv_source(&probe);
    let recipe = recipe_for_source(class);
    info!(
        "ingest proxy generate: source={} region={} class={} recipe={} interlaced={} fps={:.3}",
        source.display(),
        class.region_label(),
        class.label(),
        recipe.id(),
        probe.interlaced,
        probe.fps
    );

    // Već postoji proxy — ne encodeaj ponovo ako prolazi parity.
    if dest.is_file() && dest.metadata().map(|m| m.len()).unwrap_or(0) > 0 {
        match verify_frame_parity(dest, &probe) {
            Ok(()) => {
                info!("ingest proxy generate: skip existing {}", dest.display());
                return Ok(());
            }
            Err(err) => {
                warn!(
                    "ingest proxy existing failed parity ({err}) — regenerating {}",
                    dest.display()
                );
                let _ = std::fs::remove_file(dest);
            }
        }
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let ffmpeg = resolve_ffmpeg().ok_or_else(|| "ffmpeg nije dostupan".to_string())?;

    let started = Instant::now();
    match recipe.codec {
        ProxyCodec::XdcamHd422 => {
            run_xdcam_hd422_encode(&ffmpeg, source, dest, &probe, recipe)?;
        }
        ProxyCodec::H264 => {
            run_h264_encode(&ffmpeg, source, dest, &probe, recipe)?;
        }
    }
    verify_frame_parity(dest, &probe)?;
    let secs = started.elapsed().as_secs_f64();
    let dur = probe.duration_sec.max(0.001);
    info!(
        "ingest proxy generate: done {} in {secs:.1}s ({:.1}x realtime) recipe={}",
        dest.display(),
        dur / secs,
        recipe.id()
    );
    Ok(())
}

fn run_h264_encode(
    ffmpeg: &Path,
    source: &Path,
    dest: &Path,
    probe: &MediaProbe,
    recipe: ProxyRecipe,
) -> Result<(), String> {
    let encoder = resolve_proxy_encoder(ffmpeg);
    // Native raster: prefer GPU when available.
    if encoder == ProxyVideoEncoder::Libx264 {
        return run_h264_libx264(ffmpeg, source, dest, probe, recipe);
    }
    match run_h264_hw(ffmpeg, source, dest, probe, encoder, recipe) {
        Ok(()) => Ok(()),
        Err(err) => {
            warn!(
                "ingest proxy hw h264 ({}) failed: {err} — fallback libx264",
                encoder.label()
            );
            let _ = std::fs::remove_file(dest);
            run_h264_libx264(ffmpeg, source, dest, probe, recipe)
        }
    }
}

fn run_h264_libx264(
    ffmpeg: &Path,
    source: &Path,
    dest: &Path,
    probe: &MediaProbe,
    _recipe: ProxyRecipe,
) -> Result<(), String> {
    // No forced scale — keep ffprobe source width×height.
    run_encode_inner(
        ffmpeg,
        source,
        dest,
        probe,
        EncodeAccel::Software,
        ProxyVideoEncoder::Libx264,
        "",
        AudioMode::Aac,
        None,
    )
}

fn run_h264_hw(
    ffmpeg: &Path,
    source: &Path,
    dest: &Path,
    probe: &MediaProbe,
    encoder: ProxyVideoEncoder,
    _recipe: ProxyRecipe,
) -> Result<(), String> {
    run_encode_inner(
        ffmpeg,
        source,
        dest,
        probe,
        EncodeAccel::GpuKeep,
        encoder,
        "",
        AudioMode::Aac,
        None,
    )
}

fn run_xdcam_hd422_encode(
    ffmpeg: &Path,
    source: &Path,
    dest: &Path,
    probe: &MediaProbe,
    recipe: ProxyRecipe,
) -> Result<(), String> {
    // XDCAM HD422: MPEG-2 4:2:2 @ 50 Mbit CBR, MXF OP1a, PCM 48 kHz
    // (usklađeno s template `xdcam_hd_422` / `mpeg2_422_50mbit`).
    let (fps_num, fps_den) = rational_fps(probe.fps);
    let fps_arg = if fps_den == 1 {
        fps_num.to_string()
    } else {
        format!("{fps_num}/{fps_den}")
    };
    // GOP: 12 za 25/50 (PAL), 15 za ≈30/60 (NTSC).
    let gop = if probe.fps < 28.0 { "12" } else { "15" };

    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-y", "-hide_banner", "-nostdin", "-v", "error"]);
    cmd.args(["-i"]).arg(source);
    cmd.args(["-map", "0:v:0", "-map", "0:a:0?"]);

    // XDCAM HD422 profil = 1920×1080; scale samo ako ffprobe kaže da izvor nije taj raster.
    let needs_xdcam_raster = !probe
        .resolution
        .to_ascii_lowercase()
        .starts_with("1920x1080");
    if needs_xdcam_raster {
        cmd.args([
            "-vf",
            "scale=1920:1080:force_original_aspect_ratio=decrease,pad=1920:1080:(ow-iw)/2:(oh-ih)/2",
        ]);
    }

    cmd.args([
        "-c:v",
        "mpeg2video",
        "-pix_fmt",
        "yuv422p",
        "-b:v",
        "50M",
        "-minrate",
        "50M",
        "-maxrate",
        "50M",
        "-bufsize",
        "50M",
        "-g",
        gop,
        "-bf",
        "2",
        "-mpv_flags",
        "+strict_gop",
        "-intra_vlc",
        "1",
        "-non_linear_quant",
        "1",
        "-qmin",
        "1",
        "-qmax",
        "12",
        "-dc",
        "10",
    ]);
    if recipe.keep_interlace {
        cmd.args(["-flags", "+ildct+ilme", "-top", "1"]);
    }
    cmd.args(["-r", &fps_arg, "-fps_mode", "cfr"]);
    cmd.args(["-c:a", "pcm_s16le", "-ar", "48000", "-ac", "2"]);
    cmd.args(["-f", "mxf"]);
    cmd.arg(dest);

    let result = cmd
        .output()
        .map_err(|e| format!("ffmpeg xdcam proxy: {e}"))?;
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stderr).trim().to_string());
    }
    if !dest.is_file() {
        return Err("proxy datoteka nije kreirana".into());
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum EncodeAccel {
    GpuKeep,
    #[allow(dead_code)]
    GpuDownload,
    Software,
}

#[derive(Clone, Copy)]
enum AudioMode {
    Aac,
}

fn run_encode_inner(
    ffmpeg: &Path,
    source: &Path,
    dest: &Path,
    probe: &MediaProbe,
    accel: EncodeAccel,
    encoder: ProxyVideoEncoder,
    scale_filter: &str,
    _audio: AudioMode,
    _extra: Option<&[&str]>,
) -> Result<(), String> {
    let (fps_num, fps_den) = rational_fps(probe.fps);
    let fps_arg = if fps_den == 1 {
        fps_num.to_string()
    } else {
        format!("{fps_num}/{fps_den}")
    };
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-y", "-hide_banner", "-nostdin", "-v", "error"]);
    match accel {
        EncodeAccel::GpuKeep => append_decode_hwaccel_mode(&mut cmd, encoder, true),
        EncodeAccel::GpuDownload => {
            // ffmpeg 8 + QSV: eksplicitno nv12 u system memory za SW scale.
            match encoder {
                ProxyVideoEncoder::Qsv => {
                    cmd.args(["-hwaccel", "qsv", "-hwaccel_output_format", "nv12"]);
                }
                ProxyVideoEncoder::Nvenc => {
                    cmd.args(["-hwaccel", "cuda", "-hwaccel_output_format", "nv12"]);
                }
                _ => append_decode_hwaccel_mode(&mut cmd, encoder, false),
            }
        }
        EncodeAccel::Software => {}
    }
    cmd.args(["-i"]).arg(source);
    cmd.args(["-map", "0:v:0", "-map", "0:a:0?"]);
    append_proxy_video_encode_args(&mut cmd, encoder, scale_filter, &fps_arg);
    cmd.args(["-c:a", "aac", "-b:a", "96k", "-ac", "2"]);
    cmd.arg(dest);
    let result = cmd.output().map_err(|e| format!("ffmpeg proxy: {e}"))?;
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stderr).trim().to_string());
    }
    if !dest.is_file() {
        return Err("proxy datoteka nije kreirana".into());
    }
    Ok(())
}

fn verify_frame_parity(dest: &Path, source_probe: &MediaProbe) -> Result<(), String> {
    let dest_probe = probe_media(dest)
        .ok_or_else(|| format!("ffprobe ne može pročitati proxy: {}", dest.display()))?;
    if (source_probe.fps - dest_probe.fps).abs() > 0.08 {
        return Err(format!(
            "proxy fps {} != izvor {} — timecode bi se razmaknuo",
            dest_probe.fps, source_probe.fps
        ));
    }
    let src_frames = seconds_to_frame(source_probe.duration_sec, source_probe.fps);
    let dst_frames = seconds_to_frame(dest_probe.duration_sec, dest_probe.fps);
    // MPEG-2 mux ponekad ±2 framea; H.264 ±1.
    let slack = 2i64;
    if (src_frames - dst_frames).abs() > slack {
        return Err(format!(
            "frame mismatch: izvor={src_frames} proxy={dst_frames} (max ±{slack})"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_mp4_path_sanitizes_clip_id() {
        let dir = std::path::Path::new("/tmp/proxy");
        assert_eq!(
            proxy_mp4_path(dir, "MIRONIK 1096").file_name().unwrap(),
            "MIRONIK_1096.mp4"
        );
    }

    #[test]
    fn proxy_path_uses_mxf_for_xdcam() {
        let dir = std::path::Path::new("/tmp/proxy");
        let recipe = recipe_for_source(super::super::proxy_source::TvSourceClass::Pal50i);
        assert_eq!(
            proxy_path_for_recipe(dir, "clip1", recipe)
                .extension()
                .unwrap(),
            "mxf"
        );
    }
}
