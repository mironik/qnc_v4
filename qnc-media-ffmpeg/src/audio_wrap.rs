use std::path::Path;
use std::process::Command;

use crate::FfmpegToolchain;

pub fn wrap_audio_with_timecode(source: &Path, dest: &Path, fps: f64) -> Result<(), String> {
    wrap_audio_with_timecode_with_toolchain(source, dest, fps, &FfmpegToolchain::default())
}

pub fn wrap_audio_with_timecode_with_toolchain(
    source: &Path,
    dest: &Path,
    fps: f64,
    toolchain: &FfmpegToolchain,
) -> Result<(), String> {
    if !source.is_file() {
        return Err(format!("audio izvor ne postoji: {}", source.display()));
    }
    let fps = require_fps(fps, "audio wrap fps")?;
    let (num, den) = rational_fps(fps);
    let rate = if den == 1 {
        format!("{num}")
    } else {
        format!("{num}/{den}")
    };

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let color = format!("color=c=black:s=1920x1080:r={rate}");
    let output = Command::new(toolchain.ffmpeg())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            &color,
            "-i",
        ])
        .arg(source)
        .args([
            "-map",
            "0:v:0",
            "-map",
            "1:a:0?",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-tune",
            "stillimage",
            "-crf",
            "28",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-shortest",
            "-timecode",
            "00:00:00:00",
            "-movflags",
            "+faststart",
        ])
        .arg(dest)
        .output()
        .map_err(|e| format!("ffmpeg audio wrap start: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "audio wrap (timecode) nije uspio: {}",
            stderr_or_default(&output, "ffmpeg neuspjesan")
        ));
    }
    if !dest.is_file() || dest.metadata().map(|m| m.len()).unwrap_or(0) == 0 {
        return Err(format!(
            "audio wrap nije napisao datoteku: {}",
            dest.display()
        ));
    }
    Ok(())
}

fn require_fps(raw: f64, context: &str) -> Result<f64, String> {
    if raw.is_finite() && raw > 0.0 {
        Ok(raw)
    } else {
        Err(format!("{context}: missing valid FPS"))
    }
}

fn rational_fps(fps: f64) -> (i64, i64) {
    const NTSC: [(i64, i64); 4] = [(24000, 1001), (30000, 1001), (48000, 1001), (60000, 1001)];
    for (num, den) in NTSC {
        if (fps - (num as f64 / den as f64)).abs() < 0.01 {
            return (num, den);
        }
    }
    let rounded = fps.round();
    if (fps - rounded).abs() < 0.001 && rounded >= 1.0 {
        return (rounded as i64, 1);
    }
    ((fps * 1000.0).round() as i64, 1000)
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
    use super::wrap_audio_with_timecode_with_toolchain;
    use crate::FfmpegToolchain;

    #[test]
    fn invalid_fps_is_rejected_before_ffmpeg() {
        let toolchain = FfmpegToolchain::new("missing_ffmpeg", "missing_ffprobe").unwrap();
        let base =
            std::env::temp_dir().join(format!("qnc_audio_wrap_invalid_fps_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let source = base.join("a.wav");
        std::fs::write(&source, b"not audio").unwrap();
        let err = wrap_audio_with_timecode_with_toolchain(
            &source,
            &base.join("out.mp4"),
            0.0,
            &toolchain,
        )
        .unwrap_err();
        assert!(err.contains("missing valid FPS"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
