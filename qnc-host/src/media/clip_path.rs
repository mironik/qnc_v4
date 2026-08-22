use std::path::{Path, PathBuf};

use crate::ingest::db::open_ingest;
use crate::project::db::ProjectPaths;

use super::resolve::{is_audio_media_file, is_media_file, is_proxy_media_path};

pub fn first_existing_path(values: &[String]) -> Option<PathBuf> {
    values
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

fn existing_video_path(value: &str) -> Option<PathBuf> {
    let path = PathBuf::from(value.trim());
    path.is_file()
        .then_some(path)
        .filter(|path| is_media_file(path) && !is_audio_media_file(path))
}

fn existing_proxy_path(value: &str, trust_proxy_column: bool) -> Option<PathBuf> {
    let path = existing_video_path(value)?;
    (trust_proxy_column || is_proxy_media_path(&path)).then_some(path)
}

fn choose_filmstrip_media(
    project_proxy: &str,
    proxy: &str,
    source: &str,
    original: &str,
    fallback: Option<&Path>,
) -> Option<PathBuf> {
    existing_proxy_path(project_proxy, false)
        .or_else(|| existing_proxy_path(proxy, true))
        .or_else(|| {
            fallback
                .filter(|path| path.is_file() && is_media_file(path) && !is_audio_media_file(path))
                .map(PathBuf::from)
        })
        .or_else(|| existing_video_path(source))
        .or_else(|| existing_video_path(original))
}

pub fn resolve_filmstrip_media(
    paths: &ProjectPaths,
    project_id: &str,
    clip_id: &str,
    fallback: Option<&Path>,
) -> Option<PathBuf> {
    let conn = open_ingest(paths, project_id).ok()?;
    let row: Option<(String, String, String, String)> = conn
        .query_row(
            "SELECT project_proxy_path, proxy_path, source_path, original_path
             FROM ingest_assets
             WHERE clip_id = ?1
             ORDER BY CASE import_status WHEN 'imported' THEN 0 WHEN 'done' THEN 1 ELSE 2 END
             LIMIT 1",
            rusqlite::params![clip_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .ok();
    let Some((project_proxy, proxy, source, original)) = row else {
        return fallback
            .filter(|path| path.is_file() && is_media_file(path) && !is_audio_media_file(path))
            .map(PathBuf::from);
    };
    choose_filmstrip_media(&project_proxy, &proxy, &source, &original, fallback)
}

/// Uvezeni klipovi s pronađenom medijskom datotekom (proxy/original).
pub fn imported_clip_media_rows(
    paths: &ProjectPaths,
    project_id: &str,
) -> Result<Vec<(String, PathBuf)>, String> {
    let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT clip_id, project_proxy_path, proxy_path, source_path, original_path
             FROM ingest_assets
             WHERE import_status IN ('imported', 'done')
             ORDER BY clip_id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        let (clip_id, project_proxy, proxy, source, original) = row.map_err(|e| e.to_string())?;
        if let Some(media) = first_existing_path(&[project_proxy, proxy, source, original]) {
            out.push((clip_id, media));
        }
    }
    Ok(out)
}

pub fn imported_filmstrip_media_rows(
    paths: &ProjectPaths,
    project_id: &str,
) -> Result<Vec<(String, PathBuf)>, String> {
    let conn = open_ingest(paths, project_id).map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT clip_id, project_proxy_path, proxy_path, source_path, original_path
             FROM ingest_assets
             WHERE import_status IN ('imported', 'done')
             ORDER BY clip_id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        let (clip_id, project_proxy, proxy, source, original) = row.map_err(|e| e.to_string())?;
        if let Some(media) =
            choose_filmstrip_media(&project_proxy, &proxy, &source, &original, None)
        {
            out.push((clip_id, media));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::choose_filmstrip_media;
    use std::fs;

    #[test]
    fn filmstrip_prefers_proxy_column_over_full_res_project_path() {
        let base = std::env::temp_dir().join(format!(
            "qnc_filmstrip_media_choice_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("card").join("PROXY")).unwrap();
        fs::create_dir_all(base.join("full")).unwrap();
        let original = base.join("full").join("clip.mxf");
        let proxy = base.join("card").join("PROXY").join("clip.mp4");
        fs::write(&original, b"original").unwrap();
        fs::write(&proxy, b"proxy").unwrap();

        let selected = choose_filmstrip_media(
            original.to_string_lossy().as_ref(),
            proxy.to_string_lossy().as_ref(),
            "",
            "",
            Some(&original),
        )
        .unwrap();

        assert_eq!(selected, proxy);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn filmstrip_uses_original_only_when_no_proxy_exists() {
        let base = std::env::temp_dir().join(format!(
            "qnc_filmstrip_media_fallback_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let original = base.join("clip.mxf");
        fs::write(&original, b"original").unwrap();

        let selected =
            choose_filmstrip_media("", "", original.to_string_lossy().as_ref(), "", None).unwrap();

        assert_eq!(selected, original);
        let _ = fs::remove_dir_all(base);
    }
}
