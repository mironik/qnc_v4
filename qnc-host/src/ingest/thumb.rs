use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::locale_number::{format_decimal, parse_decimal};

/// Filmstrip / media-pool traka (112×64 u UI).
pub const FILMSTRIP_THUMB_WIDTH: u32 = 112;
pub const FILMSTRIP_THUMB_HEIGHT: u32 = 64;

#[cfg(windows)]
fn find_file_recursive(dir: &Path, file_name: &str, depth: u32) -> Option<PathBuf> {
    if depth == 0 || !dir.is_dir() {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if path.file_name().and_then(|n| n.to_str()) == Some(file_name) {
                return Some(path);
            }
        } else if path.is_dir() {
            if let Some(found) = find_file_recursive(&path, file_name, depth - 1) {
                return Some(found);
            }
        }
    }
    None
}

#[cfg(not(windows))]
fn find_file_recursive(_dir: &Path, _file_name: &str, _depth: u32) -> Option<PathBuf> {
    None
}

#[cfg(windows)]
fn winget_tool(file_name: &str) -> Option<PathBuf> {
    let local = std::env::var("LOCALAPPDATA").ok()?;
    let base = PathBuf::from(local)
        .join("Microsoft")
        .join("WinGet")
        .join("Packages");
    find_file_recursive(&base, file_name, 10)
}

#[cfg(not(windows))]
fn winget_tool(_file_name: &str) -> Option<PathBuf> {
    None
}

fn sibling_ffprobe(ffmpeg: &Path) -> Option<PathBuf> {
    ffmpeg.parent().map(|dir| {
        dir.join(if cfg!(windows) {
            "ffprobe.exe"
        } else {
            "ffprobe"
        })
    })
}

fn ffmpeg_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(raw) = std::env::var("QNC_FFMPEG") {
        let p = PathBuf::from(raw.trim());
        if !p.as_os_str().is_empty() {
            out.push(p);
        }
    }
    if let Some(p) = winget_tool(if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }) {
        out.push(p);
    }
    if let Ok(root) = std::env::var("QNC_ROOT") {
        let root = PathBuf::from(root.trim());
        if root.as_os_str() != "" {
            out.push(root.join("bin").join(if cfg!(windows) {
                "ffmpeg.exe"
            } else {
                "ffmpeg"
            }));
            out.push(
                root.join("tools")
                    .join("ffmpeg")
                    .join("bin")
                    .join(if cfg!(windows) {
                        "ffmpeg.exe"
                    } else {
                        "ffmpeg"
                    }),
            );
        }
    }
    out.push(PathBuf::from(if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }));
    out
}

pub(crate) fn resolve_ffmpeg() -> Option<PathBuf> {
    static CACHE: OnceLock<Option<PathBuf>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            for candidate in ffmpeg_candidates() {
                if candidate.is_file() {
                    return Some(candidate);
                }
                let output = Command::new(&candidate).arg("-version").output();
                if output.map(|o| o.status.success()).unwrap_or(false) {
                    return Some(candidate);
                }
            }
            None
        })
        .clone()
}

pub fn ffmpeg_available() -> bool {
    resolve_ffmpeg().is_some()
}

pub fn ffprobe_available() -> bool {
    resolve_ffprobe().is_some()
}

fn ffprobe_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(raw) = std::env::var("QNC_FFPROBE") {
        let p = PathBuf::from(raw.trim());
        if !p.as_os_str().is_empty() {
            out.push(p);
        }
    }
    if let Ok(raw) = std::env::var("QNC_FFMPEG") {
        let ffmpeg = PathBuf::from(raw.trim());
        if let Some(probe) = sibling_ffprobe(&ffmpeg) {
            out.push(probe);
        }
    }
    if let Some(p) = winget_tool(if cfg!(windows) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    }) {
        out.push(p);
    }
    if let Ok(root) = std::env::var("QNC_ROOT") {
        let root = PathBuf::from(root.trim());
        if root.as_os_str() != "" {
            out.push(root.join("bin").join(if cfg!(windows) {
                "ffprobe.exe"
            } else {
                "ffprobe"
            }));
            out.push(
                root.join("tools")
                    .join("ffmpeg")
                    .join("bin")
                    .join(if cfg!(windows) {
                        "ffprobe.exe"
                    } else {
                        "ffprobe"
                    }),
            );
        }
    }
    out.push(PathBuf::from(if cfg!(windows) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    }));
    out
}

