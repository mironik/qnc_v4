use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use qnc_service_contracts::{ArtifactRef, FilmstripFrameArtifact, FilmstripJobFrame};

use crate::FfmpegToolchain;

pub const FILMSTRIP_THUMB_WIDTH: u32 = 112;
pub const FILMSTRIP_THUMB_HEIGHT: u32 = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FfmpegFilmstripOptions {
    pub toolchain: FfmpegToolchain,
}

impl Default for FfmpegFilmstripOptions {
    fn default() -> Self {
        Self {
            toolchain: FfmpegToolchain::default(),
        }
    }
}

pub fn build_filmstrip_frame_artifacts_at_paths(
    source: &Path,
    frames: &[FilmstripJobFrame],
) -> Result<Vec<FilmstripFrameArtifact>, String> {
    build_filmstrip_frame_artifacts_at_paths_with_options(
        source,
        frames,
        &FfmpegFilmstripOptions::default(),
    )
}

pub fn build_filmstrip_frame_artifacts_at_paths_with_options(
    source: &Path,
    frames: &[FilmstripJobFrame],
    options: &FfmpegFilmstripOptions,
) -> Result<Vec<FilmstripFrameArtifact>, String> {
    if frames.len() < 2 {
        return Err("filmstrip requires at least two frames".into());
    }
    if !source.is_file() {
        return Err(format!("izvor ne postoji: {}", source.display()));
    }

    let missing = frames
        .iter()
        .any(|frame| !frame_file_ready(&frame.output_path));

    if missing {
        let _ = extract_filmstrip_frames_at_seeks_with_options(source, frames, options);
    }

    let artifacts: Vec<FilmstripFrameArtifact> = frames
        .iter()
        .filter_map(|frame| {
            let path = frame.output_path.clone();
            frame_file_ready(&path).then(|| FilmstripFrameArtifact {
                index: frame.index,
                seek_sec: frame.seek_sec,
                artifact: artifact(path),
            })
        })
        .collect();

    if artifacts.is_empty() {
        return Err("filmstrip did not produce any valid JPEG frames".into());
    }
    Ok(artifacts)
}

fn extract_filmstrip_frames_at_seeks_with_options(
    source: &Path,
    frames: &[FilmstripJobFrame],
    options: &FfmpegFilmstripOptions,
) -> Vec<Result<(), String>> {
    if frames.is_empty() {
        return Vec::new();
    }
    if !source.is_file() {
        return vec![Err(format!("izvor ne postoji: {}", source.display()))];
    }

    for frame in frames {
        if let Some(parent) = frame.output_path.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                return frames
                    .iter()
                    .map(|_| Err(format!("filmstrip output dir: {error}")))
                    .collect();
            }
        }
    }

    let Some(parent) = frames[0].output_path.parent() else {
        return frames
            .iter()
            .map(|_| Err("filmstrip output path has no parent".into()))
            .collect();
    };
    let Some(interval_sec) = filmstrip_interval_seconds(frames) else {
        return frames
            .iter()
            .map(|_| Err("filmstrip interval is invalid".into()))
            .collect();
    };
    let temp_dir = parent.join(format!(
        ".qnc_filmstrip_tmp_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    if let Err(error) = std::fs::create_dir_all(&temp_dir) {
        return frames
            .iter()
            .map(|_| Err(format!("filmstrip temp dir: {error}")))
            .collect();
    }

    let scale = filmstrip_scale_filter();
    let fps_rate = format_decimal(1.0 / interval_sec, 9);
    let filter = format!("fps={fps_rate},{scale}");
    let frame_count = frames.len().to_string();
    let pattern = temp_dir.join("%03d.jpg");
    let mut cmd = Command::new(options.toolchain.ffmpeg());
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    cmd.arg("-i").arg(source);
    cmd.args(["-an", "-sn", "-dn"]);
    cmd.arg("-vf").arg(filter);
    cmd.args([
        "-frames:v",
        &frame_count,
        "-q:v",
        "2",
        "-pix_fmt",
        "yuvj420p",
        "-strict",
        "unofficial",
        "-start_number",
        "0",
    ]);
    cmd.arg(&pattern);

    let command_result = match cmd.output() {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(stderr_or_default(&output, "filmstrip single-pass failed")),
        Err(error) => Err(format!("filmstrip single-pass start: {error}")),
    };
    if let Err(error) = command_result {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return frames.iter().map(|_| Err(error.clone())).collect();
    }

    let results: Vec<Result<(), String>> = frames
        .iter()
        .enumerate()
        .map(|(index, frame)| {
            let temp = temp_dir.join(format!("{index:03}.jpg"));
            if !frame_file_ready(&temp) {
                return Err("filmstrip single-pass frame missing".into());
            }
            move_or_copy(&temp, &frame.output_path)
        })
        .collect();
    let _ = std::fs::remove_dir_all(&temp_dir);
    results
}

