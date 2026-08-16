//! Playback media resolve — proxy-first (see docs/qnc-playback-engine.md).

use std::path::PathBuf;

use crate::media_pool::proxy_path_for_clip;
use crate::project::db::ProjectPaths;

/// Kind of file used for timeline play / scrub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayMediaKind {
    /// H.264 (or project proxy recipe) under `proxy/`.
    Proxy,
    /// Original master — export / open-source only (not PlaybackSession).
    Original,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PlayMedia {
    pub path: PathBuf,
    pub kind: PlayMediaKind,
    pub clip_id: String,
    pub field_order: String,
    pub interlaced: bool,
    pub source_class: String,
    pub proxy_recipe: String,
}

/// Resolve media for timeline playback. MVP: **proxy only**.
///
/// Missing proxy → `proxy_missing:…` (no silent fallback to original).
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
    let path = proxy_path_for_clip(paths, pid, clip)
        .filter(|p| p.is_file())
        .ok_or_else(|| format!("proxy_missing: Proxy za klip '{clip}' nije pronađen."))?;
    let meta = playback_probe_meta(paths, pid, clip);
    Ok(PlayMedia {
        path,
        kind: PlayMediaKind::Proxy,
        clip_id: clip.to_string(),
        field_order: meta.field_order,
        interlaced: meta.interlaced,
        source_class: meta.source_class,
        proxy_recipe: meta.proxy_recipe,
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
        field_order: meta.field_order,
        interlaced: meta.interlaced,
        source_class: meta.source_class,
        proxy_recipe: meta.proxy_recipe,
    })
}

#[derive(Default)]
struct PlaybackProbeMeta {
    field_order: String,
    interlaced: bool,
    source_class: String,
    proxy_recipe: String,
}

fn playback_probe_meta(paths: &ProjectPaths, project_id: &str, clip_id: &str) -> PlaybackProbeMeta {
    let Ok(conn) = crate::ingest::db::open_ingest(paths, project_id) else {
        return PlaybackProbeMeta::default();
    };
    conn.query_row(
        "SELECT COALESCE(field_order, ''), COALESCE(interlaced, 0),
                COALESCE(source_class, ''), COALESCE(proxy_recipe, '')
         FROM ingest_assets
         WHERE clip_id = ?1
         ORDER BY CASE import_status WHEN 'imported' THEN 0 WHEN 'done' THEN 1 ELSE 2 END,
                  CASE WHEN TRIM(COALESCE(project_proxy_path, '')) != '' THEN 0 ELSE 1 END,
                  source_id
         LIMIT 1",
        rusqlite::params![clip_id],
        |row| {
            Ok(PlaybackProbeMeta {
                field_order: row.get(0)?,
                interlaced: row.get::<_, i64>(1)? != 0,
                source_class: row.get(2)?,
                proxy_recipe: row.get(3)?,
            })
        },
    )
    .unwrap_or_default()
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
    fn resolve_play_media_errors_when_proxy_missing() {
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
            err.starts_with("proxy_missing"),
            "expected proxy_missing, got: {err}"
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
             VALUES ('test', ?1, ?1, ?1, 1.0, 25.0, 'active', 'imported',
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
             VALUES ('test', ?1, ?1, ?1, 1.0, 25.0, 'active', 'imported',
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
}
