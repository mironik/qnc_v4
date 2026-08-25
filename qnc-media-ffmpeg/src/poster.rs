use std::path::Path;
use std::process::Command;

use qnc_service_contracts::ArtifactRef;

use crate::FfmpegToolchain;

pub const POSTER_THUMB_WIDTH: u32 = 112;
pub const POSTER_THUMB_HEIGHT: u32 = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FfmpegPosterOptions {
    pub toolchain: FfmpegToolchain,
}

impl Default for FfmpegPosterOptions {
    fn default() -> Self {
        Self {
            toolchain: FfmpegToolchain::default(),
        }
    }
}

pub fn extract_poster_jpeg_at_seek(
    source: &Path,
    dest: &Path,
    seek_sec: f64,
) -> Result<ArtifactRef, String> {
    extract_poster_jpeg_at_seek_with_options(
        source,
        dest,
        seek_sec,
        &FfmpegPosterOptions::default(),
    )
}

pub fn extract_poster_jpeg_at_seek_with_options(
    source: &Path,
    dest: &Path,
    seek_sec: f64,
    options: &FfmpegPosterOptions,
) -> Result<ArtifactRef, String> {
    if !source.is_file() {
        return Err(format!("izvor ne postoji: {}", source.display()));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let seek = format_decimal(seek_sec.max(0.0), 2);
    let mut cmd = Command::new(options.toolchain.ffmpeg());
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    cmd.args(["-ss"]).arg(&seek).arg("-i").arg(source);
    cmd.args(["-vf"]).arg(poster_scale_filter());
    cmd.args([
        "-frames:v",
        "1",
        "-q:v",
        "2",
        "-pix_fmt",
        "yuvj420p",
        "-strict",
        "unofficial",
    ]);
    cmd.arg(dest);
    let output = cmd
        .output()
        .map_err(|e| format!("ffmpeg pokretanje: {e}"))?;
    if !output.status.success() {
        return Err(stderr_or_default(&output, "ffmpeg neuspjesan"));
    }
    if !dest.is_file() {
        return Err("ffmpeg nije kreirao poster".into());
    }
    Ok(ArtifactRef {
        path: dest.to_path_buf(),
        media_type: "image/jpeg".into(),
        render_version: None,
    })
}

fn poster_scale_filter() -> String {
    format!(
        "scale={}:{}:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2:color=black",
        POSTER_THUMB_WIDTH, POSTER_THUMB_HEIGHT, POSTER_THUMB_WIDTH, POSTER_THUMB_HEIGHT
    )
}

fn format_decimal(value: f64, precision: usize) -> String {
    let value = if value.is_finite() { value } else { 0.0 };
    format!("{:.*}", precision, value).replace(',', ".")
}

fn stderr_or_default(output: &std::process::Output, fallback: &str) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.trim().is_empty() {
        fallback.to_string()
    } else {
        stderr.trim().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::extract_poster_jpeg_at_seek_with_options;
    use crate::FfmpegToolchain;

    #[test]
    fn missing_source_is_rejected_before_ffmpeg() {
        let options = super::FfmpegPosterOptions {
            toolchain: FfmpegToolchain::new("missing_ffmpeg", "missing_ffprobe").unwrap(),
        };
        let base = std::env::temp_dir().join(format!(
            "qnc_media_poster_missing_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let err = extract_poster_jpeg_at_seek_with_options(
            &base.join("missing.mp4"),
            &base.join("poster.jpg"),
            0.0,
            &options,
        )
        .unwrap_err();
        assert!(err.contains("izvor ne postoji"));
    }
}
