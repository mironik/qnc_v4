use std::path::{Path, PathBuf};
use std::time::Instant;

use qnc_media_ffmpeg::proxy::{
    self, FfmpegProxyBuildOptions, ProxyCodec, ProxyRecipe, ProxyScale, ProxyVideoEncoder,
    TvSourceClass,
};
use qnc_media_ffmpeg::FfmpegToolchain;
use tracing::info;

use crate::ingest::thumb::{resolve_ffmpeg, resolve_ffprobe};

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

fn proxy_options() -> FfmpegProxyBuildOptions {
    let mut options = FfmpegProxyBuildOptions::default();
    if let (Some(ffmpeg), Some(ffprobe)) = (resolve_ffmpeg(), resolve_ffprobe()) {
        if let Ok(toolchain) = FfmpegToolchain::new(ffmpeg, ffprobe) {
            options.toolchain = toolchain;
        }
    }
    if let Some(profile) = crate::hardware_profile::get() {
        if profile.proxy_encoder_verified || !profile.h264_encoders.is_empty() {
            options = options.with_preferred_encoder(Some(ProxyVideoEncoder::from_label(
                &profile.proxy_encoder,
            )));
        }
        options = options.with_vaapi_device(profile.vaapi_device.clone());
    }
    options
}

/// Legacy helper: H.264 path. Prefer `proxy_path_for_recipe`.
#[allow(dead_code)]
pub fn proxy_mp4_path(proxy_dir: &Path, clip_id: &str) -> PathBuf {
    proxy_dir.join(format!("{}.mp4", safe_clip_stem(clip_id)))
}

#[allow(dead_code)]
pub fn proxy_path_for_recipe(proxy_dir: &Path, clip_id: &str, recipe: ProxyRecipe) -> PathBuf {
    proxy::proxy_path_for_recipe(proxy_dir, clip_id, recipe)
}

/// Odredi destinaciju prema tipu izvora (PAL/NTSC x p/i).
pub fn proxy_dest_for_source(
    proxy_dir: &Path,
    clip_id: &str,
    source: &Path,
) -> Result<PathBuf, String> {
    let options = proxy_options();
    proxy::proxy_dest_for_source_with_options(proxy_dir, clip_id, source, &options)
}

/// Label aktivnog proxy enkodera (diagnostics / health).
pub fn active_proxy_encoder_label() -> Option<String> {
    resolve_ffmpeg()?;
    let options = proxy_options();
    Some(proxy::resolve_proxy_encoder(&options).label().to_string())
}

/// Sazetak recepta po tipu izvora, za health / dijagnostiku.
pub fn proxy_recipe_policy_snapshot() -> serde_json::Value {
    let classes = [
        TvSourceClass::Pal50p,
        TvSourceClass::Pal50i,
        TvSourceClass::Ntsc60p,
        TvSourceClass::Ntsc60i,
        TvSourceClass::Ntsc30p,
    ];
    let mut map = serde_json::Map::new();
    for class in classes {
        let recipe = proxy::recipe_for_source(class);
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

/// Terenski proxy prema tipu izvora. Aktivni FFmpeg model zivi u qnc-media-ffmpeg.
pub fn generate_field_proxy(source: &Path, dest: &Path) -> Result<(), String> {
    let options = proxy_options();
    let source_probe = proxy::probe_media_with_options(source, &options)?;
    let class = proxy::classify_tv_source(&source_probe);
    let recipe = proxy::recipe_for_source(class);
    let fps = source_probe.timebase.fps_num as f64 / source_probe.timebase.fps_den as f64;
    info!(
        "ingest proxy generate: source={} region={} class={} recipe={} fps={:.3}",
        source.display(),
        class.region_label(),
        class.label(),
        recipe.id(),
        fps
    );

    let started = Instant::now();
    let output_probe = proxy::generate_field_proxy_with_options(source, dest, &options)?;
    let elapsed = started.elapsed().as_secs_f64().max(0.001);
    let duration = source_probe.duration_sec.unwrap_or(0.0).max(0.001);
    let output_fps = output_probe.timebase.fps_num as f64 / output_probe.timebase.fps_den as f64;
    info!(
        "ingest proxy generate: done {} in {:.1}s ({:.1}x realtime) recipe={} output_fps={:.3}",
        dest.display(),
        elapsed,
        duration / elapsed,
        recipe.id(),
        output_fps
    );
    Ok(())
}

#[allow(dead_code)]
fn h264_native_recipe() -> ProxyRecipe {
    ProxyRecipe {
        codec: ProxyCodec::H264,
        scale: ProxyScale::Native,
        keep_interlace: false,
    }
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
        let recipe = proxy::recipe_for_source(TvSourceClass::Pal50i);
        assert_eq!(
            proxy_path_for_recipe(dir, "clip1", recipe)
                .extension()
                .unwrap(),
            "mxf"
        );
    }

    #[test]
    fn h264_native_recipe_keeps_legacy_mp4_extension() {
        let dir = std::path::Path::new("/tmp/proxy");
        assert_eq!(
            proxy_path_for_recipe(dir, "Clip 001", h264_native_recipe())
                .file_name()
                .unwrap(),
            "Clip_001.mp4"
        );
    }
}