fn artifact(path: PathBuf) -> ArtifactRef {
    ArtifactRef {
        path,
        media_type: "image/jpeg".into(),
        render_version: None,
    }
}

fn move_or_copy(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(_) => {
            std::fs::copy(from, to).map_err(|e| e.to_string())?;
            Ok(())
        }
    }
}

fn filmstrip_interval_seconds(frames: &[FilmstripJobFrame]) -> Option<f64> {
    frames
        .windows(2)
        .filter_map(|pair| {
            let delta = pair[1].seek_sec - pair[0].seek_sec;
            (delta.is_finite() && delta > 0.0).then_some(delta)
        })
        .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
}

fn frame_file_ready(path: &Path) -> bool {
    path.is_file() && path.metadata().map(|m| m.len()).unwrap_or(0) > 0
}

fn filmstrip_scale_filter() -> String {
    format!(
        "scale={}:{}:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2:color=black",
        FILMSTRIP_THUMB_WIDTH,
        FILMSTRIP_THUMB_HEIGHT,
        FILMSTRIP_THUMB_WIDTH,
        FILMSTRIP_THUMB_HEIGHT
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
    use super::*;

    #[test]
    fn build_uses_existing_ready_frames_without_ffmpeg() {
        let base = std::env::temp_dir().join(format!(
            "qnc_media_filmstrip_existing_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let source = base.join("source.mp4");
        std::fs::write(&source, b"media").unwrap();
        let out_dir = base.join("filmstrip");
        std::fs::create_dir_all(&out_dir).unwrap();
        let one = out_dir.join("000_0_00.jpg");
        let two = out_dir.join("001_1_00.jpg");
        std::fs::write(&one, b"jpeg").unwrap();
        std::fs::write(&two, b"jpeg").unwrap();

        let frames = vec![
            FilmstripJobFrame {
                index: 0,
                seek_sec: 0.0,
                output_path: one.clone(),
            },
            FilmstripJobFrame {
                index: 1,
                seek_sec: 1.0,
                output_path: two.clone(),
            },
        ];
        let options = FfmpegFilmstripOptions {
            toolchain: FfmpegToolchain::new("missing_ffmpeg", "missing_ffprobe").unwrap(),
        };

        let artifacts =
            build_filmstrip_frame_artifacts_at_paths_with_options(&source, &frames, &options)
                .unwrap();

        assert_eq!(artifacts.len(), 2);
        assert_eq!(artifacts[0].artifact.path, one);
        assert_eq!(artifacts[1].artifact.path, two);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn filmstrip_output_dir_error_does_not_fall_back_to_per_frame_extracts() {
        let base = std::env::temp_dir().join(format!(
            "qnc_media_filmstrip_no_per_frame_fallback_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let source = base.join("source.mp4");
        std::fs::write(&source, b"media").unwrap();
        let blocked_parent = base.join("blocked");
        std::fs::write(&blocked_parent, b"not a dir").unwrap();

        let frames = vec![
            FilmstripJobFrame {
                index: 0,
                seek_sec: 0.0,
                output_path: blocked_parent.join("000_0_00.jpg"),
            },
            FilmstripJobFrame {
                index: 1,
                seek_sec: 1.0,
                output_path: blocked_parent.join("001_1_00.jpg"),
            },
        ];
        let options = FfmpegFilmstripOptions {
            toolchain: FfmpegToolchain::new("missing_ffmpeg", "missing_ffprobe").unwrap(),
        };

        let results = extract_filmstrip_frames_at_seeks_with_options(&source, &frames, &options);

        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| {
            result
                .as_ref()
                .err()
                .is_some_and(|error| error.contains("filmstrip output dir"))
        }));
        let _ = std::fs::remove_dir_all(base);
    }
}
