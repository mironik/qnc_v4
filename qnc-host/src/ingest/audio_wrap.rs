//! Audio import + deferred AV wrap (black video + timecode).
//!
//! Import only copies the file into `project/audio/`.
//! A background worker later builds playable MP4 wraps from **FPS already in
//! SQLite** (`ingest_assets.fps` on video rows) and the project export region
//! (PAL → 25/50, NTSC → 30/60). Never uses project/export fps as source clock.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use qnc_service_contracts::{
    AudioProbe, AudioProbeRequest, AudioWrapRequest, MediaLocator, MediaProcessor, MediaRef,
    ServiceError,
};
use rusqlite::params;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::frame_time::{normalize_fps, rational_fps, require_fps, DEFAULT_FPS};
use crate::ingest::db::open_ingest;
use crate::ingest::project_media::sanitize_clip_id;
use crate::ingest::proxy_source::{classify_tv_source, recipe_for_source};
use crate::ingest::store::ingest_probe_from_service;
use crate::ingest::thumb::resolve_ffmpeg;
use crate::media::is_audio_media_file;
use crate::project::db::{bump_project_data_revision, project_effective_settings, ProjectPaths};
use crate::project::ProjectDbBroker;

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
    if blob.contains("pal") || blob.contains("1080i50") || blob.contains("1080p50") {
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
    if n <= 0.0 {
        return "unknown".into();
    }
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
    project_db: &ProjectDbBroker,
    project_id: &str,
    region: BroadcastRegion,
) -> Vec<f64> {
    let Ok(rows) = project_db.serialize_project_write(project_id, || {
        let conn = open_ingest(paths, project_id).map_err(|error| error.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT source_path, original_path, proxy_path, project_proxy_path, fps
                 FROM ingest_assets
                 WHERE fps IS NOT NULL AND fps > 0
                   AND import_status IN ('imported', 'done', 'detected', 'generating_proxy')",
            )
            .map_err(|error| error.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, f64>(4)?,
                ))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        Ok(rows)
    }) else {
        return Vec::new();
    };

    let mut rates = HashSet::new();
    for (src, orig, proxy, proj, fps) in rows {
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
    probe: Option<&AudioProbe>,
) -> Result<(), String> {
    let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
    let (duration_sec, has_audio, audio_channels, codec) = probe
        .map(|p| {
            (
                p.duration_sec.unwrap_or(0.0),
                p.has_audio,
                p.audio_channels,
                p.codec.clone(),
            )
        })
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
    project_db: &ProjectDbBroker,
    project_id: &str,
    source_id: &str,
    clip_id: &str,
    fps: f64,
    wrap_path: &Path,
    region: BroadcastRegion,
    probe: Option<&crate::ingest::thumb::MediaProbe>,
) -> Result<(), String> {
    project_db.serialize_project_write(project_id, || {
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
        let preferred = prefer_wrap_path(&wraps, region)
            .unwrap_or_else(|| wrap_path.to_string_lossy().to_string());
        let (
            duration_sec,
            fps_db,
            resolution,
            codec,
            field_order,
            interlaced,
            source_class,
            proxy_recipe,
        ) = probe
            .map(|p| {
                let source_class = classify_tv_source(p);
                let proxy_recipe = recipe_for_source(source_class);
                (
                    p.duration_sec,
                    p.fps,
                    p.resolution.clone(),
                    p.codec.clone(),
                    p.field_order.clone(),
                    p.interlaced,
                    source_class.label().to_string(),
                    proxy_recipe.id().to_string(),
                )
            })
            .unwrap_or((
                0.0,
                fps,
                "1920x1080".into(),
                "h264".into(),
                String::new(),
                false,
                String::new(),
                String::new(),
            ));

        conn.execute(
            "UPDATE ingest_assets SET
                project_proxy_path = ?3,
                duration_sec = CASE WHEN ?4 > 0 THEN ?4 ELSE duration_sec END,
                fps = ?5,
                resolution = CASE WHEN TRIM(?6) = '' THEN resolution ELSE ?6 END,
                codec = CASE WHEN TRIM(?7) = '' THEN codec ELSE ?7 END,
                field_order = ?8,
                interlaced = ?9,
                source_class = ?10,
                proxy_recipe = ?11,
                metadata_json = ?12
             WHERE source_id = ?1 AND clip_id = ?2",
            params![
                source_id,
                clip_id,
                preferred,
                duration_sec,
                fps_db,
                resolution,
                codec,
                field_order,
                if interlaced { 1 } else { 0 },
                source_class,
                proxy_recipe,
                meta.to_string(),
            ],
        )
        .map_err(|e| e.to_string())?;
        bump_project_data_revision(&conn, "ingest").map_err(|e| e.to_string())?;
        Ok(())
    })
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

async fn probe_wrap_media(
    media_processor: Arc<dyn MediaProcessor>,
    clip_id: &str,
    media: &Path,
) -> Option<crate::ingest::thumb::MediaProbe> {
    match media_processor.probe(&media_ref(clip_id, media)).await {
        Ok(probe) => ingest_probe_from_service(probe),
        Err(error) => {
            warn!(
                "ingest audio wrap probe failed: clip={} path={} err={}",
                clip_id,
                media.display(),
                service_error_message(error)
            );
            None
        }
    }
}

pub async fn probe_audio_import_media(
    media_processor: Arc<dyn MediaProcessor>,
    clip_id: &str,
    media: &Path,
) -> Option<AudioProbe> {
    if !media.is_file() {
        return None;
    }
    match media_processor
        .probe_audio(AudioProbeRequest {
            input: media_ref(clip_id, media),
        })
        .await
    {
        Ok(probe) => Some(probe),
        Err(error) => {
            warn!(
                "ingest audio import probe failed: clip={} path={} err={}",
                clip_id,
                media.display(),
                service_error_message(error)
            );
            None
        }
    }
}

pub fn probe_audio_import_media_blocking(
    media_processor: Arc<dyn MediaProcessor>,
    clip_id: &str,
    media: &Path,
) -> Option<AudioProbe> {
    let handle = match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle,
        Err(error) => {
            warn!(
                "ingest audio import probe skipped: clip={} path={} no runtime: {}",
                clip_id,
                media.display(),
                error
            );
            return None;
        }
    };
    handle.block_on(probe_audio_import_media(media_processor, clip_id, media))
}

