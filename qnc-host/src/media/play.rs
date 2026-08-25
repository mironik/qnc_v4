//! Playback media resolve — preview-input first.

use std::path::{Path, PathBuf};

use crate::media_pool::proxy_path_for_clip;
use crate::project::db::ProjectPaths;
use rusqlite::params;

/// Kind of file used for timeline play / scrub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayMediaKind {
    /// H.264 (or project proxy recipe) under `proxy/`.
    Proxy,
    /// Original/source fallback when no proxy exists yet.
    Original,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PlayMedia {
    pub path: PathBuf,
    pub kind: PlayMediaKind,
    pub clip_id: String,
    pub fps_num: Option<u32>,
    pub fps_den: Option<u32>,
    pub duration_sec: Option<f64>,
    pub duration_frames: Option<i64>,
    pub has_audio: Option<bool>,
    pub audio_channels: Option<u8>,
    pub field_order: String,
    pub interlaced: bool,
    pub source_class: String,
    pub proxy_recipe: String,
}

/// Resolve media for preview playback.
///
/// Camera/project proxy wins. If no proxy exists, source/original is the
/// explicit ingest preview fallback.
pub fn resolve_play_media(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
) -> Result<PlayMedia, String> {
    let pid = project_id.trim();
    let clip = clip_id.trim();
    if pid.is_empty() {
        return Err("proxy_missing: project_id je prazan".into());
    }
    if clip.is_empty() {
        return Err("proxy_missing: clip_id je prazan".into());
    }
    let (path, kind) = resolve_preview_input_path(paths, pid, clip).ok_or_else(|| {
        format!("play_media_missing: Preview media za klip '{clip}' nije pronađen.")
    })?;
    let mut meta = playback_probe_meta(paths, pid, clip);
    if !meta.runtime_ready() {
        if let Some(runtime) = playback_runtime_probe(&path, clip) {
            if let Err(error) = persist_playback_runtime_probe(paths, pid, clip, runtime) {
                tracing::warn!(
                    "playback media probe cache write failed: project={} clip={} err={}",
                    pid,
                    clip,
                    error
                );
            }
            meta.merge_runtime(runtime);
        }
    }
    Ok(PlayMedia {
        path,
        kind,
        clip_id: clip.to_string(),
        fps_num: meta.fps_num,
        fps_den: meta.fps_den,
        duration_sec: meta.duration_sec,
        duration_frames: meta.duration_frames,
        has_audio: meta.has_audio,
        audio_channels: meta.audio_channels,
        field_order: meta.field_order,
        interlaced: meta.interlaced,
        source_class: meta.source_class,
        proxy_recipe: meta.proxy_recipe,
    })
}

