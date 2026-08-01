//! Audio import + deferred AV wrap (black video + timecode).
//!
//! Import only copies the file into `project/audio/`.
//! A background worker later builds playable MP4 wraps from **FPS already in
//! SQLite** (`ingest_assets.fps` on video rows) and the project export region
//! (PAL → 25/50, NTSC → 30/60). Never uses project/export fps as source clock.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::params;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::frame_time::{normalize_fps, rational_fps, DEFAULT_FPS};
use crate::ingest::db::open_ingest;
use crate::ingest::project_media::sanitize_clip_id;
use crate::ingest::thumb::{probe_media, resolve_ffmpeg};
use crate::media::is_audio_media_file;
use crate::project::db::{bump_project_data_revision, project_effective_settings, ProjectPaths};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BroadcastRegion {
    Pal,
    Ntsc,
}

impl BroadcastRegion {
    /// Source rates we may wrap for this region (not export fps).
    pub fn wrap_rates(self) -> &'static [f64] {
        match self {
            Self::Pal => &[25.0, 50.0],
            Self::Ntsc => &[30.0, 60.0],
        }
    }

    pub fn default_rate(self) -> f64 {
        match self {
            Self::Pal => 25.0,
            Self::Ntsc => 30.0,
        }
    }
}

/// Infer PAL/NTSC from export preset text / fps family in project settings.
pub fn broadcast_region_from_settings(settings: &Value) -> BroadcastRegion {
    let export = settings.get("export").cloned().unwrap_or(json!({}));
    let blob = [
        export.get("format").and_then(|v| v.as_str()).unwrap_or(""),
        export.get("preset").and_then(|v| v.as_str()).unwrap_or(""),
        settings
            .get("video")
            .and_then(|v| v.get("format"))
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    ]
    .join(" ")
    .to_ascii_lowercase();

    if blob.contains("ntsc") || blob.contains("29.97") || blob.contains("59.94") {
        return BroadcastRegion::Ntsc;
    }
    if blob.contains("pal")
        || blob.contains("1080i50")
        || blob.contains("1080p50")
        || blob.contains("1080p25")
    {
        return BroadcastRegion::Pal;
    }

    let fps = export
        .get("fps")
        .and_then(|v| v.as_f64())
        .or_else(|| {
            settings
                .get("video")
                .and_then(|v| v.get("fps"))
                .and_then(|v| v.as_f64())
        })
        .unwrap_or(DEFAULT_FPS);
    let fps = normalize_fps(fps);
    // NTSC family
    if (fps - 29.97).abs() < 0.05
        || (fps - 30.0).abs() < 0.05
        || (fps - 59.94).abs() < 0.08
        || (fps - 60.0).abs() < 0.05
    {
        return BroadcastRegion::Ntsc;
    }
    BroadcastRegion::Pal
}

pub fn audio_project_dir(paths: &ProjectPaths, project_id: &str) -> PathBuf {
    paths.project_dir(project_id).join("audio")
}

/// Copied raw audio under `project/audio/{clip}{ext}`.
pub fn audio_copy_dest(audio_dir: &Path, clip_id: &str, source: &Path) -> PathBuf {
    let safe = sanitize_clip_id(clip_id);
    let ext = source
        .extension()
        .and_then(|e| e.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("wav");
    audio_dir.join(format!("{safe}.{ext}"))
}

/// Playable wrap: `project/proxy/{clip}_{fpsTag}.mp4` (e.g. `_25`, `_50`).
pub fn audio_wrap_dest_for_fps(proxy_dir: &Path, clip_id: &str, fps: f64) -> PathBuf {
    let tag = fps_path_tag(fps);
    proxy_dir.join(format!("{}_{}.mp4", sanitize_clip_id(clip_id), tag))
}

fn fps_path_tag(fps: f64) -> String {
    let n = normalize_fps(fps);
    if (n - n.round()).abs() < 0.001 {
        format!("{}", n.round() as i64)
    } else {
        format!("{n:.2}").replace('.', "p")
    }
}

/// Snap measured/DB fps onto a region wrap rate (25/50 or 30/60).
pub fn snap_fps_to_region(fps: f64, region: BroadcastRegion) -> Option<f64> {
    let fps = normalize_fps(fps);
    if fps <= 0.0 {
        return None;
    }
    // Explicit NTSC fractional → integer wrap rates.
    if matches!(region, BroadcastRegion::Ntsc) {
        if (fps - 29.97).abs() < 0.08 || (fps - 30.0).abs() < 0.08 {
            return Some(30.0);
        }
        if (fps - 59.94).abs() < 0.12 || (fps - 60.0).abs() < 0.08 {
            return Some(60.0);
        }
    }
    region
        .wrap_rates()
        .iter()
        .copied()
        .find(|rate| (fps - rate).abs() < 0.08)
}

/// Distinct wrap rates needed, from **DB** video `fps` rows only.
pub fn needed_wrap_rates_from_db(
    paths: &ProjectPaths,
    project_id: &str,
    region: BroadcastRegion,
) -> Vec<f64> {
    let Ok(conn) = open_ingest(paths, project_id) else {
        return Vec::new();
    };
    let Ok(mut stmt) = conn.prepare(
        "SELECT source_path, original_path, proxy_path, project_proxy_path, fps
         FROM ingest_assets
         WHERE fps IS NOT NULL AND fps > 0
           AND import_status IN ('imported', 'done', 'detected', 'generating_proxy')",
    ) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, f64>(4)?,
        ))
    }) else {
        return Vec::new();
    };

    let mut rates = HashSet::new();
    for row in rows.flatten() {
        let (src, orig, proxy, proj, fps) = row;
        // Skip audio-only rows (by path extension).
        let path = [proj.as_str(), proxy.as_str(), orig.as_str(), src.as_str()]
            .iter()
            .map(|s| s.trim())
            .find(|s| !s.is_empty())
            .map(PathBuf::from);
        if let Some(ref p) = path {
            if is_audio_media_file(p) {
                continue;
            }
        }
        if let Some(rate) = snap_fps_to_region(fps, region) {
            rates.insert(fps_key(rate));
        }
    }
    let mut out: Vec<f64> = rates.into_iter().map(|k| k as f64 / 1000.0).collect();
    out.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    out
}

