use std::path::{Path, PathBuf};
use std::process::Command;

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

    let missing: Vec<FilmstripJobFrame> = frames
        .iter()
        .filter(|frame| !frame_file_ready(&frame.output_path))
        .cloned()
        .collect();

    if !missing.is_empty() {
        let results = extract_filmstrip_frames_at_seeks_with_options(source, &missing, options);
        for (frame, result) in missing.iter().zip(results.into_iter()) {
            let _ok = match result {
                Ok(()) if frame_file_ready(&frame.output_path) => true,
                _ => {
                    extract_poster_jpeg_at_seek_cpu_with_options(
                        source,
                        &frame.output_path,
                        frame.seek_sec,
                        options,
                    )
                    .is_ok()
                        && frame_file_ready(&frame.output_path)
                }
            };
        }
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

    if !frames.iter().all(|frame| {
        frame
            .output_path
            .parent()
            .map(|parent| std::fs::create_dir_all(parent).is_ok())
            .unwrap_or(true)
    }) {
        return frames
            .iter()
            .map(|frame| {
                extract_poster_jpeg_at_seek_cpu_with_options(
                    source,
                    &frame.output_path,
                    frame.seek_sec,
                    options,
                )
            })
            .collect();
    }

    let scale = filmstrip_scale_filter();
    let mut cmd = Command::new(options.toolchain.ffmpeg());
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    for frame in frames {
        cmd.arg("-ss")
            .arg(format_decimal(frame.seek_sec.max(0.0), 2))
            .arg("-i")
            .arg(source);
    }
    for (input_index, frame) in frames.iter().enumerate() {
        cmd.arg("-map")
            .arg(format!("{input_index}:v:0"))
            .arg("-vf")
            .arg(&scale)
            .args([
                "-frames:v",
                "1",
                "-q:v",
                "2",
                "-pix_fmt",
                "yuvj420p",
                "-strict",
                "unofficial",
            ])
            .arg(&frame.output_path);
    }

    match cmd.output() {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let err = stderr_or_default(&output, "filmstrip multi-seek failed");
            return frames
                .iter()
                .map(|frame| {
                    extract_poster_jpeg_at_seek_cpu_with_options(
                        source,
                        &frame.output_path,
                        frame.seek_sec,
                        options,
                    )
                    .map_err(|fallback| format!("filmstrip multi-seek: {err}; {fallback}"))
                })
                .collect();
        }
        Err(error) => {
            return frames
                .iter()
                .map(|frame| {
                    extract_poster_jpeg_at_seek_cpu_with_options(
                        source,
                        &frame.output_path,
                        frame.seek_sec,
                        options,
                    )
                    .map_err(|fallback| format!("filmstrip multi-seek start: {error}; {fallback}"))
                })
                .collect();
        }
    }

    frames
        .iter()
        .map(|frame| {
            if frame_file_ready(&frame.output_path) {
                Ok(())
            } else {
                extract_poster_jpeg_at_seek_cpu_with_options(
                    source,
                    &frame.output_path,
                    frame.seek_sec,
                    options,
                )
                .map_err(|fallback| format!("filmstrip frame missing; {fallback}"))
            }
        })
        .collect()
}

fn extract_poster_jpeg_at_seek_cpu_with_options(
    source: &Path,
    dest: &Path,
    seek_sec: f64,
    options: &FfmpegFilmstripOptions,
) -> Result<(), String> {
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
    cmd.args(["-vf"]).arg(filmstrip_scale_filter());
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
    Ok(())
}

fn artifact(path: PathBuf) -> ArtifactRef {
    ArtifactRef {
        path,
        media_type: "image/jpeg".into(),
        render_version: None,
    }
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
}