fn resolve_preview_input_path(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
) -> Option<(PathBuf, PlayMediaKind)> {
    if let Some(path) = proxy_path_for_clip(paths, project_id, clip_id).filter(|p| p.is_file()) {
        return Some((path, PlayMediaKind::Proxy));
    }

    let conn = crate::ingest::db::open_ingest(paths, project_id).ok()?;
    let mut stmt = conn
        .prepare(
            "SELECT COALESCE(project_proxy_path, ''), COALESCE(proxy_path, ''),
                    COALESCE(original_path, ''), COALESCE(source_path, '')
             FROM ingest_assets
             WHERE clip_id = ?1
             ORDER BY CASE import_status
                    WHEN 'imported' THEN 0
                    WHEN 'done' THEN 1
                    WHEN 'generating_proxy' THEN 2
                    WHEN 'original_ready' THEN 3
                    WHEN 'queued' THEN 4
                    WHEN 'processing' THEN 5
                    WHEN 'detected' THEN 6
                    ELSE 7
                END,
                source_id",
        )
        .ok()?;
    let rows = stmt
        .query_map(params![clip_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .ok()?
        .filter_map(Result::ok)
        .collect::<Vec<_>>();

    for (project_proxy, card_proxy, _, _) in &rows {
        if let Some(path) = first_existing_video_path([project_proxy, card_proxy]) {
            return Some((path, PlayMediaKind::Proxy));
        }
    }
    for (_, _, original, source) in &rows {
        if let Some(path) = first_existing_video_path([original, source]) {
            return Some((path, PlayMediaKind::Original));
        }
    }
    None
}

fn first_existing_video_path<'a>(values: impl IntoIterator<Item = &'a String>) -> Option<PathBuf> {
    values
        .into_iter()
        .map(|value| PathBuf::from(value.trim()))
        .find(|path| {
            path.is_file()
                && super::resolve::is_media_file(path)
                && !super::resolve::is_audio_media_file(path)
        })
}

/// Resolve original/master media for export and “open source”.
///
/// Prefers `original_path`, then `source_path`. Never returns the play proxy.
/// Missing file → `original_missing:…`.
pub fn resolve_original_media(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
) -> Result<PlayMedia, String> {
    use crate::ingest::db::open_ingest;
    use crate::media::first_existing_path;

    let pid = project_id.trim();
    let clip = clip_id.trim();
    if pid.is_empty() || clip.is_empty() {
        return Err("original_missing: project_id/clip_id".into());
    }
    let conn = open_ingest(paths, pid).map_err(|e| e.to_string())?;
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT COALESCE(original_path, ''), COALESCE(source_path, '')
             FROM ingest_assets
             WHERE clip_id = ?1
               AND import_status IN ('original_ready', 'generating_proxy', 'imported', 'done')
             ORDER BY CASE import_status WHEN 'imported' THEN 0 WHEN 'done' THEN 1 ELSE 2 END
             LIMIT 1",
            rusqlite::params![clip],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let Some((original, source)) = row else {
        return Err(format!(
            "original_missing: klip '{clip}' nije u ingest_assets"
        ));
    };
    let path = first_existing_path(&[original, source])
        .ok_or_else(|| format!("original_missing: Original za klip '{clip}' nije pronađen."))?;
    let meta = playback_probe_meta(paths, pid, clip);
    Ok(PlayMedia {
        path,
        kind: PlayMediaKind::Original,
        clip_id: clip.to_string(),
        fps_num: meta.fps_num,
        fps_den: meta.fps_den,
        duration_sec: meta.duration_sec,
        duration_frames: meta.duration_frames,
        has_audio: meta.has_audio,
        audio_channels: meta.audio_channels,
        field_order: meta.field_order,
        interlaced: meta.interlaced,
        source_class: meta.source_class,
        proxy_recipe: meta.proxy_recipe,
    })
}

#[derive(Default)]
struct PlaybackProbeMeta {
    fps_num: Option<u32>,
    fps_den: Option<u32>,
    duration_sec: Option<f64>,
    duration_frames: Option<i64>,
    has_audio: Option<bool>,
    audio_channels: Option<u8>,
    field_order: String,
    interlaced: bool,
    source_class: String,
    proxy_recipe: String,
}

impl PlaybackProbeMeta {
    fn runtime_ready(&self) -> bool {
        self.fps_num.is_some()
            && self.fps_den.is_some()
            && self.duration_frames.is_some_and(|frames| frames > 0)
    }

    fn merge_runtime(&mut self, runtime: PlaybackRuntimeProbe) {
        if self.fps_num.is_none() || self.fps_den.is_none() {
            self.fps_num = Some(runtime.fps_num);
            self.fps_den = Some(runtime.fps_den);
        }
        if self.duration_frames.is_none_or(|frames| frames <= 0) {
            self.duration_frames = Some(runtime.duration_frames);
        }
        if self
            .duration_sec
            .is_none_or(|sec| !sec.is_finite() || sec <= 0.0)
        {
            self.duration_sec = Some(runtime.duration_sec);
        }
        self.has_audio = Some(runtime.has_audio);
        self.audio_channels = Some(runtime.audio_channels);
    }
}

fn playback_probe_meta(paths: &ProjectPaths, project_id: &str, clip_id: &str) -> PlaybackProbeMeta {
    let Ok(conn) = crate::ingest::db::open_ingest(paths, project_id) else {
        return PlaybackProbeMeta::default();
    };
    conn.query_row(
        "SELECT COALESCE(field_order, ''), COALESCE(interlaced, 0),
                COALESCE(source_class, ''), COALESCE(proxy_recipe, ''),
                COALESCE(duration_sec, 0), COALESCE(fps, 0),
                COALESCE(source_fps_num, 0), COALESCE(source_fps_den, 1),
                COALESCE(has_audio, 0), COALESCE(audio_channels, 0)
         FROM ingest_assets
         WHERE clip_id = ?1
         ORDER BY CASE import_status WHEN 'imported' THEN 0 WHEN 'done' THEN 1 ELSE 2 END,
                  CASE WHEN TRIM(COALESCE(project_proxy_path, '')) != '' THEN 0 ELSE 1 END,
                  source_id
         LIMIT 1",
        rusqlite::params![clip_id],
        |row| {
            let duration_sec = valid_positive_f64(row.get::<_, f64>(4)?);
            let fps = valid_positive_f64(row.get::<_, f64>(5)?);
            let stored_fps_num = row.get::<_, i64>(6)?;
            let stored_fps_den = row.get::<_, i64>(7)?;
            let (fps_num, fps_den) = valid_timebase(stored_fps_num, stored_fps_den)
                .or_else(|| fps.and_then(timebase_from_fps))
                .map(|(num, den)| (Some(num), Some(den)))
                .unwrap_or((None, None));
            let duration_frames = duration_sec
                .zip(fps_num.zip(fps_den))
                .map(|(duration, (num, den))| {
                    (duration * f64::from(num) / f64::from(den)).round() as i64
                })
                .filter(|frames| *frames > 0);
            let audio_channels = row
                .get::<_, i64>(9)
                .ok()
                .and_then(|channels| u8::try_from(channels).ok().filter(|channels| *channels > 0));
            Ok(PlaybackProbeMeta {
                fps_num,
                fps_den,
                duration_sec,
                duration_frames,
                has_audio: Some(row.get::<_, i64>(8)? != 0),
                audio_channels,
                field_order: row.get(0)?,
                interlaced: row.get::<_, i64>(1)? != 0,
                source_class: row.get(2)?,
                proxy_recipe: row.get(3)?,
            })
        },
    )
    .unwrap_or_default()
}

#[derive(Debug, Clone, Copy)]
struct PlaybackRuntimeProbe {
    fps_num: u32,
    fps_den: u32,
    duration_sec: f64,
    duration_frames: i64,
    has_audio: bool,
    audio_channels: u8,
}

fn playback_runtime_probe(path: &Path, clip_id: &str) -> Option<PlaybackRuntimeProbe> {
    let report = qnc_media_ffmpeg::probe_source_runtime(path, clip_id)
        .map_err(|error| {
            tracing::warn!(
                "playback media probe failed: clip={} path={} err={}",
                clip_id,
                path.display(),
                error
            );
        })
        .ok()?;
    let fps_num = report.source.timebase.frame_rate_num;
    let fps_den = report.source.timebase.frame_rate_den;
    if fps_num == 0 || fps_den == 0 {
        return None;
    }
    let duration_frames = i64::try_from(report.source.duration_frames).ok()?;
    if duration_frames <= 0 {
        return None;
    }
    let fps = f64::from(fps_num) / f64::from(fps_den);
    let audio_channels = report
        .source
        .audio_format
        .as_ref()
        .map(|format| format.channel_count.min(4) as u8)
        .unwrap_or(0);
    Some(PlaybackRuntimeProbe {
        fps_num,
        fps_den,
        duration_sec: duration_frames as f64 / fps,
        duration_frames,
        has_audio: report.has_audio,
        audio_channels,
    })
}

fn persist_playback_runtime_probe(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    runtime: PlaybackRuntimeProbe,
) -> Result<(), String> {
    if runtime.fps_num == 0
        || runtime.fps_den == 0
        || !runtime.duration_sec.is_finite()
        || runtime.duration_sec <= 0.0
        || runtime.duration_frames <= 0
    {
        return Ok(());
    }
    let conn = crate::ingest::db::open_ingest(paths, project_id).map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE ingest_assets SET
            duration_sec = ?2,
            fps = ?3,
            has_audio = ?4,
            audio_channels = ?5,
            source_fps_num = ?6,
            source_fps_den = ?7
         WHERE clip_id = ?1",
        params![
            clip_id,
            runtime.duration_sec,
            f64::from(runtime.fps_num) / f64::from(runtime.fps_den),
            if runtime.has_audio { 1 } else { 0 },
            i64::from(runtime.audio_channels),
            i64::from(runtime.fps_num),
            i64::from(runtime.fps_den),
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn valid_positive_f64(value: f64) -> Option<f64> {
    value.is_finite().then_some(value).filter(|v| *v > 0.0)
}

fn valid_timebase(fps_num: i64, fps_den: i64) -> Option<(u32, u32)> {
    let fps_num = u32::try_from(fps_num).ok()?;
    let fps_den = u32::try_from(fps_den).ok()?;
    (fps_num > 0 && fps_den > 0).then_some((fps_num, fps_den))
}

fn timebase_from_fps(fps: f64) -> Option<(u32, u32)> {
    if !fps.is_finite() || fps <= 0.0 {
        return None;
    }
    let common = [
        (24000.0 / 1001.0, 24000, 1001),
        (30000.0 / 1001.0, 30000, 1001),
        (60000.0 / 1001.0, 60000, 1001),
    ];
    for (value, num, den) in common {
        if (fps - value).abs() < 0.01 {
            return Some((num, den));
        }
    }
    let rounded = fps.round();
    if (fps - rounded).abs() < 0.001 && rounded > 0.0 {
        return u32::try_from(rounded as i64).ok().map(|num| (num, 1));
    }
    let den = 1000u32;
    let num = (fps * f64::from(den)).round() as u32;
    (num > 0).then_some(reduce_timebase(num, den))
}

fn reduce_timebase(num: u32, den: u32) -> (u32, u32) {
    let gcd = gcd_u32(num, den).max(1);
    (num / gcd, den / gcd)
}

fn gcd_u32(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::db::{open_global, ProjectPaths};
    use std::path::PathBuf;

    fn test_paths(base: &std::path::Path) -> ProjectPaths {
        ProjectPaths {
            data_dir: base.join("data"),
            projects_root: base.join("projects"),
            seed_path: PathBuf::from("nonexistent"),
        }
    }

    fn setup_project(paths: &ProjectPaths, project_id: &str) -> PathBuf {
        let _ = open_global(paths);
        let project_dir = paths.projects_root.join(project_id);
        std::fs::create_dir_all(project_dir.join("proxy")).unwrap();
        // Under cfg(test), open_project may create qnc_project.db without a projects row.
        let _ = crate::project::db::open_project(paths, project_id).unwrap();
        project_dir
    }

    #[test]
    fn resolve_play_media_errors_when_preview_input_missing() {
        let base = std::env::temp_dir().join(format!(
            "qnc_play_miss_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "play_proxy_miss";
        setup_project(&paths, project_id);

        let err = resolve_play_media(&paths, project_id, "no_such_clip").unwrap_err();
        assert!(
            err.starts_with("play_media_missing"),
            "expected play_media_missing, got: {err}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn resolve_play_media_returns_proxy_kind() {
        let base = std::env::temp_dir().join(format!(
            "qnc_play_ok_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "play_proxy_ok";
        let project_dir = setup_project(&paths, project_id);

        let clip_id = "clip_proxy_a";
        let proxy_file = project_dir.join("proxy").join(format!("{clip_id}.mp4"));
        std::fs::write(&proxy_file, b"fake-mp4-bytes-for-path-resolve").unwrap();

        let conn = crate::ingest::db::open_ingest(&paths, project_id).unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, duration_sec, fps, status, import_status,
                 selected, proxy_path, project_proxy_path, file_extension)
             VALUES ('test', ?1, ?1, ?1, 1.0, 50.0, 'active', 'imported',
                     0, ?2, ?2, 'mp4')",
            rusqlite::params![clip_id, proxy_file.to_string_lossy().to_string()],
        )
        .unwrap();

        let play = resolve_play_media(&paths, project_id, clip_id).unwrap();
        assert_eq!(play.kind, PlayMediaKind::Proxy);
        assert_eq!(play.clip_id, clip_id);
        assert_eq!(play.path, proxy_file);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn resolve_original_media_prefers_original_over_proxy() {
        let base = std::env::temp_dir().join(format!(
            "qnc_orig_ok_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "play_orig_ok";
        let project_dir = setup_project(&paths, project_id);

        let clip_id = "clip_orig_a";
        let proxy_file = project_dir.join("proxy").join(format!("{clip_id}.mp4"));
        let original_file = project_dir.join("original").join(format!("{clip_id}.mxf"));
        std::fs::create_dir_all(original_file.parent().unwrap()).unwrap();
        std::fs::write(&proxy_file, b"proxy-bytes").unwrap();
        std::fs::write(&original_file, b"original-mxf-bytes").unwrap();

        let conn = crate::ingest::db::open_ingest(&paths, project_id).unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, duration_sec, fps, status, import_status,
                 selected, proxy_path, project_proxy_path, original_path, file_extension)
             VALUES ('test', ?1, ?1, ?1, 1.0, 50.0, 'active', 'imported',
                     0, ?2, ?2, ?3, 'mxf')",
            rusqlite::params![
                clip_id,
                proxy_file.to_string_lossy().to_string(),
                original_file.to_string_lossy().to_string()
            ],
        )
        .unwrap();

        let play = resolve_play_media(&paths, project_id, clip_id).unwrap();
        assert_eq!(play.kind, PlayMediaKind::Proxy);
        assert_eq!(play.path, proxy_file);

        let orig = resolve_original_media(&paths, project_id, clip_id).unwrap();
        assert_eq!(orig.kind, PlayMediaKind::Original);
        assert_eq!(orig.path, original_file);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn resolve_original_media_errors_when_missing() {
        let base = std::env::temp_dir().join(format!(
            "qnc_orig_miss_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "play_orig_miss";
        setup_project(&paths, project_id);
        let err = resolve_original_media(&paths, project_id, "ghost").unwrap_err();
        assert!(err.starts_with("original_missing"), "got: {err}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn resolve_play_media_uses_card_proxy_before_original() {
        let base = std::env::temp_dir().join(format!(
            "qnc_play_card_proxy_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "play_card_proxy";
        let project_dir = setup_project(&paths, project_id);

        let clip_id = "clip_card_proxy";
        let card_proxy = project_dir
            .join("card")
            .join("proxy")
            .join(format!("{clip_id}.mp4"));
        let original = project_dir.join("card").join(format!("{clip_id}.mxf"));
        std::fs::create_dir_all(card_proxy.parent().unwrap()).unwrap();
        std::fs::write(&card_proxy, b"card-proxy").unwrap();
        std::fs::write(&original, b"original").unwrap();

        let conn = crate::ingest::db::open_ingest(&paths, project_id).unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, duration_sec, fps, status, import_status,
                 selected, proxy_path, source_path, file_extension)
             VALUES ('test', ?1, ?1, ?1, 1.0, 50.0, 'active', 'detected',
                     0, ?2, ?3, 'mxf')",
            rusqlite::params![
                clip_id,
                card_proxy.to_string_lossy().to_string(),
                original.to_string_lossy().to_string()
            ],
        )
        .unwrap();

        let play = resolve_play_media(&paths, project_id, clip_id).unwrap();
        assert_eq!(play.kind, PlayMediaKind::Proxy);
        assert_eq!(play.path, card_proxy);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn resolve_play_media_uses_original_when_proxy_does_not_exist() {
        let base = std::env::temp_dir().join(format!(
            "qnc_play_original_fallback_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "play_original_fallback";
        let project_dir = setup_project(&paths, project_id);

        let clip_id = "clip_original";
        let original = project_dir.join("card").join(format!("{clip_id}.mxf"));
        std::fs::create_dir_all(original.parent().unwrap()).unwrap();
        std::fs::write(&original, b"original").unwrap();

        let conn = crate::ingest::db::open_ingest(&paths, project_id).unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, duration_sec, fps, status, import_status,
                 selected, source_path, file_extension)
             VALUES ('test', ?1, ?1, ?1, 1.0, 50.0, 'active', 'detected',
                     0, ?2, 'mxf')",
            rusqlite::params![clip_id, original.to_string_lossy().to_string()],
        )
        .unwrap();

        let play = resolve_play_media(&paths, project_id, clip_id).unwrap();
        assert_eq!(play.kind, PlayMediaKind::Original);
        assert_eq!(play.path, original);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn persist_playback_runtime_probe_updates_ingest_snapshot_metadata() {
        let base = std::env::temp_dir().join(format!(
            "qnc_play_probe_cache_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "play_probe_cache";
        setup_project(&paths, project_id);

        let clip_id = "clip_probe_cache";
        let conn = crate::ingest::db::open_ingest(&paths, project_id).unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, duration_sec, fps, status, import_status,
                 selected, file_extension)
             VALUES ('test', ?1, ?1, ?1, 0.0, 0.0, 'active', 'imported',
                     0, 'mp4')",
            rusqlite::params![clip_id],
        )
        .unwrap();

        persist_playback_runtime_probe(
            &paths,
            project_id,
            clip_id,
            PlaybackRuntimeProbe {
                fps_num: 50,
                fps_den: 1,
                duration_sec: 2.0,
                duration_frames: 100,
                has_audio: true,
                audio_channels: 2,
            },
        )
        .unwrap();

        let row = conn
            .query_row(
                "SELECT duration_sec, fps, has_audio, audio_channels, source_fps_num, source_fps_den
                 FROM ingest_assets WHERE clip_id = ?1",
                rusqlite::params![clip_id],
                |row| {
                    Ok((
                        row.get::<_, f64>(0)?,
                        row.get::<_, f64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(row, (2.0, 50.0, 1, 2, 50, 1));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn resolve_original_media_ignores_detected_unimported_source() {
        let base = std::env::temp_dir().join(format!(
            "qnc_orig_detected_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = test_paths(&base);
        let project_id = "play_orig_detected";
        let project_dir = setup_project(&paths, project_id);

        let clip_id = "clip_detected_a";
        let source_file = project_dir.join("card").join(format!("{clip_id}.mxf"));
        std::fs::create_dir_all(source_file.parent().unwrap()).unwrap();
        std::fs::write(&source_file, b"detected-source").unwrap();

        let conn = crate::ingest::db::open_ingest(&paths, project_id).unwrap();
        conn.execute(
            "INSERT INTO ingest_assets
                (source_id, clip_id, name, media_id, duration_sec, fps, status, import_status,
                 selected, source_path, file_extension)
             VALUES ('test', ?1, ?1, ?1, 1.0, 50.0, 'detected', 'detected',
                     0, ?2, 'mxf')",
            rusqlite::params![clip_id, source_file.to_string_lossy().to_string()],
        )
        .unwrap();

        let err = resolve_original_media(&paths, project_id, clip_id).unwrap_err();
        assert!(err.starts_with("original_missing"), "got: {err}");
        let _ = std::fs::remove_dir_all(&base);
    }
}