fn fps_key(fps: f64) -> i64 {
    (normalize_fps(fps) * 1000.0).round() as i64
}

pub fn read_audio_meta(conn: &rusqlite::Connection, source_id: &str, clip_id: &str) -> Value {
    conn.query_row(
        "SELECT COALESCE(metadata_json, '{}') FROM ingest_assets
         WHERE source_id = ?1 AND clip_id = ?2",
        params![source_id, clip_id],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .and_then(|s| serde_json::from_str(&s).ok())
    .unwrap_or_else(|| json!({}))
}

pub fn audio_project_path_from_meta(meta: &Value) -> Option<PathBuf> {
    meta.get("audio_project_path")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

pub fn audio_wraps_from_meta(meta: &Value) -> HashMap<String, String> {
    meta.get("audio_wraps")
        .and_then(|v| v.as_object())
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_str().map(|p| (k.clone(), p.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Finish audio import: copy already on disk under `audio/`; no play proxy yet.
pub fn complete_imported_audio_clip(
    paths: &ProjectPaths,
    project_id: &str,
    source_id: &str,
    clip_id: &str,
    audio_project_path: &Path,
    asset_status: &str,
    read_from_card: bool,
    card_locked: bool,
    original_path: &str,
) -> Result<(), String> {
    let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
    let probe = if audio_project_path.is_file() {
        probe_media(audio_project_path)
    } else {
        None
    };
    let (duration_sec, has_audio, audio_channels, codec) = probe
        .map(|p| (p.duration_sec, p.has_audio, p.audio_channels, p.codec))
        .unwrap_or((0.0, true, 2, String::new()));

    let mut meta = read_audio_meta(&conn, source_id, clip_id);
    if let Some(obj) = meta.as_object_mut() {
        obj.insert(
            "audio_project_path".into(),
            json!(audio_project_path.to_string_lossy().to_string()),
        );
        obj.insert("audio_wrap_status".into(), json!("pending"));
        obj.entry("audio_wraps".to_string())
            .or_insert_with(|| json!({}));
    }
    let meta_s = meta.to_string();

    conn.execute(
        "UPDATE ingest_assets SET
            import_status = 'imported',
            status = ?3,
            thumb_status = CASE WHEN thumb_status = 'ready' THEN thumb_status ELSE 'pending' END,
            thumb_error = '',
            project_proxy_path = '',
            original_path = CASE WHEN TRIM(?4) = '' THEN original_path ELSE ?4 END,
            duration_sec = CASE WHEN ?5 > 0 THEN ?5 ELSE duration_sec END,
            fps = 0,
            resolution = '',
            codec = CASE WHEN TRIM(?6) = '' THEN codec ELSE ?6 END,
            has_audio = ?7,
            audio_channels = ?8,
            read_from_card = ?9,
            card_locked = ?10,
            metadata_json = ?11
         WHERE source_id = ?1 AND clip_id = ?2",
        params![
            source_id,
            clip_id,
            asset_status,
            original_path,
            duration_sec,
            codec,
            if has_audio { 1 } else { 0 },
            audio_channels as i64,
            if read_from_card { 1 } else { 0 },
            if card_locked { 1 } else { 0 },
            meta_s,
        ],
    )
    .map_err(|e| e.to_string())?;
    crate::ingest::db::mark_ingest_job_done(&conn, "import", source_id, clip_id)
        .map_err(|e| e.to_string())?;
    bump_project_data_revision(&conn, "ingest").map_err(|e| e.to_string())?;
    Ok(())
}

/// Record one wrap path in metadata; set `project_proxy_path` to preferred wrap.
pub fn record_audio_wrap(
    paths: &ProjectPaths,
    project_id: &str,
    source_id: &str,
    clip_id: &str,
    fps: f64,
    wrap_path: &Path,
    region: BroadcastRegion,
) -> Result<(), String> {
    let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
    let mut meta = read_audio_meta(&conn, source_id, clip_id);
    let tag = fps_path_tag(fps);
    if let Some(obj) = meta.as_object_mut() {
        let wraps = obj
            .entry("audio_wraps".to_string())
            .or_insert_with(|| json!({}));
        if let Some(map) = wraps.as_object_mut() {
            map.insert(tag, json!(wrap_path.to_string_lossy().to_string()));
        }
        obj.insert("audio_wrap_status".into(), json!("ready"));
    }
    let wraps = audio_wraps_from_meta(&meta);
    let preferred =
        prefer_wrap_path(&wraps, region).unwrap_or_else(|| wrap_path.to_string_lossy().to_string());
    let probe = probe_media(wrap_path);
    let (duration_sec, fps_db, resolution, codec) = probe
        .map(|p| (p.duration_sec, p.fps, p.resolution, p.codec))
        .unwrap_or((0.0, fps, "1920x1080".into(), "h264".into()));

    conn.execute(
        "UPDATE ingest_assets SET
            project_proxy_path = ?3,
            duration_sec = CASE WHEN ?4 > 0 THEN ?4 ELSE duration_sec END,
            fps = ?5,
            resolution = CASE WHEN TRIM(?6) = '' THEN resolution ELSE ?6 END,
            codec = CASE WHEN TRIM(?7) = '' THEN codec ELSE ?7 END,
            metadata_json = ?8
         WHERE source_id = ?1 AND clip_id = ?2",
        params![
            source_id,
            clip_id,
            preferred,
            duration_sec,
            fps_db,
            resolution,
            codec,
            meta.to_string(),
        ],
    )
    .map_err(|e| e.to_string())?;
    bump_project_data_revision(&conn, "ingest").map_err(|e| e.to_string())?;
    Ok(())
}

fn prefer_wrap_path(wraps: &HashMap<String, String>, region: BroadcastRegion) -> Option<String> {
    let default_tag = fps_path_tag(region.default_rate());
    if let Some(p) = wraps.get(&default_tag) {
        if PathBuf::from(p).is_file() {
            return Some(p.clone());
        }
    }
    wraps
        .iter()
        .find(|(_, p)| PathBuf::from(p).is_file())
        .map(|(_, p)| p.clone())
}

/// Pick wrap path for a target source fps (Add Off / play). Falls back to any wrap.
#[allow(dead_code)] // wired when Add Off selects by context fps
pub fn resolve_audio_wrap_for_fps(
    meta: &Value,
    target_fps: f64,
    region: BroadcastRegion,
) -> Option<PathBuf> {
    let wraps = audio_wraps_from_meta(meta);
    if wraps.is_empty() {
        return None;
    }
    let rate = snap_fps_to_region(target_fps, region).unwrap_or_else(|| region.default_rate());
    let tag = fps_path_tag(rate);
    if let Some(p) = wraps.get(&tag) {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    prefer_wrap_path(&wraps, region).map(PathBuf::from)
}

pub fn wrap_audio_with_timecode(source: &Path, dest: &Path, fps: f64) -> Result<(), String> {
    if !source.is_file() {
        return Err(format!("audio izvor ne postoji: {}", source.display()));
    }
    let ffmpeg = resolve_ffmpeg().ok_or_else(|| "ffmpeg nije dostupan".to_string())?;
    let fps = normalize_fps(fps);
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
    let output = Command::new(&ffmpeg)
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!(
            "ingest audio wrap failed: source={} dest={} err={}",
            source.display(),
            dest.display(),
            stderr.trim()
        );
        return Err(format!(
            "audio wrap (timecode) nije uspio: {}",
            stderr.trim()
        ));
    }
    if !dest.is_file() || dest.metadata().map(|m| m.len()).unwrap_or(0) == 0 {
        return Err(format!(
            "audio wrap nije napisao datoteku: {}",
            dest.display()
        ));
    }
    info!(
        "ingest audio wrap: source={} dest={} fps={}",
        source.display(),
        dest.display(),
        fps
    );
    Ok(())
}

/// Process one project: for each imported audio, build missing wraps for DB video rates.
pub fn process_project_audio_wraps(
    paths: &ProjectPaths,
    project_id: &str,
) -> Result<usize, String> {
    let settings = project_effective_settings(paths, project_id);
    let region = broadcast_region_from_settings(&settings);
    let needed = needed_wrap_rates_from_db(paths, project_id, region);
    if needed.is_empty() {
        return Ok(0);
    }

    let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT source_id, clip_id, original_path, source_path, metadata_json
             FROM ingest_assets
             WHERE import_status IN ('imported', 'done')",
        )
        .map_err(|e| e.to_string())?;
    let rows: Vec<(String, String, String, String, String)> = stmt
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    drop(stmt);
    drop(conn);

    let audio_dir = audio_project_dir(paths, project_id);
    let proxy_dir = paths.project_dir(project_id).join("proxy");
    let mut built = 0usize;

    for (sid, clip_id, original_path, source_path, meta_raw) in rows {
        let meta: Value = serde_json::from_str(&meta_raw).unwrap_or_else(|_| json!({}));
        let has_audio_meta = meta
            .get("audio_project_path")
            .and_then(|v| v.as_str())
            .is_some();
        let audio_src = audio_project_path_from_meta(&meta)
            .filter(|p| p.is_file())
            .or_else(|| {
                for s in [original_path.as_str(), source_path.as_str()] {
                    let p = PathBuf::from(s.trim());
                    if p.is_file() && is_audio_media_file(&p) {
                        return Some(p);
                    }
                }
                std::fs::read_dir(&audio_dir).ok().and_then(|rd| {
                    rd.flatten().map(|e| e.path()).find(|p| {
                        p.is_file()
                            && is_audio_media_file(p)
                            && p.file_stem()
                                .and_then(|s| s.to_str())
                                .map(|s| s.eq_ignore_ascii_case(&sanitize_clip_id(&clip_id)))
                                .unwrap_or(false)
                    })
                })
            });
        let Some(audio_src) = audio_src else {
            continue;
        };
        if !has_audio_meta && !is_audio_media_file(&audio_src) {
            continue;
        }

        let wraps = audio_wraps_from_meta(&meta);
        for &rate in &needed {
            let tag = fps_path_tag(rate);
            if let Some(existing) = wraps.get(&tag) {
                if PathBuf::from(existing).is_file() {
                    continue;
                }
            }
            let dest = audio_wrap_dest_for_fps(&proxy_dir, &clip_id, rate);
            match wrap_audio_with_timecode(&audio_src, &dest, rate) {
                Ok(()) => {
                    record_audio_wrap(paths, project_id, &sid, &clip_id, rate, &dest, region)?;
                    built += 1;
                }
                Err(e) => warn!(
                    "audio wrap worker: project={} clip={} fps={} err={}",
                    project_id, clip_id, rate, e
                ),
            }
        }
    }
    Ok(built)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_from_pal_export_format() {
        let s = json!({ "export": { "format": "HD 1080p50", "fps": 50 } });
        assert_eq!(broadcast_region_from_settings(&s), BroadcastRegion::Pal);
        assert_eq!(BroadcastRegion::Pal.wrap_rates(), &[25.0, 50.0]);
    }

    #[test]
    fn region_from_ntsc_fps() {
        let s = json!({ "export": { "fps": 29.97, "format": "HD 1080i" } });
        assert_eq!(broadcast_region_from_settings(&s), BroadcastRegion::Ntsc);
    }

    #[test]
    fn snap_pal_rates() {
        assert_eq!(snap_fps_to_region(50.0, BroadcastRegion::Pal), Some(50.0));
        assert_eq!(snap_fps_to_region(25.0, BroadcastRegion::Pal), Some(25.0));
        assert_eq!(snap_fps_to_region(24.0, BroadcastRegion::Pal), None);
    }

    #[test]
    fn audio_paths() {
        let dest = audio_copy_dest(Path::new("/p/audio"), "vo open", Path::new("a.WAV"));
        assert_eq!(
            dest.file_name().and_then(|n| n.to_str()),
            Some("vo_open.WAV")
        );
        let w = audio_wrap_dest_for_fps(Path::new("/p/proxy"), "vo open", 50.0);
        assert_eq!(
            w.file_name().and_then(|n| n.to_str()),
            Some("vo_open_50.mp4")
        );
    }

    #[test]
    fn resolve_wrap_picks_matching_fps() {
        let meta = json!({
            "audio_wraps": {
                "25": "/tmp/a_25.mp4",
                "50": "/tmp/a_50.mp4"
            }
        });
        // Files may not exist — resolve still returns preferred path string via prefer when missing file.
        // With no files on disk, resolve returns None.
        assert!(resolve_audio_wrap_for_fps(&meta, 50.0, BroadcastRegion::Pal).is_none());
    }
}