pub(crate) fn resolve_ffprobe() -> Option<PathBuf> {
    static CACHE: OnceLock<Option<PathBuf>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            for candidate in ffprobe_candidates() {
                if candidate.is_file() {
                    return Some(candidate);
                }
                let output = Command::new(&candidate).arg("-version").output();
                if output.map(|o| o.status.success()).unwrap_or(false) {
                    return Some(candidate);
                }
            }
            None
        })
        .clone()
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

/// Keep source raster (even dims) — no hardcoded 720/1080 pad.
fn preview_native_scale_filter() -> &'static str {
    "scale=trunc(iw/2)*2:trunc(ih/2)*2"
}

fn ffmpeg_err(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.trim().is_empty() {
        "ffmpeg neuspješan".into()
    } else {
        stderr.trim().to_string()
    }
}

/// Raspored seek pozicija za filmstrip: pocetak svake od N vremenskih cjelina.
pub fn timeline_seek_seconds(duration_sec: f64, frames: u32) -> Vec<f64> {
    let n = frames.clamp(2, 24) as usize;
    let dur = duration_sec.max(0.01);
    let step = dur / n as f64;
    (0..n)
        .map(|index| {
            let sec = index as f64 * step;
            (sec * 100.0).round() / 100.0
        })
        .collect()
}

pub fn filmstrip_frame_path(out_dir: &Path, index: usize, sec: f64) -> PathBuf {
    let sec_label = format_decimal(sec.max(0.0), 2).replace('.', "_");
    out_dir.join(format!("{:03}_{}.jpg", index, sec_label))
}

/// Trajanje medija preko ffprobe (QNC_FFPROBE, QNC_ROOT/bin, PATH).
pub fn media_duration_sec(source: &Path) -> Option<f64> {
    probe_media(source).map(|p| p.duration_sec)
}

/// Metapodaci izvornog klipa (trajanje, fps, rezolucija, codec, interlace).
#[derive(Debug, Clone, PartialEq)]
pub struct MediaProbe {
    pub duration_sec: f64,
    pub fps: f64,
    pub resolution: String,
    pub codec: String,
    pub has_audio: bool,
    pub audio_channels: u8,
    /// ffprobe `field_order` (progressive, tt, bb, tb, bt, …).
    pub field_order: String,
    pub interlaced: bool,
}