fn media_ref(clip_id: &str, media: &Path) -> MediaRef {
    MediaRef {
        clip_id: clip_id.to_string(),
        locator: MediaLocator::LocalPath {
            path: media.to_path_buf(),
        },
    }
}

fn service_error_message(error: ServiceError) -> String {
    format!("{}: {}", error.code, error.message)
}

/// Process one project: for each imported audio, build missing wraps for DB video rates.
pub async fn process_project_audio_wraps(
    paths: &ProjectPaths,
    project_db: &ProjectDbBroker,
    media_processor: Arc<dyn MediaProcessor>,
    project_id: &str,
) -> Result<usize, String> {
    let settings = project_effective_settings(paths, project_id);
    let region = broadcast_region_from_settings(&settings);
    let needed = needed_wrap_rates_from_db(paths, project_db, project_id, region);
    if needed.is_empty() {
        return Ok(0);
    }

    let rows: Vec<(String, String, String, String, String)> =
        project_db.serialize_project_write(project_id, || {
            let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
            let mut stmt = conn
                .prepare(
                    "SELECT source_id, clip_id, original_path, source_path, metadata_json
                 FROM ingest_assets
                 WHERE import_status IN ('imported', 'done')",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            Ok(rows)
        })?;

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
            match media_processor
                .build_audio_wrap(AudioWrapRequest {
                    input: media_ref(&clip_id, &audio_src),
                    output_path: dest.clone(),
                    fps: rate,
                })
                .await
            {
                Ok(_) => {
                    let probe = probe_wrap_media(media_processor.clone(), &clip_id, &dest).await;
                    record_audio_wrap(
                        paths,
                        project_db,
                        project_id,
                        &sid,
                        &clip_id,
                        rate,
                        &dest,
                        region,
                        probe.as_ref(),
                    )?;
                    built += 1;
                }
                Err(error) => warn!(
                    "audio wrap worker: project={} clip={} fps={} err={}",
                    project_id,
                    clip_id,
                    rate,
                    service_error_message(error)
                ),
            }
        }
    }
    Ok(built)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use qnc_service_contracts::{
        ArtifactRef, AudioProbe, AudioProbeRequest, AudioWrapRequest, ExtractRangeRequest,
        FilmstripFrameArtifact, FilmstripRequest, FrameExtractRequest, FrameTimebase,
        MediaProbe as ServiceMediaProbe, PosterExtractRequest, ProxyBuildRequest, ScanMode,
        ServiceError, ServiceResult, WaveformPeaks, WaveformRequest,
    };
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

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
    fn complete_audio_import_persists_supplied_audio_probe() {
        let base = std::env::temp_dir().join(format!(
            "qnc_audio_import_probe_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let paths = ProjectPaths {
            data_dir: base.join("data"),
            projects_root: base.join("projects"),
            seed_path: base.join("seed.json"),
        };
        let project_id = "audio_import_probe_project";
        let audio_dir = audio_project_dir(&paths, project_id);
        fs::create_dir_all(&audio_dir).unwrap();
        let audio = audio_dir.join("voice_a.wav");
        fs::write(&audio, b"audio").unwrap();

        let conn = open_ingest(&paths, project_id).expect("ingest db");
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, import_status, status, metadata_json)
             VALUES ('card', 'voice_a', 'voice_a', 'voice_a', 'processing', 'pending', '{}')",
            [],
        )
        .unwrap();
        drop(conn);

        let probe = AudioProbe {
            duration_sec: Some(4.25),
            codec: "pcm_s16le".into(),
            has_audio: true,
            audio_channels: 1,
        };
        complete_imported_audio_clip(
            &paths,
            project_id,
            "card",
            "voice_a",
            &audio,
            "ready",
            false,
            false,
            "",
            Some(&probe),
        )
        .unwrap();

        let conn = open_ingest(&paths, project_id).expect("ingest db");
        let row: (String, f64, f64, String, i64, i64, String) = conn
            .query_row(
                "SELECT import_status, duration_sec, fps, codec, has_audio, audio_channels, metadata_json
                 FROM ingest_assets
                 WHERE source_id = 'card' AND clip_id = 'voice_a'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(row.0, "imported");
        assert!((row.1 - 4.25).abs() < 0.001);
        assert_eq!(row.2, 0.0);
        assert_eq!(row.3, "pcm_s16le");
        assert_eq!(row.4, 1);
        assert_eq!(row.5, 1);
        let meta: Value = serde_json::from_str(&row.6).unwrap();
        assert_eq!(
            meta.get("audio_project_path").and_then(|v| v.as_str()),
            Some(audio.to_string_lossy().as_ref())
        );
        assert_eq!(
            meta.get("audio_wrap_status").and_then(|v| v.as_str()),
            Some("pending")
        );

        let _ = fs::remove_dir_all(&base);
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

    #[derive(Default)]
    struct FakeAudioWrapProcessor {
        wrap_calls: AtomicUsize,
        probe_calls: AtomicUsize,
    }

    #[async_trait]
    impl MediaProcessor for FakeAudioWrapProcessor {
        async fn probe(&self, _input: &MediaRef) -> ServiceResult<ServiceMediaProbe> {
            self.probe_calls.fetch_add(1, Ordering::AcqRel);
            Ok(ServiceMediaProbe {
                width: 1920,
                height: 1080,
                duration_sec: Some(8.0),
                timebase: FrameTimebase::new(50, 1).unwrap(),
                scan_mode: ScanMode::Progressive,
                codec: "h264".into(),
                field_order: "progressive".into(),
                frame_count: Some(400),
                duration_frames: Some(400),
                has_video: true,
                has_audio: true,
                audio_channels: 2,
            })
        }

        async fn probe_audio(&self, _request: AudioProbeRequest) -> ServiceResult<AudioProbe> {
            Err(unused_service_error())
        }

        async fn extract_frame(&self, _request: FrameExtractRequest) -> ServiceResult<ArtifactRef> {
            Err(unused_service_error())
        }

        async fn extract_poster(
            &self,
            _request: PosterExtractRequest,
        ) -> ServiceResult<ArtifactRef> {
            Err(unused_service_error())
        }

        async fn build_filmstrip(
            &self,
            _request: FilmstripRequest,
        ) -> ServiceResult<Vec<FilmstripFrameArtifact>> {
            Err(unused_service_error())
        }

        async fn build_proxy(&self, _request: ProxyBuildRequest) -> ServiceResult<ArtifactRef> {
            Err(unused_service_error())
        }

        async fn build_audio_wrap(&self, request: AudioWrapRequest) -> ServiceResult<ArtifactRef> {
            self.wrap_calls.fetch_add(1, Ordering::AcqRel);
            if let Some(parent) = request.output_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| ServiceError::new("test_fs_error", error.to_string()))?;
            }
            fs::write(&request.output_path, b"wrap")
                .map_err(|error| ServiceError::new("test_fs_error", error.to_string()))?;
            Ok(ArtifactRef {
                path: request.output_path,
                media_type: "video/mp4".into(),
                render_version: Some("test".into()),
            })
        }

        async fn build_waveform(&self, _request: WaveformRequest) -> ServiceResult<WaveformPeaks> {
            Err(unused_service_error())
        }

        async fn extract_range(&self, _request: ExtractRangeRequest) -> ServiceResult<ArtifactRef> {
            Err(unused_service_error())
        }
    }

    fn unused_service_error() -> ServiceError {
        ServiceError::new("unused", "unused in this test")
    }

    #[tokio::test]
    async fn process_audio_wraps_uses_media_processor_and_records_sqlite() {
        let base = std::env::temp_dir().join(format!(
            "qnc_audio_wrap_adapter_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let paths = ProjectPaths {
            data_dir: base.join("data"),
            projects_root: base.join("projects"),
            seed_path: base.join("seed.json"),
        };
        let project_db = ProjectDbBroker::new(paths.clone());
        let project_id = "audio_wrap_adapter_project";
        let conn = open_ingest(&paths, project_id).expect("ingest db");
        let video = base.join("video.mp4");
        fs::write(&video, b"video").unwrap();
        let audio_dir = audio_project_dir(&paths, project_id);
        fs::create_dir_all(&audio_dir).unwrap();
        let audio = audio_dir.join("voice_a.wav");
        fs::write(&audio, b"audio").unwrap();

        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, import_status, status, source_path, fps)
             VALUES ('card', 'video_a', 'video_a', 'video_a', 'imported', 'ready', ?1, 50.0)",
            params![video.to_string_lossy().to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, import_status, status, source_path, metadata_json)
             VALUES ('card', 'voice_a', 'voice_a', 'voice_a', 'imported', 'ready', ?1, ?2)",
            params![
                audio.to_string_lossy().to_string(),
                json!({
                    "audio_project_path": audio.to_string_lossy().to_string(),
                    "audio_wrap_status": "pending",
                    "audio_wraps": {}
                })
                .to_string()
            ],
        )
        .unwrap();
        drop(conn);

        let processor = Arc::new(FakeAudioWrapProcessor::default());
        let built = process_project_audio_wraps(&paths, &project_db, processor.clone(), project_id)
            .await
            .unwrap();

        assert_eq!(built, 1);
        assert_eq!(processor.wrap_calls.load(Ordering::Acquire), 1);
        assert_eq!(processor.probe_calls.load(Ordering::Acquire), 1);
        let expected_wrap = audio_wrap_dest_for_fps(
            &paths.project_dir(project_id).join("proxy"),
            "voice_a",
            50.0,
        );
        assert!(expected_wrap.is_file());

        let conn = open_ingest(&paths, project_id).expect("ingest db");
        let row: (String, f64, String, String, String) = conn
            .query_row(
                "SELECT project_proxy_path, fps, resolution, source_class, metadata_json
                 FROM ingest_assets
                 WHERE source_id = 'card' AND clip_id = 'voice_a'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(row.0, expected_wrap.to_string_lossy());
        assert!((row.1 - 50.0).abs() < 0.001);
        assert_eq!(row.2, "1920x1080");
        assert_eq!(row.3, "pal_50p");
        let meta: Value = serde_json::from_str(&row.4).unwrap();
        assert_eq!(
            meta.get("audio_wrap_status").and_then(|v| v.as_str()),
            Some("ready")
        );

        let _ = fs::remove_dir_all(&base);
    }
}