/// ffprobe na media streamovima + format trajanje (QNC_FFPROBE, QNC_ROOT/bin, PATH).
pub fn probe_media(source: &Path) -> Option<MediaProbe> {
    if !source.is_file() {
        return None;
    }
    let ffprobe = resolve_ffprobe()?;
    let output = Command::new(&ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type,codec_name,width,height,avg_frame_rate,r_frame_rate,duration,field_order,channels",
            "-show_entries",
            "format=duration",
            "-of",
            "json",
        ])
        .arg(source)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let format = json.get("format");
    let streams = json
        .get("streams")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let stream = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(|v| v.as_str()) == Some("video"));
    let audio_stream = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(|v| v.as_str()) == Some("audio"));
    let audio_channels = streams
        .iter()
        .filter(|s| s.get("codec_type").and_then(|v| v.as_str()) == Some("audio"))
        .filter_map(|s| s.get("channels").and_then(|v| v.as_u64()))
        .max()
        .map(|channels| (channels as u8).clamp(1, 4))
        .unwrap_or(0);
    let duration_sec = format
        .and_then(|f| f.get("duration"))
        .and_then(|v| v.as_str())
        .and_then(parse_decimal)
        .filter(|d| *d > 0.0)
        .or_else(|| {
            stream
                .and_then(|s| s.get("duration"))
                .and_then(|v| v.as_str())
                .and_then(parse_decimal)
                .filter(|d| *d > 0.0)
        })
        .or_else(|| {
            audio_stream
                .and_then(|s| s.get("duration"))
                .and_then(|v| v.as_str())
                .and_then(parse_decimal)
                .filter(|d| *d > 0.0)
        })?;
    let width = stream
        .and_then(|s| s.get("width"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let height = stream
        .and_then(|s| s.get("height"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let codec = stream
        .or(audio_stream)
        .and_then(|s| s.get("codec_name"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let fps = stream
        .and_then(|s| s.get("avg_frame_rate"))
        .and_then(|v| v.as_str())
        .and_then(parse_frame_rate)
        .or_else(|| {
            stream
                .and_then(|s| s.get("r_frame_rate"))
                .and_then(|v| v.as_str())
                .and_then(parse_frame_rate)
        })
        .unwrap_or(0.0);
    let field_order = stream
        .and_then(|s| s.get("field_order"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .trim()
        .to_ascii_lowercase();
    let interlaced = matches!(
        field_order.as_str(),
        "tt" | "bb" | "tb" | "bt" | "interlaced"
    ) || field_order.contains("interlaced")
        || field_order.contains("top")
        || field_order.contains("bottom");
    // ffprobe ponekad vrati "progressive" eksplicitno.
    let interlaced = if field_order == "progressive" {
        false
    } else {
        interlaced
    };
    let resolution = if width > 0 && height > 0 {
        format!("{width}x{height}")
    } else {
        String::new()
    };
    Some(MediaProbe {
        duration_sec,
        fps,
        resolution,
        codec,
        has_audio: audio_channels > 0,
        audio_channels,
        field_order,
        interlaced,
    })
}

pub fn media_has_audio_stream(source: &Path) -> Option<bool> {
    if !source.is_file() {
        return None;
    }
    let ffprobe = resolve_ffprobe()?;
    let output = Command::new(&ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=codec_type",
            "-of",
            "csv=p=0",
        ])
        .arg(source)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

fn parse_frame_rate(text: &str) -> Option<f64> {
    let t = text.trim();
    if t.is_empty() || t == "0/0" || t.eq_ignore_ascii_case("n/a") {
        return None;
    }
    if let Some((num, den)) = t.split_once('/') {
        let n: f64 = num.trim().parse().ok()?;
        let d: f64 = den.trim().parse().ok()?;
        if n > 0.0 && d > 0.0 {
            return Some(n / d);
        }
        return None;
    }
    parse_decimal(t).filter(|v| *v > 0.0)
}

#[cfg(test)]
mod tests {
    use super::parse_frame_rate;

    #[test]
    fn parse_frame_rate_fraction() {
        assert!((parse_frame_rate("50/1").unwrap() - 50.0).abs() < 0.001);
        assert!((parse_frame_rate("30000/1001").unwrap() - 29.97).abs() < 0.01);
    }

    #[test]
    fn parse_frame_rate_rejects_invalid() {
        assert!(parse_frame_rate("0/0").is_none());
        assert!(parse_frame_rate("").is_none());
    }
}

/// Ekstrakcija JPEG-a na zadanoj seek poziciji (fallback za jedan kadar).
pub fn extract_poster_jpeg_at_seek(
    source: &Path,
    dest: &Path,
    seek_sec: f64,
) -> Result<(), String> {
    extract_jpeg_at_seek_with_hw(source, dest, seek_sec, &filmstrip_scale_filter(), true)
}

/// Preview JPEG for qnc-av-player — same raster as source/proxy (ffprobe), not filmstrip.
pub fn extract_preview_jpeg_at_seek(
    source: &Path,
    dest: &Path,
    seek_sec: f64,
) -> Result<(), String> {
    extract_jpeg_at_seek_with_hw(source, dest, seek_sec, preview_native_scale_filter(), true)
}

/// Filmstrip: isključivo CPU decode — ne dijeli GPU s proxy generate (QSV).
pub fn extract_poster_jpeg_at_seek_cpu(
    source: &Path,
    dest: &Path,
    seek_sec: f64,
) -> Result<(), String> {
    extract_jpeg_at_seek_with_hw(source, dest, seek_sec, &filmstrip_scale_filter(), false)
}

#[allow(dead_code)]
fn extract_poster_jpeg_at_seek_with_hw(
    source: &Path,
    dest: &Path,
    seek_sec: f64,
    allow_hw: bool,
) -> Result<(), String> {
    extract_jpeg_at_seek_with_hw(source, dest, seek_sec, &filmstrip_scale_filter(), allow_hw)
}

fn extract_jpeg_at_seek_with_hw(
    source: &Path,
    dest: &Path,
    seek_sec: f64,
    vf: &str,
    allow_hw: bool,
) -> Result<(), String> {
    if !source.is_file() {
        return Err(format!("izvor ne postoji: {}", source.display()));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let ffmpeg = resolve_ffmpeg()
        .ok_or("ffmpeg nije instaliran (postavi QNC_FFMPEG ili dodaj u PATH)".to_string())?;
    let seek = format_decimal(seek_sec.max(0.0), 2);
    let encoder = crate::ingest::proxy_encode::resolve_proxy_encoder(&ffmpeg);
    let run = |use_hw: bool| -> Result<(), String> {
        let mut cmd = Command::new(&ffmpeg);
        cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
        if use_hw {
            crate::ingest::proxy_encode::append_decode_hwaccel(&mut cmd, encoder);
        }
        cmd.args(["-ss"]).arg(&seek).arg("-i").arg(source);
        cmd.args(["-vf"]).arg(vf);
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
            return Err(ffmpeg_err(&output));
        }
        if !dest.is_file() {
            return Err("ffmpeg nije kreirao poster".into());
        }
        Ok(())
    };
    if allow_hw && encoder.uses_gpu() && run(true).is_ok() {
        return Ok(());
    }
    run(false)
}

/// Filmstrip frameovi se moraju vaditi kao pocetak svake vremenske cjeline.
///
/// Jedan FFmpeg proces otvara isti klip vise puta s razlicitim input seekom.
/// Time izbjegavamo 13/14 odvojenih procesa, a svaki slot i dalje dobiva
/// vlastitu pocetnu slicicu iz cijelog trajanja klipa.
pub fn extract_filmstrip_frames_at_seeks(
    source: &Path,
    seeks: &[f64],
    outputs: &[PathBuf],
) -> Vec<Result<(), String>> {
    if seeks.is_empty() {
        return vec![];
    }
    if seeks.len() != outputs.len() {
        return vec![Err("seeks/outputs mismatch".into())];
    }
    if !source.is_file() {
        return vec![Err(format!("izvor ne postoji: {}", source.display()))];
    }

    if !outputs.iter().all(|out| {
        out.parent()
            .map(|parent| std::fs::create_dir_all(parent).is_ok())
            .unwrap_or(true)
    }) {
        return seeks
            .iter()
            .zip(outputs.iter())
            .map(|(sec, out)| extract_poster_jpeg_at_seek_cpu(source, out, *sec))
            .collect();
    }

    let ffmpeg = match resolve_ffmpeg() {
        Some(path) => path,
        None => {
            return seeks
                .iter()
                .zip(outputs.iter())
                .map(|(sec, out)| {
                    extract_poster_jpeg_at_seek_cpu(source, out, *sec)
                        .map_err(|_| "ffmpeg nije instaliran".into())
                })
                .collect();
        }
    };

    let scale = filmstrip_scale_filter();
    let mut cmd = Command::new(&ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    for sec in seeks {
        cmd.arg("-ss")
            .arg(format_decimal(sec.max(0.0), 2))
            .arg("-i")
            .arg(source);
    }
    for (index, out) in outputs.iter().enumerate() {
        cmd.arg("-map")
            .arg(format!("{index}:v:0"))
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
            .arg(out);
    }

    match cmd.output() {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let err = ffmpeg_err(&output);
            return seeks
                .iter()
                .zip(outputs.iter())
                .map(|(sec, out)| {
                    extract_poster_jpeg_at_seek_cpu(source, out, *sec)
                        .map_err(|fallback| format!("filmstrip multi-seek: {err}; {fallback}"))
                })
                .collect();
        }
        Err(error) => {
            return seeks
                .iter()
                .zip(outputs.iter())
                .map(|(sec, out)| {
                    extract_poster_jpeg_at_seek_cpu(source, out, *sec).map_err(|fallback| {
                        format!("filmstrip multi-seek pokretanje: {error}; {fallback}")
                    })
                })
                .collect();
        }
    }

    seeks
        .iter()
        .zip(outputs.iter())
        .map(|(sec, out)| {
            if frame_file_ready(out) {
                Ok(())
            } else {
                extract_poster_jpeg_at_seek_cpu(source, out, *sec)
                    .map_err(|fallback| format!("filmstrip frame missing; {fallback}"))
            }
        })
        .collect()
}

fn frame_file_ready(path: &Path) -> bool {
    path.is_file() && path.metadata().map(|m| m.len()).unwrap_or(0) > 0
}
